// lepasserelle - Integration & API Layer
//
// *La Passerelle* (The Bridge) - Pure Rust orchestration, CLI, and MCP server

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod cli;
pub mod config;
pub mod errors;
pub mod leindex;
pub mod memory;

#[cfg(feature = "mcp-server")]
pub mod mcp;

pub use cli::{Cli, Commands};
pub use config::{ProjectConfig, LanguageConfig, TokenConfig, StorageConfig};
pub use errors::{LeIndexError, Result as LeIndexResult, RecoveryStrategy, ErrorContext};
pub use leindex::{LeIndex, IndexStats, AnalysisResult as LeIndexAnalysisResult, Diagnostics};
pub use memory::{MemoryManager, MemoryConfig as MemoryManagementConfig};

#[cfg(feature = "mcp-server")]
pub use mcp::{McpServer, McpServerConfig, JsonRpcRequest, JsonRpcResponse, JsonRpcError, error_codes};

/// Library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
