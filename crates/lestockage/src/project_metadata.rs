//! Project metadata storage and retrieval.
//!
//! This module provides persistence for project metadata including
//! unique project IDs, display names, and clone relationships.

use crate::UniqueProjectId;
use rusqlite::params;
use std::path::Path;
use thiserror::Error;

/// Errors that can occur when working with project metadata.
#[derive(Debug, Error)]
pub enum ProjectMetadataError {
    /// Database error
    #[error("Database error: {0}")]
    Database(#[from] rusqlite::Error),
    
    /// Invalid project ID
    #[error("Invalid project ID: {0}")]
    InvalidId(String),
    
    /// Project not found
    #[error("Project not found: {0}")]
    NotFound(String),
}

/// Result type for project metadata operations.
pub type Result<T> = std::result::Result<T, ProjectMetadataError>;

/// Project metadata record.
#[derive(Debug, Clone)]
pub struct ProjectMetadata {
    /// Unique project identifier
    pub unique_project_id: UniqueProjectId,
    /// Base name of the project (directory name)
    pub base_name: String,
    /// BLAKE3 hash of the canonical path
    pub path_hash: String,
    /// Instance number (for duplicate paths)
    pub instance: u32,
    /// Canonical path to the project
    pub canonical_path: String,
    /// Human-readable display name
    pub display_name: Option<String>,
    /// Whether this is a clone of another project
    pub is_clone: bool,
    /// Original project ID if this is a clone
    pub cloned_from: Option<String>,
}

impl ProjectMetadata {
    /// Create new project metadata from a path.
    pub fn new(project_path: &Path) -> Self {
        let canonical_path = project_path
            .canonicalize()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| project_path.to_string_lossy().to_string());
        
        let base_name = project_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| "unknown".to_string());
        
        let path_hash = blake3::hash(canonical_path.as_bytes())
            .to_hex()
            .to_string();
        
        let unique_project_id = UniqueProjectId::generate(project_path, &[]);
        
        Self {
            unique_project_id,
            base_name,
            path_hash,
            instance: 0,
            canonical_path,
            display_name: None,
            is_clone: false,
            cloned_from: None,
        }
    }
    
    /// Load existing project IDs for a given base name.
    ///
    /// # Arguments
    ///
    /// * `conn` - Database connection
    /// * `base_name` - Project base name
    ///
    /// # Returns
    ///
    /// `Result<Vec<UniqueProjectId>>` - All existing IDs with this base name
    pub fn load_existing_ids(
        conn: &rusqlite::Connection,
        base_name: &str,
    ) -> Result<Vec<UniqueProjectId>> {
        let mut stmt = conn.prepare(
            "SELECT unique_project_id FROM project_metadata WHERE base_name = ?1",
        )?;

        let ids: Vec<UniqueProjectId> = stmt
            .query_map(params![base_name], |row| {
                let id_str: String = row.get(0)?;
                Ok(UniqueProjectId::from_str(&id_str))
            })?
            .filter_map(|r| r.ok().flatten())
            .collect();

        Ok(ids)
    }
    
    /// Save project metadata to the database.
    pub fn save(&self, conn: &rusqlite::Connection) -> Result<()> {
        conn.execute(
            r#"INSERT OR REPLACE INTO project_metadata 
               (unique_project_id, base_name, path_hash, instance, canonical_path, display_name, is_clone, cloned_from)
               VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"#,
            params![
                self.unique_project_id.to_string(),
                self.base_name,
                self.path_hash,
                self.instance,
                self.canonical_path,
                self.display_name,
                self.is_clone,
                self.cloned_from,
            ],
        )?;
        Ok(())
    }
    
    /// Load project metadata by unique ID.
    pub fn load(conn: &rusqlite::Connection, id: &UniqueProjectId) -> Result<Self> {
        let mut stmt = conn.prepare(
            r#"SELECT unique_project_id, base_name, path_hash, instance, canonical_path, display_name, is_clone, cloned_from
               FROM project_metadata WHERE unique_project_id = ?1"#,
        )?;
        let result = stmt.query_row(params![id.to_string()], |row| {
            let id_str: String = row.get(0)?;
            let unique_project_id = UniqueProjectId::from_str(&id_str)
                .ok_or_else(|| rusqlite::Error::InvalidQuery)?;
            Ok(Self {
                unique_project_id,
                base_name: row.get(1)?,
                path_hash: row.get(2)?,
                instance: row.get(3)?,
                canonical_path: row.get(4)?,
                display_name: row.get(5)?,
                is_clone: row.get(6)?,
                cloned_from: row.get(7)?,
            })
        }).map_err(ProjectMetadataError::from)?;
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    
    #[test]
    fn test_project_metadata_new() {
        let dir = tempdir().unwrap();
        let path = dir.path();
        
        let metadata = ProjectMetadata::new(path);
        assert!(!metadata.base_name.is_empty());
        assert!(!metadata.canonical_path.is_empty());
        assert!(!metadata.is_clone);
    }
}
