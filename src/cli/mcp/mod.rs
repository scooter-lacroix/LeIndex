// MCP (Model Context Protocol) JSON-RPC Server
//
// This module provides a pure Rust implementation of the MCP protocol
// for LeIndex, enabling AI assistant integration via JSON-RPC 2.0.
//
// # Example
//
// ```ignore
// use leindex::{LeIndex, mcp::McpServer};
//
// #[tokio::main]
// async fn main() -> anyhow::Result<()> {
//     // Create LeIndex instance
//     let leindex = LeIndex::new("/path/to/project")?;
//
//     // Create and run MCP server
//     let config = McpServerConfig::default();
//     let server = McpServer::new(config, leindex);
//     server.run().await?;
//
//     Ok(())
// }
// ```

/// Request handlers for MCP tools.
pub mod handlers;
/// MCP protocol definitions and JSON-RPC types.
pub mod protocol;
/// Shared helpers for MCP request processing.
pub mod helpers;

/// Handler for leindex_context — PDG-based context expansion.
pub mod context_handler;
/// Handler for leindex_deep_analyze — deep code analysis.
pub mod deep_analyze_handler;
/// Handler for leindex_diagnostics — project diagnostics.
pub mod diagnostics_handler;
/// Handler for leindex_edit_apply — atomic code modifications.
pub mod edit_apply_handler;
/// Handler for leindex_edit_preview — dry-run code changes.
pub mod edit_preview_handler;
/// Handler for leindex_file_summary — structured file analysis.
pub mod file_summary_handler;
/// Handler for leindex_git_status — PDG-aware git status.
pub mod git_status_handler;
/// Handler for leindex_grep_symbols — structurally-aware symbol search.
pub mod grep_symbols_handler;
/// Handler for leindex_impact_analysis — transitive dependency impact.
pub mod impact_analysis_handler;
/// Handler for leindex_index — project indexing.
pub mod index_handler;
/// Handler for leindex_phase_analysis — multi-phase analysis.
pub mod phase_handler;
/// Handler for leindex_project_map — annotated project tree.
pub mod project_map_handler;
/// Handler for leindex_read_file — PDG-annotated file read.
pub mod read_file_handler;
/// Handler for leindex_read_symbol — targeted symbol source read.
pub mod read_symbol_handler;
/// Handler for leindex_rename_symbol — cross-file symbol rename.
pub mod rename_symbol_handler;
/// Handler for leindex_search — semantic code search.
pub mod search_handler;
/// Handler for leindex_symbol_lookup — full call graph lookup.
pub mod symbol_lookup_handler;
/// Handler for leindex_text_search — raw text/regex search.
pub mod text_search_handler;

/// MCP server implementation.
pub mod server;
/// SSE streaming support for indexing progress.
pub mod sse;

pub use protocol::{
    error_codes, JsonRpcError, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest,
    JsonRpcResponse, NotificationType,
};
pub use server::{McpServer, McpServerConfig};

/// MCP server version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
