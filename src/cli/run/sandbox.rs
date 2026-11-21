//! Sandboxed execution for the `run` capability (§4.4).
//!
//! Security shape (non-negotiable):
//! - allow-listed programs only, with per-program subcommand filters;
//! - argv is passed as an array straight to `execvp` — no shell ever
//!   interprets it; a single-string command is accepted only when it carries
//!   no shell metacharacters (it is split on whitespace, never quoted);
//! - the working directory is the project root (or a caller-supplied
//!   *relative* subdirectory), both canonicalized and checked to stay inside
//!   the canonical project root;
//! - `./script/*` entries must exist, be regular files, and canonicalize
//!   inside the project — `../` and symlink escapes are refused;
//! - stdin is closed, a timeout is mandatory, and the output env forces
//!   `CARGO_TERM_COLOR=never`, `NO_COLOR=1` and no pager;
//! - captured output is scrubbed through [`LogScrubber`] *before* it touches
//!   disk, and each stream is size-capped (bytes past the cap are drained so
//!   the child never blocks on a full pipe, counted, and the drop announced).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::run_log::{self, StoredLog};

/// Default wall-clock limit. Mandatory: a run may raise it, never remove it.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 600;

/// Per-stream cap on what is *retained* (memory and disk). Past the cap the
/// pipe keeps being drained so the child cannot deadlock; bytes are counted
/// and the drop is written into the stored log.
pub(crate) const MAX_STREAM_BYTES: usize = 4 * 1024 * 1024;

/// Characters that only make sense to a shell. A single-string command
/// containing any of them is rejected with a hint to pass `argv` instead.
const SHELL_METACHARS: &[char] = &[
    ';', '|', '&', '$', '`', '(', ')', '<', '>', '\n', '\r', '"', '\'', '\\', '*',
];

/// Everything that went wrong before (or during) a sandboxed run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunError {
    /// No command was given.
    Empty,
    /// Program is not on the allow-list.
    NotAllowed(String),
    /// A single-string command carried shell metacharacters.
    ShellMetacharacters(String),
    /// A path (cwd or script) escapes the project.
    PathEscape(String),
    /// A `./script/*` entry that does not exist.
    MissingScript(String),
    /// The process could not be spawned.
    Spawn(String),
    /// The mandatory timeout elapsed; the child was killed.
    Timeout(u64),
    /// The stored log could not be written.
    Log(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RunError::Empty => write!(f, "no command given"),
            RunError::NotAllowed(program) => write!(
                f,
                "program '{program}' is not on the run allow-list \
                 (cargo, npm, pnpm, yarn, npx tsc, pytest, python -m pytest, \
                 go test|build|vet, make, just, ./script/*)"
            ),
            RunError::ShellMetacharacters(command) => write!(
                f,
                "command string contains shell metacharacters and is refused: \
                 '{command}'. Pass argv as an array instead — it is executed \
                 without a shell"
            ),
            RunError::PathEscape(path) => {
                write!(f, "path '{path}' escapes the project and is refused")
            }
            RunError::MissingScript(script) => {
                write!(f, "script '{script}' does not exist in the project")
            }
            RunError::Spawn(error) => write!(f, "failed to spawn: {error}"),
            RunError::Timeout(secs) => write!(f, "timed out after {secs}s and was killed"),
            RunError::Log(error) => write!(f, "log store error: {error}"),
        }
    }
}

/// Build argv from the two accepted forms. The array form is used verbatim
/// (executed without a shell). The single-string form is split on whitespace
/// but refused outright when it contains shell metacharacters.
pub(crate) fn argv_from_parts(
    command: Option<&str>,
    argv: Option<&[String]>,
) -> Result<Vec<String>, RunError> {
    if let Some(argv) = argv {
        if argv.is_empty() {
            return Err(RunError::Empty);
        }
        return Ok(argv.to_vec());
    }
    let Some(command) = command else {
        return Err(RunError::Empty);
    };
    if command.chars().any(|c| SHELL_METACHARS.contains(&c)) {
        return Err(RunError::ShellMetacharacters(command.to_string()));
    }
    let split: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    if split.is_empty() {
        return Err(RunError::Empty);
    }
    Ok(split)
}

/// Allow-list with per-program subcommand filters.
fn is_allowed(argv: &[String]) -> Result<(), RunError> {
    let program = argv[0].as_str();
    let args = &argv[1..];
    let allowed = match program {
        "cargo" | "npm" | "pnpm" | "yarn" | "pytest" | "make" | "just" => true,
        // `npx <anything-but-tsc>` would happily download a package; only the
        // TypeScript compiler is allowed through.
        "npx" => args.first().map(String::as_str) == Some("tsc"),
        // `python -c '…'` and friends stay out; only the pytest module form.
        "python" | "python3" => {
            args.len() >= 2 && args[0] == "-m" && args[1] == "pytest"
        }
        "go" => matches!(
            args.first().map(String::as_str),
            Some("test" | "build" | "vet")
        ),
        // Existence and confinement are checked against the real filesystem
        // in [`validate_workdir`]; the prefix alone only narrows the shape.
        p if p.starts_with("./script/") => true,
        _ => false,
    };
    if allowed {
        Ok(())
    } else {
        Err(RunError::NotAllowed(program.to_string()))
    }
}

/// Validate the request and return the canonical working directory.
fn validate_workdir(
    argv: &[String],
    project_root: &Path,
    cwd: Option<&Path>,
) -> Result<PathBuf, RunError> {
    let program = argv
        .first()
        .ok_or(RunError::Empty)?
        .clone();
    is_allowed(argv)?;
    let root = std::fs::canonicalize(project_root)
        .map_err(|_| RunError::PathEscape(project_root.display().to_string()))?;

    let workdir = match cwd {
        None => root.clone(),
        Some(cwd) => {
            // Absolute cwds are refused outright: confinement is defined
            // relative to the project, never to the machine.
            if cwd.is_absolute() {
                return Err(RunError::PathEscape(cwd.display().to_string()));
            }
            let joined = root.join(cwd);
            let canonical = std::fs::canonicalize(&joined)
                .map_err(|_| RunError::PathEscape(cwd.display().to_string()))?;
            if !canonical.starts_with(&root) {
                return Err(RunError::PathEscape(cwd.display().to_string()));
            }
            canonical
        }
    };

    if program.starts_with("./script/") {
        let script = root.join(&program);
        let canonical = std::fs::canonicalize(&script)
            .map_err(|_| RunError::MissingScript(program.clone()))?;
        if !canonical.is_file() || !canonical.starts_with(&root) {
            // Either a symlink/.. escape or a directory: both refused.
            return Err(RunError::PathEscape(program));
        }
    }
    Ok(workdir)
}

/// Output environment: deterministic, colorless, pagerless.
fn child_env() -> HashMap<String, String> {
    let mut env: HashMap<String, String> = std::env::vars().collect();
    for (key, value) in [
        ("CARGO_TERM_COLOR", "never"),
        ("NO_COLOR", "1"),
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("MANPAGER", "cat"),
        ("TERM", "dumb"),
    ] {
        env.insert(key.to_string(), value.to_string());
    }
    env
}

/// One drained output stream: what was kept (capped) and what was dropped.
struct Captured {
    text: String,
    kept: usize,
    dropped: usize,
}

async fn drain_stream<R: tokio::io::AsyncRead + Unpin>(mut pipe: R) -> Captured {
    let mut captured = Captured {
        text: String::new(),
        kept: 0,
        dropped: 0,
    };
    let mut buf = vec![0u8; 8192];
    loop {
        match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let room = MAX_STREAM_BYTES.saturating_sub(captured.kept);
                let keep = room.min(n);
                if keep > 0 {
                    captured.text.push_str(&String::from_utf8_lossy(&buf[..keep]));
                }
                captured.kept += keep;
                captured.dropped += n - keep;
            }
        }
    }
    captured
}

/// Result of one sandboxed run: process outcome plus the stored log.
pub(crate) struct RunOutcome {
    /// Process exit code; `None` when the run was killed by the timeout.
    pub exit_code: Option<i32>,
    /// True when the mandatory timeout elapsed and the child was killed.
    pub timed_out: bool,
    /// Wall-clock duration in milliseconds (including the kill on timeout).
    pub duration_ms: u64,
    /// The stored log (name + absolute path).
    pub log: StoredLog,
    /// Combined, scrubbed output the parsers see.
    pub captured: String,
}

/// Execute an allow-listed command inside the project.
///
/// `log` must come from [`run_log::prepare`]; output is scrubbed and written
/// there before this function returns.
pub(crate) async fn execute(
    project_root: &Path,
    argv: &[String],
    cwd: Option<&Path>,
    timeout: Duration,
    log: &StoredLog,
) -> Result<RunOutcome, RunError> {
    let workdir = validate_workdir(argv, project_root, cwd)?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .current_dir(&workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    for (key, value) in child_env() {
        command.env(key, value);
    }
    // Cargo builds spawned by LeIndex land in the project's own storage so a
    // run never contends with (or pollutes) the caller's interactive target
    // dir. Script wrappers exec cargo, so they inherit the same override.
    if argv[0] == "cargo" || argv[0].starts_with("./script/") {
        let target = run_log::storage_dir(project_root).join("target");
        command.env("CARGO_TARGET_DIR", target);
    }

    let mut child = command
        .spawn()
        .map_err(|error| RunError::Spawn(error.to_string()))?;
    let stdout_pipe = child.stdout.take().ok_or_else(|| RunError::Spawn("stdout unavailable".to_string()))?;
    let stderr_pipe = child.stderr.take().ok_or_else(|| RunError::Spawn("stderr unavailable".to_string()))?;
    let stdout_task = tokio::spawn(drain_stream(stdout_pipe));
    let stderr_task = tokio::spawn(drain_stream(stderr_pipe));

    let started = Instant::now();
    let wait = child.wait();
    let (status, timed_out) = match tokio::time::timeout(timeout, wait).await {
        Ok(status) => (status, false),
        Err(_) => {
            // The child is killed via the Command handle; reap it so the
            // pipes close and the drain tasks finish.
            let _ = child.start_kill();
            let _ = child.wait().await;
            (
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timeout",
                )),
                true,
            )
        }
    };
    let stdout_captured = stdout_task.await.unwrap_or_else(|_| Captured {
        text: String::new(),
        kept: 0,
        dropped: 0,
    });
    let stderr_captured = stderr_task.await.unwrap_or_else(|_| Captured {
        text: String::new(),
        kept: 0,
        dropped: 0,
    });
    let duration_ms = started.elapsed().as_millis() as u64;

    // Scrub before anything touches disk or a return path.
    let mut content = crate::observability::LogScrubber::redact(&stdout_captured.text);
    if !stderr_captured.text.is_empty() {
        content.push_str("\n----- stderr -----\n");
        content.push_str(&crate::observability::LogScrubber::redact(&stderr_captured.text));
    }
    let total_dropped = stdout_captured.dropped + stderr_captured.dropped;
    if total_dropped > 0 {
        content.push_str(&format!(
            "\n[log truncated: {total_dropped} bytes past the {MAX_STREAM_BYTES}-byte cap were drained and dropped]\n"
        ));
    }
    run_log::write_log(&log.path, &content).map_err(|error| RunError::Log(error.to_string()))?;
    run_log::enforce_retention(project_root, super::KEEP_LOGS);

    let exit_code = if timed_out {
        None
    } else {
        status.ok().and_then(|status| status.code())
    };
    let mut captured = stdout_captured.text;
    if !stderr_captured.text.is_empty() {
        captured.push('\n');
        captured.push_str(&stderr_captured.text);
    }
    Ok(RunOutcome {
        exit_code,
        timed_out,
        duration_ms,
        log: log.clone(),
        captured,
    })
}

#[cfg(test)]
#[path = "sandbox_test.rs"]
mod sandbox_tests;
