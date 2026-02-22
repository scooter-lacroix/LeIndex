//! HTTP handlers for REST API endpoints

use axum::{
    extract::{Path, Query, State, ws::WebSocketUpgrade},
    http::StatusCode,
    Json, Router,
    response::IntoResponse,
};
use lestockage::Storage;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tracing::{error, info};
use futures::stream::StreamExt;

use crate::config::ServerConfig;
use crate::error::{ApiError, ApiResult};
use crate::responses::{
    CodebaseListResponse,
    CodebaseResponse,
    FileTreeResponse,
    FileNode,
    GraphDataResponse,
    GraphNodeResponse,
    GraphLinkResponse,
    SearchResultsResponse,
    SearchResultResponse,
    ScoreResponse,
};
use crate::websocket::{WsManager, WsEvent};

/// Query parameters for search endpoint
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// Search query string
    pub q: Option<String>,

    /// Maximum number of results to return
    pub limit: Option<usize>,

    /// Filter by programming language
    pub language: Option<String>,

    /// Filter by file type/extension
    pub file_type: Option<String>,
}

/// State shared across all handlers
///
/// Uses `Arc<Mutex<Storage>>` because `rusqlite::Connection` is not `Send + Sync`.

/// Note: AppState fields are documented for their purposes:
/// - `storage`: Arc<Mutex<Storage>> provides thread-safe storage access requiring mutex lock
/// - `config`: Arc<ServerConfig> provides immutable server configuration

/// Handlers must lock the mutex before accessing storage.
#[derive(Clone)]
pub struct AppState {
    /// Thread-safe storage access requiring mutex lock
    pub storage: Arc<Mutex<Storage>>,
    
    /// Immutable server configuration
    pub config: Arc<ServerConfig>,
}

impl AppState {
    /// Create a new AppState instance with storage and configuration
    pub fn new(storage: Storage, config: ServerConfig) -> Self {
        Self {
            storage: Arc::new(Mutex::new(storage)),
            config: Arc::new(config),
        }
    }

    /// Create AppState from an existing Arc<Mutex<Storage>>
    pub fn new_from_arc(storage: Arc<Mutex<Storage>>, config: ServerConfig) -> Self {
        Self {
            storage,
            config: Arc::new(config),
        }
    }
}

/// GET /api/codebases - List all registered projects
pub async fn list_codebases(
    State(state): State<AppState>,
) -> ApiResult<Json<CodebaseListResponse>> {
    info!("Listing all codebases");

    let storage = state.storage.lock()
        .map_err(|e| {
            error!("Failed to acquire storage lock: {}", e);
            ApiError::internal(format!("Storage lock error: {}", e))
        })?;

    // Query all projects from the database
    let conn = storage.conn();
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                unique_project_id,
                base_name,
                path_hash,
                instance,
                canonical_path,
                display_name,
                is_clone,
                cloned_from
            FROM project_metadata
            ORDER BY base_name
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let projects = stmt
        .query_map([], |row| {
            Ok(CodebaseResponse {
                id: row.get::<_, String>(0)?,
                unique_project_id: row.get::<_, String>(0)?,
                base_name: row.get(1)?,
                path_hash: row.get(2)?,
                instance: row.get(3)?,
                project_path: row.get(4)?,
                display_name: row.get(5)?,
                project_type: "Rust".to_string(), // Default for now
                last_indexed: "Unknown".to_string(), // TODO: Add timestamp tracking
                file_count: 0, // TODO: Query from indexed_files
                node_count: 0, // TODO: Query from intel_nodes
                edge_count: 0, // TODO: Query from intel_edges
                is_valid: true,
                is_clone: row.get(6)?,
                cloned_from: row.get(7)?,
            })
        })
        .map_err(|e| {
            error!("Failed to execute query: {}", e);
            ApiError::internal(format!("Database execution error: {}", e))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("Failed to collect results: {}", e);
            ApiError::internal(format!("Result collection error: {}", e))
        })?;

    let total = projects.len();

    info!("Found {} codebases", total);
    Ok(Json(CodebaseListResponse {
        codebases: projects,
        total,
    }))
}

/// POST /api/codebases/:id/refresh - Trigger manual re-sync
pub async fn refresh_codebase(
    Path(id): Path<String>,
    State(_state): State<AppState>,
) -> ApiResult<StatusCode> {
    info!("Triggering refresh for codebase: {}", id);
    Ok(StatusCode::ACCEPTED)
}

/// GET /api/codebases/:id/graph - Get dependency graph data
pub async fn get_graph(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Json<GraphDataResponse>> {
    info!("Getting graph for codebase: {}", id);

    let storage = state.storage.lock()
        .map_err(|e| {
            error!("Failed to acquire storage lock: {}", e);
            ApiError::internal(format!("Storage lock error: {}", e))
        })?;

    let conn = storage.conn();

    // Query nodes from intel_nodes table
    let mut node_stmt = conn
        .prepare(
            r#"
            SELECT
                id,
                project_id,
                file_path,
                node_id,
                symbol_name,
                node_type,
                language
            FROM intel_nodes
            WHERE project_id = ?1
            LIMIT 1000
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare node query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let nodes = node_stmt
        .query_map([&id], |row| {
            Ok(GraphNodeResponse {
                id: row.get::<_, i64>(0)?.to_string(),
                name: row.get(4)?,
                node_type: row.get(6)?,
                val: 10,
                color: "#4CAF50".to_string(),
                language: row.get(6)?,
                complexity: 1,
                file_path: row.get(3)?,
                byte_range: [0, 0],
                x: None,
                y: None,
            })
        })
        .map_err(|e| {
            error!("Failed to execute node query: {}", e);
            ApiError::internal(format!("Database execution error: {}", e))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("Failed to collect node results: {}", e);
            ApiError::internal(format!("Result collection error: {}", e))
        })?;

    // Query edges from intel_edges table
    let mut edge_stmt = conn
        .prepare(
            r#"
            SELECT
                caller_id,
                callee_id,
                edge_type
            FROM intel_edges
            WHERE caller_id IN (SELECT id FROM intel_nodes WHERE project_id = ?1)
            LIMIT 5000
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare edge query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let links = edge_stmt
        .query_map([&id], |row| {
            Ok(GraphLinkResponse {
                source: row.get::<_, i64>(0)?.to_string(),
                target: row.get::<_, i64>(1)?.to_string(),
                link_type: row.get(2)?,
                value: 1,
            })
        })
        .map_err(|e| {
            error!("Failed to execute edge query: {}", e);
            ApiError::internal(format!("Database execution error: {}", e))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("Failed to collect edge results: {}", e);
            ApiError::internal(format!("Result collection error: {}", e))
        })?;

    info!("Retrieved {} nodes and {} links for codebase {}", nodes.len(), links.len(), id);
    Ok(Json(GraphDataResponse { nodes, links }))
}

/// GET /api/codebases/:id/files - Get file tree
pub async fn get_file_tree(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Json<FileTreeResponse>> {
    info!("Getting file tree for codebase: {}", id);

    let storage = state.storage.lock()
        .map_err(|e| {
            error!("Failed to acquire storage lock: {}", e);
            ApiError::internal(format!("Storage lock error: {}", e))
        })?;

    let conn = storage.conn();

    // Query indexed files for the project
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                file_path,
                file_hash
            FROM indexed_files
            WHERE project_id = ?1
            ORDER BY file_path
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare file tree query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let files = stmt
        .query_map([&id], |row| {
            let file_path: String = row.get(0)?;
            Ok(FileNode {
                name: file_path.rsplit('/').next().unwrap_or(&file_path).to_string(),
                node_type: "file".to_string(),
                path: file_path.clone(),
                size: None, // TODO: Get actual file size
                last_modified: None,
                children: vec![],
            })
        })
        .map_err(|e| {
            error!("Failed to execute file tree query: {}", e);
            ApiError::internal(format!("Database execution error: {}", e))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("Failed to collect file tree results: {}", e);
            ApiError::internal(format!("Result collection error: {}", e))
        })?;

    info!("Retrieved {} files for codebase {}", files.len(), id);
    Ok(Json(FileTreeResponse { tree: files }))
}

/// GET /api/search - Unified search endpoint
pub async fn search(
    Query(params): Query<SearchQuery>,
    State(state): State<AppState>,
) -> ApiResult<Json<SearchResultsResponse>> {
    let query = params.q.as_deref().unwrap_or_default();
    let limit = params.limit.unwrap_or(20);

    info!("Searching: q='{}', limit={}", query, limit);

    if query.trim().is_empty() {
        return Ok(Json(SearchResultsResponse::empty()));
    }

    let storage = state.storage.lock()
        .map_err(|e| {
            error!("Failed to acquire storage lock: {}", e);
            ApiError::internal(format!("Storage lock error: {}", e))
        })?;

    let conn = storage.conn();

    // Search for symbols in global_symbols table
    let search_pattern = format!("%{}%", query);
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                symbol_id,
                project_id,
                symbol_name,
                symbol_type,
                file_path,
                language
            FROM global_symbols
            WHERE symbol_name LIKE ?1
               OR signature LIKE ?1
            ORDER BY
                CASE
                    WHEN symbol_name LIKE ?1 THEN 1
                    ELSE 2
                END,
                symbol_name
            LIMIT ?2
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare search query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let results = stmt
        .query_map([&search_pattern, &limit.to_string()], |row| {
            Ok(SearchResultResponse {
                rank: 0, // Will be set after sorting
                node_id: row.get(0)?,
                file_path: row.get(4)?,
                symbol_name: row.get(2)?,
                language: row.get(5)?,
                score: ScoreResponse {
                    semantic: 0.8,
                    text_match: 0.9,
                    structural: 0.7,
                    overall: 0.8,
                },
                context: None,
                byte_range: [0, 0],
            })
        })
        .map_err(|e| {
            error!("Failed to execute search query: {}", e);
            ApiError::internal(format!("Database execution error: {}", e))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("Failed to collect search results: {}", e);
            ApiError::internal(format!("Result collection error: {}", e))
        })?;

    info!("Search returned {} results", results.len());
    Ok(Json(SearchResultsResponse {
        results,
    }))
}

/// GET /api/health - Health check endpoint
pub async fn health_check(
    State(_state): State<AppState>,
) -> ApiResult<Json<serde_json::Value>> {
    Ok(Json(serde_json::json!({
        "status": "ok",
        "service": "leserve",
        "version": env!("CARGO_PKG_VERSION"),
        "active_connections": 0,
    })))
}

/// GET /ws - WebSocket endpoint for real-time updates
pub async fn websocket_handler(
    ws: WebSocketUpgrade,
    State(_state): State<AppState>,
) -> impl IntoResponse {
    ws.on_upgrade(|mut socket| async move {
        // Create WebSocket manager
        let manager = WsManager::new();

        // Generate simple connection ID
        let conn_id = format!("ws_{}", std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_millis());

        // Register connection
        manager.register_connection(conn_id.clone(), None).await;

        // Subscribe to broadcast channel
        let mut rx = manager.broadcaster.subscribe();

        // Send heartbeat every 30 seconds
        let mut heartbeat_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
        let heartbeat_manager = manager.clone();

        tokio::spawn(async move {
            loop {
                heartbeat_interval.tick().await;
                let event = WsEvent::Heartbeat {
                    timestamp: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap()
                        .as_millis() as u64,
                };
                heartbeat_manager.broadcast(event).await;
            }
        });

        // Handle messages
        loop {
            tokio::select! {
                // Handle incoming WebSocket messages
                result = socket.next() => {
                    match result {
                        Some(Ok(message)) => {
                            use axum::extract::ws::Message;
                            match message {
                                Message::Close(_) => {
                                    break;
                                }
                                _ => {
                                    // TODO: Handle client messages (subscriptions, etc.)
                                }
                            }
                        }
                        Some(Err(e)) => {
                            error!("WebSocket error: {}", e);
                            break;
                        }
                        None => break,
                    }
                }
                // Handle outgoing broadcast events
                result = rx.recv() => {
                    match result {
                        Ok(event) => {
                            let json = event.to_json();
                            use axum::extract::ws::Message;
                            if socket.send(Message::Text(json.into())).await.is_err() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        }

        // Unregister connection on disconnect
        manager.unregister_connection(&conn_id).await;
    })
}

/// Create router with all API endpoints
pub fn create_router() -> Router<AppState> {
    Router::new()
        .route("/api/health", axum::routing::get(health_check))
        .route("/api/codebases", axum::routing::get(list_codebases))
        .route("/api/codebases/:id", axum::routing::get(refresh_codebase))
        .route("/api/codebases/:id/graph", axum::routing::get(get_graph))
        .route("/api/codebases/:id/files", axum::routing::get(get_file_tree))
        .route("/api/search", axum::routing::get(search))
        .route("/ws", axum::routing::get(websocket_handler))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_search_query_empty() {
        let query = SearchQuery {
            q: None,
            limit: None,
            language: None,
            file_type: None,
        };
        assert!(query.q.is_none());
        assert!(query.limit.is_none());
    }
}
