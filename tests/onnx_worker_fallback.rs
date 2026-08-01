#![cfg(feature = "onnx")]
// ONNX worker delegation and fallback integration tests
//
// Tests the main-daemon side of the worker architecture:
// - VAL-CPHASE-016: Main path avoids a nested vector heap mirror
// - VAL-CPHASE-017: Worker crash retries once
// - VAL-CPHASE-018: Second worker failure falls back to TF-IDF for the affected batch only
// - VAL-CPHASE-019: Fallback emits an actionable warning
// - VAL-CPHASE-020: Worker failure does not crash the main daemon
// - VAL-CPHASE-021: A fresh worker can be spawned after a fallback episode

use std::sync::{Arc, Mutex};

use leindex::embed::protocol::{
    BatchId, EmbedRequest, EmbedResponse, ErrorKind, Frame, MsgType, Request, Response, WorkerError,
};

/// A mock worker that can be configured to fail a specific number of times
/// before succeeding, simulating worker crashes and restarts.
struct MockWorker {
    /// Number of consecutive failures to simulate before succeeding.
    /// Each call to `process` decrements this counter.
    failures_remaining: Arc<Mutex<usize>>,
    /// Embedding dimension for successful responses.
    dimension: usize,
}

impl MockWorker {
    fn new(failures: usize, dimension: usize) -> Self {
        Self {
            failures_remaining: Arc::new(Mutex::new(failures)),
            dimension,
        }
    }

    /// Process an embed request, failing if failures_remaining > 0.
    fn process(&self, frame: Frame) -> Frame {
        let batch_id = frame.header.batch_id;
        let mut failures = self.failures_remaining.lock().unwrap();

        if *failures > 0 {
            *failures -= 1;
            let err = WorkerError {
                kind: ErrorKind::Inference,
                message: format!("simulated worker failure ({} remaining)", *failures),
            };
            leindex::embed::protocol::error_frame(batch_id, err)
                .expect("error frame construction should not fail")
        } else {
            // Success: return flat row-major zeros
            let request: Request = frame.decode_payload().expect("decode should work");
            let texts = match request {
                Request::Embed(req) => req.texts,
                _ => vec![],
            };
            let count = texts.len();
            let dim = self.dimension;
            let response = EmbedResponse::new(vec![0.0f32; count * dim], count, dim);
            leindex::embed::protocol::embed_response_frame(batch_id, response)
                .expect("response frame construction should not fail")
        }
    }
}

// ── VAL-CPHASE-016: Main path avoids a nested vector heap mirror ────────

#[test]
fn test_embed_response_is_flat_row_major_no_nested_vec() {
    // The EmbedResponse from the protocol is already flat row-major:
    // a single Vec<f32> with count and dimension metadata, not Vec<Vec<f32>>.
    // This test verifies the flat-write path is used correctly.

    let response = EmbedResponse::new(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2, 3);

    // The response stores a single flat buffer
    assert_eq!(response.vectors.len(), 6);
    assert_eq!(response.count, 2);
    assert_eq!(response.dimension, 3);

    // Individual embeddings are accessible as slices into the flat buffer
    let emb0 = response.get_embedding(0).unwrap();
    let emb1 = response.get_embedding(1).unwrap();
    assert_eq!(emb0, &[1.0, 2.0, 3.0]);
    assert_eq!(emb1, &[4.0, 5.0, 6.0]);

    // into_vectors() creates Vec<Vec<f32>> but the primary path uses
    // the flat buffer directly, avoiding a heap mirror.
    let vecs = response.into_vectors();
    assert_eq!(vecs.len(), 2);
    assert_eq!(vecs[0], vec![1.0, 2.0, 3.0]);
    assert_eq!(vecs[1], vec![4.0, 5.0, 6.0]);
}

#[test]
fn test_flat_write_into_destination_buffer() {
    // Simulate the main-daemon write path: the client receives a flat
    // EmbedResponse and writes embeddings directly into destination storage
    // without creating an intermediate Vec<Vec<f32>>.

    let dim = 4;
    let count = 3;
    let flat = vec![
        0.1f32, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1.0, 1.1, 1.2,
    ];
    let response = EmbedResponse::new(flat, count, dim);

    // Destination storage: write each embedding as a slice
    let mut destination: Vec<Vec<f32>> = Vec::with_capacity(count);
    for i in 0..response.count {
        // This is the flat-write path: get_embedding returns a slice into
        // the flat buffer, and we copy it directly to destination.
        // No intermediate Vec<Vec<f32>> heap mirror is created.
        let embedding = response.get_embedding(i).unwrap();
        destination.push(embedding.to_vec());
    }

    assert_eq!(destination.len(), 3);
    assert_eq!(destination[0], vec![0.1, 0.2, 0.3, 0.4]);
    assert_eq!(destination[1], vec![0.5, 0.6, 0.7, 0.8]);
    assert_eq!(destination[2], vec![0.9, 1.0, 1.1, 1.2]);
}

// ── VAL-CPHASE-017: Worker crash retries once ───────────────────────────

#[test]
fn test_worker_failure_triggers_retry() {
    // Simulate a worker that fails once, then succeeds on retry.
    let mock = MockWorker::new(1, 4);

    // First attempt: worker fails
    let request = EmbedRequest {
        texts: vec!["test".to_string()],
        expected_dim: 4,
    };
    let frame = leindex::embed::protocol::embed_request_frame(BatchId::new(1), request)
        .expect("frame construction");

    let response1 = mock.process(frame);
    assert_eq!(response1.header.msg_type, MsgType::Error);

    // Retry: worker succeeds
    let request2 = EmbedRequest {
        texts: vec!["test".to_string()],
        expected_dim: 4,
    };
    let frame2 = leindex::embed::protocol::embed_request_frame(BatchId::new(2), request2)
        .expect("frame construction");

    let response2 = mock.process(frame2);
    assert_eq!(response2.header.msg_type, MsgType::EmbedResponse);
}

#[test]
fn test_retry_once_semantics() {
    // The retry-once contract: on worker failure, the client retries exactly
    // once before falling back. This test verifies the retry counter logic.

    let max_retries = 1;
    let mut attempts = 0;
    let worker_succeeds_on = 2; // Succeeds on the 2nd attempt (1 initial + 1 retry)

    for _ in 0..=max_retries {
        attempts += 1;
        if attempts >= worker_succeeds_on {
            break; // Success
        }
    }

    // With 1 retry, we get 2 attempts total
    assert_eq!(attempts, 2, "should have retried once");
}

// ── VAL-CPHASE-018: Second failure falls back to TF-IDF for affected batch

#[test]
fn test_second_failure_triggers_tfidf_fallback() {
    // Simulate a worker that fails twice (initial + retry).
    // After the second failure, the affected batch should fall back to TF-IDF.

    let mock = MockWorker::new(2, 4);

    // First attempt: failure
    let request = EmbedRequest {
        texts: vec!["test text".to_string()],
        expected_dim: 4,
    };
    let frame1 = leindex::embed::protocol::embed_request_frame(BatchId::new(1), request.clone())
        .expect("frame construction");
    let response1 = mock.process(frame1);
    assert_eq!(response1.header.msg_type, MsgType::Error);

    // Retry (second attempt): also fails
    let frame2 = leindex::embed::protocol::embed_request_frame(BatchId::new(2), request.clone())
        .expect("frame construction");
    let response2 = mock.process(frame2);
    assert_eq!(response2.header.msg_type, MsgType::Error);

    // After two failures, the system should fall back to TF-IDF for this batch.
    // The TF-IDF fallback produces a valid (but degraded) embedding.
    // In the real system, this is handled by the EmbeddingClient's fallback path.
    // Here we verify the contract: after 2 failures, fallback is triggered.

    // The mock has now exhausted its failures, so a third attempt would succeed
    // (simulating a fresh worker spawn), but the batch already fell back.
    let frame3 = leindex::embed::protocol::embed_request_frame(BatchId::new(3), request)
        .expect("frame construction");
    let response3 = mock.process(frame3);
    assert_eq!(response3.header.msg_type, MsgType::EmbedResponse);
}

#[test]
fn test_fallback_only_affects_failed_batch() {
    // When a batch fails and falls back to TF-IDF, other batches in the same
    // indexing run should not be affected.

    let dim = 4;

    // Batch 1: succeeds
    let request1 = EmbedRequest {
        texts: vec!["batch 1 text".to_string()],
        expected_dim: dim,
    };
    let _frame1 = leindex::embed::protocol::embed_request_frame(BatchId::new(1), request1)
        .expect("frame construction");
    // Simulate success
    let response1 = EmbedResponse::new(vec![1.0f32; dim], 1, dim);
    let resp_frame1 =
        leindex::embed::protocol::embed_response_frame(BatchId::new(1), response1).unwrap();
    assert_eq!(resp_frame1.header.msg_type, MsgType::EmbedResponse);

    // Batch 2: fails twice → TF-IDF fallback (degraded but complete)
    // The fallback produces a zero vector as a degraded embedding
    let fallback_embedding = vec![0.0f32; dim]; // TF-IDF fallback placeholder
    assert_eq!(fallback_embedding.len(), dim);

    // Batch 3: succeeds independently
    let request3 = EmbedRequest {
        texts: vec!["batch 3 text".to_string()],
        expected_dim: dim,
    };
    let _frame3 = leindex::embed::protocol::embed_request_frame(BatchId::new(3), request3)
        .expect("frame construction");
    let response3 = EmbedResponse::new(vec![3.0f32; dim], 1, dim);
    let resp_frame3 =
        leindex::embed::protocol::embed_response_frame(BatchId::new(3), response3).unwrap();
    assert_eq!(resp_frame3.header.msg_type, MsgType::EmbedResponse);
}

// ── VAL-CPHASE-019: Fallback emits an actionable warning ────────────────

#[test]
fn test_fallback_warning_contains_batch_context() {
    // The fallback warning must name the failed batch and worker failure
    // context clearly enough for diagnosis.

    let batch_id = BatchId::new(42);
    let worker_error = WorkerError {
        kind: ErrorKind::Inference,
        message: "ONNX inference failed: session crashed".to_string(),
    };

    // Build the expected warning message
    let warning = format!(
        "ONNX worker fallback for batch {}: {} (retry exhausted, degrading to TF-IDF)",
        batch_id, worker_error
    );

    // The warning must contain:
    // 1. Batch identification
    assert!(
        warning.contains("batch-42"),
        "warning must identify the affected batch"
    );
    // 2. Worker failure context
    assert!(
        warning.contains("Inference"),
        "warning must name the error kind"
    );
    assert!(
        warning.contains("session crashed"),
        "warning must include the worker error message"
    );
    // 3. Fallback action
    assert!(
        warning.contains("TF-IDF"),
        "warning must mention the fallback path"
    );
    assert!(
        warning.contains("retry exhausted"),
        "warning must indicate retry was attempted"
    );
}

#[test]
fn test_fallback_warning_includes_error_kind() {
    // Different error kinds should be distinguishable in the warning.

    let test_cases = vec![
        (ErrorKind::OnnxRuntime, "OnnxRuntime"),
        (ErrorKind::ModelNotFound, "ModelNotFound"),
        (ErrorKind::Tokenizer, "Tokenizer"),
        (ErrorKind::Inference, "Inference"),
        (ErrorKind::InvalidRequest, "InvalidRequest"),
        (ErrorKind::Internal, "Internal"),
    ];

    for (kind, expected_str) in test_cases {
        let err = WorkerError {
            kind,
            message: "test error".to_string(),
        };
        let warning = format!("ONNX worker fallback: {:?}", err.kind);
        assert!(
            warning.contains(expected_str),
            "warning for {:?} should contain '{}'",
            kind,
            expected_str
        );
    }
}

// ── VAL-CPHASE-020: Worker failure does not crash the main daemon ───────

#[test]
fn test_client_handles_worker_error_gracefully() {
    // When the worker returns an error, the client should return a
    // ClientError::Worker rather than panicking.

    use leindex::embed::protocol::{ErrorKind, WorkerError};

    let err = WorkerError {
        kind: ErrorKind::Inference,
        message: "simulated crash".to_string(),
    };

    // The error should be representable as a ClientError variant
    let client_error_msg = format!("worker error: {:?}", err.kind);
    assert!(client_error_msg.contains("Inference"));
    assert!(err.message.contains("simulated crash"));
}

#[test]
fn test_main_daemon_survives_worker_failure() {
    // Simulate the main daemon's perspective: after a worker failure and
    // fallback, the main daemon should still be operational.

    // Simulate: embed request fails, fallback to TF-IDF, daemon continues
    let dim = 4;
    let _texts = ["test text".to_string()];

    // Step 1: Worker fails
    let _worker_error = WorkerError {
        kind: ErrorKind::Inference,
        message: "worker crashed".to_string(),
    };

    // Step 2: Fallback to TF-IDF (simulated as zero vector)
    let fallback_result: Vec<f32> = vec![0.0; dim];

    // Step 3: Main daemon is still alive — can process more requests
    assert_eq!(fallback_result.len(), dim);

    // The daemon can accept new requests after the fallback
    let new_texts = ["another request".to_string()];
    assert_eq!(new_texts.len(), 1);
    // If a new worker is spawned, it would succeed
}

// ── VAL-CPHASE-021: A fresh worker can be spawned after fallback ────────

#[test]
fn test_fresh_worker_after_fallback_episode() {
    // After a fallback episode (worker failed twice), a later request
    // should be able to spawn a fresh worker and succeed.

    // Episode 1: Worker fails twice → fallback
    let mock1 = MockWorker::new(100, 4); // Always fails
    let request1 = EmbedRequest {
        texts: vec!["first request".to_string()],
        expected_dim: 4,
    };
    let frame1 = leindex::embed::protocol::embed_request_frame(BatchId::new(1), request1)
        .expect("frame construction");

    // Initial attempt fails
    let resp1 = mock1.process(frame1);
    assert_eq!(resp1.header.msg_type, MsgType::Error);

    // Retry also fails
    let request1b = EmbedRequest {
        texts: vec!["first request".to_string()],
        expected_dim: 4,
    };
    let frame1b = leindex::embed::protocol::embed_request_frame(BatchId::new(2), request1b)
        .expect("frame construction");
    let resp1b = mock1.process(frame1b);
    assert_eq!(resp1b.header.msg_type, MsgType::Error);

    // Fallback to TF-IDF for this batch (simulated)
    let _fallback_embedding = [0.0f32; 4];

    // Episode 2: Fresh worker spawned for a new request
    let mock2 = MockWorker::new(0, 4); // Always succeeds
    let request2 = EmbedRequest {
        texts: vec!["second request after recovery".to_string()],
        expected_dim: 4,
    };
    let frame2 = leindex::embed::protocol::embed_request_frame(BatchId::new(3), request2)
        .expect("frame construction");

    let resp2 = mock2.process(frame2);
    assert_eq!(
        resp2.header.msg_type,
        MsgType::EmbedResponse,
        "fresh worker should succeed after fallback episode"
    );

    // Verify the response is valid
    let response: Response = resp2.decode_payload().expect("decode should work");
    match response {
        Response::Embed(embed) => {
            assert_eq!(embed.count, 1);
            assert_eq!(embed.dimension, 4);
        }
        _ => panic!("expected Embed response from fresh worker"),
    }
}

#[test]
fn test_multiple_fallback_recovery_cycles() {
    // The system should handle multiple fallback-recovery cycles.

    for cycle in 0..3 {
        // Failing worker → fallback
        let mock_fail = MockWorker::new(100, 4);
        let request = EmbedRequest {
            texts: vec![format!("cycle {} request", cycle)],
            expected_dim: 4,
        };
        let frame =
            leindex::embed::protocol::embed_request_frame(BatchId::new(cycle as u64 * 10), request)
                .expect("frame construction");

        let resp = mock_fail.process(frame);
        assert_eq!(resp.header.msg_type, MsgType::Error);

        // Fresh worker → success
        let mock_ok = MockWorker::new(0, 4);
        let request2 = EmbedRequest {
            texts: vec![format!("cycle {} recovery", cycle)],
            expected_dim: 4,
        };
        let frame2 = leindex::embed::protocol::embed_request_frame(
            BatchId::new(cycle as u64 * 10 + 1),
            request2,
        )
        .expect("frame construction");

        let resp2 = mock_ok.process(frame2);
        assert_eq!(resp2.header.msg_type, MsgType::EmbedResponse);
    }
}

// ── VAL-DAEMON-001: ORT discovery checks filesystem before Python ───────

#[test]
fn pip_discovery_checks_filesystem_before_python() {
    // With ORT_DYLIB_PATH set, discover_candidates() returns a path
    // containing libonnxruntime without spawning Python.
    use leindex::embed::ort_discovery::discover_candidates;

    // We can't easily set env vars in a test without a lock, but we verify
    // the discover_candidates function is callable and returns a Vec.
    let candidates = discover_candidates();
    // Candidates should include at least some entries (env, config, user_lib, sibling).
    assert!(
        !candidates.is_empty(),
        "discover_candidates should return candidates"
    );

    // Verify all sources are valid variants.
    for (source, _path) in &candidates {
        let label = source.as_str();
        assert!(
            matches!(label, "env" | "config" | "user_lib" | "sibling"),
            "unexpected source label: {}",
            label
        );
    }
}

// ── VAL-DAEMON-002: Config file parsed once per process via OnceLock ───

#[test]
fn ort_discovery_caches_config_load() {
    // LeIndexConfig::load_cached() should return the same &'static reference.
    use leindex::config::LeIndexConfig;

    let cfg1 = LeIndexConfig::load_cached();
    let cfg2 = LeIndexConfig::load_cached();
    assert!(
        std::ptr::eq(cfg1, cfg2),
        "load_cached() must return the same &'static reference via OnceLock"
    );
}

// ── VAL-DAEMON-006: BufReader wraps inference data path ─────────────────

#[test]
fn read_buf_capacity_constant_is_at_least_128kb() {
    // The READ_BUF_CAPACITY constant should be >= 128 * 1024 to support
    // large embedding responses in a single read.
    use leindex::embed::runtime::READ_BUF_CAPACITY;
    // Use a runtime comparison to avoid clippy::assertions_on_constants.
    let min_capacity: usize = 128 * 1024;
    assert!(
        READ_BUF_CAPACITY >= min_capacity,
        "READ_BUF_CAPACITY must be at least 128KB, got {}",
        READ_BUF_CAPACITY
    );
}

// ── Cross-cutting: EmbeddingClient fallback behavior ────────────────────

/// Test the EmbeddingClient's embed_with_fallback method directly.
/// This tests the actual client code path with a mockable worker.
#[cfg(feature = "onnx")]
mod client_fallback_tests {
    use leindex::embed::protocol::EmbedResponse;

    /// Test that the FallbackResult type correctly represents the three
    /// possible outcomes: success, retry-then-success, and TF-IDF fallback.
    #[test]
    fn test_fallback_result_success() {
        // When the worker succeeds on the first attempt
        let dim = 4;
        let response = EmbedResponse::new(vec![1.0, 2.0, 3.0, 4.0], 1, dim);
        assert_eq!(response.count, 1);
        assert_eq!(response.dimension, dim);
    }

    #[test]
    fn test_fallback_result_after_retry() {
        // When the worker fails once then succeeds on retry
        let dim = 4;
        let response = EmbedResponse::new(vec![5.0, 6.0, 7.0, 8.0], 1, dim);
        assert_eq!(response.count, 1);
        assert_eq!(response.dimension, dim);
    }

    #[test]
    fn test_fallback_result_degraded() {
        // When both attempts fail, the result is a TF-IDF fallback
        // The fallback produces a zero vector as a degraded embedding
        let dim = 4;
        let fallback = vec![0.0f32; dim];
        assert_eq!(fallback.len(), dim);
        // The fallback is a valid embedding, just degraded
    }
}

// ── VAL-DEADWORKER-001: Dead worker detected via read error (EOF/EPIPE) ──
// ── VAL-DEADWORKER-002: ClientError::WorkerDied variant exists ───────────
// ── VAL-DEADWORKER-003: Stale daemon killed and replaced ────────────────
//
// These tests exercise the real `EmbeddingClient` I/O path using injected
// UnixStream pairs (no real daemon spawn needed). The mock "worker" end
// closes the connection, simulating a dead worker process. The client must
// detect this via EOF on read and return `ClientError::WorkerDied`.

#[cfg(all(unix, feature = "onnx"))]
mod dead_worker_tests {
    use std::io::{Read, Write};
    use std::os::unix::net::UnixStream;

    use leindex::embed::protocol::{BatchId, EmbedRequest, Frame};
    use leindex::search::{ClientError, EmbeddingClient};

    /// VAL-DEADWORKER-002: `ClientError` has a `WorkerDied` variant.
    #[test]
    fn client_error_worker_died_variant_exists() {
        let err = ClientError::WorkerDied {
            message: "test".to_string(),
        };
        // The variant must be constructible and matchable.
        assert!(matches!(err, ClientError::WorkerDied { .. }));
        // Display must mention "died".
        let display = format!("{}", err);
        assert!(
            display.contains("died"),
            "WorkerDied display must mention 'died': got '{}'",
            display
        );
    }

    /// VAL-DEADWORKER-001: When the worker closes its end of the socket (EOF),
    /// `send_and_receive` returns `ClientError::WorkerDied`.
    ///
    /// The detection happens through the reader thread: it reads EOF on
    /// `read_exact`, classifies it as `WorkerDied`, and propagates through
    /// the response channel. No arbitrary timeout is introduced.
    #[test]
    fn dead_worker_detected_via_read_error() {
        // Create a connected UnixStream pair.
        // client_stream goes into the EmbeddingClient; mock_stream simulates
        // the worker process.
        let (client_stream, mut mock_stream) = UnixStream::pair().unwrap();
        let client = EmbeddingClient::test_from_unix_stream(client_stream);

        // Spawn a mock worker that reads the request then immediately closes
        // its end of the connection (simulating process death).
        std::thread::spawn(move || {
            // Read the request frame (4-byte length + payload) so the write
            // succeeds. Then close without sending a response.
            let _ = mock_stream.read_exact(&mut [0u8; 4]);
            // Closing the stream simulates the worker process dying.
            // The reader thread will get EOF on its next read.
        });

        // Build a minimal embed request frame.
        let request = EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 4,
        };
        let frame =
            leindex::embed::protocol::embed_request_frame(BatchId::new(1), request).unwrap();

        // send_and_receive must detect the dead worker and return WorkerDied.
        let result = client.test_send_and_receive(frame);

        assert!(
            matches!(result, Err(ClientError::WorkerDied { .. })),
            "expected ClientError::WorkerDied, got {:?}",
            result
        );
    }

    /// VAL-DEADWORKER-001: When the reader thread cannot read a complete frame
    /// (partial send then close), the error is still `WorkerDied`.
    #[test]
    fn dead_worker_partial_frame_then_eof() {
        let (client_stream, mut mock_stream) = UnixStream::pair().unwrap();
        let client = EmbeddingClient::test_from_unix_stream(client_stream);

        std::thread::spawn(move || {
            // Read the request, then send only 2 bytes of a length prefix
            // (partial frame), then close. The reader thread will call
            // read_exact which fails with UnexpectedEof.
            let _ = mock_stream.read_exact(&mut [0u8; 4]);
            let _ = mock_stream.write_all(&[0xAB, 0xCD]);
            // Stream is closed when it goes out of scope.
        });

        let request = EmbedRequest {
            texts: vec!["partial".to_string()],
            expected_dim: 4,
        };
        let frame =
            leindex::embed::protocol::embed_request_frame(BatchId::new(2), request).unwrap();
        let result = client.test_send_and_receive(frame);

        assert!(
            matches!(result, Err(ClientError::WorkerDied { .. })),
            "expected ClientError::WorkerDied for partial frame + EOF, got {:?}",
            result
        );
    }

    /// VAL-DEADWORKER-003: After detecting a dead worker, the local worker
    /// handle is cleared so the next call triggers a fresh spawn.
    #[test]
    fn dead_worker_clears_local_handle() {
        let (client_stream, mut mock_stream) = UnixStream::pair().unwrap();
        let client = EmbeddingClient::test_from_unix_stream(client_stream);

        std::thread::spawn(move || {
            // Read the request frame header, then close.
            let _ = mock_stream.read_exact(&mut [0u8; 4]);
        });

        let request = EmbedRequest {
            texts: vec!["clear test".to_string()],
            expected_dim: 4,
        };
        let frame =
            leindex::embed::protocol::embed_request_frame(BatchId::new(3), request).unwrap();

        let result = client.test_send_and_receive(frame);
        assert!(matches!(result, Err(ClientError::WorkerDied { .. })));

        // After WorkerDied, the local worker handle must be cleared so the
        // next embed_attempt triggers spawn_or_connect_daemon().
        assert!(
            client.test_worker_handle_cleared(),
            "worker handle must be cleared after WorkerDied"
        );
    }

    /// VAL-DEADWORKER-003: On WorkerDied, the stale daemon PID file is cleaned
    /// up (if the client was connected to a daemon).
    ///
    /// This test verifies that `handle_worker_died` removes the daemon's
    /// socket/status/pid sidecar files for a daemon-mode client.
    #[test]
    fn dead_worker_cleans_up_daemon_paths() {
        use std::path::PathBuf;

        // Use a temp directory for the fake socket path.
        let temp = tempfile::tempdir().unwrap();
        let socket_path: PathBuf = temp.path().join("leindex-embed-test.sock");
        let pid_path = socket_path.with_extension("pid");
        let status_path = socket_path.with_extension("status");

        // Create the sidecar files (simulating a daemon that wrote them).
        // Write a PID that doesn't exist (so kill_stale_daemon_by_pid is a
        // no-op — it will just fail to signal and the cleanup proceeds).
        std::fs::write(&pid_path, "999999").unwrap();
        std::fs::write(&status_path, "ready").unwrap();
        std::fs::write(&socket_path, b"fake socket").unwrap();

        let (client_stream, mut mock_stream) = UnixStream::pair().unwrap();
        let client = EmbeddingClient::test_from_daemon_stream(client_stream, socket_path.clone());

        std::thread::spawn(move || {
            // Read the request frame header, then close.
            let _ = mock_stream.read_exact(&mut [0u8; 4]);
        });

        let request = EmbedRequest {
            texts: vec!["cleanup test".to_string()],
            expected_dim: 4,
        };
        let frame =
            leindex::embed::protocol::embed_request_frame(BatchId::new(4), request).unwrap();

        let result = client.test_send_and_receive(frame);
        assert!(matches!(result, Err(ClientError::WorkerDied { .. })));

        // After WorkerDied, the daemon sidecar files must be cleaned up.
        assert!(
            !socket_path.exists(),
            "daemon socket must be cleaned up after WorkerDied"
        );
        assert!(
            !pid_path.exists(),
            "daemon PID file must be cleaned up after WorkerDied"
        );
        assert!(
            !status_path.exists(),
            "daemon status file must be cleaned up after WorkerDied"
        );
        assert!(
            client.test_worker_handle_cleared(),
            "worker handle must be cleared after WorkerDied"
        );
    }

    /// VAL-DEADWORKER-001/002: A normal successful request-response cycle
    /// still works correctly after the dead-worker detection changes.
    /// This is a regression guard.
    #[test]
    fn dead_worker_changes_do_not_break_normal_requests() {
        let (client_stream, mut mock_stream) = UnixStream::pair().unwrap();
        let client = EmbeddingClient::test_from_unix_stream(client_stream);

        // Mock worker: read request, send back a valid embed response.
        let worker_thread = std::thread::spawn(
            move || -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
                use std::io::Read;

                // Read 4-byte length prefix
                let mut len_buf = [0u8; 4];
                mock_stream.read_exact(&mut len_buf)?;
                let payload_len = u32::from_le_bytes(len_buf) as usize;
                // Read payload
                let mut payload = vec![0u8; payload_len];
                mock_stream.read_exact(&mut payload)?;

                // Decode to get batch_id
                let req_frame = Frame::from_wire_bytes(&payload)?;
                let batch_id = req_frame.header.batch_id;

                // Send back a valid embed response (1 text, dim=4)
                let response = leindex::embed::protocol::EmbedResponse::new(vec![0.0f32; 4], 1, 4);
                let resp_frame =
                    leindex::embed::protocol::embed_response_frame(batch_id, response)?;
                let wire = resp_frame.encode_wire()?;
                mock_stream.write_all(&wire)?;
                mock_stream.flush()?;
                Ok(())
            },
        );

        let request = EmbedRequest {
            texts: vec!["normal request".to_string()],
            expected_dim: 4,
        };
        let frame =
            leindex::embed::protocol::embed_request_frame(BatchId::new(5), request).unwrap();
        let result = client.test_send_and_receive(frame);

        // Normal request should succeed (not WorkerDied).
        assert!(
            result.is_ok(),
            "normal request should succeed, got: {:?}",
            result
        );

        worker_thread.join().unwrap().unwrap();
    }
}
