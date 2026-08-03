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

use std::io::{self, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
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

pub use crate::embed::runtime_env::{
    DEFAULT_MAX_SEQ_LEN, DEFAULT_MIGRAPHX_INFERENCE_BATCH_SIZE,
    configured_onnx_inference_batch_size, configured_onnx_sequence_len,
};
use crate::embed::runtime_env::{
    DEFAULT_MIN_AVAILABLE_MB, MIGRAPHX_EXHAUSTIVE_TUNE_ENV, MIGRAPHX_FP16_ENV,
    MIGRAPHX_MODEL_CACHE_PATH_ENV, ONNX_LOG_SHAPES_ENV, build_position_ids, default_ort_threads,
    env_flag, mem_available_kib, process_rss_kib, prune_migraphx_cache, unix_now_ms,
};

#[cfg(feature = "onnx")]
fn extract_output_tensor_f32(value: &ort::value::DynValue) -> Result<Vec<f32>, String> {
    match value.try_extract_array::<f32>() {
        Ok(values) => Ok(values.iter().copied().collect()),
        Err(f32_error) => value
            .try_extract_array::<half::f16>()
            .map(|values| values.iter().map(|value| value.to_f32()).collect())
            .map_err(|f16_error| {
                format!(
                    "output is neither f32 ({}) nor f16 ({})",
                    f32_error, f16_error
                )
            }),
    }
}
/// T6 low-memory refusal: when `min_available_mb` is configured and the system
/// has less `MemAvailable` than that, return the refusal reason so the caller
/// can abort BEFORE loading the (multi-GiB) ONNX model. `None` when unset or
/// when `MemAvailable` cannot be determined (no-op, documented).
pub(crate) fn low_memory_refusal(config: &RuntimeConfig) -> Option<String> {
    let min_mb = config.min_available_mb?;
    let available_kib = mem_available_kib()?;
    (available_kib < min_mb.saturating_mul(1024)).then(|| {
        format!(
            "system MemAvailable is {} KiB, below LEINDEX_WORKER_MIN_AVAILABLE_MB={} MB; \
             refusing to load the ONNX model (memory-pressure T6)",
            available_kib, min_mb
        )
    })
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
            model_name: "qwen3-embed-0.6b".to_string(),
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
            .unwrap_or_else(|| "qwen3-embed-0.6b".to_string());

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
        let max_rss_mb = std::env::var("LEINDEX_WORKER_MAX_RSS_MB")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .filter(|&v| v > 0);
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

    /// ONNX session for neural embedding inference. Only available with `onnx` feature.
    #[cfg(feature = "onnx")]
    session: Option<Arc<Mutex<Session>>>,

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
        }
    }

    /// Emit the normal startup report after model initialization completes.
    pub fn log_startup_report(&self) {
        self.build_startup_report().log();
    }

    #[cfg(feature = "onnx")]
    #[allow(clippy::type_complexity)]
    fn init_onnx(
        config: &RuntimeConfig,
    ) -> (
        Option<Arc<Mutex<Session>>>,
        Option<Arc<tokenizers::Tokenizer>>,
        Duration,
        ProviderRuntimeStatus,
    ) {
        use std::time::Instant;

        let load_start = Instant::now();
        let mut provider_runtime_status = ProviderRuntimeStatus::fallback_to_cpu(
            "ONNX session was not initialized; neural embeddings disabled",
        );

        // VAL-ORT-005..010, VAL-ORT-017: Discover and load ORT *before* any
        // Session::builder() call. With the `load-dynamic` feature, ORT is
        // dlopen-ed here via `ort::init_from()`. If discovery fails, we bail
        // with a clear log line rather than panicking inside ort's setup_api.
        let init = crate::embed::ort_discovery::discover_and_init();
        match &init {
            crate::embed::ort_discovery::InitResult::Initialized(outcome) => {
                tracing::info!(
                    "ONNX Runtime loaded from {} [{}]",
                    outcome.path.display(),
                    outcome.source
                );
            }
            crate::embed::ort_discovery::InitResult::NotFound {
                searched,
                last_error,
            } => {
                let searched_paths: Vec<String> = searched.iter().map(|(_, p)| p.clone()).collect();
                tracing::error!(
                    searched_paths = ?searched_paths,
                    last_error,
                    "ONNX Runtime not found in any discovery source; \
                     set ORT_DYLIB_PATH or run `leindex setup`; neural embeddings disabled"
                );
                return (None, None, Duration::ZERO, provider_runtime_status);
            }
        }

        // Resolve model path
        let model_path = match ModelResolver::resolve(&config.model_name) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("failed to resolve ONNX model path: {}", e);
                return (None, None, Duration::ZERO, provider_runtime_status);
            }
        };

        // Resolve tokenizer path
        let tokenizer_path = match ModelResolver::resolve_tokenizer(&config.model_name) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("failed to resolve tokenizer path: {}", e);
                return (None, None, load_start.elapsed(), provider_runtime_status);
            }
        };

        // Load tokenizer
        let mut tokenizer = match tokenizers::Tokenizer::from_file(&tokenizer_path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "failed to load tokenizer from {}: {}",
                    tokenizer_path.display(),
                    e
                );
                return (None, None, load_start.elapsed(), provider_runtime_status);
            }
        };
        // Configure token-level truncation at load so an oversized input never
        // allocates a full-length encoding before the inference path pads it.
        // Padding targets `configured_onnx_sequence_len()`, so truncating to the
        // same length only bounds intermediate allocation — it does not change
        // the model's input shape or the result for in-bounds texts.
        let seq_len = configured_onnx_sequence_len();
        use tokenizers::utils::truncation::{
            TruncationDirection, TruncationParams, TruncationStrategy,
        };
        if let Err(error) = tokenizer.with_truncation(Some(TruncationParams {
            direction: TruncationDirection::Right,
            max_length: seq_len,
            strategy: TruncationStrategy::LongestFirst,
            stride: 0,
        })) {
            tracing::warn!(
                "failed to set tokenizer truncation (max_length={}): {}",
                seq_len,
                error
            );
        }

        // Create ONNX session
        let provider_selection = ExecutionProviderSelector::select(&config.execution_provider);
        let session_result = match provider_selection {
            Ok(selection) => {
                tracing::info!("using {} execution provider", selection.name());
                Self::build_session(&model_path, &selection.name(), config.ort_threads)
            }
            Err(fallback) => {
                tracing::warn!(
                    "requested provider unavailable, using {}: {}",
                    fallback.fallback_name(),
                    fallback.reason()
                );
                Self::build_session(&model_path, &fallback.fallback_name(), config.ort_threads)
            }
        };

        let model_load_time = load_start.elapsed();

        match &session_result {
            Ok(_) => tracing::info!("ONNX model loaded in {:?}", model_load_time),
            Err(e) => tracing::warn!("failed to build ONNX session: {}", e),
        }

        match session_result {
            Ok(outcome) => {
                let SessionBuildOutcome {
                    session,
                    provider_status,
                } = outcome;

                provider_runtime_status = provider_status;
                (
                    Some(Arc::new(Mutex::new(session))),
                    Some(Arc::new(tokenizer)),
                    model_load_time,
                    provider_runtime_status,
                )
            }
            Err(_) => (
                None,
                Some(Arc::new(tokenizer)),
                model_load_time,
                provider_runtime_status,
            ),
        }
    }

    #[cfg(feature = "onnx")]
    fn build_session(
        model_path: &std::path::Path,
        provider_name: &str,
        ort_threads: usize,
    ) -> Result<SessionBuildOutcome, ort::Error> {
        // Auto must be resolved before reaching a session builder — see
        // attach_execution_provider's debug_assert. The optimization-level
        // match below therefore only lists concrete GPU providers.
        debug_assert!(
            provider_name != "auto",
            "build_session received unresolved 'auto'; select() must run first"
        );
        // For GPU execution providers (MIGraphX/ROCm), use Level3 optimization
        // so the ONNX graph undergoes maximum operator fusion before the EP sees
        // it; at Level1 the graph is too granular and MIGraphX falls back to CPU
        // for most operators, leaving VRAM unused. Level3 enables the transformer
        // fusion passes that move computation to the GPU.
        let optimization_level = match provider_name {
            "migraphx" | "rocm" => GraphOptimizationLevel::Level3,
            _ => GraphOptimizationLevel::Level1,
        };

        // Disable memory pattern reuse: tokenized sequence lengths vary between
        // calls, and without this ORT may reuse a buffer shaped for the previous
        // sequence and report a shape mismatch.
        // T5: bound the intra-op thread pool at every session-builder site.
        let session_builder = Session::builder()?
            .with_intra_threads(ort_threads)?
            .with_memory_pattern(false)?
            .with_log_level(LogLevel::Warning)?
            .with_optimization_level(optimization_level)?;

        // VAL-ORT-015/016: short-circuit to a CPU session if a GPU provider was
        // selected but MIGraphX is not compiled into the dynamically-loaded ORT
        // binary. See `maybe_missing_ep_fallback`.
        if let Some(outcome) = maybe_missing_ep_fallback(model_path, provider_name, ort_threads)? {
            return Ok(outcome);
        }

        // Attach the selected execution provider, falling back to CPU on failure.
        let (mut session_builder, provider_status) =
            attach_execution_provider(session_builder, provider_name, ort_threads)?;

        let session = session_builder.commit_from_file(model_path)?;
        Ok(SessionBuildOutcome {
            session,
            provider_status,
        })
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
            MsgType::EmbedRequest => match self.handle_embed(frame) {
                Ok(response) => protocol::embed_response_frame(batch_id, response)
                    .unwrap_or_else(|e| self.internal_error_frame(batch_id, &e)),
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

    /// Handle an embed request.
    ///
    /// VAL-CPHASE-012: Returns flat row-major output with dimension and count metadata.
    /// VAL-CPHASE-013: Batch ordering is preserved through IPC.
    fn handle_embed(&self, frame: &Frame) -> Result<EmbedResponse, WorkerError> {
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

        // Pre-IPC oversized input handling:
        // Truncate any single text that exceeds max_text_size.
        let texts: Vec<String> = embed_req
            .texts
            .into_iter()
            .map(|t| self.truncate_text(t))
            .collect();

        #[cfg(feature = "onnx")]
        {
            if let (Some(session), Some(tokenizer)) = (&self.session, &self.tokenizer) {
                self.run_onnx_embed(session, tokenizer, &texts, embed_req.expected_dim)
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

    #[cfg(feature = "onnx")]
    fn run_onnx_embed(
        &self,
        session: &Arc<Mutex<Session>>,
        tokenizer: &Arc<tokenizers::Tokenizer>,
        texts: &[String],
        expected_dim: usize,
    ) -> Result<EmbedResponse, WorkerError> {
        // Batch tokenize all texts. Borrow as &str to avoid cloning every text
        // into the tokenizer call (the texts are already owned by the caller).
        let text_refs: Vec<&str> = texts.iter().map(String::as_str).collect();
        let encodings = tokenizer
            .encode_batch(text_refs, true)
            .map_err(|e| WorkerError {
                kind: ErrorKind::Tokenizer,
                message: format!("tokenization failed: {}", e),
            })?;

        if encodings.is_empty() {
            return Ok(EmbedResponse::new(vec![], 0, expected_dim));
        }

        if expected_dim == 0 {
            return Err(WorkerError {
                kind: ErrorKind::InvalidRequest,
                message: "expected_dim must be non-zero".to_string(),
            });
        }

        // Process encodings in sub-batches to bound peak memory.
        // Batch size is env-tunable once the selected model is validated for
        // dynamic batch; the default is safe for fixed-batch artifacts.
        let mut all_pooled: Vec<f32> = Vec::with_capacity(encodings.len() * expected_dim);

        let active_provider = &self.provider_runtime_status.execution_provider;
        let inference_batch_size =
            configured_onnx_inference_batch_size(&self.config.model_name, active_provider);
        let fixed_batch = active_provider.eq_ignore_ascii_case("migraphx")
            || active_provider.eq_ignore_ascii_case("rocm");
        for sub_batch in encodings.chunks(inference_batch_size) {
            // Keep the worker alive across a large multi-batch codebase.
            self.touch();
            if fixed_batch && sub_batch.len() < inference_batch_size {
                let mut padded = sub_batch.to_vec();
                if let Some(template) = sub_batch.first() {
                    padded.resize(inference_batch_size, template.clone());
                }
                let sub_pooled = self.run_onnx_embed_sub_batch(session, &padded, expected_dim)?;
                all_pooled.extend_from_slice(&sub_pooled[..sub_batch.len() * expected_dim]);
            } else {
                let sub_pooled = self.run_onnx_embed_sub_batch(session, sub_batch, expected_dim)?;
                all_pooled.extend_from_slice(&sub_pooled);
            }
        }

        let total_count = encodings.len();
        Ok(EmbedResponse::new(all_pooled, total_count, expected_dim))
    }

    /// Run ONNX inference on a single sub-batch of encodings and return
    /// the pooled + L2-normalized vectors (flattened row-major).
    #[cfg(feature = "onnx")]
    fn run_onnx_embed_sub_batch(
        &self,
        session: &Arc<Mutex<Session>>,
        encodings: &[tokenizers::Encoding],
        expected_dim: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        let batch_size = encodings.len();
        if batch_size == 0 {
            return Ok(vec![]);
        }

        let max_len = configured_onnx_sequence_len();
        if env_flag(ONNX_LOG_SHAPES_ENV) {
            let max_encoding_len = encodings.iter().map(|e| e.len()).max().unwrap_or(0);
            tracing::info!(
                batch_size,
                max_len,
                max_encoding_len,
                "ONNX embedding input shape"
            );
        }

        if max_len == 0 {
            return Ok(vec![0.0f32; batch_size * expected_dim]);
        }

        // Create input tensors: [batch_size, seq_len]
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        for encoding in encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();

            // Pad to max_len
            for i in 0..max_len {
                if i < ids.len() {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                } else {
                    input_ids.push(0i64);
                    attention_mask.push(0i64);
                }
            }
        }

        // Build the [batch_size, seq_len] input tensors. The array+tensor
        // creation is identical across all four inputs, so a local macro keeps
        // each as a single (labelled) expression with one error path.
        macro_rules! make_i64_tensor {
            ($data:expr, $label:literal) => {
                ort::value::Tensor::from_array(
                    ndarray::Array2::from_shape_vec((batch_size, max_len), $data).map_err(|e| {
                        WorkerError {
                            kind: ErrorKind::Inference,
                            message: format!("failed to create {} array: {}", $label, e),
                        }
                    })?,
                )
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create {} tensor: {}", $label, e),
                })?
            };
        }

        let input_ids_tensor = make_i64_tensor!(input_ids, "input_ids");
        let attention_mask_tensor = make_i64_tensor!(attention_mask.clone(), "attention_mask");
        let position_ids_tensor =
            make_i64_tensor!(build_position_ids(batch_size, max_len), "position_ids");
        // token_type_ids: BERT/GTE-style models have a `token_type_embeddings`
        // layer that requires this input — without it inference fails at
        // `/embeddings/token_type_embeddings/Gather` with "Missing Input:
        // token_type_ids". For single-text retrieval every token is segment 0,
        // so feed all-zeros. Models that lack this input never read it.
        let token_type_ids_tensor =
            make_i64_tensor!(vec![0i64; batch_size * max_len], "token_type_ids");

        let mut session_guard = session.lock().map_err(|e| WorkerError {
            kind: ErrorKind::OnnxRuntime,
            message: format!("failed to lock ONNX session: {}", e),
        })?;

        let uses_position_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "position_ids");
        let uses_token_type_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        // Feed only the inputs the model declares; extras would be rejected.
        // Arms are mutually exclusive, so each tensor moves on exactly one path.
        let outputs = match (uses_position_ids, uses_token_type_ids) {
            (true, true) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "position_ids" => position_ids_tensor,
                "token_type_ids" => token_type_ids_tensor,
            }),
            (true, false) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "position_ids" => position_ids_tensor,
            }),
            (false, true) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "token_type_ids" => token_type_ids_tensor,
            }),
            (false, false) => session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
            }),
        }
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("ONNX inference failed: {}", e),
        })?;

        self.finalize_embed_output(&outputs, batch_size, expected_dim, &attention_mask)
    }

    /// Validate the embed output shape, normalize a pre-pooled `[b, hidden]`
    /// tensor in place, or pool+normalize a `[b, seq, hidden]` tensor. Extracted
    /// from `run_onnx_embed_sub_batch` to keep that function's branch count bounded.
    #[cfg(feature = "onnx")]
    fn finalize_embed_output(
        &self,
        outputs: &ort::session::SessionOutputs<'_>,
        batch_size: usize,
        expected_dim: usize,
        attention_mask: &[i64],
    ) -> Result<Vec<f32>, WorkerError> {
        if outputs.len() == 0 {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: "ONNX model returned no outputs".to_string(),
            });
        }
        // Expected: [batch_size, seq_len, hidden_dim] or [batch_size, hidden_dim].
        let output_shape: Vec<usize> = outputs[0].shape().iter().map(|&d| d as usize).collect();
        // MIGraphX may return float16 even when the source graph is float32;
        // normalize both provider output types to the f32 storage contract.
        let embeddings_f32 = extract_output_tensor_f32(&outputs[0]).map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to extract output tensor: {}", e),
        })?;
        let (actual_seq_len, hidden_dim) = match output_shape.as_slice() {
            [bs, sl, hd] if *bs == batch_size => {
                if *hd != expected_dim {
                    return Err(WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "output dimension mismatch: model produced {}, expected {}",
                            hd, expected_dim
                        ),
                    });
                }
                (*sl, *hd)
            }
            [bs, hd] if *bs == batch_size => {
                if *hd != expected_dim {
                    return Err(WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "output dimension mismatch: model produced {}, expected {}",
                            hd, expected_dim
                        ),
                    });
                }
                // Already pooled: L2-normalize per row.
                let dim = *hd;
                let mut embeddings_f32 = embeddings_f32;
                for b in 0..batch_size {
                    let start = b * dim;
                    let end = start + dim;
                    let row = &mut embeddings_f32[start..end];
                    let norm: f32 = row.iter().map(|v| v * v).sum::<f32>().sqrt();
                    if norm > 1e-10f32 {
                        for v in row.iter_mut() {
                            *v /= norm;
                        }
                    }
                }
                return Ok(embeddings_f32);
            }
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!(
                        "unexpected output shape {:?}; expected [{}, seq_len, hidden_dim] or [{}, hidden_dim]",
                        output_shape, batch_size, batch_size
                    ),
                });
            }
        };
        if embeddings_f32.len() != batch_size * actual_seq_len * hidden_dim {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: format!(
                    "output size mismatch: shape {:?} implies {} elements, got {}",
                    output_shape,
                    batch_size * actual_seq_len * hidden_dim,
                    embeddings_f32.len()
                ),
            });
        }
        // Select the final unpadded token required by Qwen3, then L2 normalize.
        let pooled = self.pool_and_normalize(
            &embeddings_f32,
            batch_size,
            actual_seq_len,
            attention_mask,
            hidden_dim,
        )?;
        Ok(pooled.vectors)
    }

    #[cfg(feature = "onnx")]
    fn pool_and_normalize(
        &self,
        embeddings: &[f32],
        batch_size: usize,
        seq_len: usize,
        attention_mask: &[i64],
        expected_dim: usize,
    ) -> Result<EmbedResponse, WorkerError> {
        let hidden_dim = expected_dim;
        let mut pooled: Vec<f32> = Vec::with_capacity(batch_size * hidden_dim);
        let mut row: Vec<f32> = vec![0.0f32; hidden_dim];

        for b in 0..batch_size {
            row.fill(0.0);
            let mask_start = b * seq_len;
            let last_token = (0..seq_len)
                .rev()
                .find(|&s| attention_mask.get(mask_start + s).copied().unwrap_or(0) > 0);
            if let Some(token_index) = last_token {
                let embedding_start = (b * seq_len + token_index) * hidden_dim;
                let embedding = embeddings
                    .get(embedding_start..embedding_start + hidden_dim)
                    .ok_or_else(|| WorkerError {
                        kind: ErrorKind::Inference,
                        message: format!(
                            "embedding output is too short: need elements {}..{}, got {}",
                            embedding_start,
                            embedding_start + hidden_dim,
                            embeddings.len()
                        ),
                    })?;
                row.copy_from_slice(embedding);
            }

            let norm = row.iter().map(|value| value * value).sum::<f32>().sqrt();

            if norm > 1e-10f32 {
                for value in &mut row {
                    *value /= norm;
                }
            }

            pooled.extend_from_slice(&row);
        }

        Ok(EmbedResponse::new(pooled, batch_size, expected_dim))
    }

    /// Handle a rerank request.
    /// Lazily load + return the reranker cross-encoder session + tokenizer (on
    /// demand). The reranker is loaded only on the first rerank request and
    /// evicted after `RERANK_IDLE_EVICTION_SECS` of idleness
    /// (`maybe_evict_rerank`). Uses the CPU execution provider so it does not
    /// contend with the embedder's GPU session and stays cheap (a cross-encoder
    /// over a small top-N). Double-checked under `rerank_init_lock` so concurrent
    /// socket requests don't double-load.
    #[cfg(feature = "onnx")]
    fn ensure_rerank_session(
        &self,
    ) -> Result<(Arc<Mutex<Session>>, Arc<tokenizers::Tokenizer>), WorkerError> {
        // Fast path: already resident.
        if let (Some(s), Some(t)) = (
            self.rerank_session.lock().ok().and_then(|g| g.clone()),
            self.rerank_tokenizer.lock().ok().and_then(|g| g.clone()),
        ) {
            *self.last_rerank_activity.lock().unwrap() = Instant::now();
            return Ok((s, t));
        }
        let _init = self.rerank_init_lock.lock().map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("rerank init lock poisoned: {}", e),
        })?;
        // Double-check after acquiring the lock.
        if let (Some(s), Some(t)) = (
            self.rerank_session.lock().ok().and_then(|g| g.clone()),
            self.rerank_tokenizer.lock().ok().and_then(|g| g.clone()),
        ) {
            *self.last_rerank_activity.lock().unwrap() = Instant::now();
            return Ok((s, t));
        }
        let model_name = self.config.rerank_model_name.clone();
        if model_name.trim().is_empty() {
            return Err(WorkerError {
                kind: ErrorKind::ModelNotFound,
                message: "no rerank model configured".to_string(),
            });
        }
        let model_path =
            crate::embed::model_path::ModelResolver::resolve(&model_name).map_err(|e| {
                WorkerError {
                    kind: ErrorKind::ModelNotFound,
                    message: format!("rerank model '{}' not found: {}", model_name, e),
                }
            })?;
        // Rerank tokenizer: convention `{model_stem}-tokenizer.json` beside the
        // model. ModelResolver::resolve_tokenizer ignores model_name and would
        // return the EMBED tokenizer (wrong vocab), so derive the path
        // explicitly: bge-reranker-base.onnx -> bge-reranker-base-tokenizer.json.
        let tokenizer_path = format!("{}-tokenizer.json", model_path.with_extension("").display());
        let tokenizer = Arc::new(tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(
            |e| WorkerError {
                kind: ErrorKind::Tokenizer,
                message: format!("rerank tokenizer load failed ({}): {}", tokenizer_path, e),
            },
        )?);
        // Use the same concrete provider the embedder session resolved to
        // (self.provider_runtime_status.execution_provider), so the
        // cross-encoder is fast (~1-3s after a one-time compile cached as a
        // .mxr) and the reranker never receives the unresolved "auto" token.
        // The native ORT_MIGraphX_MODEL_CACHE_PATH cache persists across idle
        // evictions, so on-demand reloads stay warm. CPU is ~70s/query for
        // top-20 × 512 — unusable interactively. If the provider is unavailable
        // for this model, build_session falls back to CPU automatically.
        let provider = self.provider_runtime_status.execution_provider.as_str();
        let outcome =
            Self::build_session(&model_path, provider, self.config.ort_threads).map_err(|e| {
                WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("rerank session build failed: {}", e),
                }
            })?;
        let session = Arc::new(Mutex::new(outcome.session));
        tracing::info!(model = %model_name, provider, "reranker loaded on demand");
        *self.rerank_session.lock().unwrap() = Some(session.clone());
        *self.rerank_tokenizer.lock().unwrap() = Some(tokenizer.clone());
        *self.last_rerank_activity.lock().unwrap() = Instant::now();
        Ok((session, tokenizer))
    }

    /// Drop the reranker session + tokenizer if it has been idle longer than
    /// `RERANK_IDLE_EVICTION_SECS`. Called from the worker idle loop so the
    /// reranker's memory is reclaimed between rerank bursts. No-op if the
    /// reranker isn't loaded.
    #[cfg(feature = "onnx")]
    pub fn maybe_evict_rerank(&self) {
        let evict = {
            let last = match self.last_rerank_activity.lock() {
                Ok(g) => *g,
                Err(_) => return,
            };
            self.rerank_session
                .lock()
                .map(|g| g.is_some())
                .unwrap_or(false)
                && last.elapsed().as_secs() > RERANK_IDLE_EVICTION_SECS
        };
        if evict {
            let had = self
                .rerank_session
                .lock()
                .map(|mut g| g.take().is_some())
                .unwrap_or(false);
            if had {
                let _ = self.rerank_tokenizer.lock().map(|mut g| g.take());
                tracing::info!(secs = RERANK_IDLE_EVICTION_SECS, "reranker idle-evicted");
            }
        }
    }

    fn handle_rerank(&self, frame: &Frame) -> Result<RerankResponse, WorkerError> {
        let request: Request = frame.decode_payload().map_err(|e| WorkerError {
            kind: ErrorKind::InvalidRequest,
            message: format!("failed to decode rerank request: {}", e),
        })?;

        let rerank_req = match request {
            Request::Rerank(req) => req,
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::InvalidRequest,
                    message: "expected Rerank request".to_string(),
                });
            }
        };

        #[cfg(feature = "onnx")]
        {
            // Reranker is loaded ON DEMAND (separate from the embed session).
            let (session, tokenizer) = self.ensure_rerank_session()?;
            self.run_onnx_rerank(&session, &tokenizer, &rerank_req)
        }

        #[cfg(not(feature = "onnx"))]
        {
            // No ONNX feature: return passthrough scores
            tracing::warn!("ONNX feature not enabled for rerank, using passthrough scores");
            let results: Vec<_> = rerank_req
                .documents
                .into_iter()
                .map(|doc| protocol::RerankResult {
                    id: doc.id,
                    original_score: doc.initial_score,
                    rerank_score: doc.initial_score,
                    combined_score: doc.initial_score,
                })
                .collect();
            Ok(RerankResponse { results })
        }
    }

    #[cfg(feature = "onnx")]
    fn run_onnx_rerank(
        &self,
        session: &Arc<Mutex<Session>>,
        tokenizer: &Arc<tokenizers::Tokenizer>,
        rerank_req: &protocol::RerankRequest,
    ) -> Result<RerankResponse, WorkerError> {
        // Qwen3-Reranker (including the seq-cls ONNX port) REQUIRES its chat
        // template — the model was trained on the "Judge whether the Document
        // meets the requirements... answer yes or no" prompt. Raw (query, doc)
        // pairs are out-of-distribution and produce near-random logits (this was
        // the regression: the reranker surfaced tests/garbage). Build the full
        // templated string per document. Format verified against the seq-cls
        // model card. Instruction is code-tuned (Qwen3-Reranker is
        // instruction-sensitive; the web-search default is ~1-5% weaker on code).
        const RERANK_PREFIX: &str = "<|im_start|>system\nJudge whether the Document meets the requirements based on the Query and the Instruct provided. Note that the answer can only be \"yes\" or \"no\".<|im_end|>\n<|im_start|>user\n";
        const RERANK_SUFFIX: &str = "<|im_end|>\n<|im_start|>assistant\nThinking\n\nAnswer\n\n";
        const RERANK_INSTRUCT: &str =
            "Given a code search query, retrieve the most relevant source code";
        // Token length of the fixed assistant suffix, so rerank truncation can
        // preserve it: the Qwen3-Reranker predicts at the suffix position, so
        // dropping it (as a naive first-N truncation does) scores long
        // documents from an out-of-distribution prompt. Computed once per call;
        // add_special = false because the suffix appears mid-template (BOS is
        // only added at the template start).
        let rerank_suffix_len: usize = tokenizer
            .encode(RERANK_SUFFIX, false)
            .map(|enc| enc.get_ids().len())
            .unwrap_or(0);
        let pair_texts: Vec<String> = rerank_req
            .documents
            .iter()
            .map(|doc| {
                format!(
                    "{RERANK_PREFIX}<Instruct>: {RERANK_INSTRUCT}\n<Query>: {}\n<Document>: {}{RERANK_SUFFIX}",
                    rerank_req.query, doc.content
                )
            })
            .collect();

        // Batch tokenize all templated inputs.
        let encodings = tokenizer
            .encode_batch(pair_texts, true)
            .map_err(|e| WorkerError {
                kind: ErrorKind::Tokenizer,
                message: format!("rerank tokenization failed: {}", e),
            })?;

        if encodings.is_empty() {
            return Ok(RerankResponse { results: vec![] });
        }

        // Process encodings in sub-batches to bound peak memory.
        let mut all_rerank_scores: Vec<f32> = Vec::with_capacity(rerank_req.documents.len());

        let active_provider = &self.provider_runtime_status.execution_provider;
        let inference_batch_size =
            configured_onnx_inference_batch_size(&self.config.model_name, active_provider);
        let fixed_batch = active_provider.eq_ignore_ascii_case("migraphx")
            || active_provider.eq_ignore_ascii_case("rocm");
        for sub_batch in encodings.chunks(inference_batch_size) {
            self.touch();
            if fixed_batch && sub_batch.len() < inference_batch_size {
                let mut padded = sub_batch.to_vec();
                if let Some(template) = sub_batch.first() {
                    padded.resize(inference_batch_size, template.clone());
                }
                let sub_scores =
                    self.run_onnx_rerank_sub_batch(session, &padded, rerank_suffix_len)?;
                all_rerank_scores.extend_from_slice(&sub_scores[..sub_batch.len()]);
            } else {
                let sub_scores =
                    self.run_onnx_rerank_sub_batch(session, sub_batch, rerank_suffix_len)?;
                all_rerank_scores.extend_from_slice(&sub_scores);
            }
        }

        // Build results with combined scores: 70% rerank + 30% initial
        let mut results: Vec<_> = rerank_req
            .documents
            .iter()
            .zip(all_rerank_scores)
            .map(|(doc, rerank_score)| {
                let combined_score = 0.7 * rerank_score + 0.3 * doc.initial_score;
                protocol::RerankResult {
                    id: doc.id.clone(),
                    original_score: doc.initial_score,
                    rerank_score,
                    combined_score,
                }
            })
            .collect();

        // Sort by combined score descending
        results.sort_by(|a, b| {
            b.combined_score
                .partial_cmp(&a.combined_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Ok(RerankResponse { results })
    }

    /// Run ONNX rerank inference on a single sub-batch of encodings
    /// and return the scalar rerank scores.
    #[cfg(feature = "onnx")]
    fn run_onnx_rerank_sub_batch(
        &self,
        session: &Arc<Mutex<Session>>,
        encodings: &[tokenizers::Encoding],
        suffix_token_len: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        let batch_size = encodings.len();
        if batch_size == 0 {
            return Ok(vec![]);
        }

        // Rerank uses a larger context than the embed model: the Qwen3-Reranker
        // chat template (prefix + instruct + query + document + suffix) is ~60
        // tokens before the document, so the embed model's 128 would truncate
        // the document + the required assistant suffix.
        let max_len = RERANK_MAX_SEQ_LEN;

        if max_len == 0 {
            // Return zero scores if tokenization produced nothing
            return Ok(vec![0.0f32; batch_size]);
        }

        // Build LEFT-padded input_ids / attention_mask (decoder-style padding
        // with overflow-safe suffix preservation).
        let (input_ids, attention_mask) =
            Self::build_rerank_input(encodings, max_len, suffix_token_len);

        // Create the [batch_size, seq_len] input tensors. Identical array+tensor
        // creation across all three, so a local macro gives each one error path.
        macro_rules! make_rerank_tensor {
            ($data:expr, $label:literal) => {
                ort::value::Tensor::from_array(
                    ndarray::Array2::from_shape_vec((batch_size, max_len), $data).map_err(|e| {
                        WorkerError {
                            kind: ErrorKind::Inference,
                            message: format!("failed to create rerank {} array: {}", $label, e),
                        }
                    })?,
                )
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create rerank {} tensor: {}", $label, e),
                })?
            };
        }
        let input_ids_tensor = make_rerank_tensor!(input_ids.clone(), "input_ids");
        let attention_mask_tensor = make_rerank_tensor!(attention_mask.clone(), "attention_mask");
        let position_ids_tensor =
            make_rerank_tensor!(build_position_ids(batch_size, max_len), "position_ids");

        let mut session_guard = session.lock().map_err(|e| WorkerError {
            kind: ErrorKind::OnnxRuntime,
            message: format!("failed to lock ONNX session for rerank: {}", e),
        })?;

        let uses_position_ids = session_guard
            .inputs()
            .iter()
            .any(|input| input.name() == "position_ids");
        let outputs = if uses_position_ids {
            session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
                "position_ids" => position_ids_tensor,
            })
        } else {
            session_guard.run(ort::inputs! {
                "input_ids" => input_ids_tensor,
                "attention_mask" => attention_mask_tensor,
            })
        }
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("ONNX rerank inference failed: {}", e),
        })?;

        Self::finalize_rerank_output(&outputs, batch_size)
    }

    /// Build LEFT-padded `input_ids` / `attention_mask` for a rerank batch.
    /// Qwen3-Reranker is decoder-style: real tokens go at the END (pads at the
    /// start) so it attends up to the final assistant-suffix position. When an
    /// input overflows the window, the first (max_len - suffix) tokens AND the
    /// final `suffix_token_len` tokens are kept (document middle trimmed) so the
    /// assistant suffix the model predicts on is preserved.
    #[cfg(feature = "onnx")]
    fn build_rerank_input(
        encodings: &[tokenizers::Encoding],
        max_len: usize,
        suffix_token_len: usize,
    ) -> (Vec<i64>, Vec<i64>) {
        let batch_size = encodings.len();
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        for encoding in encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();
            let total = ids.len();
            let n = total.min(max_len);
            for _ in 0..(max_len - n) {
                input_ids.push(0);
                attention_mask.push(0);
            }
            if total > max_len && suffix_token_len > 0 && suffix_token_len < max_len {
                let front = max_len - suffix_token_len;
                for i in 0..front {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                }
                for i in (total - suffix_token_len)..total {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                }
            } else {
                for i in 0..n {
                    input_ids.push(ids[i] as i64);
                    attention_mask.push(mask[i] as i64);
                }
            }
        }
        (input_ids, attention_mask)
    }

    /// Validate the rerank output shape and sigmoid-map the raw yes/no logits into
    /// [0,1] relevance scores. Extracted from `run_onnx_rerank_sub_batch`.
    #[cfg(feature = "onnx")]
    fn finalize_rerank_output(
        outputs: &ort::session::SessionOutputs<'_>,
        batch_size: usize,
    ) -> Result<Vec<f32>, WorkerError> {
        if outputs.len() == 0 {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: "ONNX rerank model returned no outputs".to_string(),
            });
        }
        let output = &outputs[0];
        let shape: Vec<usize> = output.shape().iter().map(|&d| d as usize).collect();
        let output_values = extract_output_tensor_f32(output).map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to extract rerank output tensor: {}", e),
        })?;
        let raw_logits: Vec<f32> = match shape.as_slice() {
            [n] if *n == batch_size => output_values,
            [n, 1] if *n == batch_size => output_values,
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!(
                        "unsupported rerank output shape {:?}; expected [{}] or [{}, 1]",
                        shape, batch_size, batch_size
                    ),
                });
            }
        };
        // Qwen3-Reranker seq-cls emits a raw yes/no logit; sigmoid maps it to
        // [0,1] relevance so the 0.7*rerank + 0.3*initial combine is on the same
        // scale as the initial search score.
        Ok(raw_logits
            .into_iter()
            .map(|l| 1.0 / (1.0 + (-l).exp()))
            .collect())
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
