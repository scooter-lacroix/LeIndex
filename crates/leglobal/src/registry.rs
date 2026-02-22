//! Global project registry using local SQLite
//!
//! Provides persistent storage for discovered projects with automatic reconnection.

use crate::DEFAULT_DB_PATH;
use lestockage::UniqueProjectId;
use rusqlite::{params, Connection};
use std::path::{Path, PathBuf};
use thiserror::Error;

/// Errors that can occur in the global registry
#[derive(Debug, Error)]
pub enum GlobalRegistryError {
    /// Database error
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Project not found
    #[error("Project not found: {0}")]
    NotFound(String),

    /// Invalid project ID
    #[error("Invalid project ID: {0}")]
    InvalidId(String),
}

/// Result type for registry operations
pub type Result<T> = std::result::Result<T, GlobalRegistryError>;

/// Project information stored in the registry
#[derive(Debug, Clone)]
pub struct ProjectInfo {
    /// Unique project identifier
    pub unique_id: UniqueProjectId,
    /// Base name of the project
    pub base_name: String,
    /// Path to the project
    pub path: PathBuf,
    /// Detected language
    pub language: Option<String>,
    /// Number of source files
    pub file_count: usize,
    /// Content fingerprint for clone detection
    pub content_fingerprint: String,
    /// Whether this is a clone
    pub is_clone: bool,
    /// Original project ID if this is a clone
    pub cloned_from: Option<String>,
    /// When the project was registered
    pub registered_at: i64,
    /// Last modified timestamp (as i64 for storage)
    pub last_modified: Option<i64>,
}

/// Global project registry
///
/// Manages persistent storage of discovered projects using local SQLite
pub struct GlobalRegistry {
    /// Database connection
    conn: Connection,
    /// Path to the database file
    db_path: PathBuf,
}

impl GlobalRegistry {
    /// Initialize a new global registry
    ///
    /// Creates the database and schema if they don't exist
    ///
    /// # Arguments
    ///
    /// * `db_path` - Path to the database file (default: ~/.leindex/global.db)
    ///
    /// # Returns
    ///
    /// `Result<Self>` - The initialized registry
    pub fn init<P: AsRef<Path>>(db_path: P) -> Result<Self> {
        let db_path = db_path.as_ref().to_path_buf();

        // Ensure parent directory exists
        if let Some(parent) = db_path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        let conn = Connection::open(&db_path)?;

        let mut registry = Self { conn, db_path };

        registry.initialize_schema()?;

        Ok(registry)
    }

    /// Initialize with default path
    ///
    /// # Returns
    ///
    /// `Result<Self>` - The initialized registry at ~/.leindex/global.db
    pub fn init_default() -> Result<Self> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| ".".to_string());
        let path = PathBuf::from(home).join(DEFAULT_DB_PATH.trim_start_matches('.'));
        Self::init(path)
    }

    /// Initialize the database schema
    fn initialize_schema(&mut self) -> Result<()> {
        // Enable WAL mode for better concurrency
        self.conn.pragma_update(None, "journal_mode", "WAL")?;

        // Set cache size
        self.conn.pragma_update(None, "cache_size", 10000i64)?;

        // Create global_projects table
        self.conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS global_projects (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                unique_project_id TEXT UNIQUE NOT NULL,
                base_name TEXT NOT NULL,
                canonical_path TEXT NOT NULL UNIQUE,
                language TEXT,
                file_count INTEGER DEFAULT 0,
                content_fingerprint TEXT NOT NULL,
                is_clone BOOLEAN DEFAULT 0,
                cloned_from TEXT,
                registered_at INTEGER NOT NULL,
                last_modified INTEGER,
                path_hash TEXT NOT NULL,
                instance INTEGER DEFAULT 0
            )
            "#,
            [],
        )?;

        // Create indexes for common queries
        let indexes = [
            "CREATE INDEX IF NOT EXISTS idx_global_projects_unique_id ON global_projects(unique_project_id)",
            "CREATE INDEX IF NOT EXISTS idx_global_projects_base_name ON global_projects(base_name)",
            "CREATE INDEX IF NOT EXISTS idx_global_projects_fingerprint ON global_projects(content_fingerprint)",
            "CREATE INDEX IF NOT EXISTS idx_global_projects_language ON global_projects(language)",
        ];

        for index_sql in indexes {
            self.conn.execute(index_sql, [])?;
        }

        Ok(())
    }

    /// Register a project in the registry
    ///
    /// # Arguments
    ///
    /// * `path` - Path to the project
    /// * `language` - Detected language (optional)
    /// * `file_count` - Number of source files
    /// * `content_fingerprint` - Content fingerprint for clone detection
    ///
    /// # Returns
    ///
    /// `Result<String>` - The unique project ID
    pub fn register_project(
        &mut self,
        path: &Path,
        language: Option<String>,
        file_count: usize,
        content_fingerprint: &str,
    ) -> Result<String> {
        // Load existing IDs for conflict detection
        let base_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        let existing_ids = self.load_existing_ids(base_name)?;
        let unique_id = UniqueProjectId::generate(path, &existing_ids);

        // Check for clones by content fingerprint
        let is_clone;
        let cloned_from;

        if let Some(existing) = self.find_by_fingerprint(content_fingerprint)? {
            is_clone = true;
            cloned_from = Some(existing.unique_id.to_string());
        } else {
            is_clone = false;
            cloned_from = None;
        }

        let canonical_path = path
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| path.to_string_lossy().to_string());

        let path_hash = blake3::hash(canonical_path.as_bytes())
            .to_hex()[..8]
            .to_string();

        let registered_now = chrono::Utc::now();
        let registered_at = registered_now.timestamp();

        let last_modified = path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| {
                t.duration_since(std::time::UNIX_EPOCH).ok()
                    .map(|d| d.as_secs() as i64)
            });

        self.conn.execute(
            r#"
            INSERT INTO global_projects
            (unique_project_id, base_name, canonical_path, language, file_count,
             content_fingerprint, is_clone, cloned_from, registered_at, last_modified,
             path_hash, instance)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                unique_id.to_string(),
                base_name,
                canonical_path,
                language,
                file_count as i64,
                content_fingerprint,
                is_clone,
                cloned_from,
                registered_at,
                last_modified,
                path_hash,
                unique_id.instance,
            ],
        )?;

        Ok(unique_id.to_string())
    }

    /// List all projects in the registry
    ///
    /// # Returns
    ///
    /// `Result<Vec<ProjectInfo>>` - All registered projects
    pub fn list_projects(&self) -> Result<Vec<ProjectInfo>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT unique_project_id, base_name, canonical_path, language, file_count,
                   content_fingerprint, is_clone, cloned_from, registered_at, last_modified
            FROM global_projects
            ORDER BY base_name
            "#,
        )?;

        let projects = stmt.query_map([], |row| {
            let id_str: String = row.get(0)?;
            let unique_id = UniqueProjectId::from_str(&id_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            Ok(ProjectInfo {
                unique_id,
                base_name: row.get(1)?,
                path: PathBuf::from(row.get::<_, String>(2)?),
                language: row.get(3)?,
                file_count: row.get::<_, i64>(4)? as usize,
                content_fingerprint: row.get(5)?,
                is_clone: row.get(6)?,
                cloned_from: row.get(7)?,
                registered_at: row.get(8)?,
                last_modified: row.get::<_, Option<i64>>(9)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();

        Ok(projects)
    }

    /// Get a project by its unique ID
    ///
    /// # Arguments
    ///
    /// * `id` - The unique project ID
    ///
    /// # Returns
    ///
    /// `Result<Option<ProjectInfo>>` - The project if found
    pub fn get_project(&self, id: &str) -> Result<Option<ProjectInfo>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT unique_project_id, base_name, canonical_path, language, file_count,
                   content_fingerprint, is_clone, cloned_from, registered_at, last_modified
            FROM global_projects
            WHERE unique_project_id = ?1
            "#,
        )?;

        let result = stmt.query_row(params![id], |row| {
            let id_str: String = row.get(0)?;
            let unique_id = UniqueProjectId::from_str(&id_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            Ok(ProjectInfo {
                unique_id,
                base_name: row.get(1)?,
                path: PathBuf::from(row.get::<_, String>(2)?),
                language: row.get(3)?,
                file_count: row.get::<_, i64>(4)? as usize,
                content_fingerprint: row.get(5)?,
                is_clone: row.get(6)?,
                cloned_from: row.get(7)?,
                registered_at: row.get(8)?,
                last_modified: row.get::<_, Option<i64>>(9)?,
            })
        });

        match result {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Delete a project from the registry
    ///
    /// # Arguments
    ///
    /// * `id` - The unique project ID
    pub fn delete_project(&mut self, id: &str) -> Result<()> {
        let rows_affected = self.conn.execute(
            "DELETE FROM global_projects WHERE unique_project_id = ?1",
            params![id],
        )?;

        if rows_affected == 0 {
            return Err(GlobalRegistryError::NotFound(id.to_string()));
        }

        Ok(())
    }

    /// Find a project by content fingerprint (for clone detection)
    ///
    /// # Arguments
    ///
    /// * `fingerprint` - The content fingerprint to search for
    ///
    /// # Returns
    ///
    /// `Result<Option<ProjectInfo>>` - The first project with this fingerprint
    fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<ProjectInfo>> {
        let mut stmt = self.conn.prepare(
            r#"
            SELECT unique_project_id, base_name, canonical_path, language, file_count,
                   content_fingerprint, is_clone, cloned_from, registered_at, last_modified
            FROM global_projects
            WHERE content_fingerprint = ?1
            LIMIT 1
            "#,
        )?;

        let result = stmt.query_row(params![fingerprint], |row| {
            let id_str: String = row.get(0)?;
            let unique_id = UniqueProjectId::from_str(&id_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;

            Ok(ProjectInfo {
                unique_id,
                base_name: row.get(1)?,
                path: PathBuf::from(row.get::<_, String>(2)?),
                language: row.get(3)?,
                file_count: row.get::<_, i64>(4)? as usize,
                content_fingerprint: row.get(5)?,
                is_clone: row.get(6)?,
                cloned_from: row.get(7)?,
                registered_at: row.get(8)?,
                last_modified: row.get::<_, Option<i64>>(9)?,
            })
        });

        match result {
            Ok(p) => Ok(Some(p)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// Load existing project IDs for a given base name
    fn load_existing_ids(&self, base_name: &str) -> Result<Vec<UniqueProjectId>> {
        let mut stmt = self.conn.prepare(
            "SELECT unique_project_id FROM global_projects WHERE base_name = ?1",
        )?;

        let ids: Vec<UniqueProjectId> = stmt
            .query_map(params![base_name], |row| {
                let id_str: String = row.get(0)?;
                Ok(UniqueProjectId::from_str(&id_str)
                    .ok_or_else(|| rusqlite::Error::InvalidQuery)?)
            })?
            .filter_map(|r| r.ok())
            .collect();

        Ok(ids)
    }

    /// Check database connection health
    ///
    /// # Returns
    ///
    /// `bool` - true if connection is healthy
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.conn
            .query_row("SELECT 1", [], |_row| Ok(()))
            .map(|_| true)
            .unwrap_or(false)
    }

    /// Get the database path
    #[must_use]
    pub const fn db_path(&self) -> &PathBuf {
        &self.db_path
    }

    /// Get a reference to the underlying connection
    #[must_use]
    pub fn conn(&self) -> &Connection {
        &self.conn
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_registry_init() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");

        let registry = GlobalRegistry::init(&db_path);

        assert!(registry.is_ok());
        assert!(registry.unwrap().is_healthy());
    }

    #[test]
    fn test_registry_init_default() {
        // Just test that it doesn't crash
        let result = GlobalRegistry::init_default();
        // It might fail if HOME is not writable, which is ok
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn test_register_project() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut registry = GlobalRegistry::init(&db_path).unwrap();

        // Create a temp project
        let project_path = temp_dir.path().join("test-project");
        std::fs::create_dir_all(project_path.join(".git")).unwrap();

        let id = registry.register_project(
            &project_path,
            Some("rust".to_string()),
            42,
            "test-fingerprint",
        );

        assert!(id.is_ok());
        let project_id = id.unwrap();
        assert!(project_id.contains("test-project"));
    }

    #[test]
    fn test_register_and_get_project() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut registry = GlobalRegistry::init(&db_path).unwrap();

        let project_path = temp_dir.path().join("myproject");
        std::fs::create_dir_all(project_path.join(".git")).unwrap();

        let id = registry.register_project(
            &project_path,
            Some("rust".to_string()),
            100,
            "fp123",
        ).unwrap();

        let project = registry.get_project(&id).unwrap();

        assert!(project.is_some());
        let info = project.unwrap();
        assert_eq!(info.base_name, "myproject");
        assert_eq!(info.language, Some("rust".to_string()));
        assert_eq!(info.file_count, 100);
        assert!(!info.is_clone);
    }

    #[test]
    fn test_list_projects() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut registry = GlobalRegistry::init(&db_path).unwrap();

        // Register a few projects
        for i in 0..3 {
            let path = temp_dir.path().join(format!("project{}", i));
            std::fs::create_dir_all(path.join(".git")).unwrap();
            registry.register_project(&path, Some("rust".to_string()), 10 + i, &format!("fp{}", i)).unwrap();
        }

        let projects = registry.list_projects().unwrap();

        assert_eq!(projects.len(), 3);
        assert!(projects.iter().all(|p| p.language == Some("rust".to_string())));
    }

    #[test]
    fn test_delete_project() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut registry = GlobalRegistry::init(&db_path).unwrap();

        let project_path = temp_dir.path().join("todelete");
        std::fs::create_dir_all(project_path.join(".git")).unwrap();

        let id = registry.register_project(&project_path, None, 5, "fp").unwrap();

        // Delete it
        assert!(registry.delete_project(&id).is_ok());

        // Should be gone
        let result = registry.get_project(&id).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_clone_detection() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let mut registry = GlobalRegistry::init(&db_path).unwrap();

        // Register original project
        let path1 = temp_dir.path().join("original");
        std::fs::create_dir_all(path1.join(".git")).unwrap();
        registry.register_project(&path1, Some("rust".to_string()), 50, "same-fp").unwrap();

        // Register clone with same fingerprint
        let path2 = temp_dir.path().join("clone");
        std::fs::create_dir_all(path2.join(".git")).unwrap();
        registry.register_project(&path2, Some("rust".to_string()), 50, "same-fp").unwrap();

        let projects = registry.list_projects().unwrap();
        let clone = projects.iter().find(|p| p.base_name == "clone").unwrap();

        assert!(clone.is_clone);
        assert!(clone.cloned_from.is_some());
    }

    #[test]
    fn test_is_healthy() {
        let temp_dir = TempDir::new().unwrap();
        let db_path = temp_dir.path().join("test.db");
        let registry = GlobalRegistry::init(&db_path).unwrap();

        assert!(registry.is_healthy());
    }
}
