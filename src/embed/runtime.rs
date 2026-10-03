// Worker runtime lifecycle
//
// Implements the worker process lifecycle:
// - Cold start on first embed demand (VAL-CPHASE-005)
// - Reuse across successive batches before idle timeout (VAL-CPHASE-006)
// - Idle timeout teardown (VAL-CPHASE-007)
// - Restart on later demand after teardown (VAL-CPHASE-008)
// - Local IPC only (VAL-CPHASE-004)
//
// The runtime wraps the ONNX session and tokenizer, providing an idle
// timer that tracks time since last activity. When the idle timeout
// elapses, the runtime reports that teardown is due. The main loop
// checks this and exits cleanly so the main daemon can respawn on
// next demand.

use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::embed::model_path::ModelResolver;
use crate::embed::protocol::{
    self, BatchId, EmbedResponse, ErrorKind, Frame, MsgType, Request, RerankResponse, WorkerError,
};
use crate::embed::provider::ExecutionProviderSelector;
use crate::embed::startup::{StartupReport, StartupReporter};

// ONNX Runtime imports - only available with "onnx" feature
#[cfg(feature = "onnx")]
use ort::logging::LogLevel;
#[cfg(feature = "onnx")]
use ort::session::{Session, builder::GraphOptimizationLevel, builder::SessionBuilder};

/// Default idle timeout in seconds before the worker tears itself down.
///
/// Reduced from 300s (5 min) to 60s (1 min) to limit the window during which
/// orphaned worker processes can accumulate. Combined with PR_SET_PDEATHSIG
/// (set in the worker's `main()`), this bounds stale worker lifetime so the
/// ~1.5 GB ROCm/MIGraphX runtime held by each worker is reclaimed quickly
/// after the parent leindex process exits.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 60; // 1 minute

/// Default maximum outgoing frame size in bytes (16 MiB).
pub const DEFAULT_MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Default maximum single-text size in bytes (1 MiB).
pub const DEFAULT_MAX_TEXT_SIZE: usize = 1024 * 1024;

/// Read buffer capacity for BufReader on the IPC data path.
///
/// VAL-DAEMON-006: A 128KB buffer reduces the number of `read()` syscalls
/// for large embedding responses (e.g., 1024-dim x 32 batch x 4 bytes =
/// 128KB fits in a single read instead of many small reads).
pub const READ_BUF_CAPACITY: usize = 128 * 1024;

/// Sentinel prefix embedded in the error message when the ONNX model returns
/// a collapsed `[1, seq_len, hidden_dim]` output despite receiving a batch
/// with `batch_size > 1`. `run_onnx_embed_sub_batch` matches on this prefix
/// to retry each sequence individually rather than falling back to TF-IDF.
#[cfg(feature = "onnx")]
const COLLAPSED_BATCH_SENTINEL: &str = "__COLLAPSED_BATCH__";

use crate::embed::runtime_env::{
    DEFAULT_MAX_RSS_MB, DEFAULT_MIN_AVAILABLE_MB, MIGRAPHX_EXHAUSTIVE_TUNE_ENV, MIGRAPHX_FP16_ENV,
    MIGRAPHX_MODEL_CACHE_PATH_ENV, ONNX_LOG_SHAPES_ENV, build_position_ids, default_ort_threads,
    env_flag, mem_available_kib, process_rss_kib, prune_migraphx_cache, unix_now_ms,
};
pub use crate::embed::runtime_env::{
    DEFAULT_MAX_SEQ_LEN, DEFAULT_MIGRAPHX_INFERENCE_BATCH_SIZE,
    configured_onnx_inference_batch_size, configured_onnx_sequence_len,
};

mod onnx_session;

mod past_key_values;
pub(crate) use past_key_values::KvInput;

mod onnx_embed;

mod rerank;

#[cfg(feature = "onnx")]
fn extract_output_tensor_f32(value: &ort::value::DynValue) -> Result<Vec<f32>, String> {
    // Quantization parameters carried over from the upstream
    // electroglyph/Qwen3-Embedding-0.6B-onnx-uint8 export. The default model
    // (ScooterLacroix/qwen3-embed-0.6b-int4-code fine-tune) emits a plain f32
    // last_hidden_state and never hits this branch; the constants stay for
    // outputs from the upstream export. The model applies
    // QuantizeLinear with these constants to its
    // L2-normalized sentence_embedding output. Dequantization formula:
    //   float_value = (uint8_value - zero_point) * scale
    const UINT8_DEQUANT_SCALE: f32 = 0.002_745_098;
    const UINT8_DEQUANT_ZERO_POINT: f32 = 109.0;

    match value.try_extract_array::<f32>() {
        Ok(values) => Ok(values.iter().copied().collect()),
        Err(f32_error) => match value.try_extract_array::<half::f16>() {
            Ok(values) => Ok(values.iter().map(|value| value.to_f32()).collect()),
            Err(f16_error) => match value.try_extract_array::<u8>() {
                Ok(values) => Ok(values
                    .iter()
                    .map(|&value| (value as f32 - UINT8_DEQUANT_ZERO_POINT) * UINT8_DEQUANT_SCALE)
                    .collect()),
                Err(u8_error) => Err(format!(
                    "output is neither f32 ({}) nor f16 ({}) nor u8 ({})",
                    f32_error, f16_error, u8_error
                )),
            },
        },
    }
}
/// T6 low-memory refusal: when `min_available_mb` is configured and the system
/// has less `MemAvailable` than that, return the refusal reason so the caller
/// can abort BEFORE loading the (multi-GiB) ONNX model. `None` when unset or
/// when `MemAvailable` cannot be determined (no-op, documented).
///
/// RAM safety: the floor is checked against MemAvailable MINUS the resident
/// memory of any sibling `leindex-embed` processes. The stress-test OOM had
/// two workers each passing the floor alone while jointly exhausting the
/// cgroup — the second model load must price in the first.
pub(crate) fn low_memory_refusal(config: &RuntimeConfig) -> Option<String> {
    let min_mb = config.min_available_mb?;
    let available_kib = mem_available_kib()?;
    let siblings_kib = sibling_embed_workers_rss_kib().unwrap_or(0);
    (available_kib.saturating_sub(siblings_kib) < min_mb.saturating_mul(1024)).then(|| {
        format!(
            "system MemAvailable is {} KiB ({} KiB already held by other leindex-embed \
             workers), below LEINDEX_WORKER_MIN_AVAILABLE_MB={} MB; refusing to load \
             the ONNX model (memory-pressure T6)",
            available_kib, siblings_kib, min_mb
        )
    })
}

/// Sum of resident memory (KiB) of OTHER `leindex-embed` processes. Best
/// effort: unreadable entries are skipped; non-Linux returns `None`.
#[cfg(target_os = "linux")]
fn sibling_embed_workers_rss_kib() -> Option<u64> {
    let self_pid = std::process::id();
    let mut total = 0u64;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        if pid == self_pid {
            continue;
        }
        let comm = std::fs::read_to_string(format!("/proc/{pid}/comm"))
            .unwrap_or_default()
            .trim()
            .to_string();
        if !comm.starts_with("leindex-embed") {
            continue;
        }
        let Ok(status) = std::fs::read_to_string(format!("/proc/{pid}/status")) else {
            continue;
        };
        for line in status.lines() {
            if let Some(rest) = line.strip_prefix("VmRSS:") {
                let kib = rest
                    .trim()
                    .trim_end_matches("kB")
                    .trim()
                    .parse::<u64>()
                    .unwrap_or(0);
                total += kib;
                break;
            }
        }
    }
    Some(total)
}

#[cfg(not(target_os = "linux"))]
fn sibling_embed_workers_rss_kib() -> Option<u64> {
    None
}

/// WS10 Task 7: Sample GPU VRAM usage in MiB.
///
/// Reads VRAM from `rocm-smi` (AMD) or `nvidia-smi` (NVIDIA) on Linux.
/// Returns `None` on headless boxes or when GPU tools are unavailable.
fn sample_gpu_vram() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        // Try ROCm first (AMD GPUs on this machine).
        if let Ok(output) = std::process::Command::new("rocm-smi")
            .args(["--showmeminfo", "vram", "--json"])
            .output()
        {
            if output.status.success() {
                if let Ok(json) = serde_json::from_slice::<serde_json::Value>(&output.stdout) {
                    // ROCm JSON: { "card0": { "VRAM Total Memory (B)": N, "VRAM Total Used Memory (B)": M } }
                    for (_card, info) in json.as_object().iter().flat_map(|o| o.iter()) {
                        if let Some(used_str) = info.get("VRAM Total Used Memory (B)") {
                            if let Some(used_b) = used_str
                                .as_str()
                                .and_then(|s| s.trim().parse::<u64>().ok())
                                .or_else(|| used_str.as_u64())
                            {
                                return Some(used_b / (1024 * 1024));
                            }
                        }
                    }
                }
            }
        }
        // Try nvidia-smi (NVIDIA GPUs).
        if let Ok(output) = std::process::Command::new("nvidia-smi")
            .args(["--query-gpu=memory.used", "--format=csv,noheader,nounits"])
            .output()
        {
            if output.status.success() {
                if let Ok(text) = std::str::from_utf8(&output.stdout) {
                    if let Some(first_line) = text.lines().next() {
                        if let Ok(mib) = first_line.trim().parse::<u64>() {
                            return Some(mib);
                        }
                    }
                }
            }
        }
    }
    None
}

/// WS10 Task 5: Open the global embedding cache if the feature flag is enabled.
///
/// The cache lives at the user level (`~/.leindex/embed-cache/`). When the
/// `LEINDEX_FEATURE_GLOBAL_EMBED_CACHE` flag is OFF, returns `None` and the
/// worker operates in legacy mode (no cache probing).
fn open_cache_if_enabled() -> Option<Arc<Mutex<crate::embed::cache::GlobalEmbeddingCache>>> {
    if !crate::feature_flags::FeatureFlag::GlobalEmbedCache.is_enabled() {
        return None;
    }
    let cache_root = default_embed_cache_root();
    match crate::embed::cache::GlobalEmbeddingCache::open(&cache_root) {
        Ok(cache) => {
            tracing::info!("global embedding cache opened at {}", cache_root.display());
            Some(Arc::new(Mutex::new(cache)))
        }
        Err(e) => {
            tracing::warn!(
                "failed to open global embedding cache at {}: {}; cache disabled",
                cache_root.display(),
                e
            );
            None
        }
    }
}

/// Default user-level path for the embedding cache.
fn default_embed_cache_root() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("LEINDEX_HOME") {
        return std::path::PathBuf::from(home).join("embed-cache");
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join(".leindex")
            .join("embed-cache");
    }
    // Fallback: relative path (unusual but avoids panic).
    std::path::PathBuf::from(".leindex").join("embed-cache")
}

/// Configuration for the worker runtime.
#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    /// Idle timeout before the worker exits.
    pub idle_timeout: Duration,
    /// Maximum frame size for outgoing IPC frames.
    pub max_frame_size: usize,
    /// Maximum single-text size before truncation.
    pub max_text_size: usize,
    /// Model name to load.
    pub model_name: String,
    /// Embedding dimension.
    pub embedding_dim: usize,
    /// Requested execution provider.
    pub execution_provider: String,
    /// Reranker cross-encoder model name (loaded on demand). Empty disables
    /// reranking (handle_rerank returns passthrough scores).
    pub rerank_model_name: String,
    /// ONNX intra-op thread count (T5). Bounding ORT's thread pool is a
    /// memory-pressure lever: each ORT thread carries a stack + per-thread
    /// arena, and the worker already holds a multi-GiB model. Honored via
    /// `LEINDEX_WORKER_ORT_THREADS`; defaults to 75% of available parallelism
    /// (floored at 2).
    pub ort_threads: usize,
    /// Optional RSS cap in MiB (T6). When the worker's resident set exceeds
    /// this, it self-exits so the parent respawns a lean worker instead of
    /// compounding swap pressure. Honored via `LEINDEX_WORKER_MAX_RSS_MB`.
    pub max_rss_mb: Option<u64>,
    /// Optional system `MemAvailable` floor in MiB (T6). The worker refuses to
    /// load its ONNX model when less is available. Honored via
    /// `LEINDEX_WORKER_MIN_AVAILABLE_MB`.
    pub min_available_mb: Option<u64>,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            max_text_size: DEFAULT_MAX_TEXT_SIZE,
            model_name: "qwen3-embed-0.6b-dynamic-uint8".to_string(),
            embedding_dim: 1024,
            // Default to "auto" which will detect the best available provider.
            // The worker will try MIGraphX (AMD GPU), then CUDA, then CPU.
            execution_provider: "auto".to_string(),
            rerank_model_name: "qwen3-reranker-0.6b-seq-cls".to_string(),
            ort_threads: default_ort_threads(),
            max_rss_mb: None,
            min_available_mb: None,
        }
    }
}

impl RuntimeConfig {
    /// Create config from environment variables.
    pub fn from_env() -> Self {
        let idle_timeout = std::env::var("LEINDEX_WORKER_IDLE_TIMEOUT")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .map(Duration::from_secs)
            .unwrap_or(Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS));

        let max_frame_size = std::env::var("LEINDEX_WORKER_MAX_FRAME_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_FRAME_SIZE);

        let max_text_size = std::env::var("LEINDEX_WORKER_MAX_TEXT_SIZE")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(DEFAULT_MAX_TEXT_SIZE);

        let model_name = std::env::var("LEINDEX_WORKER_MODEL")
            .ok()
            .or_else(|| {
                // VAL-DAEMON-002: Use load_cached() so config TOML is parsed
                // at most once per process via OnceLock.
                let cfg = crate::config::LeIndexConfig::load_cached();
                let name = cfg.neural.model_name.clone();
                if name.trim().is_empty() {
                    None
                } else {
                    Some(name)
                }
            })
            .unwrap_or_else(|| "qwen3-embed-0.6b-dynamic-uint8".to_string());

        let embedding_dim = std::env::var("LEINDEX_WORKER_EMBEDDING_DIM")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1024);

        let execution_provider = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .or_else(|| {
                // VAL-DAEMON-002: Use load_cached() so config TOML is parsed
                // at most once per process via OnceLock.
                let value = crate::config::LeIndexConfig::load_cached()
                    .neural
                    .execution_provider
                    .trim()
                    .to_ascii_lowercase();
                (!value.is_empty()).then_some(value)
            })
            .unwrap_or_else(|| "auto".to_string());

        let rerank_model_name = std::env::var("LEINDEX_WORKER_RERANK_MODEL")
            .ok()
            .unwrap_or_else(|| "qwen3-reranker-0.6b-seq-cls".to_string());

        // T5: bounded intra-op thread pool (memory-pressure lever).
        let ort_threads = std::env::var("LEINDEX_WORKER_ORT_THREADS")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .filter(|&v| v > 0)
            .unwrap_or_else(default_ort_threads);

        // T6: RSS self-exit cap + MemAvailable refusal floor. The RSS cap
        // defaults to disabled (0 = off per .env.example); the MemAvailable
        // floor defaults to the documented 2048 MiB so the guard is active even
        // when the env var is unset — an unset variable must not silently
        // bypass the refusal (Codex P1).
        let max_rss_mb = match std::env::var("LEINDEX_WORKER_MAX_RSS_MB") {
            Ok(v) => match v.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(n) => Some(n),
                Err(_) => {
                    tracing::warn!(
                        value = %v,
                        "malformed LEINDEX_WORKER_MAX_RSS_MB; falling back to \
                         {} MiB (memory-pressure T6)",
                        DEFAULT_MAX_RSS_MB
                    );
                    Some(DEFAULT_MAX_RSS_MB)
                }
            },
            // Default 8192 MiB: the stress-test OOM post-mortem found two
            // concurrently-resident embed daemons (total_vm ~15 GiB each)
            // pushing a shared cgroup past its limits. An unset variable
            // must not mean "unbounded"; `0` disables.
            Err(_) => Some(DEFAULT_MAX_RSS_MB),
        };
        let min_available_mb = match std::env::var("LEINDEX_WORKER_MIN_AVAILABLE_MB") {
            Ok(v) => match v.trim().parse::<u64>() {
                Ok(0) => None,
                Ok(n) => Some(n),
                Err(_) => {
                    // A malformed override must not silently bypass the
                    // guard (Codex P2): keep the documented default floor
                    // instead of resolving to `None` like an explicit `0`.
                    tracing::warn!(
                        value = %v,
                        "malformed LEINDEX_WORKER_MIN_AVAILABLE_MB; falling back to \
                         {} MiB (memory-pressure T6)",
                        DEFAULT_MIN_AVAILABLE_MB
                    );
                    Some(DEFAULT_MIN_AVAILABLE_MB)
                }
            },
            Err(_) => Some(DEFAULT_MIN_AVAILABLE_MB),
        };

        Self {
            idle_timeout,
            max_frame_size,
            max_text_size,
            model_name,
            embedding_dim,
            execution_provider,
            rerank_model_name,
            ort_threads,
            max_rss_mb,
            min_available_mb,
        }
    }
}

/// Worker runtime state.
///
/// Tracks the idle timer and provides the main request-processing loop.
///
/// When built with the `onnx` feature, also holds the ONNX session and tokenizer
/// for neural embedding inference.
#[derive(Clone)]
pub struct WorkerRuntime {
    config: RuntimeConfig,
    last_activity: Arc<Mutex<Instant>>,
    shutdown_flag: Arc<AtomicBool>,
    started_unix_ms: u64,

    /// Active embed cancellation tokens, keyed by wire BatchId. Socket worker
    /// handlers share the runtime, so cancellation must be request-scoped:
    /// one request may never reset or cancel another request's work.
    active_embed_cancels: Arc<Mutex<HashMap<BatchId, Arc<AtomicBool>>>>,

    /// WS10 Task 5: Global embedding cache (opened when the GlobalEmbedCache
    /// feature flag is enabled). Wrapped in `Mutex` because cache writes (put,
    /// add_reference) require `&mut self`.
    cache: Option<Arc<Mutex<crate::embed::cache::GlobalEmbeddingCache>>>,

    /// ONNX session for neural embedding inference. Only available with `onnx` feature.
    #[cfg(feature = "onnx")]
    session: Option<Arc<Mutex<Session>>>,

    #[cfg(feature = "onnx")]
    input_names: Arc<OnceLock<(bool, bool)>>,

    /// Declared `past_key_values.*` KV-cache inputs (empty for BERT/GTE-style
    /// models). Cached once per runtime; feeds zero-length caches on the
    /// fresh-pass inference path.
    #[cfg(feature = "onnx")]
    kv_inputs: Arc<OnceLock<Vec<KvInput>>>,

    /// Tokenizer for text preprocessing. Only available with `onnx` feature.
    #[cfg(feature = "onnx")]
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,

    /// Model load time for startup reporting.
    #[cfg(feature = "onnx")]
    model_load_time: Duration,

    /// Actual provider status observed while building the ONNX session.
    #[cfg(feature = "onnx")]
    provider_runtime_status: ProviderRuntimeStatus,

    /// Lazy on-demand reranker cross-encoder session. `None` until the first
    /// rerank request loads it; evicted after `RERANK_IDLE_EVICTION_SECS` of
    /// rerank idleness to free memory/VRAM. Held as `Arc<Mutex<Option<...>>>`
    /// so it can be loaded + evicted through the shared `Arc<WorkerRuntime>` in
    /// the socket path (handle_rerank takes &self). Separate from `session`
    /// (the embedder) so the reranker only costs resources while in use.
    #[cfg(feature = "onnx")]
    rerank_session: Arc<Mutex<Option<Arc<Mutex<Session>>>>>,
    #[cfg(feature = "onnx")]
    rerank_tokenizer: Arc<Mutex<Option<Arc<tokenizers::Tokenizer>>>>,
    /// Last time the reranker was used (for idle eviction).
    #[cfg(feature = "onnx")]
    last_rerank_activity: Arc<Mutex<Instant>>,
    /// Serializes reranker lazy-load (prevents double-load on concurrent
    /// socket requests).
    #[cfg(feature = "onnx")]
    rerank_init_lock: Arc<Mutex<()>>,
}

struct ActiveEmbedCancelGuard {
    batch_id: BatchId,
    token: Arc<AtomicBool>,
    registry: Arc<Mutex<HashMap<BatchId, Arc<AtomicBool>>>>,
}

impl Drop for ActiveEmbedCancelGuard {
    fn drop(&mut self) {
        let mut registry = self.registry.lock().unwrap_or_else(|p| p.into_inner());
        if registry
            .get(&self.batch_id)
            .is_some_and(|active| Arc::ptr_eq(active, &self.token))
        {
            registry.remove(&self.batch_id);
        }
    }
}

/// Reranker idle eviction threshold: after this many seconds with no rerank
/// request, the lazy rerank session + tokenizer are dropped to free memory.
#[cfg(feature = "onnx")]
const RERANK_IDLE_EVICTION_SECS: u64 = 120;

/// Rerank sequence length. Larger than the embed model's (128) because the
/// Qwen3-Reranker chat template (prefix + instruct + query + document + suffix)
/// is ~60 tokens before the document even starts; 128 would truncate the
/// document + the required assistant suffix. 512 covers typical code symbols.
#[cfg(feature = "onnx")]
const RERANK_MAX_SEQ_LEN: usize = 512;

#[cfg(feature = "onnx")]
#[derive(Debug, Clone)]
struct ProviderRuntimeStatus {
    execution_provider: String,
    provider_available: bool,
    fallback_reason: Option<String>,
}

#[cfg(feature = "onnx")]
impl ProviderRuntimeStatus {
    fn available(name: impl Into<String>) -> Self {
        Self {
            execution_provider: name.into(),
            provider_available: true,
            fallback_reason: None,
        }
    }

    fn fallback_to_cpu(reason: impl Into<String>) -> Self {
        Self {
            execution_provider: "cpu".to_string(),
            provider_available: false,
            fallback_reason: Some(reason.into()),
        }
    }
}

#[cfg(feature = "onnx")]
struct SessionBuildOutcome {
    session: Session,
    provider_status: ProviderRuntimeStatus,
}

/// Build the MIGraphX execution provider with the configured options.
#[cfg(feature = "onnx")]
fn build_migraphx_ep() -> ort::ep::ExecutionProviderDispatch {
    let mut ep = ort::ep::MIGraphX::default();
    // Compiled-program persistence is owned by ORT's native
    // `ORT_MIGraphX_MODEL_CACHE_PATH` cache (set on the worker by the embedding
    // client). Cold start JIT-compiles (~300 s) and writes the `.mxr`; warm start
    // loads it (~4 s). We do NOT use the crate-level save/load (under the
    // ort-crate/ORT struct skew those read an empty save path, collide with the
    // native cache, and a synthetic warmup makes the kernel fail). Only prune
    // stale `.mxr` files to bound the ~1.2 GB-per-shape growth.
    if let Ok(cache_dir_str) = std::env::var(MIGRAPHX_MODEL_CACHE_PATH_ENV) {
        // Keep enough .mxr for the embedder (b8 + query b1) AND the on-demand
        // reranker (b8 x 512) to coexist. The prior keep=1 made them evict each
        // other every process (mutual recompile). They share one cache dir.
        prune_migraphx_cache(std::path::Path::new(&cache_dir_str), 6);
    }
    if env_flag(MIGRAPHX_FP16_ENV) {
        tracing::info!("MIGraphX FP16 enabled via {}", MIGRAPHX_FP16_ENV);
        ep = ep.with_fp16(true);
    }
    if env_flag(MIGRAPHX_EXHAUSTIVE_TUNE_ENV) {
        tracing::info!(
            "MIGraphX exhaustive tune enabled via {}",
            MIGRAPHX_EXHAUSTIVE_TUNE_ENV
        );
        ep = ep.with_exhaustive_tune(true);
    }
    ep.build()
}

/// Try to attach `provider`; on failure, rebuild a fresh CPU-only session builder
/// and report the fallback. `status_name` is the name recorded on the runtime
/// status; `ep_label` is the (display) name used in the fallback log message.
#[cfg(feature = "onnx")]
fn try_provider_or_cpu(
    builder: SessionBuilder,
    provider: ort::ep::ExecutionProviderDispatch,
    status_name: &str,
    ep_label: &str,
    ort_threads: usize,
) -> Result<(SessionBuilder, ProviderRuntimeStatus), ort::Error> {
    match builder.with_execution_providers([provider]) {
        Ok(sb) => Ok((sb, ProviderRuntimeStatus::available(status_name))),
        Err(e) => {
            let reason = format!("{} EP not available: {}; falling back to CPU", ep_label, e);
            tracing::warn!("{}", reason);
            // T5: the CPU fallback must honor the intra-op thread cap too — an
            // unbound CPU session can spawn one thread per core on top of the
            // GPU session's pool.
            let cpu_builder = Session::builder()?
                .with_intra_threads(ort_threads)?
                .with_memory_pattern(false)?
                .with_log_level(LogLevel::Warning)?
                .with_optimization_level(GraphOptimizationLevel::Level1)?
                .with_execution_providers([ort::ep::CPU::default().build()])?;
            Ok((cpu_builder, ProviderRuntimeStatus::fallback_to_cpu(reason)))
        }
    }
}

/// VAL-ORT-015/016 pre-flight: if a GPU provider was selected but MIGraphX is not
/// compiled into the dynamically-loaded ORT binary, build a CPU-only session and
/// return it so the caller can short-circuit. Returns `None` to proceed normally.
#[cfg(feature = "onnx")]
fn maybe_missing_ep_fallback(
    model_path: &std::path::Path,
    provider_name: &str,
    ort_threads: usize,
) -> Result<Option<SessionBuildOutcome>, ort::Error> {
    if !matches!(provider_name, "migraphx" | "rocm")
        || crate::embed::provider::is_migraphx_compiled_in()
    {
        return Ok(None);
    }
    tracing::warn!(
        "MIGraphX EP not available in the dynamically loaded ONNX \
         Runtime binary (got provider_name={}, is_migraphx_compiled_in=false); \
         falling back to CPU. Install onnxruntime-migraphx or point \
         ORT_DYLIB_PATH at a migraphx-enabled libonnxruntime.",
        provider_name
    );
    let session = Session::builder()?
        .with_intra_threads(ort_threads)?
        .with_memory_pattern(false)?
        .with_log_level(LogLevel::Warning)?
        .with_optimization_level(GraphOptimizationLevel::Level1)?
        .with_execution_providers([ort::ep::CPU::default().build()])?
        .commit_from_file(model_path)?;
    Ok(Some(SessionBuildOutcome {
        session,
        provider_status: ProviderRuntimeStatus::fallback_to_cpu(
            "MIGraphX EP not available in the dynamically loaded ONNX Runtime binary",
        ),
    }))
}

/// Attach the selected execution provider to `builder`, falling back to CPU on
/// registration failure. `provider_name` is recorded verbatim on the status.
///
/// Invariants (enforced in debug builds):
/// - `provider_name` is a **concrete** provider (`cpu`/`cuda`/`migraphx`/
///   `rocm`/`coreml`), never the unresolved `"auto"` token. Auto must be
///   resolved by [`ExecutionProviderSelector::select`] before reaching here so
///   that the reranker and embedder sessions always see a concrete provider.
///
/// Registration uses `.error_on_failure()` so a failed GPU/CoreML EP load is
/// surfaced as a `Result::Err` rather than silently ignored. The explicit
/// GPU→CPU fallback is preserved by [`try_provider_or_cpu`], which catches
/// that `Err` and rebuilds a CPU session — so explicit-GPU-on-a-CPU-box still
/// works (with a neural-fallback warning), it is never a hard error.
#[cfg(feature = "onnx")]
fn attach_execution_provider(
    builder: SessionBuilder,
    provider_name: &str,
    ort_threads: usize,
) -> Result<(SessionBuilder, ProviderRuntimeStatus), ort::Error> {
    debug_assert!(
        provider_name != "auto",
        "attach_execution_provider received unresolved 'auto'; select() must run first"
    );
    match provider_name {
        "cuda" => try_provider_or_cpu(
            builder,
            ort::ep::CUDA::default().build().error_on_failure(),
            provider_name,
            "CUDA",
            ort_threads,
        ),
        // Explicit "migraphx". The "auto" token is resolved upstream by the
        // selector (CoreML → MIGraphX → CUDA → CPU) and must never reach here
        // — see the debug_assert above.
        "migraphx" => try_provider_or_cpu(
            builder,
            build_migraphx_ep().error_on_failure(),
            provider_name,
            "MIGraphX",
            ort_threads,
        ),
        // ROCm EP is deprecated in favor of MIGraphX and removed from ORT;
        // "rocm" is a backwards-compat alias that registers MIGraphX and falls
        // back to CPU if registration fails. ort::ep::ROCm is never registered.
        "rocm" => try_provider_or_cpu(
            builder,
            build_migraphx_ep().error_on_failure(),
            "migraphx",
            "MIGraphX (rocm alias)",
            ort_threads,
        ),
        "coreml" => try_provider_or_cpu(
            builder,
            ort::ep::CoreML::default().build().error_on_failure(),
            provider_name,
            "CoreML",
            ort_threads,
        ),
        _ => Ok((
            builder.with_execution_providers([ort::ep::CPU::default().build()])?,
            ProviderRuntimeStatus::available("cpu"),
        )),
    }
}

impl WorkerRuntime {
    /// Create a new worker runtime with the given configuration.
    ///
    /// When built with the `onnx` feature, also initializes the ONNX session and tokenizer
    /// for neural embedding inference.
    pub fn new(config: RuntimeConfig) -> Self {
        #[cfg(feature = "onnx")]
        let (session, tokenizer, model_load_time, provider_runtime_status) =
            Self::init_onnx(&config);

        Self {
            config,
            last_activity: Arc::new(Mutex::new(Instant::now())),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            started_unix_ms: unix_now_ms(),
            active_embed_cancels: Arc::new(Mutex::new(HashMap::new())),
            cache: open_cache_if_enabled(),
            #[cfg(feature = "onnx")]
            input_names: Arc::new(OnceLock::new()),
            #[cfg(feature = "onnx")]
            kv_inputs: Arc::new(OnceLock::new()),
            #[cfg(feature = "onnx")]
            rerank_session: Arc::new(Mutex::new(None)),
            #[cfg(feature = "onnx")]
            rerank_tokenizer: Arc::new(Mutex::new(None)),
            #[cfg(feature = "onnx")]
            last_rerank_activity: Arc::new(Mutex::new(Instant::now())),
            #[cfg(feature = "onnx")]
            rerank_init_lock: Arc::new(Mutex::new(())),
            #[cfg(feature = "onnx")]
            session,
            #[cfg(feature = "onnx")]
            tokenizer,
            #[cfg(feature = "onnx")]
            model_load_time,
            #[cfg(feature = "onnx")]
            provider_runtime_status,
        }
    }

    /// Whether this runtime can serve neural requests immediately.
    pub fn is_neural_ready(&self) -> bool {
        #[cfg(feature = "onnx")]
        {
            self.session.is_some() && self.tokenizer.is_some()
        }
        #[cfg(not(feature = "onnx"))]
        {
            true
        }
    }

    /// Accessors exposed to the GPU measurement bench (no behavior change).
    /// These are doc-hidden: the bench is the only public caller.
    #[cfg(feature = "onnx")]
    #[doc(hidden)]
    pub fn bench_session(&self) -> Option<Arc<Mutex<Session>>> {
        self.session.clone()
    }

    #[cfg(feature = "onnx")]
    #[doc(hidden)]
    pub fn bench_tokenizer(&self) -> Option<Arc<tokenizers::Tokenizer>> {
        self.tokenizer.clone()
    }

    #[cfg(feature = "onnx")]
    #[doc(hidden)]
    pub fn bench_provider_status(&self) -> &str {
        &self.provider_runtime_status.execution_provider
    }

    #[cfg(feature = "onnx")]
    #[doc(hidden)]
    pub fn bench_embed_dim(&self) -> usize {
        self.config.embedding_dim
    }

    #[cfg(feature = "onnx")]
    #[doc(hidden)]
    pub fn bench_model_name(&self) -> &str {
        &self.config.model_name
    }

    /// Detect the model's declared KV-cache inputs through the same code path
    /// the embed inference uses. Doc-hidden test/bench accessor.
    #[cfg(feature = "onnx")]
    #[doc(hidden)]
    pub fn bench_detect_kv_inputs(&self) -> Vec<KvInput> {
        let Some(session) = &self.session else {
            return Vec::new();
        };
        let guard = session.lock().unwrap_or_else(|e| e.into_inner());
        past_key_values::detect_kv_inputs(&guard)
    }

    #[cfg(feature = "onnx")]
    #[doc(hidden)]
    pub fn bench_run_onnx_embed<S: AsRef<str>>(
        &self,
        session: &Arc<Mutex<Session>>,
        tokenizer: &Arc<tokenizers::Tokenizer>,
        texts: &[S],
        expected_dim: usize,
        cancel_token: &Arc<AtomicBool>,
    ) -> Result<EmbedResponse, WorkerError> {
        self.run_onnx_embed(session, tokenizer, texts, expected_dim, cancel_token)
    }

    /// Build a control-plane health response without touching model work.
    pub fn health_response(
        &self,
        state: protocol::WorkerState,
        error: Option<String>,
    ) -> protocol::HealthResponse {
        #[cfg(feature = "onnx")]
        let provider = Some(self.provider_runtime_status.execution_provider.clone());
        #[cfg(not(feature = "onnx"))]
        let provider = Some(self.config.execution_provider.clone());

        // WS10 Task 4/7: compute model/tokenizer/config digests and measure
        // host RSS + GPU VRAM. All new fields are Option with #[serde(default)]
        // so the HealthResponse remains backward-compatible with older peers.
        let model_digest = self.compute_model_digest();
        let tokenizer_digest = self.compute_tokenizer_digest();
        let config_digest = self.compute_config_digest();
        let host_rss_mib = process_rss_kib().map(|kib| kib / 1024);
        let gpu_vram_mib = self.sample_gpu_vram_mib();
        let provider_compile_cache = self.provider_compile_cache_path();

        protocol::HealthResponse {
            state,
            phase: match state {
                protocol::WorkerState::Initializing => "initializing",
                protocol::WorkerState::Ready => "ready",
                protocol::WorkerState::Failed => "failed",
            }
            .to_string(),
            started_unix_ms: self.started_unix_ms,
            provider,
            model: self.config.model_name.clone(),
            error,
            model_digest,
            tokenizer_digest,
            config_digest,
            host_rss_mib,
            gpu_vram_mib,
            provider_compile_cache,
        }
    }

    /// Compute the blake3 digest of the loaded ONNX model weights (WS10 Task 4/7).
    fn compute_model_digest(&self) -> Option<[u8; 32]> {
        let model_path = ModelResolver::resolve(&self.config.model_name).ok()?;
        let model_bytes = std::fs::read(&model_path).ok()?;
        Some(blake3::hash(&model_bytes).into())
    }

    /// Compute the blake3 digest of the tokenizer configuration (WS10 Task 4/7).
    fn compute_tokenizer_digest(&self) -> Option<[u8; 32]> {
        let tokenizer_path = ModelResolver::resolve_tokenizer(&self.config.model_name).ok()?;
        let tokenizer_bytes = std::fs::read(&tokenizer_path).ok()?;
        Some(blake3::hash(&tokenizer_bytes).into())
    }

    /// Compute the blake3 digest of worker config that affects output vectors.
    fn compute_config_digest(&self) -> Option<[u8; 32]> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"LEINDEX-WORKER-CONFIG-V1");
        hasher.update(self.config.model_name.as_bytes());
        hasher.update(&self.config.embedding_dim.to_le_bytes());
        hasher.update(self.config.execution_provider.as_bytes());
        #[cfg(feature = "onnx")]
        hasher.update(&configured_onnx_sequence_len().to_le_bytes());
        #[cfg(feature = "onnx")]
        hasher.update(&self.config.ort_threads.to_le_bytes());
        Some(hasher.finalize().into())
    }

    /// Sample GPU VRAM allocation in MiB (WS10 Task 7).
    fn sample_gpu_vram_mib(&self) -> Option<u64> {
        sample_gpu_vram()
    }

    /// Provider compile-cache path, if configured (WS10 Task 7).
    fn provider_compile_cache_path(&self) -> Option<String> {
        std::env::var(MIGRAPHX_MODEL_CACHE_PATH_ENV).ok()
    }

    /// Emit the normal startup report after model initialization completes.
    pub fn log_startup_report(&self) {
        self.build_startup_report().log();
    }

    /// T6: whether the worker's resident set exceeds `LEINDEX_WORKER_MAX_RSS_MB`.
    /// When it does, the run loop (and the socket accept loop) self-exit so the
    /// parent can respawn a lean worker instead of holding a multi-GiB
    /// swapped-out model forever (the swap-saturation root cause this batch
    /// targets). Logs the trigger.
    pub(crate) fn rss_over_cap(&self) -> bool {
        let Some(max_mb) = self.config.max_rss_mb else {
            return false;
        };
        let Some(rss_kib) = process_rss_kib() else {
            return false;
        };
        if rss_kib <= max_mb.saturating_mul(1024) {
            return false;
        }
        tracing::warn!(
            rss_kib,
            max_rss_mb = max_mb,
            "worker RSS exceeds LEINDEX_WORKER_MAX_RSS_MB; self-exiting (memory-pressure T6)"
        );
        true
    }

    /// Get a handle to the shutdown flag for external signaling.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown_flag)
    }

    /// Check if the idle timeout has elapsed.
    pub fn is_idle_expired(&self) -> bool {
        self.last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .elapsed()
            >= self.config.idle_timeout
    }

    /// Reset the idle timer (called after each successful request).
    pub fn touch(&self) {
        *self
            .last_activity
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Instant::now();
    }

    /// Run the main IPC loop over the given reader/writer pair.
    ///
    /// This is the core event loop:
    /// 1. Read a frame from the IPC channel
    /// 2. Process the request
    /// 3. Write the response frame
    /// 4. Check idle timeout and exit if expired
    ///
    /// VAL-CPHASE-004: Uses local IPC only (stdin/stdout pipes or Unix socket).
    pub fn run<R: Read + Send + 'static, W: Write>(
        &self,
        reader: R,
        writer: W,
    ) -> anyhow::Result<()> {
        self.log_startup_report();

        self.run_loop(reader, writer)
    }

    /// Build the startup report based on current configuration.
    fn build_startup_report(&self) -> StartupReport {
        let mut reporter = StartupReporter::new();

        // Resolve model path
        let model_resolution = ModelResolver::resolve(&self.config.model_name);
        let (_model_path, _model_source) = match model_resolution {
            Ok(path) => {
                let source = ModelResolver::source_for_path(&path);
                reporter.set_model_path(&path, source);
                (Some(path), source.to_string())
            }
            Err(e) => {
                reporter.set_model_error(&e.to_string());
                (None, format!("error: {}", e))
            }
        };

        // Determine execution provider
        #[cfg(feature = "onnx")]
        {
            reporter.set_execution_provider(
                &self.provider_runtime_status.execution_provider,
                self.provider_runtime_status.provider_available,
                self.provider_runtime_status.fallback_reason.as_deref(),
            );
            // T5: explicit, actionable warning when a GPU provider was requested
            // but the worker fell back to CPU. A deliberate `cpu` configuration
            // has provider_available=true and no fallback_reason, so it stays
            // silent here — the CPU path is fully operational by user choice.
            if !self.provider_runtime_status.provider_available {
                if let Some(reason) = &self.provider_runtime_status.fallback_reason {
                    tracing::warn!(
                        reason = %reason,
                        provider = %self.provider_runtime_status.execution_provider,
                        "neural worker is on CPU after a GPU provider was requested; \
                         inference will be 100-1000x slower than GPU. Point ORT_DYLIB_PATH at \
                         a migraphx-enabled libonnxruntime, or set execution_provider=\"cpu\" to \
                         use CPU embeddings deliberately."
                    );
                }
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            let provider_result =
                ExecutionProviderSelector::select(&self.config.execution_provider);
            match provider_result {
                Ok(provider) => {
                    reporter.set_execution_provider(&provider.name(), true, None);
                }
                Err(fallback) => {
                    reporter.set_execution_provider(
                        &fallback.fallback_name(),
                        false,
                        Some(&fallback.reason()),
                    );
                }
            }
        }

        reporter.set_model_name(&self.config.model_name);
        reporter.set_quantization_mode("none"); // Will be updated when quantization is wired
        #[cfg(feature = "onnx")]
        reporter.set_warm_load_latency(self.model_load_time);
        #[cfg(not(feature = "onnx"))]
        reporter.set_warm_load_latency(Duration::from_millis(0)); // Placeholder until real ONNX load

        // VAL-ORT-022: surface the resolved ORT dylib path/source so operators
        // can verify which ORT the worker actually loaded. `last_ort_outcome()`
        // is populated by `ort_discovery::discover_and_init()` during
        // `init_onnx()`.
        if let Some(outcome) = crate::embed::ort_discovery::last_outcome() {
            reporter.set_ort_path(&outcome.path, outcome.source.as_str());
        }

        reporter.build()
    }

    /// Inner loop: read frames, process, respond, check idle.
    ///
    /// Uses a read timeout on the reader so that the idle timeout check
    /// at the top of the loop is reached even when no data is arriving.
    /// Without this, a blocking `read_exact` would block forever and the
    /// worker would never tear down its ONNX session on idle.
    pub fn run_loop<R: Read + Send + 'static, W: Write>(
        &self,
        reader: R,
        mut writer: W,
    ) -> anyhow::Result<()> {
        // Wrap the reader in a BufReader so we can call `set_read_timeout`
        // via the underlying handle. We use a cross-platform approach:
        // spawn a helper thread that reads and sends results via a channel.
        let (tx, rx) = std::sync::mpsc::channel();
        let read_timeout = Duration::from_secs(5);

        // Derive incoming frame size limit from config (with 2× headroom).
        let max_incoming_frame = self.config.max_frame_size.saturating_mul(2);

        // Reader helper thread: reads frames from the IPC channel and sends them
        // to the main loop via the `tx` channel.
        //
        // Lifecycle: the thread blocks on `read_exact`, which will return EOF when
        // the parent process closes the pipe (e.g., on shutdown or process exit).
        // When the main loop exits (idle timeout or shutdown), the `tx` sender is
        // dropped, causing the reader thread's `tx.send()` to fail and the thread
        // to break out of its loop. The thread is not joinable from this scope, but
        // it will exit naturally when either:
        //   1. The pipe closes (EOF on read_exact), or
        //   2. The `tx` channel is disconnected (main loop exited).
        std::thread::spawn(move || {
            // VAL-DAEMON-006: Use 128KB BufReader capacity to reduce syscall
            // count for large embedding requests and responses.
            let mut buf_reader = io::BufReader::with_capacity(READ_BUF_CAPACITY, reader);
            let mut frame_buf: Vec<u8> = Vec::new();
            loop {
                // Read 4-byte length prefix
                let mut len_buf = [0u8; 4];
                match buf_reader.read_exact(&mut len_buf) {
                    Ok(()) => {
                        let payload_len = u32::from_le_bytes(len_buf) as usize;
                        // Guard against unreasonably large frames BEFORE allocation
                        // to prevent OOM from a malicious or malfunctioning main process.
                        if payload_len > max_incoming_frame {
                            let _ = tx.send(Err(io::Error::new(
                                io::ErrorKind::InvalidData,
                                format!("incoming frame too large: {payload_len} bytes (max: {max_incoming_frame} bytes)"),
                            )));
                            break;
                        }
                        frame_buf.clear();
                        frame_buf.resize(payload_len, 0);
                        match buf_reader.read_exact(&mut frame_buf) {
                            Ok(()) => {
                                if tx.send(Ok(std::mem::take(&mut frame_buf))).is_err() {
                                    break; // Receiver dropped
                                }
                            }
                            Err(e) => {
                                let _ = tx.send(Err(e));
                                break;
                            }
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });

        loop {
            // Check external shutdown signal
            if self.shutdown_flag.load(Ordering::Relaxed) {
                tracing::info!("shutdown signal received, worker exiting");
                return Ok(());
            }

            // Check idle timeout
            if self.is_idle_expired() {
                tracing::info!(
                    "idle timeout ({:?}) expired, worker shutting down",
                    self.config.idle_timeout
                );
                return Ok(());
            }

            // T6: sustained-RSS self-exit guard (extracted so the loop body's
            // cyclomatic complexity stays bounded).
            if self.rss_over_cap() {
                return Ok(());
            }

            // Evict the on-demand reranker if it has been idle long enough to
            // free its memory/VRAM between rerank bursts. (Worker itself stays
            // alive for embeds; only the rerank session is reclaimed.)
            #[cfg(feature = "onnx")]
            self.maybe_evict_rerank();

            // Read frame with timeout so idle check fires periodically
            let frame_buf = match rx.recv_timeout(read_timeout) {
                Ok(Ok(buf)) => buf,
                Ok(Err(e)) => {
                    if e.kind() == io::ErrorKind::UnexpectedEof {
                        tracing::debug!("IPC channel closed, worker shutting down");
                        return Ok(());
                    }
                    return Err(anyhow::anyhow!("failed to read frame: {}", e));
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    // Read timed out — loop back to check idle expiry.
                    continue;
                }
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    tracing::debug!("IPC channel closed, worker shutting down");
                    return Ok(());
                }
            };

            let frame = Frame::from_wire_bytes(&frame_buf)?;
            let _batch_id = frame.header.batch_id;

            // Process the request
            let response = self.dispatch(&frame);

            // Write the response frame
            let wire = response.encode_wire()?;
            writer.write_all(&wire)?;
            writer.flush()?;

            // Reset idle timer after successful processing
            self.touch();
        }
    }

    /// Dispatch a request frame to the appropriate handler.
    pub fn dispatch(&self, frame: &Frame) -> Frame {
        // Reset the idle timer on request entry. Without this, the accept loop's
        // top-of-iteration idle check can kill the worker during a long-running
        // inference (e.g. the first MIGraphX JIT compile) even though it is
        // actively processing. `touch()` is also called after write and between
        // sub-batches for defense in depth.
        self.touch();
        let batch_id = frame.header.batch_id;

        match frame.header.msg_type {
            MsgType::EmbedRequest => match self.register_embed_cancel(batch_id) {
                Ok((_guard, cancel_token)) => match self.handle_embed(frame, &cancel_token) {
                    Ok(response) => protocol::embed_response_frame(batch_id, response)
                        .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
                    Err(e) => protocol::error_frame(batch_id, e)
                        .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
                },
                Err(e) => protocol::error_frame(batch_id, e)
                    .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
            },
            MsgType::RerankRequest => match self.handle_rerank(frame) {
                Ok(response) => protocol::rerank_response_frame(batch_id, response)
                    .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
                Err(e) => protocol::error_frame(batch_id, e)
                    .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
            },
            MsgType::HealthRequest => protocol::health_response_frame(
                batch_id,
                self.health_response(
                    if self.is_neural_ready() {
                        protocol::WorkerState::Ready
                    } else {
                        protocol::WorkerState::Failed
                    },
                    (!self.is_neural_ready())
                        .then(|| "neural runtime unavailable after initialization".to_string()),
                ),
            )
            .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
            MsgType::CacheProbe => match self.handle_cache_probe(frame) {
                Ok(response) => protocol::cache_probe_response_frame(batch_id, response)
                    .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
                Err(e) => protocol::error_frame(batch_id, e)
                    .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
            },
            MsgType::Cancel => {
                // WS10 Task 5: set the per-batch cancel flag. The embed loop
                // checks this between sub-batches and stops after the current
                // batch completes, returning an error response.
                tracing::info!(
                    batch_id = %batch_id,
                    "cancel signal received for batch"
                );
                if let Some(token) = self
                    .active_embed_cancels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get(&batch_id)
                    .cloned()
                {
                    token.store(true, Ordering::Release);
                }
                protocol::cancel_response_frame(
                    batch_id,
                    protocol::CancelResponse { acknowledged: true },
                )
                .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e))
            }
            _ => {
                let err = WorkerError {
                    kind: ErrorKind::InvalidRequest,
                    message: format!(
                        "unexpected message type {:?} from main daemon",
                        frame.header.msg_type
                    ),
                };
                protocol::error_frame(batch_id, err)
                    .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e))
            }
        }
    }

    fn register_embed_cancel(
        &self,
        batch_id: BatchId,
    ) -> Result<(ActiveEmbedCancelGuard, Arc<AtomicBool>), WorkerError> {
        let token = Arc::new(AtomicBool::new(false));
        let mut registry = self
            .active_embed_cancels
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        if registry.contains_key(&batch_id) {
            return Err(WorkerError {
                kind: ErrorKind::InvalidRequest,
                message: format!("duplicate active embed batch id: {batch_id}"),
            });
        }
        registry.insert(batch_id, Arc::clone(&token));
        drop(registry);
        Ok((
            ActiveEmbedCancelGuard {
                batch_id,
                token: Arc::clone(&token),
                registry: Arc::clone(&self.active_embed_cancels),
            },
            token,
        ))
    }

    /// Handle an embed request.
    ///
    /// VAL-CPHASE-012: Returns flat row-major output with dimension and count metadata.
    /// VAL-CPHASE-013: Batch ordering is preserved through IPC.
    ///
    /// WS10 Task 5: When `cache_keys` is non-empty and the global embedding
    /// cache is open, the worker probes cached entries first, embeds only the
    /// misses, writes fresh vectors back to the cache, and returns the complete
    /// result set in input order (VAL-CACHE-008).
    fn handle_embed(
        &self,
        frame: &Frame,
        cancel_token: &Arc<AtomicBool>,
    ) -> Result<EmbedResponse, WorkerError> {
        let request: Request = frame.decode_payload().map_err(|e| WorkerError {
            kind: ErrorKind::InvalidRequest,
            message: format!("failed to decode embed request: {}", e),
        })?;

        let embed_req = match request {
            Request::Embed(req) => req,
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::InvalidRequest,
                    message: "expected Embed request".to_string(),
                });
            }
        };

        if embed_req.texts.is_empty() {
            return Ok(EmbedResponse::new(vec![], 0, embed_req.expected_dim));
        }

        // Validate cache_keys length if provided.
        if !embed_req.cache_keys.is_empty() && embed_req.cache_keys.len() != embed_req.texts.len() {
            return Err(WorkerError {
                kind: ErrorKind::InvalidRequest,
                message: format!(
                    "cache_keys length ({}) does not match texts length ({})",
                    embed_req.cache_keys.len(),
                    embed_req.texts.len()
                ),
            });
        }

        // Pre-IPC oversized input handling:
        // Truncate any single text that exceeds max_text_size.
        let texts: Vec<String> = embed_req
            .texts
            .into_iter()
            .map(|t| self.truncate_text(t))
            .collect();

        // WS10 Task 5: Cache-aware path — probe cache, embed misses, put results.
        if !embed_req.cache_keys.is_empty() {
            return self.handle_embed_with_cache(
                &texts,
                &embed_req.cache_keys,
                embed_req.expected_dim,
                cancel_token,
            );
        }

        #[cfg(feature = "onnx")]
        {
            if let (Some(session), Some(tokenizer)) = (&self.session, &self.tokenizer) {
                self.run_onnx_embed(
                    session,
                    tokenizer,
                    &texts,
                    embed_req.expected_dim,
                    cancel_token,
                )
            } else {
                Err(WorkerError {
                    kind: ErrorKind::ModelNotFound,
                    message: "ONNX session or tokenizer not initialized".to_string(),
                })
            }
        }

        #[cfg(not(feature = "onnx"))]
        {
            // No ONNX feature: return zero vectors
            tracing::warn!("ONNX feature not enabled, returning zero vectors");
            let count = texts.len();
            let dim = embed_req.expected_dim;
            let vectors = vec![0.0f32; count * dim];
            Ok(EmbedResponse::new(vectors, count, dim))
        }
    }

    /// WS10 Task 5: Cache-aware embed path (probe → batch-miss → put).
    ///
    /// 1. Probe the cache for all keys.
    /// 2. Embed only the misses under the BatchBudget.
    /// 3. Write miss vectors back to the cache.
    /// 4. Return the complete vector set (hits + misses) in input order.
    ///
    /// VAL-CACHE-008: Output ordering matches input text ordering regardless
    /// of which texts were cache hits vs misses.
    ///
    /// VAL-CACHE-009: The cancel flag is checked between sub-batches.
    /// If cancelled, returns an error after the current batch completes.
    fn handle_embed_with_cache(
        &self,
        texts: &[String],
        cache_keys: &[crate::embed::cache::CacheKey],
        expected_dim: usize,
        cancel_token: &Arc<AtomicBool>,
    ) -> Result<EmbedResponse, WorkerError> {
        let n = texts.len();
        debug_assert_eq!(n, cache_keys.len());
        if expected_dim == 0 {
            return Err(WorkerError {
                kind: ErrorKind::InvalidRequest,
                message: "expected_dim must be non-zero".to_string(),
            });
        }

        let total_values = n.checked_mul(expected_dim).ok_or_else(|| WorkerError {
            kind: ErrorKind::InvalidRequest,
            message: "embedding response size overflow".to_string(),
        })?;
        let mut vectors = vec![0.0f32; total_values];
        let mut filled = vec![false; n];

        // Probe while holding the cache lock, then release it before inference.
        if let Some(cache_arc) = &self.cache {
            let mut cache = cache_arc.lock().unwrap_or_else(|p| p.into_inner());
            self.apply_probe_hits(
                &mut cache,
                cache_keys,
                expected_dim,
                &mut vectors,
                &mut filled,
            );
        }

        let miss_indices: Vec<usize> = filled
            .iter()
            .enumerate()
            .filter_map(|(i, is_filled)| (!is_filled).then_some(i))
            .collect();
        if miss_indices.is_empty() {
            return EmbedResponse::try_new(vectors, n, expected_dim).map_err(|message| {
                WorkerError {
                    kind: ErrorKind::Inference,
                    message,
                }
            });
        }

        // Borrow miss texts instead of cloning their String bodies.
        let miss_texts: Vec<&str> = miss_indices.iter().map(|&i| texts[i].as_str()).collect();
        let miss_vectors = self.embed_texts(&miss_texts, expected_dim, cancel_token)?;
        let expected_miss_values = miss_indices.len() * expected_dim;
        if miss_vectors.len() != expected_miss_values {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: format!(
                    "miss embedding output length mismatch: got {}, expected {}",
                    miss_vectors.len(),
                    expected_miss_values
                ),
            });
        }

        // Copy directly into final flat row ranges; no nested per-row vectors.
        for (miss_row, &output_row) in miss_indices.iter().enumerate() {
            let source_start = miss_row * expected_dim;
            let target_start = output_row * expected_dim;
            vectors[target_start..target_start + expected_dim]
                .copy_from_slice(&miss_vectors[source_start..source_start + expected_dim]);
            filled[output_row] = true;
        }

        // Write misses after inference; never hold the cache mutex across ORT.
        if let Some(cache_arc) = &self.cache {
            let mut cache = cache_arc.lock().unwrap_or_else(|p| p.into_inner());
            self.put_miss_vectors(
                &mut cache,
                texts,
                cache_keys,
                &miss_indices,
                &miss_vectors,
                expected_dim,
            );
        }

        debug_assert!(filled.into_iter().all(|value| value));
        EmbedResponse::try_new(vectors, n, expected_dim).map_err(|message| WorkerError {
            kind: ErrorKind::Inference,
            message,
        })
    }

    /// Copy probed cache hits into `vectors`, marking filled rows in `filled`.
    /// Malformed rows (out-of-range index or wrong dimension) are logged and
    /// dropped so the affected text is re-embedded as a miss.
    fn apply_probe_hits(
        &self,
        cache: &mut crate::embed::cache::store::GlobalEmbeddingCache,
        cache_keys: &[crate::embed::cache::CacheKey],
        expected_dim: usize,
        vectors: &mut [f32],
        filled: &mut [bool],
    ) {
        match cache.probe(cache_keys) {
            Ok(probe_result) => {
                let hit_count = probe_result.hits.len();
                for (idx, vector) in probe_result.hits {
                    if idx < filled.len() && vector.len() == expected_dim {
                        let start = idx * expected_dim;
                        vectors[start..start + expected_dim].copy_from_slice(&vector);
                        filled[idx] = true;
                    } else {
                        tracing::warn!(
                            index = idx,
                            cached_dim = vector.len(),
                            expected_dim,
                            "ignoring malformed embedding cache row"
                        );
                    }
                }
                tracing::debug!(
                    total = filled.len(),
                    hits = hit_count,
                    misses = filled.iter().filter(|&&is_filled| !is_filled).count(),
                    "cache probe complete"
                );
            }
            Err(e) => tracing::warn!(error = %e, "cache probe failed; embedding all texts"),
        }
    }

    /// Write the freshly embedded miss vectors back to the cache, row by row.
    /// Write failures are logged and otherwise ignored: the cache is
    /// rebuildable, so a dropped write only costs a future miss.
    fn put_miss_vectors(
        &self,
        cache: &mut crate::embed::cache::store::GlobalEmbeddingCache,
        texts: &[String],
        cache_keys: &[crate::embed::cache::CacheKey],
        miss_indices: &[usize],
        miss_vectors: &[f32],
        expected_dim: usize,
    ) {
        for (miss_row, &miss_idx) in miss_indices.iter().enumerate() {
            let start = miss_row * expected_dim;
            let vec_slice = &miss_vectors[start..start + expected_dim];
            let source_text = texts.get(miss_idx).map(String::as_str);
            if let Err(e) = cache.put(&cache_keys[miss_idx], vec_slice, source_text) {
                tracing::warn!(error = %e, "failed to write embedding to cache");
            }
        }
    }

    /// Embed texts using ONNX inference (or zero vectors if ONNX is not enabled).
    ///
    /// WS10 Task 5: Checks the cancel flag between sub-batches (VAL-CACHE-009).
    fn embed_texts<S: AsRef<str>>(
        &self,
        texts: &[S],
        expected_dim: usize,
        cancel_token: &Arc<AtomicBool>,
    ) -> Result<Vec<f32>, WorkerError> {
        #[cfg(feature = "onnx")]
        {
            if let (Some(session), Some(tokenizer)) = (&self.session, &self.tokenizer) {
                self.run_onnx_embed(session, tokenizer, texts, expected_dim, cancel_token)
                    .map(|resp| resp.vectors)
            } else {
                Err(WorkerError {
                    kind: ErrorKind::ModelNotFound,
                    message: "ONNX session or tokenizer not initialized".to_string(),
                })
            }
        }
        #[cfg(not(feature = "onnx"))]
        {
            tracing::warn!("ONNX feature not enabled, returning zero vectors");
            Ok(vec![0.0f32; texts.len() * expected_dim])
        }
    }

    /// WS10 Task 4: Handle a cache probe request.
    ///
    /// Returns hit/miss index lists without performing any embedding work.
    fn handle_cache_probe(
        &self,
        frame: &Frame,
    ) -> Result<protocol::CacheProbeResponse, WorkerError> {
        let request: Request = frame.decode_payload().map_err(|e| WorkerError {
            kind: ErrorKind::InvalidRequest,
            message: format!("failed to decode cache probe request: {}", e),
        })?;

        let probe_req = match request {
            Request::CacheProbe(req) => req,
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::InvalidRequest,
                    message: "expected CacheProbe request".to_string(),
                });
            }
        };

        let Some(cache_arc) = &self.cache else {
            // Cache not open: treat all keys as misses.
            return Ok(protocol::CacheProbeResponse {
                hit_indices: vec![],
                miss_indices: (0..probe_req.keys.len()).collect(),
            });
        };

        let mut cache = cache_arc.lock().unwrap_or_else(|p| p.into_inner());
        let probe_result = cache.probe(&probe_req.keys).map_err(|e| WorkerError {
            kind: ErrorKind::Internal,
            message: format!("cache probe failed: {}", e),
        })?;

        Ok(protocol::CacheProbeResponse {
            hit_indices: probe_result.hits.keys().copied().collect(),
            miss_indices: probe_result.misses,
        })
    }

    /// Truncate a single text to the configured maximum size.
    ///
    /// VAL-CPHASE-015: A single overlarge text is truncated before IPC framing
    /// rather than overflowing transport.
    fn truncate_text(&self, text: String) -> String {
        if text.len() <= self.config.max_text_size {
            return text;
        }

        // Truncate at a character boundary to avoid panics
        let mut end = self.config.max_text_size;
        while !text.is_char_boundary(end) && end > 0 {
            end -= 1;
        }
        tracing::warn!(
            original_len = text.len(),
            truncated_len = end,
            "truncated oversized text before IPC framing"
        );
        text[..end].to_string()
    }

    /// Build an internal error frame for protocol-level failures.
    fn internal_error_frame(&self, batch_id: BatchId, err: &anyhow::Error) -> Frame {
        let worker_err = WorkerError {
            kind: ErrorKind::Internal,
            message: format!("internal error: {}", err),
        };
        // This should not fail since WorkerError is simple, but fall back to a
        // minimal frame if it does.
        protocol::error_frame(batch_id, worker_err).unwrap_or_else(|_| Frame {
            header: protocol::FrameHeader {
                batch_id,
                msg_type: MsgType::Error,
            },
            payload: vec![],
        })
    }
}

impl Drop for WorkerRuntime {
    fn drop(&mut self) {
        // Drop the ONNX session first so the ort/MIGraphX/ROCm destructors free
        // compiled-program and workspace GPU memory deterministically on
        // shutdown, rather than leaving it for `process::exit`/SIGKILL (which
        // skip Drop entirely). WorkerRuntime is Clone and shares the session via
        // an Arc, so the underlying Session — and its EP resources — are only
        // released when the last clone drops; `worker_main` drains the runtime
        // explicitly before exiting to guarantee that happens here.
        #[cfg(feature = "onnx")]
        {
            self.session = None;
            tracing::trace!("WorkerRuntime dropped; ONNX/GPU resources released");
        }
    }
}

#[cfg(test)]
#[path = "runtime_test.rs"]
mod tests;

#[cfg(test)]
#[path = "worker_cache_test.rs"]
mod worker_cache_tests;
