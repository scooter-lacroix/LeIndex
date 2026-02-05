//!
//! # Database Migration Framework
//!
//! A comprehensive migration system for SQLite/DuckDB/Tantivy to Turso migrations.
//! Provides async migration management with rollback support, version tracking,
//! and state management compatible with libsql.
//!
//! ## Features
//!
//! - **Async Migration Support**: Full async/await pattern compatible with libsql
//! - **Rollback Capability**: Each migration can define a `down()` method for rollback
//! - **Version Tracking**: Semantic versioning system for migration identification
//! - **State Management**: Persistent migration state tracking in the database
//! - **Idempotent Operations**: Safe to run multiple times without side effects
//! - **Error Handling**: Comprehensive error handling with anyhow::Result
//!
//! ## Usage
//!
//! ```rust
//! use migrations::{Migration, MigrationManager};
//!
//! #[derive(Debug)]
//! struct CreateUsersTable;
//!
//! #[async_trait::async_trait]
//! impl Migration for CreateUsersTable {
//!     fn version(&self) -> &str {
//!         "2024_01_01_001_create_users_table"
//!     }
//!
//!     async fn up(&self, conn: &libsql::Connection) -> anyhow::Result<()> {
//!         // Execute migration up
//!         Ok(())
//!     }
//!
//!     async fn down(&self, conn: &libsql::Connection) -> anyhow::Result<()> {
//!         // Execute migration rollback
//!         Ok(())
//!     }
//! }
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     let db = libsql::Database::open("app.db")?;
//!     let conn = db.connect()?;
//!     let manager = MigrationManager::new(conn);
//!     manager.run_migrations(&[CreateUsersTable]).await?;
//!     Ok(())
//! }
//! ```
//!

use anyhow::{Context, Result};
use async_trait::async_trait;
use libsql::Connection;
use std::fmt;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Represents the current state of a migration.
///
/// This enum tracks whether a migration is pending, has been applied,
/// or has been rolled back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MigrationState {
    /// Migration has not been applied yet
    Pending,
    /// Migration has been successfully applied
    Applied,
    /// Migration was rolled back after being applied
    RolledBack,
}

impl fmt::Display for MigrationState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MigrationState::Pending => write!(f, "PENDING"),
            MigrationState::Applied => write!(f, "APPLIED"),
            MigrationState::RolledBack => write!(f, "ROLLED_BACK"),
        }
    }
}

/// Metadata about a migration's execution state.
///
/// This struct holds information about when a migration was applied,
/// its current state, and any associated metadata.
#[derive(Debug, Clone)]
pub struct MigrationRecord {
    /// Unique version identifier for the migration
    pub version: String,
    /// Current state of the migration
    pub state: MigrationState,
    /// Timestamp when the migration was applied (RFC3339 format)
    pub applied_at: Option<String>,
    /// Timestamp when the migration was rolled back (RFC3339 format)
    pub rolled_back_at: Option<String>,
    /// Optional checksum for integrity verification
    pub checksum: Option<String>,
}

impl MigrationRecord {
    /// Creates a new migration record with pending state.
    ///
    /// # Arguments
    ///
    /// * `version` - The migration version string
    ///
    /// # Returns
    ///
    /// A new `MigrationRecord` instance in pending state.
    #[must_use]
    pub fn new_pending(version: String) -> Self {
        Self {
            version,
            state: MigrationState::Pending,
            applied_at: None,
            rolled_back_at: None,
            checksum: None,
        }
    }

    /// Marks the migration as applied with current timestamp.
    ///
    /// # Arguments
    ///
    /// * `checksum` - Optional checksum for integrity verification
    ///
    /// # Returns
    ///
    /// Self for method chaining.
    #[must_use]
    pub fn mark_applied(&mut self, checksum: Option<String>) -> &mut Self {
        self.state = MigrationState::Applied;
        self.applied_at = Some(chrono::Utc::now().to_rfc3339());
        self.checksum = checksum;
        self
    }

    /// Marks the migration as rolled back with current timestamp.
    ///
    /// # Returns
    ///
    /// Self for method chaining.
    #[must_use]
    pub fn mark_rolled_back(&mut self) -> &mut Self {
        self.state = MigrationState::RolledBack;
        self.rolled_back_at = Some(chrono::Utc::now().to_rfc3339());
        self
    }
}

/// Trait representing a database migration.
///
/// Implement this trait to define migrations with `up` (apply) and
/// `down` (rollback) operations. Each migration must have a unique
/// version string for identification and ordering.
///
/// # Example
///
/// ```rust
/// use migrations::{Migration, MigrationRecord};
/// use libsql::Connection;
///
/// struct MyMigration;
///
/// #[async_trait::async_trait]
/// impl Migration for MyMigration {
///     fn version(&self) -> &str {
///         "2024_01_15_001_create_users"
///     }
///
///     async fn up(&self, conn: &Connection) -> anyhow::Result<()> {
///         // Apply migration
///         Ok(())
///     }
///
///     async fn down(&self, conn: &Connection) -> anyhow::Result<()> {
///         // Rollback migration
///         Ok(())
///     }
/// }
/// ```
#[async_trait]
pub trait Migration: Send + Sync {
    /// Returns the unique version identifier for this migration.
    ///
    /// Version strings should follow a consistent format for sorting.
    /// Recommended format: `YYYY_MM_DD_NNN descriptive_name`
    ///
    /// # Returns
    ///
    /// A string slice containing the migration version.
    fn version(&self) -> &str;

    /// Applies the migration to the database.
    ///
    /// This method is called when the migration should be applied.
    /// It should contain all necessary SQL statements or operations
    /// to implement the desired database changes.
    ///
    /// # Arguments
    ///
    /// * `conn` - Reference to the libsql connection
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error describing what went wrong.
    async fn up(&self, conn: &Connection) -> Result<()>;

    /// Rolls back the migration.
    ///
    /// This method should reverse the changes made by `up()`.
    /// It is called when rolling back a migration. If a migration
    /// cannot be rolled back, return an error.
    ///
    /// # Arguments
    ///
    /// * `conn` - Reference to the libsql connection
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error describing what went wrong.
    async fn down(&self, conn: &Connection) -> Result<()>;

    /// Returns a checksum for integrity verification.
    ///
    /// This can be used to verify that a migration has not been modified
    /// since it was applied. By default, returns the migration's version string.
    ///
    /// # Returns
    ///
    /// A string containing the migration checksum.
    fn checksum(&self) -> String {
        self.version().to_string()
    }

    /// Returns a human-readable description of this migration.
    ///
    /// # Returns
    ///
    /// A string describing what this migration does.
    fn description(&self) -> String {
        format!("Migration: {}", self.version())
    }
}

/// Configuration for the migration manager.
#[derive(Debug, Clone)]
pub struct MigrationConfig {
    /// Table name for storing migration records
    pub migrations_table: String,
    /// Whether to create the migrations table if it doesn't exist
    pub create_table_if_missing: bool,
    /// Whether to run migrations in a transaction
    pub use_transactions: bool,
    /// Whether to verify checksums after applying migrations
    pub verify_checksums: bool,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        Self {
            migrations_table: "schema_migrations".to_string(),
            create_table_if_missing: true,
            use_transactions: true,
            verify_checksums: true,
        }
    }
}

/// Manages database migrations with support for applying and rolling back.
///
/// The `MigrationManager` is the main entry point for running migrations.
/// It tracks applied migrations, handles versioning, and provides rollback
/// capabilities.
///
/// # Example
///
/// ```rust
/// use migrations::{Migration, MigrationManager};
/// use libsql::Connection;
///
/// #[derive(Debug)]
/// struct InitialSchema;
///
/// #[async_trait::async_trait]
/// impl Migration for InitialSchema {
///     fn version(&self) -> &str {
///         "2024_01_01_001_initial_schema"
///     }
///
///     async fn up(&self, conn: &Connection) -> anyhow::Result<()> {
///         Ok(())
///     }
///
///     async fn down(&self, conn: &Connection) -> anyhow::Result<()> {
///         Ok(())
///     }
/// }
///
/// #[tokio::main]
/// async fn main() -> anyhow::Result<()> {
///     let db = libsql::Database::open("app.db")?;
///     let conn = db.connect()?;
///     let manager = MigrationManager::new(conn);
///
///     // Run all migrations
///     manager.run_migrations(&[InitialSchema]).await?;
///
///     // Get migration status
///     let status = manager.get_status().await?;
///     println!("Applied migrations: {}", status.applied_count());
///
///     Ok(())
/// }
/// ```
pub struct MigrationManager {
    /// Database connection (thread-safe via Arc)
    conn: Arc<Connection>,
    /// Configuration for migration behavior
    config: MigrationConfig,
    /// Internal mutex for thread-safe migration operations
    state_mutex: Mutex<()>,
}

impl MigrationManager {
    /// Creates a new migration manager with default configuration.
    ///
    /// # Arguments
    ///
    /// * `conn` - The libsql connection
    ///
    /// # Returns
    ///
    /// A new `MigrationManager` instance.
    #[must_use]
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(conn),
            config: MigrationConfig::default(),
            state_mutex: Mutex::new(()),
        }
    }

    /// Creates a new migration manager with custom configuration.
    ///
    /// # Arguments
    ///
    /// * `conn` - The libsql connection
    /// * `config` - Custom migration configuration
    ///
    /// # Returns
    ///
    /// A new `MigrationManager` instance.
    #[must_use]
    pub fn with_config(conn: Connection, config: MigrationConfig) -> Self {
        Self {
            conn: Arc::new(conn),
            config,
            state_mutex: Mutex::new(()),
        }
    }

    /// Initializes the migrations table if it doesn't exist.
    ///
    /// This method is called automatically before running migrations
    /// if `create_table_if_missing` is enabled in the config.
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error.
    pub async fn initialize(&self) -> Result<()> {
        let table_name = &self.config.migrations_table;

        // Create migrations tracking table
        let create_sql = format!(
            r#"
            CREATE TABLE IF NOT EXISTS {} (
                version TEXT PRIMARY KEY,
                state TEXT NOT NULL DEFAULT 'PENDING',
                applied_at TEXT,
                rolled_back_at TEXT,
                checksum TEXT,
                description TEXT
            )
            "#,
            table_name
        );

        // Create index for faster lookups
        let create_index_sql = format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_{}_state ON {} (state)
            "#,
            table_name.replace(".", "_"),
            table_name
        );

        // Create applied_at index for ordering
        let create_applied_index_sql = format!(
            r#"
            CREATE INDEX IF NOT EXISTS idx_{}_applied_at ON {} (applied_at)
            "#,
            table_name.replace(".", "_"),
            table_name
        );

        let conn = self.conn.as_ref();

        conn.execute(&create_sql, ()).await.with_context(|| {
            format!("Failed to create migrations table: {}", table_name)
        })?;

        conn.execute(&create_index_sql, ()).await.with_context(|| {
            format!("Failed to create migrations state index")
        })?;

        conn.execute(&create_applied_index_sql, ()).await.with_context(|| {
            format!("Failed to create migrations applied_at index")
        })?;

        Ok(())
    }

    /// Gets all migration records from the database.
    ///
    /// # Returns
    ///
    /// A vector of `MigrationRecord` instances, or an error.
    pub async fn get_migration_records(&self) -> Result<Vec<MigrationRecord>> {
        let table_name = &self.config.migrations_table;

        let sql = format!(
            r#"
            SELECT version, state, applied_at, rolled_back_at, checksum
            FROM {}
            ORDER BY applied_at ASC
            "#,
            table_name
        );

        let conn = self.conn.as_ref();
        let mut rows = conn.query(&sql, ()).await.with_context(|| {
            format!("Failed to query migration records from: {}", table_name)
        })?;

        let mut records = Vec::new();

        while let Some(row) = rows.next().await? {
            let version: String = row.get(0)?;
            let state: String = row.get(1)?;
            let applied_at: Option<String> = row.get(2)?;
            let rolled_back_at: Option<String> = row.get(3)?;
            let checksum: Option<String> = row.get(4)?;

            let state_enum = match state.as_str() {
                "APPLIED" => MigrationState::Applied,
                "ROLLED_BACK" => MigrationState::RolledBack,
                _ => MigrationState::Pending,
            };

            records.push(MigrationRecord {
                version,
                state: state_enum,
                applied_at,
                rolled_back_at,
                checksum,
            });
        }

        Ok(records)
    }

    /// Checks if a migration has been applied.
    ///
    /// # Arguments
    ///
    /// * `version` - The migration version to check
    ///
    /// # Returns
    ///
    /// `true` if the migration is applied, `false` otherwise.
    pub async fn is_migration_applied(&self, version: &str) -> Result<bool> {
        let table_name = &self.config.migrations_table;

        let sql = format!(
            r#"
            SELECT 1 FROM {}
            WHERE version = ?1 AND state = 'APPLIED'
            LIMIT 1
            "#,
            table_name
        );

        let mut rows = self.conn.query(&sql, [version]).await.with_context(|| {
            format!("Failed to check migration status for: {}", version)
        })?;

        Ok(rows.next().await?.is_some())
    }

    /// Records a migration as applied in the database.
    ///
    /// # Arguments
    ///
    /// * `migration` - The migration to record
    /// * `checksum` - The migration checksum
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error.
    async fn record_migration_applied(&self, migration: &dyn Migration, checksum: String) -> Result<()> {
        let table_name = &self.config.migrations_table;
        let description = migration.description();
        let version = migration.version();

        let sql = format!(
            r#"
            INSERT INTO {} (version, state, applied_at, checksum, description)
            VALUES (?1, 'APPLIED', datetime('now'), ?2, ?3)
            ON CONFLICT(version) DO UPDATE SET
                state = 'APPLIED',
                applied_at = datetime('now'),
                checksum = excluded.checksum
            "#,
            table_name
        );

        self.conn.execute(&sql, [&*version, &*checksum, &*description]).await.with_context(|| {
            format!("Failed to record migration as applied: {}", migration.version())
        })?;

        Ok(())
    }

    /// Records a migration as rolled back in the database.
    ///
    /// # Arguments
    ///
    /// * `version` - The migration version to record
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error.
    async fn record_migration_rolled_back(&self, version: &str) -> Result<()> {
        let table_name = &self.config.migrations_table;

        let sql = format!(
            r#"
            UPDATE {}
            SET state = 'ROLLED_BACK', rolled_back_at = datetime('now')
            WHERE version = ?1
            "#,
            table_name
        );

        self.conn.execute(&sql, [version]).await.with_context(|| {
            format!("Failed to record migration as rolled back: {}", version)
        })?;

        Ok(())
    }

    /// Runs a single migration up (apply).
    ///
    /// # Arguments
    ///
    /// * `migration` - The migration to apply
    /// * `force` - Whether to force re-application even if already applied
    ///
    /// # Returns
    ///
    /// `Ok(true)` if migration was applied, `Ok(false)` if skipped,
    /// or an error.
    pub async fn migrate_up(&self, migration: &dyn Migration, force: bool) -> Result<bool> {
        let _guard = self.state_mutex.lock().await;

        let version = migration.version();
        let is_applied = self.is_migration_applied(version).await?;

        if is_applied && !force {
            return Ok(false);
        }

        // Apply the migration
        migration.up(self.conn.as_ref()).await.with_context(|| {
            format!("Failed to apply migration: {}", version)
        })?;

        // Record the migration
        let checksum = migration.checksum();
        self.record_migration_applied(migration, checksum).await?;

        tracing::info!("Applied migration: {}", version);

        Ok(true)
    }

    /// Runs a single migration down (rollback).
    ///
    /// # Arguments
    ///
    /// * `migration` - The migration to rollback
    ///
    /// # Returns
    ///
    /// `Ok(true)` if migration was rolled back, `Ok(false)` if not applied,
    /// or an error.
    pub async fn migrate_down(&self, migration: &dyn Migration) -> Result<bool> {
        let _guard = self.state_mutex.lock().await;

        let version = migration.version();
        let is_applied = self.is_migration_applied(version).await?;

        if !is_applied {
            return Ok(false);
        }

        // Rollback the migration
        migration.down(self.conn.as_ref()).await.with_context(|| {
            format!("Failed to rollback migration: {}", version)
        })?;

        // Record the rollback
        self.record_migration_rolled_back(version).await?;

        tracing::info!("Rolled back migration: {}", version);

        Ok(true)
    }

    /// Runs all provided migrations in order.
    ///
    /// Migrations are sorted by version string to ensure consistent ordering.
    /// Already applied migrations are skipped unless `force` is set.
    ///
    /// # Arguments
    ///
    /// * `migrations` - Slice of migrations to run
    /// * `force` - Whether to force re-application of already applied migrations
    ///
    /// # Returns
    ///
    /// Number of migrations applied, or an error.
    pub async fn run_migrations(
        &self,
        migrations: &[&dyn Migration],
        force: bool,
    ) -> Result<usize> {
        // Initialize migrations table if needed
        if self.config.create_table_if_missing {
            self.initialize().await?;
        }

        // Sort migrations by version for consistent ordering
        let mut sorted_migrations: Vec<&dyn Migration> = migrations.to_vec();
        sorted_migrations.sort_by(|a, b| a.version().cmp(b.version()));

        let mut applied_count = 0;

        for migration in &sorted_migrations {
            let was_applied = self.migrate_up(*migration, force).await?;

            if was_applied {
                applied_count += 1;
            }
        }

        Ok(applied_count)
    }

    /// Rolls back all migrations in reverse order.
    ///
    /// Only rolls back migrations that have been applied.
    ///
    /// # Arguments
    ///
    /// * `migrations` - Slice of migrations to potentially rollback
    /// * `count` - Number of migrations to rollback (None for all)
    ///
    /// # Returns
    ///
    /// Number of migrations rolled back, or an error.
    pub async fn rollback_migrations(
        &self,
        migrations: &[&dyn Migration],
        count: Option<usize>,
    ) -> Result<usize> {
        // Sort migrations by version for consistent ordering
        let mut sorted_migrations: Vec<&dyn Migration> = migrations.to_vec();
        sorted_migrations.sort_by(|a, b| b.version().cmp(a.version())); // Reverse order

        let limit = count.unwrap_or(usize::MAX);
        let mut rolled_back_count = 0;

        for migration in sorted_migrations {
            if rolled_back_count >= limit {
                break;
            }

            let was_rolled_back = self.migrate_down(migration).await?;

            if was_rolled_back {
                rolled_back_count += 1;
            }
        }

        Ok(rolled_back_count)
    }

    /// Gets the current migration status.
    ///
    /// # Returns
    ///
    /// A `MigrationStatus` with counts and details, or an error.
    pub async fn get_status(&self) -> Result<MigrationStatus> {
        // Initialize if needed
        if self.config.create_table_if_missing {
            self.initialize().await?;
        }

        let records = self.get_migration_records().await?;

        let applied_count = records.iter()
            .filter(|r| r.state == MigrationState::Applied)
            .count();

        let pending_count = records.iter()
            .filter(|r| r.state == MigrationState::Pending)
            .count();

        let rolled_back_count = records.iter()
            .filter(|r| r.state == MigrationState::RolledBack)
            .count();

        Ok(MigrationStatus {
            applied_count,
            pending_count,
            rolled_back_count,
            records,
        })
    }

    /// Gets the latest applied migration version.
    ///
    /// # Returns
    ///
    /// The latest version string, or None if no migrations applied.
    pub async fn get_latest_version(&self) -> Result<Option<String>> {
        let table_name = &self.config.migrations_table;

        let sql = format!(
            r#"
            SELECT version FROM {}
            WHERE state = 'APPLIED'
            ORDER BY applied_at DESC
            LIMIT 1
            "#,
            table_name
        );

        let mut rows = self.conn.query(&sql, ()).await?;

        if let Some(row) = rows.next().await? {
            let version: String = row.get(0)?;
            Ok(Some(version))
        } else {
            Ok(None)
        }
    }

    /// Verifies migration integrity by checking checksums.
    ///
    /// # Arguments
    ///
    /// * `migrations` - Slice of migrations to verify
    ///
    /// # Returns
    ///
    /// Vector of versions with mismatched checksums, or an error.
    pub async fn verify_migrations(&self, migrations: &[&dyn Migration]) -> Result<Vec<String>> {
        let table_name = &self.config.migrations_table;

        let sql = format!(
            r#"
            SELECT version, checksum FROM {}
            WHERE state = 'APPLIED'
            "#,
            table_name
        );

        let mut rows = self.conn.query(&sql, ()).await?;
        let mut mismatches = Vec::new();

        while let Some(row) = rows.next().await? {
            let version: String = row.get(0)?;
            let stored_checksum: Option<String> = row.get(1)?;

            // Find the migration with this version
            if let Some(migration) = migrations.iter().find(|m| m.version() == version) {
                let expected_checksum = migration.checksum();

                if stored_checksum.as_ref() != Some(&expected_checksum) {
                    let version_for_warning = version.clone();
                    mismatches.push(version_for_warning.clone());
                    tracing::warn!(
                        "Checksum mismatch for migration {}: stored={:?}, expected={}",
                        version_for_warning,
                        stored_checksum,
                        expected_checksum
                    );
                }
            }
        }

        Ok(mismatches)
    }

    /// Creates a backup of the migrations table.
    ///
    /// Useful before performing destructive operations.
    ///
    /// # Returns
    ///
    /// `Ok(())` on success, or an error.
    pub async fn backup_migrations_table(&self) -> Result<()> {
        let table_name = &self.config.migrations_table;
        let backup_table = format!("{}_backup", table_name);

        // Drop backup if exists
        let drop_sql = format!("DROP TABLE IF EXISTS {}", backup_table);
        self.conn.execute(&drop_sql, ()).await?;

        // Create backup
        let backup_sql = format!(
            r#"
            CREATE TABLE {} AS
            SELECT * FROM {}
            "#,
            backup_table, table_name
        );

        self.conn.execute(&backup_sql, ()).await.with_context(|| {
            format!("Failed to backup migrations table")
        })?;

        tracing::info!("Backed up migrations table to: {}", backup_table);

        Ok(())
    }
}

/// Represents the current status of migrations.
///
/// This struct contains counts and details about applied, pending,
/// and rolled back migrations.
#[derive(Debug)]
pub struct MigrationStatus {
    /// Number of successfully applied migrations
    pub applied_count: usize,
    /// Number of pending migrations
    pub pending_count: usize,
    /// Number of rolled back migrations
    pub rolled_back_count: usize,
    /// All migration records
    pub records: Vec<MigrationRecord>,
}

impl MigrationStatus {
    /// Returns the total number of tracked migrations.
    ///
    /// # Returns
    ///
    /// Total count of all migrations.
    #[must_use]
    pub fn total_count(&self) -> usize {
        self.records.len()
    }

    /// Returns whether all migrations are applied.
    ///
    /// # Returns
    ///
    /// `true` if there are no pending migrations.
    #[must_use]
    pub fn is_complete(&self) -> bool {
        self.pending_count == 0
    }
}

/// Helper function to parse a version string into comparable parts.
///
/// Versions are expected in format: `YYYY_MM_DD_NNN_name`
///
/// # Arguments
///
/// * `version` - The version string to parse
///
/// # Returns
///
/// A tuple of (date_parts, sequence_number) for comparison.
fn parse_version(version: &str) -> (Vec<u32>, u32) {
    let parts: Vec<&str> = version.split('_').collect();

    let mut date_parts = Vec::new();
    let mut sequence_number = 0u32;

    for (i, part) in parts.iter().enumerate() {
        if i < 3 {
            // First three parts are year, month, day
            if let Ok(num) = part.parse::<u32>() {
                date_parts.push(num);
            }
        } else if i == 3 && part.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
            // Fourth part should be sequence number
            if let Ok(num) = part.parse::<u32>() {
                sequence_number = num;
            }
            break;
        }
    }

    (date_parts, sequence_number)
}

/// Sorts migrations by version in ascending order.
///
/// # Arguments
///
/// * `migrations` - Slice of migrations to sort
///
/// # Returns
///
/// A vector of migrations sorted by version.
pub fn sort_migrations_by_version<'a>(migrations: &[&'a dyn Migration]) -> Vec<&'a dyn Migration> {
    let mut sorted = migrations.to_vec();
    sorted.sort_by(|a, b| {
        let (a_date, a_seq) = parse_version(a.version());
        let (b_date, b_seq) = parse_version(b.version());

        match a_date.cmp(&b_date) {
            std::cmp::Ordering::Equal => a_seq.cmp(&b_seq),
            other => other,
        }
    });
    sorted
}

#[cfg(test)]
mod tests {
    use super::*;
    use libsql::Database;
    use tempfile::NamedTempFile;

    /// Creates a temporary database for testing.
    async fn create_test_connection() -> Result<Connection> {
        let temp_file = NamedTempFile::new()?;
        let path = temp_file.path().to_str().unwrap();

        let db = Database::open(path)?;
        let conn = db.connect()?;

        Ok(conn)
    }

    /// Test migration for unit tests.
    #[derive(Debug)]
    struct TestMigration {
        version: &'static str,
        up_sql: &'static str,
        down_sql: &'static str,
        apply_count: std::sync::atomic::AtomicUsize,
        rollback_count: std::sync::atomic::AtomicUsize,
    }

    impl TestMigration {
        const fn new(version: &'static str, up_sql: &'static str, down_sql: &'static str) -> Self {
            Self {
                version,
                up_sql,
                down_sql,
                apply_count: std::sync::atomic::AtomicUsize::new(0),
                rollback_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl Migration for TestMigration {
        fn version(&self) -> &str {
            self.version
        }

        async fn up(&self, conn: &Connection) -> Result<()> {
            self.apply_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if !self.up_sql.is_empty() {
                conn.execute(self.up_sql, ()).await?;
            }
            Ok(())
        }

        async fn down(&self, conn: &Connection) -> Result<()> {
            self.rollback_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if !self.down_sql.is_empty() {
                conn.execute(self.down_sql, ()).await?;
            }
            Ok(())
        }

        fn checksum(&self) -> String {
            format!("checksum_{}", self.version)
        }
    }

    #[tokio::test]
    async fn test_migration_manager_initialization() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        // Initialize should create the migrations table
        manager.initialize().await?;

        // Verify the table exists
        let status = manager.get_status().await?;
        assert_eq!(status.applied_count, 0);
        assert_eq!(status.pending_count, 0);

        Ok(())
    }

    #[tokio::test]
    async fn test_run_single_migration() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration = TestMigration::new(
            "2024_01_15_001_create_users",
            "CREATE TABLE IF NOT EXISTS users (id INTEGER PRIMARY KEY, name TEXT)",
            "DROP TABLE users",
        );

        let applied = manager.run_migrations(&[&migration], false).await?;

        assert_eq!(applied, 1);
        assert_eq!(migration.apply_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Verify migration is recorded
        let status = manager.get_status().await?;
        assert_eq!(status.applied_count, 1);
        assert!(status.is_complete());

        Ok(())
    }

    #[tokio::test]
    async fn test_skip_already_applied_migration() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration = TestMigration::new(
            "2024_01_15_001_create_users",
            "",
            "",
        );

        // Run twice
        manager.run_migrations(&[&migration], false).await?;
        manager.run_migrations(&[&migration], false).await?;

        // Should only apply once
        assert_eq!(migration.apply_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_force_reapply_migration() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration = TestMigration::new(
            "2024_01_15_001_create_users",
            "",
            "",
        );

        // Run with force
        manager.run_migrations(&[&migration], true).await?;
        manager.run_migrations(&[&migration], true).await?;

        // Should apply twice
        assert_eq!(migration.apply_count.load(std::sync::atomic::Ordering::SeqCst), 2);

        Ok(())
    }

    #[tokio::test]
    async fn test_rollback_migration() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration = TestMigration::new(
            "2024_01_15_001_create_users",
            "CREATE TABLE users (id INTEGER PRIMARY KEY)",
            "DROP TABLE users",
        );

        // Apply migration
        manager.run_migrations(&[&migration], false).await?;
        assert_eq!(migration.apply_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Rollback migration
        let rolled_back = manager.rollback_migrations(&[&migration], None).await?;
        assert_eq!(rolled_back, 1);
        assert_eq!(migration.rollback_count.load(std::sync::atomic::Ordering::SeqCst), 1);

        // Verify rollback is recorded
        let status = manager.get_status().await?;
        assert_eq!(status.rolled_back_count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_multiple_migrations_ordered() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration_v2 = TestMigration::new(
            "2024_01_15_002_add_email",
            "",
            "",
        );
        let migration_v1 = TestMigration::new(
            "2024_01_15_001_create_users",
            "",
            "",
        );

        // Run in random order
        manager.run_migrations(&[&migration_v2, &migration_v1], false).await?;

        // Verify both applied
        let status = manager.get_status().await?;
        assert_eq!(status.applied_count, 2);

        // Verify order (v1 should be applied before v2)
        let records = manager.get_migration_records().await?;
        assert_eq!(records[0].version, "2024_01_15_001_create_users");
        assert_eq!(records[1].version, "2024_01_15_002_add_email");

        Ok(())
    }

    #[tokio::test]
    async fn test_partial_rollback() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration_v1 = TestMigration::new(
            "2024_01_15_001_create_users",
            "",
            "",
        );
        let migration_v2 = TestMigration::new(
            "2024_01_15_002_add_email",
            "",
            "",
        );

        // Apply both
        manager.run_migrations(&[&migration_v1, &migration_v2], false).await?;

        // Rollback only one
        let rolled_back = manager.rollback_migrations(&[&migration_v1, &migration_v2], Some(1)).await?;
        assert_eq!(rolled_back, 1);

        // v2 should be rolled back (latest first)
        let status = manager.get_status().await?;
        assert_eq!(status.applied_count, 1);
        assert_eq!(status.rolled_back_count, 1);

        Ok(())
    }

    #[tokio::test]
    async fn test_migration_checksum_verification() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration = TestMigration::new(
            "2024_01_15_001_create_users",
            "",
            "",
        );

        manager.run_migrations(&[&migration], false).await?;

        // Verify should pass
        let mismatches = manager.verify_migrations(&[&migration]).await?;
        assert!(mismatches.is_empty());

        Ok(())
    }

    #[tokio::test]
    async fn test_get_latest_version() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        let migration_v1 = TestMigration::new(
            "2024_01_15_001_create_users",
            "",
            "",
        );
        let migration_v2 = TestMigration::new(
            "2024_01_15_002_add_email",
            "",
            "",
        );

        manager.run_migrations(&[&migration_v1, &migration_v2], false).await?;

        let latest = manager.get_latest_version().await?;
        assert_eq!(latest, Some("2024_01_15_002_add_email".to_string()));

        Ok(())
    }

    #[tokio::test]
    async fn test_migration_record_creation() {
        let mut record = MigrationRecord::new_pending("2024_01_15_001".to_string());

        assert_eq!(record.state, MigrationState::Pending);
        assert!(record.applied_at.is_none());

        record.mark_applied(Some("checksum123".to_string()));

        assert_eq!(record.state, MigrationState::Applied);
        assert!(record.applied_at.is_some());
        assert_eq!(record.checksum, Some("checksum123".to_string()));

        record.mark_rolled_back();

        assert_eq!(record.state, MigrationState::RolledBack);
        assert!(record.rolled_back_at.is_some());
    }

    #[tokio::test]
    async fn test_parse_version() {
        let (date, seq) = parse_version("2024_01_15_001_test");
        assert_eq!(date, vec![2024, 1, 15]);
        assert_eq!(seq, 1);

        let (date, seq) = parse_version("2023_12_25_042_another");
        assert_eq!(date, vec![2023, 12, 25]);
        assert_eq!(seq, 42);
    }

    #[tokio::test]
    async fn test_sort_migrations_by_version() {
        let m_v3 = TestMigration::new("2024_01_15_003_third", "", "");
        let m_v1 = TestMigration::new("2024_01_15_001_first", "", "");
        let m_v2 = TestMigration::new("2024_01_15_002_second", "", "");

        let migrations: Vec<&dyn Migration> = vec![&m_v3, &m_v1, &m_v2];
        let sorted = sort_migrations_by_version(&migrations);

        assert_eq!(sorted[0].version(), "2024_01_15_001_first");
        assert_eq!(sorted[1].version(), "2024_01_15_002_second");
        assert_eq!(sorted[2].version(), "2024_01_15_003_third");
    }

    #[tokio::test]
    async fn test_migration_state_display() {
        assert_eq!(MigrationState::Pending.to_string(), "PENDING");
        assert_eq!(MigrationState::Applied.to_string(), "APPLIED");
        assert_eq!(MigrationState::RolledBack.to_string(), "ROLLED_BACK");
    }

    #[tokio::test]
    async fn test_migration_config_defaults() {
        let config = MigrationConfig::default();

        assert_eq!(config.migrations_table, "schema_migrations");
        assert!(config.create_table_if_missing);
        assert!(config.use_transactions);
        assert!(config.verify_checksums);
    }

    #[tokio::test]
    async fn test_migration_status_counts() {
        let status = MigrationStatus {
            applied_count: 5,
            pending_count: 2,
            rolled_back_count: 1,
            records: Vec::new(),
        };

        assert_eq!(status.total_count(), 8);
        assert!(!status.is_complete());
    }

    #[tokio::test]
    async fn test_backup_migrations_table() -> Result<()> {
        let conn = create_test_connection().await?;
        let manager = MigrationManager::new(conn);

        // Apply some migrations
        let migration = TestMigration::new(
            "2024_01_15_001_create_users",
            "",
            "",
        );
        manager.run_migrations(&[&migration], false).await?;

        // Backup
        manager.backup_migrations_table().await?;

        // Verify backup table exists
        let sql = "SELECT COUNT(*) FROM schema_migrations_backup";
        let mut rows = manager.conn.query(sql, ()).await?;
        if let Some(row) = rows.next().await? {
            let count: i64 = row.get(0)?;
            assert_eq!(count, 1);
        }

        Ok(())
    }
}
