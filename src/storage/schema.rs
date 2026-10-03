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
        // No migrations, no DDL, and no `schema_version` writes — the published
        // generation is treated as an immutable, already-initialized snapshot.
        // Validate the marker before accepting the handle so an incompatible
        // generation cannot be hydrated as if it were current.
        let current: u32 = conn.query_row(
            "SELECT COALESCE(MAX(version), 0) FROM schema_version WHERE key = 'schema'",
            [],
            |row| row.get(0),
        )?;
        if current != Self::SCHEMA_VERSION {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "Database schema v{} does not match this version (v{}).",
                current,
                Self::SCHEMA_VERSION
            )));
        }
        let config = StorageConfig {
            db_path: path.to_string_lossy().to_string(),
            wal_enabled: false,
            cache_size_kib: Some(-2000),
            mmap_size: Some(PROJECT_STORE_MMAP_SIZE),
        };
        // Apply the read-cache and mmap pragmas (connection-level settings; valid
        // on a read-only connection) so readers use the same memory profile as the
        // writer rather than SQLite defaults.
        if let Some(cache_size_kib) = config.cache_size_kib {
            conn.pragma_update(None, "cache_size", cache_size_kib)?;
        }
        if let Some(mmap_size) = config.mmap_size {
            conn.pragma_update(None, "mmap_size", mmap_size)?;
        }

        Ok(Self { conn, config })
    }
    /// Open storage with custom config
    pub fn open_with_config<P: AsRef<Path>>(path: P, config: StorageConfig) -> SqliteResult<Self> {
        let conn = Connection::open(path)?;

        // Enable WAL mode for better concurrency
        if config.wal_enabled {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            // With WAL mode, synchronous=NORMAL is safe: SQLite still guarantees
            // that committed transactions are durable across application crashes;
            // only a simultaneous OS crash + power loss can lose the last few
            // transactions. That is acceptable for a rebuildable code index and
            // eliminates most fsync calls on the write path.
            conn.pragma_update(None, "synchronous", "NORMAL")?;
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
        self.initialize_trigram_index_table()?;
        self.initialize_community_tables()
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
                embedding_format INTEGER,
                precision INTEGER NOT NULL DEFAULT 0
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
                // Content hash column required by save_pdg's unchanged-row
                // skip and by the idx_nodes_hash index; an ancient table
                // without it cannot even open (CREATE INDEX fails). Empty
                // hashes mark rows as changed, so the next save rewrites them.
                "content_hash",
                "ALTER TABLE intel_nodes ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''",
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
            // Timestamp columns used by the save_pdg upsert (pdg_store.rs).
            // Databases created before these columns existed in the CREATE
            // TABLE must be repaired here — `CREATE TABLE IF NOT EXISTS`
            // never adds columns to an existing table, and the save_pdg
            // INSERT references them by name, so a legacy schema would fail
            // every PDG save with "table intel_nodes has no column named
            // created_at". A default is required because SQLite cannot add
            // a NOT NULL column without one; 0 marks pre-migration rows and
            // is corrected on the next save_pdg upsert.
            (
                "created_at",
                "ALTER TABLE intel_nodes ADD COLUMN created_at INTEGER NOT NULL DEFAULT 0",
                None,
            ),
            (
                "updated_at",
                "ALTER TABLE intel_nodes ADD COLUMN updated_at INTEGER NOT NULL DEFAULT 0",
                None,
            ),
            (
                // Leiden community membership (roadmap Part IV). NULL = not
                // yet computed for this node's generation.
                "community_id",
                "ALTER TABLE intel_nodes ADD COLUMN community_id INTEGER",
                None,
            ),
            (
                // SCIP precision definition marker. Legacy rows remain Tier-0.
                "precision",
                "ALTER TABLE intel_nodes ADD COLUMN precision INTEGER NOT NULL DEFAULT 0",
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

    fn initialize_community_tables(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&["CREATE TABLE IF NOT EXISTS intel_communities (
                id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL,
                community INTEGER NOT NULL,
                algorithm TEXT NOT NULL,
                quality_name TEXT NOT NULL,
                resolution REAL NOT NULL,
                node_count INTEGER NOT NULL,
                quality_score REAL,
                label TEXT,
                computed_at INTEGER NOT NULL
            )"])
    }

    fn initialize_query_indexes(&self) -> SqliteResult<()> {
        self.execute_schema_statements(&[
            "CREATE INDEX IF NOT EXISTS idx_nodes_project ON intel_nodes(project_id)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_file ON intel_nodes(file_path)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_symbol ON intel_nodes(symbol_name)",
            "CREATE INDEX IF NOT EXISTS idx_nodes_hash ON intel_nodes(content_hash)",
            // Natural node key for the save_pdg upsert
            // (`ON CONFLICT(project_id, node_id) DO UPDATE`). The v3->v4
            // migration dedupes legacy rows before this index is created.
            "CREATE UNIQUE INDEX IF NOT EXISTS uq_intel_nodes_project_node ON intel_nodes(project_id, node_id)",
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
                updated_at INTEGER NOT NULL DEFAULT (strftime('%s', 'now')),
                content_hash TEXT NOT NULL DEFAULT ''
            )"])?;
        // CREATE TABLE IF NOT EXISTS never adds columns to an existing table;
        // stores created before the hash-skip need the column added (empty
        // hash ⇒ first save rewrites once and converges).
        let has_column = self
            .conn
            .prepare("PRAGMA table_info(trigram_index)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<SqliteResult<Vec<String>>>()?
            .iter()
            .any(|column| column == "content_hash");
        if !has_column {
            self.conn.execute(
                "ALTER TABLE trigram_index ADD COLUMN content_hash TEXT NOT NULL DEFAULT ''",
                [],
            )?;
        }
        // Leiden timing metric: CREATE IF NOT EXISTS never alters existing
        // tables, so the PRAGMA-check+ALTER repair applies here too.
        let telemetry_has_column = self
            .conn
            .prepare("PRAGMA table_info(cache_telemetry)")?
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<SqliteResult<Vec<String>>>()?
            .iter()
            .any(|column| column == "community_recompute_ms");
        if !telemetry_has_column {
            self.conn.execute(
                "ALTER TABLE cache_telemetry ADD COLUMN community_recompute_ms INTEGER NOT NULL DEFAULT 0",
                [],
            )?;
        }
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
    pub const SCHEMA_VERSION: u32 = 5;

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
        // Migration v3 to v4: dedupe intel_nodes so (project_id, node_id) can
        // become the upsert conflict target, then create the unique index.
        if current < 4 {
            self.migrate_v3_to_v4()?;
        }
        // Migration v4 to v5: backfill qualified_name for stores that were
        // already at v4 while the v3→v4 backfill was still gated behind it —
        // exactly the population holding the empty sentinel.
        if current < 5 {
            self.migrate_v4_to_v5()?;
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
    ///
    /// Runs before `initialize_schema`, so the `qualified_name` column (added
    /// to ancient stores by `ensure_intel_node_columns`) may not exist yet;
    /// the index is skipped in that case — `initialize_query_indexes`
    /// recreates it after the column repair.
    fn migrate_v2_to_v3(&mut self) -> SqliteResult<()> {
        let table_exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'intel_nodes')",
            [],
            |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(());
        }
        let columns = self.intel_node_column_names()?;
        if !columns.iter().any(|c| c == "qualified_name") {
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

    /// Migration from v3 to v4: unique (project_id, node_id) for the save_pdg
    /// upsert conflict target.
    ///
    /// Legacy rows predate the natural node key: `node_id` is either absent
    /// or backfilled from `symbol_name`, which repeats across files (e.g. two
    /// `init` functions). `ON CONFLICT(project_id, node_id)` requires a
    /// UNIQUE index, so duplicate keys must be resolved first. Rather than
    /// deleting the "duplicate" rows — which are genuinely distinct symbols
    /// (same name, different file) carrying real index data and edges — every
    /// non-minimal row of a duplicate group is **re-keyed** to the natural
    /// `<file_path>:<qualified_name>` form (with a final `:id` disambiguator
    /// for any residual collision, e.g. a true duplicate write). No row and
    /// no edge is lost; re-keyed rows are simply rewritten with their natural
    /// key at the next re-index of their file.
    ///
    /// This migration runs before `initialize_schema`, so it must be
    /// self-sufficient: the `node_id` / `qualified_name` columns are added
    /// here if the legacy table predates them.
    fn migrate_v3_to_v4(&mut self) -> SqliteResult<()> {
        let table_exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'intel_nodes')",
            [],
            |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(());
        }
        let columns = self.intel_node_column_names()?;
        for (name, addition) in [
            (
                "node_id",
                "ALTER TABLE intel_nodes ADD COLUMN node_id TEXT DEFAULT ''",
            ),
            (
                "qualified_name",
                "ALTER TABLE intel_nodes ADD COLUMN qualified_name TEXT DEFAULT ''",
            ),
        ] {
            if !columns.iter().any(|column| column == name) {
                self.conn.execute(addition, [])?;
            }
        }
        // The qualified_name backfill runs HERE as well: this migration adds
        // the column when the legacy table predates it, which makes
        // `ensure_intel_node_columns` see it as present and skip its own
        // repair — without this line every legacy row would keep
        // `qualified_name = ''` forever, invisible to the qualified-name
        // index and lookups. The two sentinel backfills run in batches so
        // each statement commits and releases the write lock instead of
        // holding it for one multi-million-row rewrite inside `open`; both
        // predicates shrink monotonically, so a crash mid-backfill resumes
        // (the stored schema version only advances after all migrations
        // return). The two dedup statements stay single-shot: batching them
        // re-evaluates the duplicate predicate against rows their own
        // earlier batches renamed, which can diverge instead of converging.
        self.backfill_column_batched("node_id", "symbol_name")?;
        self.backfill_column_batched("qualified_name", "symbol_name")?;
        self.conn.execute_batch(
            "UPDATE intel_nodes
                SET node_id = file_path || ':' || COALESCE(NULLIF(qualified_name, ''), symbol_name)
              WHERE id NOT IN (SELECT MIN(id) FROM intel_nodes GROUP BY project_id, node_id);
             UPDATE intel_nodes
                SET node_id = node_id || ':' || id
              WHERE id NOT IN (SELECT MIN(id) FROM intel_nodes GROUP BY project_id, node_id);",
        )?;
        Ok(())
    }

    /// Rewrite `column = ''` rows to `fallback` in bounded batches.
    ///
    /// Each batch is its own implicit transaction, so the SQLite write lock
    /// is released between batches and other connections can interleave —
    /// the unbatched single UPDATE held it for the whole rewrite of what is
    /// the largest table in the store, executed inside [`Storage::open`].
    /// The predicate shrinks every batch (rewritten rows no longer match),
    /// so the loop terminates and a crashed backfill resumes on the next
    /// open.
    fn backfill_column_batched(&mut self, column: &str, fallback: &str) -> SqliteResult<()> {
        if !matches!(
            (column, fallback),
            ("node_id" | "qualified_name", "symbol_name")
        ) {
            // Only the two caller-internal pairs are allowed; keep the SQL
            // below injection-proof by construction.
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "unsupported backfill pair {column} <- {fallback}"
            )));
        }
        const BATCH_ROWS: i64 = 10_000;
        loop {
            let changed = self.conn.execute(
                &format!(
                    "UPDATE intel_nodes SET {column} = {fallback} \
                     WHERE {column} = '' \
                       AND id IN (SELECT id FROM intel_nodes WHERE {column} = '' LIMIT {BATCH_ROWS})"
                ),
                [],
            )?;
            if changed == 0 {
                return Ok(());
            }
        }
    }

    /// Migration from v4 to v5: backfill `qualified_name` for stores that
    /// sat at v4 while the v3→v4 migration was adding the column without
    /// repairing it. Once that migration (or `ensure_intel_node_columns`)
    /// has added the column, the presence check in the column repair skips
    /// it — so every row of an already-v4 store keeps the empty sentinel
    /// and stays invisible to qualified-name lookups. This migration runs
    /// the backfill UNCONDITIONALLY on the affected population.
    fn migrate_v4_to_v5(&mut self) -> SqliteResult<()> {
        let table_exists: bool = self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'intel_nodes')",
            [],
            |row| row.get(0),
        )?;
        if !table_exists {
            return Ok(());
        }
        // Fresh stores at v3-and-below may predate the column entirely;
        // initialize_schema creates it with the table and their rows never
        // carry the sentinel.
        let has_column = self
            .intel_node_column_names()?
            .iter()
            .any(|column| column == "qualified_name");
        if !has_column {
            return Ok(());
        }
        // Batched: this is the population where every row matches the
        // sentinel, so the unbatched single UPDATE rewrote the whole table
        // under one write lock inside `open` (the exact shape the migration
        // guidelines call out). The predicate shrinks each batch and the
        // schema version only advances after this returns, so a crash
        // mid-backfill resumes on the next open.
        self.backfill_column_batched("qualified_name", "symbol_name")
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

        assert_eq!(table_count, 9); // intel_nodes, intel_edges, analysis_cache, cache_telemetry, global_symbols, external_refs, project_deps, project_metadata, intel_communities
    }

    #[test]
    fn test_v3_to_v4_migration_rekeys_duplicate_node_ids_without_deleting() {
        let temp_file = NamedTempFile::new().unwrap();

        // Simulate a legacy v3 database: open (creates v4 schema), then drop the
        // v4 unique index and downgrade the schema version so re-opening runs
        // the v3 -> v4 migration against real duplicate rows.
        {
            let mut storage = Storage::open(temp_file.path()).unwrap();
            storage
                .conn_mut()
                .execute_batch(
                    "DROP INDEX IF EXISTS uq_intel_nodes_project_node;
                     DELETE FROM schema_version WHERE key = 'schema';
                     INSERT INTO schema_version (key, version) VALUES ('schema', 3);
                     INSERT INTO intel_nodes
                       (project_id, file_path, node_id, symbol_name, qualified_name, language,
                        node_type, signature, complexity, content_hash, embedding,
                        byte_range_start, byte_range_end, created_at, updated_at, embedding_format)
                     VALUES
                       ('proj', 'a.rs', 'dup', 'f1', 'dup', 'rust', 'Function', NULL, 1, 'h1', NULL, 0, 10, 1, 1, 0),
                       ('proj', 'b.rs', 'dup', 'f2', 'dup', 'rust', 'Function', NULL, 2, 'h2', NULL, 0, 10, 1, 1, 0),
                       ('proj', 'c.rs', 'uniq', 'f3', 'uniq', 'rust', 'Function', NULL, 3, 'h3', NULL, 0, 10, 1, 1, 0);",
                )
                .unwrap();
            storage.close().expect("WAL checkpoint on close");
        }

        // Re-open: the v3 -> v4 migration re-keys duplicate (project_id,
        // node_id) rows to the natural `<file_path>:<qualified_name>` form —
        // the two 'dup' rows are genuinely distinct symbols in different
        // files, so deleting either would lose real index data — and
        // initialize_query_indexes recreates the unique index the upsert
        // depends on.
        let storage = Storage::open(temp_file.path()).unwrap();

        let rows: Vec<(i64, String, String)> = {
            let mut stmt = storage
                .conn()
                .prepare(
                    "SELECT id, node_id, symbol_name FROM intel_nodes WHERE project_id = 'proj' ORDER BY id",
                )
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        // All three rows survive; only the duplicate row in b.rs is re-keyed.
        assert_eq!(rows.len(), 3, "duplicate node_id rows must not be deleted");
        assert_eq!(rows[0], (1, "dup".to_string(), "f1".to_string()));
        assert_eq!(
            rows[1],
            (2, "b.rs:dup".to_string(), "f2".to_string()),
            "the duplicate is re-keyed to its natural file-qualified key"
        );
        assert_eq!(rows[2], (3, "uniq".to_string(), "f3".to_string()));

        let idx_count: i64 = storage
            .conn()
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name = 'uq_intel_nodes_project_node'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            idx_count, 1,
            "unique (project_id, node_id) index must exist"
        );
    }

    #[test]
    fn test_v3_to_v4_migration_opens_a_store_predating_node_id_and_edges() {
        // A v3 store may predate both the `node_id` column and the
        // `intel_edges` table. Migrations run before initialize_schema, so
        // the v3 -> v4 migration must add what it needs itself instead of
        // failing to open the store outright.
        let temp_file = NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(temp_file.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (key TEXT PRIMARY KEY, version INTEGER NOT NULL);
                 INSERT INTO schema_version (key, version) VALUES ('schema', 3);
                 CREATE TABLE intel_nodes (
                     id INTEGER PRIMARY KEY,
                     project_id TEXT NOT NULL,
                     file_path TEXT NOT NULL,
                     symbol_name TEXT NOT NULL,
                     node_type TEXT NOT NULL
                 );
                 INSERT INTO intel_nodes (project_id, file_path, symbol_name, node_type)
                 VALUES ('proj', 'a.rs', 'init', 'Function'), ('proj', 'b.rs', 'init', 'Function');",
            )
            .unwrap();
        }

        let storage = Storage::open(temp_file.path())
            .expect("an ancient v3 store must open and self-repair, not fail");

        let node_ids: Vec<(String, String, String)> = {
            let mut stmt = storage
                .conn()
                .prepare(
                    "SELECT file_path, node_id, qualified_name FROM intel_nodes ORDER BY file_path",
                )
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            node_ids,
            vec![
                ("a.rs".to_string(), "init".to_string(), "init".to_string()),
                (
                    "b.rs".to_string(),
                    "b.rs:init".to_string(),
                    "init".to_string()
                ),
            ],
            "same-named symbols in different files are re-keyed (not deleted) \
             and qualified_name is backfilled even though the migration added \
             the column itself"
        );
    }

    #[test]
    fn test_v4_to_v5_migration_backfills_qualified_name_for_already_v4_stores() {
        // The population the v5 migration exists for: a store already
        // recorded at v4 while the v3→v4 migration added the column without
        // running the backfill. `current < 4` gates meant the repair never
        // ran on upgrade — these rows kept `qualified_name = ''` forever.
        let temp_file = NamedTempFile::new().unwrap();
        {
            let conn = rusqlite::Connection::open(temp_file.path()).unwrap();
            conn.execute_batch(
                "CREATE TABLE schema_version (key TEXT PRIMARY KEY, version INTEGER NOT NULL);
                 INSERT INTO schema_version (key, version) VALUES ('schema', 4);
                 CREATE TABLE intel_nodes (
                     id INTEGER PRIMARY KEY,
                     project_id TEXT NOT NULL,
                     file_path TEXT NOT NULL,
                     node_id TEXT NOT NULL,
                     symbol_name TEXT NOT NULL,
                     qualified_name TEXT DEFAULT '',
                     node_type TEXT NOT NULL
                 );
                 INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, node_type)
                 VALUES ('proj', 'a.rs', 'a-symbol', 'alpha', '', 'Function'),
                        ('proj', 'b.rs', 'b-symbol', 'beta', 'kept', 'Function');",
            )
            .unwrap();
        }

        let storage =
            Storage::open(temp_file.path()).expect("a v4 store must open and backfill, not fail");

        let rows: Vec<(String, String)> = {
            let mut stmt = storage
                .conn()
                .prepare("SELECT symbol_name, qualified_name FROM intel_nodes ORDER BY symbol_name")
                .unwrap();
            stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                ("alpha".to_string(), "alpha".to_string()),
                ("beta".to_string(), "kept".to_string()),
            ],
            "the empty sentinel is backfilled on upgrade; a real qualified_name is untouched"
        );
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

    #[test]
    fn test_pragma_synchronous_is_normal() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();

        // synchronous=NORMAL returns integer 1 in SQLite.
        let sync_level: i64 = storage
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            sync_level, 1,
            "synchronous should be NORMAL (1) when WAL is enabled, got {sync_level}"
        );
    }

    #[test]
    fn test_pragma_journal_mode_still_wal() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();

        let journal_mode: String = storage
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            journal_mode.to_lowercase(),
            "wal",
            "journal_mode should remain WAL after adding synchronous=NORMAL"
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
