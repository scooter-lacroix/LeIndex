// MCP (Model Context Protocol) JSON-RPC Server
//
// This module provides a pure Rust implementation of the MCP protocol
// for LeIndex, enabling AI assistant integration via JSON-RPC 2.0.
//
// # Example
//
// ```ignore
// use lepasserelle::{LeIndex, mcp::McpServer};
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
/// MCP server implementation.
pub mod server;

pub use protocol::{error_codes, JsonRpcError, JsonRpcRequest, JsonRpcResponse};
pub use server::{McpServer, McpServerConfig};

/// MCP server version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
