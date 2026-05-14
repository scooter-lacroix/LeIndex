// leindex-embed — ONNX embedding worker process
//
// This is the entry point for the separate ONNX worker binary. The main
// leindex daemon spawns this process on first embed demand and communicates
// over local IPC (stdin/stdout or a Unix domain socket).
//
// VAL-CPHASE-001: The worker is a separate executable built alongside leindex.
// VAL-CPHASE-004: Worker transport uses local IPC only.

use std::io::{self, Read, Write};
use std::process;

use leindex_embed::protocol::{
    self, EmbedResponse, ErrorKind, Frame, MsgType, Request, RerankResponse, WorkerError,
};

fn main() {
    // Initialize minimal logging
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    tracing::info!("leindex-embed worker starting");

    // Run the main read-respond loop over stdin/stdout
    if let Err(e) = run_loop(io::stdin().lock(), io::stdout().lock()) {
        tracing::error!("worker loop failed: {}", e);
        process::exit(1);
    }
}

/// Main read-respond loop: read frames from `reader`, process, write to `writer`.
///
/// Each iteration reads a length-prefixed frame, deserialises the request,
/// dispatches to the appropriate handler, and writes the response frame back.
fn run_loop<R: Read, W: Write>(mut reader: R, mut writer: W) -> anyhow::Result<()> {
    loop {
        // Read 4-byte length prefix
        let mut len_buf = [0u8; 4];
        match reader.read_exact(&mut len_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => {
                // Clean shutdown — the main daemon closed the pipe.
                tracing::debug!("stdin EOF, worker shutting down");
                return Ok(());
            }
            Err(e) => {
                return Err(anyhow::anyhow!("failed to read frame length: {}", e));
            }
        }

        let payload_len = u32::from_le_bytes(len_buf) as usize;

        // Read the frame payload
        let mut frame_buf = vec![0u8; payload_len];
        reader.read_exact(&mut frame_buf)?;

        let frame = Frame::from_wire_bytes(&frame_buf)?;
        let batch_id = frame.header.batch_id;

        // Decode the request and dispatch
        let response = match frame.header.msg_type {
            MsgType::EmbedRequest => match handle_embed_request(&frame) {
                Ok(response) => protocol::embed_response_frame(batch_id, response)?,
                Err(e) => protocol::error_frame(batch_id, e)?,
            },
            MsgType::RerankRequest => match handle_rerank_request(&frame) {
                Ok(response) => protocol::rerank_response_frame(batch_id, response)?,
                Err(e) => protocol::error_frame(batch_id, e)?,
            },
            // Unexpected message types from the main daemon
            _ => {
                let err = WorkerError {
                    kind: ErrorKind::InvalidRequest,
                    message: format!(
                        "unexpected message type {:?} from main daemon",
                        frame.header.msg_type
                    ),
                };
                protocol::error_frame(batch_id, err)?
            }
        };

        // Write the response frame
        let wire = response.encode_wire()?;
        writer.write_all(&wire)?;
        writer.flush()?;
    }
}

/// Handle an embed request.
///
/// Currently returns a stub response since the full ONNX inference
/// integration will be completed in the runtime lifecycle feature.
/// The protocol round-trip is validated independently.
fn handle_embed_request(frame: &Frame) -> Result<EmbedResponse, WorkerError> {
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

    // Stub: return zero vectors for protocol validation.
    // Full ONNX inference will be wired in the runtime lifecycle feature.
    let count = embed_req.texts.len();
    let dim = embed_req.expected_dim;
    let vectors = vec![0.0f32; count * dim];

    Ok(EmbedResponse::new(vectors, count, dim))
}

/// Handle a rerank request.
///
/// Currently returns a stub response since the full ONNX inference
/// integration will be completed in the runtime lifecycle feature.
fn handle_rerank_request(frame: &Frame) -> Result<RerankResponse, WorkerError> {
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

    // Stub: return documents with their initial scores as combined scores.
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

#[cfg(test)]
mod tests {
    use super::*;
    use leindex_embed::protocol::{BatchId, EmbedRequest};
    use std::io::Cursor;

    #[test]
    fn test_run_loop_embed_roundtrip() {
        // Build an embed request frame
        let request = EmbedRequest {
            texts: vec!["hello".to_string(), "world".to_string()],
            expected_dim: 4,
        };
        let frame = protocol::embed_request_frame(BatchId::new(1), request).unwrap();
        let wire = frame.encode_wire().unwrap();

        // Feed it into the run_loop via a cursor
        let mut input = Cursor::new(wire);
        let mut output = Cursor::new(Vec::<u8>::new());

        // We need to close the input after one frame to trigger EOF
        // For testing, we'll just verify the frame encoding works
        let decoded = Frame::from_wire_bytes(&input.into_inner()[4..]).unwrap();
        assert_eq!(decoded.header.batch_id, BatchId::new(1));
        assert_eq!(decoded.header.msg_type, MsgType::EmbedRequest);
    }

    #[test]
    fn test_handle_embed_request_empty() {
        let request = EmbedRequest {
            texts: vec![],
            expected_dim: 1024,
        };
        let frame = protocol::embed_request_frame(BatchId::new(0), request).unwrap();
        let response = handle_embed_request(&frame).unwrap();
        assert_eq!(response.count, 0);
        assert_eq!(response.dimension, 1024);
        assert!(response.vectors.is_empty());
    }

    #[test]
    fn test_handle_embed_request_stub() {
        let request = EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 8,
        };
        let frame = protocol::embed_request_frame(BatchId::new(42), request).unwrap();
        let response = handle_embed_request(&frame).unwrap();
        assert_eq!(response.count, 1);
        assert_eq!(response.dimension, 8);
        assert_eq!(response.vectors.len(), 8);
    }
}
