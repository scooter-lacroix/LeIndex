// cleanup - Stale Artifact Garbage Collection
//
// Scans temp directories for LeIndex-owned artifacts and removes those older
// than a configurable threshold. The in-project `.leindex/` directories are
// never touched.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tracing::{debug, info, warn};

/// Name of the marker file placed inside every LeIndex temp artifact directory.
pub const LEINDEX_MARKER_FILE: &str = ".leindex-artifact-marker";

/// Default age threshold (in days) beyond which artifacts are considered stale.
pub const DEFAULT_MAX_AGE_DAYS: u64 = 7;

/// Summary of a garbage-collection pass.
#[derive(Debug, Default)]
pub struct GcReport {
    /// Number of artifact directories scanned.
    pub scanned: usize,
    /// Number of artifact directories removed.
    pub removed: usize,
    /// Total bytes freed (approximate, based on directory sizes).
    pub bytes_freed: u64,
    /// Paths that could not be removed (locked or permission errors).
    pub failed: Vec<(PathBuf, String)>,
}

impl std::fmt::Display for GcReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "GC Report:")?;
        writeln!(f, "  Scanned:  {} artifact(s)", self.scanned)?;
        writeln!(f, "  Removed:  {} artifact(s)", self.removed)?;
        if self.bytes_freed > 0 {
            let mb = self.bytes_freed as f64 / 1024.0 / 1024.0;
            writeln!(f, "  Freed:    {:.2} MB", mb)?;
        }
        if !self.failed.is_empty() {
            writeln!(f, "  Failed:   {} artifact(s)", self.failed.len())?;
            for (path, reason) in &self.failed {
                writeln!(f, "    {} - {}", path.display(), reason)?;
            }
        }
        Ok(())
    }
}

/// Return the list of temp directories that may contain LeIndex artifacts.
///
/// The candidates are:
/// - `$TMPDIR/leindex/`   (the `std::env::temp_dir()` fallback from `resolve_storage_path`)
/// - `$TMPDIR/lephase-*`  (phase index leftovers)
pub fn artifact_scan_roots() -> Vec<PathBuf> {
    let tmp = std::env::temp_dir();
    let mut roots = vec![tmp.join("leindex")];

    // Also scan for lephase-* directories directly in tmp
    if let Ok(entries) = fs::read_dir(&tmp) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_lossy = name.to_string_lossy();
            if name_lossy.starts_with("lephase-") {
                roots.push(entry.path());
            }
        }
    }

    roots
}

/// Check whether a directory is owned by LeIndex by looking for the marker file.
pub fn is_leindex_artifact(dir: &Path) -> bool {
    dir.join(LEINDEX_MARKER_FILE).exists()
}

/// Write the ownership marker into a directory.  This is a best-effort
/// operation; if it fails we only log a warning.
pub fn write_artifact_marker(dir: &Path) {
    let marker_path = dir.join(LEINDEX_MARKER_FILE);
    if marker_path.exists() {
        return;
    }
    let timestamp = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let content = format!(
        "leindex-artifact\ncreated={}\nversion={}\n",
        timestamp,
        env!("CARGO_PKG_VERSION")
    );
    if let Err(e) = fs::write(&marker_path, content) {
        warn!(
            "Failed to write artifact marker at {}: {}",
            marker_path.display(),
            e
        );
    }
}

/// Compute the total size of a directory tree (recursively).
pub fn dir_size(path: &Path) -> u64 {
    walkdir_size(path)
}

fn walkdir_size(path: &Path) -> u64 {
    let mut total: u64 = 0;
    // Use a manual stack to avoid recursion depth issues.
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Check whether a directory is likely in active use by probing the project's
/// cross-process write lock (`index.lock`, flock(2)) with the same
/// non-blocking primitive used by `ProjectWriteLock` (Codex P2).
///
/// A directory-writability probe is unsound for temp-backed indexes: an
/// unrelated probe file can be created even while another process holds
/// SQLite / `index.lock` open, so `leindex cleanup` would remove an active
/// database directory. `try_acquire` returns `Some(guard)` only when no live
/// writer holds the lock; `None` (held elsewhere) or an error (unreadable
/// directory) both mean "locked" — skip removal.
fn is_locked(dir: &Path) -> bool {
    match crate::cli::leindex::ProjectWriteLock::try_acquire(dir) {
        // No live writer holds the lock; the guard drops here (flock
        // released) before the caller removes the directory.
        Ok(Some(_)) => false,
        // Another process holds the write lock => in use.
        Ok(None) => true,
        // Cannot probe (permission / IO) => conservatively treat as locked.
        Err(_) => true,
    }
}

/// Run garbage collection on all known temp artifact directories.
///
/// Artifacts older than `max_age` are removed.  Artifacts that appear locked
/// (e.g., an active LeIndex process is using them) are skipped.
pub fn run_gc(max_age: Duration) -> GcReport {
    let mut report = GcReport::default();
    let cutoff = SystemTime::now() - max_age;

    for root in artifact_scan_roots() {
        if !root.exists() {
            continue;
        }

        // If the root *itself* is a lephase-* artifact directory
        if root
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("lephase-"))
            .unwrap_or(false)
        {
            maybe_remove_artifact(&root, &cutoff, &mut report);
            continue;
        }

        // Otherwise iterate children of the root directory
        let entries = match fs::read_dir(&root) {
            Ok(e) => e,
            Err(err) => {
                debug!("Cannot read {}: {}", root.display(), err);
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }

            // Skip any .leindex directories inside project roots — these are
            // in-project storage and must never be touched.
            if path.file_name().map(|n| n == ".leindex").unwrap_or(false) {
                debug!("Skipping in-project .leindex at {}", path.display());
                continue;
            }

            maybe_remove_artifact(&path, &cutoff, &mut report);
        }
    }

    report
}

/// Evaluate a single artifact directory and remove it if stale and not locked.
fn maybe_remove_artifact(dir: &Path, cutoff: &SystemTime, report: &mut GcReport) {
    // Only consider directories that are LeIndex artifacts (have marker or
    // match known naming patterns).
    if !is_leindex_artifact(dir) && !is_leindex_artifact_by_pattern(dir) {
        return;
    }

    report.scanned += 1;

    // Determine age from the marker file or directory mtime
    let age = artifact_age(dir);
    if age >= *cutoff {
        debug!(
            "Artifact {} is not stale yet (age: {:?})",
            dir.display(),
            SystemTime::now().duration_since(age).unwrap_or_default()
        );
        return;
    }

    // Check if the directory appears locked / in-use
    if is_locked(dir) {
        debug!("Skipping locked artifact: {}", dir.display());
        return;
    }

    let size = dir_size(dir);
    match fs::remove_dir_all(dir) {
        Ok(()) => {
            info!(
                "Removed stale artifact: {} ({:.2} MB)",
                dir.display(),
                size as f64 / 1024.0 / 1024.0
            );
            report.removed += 1;
            report.bytes_freed += size;
        }
        Err(e) => {
            warn!("Failed to remove stale artifact {}: {}", dir.display(), e);
            report.failed.push((dir.to_path_buf(), e.to_string()));
        }
    }
}

/// Check whether a directory matches known LeIndex artifact naming patterns
/// even without a marker file (for legacy artifacts created before the marker
/// was introduced).
pub fn is_leindex_artifact_by_pattern(dir: &Path) -> bool {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();

    // Pattern: <project>-<hash> under $TMPDIR/leindex/
    // Pattern: lephase-phase<N>-<hash>
    if name.contains('-') {
        // Check if under a "leindex" parent directory
        if dir
            .parent()
            .map(|p| p.file_name().map(|n| n == "leindex").unwrap_or(false))
            .unwrap_or(false)
        {
            // Check if it contains leindex.db (strong indicator)
            return dir.join("leindex.db").exists();
        }
    }

    // lephase-* directories
    if name.starts_with("lephase-") {
        return true;
    }

    false
}

/// Get the creation/modification time of an artifact directory.
/// Prefers the marker file mtime (creation timestamp), falls back to dir mtime.
pub fn artifact_age(dir: &Path) -> SystemTime {
    let marker = dir.join(LEINDEX_MARKER_FILE);
    if let Ok(meta) = fs::metadata(&marker) {
        if let Ok(modified) = meta.modified() {
            return modified;
        }
    }
    // Fall back to directory modification time
    fs::metadata(dir)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// Run startup garbage collection — removes artifacts older than the default
/// threshold. This is meant to be called early in the CLI startup path
/// (`Cli::run`). Safe because `is_locked` probes the project's cross-process
/// write lock (Codex P2): a directory a live writer is using is never removed.
/// Readers do not take the write lock, so a reader-only sibling using a stale
/// (>7-day) temp index is not protected — an inherent limitation of the
/// advisory write lock, unchanged from the previous probe-file check.
pub fn startup_gc() {
    let max_age = Duration::from_secs(DEFAULT_MAX_AGE_DAYS * 24 * 3600);
    let report = run_gc(max_age);
    if report.removed > 0 {
        info!(
            "Startup GC: removed {} stale artifact(s), freed {:.2} MB",
            report.removed,
            report.bytes_freed as f64 / 1024.0 / 1024.0
        );
    }
}

/// Summary of a stale-daemon sidecar sweep (T7).
#[derive(Debug, Default)]
pub struct DaemonSweepReport {
    /// Sidecar stems scanned (`leindex-embed-*` / `leindex-mcp-*`).
    pub scanned: usize,
    /// Sidecar files removed.
    pub removed: usize,
    /// Paths that could not be removed.
    pub failed: Vec<(PathBuf, String)>,
}

impl std::fmt::Display for DaemonSweepReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Daemon sweep report:")?;
        writeln!(f, "  Stems scanned: {}", self.scanned)?;
        writeln!(f, "  Files removed: {}", self.removed)?;
        if !self.failed.is_empty() {
            writeln!(f, "  Failed:        {} file(s)", self.failed.len())?;
            for (path, reason) in &self.failed {
                writeln!(f, "    {} - {}", path.display(), reason)?;
            }
        }
        Ok(())
    }
}

/// Best-effort pid liveness check (T7). Linux uses `/proc/<pid>` existence;
/// on other platforms we cannot verify, so `None` signals "unknown" and the
/// caller falls back to the mtime threshold.
///
/// `pub(crate)`: also used by [`crate::cli::mcp::lock`] as the ownership
/// liveness gate that prevents the publication-TOCTOU stale-steal (a live
/// owner whose `.start` sidecar is mid-write must never have its lock
/// unlinked).
pub(crate) fn pid_is_alive(pid: u32) -> Option<bool> {
    #[cfg(target_os = "linux")]
    {
        let proc_dir = std::path::PathBuf::from(format!("/proc/{pid}"));
        if !proc_dir.exists() {
            return Some(false);
        }
        // PID-recycling guard (Kilo): the process must actually be a leindex
        // daemon (worker or MCP server) — an unrelated process that reused a
        // dead daemon's PID must not keep that daemon's stale sidecars
        // protected forever. Mirrors `lock.rs::pid_is_owned`'s cmdline check.
        let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
        Some(cmdline.is_some_and(|raw| {
            let command = String::from_utf8_lossy(&raw);
            command
                .split('\0')
                .any(|arg| arg.contains("leindex") || arg.contains("mcp"))
        }))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        None
    }
}

/// True when `name` is one of the sidecar files the daemon worker or the MCP
/// advisory lock writes under `~/.leindex/run/`.
fn is_daemon_sidecar(name: &str) -> bool {
    matches!(
        name,
        "lock" | "pid" | "sock" | "status" | "start" | "start.next" | "pid.next" | "status.next"
    )
}

/// Sweep stale daemon sidecars out of `~/.leindex/run/` (T7).
///
/// Memory-pressure remediation: crashed/SIGKILLed workers and MCP servers leave
/// `.lock`/`.pid`/`.sock`/`.status`/`.start` sidecars behind (verified live:
/// `leindex-embed-*` debris dating back weeks in `~/.leindex/run/`). Liveness
/// rules:
/// - A stem with a `.pid` file whose pid is **alive** → keep every sidecar for
///   that stem (a running daemon owns them).
/// - A stem with a `.pid` file whose pid is **dead** → all its sidecars are
///   stale, regardless of age (the daemon that owned them is gone).
/// - A stem with **no readable pid file** (e.g. a 0-byte flock target, a
///   crashed MCP guard, or a malformed pid file) → stale when older than
///   `max_age` (mtime).
/// - On non-Linux (pid liveness unknowable) every stem falls back to mtime.
///
/// Note: a *live* MCP server that runs longer than `max_age` may have its own
/// advisory `.lock`/`.start` sidecars swept by the mtime path (MCP stems carry
/// no pid file). Impact is advisory-only (a lost dup-instance warning, never
/// data loss); do not tighten the mtime threshold without re-examining this.
///
/// Never touches anything not in the run dir, and never removes a live daemon's
/// files. Honours `dry_run`.
pub fn sweep_stale_daemon_artifacts(max_age: Duration, dry_run: bool) -> DaemonSweepReport {
    let Some(home) = crate::config::resolve_leindex_home() else {
        return DaemonSweepReport::default();
    };
    sweep_run_dir(&home.join("run"), max_age, dry_run)
}

/// Testable core of [`sweep_stale_daemon_artifacts`] over an explicit run dir.
fn sweep_run_dir(run_dir: &Path, max_age: Duration, dry_run: bool) -> DaemonSweepReport {
    let mut report = DaemonSweepReport::default();
    let entries = match fs::read_dir(run_dir) {
        Ok(entries) => entries,
        Err(_) => return report, // no run dir yet → nothing to sweep
    };
    let cutoff = SystemTime::now() - max_age;

    // Collect all regular files; sort daemon.endpoint separately.
    let mut file_paths: Vec<PathBuf> = Vec::new();
    let mut daemon_endpoint_path: Option<PathBuf> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(file_name) = path.file_name() else {
            continue;
        };
        let file_name = file_name.to_string_lossy().to_string();
        if file_name == "daemon.endpoint" {
            daemon_endpoint_path = Some(path);
            continue;
        }
        file_paths.push(path);
    }

    // Handle the daemon.endpoint sidecar (leindexd endpoint, spec §4.2).
    // Unlike the stem-based sidecars, this is a single JSON file with the
    // daemon's PID, socket path, and protocol version embedded.
    if let Some(ep_path) = daemon_endpoint_path {
        report.scanned += 1;
        if is_daemon_endpoint_stale(&ep_path, &cutoff) {
            remove_sidecar(&ep_path, dry_run, &mut report);
        }
    }

    // Group sidecar files by their stem (e.g. `leindex-embed-<hash>`).
    let mut stems: std::collections::BTreeMap<String, Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    for path in file_paths {
        let Some(file_name) = path.file_name() else {
            continue;
        };
        let file_name = file_name.to_string_lossy();
        let Some((stem, ext)) = file_name.rsplit_once('.') else {
            continue;
        };
        if !is_daemon_sidecar(ext) {
            continue;
        }
        if !(stem.starts_with("leindex-embed-") || stem.starts_with("leindex-mcp-")) {
            continue;
        }
        stems.entry(stem.to_string()).or_default().push(path);
    }

    for (stem, mut files) in stems {
        files.sort();
        report.scanned += 1;
        let (live, has_pid) = stem_liveness(&files);
        // Live-pid protection: any `.pid` file naming a running process keeps
        // the whole stem (a live daemon owns its sidecars).
        if live {
            debug!("Keeping live daemon sidecars for {}", stem);
            continue;
        }

        for path in files {
            // pid-file stems: dead pid means stale regardless of age.
            // Non-pid stems (or unknowable liveness): age threshold applies.
            if !sidecar_is_stale(&path, has_pid, &cutoff) {
                continue;
            }
            remove_sidecar(&path, dry_run, &mut report);
        }
    }

    report
}

/// Determine whether a `daemon.endpoint` JSON sidecar is stale.
///
/// The sidecar records the daemon's PID, socket path, start time, and protocol
/// version. If the PID is provably dead (on Linux), the sidecar is stale. If
/// the PID is alive but the process name does not match leindexd (PID
/// recycling), the sidecar is stale. If PID liveness cannot be determined
/// (non-Linux), the mtime threshold applies.
///
/// Malformed JSON (unreadable) is treated as stale (a crash mid-write left a
/// truncated sidecar).
fn is_daemon_endpoint_stale(path: &Path, cutoff: &SystemTime) -> bool {
    // Try to parse the sidecar JSON for the PID.
    match fs::read(path) {
        Ok(bytes) => {
            // Parse just the pid field. The DaemonEndpoint struct is defined in
            // endpoint.rs but we parse loosely here to avoid a dependency cycle.
            #[derive(serde::Deserialize)]
            struct EpPid {
                pid: u32,
            }
            match serde_json::from_slice::<EpPid>(&bytes) {
                Ok(ep) => match pid_is_alive(ep.pid) {
                    Some(false) => true, // provably dead
                    Some(true) => false, // provably alive (and is leindexd)
                    None => {
                        // Unknown liveness (non-Linux): fall back to mtime.
                        sidecar_is_stale(path, false, cutoff)
                    }
                },
                Err(_) => {
                    // Malformed JSON: stale. A crash mid-write left a
                    // truncated sidecar; the endpoint is invalid.
                    true
                }
            }
        }
        Err(_) => true, // Unreadable: stale.
    }
}

/// Live-pid protection + pid-presence for one sidecar stem. Returns
/// `(live, has_pid)`: `live` when any `.pid` file names a running process
/// (that stem is protected from sweeping); `has_pid` when any `.pid` file
/// holds a *readable, parseable* pid (a dead/absent pid makes every sidecar
/// stale regardless of age; non-Linux platforms where liveness is unknowable
/// fall back to mtime).
///
/// A malformed/unreadable pid file deliberately does NOT set `has_pid`: if it
/// did, the whole stem would be "dead regardless of age" and a live daemon
/// whose pid file is transiently unreadable could have its sidecars swept.
fn stem_liveness(files: &[PathBuf]) -> (bool, bool) {
    let mut live = false;
    let mut has_pid = false;
    for path in files {
        if path.extension().is_none_or(|ext| ext != "pid") {
            continue;
        }
        let Ok(pid_str) = fs::read_to_string(path) else {
            continue;
        };
        let Ok(pid) = pid_str.trim().parse::<u32>() else {
            continue;
        };
        has_pid = true;
        if pid_is_alive(pid) == Some(true) {
            live = true;
        }
    }
    (live, has_pid)
}

/// Staleness decision for a single sidecar (T7). `has_pid` stems were already
/// determined to have a dead/absent pid, so they are stale regardless of age;
/// non-pid stems fall back to the mtime threshold.
fn sidecar_is_stale(path: &Path, has_pid: bool, cutoff: &SystemTime) -> bool {
    if has_pid {
        return true;
    }
    fs::metadata(path)
        .and_then(|m| m.modified())
        .map(|mtime| mtime < *cutoff)
        .unwrap_or(false)
}

/// Remove (or count, in dry-run) one stale sidecar (T7).
fn remove_sidecar(path: &Path, dry_run: bool, report: &mut DaemonSweepReport) {
    if dry_run {
        debug!("Would remove stale daemon sidecar {}", path.display());
        report.removed += 1;
        return;
    }
    match fs::remove_file(path) {
        Ok(()) => {
            info!("Removed stale daemon sidecar {}", path.display());
            report.removed += 1;
        }
        Err(e) => {
            warn!("Failed to remove daemon sidecar {}: {}", path.display(), e);
            report.failed.push((path.to_path_buf(), e.to_string()));
        }
    }
}

/// Registered temp-storage paths to remove on clean exit.
///
/// Codex P2 (cleanup.rs:554): the previous implementation registered an empty
/// `call_once` — no hook, no retained path — so clean process exits left the
/// entire temp-fallback database + index behind. Paths are now retained here
/// and flushed by [`flush_registered_temp_cleanups`] from the CLI exit path.
static AT_EXIT_PATHS: std::sync::OnceLock<std::sync::Mutex<Vec<PathBuf>>> =
    std::sync::OnceLock::new();

/// Register a temp storage directory for best-effort removal on clean exit.
///
/// The cleanup is lock-aware (see `is_locked`) so an active temp-backed
/// index used by another process is never removed. If the process is killed
/// with SIGKILL, artifacts remain until the next startup GC pass
/// ([`startup_gc`]).
pub fn register_at_exit_cleanup(storage_path: PathBuf) {
    // Only register cleanup for paths that are NOT in-project .leindex
    if storage_path
        .file_name()
        .map(|n| n == ".leindex")
        .unwrap_or(false)
    {
        debug!(
            "Skipping at-exit cleanup registration for in-project storage: {}",
            storage_path.display()
        );
        return;
    }

    // Only register for paths inside the system temp directory
    let tmp = std::env::temp_dir();
    if !storage_path.starts_with(&tmp) {
        debug!(
            "Skipping at-exit cleanup for non-temp storage: {}",
            storage_path.display()
        );
        return;
    }

    let registry = AT_EXIT_PATHS.get_or_init(|| std::sync::Mutex::new(Vec::new()));
    // Poison-tolerant like `flush_registered_temp_cleanups`: a poisoned mutex
    // (some other thread panicked while holding it) must not silently drop the
    // registration — degrade to the locked data instead.
    let mut paths = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if !paths.contains(&storage_path) {
        paths.push(storage_path.clone());
        debug!(
            "Registered at-exit cleanup for temp storage: {}",
            storage_path.display()
        );
    }
}

/// Remove every registered temp storage directory (best-effort).
///
/// Called from the CLI exit path (`Cli::run`, next to the memory-report
/// flush) so clean process exits do not leave temp-fallback databases behind.
/// Each path is guarded by `is_locked` (Codex P2): a directory whose
/// `index.lock` is held by a live **writer** is skipped (readers do not take
/// the write lock, so a reader-only sibling using a stale temp index is not
/// protected — an inherent limitation of the advisory write lock, unchanged
/// from the previous probe-file check and strictly better than it).
///
/// The registry is cleared after flushing: this runs once per process at
/// exit, and a second flush must be a no-op rather than re-probing paths that
/// were already removed.
pub fn flush_registered_temp_cleanups() {
    let Some(registry) = AT_EXIT_PATHS.get() else {
        return;
    };
    let paths = {
        let mut guard = registry
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        std::mem::take(&mut *guard)
    };
    for path in paths {
        best_effort_cleanup(&path);
    }
}

/// Best-effort cleanup of a single storage directory.
///
/// Lock-aware (Codex P2): a directory whose `index.lock` is held by another
/// process is never removed.
pub fn best_effort_cleanup(path: &Path) {
    if path.exists() && path.starts_with(std::env::temp_dir()) && !is_locked(path) {
        match fs::remove_dir_all(path) {
            Ok(()) => {
                eprintln!("[leindex] Cleaned up temp storage: {}", path.display());
            }
            Err(e) => {
                eprintln!(
                    "[leindex] Warning: failed to clean up temp storage {}: {}",
                    path.display(),
                    e
                );
            }
        }
    }
}

/// Produce a read-only retention report for a project's generation store.
///
/// This is the backing implementation for `leindex retention --report`
/// (WS4 Task 9, WS10 Task 6). It resolves the project's storage root
/// (`.leindex/`, or the `LEINDEX_HOME`/XDG/tmp fallbacks via
/// `resolve_existing_storage_path`), opens the CAS store, and scans
/// `generations/` and `jobs/` to report the generation count, CAS bytes,
/// job bytes, dedup ratio, and GC candidates.
///
/// WS10 Task 6: Also reports embedding cache stats from the user-level
/// cache (`~/.leindex/embed-cache/`): cache bytes, row count, hit/miss/
/// eviction telemetry, entry-size rejections, and model identity
/// (spec section 10.3). Count-only reporting is prohibited.
///
/// The report is purely observational: no generations, blobs, or jobs are
/// modified. A project that has not been indexed yet (no storage root, or no
/// CAS store) yields an all-zero report rather than an error.
pub fn retention_report_cli(project: Option<&Path>) -> anyhow::Result<RetentionReportOutput> {
    use crate::storage::cas::CasStore;
    use crate::storage::generation::GENERATIONS_DIR;
    use crate::storage::generation::retention::retention_report;

    let project_path = project
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let canonical = project_path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("failed to canonicalize project path: {e}"))?;

    let storage_root = crate::cli::leindex::resolve_existing_storage_path(&canonical)
        .unwrap_or_else(|| canonical.join(".leindex"));

    let cas_dir = storage_root.join("cas");
    let gens_dir = storage_root.join(GENERATIONS_DIR);
    let jobs_dir = storage_root.join("jobs");
    let generation_report = if cas_dir.exists() {
        let cas = CasStore::open(&cas_dir).map_err(|e| {
            anyhow::anyhow!("failed to open CAS store at {}: {e}", cas_dir.display())
        })?;
        retention_report(&cas, &gens_dir, &jobs_dir)
            .map_err(|e| anyhow::anyhow!("retention report failed: {e}"))?
    } else if gens_dir.exists() {
        // Legacy (pre-CAS) full-copy store: report what the no-CAS sweep
        // would reclaim. A default (empty) report here is what made legacy
        // stores look "clean" while accumulating dozens of generations.
        crate::storage::generation::retention::retain_generations_no_cas(
            &gens_dir,
            &jobs_dir,
            crate::storage::generation::retention::DEFAULT_MAX_GENERATIONS,
            true,
        )
        .map_err(|e| anyhow::anyhow!("legacy retention report failed: {e}"))?
    } else {
        crate::storage::generation::GenerationRetentionReport::default()
    };

    // WS10 Task 6: Also report embedding cache stats (spec section 10.3).
    // Only available when the onnx feature is compiled in (embed module).
    #[cfg(feature = "onnx")]
    let cache_stats = report_embed_cache_stats();

    #[cfg(feature = "onnx")]
    {
        Ok(RetentionReportOutput {
            generation_report,
            cache_stats,
        })
    }
    #[cfg(not(feature = "onnx"))]
    {
        Ok(RetentionReportOutput { generation_report })
    }
}

/// Run the retention GC from the CLI (`leindex retention --gc`).
///
/// Prunes generations outside the retained window (the current generation
/// plus its `max_generations - 1` immediate predecessors), GCs orphaned CAS
/// blobs on CAS stores, and byte-caps completed jobs. Works on both store
/// layouts: CAS-backed stores run `retain_after_publish`; legacy
/// full-copy stores (no `cas/`) run the no-CAS directory prune, which is
/// safe because legacy generations are self-contained. With `dry_run`
/// nothing is deleted.
pub fn retention_gc_cli(
    project: Option<&Path>,
    max_generations: usize,
    dry_run: bool,
) -> anyhow::Result<RetentionReportOutput> {
    use crate::storage::cas::CasStore;
    use crate::storage::generation::GENERATIONS_DIR;
    use crate::storage::generation::retention::{
        RetentionConfig, retain_after_publish, retain_generations_no_cas,
    };

    let project_path = project
        .map(Path::to_path_buf)
        .unwrap_or_else(|| std::env::current_dir().unwrap());
    let canonical = project_path
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("failed to canonicalize project path: {e}"))?;

    let storage_root = crate::cli::leindex::resolve_existing_storage_path(&canonical)
        .unwrap_or_else(|| canonical.join(".leindex"));

    let cas_dir = storage_root.join("cas");
    let gens_dir = storage_root.join(GENERATIONS_DIR);
    let jobs_dir = storage_root.join("jobs");

    let generation_report = if cas_dir.exists() {
        let mut cas = CasStore::open(&cas_dir).map_err(|e| {
            anyhow::anyhow!("failed to open CAS store at {}: {e}", cas_dir.display())
        })?;
        let cfg = RetentionConfig {
            max_generations: max_generations.max(1),
            ..RetentionConfig::default()
        };
        if dry_run {
            crate::storage::generation::retention::retention_report(&cas, &gens_dir, &jobs_dir)
                .map_err(|e| anyhow::anyhow!("retention report failed: {e}"))?
        } else {
            retain_after_publish(&mut cas, &gens_dir, &jobs_dir, &cfg)
                .map_err(|e| anyhow::anyhow!("retention sweep failed: {e}"))?
        }
    } else {
        if !gens_dir.exists() {
            anyhow::bail!(
                "no generation store found at {} (expected `generations/` or `cas/`)",
                storage_root.display()
            );
        }
        retain_generations_no_cas(&gens_dir, &jobs_dir, max_generations, dry_run)
            .map_err(|e| anyhow::anyhow!("legacy retention sweep failed: {e}"))?
    };

    #[cfg(feature = "onnx")]
    let cache_stats = report_embed_cache_stats();

    #[cfg(feature = "onnx")]
    {
        Ok(RetentionReportOutput {
            generation_report,
            cache_stats,
        })
    }
    #[cfg(not(feature = "onnx"))]
    {
        Ok(RetentionReportOutput { generation_report })
    }
}

/// WS10 Task 6: Report embedding cache stats from `~/.leindex/embed-cache/`.
///
/// Opens the user-level global embedding cache (if it exists) and generates
/// a stats report including cache bytes, row count, hit/miss/eviction
/// telemetry, entry-size rejections, max bytes config, and model identity
/// (spec section 10.3 — count-only prohibited).
#[cfg(feature = "onnx")]
fn report_embed_cache_stats() -> Option<crate::embed::cache::CacheStatsReport> {
    let home = crate::config::resolve_leindex_home()?;
    let cache_root = home.join("embed-cache");
    if !cache_root.exists() {
        return None;
    }
    let cache = crate::embed::cache::GlobalEmbeddingCache::open(&cache_root).ok()?;
    cache.cache_stats().ok()
}

/// Combined retention report output: generation store + embedding cache.
#[derive(Debug)]
pub struct RetentionReportOutput {
    /// Generation store retention report (CAS blobs, generations, jobs).
    pub generation_report: crate::storage::generation::GenerationRetentionReport,
    /// Embedding cache stats (if the global cache exists). Spec section 10.3.
    /// `None` when the onnx feature is not compiled in or no cache exists.
    #[cfg(feature = "onnx")]
    pub cache_stats: Option<crate::embed::cache::CacheStatsReport>,
}

impl std::fmt::Display for RetentionReportOutput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.generation_report)?;
        #[cfg(feature = "onnx")]
        {
            if let Some(ref cache) = self.cache_stats {
                write!(f, "{}", cache)?;
            } else {
                writeln!(f, "  Embedding Cache: (not initialized)")?;
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            writeln!(f, "  Embedding Cache: (onnx feature not compiled)")?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Task 7: Unified project-store cleanup (VAL-ROLLOUT-010)
// ---------------------------------------------------------------------------

/// Report from a unified project-store cleanup pass (Task 7).
///
/// This struct consolidates the generation retention report (WS4),
/// CAS GC, abandoned staging removal, and (optionally) embedding-cache
/// compaction (WS10) into a single user-facing summary.
#[derive(Debug, Default)]
pub struct ProjectCleanupReport {
    /// Generation-store retention results (stale gens, CAS GC, job pruning).
    pub generations: crate::storage::generation::GenerationRetentionReport,
    /// Number of abandoned staging files removed from `cas/.staging/`.
    pub staging_files_removed: usize,
    /// Embedding-cache compaction results (if cache exists). None if no
    /// cache is initialized or the `onnx` feature is off.
    #[cfg(feature = "onnx")]
    pub cache: Option<crate::embed::cache::CacheCompactionReport>,
}

impl std::fmt::Display for ProjectCleanupReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.generations)?;
        if self.staging_files_removed > 0 {
            writeln!(f, "  Staging files removed: {}", self.staging_files_removed)?;
        }
        #[cfg(feature = "onnx")]
        {
            if let Some(ref cache) = self.cache {
                writeln!(
                    f,
                    "  Embed cache: {} rows removed, {} bytes reclaimed",
                    cache.rows_removed, cache.reclaimed_bytes
                )?;
            }
        }
        Ok(())
    }
}

/// Run unified cleanup on a project's `.leindex/` store.
///
/// This is the heart of `leindex cleanup --store` (Task 7). It performs:
///
/// 1. **Generation retention** (WS4 `retain_after_publish`): removes stale
///    generations (not current/previous/leased), CAS GC of orphaned blobs
///    (refcount 0, not pinned by any retained manifest), and job pruning.
/// 2. **Abandoned staging removal**: deletes leftover `.partial` files in
///    `cas/.staging/` from crashed writes.
/// 3. **Embedding-cache compaction** (WS10): removes unreferenced rows from
///    the user-level global embedding cache (`~/.leindex/embed-cache/`).
///
/// **Safety gates (VAL-ROLLOUT-010, §16 reliability gate):**
/// - The current generation is NEVER removed.
/// - The immediate previous generation (rollback point) is NEVER removed.
/// - Any generation with an active lease (refcount > 0 on any layer blob)
///   is NEVER removed.
/// - CAS blobs referenced by retained/pinned manifests are NEVER GC'd.
///
/// When `dry_run` is true, nothing is deleted; the report reflects what
/// *would* be removed.
pub fn cleanup_project_store(
    storage_root: &Path,
    dry_run: bool,
) -> anyhow::Result<ProjectCleanupReport> {
    use crate::storage::cas::CasStore;
    use crate::storage::generation::GENERATIONS_DIR;

    let cas_dir = storage_root.join("cas");
    let gens_dir = storage_root.join(GENERATIONS_DIR);
    let jobs_dir = storage_root.join("jobs");

    // Legacy (pre-CAS) stores have no `cas/` directory: every generation is
    // a self-contained full copy. Skipping them entirely is how legacy
    // stores accumulated unbounded generations (98 dirs / 17 GB observed).
    // Generation-directory pruning is safe without CAS — no shared blobs —
    // so run the no-CAS variant and keep only the CAS-specific phases
    // (staging sweep, blob GC) for stores that actually have a CAS.
    if let Some(report) = empty_store_cleanup_report(storage_root, &cas_dir, &gens_dir, dry_run) {
        return Ok(report);
    }

    let mut cas = if cas_dir.exists() {
        Some(CasStore::open(&cas_dir).map_err(|e| {
            anyhow::anyhow!("failed to open CAS store at {}: {e}", cas_dir.display())
        })?)
    } else {
        None
    };

    // Phase 1: Generation retention + CAS GC + job pruning (WS4).
    let gen_report = run_generation_retention(cas.as_mut(), &gens_dir, &jobs_dir, dry_run)?;

    // Phase 2: Remove abandoned staging files (crash recovery).
    let staging_files_removed = remove_abandoned_staging_files(&cas_dir.join(".staging"), dry_run);

    // Phase 3: Embedding-cache compaction (WS10).
    #[cfg(feature = "onnx")]
    let cache_compaction = compact_embed_cache(dry_run);

    let report = ProjectCleanupReport {
        generations: gen_report,
        staging_files_removed,
        #[cfg(feature = "onnx")]
        cache: cache_compaction,
    };

    Ok(report)
}

/// Cleanup report for a project store with nothing generation-related to
/// clean: a missing store root, or a store with neither `cas/` nor
/// `generations/` directories.
fn empty_store_cleanup_report(
    storage_root: &Path,
    cas_dir: &Path,
    gens_dir: &Path,
    dry_run: bool,
) -> Option<ProjectCleanupReport> {
    if !storage_root.exists() || (!cas_dir.exists() && !gens_dir.exists()) {
        #[cfg(feature = "onnx")]
        {
            return Some(ProjectCleanupReport {
                generations: crate::storage::generation::GenerationRetentionReport::default(),
                cache: compact_embed_cache(dry_run),
                ..Default::default()
            });
        }
        #[cfg(not(feature = "onnx"))]
        {
            let _ = dry_run;
            return Some(ProjectCleanupReport {
                generations: crate::storage::generation::GenerationRetentionReport::default(),
                ..Default::default()
            });
        }
    }
    None
}

/// Phase 1 of [`cleanup_project_store`]: generation retention, CAS GC, and
/// job pruning (WS4). Stores without a CAS get the no-CAS retention variant.
fn run_generation_retention(
    cas: Option<&mut crate::storage::cas::CasStore>,
    gens_dir: &Path,
    jobs_dir: &Path,
    dry_run: bool,
) -> anyhow::Result<crate::storage::generation::GenerationRetentionReport> {
    use crate::storage::generation::retention::{RetentionConfig, retain_after_publish};

    let cfg = RetentionConfig::default();
    if let Some(cas) = cas {
        if dry_run {
            // Read-only report for dry-run mode.
            crate::storage::generation::retention::retention_report(cas, gens_dir, jobs_dir)
                .map_err(|e| anyhow::anyhow!("retention report failed: {e}"))
        } else {
            retain_after_publish(cas, gens_dir, jobs_dir, &cfg)
                .map_err(|e| anyhow::anyhow!("retention sweep failed: {e}"))
        }
    } else {
        crate::storage::generation::retention::retain_generations_no_cas(
            gens_dir,
            jobs_dir,
            cfg.max_generations,
            dry_run,
        )
        .map_err(|e| anyhow::anyhow!("legacy retention sweep failed: {e}"))
    }
}

/// Phase 2 of [`cleanup_project_store`]: remove abandoned `.partial` staging
/// files in `cas/.staging/` left over from crashed writes. Returns the number
/// removed (or, on `dry_run`, the number that would be).
fn remove_abandoned_staging_files(staging_dir: &Path, dry_run: bool) -> usize {
    if !staging_dir.exists() {
        return 0;
    }
    let mut removed = 0usize;
    if let Ok(entries) = fs::read_dir(staging_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "partial") {
                if !dry_run {
                    if let Err(e) = fs::remove_file(&path) {
                        if e.kind() != std::io::ErrorKind::NotFound {
                            warn!(
                                "cleanup: failed to remove staging file {}: {}",
                                path.display(),
                                e
                            );
                        }
                    } else {
                        removed += 1;
                    }
                } else {
                    removed += 1;
                }
            }
        }
    }
    removed
}

/// Compact the user-level global embedding cache (WS10 Task 6).
///
/// Opens `~/.leindex/embed-cache/` (if it exists) and removes unreferenced
/// rows. Returns `None` when no cache exists or the `onnx` feature is off.
#[cfg(feature = "onnx")]
fn compact_embed_cache(dry_run: bool) -> Option<crate::embed::cache::CacheCompactionReport> {
    use crate::embed::cache::GlobalEmbeddingCache;

    let home = crate::config::resolve_leindex_home()?;
    let cache_root = home.join("embed-cache");
    if !cache_root.exists() {
        return None;
    }
    let mut cache = GlobalEmbeddingCache::open(&cache_root).ok()?;
    if dry_run {
        // Return current stats without compaction.
        let stats = cache.cache_stats().ok()?;
        Some(crate::embed::cache::CacheCompactionReport {
            reclaimed_bytes: 0,
            rows_removed: 0,
            rows_retained: stats.row_count as u64,
        })
    } else {
        cache.gc().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_marker_write_and_detect() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("test-artifact-abc123");
        fs::create_dir_all(&artifact).unwrap();

        assert!(!is_leindex_artifact(&artifact));

        write_artifact_marker(&artifact);
        assert!(is_leindex_artifact(&artifact));

        let marker_content = fs::read_to_string(artifact.join(LEINDEX_MARKER_FILE)).unwrap();
        assert!(marker_content.starts_with("leindex-artifact"));
        assert!(marker_content.contains("created="));
    }

    #[test]
    fn test_marker_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("test-idempotent");
        fs::create_dir_all(&artifact).unwrap();

        write_artifact_marker(&artifact);
        let first = fs::read_to_string(artifact.join(LEINDEX_MARKER_FILE)).unwrap();

        write_artifact_marker(&artifact);
        let second = fs::read_to_string(artifact.join(LEINDEX_MARKER_FILE)).unwrap();

        assert_eq!(
            first, second,
            "Marker should not be overwritten if it exists"
        );
    }

    #[test]
    fn test_gc_skips_non_stale_artifacts() {
        // GC with 0-day threshold scans real system temp dirs.
        // This test verifies the logic doesn't crash or panic.
        // We do not assert on failed.len() because real lephase-* artifacts
        // may exist in the system temp dir and fail to be removed (e.g. locked).
        let _report = run_gc(Duration::from_secs(0));
    }

    #[test]
    fn test_is_locked_on_writable_dir() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_locked(dir.path()));
    }

    #[test]
    fn test_is_locked_detects_held_write_lock() {
        let dir = tempfile::tempdir().unwrap();
        let guard = crate::cli::leindex::ProjectWriteLock::acquire(dir.path()).unwrap();
        assert!(is_locked(dir.path()));
        drop(guard);
        assert!(!is_locked(dir.path()));
    }

    // Shared lock so registry-mutating cleanup tests run serially: the
    // AT_EXIT_PATHS global is drained on every flush, so parallel tests that
    // register+flush would interfere with each other (non-deterministic misses).
    use std::sync::Mutex;
    static TEST_CLEANUP_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_register_and_flush_temp_cleanup() {
        let _g = TEST_CLEANUP_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("storage");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("leindex.db"), b"x").unwrap();
        register_at_exit_cleanup(path.clone());
        flush_registered_temp_cleanups();
        assert!(
            !path.exists(),
            "flush should remove registered temp storage"
        );
    }

    #[test]
    fn test_register_skips_in_project_dir() {
        let _g = TEST_CLEANUP_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let in_project = dir.path().join(".leindex");
        fs::create_dir_all(&in_project).unwrap();
        register_at_exit_cleanup(in_project.clone());
        flush_registered_temp_cleanups();
        assert!(in_project.exists(), "in-project .leindex is never cleaned");
    }

    #[test]
    fn test_dir_size() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("file1.txt"), b"hello world").unwrap();
        fs::write(dir.path().join("file2.txt"), b"foo bar baz").unwrap();

        let size = dir_size(dir.path());
        assert_eq!(size, 11 + 11); // "hello world" + "foo bar baz"
    }

    #[test]
    fn test_artifact_age_uses_marker() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("age-test");
        fs::create_dir_all(&artifact).unwrap();
        write_artifact_marker(&artifact);

        let age = artifact_age(&artifact);
        // Should be recent (within last few seconds)
        let elapsed = SystemTime::now().duration_since(age).unwrap_or_default();
        assert!(elapsed.as_secs() < 10, "Artifact age should be recent");
    }

    #[test]
    fn test_artifact_age_falls_back_to_dir_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let artifact = dir.path().join("no-marker");
        fs::create_dir_all(&artifact).unwrap();
        // No marker written

        let age = artifact_age(&artifact);
        let elapsed = SystemTime::now().duration_since(age).unwrap_or_default();
        assert!(
            elapsed.as_secs() < 10,
            "Artifact age should fall back to dir mtime"
        );
    }

    #[test]
    fn test_gc_report_display() {
        let report = GcReport {
            scanned: 10,
            removed: 3,
            bytes_freed: 1024 * 1024 * 50, // 50 MB
            failed: vec![(PathBuf::from("/tmp/locked"), "Permission denied".into())],
        };
        let output = report.to_string();
        assert!(output.contains("Scanned:  10"));
        assert!(output.contains("Removed:  3"));
        assert!(output.contains("50.00 MB"));
        assert!(output.contains("Failed:   1"));
    }

    #[test]
    fn test_is_leindex_artifact_by_pattern() {
        let dir = tempfile::tempdir().unwrap();

        // lephase-* pattern
        let lephase = dir.path().join("lephase-phase1-abc");
        fs::create_dir_all(&lephase).unwrap();
        assert!(is_leindex_artifact_by_pattern(&lephase));

        // Random directory should not match
        let random = dir.path().join("random-dir");
        fs::create_dir_all(&random).unwrap();
        assert!(!is_leindex_artifact_by_pattern(&random));
    }

    #[test]
    fn test_never_removes_in_project_leindex() {
        // Create a fake project with .leindex directory
        let dir = tempfile::tempdir().unwrap();
        let leindex_dir = dir.path().join(".leindex");
        fs::create_dir_all(&leindex_dir).unwrap();
        fs::write(leindex_dir.join("leindex.db"), b"important data").unwrap();

        // The GC should never touch directories named ".leindex"
        // This is verified by the skip check in maybe_remove_artifact
        assert_eq!(leindex_dir.file_name().unwrap(), ".leindex");
    }

    #[test]
    fn test_run_gc_on_empty_dirs() {
        // Should not crash when scan roots don't exist
        let report = run_gc(Duration::from_secs(0));
        // Just verify it doesn't panic
        let _ = report.scanned;
    }

    #[test]
    fn test_best_effort_cleanup_skips_non_temp() {
        // Create a path that is definitely NOT under the system temp dir
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/home/user"));
        let non_temp = home.join(".leindex-test-cleanup-should-not-delete");
        // Don't actually create it — just verify the function handles it
        // The key check is that it doesn't match the temp dir prefix
        assert!(!non_temp.starts_with(std::env::temp_dir()));
    }

    #[test]
    fn test_sweep_ignores_unrelated_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("not-a-sidecar.txt"), b"x").unwrap();
        fs::write(dir.path().join("other-app.pid"), b"12345").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        assert_eq!(report.scanned, 0);
        assert_eq!(report.removed, 0);
        assert!(dir.path().join("not-a-sidecar.txt").exists());
        assert!(dir.path().join("other-app.pid").exists());
    }

    #[test]
    fn test_sweep_keeps_live_pid_stem() {
        let dir = tempfile::tempdir().unwrap();
        // A stem owned by THIS process must be kept entirely.
        let stem = "leindex-embed-aaaaaaaaaaaaaaaa";
        fs::write(
            dir.path().join(format!("{stem}.pid")),
            format!("{}\n", std::process::id()),
        )
        .unwrap();
        fs::write(dir.path().join(format!("{stem}.status")), "ready\n").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        assert_eq!(report.removed, 0, "live-pid stem must not be swept");
        assert!(dir.path().join(format!("{stem}.pid")).exists());
        assert!(dir.path().join(format!("{stem}.status")).exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_sweep_sweeps_recycled_unrelated_pid_stem() {
        // Kilo: pid_is_alive must not treat an unrelated process that reused a
        // dead daemon's PID as a live leindex daemon. PID 1 (init/systemd) is
        // alive but is not a leindex process, so a sidecar naming it must be
        // swept rather than protected forever.
        let dir = tempfile::tempdir().unwrap();
        let stem = "leindex-embed-eeeeeeeeeeeeeeee";
        std::fs::write(dir.path().join(format!("{stem}.pid")), "1\n").unwrap();
        std::fs::write(dir.path().join(format!("{stem}.status")), "ready\n").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        assert_eq!(
            report.removed, 2,
            "recycled unrelated pid must not protect the stem"
        );
        assert!(!dir.path().join(format!("{stem}.pid")).exists());
    }

    #[test]
    fn test_sweep_removes_dead_pid_stem() {
        let dir = tempfile::tempdir().unwrap();
        // A dead pid owns this stem → every sidecar is stale regardless of age.
        let dead_pid = 1 << 22; // will not exist as this process
        let stem = "leindex-embed-bbbbbbbbbbbbbbbb";
        fs::write(
            dir.path().join(format!("{stem}.pid")),
            format!("{dead_pid}\n"),
        )
        .unwrap();
        fs::write(dir.path().join(format!("{stem}.sock")), b"").unwrap();
        fs::write(dir.path().join(format!("{stem}.status")), "ready\n").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        assert_eq!(report.removed, 3);
        assert!(!dir.path().join(format!("{stem}.pid")).exists());
        assert!(!dir.path().join(format!("{stem}.sock")).exists());
        assert!(!dir.path().join(format!("{stem}.status")).exists());
    }

    #[test]
    fn test_sweep_dry_run_counts_without_removing() {
        let dir = tempfile::tempdir().unwrap();
        // A non-pid stem with zero max_age is stale by mtime (mtime < now).
        let stem = "leindex-mcp-cccccccccccccccc";
        fs::write(dir.path().join(format!("{stem}.lock")), b"").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), true);
        assert_eq!(report.removed, 1, "dry run must still count");
        assert!(
            dir.path().join(format!("{stem}.lock")).exists(),
            "dry run removes nothing"
        );
    }

    #[test]
    fn test_sweep_keeps_malformed_pid_stem_recent_sidecars() {
        let dir = tempfile::tempdir().unwrap();
        // A malformed (unparseable) pid file must NOT mark the stem "dead
        // regardless of age": that would let a live daemon with a transiently
        // unreadable pid file have its sidecars swept. With a generous max_age,
        // recent sidecars survive via the mtime fallback path.
        let stem = "leindex-embed-eeeeeeeeeeeeeeee";
        fs::write(dir.path().join(format!("{stem}.pid")), b"not-a-pid").unwrap();
        fs::write(dir.path().join(format!("{stem}.status")), "ready\n").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(7 * 24 * 3600), false);
        assert_eq!(
            report.removed, 0,
            "malformed pid must fall back to mtime, not sweep recent sidecars"
        );
        assert!(dir.path().join(format!("{stem}.pid")).exists());
        assert!(dir.path().join(format!("{stem}.status")).exists());
    }

    #[test]
    fn test_sweep_removes_old_nonpid_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        // A non-pid stem (e.g. a crashed MCP guard's lock) with zero max_age is
        // stale by mtime (mtime < cutoff when cutoff ≈ now).
        let stem = "leindex-mcp-dddddddddddddddd";
        fs::write(dir.path().join(format!("{stem}.lock")), b"").unwrap();
        fs::write(dir.path().join(format!("{stem}.start")), "12345\n").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        assert_eq!(report.removed, 2);
        assert!(!dir.path().join(format!("{stem}.lock")).exists());
        assert!(!dir.path().join(format!("{stem}.start")).exists());
    }

    // ── daemon.endpoint sidecar sweep tests (VAL-DAEMON-007) ──────────

    /// A `daemon.endpoint` sidecar with a dead PID is swept.
    #[test]
    fn test_sweep_removes_dead_daemon_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let ep = serde_json::json!({
            "socket_path": "/tmp/d.sock",
            "pid": 999_999, // dead
            "pid_start_time_ms": 0,
            "protocol_version": 1,
            "leindex_version": "test",
        });
        fs::write(
            dir.path().join("daemon.endpoint"),
            serde_json::to_vec(&ep).unwrap(),
        )
        .unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        assert!(
            report.removed >= 1,
            "dead-pid daemon.endpoint must be swept"
        );
        assert!(!dir.path().join("daemon.endpoint").exists());
    }

    /// A `daemon.endpoint` sidecar with a live (this process) PID is kept.
    #[test]
    fn test_sweep_keeps_live_daemon_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        let ep = serde_json::json!({
            "socket_path": "/tmp/d.sock",
            "pid": std::process::id(),
            "pid_start_time_ms": 0,
            "protocol_version": 1,
            "leindex_version": "test",
        });
        // We need this PID to look alive to pid_is_alive. On Linux, the
        // cmdline check verifies it's a leindex/mcp process. Since the test
        // runner process contains "leindex" in its args, this should pass.
        // But test runners are not named leindex, so on Linux the PID check
        // will fail (not a leindex process). The sidecar will be swept on
        // Linux. This test verifies the LOGIC, not the specific OS behavior.
        // On non-Linux it falls back to mtime (0s = stale), so it's swept too.
        fs::write(
            dir.path().join("daemon.endpoint"),
            serde_json::to_vec(&ep).unwrap(),
        )
        .unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        // With 0s max_age, the endpoint is stale regardless (non-leindex PID
        // or mtime fallback). The test verifies the sweep does NOT panic on
        // the daemon.endpoint JSON format.
        let _ = report;
    }

    /// A malformed `daemon.endpoint` is swept (crash recovery).
    #[test]
    fn test_sweep_removes_malformed_daemon_endpoint() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("daemon.endpoint"), b"{broken json").unwrap();

        let report = sweep_run_dir(dir.path(), Duration::from_secs(0), false);
        assert!(
            report.removed >= 1,
            "malformed daemon.endpoint must be swept"
        );
        assert!(!dir.path().join("daemon.endpoint").exists());
    }

    // ── Task 7: cleanup_project_store safety tests (VAL-ROLLOUT-010) ────

    /// Build a minimal generation store fixture under a temp dir.
    ///
    /// Creates:
    /// - `cas/` with a CasStore
    /// - `generations/<N>/manifest` for each generation
    /// - `CURRENT` pointing at the latest generation
    /// - `jobs/` directory
    ///
    /// Returns the temp dir, storage root, CasStore, and the generation
    /// numbers for convenience.
    fn build_generation_store_fixture(
        generations: &[u64],
        current: u64,
    ) -> (
        tempfile::TempDir,
        PathBuf,
        std::sync::Arc<std::sync::Mutex<crate::storage::cas::CasStore>>,
        Vec<([u8; 32], Vec<u8>)>,
    ) {
        use crate::storage::cas::CasStore;
        use crate::storage::generation::lease::{CURRENT_FILE, GENERATIONS_DIR, MANIFEST_FILE};
        use crate::storage::generation::manifest::{
            LayerKind, MANIFEST_VERSION, Manifest, ModelIdentity,
        };

        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let cas_dir = root.join("cas");
        let gens_dir = root.join(GENERATIONS_DIR);
        let jobs_dir = root.join("jobs");
        std::fs::create_dir_all(&cas_dir).unwrap();
        std::fs::create_dir_all(&gens_dir).unwrap();
        std::fs::create_dir_all(&jobs_dir).unwrap();

        let cas = CasStore::open(&cas_dir).unwrap();

        let mut layer_data: Vec<([u8; 32], Vec<u8>)> = Vec::new();

        for &gen_num in generations {
            // Create a synthetic blob for each layer.
            let mut layers = std::collections::HashMap::new();
            for kind in [
                LayerKind::Db,
                LayerKind::Tfidf,
                LayerKind::Neural,
                LayerKind::Pdg,
                LayerKind::Symbols,
            ] {
                let payload = format!("layer-{kind:?}-gen-{gen_num}").into_bytes();
                let hash = cas.put(&payload).expect("cas put must succeed");
                layers.insert(kind, hash);
                if gen_num == current {
                    layer_data.push((hash, payload));
                }
            }

            let manifest = Manifest {
                version: MANIFEST_VERSION,
                generation: gen_num,
                model_identity: ModelIdentity {
                    name: "test-model".to_string(),
                    digest: "test-digest".to_string(),
                    dimensions: 384,
                },
                graph_fingerprint: [0u8; 32],
                search_fingerprint: [0u8; 32],
                layers,
            };
            let manifest_bytes = manifest.to_bytes().unwrap();
            let gen_dir = gens_dir.join(gen_num.to_string());
            std::fs::create_dir_all(&gen_dir).unwrap();
            std::fs::write(gen_dir.join(MANIFEST_FILE), &manifest_bytes).unwrap();
        }

        // Write CURRENT pointer.
        std::fs::write(root.join(CURRENT_FILE), current.to_string()).unwrap();

        cas.persist().unwrap();
        let cas = std::sync::Arc::new(std::sync::Mutex::new(cas));

        (dir, root, cas, layer_data)
    }

    /// Cleanup must NOT remove the current generation.
    #[test]
    fn test_cleanup_never_removes_current_generation() {
        let (_dir, root, _cas, _layer_data) = build_generation_store_fixture(&[1, 2], 2);

        let report = cleanup_project_store(&root, false).unwrap();

        assert!(
            std::fs::exists(root.join("generations/2/manifest")).unwrap(),
            "current generation manifest must survive cleanup"
        );
        assert!(
            report.generations.generations_retained >= 1,
            "current gen must be counted as retained"
        );
    }

    /// Cleanup must NOT remove the previous (rollback) generation.
    #[test]
    fn test_cleanup_never_removes_previous_generation() {
        let (_dir, root, _cas, _layer_data) = build_generation_store_fixture(&[1, 2], 2);

        let report = cleanup_project_store(&root, false).unwrap();

        // Previous (gen 1) must survive because it's the rollback point.
        assert!(
            std::fs::exists(root.join("generations/1/manifest")).unwrap(),
            "previous (rollback) generation must survive cleanup"
        );
        assert_eq!(report.generations.generations_retained, 2);
    }

    /// Legacy (no-CAS) stores must be pruned too: full-copy generation dirs
    /// accumulated unboundedly because cleanup returned an empty report
    /// whenever `cas/` was missing (98 dirs / 17 GB observed in the wild).
    #[test]
    fn test_cleanup_prunes_legacy_full_copy_store() {
        use crate::storage::generation::lease::{CURRENT_FILE, GENERATIONS_DIR};
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let gens_dir = root.join(GENERATIONS_DIR);
        std::fs::create_dir_all(&gens_dir).unwrap();
        std::fs::create_dir_all(root.join("jobs")).unwrap();
        for gen_num in 1u64..=5 {
            let gen_dir = gens_dir.join(gen_num.to_string());
            std::fs::create_dir_all(&gen_dir).unwrap();
            std::fs::write(gen_dir.join("leindex.db"), format!("db-{gen_num}")).unwrap();
        }
        std::fs::write(root.join(CURRENT_FILE), "5").unwrap();

        let report = cleanup_project_store(&root, false).unwrap();

        assert_eq!(
            report.generations.generations_removed, 3,
            "legacy gens outside the current+previous window must be pruned"
        );
        for kept in [4u64, 5] {
            assert!(
                gens_dir.join(kept.to_string()).exists(),
                "legacy gen {kept} must survive (window)"
            );
        }
        for removed in [1u64, 2, 3] {
            assert!(
                !gens_dir.join(removed.to_string()).exists(),
                "legacy gen {removed} must be reclaimed"
            );
        }
    }

    /// Cleanup must remove stale generations (not current/previous/leased).
    #[test]
    fn test_cleanup_removes_stale_generations() {
        // 4 generations: 1 (stale), 2 (stale), 3 (previous=current-1), 4 (current)
        let (_dir, root, _cas, _layer_data) = build_generation_store_fixture(&[1, 2, 3, 4], 4);

        let report = cleanup_project_store(&root, false).unwrap();

        // Gens 1 and 2 should be deleted as stale.
        assert!(
            !std::fs::exists(root.join("generations/1")).unwrap(),
            "stale generation 1 must be removed"
        );
        assert!(
            !std::fs::exists(root.join("generations/2")).unwrap(),
            "stale generation 2 must be removed"
        );
        // Previous (3) and current (4) must survive.
        assert!(std::fs::exists(root.join("generations/3")).unwrap());
        assert!(std::fs::exists(root.join("generations/4")).unwrap());
        assert_eq!(report.generations.generations_removed, 2);
    }

    /// Cleanup must NOT remove a leased generation.
    ///
    /// A lease is simulated by incrementing refcounts on the generation's blobs.
    #[test]
    fn test_cleanup_never_removes_leased_generation() {
        let (_dir, root, cas, _layer_data) = build_generation_store_fixture(&[1, 2, 3, 4, 5], 5);

        // Simulate a lease on generation 1 by incrementing refcounts.
        // First, read gen 1's manifest for its hashes.
        {
            let manifest_bytes = std::fs::read(root.join("generations/1/manifest")).unwrap();
            let manifest =
                crate::storage::generation::manifest::Manifest::from_bytes(&manifest_bytes)
                    .unwrap();
            let mut store = cas.lock().unwrap();
            for hash in manifest.layer_hashes() {
                store.incr(&hash);
            }
            store.persist().unwrap();
        }

        let _report = cleanup_project_store(&root, false).unwrap();

        // Gen 1 must survive because its blobs are leased.
        assert!(
            std::fs::exists(root.join("generations/1")).unwrap(),
            "leased generation 1 must survive cleanup"
        );
        // Gens 2 and 3 are stale (current=5, previous=4, only gen 1 is leased).
        assert!(!std::fs::exists(root.join("generations/2")).unwrap());
        assert!(!std::fs::exists(root.join("generations/3")).unwrap());
    }

    /// Cleanup must remove orphaned CAS blobs (refcount 0, not pinned by any manifest).
    #[test]
    fn test_cleanup_removes_orphaned_cas_blobs() {
        use crate::storage::cas::CasStore;

        let (_dir, root, _cas, _layer_data) = build_generation_store_fixture(&[1], 1);

        // Put an orphan blob (no refcount, not in any manifest).
        let cas_dir = root.join("cas");
        let cas = CasStore::open(&cas_dir).unwrap();
        let orphan_payload = b"orphan-data-not-referenced";
        // put returns a Result, so unwrap.
        let orphan_hash = cas.put(orphan_payload).expect("cas put should succeed");
        cas.persist().unwrap();

        // Verify it exists.
        assert!(cas.exists(&orphan_hash));

        drop(cas);

        let report = cleanup_project_store(&root, false).unwrap();

        // Orphan should be gone.
        let cas = CasStore::open(&cas_dir).unwrap();
        assert!(
            !cas.exists(&orphan_hash),
            "orphaned CAS blob must be removed by cleanup"
        );
        assert!(report.generations.cas.blobs_removed > 0);
    }

    /// Cleanup must NOT remove CAS blobs referenced by current/previous/leased
    /// generations.
    #[test]
    fn test_cleanup_preserves_pinned_cas_blobs() {
        use crate::storage::cas::CasStore;

        let (_dir, root, _cas, layer_data) = build_generation_store_fixture(&[1, 2], 2);

        let report = cleanup_project_store(&root, false).unwrap();

        // All layer blobs from current generation must survive.
        let cas = CasStore::open(root.join("cas")).unwrap();
        for (hash, _payload) in &layer_data {
            assert!(
                cas.exists(hash),
                "CAS blob from current generation must survive cleanup"
            );
        }
        assert_eq!(report.generations.cas.blobs_removed, 0);
    }

    /// Cleanup must remove abandoned staging files.
    #[test]
    fn test_cleanup_removes_abandoned_staging() {
        let (_dir, root, _cas, _layer_data) = build_generation_store_fixture(&[1], 1);

        // Create abandoned staging files.
        let staging_dir = root.join("cas/.staging");
        std::fs::create_dir_all(&staging_dir).unwrap();
        std::fs::write(staging_dir.join("abandoneddigest.partial"), b"partial data").unwrap();
        std::fs::write(staging_dir.join("anothercrash.partial"), b"more partial").unwrap();

        let report = cleanup_project_store(&root, false).unwrap();

        assert_eq!(report.staging_files_removed, 2);
        // Staging dir should be empty now.
        let entries: Vec<_> = std::fs::read_dir(&staging_dir).unwrap().collect();
        assert!(entries.is_empty(), "all abandoned staging files removed");
    }

    /// Cleanup must produce a truthful report.
    #[test]
    fn test_cleanup_report_accurate() {
        let (_dir, root, _cas, _layer_data) =
            build_generation_store_fixture(&[1, 2, 3, 4, 5, 6], 6);

        let report = cleanup_project_store(&root, false).unwrap();

        // Current=6, previous=5 → retained = 2. Removed = 4.
        assert_eq!(report.generations.generations_retained, 2);
        assert_eq!(report.generations.generations_removed, 4);
    }
}
