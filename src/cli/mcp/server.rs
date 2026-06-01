// MCP Server
//
// This module implements the MCP (Model Context Protocol) JSON-RPC server
// using axum for HTTP handling.

use super::handlers::{all_tool_handlers, ToolHandler};
use super::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use crate::cli::registry::ProjectRegistry;
use anyhow::Context;
use axum::{
    extract::Json,
    http::{HeaderMap, HeaderValue, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Router,
};
use futures_util::stream::{Stream, StreamExt};
use serde::Serialize;
use serde_json::Value;
use dashmap::DashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{
    atomic::{AtomicBool, AtomicU64, Ordering},
    Arc,
};
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_stream::wrappers::ReceiverStream;
use tracing::{debug, error, info, warn};

/// Global server state — multi-project registry.
///
/// Replaces the old `Arc<Mutex<LeIndex>>` singleton. Multiple projects can
/// be loaded in one process, with per-project coordination in `ProjectRegistry`.
pub static SERVER_STATE: std::sync::OnceLock<Arc<ProjectRegistry>> = std::sync::OnceLock::new();

/// Global server instance — for handshake and state management.
pub static SERVER_INSTANCE: std::sync::OnceLock<Arc<McpServer>> = std::sync::OnceLock::new();

/// Global tool handlers list
pub static HANDLERS: std::sync::OnceLock<Vec<ToolHandler>> = std::sync::OnceLock::new();

/// Monotonic counter for generating session IDs (no `uuid` dependency).
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(1);

/// Generate a unique session ID string: `"leindex-<pid>-<seq>"`.
fn generate_session_id() -> String {
    let pid = std::process::id();
    let seq = SESSION_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("leindex-{pid}-{seq}")
}

/// Default MCP server port.
///
/// Chosen in IANA dynamic/private range (49152-65535) and well above the
/// common dev-server range. This port is unlikely to be in use by other
/// processes, but the runtime still auto-falls-back to the next 10 ports
/// (and ultimately any free port up to 65535) if a conflict occurs.
///
/// Override with the `LEINDEX_PORT` environment variable.
pub const DEFAULT_MCP_PORT: u16 = 47500;

/// Default per-tool-call timeout in seconds.
///
/// Hard cap on any single MCP tool call so a slow operation cannot block
/// the server indefinitely. Individual tool handlers may set a tighter
/// internal timeout.
pub const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Number of consecutive ports to try on `bind()` failure before giving up.
pub const BIND_FALLBACK_PORT_RANGE: u16 = 10;

/// Default cap on concurrently-tracked HTTP sessions before the oldest
/// idle session is evicted to make room for a new one. Tunable via the
/// `LEINDEX_MAX_SESSIONS` environment variable; the env override is
/// useful for CI farms that spawn many short-lived clients.
pub const DEFAULT_MAX_HTTP_SESSIONS: usize = 1000;

/// Environment variable that overrides `DEFAULT_MAX_HTTP_SESSIONS`.
pub const MAX_SESSIONS_ENV: &str = "LEINDEX_MAX_SESSIONS";

/// Resolved cap on concurrent HTTP sessions.
pub fn max_http_sessions() -> usize {
    match std::env::var(MAX_SESSIONS_ENV) {
        Ok(v) => v
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .unwrap_or(DEFAULT_MAX_HTTP_SESSIONS),
        Err(_) => DEFAULT_MAX_HTTP_SESSIONS,
    }
}

/// MCP Server configuration
#[derive(Clone, Debug)]
pub struct McpServerConfig {
    /// Address to bind the server to
    pub bind_address: SocketAddr,

    /// Whether to enable CORS for all origins
    pub enable_cors: bool,

    /// Maximum request size in megabytes
    pub max_request_size_mb: usize,

    /// Request timeout in seconds (per tool call)
    pub request_timeout_secs: u64,
}

impl Default for McpServerConfig {
    fn default() -> Self {
        Self {
            // Use 127.0.0.1 (loopback only) on a high, rarely-used port in the
            // IANA dynamic/private range. The server attempts to auto-fallback
            // to the next consecutive ports if the default is in use.
            bind_address: SocketAddr::from(([127, 0, 0, 1], DEFAULT_MCP_PORT)),
            enable_cors: true,
            max_request_size_mb: 10,
            request_timeout_secs: DEFAULT_REQUEST_TIMEOUT_SECS,
        }
    }
}

/// MCP Server
#[derive(Clone)]
pub struct McpServer {
    /// Configuration for the server
    pub config: McpServerConfig,
    /// Multi-project registry (kept alive for the server's lifetime).
    pub _registry: Arc<ProjectRegistry>,
    /// Flag to track MCP handshake completion (used by stdio transport — single client).
    pub(crate) handshake_complete: Arc<AtomicBool>,
    /// Per-session handshake state for HTTP and stdio transports (session ID → (handshaked, last_access_time)).
    /// Keyed by the `Mcp-Session-Id` header value for HTTP, and generated session ID for stdio.
    pub(crate) session_handshakes: Arc<DashMap<String, (bool, Instant)>>,
    /// Session IDs that currently have an in-flight tool call.
    ///
    /// Cleanup skips these so a long-running tool call is never evicted
    /// mid-flight by the idle-expiration sweep. Keys are `Arc<str>` so
    /// `begin_request`/`end_request` bump a refcount instead of
    /// allocating a new `String` for every request.
    pub(crate) in_flight: Arc<DashMap<Arc<str>, ()>>,
}

impl McpServer {
    /// Create a new MCP server instance
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = McpServerConfig::default();
    /// let server = McpServer::new(config)?;
    /// server.run().await?;
    /// ```
    pub fn new(config: McpServerConfig) -> anyhow::Result<Self> {
        let registry = Arc::new(ProjectRegistry::new(
            crate::cli::registry::DEFAULT_MAX_PROJECTS,
        ));
        SERVER_STATE
            .set(registry.clone())
            .map_err(|_| anyhow::anyhow!("Server state already initialized"))?;

        // Initialize handlers
        let handlers: Vec<ToolHandler> = all_tool_handlers();
        HANDLERS
            .set(handlers)
            .map_err(|_| anyhow::anyhow!("Handlers already initialized"))?;

        info!(
            "MCP server initialized (multi-project registry, max {} projects)",
            crate::cli::registry::DEFAULT_MAX_PROJECTS
        );

        let server = Self {
            config,
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
        };

        SERVER_INSTANCE
            .set(Arc::new(server.clone()))
            .map_err(|_| anyhow::anyhow!("Server instance already initialized"))?;

        Ok(server)
    }

    /// Create MCP server with custom configuration
    ///
    /// # Arguments
    ///
    /// * `bind_address` - Address to bind the server to
    ///
    /// # Returns
    ///
    /// `Result<McpServer>` - New server instance or error
    pub fn with_address(bind_address: SocketAddr) -> anyhow::Result<Self> {
        let config = McpServerConfig {
            bind_address,
            ..Default::default()
        };
        Self::new(config)
    }

    /// Clean up stale sessions that have not been accessed within the timeout.
    ///
    /// A+ hotspot cleanup: prevents session-tracking state from growing
    /// monotonically across long-lived server sessions (VAL-APLUS-025).
    ///
    /// Sessions with an in-flight tool call are preserved — evicting them
    /// mid-request would cause spurious "Server not initialized" errors
    /// for the active client.
    pub fn cleanup_stale_sessions(&self, max_idle: std::time::Duration) -> usize {
        let before = self.session_handshakes.len();
        self.session_handshakes.retain(|sid, (_, last_access)| {
            // `sid` is `&String`; look up in `in_flight` as `&str` so
            // DashMap resolves the Borrow trait to `Arc<str>: Borrow<str>`.
            if self.in_flight.contains_key(sid.as_str()) {
                // Active request — never evict.
                true
            } else {
                last_access.elapsed() < max_idle
            }
        });
        before - self.session_handshakes.len()
    }

    /// Get the number of active sessions (for diagnostics and testing).
    pub fn active_session_count(&self) -> usize {
        self.session_handshakes.len()
    }

    /// Returns true if the given session has an in-flight tool call.
    ///
    /// The cleanup task uses this to avoid evicting sessions that are
    /// currently processing a request. `Arc<str>` and `&str` produce
    /// the same hash, so DashMap's borrow-based lookup works without
    /// allocating.
    pub fn session_in_flight(&self, session_id: &str) -> bool {
        self.in_flight.contains_key(session_id)
    }

    /// Mark a session as having an in-flight request.
    ///
    /// Idempotent — repeated calls are no-ops. The `Arc<str>` key
    /// refcounts the session ID rather than allocating a new `String`
    /// for every request.
    pub fn begin_request(&self, session_id: &str) {
        self.in_flight
            .insert(Arc::<str>::from(session_id), ());
    }

    /// Mark a session's in-flight request as complete.
    pub fn end_request(&self, session_id: &str) {
        self.in_flight.remove(session_id);
    }

    /// Acquire a panic-safe in-flight guard for the given session.
    ///
    /// The returned `InFlightGuard` removes the session from the
    /// `in_flight` set on drop, so the cleanup task never sees a
    /// session as in-flight if the tool call panics, the timeout
    /// future is dropped, or any other path bypasses the explicit
    /// `end_request` call. This is a small RAII wrapper that
    /// eliminates an entire class of session-leak bugs.
    pub fn in_flight_guard(server: &Arc<Self>, session_id: &str) -> InFlightGuard {
        server.begin_request(session_id);
        InFlightGuard {
            server: Arc::clone(server),
            session_id: Arc::<str>::from(session_id),
        }
    }

    /// Run the MCP server
    ///
    /// Starts the axum HTTP server and handles incoming requests.
    /// A background task runs `cleanup_stale_sessions` every 60 seconds
    /// to prevent session-tracking state from growing monotonically
    /// (VAL-APLUS-025).
    ///
    /// # Returns
    ///
    /// `anyhow::Result<()>` - Ok on successful shutdown, error on failure
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = McpServerConfig::default();
    /// let server = McpServer::new(config)?;
    /// server.run().await?;
    /// ```
    pub async fn run(self) -> anyhow::Result<()> {
        let bind_address = self.config.bind_address;
        let router = Self::router();

        // Spawn background task to clean up stale sessions periodically.
        // Uses 60-second interval and 5-minute idle threshold.
        // The task body is wrapped to catch panics so the cleanup loop
        // doesn't die silently (Fix 6).
        let cleanup_server = self.clone();
        let _cleanup_handle = tokio::spawn(async move {
            const CLEANUP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(60);
            const SESSION_MAX_IDLE: std::time::Duration = std::time::Duration::from_secs(300); // 5 minutes
            let mut interval = tokio::time::interval(CLEANUP_INTERVAL);
            loop {
                interval.tick().await;
                let removed = cleanup_server.cleanup_stale_sessions(SESSION_MAX_IDLE);
                if removed > 0 {
                    debug!("Cleaned up {} stale session(s)", removed);
                }
            }
        });
        // Detach with error logging: if the cleanup task panics or errors,
        // log it rather than dying silently.
        tokio::spawn(async move {
            if let Err(e) = _cleanup_handle.await {
                error!("cleanup task died: {e}");
            }
        });

        info!("Starting MCP server on {}", bind_address);

        // Auto-bind-fallback: if the default port is taken, try the next
        // consecutive ports before giving up. Eliminates the most common
        // "MCP fails to connect" failure mode where another process holds
        // the port and the user has no idea why the server won't start.
        let listener = bind_with_fallback(bind_address).await?;
        if listener.local_addr()? != bind_address {
            warn!(
                "Default port {} was unavailable; bound to fallback {}",
                bind_address.port(),
                listener.local_addr()?.port()
            );
        }
        axum::serve(listener, router.into_make_service())
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

/// Try to bind to `preferred`. If unavailable, walk forward up to
/// `BIND_FALLBACK_PORT_RANGE` ports, then accept whatever ephemeral port
/// the OS hands out. This makes "another process took my port" a
/// recoverable warning instead of a fatal startup error.
async fn bind_with_fallback(
    preferred: SocketAddr,
) -> anyhow::Result<tokio::net::TcpListener> {
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..=BIND_FALLBACK_PORT_RANGE {
        let candidate = SocketAddr::new(preferred.ip(), preferred.port().saturating_add(offset));
        match tokio::net::TcpListener::bind(candidate).await {
            Ok(listener) => return Ok(listener),
            Err(e) => {
                debug!("bind({}) failed: {}", candidate, e);
                last_err = Some(e);
            }
        }
    }
    // Last resort — ask the OS for any free ephemeral port on the same IP.
    match tokio::net::TcpListener::bind(SocketAddr::new(preferred.ip(), 0)).await {
        Ok(listener) => Ok(listener),
        Err(ephemeral_err) => {
            // Surface a single descriptive error that mentions both the
            // preferred port and the ephemeral-bind failure so the user
            // can see why nothing worked without having to dig through
            // two stacked anyhow contexts.
            let preferred_err = last_err
                .as_ref()
                .map(|e| format!(" ({})", e))
                .unwrap_or_default();
            Err(anyhow::anyhow!(
                "failed to bind to {} or any of the next {} ports{}; \
                 ephemeral-bind fallback also failed: {}",
                preferred,
                BIND_FALLBACK_PORT_RANGE,
                preferred_err,
                ephemeral_err,
            ))
        }
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
            .unwrap_or_else(|_| Event::default().data("error"));
        Ok(event_data)
    });

    Sse::new(stream).keep_alive(
        KeepAlive::new()
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
        let idx = handle.read().await;
        if idx.is_indexed() && !idx.is_stale_fast() && !force_reindex {
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
///
/// For HTTP transport, generates a per-session ID and stores it in the session map.
fn handle_initialize(server: &McpServer) -> (Value, Option<String>) {
    // Generate a session ID for HTTP transport
    let session_id = generate_session_id();

    // Store in per-session map with eviction logic
    {
        let max_sessions = max_http_sessions();
        if server.session_handshakes.len() >= max_sessions {
            // Find the oldest session (by last_access_time) that is NOT
            // currently processing a request. `cleanup_stale_sessions`
            // applies the same rule; without it, a long tool call can be
            // evicted mid-request, which surfaces as a spurious
            // "Server not initialized" error for the active client.
            let oldest_id = server
                .session_handshakes
                .iter()
                .filter(|r| !server.in_flight.contains_key(r.key().as_str()))
                .min_by_key(|r| r.value().1)
                .map(|r| r.key().clone());
            if let Some(id) = oldest_id {
                server.session_handshakes.remove(id.as_str());
            }
        }
        server.session_handshakes.insert(session_id.clone(), (true, Instant::now()));
    }

    let result = serde_json::json!({
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
        },
        "instructions": [
            "Projects are no longer auto-indexed on startup. Use explicit tool calls to index projects.",
            "The server must receive an 'initialize' call before processing other requests."
        ]
    });

    (result, Some(session_id))
}

/// Handle MCP ping request
///
/// Simple health check that returns an empty result.
fn handle_ping() -> Value {
    serde_json::json!({})
}

/// JSON-RPC request handler
async fn json_rpc_handler(headers: HeaderMap, Json(body): Json<Value>) -> Response {
    // Extract Mcp-Session-Id header (if present)
    let incoming_session_id = headers
        .get("Mcp-Session-Id")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

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
            }))
            .into_response();
        }
    };

    let server_instance = match SERVER_INSTANCE.get() {
        Some(s) => s,
        None => {
            warn!("Server instance not initialized");
            return Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_req.id,
                "error": {
                    "code": -32603,
                    "message": "Server instance not initialized"
                }
            }))
            .into_response();
        }
    };

    let state = server_instance._registry.clone();

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
            }))
            .into_response();
        }
    };

    debug!("Received JSON-RPC request: method={}", json_req.method);
    let id = json_req.id.clone().unwrap_or(serde_json::Value::Null);

    if let Err(e) = json_req.validate() {
        warn!("Invalid JSON-RPC request: {}", e);
        let resp = JsonRpcResponse::error(id, e);
        return Json(serde_json::to_value(&resp).unwrap()).into_response();
    }

    // Track if this is a notification (no response should be sent per JSON-RPC 2.0 spec)
    let is_notification = json_req.id.is_none();

    // Per-session handshake check for HTTP transport
    // Notifications (id is null) must not receive a response per JSON-RPC 2.0 spec
    if is_notification {
        return StatusCode::NO_CONTENT.into_response();
    }

    // `ping` is a health probe and must work without a session.
    // `initialize` provisions a new session and is allowed pre-handshake.
    if json_req.method == "initialize" {
        // Generate new session, store, and return session ID header
    } else if json_req.method == "ping" {
        // Health probe — no session required.
    } else {
        // All other methods require a valid handshaked session.
        let session_ok = match &incoming_session_id {
            Some(sid) => {
                if let Some(mut entry) = server_instance.session_handshakes.get_mut(sid) {
                    // Update last access time
                    entry.1 = Instant::now();
                    entry.0
                } else {
                    false
                }
            }
            None => false,
        };

        if !session_ok {
            return Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": json_req.id,
                "error": {
                    "code": -32000,
                    "message": "Server not initialized. Call 'initialize' first."
                }
            }))
            .into_response();
        }
    }

    let response = match json_req.method.as_str() {
        "initialize" => {
            let (result, session_id) = handle_initialize(server_instance);
            let resp = JsonRpcResponse::success(id.clone(), result);
            let body = Json(serde_json::to_value(&resp).unwrap()).into_response();
            // Attach Mcp-Session-Id response header
            if let Some(sid) = session_id {
                let mut response = body;
                let sid_header = HeaderValue::from_str(&sid)
                    .unwrap_or_else(|_| HeaderValue::from_static("unknown"));
                response.headers_mut().insert("Mcp-Session-Id", sid_header);
                return response;
            }
            return body;
        }
        "ping" => Ok(handle_ping()),
        "tools/call" => {
            // Per-tool-call hard timeout + in-flight session tracking so the
            // background cleanup task never evicts a session that's still
            // processing a request. The in-flight guard is RAII — its Drop
            // always calls `end_request`, so a panic inside `handle_tool_call`
            // or a dropped timeout future cannot leak the session in the
            // in-flight map forever.
            let timeout = std::time::Duration::from_secs(server_instance.config.request_timeout_secs);
            let _guard = incoming_session_id
                .as_ref()
                .map(|sid| McpServer::in_flight_guard(server_instance, sid));
            let tool_result = tokio::time::timeout(
                timeout,
                handle_tool_call(&state, handlers, &json_req),
            )
            .await;
            match tool_result {
                Ok(result) => result,
                Err(_) => Err(JsonRpcError::internal_error(format!(
                    "Tool call timed out after {}s",
                    server_instance.config.request_timeout_secs
                ))),
            }
        }
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

    // Return response body (notifications already handled at function entry)
    Json(serde_json::to_value(&resp).unwrap()).into_response()
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
            // The MCP transport is what the LLM actually sees. Run the
            // tool's raw value through the per-tool payload trimmer so we
            // hand the model only the fields it needs (no scoring
            // internals, no byte ranges, no tfidf/neural split, etc.).
            // The CLI surface uses `output::render_tool_output` over the
            // same value for its human-readable view.
            let trimmed = crate::cli::mcp::output::trim_llm_payload(&tool_call.name, &value);
            let payload = serde_json::to_string_pretty(&trimmed)
                .unwrap_or_else(|_| "Error serializing result".to_string());
            Ok(serde_json::json!({
                "content": [
                    {
                        "type": "text",
                        "text": payload
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
///
/// Public discovery endpoint — no handshake required.
/// If a `Mcp-Session-Id` header is present, it is validated but
/// the endpoint still functions without one.
async fn list_tools_handler(headers: HeaderMap) -> Json<Value> {
    // Validate session ID if present, but don't require one
    if let Some(sid) = headers.get("Mcp-Session-Id").and_then(|v| v.to_str().ok()) {
        if let Some(server) = SERVER_INSTANCE.get() {
            if let Some(mut entry) = server.session_handshakes.get_mut(sid) {
                // Update last access time
                entry.1 = Instant::now();
                if !entry.0 {
                    return Json(serde_json::json!({
                        "error": "Invalid session. Call 'initialize' first."
                    }));
                }
            }
            // Unknown session ID on a discovery endpoint — allow it (client may be probing)
        }
    }

    // Only verify that the server instance exists.
    if SERVER_INSTANCE.get().is_none() {
        return Json(serde_json::json!({
            "error": "Server instance not initialized"
        }));
    }

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

// ============================================================================
// In-Flight Guard (RAII panic safety for tool-call tracking)
// ============================================================================

/// RAII guard returned by [`McpServer::in_flight_guard`].
///
/// Removes the session from the server's `in_flight` set on `Drop`, so
/// the cleanup task never sees a session as in-flight if the tool call
/// panics, the timeout future is dropped, or any other code path
/// bypasses the explicit `end_request` call. Without this guard, a
/// panic inside a tool handler would leak the session in the in-flight
/// map forever, defeating the cleanup task's ability to evict the
/// session.
///
/// # Note
///
/// `Arc::<str>::from(&str)` *does* allocate a new heap block on
/// insert — the optimization vs. `String` is that subsequent
/// `DashMap::contains_key(&str)` lookups don't allocate because the
/// `Borrow` trait makes `&str` and `Arc<str>` use the same hash.
pub struct InFlightGuard {
    server: Arc<McpServer>,
    session_id: Arc<str>,
}

impl Drop for InFlightGuard {
    fn drop(&mut self) {
        self.server.end_request(&self.session_id);
    }
}

// ============================================================================
// Unix Domain Socket Transport
// ============================================================================

/// RAII guard that removes the socket file on drop.
#[cfg(unix)]
pub struct SocketCleanupGuard {
    path: std::path::PathBuf,
}

#[cfg(unix)]
impl Drop for SocketCleanupGuard {
    fn drop(&mut self) {
        if self.path.exists() {
            let _ = std::fs::remove_file(&self.path);
            debug!("Cleaned up socket file: {}", self.path.display());
        }
    }
}

#[cfg(unix)]
impl McpServer {
    /// Run the MCP server over a Unix domain socket.
    ///
    /// Binds to `socket_path`, accepts connections in a loop, and spawns a
    /// tokio task per connection. Each connection gets its own session ID
    /// registered in `session_handshakes`. The JSON-RPC message loop reuses
    /// the same handler logic as the stdio transport.
    ///
    /// The socket file is removed when the returned future completes or is
    /// dropped (via `SocketCleanupGuard`).
    pub async fn run_socket(&self, socket_path: &std::path::Path) -> anyhow::Result<()> {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
        use tokio::net::UnixListener;

        // Remove stale socket file if present
        if socket_path.exists() {
            std::fs::remove_file(socket_path).context("Failed to remove existing socket file")?;
        }

        // Ensure parent directory exists
        if let Some(parent) = socket_path.parent() {
            std::fs::create_dir_all(parent).context("Failed to create socket directory")?;
        }

        let listener = UnixListener::bind(socket_path)
            .with_context(|| format!("Failed to bind Unix socket at {}", socket_path.display()))?;

        let _guard = SocketCleanupGuard {
            path: socket_path.to_path_buf(),
        };

        info!(
            "MCP server listening on Unix socket: {}",
            socket_path.display()
        );

        loop {
            let (stream, _addr) = listener
                .accept()
                .await
                .context("Failed to accept connection")?;

            let session_id = generate_session_id();
            self.session_handshakes.insert(session_id.clone(), (false, Instant::now()));

            let session_id_clone = session_id.clone();
            let session_handshakes = self.session_handshakes.clone();
            let handshake_complete = self.handshake_complete.clone();

            tokio::spawn(async move {
                debug!(
                    "Accepted Unix socket connection (session: {})",
                    session_id_clone
                );

                let (reader, mut writer) = stream.into_split();
                let mut reader = BufReader::new(reader);
                let mut use_content_length = false;

                // Security limits to prevent memory exhaustion attacks
                const MAX_LINE_LENGTH: usize = 10_240; // 10KB max line length
                const MAX_PAYLOAD_SIZE: usize = 10_485_760; // 10MB max payload size

                loop {
                    let mut line = String::new();
                    match reader.read_line(&mut line).await {
                        Ok(0) => break, // EOF
                        Ok(_) => {
                            if line.len() > MAX_LINE_LENGTH {
                                debug!(
                                    "Line too long (session {}): {} bytes",
                                    session_id_clone,
                                    line.len()
                                );
                                break;
                            }
                        }
                        Err(e) => {
                            debug!("Socket read error (session {}): {}", session_id_clone, e);
                            break;
                        }
                    };

                    let line_trim = line.trim_end();
                    if line_trim.is_empty() {
                        continue;
                    }

                    let json_payload = if line_trim
                        .to_ascii_lowercase()
                        .starts_with("content-length:")
                    {
                        let len_str = line_trim.split(':').nth(1).unwrap_or("").trim();
                        let length: usize = match len_str.parse() {
                            Ok(v) => v,
                            Err(e) => {
                                debug!("Invalid Content-Length header: {}", e);
                                continue;
                            }
                        };

                        // Reject excessively large payloads to prevent OOM
                        if length > MAX_PAYLOAD_SIZE {
                            debug!(
                                "Payload too large (session {}): {} bytes",
                                session_id_clone, length
                            );
                            break;
                        }

                        // Consume remaining header lines until blank line
                        loop {
                            let mut header = String::new();
                            match reader.read_line(&mut header).await {
                                Ok(0) => break,
                                Ok(_) => {
                                    if header.len() > MAX_LINE_LENGTH {
                                        debug!(
                                            "Header line too long (session {}): {} bytes",
                                            session_id_clone,
                                            header.len()
                                        );
                                        break;
                                    }
                                    if header.trim().is_empty() {
                                        break;
                                    }
                                }
                                Err(_) => break,
                            }
                        }

                        let mut buf = vec![0u8; length];
                        if let Err(e) = reader.read_exact(&mut buf).await {
                            debug!("Failed to read JSON payload: {}", e);
                            break;
                        }

                        use_content_length = true;
                        String::from_utf8_lossy(&buf).to_string()
                    } else {
                        line_trim.to_string()
                    };

                    // Parse and handle the JSON-RPC message
                    let response_json = match handle_socket_message(
                        &json_payload,
                        &session_id_clone,
                        &session_handshakes,
                        &handshake_complete,
                    )
                    .await
                    {
                        Some(json) => json,
                        None => continue, // Notification, no response
                    };

                    // Send response
                    if use_content_length {
                        let msg = format!(
                            "Content-Length: {}\r\n\r\n{}",
                            response_json.len(),
                            response_json
                        );
                        if writer.write_all(msg.as_bytes()).await.is_err() {
                            break;
                        }
                    } else {
                        let msg = format!("{}\n", response_json);
                        if writer.write_all(msg.as_bytes()).await.is_err() {
                            break;
                        }
                    }
                    let _ = writer.flush().await;
                }

                // Clean up session on disconnect
                session_handshakes.remove(&session_id_clone);

                debug!("Socket connection closed (session: {})", session_id_clone);
            });
        }

        #[allow(unreachable_code)]
        {
            // _guard is dropped here, cleaning up the socket file
            Ok(())
        }
    }
}

/// Handle a single JSON-RPC message received over a Unix socket connection.
/// Returns `Some(response_json)` or `None` for notifications (no response).
#[cfg(unix)]
async fn handle_socket_message(
    json_payload: &str,
    session_id: &str,
    session_handshakes: &Arc<DashMap<String, (bool, Instant)>>,
    handshake_complete: &Arc<AtomicBool>,
) -> Option<String> {
    use super::protocol::{JsonRpcMessage, JsonRpcResponse};
    use crate::cli::mcp::server::{handle_tool_call, list_tools_json, HANDLERS, SERVER_STATE};

    let message = match JsonRpcMessage::from_json(json_payload) {
        Ok(m) => m,
        Err(e) => {
            let error_response = JsonRpcResponse::error(serde_json::Value::Null, e);
            return serde_json::to_string(&error_response).ok();
        }
    };

    match message {
        JsonRpcMessage::Notification(notification) => {
            debug!("Ignoring notification on socket: {}", notification.method);
            None
        }
        JsonRpcMessage::Request(request) => {
            let request_id = request.id.clone().unwrap_or(serde_json::Value::Null);
            let method_name = request.method.clone();

            // Notifications with null id must not receive a response
            if request.id.is_none() {
                debug!("Ignoring notification: {}", method_name);
                return None;
            }

            let state = match SERVER_STATE.get() {
                Some(s) => s,
                None => {
                    let resp = JsonRpcResponse::error(
                        request_id,
                        super::protocol::JsonRpcError::new(-32603, "Server state not initialized"),
                    );
                    return serde_json::to_string(&resp).ok();
                }
            };

            let handlers = match HANDLERS.get() {
                Some(h) => h,
                None => {
                    let resp = JsonRpcResponse::error(
                        request_id,
                        super::protocol::JsonRpcError::new(-32603, "Handlers not initialized"),
                    );
                    return serde_json::to_string(&resp).ok();
                }
            };

            // Check handshake state for this session (allow initialize and ping before handshake)
            if method_name != "initialize" && method_name != "ping" {
                let handshaked =
                    if let Some(mut entry) = session_handshakes.get_mut(session_id) {
                        // Update last access time to prevent eviction
                        entry.1 = Instant::now();
                        entry.0
                    } else {
                        false
                    };
                if !handshaked {
                    let resp = JsonRpcResponse::error(
                        request_id,
                        super::protocol::JsonRpcError::new(
                            -32600,
                            "Server not initialized. Call 'initialize' first.",
                        ),
                    );
                    return serde_json::to_string(&resp).ok();
                }
            }

            let response = match method_name.as_str() {
                "initialize" => {
                    // Mark session as handshaked
                    handshake_complete.store(true, Ordering::SeqCst);
                    session_handshakes.insert(session_id.to_string(), (true, Instant::now()));

                    let result = serde_json::json!({
                        "protocolVersion": "2024-11-05",
                        "capabilities": {
                            "tools": { "listChanged": true },
                            "prompts": { "listChanged": true },
                            "resources": { "listChanged": true, "subscribe": false },
                            "logging": {},
                            "progress": true
                        },
                        "serverInfo": {
                            "name": "leindex",
                            "version": env!("CARGO_PKG_VERSION"),
                            "description": "LeIndex MCP Server - Semantic code indexing and analysis with PDG-based tools"
                        }
                    });
                    JsonRpcResponse::success(request_id, result)
                }
                "ping" => JsonRpcResponse::success(request_id, serde_json::json!({})),
                "tools/call" => {
                    let result = handle_tool_call(state, handlers, &request).await;
                    JsonRpcResponse::from_result(request_id, result)
                }
                "tools/list" => JsonRpcResponse::success(request_id, list_tools_json(handlers)),
                "prompts/list" => JsonRpcResponse::success(request_id, list_prompts_json()),
                "prompts/get" => {
                    let result = handle_prompt_get(&request);
                    match result {
                        Ok(value) => JsonRpcResponse::success(request_id, value),
                        Err(e) => JsonRpcResponse::error(request_id, e),
                    }
                }
                "resources/list" => JsonRpcResponse::success(request_id, list_resources_json()),
                "resources/read" => {
                    let result = handle_resource_read(&request);
                    match result {
                        Ok(value) => JsonRpcResponse::success(request_id, value),
                        Err(e) => JsonRpcResponse::error(request_id, e),
                    }
                }
                _ => JsonRpcResponse::error(
                    request_id,
                    super::protocol::JsonRpcError::method_not_found(method_name),
                ),
            };

            serde_json::to_string(&response).ok()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize any test that mutates process-global environment
    /// variables. `std::env::set_var` is not thread-safe and `cargo
    /// test` runs tests in parallel by default — the
    /// `test_max_http_sessions_env_override` test below reads/writes
    /// `LEINDEX_MAX_SESSIONS` while other tests concurrently call
    /// `max_http_sessions()` (which reads the same variable), which
    /// is an active data race under POSIX. Holding this mutex for the
    /// entire test body guarantees only one test at a time touches
    /// the env.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_server_config_default() {
        let config = McpServerConfig::default();
        assert_eq!(
            config.bind_address,
            SocketAddr::from(([127, 0, 0, 1], DEFAULT_MCP_PORT))
        );
        // Loopback-only binding on a high, rarely-used port. We avoid the
        // well-known dev-server range (<10000) which collides with Node,
        // Rails, Django, etc. 47500 sits well above that and below the
        // IANA dynamic range (49152+) so it's reliably available.
        assert!(config.bind_address.ip().is_loopback());
        assert!(config.bind_address.port() >= 10000);
    }

    #[cfg(unix)]
    #[test]
    fn test_socket_cleanup_guard_removes_file() {
        let dir = std::env::temp_dir().join("leindex_test_socket_guard");
        std::fs::create_dir_all(&dir).unwrap();
        let socket_path = dir.join("test.sock");
        std::fs::write(&socket_path, b"").unwrap();
        assert!(socket_path.exists());

        {
            let _guard = SocketCleanupGuard {
                path: socket_path.clone(),
            };
        }
        assert!(!socket_path.exists());

        let _ = std::fs::remove_dir_all(&dir);
    }

    // ---- A+ MCP session cleanup tests (VAL-APLUS-025, VAL-APLUS-026) ----

    /// VAL-APLUS-025: MCP session handshake handling preserves behavior under
    /// concurrent sessions. Multiple sessions can be initialized and tracked
    /// independently without corrupting each other.
    #[test]
    fn test_concurrent_session_handshake_isolation() {
        let registry = Arc::new(ProjectRegistry::new(5));
        let server = McpServer {
            config: McpServerConfig::default(),
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
        };

        // Simulate multiple concurrent session handshakes
        let (result1, sid1) = handle_initialize_for_test(&server);
        let (result2, sid2) = handle_initialize_for_test(&server);
        let (result3, sid3) = handle_initialize_for_test(&server);

        // All should succeed with unique session IDs
        assert!(result1.get("protocolVersion").is_some());
        assert!(result2.get("protocolVersion").is_some());
        assert!(result3.get("protocolVersion").is_some());

        // Session IDs must be unique
        assert_ne!(sid1, sid2);
        assert_ne!(sid2, sid3);
        assert_ne!(sid1, sid3);

        // All sessions should be tracked
        assert_eq!(server.active_session_count(), 3);

        // All sessions should be marked as handshaked
        {
            assert!(server.session_handshakes.get(&sid1).unwrap().0);
            assert!(server.session_handshakes.get(&sid2).unwrap().0);
            assert!(server.session_handshakes.get(&sid3).unwrap().0);
        }
    }

    /// VAL-APLUS-026: MCP session tracking remains isolated per session.
    /// Operations on one session do not corrupt or block unrelated session state.
    #[test]
    fn test_session_isolation_per_session() {
        let registry = Arc::new(ProjectRegistry::new(5));
        let server = McpServer {
            config: McpServerConfig::default(),
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
        };

        let (_, sid1) = handle_initialize_for_test(&server);
        let (_, sid2) = handle_initialize_for_test(&server);

        // Remove session 1
        server.session_handshakes.remove(&sid1);

        // Session 2 should still be valid
        assert!(server.session_handshakes.get(&sid2).is_some());
        assert!(server.session_handshakes.get(&sid2).unwrap().0);

        assert_eq!(server.active_session_count(), 1);
    }

    /// VAL-APLUS-025 variant: stale session cleanup removes only expired sessions.
    #[test]
    fn test_stale_session_cleanup() {
        let registry = Arc::new(ProjectRegistry::new(5));
        let server = McpServer {
            config: McpServerConfig::default(),
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
        };

        // Create a session
        let (_, sid) = handle_initialize_for_test(&server);
        assert_eq!(server.active_session_count(), 1);

        // Manually age the session's last_access time to simulate staleness
        if let Some(mut entry) = server.session_handshakes.get_mut(&sid) {
            entry.1 = Instant::now() - std::time::Duration::from_secs(600);
        }

        // Cleanup with a 60-second idle timeout should remove the stale session
        let removed = server.cleanup_stale_sessions(std::time::Duration::from_secs(60));
        assert_eq!(removed, 1);
        assert_eq!(server.active_session_count(), 0);
    }

    /// Helper: simulate an initialize call and return (result, session_id).
    fn handle_initialize_for_test(server: &McpServer) -> (Value, String) {
        let (result, sid) = handle_initialize(server);
        (result, sid.unwrap())
    }

    /// Regression test for HIGH #2: when `handle_initialize` is forced to
    /// evict to make room for a new session, it must NOT evict a session
    /// that is currently processing a request. Without the in_flight
    /// filter, a long-running tool call could be evicted mid-request,
    /// producing spurious "Server not initialized" errors for the
    /// active client.
    ///
    /// Test strategy: lower the session cap to 2, register 2 sessions
    /// (one of which is in_flight), and call `handle_initialize` a
    /// third time. The in_flight session must survive; the idle
    /// session must be evicted.
    #[test]
    fn test_handle_initialize_does_not_evict_in_flight_session() {
        // Override the cap for this test. We can't override
        // `DEFAULT_MAX_HTTP_SESSIONS` (it's a const) but we can verify
        // the eviction logic by pre-loading 2 sessions and forcing a
        // third call to be on the boundary.
        //
        // To exercise the eviction code path directly we use a slightly
        // higher load than the cap: register MAX sessions, then call
        // initialize once more and check the in_flight session is kept.
        let registry = Arc::new(ProjectRegistry::new(5));
        let server = McpServer {
            config: McpServerConfig::default(),
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
        };

        // Fill the session map to the cap. We use `insert` directly
        // because `handle_initialize` generates its own session IDs and
        // we want to control which ones are "old" and "in flight".
        let now = Instant::now();
        for i in 0..DEFAULT_MAX_HTTP_SESSIONS {
            let sid = format!("sess-{i:04}");
            server
                .session_handshakes
                .insert(sid.clone(), (true, now));
        }
        assert_eq!(server.active_session_count(), DEFAULT_MAX_HTTP_SESSIONS);

        // Mark the very first session (oldest by insertion order) as
        // in-flight. `last_access_time` is the same as the others, so
        // without the in_flight filter it would be evicted.
        let in_flight_sid = "sess-0000".to_string();
        server.begin_request(&in_flight_sid);
        assert!(server.session_in_flight(&in_flight_sid));

        // The next initialize call should trigger eviction logic.
        let (_, new_sid) = handle_initialize_for_test(&server);

        // The in-flight session must still be present.
        assert!(
            server.session_handshakes.contains_key(&in_flight_sid),
            "in_flight session {} was evicted during initialize",
            in_flight_sid,
        );
        // The new session must be present.
        assert!(
            server.session_handshakes.contains_key(&new_sid),
            "newly initialized session {} was not registered",
            new_sid,
        );
        // The total count is at most MAX + 1 (the new session, since
        // the in_flight one was preserved and exactly one other was
        // evicted).
        assert!(
            server.active_session_count() <= DEFAULT_MAX_HTTP_SESSIONS + 1,
            "session count {} exceeds cap + 1",
            server.active_session_count(),
        );

        // Cleanup: end the request so the in_flight map is empty for
        // other tests in the same process.
        server.end_request(&in_flight_sid);
    }

    /// `LEINDEX_MAX_SESSIONS` env var overrides the default session cap.
    #[test]
    fn test_max_http_sessions_env_override() {
        // Hold the env-mutation lock for the entire body. Other tests in
        // this module (e.g. the eviction test) call `handle_initialize`,
        // which reads `LEINDEX_MAX_SESSIONS` via `max_http_sessions()`;
        // racing `std::env::set_var` against a concurrent
        // `std::env::var` is undefined behaviour. Serialising on
        // `ENV_LOCK` keeps every env read/write in this test entirely
        // single-threaded.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // Default (env unset) returns the default.
        unsafe {
            std::env::set_var(MAX_SESSIONS_ENV, "42");
        }
        assert_eq!(max_http_sessions(), 42);
        unsafe {
            std::env::remove_var(MAX_SESSIONS_ENV);
        }
        assert_eq!(max_http_sessions(), DEFAULT_MAX_HTTP_SESSIONS);
        // Bogus value falls back to default.
        unsafe {
            std::env::set_var(MAX_SESSIONS_ENV, "not-a-number");
        }
        assert_eq!(max_http_sessions(), DEFAULT_MAX_HTTP_SESSIONS);
        unsafe {
            std::env::remove_var(MAX_SESSIONS_ENV);
        }
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
                    text: "Welcome to LeIndex! Here's how to get started:\n\n1. **Indexing**: First, index your project with `leindex.index`\n2. **Searching**: Use `leindex.search` for semantic code search\n3. **Analysis**: Use `leindex.deep-analyze` for comprehensive code analysis\n4. **Context**: Use `leindex.context` to expand around specific symbols\n\nPro tip: LeIndex auto-indexes on first use, so you can start searching immediately!".to_string(),
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
                            "Let me help you investigate: {}\n\nHere's the recommended workflow:\n\n1. **Start broad**: Use `leindex.search` with a natural language query like '{}'\n2. **Find entry points**: Look for the most relevant symbols in the results\n3. **Deep dive**: Use `leindex.deep-analyze` on the most relevant symbol\n4. **Expand context**: Use `leindex.context` to see how the symbol is used\n5. **Navigate**: Follow symbol references with `leindex.read-symbol`\n\nWould you like me to help you with any specific step?",
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
  "name": "leindex.index",
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
  "name": "leindex.search",
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
  "name": "leindex.deep-analyze",
  "arguments": {
    "query": "User::authenticate"
  }
}
```

## Available Tools

- `leindex.search` - Semantic code search
- `leindex.deep-analyze` - Comprehensive code analysis
- `leindex.context` - Expand symbol context
- `leindex.grep-symbols` - Search symbols by name
- `leindex.read-file` - Read file with PDG annotations
- `leindex.file-summary` - Get file structural summary

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
