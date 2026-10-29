// Memory Cap — RSS monitoring for admission (WS5 §5.3).
//
// Provides:
// - `current_rss_mb()`: reads current RSS via /proc/self/status (Linux) or sysinfo fallback
// - `MemoryCapGuard`: periodic RSS observer that logs pressure; over-cap reports
//   `CapStatus::OverCap` (a deferral signal) rather than aborting work
// - `apply_hard_limit()`: sets RLIMIT_AS as a hard ceiling (Linux-only)
//
// VAL-SCHED-015: the bail-at-cap error path is REMOVED from the indexing hot
// loop. A memory cap prevents overlapping peaks; it never converts valid work
// into errors. The global admission controller (`scheduler::admission`) owns
// capacity decisions and defers/reduces instead of erroring; this guard only
// observes RSS and reports the pressure signal.

use anyhow::{Result, bail};
use tracing::{info, warn};

/// Read the current RSS (Resident Set Size) in megabytes.
///
/// On Linux, reads VmRSS from `/proc/self/status` for accuracy.
/// Falls back to `sysinfo` on other platforms.
pub fn current_rss_mb() -> Result<u64> {
    #[cfg(target_os = "linux")]
    {
        read_rss_procfs()
    }
    #[cfg(not(target_os = "linux"))]
    {
        read_rss_sysinfo()
    }
}

#[cfg(target_os = "linux")]
fn read_rss_procfs() -> Result<u64> {
    let status = std::fs::read_to_string("/proc/self/status")?;
    for line in status.lines() {
        if line.starts_with("VmRSS:") {
            // Format: "VmRSS:    123456 kB"
            let kb: u64 = line
                .split_whitespace()
                .nth(1)
                .ok_or_else(|| anyhow::anyhow!("malformed VmRSS line"))?
                .parse()
                .map_err(|_| anyhow::anyhow!("non-numeric VmRSS value"))?;
            return Ok(kb / 1024); // kB → MB
        }
    }
    bail!("VmRSS not found in /proc/self/status")
}

#[cfg(not(target_os = "linux"))]
fn read_rss_sysinfo() -> Result<u64> {
    use sysinfo::System;
    let mut sys = System::new();
    sys.refresh_processes(sysinfo::ProcessesToUpdate::All, true);
    let pid = sysinfo::Pid::from(std::process::id() as usize);
    if let Some(proc) = sys.process(pid) {
        Ok(proc.memory() / (1024 * 1024))
    } else {
        bail!("Could not read process memory via sysinfo")
    }
}

/// Set a hard RLIMIT_AS ceiling at 110% of the requested cap.
///
/// This causes the OS to deny memory allocations that would exceed the limit,
/// producing an OOM-style error instead of triggering the system-level OOM
/// killer. Linux-only; no-op on other platforms.
///
/// # Arguments
/// * `mb` - The soft cap in megabytes
pub fn apply_hard_limit(mb: u64) -> Result<()> {
    let hard_mb = mb * 110 / 100; // 10% headroom
    let hard_bytes = hard_mb * 1024 * 1024;

    #[cfg(target_os = "linux")]
    {
        let rlim = libc::rlimit {
            rlim_cur: hard_bytes,
            rlim_max: libc::RLIM_INFINITY,
        };
        let result = unsafe { libc::setrlimit(libc::RLIMIT_AS, &rlim) };
        if result != 0 {
            let err = std::io::Error::last_os_error();
            warn!(
                "Failed to set RLIMIT_AS to {} MB ({} bytes): {}. Continuing without hard limit.",
                hard_mb, hard_bytes, err
            );
        } else {
            info!(
                "Set hard RLIMIT_AS ceiling to {} MB (110% of {} MB cap)",
                hard_mb, mb
            );
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = hard_bytes;
        info!(
            "Hard RSS limit not supported on this platform; soft monitoring only (cap = {} MB)",
            mb
        );
    }

    Ok(())
}

/// Outcome of a memory-cap observation.
///
/// The guard never errors (VAL-SCHED-015): every observation resolves to one
/// of these two states, and over-cap pressure is surfaced as a deferral signal
/// for the admission gate rather than a fatal indexing error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapStatus {
    /// RSS is below the cap (or within the warning band).
    Ok,
    /// RSS exceeds the cap: heavy work should defer until memory frees up.
    OverCap,
}

/// Periodic memory cap observer.
///
/// Call `check()` at regular intervals during indexing (e.g., after each batch
/// of nodes). It logs a warning when RSS exceeds 90% of the cap and reports
/// `CapStatus::OverCap` (a deferral signal) when RSS exceeds 100% of the cap.
/// It never aborts indexing — deferral, not erroring (§5.3, VAL-SCHED-015).
pub struct MemoryCapGuard {
    /// Soft cap in megabytes
    cap_mb: u64,
    /// Warning threshold (90% of cap by default)
    warn_threshold_mb: u64,
    /// Whether a warning has already been emitted (avoid log spam)
    warned: bool,
    /// Counter for periodic checks (check every N calls)
    check_counter: u64,
    /// Interval: check RSS every N calls to `check()`
    check_interval: u64,
}

impl MemoryCapGuard {
    /// Create a new guard with the given cap in megabytes.
    ///
    /// RSS is only checked every `check_interval` calls to amortize the cost
    /// of reading `/proc/self/status`.
    pub fn new(cap_mb: u64) -> Self {
        Self {
            cap_mb,
            warn_threshold_mb: cap_mb * 90 / 100,
            warned: false,
            check_counter: 0,
            check_interval: 100, // check every 100 calls
        }
    }

    /// Check current RSS against the cap.
    ///
    /// Returns `CapStatus::Ok` when under the cap, logs a warning at 90%, and
    /// returns `CapStatus::OverCap` (a deferral signal, never an error) when
    /// the cap is exceeded. The check is throttled to only run every
    /// `check_interval` calls to avoid excessive `/proc` reads.
    pub fn check(&mut self) -> CapStatus {
        self.check_counter += 1;
        if self.check_counter % self.check_interval != 0 {
            return CapStatus::Ok;
        }
        self.check_now()
    }

    /// Force an immediate RSS check regardless of the counter.
    ///
    /// Never returns an error. Over-cap pressure is reported as
    /// `CapStatus::OverCap` and logged as a deferral signal; the admission
    /// controller owns the actual guard/reduce/defer decision (§5.3).
    pub fn check_now(&mut self) -> CapStatus {
        match current_rss_mb() {
            Ok(rss) => {
                if rss > self.cap_mb {
                    warn!(
                        "Memory pressure: RSS is {} MB, cap is {} MB — DEFERRING heavy work \
                         (admission gate owns capacity; no indexing task is errored)",
                        rss, self.cap_mb
                    );
                    return CapStatus::OverCap;
                }
                if rss > self.warn_threshold_mb && !self.warned {
                    warn!(
                        "Approaching memory cap: RSS is {} MB ({}% of {} MB cap)",
                        rss,
                        rss * 100 / self.cap_mb,
                        self.cap_mb
                    );
                    self.warned = true;
                }
                CapStatus::Ok
            }
            Err(e) => {
                // If we can't read RSS, just log and continue — don't block indexing
                warn!("Could not read RSS for memory cap check: {}", e);
                CapStatus::Ok
            }
        }
    }

    /// Get the configured cap in MB.
    pub fn cap_mb(&self) -> u64 {
        self.cap_mb
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_current_rss_is_reasonable() {
        // RSS should be non-zero and less than 10 GB for a test process
        let rss = current_rss_mb().expect("should be able to read RSS");
        assert!(rss > 0, "RSS should be positive, got {}", rss);
        assert!(rss < 10_000, "RSS should be less than 10 GB, got {}", rss);
    }

    #[test]
    fn test_memory_cap_guard_under_cap() {
        // Use a very high cap so it never triggers
        let mut guard = MemoryCapGuard::new(1_000_000);
        guard.check_interval = 1; // check every call
        assert_eq!(guard.check(), CapStatus::Ok);
    }

    #[test]
    fn test_memory_cap_guard_throttling() {
        let mut guard = MemoryCapGuard::new(1_000_000);
        guard.check_interval = 1000;
        // First 999 calls should be no-ops
        for _ in 0..999 {
            assert_eq!(guard.check(), CapStatus::Ok);
        }
        // The 1000th call should actually check RSS
        assert_eq!(guard.check(), CapStatus::Ok);
    }

    #[test]
    fn test_memory_cap_guard_over_cap_defers_not_errors() {
        // Use a tiny cap (1 MB) that should always be exceeded. Over-cap
        // pressure reports `OverCap` (a deferral signal) and NEVER an error —
        // VAL-SCHED-015 / §5.3: a cap prevents peaks, not valid-work failures.
        let mut guard = MemoryCapGuard::new(1);
        guard.check_interval = 1;
        let status = guard.check();
        assert_eq!(
            status,
            CapStatus::OverCap,
            "exceeding the cap must defer (OverCap), never error"
        );
    }

    /// VAL-SCHED-015: indexing with a cap set artificially low defers
    /// (reports OverCap) across many hot-loop checks and NEVER returns an
    /// error. This is the contract the indexing loop relies on.
    #[test]
    fn test_no_error_at_cap() {
        let mut guard = MemoryCapGuard::new(1);
        guard.check_interval = 1;
        // Simulate the hot-loop checking RSS after each phase boundary.
        let mut over_cap_signal_seen = false;
        for _ in 0..50 {
            match guard.check() {
                CapStatus::Ok => {}
                CapStatus::OverCap => over_cap_signal_seen = true,
            }
        }
        assert!(
            over_cap_signal_seen,
            "a 1 MB cap must trigger the deferral (OverCap) signal at least once"
        );
        // The guard API is total: it never produced an Err for any observation.
    }
}
