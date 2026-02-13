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
    response::{sse::{Event, Sse}, Json},
    routing::{get, post},
    Router, Server,
};
use futures_util::stream::{Stream, StreamExt};
use serde_json::Value;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};
use tokio_stream::wrappers::ReceiverStream;
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
            .route("/mcp/index/stream", post(index_stream_handler))
            .layer(tower_http::cors::CorsLayer::very_permissive())
    }
}

/// SSE handler for streaming indexing progress
///
/// This endpoint accepts POST requests with indexing parameters
/// and returns an SSE stream of progress events.
///
/// # Arguments
///
/// * `body` - JSON request body containing:
///   - `project_path` - Absolute path to project directory to index
///   - `force_reindex` - Optional boolean to force re-indexing
///
/// # Returns
///
/// Sse stream that sends progress events as indexing progresses
pub async fn index_stream_handler(
    Json(body): Json<Value>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>> + Send> {
    use super::protocol::ProgressEvent;

    // Create a channel for sending events
    let (tx, rx) = mpsc::channel::<ProgressEvent>(100);

    // Spawn background task for indexing
    tokio::spawn(async move {
        let state = match SERVER_STATE.get() {
            Some(s) => s,
            None => {
                let _ = tx.send(ProgressEvent::error("Server not initialized")).await;
                return;
            }
        };

        // Extract parameters from body
        let project_path = match body.get("project_path").and_then(|v: &Value| v.as_str()) {
            Some(p) => p.to_string(),
            None => {
                let _ = tx.send(ProgressEvent::error("Missing project_path")).await;
                return;
            }
        };

        let force_reindex = body
            .get("force_reindex")
            .and_then(|v: &Value| v.as_bool())
            .unwrap_or(false);

        // Send starting event
        let _ = tx.send(ProgressEvent::progress(
                "starting",
                0,
                0,
                format!("Starting indexing for: {}", project_path),
            ))
            .await;

        // Perform indexing with progress callbacks
        match index_with_progress(state, &project_path, force_reindex, tx.clone()).await {
            Ok(stats) => {
                let _ = tx.send(ProgressEvent::complete(
                        "indexing",
                        format!("Done: {} files", stats.files_parsed),
                    ))
                    .await;
            }
            Err(e) => {
                let _ = tx.send(ProgressEvent::error(format!(
                        "Error: {}", e)))
                    .await;
            }
        }
    });

    // Create SSE stream from receiver
    let stream = ReceiverStream::new(rx)
        .map(|event| -> Result<Event, Infallible> {
            let event_data = Event::default()
                .json_data(event)
                .unwrap_or_else(|_| Event::default().data("error".to_string()));
            Ok(event_data)
        });

    Sse::new(stream)
        .keep_alive(
            axum::response::sse::KeepAlive::new()
                .interval(std::time::Duration::from_secs(15))
                .text("keep-alive")
        )
}

/// Perform indexing with progress reporting via channel
///
/// This helper function runs the indexing operation while sending progress
/// events through the provided channel.
///
/// # Arguments
///
/// * `state` - Reference to global LeIndex state
/// * `project_path` - Path to project to index
/// * `force_reindex` - Whether to re-index even if already indexed
/// * `tx` - Channel sender for progress events
///
/// # Returns
///
/// * `Result<IndexStats, JsonRpcError>` - Index statistics or error
pub async fn index_with_progress(
    state: &Arc<Mutex<LeIndex>>,
    project_path: &str,
    force_reindex: bool,
    tx: mpsc::Sender<super::protocol::ProgressEvent>,
) -> Result<crate::leindex::IndexStats, JsonRpcError> {
    use super::protocol::ProgressEvent;

    let index = state.lock().await;

    // Check if already indexed and we're not forcing reindex
    if index.is_indexed() && !force_reindex {
        let _ = tx.send(ProgressEvent::progress(
                    "skipping",
                    1,
                    1,
                    "Already indexed",
                ))
                .await;
        return Ok(index.get_stats().clone());
    }

    // Send collecting files event
    let _ = tx.send(ProgressEvent::progress(
                "collecting",
                0,
                0,
                "Collecting source files...",
            ))
            .await;

    // Perform indexing in blocking task
    let project_path = project_path.to_string();
    let project_path_for_blocking = project_path.clone();
    let stats = tokio::task::spawn_blocking(move || {
        let mut temp_leindex = LeIndex::new(&project_path_for_blocking).map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to create LeIndex: {}", e))
        })?;

        temp_leindex
            .index_project(force_reindex)
            .map_err(|e| {
                JsonRpcError::indexing_failed(format!("Indexing failed: {}", e))
            })
    })
    .await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))??;

    // Update shared state by loading newly indexed project from storage
    let mut index = state.lock().await;

    let path = std::path::Path::new(&project_path)
        .canonicalize()
        .map_err(|e| JsonRpcError::internal_error(format!("Failed to canonicalize path: {}", e)))
        ?;

    if index.project_path() != path {
        info!("Switching projects: {:?} -> {:?}", index.project_path(), path);
        let _ = tx.send(ProgressEvent::progress(
                    "switching_projects",
                    0,
                    0,
                    format!("{:?}", index.project_path()),
                ))
                .await;

        let _ = index.close();
        *index = LeIndex::new(&path).map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to re-initialize LeIndex: {}", e))
        })?;
    }

    let _ = tx.send(ProgressEvent::progress(
                "loading_storage",
                0,
                0,
                "Loading indexed data from storage...",
            ))
            .await;

    index
        .load_from_storage()
        .map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to load indexed data: {}", e))
        })?;

    Ok(stats)
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
