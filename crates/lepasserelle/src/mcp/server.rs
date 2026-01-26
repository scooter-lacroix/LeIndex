// MCP Server
//
// This module implements the MCP (Model Context Protocol) JSON-RPC server
// using axum for HTTP handling.

use anyhow::Context;
use super::handlers::{
    DeepAnalyzeHandler, DiagnosticsHandler, IndexHandler, ContextHandler, SearchHandler,
    ToolHandler,
};
use super::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::leindex::LeIndex;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{IntoResponse, Json, Sse},
    routing::{get, post},
    Router,
};
use serde_json::json;
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tracing::{debug, error, info, warn};

/// MCP Server configuration
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    /// Address to bind the server to
    pub bind_address: SocketAddr,

    /// Enable CORS
    pub enable_cors: bool,

    /// Maximum request size in MB
    pub max_request_size_mb: usize,

    /// Request timeout in seconds
    pub request_timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            bind_address: SocketAddr::from(([127, 0, 0, 1], 3000)),
            enable_cors: true,
            max_request_size_mb: 10,
            request_timeout_secs: 300,
        }
    }
}

/// MCP Server
///
/// The main server struct that manages the LeIndex instance and tool handlers.
pub struct McpServer {
    /// Server configuration
    config: McpServerConfig,

    /// LeIndex instance (shared, wrapped for async access)
    leindex: Arc<Mutex<LeIndex>>,

    /// Registered tool handlers
    handlers: Vec<super::handlers::ToolHandler>,
}

impl McpServer {
    /// Create a new MCP server
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    /// * `leindex` - LeIndex instance to use for operations
    ///
    /// # Example
    ///
    /// ```ignore
    /// let leindex = LeIndex::new("/path/to/project")?;
    /// let server = McpServer::new(McpServerConfig::default(), leindex)?;
    /// ```
    pub fn new(config: McpServerConfig, leindex: LeIndex) -> Self {
        let server = Self {
            config,
            leindex: Arc::new(Mutex::new(leindex)),
            handlers: Self::default_handlers(),
        };

        info!("Registered {} tool handlers", server.handlers.len());

        server
    }

    /// Get the default set of tool handlers
    fn default_handlers() -> Vec<super::handlers::ToolHandler> {
        vec![
            super::handlers::ToolHandler::Index(super::handlers::IndexHandler),
            super::handlers::ToolHandler::Search(super::handlers::SearchHandler),
            super::handlers::ToolHandler::DeepAnalyze(super::handlers::DeepAnalyzeHandler),
            super::handlers::ToolHandler::Context(super::handlers::ContextHandler),
            super::handlers::ToolHandler::Diagnostics(super::handlers::DiagnosticsHandler),
        ]
    }

    /// Build the axum router
    fn router(&self) -> Router<Arc<McpServer>> {
        Router::new()

            // JSON-RPC endpoint
            .route("/mcp", post(json_rpc_handler))

            // MCP discovery endpoints
            .route("/mcp/tools/list", get(list_tools_handler))

            // Health check endpoint
            .route("/health", get(health_check_handler))

            // Server state
            .with_state(Arc::new(self.clone()))

            // CORS layer
            .layer(CorsLayer::very_permissive())
    }

    /// Start the server (blocks until shutdown)
    ///
    /// # Example
    ///
    /// ```ignore
    /// server.run().await?;
    /// ```
    pub async fn run(self) -> anyhow::Result<()> {
        let bind_address = self.config.bind_address;
        let router = self.router();

        info!("Starting MCP server on {}", bind_address);

        let listener = tokio::net::TcpListener::bind(bind_address).await
            .context("Failed to bind to address")?;

        axum::serve(listener, router).await
            .context("Server error")?;

        Ok(())
    }
}

/// Clone the server (needed for Arc wrapping)
impl Clone for McpServer {
    fn clone(&self) -> Self {
        Self {
            config: self.config.clone(),
            leindex: Arc::clone(&self.leindex),
            handlers: self.handlers.clone(),
        }
    }
}

// ========================================================================
// HTTP HANDLERS
// ========================================================================

/// JSON-RPC request handler
///
/// This is the main entry point for all MCP requests.
async fn json_rpc_handler(
    State(server): State<Arc<McpServer>>,
    Json(req): Json<JsonRpcRequest>,
) -> impl IntoResponse {
    debug!("Received JSON-RPC request: method={}", req.method);

    let id = req.id.clone();

    // Validate the request
    if let Err(e) = req.validate() {
        warn!("Invalid JSON-RPC request: {}", e);
        return error_response(id, e);
    }

    // Route the request to the appropriate handler
    let response = match req.method.as_str() {
        "tools/call" => handle_tool_call(server, req).await,
        "tools/list" => Ok(list_tools(server)),
        _ => Err(JsonRpcError::method_not_found(req.method)),
    };

    match response {
        Ok(result) => {
            debug!("Request completed successfully");
            (StatusCode::OK, Json(JsonRpcResponse::success(id, result))).into_response()
        }
        Err(e) => {
            warn!("Request failed: {}", e);
            error_response(id, e)
        }
    }
}

/// Handle a tool call request
async fn handle_tool_call(
    server: Arc<McpServer>,
    req: JsonRpcRequest,
) -> Result<serde_json::Value, JsonRpcError> {
    // Extract tool call parameters
    let tool_call = req.extract_tool_call()?;

    debug!("Tool call: name={}", tool_call.name);

    // Find the handler
    let handler = server.handlers.iter()
        .find(|h| h.name() == tool_call.name)
        .ok_or_else(|| JsonRpcError::method_not_found(tool_call.name.clone()))?;

    // Execute the tool
    let result = handler.execute(&server.leindex, tool_call.arguments).await?;

    Ok(result)
}

/// List available tools
fn list_tools(server: Arc<McpServer>) -> serde_json::Value {
    let tools: Vec<_> = server.handlers.iter()
        .map(|handler| {
            json!({
                "name": handler.name(),
                "description": handler.description(),
                "inputSchema": handler.argument_schema()
            })
        })
        .collect();

    json!({ "tools": tools })
}

/// Health check handler
async fn health_check_handler() -> impl IntoResponse {
    (StatusCode::OK, Json(json!({
        "status": "ok",
        "service": "leindex-mcp-server",
        "version": env!("CARGO_PKG_VERSION")
    })))
}

/// List tools handler (GET endpoint)
async fn list_tools_handler(
    State(server): State<Arc<McpServer>>,
) -> impl IntoResponse {
    (StatusCode::OK, Json(list_tools(server)))
}

/// Create an error response
fn error_response(id: serde_json::Value, error: JsonRpcError) -> axum::response::Response {
    (StatusCode::OK, Json(JsonRpcResponse::error(id, error))).into_response()
}

// ========================================================================
// TESTS
// ========================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_default() {
        let config = McpServerConfig::default();
        assert_eq!(config.bind_address, SocketAddr::from(([127, 0, 0, 1], 3000)));
        assert!(config.enable_cors);
        assert_eq!(config.max_request_size_mb, 10);
    }

    #[test]
    fn test_list_tools() {
        // Create a dummy server for testing
        let config = McpServerConfig::default();
        let leindex = LeIndex::new(".").unwrap(); // Use current directory
        let server = Arc::new(McpServer::new(config, leindex));

        let tools = list_tools(server);
        let tools_array = tools["tools"].as_array().unwrap();

        assert!(!tools_array.is_empty());
        assert!(tools_array.len() >= 5); // At least 5 default tools

        // Check that expected tools are present
        let tool_names: Vec<_> = tools_array.iter()
            .filter_map(|t| t["name"].as_str())
            .collect();

        assert!(tool_names.contains(&"leindex_index"));
        assert!(tool_names.contains(&"leindex_search"));
        assert!(tool_names.contains(&"leindex_deep_analyze"));
        assert!(tool_names.contains(&"leindex_context"));
        assert!(tool_names.contains(&"leindex_diagnostics"));
    }
}
