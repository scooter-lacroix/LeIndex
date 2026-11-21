//! On-disk log store for the `run` capability: `<storage>/logs/<name>.log`,
//! addressed by `log://<name>` handles, with count-based retention.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Hard cap on one stored log (8 MiB), truncation note included.
pub(crate) const MAX_LOG_BYTES: usize = 8 * 1024 * 1024;

/// Newest logs kept; older `*.log` files are deleted after each run.
pub(crate) const KEEP_LOGS: usize = 50;

/// URI scheme of a log handle.
const HANDLE_PREFIX: &str = "log://";

/// A log that a run writes to.
#[derive(Debug, Clone)]
pub(crate) struct StoredLog {
    /// Handle name (no scheme, no extension).
    pub name: String,
    /// Absolute path of the `.log` file.
    pub path: PathBuf,
}

/// `<storage>/logs`.
pub(crate) fn logs_dir(storage: &Path) -> PathBuf {
    storage.join("logs")
}

/// `log://<name>`.
pub(crate) fn handle(name: &str) -> String {
    format!("{HANDLE_PREFIX}{name}")
}

/// Accept `[A-Za-z0-9_-]{1,64}` only, so a name can never traverse out of the
/// logs directory.
pub(crate) fn sanitize_name(raw: &str) -> Option<String> {
    let valid = !raw.is_empty()
        && raw.len() <= 64
        && raw
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    valid.then(|| raw.to_string())
}

/// Create the logs directory and reserve the log for `name` (default
/// `run-<unix-ms>`). Returns an error for an invalid name.
pub(crate) fn prepare(storage: &Path, name: Option<&str>) -> io::Result<StoredLog> {
    let name = match name {
        Some(raw) => sanitize_name(raw).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("invalid log name '{raw}': use 1-64 chars of [A-Za-z0-9_-]"),
            )
        })?,
        None => {
            let millis = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            format!("run-{millis}")
        }
    };
    let dir = logs_dir(storage);
    std::fs::create_dir_all(&dir)?;
    let dir = std::fs::canonicalize(dir)?;
    Ok(StoredLog {
        path: dir.join(format!("{name}.log")),
        name,
    })
}

/// Read a stored log by `log://<name>` handle (or bare name).
pub(crate) fn read_by_handle(storage: &Path, handle: &str) -> io::Result<String> {
    let name = handle.strip_prefix(HANDLE_PREFIX).unwrap_or(handle);
    let name = sanitize_name(name).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid log handle '{handle}'"),
        )
    })?;
    std::fs::read_to_string(logs_dir(storage).join(format!("{name}.log")))
}

/// Delete all but the `keep` most recently modified `*.log` files.
pub(crate) fn enforce_retention(storage: &Path, keep: usize) -> io::Result<()> {
    let mut logs: Vec<(SystemTime, PathBuf)> = std::fs::read_dir(logs_dir(storage))?
        .filter_map(Result::ok)
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "log"))
        .filter_map(|entry| Some((entry.metadata().ok()?.modified().ok()?, entry.path())))
        .collect();
    logs.sort_by(|a, b| b.0.cmp(&a.0));
    for (_, path) in logs.into_iter().skip(keep) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
