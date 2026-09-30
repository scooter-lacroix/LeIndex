use super::*;

impl WorkerRuntime {
    #[cfg(feature = "onnx")]
    #[allow(clippy::type_complexity)]
    pub(super) fn init_onnx(
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
    pub(super) fn build_cpu_session(
        model_path: &std::path::Path,
        ort_threads: usize,
    ) -> Result<Session, ort::Error> {
        Session::builder()?
            .with_intra_threads(ort_threads)?
            .with_memory_pattern(false)?
            .with_log_level(LogLevel::Warning)?
            .with_optimization_level(GraphOptimizationLevel::Level1)?
            .with_execution_providers([ort::ep::CPU::default().build()])?
            .commit_from_file(model_path)
    }

    #[cfg(feature = "onnx")]
    pub(super) fn probe_migraphx_compile_timeout(
        model_path: &std::path::Path,
        provider_name: &str,
        ort_threads: usize,
        max_wait: Duration,
    ) -> Result<(), String> {
        if std::env::var_os("LEINDEX_MIGRAPHX_PROBE_CHILD").is_some() {
            return Err("refusing recursive MIGraphX probe child".to_string());
        }
        let executable = std::env::current_exe()
            .map_err(|error| format!("failed to resolve worker executable: {}", error))?;
        let mut child = std::process::Command::new(executable)
            .arg(crate::embed::worker_main::INTERNAL_WORKER_TOKEN)
            .arg("--migraphx-probe")
            .arg(model_path)
            .arg(provider_name)
            .arg(ort_threads.to_string())
            .env("LEINDEX_MIGRAPHX_PROBE_CHILD", "1")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .map_err(|error| format!("failed to spawn MIGraphX probe child: {}", error))?;
        let deadline = Instant::now() + max_wait;
        loop {
            match child.try_wait() {
                Ok(Some(status)) if status.success() => return Ok(()),
                Ok(Some(status)) => {
                    return Err(format!(
                        "MIGraphX probe child exited with status {}",
                        status
                    ));
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(25));
                }
                Ok(None) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "MIGraphX compile probe timed out after {:?}; child killed",
                        max_wait
                    ));
                }
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("MIGraphX probe child wait failed: {}", error));
                }
            }
        }
    }

    #[cfg(feature = "onnx")]
    pub(crate) fn run_migraphx_probe_child(
        model_path: &std::path::Path,
        provider_name: &str,
        ort_threads: usize,
    ) -> Result<(), String> {
        if std::env::var_os("LEINDEX_MIGRAPHX_PROBE_CHILD").is_none() {
            return Err("MIGraphX probe child marker is missing".to_string());
        }
        match crate::embed::ort_discovery::discover_and_init() {
            crate::embed::ort_discovery::InitResult::Initialized(_) => {}
            crate::embed::ort_discovery::InitResult::NotFound { last_error, .. } => {
                return Err(format!(
                    "failed to initialize ONNX Runtime: {}",
                    last_error.unwrap_or_else(|| "unknown ORT discovery error".to_string())
                ));
            }
        }
        let (session, provider_status) =
            Self::build_session_without_probe(model_path, provider_name, ort_threads)
                .map_err(|error| format!("failed to build probe session: {}", error))?;
        if provider_status.execution_provider != "migraphx" {
            return Err(format!(
                "probe provider was {} instead of MIGraphX",
                provider_status.execution_provider
            ));
        }
        // Smoke-inference shapes must match the model's declared input
        // shape. A statically exported graph accepts ONLY its exact
        // dimensions — e.g. the b8-s128 qwen3 export rejects the previously
        // hardcoded b1-s16 probe tensors ("Got: 1, Expected: 8"), which
        // failed the probe on every start and forced the worker onto CPU.
        // Static (positive) dims are taken from the session metadata;
        // dynamic dims (negative in ORT metadata) keep the small probe
        // defaults.
        let declared_dims: Vec<i64> = session
            .inputs()
            .iter()
            .find(|input| input.name() == "input_ids")
            .and_then(|input| {
                input
                    .dtype()
                    .tensor_shape()
                    .map(|shape| shape.iter().copied().collect())
            })
            .unwrap_or_default();
        let static_dim = |index: usize| -> Option<usize> {
            declared_dims
                .get(index)
                .and_then(|dim| (*dim > 0).then_some(*dim as usize))
        };
        let batch_size = static_dim(0).unwrap_or(1);
        let max_len = static_dim(1).unwrap_or_else(|| configured_onnx_sequence_len().min(16));
        let session = Arc::new(Mutex::new(session));
        let make_tensor = |data: Vec<i64>, label: &str| {
            ort::value::Tensor::from_array(
                ndarray::Array2::from_shape_vec((batch_size, max_len), data)
                    .map_err(|error| format!("{} array: {}", label, error))?,
            )
            .map_err(|error| format!("{} tensor: {}", label, error))
        };
        let input_ids = make_tensor(vec![0; batch_size * max_len], "input_ids")?;
        let attention_mask = make_tensor(vec![0; batch_size * max_len], "attention_mask")?;
        let position_ids = make_tensor(build_position_ids(batch_size, max_len), "position_ids")?;
        let token_type_ids = make_tensor(vec![0; batch_size * max_len], "token_type_ids")?;
        let mut guard = session
            .lock()
            .map_err(|error| format!("session lock: {}", error))?;
        let uses_position_ids = guard
            .inputs()
            .iter()
            .any(|input| input.name() == "position_ids");
        let uses_token_type_ids = guard
            .inputs()
            .iter()
            .any(|input| input.name() == "token_type_ids");
        let result = match (uses_position_ids, uses_token_type_ids) {
            (true, true) => guard.run(ort::inputs! {
                "input_ids" => input_ids, "attention_mask" => attention_mask,
                "position_ids" => position_ids, "token_type_ids" => token_type_ids,
            }),
            (true, false) => guard.run(ort::inputs! {
                "input_ids" => input_ids, "attention_mask" => attention_mask,
                "position_ids" => position_ids,
            }),
            (false, true) => guard.run(ort::inputs! {
                "input_ids" => input_ids, "attention_mask" => attention_mask,
                "token_type_ids" => token_type_ids,
            }),
            (false, false) => guard.run(ort::inputs! {
                "input_ids" => input_ids, "attention_mask" => attention_mask,
            }),
        };
        result
            .map(|_| ())
            .map_err(|error| format!("MIGraphX probe inference: {}", error))
    }

    #[cfg(feature = "onnx")]
    pub(super) fn build_session_without_probe(
        model_path: &std::path::Path,
        provider_name: &str,
        ort_threads: usize,
    ) -> Result<(Session, ProviderRuntimeStatus), ort::Error> {
        let optimization_level = match provider_name {
            "migraphx" | "rocm" => GraphOptimizationLevel::Level3,
            _ => GraphOptimizationLevel::Level1,
        };
        let session_builder = Session::builder()?
            .with_intra_threads(ort_threads)?
            .with_memory_pattern(false)?
            .with_log_level(LogLevel::Warning)?
            .with_optimization_level(optimization_level)?;
        let (mut session_builder, provider_status) =
            attach_execution_provider(session_builder, provider_name, ort_threads)?;
        session_builder
            .commit_from_file(model_path)
            .map(|session| (session, provider_status))
    }
    #[cfg(feature = "onnx")]
    pub(super) fn build_session(
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
        if matches!(provider_name, "migraphx" | "rocm") {
            // First-time MIGraphX compilation of a large model (the 0.6B
            // reranker compiles for well over 20s on cold cache) exceeds the
            // old hardcoded 20s budget, which silently forced the model onto
            // CPU — the single worst latency outcome (minutes of CPU
            // inference for one rerank batch). Default raised to 120s and
            // overridable via LEINDEX_MIGRAPHX_PROBE_TIMEOUT_SECS for
            // constrained environments.
            let default_probe_timeout = Duration::from_secs(120);
            let probe_timeout = std::env::var("LEINDEX_MIGRAPHX_PROBE_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse::<u64>().ok())
                .map(Duration::from_secs)
                .unwrap_or(default_probe_timeout);
            return match Self::probe_migraphx_compile_timeout(
                model_path,
                provider_name,
                ort_threads,
                probe_timeout,
            ) {
                Ok(()) => Ok(SessionBuildOutcome {
                    session,
                    provider_status,
                }),
                Err(reason) => {
                    tracing::warn!(
                        "{}; falling back to CPU for {}",
                        reason,
                        model_path.display()
                    );
                    return Ok(SessionBuildOutcome {
                        session: Self::build_cpu_session(model_path, ort_threads)?,
                        provider_status: ProviderRuntimeStatus::fallback_to_cpu(format!(
                            "MIGraphX compile probe: {}",
                            reason
                        )),
                    });
                }
            };
        }
        Ok(SessionBuildOutcome {
            session,
            provider_status,
        })
    }
}
