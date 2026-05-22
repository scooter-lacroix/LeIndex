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
use std::sync::{Mutex, OnceLock};
use tracing::{debug, info, warn};

/// Global tracker instance, set once at startup.
/// Wrapped in `Option` so `shutdown()` can take the tracker out and
/// explicitly write the report — Rust does NOT run `Drop` for statics.
static GLOBAL_TRACKER: OnceLock<Mutex<Option<MemoryReportTracker>>> = OnceLock::new();

/// Store the tracker in the global slot. Call once during startup.
pub fn init_tracker(tracker: MemoryReportTracker) {
    let _ = GLOBAL_TRACKER.set(Mutex::new(Some(tracker)));
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
            written: false,
        }
    }

    /// Record an RSS observation, updating the peak if necessary.
    pub fn observe_rss(&mut self, rss_bytes: u64) {
        if rss_bytes > self.peak_rss_bytes {
            self.peak_rss_bytes = rss_bytes;
        }
    }

    /// Record a completed phase summary.
    pub fn record_phase(
        &mut self,
        name: impl Into<String>,
        peak_rss_bytes: u64,
        sample_count: u64,
    ) {
        self.phases.push(PhaseSummary {
            name: name.into(),
            peak_rss_bytes,
            sample_count,
        });
        if peak_rss_bytes > self.peak_rss_bytes {
            self.peak_rss_bytes = peak_rss_bytes;
        }
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

    #[test]
    fn test_resolve_report_path_none_when_unset() {
        // Clear env var to ensure clean state
        std::env::remove_var(MEMORY_REPORT_ENV);
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
        // Clean up first to avoid interference from parallel tests
        std::env::remove_var(MEMORY_REPORT_ENV);
        std::env::set_var(MEMORY_REPORT_ENV, "/tmp/env-report.json");
        let result = resolve_report_path(None);
        assert_eq!(result, Some(PathBuf::from("/tmp/env-report.json")));
        std::env::remove_var(MEMORY_REPORT_ENV);
    }

    #[test]
    fn test_flag_takes_precedence_over_env() {
        std::env::set_var(MEMORY_REPORT_ENV, "/tmp/env-report.json");
        let flag_path = Path::new("/tmp/flag-report.json");
        let result = resolve_report_path(Some(flag_path));
        assert_eq!(result, Some(PathBuf::from("/tmp/flag-report.json")));
        std::env::remove_var(MEMORY_REPORT_ENV);
    }

    #[test]
    fn test_empty_env_var_ignored() {
        // Ensure clean state first
        std::env::remove_var(MEMORY_REPORT_ENV);
        std::env::set_var(MEMORY_REPORT_ENV, "");
        let result = resolve_report_path(None);
        assert!(
            result.is_none(),
            "empty env var should be ignored, got {:?}",
            result
        );
        std::env::remove_var(MEMORY_REPORT_ENV);
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
