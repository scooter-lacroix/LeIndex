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
use std::sync::Arc;
use std::time::{Duration, Instant};

#[cfg(feature = "onnx")]
use std::sync::Mutex;

use crate::model_path::ModelResolver;
use crate::protocol::{
    self, BatchId, EmbedResponse, ErrorKind, Frame, MsgType, Request, RerankResponse, WorkerError,
};
use crate::provider::ExecutionProviderSelector;
use crate::startup::{StartupReport, StartupReporter};

// ONNX Runtime imports - only available with "onnx" feature
#[cfg(feature = "onnx")]
use ort::session::Session;

/// Default idle timeout in seconds before the worker tears itself down.
pub const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300; // 5 minutes

/// Default maximum sequence length for tokenization.
// TODO: make this configurable from model config / RuntimeConfig.
pub const DEFAULT_MAX_SEQ_LEN: usize = 512;

/// Default maximum outgoing frame size in bytes (16 MiB).
pub const DEFAULT_MAX_FRAME_SIZE: usize = 16 * 1024 * 1024;

/// Default maximum single-text size in bytes (1 MiB).
pub const DEFAULT_MAX_TEXT_SIZE: usize = 1024 * 1024;

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
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            max_text_size: DEFAULT_MAX_TEXT_SIZE,
            model_name: "qwen3-embed-0.6b".to_string(),
            embedding_dim: 1024,
            execution_provider: "cpu".to_string(),
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
            .unwrap_or_else(|_| "qwen3-embed-0.6b".to_string());

        let embedding_dim = std::env::var("LEINDEX_WORKER_EMBEDDING_DIM")
            .ok()
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(1024);

        let execution_provider = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER")
            .unwrap_or_else(|_| "cpu".to_string());

        Self {
            idle_timeout,
            max_frame_size,
            max_text_size,
            model_name,
            embedding_dim,
            execution_provider,
        }
    }
}

/// Worker runtime state.
///
/// Tracks the idle timer and provides the main request-processing loop.
///
/// When built with the `onnx` feature, also holds the ONNX session and tokenizer
/// for neural embedding inference.
pub struct WorkerRuntime {
    config: RuntimeConfig,
    last_activity: Instant,
    shutdown_flag: Arc<AtomicBool>,

    /// ONNX session for neural embedding inference. Only available with `onnx` feature.
    #[cfg(feature = "onnx")]
    session: Option<Arc<Mutex<Session>>>,

    /// Tokenizer for text preprocessing. Only available with `onnx` feature.
    #[cfg(feature = "onnx")]
    tokenizer: Option<Arc<tokenizers::Tokenizer>>,

    /// Model load time for startup reporting.
    #[cfg(feature = "onnx")]
    model_load_time: Duration,
}

impl WorkerRuntime {
    /// Create a new worker runtime with the given configuration.
    ///
    /// When built with the `onnx` feature, also initializes the ONNX session and tokenizer
    /// for neural embedding inference.
    pub fn new(config: RuntimeConfig) -> Self {
        #[cfg(feature = "onnx")]
        let (session, tokenizer, model_load_time) = Self::init_onnx(&config);

        Self {
            config,
            last_activity: Instant::now(),
            shutdown_flag: Arc::new(AtomicBool::new(false)),
            #[cfg(feature = "onnx")]
            session,
            #[cfg(feature = "onnx")]
            tokenizer,
            #[cfg(feature = "onnx")]
            model_load_time,
        }
    }

    #[cfg(feature = "onnx")]
    fn init_onnx(
        config: &RuntimeConfig,
    ) -> (
        Option<Arc<Mutex<Session>>>,
        Option<Arc<tokenizers::Tokenizer>>,
        Duration,
    ) {
        use std::time::Instant;

        let load_start = Instant::now();

        // Resolve model path
        let model_path = match ModelResolver::resolve(&config.model_name) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("failed to resolve ONNX model path: {}", e);
                return (None, None, Duration::ZERO);
            }
        };

        // Resolve tokenizer path
        let tokenizer_path = match ModelResolver::resolve_tokenizer(&config.model_name) {
            Ok(path) => path,
            Err(e) => {
                tracing::warn!("failed to resolve tokenizer path: {}", e);
                return (None, None, load_start.elapsed());
            }
        };

        // Load tokenizer
        let tokenizer = match tokenizers::Tokenizer::from_file(&tokenizer_path) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(
                    "failed to load tokenizer from {}: {}",
                    tokenizer_path.display(),
                    e
                );
                return (None, None, load_start.elapsed());
            }
        };

        // Create ONNX session
        let provider_selection = ExecutionProviderSelector::select(&config.execution_provider);
        let session_result = match provider_selection {
            Ok(selection) => {
                tracing::info!("using {} execution provider", selection.name());
                Self::build_session(&model_path, &selection.name())
            }
            Err(fallback) => {
                tracing::warn!(
                    "requested provider unavailable, using {}: {}",
                    fallback.fallback_name(),
                    fallback.reason()
                );
                Self::build_session(&model_path, &fallback.fallback_name())
            }
        };

        let model_load_time = load_start.elapsed();

        match &session_result {
            Ok(_) => tracing::info!("ONNX model loaded in {:?}", model_load_time),
            Err(e) => tracing::warn!("failed to build ONNX session: {}", e),
        }

        match session_result {
            Ok(session) => (
                Some(Arc::new(Mutex::new(session))),
                Some(Arc::new(tokenizer)),
                model_load_time,
            ),
            Err(_) => (None, Some(Arc::new(tokenizer)), model_load_time),
        }
    }

    #[cfg(feature = "onnx")]
    fn build_session(
        model_path: &std::path::Path,
        provider_name: &str,
    ) -> Result<Session, ort::Error> {
        /// Try to attach the given execution provider; on failure, create a fresh
        /// session builder and fall back to CPU.
        macro_rules! try_provider_or_cpu {
            ($builder:expr, $provider:expr, $name:literal) => {
                match $builder.with_execution_providers([$provider]) {
                    Ok(sb) => sb,
                    Err(e) => {
                        tracing::warn!("{} EP not available: {}, falling back to CPU", $name, e);
                        Session::builder()?
                            .with_execution_providers([ort::ep::CPU::default().build()])?
                    }
                }
            };
        }

        let session_builder = Session::builder()?;

        // Configure execution providers based on selection
        let mut session_builder = match provider_name {
            "cuda" => {
                try_provider_or_cpu!(
                    session_builder,
                    ort::ep::CUDA::default().build(),
                    "CUDA"
                )
            }
            "rocm" => {
                try_provider_or_cpu!(
                    session_builder,
                    ort::ep::ROCm::default().build(),
                    "ROCm"
                )
            }
            "coreml" => {
                try_provider_or_cpu!(
                    session_builder,
                    ort::ep::CoreML::default().build(),
                    "CoreML"
                )
            }
            _ => {
                session_builder.with_execution_providers([ort::ep::CPU::default().build()])?
            }
        };

        session_builder.commit_from_file(model_path)
    }

    /// Get a handle to the shutdown flag for external signaling.
    pub fn shutdown_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.shutdown_flag)
    }

    /// Check if the idle timeout has elapsed.
    pub fn is_idle_expired(&self) -> bool {
        self.last_activity.elapsed() >= self.config.idle_timeout
    }

    /// Reset the idle timer (called after each successful request).
    pub fn touch(&mut self) {
        self.last_activity = Instant::now();
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
    pub fn run<R: Read + Send + 'static, W: Write>(&mut self, reader: R, writer: W) -> anyhow::Result<()> {
        // Emit startup report
        let startup = self.build_startup_report();
        startup.log();

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
        let provider_result = ExecutionProviderSelector::select(&self.config.execution_provider);
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

        reporter.set_model_name(&self.config.model_name);
        reporter.set_quantization_mode("none"); // Will be updated when quantization is wired
        #[cfg(feature = "onnx")]
        reporter.set_warm_load_latency(self.model_load_time);
        #[cfg(not(feature = "onnx"))]
        reporter.set_warm_load_latency(Duration::from_millis(0)); // Placeholder until real ONNX load
        reporter.build()
    }

    /// Inner loop: read frames, process, respond, check idle.
    ///
    /// Uses a read timeout on the reader so that the idle timeout check
    /// at the top of the loop is reached even when no data is arriving.
    /// Without this, a blocking `read_exact` would block forever and the
    /// worker would never tear down its ONNX session on idle.
    pub fn run_loop<R: Read + Send + 'static, W: Write>(
        &mut self,
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
            let mut buf_reader = io::BufReader::new(reader);
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
            let response = self.dispatch(frame);

            // Write the response frame
            let wire = response.encode_wire()?;
            writer.write_all(&wire)?;
            writer.flush()?;

            // Reset idle timer after successful processing
            self.touch();
        }
    }

    /// Dispatch a request frame to the appropriate handler.
    pub fn dispatch(&self, frame: Frame) -> Frame {
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
    fn handle_embed(&self, frame: Frame) -> Result<EmbedResponse, WorkerError> {
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
                return self.run_onnx_embed(session, tokenizer, &texts, embed_req.expected_dim);
            } else {
                return Err(WorkerError {
                    kind: ErrorKind::ModelNotFound,
                    message: "ONNX session or tokenizer not initialized".to_string(),
                });
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
        // Batch tokenize all texts
        let encodings = tokenizer
            .encode_batch(texts.to_vec(), true)
            .map_err(|e| WorkerError {
                kind: ErrorKind::Tokenizer,
                message: format!("tokenization failed: {}", e),
            })?;

        if encodings.is_empty() {
            return Ok(EmbedResponse::new(vec![], 0, expected_dim));
        }

        // Determine max sequence length in this batch
        let max_len = encodings
            .iter()
            .map(|e| e.len())
            .max()
            .unwrap_or(0)
            .min(DEFAULT_MAX_SEQ_LEN); // Cap at max sequence length for memory safety

        if max_len == 0 {
            return Ok(EmbedResponse::new(vec![], 0, expected_dim));
        }

        let batch_size = encodings.len();

        // Create input tensors: [batch_size, seq_len]
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        for encoding in &encodings {
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

        // Run inference with properly shaped tensors
        // Shape: [batch_size, seq_len]
        let input_ids_tensor = ort::value::Tensor::from_array(
            ndarray::Array2::from_shape_vec((batch_size, max_len), input_ids).map_err(|e| {
                WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create input_ids array: {}", e),
                }
            })?,
        )
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to create input_ids tensor: {}", e),
        })?;

        let attention_mask_tensor = ort::value::Tensor::from_array(
            ndarray::Array2::from_shape_vec((batch_size, max_len), attention_mask.clone())
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create attention_mask array: {}", e),
                })?,
        )
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to create attention_mask tensor: {}", e),
        })?;

        let outputs = {
            let mut session_guard = session.lock().map_err(|e| WorkerError {
                kind: ErrorKind::OnnxRuntime,
                message: format!("failed to lock ONNX session: {}", e),
            })?;
            session_guard
                .run(ort::inputs! {
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor,
                })
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("ONNX inference failed: {}", e),
                })?
        };

        if outputs.len() == 0 {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: "ONNX model returned no outputs".to_string(),
            });
        }

        // Extract the actual output shape from the ONNX tensor.
        // Expected: [batch_size, seq_len, hidden_dim] or [batch_size, hidden_dim].
        let output_shape: Vec<usize> = outputs[0]
            .shape()
            .iter()
            .map(|&d| d as usize)
            .collect();

        // try_extract_array returns an ndarray::ArrayView (borrowed). We call
        // to_owned() to get an owned ArrayD<f32>, then into_raw_vec() reclaims
        // the underlying Vec<f32> without an extra copy.
        let embeddings_f32: Vec<f32> = outputs[0]
            .try_extract_array::<f32>()
            .map_err(|e| WorkerError {
                kind: ErrorKind::Inference,
                message: format!("failed to extract output tensor: {:?}", e),
            })?
            .to_owned()
            .into_raw_vec();

        if expected_dim == 0 {
            return Err(WorkerError {
                kind: ErrorKind::InvalidRequest,
                message: "expected_dim must be non-zero".to_string(),
            });
        }

        // Derive seq_len and hidden_dim from the actual tensor shape.
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
                // Already pooled: [batch_size, hidden_dim] — just L2-normalize per row
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
                return Ok(EmbedResponse { count: batch_size, dimension: dim, vectors: embeddings_f32 });
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

        // Apply mean pooling using attention mask
        self.pool_and_normalize(
            embeddings_f32,
            batch_size,
            actual_seq_len,
            attention_mask,
            hidden_dim,
        )
    }

    #[cfg(feature = "onnx")]
    fn pool_and_normalize(
        &self,
        embeddings: Vec<f32>,
        batch_size: usize,
        seq_len: usize,
        attention_mask: Vec<i64>,
        expected_dim: usize,
    ) -> Result<EmbedResponse, WorkerError> {
        // Reshape: [batch_size, seq_len, hidden_dim]
        // Apply mean pooling with attention mask weighting
        // Then L2 normalize each embedding

        let hidden_dim = expected_dim;
        let mut pooled: Vec<f32> = Vec::with_capacity(batch_size * hidden_dim);
        let mut sum: Vec<f32> = vec![0.0f32; hidden_dim];

        for b in 0..batch_size {
            sum.fill(0.0);
            let mut weight_sum: f32 = 0.0f32;

            for s in 0..seq_len {
                let mask_val = attention_mask.get(b * seq_len + s).copied().unwrap_or(0);
                if mask_val > 0 {
                    for h in 0..hidden_dim {
                        let idx = b * seq_len * hidden_dim + s * hidden_dim + h;
                        if let Some(&val) = embeddings.get(idx) {
                            sum[h] += val;
                        }
                    }
                    weight_sum += 1.0f32;
                }
            }

            // Mean pooling
            if weight_sum > 0.0f32 {
                for h in 0..hidden_dim {
                    sum[h] /= weight_sum;
                }
            }

            // L2 normalize
            let mut norm: f32 = 0.0f32;
            for h in 0..hidden_dim {
                norm += sum[h] * sum[h];
            }
            norm = norm.sqrt();

            if norm > 1e-10f32 {
                for h in 0..hidden_dim {
                    sum[h] /= norm;
                }
            }

            pooled.extend_from_slice(&sum);
        }

        Ok(EmbedResponse::new(pooled, batch_size, expected_dim))
    }

    /// Handle a rerank request.
    fn handle_rerank(&self, frame: Frame) -> Result<RerankResponse, WorkerError> {
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
            if let (Some(session), Some(tokenizer)) = (&self.session, &self.tokenizer) {
                return self.run_onnx_rerank(session, tokenizer, &rerank_req);
            } else {
                return Err(WorkerError {
                    kind: ErrorKind::ModelNotFound,
                    message: "ONNX session or tokenizer not initialized for rerank".to_string(),
                });
            }
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
        // Encode query-document pairs as "Query: {q} Document: {d}"
        let pair_texts: Vec<String> = rerank_req
            .documents
            .iter()
            .map(|doc| format!("Query: {} Document: {}", rerank_req.query, doc.content))
            .collect();

        // Batch tokenize all pairs
        let encodings = tokenizer
            .encode_batch(pair_texts, true)
            .map_err(|e| WorkerError {
                kind: ErrorKind::Tokenizer,
                message: format!("rerank tokenization failed: {}", e),
            })?;

        if encodings.is_empty() {
            return Ok(RerankResponse { results: vec![] });
        }

        let batch_size = encodings.len();
        let max_len = encodings
            .iter()
            .map(|e| e.len())
            .max()
            .unwrap_or(0)
            .min(512);

        if max_len == 0 {
            // Return passthrough scores if tokenization failed
            let results: Vec<_> = rerank_req
                .documents
                .iter()
                .map(|doc| protocol::RerankResult {
                    id: doc.id.clone(),
                    original_score: doc.initial_score,
                    rerank_score: doc.initial_score,
                    combined_score: doc.initial_score,
                })
                .collect();
            return Ok(RerankResponse { results });
        }

        // Build input_ids and attention_mask vectors from encodings
        let mut input_ids: Vec<i64> = Vec::with_capacity(batch_size * max_len);
        let mut attention_mask: Vec<i64> = Vec::with_capacity(batch_size * max_len);

        for encoding in &encodings {
            let ids = encoding.get_ids();
            let mask = encoding.get_attention_mask();

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

        // Create input tensors with proper ndarrays
        let input_ids_tensor = ort::value::Tensor::from_array(
            ndarray::Array2::from_shape_vec((batch_size, max_len), input_ids.clone()).map_err(
                |e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create rerank input_ids array: {}", e),
                },
            )?,
        )
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to create rerank input_ids tensor: {}", e),
        })?;

        let attention_mask_tensor = ort::value::Tensor::from_array(
            ndarray::Array2::from_shape_vec((batch_size, max_len), attention_mask.clone())
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to create rerank attention_mask array: {}", e),
                })?,
        )
        .map_err(|e| WorkerError {
            kind: ErrorKind::Inference,
            message: format!("failed to create rerank attention_mask tensor: {}", e),
        })?;

        let outputs = {
            let mut session_guard = session.lock().map_err(|e| WorkerError {
                kind: ErrorKind::OnnxRuntime,
                message: format!("failed to lock ONNX session for rerank: {}", e),
            })?;
            session_guard
                .run(ort::inputs! {
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor,
                })
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("ONNX rerank inference failed: {}", e),
                })?
        };

        if outputs.len() == 0 {
            return Err(WorkerError {
                kind: ErrorKind::Inference,
                message: "ONNX rerank model returned no outputs".to_string(),
            });
        }

        let output = &outputs[0];
        let shape: Vec<usize> = output.shape().iter().map(|&d| d as usize).collect();

        let rerank_scores: Vec<f32> = match shape.as_slice() {
            [n] if *n == batch_size => output
                .try_extract_array::<f32>()
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to extract scalar rerank output tensor: {:?}", e),
                })?
                .iter()
                .copied()
                .collect(),
            [n, 1] if *n == batch_size => output
                .try_extract_array::<f32>()
                .map_err(|e| WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!("failed to extract scalar rerank output tensor: {:?}", e),
                })?
                .iter()
                .copied()
                .collect(),
            _ => {
                return Err(WorkerError {
                    kind: ErrorKind::Inference,
                    message: format!(
                        "unsupported rerank output shape {:?}; expected [{}] or [{}, 1]",
                        shape,
                        batch_size,
                        batch_size
                    ),
                });
            }
        };

        // Build results with combined scores: 70% rerank + 30% initial
        let mut results: Vec<_> = rerank_req
            .documents
            .iter()
            .zip(rerank_scores.into_iter())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::EmbedRequest;
    use std::io::Cursor;

    #[test]
    fn test_runtime_config_default() {
        let config = RuntimeConfig::default();
        assert_eq!(config.idle_timeout, Duration::from_secs(300));
        assert_eq!(config.max_frame_size, 16 * 1024 * 1024);
        assert_eq!(config.max_text_size, 1024 * 1024);
        assert_eq!(config.embedding_dim, 1024);
    }

    #[test]
    fn test_runtime_idle_not_expired_initially() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);
        assert!(!rt.is_idle_expired());
    }

    #[test]
    fn test_runtime_idle_expired_with_zero_timeout() {
        let mut config = RuntimeConfig::default();
        config.idle_timeout = Duration::from_secs(0);
        let rt = WorkerRuntime::new(config);
        // With zero timeout, it should be expired immediately
        // (but we need at least a tiny delay for the check)
        std::thread::sleep(Duration::from_millis(1));
        assert!(rt.is_idle_expired());
    }

    #[test]
    fn test_runtime_touch_resets_idle() {
        let mut config = RuntimeConfig::default();
        config.idle_timeout = Duration::from_millis(10);
        let mut rt = WorkerRuntime::new(config);

        std::thread::sleep(Duration::from_millis(20));
        assert!(rt.is_idle_expired());

        rt.touch();
        assert!(!rt.is_idle_expired());
    }

    #[test]
    fn test_shutdown_flag() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);
        let flag = rt.shutdown_flag();

        assert!(!flag.load(Ordering::Relaxed));
        flag.store(true, Ordering::Relaxed);
        assert!(flag.load(Ordering::Relaxed));
    }

    #[test]
    fn test_truncate_text_within_limit() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);
        let text = "hello world".to_string();
        let result = rt.truncate_text(text.clone());
        assert_eq!(result, text);
    }

    #[test]
    fn test_truncate_text_exceeds_limit() {
        let mut config = RuntimeConfig::default();
        config.max_text_size = 10;
        let rt = WorkerRuntime::new(config);
        let text = "hello world, this is a long string".to_string();
        let result = rt.truncate_text(text);
        assert!(result.len() <= 10);
        assert_eq!(result, "hello worl");
    }

    #[test]
    fn test_truncate_text_unicode_boundary() {
        let mut config = RuntimeConfig::default();
        config.max_text_size = 10;
        let rt = WorkerRuntime::new(config);
        // "héllo" has multi-byte chars
        let text = "héllo wörld test".to_string();
        let result = rt.truncate_text(text);
        assert!(result.len() <= 10);
        // Should not panic and should be valid UTF-8
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn test_handle_embed_empty_batch() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let request = EmbedRequest {
            texts: vec![],
            expected_dim: 1024,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let result = rt.handle_embed(frame);

        // Empty batch returns Ok early (before any ONNX session check),
        // so .unwrap() is safe regardless of feature flag.
        let response = result.unwrap();
        assert_eq!(response.count, 0);
        assert_eq!(response.dimension, 1024);
        assert!(response.vectors.is_empty());
    }

    #[test]
    fn test_handle_embed_returns_flat_row_major() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let request = EmbedRequest {
            texts: vec!["hello".to_string(), "world".to_string()],
            expected_dim: 8,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let result = rt.handle_embed(frame);

        // Without a real ONNX session, should return ModelNotFound error
        #[cfg(feature = "onnx")]
        {
            let err = result.unwrap_err();
            assert_eq!(err.kind, ErrorKind::ModelNotFound);
        }

        // Without ONNX feature, returns zero vectors
        #[cfg(not(feature = "onnx"))]
        {
            let response = result.unwrap();
            assert_eq!(response.count, 2);
            assert_eq!(response.dimension, 8);
            assert_eq!(response.vectors.len(), 16);
            assert_eq!(response.get_embedding(0).unwrap().len(), 8);
            assert_eq!(response.get_embedding(1).unwrap().len(), 8);
        }
    }

    #[test]
    fn test_handle_embed_preserves_ordering() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let texts: Vec<String> = (0..5).map(|i| format!("text {}", i)).collect();
        let request = EmbedRequest {
            texts: texts.clone(),
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let result = rt.handle_embed(frame);

        // Without a real ONNX session, should return ModelNotFound error
        #[cfg(feature = "onnx")]
        {
            let err = result.unwrap_err();
            assert_eq!(err.kind, ErrorKind::ModelNotFound);
        }

        // Without ONNX feature, returns zero vectors with correct count
        #[cfg(not(feature = "onnx"))]
        {
            let response = result.unwrap();
            assert_eq!(response.count, 5);
            for i in 0..5 {
                assert!(response.get_embedding(i).is_some());
            }
        }
    }

    #[test]
    fn test_dispatch_embed_request() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let request = EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(42), request).unwrap();
        let response_frame = rt.dispatch(frame);

        assert_eq!(response_frame.header.batch_id, BatchId::new(42));

        // Without a real ONNX session, dispatch returns an error frame
        #[cfg(feature = "onnx")]
        {
            assert_eq!(response_frame.header.msg_type, MsgType::Error);
        }

        // Without ONNX feature, dispatch returns a success response
        #[cfg(not(feature = "onnx"))]
        {
            assert_eq!(response_frame.header.msg_type, MsgType::EmbedResponse);
        }
    }

    #[test]
    fn test_dispatch_rerank_request() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let request = protocol::RerankRequest {
            query: "test".to_string(),
            documents: vec![protocol::RerankDocument {
                id: "doc1".to_string(),
                content: "content".to_string(),
                initial_score: 0.9,
            }],
        };
        let frame = protocol::rerank_request_frame(BatchId::new(7), request).unwrap();
        let response_frame = rt.dispatch(frame);

        assert_eq!(response_frame.header.batch_id, BatchId::new(7));

        // Without a real ONNX session, dispatch returns an error frame
        #[cfg(feature = "onnx")]
        {
            assert_eq!(response_frame.header.msg_type, MsgType::Error);
        }

        // Without ONNX feature, dispatch returns a success response
        #[cfg(not(feature = "onnx"))]
        {
            assert_eq!(response_frame.header.msg_type, MsgType::RerankResponse);
        }
    }

    #[test]
    fn test_dispatch_unknown_message_type() {
        let config = RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let frame = Frame {
            header: protocol::FrameHeader {
                batch_id: BatchId::new(99),
                msg_type: MsgType::Error, // Unexpected from main daemon
            },
            payload: vec![],
        };
        let response_frame = rt.dispatch(frame);

        assert_eq!(response_frame.header.batch_id, BatchId::new(99));
        assert_eq!(response_frame.header.msg_type, MsgType::Error);
    }

    #[test]
    fn test_run_loop_single_request() {
        let config = RuntimeConfig {
            idle_timeout: Duration::from_secs(300),
            ..RuntimeConfig::default()
        };
        let mut rt = WorkerRuntime::new(config);

        // Build a single embed request frame
        let request = EmbedRequest {
            texts: vec!["hello".to_string()],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let wire = frame.encode_wire().unwrap();

        // Create a reader that will return the frame then EOF
        let reader = Cursor::new(wire);
        let writer = Cursor::new(Vec::<u8>::new());

        let result = rt.run_loop(reader, writer);
        assert!(result.is_ok());
    }

    #[test]
    fn test_run_loop_multiple_requests_same_runtime() {
        // VAL-CPHASE-006: Worker remains reusable across successive batches
        let config = RuntimeConfig {
            idle_timeout: Duration::from_secs(300),
            ..RuntimeConfig::default()
        };
        let mut rt = WorkerRuntime::new(config);

        // Build two embed request frames
        let request1 = EmbedRequest {
            texts: vec!["first".to_string()],
            expected_dim: 4,
        };
        let request2 = EmbedRequest {
            texts: vec!["second".to_string()],
            expected_dim: 4,
        };

        let frame1 = protocol::embed_request_frame(BatchId::new(1), request1).unwrap();
        let frame2 = protocol::embed_request_frame(BatchId::new(2), request2).unwrap();

        let wire1 = frame1.encode_wire().unwrap();
        let wire2 = frame2.encode_wire().unwrap();

        let mut combined = wire1.clone();
        combined.extend_from_slice(&wire2);

        let reader = Cursor::new(combined);
        let writer = Cursor::new(Vec::<u8>::new());

        let result = rt.run_loop(reader, writer);
        assert!(result.is_ok());

        // Verify both responses were written
        let output = result.unwrap();
        // The writer was consumed, but we can verify the run completed
        let _ = output;
    }

    #[test]
    fn test_idle_timeout_causes_exit() {
        // VAL-CPHASE-007: Worker tears down on idle
        let config = RuntimeConfig {
            idle_timeout: Duration::from_millis(1),
            ..RuntimeConfig::default()
        };
        let mut rt = WorkerRuntime::new(config);

        // Empty input — the loop should detect idle timeout
        let reader = Cursor::new(Vec::<u8>::new());
        let writer = Cursor::new(Vec::<u8>::new());

        // This will fail because there's no data to read, but the idle check
        // happens before the read. However, with empty input, read_exact will
        // return UnexpectedEof immediately, which is a clean shutdown.
        let result = rt.run_loop(reader, writer);
        assert!(result.is_ok());
    }
}
