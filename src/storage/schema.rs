// Storage schema and database management

use crate::storage::{ProjectMetadata, UniqueProjectId};
use rusqlite::{Connection, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ============================================================================
// A+ SQLite budget constants (Section 5.2)
// ============================================================================

/// Global registry connection: thin cache, no mmap.
/// Single connection, rare access.
pub const GLOBAL_REGISTRY_CACHE_SIZE_KIB: i64 = -2000; // 2 MiB
/// Global registry mmap size: disabled (no mmap for global registry).
pub const GLOBAL_REGISTRY_MMAP_SIZE: i64 = 0;

/// Project writer connection: larger cache for hot write path.
pub const PROJECT_WRITER_CACHE_SIZE_KIB: i64 = -16000; // 16 MiB

/// Project reader connection: thin cache for point lookups.
pub const PROJECT_READER_CACHE_SIZE_KIB: i64 = -2000; // 2 MiB

/// Project store mmap cap (shared by writer and readers at OS level).
pub const PROJECT_STORE_MMAP_SIZE: i64 = 67_108_864; // 64 MiB

/// Storage configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StorageConfig {
    /// Database path
    pub db_path: String,

    /// Whether to enable WAL mode
    pub wal_enabled: bool,

    /// Cache size in KiB (negative = KiB units per SQLite convention).
    /// Defaults to the writer budget for backward compatibility.
    pub cache_size_kib: Option<i64>,

    /// mmap_size cap in bytes. Defaults to PROJECT_STORE_MMAP_SIZE.
    pub mmap_size: Option<i64>,
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            db_path: "leindex.db".to_string(),
            wal_enabled: true,
            cache_size_kib: Some(PROJECT_WRITER_CACHE_SIZE_KIB),
            mmap_size: Some(PROJECT_STORE_MMAP_SIZE),
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

        // Allow concurrent access: wait up to 5 seconds for locks instead of
        // immediately failing.  This is critical when multiple LeIndex instances
        // (or a ProjectRegistry) access the same project's .leindex/leindex.db.
        conn.pragma_update(None, "busy_timeout", 5000)?;

        // Set cache size if specified (negative = KiB per SQLite convention)
        if let Some(cache_size_kib) = config.cache_size_kib {
            conn.pragma_update(None, "cache_size", cache_size_kib)?;
        }

        // Set mmap_size cap if specified
        if let Some(mmap_size) = config.mmap_size {
            conn.pragma_update(None, "mmap_size", mmap_size)?;
        }

        let mut storage = Self { conn, config };

        // Check schema version BEFORE any DDL — reject newer databases early
        // so an older binary cannot corrupt a schema it doesn't understand.
        storage.run_migrations()?;

        // Initialize schema (CREATE TABLE IF NOT EXISTS — safe after version check)
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
    last_indexed TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
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
                updated_at INTEGER NOT NULL,
                embedding_format INTEGER
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

        if !columns.iter().any(|c| c == "embedding_format") {
            self.conn.execute(
                "ALTER TABLE intel_nodes ADD COLUMN embedding_format INTEGER",
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

        // Persistent cache telemetry for cross-session hit-rate tracking.
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS cache_telemetry (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                cache_hits INTEGER NOT NULL DEFAULT 0,
                cache_misses INTEGER NOT NULL DEFAULT 0,
                cache_writes INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            [],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO cache_telemetry (id, cache_hits, cache_misses, cache_writes, updated_at)
             VALUES (1, 0, 0, 0, strftime('%s', 'now'))",
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
        metadata
            .save(&self.conn)
            .map_err(|_| rusqlite::Error::InvalidQuery)
    }

    /// Current schema version. Increment when adding migrations.
    const SCHEMA_VERSION: u32 = 2;

    /// Run database migrations based on the stored schema version.
    /// Creates the version tracking table if it doesn't exist.
    fn run_migrations(&mut self) -> SqliteResult<()> {
        // Create version tracking table
        self.conn.execute(
            "CREATE TABLE IF NOT EXISTS schema_version (
                key TEXT PRIMARY KEY,
                version INTEGER NOT NULL
            )",
            [],
        )?;

        // Read current version
        let current: u32 = self
            .conn
            .query_row(
                "SELECT COALESCE(MAX(version), 0) FROM schema_version WHERE key = 'schema'",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        // Reject databases from newer versions — they may contain data
        // this version cannot interpret.
        if current > Self::SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "Database schema v{} is newer than this version (v{}). Please upgrade LeIndex.",
                current,
                Self::SCHEMA_VERSION
            )));
        }

        // Migration v1 to v2: Add last_indexed column to project_metadata
        if current < 2 {
            self.migrate_v1_to_v2()?;
        }

        // Update stored version
        self.conn.execute(
            "INSERT OR REPLACE INTO schema_version (key, version) VALUES ('schema', ?1)",
            [Self::SCHEMA_VERSION],
        )?;

        Ok(())
    }

    /// Migration from v1 to v2: Add last_indexed column to project_metadata table
    fn migrate_v1_to_v2(&mut self) -> SqliteResult<()> {
        let table_exists: bool = self.conn.query_row(
            "SELECT EXISTS(
                SELECT 1 FROM sqlite_master
                WHERE type = 'table' AND name = 'project_metadata'
            )",
            [],
            |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(());
        }

        // Check if column already exists
        let columns: Vec<String> = self
            .conn
            .prepare("PRAGMA table_info(project_metadata)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<SqliteResult<Vec<_>>>()?;

        if !columns.iter().any(|c| c == "last_indexed") {
            self.conn.execute(
                "ALTER TABLE project_metadata ADD COLUMN last_indexed TIMESTAMP DEFAULT CURRENT_TIMESTAMP",
                [],
            )?;
        }
        Ok(())
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
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND (name LIKE 'intel_%' OR name = 'analysis_cache' OR name = 'cache_telemetry' OR name LIKE 'global_%' OR name LIKE 'external_%' OR name LIKE 'project_%')",
                [],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(table_count, 8); // intel_nodes, intel_edges, analysis_cache, cache_telemetry, global_symbols, external_refs, project_deps, project_metadata
    }

    // A+ VAL-APLUS-007: Project writer SQLite connection uses the writer cache cap
    #[test]
    fn test_project_writer_cache_budget() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();

        let cache_size: i64 = storage
            .conn
            .query_row("PRAGMA cache_size", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            cache_size, PROJECT_WRITER_CACHE_SIZE_KIB,
            "project writer cache_size should be {} (16 MiB), got {}",
            PROJECT_WRITER_CACHE_SIZE_KIB, cache_size
        );
    }

    // A+ VAL-APLUS-009: Project store mmap cap is bounded to 64 MiB
    #[test]
    fn test_project_store_mmap_cap() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();

        let mmap_size: i64 = storage
            .conn
            .query_row("PRAGMA mmap_size", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            mmap_size, PROJECT_STORE_MMAP_SIZE,
            "project store mmap_size should be {} (64 MiB), got {}",
            PROJECT_STORE_MMAP_SIZE, mmap_size
        );
    }

    // A+ VAL-APLUS-008: Project reader SQLite connections use the thin reader cap
    #[test]
    fn test_project_reader_cache_budget() {
        // Verify the reader constant is the thin budget
        assert_eq!(
            PROJECT_READER_CACHE_SIZE_KIB, -2000,
            "reader cache should be -2000 (2 MiB thin budget)"
        );

        // Verify a connection opened with reader config gets the right pragma
        let temp_file = NamedTempFile::new().unwrap();
        let reader_config = StorageConfig {
            db_path: temp_file.path().to_string_lossy().to_string(),
            wal_enabled: true,
            cache_size_kib: Some(PROJECT_READER_CACHE_SIZE_KIB),
            mmap_size: Some(PROJECT_STORE_MMAP_SIZE),
        };
        let storage = Storage::open_with_config(temp_file.path(), reader_config).unwrap();

        let cache_size: i64 = storage
            .conn
            .query_row("PRAGMA cache_size", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            cache_size, PROJECT_READER_CACHE_SIZE_KIB,
            "reader cache_size should be {} (2 MiB), got {}",
            PROJECT_READER_CACHE_SIZE_KIB, cache_size
        );
    }
}
