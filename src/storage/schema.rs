// Storage schema and database management

use crate::storage::{ProjectMetadata, UniqueProjectId};
use rusqlite::{Connection, OpenFlags, Result as SqliteResult};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Default number of reader connections in the fixed reader pool.
pub const DEFAULT_READER_POOL_SIZE: usize = 2;

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
    /// Open storage in read-only mode (no WAL, no migrations, no schema init)
    ///
    /// This is used for hydrating immutable generations (archived published runs).
    /// Does NOT enable WAL mode, does NOT run migrations, and does NOT initialize schema.
    /// The database is assumed to already exist and be fully initialized.
    pub fn open_readonly<P: AsRef<Path>>(path: P) -> SqliteResult<Self> {
        // Bind once so the generic `P` is consumed before we borrow it again
        // for `db_path` below (otherwise `open_with_flags` moves `path`).
        let path = path.as_ref();
        let conn = Connection::open_with_flags(
            path,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;

        // Short busy timeout so concurrent readers contend briefly, not forever.
        conn.pragma_update(None, "busy_timeout", 1000)?;

        // Read-only config: no WAL, a thin read cache, shared mmap at OS level.
        // No migrations, no DDL, no `schema_version` writes — the published
        // generation is treated as an immutable, already-initialized snapshot.
        let config = StorageConfig {
            db_path: path.to_string_lossy().to_string(),
            wal_enabled: false,
            cache_size_kib: Some(-2000),
            mmap_size: Some(PROJECT_STORE_MMAP_SIZE),
        };

        Ok(Self { conn, config })
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
        self.initialize_project_metadata_schema()?;
        self.initialize_core_tables()?;
        self.initialize_cache_tables()?;
        self.initialize_cross_project_tables()?;
        self.initialize_query_indexes()?;
        self.initialize_trigram_index_table()
    }

    fn execute_schema_statements(&self, statements: &[&str]) -> SqliteResult<()> {
        for statement in statements {
            self.conn.execute(statement, [])?;
        }
        Ok(())
    }

    fn initialize_project_metadata_schema(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&[
            r#"CREATE TABLE IF NOT EXISTS project_metadata (
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
)"#,
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_unique_id ON project_metadata(unique_project_id)",
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_canonical_path ON project_metadata(canonical_path)",
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_base_hash ON project_metadata(base_name, path_hash)",
            "CREATE INDEX IF NOT EXISTS idx_project_metadata_base_name ON project_metadata(base_name)",
        ])
    }

    fn initialize_core_tables(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&[
            "CREATE TABLE IF NOT EXISTS indexed_files (
                file_path TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                file_hash TEXT NOT NULL,
                last_indexed INTEGER NOT NULL
            )",
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
        ])?;
        self.ensure_intel_node_columns()
    }

    fn ensure_intel_node_columns(&self) -> SqliteResult<()> {
        let columns = self.intel_node_column_names()?;
        for (name, addition, repair) in [
            (
                "node_id",
                "ALTER TABLE intel_nodes ADD COLUMN node_id TEXT DEFAULT ''",
                Some("UPDATE intel_nodes SET node_id = symbol_name WHERE node_id = ''"),
            ),
            (
                "qualified_name",
                "ALTER TABLE intel_nodes ADD COLUMN qualified_name TEXT DEFAULT ''",
                Some(
                    "UPDATE intel_nodes SET qualified_name = symbol_name WHERE qualified_name = ''",
                ),
            ),
            (
                "language",
                "ALTER TABLE intel_nodes ADD COLUMN language TEXT DEFAULT 'unknown'",
                None,
            ),
            (
                "byte_range_start",
                "ALTER TABLE intel_nodes ADD COLUMN byte_range_start INTEGER",
                None,
            ),
            (
                "byte_range_end",
                "ALTER TABLE intel_nodes ADD COLUMN byte_range_end INTEGER",
                None,
            ),
            (
                "embedding_format",
                "ALTER TABLE intel_nodes ADD COLUMN embedding_format INTEGER",
                None,
            ),
        ] {
            self.ensure_intel_node_column(&columns, name, addition, repair)?;
        }
        Ok(())
    }

    fn intel_node_column_names(&self) -> SqliteResult<Vec<String>> {
        self.conn
            .prepare("PRAGMA table_info(intel_nodes)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect()
    }

    fn ensure_intel_node_column(
        &self,
        columns: &[String],
        name: &str,
        addition: &str,
        repair: Option<&str>,
    ) -> SqliteResult<()> {
        if columns.iter().any(|column| column == name) {
            return Ok(());
        }
        self.conn.execute(addition, [])?;
        if let Some(repair) = repair {
            self.conn.execute(repair, [])?;
        }
        Ok(())
    }

    fn initialize_cache_tables(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&[
            "CREATE TABLE IF NOT EXISTS intel_edges (
                caller_id INTEGER NOT NULL,
                callee_id INTEGER NOT NULL,
                edge_type TEXT NOT NULL,
                metadata TEXT,
                FOREIGN KEY(caller_id) REFERENCES intel_nodes(id),
                FOREIGN KEY(callee_id) REFERENCES intel_nodes(id),
                PRIMARY KEY(caller_id, callee_id, edge_type)
            )",
            "CREATE TABLE IF NOT EXISTS analysis_cache (
                node_hash TEXT PRIMARY KEY,
                cfg_data BLOB,
                complexity_metrics BLOB,
                timestamp INTEGER NOT NULL
            )",
            "CREATE TABLE IF NOT EXISTS cache_telemetry (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                cache_hits INTEGER NOT NULL DEFAULT 0,
                cache_misses INTEGER NOT NULL DEFAULT 0,
                cache_writes INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )",
            "INSERT OR IGNORE INTO cache_telemetry (id, cache_hits, cache_misses, cache_writes, updated_at)
             VALUES (1, 0, 0, 0, strftime('%s', 'now'))",
        ])
    }

    fn initialize_cross_project_tables(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&[
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
            "CREATE TABLE IF NOT EXISTS project_deps (
                dep_id TEXT PRIMARY KEY,
                project_id TEXT NOT NULL,
                depends_on_project_id TEXT NOT NULL,
                dependency_type TEXT NOT NULL,
                UNIQUE(project_id, depends_on_project_id)
            )",
        ])
    }

    fn initialize_query_indexes(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&[
            "CREATE INDEX IF NOT EXISTS idx_nodes_project ON intel_nodes(project_id)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_file ON intel_nodes(file_path)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_symbol ON intel_nodes(symbol_name)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_hash ON intel_nodes(content_hash)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_project_file_name ON intel_nodes(project_id, file_path, symbol_name COLLATE NOCASE)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_project_qualified ON intel_nodes(project_id, qualified_name COLLATE NOCASE)",
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_name ON global_symbols(symbol_name)",
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_type ON global_symbols(symbol_type)",
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_project ON global_symbols(project_id)",
            "CREATE INDEX IF NOT EXISTS idx_global_symbols_public ON global_symbols(symbol_id) WHERE is_public = 1",
            "CREATE INDEX IF NOT EXISTS idx_external_refs_source ON external_refs(source_symbol_id)",
            "CREATE INDEX IF NOT EXISTS idx_external_refs_target ON external_refs(target_symbol_id)",
            "CREATE INDEX IF NOT EXISTS idx_project_deps_project ON project_deps(project_id)",
        ])
    }

    fn initialize_trigram_index_table(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&["CREATE TABLE IF NOT EXISTS trigram_index (
                project_id TEXT PRIMARY KEY,
                index_data BLOB NOT NULL,
                node_count INTEGER NOT NULL DEFAULT 0,
                trigram_count INTEGER NOT NULL DEFAULT 0,
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now'))
            )"])
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
            self.conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
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
    pub const SCHEMA_VERSION: u32 = 3;

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
        if current < 3 {
            self.migrate_v2_to_v3()?;
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

    /// Migration from v2 to v3: bounded catalog point-lookup indexes.
    fn migrate_v2_to_v3(&mut self) -> SqliteResult<()> {
        let table_exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'intel_nodes')",
            [],
            |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(());
        }
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_nodes_project_file_name ON intel_nodes(project_id, file_path, symbol_name COLLATE NOCASE)",
            [],
        )?;
        self.conn.execute(
            "CREATE INDEX IF NOT EXISTS idx_nodes_project_qualified ON intel_nodes(project_id, qualified_name COLLATE NOCASE)",
            [],
        )?;
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
    fn close_checkpoints_wal_without_execute_return_error() {
        let temp_file = NamedTempFile::new().unwrap();
        let mut storage = Storage::open(temp_file.path()).unwrap();
        storage
            .conn_mut()
            .execute_batch(
                "CREATE TABLE close_probe(value INTEGER); INSERT INTO close_probe VALUES (1);",
            )
            .unwrap();
        storage
            .close()
            .expect("WAL checkpoint should close cleanly");
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

// ============================================================================
// B-phase fixed reader topology (VAL-BPHASE-026..028)
// ============================================================================

/// Role of a storage connection within the pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StorageRole {
    /// Writer connection: larger cache, write-capable.
    Writer,
    /// Reader connection: thin cache, read-only queries.
    Reader,
}

/// Fixed-size storage connection pool: one writer + a bounded reader pool.
///
/// This replaces ad-hoc `Storage::open` calls with a topology that keeps
/// SQLite residency bounded: one writer with a larger cache budget and a
/// fixed small set of thin-cache reader connections.
pub struct StoragePool {
    writer: Storage,
    readers: Vec<Storage>,
}

impl StoragePool {
    /// Open a storage pool at the given path with the specified writer and
    /// reader configurations.
    ///
    /// Creates one writer connection and `DEFAULT_READER_POOL_SIZE` reader
    /// connections, all pointing at the same database file.
    pub fn open<P: AsRef<Path>>(
        db_path: P,
        writer_config: StorageConfig,
        reader_config: StorageConfig,
    ) -> SqliteResult<Self> {
        let writer = Storage::open_with_config(&db_path, writer_config)?;

        let mut readers = Vec::with_capacity(DEFAULT_READER_POOL_SIZE);
        for _ in 0..DEFAULT_READER_POOL_SIZE {
            let reader = Storage::open_with_config(&db_path, reader_config.clone())?;
            readers.push(reader);
        }

        Ok(Self { writer, readers })
    }

    /// Open a storage pool with a custom reader pool size.
    pub fn open_with_pool_size<P: AsRef<Path>>(
        db_path: P,
        writer_config: StorageConfig,
        reader_config: StorageConfig,
        pool_size: usize,
    ) -> SqliteResult<Self> {
        let writer = Storage::open_with_config(&db_path, writer_config)?;

        let mut readers = Vec::with_capacity(pool_size);
        for _ in 0..pool_size {
            let reader = Storage::open_with_config(&db_path, reader_config.clone())?;
            readers.push(reader);
        }

        Ok(Self { writer, readers })
    }

    /// Returns true if the writer connection is present.
    pub fn has_writer(&self) -> bool {
        true // writer is always present after open
    }

    /// Get a reference to the writer connection.
    pub fn writer(&self) -> &Storage {
        &self.writer
    }

    /// Get a mutable reference to the writer connection.
    pub fn writer_mut(&mut self) -> &mut Storage {
        &mut self.writer
    }

    /// Get a reference to a reader connection by index.
    ///
    /// Returns an error if the index is out of bounds.
    pub fn reader(&self, index: usize) -> Result<&Storage, StoragePoolError> {
        self.readers
            .get(index)
            .ok_or(StoragePoolError::ReaderOutOfBounds {
                index,
                pool_size: self.readers.len(),
            })
    }

    /// Get a mutable reference to a reader connection by index.
    pub fn reader_mut(&mut self, index: usize) -> Result<&mut Storage, StoragePoolError> {
        let pool_size = self.readers.len();
        self.readers
            .get_mut(index)
            .ok_or(StoragePoolError::ReaderOutOfBounds { index, pool_size })
    }

    /// Number of reader connections in the pool.
    pub fn reader_count(&self) -> usize {
        self.readers.len()
    }

    /// Close all connections in the pool.
    pub fn close_all(&mut self) -> SqliteResult<()> {
        self.writer.close()?;
        for reader in &mut self.readers {
            reader.close()?;
        }
        Ok(())
    }
}

/// Errors from storage pool operations.
#[derive(Debug, thiserror::Error)]
pub enum StoragePoolError {
    /// Requested reader index exceeds the fixed pool size.
    #[error("reader index {index} out of bounds (pool size: {pool_size})")]
    ReaderOutOfBounds {
        /// Requested index.
        index: usize,
        /// Actual pool size.
        pool_size: usize,
    },
}
