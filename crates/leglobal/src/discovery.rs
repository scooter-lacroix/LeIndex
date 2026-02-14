//! Project discovery and scanning

use lestockage::UniqueProjectId;
use std::path::PathBuf;
use thiserror::Error;

/// Errors that can occur during discovery
#[derive(Debug, Error)]
pub enum DiscoveryError {
    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Invalid project
    #[error("Invalid project: {0}")]
    InvalidProject(String),
}

/// A discovered project
#[derive(Debug, Clone)]
pub struct DiscoveredProject {
    /// Unique project ID
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
    /// Whether the project is valid
    pub is_valid: bool,
}

/// Discovery engine for finding projects
#[derive(Clone)]
pub struct DiscoveryEngine {
    /// Search paths
    search_paths: Vec<PathBuf>,
}

impl DiscoveryEngine {
    /// Create a new discovery engine
    #[must_use]
    pub fn new() -> Self {
        Self {
            search_paths: Vec::new(),
        }
    }

    /// Add a search path
    pub fn add_search_path(&mut self, path: PathBuf) {
        self.search_paths.push(path);
    }

    /// Discover projects in registered search paths
    pub fn discover(&self) -> std::result::Result<Vec<DiscoveredProject>, DiscoveryError> {
        // For now, return empty list
        // TODO: Implement actual discovery logic
        Ok(Vec::new())
    }
}

impl Default for DiscoveryEngine {
    fn default() -> Self {
        Self::new()
    }
}
