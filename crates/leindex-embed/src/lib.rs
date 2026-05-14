// LeIndex Embed Worker — Protocol and ONNX inference worker
//
// This crate provides:
// - IPC protocol types shared between the main leindex daemon and the
//   leindex-embed worker process
// - The worker binary that owns ONNX Runtime, tokenizers, and model loading
//
// VAL-CPHASE-001: The worker is a separate executable built alongside leindex.
// VAL-CPHASE-002: ONNX runtime deps (ort, tokenizers) belong only to this crate.
// VAL-CPHASE-003: Protocol frames round-trip without losing data.

pub mod protocol;

pub use protocol::{
    BatchId, EmbedRequest, EmbedResponse, ErrorKind, Frame, FrameHeader, Request, RerankRequest,
    RerankResponse, Response,
};
