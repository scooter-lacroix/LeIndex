// lepasserelle - Integration & API Layer
//
// *La Passerelle* (The Bridge) - Pure Rust orchestration, CLI, and MCP server

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod leindex;
pub mod memory;
pub mod mcp;

pub use leindex::{LeIndex, IndexStats, AnalysisResult as LeIndexAnalysisResult, Diagnostics};
pub use memory::{MemoryManager, MemoryConfig};
pub use mcp::{LeIndexDeepAnalyze, AnalysisResult as McpAnalysisResult};

/// Library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
