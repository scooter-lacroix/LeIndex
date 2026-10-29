//! File identity for the text-search freshness check.
//!
//! Extracted from `engine.rs` by pure code motion (Large File Detection gate:
//! `engine.rs` crossed 2000 lines). Re-exported from `engine` so
//! `engine::FileIdentity` keeps its path.

use std::time::UNIX_EPOCH;

/// The freshness identity recorded at index time and re-read at search time.
///
/// Size + mtime alone miss same-size edits that preserve mtime (metadata-
/// preserving copies, coarse-resolution filesystems): the stale trigram plan
/// then excludes the edited file and the live-content verifier never reads
/// it, so results introduced by the edit stay missing until a rebuild. ctime
/// (inode change time) advances on any content write and ino changes on
/// replace-by-rename; both are compared whenever the platform provides them
/// (0 elsewhere, where the check degrades to the old size/mtime behavior).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// Size in bytes.
    pub size: u64,
    /// Modification time (ns since epoch).
    pub mtime_ns: i64,
    /// ctime (unix) or creation time (windows), ns since epoch; 0 if
    /// unavailable. NANOSECOND resolution: second-granular ctime misses an
    /// in-place same-length edit whose mtime is restored within the same
    /// clock second (only ctime_nsec moved), which the stale plan then
    /// excluded forever.
    pub ctime_ns: i64,
    /// Inode number (unix); 0 elsewhere.
    pub ino: u64,
}

pub(super) fn identity(meta: &std::fs::Metadata) -> FileIdentity {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64);
    let (ctime_ns, ino) = platform_identity(meta);
    FileIdentity {
        size: meta.len(),
        mtime_ns: mtime,
        ctime_ns,
        ino,
    }
}

#[cfg(unix)]
fn platform_identity(meta: &std::fs::Metadata) -> (i64, u64) {
    use std::os::unix::fs::MetadataExt;
    (meta.ctime_nsec(), meta.ino())
}

#[cfg(windows)]
fn platform_identity(meta: &std::fs::Metadata) -> (i64, u64) {
    let created = meta
        .created()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos().min(i64::MAX as u128) as i64);
    (created, 0)
}

#[cfg(not(any(unix, windows)))]
fn platform_identity(_meta: &std::fs::Metadata) -> (i64, u64) {
    (0, 0)
}
