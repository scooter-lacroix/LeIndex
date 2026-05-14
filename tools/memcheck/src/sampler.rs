//! Memory sampler — reads RSS and memory breakdown from /proc on Linux.
//!
//! Primary metric: VmRSS from `/proc/<pid>/status` (VAL-MEASURE-005).
//! Secondary: PSS from `smaps_rollup`; mapped-file vs anonymous from `smaps`
//! when available (VAL-MEASURE-006).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A single memory sample.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySample {
    /// RSS in KiB — the primary regression metric.
    pub rss_kib: u64,
    /// Mapped-file memory in KiB (Linux, 0 if unavailable).
    pub mapped_file_kib: u64,
    /// Anonymous memory in KiB (Linux, 0 if unavailable).
    pub anon_kib: u64,
    /// PSS in KiB (Linux, 0 if unavailable).
    pub pss_kib: u64,
}

/// Read a single memory sample for the given PID.
///
/// Reads VmRSS from `/proc/<pid>/status` (primary), then PSS from
/// `smaps_rollup`, and mapped-file / anonymous breakdown from full `smaps`.
/// The `smaps` read is the most expensive part; callers that need faster
/// sampling can use [`sample_fast`] instead.
pub fn sample(pid: u32) -> anyhow::Result<MemorySample> {
    let rss = read_vm_rss(pid)?;
    let (mapped, anon, pss) = read_smaps_breakdown(pid);
    Ok(MemorySample {
        rss_kib: rss,
        mapped_file_kib: mapped,
        anon_kib: anon,
        pss_kib: pss,
    })
}

/// Read a fast sample — VmRSS only, no smaps overhead.
///
/// Useful for high-frequency sampling where the mapped-file / anon
/// breakdown is not needed on every tick.
#[allow(dead_code)]
pub fn sample_fast(pid: u32) -> anyhow::Result<MemorySample> {
    let rss = read_vm_rss(pid)?;
    Ok(MemorySample {
        rss_kib: rss,
        mapped_file_kib: 0,
        anon_kib: 0,
        pss_kib: 0,
    })
}

/// Read VmRSS from /proc/<pid>/status.
fn read_vm_rss(pid: u32) -> anyhow::Result<u64> {
    let path = PathBuf::from(format!("/proc/{}/status", pid));
    let content = std::fs::read_to_string(&path)
        .map_err(|_| anyhow::anyhow!("cannot read {}", path.display()))?;

    for line in content.lines() {
        if line.starts_with("VmRSS:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                let kib: u64 = parts[1]
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid VmRSS value in {}", path.display()))?;
                return Ok(kib);
            }
        }
    }

    anyhow::bail!("VmRSS not found in {}", path.display())
}

/// Read mapped-file, anonymous, and PSS from `/proc/<pid>/smaps`.
///
/// Returns `(mapped_file_kib, anon_kib, pss_kib)` — all 0 if unavailable.
///
/// Strategy: parse the full `smaps` file once. Each VMA header line has the
/// form `addr-addr perms offset dev inode [pathname]`. If a pathname is
/// present (fields > 5), the mapping is file-backed; otherwise it is
/// anonymous. We accumulate `Rss:` from detail lines into the appropriate
/// bucket, and also extract `Pss:` from the rollup section.
fn read_smaps_breakdown(pid: u32) -> (u64, u64, u64) {
    // Try smaps_rollup first for PSS (much smaller file).
    let pss = read_pss_from_rollup(pid);

    // Full smaps for mapped-file vs anonymous breakdown.
    let (mf, anon) = read_mapped_anon_smaps(pid);
    (mf, anon, pss)
}

/// Read PSS from `/proc/<pid>/smaps_rollup` (small file, fast).
fn read_pss_from_rollup(pid: u32) -> u64 {
    let path = PathBuf::from(format!("/proc/{}/smaps_rollup", pid));
    let Ok(content) = std::fs::read_to_string(&path) else {
        return 0;
    };

    for line in content.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 2 {
            if parts[0] == "Pss:" {
                if let Ok(v) = parts[1].parse::<u64>() {
                    return v;
                }
            }
        }
    }
    0
}

/// Read mapped-file and anonymous memory from `/proc/<pid>/smaps`.
///
/// Parses VMA header lines to classify each mapping as file-backed or
/// anonymous, then accumulates `Rss:` detail lines into the appropriate
/// bucket.
fn read_mapped_anon_smaps(pid: u32) -> (u64, u64) {
    let path = PathBuf::from(format!("/proc/{}/smaps", pid));
    let Ok(content) = std::fs::read_to_string(&path) else {
        return (0, 0);
    };

    let mut mapped_file: u64 = 0;
    let mut anon: u64 = 0;
    let mut is_file_mapped = false;

    for line in content.lines() {
        let trimmed = line.trim();

        // VMA header lines: "55a1b2c3d000-55a1b2c4e000 r--p ..."
        // They start with a hex digit and contain a '-' between address ranges.
        // Detail lines are indented and start with a label like "Rss:", "Size:", etc.
        if is_vma_header(trimmed) {
            // File-backed mappings have a pathname after the inode field.
            // Format: address perms offset dev inode [pathname]
            // Fields:    0       1      2     3    4       5+
            let fields: Vec<&str> = trimmed.split_whitespace().collect();
            is_file_mapped = fields.len() > 5;
            continue;
        }

        // Detail line — look for Rss:
        if !is_file_mapped && !trimmed.starts_with("Rss:") {
            continue;
        }
        if is_file_mapped && !trimmed.starts_with("Rss:") {
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix("Rss:") {
            if let Ok(value) = rest
                .trim()
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse::<u64>()
            {
                if is_file_mapped {
                    mapped_file += value;
                } else {
                    anon += value;
                }
            }
        }
    }

    (mapped_file, anon)
}

/// Check if a line is a VMA header in `/proc/<pid>/smaps`.
///
/// VMA headers start with a hex address range like `55a1b2c3d000-55a1b2c4e000`.
fn is_vma_header(line: &str) -> bool {
    // Must start with a hex digit and contain '-' before any space.
    let Some(first) = line.chars().next() else {
        return false;
    };
    if !first.is_ascii_hexdigit() {
        return false;
    }
    // Look for the address range separator before any whitespace.
    for ch in line.chars() {
        if ch == '-' {
            return true;
        }
        if ch.is_whitespace() {
            break;
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_current_process() {
        let pid = std::process::id();
        let sample = sample(pid);
        assert!(sample.is_ok(), "should be able to sample current process");
        let s = sample.unwrap();
        assert!(s.rss_kib > 0, "RSS should be positive");
    }

    #[test]
    fn test_sample_fast_current_process() {
        let pid = std::process::id();
        let s = sample_fast(pid).expect("fast sample should work");
        assert!(s.rss_kib > 0, "RSS should be positive");
        // Fast sample does not populate mapped/anon/pss
        assert_eq!(s.mapped_file_kib, 0);
        assert_eq!(s.anon_kib, 0);
        assert_eq!(s.pss_kib, 0);
    }

    #[test]
    fn test_read_vm_rss_current() {
        let pid = std::process::id();
        let rss = read_vm_rss(pid);
        assert!(rss.is_ok(), "should read VmRSS for current process");
        assert!(rss.unwrap() > 0, "VmRSS should be positive");
    }

    #[test]
    fn test_read_smaps_breakdown_current() {
        let pid = std::process::id();
        let (mf, anon, pss) = read_smaps_breakdown(pid);
        // Both should be non-negative; at least one should be positive
        assert!(mf + anon > 0, "mapped_file + anon should be positive");
        // PSS may be 0 if smaps_rollup is unavailable, but on Linux it should work
        #[cfg(target_os = "linux")]
        assert!(pss > 0, "PSS should be positive on Linux");
    }

    #[test]
    fn test_is_vma_header() {
        assert!(is_vma_header(
            "55a1b2c3d000-55a1b2c4e000 r--p 00000000 08:01 12345  /usr/lib/libfoo.so"
        ));
        assert!(is_vma_header(
            "7f1234567000-7f1234568000 rw-p 00000000 00:00 0"
        ));
        assert!(!is_vma_header("Rss:                 4 kB"));
        assert!(!is_vma_header("Size:              256 kB"));
        assert!(!is_vma_header(""));
        assert!(!is_vma_header("VmFlags: rd ex mr mw me"));
    }

    #[test]
    fn test_read_mapped_anon_smaps_current() {
        let pid = std::process::id();
        let (mf, anon) = read_mapped_anon_smaps(pid);
        // On a real process, both should be populated
        assert!(
            mf > 0 || anon > 0,
            "at least one memory type should be present"
        );
    }
}
