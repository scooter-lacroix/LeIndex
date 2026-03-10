//! HTTP handlers for REST API endpoints

use crate::storage::Storage;
use axum::{
    extract::{ws::WebSocketUpgrade, Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    Json, Router,
};
use futures::stream::StreamExt;
use serde::Deserialize;
use std::sync::{Arc, Mutex};
use tracing::{error, info};

use crate::server::config::ServerConfig;
use crate::server::error::{ApiError, ApiResult};
use crate::server::responses::{
    CacheOverviewResponse, CodebaseDetailResponse, CodebaseListResponse, CodebaseResponse,
    DashboardCodebaseMetricsResponse, DashboardOverviewResponse,
    ExternalDependencyOverviewResponse, FeatureStatusResponse, FileNode, FileTreeResponse,
    GraphDataResponse, GraphLinkResponse, GraphNodeResponse, LanguageDistributionResponse,
    ScoreResponse, SearchResultResponse, SearchResultsResponse,
};
use crate::server::websocket::{WsEvent, WsManager};

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

    let storage = state.storage.lock().map_err(|e| {
        error!("Failed to acquire storage lock: {}", e);
        ApiError::internal(format!("Storage lock error: {}", e))
    })?;

    // Query all projects from the database
    let conn = storage.conn();
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                pm.unique_project_id,
                pm.base_name,
                pm.path_hash,
                pm.instance,
                pm.canonical_path,
                pm.display_name,
                COALESCE(fc.file_count, 0) AS file_count,
                COALESCE(nc.node_count, 0) AS node_count,
                COALESCE(ec.edge_count, 0) AS edge_count,
                pm.is_clone,
                pm.cloned_from
            FROM project_metadata pm
            LEFT JOIN (
                SELECT project_id, COUNT(*) AS file_count
                FROM indexed_files
                GROUP BY project_id
            ) fc ON fc.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT project_id, COUNT(*) AS node_count
                FROM intel_nodes
                GROUP BY project_id
            ) nc ON nc.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT n.project_id, COUNT(*) AS edge_count
                FROM intel_edges e
                INNER JOIN intel_nodes n ON e.caller_id = n.id
                GROUP BY n.project_id
            ) ec ON ec.project_id = pm.unique_project_id
            ORDER BY pm.base_name, pm.instance
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
                file_count: row.get(6)?,
                node_count: row.get(7)?,
                edge_count: row.get(8)?,
                is_valid: true,
                is_clone: row.get(9)?,
                cloned_from: row.get(10)?,
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

/// GET /api/codebases/:id - Get one codebase by ID.
pub async fn get_codebase(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Json<CodebaseDetailResponse>> {
    info!("Getting codebase detail: {}", id);

    let storage = state.storage.lock().map_err(|e| {
        error!("Failed to acquire storage lock: {}", e);
        ApiError::internal(format!("Storage lock error: {}", e))
    })?;

    let conn = storage.conn();
    let mut stmt = conn
        .prepare(
            r#"
            SELECT
                pm.unique_project_id,
                pm.base_name,
                pm.path_hash,
                pm.instance,
                pm.canonical_path,
                pm.display_name,
                COALESCE(fc.file_count, 0) AS file_count,
                COALESCE(nc.node_count, 0) AS node_count,
                COALESCE(ec.edge_count, 0) AS edge_count,
                pm.is_clone,
                pm.cloned_from
            FROM project_metadata pm
            LEFT JOIN (
                SELECT project_id, COUNT(*) AS file_count
                FROM indexed_files
                GROUP BY project_id
            ) fc ON fc.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT project_id, COUNT(*) AS node_count
                FROM intel_nodes
                GROUP BY project_id
            ) nc ON nc.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT n.project_id, COUNT(*) AS edge_count
                FROM intel_edges e
                INNER JOIN intel_nodes n ON e.caller_id = n.id
                GROUP BY n.project_id
            ) ec ON ec.project_id = pm.unique_project_id
            WHERE pm.unique_project_id = ?1
            LIMIT 1
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare get_codebase query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let mut rows = stmt.query([&id]).map_err(|e| {
        error!("Failed to execute get_codebase query: {}", e);
        ApiError::internal(format!("Database execution error: {}", e))
    })?;
    let maybe_row = if let Some(row) = rows.next().map_err(|e| {
        error!("Failed to read get_codebase row: {}", e);
        ApiError::internal(format!("Database row read error: {}", e))
    })? {
        Some(CodebaseResponse {
            id: row.get::<_, String>(0).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase id column: {}", e))
            })?,
            unique_project_id: row.get::<_, String>(0).map_err(|e| {
                ApiError::internal(format!(
                    "Failed to read codebase unique_project_id column: {}",
                    e
                ))
            })?,
            base_name: row.get(1).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase base_name column: {}", e))
            })?,
            path_hash: row.get(2).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase path_hash column: {}", e))
            })?,
            instance: row.get(3).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase instance column: {}", e))
            })?,
            project_path: row.get(4).map_err(|e| {
                ApiError::internal(format!(
                    "Failed to read codebase project_path column: {}",
                    e
                ))
            })?,
            display_name: row.get(5).map_err(|e| {
                ApiError::internal(format!(
                    "Failed to read codebase display_name column: {}",
                    e
                ))
            })?,
            project_type: "Rust".to_string(),
            last_indexed: "Unknown".to_string(),
            file_count: row.get(6).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase file_count column: {}", e))
            })?,
            node_count: row.get(7).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase node_count column: {}", e))
            })?,
            edge_count: row.get(8).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase edge_count column: {}", e))
            })?,
            is_valid: true,
            is_clone: row.get(9).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase is_clone column: {}", e))
            })?,
            cloned_from: row.get(10).map_err(|e| {
                ApiError::internal(format!("Failed to read codebase cloned_from column: {}", e))
            })?,
        })
    } else {
        None
    };

    let codebase = maybe_row.ok_or_else(|| ApiError::not_found(id))?;
    Ok(Json(CodebaseDetailResponse { codebase }))
}

/// POST /api/codebases/:id/refresh - Trigger manual re-sync.
pub async fn refresh_codebase(
    Path(id): Path<String>,
    State(_state): State<AppState>,
) -> ApiResult<StatusCode> {
    info!("Triggering refresh for codebase: {}", id);
    Ok(StatusCode::ACCEPTED)
}

/// GET /api/dashboard/overview - Aggregate dashboard metrics.
pub async fn dashboard_overview(
    State(state): State<AppState>,
) -> ApiResult<Json<DashboardOverviewResponse>> {
    info!("Building dashboard overview");

    let storage = state.storage.lock().map_err(|e| {
        error!("Failed to acquire storage lock: {}", e);
        ApiError::internal(format!("Storage lock error: {}", e))
    })?;
    let conn = storage.conn();

    let mut codebase_stmt = conn
        .prepare(
            r#"
            SELECT
                pm.unique_project_id,
                pm.display_name,
                pm.canonical_path,
                COALESCE(fc.file_count, 0) AS file_count,
                COALESCE(nc.node_count, 0) AS node_count,
                COALESCE(ec.edge_count, 0) AS edge_count,
                COALESCE(ic.import_edge_count, 0) AS import_edge_count,
                COALESCE(er.external_ref_count, 0) AS external_ref_count,
                COALESCE(pd.dependency_link_count, 0) AS dependency_link_count
            FROM project_metadata pm
            LEFT JOIN (
                SELECT project_id, COUNT(*) AS file_count
                FROM indexed_files
                GROUP BY project_id
            ) fc ON fc.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT project_id, COUNT(*) AS node_count
                FROM intel_nodes
                GROUP BY project_id
            ) nc ON nc.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT n.project_id, COUNT(*) AS edge_count
                FROM intel_edges e
                INNER JOIN intel_nodes n ON e.caller_id = n.id
                GROUP BY n.project_id
            ) ec ON ec.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT n.project_id, COUNT(*) AS import_edge_count
                FROM intel_edges e
                INNER JOIN intel_nodes n ON e.caller_id = n.id
                WHERE lower(e.edge_type) = 'import'
                GROUP BY n.project_id
            ) ic ON ic.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT source_project_id AS project_id, COUNT(*) AS external_ref_count
                FROM external_refs
                GROUP BY source_project_id
            ) er ON er.project_id = pm.unique_project_id
            LEFT JOIN (
                SELECT project_id, COUNT(*) AS dependency_link_count
                FROM project_deps
                GROUP BY project_id
            ) pd ON pd.project_id = pm.unique_project_id
            ORDER BY pm.base_name, pm.instance
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare codebase metrics query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let codebases = codebase_stmt
        .query_map([], |row| {
            Ok(DashboardCodebaseMetricsResponse {
                id: row.get(0)?,
                display_name: row.get(1)?,
                project_path: row.get(2)?,
                file_count: row.get(3)?,
                node_count: row.get(4)?,
                edge_count: row.get(5)?,
                import_edge_count: row.get(6)?,
                external_ref_count: row.get(7)?,
                dependency_link_count: row.get(8)?,
            })
        })
        .map_err(|e| {
            error!("Failed to execute codebase metrics query: {}", e);
            ApiError::internal(format!("Database execution error: {}", e))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("Failed to collect codebase metrics: {}", e);
            ApiError::internal(format!("Result collection error: {}", e))
        })?;

    let total_files = codebases.iter().map(|c| c.file_count).sum::<i64>();
    let total_nodes = codebases.iter().map(|c| c.node_count).sum::<i64>();
    let total_edges = codebases.iter().map(|c| c.edge_count).sum::<i64>();

    let mut lang_stmt = conn
        .prepare(
            r#"
            SELECT language, COUNT(*) AS count
            FROM intel_nodes
            GROUP BY language
            ORDER BY count DESC, language
            LIMIT 24
            "#,
        )
        .map_err(|e| {
            error!("Failed to prepare language distribution query: {}", e);
            ApiError::internal(format!("Database query error: {}", e))
        })?;

    let language_distribution = lang_stmt
        .query_map([], |row| {
            Ok(LanguageDistributionResponse {
                language: row.get(0)?,
                count: row.get(1)?,
            })
        })
        .map_err(|e| {
            error!("Failed to execute language distribution query: {}", e);
            ApiError::internal(format!("Database execution error: {}", e))
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| {
            error!("Failed to collect language distribution: {}", e);
            ApiError::internal(format!("Result collection error: {}", e))
        })?;

    let analysis_cache_entries: i64 = conn
        .query_row("SELECT COUNT(*) FROM analysis_cache", [], |row| row.get(0))
        .unwrap_or(0);

    let telemetry_result = conn.query_row(
        "SELECT cache_hits, cache_misses FROM cache_telemetry WHERE id = 1",
        [],
        |row| {
            let hits: i64 = row.get(0)?;
            let misses: i64 = row.get(1)?;
            Ok((hits, misses))
        },
    );
    let estimated_hit_rate = telemetry_result.ok().and_then(|(hits, misses)| {
        let total = hits + misses;
        if total > 0 {
            Some((hits as f64) / (total as f64))
        } else {
            None
        }
    });

    let temperature = match estimated_hit_rate {
        Some(rate) if rate >= 0.75 => "hot",
        Some(rate) if rate >= 0.35 => "warm",
        Some(_) => "cold",
        None if analysis_cache_entries > 0 => "warm",
        None => "cold",
    }
    .to_string();

    let external_refs: i64 = conn
        .query_row("SELECT COUNT(*) FROM external_refs", [], |row| row.get(0))
        .unwrap_or(0);
    let dependency_links: i64 = conn
        .query_row("SELECT COUNT(*) FROM project_deps", [], |row| row.get(0))
        .unwrap_or(0);
    let import_edges: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM intel_edges WHERE lower(edge_type) = 'import'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    let generated_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default();

    Ok(Json(DashboardOverviewResponse {
        generated_at,
        status: "healthy".to_string(),
        total_codebases: codebases.len(),
        total_files,
        total_nodes,
        total_edges,
        language_distribution,
        feature_status: FeatureStatusResponse {
            multi_project_enabled: true,
            cache_telemetry_enabled: true,
            external_dependency_resolution_enabled: true,
            context_aware_editing_enabled: true,
            bounded_impact_analysis_enabled: true,
        },
        cache: CacheOverviewResponse {
            analysis_cache_entries,
            temperature,
            estimated_hit_rate,
        },
        external_dependencies: ExternalDependencyOverviewResponse {
            external_refs,
            project_dependency_links: dependency_links,
            import_edges,
        },
        codebases,
    }))
}

/// GET /api/codebases/:id/graph - Get dependency graph data
pub async fn get_graph(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Json<GraphDataResponse>> {
    info!("Getting graph for codebase: {}", id);

    let storage = state.storage.lock().map_err(|e| {
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

    info!(
        "Retrieved {} nodes and {} links for codebase {}",
        nodes.len(),
        links.len(),
        id
    );
    Ok(Json(GraphDataResponse { nodes, links }))
}

/// GET /api/codebases/:id/files - Get file tree
pub async fn get_file_tree(
    Path(id): Path<String>,
    State(state): State<AppState>,
) -> ApiResult<Json<FileTreeResponse>> {
    info!("Getting file tree for codebase: {}", id);

    let storage = state.storage.lock().map_err(|e| {
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
                name: file_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&file_path)
                    .to_string(),
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

    let storage = state.storage.lock().map_err(|e| {
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
    Ok(Json(SearchResultsResponse { results }))
}

/// GET /api/health - Health check endpoint
pub async fn health_check(State(_state): State<AppState>) -> ApiResult<Json<serde_json::Value>> {
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
        let conn_id = format!(
            "ws_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis()
        );

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
        .route(
            "/api/dashboard/overview",
            axum::routing::get(dashboard_overview),
        )
        .route("/api/codebases", axum::routing::get(list_codebases))
        .route(
            "/api/codebases/:id",
            axum::routing::get(get_codebase).post(refresh_codebase),
        )
        .route("/api/codebases/:id/graph", axum::routing::get(get_graph))
        .route(
            "/api/codebases/:id/files",
            axum::routing::get(get_file_tree),
        )
        .route("/api/search", axum::routing::get(search))
        .route("/ws", axum::routing::get(websocket_handler))
        .route("/ws/events", axum::routing::get(websocket_handler))
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
