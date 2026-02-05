// MCP Server
//
// This module implements the MCP (Model Context Protocol) JSON-RPC server
// using axum for HTTP handling.

use super::handlers::{
    ContextHandler, DeepAnalyzeHandler, DiagnosticsHandler, IndexHandler,
    PhaseAnalysisAliasHandler, PhaseAnalysisHandler, SearchHandler, ToolHandler,
};
use super::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::leindex::LeIndex;
use anyhow::Context;
use axum::{
    response::Json,
    routing::{get, post},
    Router, Server,
};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;
use tower_http::cors::CorsLayer;
use tracing::{debug, info, warn};

/// Global server state - using OnceLock for lazy initialization
/// This works with axum 0.6's trait bounds
pub static SERVER_STATE: std::sync::OnceLock<Arc<Mutex<LeIndex>>> = std::sync::OnceLock::new();

/// Global tool handlers list
pub static HANDLERS: std::sync::OnceLock<Vec<ToolHandler>> = std::sync::OnceLock::new();

/// MCP Server configuration
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    /// Address to bind the server to
    pub bind_address: SocketAddr,

    /// Whether to enable CORS for all origins
    pub enable_cors: bool,

    /// Maximum request size in megabytes
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
pub struct McpServer {
    /// Configuration for the server
    pub config: McpServerConfig,
    /// Shared state containing the LeIndex instance
    pub _state: Arc<Mutex<LeIndex>>, // Keep reference to prevent drop
}

impl McpServer {
    /// Create a new MCP server instance
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
    /// let config = McpServerConfig::default();
    /// let server = McpServer::new(config, leindex)?;
    /// server.run().await?;
    /// ```
    pub fn new(config: McpServerConfig, leindex: LeIndex) -> anyhow::Result<Self> {
        // Initialize global state
        let state = Arc::new(Mutex::new(leindex));
        SERVER_STATE
            .set(state.clone())
            .map_err(|_| anyhow::anyhow!("Server state already initialized"))?;

        // Initialize handlers
        let handlers: Vec<ToolHandler> = vec![
            ToolHandler::DeepAnalyze(DeepAnalyzeHandler),
            ToolHandler::Diagnostics(DiagnosticsHandler),
            ToolHandler::Index(IndexHandler),
            ToolHandler::Context(ContextHandler),
            ToolHandler::Search(SearchHandler),
            ToolHandler::PhaseAnalysis(PhaseAnalysisHandler),
            ToolHandler::PhaseAnalysisAlias(PhaseAnalysisAliasHandler),
        ];
        HANDLERS
            .set(handlers)
            .map_err(|_| anyhow::anyhow!("Handlers already initialized"))?;

        info!("MCP server initialized");

        Ok(Self {
            config,
            _state: state,
        })
    }

    /// Create MCP server with custom configuration
    ///
    /// # Arguments
    ///
    /// * `bind_address` - Address to bind the server to
    /// * `leindex` - LeIndex instance to use for operations
    ///
    /// # Returns
    ///
    /// `Result<McpServer>` - New server instance or error
    pub fn with_address(bind_address: SocketAddr, leindex: LeIndex) -> anyhow::Result<Self> {
        let config = McpServerConfig {
            bind_address,
            ..Default::default()
        };
        Self::new(config, leindex)
    }

    /// Run the MCP server
    ///
    /// Starts the axum HTTP server and handles incoming requests.
    /// This function will block until the server is shut down.
    ///
    /// # Returns
    ///
    /// `anyhow::Result<()>` - Ok on successful shutdown, error on failure
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = McpServerConfig::default();
    /// let server = McpServer::new(config, leindex)?;
    /// server.run().await?;
    /// ```
    pub async fn run(self) -> anyhow::Result<()> {
        let bind_address = self.config.bind_address;
        let router = Self::router();

        info!("Starting MCP server on {}", bind_address);

        Server::bind(&bind_address)
            .serve(router.into_make_service())
            .await
            .context("Server error")?;

        Ok(())
    }

    fn router() -> Router {
        Router::new()
            .route("/mcp", post(json_rpc_handler))
            .route("/mcp/tools/list", get(list_tools_handler))
            .route("/health", get(health_check_handler))
            .layer(CorsLayer::very_permissive())
    }
}

/// JSON-RPC request handler
async fn json_rpc_handler(Json(body): Json<Value>) -> Json<Value> {
    // Parse JSON-RPC request
    let json_req: JsonRpcRequest = match serde_json::from_value(body.clone()) {
        Ok(r) => r,
        Err(e) => {
            warn!("Failed to parse JSON-RPC request: {}", e);
            return Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": null,
                "error": {
                    "code": -32700,
                    "message": "Invalid JSON"
                }
            }));
        }
    };

    let state = match SERVER_STATE.get() {
        Some(s) => s,
        None => {
            warn!("Server state not initialized");
            return Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_req.id,
                "error": {
                    "code": -32603,
                    "message": "Server not initialized"
                }
            }));
        }
    };

    let handlers = match HANDLERS.get() {
        Some(h) => h,
        None => {
            warn!("Handlers not initialized");
            return Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_req.id,
                "error": {
                    "code": -32603,
                    "message": "Handlers not initialized"
                }
            }));
        }
    };

    debug!("Received JSON-RPC request: method={}", json_req.method);
    let id = json_req.id.clone();

    if let Err(e) = json_req.validate() {
        warn!("Invalid JSON-RPC request: {}", e);
        let resp = JsonRpcResponse::error(id, e);
        return Json(serde_json::to_value(&resp).unwrap());
    }

    let response = match json_req.method.as_str() {
        "tools/call" => handle_tool_call(state, handlers, json_req).await,
        "tools/list" => Ok(list_tools_json(handlers)),
        _ => Err(JsonRpcError::method_not_found(json_req.method.clone())),
    };

    let resp = match response {
        Ok(result) => {
            debug!("Request completed successfully");
            JsonRpcResponse::success(id, result)
        }
        Err(e) => {
            warn!("Request failed: {}", e);
            JsonRpcResponse::error(id, e)
        }
    };

    Json(serde_json::to_value(&resp).unwrap())
}

/// Handle tool call requests
pub async fn handle_tool_call(
    state: &Arc<Mutex<LeIndex>>,
    handlers: &[ToolHandler],
    req: JsonRpcRequest,
) -> Result<Value, JsonRpcError> {
    let tool_call = req.extract_tool_call()?;
    debug!("Tool call: name={}", tool_call.name);

    let handler = handlers
        .iter()
        .find(|h| h.name() == tool_call.name)
        .ok_or_else(|| JsonRpcError::method_not_found(tool_call.name.clone()))?;

    // Execute the tool and wrap the result in standard MCP content format
    match handler.execute(state, tool_call.arguments).await {
        Ok(value) => {
            // For DeepAnalyze and Context, we might want to be more specific,
            // but for now, we just stringify the result.
            // MCP expects { content: [{ type: "text", text: "..." }], isError: false }
            Ok(serde_json::json!({
                "content": [
                    {
                        "type": "text",
                        "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| "Error serializing result".to_string())
                    }
                ],
                "isError": false
            }))
        }
        Err(e) => {
            // MCP standard: return errors as a successful JSON-RPC response with isError: true
            warn!("Tool execution failed: {}", e);
            Ok(serde_json::json!({
                "content": [
                    {
                        "type": "text",
                        "text": format!("Error: {}", e)
                    }
                ],
                "isError": true
            }))
        }
    }
}

/// List tools as JSON
pub fn list_tools_json(handlers: &[ToolHandler]) -> Value {
    let tools: Vec<_> = handlers
        .iter()
        .map(|handler| {
            serde_json::json!({
                "name": handler.name(),
                "description": handler.description(),
                "inputSchema": handler.argument_schema()
            })
        })
        .collect();

    serde_json::json!({ "tools": tools })
}

/// List tools handler
async fn list_tools_handler() -> Json<Value> {
    let handlers = match HANDLERS.get() {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({
                "error": "Handlers not initialized"
            }));
        }
    };

    Json(list_tools_json(handlers))
}

/// Health check handler
async fn health_check_handler() -> Json<Value> {
    Json(serde_json::json!({
        "status": "ok",
        "service": "leindex-mcp-server",
        "version": env!("CARGO_PKG_VERSION")
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_config_default() {
        let config = McpServerConfig::default();
        assert_eq!(
            config.bind_address,
            SocketAddr::from(([127, 0, 0, 1], 3000))
        );
    }
}
