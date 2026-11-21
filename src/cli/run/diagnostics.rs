//! Scoped checks and the last-run diagnostics store (§4.4, cross-lane
//! contract for `validate:"check"` and the §6.6 read overlay).
//!
//! `check_scoped` runs the check that owns a file (`cargo check -p <pkg>` or
//! `npx tsc --noEmit`) through the sandbox and persists the diagnostics as
//! JSON in `<storage>/logs/last-diagnostics.json`. `diagnostics_for` reads
//! that store back for a file and line range.

use std::ops::RangeInclusive;
use std::path::{Path, PathBuf};
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
    let temp = dir.join(format!("{STORE_FILE}.tmp"));
    std::fs::write(&temp, serde_json::to_vec(&store)?)
        .with_context(|| format!("write {}", temp.display()))?;
    std::fs::rename(&temp, store_path(storage)).context("publish diagnostics store")
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
    /// `cargo check -p <name>`.
    Cargo(String),
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
                return Ok(Scope::Cargo(name.to_string()));
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

/// `cargo check` JSON lines to diagnostics. Messages without a span (the
/// "aborting due to" trailer) carry no location and are skipped.
pub(crate) fn parse_cargo_diagnostics(project_root: &Path, output: &str) -> Vec<Diagnostic> {
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
            file: absolute(project_root, Path::new(name)),
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
        Scope::Cargo(package) => (
            ["cargo", "check", "-p", package, "--message-format=json"]
                .map(String::from)
                .to_vec(),
            None,
        ),
        Scope::Tsc(dir) => (
            ["npx", "tsc", "--noEmit", "--pretty", "false"]
                .map(String::from)
                .to_vec(),
            Some(dir.clone()),
        ),
    };
    let storage = storage_for(&root);
    let mut summary = run_blocking(&root, &storage, &argv, cwd.as_deref())?;

    let log_path = summary
        .log_path
        .clone()
        .ok_or_else(|| anyhow!("run produced no log"))?;
    let output = std::fs::read_to_string(&log_path)
        .with_context(|| format!("read {}", log_path.display()))?;
    let diagnostics = match &scope {
        Scope::Cargo(_) => parse_cargo_diagnostics(&root, &output),
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
                        name: Some("check-scoped"),
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
