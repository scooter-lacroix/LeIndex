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

/// Dispatch macro for `ToolHandler` enum and match-arm generation.
#[macro_use]
pub mod macros;
/// Request handlers for MCP tools.
pub mod handlers;
/// Shared helpers for MCP request processing.
pub mod helpers;
/// MCP protocol definitions and JSON-RPC types.
pub mod protocol;
/// Beautiful output formatting for LeIndex tools.
pub mod output;

/// Handler for LeIndex [Context] — PDG-based context expansion.
pub mod context_handler;
/// Handler for LeIndex [Deep Analyze] — deep code analysis.
pub mod deep_analyze_handler;
/// Handler for LeIndex [Diagnostics] — project diagnostics.
pub mod diagnostics_handler;
/// Handler for LeIndex [Edit Apply] — atomic code modifications.
pub mod edit_apply_handler;
/// Disk-persistent cache for edit previews.
pub mod edit_cache;
/// Handler for LeIndex [Edit Preview] — dry-run code changes.
pub mod edit_preview_handler;
/// Handler for LeIndex [File Summary] — structured file analysis.
pub mod file_summary_handler;
/// Handler for LeIndex [Git Status] — PDG-aware git status.
pub mod git_status_handler;
/// Handler for LeIndex [Grep Symbols] — structurally-aware symbol search.
pub mod grep_symbols_handler;
/// Handler for LeIndex [Impact Analysis] — transitive dependency impact.
pub mod impact_analysis_handler;
/// Handler for LeIndex [Index] — project indexing.
pub mod index_handler;
/// Handler for LeIndex [Phase Analysis] — multi-phase analysis.
pub mod phase_handler;
/// Handler for LeIndex [Project Map] — annotated project tree.
pub mod project_map_handler;
/// Handler for LeIndex [Read File] — PDG-annotated file read.
pub mod read_file_handler;
/// Handler for LeIndex [Read Symbol] — targeted symbol source read.
pub mod read_symbol_handler;
/// Handler for LeIndex [Rename Symbol] — cross-file symbol rename.
pub mod rename_symbol_handler;
/// Handler for LeIndex [Search] — semantic code search.
pub mod search_handler;
/// Handler for LeIndex [Symbol Lookup] — full call graph lookup.
pub mod symbol_lookup_handler;
/// Handler for LeIndex [Text Search] — raw text/regex search.
pub mod text_search_handler;
/// Handler for LeIndex [Write] — atomic file creation with PDG surfacing.
pub mod write_handler;

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
