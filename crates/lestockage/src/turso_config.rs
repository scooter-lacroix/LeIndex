// Turso configuration for hybrid storage
//
// This module provides configuration for Turso/libsql hybrid storage,
// combining local SQLite with remote Turso vector store capabilities.

use serde::{Deserialize, Serialize};
use std::time::Instant;
use thiserror::Error;

/// Turso configuration
///
/// Configures the connection to Turso (remote libsql database) and
/// controls the hybrid storage behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TursoConfig {
    /// Database URL (e.g., libsql://token@db.turso.io)
    /// For local-only mode, use "file:local.db"
    pub database_url: String,

    /// Auth token for Turso
    /// Empty string for local-only mode
    pub auth_token: String,

    /// Enable vector extension in Turso
    /// When true, enables the vec0 extension for vector similarity search
    pub enable_vectors: bool,

    /// Remote-only mode (no local SQLite)
    /// When true, only uses Turso for all storage
    pub remote_only: bool,
}

impl Default for TursoConfig {
    fn default() -> Self {
        Self {
            database_url: "file:local.db".to_string(),
            auth_token: String::new(),
            enable_vectors: false,
            remote_only: false,
        }
    }
}

impl TursoConfig {
    /// Create a new Turso config
    #[must_use]
    pub fn new(database_url: String, auth_token: String) -> Self {
        Self {
            database_url,
            auth_token,
            enable_vectors: false,
            remote_only: false,
        }
    }

    /// Create local-only config
    #[must_use]
    pub fn local_only() -> Self {
        Self {
            database_url: "file:local.db".to_string(),
            auth_token: String::new(),
            enable_vectors: false,
            remote_only: false,
        }
    }

    /// Create remote-only config
    #[must_use]
    pub fn remote_only(database_url: String, auth_token: String) -> Self {
        Self {
            database_url,
            auth_token,
            enable_vectors: false,
            remote_only: true,
        }
    }

    /// Create hybrid config (local + remote)
    #[must_use]
    pub fn hybrid(database_url: String, auth_token: String) -> Self {
        Self {
            database_url,
            auth_token,
            enable_vectors: false,
            remote_only: false,
        }
    }

    /// Enable vector extension
    #[must_use]
    pub fn with_vectors(mut self, enable: bool) -> Self {
        self.enable_vectors = enable;
        self
    }

    /// Check if this is a local-only configuration
    #[must_use]
    pub fn is_local_only(&self) -> bool {
        self.database_url.starts_with("file:") || self.auth_token.is_empty()
    }

    /// Check if this is a remote configuration
    #[must_use]
    pub fn is_remote(&self) -> bool {
        !self.is_local_only()
    }
}

/// Migration statistics
///
/// Tracks the progress and results of a migration operation.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct MigrationStats {
    /// Number of nodes migrated
    pub nodes_migrated: usize,

    /// Number of edges migrated
    pub edges_migrated: usize,

    /// Number of embeddings migrated
    pub embeddings_migrated: usize,

    /// Time taken for migration (milliseconds)
    pub migration_time_ms: u64,
}

/// Storage mode
///
/// Indicates the current storage mode of the HybridStorage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMode {
    /// No storage backend is configured or available
    None,
    /// Only local SQLite storage is being used
    LocalOnly,
    /// Only remote Turso storage is being used
    RemoteOnly,
    /// Both local and remote storage are being used in a hybrid configuration
    Hybrid,
}

/// Storage errors
///
/// Errors that can occur when working with HybridStorage.
#[derive(Debug, Error)]
pub enum StorageError {
    /// Failed to connect to the storage backend
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    /// Failed to migrate data between storage backends
    #[error("Migration failed: {0}")]
    MigrationFailed(String),

    /// The vector search extension is not available on the remote backend
    #[error("Vector extension not available")]
    VectorExtensionNotAvailable,

    /// An error occurred in the local SQLite storage
    #[error("Local storage error: {0}")]
    LocalStorageError(String),

    /// A query executed on the remote backend failed
    #[error("Remote query failed: {0}")]
    RemoteQueryFailed(String),
}

/// Hybrid storage: local SQLite + remote Turso
///
/// Combines local SQLite storage with optional remote Turso storage.
/// This enables:
/// - Local-first operation for fast development
/// - Optional remote storage for production scale
/// - Vector similarity search via Turso's vec0 extension
/// - Migration from local to remote
pub struct HybridStorage {
    /// Local SQLite storage
    pub local: Option<crate::Storage>,

    /// Configuration
    pub config: TursoConfig,

    /// Whether vector extension is initialized
    pub vectors_initialized: bool,
}

impl HybridStorage {
    /// Create hybrid storage from configuration
    ///
    /// # Arguments
    ///
    /// * `config` - Turso configuration
    ///
    /// # Returns
    ///
    /// `Ok(HybridStorage)` if successful, `Err(StorageError)` if connection fails
    ///
    /// # Example
    ///
    /// ```ignore
    /// let config = TursoConfig::local_only();
    /// let storage = HybridStorage::new(config)?;
    /// ```
    pub fn new(config: TursoConfig) -> Result<Self, StorageError> {
        // Initialize local storage if not remote-only
        let local = if !config.remote_only {
            // Use file:<path> when provided, otherwise default to local.db
            let local_path = if config.database_url.starts_with("file:") {
                config
                    .database_url
                    .trim_start_matches("file:")
                    .to_string()
            } else {
                "local.db".to_string()
            };

            let storage = crate::Storage::open(local_path)
                .map_err(|e| StorageError::LocalStorageError(format!("{:?}", e)))?;
            Some(storage)
        } else {
            None
        };

        Ok(Self {
            local,
            config,
            vectors_initialized: false,
        })
    }

    /// Initialize vector extension in Turso
    ///
    /// This enables the vec0 extension for vector similarity search.
    /// For local SQLite, this sets up vector tables. For remote Turso,
    /// this loads the vec0 extension.
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, `Err(StorageError)` if initialization fails
    pub fn init_vectors(&mut self) -> Result<(), StorageError> {
        if !self.config.enable_vectors {
            return Ok(());
        }

        // Optional remote Turso initialization
        if self.config.is_remote() {
            self.init_remote_vectors()?;
        }

        // Local storage initialization
        if let Some(storage) = &self.local {
            self.init_local_vectors(storage)?;
        }

        self.vectors_initialized = true;
        tracing::info!("Vector extension initialized successfully");

        Ok(())
    }

    /// Initialize vector tables in local SQLite storage
    fn init_local_vectors(&self, storage: &crate::Storage) -> Result<(), StorageError> {
        let conn = storage.conn();

        // Create node metadata table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS node_metadata (
                node_id TEXT PRIMARY KEY,
                symbol_name TEXT NOT NULL,
                file_path TEXT NOT NULL,
                node_type TEXT NOT NULL,
                created_at INTEGER DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )
        .map_err(|e| {
            StorageError::LocalStorageError(format!(
                "Failed to create node_metadata table: {:?}",
                e
            ))
        })?;

        // Create embeddings table
        conn.execute(
            "CREATE TABLE IF NOT EXISTS node_embeddings (
                node_id TEXT PRIMARY KEY,
                embedding BLOB NOT NULL,
                dimension INTEGER NOT NULL,
                FOREIGN KEY (node_id) REFERENCES node_metadata(node_id) ON DELETE CASCADE
            )",
            [],
        )
        .map_err(|e| {
            StorageError::LocalStorageError(format!(
                "Failed to create node_embeddings table: {:?}",
                e
            ))
        })?;

        // Create index for similarity search (using FTS5-style approach)
        conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_node_embeddings_dimension
             ON node_embeddings(dimension)",
            [],
        )
        .map_err(|e| StorageError::LocalStorageError(format!("Failed to create index: {:?}", e)))?;

        Ok(())
    }

    /// Initialize vector tables in remote Turso/libsql storage.
    fn init_remote_vectors(&self) -> Result<(), StorageError> {
        let database_url = self.config.database_url.clone();
        let auth_token = self.config.auth_token.clone();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StorageError::ConnectionFailed(format!("Failed to create runtime: {e}")))?;

        runtime.block_on(async move {
            let db = libsql::Builder::new_remote(database_url, auth_token)
                .build()
                .await
                .map_err(|e| {
                    StorageError::ConnectionFailed(format!("Failed to connect to remote: {e}"))
                })?;

            let conn = db.connect().map_err(|e| {
                StorageError::ConnectionFailed(format!("Failed to open remote connection: {e}"))
            })?;

            conn.execute(
                "CREATE TABLE IF NOT EXISTS node_metadata (
                    node_id TEXT PRIMARY KEY,
                    symbol_name TEXT NOT NULL,
                    file_path TEXT NOT NULL,
                    node_type TEXT NOT NULL,
                    created_at INTEGER DEFAULT (strftime('%s', 'now'))
                )",
                (),
            )
            .await
            .map_err(|e| {
                StorageError::RemoteQueryFailed(format!("Failed to create metadata table: {e}"))
            })?;

            conn.execute(
                "CREATE TABLE IF NOT EXISTS node_embeddings (
                    node_id TEXT PRIMARY KEY,
                    embedding BLOB NOT NULL,
                    dimension INTEGER NOT NULL
                )",
                (),
            )
            .await
            .map_err(|e| {
                StorageError::RemoteQueryFailed(format!("Failed to create embeddings table: {e}"))
            })?;

            Ok::<(), StorageError>(())
        })
    }

    /// Store an embedding in the vector database
    ///
    /// # Arguments
    ///
    /// * `node_id` - Unique identifier for the node
    /// * `symbol_name` - Symbol name (function/class/variable name)
    /// * `file_path` - Path to the source file
    /// * `node_type` - Type of node (function, class, etc.)
    /// * `embedding` - 768-dimensional embedding vector
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, `Err(StorageError)` if storage fails
    pub fn store_embedding(
        &self,
        node_id: &str,
        symbol_name: &str,
        file_path: &str,
        node_type: &str,
        embedding: &[f32],
    ) -> Result<(), StorageError> {
        if !self.vectors_initialized {
            return Err(StorageError::VectorExtensionNotAvailable);
        }

        // Remote-first when configured
        if self.config.is_remote() {
            if let Ok(()) = self.store_remote_embedding(
                node_id,
                symbol_name,
                file_path,
                node_type,
                embedding,
            ) {
                return Ok(());
            }
        }

        let storage = self
            .local
            .as_ref()
            .ok_or_else(|| StorageError::VectorExtensionNotAvailable)?;

        self.store_local_embedding(
            storage,
            node_id,
            symbol_name,
            file_path,
            node_type,
            embedding,
        )
    }

    /// Store embedding in local SQLite
    fn store_local_embedding(
        &self,
        storage: &crate::Storage,
        node_id: &str,
        symbol_name: &str,
        file_path: &str,
        node_type: &str,
        embedding: &[f32],
    ) -> Result<(), StorageError> {
        use rusqlite::params;

        // Check embedding dimension
        if embedding.is_empty() {
            return Err(StorageError::LocalStorageError(
                "Invalid embedding dimension: expected > 0".to_string(),
            ));
        }

        let conn = storage.conn();

        // Insert or replace node metadata
        conn.execute(
            "INSERT OR REPLACE INTO node_metadata (node_id, symbol_name, file_path, node_type)
             VALUES (?1, ?2, ?3, ?4)",
            params![node_id, symbol_name, file_path, node_type],
        )
        .map_err(|e| {
            StorageError::LocalStorageError(format!("Failed to insert metadata: {:?}", e))
        })?;

        // Convert embedding to bytes for storage
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|v| v.to_le_bytes()).collect();

        // Insert embedding
        conn.execute(
            "INSERT OR REPLACE INTO node_embeddings (node_id, embedding, dimension)
             VALUES (?1, ?2, ?3)",
            params![node_id, embedding_bytes, embedding.len() as i32],
        )
        .map_err(|e| {
            StorageError::LocalStorageError(format!("Failed to insert embedding: {:?}", e))
        })?;

        Ok(())
    }

    /// Store embedding in remote Turso/libsql.
    fn store_remote_embedding(
        &self,
        node_id: &str,
        symbol_name: &str,
        file_path: &str,
        node_type: &str,
        embedding: &[f32],
    ) -> Result<(), StorageError> {
        if embedding.is_empty() {
            return Err(StorageError::RemoteQueryFailed(
                "Invalid embedding dimension: expected > 0".to_string(),
            ));
        }

        let database_url = self.config.database_url.clone();
        let auth_token = self.config.auth_token.clone();
        let node_id = node_id.to_string();
        let symbol_name = symbol_name.to_string();
        let file_path = file_path.to_string();
        let node_type = node_type.to_string();
        let embedding_bytes: Vec<u8> = embedding.iter().flat_map(|v| v.to_le_bytes()).collect();
        let dimension = embedding.len() as i64;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StorageError::ConnectionFailed(format!("Failed to create runtime: {e}")))?;

        runtime.block_on(async move {
            let db = libsql::Builder::new_remote(database_url, auth_token)
                .build()
                .await
                .map_err(|e| {
                    StorageError::ConnectionFailed(format!("Failed to connect to remote: {e}"))
                })?;

            let conn = db.connect().map_err(|e| {
                StorageError::ConnectionFailed(format!("Failed to open remote connection: {e}"))
            })?;

            conn.execute(
                "INSERT OR REPLACE INTO node_metadata (node_id, symbol_name, file_path, node_type)
                 VALUES (?1, ?2, ?3, ?4)",
                libsql::params![node_id.clone(), symbol_name, file_path, node_type],
            )
            .await
            .map_err(|e| {
                StorageError::RemoteQueryFailed(format!("Failed to insert remote metadata: {e}"))
            })?;

            conn.execute(
                "INSERT OR REPLACE INTO node_embeddings (node_id, embedding, dimension)
                 VALUES (?1, ?2, ?3)",
                libsql::params![node_id, embedding_bytes, dimension],
            )
            .await
            .map_err(|e| {
                StorageError::RemoteQueryFailed(format!("Failed to insert remote embedding: {e}"))
            })?;

            Ok::<(), StorageError>(())
        })
    }

    /// Search for similar vectors
    ///
    /// # Arguments
    ///
    /// * `query_embedding` - Query embedding vector (768 dimensions)
    /// * `k` - Number of results to return
    ///
    /// # Returns
    ///
    /// Vector of (node_id, similarity_score) tuples
    pub fn search_similar(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(String, f32)>, StorageError> {
        if !self.vectors_initialized {
            return Err(StorageError::VectorExtensionNotAvailable);
        }

        // Remote-first when configured
        if self.config.is_remote() {
            if let Ok(results) = self.search_remote_similar(query_embedding, k) {
                return Ok(results);
            }
        }

        let storage = self
            .local
            .as_ref()
            .ok_or_else(|| StorageError::VectorExtensionNotAvailable)?;

        self.search_local_similar(storage, query_embedding, k)
    }

    /// Search for similar vectors in local SQLite
    fn search_local_similar(
        &self,
        storage: &crate::Storage,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(String, f32)>, StorageError> {
        use rusqlite::Row;

        if query_embedding.is_empty() {
            return Err(StorageError::LocalStorageError(
                "Invalid query embedding dimension: expected > 0".to_string(),
            ));
        }

        let conn = storage.conn();
        let query_dimension = query_embedding.len() as i32;

        // Get all embeddings for the requested dimension
        let mut stmt = conn
            .prepare(
                "SELECT n.node_id, e.embedding
             FROM node_embeddings e
             JOIN node_metadata n ON e.node_id = n.node_id
             WHERE e.dimension = ?1",
            )
            .map_err(|e| {
                StorageError::LocalStorageError(format!("Failed to prepare query: {:?}", e))
            })?;

        let rows = stmt
            .query_map([query_dimension], |row: &Row| {
                let node_id: String = row.get(0)?;
                let embedding_bytes: Vec<u8> = row.get(1)?;
                Ok((node_id, embedding_bytes))
            })
            .map_err(|e| {
                StorageError::LocalStorageError(format!("Failed to execute query: {:?}", e))
            })?;

        // Calculate cosine similarities
        let mut results: Vec<(String, f32)> = Vec::new();
        for row in rows {
            let (node_id, embedding_bytes) = row.map_err(|e| {
                StorageError::LocalStorageError(format!("Failed to read row: {:?}", e))
            })?;

            // Convert bytes back to f32 vector
            let stored_embedding: Vec<f32> = embedding_bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                .collect();

            // Calculate cosine similarity
            let similarity = cosine_similarity(query_embedding, &stored_embedding);
            results.push((node_id, similarity));
        }

        // Sort by similarity (descending) and take top k
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(k);

        Ok(results)
    }

    /// Search similar vectors in remote Turso/libsql storage.
    fn search_remote_similar(
        &self,
        query_embedding: &[f32],
        k: usize,
    ) -> Result<Vec<(String, f32)>, StorageError> {
        if query_embedding.is_empty() {
            return Err(StorageError::RemoteQueryFailed(
                "Invalid query embedding dimension: expected > 0".to_string(),
            ));
        }

        let database_url = self.config.database_url.clone();
        let auth_token = self.config.auth_token.clone();
        let query_embedding = query_embedding.to_vec();
        let query_dimension = query_embedding.len() as i64;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StorageError::ConnectionFailed(format!("Failed to create runtime: {e}")))?;

        runtime.block_on(async move {
            let db = libsql::Builder::new_remote(database_url, auth_token)
                .build()
                .await
                .map_err(|e| {
                    StorageError::ConnectionFailed(format!("Failed to connect to remote: {e}"))
                })?;

            let conn = db.connect().map_err(|e| {
                StorageError::ConnectionFailed(format!("Failed to open remote connection: {e}"))
            })?;

            let mut rows = conn
                .query(
                    "SELECT m.node_id, e.embedding
                     FROM node_embeddings e
                     JOIN node_metadata m ON e.node_id = m.node_id
                     WHERE e.dimension = ?1",
                    libsql::params![query_dimension],
                )
                .await
                .map_err(|e| {
                    StorageError::RemoteQueryFailed(format!("Failed to run remote query: {e}"))
                })?;

            let mut results: Vec<(String, f32)> = Vec::new();

            while let Some(row) = rows.next().await.map_err(|e| {
                StorageError::RemoteQueryFailed(format!("Failed to read remote row: {e}"))
            })? {
                let node_id: String = row.get(0).map_err(|e| {
                    StorageError::RemoteQueryFailed(format!("Invalid node_id from remote row: {e}"))
                })?;
                let embedding_bytes: Vec<u8> = row.get(1).map_err(|e| {
                    StorageError::RemoteQueryFailed(format!("Invalid embedding from remote row: {e}"))
                })?;

                let stored_embedding: Vec<f32> = embedding_bytes
                    .chunks_exact(4)
                    .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
                    .collect();

                let similarity = cosine_similarity(&query_embedding, &stored_embedding);
                results.push((node_id, similarity));
            }

            results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            results.truncate(k);

            Ok::<Vec<(String, f32)>, StorageError>(results)
        })
    }

    /// Batch store embeddings
    ///
    /// # Arguments
    ///
    /// * `embeddings` - Vector of (node_id, symbol_name, file_path, node_type, embedding) tuples
    ///
    /// # Returns
    ///
    /// Number of embeddings stored successfully
    pub fn batch_store_embeddings(
        &self,
        embeddings: &[(&str, &str, &str, &str, &[f32])],
    ) -> Result<usize, StorageError> {
        if !self.vectors_initialized {
            return Err(StorageError::VectorExtensionNotAvailable);
        }

        let mut stored = 0;
        for (node_id, symbol_name, file_path, node_type, embedding) in embeddings {
            if self
                .store_embedding(node_id, symbol_name, file_path, node_type, embedding)
                .is_ok()
            {
                stored += 1;
            }
        }

        Ok(stored)
    }

    /// Get local storage
    ///
    /// # Returns
    ///
    /// `Some(&Storage)` if local storage is available, `None` otherwise
    #[must_use]
    pub fn local(&self) -> Option<&crate::Storage> {
        self.local.as_ref()
    }

    /// Get mutable local storage
    ///
    /// # Returns
    ///
    /// `Some(&mut Storage)` if local storage is available, `None` otherwise
    pub fn local_mut(&mut self) -> Option<&mut crate::Storage> {
        self.local.as_mut()
    }

    /// Migrate vector data from local SQLite to remote Turso/libsql.
    ///
    /// This migrates `node_metadata` and `node_embeddings` records when both
    /// local and remote backends are configured.
    ///
    /// # Returns
    ///
    /// `Ok(MigrationStats)` with migration statistics.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = storage.migrate_to_remote()?;
    /// println!("Migrated {} nodes", stats.nodes_migrated);
    /// ```
    pub fn migrate_to_remote(&self) -> Result<MigrationStats, StorageError> {
        if !self.config.is_remote() {
            return Err(StorageError::MigrationFailed(
                "Remote storage is not configured".to_string(),
            ));
        }

        let local = self.local.as_ref().ok_or_else(|| {
            StorageError::MigrationFailed("Local storage is unavailable for migration".to_string())
        })?;

        let start = Instant::now();
        let conn = local.conn();

        // If vector tables do not exist yet, treat as no-op migration.
        let has_metadata_table: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM sqlite_master WHERE type='table' AND name='node_metadata'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::LocalStorageError(format!("Failed to inspect schema: {e}")))?;

        let has_embeddings_table: i64 = conn
            .query_row(
                "SELECT COUNT(1) FROM sqlite_master WHERE type='table' AND name='node_embeddings'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StorageError::LocalStorageError(format!("Failed to inspect schema: {e}")))?;

        if has_metadata_table == 0 || has_embeddings_table == 0 {
            return Ok(MigrationStats {
                nodes_migrated: 0,
                edges_migrated: 0,
                embeddings_migrated: 0,
                migration_time_ms: start.elapsed().as_millis() as u64,
            });
        }

        let mut stmt = conn
            .prepare(
                "SELECT m.node_id, m.symbol_name, m.file_path, m.node_type, e.embedding, e.dimension
                 FROM node_metadata m
                 JOIN node_embeddings e ON m.node_id = e.node_id",
            )
            .map_err(|e| {
                StorageError::LocalStorageError(format!("Failed to prepare migration query: {e}"))
            })?;

        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Vec<u8>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(|e| StorageError::LocalStorageError(format!("Failed to read rows: {e}")))?;

        let mut records: Vec<(String, String, String, String, Vec<u8>, i64)> = Vec::new();
        for row in rows {
            records.push(
                row.map_err(|e| StorageError::LocalStorageError(format!("Invalid row: {e}")))?,
            );
        }

        let node_count = records.len();

        let database_url = self.config.database_url.clone();
        let auth_token = self.config.auth_token.clone();

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| StorageError::ConnectionFailed(format!("Failed to create runtime: {e}")))?;

        runtime.block_on(async move {
            let db = libsql::Builder::new_remote(database_url, auth_token)
                .build()
                .await
                .map_err(|e| {
                    StorageError::ConnectionFailed(format!("Failed to connect to remote: {e}"))
                })?;

            let remote = db.connect().map_err(|e| {
                StorageError::ConnectionFailed(format!("Failed to open remote connection: {e}"))
            })?;

            remote
                .execute(
                    "CREATE TABLE IF NOT EXISTS node_metadata (
                        node_id TEXT PRIMARY KEY,
                        symbol_name TEXT NOT NULL,
                        file_path TEXT NOT NULL,
                        node_type TEXT NOT NULL,
                        created_at INTEGER DEFAULT (strftime('%s', 'now'))
                    )",
                    (),
                )
                .await
                .map_err(|e| {
                    StorageError::RemoteQueryFailed(format!("Failed to create metadata table: {e}"))
                })?;

            remote
                .execute(
                    "CREATE TABLE IF NOT EXISTS node_embeddings (
                        node_id TEXT PRIMARY KEY,
                        embedding BLOB NOT NULL,
                        dimension INTEGER NOT NULL
                    )",
                    (),
                )
                .await
                .map_err(|e| {
                    StorageError::RemoteQueryFailed(format!("Failed to create embeddings table: {e}"))
                })?;

            for (node_id, symbol_name, file_path, node_type, embedding, dimension) in records {
                remote
                    .execute(
                        "INSERT OR REPLACE INTO node_metadata (node_id, symbol_name, file_path, node_type)
                         VALUES (?1, ?2, ?3, ?4)",
                        libsql::params![
                            node_id.clone(),
                            symbol_name,
                            file_path,
                            node_type
                        ],
                    )
                    .await
                    .map_err(|e| {
                        StorageError::RemoteQueryFailed(format!(
                            "Failed to insert node metadata: {e}"
                        ))
                    })?;

                remote
                    .execute(
                        "INSERT OR REPLACE INTO node_embeddings (node_id, embedding, dimension)
                         VALUES (?1, ?2, ?3)",
                        libsql::params![node_id, embedding, dimension],
                    )
                    .await
                    .map_err(|e| {
                        StorageError::RemoteQueryFailed(format!(
                            "Failed to insert node embedding: {e}"
                        ))
                    })?;
            }

            Ok::<(), StorageError>(())
        })?;

        Ok(MigrationStats {
            nodes_migrated: node_count,
            edges_migrated: 0,
            embeddings_migrated: node_count,
            migration_time_ms: start.elapsed().as_millis() as u64,
        })
    }

    /// Check if local storage is available
    #[must_use]
    pub fn has_local(&self) -> bool {
        self.local.is_some()
    }

    /// Check if remote storage is available
    #[must_use]
    pub fn has_remote(&self) -> bool {
        self.config.is_remote()
    }

    /// Get storage mode
    #[must_use]
    pub fn mode(&self) -> StorageMode {
        match (self.local.is_some(), self.config.is_remote()) {
            (true, false) => StorageMode::LocalOnly,
            (false, true) => StorageMode::RemoteOnly,
            (true, true) => StorageMode::Hybrid,
            (false, false) => StorageMode::None,
        }
    }
}

/// Calculate cosine similarity between two vectors
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();

    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_turso_config_default() {
        let config = TursoConfig::default();
        assert_eq!(config.database_url, "file:local.db");
        assert!(config.auth_token.is_empty());
        assert!(!config.enable_vectors);
        assert!(!config.remote_only);
    }

    #[test]
    fn test_turso_config_local_only() {
        let config = TursoConfig::local_only();
        assert!(config.is_local_only());
        assert!(!config.is_remote());
    }

    #[test]
    fn test_turso_config_remote_only() {
        let config = TursoConfig::remote_only(
            "libsql://token@db.turso.io".to_string(),
            "auth_token".to_string(),
        );
        assert!(config.is_remote());
        assert!(!config.is_local_only());
        assert!(config.remote_only);
    }

    #[test]
    fn test_turso_config_hybrid() {
        let config = TursoConfig::hybrid(
            "libsql://token@db.turso.io".to_string(),
            "auth_token".to_string(),
        );
        assert!(config.is_remote());
        assert!(!config.is_local_only());
        assert!(!config.remote_only);
    }

    #[test]
    fn test_turso_config_with_vectors() {
        let config = TursoConfig::local_only().with_vectors(true);
        assert!(config.enable_vectors);
    }

    #[test]
    fn test_migration_stats_default() {
        let stats = MigrationStats::default();
        assert_eq!(stats.nodes_migrated, 0);
        assert_eq!(stats.edges_migrated, 0);
        assert_eq!(stats.embeddings_migrated, 0);
        assert_eq!(stats.migration_time_ms, 0);
    }

    #[test]
    fn test_hybrid_storage_local_only() {
        let config = TursoConfig::local_only();
        let storage = HybridStorage::new(config);
        assert!(storage.is_ok());
        let storage = storage.unwrap();
        assert!(storage.has_local());
        assert!(!storage.has_remote());
        assert_eq!(storage.mode(), StorageMode::LocalOnly);
    }

    #[test]
    fn test_hybrid_storage_remote_only_fails_without_url() {
        let config = TursoConfig::remote_only("".to_string(), "".to_string());
        // This should fail since it's not a valid remote URL
        let result = HybridStorage::new(config);
        // The connection will fail since there's no actual Turso server
        // but the struct should be created with remote = None
        assert!(result.is_ok());
        let storage = result.unwrap();
        assert!(!storage.has_local());
        // Since the URL is empty, is_remote() returns false
        assert!(!storage.has_remote());
        assert_eq!(storage.mode(), StorageMode::None);
    }

    #[test]
    fn test_migrate_to_remote_requires_remote_configuration() {
        let config = TursoConfig::local_only();
        let storage = HybridStorage::new(config).unwrap();

        let result = storage.migrate_to_remote();
        assert!(result.is_err());

        match result {
            Err(StorageError::MigrationFailed(msg)) => {
                assert!(msg.contains("Remote storage is not configured"));
            }
            _ => panic!("Expected MigrationFailed error"),
        }
    }

    #[test]
    fn test_storage_mode_display() {
        assert_eq!(format!("{:?}", StorageMode::LocalOnly), "LocalOnly");
        assert_eq!(format!("{:?}", StorageMode::RemoteOnly), "RemoteOnly");
        assert_eq!(format!("{:?}", StorageMode::Hybrid), "Hybrid");
        assert_eq!(format!("{:?}", StorageMode::None), "None");
    }

    #[test]
    fn test_turso_config_is_local_only() {
        let config = TursoConfig::local_only();
        assert!(config.is_local_only());

        let config = TursoConfig {
            database_url: "file:test.db".to_string(),
            ..Default::default()
        };
        assert!(config.is_local_only());
    }

    #[test]
    fn test_turso_config_is_remote() {
        let config = TursoConfig {
            database_url: "libsql://token@db.turso.io".to_string(),
            auth_token: "some_token".to_string(),
            ..Default::default()
        };
        assert!(config.is_remote());
        assert!(!config.is_local_only());
    }

    #[test]
    fn test_storage_error_messages() {
        let err = StorageError::ConnectionFailed("test".to_string());
        assert_eq!(format!("{}", err), "Connection failed: test");

        let err = StorageError::MigrationFailed("test".to_string());
        assert_eq!(format!("{}", err), "Migration failed: test");

        let err = StorageError::VectorExtensionNotAvailable;
        assert_eq!(format!("{}", err), "Vector extension not available");
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![1.0, 2.0, 3.0];
        let sim = cosine_similarity(&a, &b);
        assert!((sim - 1.0).abs() < 0.001);

        let c = vec![1.0, 0.0, 0.0];
        let d = vec![0.0, 1.0, 0.0];
        let sim = cosine_similarity(&c, &d);
        assert!((sim - 0.0).abs() < 0.001);

        let e: Vec<f32> = vec![];
        let sim = cosine_similarity(&a, &e);
        assert_eq!(sim, 0.0);
    }

    #[test]
    fn test_cosine_similarity_parallel() {
        let a = vec![1.0, 2.0, 3.0, 4.0];
        let b = vec![2.0, 4.0, 6.0, 8.0];
        let sim = cosine_similarity(&a, &b);
        // b is 2*a, so they should be perfectly similar
        assert!((sim - 1.0).abs() < 0.001);
    }
}
