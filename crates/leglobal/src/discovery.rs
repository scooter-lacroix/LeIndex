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

    /// Create a new discovery engine with root paths
    #[must_use]
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self {
            search_paths: roots,
        }
    }

    /// Add a search path
    pub fn add_search_path(&mut self, path: PathBuf) {
        self.search_paths.push(path);
    }

    /// Discover projects in registered search paths
    pub fn discover(&self) -> std::result::Result<Vec<DiscoveredProject>, DiscoveryError> {
        let mut discovered = Vec::new();

        // Scan each search path for valid projects
        for search_path in &self.search_paths {
            // Check if path exists and is a directory
            if !search_path.exists() || !search_path.is_dir() {
                continue;
            }

            // First, check if the search path itself is a git repo
            let git_dir = search_path.join(".git");
            if git_dir.exists() {
                if let Some(project) = scan_project(search_path) {
                    discovered.push(project);
                }
            }

            // Then, recursively scan subdirectories for projects
            if let Ok(entries) = scan_subdirectories(search_path, 2) {
                discovered.extend(entries);
            }
        }

        Ok(discovered)
    }
}

/// Scan a single path as a potential project
fn scan_project(path: &std::path::Path) -> Option<DiscoveredProject> {
    // Check for .git directory (valid git repo)
    let git_dir = path.join(".git");
    if !git_dir.exists() {
        return None;
    }

    // Count source files
    let readdir = std::fs::read_dir(path).ok()?;
    let entries: Vec<std::fs::DirEntry> = readdir.filter_map(|e| e.ok()).collect();

    let file_count = entries
        .iter()
        .filter(|entry| {
            // Filter out directories and hidden files
            entry.path().is_file() &&
                !entry.file_name().to_string_lossy().starts_with('.')
        })
        .count();

    // Skip if no source files
    if file_count == 0 {
        return None;
    }

    // Detect language from extensions
    let language = detect_language(path);

    // Generate content fingerprint (simplified - just hash of path and file count)
    let content_data = format!("{}:{}", path.display(), file_count);
    let content_fingerprint = blake3::hash(content_data.as_bytes()).to_hex()[..8].to_string();

    // Generate unique project ID
    let base_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let unique_id = UniqueProjectId::new(
        base_name.clone(),
        content_fingerprint.clone(),
        0 // instance 0 for first discovery
    );

    Some(DiscoveredProject {
        unique_id,
        base_name,
        path: path.to_path_buf(),
        language,
        file_count,
        content_fingerprint,
        is_valid: true,
    })
}

/// Recursively scan subdirectories for projects
fn scan_subdirectories(path: &std::path::Path, max_depth: usize) -> std::io::Result<Vec<DiscoveredProject>> {
    let mut discovered = Vec::new();

    if max_depth == 0 {
        return Ok(discovered);
    }

    let readdir = std::fs::read_dir(path)?;
    for entry in readdir.filter_map(|e| e.ok()) {
        let entry_path = entry.path();
        if entry_path.is_dir() {
            // Check if this directory is a git repo
            if let Some(project) = scan_project(&entry_path) {
                discovered.push(project);
            } else {
                // Recurse into subdirectory
                discovered.extend(scan_subdirectories(&entry_path, max_depth - 1)?);
            }
        }
    }

    Ok(discovered)
}

/// Detect programming language from directory contents
fn detect_language(path: &std::path::Path) -> Option<String> {
    let readdir = match std::fs::read_dir(path) {
        Ok(rd) => rd,
        Err(_) => return None,
    };
    let entries: Vec<std::fs::DirEntry> = readdir.filter_map(|e| e.ok()).collect();

    let extensions: Vec<String> = entries
        .iter()
        .filter_map(|entry| {
            entry.path().extension().and_then(|ext| ext.to_str().map(|s| s.to_string()))
        })
        .collect();

    // Count extensions
    let mut counts: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    for ext in extensions {
        *counts.entry(ext).or_insert(0) += 1;
    }

    // Find most common extension
    let most_common = counts.into_iter()
        .max_by_key(|(_, count)| *count);

    match most_common {
        Some((ext, _)) => match ext.as_str() {
            "rs" => Some("rust".to_string()),
            "go" => Some("go".to_string()),
            "py" => Some("python".to_string()),
            "ts" | "tsx" | "js" => Some("typescript".to_string()),
            "java" => Some("java".to_string()),
            "cpp" | "cc" | "cxx" | "h" => Some("cpp".to_string()),
            "c" => Some("c".to_string()),
            "cs" => Some("csharp".to_string()),
            "rb" => Some("ruby".to_string()),
            "php" => Some("php".to_string()),
            "scala" => Some("scala".to_string()),
            "kt" | "kts" => Some("kotlin".to_string()),
            "swift" => Some("swift".to_string()),
            _ => None,
        },
        None => None,
    }
}

impl Default for DiscoveryEngine {
    fn default() -> Self {
        Self::new()
    }
}
