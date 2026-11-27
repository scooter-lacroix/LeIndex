//! ONNX embed worker — IPC protocol, ONNX inference runtime, and worker lifecycle.
//!
//! Public for the package worker binary and integration tests; not a stable
//! user API. The worker owns model/session state in a separate process for
//! crash/OOM isolation. Provider selection, model-path resolution, batch
//! splitting, and idle teardown live here.

pub mod batch;
pub mod batching;
pub mod cache;
pub mod model_path;
pub mod ort_discovery;
pub mod protocol;
pub mod provider;
pub mod runtime;
/// Env-config helpers + T5/T6 memory guards (kept out of `runtime.rs` so it
/// stays under the 2000-line Large-File gate).
mod runtime_env;
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
// Re-export of the crate-wide test env lock. It used to be a mutex local to
// this module, documented as "crate-level" while excluding nobody outside
// `embed` — so an `embed` test could be mid-way through a LEINDEX_HOME
// override while a `config` test (which takes `crate::test_env::EnvLock`)
// unset the same variable, and `read_config_ort_path` returned None against a
// config that was just written. The shared lock makes those mutually
// exclusive; see src/test_env.rs for the full rationale.
#[cfg(test)]
pub(crate) mod test_util {
    use std::sync::{MutexGuard, PoisonError};

    /// Global lock for serializing env-var-mutating tests across all modules.
    pub struct EnvTestLock;

    impl EnvTestLock {
        /// Acquire the crate-wide env lock, mirroring `Mutex::lock`'s result
        /// so existing `.lock().unwrap()` call sites keep working.
        #[allow(clippy::type_complexity)]
        pub fn lock(
            &self,
        ) -> Result<MutexGuard<'static, ()>, PoisonError<MutexGuard<'static, ()>>> {
            crate::test_env::EnvLock.lock()
        }
    }

    /// Handle to the crate-wide env lock (see [`EnvTestLock`]).
    pub static ENV_TEST_LOCK: EnvTestLock = EnvTestLock;
}
