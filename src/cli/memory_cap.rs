// Memory Cap — RSS monitoring and hard limit enforcement for indexing.
//
// Provides:
// - `current_rss_mb()`: reads current RSS via /proc/self/status (Linux) or sysinfo fallback
// - `MemoryCapGuard`: periodic checker that warns at 90% and errors at 100% of a cap
// - `apply_hard_limit()`: sets RLIMIT_AS as a hard ceiling (Linux-only)

use anyhow::{bail, Result};
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

/// Periodic memory cap checker.
///
/// Call `check()` at regular intervals during indexing (e.g., after each batch
/// of nodes). It will:
/// - Log a warning when RSS exceeds 90% of the cap
/// - Return an error when RSS exceeds 100% of the cap
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
    /// Returns `Ok(())` if under the cap, logs a warning at 90%,
    /// and returns an error if the cap is exceeded.
    ///
    /// The check is throttled to only run every `check_interval` calls
    /// to avoid excessive `/proc` reads.
    pub fn check(&mut self) -> Result<()> {
        self.check_counter += 1;
        if self.check_counter % self.check_interval != 0 {
            return Ok(());
        }

        self.check_now()
    }

    /// Force an immediate RSS check regardless of the counter.
    pub fn check_now(&mut self) -> Result<()> {
        match current_rss_mb() {
            Ok(rss) => {
                if rss > self.cap_mb {
                    bail!(
                        "Memory cap exceeded: RSS is {} MB, cap is {} MB. \
                         Indexing stopped gracefully. Increase --max-memory or index a smaller project.",
                        rss, self.cap_mb
                    );
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
                Ok(())
            }
            Err(e) => {
                // If we can't read RSS, just log and continue — don't block indexing
                warn!("Could not read RSS for memory cap check: {}", e);
                Ok(())
            }
        }
    }

    /// Get the configured cap in MB.
    #[allow(dead_code)]
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
        guard.check().expect("should not error when under cap");
    }

    #[test]
    fn test_memory_cap_guard_throttling() {
        let mut guard = MemoryCapGuard::new(1_000_000);
        guard.check_interval = 1000;
        // First 999 calls should be no-ops
        for _ in 0..999 {
            guard.check().expect("should not error");
        }
        // The 1000th call should actually check RSS
        guard.check().expect("should not error");
    }

    #[test]
    fn test_memory_cap_guard_over_cap() {
        // Use a tiny cap (1 MB) that should always be exceeded
        let mut guard = MemoryCapGuard::new(1);
        guard.check_interval = 1;
        let result = guard.check();
        assert!(result.is_err(), "should error when RSS exceeds cap");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("Memory cap exceeded"),
            "error should mention cap exceeded: {}",
            err_msg
        );
    }
}
