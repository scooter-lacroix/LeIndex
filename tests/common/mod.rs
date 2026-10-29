//! Shared telemetry capture utilities for §13 verification scenarios.
//!
//! Each functional and soak/fault scenario must capture:
//! - **Correctness**: assertion outcome (pass/fail)
//! - **Latency**: wall-clock duration of key operations
//! - **CPU**: CPU time consumed during the scenario
//! - **Memory**: RSS (resident set size) sampled at beginning/end
//! - **Threads**: thread count of the process under test
//! - **Swap**: VmSwap from `/proc/<pid>/status`
//! - **GPU**: VRAM utilization (if a GPU is present)
//! - **Disk-IO**: read/write bytes from `/proc/<pid>/io`
//!
//! These helpers read `/proc` on Linux. On non-Linux platforms they return
//! zero-valued samples so the scenarios still compile and run.
//!
//! Used by `tests/fault/` and `tests/soak/` verification suites.

use std::path::PathBuf;
use std::time::{Duration, Instant};

/// A single telemetry sample captured at one point in time.
///
/// All memory/swap fields are in KiB to match `/proc/<pid>/status` units.
/// Disk-IO fields are in bytes to match `/proc/<pid>/io` units.
#[derive(Debug, Clone, Default)]
pub struct TelemetrySample {
    /// Resident set size in KiB (`VmRSS`).
    pub rss_kib: u64,
    /// Virtual memory size in KiB (`VmSize`).
    pub vmsize_kib: u64,
    /// Swap usage in KiB (`VmSwap`).
    pub swap_kib: u64,
    /// Thread count (`Threads`).
    pub threads: u64,
    /// GPU VRAM used in MiB (0 if no GPU).
    pub gpu_vram_mib: u64,
    /// GPU utilization percentage (0 if no GPU).
    pub gpu_util_pct: u8,
    /// Bytes read from disk (`read_bytes`).
    pub disk_read_bytes: u64,
    /// Bytes written to disk (`write_bytes`).
    pub disk_write_bytes: u64,
    /// CPU time (user + system) consumed, in microseconds.
    pub cpu_time_us: u64,
}

/// Aggregated telemetry for a single scenario run.
#[derive(Debug, Clone)]
pub struct ScenarioTelemetry {
    /// Human-readable scenario name (e.g., `"03_search_during_indexing"`).
    pub scenario_name: String,
    /// Scenario number (1-24 from §13 verification matrix).
    pub scenario_number: u8,
    /// Sample taken at the start of the measured region.
    pub start: TelemetrySample,
    /// Sample taken at the end of the measured region.
    pub end: TelemetrySample,
    /// Wall-clock latency of the measured region.
    pub latency: Duration,
    /// Whether the correctness assertion passed.
    pub correctness_passed: bool,
}

impl ScenarioTelemetry {
    /// RSS delta in KiB (end - start). Positive = growth.
    pub fn rss_delta_kib(&self) -> i64 {
        self.end.rss_kib as i64 - self.start.rss_kib as i64
    }

    /// Swap delta in KiB (end - start).
    pub fn swap_delta_kib(&self) -> i64 {
        self.end.swap_kib as i64 - self.start.swap_kib as i64
    }

    /// Disk read delta in bytes.
    pub fn disk_read_delta(&self) -> u64 {
        self.end
            .disk_read_bytes
            .saturating_sub(self.start.disk_read_bytes)
    }

    /// Disk write delta in bytes.
    pub fn disk_write_delta(&self) -> u64 {
        self.end
            .disk_write_bytes
            .saturating_sub(self.start.disk_write_bytes)
    }

    /// CPU time delta in microseconds.
    pub fn cpu_time_delta_us(&self) -> u64 {
        self.end.cpu_time_us.saturating_sub(self.start.cpu_time_us)
    }

    /// Return a summary string for logging/assertion evidence.
    pub fn summary(&self) -> String {
        format!(
            "scenario {} '{}': {} | latency={:.1}ms | rss_delta={:+}KiB | \
             swap_delta={:+}KiB | threads={} | cpu={}ms | disk_r={}B | disk_w={}B | \
             gpu_vram={}MiB",
            self.scenario_number,
            self.scenario_name,
            if self.correctness_passed {
                "PASS"
            } else {
                "FAIL"
            },
            self.latency.as_secs_f64() * 1000.0,
            self.rss_delta_kib(),
            self.swap_delta_kib(),
            self.end.threads,
            self.cpu_time_delta_us() / 1000,
            self.disk_read_delta(),
            self.disk_write_delta(),
            self.end.gpu_vram_mib,
        )
    }
}

/// Capture a telemetry sample for the current process.
pub fn capture_sample() -> TelemetrySample {
    capture_sample_for_pid(std::process::id())
}

/// Capture a telemetry sample for a specific PID.
pub fn capture_sample_for_pid(pid: u32) -> TelemetrySample {
    let mut sample = TelemetrySample::default();

    // /proc/<pid>/status: VmRSS, VmSize, VmSwap, Threads
    let status_path = PathBuf::from(format!("/proc/{pid}/status"));
    if let Ok(content) = std::fs::read_to_string(&status_path) {
        for line in content.lines() {
            if line.starts_with("VmRSS:") {
                sample.rss_kib = parse_first_number(line);
            } else if line.starts_with("VmSize:") {
                sample.vmsize_kib = parse_first_number(line);
            } else if line.starts_with("VmSwap:") {
                sample.swap_kib = parse_first_number(line);
            } else if line.starts_with("Threads:") {
                sample.threads = parse_first_number(line);
            }
        }
    }

    // /proc/<pid>/io: read_bytes, write_bytes
    let io_path = PathBuf::from(format!("/proc/{pid}/io"));
    if let Ok(content) = std::fs::read_to_string(&io_path) {
        for line in content.lines() {
            if line.starts_with("read_bytes:") {
                sample.disk_read_bytes = parse_first_number(line);
            } else if line.starts_with("write_bytes:") {
                sample.disk_write_bytes = parse_first_number(line);
            }
        }
    }

    // CPU time from /proc/<pid>/stat (fields 14+15: utime + stime in clock ticks)
    let stat_path = PathBuf::from(format!("/proc/{pid}/stat"));
    if let Ok(content) = std::fs::read_to_string(&stat_path) {
        if let Some(close_paren) = content.rfind(')') {
            let rest = &content[close_paren + 1..];
            let fields: Vec<&str> = rest.split_whitespace().collect();
            // Field 14 (utime) = index 11 after the state/ppid/pgrp/session/tty/tpgid/flags/minflt/cminflt/majflt/cmajflt
            // state=0, ppid=1, pgrp=2, session=3, tty=4, tpgid=5, flags=6, minflt=7, cminflt=8, majflt=9, cmajflt=10, utime=11, stime=12
            let utime: u64 = fields.get(11).and_then(|s| s.parse().ok()).unwrap_or(0);
            let stime: u64 = fields.get(12).and_then(|s| s.parse().ok()).unwrap_or(0);
            let clock_tick_hz = unsafe { libc::sysconf(libc::_SC_CLK_TCK) }.max(1) as u64;
            let total_ticks = utime.saturating_add(stime);
            // CPU time in microseconds = total_ticks * 1_000_000 / clock_tick_hz
            sample.cpu_time_us = total_ticks
                .saturating_mul(1_000_000)
                .checked_div(clock_tick_hz)
                .unwrap_or(0);
        }
    }

    // GPU: try rocm-smi or nvidia-smi on the current process.
    // This is best-effort; on headless CI it returns 0.
    sample.gpu_vram_mib = sample_gpu_vram();
    sample.gpu_util_pct = sample_gpu_util();

    sample
}

/// Run a closure and capture telemetry around it.
///
/// The closure returns `Result<T, E>` where `Ok` means correctness passed.
pub fn run_with_telemetry<T, E>(
    scenario_name: &str,
    scenario_number: u8,
    f: impl FnOnce() -> Result<T, E>,
) -> (Result<T, E>, ScenarioTelemetry) {
    let start = Instant::now();
    let start_sample = capture_sample();
    let result = f();
    let end_sample = capture_sample();
    let latency = start.elapsed();

    let telemetry = ScenarioTelemetry {
        scenario_name: scenario_name.to_string(),
        scenario_number,
        start: start_sample,
        end: end_sample,
        latency,
        correctness_passed: result.is_ok(),
    };

    (result, telemetry)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn parse_first_number(line: &str) -> u64 {
    line.split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

fn sample_gpu_vram() -> u64 {
    // Try nvidia-smi for the current process (most common CI GPU).
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-compute-apps=used_memory",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = text.lines().next() {
                if let Ok(v) = first_line.trim().parse::<u64>() {
                    return v;
                }
            }
        }
    }
    // Try rocm-smi.
    if let Ok(output) = std::process::Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--json"])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(&text) {
                if let Some(obj) = parsed.as_object() {
                    if let Some((_, dev_data)) = obj.iter().next() {
                        if let Some(vram_b) = dev_data
                            .get("VRAM Total Used (B)")
                            .and_then(|v| v.as_str())
                            .and_then(|s| s.parse::<u64>().ok())
                        {
                            return vram_b / (1024 * 1024);
                        }
                    }
                }
            }
        }
    }
    0
}

fn sample_gpu_util() -> u8 {
    // nvidia-smi
    if let Ok(output) = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
    {
        if output.status.success() {
            let text = String::from_utf8_lossy(&output.stdout);
            if let Some(first_line) = text.lines().next() {
                if let Ok(v) = first_line.trim().parse::<u8>() {
                    return v;
                }
            }
        }
    }
    0
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_capture_sample_returns_rss() {
        let sample = capture_sample();
        assert!(
            sample.rss_kib > 0,
            "RSS should be positive for current process"
        );
    }

    #[test]
    fn test_run_with_telemetry_captures_latency() {
        let (result, telemetry) = run_with_telemetry("test", 99, || Ok::<_, ()>(42));
        assert!(result.is_ok());
        assert_eq!(telemetry.scenario_name, "test");
        assert!(telemetry.latency.as_nanos() > 0);
        assert!(telemetry.correctness_passed);
    }

    #[test]
    fn test_run_with_telemetry_captures_failure() {
        let (_result, telemetry) = run_with_telemetry::<(), ()>("fail_test", 98, || Err(()));
        assert!(!telemetry.correctness_passed);
    }

    #[test]
    fn test_telemetry_summary_is_human_readable() {
        let t = ScenarioTelemetry {
            scenario_name: "test".to_string(),
            scenario_number: 1,
            start: TelemetrySample::default(),
            end: TelemetrySample {
                rss_kib: 50000,
                vmsize_kib: 100000,
                swap_kib: 100,
                threads: 10,
                ..Default::default()
            },
            latency: Duration::from_millis(42),
            correctness_passed: true,
        };
        let s = t.summary();
        assert!(s.contains("PASS"));
        assert!(s.contains("scenario 1"));
        assert!(s.contains("threads=10"));
    }
}
