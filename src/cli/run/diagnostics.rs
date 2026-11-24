//! Scoped checks and the last-run diagnostics store (§4.4, cross-lane
//! contract for `validate:"check"` and the §6.6 read overlay).
//!
//! `check_scoped` runs the check that owns a file (`cargo check -p <pkg>` or
//! `npx tsc --noEmit`) through the sandbox and persists the diagnostics as
//! JSON in `<storage>/logs/last-diagnostics.json`. `diagnostics_for` reads
//! that store back for a file and line range.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use super::parsers::Summary;
use super::run_log;
use super::sandbox::{self, RunRequest};

/// Diagnostic severity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Severity {
    /// A compile/type error.
    Error,
    /// A warning or lint.
    Warning,
}

/// One diagnostic from a build or check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Diagnostic {
    /// Absolute path of the file.
    pub file: PathBuf,
    /// 1-based line.
    pub line: usize,
    /// Error or warning.
    pub severity: Severity,
    /// Lint or error code (`E0308`, `clippy::needless_borrow`, `TS2322`).
    pub code: Option<String>,
    /// Human message.
    pub message: String,
}

/// File name of the persisted store, inside the logs directory (retention
/// only removes `*.log`, so it survives log rotation).
const STORE_FILE: &str = "last-diagnostics.json";

#[derive(Serialize, Deserialize)]
struct Store {
    checked_at_unix_ms: u128,
    diagnostics: Vec<Diagnostic>,
}

/// Storage directory for a project, whether or not it is indexed.
pub(crate) fn storage_for(project_root: &Path) -> PathBuf {
    crate::cli::leindex::resolve_existing_storage_path(project_root)
        .unwrap_or_else(|| project_root.join(".leindex"))
}

fn store_path(storage: &Path) -> PathBuf {
    run_log::logs_dir(storage).join(STORE_FILE)
}

/// Replace the stored diagnostics (atomic: temp file + rename).
pub(crate) fn record(storage: &Path, diagnostics: &[Diagnostic]) -> Result<()> {
    let dir = run_log::logs_dir(storage);
    std::fs::create_dir_all(&dir).with_context(|| format!("create {}", dir.display()))?;
    let store = Store {
        checked_at_unix_ms: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis()),
        diagnostics: diagnostics.to_vec(),
    };
    // A private temporary file per writer: concurrent `record` calls must not
    // share one temp path (one writer's rename would invalidate another's).
    static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);
    let temp = dir.join(format!(
        "{STORE_FILE}.{pid}.{nanos}.{n}.tmp",
        pid = std::process::id(),
        nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos()),
        n = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::write(&temp, serde_json::to_vec(&store)?)
        .with_context(|| format!("write {}", temp.display()))?;
    if let Err(error) = std::fs::rename(&temp, store_path(storage)) {
        // Do not leave a stale temporary behind when the publish fails.
        let _ = std::fs::remove_file(&temp);
        return Err(error).context("publish diagnostics store");
    }
    Ok(())
}

/// The last run's stored diagnostics for `file` within `lines`; empty when
/// nothing is stored, the store is unreadable, or nothing matches.
pub(crate) fn diagnostics_for(
    project_root: &Path,
    file: &Path,
    lines: RangeInclusive<usize>,
) -> Vec<Diagnostic> {
    let Ok(raw) = std::fs::read(store_path(&storage_for(project_root))) else {
        return Vec::new();
    };
    let Ok(store) = serde_json::from_slice::<Store>(&raw) else {
        return Vec::new();
    };
    let wanted = absolute(project_root, file);
    store
        .diagnostics
        .into_iter()
        .filter(|d| d.file == wanted && lines.contains(&d.line))
        .collect()
}

fn absolute(project_root: &Path, file: &Path) -> PathBuf {
    if file.is_absolute() {
        file.to_path_buf()
    } else {
        project_root.join(file)
    }
}

/// How a file's owning package is checked.
#[derive(Debug, PartialEq, Eq)]
enum Scope {
    /// `cargo check -p <name>` run from `dir`, the directory that holds the
    /// `[package]` manifest. Cargo resolves the workspace from the working
    /// directory upward, so a member nested below the project root is only
    /// reachable when the check runs inside that member (or an ancestor
    /// that is itself a workspace root above it).
    ///
    /// `workspace_root` is the directory Cargo resolves that workspace to
    /// (see [`cargo_workspace_root`]). Cargo prints every diagnostic
    /// `file_name` relative to it — NOT to `dir` — so that is the base the
    /// paths are resolved against.
    Cargo {
        package: String,
        dir: PathBuf,
        workspace_root: PathBuf,
    },
    /// `npx tsc --noEmit` from this directory.
    Tsc(PathBuf),
}

/// Walk from the file's directory up to the project root; the nearest
/// directory with a `[package]` Cargo.toml or a package.json wins.
fn find_scope(project_root: &Path, file: &Path) -> Result<Scope> {
    let mut dir = file.parent().map(Path::to_path_buf);
    while let Some(current) = dir {
        if !current.starts_with(project_root) {
            break;
        }
        if let Ok(text) = std::fs::read_to_string(current.join("Cargo.toml")) {
            let table: toml::Table = text
                .parse()
                .with_context(|| format!("parse {}/Cargo.toml", current.display()))?;
            if let Some(name) = table
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(toml::Value::as_str)
            {
                return Ok(Scope::Cargo {
                    package: name.to_string(),
                    dir: current.clone(),
                    workspace_root: cargo_workspace_root(&current),
                });
            }
        }
        if current.join("package.json").is_file() {
            return Ok(Scope::Tsc(current));
        }
        if current == project_root {
            break;
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    Err(anyhow!(
        "unsupported: no Cargo.toml with [package] or package.json owns {} inside the project \
         (check_scoped supports cargo and tsc; pyright is not wired)",
        file.display()
    ))
}

/// The workspace root Cargo resolves a package directory to.
///
/// Cargo walks upward from the package directory and stops at the first
/// `Cargo.toml` whose `[workspace]` table claims it: a root with no
/// `members` list, or one whose `members`/`exclude` admit the package. A
/// `[workspace]` table inside the package itself (`members = []` or not
/// listing itself) therefore makes the package its own root — which is why
/// a stand-alone member prints `src/lib.rs` while a workspace member prints
/// `member/src/lib.rs`. Stops at `project_root` so a stray parent manifest
/// outside the project can never become the base.
///
/// Everything is decided from the manifest text, not `cargo metadata`, so
/// no second Cargo process is spawned for every scoped check.
fn cargo_workspace_root(member_dir: &Path) -> PathBuf {
    /// Does Cargo member pattern `pattern` list the project-relative path
    /// `rel`? Cargo accepts a literal path, `dir/*` (one level) and
    /// `dir/**` (any depth); the latter two are matched here without a glob
    /// dependency.
    fn lists(pattern: &str, rel: &str) -> bool {
        if let Some(prefix) = pattern.strip_suffix("/**") {
            return rel == prefix || rel.starts_with(&format!("{prefix}/"));
        }
        if let Some(prefix) = pattern.strip_suffix("/*") {
            return match rel.strip_prefix(&format!("{prefix}/")) {
                Some(rest) => !rest.contains('/'),
                None => false,
            };
        }
        pattern == rel
    }

    let mut current = Some(member_dir.to_path_buf());
    while let Some(dir) = current {
        let manifest = dir.join("Cargo.toml");
        if let Ok(text) = std::fs::read_to_string(&manifest)
            && let Ok(table) = text.parse::<toml::Table>()
            && let Some(workspace) = table.get("workspace")
        {
            let claims_root = match workspace.get("members").and_then(toml::Value::as_array) {
                // No `members` key: a root package or a virtual manifest
                // whose members are discovered implicitly. It is the root.
                None => true,
                Some(_) => {
                    let rel = member_dir
                        .strip_prefix(&dir)
                        .map(|r| r.to_string_lossy().replace('\\', "/"))
                        .unwrap_or_default();
                    let strs = |key: &str| -> Vec<String> {
                        workspace
                            .get(key)
                            .and_then(toml::Value::as_array)
                            .map(|xs| {
                                xs.iter()
                                    .filter_map(toml::Value::as_str)
                                    .map(str::to_string)
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    let listed = strs("members").iter().any(|g| lists(g, &rel));
                    let excluded = strs("exclude").iter().any(|g| lists(g, &rel));
                    listed && !excluded
                }
            };
            if claims_root {
                return dir;
            }
        }
        current = dir.parent().map(Path::to_path_buf);
    }
    // No workspace claims the package anywhere above it: Cargo still runs
    // it as its own root, so its directory is the base.
    member_dir.to_path_buf()
}

/// `cargo check` JSON lines to diagnostics. Messages without a span (the
/// "aborting due to" trailer) carry no location and are skipped. `file_name`
/// is relative to the workspace root Cargo resolved, so `base` is that root
/// (see [`cargo_workspace_root`]) — not the directory the check ran in.
pub(crate) fn parse_cargo_diagnostics(base: &Path, output: &str) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = Vec::new();
    for line in output.lines() {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value["reason"].as_str() != Some("compiler-message") {
            continue;
        }
        let message = &value["message"];
        let severity = match message["level"].as_str() {
            Some("error") => Severity::Error,
            Some("warning") => Severity::Warning,
            _ => continue,
        };
        let Some(spans) = message["spans"].as_array() else {
            continue;
        };
        let Some(span) = spans
            .iter()
            .find(|span| span["is_primary"].as_bool() == Some(true))
            .or_else(|| spans.first())
        else {
            continue;
        };
        let (Some(name), Some(line_start)) =
            (span["file_name"].as_str(), span["line_start"].as_u64())
        else {
            continue;
        };
        let diagnostic = Diagnostic {
            file: absolute(base, Path::new(name)),
            line: usize::try_from(line_start).unwrap_or(usize::MAX),
            severity,
            code: message["code"]["code"].as_str().map(str::to_string),
            message: message["message"].as_str().unwrap_or("").to_string(),
        };
        if !found.contains(&diagnostic) {
            found.push(diagnostic);
        }
    }
    found
}

/// `tsc` text (`path(line,col): error TSxxxx: message`) to diagnostics; paths
/// are relative to the directory tsc ran in.
pub(crate) fn parse_tsc_diagnostics(base: &Path, output: &str) -> Vec<Diagnostic> {
    let mut found: Vec<Diagnostic> = Vec::new();
    for line in output.lines() {
        let Some((location, rest)) = line.split_once("): ") else {
            continue;
        };
        let Some((path, position)) = location.rsplit_once('(') else {
            continue;
        };
        let Some(line_number) = position
            .split(',')
            .next()
            .and_then(|n| n.trim().parse::<usize>().ok())
        else {
            continue;
        };
        let (severity, rest) = if let Some(rest) = rest.strip_prefix("error ") {
            (Severity::Error, rest)
        } else if let Some(rest) = rest.strip_prefix("warning ") {
            (Severity::Warning, rest)
        } else {
            continue;
        };
        let Some((code, message)) = rest.split_once(": ") else {
            continue;
        };
        let diagnostic = Diagnostic {
            file: absolute(base, Path::new(path.trim())),
            line: line_number,
            severity,
            code: Some(code.trim().to_string()),
            message: message.trim().to_string(),
        };
        if !found.contains(&diagnostic) {
            found.push(diagnostic);
        }
    }
    found
}

/// Run the check that owns `file`, persist its diagnostics, and summarise.
/// At most `max` failures are listed; the headline states how many were cut.
pub(crate) fn check_scoped(project_root: &Path, file: &Path, max: usize) -> Result<Summary> {
    let root = std::fs::canonicalize(project_root)
        .with_context(|| format!("canonicalize {}", project_root.display()))?;
    let file = std::fs::canonicalize(absolute(&root, file))
        .with_context(|| format!("canonicalize {}", file.display()))?;
    if !file.starts_with(&root) {
        bail!("{} is outside the project", file.display());
    }
    let scope = find_scope(&root, &file)?;
    let (argv, cwd): (Vec<String>, Option<PathBuf>) = match &scope {
        Scope::Cargo {
            package,
            dir,
            workspace_root: _,
        } => (
            [
                "cargo",
                "check",
                "-p",
                package,
                // Plain `cargo check` selects only lib and bin targets, so a
                // file that belongs to a test target (or to `#[cfg(test)]`
                // code) would not be compiled and the check could report
                // success without ever looking at it. `--all-targets` also
                // builds tests, examples, benches and build scripts.
                "--all-targets",
                "--message-format=json",
            ]
            .map(String::from)
            .to_vec(),
            // Cargo searches upward from the working directory only; the
            // project root may not be the workspace this member belongs to.
            Some(dir.clone()),
        ),
        Scope::Tsc(dir) => (
            ["npx", "tsc", "--noEmit", "--pretty", "false"]
                .map(String::from)
                .to_vec(),
            Some(dir.clone()),
        ),
    };
    let storage = storage_for(&root);
    // No explicit log name: a fixed name would be shared by every scoped
    // check (and by a user run of the same name), so a concurrent check could
    // overwrite the log before it is parsed here. An anonymous name is
    // unique per invocation and the summary carries the exact path read back.
    let mut summary = run_blocking(&root, &storage, &argv, cwd.as_deref())?;

    let log_path = summary
        .log_path
        .clone()
        .ok_or_else(|| anyhow!("run produced no log"))?;
    let output = std::fs::read_to_string(&log_path)
        .with_context(|| format!("read {}", log_path.display()))?;
    let diagnostics = match &scope {
        // Cargo prints `file_name` relative to the WORKSPACE root it resolved
        // (which may be an ancestor of the member the check ran in), so that
        // is the base. `tsc` paths are relative to the cwd it ran in.
        Scope::Cargo { workspace_root, .. } => parse_cargo_diagnostics(workspace_root, &output),
        Scope::Tsc(dir) => parse_tsc_diagnostics(dir, &output),
    };
    record(&storage, &diagnostics)?;

    let total = summary.failures.len();
    if total > max {
        summary.failures.truncate(max);
        summary
            .headline
            .push_str(&format!(" (showing first {max} of {total} failures)"));
    }
    Ok(summary)
}

/// `sandbox::run` is async; run it on its own thread and runtime so this
/// works from sync and async callers alike (no `block_on` inside a runtime).
fn run_blocking(
    root: &Path,
    storage: &Path,
    argv: &[String],
    cwd: Option<&Path>,
) -> Result<Summary> {
    std::thread::scope(|scope| {
        scope
            .spawn(|| -> Result<Summary> {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("build runtime")?;
                runtime
                    .block_on(sandbox::run(RunRequest {
                        project_root: root,
                        storage,
                        argv: Some(argv),
                        command: None,
                        cwd,
                        timeout_secs: None,
                        // Anonymous: unique per invocation (see `check_scoped`).
                        name: None,
                    }))
                    .map_err(|error| anyhow!(error.to_string()))
            })
            .join()
            .map_err(|_| anyhow!("check thread panicked"))?
    })
}

#[cfg(test)]
#[path = "diagnostics_test.rs"]
mod diagnostics_tests;
