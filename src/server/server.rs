//! Server instance management

use std::collections::HashSet;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::signal;
use tracing::{error, info};

use crate::server::config::ServerConfig;
use crate::server::error::ApiError;
use crate::server::handlers::{create_router, AppState};
use crate::storage::Storage;
use walkdir::WalkDir;

/// LeIndex HTTP/WebSocket server
///
/// Manages Axum server lifecycle including startup,
/// graceful shutdown, and connection management.
pub struct LeIndexServer {
    /// Server configuration
    config: ServerConfig,

    /// Storage layer wrapped in Arc<Mutex> for thread safety
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
    /// Reference to Arc<Mutex<Storage>>
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_server_default_config() {
        let config = ServerConfig::default();
        let server = LeIndexServer::new(config);
        assert!(server.is_ok());
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

/// Attach a project database and copy its contents into the server database.
fn ingest_project_db(target: &mut Storage, project_db: &Path) -> Result<(), ApiError> {
    let db_str = project_db
        .to_str()
        .ok_or_else(|| ApiError::internal("Invalid project db path"))?
        .replace('\'', "''");

    let sql = format!(
        "
        ATTACH DATABASE '{db}' AS project;
        INSERT OR IGNORE INTO project_metadata SELECT * FROM project.project_metadata;
        INSERT OR IGNORE INTO indexed_files SELECT * FROM project.indexed_files;
        INSERT OR IGNORE INTO intel_nodes SELECT * FROM project.intel_nodes;
        INSERT OR IGNORE INTO intel_edges SELECT * FROM project.intel_edges;
        INSERT OR IGNORE INTO global_symbols SELECT * FROM project.global_symbols;
        INSERT OR IGNORE INTO external_refs SELECT * FROM project.external_refs;
        INSERT OR IGNORE INTO project_deps SELECT * FROM project.project_deps;
        DETACH DATABASE project;
        ",
        db = db_str
    );

    target
        .conn()
        .execute_batch(&sql)
        .map_err(|e| ApiError::internal(format!("Ingest failed: {}", e)))?;

    Ok(())
}
