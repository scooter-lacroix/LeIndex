// leindex-embed — ONNX embedding worker process
//
// This is the entry point for the separate ONNX worker binary. The main
// leindex daemon spawns this process on first embed demand and communicates
// over local IPC (stdin/stdout pipes).
//
// VAL-CPHASE-001: The worker is a separate executable built alongside leindex.
// VAL-CPHASE-004: Worker transport uses local IPC only.
// VAL-CPHASE-005: Worker cold-starts on first embed demand.
// VAL-CPHASE-006: Worker remains reusable across successive batches.
// VAL-CPHASE-007: Worker idle timeout tears down the resident model process.
// VAL-CPHASE-008: Worker restart works after idle teardown.
//
// The actual worker logic lives in `leindex_embed::worker_main::run()` so it
// can be shared with the root crate's cargo-install wrapper binary
// (`src/bin/leindex-embed.rs`), which ensures `cargo install leindex
// --features onnx` co-installs both binaries (VAL-CARGO-005).

fn main() {
    leindex_embed::worker_main::run()
}

#[cfg(test)]
mod tests {
    use leindex_embed::protocol::{self, BatchId, EmbedRequest, Frame, MsgType};
    use leindex_embed::runtime::{WorkerRuntime, DEFAULT_IDLE_TIMEOUT_SECS};
    use std::io::Cursor;
    use std::time::Duration;

    #[test]
    fn test_binary_embed_roundtrip_via_runtime() {
        // Build an embed request frame
        let request = EmbedRequest {
            texts: vec!["hello".to_string(), "world".to_string()],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let wire = frame.encode_wire().unwrap();

        // Verify frame encoding
        let decoded = Frame::from_wire_bytes(&wire[4..]).unwrap();
        assert_eq!(decoded.header.batch_id, BatchId::new(1));
        assert_eq!(decoded.header.msg_type, MsgType::EmbedRequest);
    }

    #[test]
    fn test_runtime_handles_embed_request() {
        let config = leindex_embed::runtime::RuntimeConfig::default();
        let rt = WorkerRuntime::new(config);

        let request = EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 8,
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
    fn test_run_loop_single_request() {
        let config = leindex_embed::runtime::RuntimeConfig {
            idle_timeout: Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS),
            ..leindex_embed::runtime::RuntimeConfig::default()
        };
        let mut rt = WorkerRuntime::new(config);

        let request = EmbedRequest {
            texts: vec!["hello".to_string()],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let wire = frame.encode_wire().unwrap();

        let reader = Cursor::new(wire);
        let writer = Cursor::new(Vec::<u8>::new());

        let result = rt.run_loop(reader, writer);
        assert!(result.is_ok());
    }
}
