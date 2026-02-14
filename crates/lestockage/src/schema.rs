// Storage schema and database management

use rusqlite::{Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::Path;
use crate::{ProjectMetadata, UniqueProjectId};

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Database path
    pub db_path: String,

    /// Whether to enable WAL mode
    pub wal_enabled: bool,

    /// Cache size in pages
    pub cache_size_pages: Option<usize>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: "leindex.db".to_string(),
            wal_enabled: true,
            cache_size_pages: Some(10000),
        }
    }
}

/// Main storage interface
pub struct Storage {
    conn: Connection,
    #[allow(dead_code)]
    config: StorageConfig,
}

impl Storage {
    /// Open storage with default config
    pub fn open<P: AsRef<Path>>(path: P) -> SqliteResult<Self> {
        Self::open_with_config(path, StorageConfig::default())
    }

    /// Open storage with custom config
    pub fn open_with_config<P: AsRef<Path>>(path: P, config: StorageConfig) -> SqliteResult<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrency
        if config.wal_enabled {
            conn.pragma_update(None, "journal_mode", "WAL")?;
        }

        // Set cache size if specified
        if let Some(cache_size) = config.cache_size_pages {
            conn.pragma_update(None, "cache_size", cache_size)?;
        }

        let mut storage = Self { conn, config };

        // Initialize schema
        storage.initialize_schema()?;

        Ok(storage)
    }

    /// Initialize database schema
    fn initialize_schema(&mut self) -> SqliteResult<()> {
        // Initialize project_metadata table first
        // SQL schema for project_metadata table
        let project_metadata_schema = r#"
CREATE TABLE IF NOT EXISTS project_metadata (
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
    UNIQUE(canonical_path)
)
"#;

        // SQL indexes for project_metadata table
        let project_metadata_indexes = [
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_unique_id ON project_metadata(unique_project_id)",
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_canonical_path ON project_metadata(canonical_path)",
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_base_hash ON project_metadata(base_name, path_hash)",
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_base_name ON project_metadata(base_name)",
        ];

        self.conn.execute(project_metadata_schema, [])?;
        for index_sql in project_metadata_indexes {
            self.conn.execute(index_sql, [])?;
        }

        // Create indexed_files table for incremental indexing
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS indexed_files (
                file_path TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                file_hash TEXT NOT NULL,
                last_indexed INTEGER NOT NULL
            )",
            [],
        )?;

        // Create intel_nodes table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS intel_nodes (
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
                updated_at INTEGER NOT NULL
            )",
            [],
        )?;

        // Migration: Ensure new columns exist for existing databases
        let columns: Vec<String> = self
            .conn
            .prepare("PRAGMA table_info(intel_nodes)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<SqliteResult<Vec<_>>>()?;

        if !columns.iter().any(|c| c == "node_id") {
            self.conn.execute(
                "ALTER TABLE intel_nodes ADD COLUMN node_id TEXT DEFAULT ''",
                [],
            )?;
            // Update node_id with symbol_name for existing records
            self.conn.execute(
                "UPDATE intel_nodes SET node_id = symbol_name WHERE node_id = ''",
                [],
            )?;
        }
        if !columns.iter().any(|c| c == "qualified_name") {
            self.conn.execute(
                "ALTER TABLE intel_nodes ADD COLUMN qualified_name TEXT DEFAULT ''",
                [],
            )?;
            self.conn.execute(
                "UPDATE intel_nodes SET qualified_name = symbol_name WHERE qualified_name = ''",
                [],
            )?;
        }
        if !columns.iter().any(|c| c == "language") {
            self.conn.execute(
                "ALTER TABLE intel_nodes ADD COLUMN language TEXT DEFAULT 'unknown'",
                [],
            )?;
        }
        if !columns.iter().any(|c| c == "byte_range_start") {
            self.conn.execute(
                "ALTER TABLE intel_nodes ADD COLUMN byte_range_start INTEGER",
                [],
            )?;
        }
        if !columns.iter().any(|c| c == "byte_range_end") {
            self.conn.execute(
                "ALTER TABLE intel_nodes ADD COLUMN byte_range_end INTEGER",
                [],
            )?;
        }

        // Create intel_edges table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS intel_edges (
                caller_id INTEGER NOT NULL,
                callee_id INTEGER NOT NULL,
                edge_type TEXT NOT NULL,
                metadata TEXT,
                FOREIGN KEY(caller_id) REFERENCES intel_nodes(id),
                FOREIGN KEY(callee_id) REFERENCES intel_nodes(id),
                PRIMARY KEY(caller_id, callee_id, edge_type)
            )",
            [],
        )?;

        // Create analysis_cache table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS analysis_cache (
                node_hash TEXT PRIMARY KEY,
                cfg_data BLOB,
                complexity_metrics BLOB,
                timestamp INTEGER NOT NULL
            )",
            [],
        )?;

        // Create global_symbols table (Phase 7: Cross-Project Resolution)
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS global_symbols (
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
            )",
            [],
        )?;

        // Create external_refs table (Phase 7: Cross-Project Resolution)
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS external_refs (
                ref_id TEXT PRIMARY KEY,
                source_project_id TEXT NOT NULL,
                source_symbol_id TEXT NOT NULL,
                target_project_id TEXT NOT NULL,
                target_symbol_id TEXT NOT NULL,
                ref_type TEXT NOT NULL,
                FOREIGN KEY (source_symbol_id) REFERENCES global_symbols(symbol_id),
                FOREIGN KEY (target_symbol_id) REFERENCES global_symbols(symbol_id)
            )",
            [],
        )?;

        // Create project_deps table (Phase 7: Cross-Project Resolution)
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS project_deps (
                dep_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                depends_on_project_id TEXT NOT NULL,
                dependency_type TEXT NOT NULL,
                UNIQUE(project_id, depends_on_project_id)
            )",
            [],
        )?;

        // Create indexes for query performance
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_nodes_project ON intel_nodes(project_id)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_nodes_file ON intel_nodes(file_path)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_nodes_symbol ON intel_nodes(symbol_name)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_nodes_hash ON intel_nodes(content_hash)",
            [],
        )?;

        // Create indexes for global_symbols (Phase 7)
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_name ON global_symbols(symbol_name)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_type ON global_symbols(symbol_type)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_project ON global_symbols(project_id)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_public ON global_symbols(symbol_id) WHERE is_public = 1",
            [],
        )?;

        // Create indexes for external_refs (Phase 7)
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_external_refs_source ON external_refs(source_symbol_id)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_external_refs_target ON external_refs(target_symbol_id)",
            [],
        )?;

        // Create indexes for project_deps (Phase 7)
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_project_deps_project ON project_deps(project_id)",
            [],
        )?;

        Ok(())
    }

    /// Get the underlying connection
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Get mutable connection
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }

    /// Close the storage connection and ensure WAL is checkpointed
    ///
    /// This explicitly checkpoints the WAL (Write-Ahead Log) to the main database file
    /// and closes the SQLite connection. This should be called before switching projects
    /// to ensure file locks are released properly.
    pub fn close(&mut self) -> SqliteResult<()> {
        // Force WAL checkpoint to ensure all data is written to main DB
        // This releases locks on the -wal and -shm files
        if self.config.wal_enabled {
            self.conn.execute("PRAGMA wal_checkpoint(TRUNCATE)", [])?;
        }
        // Optionally run optimize to clean up the database file
        // self.conn.execute("PRAGMA optimize", [])?;
        Ok(())
    }

    /// Load existing project IDs for a given base name.
    ///
    /// This is used for unique project ID generation to avoid conflicts.
    pub fn load_existing_ids(&self, base_name: &str) -> SqliteResult<Vec<UniqueProjectId>> {
        ProjectMetadata::load_existing_ids(&self.conn, base_name)
            .map_err(|_| rusqlite::Error::InvalidQuery)
    }
    
    /// Store project metadata.
    ///
    /// This persists the unique project ID and associated metadata.
    pub fn store_project_metadata(
        &self,
        unique_id: &UniqueProjectId,
        project_path: &Path,
    ) -> SqliteResult<()> {
        let metadata = ProjectMetadata::new(project_path);
        // Override with the provided unique_id
        let metadata = ProjectMetadata {
            unique_project_id: unique_id.clone(),
            ..metadata
        };
        metadata.save(&self.conn).map_err(|_| rusqlite::Error::InvalidQuery)
    }

}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_storage_creation() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path());
        assert!(storage.is_ok());
    }

    #[test]
    fn test_schema_initialization() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();

        // Check that tables exist
        let table_count: i64 = storage
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND (name LIKE 'intel_%' OR name = 'analysis_cache' OR name LIKE 'global_%' OR name LIKE 'external_%' OR name LIKE 'project_%')",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(table_count, 7); // intel_nodes, intel_edges, analysis_cache, global_symbols, external_refs, project_deps, project_metadata
    }
}
