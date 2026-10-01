//! Memory sampler — reads RSS and memory breakdown from /proc on Linux.
//!
//! Primary metric: VmRSS from `/proc/<pid>/status` (VAL-MEASURE-005).
//! Secondary: PSS from `smaps_rollup`; mapped-file vs anonymous from `smaps`
//! when available (VAL-MEASURE-006).
//!
//! Worker-aware sampling (VAL-CPHASE-034): workers are identified by the hidden
//! argv token and sampled only when they are direct children of the measured
//! process.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// A single memory sample, optionally including a worker process.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemorySample {
    /// RSS in KiB — the primary regression metric (main process).
    pub rss_kib: u64,
    /// Mapped-file memory in KiB (Linux, 0 if unavailable).
    pub mapped_file_kib: u64,
    /// Anonymous memory in KiB (Linux, 0 if unavailable).
    pub anon_kib: u64,
    /// PSS in KiB (Linux, 0 if unavailable).
    pub pss_kib: u64,
    /// Worker process RSS in KiB, if a worker was detected (VAL-CPHASE-034).
    /// 0 when no worker is running or worker tracking is not enabled.
    pub worker_rss_kib: u64,
    /// GPU sample (VRAM utilization), if a GPU was detected (§14 item 8).
    #[serde(default)]
    pub gpu: GpuSample,
}

/// GPU VRAM/utilization sample (§14 item 8, §2.1 GPU memory reporting).
///
/// On a machine with ROCm (`rocm-smi`) or CUDA (`nvidia-smi`), fields are
/// `Some`. On headless boxes (no GPU tools), all fields are `None`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct GpuSample {
    /// VRAM used in MiB.
    pub vram_used_mib: Option<u64>,
    /// GPU utilization percentage.
    pub gpu_utilization_pct: Option<u8>,
    /// Provider: "rocm" | "cuda" | "migraphx".
    pub provider: Option<String>,
}

/// Sample GPU VRAM and utilization via rocm-smi or nvidia-smi (first device only).
///
/// Returns `GpuSample::default()` (all None) on headless boxes where neither
/// tool exists. Never panics.
pub fn sample_gpu() -> GpuSample {
    // Try ROCm first (AMD/ROCm/MIGraphX).
    if let Some(sample) = sample_gpu_rocm() {
        return sample;
    }
    // Try CUDA (NVIDIA).
    if let Some(sample) = sample_gpu_cuda() {
        return sample;
    }
    // Headless: nothing found.
    GpuSample::default()
}

/// Parse VRAM from `rocm-smi --showmeminfo vram --json`.
fn sample_gpu_rocm() -> Option<GpuSample> {
    let output = std::process::Command::new("rocm-smi")
        .args(["--showmeminfo", "vram", "--json"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let json_str = String::from_utf8_lossy(&output.stdout);
    let parsed: serde_json::Value = serde_json::from_str(&json_str).ok()?;

    // rocm-smi --json returns a map keyed by device, e.g. {"card0": {"VRAM Total Used (B)": "1234567"}}
    // We take the first device.
    let obj = parsed.as_object()?;
    let (_dev_name, dev_data) = obj.iter().next()?;
    let dev = dev_data.as_object()?;

    let vram_used_mib = dev
        .iter()
        .find(|(k, _)| k.contains("VRAM") && k.contains("Used"))
        .and_then(|(_, v)| v.as_str())
        .and_then(|s| s.parse::<u64>().ok())
        .map(|bytes| bytes / (1024 * 1024));

    // GPU utilization is not directly available from --showmeminfo vram,
    // but we can try to get it from the general JSON.
    let gpu_utilization_pct = dev
        .iter()
        .find(|(k, _)| k.contains("GPU") && k.contains("Use"))
        .and_then(|(_, v)| v.as_str())
        .and_then(|s| s.trim_matches('%').parse::<u8>().ok());

    Some(GpuSample {
        vram_used_mib,
        gpu_utilization_pct,
        provider: Some("rocm".to_string()),
    })
}

/// Parse VRAM and utilization from `nvidia-smi` CSV format.
fn sample_gpu_cuda() -> Option<GpuSample> {
    let output = std::process::Command::new("nvidia-smi")
        .args([
            "--query-gpu=memory.used,utilization.gpu",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let csv = String::from_utf8_lossy(&output.stdout);
    let line = csv.lines().next()?;
    let parts: Vec<&str> = line.trim().split(',').map(|s| s.trim()).collect();
    if parts.len() < 2 {
        return None;
    }
    let vram_used_mib = parts[0].parse::<u64>().ok();
    let gpu_utilization_pct = parts[1].parse::<u8>().ok();

    Some(GpuSample {
        vram_used_mib,
        gpu_utilization_pct,
        provider: Some("cuda".to_string()),
    })
}

/// If worker tracking is enabled, also discovers a direct child running in
/// worker mode (VAL-CPHASE-034).
pub fn sample(pid: u32, track_worker: bool) -> anyhow::Result<MemorySample> {
    let rss = read_vm_rss(pid)?;
    let (mapped, anon, pss) = read_smaps_breakdown(pid);

    let worker_rss = if track_worker {
        find_child_worker_rss(pid)
    } else {
        0
    };

    // GPU sampling is global (not per-pid), but cheap enough to inline.
    let gpu = sample_gpu();

    Ok(MemorySample {
        rss_kib: rss,
        mapped_file_kib: mapped,
        anon_kib: anon,
        pss_kib: pss,
        worker_rss_kib: worker_rss,
        gpu,
    })
}

/// Fast sample (VmRSS only) — used by high-frequency sampling tests.
#[cfg(test)]
fn sample_fast(pid: u32) -> anyhow::Result<MemorySample> {
    let rss = read_vm_rss(pid)?;
    Ok(MemorySample {
        rss_kib: rss,
        mapped_file_kib: 0,
        anon_kib: 0,
        pss_kib: 0,
        worker_rss_kib: 0,
        gpu: GpuSample::default(),
    })
}

/// Source of truth: `src/embed/worker_main.rs` (`INTERNAL_WORKER_TOKEN`).
pub const WORKER_CMDLINE_TOKEN: &str = "--internal-embed-worker";

/// Whether `/proc/<pid>/cmdline` contains the worker token as an argument.
pub fn is_worker_process(pid: u32) -> bool {
    let path = format!("/proc/{pid}/cmdline");
    let Ok(cmdline) = std::fs::read(&path) else {
        return false;
    };
    cmdline
        .split(|byte| *byte == 0)
        .any(|argument| argument == WORKER_CMDLINE_TOKEN.as_bytes())
}

/// Find the RSS of a direct child running in worker mode.
///
/// Scans `/proc` for direct children whose NUL-separated argv contains the
/// hidden worker token. Returns the RSS of the first matching child, or 0.
/// Ownership is checked independently of the process name because the worker
/// re-executes the same binary as its parent.
fn find_child_worker_rss(parent_pid: u32) -> u64 {
    let proc_dir = match std::fs::read_dir("/proc") {
        Ok(d) => d,
        Err(_) => return 0,
    };

    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let Some(name_str) = name.to_str() else {
            continue;
        };
        let Ok(child_pid) = name_str.parse::<u32>() else {
            continue;
        };
        if child_pid == parent_pid || !is_child_of(child_pid, parent_pid) {
            continue;
        }
        if is_worker_process(child_pid)
            && let Ok(rss) = read_vm_rss(child_pid)
        {
            return rss;
        }
    }
    0
}

/// Check if `child_pid` is a child of `parent_pid` by reading
/// `/proc/<child_pid>/stat` and checking the ppid field.
fn is_child_of(child_pid: u32, parent_pid: u32) -> bool {
    let path = format!("/proc/{}/stat", child_pid);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };

    // Format: pid (comm) state ppid ...
    // The comm field may contain spaces and parens, so find the last ')'
    // and parse from there.
    let Some(close_paren) = content.rfind(')') else {
        return false;
    };

    let rest = &content[close_paren + 1..];
    let mut fields = rest.split_whitespace();

    // Skip state field (field 3 after pid)
    fields.next(); // state

    // ppid is field 4
    if let Some(ppid_str) = fields.next() {
        if let Ok(ppid) = ppid_str.parse::<u32>() {
            return ppid == parent_pid;
        }
    }

    false
}

/// Read VmRSS from /proc/`<pid>`/status.
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
        if parts.len() >= 2 && parts[0] == "Pss:" {
            if let Ok(v) = parts[1].parse::<u64>() {
                return v;
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
            is_file_mapped = fields.len() > 5 && !fields[5].starts_with('[');
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
            if let Ok(value) = rest.split_whitespace().next().unwrap_or("0").parse::<u64>() {
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

/// Capture a heap-profile snapshot for the given PID at a phase boundary.
///
/// On default (glibc) builds, captures `/proc/<pid>/smaps` as a phase-boundary
/// snapshot — works on all Linux without requiring the memprof build.
/// The output file is named `<phase>_<boundary>.smaps` and written to `out_dir`.
///
/// When `cargo build --features memprof` is used, engineers can set
/// `MALLOC_CONF=prof:true` and use jemalloc epoch-based dumping for deeper
/// analysis (see `src/bin/leindex.rs` doc comment).
///
/// Returns the path to the written snapshot file.
pub fn capture_heap_profile(
    pid: u32,
    phase: &str,
    boundary: &str,
    out_dir: &Path,
) -> std::io::Result<PathBuf> {
    let smaps_path = PathBuf::from(format!("/proc/{}/smaps", pid));
    let content = std::fs::read_to_string(&smaps_path)?;

    let file_name = format!("{}_{}.smaps", phase, boundary);
    let out_path = out_dir.join(file_name);
    std::fs::write(&out_path, &content)?;

    Ok(out_path)
}

/// Descendant process-tree summary (§14 item: "count descendant processes").
///
/// Walks `/proc/*/stat` PPID fields in BFS from `root_pid` to discover all
/// living descendants. Reports total count, per-name counts, and combined RSS.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct DescendantTree {
    /// Total number of descendant processes (not counting root).
    pub total: usize,
    /// Process name → count.
    pub by_name: HashMap<String, usize>,
    /// Sum of RSS across all descendants, in KiB.
    pub combined_rss_kib: u64,
}

use std::collections::HashMap;

/// Count all descendant processes of `root_pid` via BFS over `/proc/*/stat`.
///
/// Walks the process tree starting from `root_pid`, visiting every process
/// whose PPID matches a discovered ancestor. Returns a [`DescendantTree`]
/// summarizing total count, per-name counts, and combined RSS.
///
/// `root_pid` itself is NOT counted (only its descendants).
pub fn count_descendants(root_pid: u32) -> std::io::Result<DescendantTree> {
    // Build a map of pid → (ppid, name, rss_kib) for all live processes.
    let mut all_procs: Vec<(u32, u32, String, u64)> = Vec::new();
    let proc_dir = std::fs::read_dir("/proc")?;
    for entry in proc_dir.flatten() {
        let name = entry.file_name();
        let name_str = match name.to_str() {
            Some(s) => s,
            None => continue,
        };
        let pid: u32 = match name_str.parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        // Read ppid and name from /proc/<pid>/stat
        let stat_path = format!("/proc/{}/stat", pid);
        let Ok(content) = std::fs::read_to_string(&stat_path) else {
            continue;
        };
        let Some(close_paren) = content.rfind(')') else {
            continue;
        };
        let comm_raw = &content[..close_paren];
        // Extract comm between first '(' and last ')'
        let comm = comm_raw
            .split_once('(')
            .map(|(_, name)| name.to_string())
            .unwrap_or_default();
        let rest = &content[close_paren + 1..];
        let mut fields = rest.split_whitespace();
        fields.next(); // state
        let ppid: u32 = fields.next().and_then(|s| s.parse().ok()).unwrap_or(0);
        // RSS is field 24 in /proc/<pid>/stat
        let rss_kib: u64 = fields
            .nth(21) // field 24 (0-indexed: state=1, ppid=2, ..., rss=24 -> skip 4-23 = 20 fields, then next is 24)
            .unwrap_or("0")
            .parse()
            .unwrap_or(0);
        all_procs.push((pid, ppid, comm, rss_kib));
    }

    // BFS from root_pid
    let mut tree = DescendantTree::default();
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(root_pid);
    let mut visited: std::collections::HashSet<u32> = std::collections::HashSet::new();

    while let Some(current_pid) = queue.pop_front() {
        for &(pid, ppid, ref name, rss) in &all_procs {
            if ppid == current_pid && !visited.contains(&pid) && pid != root_pid {
                visited.insert(pid);
                tree.total += 1;
                *tree.by_name.entry(name.clone()).or_insert(0) += 1;
                tree.combined_rss_kib += rss;
                queue.push_back(pid);
            }
        }
    }

    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sample_current_process() {
        let pid = std::process::id();
        let sample = sample(pid, false);
        assert!(sample.is_ok(), "should be able to sample current process");
        let s = sample.unwrap();
        assert!(s.rss_kib > 0, "RSS should be positive");
        assert_eq!(s.worker_rss_kib, 0, "no worker expected");
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
        assert_eq!(s.worker_rss_kib, 0);
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

    #[test]
    fn test_find_child_worker_rss_no_worker() {
        let pid = std::process::id();
        let rss = find_child_worker_rss(pid);
        assert_eq!(rss, 0, "no worker child expected for memcheck process");
    }

    #[test]
    fn test_is_child_of_self() {
        let pid = std::process::id();
        // Our own process is not a child of itself
        assert!(!is_child_of(pid, pid));
    }

    #[test]
    fn test_gpu_sample_returns_some_on_amdgpu_or_none_elsewhere() {
        let s = sample_gpu();
        // On a box with ROCm: vram_used_mib is Some. On headless CI: all None.
        // Either is valid; we just assert it doesn't panic and the struct is usable.
        let _ = s.vram_used_mib;
        let _ = s.gpu_utilization_pct;
        let _ = s.provider;
    }

    #[test]
    fn test_gpu_sample_is_consistent() {
        // Two calls should return consistent types (both Some or both None for provider).
        let s1 = sample_gpu();
        let s2 = sample_gpu();
        assert_eq!(s1.provider.is_some(), s2.provider.is_some());
    }

    #[test]
    fn test_gpu_sample_default_is_all_none() {
        let s = GpuSample::default();
        assert!(s.vram_used_mib.is_none());
        assert!(s.gpu_utilization_pct.is_none());
        assert!(s.provider.is_none());
    }

    #[test]
    fn test_heap_profile_writes_file() {
        let tmp = tempfile::tempdir().unwrap();
        let p = capture_heap_profile(std::process::id(), "test", "before", tmp.path()).unwrap();
        assert!(p.exists(), "heap profile file should exist");
        assert!(
            p.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .contains("test_before")
        );
        let metadata = std::fs::metadata(&p).unwrap();
        assert!(metadata.len() > 0, "heap profile file should be non-empty");
    }

    #[test]
    fn test_heap_profile_missing_pid() {
        let tmp = tempfile::tempdir().unwrap();
        let result = capture_heap_profile(u32::MAX, "test", "before", tmp.path());
        assert!(result.is_err(), "missing pid should produce an error");
    }

    #[test]
    fn test_count_descendants_includes_spawned_child() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .unwrap();
        // Brief wait for /proc to reflect the child
        std::thread::sleep(std::time::Duration::from_millis(100));

        let tree = count_descendants(std::process::id()).unwrap();
        assert!(
            tree.total >= 1,
            "should find at least 1 descendant (the sleep child), got {}",
            tree.total
        );
        // The child should appear in by_name
        assert!(
            tree.by_name.contains_key("sleep") || tree.by_name.values().sum::<usize>() >= 1,
            "by_name should contain the child process"
        );
        child.kill().ok();
        child.wait().ok();

        // After killing, OUR child must be gone from the process table.
        // Asserting a global total==0 races with sibling tests' transient
        // children (chrono_now's `date`, git probes) under parallel test
        // threads, so scope the assertion to the child we spawned.
        std::thread::sleep(std::time::Duration::from_millis(100));
        let child_pid = child.id();
        let child_gone = !std::path::Path::new(&format!("/proc/{child_pid}")).exists();
        let tree_after = count_descendants(std::process::id()).unwrap();
        assert!(
            child_gone && !tree_after.by_name.contains_key("sleep"),
            "after killing the child (pid {child_pid}), it must no longer be a descendant: {:?}",
            tree_after.by_name
        );
    }

    #[test]
    fn test_count_descendants_no_children() {
        // The test process might have residual children from other tests
        // running concurrently (e.g. test_count_descendants_includes_spawned_child
        // spawns a "sleep"). We only check that total is small (no big tree).
        let tree = count_descendants(std::process::id()).unwrap();
        assert!(
            tree.total < 5,
            "test process should have very few descendants: {} found: {:?}",
            tree.total,
            tree.by_name
        );
    }

    #[test]
    fn test_descendant_tree_serde_roundtrip() {
        let mut by_name = HashMap::new();
        by_name.insert("leindex-embed".to_string(), 1);
        let tree = DescendantTree {
            total: 1,
            by_name,
            combined_rss_kib: 50000,
        };
        let json = serde_json::to_string(&tree).unwrap();
        let deserialized: DescendantTree = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, tree);
    }
}
