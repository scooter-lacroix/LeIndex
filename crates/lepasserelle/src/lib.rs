// lepasserelle - Integration & API Layer
//
// *La Passerelle* (The Bridge) - Pure Rust orchestration, CLI, and MCP server
//
// This crate provides the integration and API layer for LeIndex, offering:
//
// - **LeIndex Orchestration**: Unified API for parsing, indexing, searching, and analysis
// - **CLI Interface**: Command-line tool for project indexing, search, and diagnostics
// - **MCP Server**: Model Context Protocol server for LLM tool integration
// - **Memory Management**: Automatic cache spilling and restoration with LRU eviction
//
// ## Features
//
// - `mcp-server` (default): MCP JSON-RPC server functionality
//
// ## Example
//
// ```no_run
// use lepasserelle::LeIndex;
//
// let leindex = LeIndex::new("/path/to/project")?;
// let stats = leindex.index_project()?;
// println!("Indexed {} files", stats.files_parsed);
//
// let results = leindex.search("authentication", 10)?;
// ```
//!
//! # LePasserelle - Integration & API Layer
//!
//! This crate provides the integration and API layer for LeIndex, offering:
//!
//! - **LeIndex Orchestration**: Unified API for parsing, indexing, searching, and analysis
//! - **CLI Interface**: Command-line tool for project indexing, search, and diagnostics
//! - **MCP Server**: Model Context Protocol server for LLM tool integration
//! - **Memory Management**: Automatic cache spilling and restoration with LRU eviction
//!
//! ## Features
//!
//! - `mcp-server` (default): MCP JSON-RPC server functionality
//!
//! ## Quick Start
//!
//! ```text
//! use lepasserelle::LeIndex;
//!
//! // Create a LeIndex instance for a project
//! let leindex = LeIndex::new("/path/to/project")?;
//!
//! // Index the project
//! let stats = leindex.index_project()?;
//! println!("Indexed {} files", stats.files_parsed);
//!
//! // Search for code
//! let results = leindex.search("authentication", 10)?;
//! for result in results {
//!     println!("{}: {}", result.symbol_name, result.file_path);
//! }
//! ```
//!
//! ## CLI Usage
//!
//! ```bash
//! # Index a project
//! leindex index /path/to/project
//!
//! # Search for code
//! leindex search "authentication"
//!
//! # Deep analysis
//! leindex analyze "How does authentication work?"
//!
//! # System diagnostics
//! leindex diagnostics
//! ```

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
