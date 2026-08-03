//! Worker environment-configuration helpers (T5/T6 memory-pressure batch).
//!
//! Extracted from `runtime.rs` so that file stays comfortably under the
//! 2000-line Large-File gate. Holds:
//! - ONNX inference batch-size / sequence-length tuning (env-tunable).
//! - The `LEINDEX_WORKER_ORT_THREADS` default (T5).
//! - The T6 RSS / `MemAvailable` guards (`process_rss_kib`, `mem_available_kib`,
//!   `low_memory_refusal`).

/// Default maximum single-text size in bytes (1 MiB).
pub const DEFAULT_MAX_SEQ_LEN: usize = 128;

/// Default maximum texts per ONNX inference call for legacy fixed-batch models.
/// `pub(crate)` because `runtime_test.rs` asserts against these defaults.
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const DEFAULT_ONNX_INFERENCE_BATCH_SIZE: usize = 1;
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const DEFAULT_DYNAMIC_ONNX_INFERENCE_BATCH_SIZE: usize = 32;
/// MIGraphX compiles the first input shape into a fixed program. Every request
/// in a session must therefore use one stable batch dimension.
pub const DEFAULT_MIGRAPHX_INFERENCE_BATCH_SIZE: usize = 8;
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
const MAX_ONNX_INFERENCE_BATCH_SIZE: usize = 256;
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const ONNX_INFERENCE_BATCH_SIZE_ENV: &str = "LEINDEX_ONNX_INFERENCE_BATCH_SIZE";
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
const MIN_ONNX_SEQUENCE_LEN: usize = 8;
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const MAX_ONNX_SEQUENCE_LEN: usize = 512;
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const ONNX_SEQUENCE_LEN_ENV: &str = "LEINDEX_ONNX_SEQUENCE_LEN";
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const ONNX_LOG_SHAPES_ENV: &str = "LEINDEX_ONNX_LOG_SHAPES";
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const MIGRAPHX_FP16_ENV: &str = "LEINDEX_MIGRAPHX_FP16";
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const MIGRAPHX_EXHAUSTIVE_TUNE_ENV: &str = "LEINDEX_MIGRAPHX_EXHAUSTIVE_TUNE";

/// Profile directory holding compiled MIGraphX `.mxr` programs. Set on the
/// worker process by the embedding client; keyed on model + batch + seq (NOT
/// package version) so a release bump does not invalidate the cache.
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) const MIGRAPHX_MODEL_CACHE_PATH_ENV: &str = "ORT_MIGRAPHX_MODEL_CACHE_PATH";

#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub fn configured_onnx_inference_batch_size(model_name: &str, provider: &str) -> usize {
    std::env::var(ONNX_INFERENCE_BATCH_SIZE_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v > 0)
        .map(|v| v.min(MAX_ONNX_INFERENCE_BATCH_SIZE))
        .unwrap_or_else(|| {
            if provider.eq_ignore_ascii_case("migraphx") || provider.eq_ignore_ascii_case("rocm") {
                DEFAULT_MIGRAPHX_INFERENCE_BATCH_SIZE
            } else if model_name.ends_with("-dynamic") {
                DEFAULT_DYNAMIC_ONNX_INFERENCE_BATCH_SIZE
            } else {
                DEFAULT_ONNX_INFERENCE_BATCH_SIZE
            }
        })
}

#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub fn configured_onnx_sequence_len() -> usize {
    std::env::var(ONNX_SEQUENCE_LEN_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&v| v >= MIN_ONNX_SEQUENCE_LEN)
        .map(|v| v.min(MAX_ONNX_SEQUENCE_LEN))
        .unwrap_or(DEFAULT_MAX_SEQ_LEN)
}

#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            matches!(
                value.to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

/// Default ONNX intra-op thread count: 75% of available parallelism, floored
/// at 2 and capped at the actual parallelism (T5) — a 1-core box gets 1 thread,
/// never more than the hardware provides. Kept out of `RuntimeConfig::default`'s
/// literal so tests and callers share one source of truth.
pub(crate) fn default_ort_threads() -> usize {
    std::thread::available_parallelism()
        .map(|n| {
            let n = n.get();
            ((n.saturating_mul(3) / 4).max(2)).min(n)
        })
        .unwrap_or(2)
}

/// Resident set size of this process in KiB (T6), from `/proc/self/statm`.
/// Linux-only: the `procfs`-style reads do not exist on macOS/Windows, so the
/// T6 RSS guard is a documented no-op there (returns `None`).
#[cfg(target_os = "linux")]
pub(crate) fn process_rss_kib() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let resident_pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
    // SAFETY: sysconf(_SC_PAGESIZE) is a plain scalar libc call.
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if page_size <= 0 {
        return None;
    }
    Some(resident_pages.saturating_mul(page_size as u64) / 1024)
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn process_rss_kib() -> Option<u64> {
    None
}

/// System `MemAvailable` in KiB (T6), from `/proc/meminfo`. Linux-only;
/// returns `None` elsewhere so the refusal guard is a documented no-op.
#[cfg(target_os = "linux")]
pub(crate) fn mem_available_kib() -> Option<u64> {
    let contents = std::fs::read_to_string("/proc/meminfo").ok()?;
    let line = contents
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))?;
    line.split_whitespace().nth(1)?.parse().ok()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn mem_available_kib() -> Option<u64> {
    None
}

/// Position IDs tensor for models that declare a `position_ids` input.
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) fn build_position_ids(batch_size: usize, sequence_len: usize) -> Vec<i64> {
    (0..batch_size)
        .flat_map(|_| (0..sequence_len).map(|position| position as i64))
        .collect()
}

/// Remove all but the `keep` newest `.mxr` files from a cache dir.
///
/// Without this, every model/shape/version change leaves a ~1.2 GB orphan
/// (MIGraphX JIT artifacts are large), so the cache grows without bound across
/// releases. Keeping one is sufficient because the active profile recompiles
/// into the same deterministic `compiled.mxr` path.
#[cfg_attr(not(feature = "onnx"), allow(dead_code))]
pub(crate) fn prune_migraphx_cache(dir: &std::path::Path, keep: usize) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut mxr: Vec<(std::path::PathBuf, std::time::SystemTime)> = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "mxr"))
        .filter_map(|path| {
            std::fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .map(|mtime| (path, mtime))
        })
        .collect();
    mxr.sort_by_key(|item| std::cmp::Reverse(item.1)); // newest first
    for (path, _) in mxr.into_iter().skip(keep) {
        match std::fs::remove_file(&path) {
            Ok(()) => tracing::debug!("pruned stale MIGraphX cache file: {}", path.display()),
            Err(error) => {
                tracing::warn!(
                    "failed to prune stale MIGraphX cache file {}: {}",
                    path.display(),
                    error
                )
            }
        }
    }
}

/// Current unix time in milliseconds (used for startup reporting).
pub(crate) fn unix_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}
