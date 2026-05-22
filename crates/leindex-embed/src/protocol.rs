// IPC Protocol for leindex ↔ leindex-embed communication
//
// The protocol uses length-prefixed bincode frames over a local byte stream
// (Unix domain socket or stdin/stdout pipe). Each frame carries a header with
// batch identity and message type, followed by a serialised payload.
//
// Design goals:
// - Batch identity survives round-trip so the main daemon can correlate
//   responses with the originating request.
// - Payload ordering is preserved: embeddings are returned in the same order
//   as the input texts.
// - Error identity is preserved: the worker reports structured errors rather
//   than opaque strings so the main daemon can decide on fallback behavior.
// - Flat row-major output: embedding vectors are returned as a single
//   contiguous f32 buffer with dimension and count metadata, avoiding nested
//   Vec<Vec<f32>> on the wire.

use serde::{Deserialize, Serialize};

/// Unique batch identifier carried through the request/response cycle.
///
/// The main daemon assigns a batch ID when sending a request. The worker
/// echoes it back unchanged so the daemon can correlate responses even when
/// multiple batches are in flight.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct BatchId(pub u64);

impl BatchId {
    /// Create a new batch ID.
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

impl std::fmt::Display for BatchId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "batch-{}", self.0)
    }
}

/// Frame header preceding every message on the wire.
///
/// The header carries the batch ID and message type so the receiver can
/// dispatch before deserialising the full payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrameHeader {
    /// Batch identifier for correlation.
    pub batch_id: BatchId,
    /// Type of message carried in the payload.
    pub msg_type: MsgType,
}

/// Discriminant for protocol message types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MsgType {
    /// Embed request: texts → vectors.
    EmbedRequest,
    /// Embed response: flat row-major vectors with metadata.
    EmbedResponse,
    /// Rerank request: query + documents → scores.
    RerankRequest,
    /// Rerank response: scored results.
    RerankResponse,
    /// Worker error response.
    Error,
}

/// A complete protocol frame: header + serialised payload.
///
/// On the wire this is encoded as:
/// ```text
/// [4 bytes: payload length, little-endian u32]
/// [bincode-encoded Frame (header + payload)]
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Frame {
    pub header: FrameHeader,
    pub payload: Vec<u8>,
}

impl Frame {
    /// Create a new frame from a header and a serialisable payload.
    pub fn new<T: Serialize>(header: FrameHeader, payload: &T) -> anyhow::Result<Self> {
        let payload_bytes = bincode::serialize(payload)?;
        Ok(Self {
            header,
            payload: payload_bytes,
        })
    }

    /// Decode the payload as a specific type.
    pub fn decode_payload<T: for<'de> Deserialize<'de>>(&self) -> anyhow::Result<T> {
        Ok(bincode::deserialize(&self.payload)?)
    }

    /// Encode the entire frame for wire transmission.
    ///
    /// Wire format: `[4-byte LE length][bincode Frame]`
    pub fn encode_wire(&self) -> anyhow::Result<Vec<u8>> {
        let frame_bytes = bincode::serialize(self)?;
        let len = u32::try_from(frame_bytes.len()).map_err(|_| {
            anyhow::anyhow!(
                "frame payload too large: {} bytes exceeds u32::MAX",
                frame_bytes.len()
            )
        })?;
        let mut wire = Vec::with_capacity(4 + frame_bytes.len());
        wire.extend_from_slice(&len.to_le_bytes());
        wire.extend_from_slice(&frame_bytes);
        Ok(wire)
    }

    /// Decode a frame from wire bytes (without the 4-byte length prefix).
    pub fn from_wire_bytes(bytes: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::deserialize(bytes)?)
    }
}

// ── Embed request/response ──────────────────────────────────────────────

/// Embedding request: a batch of texts to embed.
///
/// VAL-CPHASE-012: The response returns flat row-major output with dimension
/// and count metadata rather than nested vector payloads.
/// VAL-CPHASE-013: Batch ordering is preserved through IPC.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedRequest {
    /// Texts to embed, in order.
    pub texts: Vec<String>,
    /// Expected embedding dimension (for validation).
    pub expected_dim: usize,
}

/// Embedding response: flat row-major vectors with metadata.
///
/// The `vectors` buffer contains `count * dimension` f32 values in row-major
/// order. Embedding `i` occupies `vectors[i * dimension .. (i + 1) * dimension]`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedResponse {
    /// Flat row-major embedding buffer.
    pub vectors: Vec<f32>,
    /// Number of embeddings returned.
    pub count: usize,
    /// Dimension of each embedding.
    pub dimension: usize,
}

impl EmbedResponse {
    /// Create a new embed response from a flat buffer.
    pub fn new(vectors: Vec<f32>, count: usize, dimension: usize) -> Self {
        debug_assert_eq!(vectors.len(), count * dimension);
        Self {
            vectors,
            count,
            dimension,
        }
    }

    /// Extract the embedding for a specific index.
    ///
    /// Returns `None` if the index is out of bounds.
    pub fn get_embedding(&self, index: usize) -> Option<&[f32]> {
        if index >= self.count {
            return None;
        }
        let start = index * self.dimension;
        let end = start + self.dimension;
        Some(&self.vectors[start..end])
    }

    /// Convert into individual embedding vectors.
    pub fn into_vectors(self) -> Vec<Vec<f32>> {
        let dim = self.dimension;
        self.vectors
            .chunks_exact(dim)
            .map(|chunk| chunk.to_vec())
            .collect()
    }
}

// ── Rerank request/response ─────────────────────────────────────────────

/// Reranking request: a query and a set of documents to score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankRequest {
    /// The search query.
    pub query: String,
    /// Documents to rerank, each with an initial score.
    pub documents: Vec<RerankDocument>,
}

/// A document to be reranked.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankDocument {
    /// Unique identifier for the document.
    pub id: String,
    /// Document content text.
    pub content: String,
    /// Initial score from the primary search.
    pub initial_score: f32,
}

/// Reranking response: scored results in ranked order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankResponse {
    /// Reranked results, sorted by combined score descending.
    pub results: Vec<RerankResult>,
}

/// A single reranked result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankResult {
    /// Document identifier.
    pub id: String,
    /// Original score from the primary search.
    pub original_score: f32,
    /// Score from neural reranking.
    pub rerank_score: f32,
    /// Combined score (weighted average).
    pub combined_score: f32,
}

// ── Top-level request/response enums ────────────────────────────────────

/// Top-level request message from main daemon to worker.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Request {
    /// Embed a batch of texts.
    Embed(EmbedRequest),
    /// Rerank documents against a query.
    Rerank(RerankRequest),
}

/// Top-level response message from worker to main daemon.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Response {
    /// Embedding results.
    Embed(EmbedResponse),
    /// Reranking results.
    Rerank(RerankResponse),
    /// Worker error.
    Error(WorkerError),
}

/// Structured error from the worker.
///
/// VAL-CPHASE-003: Error identity is preserved through the protocol so the
/// main daemon can decide on fallback behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerError {
    /// Error classification.
    pub kind: ErrorKind,
    /// Human-readable error message.
    pub message: String,
}

/// Classification of worker errors for fallback decisions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ErrorKind {
    /// ONNX Runtime initialization or execution failure.
    OnnxRuntime,
    /// Model file not found or unreadable.
    ModelNotFound,
    /// Tokenizer failure.
    Tokenizer,
    /// Inference execution failure.
    Inference,
    /// Invalid request (e.g., empty batch, dimension mismatch).
    InvalidRequest,
    /// Internal worker error.
    Internal,
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}: {}", self.kind, self.message)
    }
}

impl std::error::Error for WorkerError {}

// ── Frame helpers for typed construction ─────────────────────────────────

/// Build a frame for an embed request.
pub fn embed_request_frame(batch_id: BatchId, request: EmbedRequest) -> anyhow::Result<Frame> {
    Frame::new(
        FrameHeader {
            batch_id,
            msg_type: MsgType::EmbedRequest,
        },
        &Request::Embed(request),
    )
}

/// Build a frame for an embed response.
pub fn embed_response_frame(batch_id: BatchId, response: EmbedResponse) -> anyhow::Result<Frame> {
    Frame::new(
        FrameHeader {
            batch_id,
            msg_type: MsgType::EmbedResponse,
        },
        &Response::Embed(response),
    )
}

/// Build a frame for a rerank request.
pub fn rerank_request_frame(batch_id: BatchId, request: RerankRequest) -> anyhow::Result<Frame> {
    Frame::new(
        FrameHeader {
            batch_id,
            msg_type: MsgType::RerankRequest,
        },
        &Request::Rerank(request),
    )
}

/// Build a frame for a rerank response.
pub fn rerank_response_frame(batch_id: BatchId, response: RerankResponse) -> anyhow::Result<Frame> {
    Frame::new(
        FrameHeader {
            batch_id,
            msg_type: MsgType::RerankResponse,
        },
        &Response::Rerank(response),
    )
}

/// Build a frame for a worker error.
pub fn error_frame(batch_id: BatchId, error: WorkerError) -> anyhow::Result<Frame> {
    Frame::new(
        FrameHeader {
            batch_id,
            msg_type: MsgType::Error,
        },
        &Response::Error(error),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_batch_id_display() {
        let id = BatchId::new(42);
        assert_eq!(format!("{}", id), "batch-42");
    }

    #[test]
    fn test_batch_id_equality() {
        assert_eq!(BatchId::new(7), BatchId::new(7));
        assert_ne!(BatchId::new(7), BatchId::new(8));
    }

    #[test]
    fn test_embed_response_get_embedding() {
        let resp = EmbedResponse::new(
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            2, // count
            3, // dimension
        );
        assert_eq!(resp.get_embedding(0), Some(&[1.0, 2.0, 3.0][..]));
        assert_eq!(resp.get_embedding(1), Some(&[4.0, 5.0, 6.0][..]));
        assert_eq!(resp.get_embedding(2), None);
    }

    #[test]
    fn test_embed_response_into_vectors() {
        let resp = EmbedResponse::new(vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0], 2, 3);
        let vecs = resp.into_vectors();
        assert_eq!(vecs.len(), 2);
        assert_eq!(vecs[0], vec![1.0, 2.0, 3.0]);
        assert_eq!(vecs[1], vec![4.0, 5.0, 6.0]);
    }

    #[test]
    fn test_frame_roundtrip_embed_request() {
        let batch_id = BatchId::new(123);
        let request = EmbedRequest {
            texts: vec!["hello world".to_string(), "foo bar".to_string()],
            expected_dim: 1024,
        };

        let frame = embed_request_frame(batch_id, request.clone()).unwrap();
        let wire = frame.encode_wire().unwrap();

        // Skip the 4-byte length prefix
        let decoded_frame = Frame::from_wire_bytes(&wire[4..]).unwrap();

        assert_eq!(decoded_frame.header.batch_id, batch_id);
        assert_eq!(decoded_frame.header.msg_type, MsgType::EmbedRequest);

        let decoded_request: Request = decoded_frame.decode_payload().unwrap();
        match decoded_request {
            Request::Embed(embed_req) => {
                assert_eq!(embed_req.texts, request.texts);
                assert_eq!(embed_req.expected_dim, request.expected_dim);
            }
            _ => panic!("Expected Embed request"),
        }
    }

    #[test]
    fn test_frame_roundtrip_embed_response() {
        let batch_id = BatchId::new(456);
        let response = EmbedResponse::new(vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6], 2, 3);

        let frame = embed_response_frame(batch_id, response.clone()).unwrap();
        let wire = frame.encode_wire().unwrap();

        let decoded_frame = Frame::from_wire_bytes(&wire[4..]).unwrap();

        assert_eq!(decoded_frame.header.batch_id, batch_id);
        assert_eq!(decoded_frame.header.msg_type, MsgType::EmbedResponse);

        let decoded_response: Response = decoded_frame.decode_payload().unwrap();
        match decoded_response {
            Response::Embed(embed_resp) => {
                assert_eq!(embed_resp.vectors, response.vectors);
                assert_eq!(embed_resp.count, response.count);
                assert_eq!(embed_resp.dimension, response.dimension);
            }
            _ => panic!("Expected Embed response"),
        }
    }

    #[test]
    fn test_frame_roundtrip_rerank_request() {
        let batch_id = BatchId::new(789);
        let request = RerankRequest {
            query: "test query".to_string(),
            documents: vec![
                RerankDocument {
                    id: "doc1".to_string(),
                    content: "first doc".to_string(),
                    initial_score: 0.9,
                },
                RerankDocument {
                    id: "doc2".to_string(),
                    content: "second doc".to_string(),
                    initial_score: 0.7,
                },
            ],
        };

        let frame = rerank_request_frame(batch_id, request.clone()).unwrap();
        let wire = frame.encode_wire().unwrap();

        let decoded_frame = Frame::from_wire_bytes(&wire[4..]).unwrap();

        assert_eq!(decoded_frame.header.batch_id, batch_id);
        assert_eq!(decoded_frame.header.msg_type, MsgType::RerankRequest);

        let decoded_request: Request = decoded_frame.decode_payload().unwrap();
        match decoded_request {
            Request::Rerank(rerank_req) => {
                assert_eq!(rerank_req.query, request.query);
                assert_eq!(rerank_req.documents.len(), 2);
                assert_eq!(rerank_req.documents[0].id, "doc1");
                assert_eq!(rerank_req.documents[1].id, "doc2");
            }
            _ => panic!("Expected Rerank request"),
        }
    }

    #[test]
    fn test_frame_roundtrip_error() {
        let batch_id = BatchId::new(999);
        let error = WorkerError {
            kind: ErrorKind::ModelNotFound,
            message: "model file missing".to_string(),
        };

        let frame = error_frame(batch_id, error.clone()).unwrap();
        let wire = frame.encode_wire().unwrap();

        let decoded_frame = Frame::from_wire_bytes(&wire[4..]).unwrap();

        assert_eq!(decoded_frame.header.batch_id, batch_id);
        assert_eq!(decoded_frame.header.msg_type, MsgType::Error);

        let decoded_response: Response = decoded_frame.decode_payload().unwrap();
        match decoded_response {
            Response::Error(err) => {
                assert_eq!(err.kind, ErrorKind::ModelNotFound);
                assert_eq!(err.message, "model file missing");
            }
            _ => panic!("Expected Error response"),
        }
    }

    #[test]
    fn test_batch_id_preserved_through_wire() {
        // Verify that batch identity survives a full encode → decode cycle.
        let original_id = BatchId::new(0xDEADBEEF);
        let request = EmbedRequest {
            texts: vec!["test".to_string()],
            expected_dim: 4,
        };

        let frame = embed_request_frame(original_id, request).unwrap();
        let wire = frame.encode_wire().unwrap();
        let decoded = Frame::from_wire_bytes(&wire[4..]).unwrap();

        assert_eq!(decoded.header.batch_id, original_id);
    }

    #[test]
    fn test_payload_ordering_preserved() {
        // Verify that embedding ordering matches input text ordering.
        let texts: Vec<String> = (0..10).map(|i| format!("text {}", i)).collect();
        let request = EmbedRequest {
            texts: texts.clone(),
            expected_dim: 4,
        };

        let frame = embed_request_frame(BatchId::new(1), request).unwrap();
        let decoded: Request = frame.decode_payload().unwrap();

        match decoded {
            Request::Embed(embed_req) => {
                assert_eq!(embed_req.texts, texts);
            }
            _ => panic!("Expected Embed request"),
        }
    }

    #[test]
    fn test_empty_batch_roundtrip() {
        let request = EmbedRequest {
            texts: vec![],
            expected_dim: 1024,
        };

        let frame = embed_request_frame(BatchId::new(0), request).unwrap();
        let decoded: Request = frame.decode_payload().unwrap();

        match decoded {
            Request::Embed(embed_req) => {
                assert!(embed_req.texts.is_empty());
            }
            _ => panic!("Expected Embed request"),
        }
    }
}
