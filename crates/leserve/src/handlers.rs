//! HTTP handlers for REST API endpoints

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json, Router,
};
use lestockage::Storage;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tracing::info;

use crate::config::ServerConfig;
use crate::error::ApiResult;
use crate::responses::{
    CodebaseListResponse,
    FileTreeResponse,
    GraphDataResponse,
    SearchResultsResponse,
};

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
    State(_state): State<AppState>,
) -> ApiResult<Json<CodebaseListResponse>> {
    info!("Listing all codebases");
    let response = CodebaseListResponse::empty();
    Ok(Json(response))
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
    State(_state): State<AppState>,
) -> ApiResult<Json<GraphDataResponse>> {
    info!("Getting graph for codebase: {}", id);
    let response = GraphDataResponse {
        nodes: vec![],
        links: vec![],
    };
    Ok(Json(response))
}

/// GET /api/codebases/:id/files - Get file tree
pub async fn get_file_tree(
    Path(id): Path<String>,
    State(_state): State<AppState>,
) -> ApiResult<Json<FileTreeResponse>> {
    info!("Getting file tree for codebase: {}", id);
    let response = FileTreeResponse {
        tree: vec![],
    };
    Ok(Json(response))
}

/// GET /api/search - Unified search endpoint
pub async fn search(
    Query(params): Query<SearchQuery>,
    State(_state): State<AppState>,
) -> ApiResult<Json<SearchResultsResponse>> {
    let query = params.q.as_deref().unwrap_or_default();
    let limit = params.limit.unwrap_or(20);

    info!("Searching: q='{}', limit={}", query, limit);

    if query.trim().is_empty() {
        return Ok(Json(SearchResultsResponse::empty()));
    }

    info!("Search not yet implemented, returning empty results");
    Ok(Json(SearchResultsResponse::empty()))
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

/// Create router with all API endpoints
pub fn create_router() -> Router<AppState> {
    Router::new()
        .route("/api/health", axum::routing::get(health_check))
        .route("/api/codebases", axum::routing::get(list_codebases))
        .route("/api/codebases/:id", axum::routing::get(refresh_codebase))
        .route("/api/codebases/:id/graph", axum::routing::get(get_graph))
        .route("/api/codebases/:id/files", axum::routing::get(get_file_tree))
        .route("/api/search", axum::routing::get(search))
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
