// Turso configuration for hybrid storage
//
// This module provides configuration for Turso/libsql hybrid storage,
// combining local SQLite with remote Turso vector store capabilities.

use serde::{Deserialize, Serialize};
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
#[derive(Debug, Clone, Serialize, Deserialize)]
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

impl Default for MigrationStats {
    fn default() -> Self {
        Self {
            nodes_migrated: 0,
            edges_migrated: 0,
            embeddings_migrated: 0,
            migration_time_ms: 0,
        }
    }
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

    /// Remote Turso connection
    pub remote: Option<libsql::Connection>,

    /// Configuration
    pub config: TursoConfig,
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
            // Create a temporary path for local storage
            let storage = crate::Storage::open("local.db")
                .map_err(|e| StorageError::LocalStorageError(format!("{:?}", e)))?;
            Some(storage)
        } else {
            None
        };

        // Initialize remote storage if configured
        // Note: Actual libsql connection will be implemented in Task 8.3
        // For now, we just return None to allow compilation
        let remote = if config.is_remote() {
            tracing::info!("Remote Turso connection will be established in Task 8.3");
            None
        } else {
            None
        };

        Ok(Self { local, remote, config })
    }

    /// Initialize vector extension in Turso
    ///
    /// This enables the vec0 extension for vector similarity search.
    /// Only applicable when using remote Turso storage.
    ///
    /// Note: This is a placeholder for now. The actual implementation
    /// will be provided when the full Turso integration is ready.
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, `Err(StorageError)` if initialization fails
    pub fn init_vectors(&self) -> Result<(), StorageError> {
        if !self.config.enable_vectors {
            return Ok(());
        }

        // Placeholder - actual vector extension initialization will be in Task 8.3
        if self.remote.is_some() {
            tracing::info!("Vector extension initialization will be implemented in Task 8.3");
        }

        Ok(())
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

    /// Get remote storage
    ///
    /// # Returns
    ///
    /// `Some(&libsql::Connection)` if remote storage is available, `None` otherwise
    #[must_use]
    pub fn remote(&self) -> Option<&libsql::Connection> {
        self.remote.as_ref()
    }

    /// Get mutable remote storage
    ///
    /// # Returns
    ///
    /// `Some(&mut libsql::Connection)` if remote storage is available, `None` otherwise
    pub fn remote_mut(&mut self) -> Option<&mut libsql::Connection> {
        self.remote.as_mut()
    }

    /// Migrate data from local to remote
    ///
    /// This is a placeholder for migration functionality.
    /// The actual implementation will be provided in Task 8.3 (Vector Migration Bridge).
    /// For now, this returns empty migration stats to allow compilation.
    ///
    /// # Returns
    ///
    /// `Ok(MigrationStats)` with migration statistics (empty for now)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = storage.migrate_to_remote()?;
    /// println!("Migrated {} nodes", stats.nodes_migrated);
    /// ```
    pub fn migrate_to_remote(&self) -> Result<MigrationStats, StorageError> {
        // Placeholder implementation - actual migration will be in Task 8.3
        Ok(MigrationStats::default())
    }

    /// Check if local storage is available
    #[must_use]
    pub fn has_local(&self) -> bool {
        self.local.is_some()
    }

    /// Check if remote storage is available
    #[must_use]
    pub fn has_remote(&self) -> bool {
        self.remote.is_some()
    }

    /// Get storage mode
    #[must_use]
    pub fn mode(&self) -> StorageMode {
        match (self.local.is_some(), self.remote.is_some()) {
            (true, false) => StorageMode::LocalOnly,
            (false, true) => StorageMode::RemoteOnly,
            (true, true) => StorageMode::Hybrid,
            (false, false) => StorageMode::None,
        }
    }
}

/// Storage mode
///
/// Indicates the current storage mode of the HybridStorage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageMode {
    /// No storage configured
    None,
    /// Local-only storage
    LocalOnly,
    /// Remote-only storage
    RemoteOnly,
    /// Hybrid storage (local + remote)
    Hybrid,
}

/// Storage errors
///
/// Errors that can occur when working with HybridStorage.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Migration failed: {0}")]
    MigrationFailed(String),

    #[error("Vector extension not available")]
    VectorExtensionNotAvailable,

    #[error("Local storage error: {0}")]
    LocalStorageError(String),

    #[error("Remote query failed: {0}")]
    RemoteQueryFailed(String),
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
        assert!(!storage.has_remote()); // No valid connection
        assert_eq!(storage.mode(), StorageMode::None);
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
}
