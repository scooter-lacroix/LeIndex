// lepasserelle - Integration & API Layer
//
// *La Passerelle* (The Bridge) - Pure Rust orchestration, CLI, and MCP server

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod memory;
pub mod mcp;

pub use memory::{MemoryManager, MemoryConfig};
pub use mcp::{LeIndexDeepAnalyze, AnalysisResult};

/// Library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
