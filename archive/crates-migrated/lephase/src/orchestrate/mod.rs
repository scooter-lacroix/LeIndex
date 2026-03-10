//! Orchestration layer for running phase analysis in autonomous loops.

/// Orchestration runtime configuration.
pub mod context;
/// Retry-aware orchestration engine.
pub mod engine;
/// Orchestration request models.
pub mod model;
/// Command/request parser helpers.
pub mod parser;
/// Runner abstraction for phase execution.
pub mod runner;
/// Orchestration lifecycle state.
pub mod state;

pub use context::OrchestrationContext;
pub use engine::{OrchestrationEngine, OrchestrationRunReport};
pub use model::OrchestrationRequest;
pub use parser::parse_request;
pub use runner::{DefaultPhaseRunner, PhaseRunner};
pub use state::{OrchestrationState, RunStatus};
