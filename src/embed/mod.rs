//! ONNX embed worker — IPC protocol, ONNX inference runtime, and worker lifecycle.
//!
//! Public for the package worker binary and integration tests; not a stable
//! user API. The worker owns model/session state in a separate process for
//! crash/OOM isolation. Provider selection, model-path resolution, batch
//! splitting, and idle teardown live here.

pub mod batch;
pub mod model_path;
pub mod ort_discovery;
pub mod protocol;
pub mod provider;
pub mod runtime;
pub mod startup;
pub mod worker_main;

pub use protocol::{
    BatchId, EmbedRequest, EmbedResponse, ErrorKind, Frame, FrameHeader, HealthRequest,
    HealthResponse, Request, RerankRequest, RerankResponse, Response, WorkerState,
};

pub use batch::{BatchConfig, split_request, stitch_responses, truncate_text};
pub use model_path::ModelResolver;
pub use ort_discovery::{
    DiscoveryOutcome, DiscoverySource, InitResult, discover_and_init,
    last_outcome as last_ort_outcome,
};
pub use provider::{ExecutionProviderSelector, is_cuda_compiled_in, is_migraphx_compiled_in};
pub use runtime::{
    DEFAULT_IDLE_TIMEOUT_SECS, DEFAULT_MAX_FRAME_SIZE, DEFAULT_MAX_TEXT_SIZE, READ_BUF_CAPACITY,
    RuntimeConfig, WorkerRuntime,
};
pub use startup::{StartupReport, StartupReporter};

// ── Test utilities ──────────────────────────────────────────────────────
//
// A crate-level mutex shared across all test modules so that tests mutating
// process-global state (env vars like LEINDEX_HOME, ORT_DYLIB_PATH) can be
// serialized. Each test module's TEST_LOCK references this to avoid parallel
// conflicts between e.g. config and ort_discovery tests.
#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::Mutex;

    /// Global lock for serializing env-var-mutating tests across all modules.
    pub static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());
}
