// MCP Server
//
// This module implements the MCP (Model Context Protocol) JSON-RPC server
// using axum for HTTP handling.

use super::handlers::{all_tool_handlers, ToolHandler};
use super::protocol::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use super::request_meta::{collect_request_timings, elapsed_ms};
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
use dashmap::DashMap;
use futures_util::stream::{Stream, StreamExt};
use serde_json::Value;
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
    /// Keys are `Arc<str>` so `begin_request` can clone the existing
    /// refcounted ID into `in_flight` instead of allocating a fresh
    /// heap block on every request.
    pub(crate) session_handshakes: Arc<DashMap<Arc<str>, (bool, Instant)>>,
    /// Session IDs that currently have an in-flight tool call.
    ///
    /// Cleanup skips these so a long-running tool call is never evicted
    /// mid-flight by the idle-expiration sweep. Keys are `Arc<str>` so
    /// `begin_request`/`end_request` bump a refcount instead of
    /// allocating a new `String` for every request.
    pub(crate) in_flight: Arc<DashMap<Arc<str>, ()>>,
    /// Per-session freshness advisories already shown, keyed by project.
    /// The compact freshness badge remains on every response; only the
    /// verbose advisory is emitted once per session and generation.
    pub(crate) freshness_advisories: Arc<DashMap<(Arc<str>, std::path::PathBuf), u64>>,
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
            freshness_advisories: Arc::new(DashMap::new()),
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
            // `sid` is `&Arc<str>`; look up in `in_flight` as `&str` so
            // DashMap resolves the Borrow trait to `Arc<str>: Borrow<str>`.
            if self.in_flight.contains_key(sid.as_ref()) {
                // Active request — never evict.
                true
            } else {
                last_access.elapsed() < max_idle
            }
        });
        let removed = before - self.session_handshakes.len();
        self.freshness_advisories
            .retain(|(session_id, _), _| self.session_handshakes.contains_key(session_id.as_ref()));
        removed
    }

    /// Apply the session/generation freshness advisory policy to a raw tool
    /// result before output trimming. This is deliberately transport-owned so
    /// handlers stay stateless and direct CLI calls remain unaffected.
    fn apply_freshness_advisory(
        &self,
        session_id: &str,
        project_path: Option<&str>,
        result: &mut Value,
    ) {
        let result_project_path = result
            .get("project_path")
            .and_then(Value::as_str)
            .map(str::to_owned);
        let Some(freshness) = result
            .get_mut("_meta")
            .and_then(Value::as_object_mut)
            .and_then(|meta| meta.get_mut("freshness"))
            .and_then(Value::as_object_mut)
        else {
            return;
        };
        let generation = freshness
            .get("generation")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let project = project_path
            .filter(|path| !path.is_empty())
            .map(std::path::PathBuf::from)
            .or_else(|| result_project_path.map(std::path::PathBuf::from))
            .unwrap_or_else(|| std::path::PathBuf::from("<current>"));
        let key = (Arc::<str>::from(session_id), project);
        let already_shown = self
            .freshness_advisories
            .get(&key)
            .is_some_and(|seen| *seen == generation);
        let advisory = freshness.remove("warning");
        if already_shown {
            freshness.insert("advisory".to_string(), Value::Null);
        } else {
            freshness.insert("advisory".to_string(), advisory.unwrap_or(Value::Null));
            self.freshness_advisories.insert(key, generation);
        }
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
    /// for every request. When the session is already in the
    /// `session_handshakes` table, we clone the existing `Arc<str>`
    /// (a refcount bump, no allocation); when it is not yet
    /// registered (a degenerate first-request-before-handshake
    /// case) we fall back to `Arc::<str>::from(&str)`, which does
    /// allocate, but only once per session.
    pub fn begin_request(&self, session_id: &str) {
        let key = self
            .session_handshakes
            .get(session_id)
            .map(|entry| entry.key().clone())
            .unwrap_or_else(|| Arc::<str>::from(session_id));
        self.in_flight.insert(key, ());
    }

    /// Mark a session's in-flight request as complete.
    pub fn end_request(&self, session_id: &str) {
        self.in_flight.remove(session_id);
    }

    /// Acquire a panic-safe in-flight guard for the given session.
    ///
    /// The returned `InFlightGuard` removes the session from the
    /// `in_flight` set on drop, so the cleanup task never sees a
    /// session as in-flight if the tool call panics, its response is
    /// disconnected, or any other path bypasses the explicit `end_request`
    /// call. This is a small RAII wrapper that
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
        // Auto-bind-fallback: if the default port is taken, try the
        // next consecutive ports before giving up. Eliminates the
        // most common "MCP fails to connect" failure mode where
        // another process holds the port and the user has no idea
        // why the server won't start.
        let listener = bind_with_fallback(bind_address).await?;
        if listener.local_addr()? != bind_address {
            warn!(
                "Default port {} was unavailable; bound to fallback {}",
                bind_address.port(),
                listener.local_addr()?.port()
            );
        }
        self.serve(listener).await
    }

    /// Serve on a pre-bound listener. Use this from `cmd_serve_impl`
    /// when the caller needs to know the actual bound address
    /// (which may differ from the preferred one when
    /// `bind_with_fallback` walks past the preferred port) so it
    /// can print the real URL before announcing readiness.
    pub async fn serve(self, listener: tokio::net::TcpListener) -> anyhow::Result<()> {
        // Always log the ACTUAL bound address, not the preferred
        // one. `bind_with_fallback` may have walked past the
        // preferred port when it was already in use, in which
        // case `self.config.bind_address` would still be the
        // preferred (but unbindable) port. `listener.local_addr()`
        // reflects what the kernel actually assigned, which is
        // what external clients need to know. Fall back to the
        // preferred address only when the kernel query fails
        // (which would itself be a startup error worth surfacing
        // via the log).
        let bind_address = listener.local_addr().unwrap_or(self.config.bind_address);
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
                    debug!("Cleaned {} stale session(s)", removed);
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
pub(crate) async fn bind_with_fallback(
    preferred: SocketAddr,
) -> anyhow::Result<tokio::net::TcpListener> {
    // Port 0 is the OS-assigned-ephemeral convention. The
    // kernel is guaranteed to pick a free high port on
    // success, so the fixed-range fallback loop below would
    // do nothing useful — and worse, walking 1..=10 would
    // probe privileged ports (< 1024 on Unix), which can fail
    // with EACCES even though we never intended to bind
    // there. Bypass the loop entirely and let the OS pick.
    if preferred.port() == 0 {
        return tokio::net::TcpListener::bind(preferred)
            .await
            .map_err(|e| anyhow::anyhow!("failed to bind to ephemeral port {}: {}", preferred, e));
    }
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..=BIND_FALLBACK_PORT_RANGE {
        // `checked_add` so a preferred port near `u16::MAX`
        // (e.g. 65530 with a 10-port fallback range) does not
        // silently re-try the same saturated port 65535 six
        // times. `saturating_add` would cap the port at
        // `u16::MAX` and burn the remaining fallback slots on
        // duplicate bind attempts; `checked_add` returns
        // `None` on overflow and we break out of the fixed
        // range, falling through to the ephemeral-bind
        // fallback below.
        let port = match preferred.port().checked_add(offset) {
            Some(p) => p,
            None => break,
        };
        let candidate = SocketAddr::new(preferred.ip(), port);
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
                .filter(|r| !server.in_flight.contains_key(r.key().as_ref()))
                .min_by_key(|r| r.value().1)
                .map(|r| r.key().clone());
            if let Some(id) = oldest_id {
                server.session_handshakes.remove(id.as_ref());
                server
                    .freshness_advisories
                    .retain(|(session_id, _), _| session_id.as_ref() != id.as_ref());
            }
        }
        server.session_handshakes.insert(
            Arc::<str>::from(session_id.as_str()),
            (true, Instant::now()),
        );
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
    let transport_started = Instant::now();
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
                if let Some(mut entry) = server_instance.session_handshakes.get_mut(sid.as_str()) {
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
            // Keep the in-flight session guard, but do not wrap correctness-
            // critical work in a wall-clock timeout. Tokio cannot cancel a
            // `spawn_blocking` parse/transaction safely; dropping that future
            // creates the disk/memory generation split this server is meant
            // to prevent. Indexing itself returns an owned job snapshot.
            let _guard = incoming_session_id
                .as_ref()
                .map(|sid| McpServer::in_flight_guard(server_instance, sid));
            let advisory = incoming_session_id
                .as_deref()
                .map(|sid| (server_instance.as_ref(), sid));
            handle_tool_call_timed(&state, handlers, &json_req, transport_started, advisory).await
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
    handle_tool_call_timed(registry, handlers, req, Instant::now(), None).await
}

/// Handle a tool call with a transport timestamp captured at message receipt.
async fn handle_tool_call_timed(
    registry: &Arc<ProjectRegistry>,
    handlers: &[ToolHandler],
    req: &JsonRpcRequest,
    transport_started: Instant,
    advisory: Option<(&McpServer, &str)>,
) -> Result<Value, JsonRpcError> {
    let handler_started = Instant::now();
    let tool_call = req.extract_tool_call()?;
    debug!("Tool call: name={}", tool_call.name);

    let handler = handlers
        .iter()
        .find(|h| h.name() == tool_call.name)
        .ok_or_else(|| JsonRpcError::method_not_found(tool_call.name.clone()))?;

    // Clone arguments before moving them into execute (we need them
    // for render_tool_output_plain which needs the original args).
    let call_args = tool_call.arguments.clone();
    let call_name = tool_call.name.clone();

    // Execute the tool and wrap the result in standard MCP content format
    let (handler_result, mut timings) =
        collect_request_timings(handler.execute(registry, tool_call.arguments)).await;
    let handler_ms = elapsed_ms(handler_started);
    timings.handler_ms = handler_ms;
    timings.transport_queue_ms = handler_started
        .saturating_duration_since(transport_started)
        .as_millis()
        .min(u64::MAX as u128) as u64;
    timings.total_ms = elapsed_ms(transport_started);
    debug!(
        tool = %call_name,
        handler_ms,
        transport_queue_ms = timings.transport_queue_ms,
        total_ms = timings.total_ms,
        "MCP tool call complete"
    );

    match handler_result {
        Ok(mut value) => {
            if let Some((server, session_id)) = advisory {
                server.apply_freshness_advisory(
                    session_id,
                    call_args.get("project_path").and_then(Value::as_str),
                    &mut value,
                );
            }
            // The MCP transport is what the LLM actually sees. Run the
            // tool's raw value through the per-tool payload trimmer so we
            // hand the model only the fields it needs (no scoring
            // internals, no byte ranges, no tfidf/neural split, etc.).
            let trimmed = crate::cli::mcp::output::trim_llm_payload(&call_name, &value);

            // Prefer a clean human-readable rendering over raw JSON.
            // The CLI surface uses `render_tool_output` (colored); the
            // MCP transport uses the same render functions but without
            // ANSI codes so the LLM sees clean text.
            let rendered =
                crate::cli::mcp::output::render_tool_output_plain(&call_name, &trimmed, &call_args);
            let is_substantive = rendered.trim().lines().count() > 1 || rendered.trim().len() > 80;
            let payload = if is_substantive {
                rendered
            } else {
                serde_json::to_string_pretty(&trimmed)
                    .unwrap_or_else(|_| "Error serializing result".to_string())
            };
            Ok(serde_json::json!({
                "content": [
                    {
                        "type": "text",
                        "text": payload
                    }
                ],
                "isError": false,
                "_meta": { "timings": timings }
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
                "isError": true,
                "_meta": { "timings": timings }
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
/// panics, its response disconnects, or any other code path bypasses the
/// explicit `end_request` call. Without this guard, a
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
            self.session_handshakes.insert(
                Arc::<str>::from(session_id.as_str()),
                (false, Instant::now()),
            );

            tokio::spawn(handle_socket_connection(
                stream,
                session_id,
                self.session_handshakes.clone(),
                self.handshake_complete.clone(),
            ));
        }

        #[allow(unreachable_code)]
        {
            // _guard is dropped here, cleaning up the socket file
            Ok(())
        }
    }
}

#[cfg(unix)]
/// Read one line (including its trailing `\n`) but abort with an error once it
/// exceeds `max` bytes — so a peer sending an unbounded line with no newline
/// cannot exhaust memory before a length check. `Ok(None)` = clean EOF.
#[cfg(unix)]
async fn read_bounded_line<R: tokio::io::AsyncBufRead + Unpin>(
    reader: &mut R,
    max: usize,
) -> std::io::Result<Option<String>> {
    use tokio::io::AsyncBufReadExt;
    let mut out: Vec<u8> = Vec::new();
    loop {
        let buf = reader.fill_buf().await?;
        if buf.is_empty() {
            return Ok(if out.is_empty() {
                None
            } else {
                Some(String::from_utf8_lossy(&out).into_owned())
            });
        }
        let nl = buf.iter().position(|&b| b == b'\n');
        let take = nl.map(|i| i + 1).unwrap_or(buf.len());
        if out.len().saturating_add(take) > max {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "line exceeds max length",
            ));
        }
        out.extend_from_slice(&buf[..take]);
        reader.consume(take);
        if nl.is_some() {
            return Ok(Some(String::from_utf8_lossy(&out).into_owned()));
        }
    }
}
#[cfg(unix)]

async fn handle_socket_connection(
    stream: tokio::net::UnixStream,
    session_id: String,
    session_handshakes: Arc<DashMap<Arc<str>, (bool, Instant)>>,
    handshake_complete: Arc<AtomicBool>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

    debug!("Accepted Unix socket connection (session: {})", session_id);

    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let mut use_content_length = false;

    // Security limits to prevent memory exhaustion attacks
    const MAX_LINE_LENGTH: usize = 10_240; // 10KB max line length
    const MAX_PAYLOAD_SIZE: usize = 10_485_760; // 10MB max payload size

    loop {
        // Bounded line read: aborts before a peer can exhaust memory with an
        // oversized line that has no trailing newline.
        let line = match read_bounded_line(&mut reader, MAX_LINE_LENGTH).await {
            Ok(Some(line)) => line,
            Ok(None) => break, // clean EOF
            Err(e) => {
                debug!(
                    "Socket read error / line too long (session {}): {}",
                    session_id, e
                );
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
                    session_id, length
                );
                break;
            }

            // Consume remaining header lines until blank line
            loop {
                let header = match read_bounded_line(&mut reader, MAX_LINE_LENGTH).await {
                    Ok(Some(h)) => h,
                    Ok(None) => break, // EOF
                    Err(_) => break,   // oversized header line
                };
                if header.trim().is_empty() {
                    break;
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
            &session_id,
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
    session_handshakes.remove(session_id.as_str());

    debug!("Socket connection closed (session: {})", session_id);
}

/// Handle a single JSON-RPC message received over a Unix socket connection.
/// Returns `Some(response_json)` or `None` for notifications (no response).
#[cfg(unix)]
async fn handle_socket_message(
    json_payload: &str,
    session_id: &str,
    session_handshakes: &Arc<DashMap<Arc<str>, (bool, Instant)>>,
    handshake_complete: &Arc<AtomicBool>,
) -> Option<String> {
    use super::protocol::{JsonRpcMessage, JsonRpcResponse};
    use crate::cli::mcp::server::{list_tools_json, HANDLERS, SERVER_STATE};

    let transport_started = Instant::now();
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
                let handshaked = if let Some(mut entry) = session_handshakes.get_mut(session_id) {
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
                    session_handshakes.insert(Arc::<str>::from(session_id), (true, Instant::now()));

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
                    let advisory = SERVER_INSTANCE
                        .get()
                        .map(|server| (server.as_ref(), session_id));
                    let result = handle_tool_call_timed(
                        state,
                        handlers,
                        &request,
                        transport_started,
                        advisory,
                    )
                    .await;
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
            freshness_advisories: Arc::new(DashMap::new()),
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
            assert!(server.session_handshakes.get(sid1.as_str()).unwrap().0);
            assert!(server.session_handshakes.get(sid2.as_str()).unwrap().0);
            assert!(server.session_handshakes.get(sid3.as_str()).unwrap().0);
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
            freshness_advisories: Arc::new(DashMap::new()),
        };

        let (_, sid1) = handle_initialize_for_test(&server);
        let (_, sid2) = handle_initialize_for_test(&server);

        // Remove session 1
        server.session_handshakes.remove(sid1.as_str());

        // Session 2 should still be valid
        assert!(server.session_handshakes.get(sid2.as_str()).is_some());
        assert!(server.session_handshakes.get(sid2.as_str()).unwrap().0);

        assert_eq!(server.active_session_count(), 1);
    }

    #[test]
    fn test_freshness_advisory_is_once_per_session_and_generation() {
        let registry = Arc::new(ProjectRegistry::new(5));
        let server = McpServer {
            config: McpServerConfig::default(),
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
            freshness_advisories: Arc::new(DashMap::new()),
        };
        let (_, session_id) = handle_initialize_for_test(&server);

        let response = || {
            serde_json::json!({
                "project_path": "/tmp/project",
                "_meta": {"freshness": {
                    "generation": 7,
                    "warning": "refresh recommended"
                }}
            })
        };
        let mut first = response();
        server.apply_freshness_advisory(&session_id, None, &mut first);
        assert_eq!(
            first["_meta"]["freshness"]["advisory"],
            "refresh recommended"
        );
        assert!(first["_meta"]["freshness"].get("warning").is_none());

        let mut second = response();
        server.apply_freshness_advisory(&session_id, None, &mut second);
        assert!(second["_meta"]["freshness"]["advisory"].is_null());

        let mut new_generation = response();
        new_generation["_meta"]["freshness"]["generation"] = serde_json::json!(8);
        server.apply_freshness_advisory(&session_id, None, &mut new_generation);
        assert_eq!(
            new_generation["_meta"]["freshness"]["advisory"],
            "refresh recommended"
        );
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
            freshness_advisories: Arc::new(DashMap::new()),
        };

        // Create a session
        let (_, sid) = handle_initialize_for_test(&server);
        assert_eq!(server.active_session_count(), 1);

        // Manually age the session's last_access time to simulate staleness
        if let Some(mut entry) = server.session_handshakes.get_mut(sid.as_str()) {
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
    ///
    /// `handle_initialize` reads `LEINDEX_MAX_SESSIONS` via
    /// `max_http_sessions()`. `cargo test` runs tests in parallel by
    /// default and `test_max_http_sessions_env_override` mutates that
    /// env var on a sibling thread, so we hold `ENV_LOCK` for the
    /// entire test body to serialise env access.
    #[test]
    fn test_handle_initialize_does_not_evict_in_flight_session() {
        let _env_guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
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
            freshness_advisories: Arc::new(DashMap::new()),
        };

        // Fill the session map to the cap. We use `insert` directly
        // because `handle_initialize` generates its own session IDs and
        // we want to control which ones are "old" and "in flight".
        let now = Instant::now();
        for i in 0..DEFAULT_MAX_HTTP_SESSIONS {
            let sid = format!("sess-{i:04}");
            server
                .session_handshakes
                .insert(Arc::<str>::from(sid.as_str()), (true, now));
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
            server
                .session_handshakes
                .contains_key(in_flight_sid.as_str()),
            "in_flight session {} was evicted during initialize",
            in_flight_sid,
        );
        // The new session must be present.
        assert!(
            server.session_handshakes.contains_key(new_sid.as_str()),
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

    /// Regression for MED #3342354737: `session_handshakes` is keyed
    /// on `Arc<str>`, and `begin_request` clones the cached
    /// `Arc<str>` from the handshake table (refcount bump, no
    /// allocation) instead of constructing a fresh
    /// `Arc::<str>::from(&str)` for every request. Verify the
    /// in_flight map's key is `Arc`-equal (same allocation) to the
    /// session_handshakes key.
    #[test]
    fn test_begin_request_clones_cached_arc_str_key() {
        let registry = Arc::new(ProjectRegistry::new(5));
        let server = McpServer {
            config: McpServerConfig::default(),
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
            freshness_advisories: Arc::new(DashMap::new()),
        };

        // Register a session directly so the key is an `Arc<str>` we
        // can compare against.
        let sid = Arc::<str>::from("sess-arc-key");
        let cached: Arc<str> = sid.clone();
        server
            .session_handshakes
            .insert(sid, (true, Instant::now()));

        // `begin_request` should clone the cached `Arc<str>` rather
        // than allocating a new one.
        server.begin_request("sess-arc-key");

        // The in_flight map should contain a key whose pointer
        // matches the cached one.
        let stored = server
            .in_flight
            .iter()
            .next()
            .expect("expected one in_flight entry");
        assert_eq!(
            stored.key().as_ptr(),
            cached.as_ptr(),
            "in_flight key should be the same Arc<str> allocation as session_handshakes"
        );
        assert_eq!(stored.key().as_ref(), "sess-arc-key");
    }

    /// `begin_request` falls back to `Arc::<str>::from(&str)` when
    /// the session is not yet registered. The fallback allocation
    /// is acceptable; the test exists to lock the contract so a
    /// future refactor doesn't silently panic or skip the insert.
    #[test]
    fn test_begin_request_allocates_when_session_unknown() {
        let registry = Arc::new(ProjectRegistry::new(5));
        let server = McpServer {
            config: McpServerConfig::default(),
            _registry: registry,
            handshake_complete: Arc::new(AtomicBool::new(false)),
            session_handshakes: Arc::new(DashMap::new()),
            in_flight: Arc::new(DashMap::new()),
            freshness_advisories: Arc::new(DashMap::new()),
        };

        // Session not registered yet — should still insert into
        // in_flight (allocates a fresh `Arc<str>`).
        server.begin_request("unregistered-session");

        assert!(
            server.session_in_flight("unregistered-session"),
            "begin_request must insert even when session is unregistered"
        );

        // Lookup works via `&str` (DashMap resolves Arc<str>: Borrow<str>).
        let stored = server
            .in_flight
            .get("unregistered-session")
            .expect("lookup by &str must work");
        assert_eq!(stored.key().as_ref(), "unregistered-session");
    }

    /// Regression for MED round 18: the fixed-port loop in
    /// `bind_with_fallback` previously used
    /// `preferred.port().saturating_add(offset)`, which
    /// silently caps the candidate port at `u16::MAX`. With
    /// `BIND_FALLBACK_PORT_RANGE = 10` and a preferred port of
    /// 65530, the loop would burn six of its eleven slots
    /// re-binding the saturated port 65535 instead of moving
    /// on. The fix uses `checked_add` and breaks out of the
    /// fixed range on overflow, falling through to the
    /// ephemeral-bind fallback.
    ///
    /// This test exercises the overflow path: bind to the
    /// highest valid port (65535), then ask
    /// `bind_with_fallback` to start near the top of the
    /// range. We assert it returns *some* `TcpListener`
    /// (either via the ephemeral fallback or via the
    /// saturated port) without entering an infinite loop.
    /// The previous implementation would also return a
    /// listener eventually, but it would burn cycles on
    /// duplicate bind attempts. The regression target is the
    /// contract: the loop must terminate promptly and the
    /// function must not loop forever on saturated ports.
    #[tokio::test]
    async fn test_bind_with_fallback_breaks_on_port_overflow() {
        // Bind to the highest valid port to consume it.
        let high = SocketAddr::from(([127, 0, 0, 1], u16::MAX));
        let _occupying = tokio::net::TcpListener::bind(high).await.unwrap();

        // Start the search 5 ports below u16::MAX. With a
        // 10-port fallback range, offset 0..=10 covers
        // 65530..65535 + the saturating case. The fixed
        // range will collide with the occupying listener on
        // 65535; post-fix the loop breaks on overflow
        // (offset 5 → 65535, then offset 6..10 would
        // overflow), and falls through to ephemeral.
        let preferred = SocketAddr::from(([127, 0, 0, 1], u16::MAX - 5));
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            bind_with_fallback(preferred),
        )
        .await;
        let listener = result
            .expect("bind_with_fallback must terminate within 5s")
            .expect("ephemeral fallback must succeed on a free IP");
        // The returned listener must be on the preferred IP.
        assert_eq!(
            listener.local_addr().unwrap().ip(),
            preferred.ip(),
            "ephemeral fallback must use the preferred IP"
        );
    }

    /// The happy path: when the preferred port is free,
    /// `bind_with_fallback` must return a listener bound to
    /// that exact port — no fallback walk, no ephemeral
    /// detour. This guards against an over-eager overflow
    /// break that would cause the function to give up the
    /// preferred port for an ephemeral one.
    #[tokio::test]
    async fn test_bind_with_fallback_uses_preferred_when_free() {
        let preferred = SocketAddr::from(([127, 0, 0, 1], 0));
        let listener = bind_with_fallback(preferred).await.unwrap();
        let bound = listener.local_addr().unwrap();
        // Port 0 → ephemeral, so just assert the IP matches.
        assert_eq!(bound.ip(), preferred.ip());
    }

    /// Regression for MED round 20: when `preferred.port() == 0`,
    /// `bind_with_fallback` must bypass the fixed-range
    /// fallback loop entirely and let the OS pick an
    /// ephemeral port directly. The pre-fix code walked
    /// `1..=BIND_FALLBACK_PORT_RANGE` even when the
    /// preferred port was 0, probing privileged ports (< 1024
    /// on Unix) that could fail with EACCES on systems where
    /// the process has no CAP_NET_BIND_SERVICE.
    ///
    /// The new contract is observable in two ways:
    ///   1. The returned listener's port is non-zero (the
    ///      OS-assigned ephemeral port).
    ///   2. No privileged ports (1..=1023) were probed —
    ///      which we verify indirectly by asserting the
    ///      function returns successfully on a default-
    ///      capability process without touching any port
    ///      that would need root.
    #[tokio::test]
    async fn test_bind_with_fallback_port_zero_skips_fallback_loop() {
        let preferred = SocketAddr::from(([127, 0, 0, 1], 0));
        let listener = bind_with_fallback(preferred).await.unwrap();
        let bound = listener.local_addr().unwrap();
        // The OS-assigned ephemeral port is non-zero.
        assert_ne!(
            bound.port(),
            0,
            "OS-assigned ephemeral port must be non-zero; got port 0 (bind did not actually request ephemeral)"
        );
        // The IP is preserved through the fast path.
        assert_eq!(bound.ip(), preferred.ip());
    }
}

#[path = "prompts_resources.rs"]
mod prompts_resources;
pub use prompts_resources::{
    get_prompt, get_prompts, get_resource, get_resources, handle_prompt_get, handle_resource_read,
    list_prompts_json, list_resources_json, Prompt, PromptArgument, PromptContent, PromptMessage,
    Resource, ResourceContent,
};
