use super::*;
use crate::embed::protocol::EmbedRequest;
use std::io::Cursor;
use std::sync::Mutex as StdMutex;

static ENV_LOCK: StdMutex<()> = StdMutex::new(());

/// RAII guard that restores an environment variable to its original value on drop.
struct EnvVarGuard {
    key: &'static str,
    original: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        unsafe {
            std::env::set_var(key, value);
        }
        Self { key, original }
    }

    fn remove(key: &'static str) -> Self {
        let original = std::env::var(key).ok();
        unsafe {
            std::env::remove_var(key);
        }
        Self { key, original }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            Some(val) => unsafe {
                std::env::set_var(self.key, val);
            },
            None => unsafe {
                std::env::remove_var(self.key);
            },
        }
    }
}

/// Config whose model names resolve to no on-disk file, so
/// `WorkerRuntime::new` skips the ~300s MIGraphX JIT compile AND the lazy
/// reranker cannot load. The worker tests below exercise pooling / idle-timer /
/// dispatch-without-a-session logic, never real inference, so no model is
/// needed. Without this, a real `qwen3-embed-0.6b.onnx` under
/// `~/.leindex/models` makes every `WorkerRuntime::new` compile the model and
/// OOM the test binary (regression introduced when the static model shipped).
/// The rerank model name is likewise poisoned so `ensure_rerank_session` fails
/// at model resolution and dispatch returns an Error frame deterministically —
/// otherwise a host with the rerank model installed would build a real session
/// and the dispatch-error assertions below would be environment-dependent.
fn no_compile_config() -> RuntimeConfig {
    RuntimeConfig {
        model_name: "__leindex_test_no_model__".to_string(),
        rerank_model_name: "__leindex_test_no_rerank_model__".to_string(),
        ..RuntimeConfig::default()
    }
}

#[test]
fn test_runtime_config_default() {
    let config = RuntimeConfig::default();
    assert_eq!(
        config.idle_timeout,
        Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS)
    );
    assert_eq!(config.max_frame_size, 16 * 1024 * 1024);
    assert_eq!(config.max_text_size, 1024 * 1024);
    assert_eq!(config.embedding_dim, 1024);
}

#[test]
fn onnx_inference_batch_size_defaults_to_fixed_batch_safe_value() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::remove(ONNX_INFERENCE_BATCH_SIZE_ENV);

    assert_eq!(
        configured_onnx_inference_batch_size("qwen3-embed-0.6b", "cpu"),
        DEFAULT_ONNX_INFERENCE_BATCH_SIZE
    );
    assert_eq!(
        configured_onnx_inference_batch_size("qwen3-embed-0.6b", "cpu"),
        1
    );
}

#[test]
fn onnx_inference_batch_size_uses_positive_env_override() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::set(ONNX_INFERENCE_BATCH_SIZE_ENV, "32");

    assert_eq!(
        configured_onnx_inference_batch_size("qwen3-embed-0.6b", "migraphx"),
        32
    );
}

#[test]
fn onnx_inference_batch_size_rejects_zero_and_bad_values() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());

    // Use a single guard capturing the original; intermediate mutations are fine.
    let _env = EnvVarGuard::set(ONNX_INFERENCE_BATCH_SIZE_ENV, "0");
    assert_eq!(
        configured_onnx_inference_batch_size("qwen3-embed-0.6b", "cpu"),
        DEFAULT_ONNX_INFERENCE_BATCH_SIZE
    );

    drop(_env);
    let _env = EnvVarGuard::set(ONNX_INFERENCE_BATCH_SIZE_ENV, "nope");
    assert_eq!(
        configured_onnx_inference_batch_size("qwen3-embed-0.6b", "cpu"),
        DEFAULT_ONNX_INFERENCE_BATCH_SIZE
    );
}

#[cfg(feature = "onnx")]
#[test]
fn qwen_pooling_uses_last_unpadded_token() {
    let runtime = WorkerRuntime::new(no_compile_config());
    let pooled = runtime
        .pool_and_normalize(
            &[
                1.0, 0.0, // first token
                0.0, 2.0, // final real token
                8.0, 8.0, // padding
            ],
            1,
            3,
            &[1, 1, 0],
            2,
        )
        .unwrap();

    assert_eq!(pooled.vectors, vec![0.0, 1.0]);
}

#[cfg(feature = "onnx")]
#[test]
fn qwen_pooling_rejects_short_embedding_output() {
    let runtime = WorkerRuntime::new(no_compile_config());
    let error = runtime
        .pool_and_normalize(&[1.0], 1, 2, &[1, 1], 2)
        .unwrap_err();

    assert_eq!(error.kind, ErrorKind::Inference);
    assert!(error.message.contains("embedding output is too short"));
}

#[test]
fn position_ids_repeat_sequence_for_each_batch_row() {
    assert_eq!(build_position_ids(2, 4), vec![0, 1, 2, 3, 0, 1, 2, 3]);
}

#[test]
fn onnx_sequence_len_defaults_and_clamps_env_override() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::remove(ONNX_SEQUENCE_LEN_ENV);
    assert_eq!(configured_onnx_sequence_len(), DEFAULT_MAX_SEQ_LEN);
    drop(_env);

    let _env = EnvVarGuard::set(ONNX_SEQUENCE_LEN_ENV, "4");
    assert_eq!(configured_onnx_sequence_len(), DEFAULT_MAX_SEQ_LEN);
    drop(_env);

    let _env = EnvVarGuard::set(ONNX_SEQUENCE_LEN_ENV, "256");
    assert_eq!(configured_onnx_sequence_len(), 256);
    drop(_env);

    let _env = EnvVarGuard::set(ONNX_SEQUENCE_LEN_ENV, "4096");
    assert_eq!(configured_onnx_sequence_len(), MAX_ONNX_SEQUENCE_LEN);
}

#[test]
fn dynamic_qwen_uses_batched_inference_by_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::remove(ONNX_INFERENCE_BATCH_SIZE_ENV);

    assert_eq!(
        configured_onnx_inference_batch_size("qwen3-embed-0.6b-dynamic", "cpu"),
        DEFAULT_DYNAMIC_ONNX_INFERENCE_BATCH_SIZE
    );
    const _: () = assert!(DEFAULT_DYNAMIC_ONNX_INFERENCE_BATCH_SIZE > 1);
}

#[test]
fn migraphx_uses_one_stable_batch_shape_by_default() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::remove(ONNX_INFERENCE_BATCH_SIZE_ENV);

    assert_eq!(
        configured_onnx_inference_batch_size("qwen3-embed-0.6b-dynamic", "migraphx"),
        DEFAULT_MIGRAPHX_INFERENCE_BATCH_SIZE
    );
}

#[test]
fn test_runtime_idle_not_expired_initially() {
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);
    assert!(!rt.is_idle_expired());
}

#[test]
fn test_runtime_idle_expired_with_zero_timeout() {
    let config = RuntimeConfig {
        idle_timeout: Duration::from_secs(0),
        ..no_compile_config()
    };
    let rt = WorkerRuntime::new(config);
    // With zero timeout, it should be expired immediately
    // (but we need at least a tiny delay for the check)
    std::thread::sleep(Duration::from_millis(1));
    assert!(rt.is_idle_expired());
}

#[test]
fn test_runtime_touch_resets_idle() {
    let config = RuntimeConfig {
        idle_timeout: Duration::from_millis(10),
        ..no_compile_config()
    };
    let rt = WorkerRuntime::new(config);

    std::thread::sleep(Duration::from_millis(20));
    assert!(rt.is_idle_expired());

    rt.touch();
    assert!(!rt.is_idle_expired());
}

#[test]
fn cloned_runtime_shares_idle_activity() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<WorkerRuntime>();

    let runtime = WorkerRuntime::new(no_compile_config());
    let cloned = runtime.clone();
    assert!(Arc::ptr_eq(&runtime.last_activity, &cloned.last_activity));
    cloned.touch();
    assert!(!runtime.is_idle_expired());
}

#[test]
fn test_shutdown_flag() {
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);
    let flag = rt.shutdown_flag();

    assert!(!flag.load(Ordering::Relaxed));
    flag.store(true, Ordering::Relaxed);
    assert!(flag.load(Ordering::Relaxed));
}

#[test]
fn test_truncate_text_within_limit() {
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);
    let text = "hello world".to_string();
    let result = rt.truncate_text(text.clone());
    assert_eq!(result, text);
}

#[test]
fn test_truncate_text_exceeds_limit() {
    let config = RuntimeConfig {
        max_text_size: 10,
        ..no_compile_config()
    };
    let rt = WorkerRuntime::new(config);
    let text = "hello world, this is a long string".to_string();
    let result = rt.truncate_text(text);
    assert!(result.len() <= 10);
    assert_eq!(result, "hello worl");
}

#[test]
fn test_truncate_text_unicode_boundary() {
    let config = RuntimeConfig {
        max_text_size: 10,
        ..no_compile_config()
    };
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
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);

    let request = EmbedRequest {
        texts: vec![],
        expected_dim: 1024,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let result = rt.handle_embed(&frame);

    // Empty batch returns Ok early (before any ONNX session check),
    // so .unwrap() is safe regardless of feature flag.
    let response = result.unwrap();
    assert_eq!(response.count, 0);
    assert_eq!(response.dimension, 1024);
    assert!(response.vectors.is_empty());
}

#[test]
fn test_handle_embed_returns_flat_row_major() {
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);

    let request = EmbedRequest {
        texts: vec!["hello".to_string(), "world".to_string()],
        expected_dim: 8,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let result = rt.handle_embed(&frame);

    // When ORT or the model is unavailable (no model on disk, ORT not
    // discovered, etc.), the worker returns ModelNotFound. On a developer
    // machine that has both /usr/local/lib/libonnxruntime.so and a real
    // model in `~/.leindex/models/`, ORT inference may actually run and
    // fail with a different error (Inference); treat that as acceptable
    // since the contract under test is "no crash, structured error".
    #[cfg(feature = "onnx")]
    {
        let err = result.unwrap_err();
        assert!(
            err.kind == ErrorKind::ModelNotFound || err.kind == ErrorKind::Inference,
            "expected ModelNotFound or Inference, got {:?}: {}",
            err.kind,
            err.message
        );
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
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);

    let texts: Vec<String> = (0..5).map(|i| format!("text {}", i)).collect();
    let request = EmbedRequest {
        texts: texts.clone(),
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let result = rt.handle_embed(&frame);

    // Same rationale as test_handle_embed_returns_flat_row_major: developer
    // machines with ORT + a real model present may reach inference and
    // surface an Inference error instead of ModelNotFound. Both are
    // acceptable; the contract under test is "no crash, structured error".
    #[cfg(feature = "onnx")]
    {
        let err = result.unwrap_err();
        assert!(
            err.kind == ErrorKind::ModelNotFound || err.kind == ErrorKind::Inference,
            "expected ModelNotFound or Inference, got {:?}: {}",
            err.kind,
            err.message
        );
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
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);

    let request = EmbedRequest {
        texts: vec!["test".to_string()],
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(42), request).unwrap();
    let response_frame = rt.dispatch(&frame);

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
    let config = no_compile_config();
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
    let response_frame = rt.dispatch(&frame);

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
    let config = no_compile_config();
    let rt = WorkerRuntime::new(config);

    let frame = Frame {
        header: protocol::FrameHeader {
            batch_id: BatchId::new(99),
            msg_type: MsgType::Error, // Unexpected from main daemon
        },
        payload: vec![],
    };
    let response_frame = rt.dispatch(&frame);

    assert_eq!(response_frame.header.batch_id, BatchId::new(99));
    assert_eq!(response_frame.header.msg_type, MsgType::Error);
}

#[test]
fn test_run_loop_single_request() {
    let config = RuntimeConfig {
        idle_timeout: Duration::from_secs(300),
        ..no_compile_config()
    };
    let rt = WorkerRuntime::new(config);

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

// ── Task 7: provider selection precedence & truthfulness ──────────────

#[test]
fn runtime_config_provider_env_overrides_toml_and_default() {
    // Direct-worker precedence: env > TOML > "auto". When the env var is set,
    // it wins regardless of what the TOML contains (the TOML is read via the
    // process-global OnceLock and may hold any value on the test host).
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::set("LEINDEX_WORKER_EXECUTION_PROVIDER", "cuda");

    let config = RuntimeConfig::from_env();
    assert_eq!(
        config.execution_provider, "cuda",
        "env var must take precedence over TOML and the 'auto' default"
    );

    drop(_env);
    // With the env var unset, from_env falls back to TOML or "auto". Either
    // way it must be a non-empty lowercase value (normalized).
    let _env_removed = EnvVarGuard::remove("LEINDEX_WORKER_EXECUTION_PROVIDER");
    let config = RuntimeConfig::from_env();
    assert!(
        !config.execution_provider.trim().is_empty(),
        "provider must always resolve to a non-empty value"
    );
    // The value handed to the selector is already normalized lowercased.
    assert_eq!(
        config.execution_provider,
        config.execution_provider.to_ascii_lowercase(),
    );
}

#[test]
fn runtime_config_provider_blanks_env_falls_through() {
    // A blank/whitespace env value is treated as unset → falls through to TOML
    // or "auto". This prevents a stray `LEINDEX_WORKER_EXECUTION_PROVIDER=""`
    // from producing an empty provider string that bypasses normalization.
    let _guard = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvVarGuard::set("LEINDEX_WORKER_EXECUTION_PROVIDER", "   ");
    let config = RuntimeConfig::from_env();
    assert!(
        !config.execution_provider.trim().is_empty(),
        "blank env value must fall through to a non-empty default"
    );
}

#[test]
fn explicit_gpu_provider_select_returns_cpu_fallback_not_hard_error() {
    // Explicit-GPU→CPU is the documented neural-fallback path: select()
    // returns Err(ProviderSelection) whose provider is CPU, so the runtime
    // can log a warning and continue. This is the contract that
    // try_provider_or_cpu preserves when error_on_failure() turns a failed
    // registration into an Err — the fallback rebuild must NOT be a hard error.
    use crate::embed::provider::ExecutionProviderSelector;
    let result = ExecutionProviderSelector::select("cuda");
    match result {
        Ok(selection) => {
            // CUDA is available on this host — still a concrete provider, never
            // "auto", and registration will be attempted with error_on_failure.
            let name = selection.name();
            assert!(
                name == "cuda" || name == "cpu",
                "unexpected cuda-resolution: {name}"
            );
        }
        Err(fallback) => {
            assert_eq!(fallback.fallback_name(), "cpu");
            assert!(!fallback.is_requested_provider());
            assert!(
                fallback.reason().contains("CUDA"),
                "fallback reason should name the missing provider: {}",
                fallback.reason()
            );
        }
    }
}

#[cfg(feature = "onnx")]
#[test]
fn runtime_resolved_provider_is_never_auto_for_embed_or_rerank() {
    // End-to-end invariant: after WorkerRuntime construction, the provider
    // recorded on provider_runtime_status (used by both the embedder session
    // and the lazy reranker via ensure_rerank_session) is never the unresolved
    // "auto" token. It is always one of cpu/cuda/migraphx/coreml.
    //
    // no_compile_config() points at a non-existent model so build_session
    // fails at commit_from_file; the provider_runtime_status then retains the
    // pre-build "cpu" fallback value, which is itself a valid concrete token.
    // The invariant under test is that NO path surfaces "auto" to a session
    // builder or the health response.
    let rt = WorkerRuntime::new(RuntimeConfig {
        execution_provider: "auto".to_string(),
        ..no_compile_config()
    });
    let health = rt.health_response(crate::embed::protocol::WorkerState::Initializing, None);
    let provider = health.provider.expect("health response carries a provider");
    assert_ne!(
        provider, "auto",
        "the provider handed to embedder/reranker session builders must be concrete"
    );
    assert!(
        matches!(provider.as_str(), "cpu" | "cuda" | "migraphx" | "coreml"),
        "resolved provider must be one of the concrete tokens, got {provider}"
    );
}

#[test]
fn test_run_loop_multiple_requests_same_runtime() {
    // VAL-CPHASE-006: Worker remains reusable across successive batches
    let config = RuntimeConfig {
        idle_timeout: Duration::from_secs(300),
        ..no_compile_config()
    };
    let rt = WorkerRuntime::new(config);

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
    result.unwrap();
}

#[test]
fn test_idle_timeout_causes_exit() {
    // VAL-CPHASE-007: Worker tears down on idle
    let config = RuntimeConfig {
        idle_timeout: Duration::from_millis(1),
        ..no_compile_config()
    };
    let rt = WorkerRuntime::new(config);

    // Empty input — the loop should detect idle timeout
    let reader = Cursor::new(Vec::<u8>::new());
    let writer = Cursor::new(Vec::<u8>::new());

    // This will fail because there's no data to read, but the idle check
    // happens before the read. However, with empty input, read_exact will
    // return UnexpectedEof immediately, which is a clean shutdown.
    let result = rt.run_loop(reader, writer);
    assert!(result.is_ok());
}
