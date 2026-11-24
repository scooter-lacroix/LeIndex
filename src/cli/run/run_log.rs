//! On-disk log store for the `run` capability: `<storage>/logs/<name>.log`,
//! addressed by `log://<name>` handles, with count-based retention.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
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
/// `run-<unix-ms>-<pid>-<n>`). Returns an error for an invalid name.
///
/// An explicit name is an address the caller chose: it replaces any earlier
/// log of that name. An anonymous name must be unique per call — two runs
/// that reach `prepare` in the same millisecond would otherwise share one
/// path and one `log://` handle, and the second `store_and_summarise` would
/// overwrite the first run's output. Generated names therefore carry the
/// process id and an atomic counter, and the file is reserved with
/// `create_new` so even two processes colliding on the same (millis, pid,
/// counter) triple cannot land on the same log.
pub(crate) fn prepare(storage: &Path, name: Option<&str>) -> io::Result<StoredLog> {
    let dir = logs_dir(storage);
    std::fs::create_dir_all(&dir)?;
    let dir = std::fs::canonicalize(dir)?;
    let Some(raw) = name else {
        return reserve_unique(&dir);
    };
    let name = sanitize_name(raw).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid log name '{raw}': use 1-64 chars of [A-Za-z0-9_-]"),
        )
    })?;
    Ok(StoredLog {
        path: dir.join(format!("{name}.log")),
    })
}

/// Allocate a log path no other run holds, creating the file exclusively.
/// A name that already exists is skipped; after a bounded number of
/// collisions this is an error, never a silent overwrite.
fn reserve_unique(dir: &Path) -> io::Result<StoredLog> {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    for _ in 0..1000 {
        let sequence = SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path = dir.join(format!(
            "run-{millis}-{pid}-{sequence}.log",
            pid = std::process::id()
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(_) => return Ok(StoredLog { path }),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not reserve a unique log name after 1000 attempts",
    ))
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
    logs.sort_by_key(|a| std::cmp::Reverse(a.0));
    for (_, path) in logs.into_iter().skip(keep) {
        std::fs::remove_file(path)?;
    }
    Ok(())
}
