// Lightweight memory report for graceful shutdown.
//
// When the `--memory-report` flag or `LEINDEX_MEMORY_REPORT` env var is set,
// leindex writes a compact JSON summary at shutdown containing peak RSS
// and phase-level max/sample information.
//
// VAL-MEASURE-022: Report is written on graceful shutdown.
// VAL-MEASURE-023: Report remains compact and summary-only.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use tracing::{debug, info, warn};

/// Global tracker instance, set once at startup.
/// Wrapped in `Option` so `shutdown()` can take the tracker out and
/// explicitly write the report — Rust does NOT run `Drop` for statics.
static GLOBAL_TRACKER: OnceLock<Mutex<Option<MemoryReportTracker>>> = OnceLock::new();

/// Set once at shutdown so the background sampler stops instead of polling a
/// tracker that has already been written and taken.
static SAMPLER_STOP: AtomicBool = AtomicBool::new(false);

/// Interval between background RSS samples.
///
/// A one-shot `leindex index` of a small corpus finishes in ~2.5 s and has
/// phases that last a few hundred milliseconds. Sampling at 25 ms puts tens
/// of observations inside every phase, so a phase's reported peak reflects
/// its real high-water mark instead of only the two instants where an
/// explicit `observe_rss` boundary happened to fire. One `/proc/self/status`
/// read costs a few microseconds, so the sampling overhead is negligible.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(25);

/// Store the tracker in the global slot. Call once during startup.
///
/// Also starts the background sampler thread. Without it the report can only
/// report peaks at phase boundaries, which misses the transient high-water
/// mark inside a phase — the exact number `--memory-report` exists to expose.
pub fn init_tracker(tracker: MemoryReportTracker) {
    if GLOBAL_TRACKER.set(Mutex::new(Some(tracker))).is_err() {
        // Already initialized (the flag and the env var both resolved, or a
        // second `Cli::run` in one process). The first tracker owns the
        // report; starting a second sampler would double-count samples.
        return;
    }
    let spawned = std::thread::Builder::new()
        .name("mem-report-sampler".to_string())
        .spawn(|| {
            while !SAMPLER_STOP.load(Ordering::Relaxed) {
                std::thread::sleep(SAMPLE_INTERVAL);
                sample_rss();
            }
        });
    if let Err(error) = spawned {
        // The report still works from explicit observations; it just loses
        // mid-phase peaks. Never fail startup over instrumentation.
        warn!("Memory report sampler thread failed to start: {error}");
    }
}

/// Record an RSS observation against the global tracker (if initialized).
/// Reads current RSS automatically.
pub fn observe_rss(phase: &str) {
    if let Some(tracker) = GLOBAL_TRACKER.get() {
        let rss = current_rss_bytes();
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = guard.as_mut() {
            t.observe_rss(rss);
        }
        debug!("Memory observation: phase={}, rss={} bytes", phase, rss);
    }
}

/// Feed one sampler reading to the active phase (if any).
fn sample_rss() {
    if let Some(tracker) = GLOBAL_TRACKER.get() {
        let rss = current_rss_bytes();
        if rss == 0 {
            return;
        }
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = guard.as_mut() {
            t.sample(rss);
        }
    }
}

/// Enter a named phase: subsequent observations and sampler readings attribute
/// to it until [`exit_phase`] (or the next `enter_phase`, which closes the
/// previous phase first).
pub fn enter_phase(name: &str) {
    if let Some(tracker) = GLOBAL_TRACKER.get() {
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = guard.as_mut() {
            t.enter_phase(name);
        }
    }
}

/// Close the active phase, pushing its summary onto the report.
pub fn exit_phase() {
    if let Some(tracker) = GLOBAL_TRACKER.get() {
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = guard.as_mut() {
            t.exit_phase();
        }
    }
}

/// Close the active phase only if it is still `name`.
///
/// A phase can hand off to a later one before its own guard drops (the lexical
/// phase opens `persist` partway through). Without this check the guard's drop
/// would close whichever phase happens to be active at that moment and
/// mis-attribute its peak.
pub fn close_phase(name: &str) {
    if let Some(tracker) = GLOBAL_TRACKER.get() {
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = guard.as_mut() {
            if t.active_phase() == Some(name) {
                t.exit_phase();
            }
        }
    }
}

/// Scoped phase marker. Enters the phase on construction and closes it on
/// drop, so an early `?` return still records the phase instead of silently
/// attributing the rest of the run to it.
pub struct PhaseScope {
    name: &'static str,
}

impl PhaseScope {
    /// Enter `name` for the lifetime of the returned guard.
    pub fn enter(name: &'static str) -> Self {
        enter_phase(name);
        Self { name }
    }
}

impl Drop for PhaseScope {
    fn drop(&mut self) {
        close_phase(self.name);
    }
}

/// Record a completed phase against the global tracker (if initialized).
pub fn record_phase(name: &str, peak_rss_bytes: u64, sample_count: u64) {
    if let Some(tracker) = GLOBAL_TRACKER.get() {
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(t) = guard.as_mut() {
            t.record_phase(name, peak_rss_bytes, sample_count);
        }
    }
}

/// Take the tracker out of the global slot and write the report.
///
/// Must be called explicitly at shutdown because Rust does **not** run
/// destructors for static variables. After this call the global slot
/// contains `None` and subsequent `observe_rss` / `record_phase` calls
/// are no-ops.
pub fn shutdown() {
    SAMPLER_STOP.store(true, Ordering::Relaxed);
    if let Some(tracker) = GLOBAL_TRACKER.get() {
        let mut guard = tracker.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(mut t) = guard.take() {
            if let Err(e) = t.write_report() {
                warn!("Failed to write memory report on shutdown: {}", e);
            }
        }
    }
}

/// Environment variable that enables memory report writing (same as --memory-report).
pub const MEMORY_REPORT_ENV: &str = "LEINDEX_MEMORY_REPORT";

/// Compact memory report written at shutdown.
///
/// Contains only summary-level data: peak RSS and a small number of
/// phase-level max/sample entries. Not a verbose trace dump.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReport {
    /// Schema version for forward compatibility.
    pub version: u32,
    /// Process uptime in seconds at report time.
    pub uptime_secs: f64,
    /// Peak RSS in bytes observed during the process lifetime.
    pub peak_rss_bytes: u64,
    /// Timestamp (UNIX epoch) when the report was generated.
    pub timestamp_secs: u64,
    /// Phase-level summary entries (at most a handful).
    pub phases: Vec<PhaseSummary>,
}

/// Summary of a single measurement phase.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseSummary {
    /// Phase name (e.g. "index", "query", "idle").
    pub name: String,
    /// Peak RSS in bytes during this phase.
    pub peak_rss_bytes: u64,
    /// Number of samples taken during this phase.
    pub sample_count: u64,
}

/// Tracker that accumulates peak RSS observations and writes a report on drop.
pub struct MemoryReportTracker {
    /// Target output path.
    path: PathBuf,
    /// Start time of the process.
    start: std::time::Instant,
    /// Peak RSS observed so far.
    peak_rss_bytes: u64,
    /// Phase summaries accumulated so far.
    phases: Vec<PhaseSummary>,
    /// Name of the phase currently accumulating observations, if any.
    active: Option<String>,
    /// Peak RSS seen inside `active`.
    active_peak: u64,
    /// Number of observations attributed to `active`.
    active_samples: u64,
    /// Whether a report has already been written (prevent double-write).
    written: bool,
}

impl MemoryReportTracker {
    /// Create a new tracker that will write to `path` on graceful shutdown.
    pub fn new(path: PathBuf) -> Self {
        debug!("Memory report will be written to {}", path.display());
        Self {
            path,
            start: std::time::Instant::now(),
            peak_rss_bytes: 0,
            phases: Vec::new(),
            active: None,
            active_peak: 0,
            active_samples: 0,
            written: false,
        }
    }

    /// Record an RSS observation, updating the peak if necessary.
    pub fn observe_rss(&mut self, rss_bytes: u64) {
        if rss_bytes > self.peak_rss_bytes {
            self.peak_rss_bytes = rss_bytes;
        }
        self.sample(rss_bytes);
    }

    /// Attribute one RSS reading to the active phase. A no-op when no phase is
    /// entered, so pre-index startup work never leaks into the first phase.
    pub fn sample(&mut self, rss_bytes: u64) {
        if rss_bytes > self.peak_rss_bytes {
            self.peak_rss_bytes = rss_bytes;
        }
        if self.active.is_some() {
            self.active_samples += 1;
            if rss_bytes > self.active_peak {
                self.active_peak = rss_bytes;
            }
        }
    }

    /// Name of the phase currently accumulating observations, if any.
    pub fn active_phase(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Test-only mirror of the free [`close_phase`], so the handoff behavior
    /// can be exercised without touching the process-global tracker.
    #[cfg(test)]
    fn close_phase_for_test(&mut self, name: &str) {
        if self.active_phase() == Some(name) {
            self.exit_phase();
        }
    }

    /// Begin attributing observations to `name`. Closes any phase still open.
    pub fn enter_phase(&mut self, name: impl Into<String>) {
        self.exit_phase();
        let rss = current_rss_bytes();
        // The reading at entry is a real process observation, so it belongs in
        // `peak_rss_bytes` as well as in the phase's own peak. Folding it into
        // only `active_peak` let a phase report more than the whole process.
        if rss > self.peak_rss_bytes {
            self.peak_rss_bytes = rss;
        }
        self.active = Some(name.into());
        self.active_peak = rss;
        self.active_samples = 1;
    }

    /// Close the active phase and push its summary. Idempotent when nothing is
    /// open, so `run_lexical` (which marks both lexical and persist) can close
    /// lexical itself and leave `persist` to a later boundary.
    pub fn exit_phase(&mut self) {
        let Some(name) = self.active.take() else {
            return;
        };
        let samples = self.active_samples;
        let peak = self.active_peak;
        self.active_samples = 0;
        self.active_peak = 0;
        // A phase entered but never sampled (the clock granularity beat us)
        // still happened; report it with one observation rather than dropping
        // it from the sequence.
        self.push_summary(name, peak, samples.max(1));
    }

    /// Record a completed phase summary.
    pub fn record_phase(
        &mut self,
        name: impl Into<String>,
        peak_rss_bytes: u64,
        sample_count: u64,
    ) {
        if peak_rss_bytes > self.peak_rss_bytes {
            self.peak_rss_bytes = peak_rss_bytes;
        }
        self.push_summary(name.into(), peak_rss_bytes, sample_count);
    }

    /// Record a completed phase summary without touching the process peak.
    ///
    /// `enter_phase` already folds every observation into `peak_rss_bytes`, so
    /// replaying a phase peak that way again would be a no-op at best.
    fn push_summary(&mut self, name: String, peak_rss_bytes: u64, sample_count: u64) {
        self.phases.push(PhaseSummary {
            name,
            peak_rss_bytes,
            sample_count,
        });
    }

    /// Write the report to disk.
    pub fn write_report(&mut self) -> std::io::Result<()> {
        if self.written {
            return Ok(());
        }
        self.written = true;

        let report = MemoryReport {
            version: 1,
            uptime_secs: self.start.elapsed().as_secs_f64(),
            peak_rss_bytes: self.peak_rss_bytes,
            timestamp_secs: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            phases: self.phases.clone(),
        };

        // Create parent directory if needed.
        if let Some(parent) = self.path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }

        let json = serde_json::to_string_pretty(&report)?;
        std::fs::write(&self.path, json)?;
        info!("Memory report written to {}", self.path.display());
        Ok(())
    }
}

impl Drop for MemoryReportTracker {
    fn drop(&mut self) {
        if let Err(e) = self.write_report() {
            warn!("Failed to write memory report: {}", e);
        }
    }
}

/// Resolve the memory report path from CLI flag or environment variable.
///
/// Returns `Some(path)` if either source is set, `None` otherwise.
/// The CLI flag takes precedence over the environment variable.
pub fn resolve_report_path(cli_flag: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = cli_flag {
        return Some(p.to_path_buf());
    }
    std::env::var(MEMORY_REPORT_ENV)
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
}

/// Read the current RSS in bytes using /proc on Linux or sysinfo fallback.
pub fn current_rss_bytes() -> u64 {
    #[cfg(target_os = "linux")]
    {
        read_rss_procfs_bytes().unwrap_or(0)
    }
    #[cfg(not(target_os = "linux"))]
    {
        read_rss_sysinfo_bytes().unwrap_or(0)
    }
}

#[cfg(target_os = "linux")]
fn read_rss_procfs_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            let kb: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

#[cfg(not(target_os = "linux"))]
fn read_rss_sysinfo_bytes() -> Option<u64> {
    use sysinfo::System;
    let mut sys = System::new();
    let pid = sysinfo::Pid::from(std::process::id() as usize);
    let pid_list = [pid];
    sys.refresh_processes(sysinfo::ProcessesToUpdate::Some(&pid_list), true);
    sys.process(pid).map(|p| p.memory())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    /// `LEINDEX_MEMORY_REPORT` is process-global state: parallel tests in this
    /// module that set/remove it would race each other's reads, and a
    /// module-local lock would still race every other module. Every
    /// env-mutating test holds the shared lock across its arrange/act/cleanup
    /// window.
    use crate::test_env::EnvLock as ENV_LOCK;

    fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    #[test]
    fn test_resolve_report_path_none_when_unset() {
        let _guard = env_lock();
        // Clear env var to ensure clean state
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(MEMORY_REPORT_ENV) };
        assert!(resolve_report_path(None).is_none());
    }

    #[test]
    fn test_resolve_report_path_from_flag() {
        let path = Path::new("/tmp/test-report.json");
        let result = resolve_report_path(Some(path));
        assert_eq!(result, Some(PathBuf::from("/tmp/test-report.json")));
    }

    #[test]
    fn test_resolve_report_path_from_env() {
        let _guard = env_lock();
        // Clean up first to avoid interference from parallel tests
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(MEMORY_REPORT_ENV) };
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(MEMORY_REPORT_ENV, "/tmp/env-report.json") };
        let result = resolve_report_path(None);
        assert_eq!(result, Some(PathBuf::from("/tmp/env-report.json")));
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(MEMORY_REPORT_ENV) };
    }

    #[test]
    fn test_flag_takes_precedence_over_env() {
        let _guard = env_lock();
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(MEMORY_REPORT_ENV, "/tmp/env-report.json") };
        let flag_path = Path::new("/tmp/flag-report.json");
        let result = resolve_report_path(Some(flag_path));
        assert_eq!(result, Some(PathBuf::from("/tmp/flag-report.json")));
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(MEMORY_REPORT_ENV) };
    }

    #[test]
    fn test_empty_env_var_ignored() {
        let _guard = env_lock();
        // Ensure clean state first
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(MEMORY_REPORT_ENV) };
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(MEMORY_REPORT_ENV, "") };
        let result = resolve_report_path(None);
        assert!(
            result.is_none(),
            "empty env var should be ignored, got {:?}",
            result
        );
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::remove_var(MEMORY_REPORT_ENV) };
    }

    #[test]
    fn test_tracker_writes_report() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("report.json");
        let mut tracker = MemoryReportTracker::new(path.clone());
        tracker.observe_rss(100_000_000);
        tracker.record_phase("index", 150_000_000, 42);
        tracker.write_report().unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        let report: MemoryReport = serde_json::from_str(&contents).unwrap();
        assert_eq!(report.version, 1);
        assert_eq!(report.peak_rss_bytes, 150_000_000);
        assert_eq!(report.phases.len(), 1);
        assert_eq!(report.phases[0].name, "index");
        assert_eq!(report.phases[0].peak_rss_bytes, 150_000_000);
        assert_eq!(report.phases[0].sample_count, 42);
        assert!(report.uptime_secs >= 0.0);
        assert!(report.timestamp_secs > 0);
    }

    #[test]
    fn test_tracker_drop_writes_report() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("drop-report.json");
        {
            let mut tracker = MemoryReportTracker::new(path.clone());
            tracker.observe_rss(50_000_000);
            // tracker drops here and should write the report
        }
        let contents = fs::read_to_string(&path).unwrap();
        let report: MemoryReport = serde_json::from_str(&contents).unwrap();
        assert_eq!(report.peak_rss_bytes, 50_000_000);
    }

    #[test]
    fn test_report_is_compact() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("compact-report.json");
        let mut tracker = MemoryReportTracker::new(path.clone());
        tracker.observe_rss(100_000_000);
        tracker.record_phase("index", 150_000_000, 42);
        tracker.record_phase("query", 120_000_000, 10);
        tracker.write_report().unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        // Report should be small — well under 2 KiB for a summary-only report
        assert!(
            contents.len() < 2048,
            "Report should be compact, got {} bytes",
            contents.len()
        );
    }

    #[test]
    fn test_double_write_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("idempotent-report.json");
        let mut tracker = MemoryReportTracker::new(path.clone());
        tracker.observe_rss(100_000_000);
        tracker.write_report().unwrap();
        tracker.write_report().unwrap(); // second call should be a no-op

        let contents = fs::read_to_string(&path).unwrap();
        let report: MemoryReport = serde_json::from_str(&contents).unwrap();
        assert_eq!(report.peak_rss_bytes, 100_000_000);
    }

    #[test]
    fn test_phase_entries_accumulate_in_enter_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("phases-report.json");
        let mut tracker = MemoryReportTracker::new(path.clone());
        tracker.enter_phase("scan");
        tracker.sample(10_000_000);
        tracker.sample(30_000_000);
        tracker.enter_phase("parse");
        tracker.sample(20_000_000);
        tracker.exit_phase();
        tracker.write_report().unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        let report: MemoryReport = serde_json::from_str(&contents).unwrap();
        let names: Vec<&str> = report.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["scan", "parse"]);
        // `enter_phase` seeds a phase's peak with the live RSS at entry (the
        // entry state is part of what the phase costs) and folds that reading
        // into the process peak, so the report can only be asserted as a lower
        // bound over the samples injected here — the test process's own RSS is
        // the real floor, and it may exceed any value injected above.
        assert!(report.phases[0].peak_rss_bytes >= 30_000_000);
        assert!(report.phases[1].peak_rss_bytes >= 20_000_000);
        assert!(report.phases[0].sample_count >= 2);
        assert!(report.phases[1].sample_count >= 1);
        assert!(report.peak_rss_bytes >= report.phases[0].peak_rss_bytes);
        assert!(report.peak_rss_bytes >= report.phases[1].peak_rss_bytes);
    }

    #[test]
    fn test_record_phase_values_are_reported_verbatim() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("verbatim-report.json");
        let mut tracker = MemoryReportTracker::new(path.clone());
        tracker.record_phase("index", 150_000_000, 7);
        tracker.write_report().unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        let report: MemoryReport = serde_json::from_str(&contents).unwrap();
        assert_eq!(report.phases.len(), 1);
        assert_eq!(report.phases[0].peak_rss_bytes, 150_000_000);
        assert_eq!(report.phases[0].sample_count, 7);
    }

    #[test]
    fn test_observations_outside_a_phase_do_not_invent_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("no-phase-report.json");
        let mut tracker = MemoryReportTracker::new(path.clone());
        tracker.observe_rss(40_000_000);
        tracker.write_report().unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        let report: MemoryReport = serde_json::from_str(&contents).unwrap();
        assert!(
            report.phases.is_empty(),
            "startup observations must not be attributed to a phase that never ran"
        );
        assert_eq!(report.peak_rss_bytes, 40_000_000);
    }

    #[test]
    fn test_next_enter_closes_the_previous_phase() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("handoff-report.json");
        let mut tracker = MemoryReportTracker::new(path.clone());
        tracker.enter_phase("lexical");
        tracker.enter_phase("persist");
        tracker.exit_phase();
        tracker.write_report().unwrap();

        let contents = fs::read_to_string(&path).unwrap();
        let report: MemoryReport = serde_json::from_str(&contents).unwrap();
        let names: Vec<&str> = report.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["lexical", "persist"],
            "a handoff must close the previous phase exactly once"
        );
    }

    #[test]
    fn test_close_phase_ignores_a_phase_that_already_handed_off() {
        let mut tracker = MemoryReportTracker::new(PathBuf::from("/tmp/unused-report.json"));
        tracker.enter_phase("lexical");
        tracker.enter_phase("persist");
        // The lexical scope's guard drops after handing off to `persist`.
        tracker.close_phase_for_test("lexical");
        assert_eq!(tracker.active_phase(), Some("persist"));
        tracker.exit_phase();
        let names: Vec<&str> = tracker.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, vec!["lexical", "persist"]);
    }

    #[test]
    fn test_current_rss_bytes_is_reasonable() {
        let rss = current_rss_bytes();
        // RSS should be non-zero and less than 10 GB
        assert!(rss > 0, "RSS should be positive, got {}", rss);
        assert!(
            rss < 10_000_000_000,
            "RSS should be less than 10 GB, got {}",
            rss
        );
    }
}
