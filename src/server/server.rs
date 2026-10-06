//! Server instance management

use rusqlite::params;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::signal;
use tracing::{error, info};

use crate::server::config::ServerConfig;
use crate::server::error::ApiError;
use crate::server::handlers::{AppState, create_router};
use crate::storage::Storage;
use walkdir::WalkDir;

/// LeIndex HTTP/WebSocket server
///
/// Manages Axum server lifecycle including startup,
/// graceful shutdown, and connection management.
pub struct LeIndexServer {
    /// Server configuration
    config: ServerConfig,

    /// Storage layer wrapped in `Arc<Mutex>` for thread safety
    storage: Arc<Mutex<Storage>>,
}

impl LeIndexServer {
    /// Create new server instance
    ///
    /// # Arguments
    ///
    /// * `config` - Server configuration
    ///
    /// # Returns
    ///
    /// `Result<LeIndexServer, ApiError>` - Server or error
    pub fn new(config: ServerConfig) -> Result<Self, ApiError> {
        // Validate config
        if let Err(e) = config.validate() {
            return Err(ApiError::internal(format!("Invalid config: {}", e)));
        }

        // Open storage
        let mut storage = Storage::open(&config.db_path).map_err(|e| {
            error!("Failed to open storage: {}", e);
            ApiError::internal(format!("Failed to open storage: {}", e))
        })?;

        // Discover existing LeIndex project databases on the system and ingest them
        let discovered = discover_leindex_dbs();
        if discovered.is_empty() {
            info!("No existing LeIndex project databases discovered");
        } else {
            info!(
                "Discovered {} LeIndex project database(s)",
                discovered.len()
            );
        }
        for db_path in discovered {
            if let Err(e) = ingest_project_db(&mut storage, &db_path) {
                error!("Failed to ingest {:?}: {}", db_path, e);
            }
            // Post-flip stores carry no graph rows for the ATTACH copy above;
            // materialize the graph from their generation layer (D6).
            if let Err(e) = ingest_project_graph_layer(&mut storage, &db_path) {
                error!("Failed to ingest graph layer {:?}: {}", db_path, e);
            }
        }

        Ok(Self {
            config,
            storage: Arc::new(Mutex::new(storage)),
        })
    }

    /// Get socket address for binding
    ///
    /// # Returns
    ///
    /// `Result<SocketAddr, ApiError>` - Parsed address or error
    pub fn socket_addr(&self) -> Result<SocketAddr, ApiError> {
        format!("{}:{}", self.config.host, self.config.port)
            .parse::<SocketAddr>()
            .map_err(|e| ApiError::internal(format!("Failed to parse address: {}", e)))
    }

    /// Start server
    ///
    /// # Returns
    ///
    /// `Result<(), ApiError>` - Success or error
    pub async fn start(&self) -> Result<(), ApiError> {
        let addr = self.socket_addr()?;

        // Build application state (clone Arc<Mutex<Storage>>)
        let state = AppState::new_from_arc(Arc::clone(&self.storage), self.config.clone());

        // Build router
        let app = create_router().with_state(state);

        // Create server
        let listener = tokio::net::TcpListener::bind(addr).await.map_err(|e| {
            error!("Failed to bind to {}: {:?}", addr, e);
            ApiError::internal(format!("Failed to bind to {}: {}", addr, e))
        })?;

        info!(
            "Server listening on: http://{}:{}",
            self.config.host, self.config.port
        );

        axum::serve(listener, app)
            .await
            .map_err(|e| ApiError::internal(format!("Server error: {}", e)))
    }

    /// Wait for shutdown signal
    ///
    /// Blocks until Ctrl+C is received
    pub async fn wait_for_shutdown(&self) {
        let ctrl_c = async {
            signal::ctrl_c()
                .await
                .expect("Failed to install Ctrl+C handler");
            info!("Received shutdown signal");
        };

        #[cfg(unix)]
        let terminate = async {
            use tokio::signal::unix;
            unix::signal(unix::SignalKind::terminate())
                .expect("Failed to install TERM handler")
                .recv()
                .await;
            info!("Received TERM signal");
        };

        #[cfg(not(unix))]
        let terminate = std::future::pending::<()>();

        tokio::select! {
            _ = ctrl_c => {},
            _ = terminate => {},
        }
    }

    /// Get storage reference
    ///
    /// # Returns
    ///
    /// Reference to `Arc<Mutex<Storage>>`
    #[must_use]
    pub fn storage(&self) -> Arc<Mutex<Storage>> {
        Arc::clone(&self.storage)
    }

    /// Get server URL
    ///
    /// # Returns
    ///
    /// Formatted server URL
    #[must_use]
    pub fn server_url(&self) -> String {
        format!("http://{}:{}", self.config.host, self.config.port)
    }

    /// Get WebSocket URL
    ///
    /// # Returns
    ///
    /// Formatted WebSocket URL
    #[must_use]
    pub fn websocket_url(&self) -> String {
        format!("ws://{}:{}/ws/events", self.config.host, self.config.port)
    }
}

/// Search the filesystem for `.leindex/leindex.db` project databases.
/// Roots are taken from `LEINDEX_DISCOVERY_ROOTS` (comma-separated) when set;
/// otherwise defaults to `$HOME` and the current working directory.
fn discover_leindex_dbs() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    if let Ok(env_roots) = std::env::var("LEINDEX_DISCOVERY_ROOTS") {
        for part in env_roots.split(',') {
            let trimmed = part.trim();
            if !trimmed.is_empty() {
                roots.push(PathBuf::from(trimmed));
            }
        }
    }

    if roots.is_empty() {
        if let Ok(home) = std::env::var("HOME") {
            roots.push(PathBuf::from(home));
        }
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd);
        }
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut found: Vec<PathBuf> = Vec::new();

    for root in roots {
        if !root.exists() {
            continue;
        }
        for entry in WalkDir::new(root)
            .follow_links(false)
            .max_depth(8)
            .into_iter()
            .filter_map(Result::ok)
        {
            let path = entry.path();
            if path.file_name().map(|n| n == "leindex.db").unwrap_or(false) {
                if let Some(parent) = path.parent() {
                    if parent.file_name().map(|n| n == ".leindex").unwrap_or(false) {
                        if let Ok(canon) = path.canonicalize() {
                            if seen.insert(canon.clone()) {
                                found.push(canon);
                            }
                        }
                    }
                }
            }
        }
    }

    found
}

/// Materialize a project's graph from its CURRENT generation's Pdg layer into
/// the cross-project store (batched INSERT). Per-project stores no longer
/// carry graph rows on the save path (D5), so a post-flip source contributes
/// its layer-decoded graph here instead of via ATTACH; legacy sources (which
/// still hold rows) are copied by `ingest_project_db` directly. The
/// cross-project store keeps its SQL aggregation-cache shape (D6).
fn ingest_project_graph_layer(target: &mut Storage, project_db: &Path) -> Result<(), ApiError> {
    let Some(storage_root) = project_db.parent() else {
        return Ok(());
    };
    let Ok(snapshot) = crate::storage::generation::GenerationSnapshot::open(storage_root) else {
        return Ok(());
    };
    let Some(reader) = snapshot.pdg() else {
        return Ok(());
    };
    let pdg = reader
        .to_program_dependence_graph()
        .map_err(|error| ApiError::internal(format!("Graph layer decode failed: {error}")))?;

    let conn = target.conn();
    let project_id: String = conn
        .query_row(
            "SELECT unique_project_id FROM project_metadata WHERE canonical_path = ?1 LIMIT 1",
            params![
                project_db
                    .parent()
                    .and_then(|p| p.parent())
                    .and_then(|p| p.to_str())
                    .unwrap_or_default()
            ],
            |row| row.get(0),
        )
        .unwrap_or_default();
    if project_id.is_empty() {
        return Ok(());
    }

    let mut insert = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO intel_nodes (id, project_id, file_path, node_id, symbol_name, \
             qualified_name, language, node_type, signature, complexity, content_hash, embedding, \
             byte_range_start, byte_range_end, created_at, updated_at, embedding_format, precision) \
             VALUES ((SELECT COALESCE(MAX(id), 0) + 1 FROM intel_nodes), ?1, ?2, ?3, ?4, ?4, ?5, ?6, \
             NULL, ?7, '', NULL, ?8, ?9, 0, 0, 0, ?10)",
        )
        .map_err(|e| ApiError::internal(format!("Graph materialize failed: {e}")))?;
    for idx in pdg.node_indices() {
        let Some(node) = pdg.get_node(idx) else {
            continue;
        };
        let node_type = match node.node_type {
            crate::graph::pdg::NodeType::Function => "function",
            crate::graph::pdg::NodeType::Class => "class",
            crate::graph::pdg::NodeType::Method => "method",
            crate::graph::pdg::NodeType::Variable => "variable",
            crate::graph::pdg::NodeType::Module => "module",
            crate::graph::pdg::NodeType::External => "external",
            crate::graph::pdg::NodeType::DocSection => "doc_section",
            crate::graph::pdg::NodeType::FileSummary => "file_summary",
        };
        insert
            .execute(params![
                project_id,
                node.file_path.to_string(),
                node.id,
                node.name,
                node.language,
                node_type,
                node.complexity as i64,
                node.byte_range.0 as i64,
                node.byte_range.1 as i64,
                i64::from(pdg.is_precision_symbol(&node.id)),
            ])
            .map_err(|e| ApiError::internal(format!("Graph materialize failed: {e}")))?;
    }

    let mut edge_insert = conn
        .prepare_cached(
            "INSERT OR IGNORE INTO intel_edges (caller_id, callee_id, edge_type, metadata) \
             SELECT caller.id, callee.id, ?3, NULL \
             FROM intel_nodes caller, intel_nodes callee \
             WHERE caller.project_id = ?1 AND callee.project_id = ?1 \
             AND caller.node_id = ?2 AND callee.node_id = ?4",
        )
        .map_err(|e| ApiError::internal(format!("Graph materialize failed: {e}")))?;
    for edge_id in pdg.edge_indices() {
        let Some(edge) = pdg.get_edge(edge_id) else {
            continue;
        };
        let Some((source, target)) = pdg.edge_endpoints(edge_id) else {
            continue;
        };
        let (Some(source_node), Some(target_node)) = (pdg.get_node(source), pdg.get_node(target))
        else {
            continue;
        };
        let edge_type = match edge.edge_type {
            crate::graph::pdg::EdgeType::Call => "call",
            crate::graph::pdg::EdgeType::DataDependency => "data_dependency",
            crate::graph::pdg::EdgeType::Inheritance => "inheritance",
            crate::graph::pdg::EdgeType::Import => "import",
            crate::graph::pdg::EdgeType::Containment => "containment",
            crate::graph::pdg::EdgeType::TypeOf => "type_of",
            crate::graph::pdg::EdgeType::StateTransition => "state_transition",
            crate::graph::pdg::EdgeType::CommandArgument => "command_argument",
            crate::graph::pdg::EdgeType::Environment => "environment",
            crate::graph::pdg::EdgeType::Stdin => "stdin",
        };
        edge_insert
            .execute(params![
                project_id,
                source_node.id,
                edge_type,
                target_node.id,
            ])
            .map_err(|e| ApiError::internal(format!("Graph materialize failed: {e}")))?;
    }
    Ok(())
}

/// Attach a project database and copy its contents into the server database.
fn ingest_project_db(target: &mut Storage, project_db: &Path) -> Result<(), ApiError> {
    let db_str = project_db
        .to_str()
        .ok_or_else(|| ApiError::internal("Invalid project db path"))?;

    let conn = target.conn();
    conn.execute("ATTACH DATABASE ?1 AS project", params![db_str])
        .map_err(|e| ApiError::internal(format!("Ingest failed: {}", e)))?;

    // Keep the import explicit rather than relying on column positions. The
    // precision marker was added after older project databases were written;
    // omitting it when it is absent lets SQLite apply the target default.
    let source_columns = conn
        .prepare("PRAGMA project.table_info(intel_nodes)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| ApiError::internal(format!("Ingest failed: {}", e)))?;
    let target_columns = conn
        .prepare("PRAGMA main.table_info(intel_nodes)")
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|e| ApiError::internal(format!("Ingest failed: {}", e)))?;

    let node_columns = "id, project_id, file_path, node_id, symbol_name, qualified_name, \
        language, node_type, signature, complexity, content_hash, embedding, \
        byte_range_start, byte_range_end, created_at, updated_at, embedding_format";
    let mut node_insert_columns = node_columns.to_string();
    let mut node_select_columns = node_columns.to_string();
    for optional in ["precision", "community_id"] {
        if source_columns.iter().any(|column| column == optional)
            && target_columns.iter().any(|column| column == optional)
        {
            node_insert_columns.push_str(", ");
            node_insert_columns.push_str(optional);
            node_select_columns.push_str(", ");
            node_select_columns.push_str(optional);
        }
    }

    let mut statements = "
        INSERT OR IGNORE INTO project_metadata SELECT * FROM project.project_metadata;
        INSERT OR IGNORE INTO indexed_files SELECT * FROM project.indexed_files;
        INSERT OR IGNORE INTO global_symbols SELECT * FROM project.global_symbols;
        INSERT OR IGNORE INTO external_refs SELECT * FROM project.external_refs;
        INSERT OR IGNORE INTO project_deps SELECT * FROM project.project_deps;
        "
    .to_string();

    // D6: per-project stores no longer carry graph rows on the save path, but
    // legacy stores (pre-flip) still do, and the cross-project store keeps its
    // SQL aggregation-cache shape. Copy graph rows only when the attached
    // source actually has them; otherwise the source project's graph is
    // materialized from its generation layer by the caller (see
    // `ingest_project_graph_layer`).
    let has_graph_rows: bool = conn
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM project.sqlite_master \
             WHERE type = 'table' AND name = 'intel_nodes')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| ApiError::internal(format!("Ingest failed: {}", e)))?;
    if has_graph_rows {
        statements.push_str(&format!(
            "INSERT OR IGNORE INTO intel_nodes ({node_insert_columns}) \
             SELECT {node_select_columns} FROM project.intel_nodes;\n\
             INSERT OR IGNORE INTO intel_edges SELECT * FROM project.intel_edges;\n"
        ));
    }

    conn.execute_batch(&statements)
        .map_err(|e| ApiError::internal(format!("Ingest failed: {}", e)))?;
    conn.execute_batch("DETACH DATABASE project;")
        .map_err(|e| ApiError::internal(format!("Ingest failed: {}", e)))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_default_config() {
        // Use a temporary database path to avoid conflicts
        let temp_dir = std::env::temp_dir();
        let db_path = temp_dir.join(format!("leindex_test_{}.db", std::process::id()));
        let config = ServerConfig {
            db_path: db_path.to_string_lossy().to_string(),
            ..Default::default()
        };

        let server = LeIndexServer::new(config);
        assert!(server.is_ok());

        // Clean up the test database
        let _ = std::fs::remove_file(db_path);
    }

    #[test]
    fn test_ingest_legacy_project_without_precision_column() {
        let temp_dir = tempfile::tempdir().unwrap();
        let target_path = temp_dir.path().join("target.db");
        let project_path = temp_dir.path().join("project.db");
        let mut target = Storage::open(&target_path).unwrap();
        let project = rusqlite::Connection::open(&project_path).unwrap();

        project
            .execute_batch(
                "CREATE TABLE project_metadata (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    unique_project_id TEXT UNIQUE NOT NULL,
                    base_name TEXT NOT NULL,
                    path_hash TEXT NOT NULL,
                    instance INTEGER DEFAULT 0,
                    canonical_path TEXT NOT NULL,
                    display_name TEXT,
                    is_clone BOOLEAN DEFAULT 0,
                    cloned_from TEXT,
                    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    last_indexed TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
                    UNIQUE(canonical_path)
                );
                CREATE TABLE indexed_files (
                    file_path TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    file_hash TEXT NOT NULL,
                    last_indexed INTEGER NOT NULL
                );
                CREATE TABLE intel_nodes (
                    id INTEGER PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    file_path TEXT NOT NULL,
                    node_id TEXT NOT NULL,
                    symbol_name TEXT NOT NULL,
                    qualified_name TEXT NOT NULL,
                    language TEXT NOT NULL DEFAULT 'unknown',
                    node_type TEXT NOT NULL,
                    signature TEXT,
                    complexity INTEGER,
                    content_hash TEXT NOT NULL,
                    embedding BLOB,
                    byte_range_start INTEGER,
                    byte_range_end INTEGER,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL,
                    embedding_format INTEGER
                );
                CREATE TABLE intel_edges (
                    caller_id INTEGER NOT NULL,
                    callee_id INTEGER NOT NULL,
                    edge_type TEXT NOT NULL,
                    metadata TEXT,
                    PRIMARY KEY(caller_id, callee_id, edge_type)
                );
                CREATE TABLE global_symbols (
                    symbol_id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    symbol_name TEXT NOT NULL,
                    symbol_type TEXT NOT NULL,
                    signature TEXT,
                    file_path TEXT NOT NULL,
                    byte_range_start INTEGER,
                    byte_range_end INTEGER,
                    complexity INTEGER DEFAULT 1,
                    is_public INTEGER DEFAULT 0,
                    created_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                    UNIQUE(project_id, symbol_name, signature)
                );
                CREATE TABLE external_refs (
                    ref_id TEXT PRIMARY KEY,
                    source_project_id TEXT NOT NULL,
                    source_symbol_id TEXT NOT NULL,
                    target_project_id TEXT NOT NULL,
                    target_symbol_id TEXT NOT NULL,
                    ref_type TEXT NOT NULL
                );
                CREATE TABLE project_deps (
                    dep_id TEXT PRIMARY KEY,
                    project_id TEXT NOT NULL,
                    depends_on_project_id TEXT NOT NULL,
                    dependency_type TEXT NOT NULL,
                    UNIQUE(project_id, depends_on_project_id)
                );
                INSERT INTO intel_nodes (
                    id, project_id, file_path, node_id, symbol_name, qualified_name,
                    language, node_type, signature, complexity, content_hash,
                    embedding, byte_range_start, byte_range_end, created_at,
                    updated_at, embedding_format
                ) VALUES (
                    1, 'legacy-project', 'src/lib.rs', 'fn:main', 'main', 'main',
                    'rust', 'function', NULL, 1, 'hash', NULL, 0, 10, 1, 1, 0
                );",
            )
            .unwrap();
        drop(project);

        ingest_project_db(&mut target, &project_path).unwrap();

        let (node_count, precision): (i64, i64) = target
            .conn()
            .query_row(
                "SELECT COUNT(*), COALESCE(MAX(precision), -1) FROM intel_nodes",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(node_count, 1);
        assert_eq!(precision, 0, "legacy rows receive the target default");
    }
}
