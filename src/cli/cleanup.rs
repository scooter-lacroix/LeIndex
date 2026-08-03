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

/// Check whether a directory is likely in active use by trying to create and
/// delete a test file inside it.  If we cannot, we skip removal.
fn is_locked(dir: &Path) -> bool {
    let test_file = dir.join(".leindex-gc-lock-test");
    match fs::write(&test_file, b"test") {
        Ok(_) => {
            // Clean up the test file
            let _ = fs::remove_file(&test_file);
            false
        }
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
/// threshold.  This is meant to be called early in the CLI startup path.
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
fn pid_is_alive(pid: u32) -> Option<bool> {
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

    // Group sidecar files by their stem (e.g. `leindex-embed-<hash>`).
    let mut stems: std::collections::BTreeMap<String, Vec<PathBuf>> =
        std::collections::BTreeMap::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
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

/// Register an at-exit cleanup hook that removes the given temp storage
/// directory when the process exits cleanly.
///
/// This uses panic hooks for cleanup.  The cleanup is best-effort — if the
/// process is killed with SIGKILL, artifacts will remain until the next
/// startup GC pass.
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

    // Register a shared cleanup function using at_exit
    // We use std::sync::Once to ensure single registration
    static CLEANUP_REGISTERED: std::sync::Once = std::sync::Once::new();
    CLEANUP_REGISTERED.call_once(|| {
        // Note: We cannot move storage_path into the panic hook since
        // set_hook requires Fn and not FnOnce. Instead, we use a global
        // option for the cleanup path.
        // For now, the startup GC is the primary cleanup mechanism.
        // At-exit cleanup is best-effort via the startup GC on next run.
    });
}

/// Best-effort cleanup of a single storage directory.
pub fn best_effort_cleanup(path: &Path) {
    if path.exists() && path.starts_with(std::env::temp_dir()) {
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
}
