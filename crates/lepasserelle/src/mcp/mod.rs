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

mod protocol;
mod server;
mod handlers;

pub use server::{McpServer, McpServerConfig};
pub use protocol::{JsonRpcRequest, JsonRpcResponse, JsonRpcError, error_codes};

/// MCP server version
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
