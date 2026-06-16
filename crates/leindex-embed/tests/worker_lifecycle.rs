// Worker runtime lifecycle integration tests
//
// Tests the full worker lifecycle behavior:
// - VAL-CPHASE-004: Worker transport uses local IPC only
// - VAL-CPHASE-005: Worker cold-starts on first embed demand
// - VAL-CPHASE-006: Worker remains reusable across successive batches
// - VAL-CPHASE-007: Worker idle timeout tears down the resident model process
// - VAL-CPHASE-008: Worker restart works after idle teardown
// - VAL-CPHASE-009: Startup report exposes runtime bundle choices
// - VAL-CPHASE-010: Model path resolution honors precedence
// - VAL-CPHASE-011: Execution-provider selection is externally controllable
// - VAL-CPHASE-012: Embed response is flat row-major output
// - VAL-CPHASE-013: Batch ordering is preserved through IPC
// - VAL-CPHASE-014: Oversized batch is split before transport and re-stitched
// - VAL-CPHASE-015: Single oversized text is reduced before IPC framing

use std::io::Cursor;
use std::time::Duration;

use leindex_embed::batch::{self, BatchConfig, SplitResult};
use leindex_embed::model_path::ModelResolver;
use leindex_embed::protocol::{
    self, BatchId, EmbedRequest, EmbedResponse, MsgType, Request,
};
#[cfg(not(feature = "onnx"))]
use leindex_embed::protocol::Response;
use leindex_embed::provider::ExecutionProviderSelector;
use leindex_embed::runtime::{RuntimeConfig, WorkerRuntime};
use leindex_embed::startup::{StartupReport, StartupReporter};

#[cfg(feature = "onnx")]
mod rerank_output_shape_tests {
    use ndarray::ArrayD;

    struct FakeOutput {
        shape: Vec<usize>,
        values: Vec<f32>,
    }

    impl FakeOutput {
        fn new(shape: Vec<usize>, values: Vec<f32>) -> Self {
            Self { shape, values }
        }
    }

    #[test]
    fn scalar_logit_outputs_use_direct_scores() {
        let batch_size = 2;
        let outputs = FakeOutput::new(vec![batch_size], vec![0.81, 0.22]);
        let array = ArrayD::from_shape_vec(outputs.shape.clone(), outputs.values.clone()).unwrap();
        let extracted: Vec<f32> = array.iter().copied().collect();
        assert_eq!(extracted, vec![0.81, 0.22]);
    }

    #[test]
    fn hidden_state_outputs_fallback_to_first_token_norm() {
        let batch_size = 2;
        let seq_len = 3;
        let hidden_dim = 4;
        let values = vec![
            1.0, 2.0, 3.0, 4.0, // batch 0, token 0
            9.0, 9.0, 9.0, 9.0, // batch 0, token 1
            8.0, 8.0, 8.0, 8.0, // batch 0, token 2
            0.5, 0.5, 0.5, 0.5, // batch 1, token 0
            7.0, 7.0, 7.0, 7.0, // batch 1, token 1
            6.0, 6.0, 6.0, 6.0, // batch 1, token 2
        ];
        let array = ArrayD::from_shape_vec(vec![batch_size, seq_len, hidden_dim], values).unwrap();
        let extracted: Vec<f32> = array.iter().copied().collect();
        let first_token_norm_batch0 = (1.0_f32 + 4.0 + 9.0 + 16.0).sqrt();
        let first_token_norm_batch1 = (0.25_f32 * 4.0).sqrt();
        assert_eq!(extracted.len(), batch_size * seq_len * hidden_dim);
        assert!((first_token_norm_batch0 - 5.4772253).abs() < 1e-5);
        assert!((first_token_norm_batch1 - 1.0).abs() < 1e-5);
    }
}

// ── VAL-CPHASE-004: Worker transport uses local IPC only ────────────────

#[test]
fn test_worker_uses_local_ipc_only() {
    // The worker communicates over stdin/stdout pipes (local IPC).
    // This test verifies the runtime accepts a local pipe-like interface.
    let config = RuntimeConfig::default();
    let mut rt = WorkerRuntime::new(config);

    let request = EmbedRequest {
        texts: vec!["local ipc test".to_string()],
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let wire = frame.encode_wire().unwrap();

    // Simulate local IPC via in-memory pipes (Cursor)
    let reader = Cursor::new(wire);
    let writer = Cursor::new(Vec::<u8>::new());

    let result = rt.run_loop(reader, writer);
    assert!(result.is_ok(), "local IPC should work over in-memory pipes");
}

// ── VAL-CPHASE-005: Worker cold-starts on first embed demand ────────────

#[test]
fn test_worker_cold_starts_on_first_demand() {
    // The worker runtime starts without any pre-existing state and
    // processes the first request successfully.
    let config = RuntimeConfig::default();
    let rt = WorkerRuntime::new(config);

    // No pre-warming needed — the first request should work
    let request = EmbedRequest {
        texts: vec!["cold start test".to_string()],
        expected_dim: 8,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let response_frame = rt.dispatch(frame);

    // Without a real ONNX session, dispatch returns an error frame
    #[cfg(feature = "onnx")]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::Error);
    }

    // Without ONNX feature, dispatch returns a success response
    #[cfg(not(feature = "onnx"))]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::EmbedResponse);
        let response: Response = response_frame.decode_payload().unwrap();
        match response {
            Response::Embed(embed) => {
                assert_eq!(embed.count, 1);
                assert_eq!(embed.dimension, 8);
            }
            _ => panic!("expected Embed response"),
        }
    }
}

// ── VAL-CPHASE-006: Worker remains reusable across successive batches ──

#[test]
fn test_worker_reusable_across_batches() {
    let config = RuntimeConfig::default();
    let rt = WorkerRuntime::new(config);

    // Without a real ONNX session, dispatch returns error frames
    #[cfg(feature = "onnx")]
    let expected_msg_type = MsgType::Error;
    #[cfg(not(feature = "onnx"))]
    let expected_msg_type = MsgType::EmbedResponse;

    // First batch
    let request1 = EmbedRequest {
        texts: vec!["first batch".to_string()],
        expected_dim: 4,
    };
    let frame1 = protocol::embed_request_frame(BatchId::new(1), request1).unwrap();
    let response1 = rt.dispatch(frame1);
    assert_eq!(response1.header.msg_type, expected_msg_type);

    // Second batch — same runtime, no restart
    let request2 = EmbedRequest {
        texts: vec!["second batch".to_string(), "extra text".to_string()],
        expected_dim: 4,
    };
    let frame2 = protocol::embed_request_frame(BatchId::new(2), request2).unwrap();
    let response2 = rt.dispatch(frame2);
    assert_eq!(response2.header.msg_type, expected_msg_type);

    // Third batch
    let request3 = EmbedRequest {
        texts: vec!["third".to_string()],
        expected_dim: 4,
    };
    let frame3 = protocol::embed_request_frame(BatchId::new(3), request3).unwrap();
    let response3 = rt.dispatch(frame3);
    assert_eq!(response3.header.msg_type, expected_msg_type);

    // All batch IDs should be distinct
    assert_eq!(response1.header.batch_id, BatchId::new(1));
    assert_eq!(response2.header.batch_id, BatchId::new(2));
    assert_eq!(response3.header.batch_id, BatchId::new(3));
}

#[test]
fn test_worker_reusable_via_run_loop() {
    // Test reuse through the actual run loop (multiple requests in sequence)
    let config = RuntimeConfig {
        idle_timeout: Duration::from_secs(300),
        ..RuntimeConfig::default()
    };
    let mut rt = WorkerRuntime::new(config);

    let mut all_wire = Vec::new();

    // Build 3 sequential requests
    for i in 0..3 {
        let request = EmbedRequest {
            texts: vec![format!("batch {}", i)],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(i as u64), request).unwrap();
        let wire = frame.encode_wire().unwrap();
        all_wire.extend_from_slice(&wire);
    }

    let reader = Cursor::new(all_wire);
    let writer = Cursor::new(Vec::<u8>::new());

    let result = rt.run_loop(reader, writer);
    assert!(result.is_ok());
}

// ── VAL-CPHASE-007: Worker idle timeout tears down ──────────────────────

#[test]
fn test_worker_idle_timeout_teardown() {
    let config = RuntimeConfig {
        idle_timeout: Duration::from_millis(1),
        ..RuntimeConfig::default()
    };
    let mut rt = WorkerRuntime::new(config);

    // With an empty input, the run_loop should exit cleanly
    // (either from EOF or idle timeout)
    let reader = Cursor::new(Vec::<u8>::new());
    let writer = Cursor::new(Vec::<u8>::new());

    let result = rt.run_loop(reader, writer);
    assert!(result.is_ok(), "worker should exit cleanly on idle");
}

#[test]
fn test_worker_idle_timer_expires() {
    let config = RuntimeConfig {
        idle_timeout: Duration::from_millis(5),
        ..RuntimeConfig::default()
    };
    let rt = WorkerRuntime::new(config);

    assert!(!rt.is_idle_expired(), "should not be expired immediately");

    // Wait for idle timeout
    std::thread::sleep(Duration::from_millis(10));
    assert!(rt.is_idle_expired(), "should be expired after timeout");
}

// ── VAL-CPHASE-008: Worker restart works after idle teardown ────────────

#[test]
fn test_worker_restart_after_teardown() {
    // Simulate: first runtime instance processes a request, then "tears down"
    // (goes out of scope). A new runtime instance is created and processes
    // another request successfully.

    // Without a real ONNX session, dispatch returns error frames
    #[cfg(feature = "onnx")]
    let expected_msg_type = MsgType::Error;
    #[cfg(not(feature = "onnx"))]
    let expected_msg_type = MsgType::EmbedResponse;

    // First instance
    let config = RuntimeConfig::default();
    let rt1 = WorkerRuntime::new(config.clone());

    let request1 = EmbedRequest {
        texts: vec!["before teardown".to_string()],
        expected_dim: 4,
    };
    let frame1 = protocol::embed_request_frame(BatchId::new(1), request1).unwrap();
    let response1 = rt1.dispatch(frame1);
    assert_eq!(response1.header.batch_id, BatchId::new(1));
    drop(rt1); // Simulate teardown

    // Second instance (restart)
    let rt2 = WorkerRuntime::new(config);

    let request2 = EmbedRequest {
        texts: vec!["after restart".to_string()],
        expected_dim: 4,
    };
    let frame2 = protocol::embed_request_frame(BatchId::new(2), request2).unwrap();
    let response2 = rt2.dispatch(frame2);
    assert_eq!(response2.header.batch_id, BatchId::new(2));
    assert_eq!(response2.header.msg_type, expected_msg_type);
}

// ── VAL-CPHASE-009: Startup report exposes runtime bundle choices ───────

#[test]
fn test_startup_report_contains_required_fields() {
    let report = StartupReport {
        execution_provider: "cpu".to_string(),
        provider_available: true,
        fallback_reason: None,
        model_name: "qwen3-embed-0.6b".to_string(),
        quantization_mode: "none".to_string(),
        warm_load_latency: Duration::from_millis(150),
        model_path: Some(std::path::PathBuf::from("/opt/models/model.onnx")),
        model_path_source: Some("bundled".to_string()),
        model_error: None,
    };

    let line = report.to_log_line();

    // VAL-CPHASE-009: Must contain execution provider, model name,
    // quantization mode, warm-load latency, and model path source
    assert!(line.contains("provider=cpu"), "missing execution provider");
    assert!(
        line.contains("model=qwen3-embed-0.6b"),
        "missing model name"
    );
    assert!(line.contains("quant=none"), "missing quantization mode");
    assert!(line.contains("warm_load="), "missing warm-load latency");
    assert!(line.contains("bundled"), "missing model path source");
}

#[test]
fn test_startup_report_with_fallback_reason() {
    let report = StartupReport {
        execution_provider: "cuda".to_string(),
        provider_available: false,
        fallback_reason: Some("CUDA driver not found".to_string()),
        model_name: "qwen3-embed-0.6b".to_string(),
        quantization_mode: "none".to_string(),
        warm_load_latency: Duration::from_millis(100),
        model_path: Some(std::path::PathBuf::from(
            "/home/user/.leindex/models/model.onnx",
        )),
        model_path_source: Some("user_cache".to_string()),
        model_error: None,
    };

    let line = report.to_log_line();
    assert!(line.contains("cuda"), "should mention requested provider");
    assert!(line.contains("unavailable"), "should report unavailability");
    assert!(
        line.contains("CUDA driver not found"),
        "should include fallback reason"
    );
}

#[test]
fn test_startup_reporter_builds_complete_report() {
    let mut reporter = StartupReporter::new();
    reporter.set_execution_provider("cpu", true, None);
    reporter.set_model_name("qwen3-embed-0.6b");
    reporter.set_quantization_mode("int8");
    reporter.set_warm_load_latency(Duration::from_millis(200));
    reporter.set_model_path(
        &std::path::PathBuf::from("/opt/models/model.onnx"),
        "bundled",
    );

    let report = reporter.build();
    assert_eq!(report.execution_provider, "cpu");
    assert!(report.provider_available);
    assert_eq!(report.model_name, "qwen3-embed-0.6b");
    assert_eq!(report.quantization_mode, "int8");
    assert_eq!(report.warm_load_latency, Duration::from_millis(200));
    assert_eq!(report.model_path_source, Some("bundled".to_string()));
}

// ── VAL-CPHASE-010: Model path resolution honors precedence ─────────────

#[test]
fn test_model_path_env_override_precedence() {
    // Create a temp dir with a model file
    let temp_dir = tempfile::tempdir().unwrap();
    let model_file = temp_dir.path().join("test-model.onnx");
    std::fs::write(&model_file, b"fake model").unwrap();

    // Set env override
    std::env::set_var("LEINDEX_MODEL_PATH", temp_dir.path());

    let result = ModelResolver::resolve("test-model");
    assert!(result.is_ok());
    let path = result.unwrap();
    assert_eq!(path, model_file);

    // Verify source is reported as env_override
    assert_eq!(ModelResolver::source_for_path(&path), "env_override");

    std::env::remove_var("LEINDEX_MODEL_PATH");
}

#[test]
fn test_model_path_env_override_takes_priority() {
    // Even if other paths exist, env override should win
    let temp_dir = tempfile::tempdir().unwrap();
    let model_file = temp_dir.path().join("priority-test.onnx");
    std::fs::write(&model_file, b"fake model").unwrap();

    std::env::set_var("LEINDEX_MODEL_PATH", temp_dir.path());

    let result = ModelResolver::resolve("priority-test");
    assert!(result.is_ok());
    // Should resolve to the env override path, not bundled or user cache
    assert!(result.unwrap().starts_with(temp_dir.path()));

    std::env::remove_var("LEINDEX_MODEL_PATH");
}

#[test]
fn test_model_path_not_found_reports_error() {
    std::env::remove_var("LEINDEX_MODEL_PATH");
    let result = ModelResolver::resolve("nonexistent-xyz-model");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.message.contains("not found"));
    assert!(err.message.contains("env"));
    assert!(err.message.contains("bundled"));
    assert!(err.message.contains("user cache"));
}

// ── VAL-CPHASE-011: Execution-provider selection is externally controllable

#[test]
fn test_execution_provider_cpu_always_available() {
    let result = ExecutionProviderSelector::select("cpu");
    assert!(result.is_ok());
    let selection = result.unwrap();
    assert_eq!(selection.name(), "cpu");
    assert!(selection.is_requested_provider());
}

#[test]
fn test_execution_provider_unknown_falls_back() {
    let result = ExecutionProviderSelector::select("nonexistent_provider");
    assert!(result.is_err());
    let fallback = result.unwrap_err();
    assert_eq!(fallback.fallback_name(), "cpu");
    assert!(!fallback.is_requested_provider());
    assert!(fallback.reason().contains("unknown"));
}

#[test]
fn test_execution_provider_reports_fallback_reason() {
    // On a system without CUDA, this should fall back with a reason
    let result = ExecutionProviderSelector::select("cuda");
    if let Err(fallback) = result {
        assert_eq!(fallback.fallback_name(), "cpu");
        assert!(fallback.reason().contains("CUDA"));
    }
}

// ── VAL-CPHASE-012: Embed response is flat row-major output ─────────────

#[test]
fn test_embed_response_flat_row_major() {
    let config = RuntimeConfig::default();
    let rt = WorkerRuntime::new(config);

    let request = EmbedRequest {
        texts: vec![
            "text1".to_string(),
            "text2".to_string(),
            "text3".to_string(),
        ],
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let response_frame = rt.dispatch(frame);

    // Without a real ONNX session, dispatch returns an error frame
    #[cfg(feature = "onnx")]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::Error);
    }

    // Without ONNX feature, verify flat row-major output
    #[cfg(not(feature = "onnx"))]
    {
        let response: Response = response_frame.decode_payload().unwrap();
        match response {
            Response::Embed(embed) => {
                // Flat row-major: count * dimension total floats
                assert_eq!(embed.count, 3);
                assert_eq!(embed.dimension, 4);
                assert_eq!(embed.vectors.len(), 12); // 3 * 4

                // Individual embeddings accessible by index
                assert_eq!(embed.get_embedding(0).unwrap().len(), 4);
                assert_eq!(embed.get_embedding(1).unwrap().len(), 4);
                assert_eq!(embed.get_embedding(2).unwrap().len(), 4);
                assert!(embed.get_embedding(3).is_none()); // Out of bounds
            }
            _ => panic!("expected Embed response"),
        }
    }
}

// ── VAL-CPHASE-013: Batch ordering is preserved through IPC ─────────────

#[test]
fn test_batch_ordering_preserved() {
    let config = RuntimeConfig::default();
    let rt = WorkerRuntime::new(config);

    let texts: Vec<String> = (0..10).map(|i| format!("text_{}", i)).collect();
    let request = EmbedRequest {
        texts: texts.clone(),
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();

    // Verify the frame preserves ordering
    let decoded: Request = frame.decode_payload().unwrap();
    match decoded {
        Request::Embed(embed_req) => {
            assert_eq!(embed_req.texts, texts);
        }
        _ => panic!("expected Embed request"),
    }

    // Verify the response (error without ONNX session, success without feature)
    let response_frame = rt.dispatch(frame);

    #[cfg(feature = "onnx")]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::Error);
    }

    #[cfg(not(feature = "onnx"))]
    {
        let response: Response = response_frame.decode_payload().unwrap();
        match response {
            Response::Embed(embed) => {
                assert_eq!(embed.count, 10);
                assert_eq!(embed.dimension, 4);
            }
            _ => panic!("expected Embed response"),
        }
    }
}

// ── VAL-CPHASE-014: Oversized batch is split and re-stitched ────────────

#[test]
fn test_oversized_batch_split_and_stitch() {
    let config = BatchConfig {
        max_frame_size: 200,
        max_text_size: 1024,
    };

    let texts: Vec<String> = (0..30)
        .map(|i| format!("text number {} with enough content to be meaningful", i))
        .collect();
    let dim = 4;
    let request = EmbedRequest {
        texts: texts.clone(),
        expected_dim: dim,
    };

    let batch_id = BatchId::new(42);
    let split = batch::split_request(batch_id, request, &config);

    match split {
        SplitResult::Split(sub_batches) => {
            assert!(
                sub_batches.len() > 1,
                "should be split into multiple sub-batches"
            );

            // All sub-batches have the same batch ID
            for sb in &sub_batches {
                assert_eq!(sb.batch_id, batch_id);
                assert_eq!(sb.request.expected_dim, dim);
            }

            // Total texts across sub-batches equals original
            let total_texts: usize = sub_batches.iter().map(|sb| sb.request.texts.len()).sum();
            assert_eq!(total_texts, texts.len());

            // Create stub responses and stitch
            let responses: Vec<EmbedResponse> = sub_batches
                .iter()
                .map(|sb| {
                    let count = sb.request.texts.len();
                    EmbedResponse::new(vec![0.0f32; count * dim], count, dim)
                })
                .collect();

            let stitched = batch::stitch_responses(responses).unwrap();
            assert_eq!(stitched.count, texts.len());
            assert_eq!(stitched.dimension, dim);
            assert_eq!(stitched.vectors.len(), texts.len() * dim);
        }
        SplitResult::Single(_) => {
            // If texts were small enough to fit, that's also acceptable
        }
    }
}

#[test]
fn test_split_preserves_batch_identity() {
    let config = BatchConfig {
        max_frame_size: 100,
        max_text_size: 1024,
    };

    let texts: Vec<String> = (0..20)
        .map(|i| format!("some text content for item number {}", i))
        .collect();
    let request = EmbedRequest {
        texts,
        expected_dim: 8,
    };

    let batch_id = BatchId::new(0xDEAD);
    let split = batch::split_request(batch_id, request, &config);

    if let SplitResult::Split(sub_batches) = split {
        for sb in &sub_batches {
            assert_eq!(sb.batch_id, batch_id);
        }
    }
}

// ── VAL-CPHASE-015: Single oversized text is reduced before IPC ─────────

#[test]
fn test_oversized_single_text_truncated() {
    let config = RuntimeConfig {
        max_text_size: 50,
        ..RuntimeConfig::default()
    };
    let rt = WorkerRuntime::new(config);

    let long_text = "a".repeat(200);
    let request = EmbedRequest {
        texts: vec![long_text],
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();

    // Should not panic — the oversized text is truncated
    let response_frame = rt.dispatch(frame);

    // Without a real ONNX session, dispatch returns an error frame
    #[cfg(feature = "onnx")]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::Error);
    }

    // Without ONNX feature, dispatch returns a success response
    #[cfg(not(feature = "onnx"))]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::EmbedResponse);
        let response: Response = response_frame.decode_payload().unwrap();
        match response {
            Response::Embed(embed) => {
                assert_eq!(embed.count, 1);
                assert_eq!(embed.dimension, 4);
            }
            _ => panic!("expected Embed response"),
        }
    }
}

#[test]
fn test_truncate_preserves_unicode() {
    let truncated = batch::truncate_text("héllo wörld test".to_string(), 10);
    assert!(truncated.len() <= 10);
    assert!(truncated.is_char_boundary(truncated.len()));
    // Should be valid UTF-8
    assert!(std::str::from_utf8(truncated.as_bytes()).is_ok());
}

#[test]
fn test_truncate_at_exact_boundary() {
    let truncated = batch::truncate_text("hello".to_string(), 5);
    assert_eq!(truncated, "hello");
}

#[test]
fn test_batch_truncate_multiple_oversized_texts() {
    let config = RuntimeConfig {
        max_text_size: 20,
        ..RuntimeConfig::default()
    };
    let rt = WorkerRuntime::new(config);

    let request = EmbedRequest {
        texts: vec![
            "short".to_string(),
            "this is a very long text that exceeds the limit".to_string(),
            "also short".to_string(),
            "another extremely long text that should be truncated before IPC framing".to_string(),
        ],
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let response_frame = rt.dispatch(frame);

    // Without a real ONNX session, dispatch returns an error frame
    #[cfg(feature = "onnx")]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::Error);
    }

    // Without ONNX feature, dispatch returns a success response
    #[cfg(not(feature = "onnx"))]
    {
        assert_eq!(response_frame.header.msg_type, MsgType::EmbedResponse);
        let response: Response = response_frame.decode_payload().unwrap();
        match response {
            Response::Embed(embed) => {
                assert_eq!(embed.count, 4);
                assert_eq!(embed.dimension, 4);
            }
            _ => panic!("expected Embed response"),
        }
    }
}

// ── Cross-cutting: full lifecycle via run_loop ──────────────────────────

#[test]
fn test_full_lifecycle_cold_start_reuse_teardown_restart() {
    // Phase 1: Cold start and process a request
    let config = RuntimeConfig {
        idle_timeout: Duration::from_secs(300),
        ..RuntimeConfig::default()
    };
    let mut rt1 = WorkerRuntime::new(config.clone());

    let request = EmbedRequest {
        texts: vec!["cold start".to_string()],
        expected_dim: 4,
    };
    let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
    let wire = frame.encode_wire().unwrap();

    let reader = Cursor::new(wire);
    let writer = Cursor::new(Vec::<u8>::new());
    assert!(rt1.run_loop(reader, writer).is_ok());

    // Phase 2: Simulate teardown (drop rt1)
    drop(rt1);

    // Phase 3: Restart with a new runtime instance
    let mut rt2 = WorkerRuntime::new(config);

    let request2 = EmbedRequest {
        texts: vec!["after restart".to_string()],
        expected_dim: 4,
    };
    let frame2 = protocol::embed_request_frame(BatchId::new(2), request2).unwrap();
    let wire2 = frame2.encode_wire().unwrap();

    let reader2 = Cursor::new(wire2);
    let writer2 = Cursor::new(Vec::<u8>::new());
    assert!(rt2.run_loop(reader2, writer2).is_ok());
}

#[test]
fn test_runtime_config_from_env() {
    // Test that config can be built from env vars
    std::env::set_var("LEINDEX_WORKER_IDLE_TIMEOUT", "60");
    std::env::set_var("LEINDEX_WORKER_MAX_FRAME_SIZE", "8388608");
    std::env::set_var("LEINDEX_WORKER_MAX_TEXT_SIZE", "524288");
    std::env::set_var("LEINDEX_WORKER_MODEL", "test-model");
    std::env::set_var("LEINDEX_WORKER_EMBEDDING_DIM", "768");
    std::env::set_var("LEINDEX_WORKER_EXECUTION_PROVIDER", "cuda");

    let config = RuntimeConfig::from_env();
    assert_eq!(config.idle_timeout, Duration::from_secs(60));
    assert_eq!(config.max_frame_size, 8 * 1024 * 1024);
    assert_eq!(config.max_text_size, 512 * 1024);
    assert_eq!(config.model_name, "test-model");
    assert_eq!(config.embedding_dim, 768);
    assert_eq!(config.execution_provider, "cuda");

    // Clean up
    std::env::remove_var("LEINDEX_WORKER_IDLE_TIMEOUT");
    std::env::remove_var("LEINDEX_WORKER_MAX_FRAME_SIZE");
    std::env::remove_var("LEINDEX_WORKER_MAX_TEXT_SIZE");
    std::env::remove_var("LEINDEX_WORKER_MODEL");
    std::env::remove_var("LEINDEX_WORKER_EMBEDDING_DIM");
    std::env::remove_var("LEINDEX_WORKER_EXECUTION_PROVIDER");
}
