// LeIndex Embed Worker — Protocol and ONNX inference worker
//
// This crate provides:
// - IPC protocol types shared between the main leindex daemon and the
//   leindex-embed worker process
// - The worker binary that owns ONNX Runtime, tokenizers, and model loading
// - Worker runtime lifecycle (cold start, reuse, idle teardown, restart)
// - Startup reporting, model-path resolution, and execution-provider selection
// - Batch splitting and oversized-input handling
//
// VAL-CPHASE-001: The worker is a separate executable built alongside leindex.
// VAL-CPHASE-002: ONNX runtime deps (ort, tokenizers) belong only to this crate.
// VAL-CPHASE-003: Protocol frames round-trip without losing data.
// VAL-CPHASE-004: Worker transport uses local IPC only.
// VAL-CPHASE-005: Worker cold-starts on first embed demand.
// VAL-CPHASE-006: Worker remains reusable across successive batches.
// VAL-CPHASE-007: Worker idle timeout tears down the resident model process.
// VAL-CPHASE-008: Worker restart works after idle teardown.
// VAL-CPHASE-009: Startup report exposes runtime bundle choices.
// VAL-CPHASE-010: Model path resolution honors precedence.
// VAL-CPHASE-011: Execution-provider selection is externally controllable.
// VAL-CPHASE-012: Embed response is flat row-major output.
// VAL-CPHASE-013: Batch ordering is preserved through IPC.
// VAL-CPHASE-014: Oversized batch is split before transport and re-stitched.
// VAL-CPHASE-015: Single oversized text is reduced before IPC framing.

pub mod batch;
pub mod model_path;
pub mod ort_discovery;
pub mod protocol;
pub mod provider;
pub mod runtime;
pub mod startup;

pub use protocol::{
    BatchId, EmbedRequest, EmbedResponse, ErrorKind, Frame, FrameHeader, Request, RerankRequest,
    RerankResponse, Response,
};

pub use batch::{split_request, stitch_responses, truncate_text, BatchConfig};
pub use model_path::ModelResolver;
pub use ort_discovery::{
    discover_and_init, last_outcome as last_ort_outcome, DiscoveryOutcome, DiscoverySource,
    InitResult,
};
pub use provider::{is_cuda_compiled_in, is_migraphx_compiled_in, ExecutionProviderSelector};
pub use runtime::{
    RuntimeConfig, WorkerRuntime, DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_MAX_FRAME_SIZE,
    DEFAULT_MAX_TEXT_SIZE,
};
pub use startup::{StartupReport, StartupReporter};
