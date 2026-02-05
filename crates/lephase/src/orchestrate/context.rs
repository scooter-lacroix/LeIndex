use crate::options::PhaseOptions;

/// Runtime configuration for orchestration execution.
#[derive(Debug, Clone)]
pub struct OrchestrationContext {
    /// Analysis options applied to each run attempt.
    pub options: PhaseOptions,
    /// Maximum retry attempts for recoverable failures.
    pub max_retries: usize,
    /// Delay between retries in milliseconds.
    pub retry_delay_ms: u64,
    /// Optional overall wall-clock timeout budget for the orchestration loop.
    pub timeout_ms: Option<u64>,
}

impl OrchestrationContext {
    /// Create orchestration context with sane defaults.
    pub fn new(options: PhaseOptions) -> Self {
        Self {
            options,
            max_retries: 2,
            retry_delay_ms: 150,
            timeout_ms: None,
        }
    }
}
