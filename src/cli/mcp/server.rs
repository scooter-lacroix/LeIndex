// MCP Server
//
// This module implements the MCP (Model Context Protocol) JSON-RPC server
// using axum for HTTP handling.

use super::handlers::{
    ContextHandler, DeepAnalyzeHandler, DiagnosticsHandler, EditApplyHandler, EditPreviewHandler,
    FileSummaryHandler, GitStatusHandler, GrepSymbolsHandler, ImpactAnalysisHandler, IndexHandler,
    PhaseAnalysisAliasHandler, PhaseAnalysisHandler, ProjectMapHandler, ReadFileHandler,
    ReadSymbolHandler, RenameSymbolHandler, SearchHandler, SymbolLookupHandler, TextSearchHandler,
    ToolHandler,
};
use super::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::cli::registry::ProjectRegistry;
use anyhow::Context;
use axum_06::{
    response::{
        sse::{Event, Sse},
        Json,
    },
    routing::{get, post},
    Router, Server,
};
use futures_util::stream::{Stream, StreamExt};
use serde::Serialize;
use serde_json::Value;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, info, warn};

/// Global server state — multi-project registry.
///
/// Replaces the old `Arc<Mutex<LeIndex>>` singleton. Multiple projects can
/// be loaded in one process, with per-project coordination in `ProjectRegistry`.
pub static SERVER_STATE: std::sync::OnceLock<Arc<ProjectRegistry>> = std::sync::OnceLock::new();

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
    /// Multi-project registry (kept alive for the server's lifetime).
    pub _registry: Arc<ProjectRegistry>,
}

impl McpServer {
    /// Create a new MCP server instance
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    /// * `leindex` - Initial LeIndex instance (the startup project)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let leindex = LeIndex::new("/path/to/project")?;
    /// let config = McpServerConfig::default();
    /// let server = McpServer::new(config, leindex)?;
    /// server.run().await?;
    /// ```
    pub fn new(
        config: McpServerConfig,
        leindex: crate::cli::leindex::LeIndex,
    ) -> anyhow::Result<Self> {
        let registry = Arc::new(ProjectRegistry::with_initial_project(
            crate::cli::registry::DEFAULT_MAX_PROJECTS,
            leindex,
        ));
        SERVER_STATE
            .set(registry.clone())
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
            // Phase C: Tool Supremacy
            ToolHandler::FileSummary(FileSummaryHandler),
            ToolHandler::SymbolLookup(SymbolLookupHandler),
            ToolHandler::ProjectMap(ProjectMapHandler),
            ToolHandler::GrepSymbols(GrepSymbolsHandler),
            ToolHandler::ReadSymbol(ReadSymbolHandler),
            // Phase D: Context-Aware Editing
            ToolHandler::EditPreview(EditPreviewHandler),
            ToolHandler::EditApply(EditApplyHandler),
            ToolHandler::RenameSymbol(RenameSymbolHandler),
            ToolHandler::ImpactAnalysis(ImpactAnalysisHandler),
            // Phase E: Precision Tooling
            ToolHandler::TextSearch(TextSearchHandler),
            ToolHandler::ReadFile(ReadFileHandler),
            ToolHandler::GitStatus(GitStatusHandler),
        ];
        HANDLERS
            .set(handlers)
            .map_err(|_| anyhow::anyhow!("Handlers already initialized"))?;

        info!(
            "MCP server initialized (multi-project registry, max {} projects)",
            crate::cli::registry::DEFAULT_MAX_PROJECTS
        );

        Ok(Self {
            config,
            _registry: registry,
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
    pub fn with_address(
        bind_address: SocketAddr,
        leindex: crate::cli::leindex::LeIndex,
    ) -> anyhow::Result<Self> {
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
        // Note: CORS layer removed due to axum 0.6 / tower-http compatibility issues
        // Can be re-added when upgrading to axum 0.7 with matching tower-http version
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
                let _ = tx
                    .send(ProgressEvent::error("Server not initialized"))
                    .await;
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

        let force_reindex = match body.get("force_reindex") {
            Some(Value::Bool(v)) => *v,
            Some(Value::String(v)) => {
                matches!(v.to_ascii_lowercase().as_str(), "true" | "1" | "yes")
            }
            Some(Value::Number(v)) => v.as_u64().map(|n| n != 0).unwrap_or(false),
            _ => false,
        };

        // Send starting event
        let _ = tx
            .send(ProgressEvent::progress(
                "starting",
                0,
                0,
                format!("Starting indexing for: {}", project_path),
            ))
            .await;

        // Perform indexing with progress callbacks
        match index_with_progress(state, &project_path, force_reindex, tx.clone()).await {
            Ok(stats) => {
                let _ = tx
                    .send(ProgressEvent::complete(
                        "indexing",
                        format!("Done: {} files", stats.files_parsed),
                    ))
                    .await;
            }
            Err(e) => {
                let _ = tx.send(ProgressEvent::error(format!("Error: {}", e))).await;
            }
        }
    });

    // Create SSE stream from receiver
    let stream = ReceiverStream::new(rx).map(|event| -> Result<Event, Infallible> {
        let event_data = Event::default()
            .json_data(event)
            .unwrap_or_else(|_| Event::default().data("error".to_string()));
        Ok(event_data)
    });

    Sse::new(stream).keep_alive(
        axum_06::response::sse::KeepAlive::new()
            .interval(std::time::Duration::from_secs(15))
            .text("keep-alive"),
    )
}

/// Perform indexing with progress reporting via channel.
///
/// Uses the `ProjectRegistry` to look up the project and index it.
/// The old data stays readable during indexing; only a brief write-lock
/// swap happens at the end.
pub async fn index_with_progress(
    registry: &Arc<ProjectRegistry>,
    project_path: &str,
    force_reindex: bool,
    tx: mpsc::Sender<super::protocol::ProgressEvent>,
) -> Result<crate::cli::leindex::IndexStats, JsonRpcError> {
    use super::protocol::ProgressEvent;

    // Quick cached check first so we can emit a skip event immediately.
    let handle = registry.get_or_load(Some(project_path)).await?;
    let cached_stats = {
        let idx = handle.lock().await;
        if idx.is_indexed() && !force_reindex {
            Some(idx.get_stats().clone())
        } else {
            None
        }
    };

    if let Some(stats) = cached_stats {
        let _ = tx
            .send(ProgressEvent::progress("skipping", 1, 1, "Already indexed"))
            .await;
        return Ok(stats);
    }

    let _ = tx
        .send(ProgressEvent::progress(
            "collecting",
            0,
            0,
            "Collecting source files...",
        ))
        .await;

    let _ = tx
        .send(ProgressEvent::progress(
            "consolidating",
            0,
            0,
            "Waiting for any in-flight index on this project...",
        ))
        .await;

    let stats = registry
        .index_project(Some(project_path), force_reindex)
        .await?;

    let _ = tx
        .send(ProgressEvent::progress(
            "loading_storage",
            0,
            0,
            "Loading indexed data...",
        ))
        .await;

    Ok(stats)
}

/// Handle MCP initialize request
///
/// Returns server capabilities and information as per MCP protocol.
/// This is the first request sent by MCP clients to negotiate capabilities.
fn handle_initialize() -> Value {
    serde_json::json!({
        "protocolVersion": "2024-11-05",
        "capabilities": {
            "tools": {
                "listChanged": true
            },
            "prompts": {
                "listChanged": true
            },
            "resources": {
                "listChanged": true,
                "subscribe": false
            },
            "logging": {},
            "progress": true
        },
        "serverInfo": {
            "name": "leindex",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "LeIndex MCP Server - Semantic code indexing and analysis with PDG-based tools for superior code comprehension"
        }
    })
}

/// Handle MCP ping request
///
/// Simple health check that returns an empty result.
fn handle_ping() -> Value {
    serde_json::json!({})
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
    let id = json_req.id.clone().unwrap_or(serde_json::Value::Null);

    if let Err(e) = json_req.validate() {
        warn!("Invalid JSON-RPC request: {}", e);
        let resp = JsonRpcResponse::error(id, e);
        return Json(serde_json::to_value(&resp).unwrap());
    }

    let response = match json_req.method.as_str() {
        "initialize" => Ok(handle_initialize()),
        "ping" => Ok(handle_ping()),
        "tools/call" => handle_tool_call(state, handlers, &json_req).await,
        "tools/list" => Ok(list_tools_json(handlers)),
        "prompts/list" => Ok(list_prompts_json()),
        "prompts/get" => handle_prompt_get(&json_req),
        "resources/list" => Ok(list_resources_json()),
        "resources/read" => handle_resource_read(&json_req),
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
    registry: &Arc<ProjectRegistry>,
    handlers: &[ToolHandler],
    req: &JsonRpcRequest,
) -> Result<Value, JsonRpcError> {
    let tool_call = req.extract_tool_call()?;
    debug!("Tool call: name={}", tool_call.name);

    let handler = handlers
        .iter()
        .find(|h| h.name() == tool_call.name)
        .ok_or_else(|| JsonRpcError::method_not_found(tool_call.name.clone()))?;

    // Execute the tool and wrap the result in standard MCP content format
    match handler.execute(registry, tool_call.arguments).await {
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

/// List prompts as JSON
pub fn list_prompts_json() -> Value {
    let prompts = get_prompts();
    serde_json::json!({ "prompts": prompts })
}

/// Handle prompts/get request
pub fn handle_prompt_get(req: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
    let params = req
        .params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("Missing params for prompts/get"))?;

    let name = params
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("Missing or invalid 'name' field"))?;

    let arguments = params.get("arguments").cloned();
    let messages = get_prompt(name, arguments)?;

    Ok(serde_json::json!({
        "description": format!("Prompt: {}", name),
        "messages": messages
    }))
}

/// List resources as JSON
pub fn list_resources_json() -> Value {
    let resources = get_resources();
    serde_json::json!({ "resources": resources })
}

/// Handle resources/read request
pub fn handle_resource_read(req: &JsonRpcRequest) -> Result<Value, JsonRpcError> {
    let params = req
        .params
        .as_ref()
        .ok_or_else(|| JsonRpcError::invalid_params("Missing params for resources/read"))?;

    let uri = params
        .get("uri")
        .and_then(|v| v.as_str())
        .ok_or_else(|| JsonRpcError::invalid_params("Missing or invalid 'uri' field"))?;

    let content = get_resource(uri)?;
    Ok(serde_json::json!({ "contents": [content] }))
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

// ============================================================================
// MCP Prompts Implementation
// ============================================================================

/// A prompt definition for the MCP prompts capability
#[derive(Debug, Clone, Serialize)]
pub struct Prompt {
    /// Unique identifier for the prompt
    pub name: String,
    /// Human-readable description
    pub description: String,
    /// Optional arguments the prompt accepts
    #[serde(skip_serializing_if = "Option::is_none")]
    pub arguments: Option<Vec<PromptArgument>>,
}

/// A prompt argument definition
#[derive(Debug, Clone, Serialize)]
pub struct PromptArgument {
    /// Argument name
    pub name: String,
    /// Argument description
    pub description: String,
    /// Whether the argument is required
    pub required: bool,
}

/// A prompt message (content)
#[derive(Debug, Clone, Serialize)]
pub struct PromptMessage {
    /// Role of the message sender
    pub role: String,
    /// Content of the message
    pub content: PromptContent,
}

/// Content of a prompt message
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type")]
pub enum PromptContent {
    /// Text content
    #[serde(rename = "text")]
    Text {
        /// The text content of the message
        text: String,
    },
}

/// Get the list of available prompts
pub fn get_prompts() -> Vec<Prompt> {
    vec![
        Prompt {
            name: "quickstart".to_string(),
            description: "Quick introduction to using LeIndex effectively".to_string(),
            arguments: None,
        },
        Prompt {
            name: "investigation_workflow".to_string(),
            description: "Step-by-step guide for investigating code with LeIndex".to_string(),
            arguments: Some(vec![PromptArgument {
                name: "query".to_string(),
                description: "What you're trying to find or understand".to_string(),
                required: true,
            }]),
        },
    ]
}

/// Get a specific prompt by name
pub fn get_prompt(
    name: &str,
    arguments: Option<Value>,
) -> Result<Vec<PromptMessage>, JsonRpcError> {
    match name {
        "quickstart" => Ok(vec![
            PromptMessage {
                role: "user".to_string(),
                content: PromptContent::Text {
                    text: "Welcome to LeIndex! Here's how to get started:\n\n1. **Indexing**: First, index your project with `leindex_index`\n2. **Searching**: Use `leindex_search` for semantic code search\n3. **Analysis**: Use `leindex_deep_analyze` for comprehensive code analysis\n4. **Context**: Use `leindex_context` to expand around specific symbols\n\nPro tip: LeIndex auto-indexes on first use, so you can start searching immediately!".to_string(),
                },
            },
        ]),
        "investigation_workflow" => {
            let query = arguments
                .as_ref()
                .and_then(|a| a.get("query"))
                .and_then(|q| q.as_str())
                .unwrap_or("your code investigation");

            Ok(vec![
                PromptMessage {
                    role: "user".to_string(),
                    content: PromptContent::Text {
                        text: format!(
                            "Let me help you investigate: {}\n\nHere's the recommended workflow:\n\n1. **Start broad**: Use `leindex_search` with a natural language query like '{}'\n2. **Find entry points**: Look for the most relevant symbols in the results\n3. **Deep dive**: Use `leindex_deep_analyze` on the most relevant symbol\n4. **Expand context**: Use `leindex_context` to see how the symbol is used\n5. **Navigate**: Follow symbol references with `leindex_read_symbol`\n\nWould you like me to help you with any specific step?",
                            query, query
                        ),
                    },
                },
            ])
        }
        _ => Err(JsonRpcError::method_not_found(format!("Prompt '{}' not found", name))),
    }
}

// ============================================================================
// MCP Resources Implementation
// ============================================================================

/// A resource definition for the MCP resources capability
#[derive(Debug, Clone, Serialize)]
pub struct Resource {
    /// Unique URI for the resource
    pub uri: String,
    /// Human-readable name
    pub name: String,
    /// MIME type of the resource
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Resource description
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Resource content
#[derive(Debug, Clone, Serialize)]
pub struct ResourceContent {
    /// Resource URI
    pub uri: String,
    /// MIME type
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Text content (if text resource)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Binary content (if binary resource)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub blob: Option<String>,
}

/// Get the list of available resources
pub fn get_resources() -> Vec<Resource> {
    vec![
        Resource {
            uri: "leindex://docs/quickstart".to_string(),
            name: "LeIndex Quickstart Guide".to_string(),
            mime_type: Some("text/markdown".to_string()),
            description: Some("Quick start guide for using LeIndex".to_string()),
        },
        Resource {
            uri: "leindex://docs/server-config".to_string(),
            name: "Server Configuration".to_string(),
            mime_type: Some("text/markdown".to_string()),
            description: Some("Configuration options for LeIndex server".to_string()),
        },
    ]
}

/// Get a specific resource by URI
pub fn get_resource(uri: &str) -> Result<ResourceContent, JsonRpcError> {
    match uri {
        "leindex://docs/quickstart" => Ok(ResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: Some(QUICKSTART_GUIDE.to_string()),
            blob: None,
        }),
        "leindex://docs/server-config" => Ok(ResourceContent {
            uri: uri.to_string(),
            mime_type: Some("text/markdown".to_string()),
            text: Some(SERVER_CONFIG_GUIDE.to_string()),
            blob: None,
        }),
        _ => Err(JsonRpcError::method_not_found(format!(
            "Resource '{}' not found",
            uri
        ))),
    }
}

/// Quickstart guide content
const QUICKSTART_GUIDE: &str = r#"# LeIndex Quickstart Guide

## Installation

```bash
cargo install leindex
```

## Basic Usage

### 1. Index a Project

```bash
leindex index /path/to/project
```

Or use the MCP tool:
```json
{
  "name": "leindex_index",
  "arguments": {
    "project_path": "/path/to/project"
  }
}
```

### 2. Search Code

```bash
leindex search "how is authentication handled"
```

Or use the MCP tool:
```json
{
  "name": "leindex_search",
  "arguments": {
    "query": "how is authentication handled",
    "limit": 10
  }
}
```

### 3. Deep Analysis

```bash
leindex analyze --symbol "User::authenticate"
```

Or use the MCP tool:
```json
{
  "name": "leindex_deep_analyze",
  "arguments": {
    "query": "User::authenticate"
  }
}
```

## Available Tools

- `leindex_search` - Semantic code search
- `leindex_deep_analyze` - Comprehensive code analysis
- `leindex_context` - Expand symbol context
- `leindex_grep_symbols` - Search symbols by name
- `leindex_read_file` - Read file with PDG annotations
- `leindex_file_summary` - Get file structural summary

## Environment Variables

- `LEINDEX_HOME` - Storage directory (default: ~/.leindex)
- `LEINDEX_PORT` - Server port (default: 47268)
"#;

/// Server configuration guide content
const SERVER_CONFIG_GUIDE: &str = r#"# LeIndex Server Configuration

## Configuration Options

The LeIndex server can be configured via:

1. Command-line arguments
2. Environment variables
3. Configuration file (config.yaml)

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `LEINDEX_HOME` | Storage/index directory | `~/.leindex` |
| `LEINDEX_PORT` | HTTP server port | `47268` |
| `LEINDEX_HOST` | HTTP server host | `127.0.0.1` |

## MCP Server Mode

Start the MCP server:

```bash
leindex mcp --stdio
```

For HTTP transport:

```bash
leindex serve
```

## Feature Flags

When building from source:

- `full` - All features (default)
- `minimal` - Parse and search only
- `cli` - CLI + MCP server
- `server` - HTTP server only

## Multi-Project Support

The server supports multiple concurrent projects:

```bash
leindex serve --max-projects 10
```

Default maximum: 5 projects.
"#;
