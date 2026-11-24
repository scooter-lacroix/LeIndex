//! Sandboxed execution for the `run` capability (§4.4).
//!
//! - allow-listed programs only; argv array is primary, a single string is
//!   accepted only when free of shell metacharacters (split on whitespace);
//! - no shell ever interprets the command;
//! - cwd is canonicalized and must stay inside the canonical project root;
//! - `script/*` executables must canonicalize inside `<project>/script/`;
//! - stdin is null, the environment is forced colourless/pagerless;
//! - one deadline covers the child AND output collection, and on expiry the
//!   whole process group is killed, so a descendant holding the pipes open
//!   cannot hang the call;
//! - output is capped, scrubbed line by line by `LogScrubber` before it is
//!   stored, and the cut is announced in the log. stdout and stderr are
//!   collected separately and stored as stdout followed by stderr, so a
//!   structured stdout record (Cargo's JSON lines) can never be split by an
//!   interleaved stderr chunk.

use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::parsers::{self, Summary, ToolKind};
use super::run_log::{self, MAX_LOG_BYTES, StoredLog};
use crate::observability::LogScrubber;

/// Timeout applied when the caller gives none. A timeout can be raised up to
/// [`MAX_TIMEOUT_SECS`] but never removed.
pub(crate) const DEFAULT_TIMEOUT_SECS: u64 = 600;
/// Upper bound for a caller-supplied timeout.
pub(crate) const MAX_TIMEOUT_SECS: u64 = 3600;

/// Characters only a shell understands; a single-string command containing
/// any is refused.
const SHELL_METACHARS: &[char] = &[';', '|', '&', '$', '`', '>', '<', '"', '\'', '\\', '\n'];

/// Reserved room inside the log cap for the truncation note.
const NOTE_RESERVE: usize = 256;

/// Everything that can go wrong before or during a run.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum RunError {
    /// No command given.
    Empty,
    /// Program (or python module) is not on the allow-list.
    NotAllowed(String),
    /// A single-string command carried shell metacharacters.
    ShellMetacharacters(String),
    /// cwd or script path escapes the project.
    PathEscape(String),
    /// `script/*` entry does not exist.
    MissingScript(String),
    /// Spawn failure.
    Spawn(String),
    /// Log store failure.
    Log(String),
}

impl std::fmt::Display for RunError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "no command given"),
            Self::NotAllowed(what) => write!(
                f,
                "'{what}' is not allowed; allow-list: cargo, npm, pnpm, yarn, npx, pytest, \
                 python -m pytest|pyright, go, make, just, script/*"
            ),
            Self::ShellMetacharacters(command) => write!(
                f,
                "command string has shell metacharacters ('{command}'); pass argv as an array \
                 (no shell is used)"
            ),
            Self::PathEscape(path) => write!(f, "path '{path}' escapes the project"),
            Self::MissingScript(script) => {
                write!(f, "script '{script}' not found in <project>/script/")
            }
            Self::Spawn(error) => write!(f, "spawn failed: {error}"),
            Self::Log(error) => write!(f, "log store: {error}"),
        }
    }
}

impl std::error::Error for RunError {}

/// One run request.
pub(crate) struct RunRequest<'a> {
    /// Canonical or raw project root.
    pub project_root: &'a Path,
    /// Storage dir that owns `logs/`.
    pub storage: &'a Path,
    /// Array form (primary).
    pub argv: Option<&'a [String]>,
    /// Single-string form.
    pub command: Option<&'a str>,
    /// Working directory (absolute or project-relative).
    pub cwd: Option<&'a Path>,
    /// Timeout in seconds; `None` means [`DEFAULT_TIMEOUT_SECS`].
    pub timeout_secs: Option<u64>,
    /// Log handle name; `None` reserves a unique `run-<ms>-<pid>-<n>`.
    pub name: Option<&'a str>,
}

/// Build argv from the two accepted forms.
pub(crate) fn argv_from_parts(
    command: Option<&str>,
    argv: Option<&[String]>,
) -> Result<Vec<String>, RunError> {
    if let Some(argv) = argv {
        return if argv.is_empty() {
            Err(RunError::Empty)
        } else {
            Ok(argv.to_vec())
        };
    }
    let command = command.ok_or(RunError::Empty)?;
    if command.chars().any(|c| SHELL_METACHARS.contains(&c)) {
        return Err(RunError::ShellMetacharacters(command.to_string()));
    }
    let split: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    if split.is_empty() {
        Err(RunError::Empty)
    } else {
        Ok(split)
    }
}

/// `script/x` and `./script/x` (no `..`) name a project script.
fn script_relative(program: &str) -> Option<&str> {
    let rel = program.strip_prefix("./").unwrap_or(program);
    (rel.starts_with("script/")
        && !Path::new(rel)
            .components()
            .any(|c| c == Component::ParentDir))
    .then_some(rel)
}

/// Allow-list. Returns the script path when argv[0] is a project script.
fn check_allowed(argv: &[String]) -> Result<Option<String>, RunError> {
    let program = argv.first().ok_or(RunError::Empty)?.as_str();
    if let Some(rel) = script_relative(program) {
        return Ok(Some(rel.to_string()));
    }
    let allowed = match program {
        "cargo" | "npm" | "pnpm" | "yarn" | "npx" | "pytest" | "go" | "make" | "just" => true,
        "python" | "python3" => {
            argv.get(1).map(String::as_str) == Some("-m")
                && matches!(argv.get(2).map(String::as_str), Some("pytest" | "pyright"))
        }
        _ => false,
    };
    if allowed {
        Ok(None)
    } else {
        Err(RunError::NotAllowed(argv.join(" ")))
    }
}

/// Canonical project root, the working directory, and the absolute script
/// path (when argv[0] is a script). Every path is confined to the root.
fn resolve_paths(
    argv: &[String],
    project_root: &Path,
    cwd: Option<&Path>,
) -> Result<(PathBuf, PathBuf, Option<PathBuf>), RunError> {
    let script = check_allowed(argv)?;
    let root = std::fs::canonicalize(project_root)
        .map_err(|_| RunError::PathEscape(project_root.display().to_string()))?;
    let workdir = match cwd {
        None => root.clone(),
        Some(cwd) => {
            let joined = if cwd.is_absolute() {
                cwd.to_path_buf()
            } else {
                root.join(cwd)
            };
            let canonical = std::fs::canonicalize(&joined)
                .map_err(|_| RunError::PathEscape(cwd.display().to_string()))?;
            if !canonical.starts_with(&root) || !canonical.is_dir() {
                return Err(RunError::PathEscape(cwd.display().to_string()));
            }
            canonical
        }
    };
    let script_path = match script {
        None => None,
        Some(rel) => {
            let canonical = std::fs::canonicalize(root.join(&rel))
                .map_err(|_| RunError::MissingScript(rel.clone()))?;
            if !canonical.is_file() || !canonical.starts_with(root.join("script")) {
                return Err(RunError::PathEscape(rel));
            }
            Some(canonical)
        }
    };
    Ok((root, workdir, script_path))
}

/// Parser for a command, from its argv.
pub(crate) fn tool_kind(argv: &[String]) -> ToolKind {
    let has = |needle: &str| argv.iter().any(|a| a == needle);
    match argv[0].as_str() {
        "cargo" if argv.iter().any(|a| a.starts_with("--message-format=json")) => {
            ToolKind::CargoJson
        }
        "cargo" if has("test") => ToolKind::CargoTest,
        "pytest" => ToolKind::Pytest,
        "python" | "python3" if has("pytest") => ToolKind::Pytest,
        // pyright and the go toolchain both print `file:line:col: message`.
        "python" | "python3" | "go" => ToolKind::FileLineCol,
        "npx" if has("tsc") => ToolKind::Tsc,
        "tsc" => ToolKind::Tsc,
        _ => ToolKind::UnknownTool,
    }
}

/// `cargo check|clippy|build` gets JSON output so errors dedupe by code and
/// warnings count per lint.
fn with_json_format(mut argv: Vec<String>) -> Vec<String> {
    let machine = argv.iter().any(|a| a.starts_with("--message-format"));
    let compiles = argv
        .get(1)
        .is_some_and(|sub| matches!(sub.as_str(), "check" | "clippy" | "build"));
    if argv[0] == "cargo" && compiles && !machine {
        argv.push("--message-format=json".to_string());
    }
    argv
}

fn child_env(command: &mut Command) {
    for (key, value) in [
        ("CARGO_TERM_COLOR", "never"),
        ("NO_COLOR", "1"),
        ("PAGER", "cat"),
        ("GIT_PAGER", "cat"),
        ("GIT_TERMINAL_PROMPT", "0"),
    ] {
        command.env(key, value);
    }
}

/// Capped buffer fed by one pipe; bytes past the cap are counted.
#[derive(Default)]
struct Sink {
    bytes: Vec<u8>,
    dropped: usize,
}

impl Sink {
    /// Room left before the cap, reserving space for the truncation note.
    fn room(&self) -> usize {
        (MAX_LOG_BYTES - NOTE_RESERVE).saturating_sub(self.bytes.len())
    }

    /// Append up to the cap; the rest is counted as dropped.
    fn push(&mut self, chunk: &[u8]) {
        let keep = self.room().min(chunk.len());
        self.bytes.extend_from_slice(&chunk[..keep]);
        self.dropped += chunk.len() - keep;
    }
}

async fn pump<R: tokio::io::AsyncRead + Unpin>(mut pipe: R, sink: Arc<Mutex<Sink>>) {
    let mut buf = vec![0u8; 8192];
    while let Ok(n) = pipe.read(&mut buf).await {
        if n == 0 {
            break;
        }
        let mut sink = sink.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        sink.push(&buf[..n]);
    }
}

/// Kill the child and its whole process group.
///
/// `pgid` is the process-group id captured when the child was spawned (it
/// is the child's own pid because the command was started with
/// `process_group(0)`). It must be passed in explicitly: by the time a run
/// is torn down the direct child may already have been reaped, and tokio's
/// `Child::id()` then returns `None`, which would skip the group signal and
/// leave every descendant alive.
fn kill_tree(child: &mut tokio::process::Child, pgid: Option<i32>) {
    #[cfg(unix)]
    if let Some(pgid) = pgid {
        // SAFETY: plain signal delivery to the group this call created with
        // `process_group(0)`; no memory is touched.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    let _ = child.start_kill();
}

/// Scrub line by line, then enforce the hard cap with an announced cut.
/// `LogScrubber::redact` removes every occurrence of each secret pattern on a
/// line, so one pass per line leaves no secret behind.
pub(crate) fn finish_log(raw: &[u8], dropped: usize, timed_out: Option<u64>) -> String {
    let text = String::from_utf8_lossy(raw);
    let mut out = String::with_capacity(text.len());
    for line in text.lines() {
        out.push_str(&LogScrubber::redact(line));
        out.push('\n');
    }
    if dropped > 0 {
        out.push_str(&format!(
            "[log truncated: {dropped} bytes dropped past the 8 MiB cap]\n"
        ));
    }
    if let Some(secs) = timed_out {
        out.push_str(&format!("[killed: timed out after {secs}s]\n"));
    }
    if out.len() > MAX_LOG_BYTES {
        let mut cut = MAX_LOG_BYTES - NOTE_RESERVE;
        while !out.is_char_boundary(cut) {
            cut -= 1;
        }
        out.truncate(cut);
        out.push_str("\n[log truncated after scrubbing: 8 MiB cap]\n");
    }
    out
}

/// Run an allow-listed command and summarise it.
pub(crate) async fn run(request: RunRequest<'_>) -> Result<Summary, RunError> {
    let argv = with_json_format(argv_from_parts(request.command, request.argv)?);
    let (root, workdir, script_path) = resolve_paths(&argv, request.project_root, request.cwd)?;
    let log = run_log::prepare(request.storage, request.name)
        .map_err(|error| RunError::Log(error.to_string()))?;
    let timeout_secs = request
        .timeout_secs
        .unwrap_or(DEFAULT_TIMEOUT_SECS)
        .clamp(1, MAX_TIMEOUT_SECS);
    let (exit_code, duration_ms, raw, dropped, timed_out) =
        execute(&argv, &root, &workdir, script_path, timeout_secs).await?;
    store_and_summarise(
        &argv,
        &log,
        exit_code,
        duration_ms,
        &raw,
        dropped,
        timed_out,
        request.storage,
    )
}

#[allow(clippy::too_many_arguments)]
fn store_and_summarise(
    argv: &[String],
    log: &StoredLog,
    exit_code: i32,
    duration_ms: u64,
    raw: &[u8],
    dropped: usize,
    timed_out: Option<u64>,
    storage: &Path,
) -> Result<Summary, RunError> {
    let content = finish_log(raw, dropped, timed_out);
    std::fs::write(&log.path, &content).map_err(|error| RunError::Log(error.to_string()))?;
    run_log::enforce_retention(storage, run_log::KEEP_LOGS)
        .map_err(|error| RunError::Log(error.to_string()))?;
    let mut summary = parsers::summarize(
        tool_kind(argv),
        &content,
        exit_code,
        duration_ms,
        Some(log.path.clone()),
    );
    if let Some(secs) = timed_out {
        summary.headline = format!(
            "timed out after {secs}s and was killed; {}",
            summary.headline
        );
    }
    Ok(summary)
}

async fn execute(
    argv: &[String],
    root: &Path,
    workdir: &Path,
    script_path: Option<PathBuf>,
    timeout_secs: u64,
) -> Result<(i32, u64, Vec<u8>, usize, Option<u64>), RunError> {
    let program = script_path
        .as_deref()
        .map_or_else(|| PathBuf::from(&argv[0]), Path::to_path_buf);
    let mut command = Command::new(program);
    command
        .args(&argv[1..])
        .current_dir(workdir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    child_env(&mut command);
    command.env("LEINDEX_RUN_PROJECT", root);
    #[cfg(unix)]
    command.process_group(0);

    let started = Instant::now();
    let deadline = started + Duration::from_secs(timeout_secs);
    let mut child = command
        .spawn()
        .map_err(|error| RunError::Spawn(format!("{}: {error}", argv[0])))?;
    // Capture the group leader's pid while the child is guaranteed alive:
    // `Child::id()` returns `None` once the child has been waited on, and
    // the timeout path below runs exactly then.
    #[cfg(unix)]
    let pgid = child.id().and_then(|pid| i32::try_from(pid).ok());
    #[cfg(not(unix))]
    let pgid: Option<i32> = None;
    // stdout and stderr are collected separately so a stderr chunk landing
    // between two chunks of one stdout record (Cargo's JSON lines) can never
    // corrupt that record. The stored log is stdout followed by stderr.
    let stdout_sink = Arc::new(Mutex::new(Sink::default()));
    let stderr_sink = Arc::new(Mutex::new(Sink::default()));
    let mut pumps = Vec::new();
    if let Some(pipe) = child.stdout.take() {
        pumps.push(tokio::spawn(pump(pipe, stdout_sink.clone())));
    }
    if let Some(pipe) = child.stderr.take() {
        pumps.push(tokio::spawn(pump(pipe, stderr_sink.clone())));
    }

    // One deadline for the child AND the pipes: a descendant that outlives
    // the child and keeps the pipes open would otherwise block forever.
    let remaining = deadline.saturating_duration_since(Instant::now());
    let finished = tokio::time::timeout(remaining, async {
        let status = child.wait().await;
        for task in &mut pumps {
            let _ = task.await;
        }
        status
    })
    .await;

    let (exit_code, timed_out) = match finished {
        Ok(Ok(status)) => (status.code().unwrap_or(-1), None),
        Ok(Err(error)) => return Err(RunError::Spawn(error.to_string())),
        Err(_) => {
            kill_tree(&mut child, pgid);
            let _ = child.wait().await;
            for task in &pumps {
                task.abort();
            }
            (-1, Some(timeout_secs))
        }
    };
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let mut stdout = stdout_sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let stderr = stderr_sink
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let dropped = stdout.dropped + stderr.dropped;
    let mut raw = std::mem::take(&mut stdout.bytes);
    raw.extend_from_slice(&stderr.bytes);
    Ok((exit_code, duration_ms, raw, dropped, timed_out))
}
