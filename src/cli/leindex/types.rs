// Data types and constants for the leindex module.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::search::search::SearchResult;

// Supported source file extensions for indexing
pub(crate) const SOURCE_FILE_EXTENSIONS: &[&str] = &[
    // Main languages
    "rs", "py", "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts",
    // Systems languages
    "go", "java", "cpp", "cc", "cxx", "c", "h", "hpp", // Scripting & other
    "cs", "rb", "php", "lua", "scala", "sc", "sh", "bash", "json",
];

// Directories to always skip during source collection
pub(crate) use crate::cli::skip_dirs::SKIP_DIRS;

pub(crate) const DEPENDENCY_MANIFEST_NAMES: &[&str] = &[
    "Cargo.lock",
    "Cargo.toml",
    "package-lock.json",
    "package.json",
    "npm-shrinkwrap.json",
    "yarn.lock",
    "pnpm-lock.yaml",
    "requirements.txt",
    "Pipfile.lock",
    "pyproject.toml",
    "poetry.lock",
    "go.mod",
    "go.sum",
    "Gemfile.lock",
    "composer.lock",
];

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
/// Result of scanning the project for source and manifest files.
///
/// `source_directories` tracks unique directories containing source files.
/// When a new file is added, its directory's mtime changes, so checking
/// these ~10-20 directories (instead of thousands of files) detects new
/// additions in <1ms.
pub(crate) struct ProjectFileScan {
    pub(crate) source_paths: Vec<PathBuf>,
    pub(crate) manifest_paths: Vec<PathBuf>,
    #[serde(default)]
    pub(crate) source_directories: Vec<PathBuf>,
    /// Hashes of manifest/lockfile contents at scan time, used for
    /// incremental reindex when only dependencies changed.
    /// Format: manifest_path → blake3 hex hash
    #[serde(default)]
    pub(crate) manifest_hashes: std::collections::HashMap<String, String>,
}

/// Per-file statistics cached from PDG
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct FileStats {
    /// Number of symbols in this file
    pub symbol_count: usize,
    /// Total complexity of all symbols in this file
    pub total_complexity: u32,
    /// Names of symbols in this file
    pub symbol_names: Vec<String>,
    /// Number of distinct files this file depends on (cross-file outgoing edges)
    #[serde(default)]
    pub outgoing_deps: usize,
    /// Number of distinct files that depend on this file (cross-file incoming edges)
    #[serde(default)]
    pub incoming_deps: usize,
}

/// Statistics from indexing operations
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexStats {
    /// Total number of files in the project
    pub total_files: usize,

    /// Total number of files encountered during indexing
    pub files_parsed: usize,

    /// Number of files successfully parsed
    pub successful_parses: usize,

    /// Number of files that failed to parse
    pub failed_parses: usize,

    /// Total number of code signatures extracted across all files
    pub total_signatures: usize,

    /// Total number of nodes created in the Program Dependence Graph
    pub pdg_nodes: usize,

    /// Total number of edges created in the Program Dependence Graph
    pub pdg_edges: usize,

    /// Total number of nodes successfully indexed for semantic search
    pub indexed_nodes: usize,

    /// Total time taken for the indexing process in milliseconds
    pub indexing_time_ms: u64,

    /// Number of external dependencies found in lock files
    #[serde(default)]
    pub external_deps_in_lockfile: usize,

    /// Number of external module nodes resolved via lock files
    #[serde(default)]
    pub external_deps_resolved: usize,

    /// Number of unique external imports still unresolved after manifest matching
    #[serde(default)]
    pub external_deps_unresolved: usize,
}

/// Result from a deep analysis operation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnalysisResult {
    /// The original analysis query
    pub query: String,

    /// Search results serving as entry points for analysis
    pub results: Vec<SearchResult>,

    /// Expanded code context generated from PDG traversal
    pub context: Option<String>,

    /// Estimated number of tokens used in the expanded context
    pub tokens_used: usize,

    /// Total time taken for the analysis process in milliseconds
    pub processing_time_ms: u64,
}

/// Diagnostics information about the indexed project
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Diagnostics {
    /// Absolute path to the project directory
    pub project_path: String,
    /// Unique identifier for the project (legacy, for backward compatibility)
    pub project_id: String,
    /// Unique project identifier with BLAKE3-based path hashing
    pub unique_project_id: String,
    /// Display name (user-friendly with clone indicator)
    pub display_name: String,
    /// Statistics from the last indexing operation
    pub stats: IndexStats,
    /// Current memory usage of the process in bytes
    pub memory_usage_bytes: usize,
    /// Total available system memory in bytes
    pub total_memory_bytes: usize,
    /// Current memory usage as a percentage of total system memory
    pub memory_usage_percent: f64,
    /// Whether the memory usage has exceeded the configured threshold
    pub memory_threshold_exceeded: bool,
    /// Current number of entries stored in the in-memory cache
    pub cache_entries: usize,
    /// Total size of the in-memory cache in bytes
    pub cache_bytes: usize,
    /// Number of cache entries that have been spilled to disk
    pub spilled_entries: usize,
    /// Total size of the spilled cache on disk in bytes
    pub spilled_bytes: usize,
    /// Total cache hits (memory + disk).
    #[serde(default)]
    pub cache_hits: usize,
    /// Number of in-memory cache hits.
    #[serde(default)]
    pub cache_memory_hits: usize,
    /// Number of disk-restored cache hits.
    #[serde(default)]
    pub cache_disk_hits: usize,
    /// Number of cache misses.
    #[serde(default)]
    pub cache_misses: usize,
    /// Cache hit rate in [0.0, 1.0].
    #[serde(default)]
    pub cache_hit_rate: f64,
    /// Number of cache write operations.
    #[serde(default)]
    pub cache_writes: usize,
    /// Number of cache spill/persist operations.
    #[serde(default)]
    pub cache_spills: usize,
    /// Number of cache restore operations.
    #[serde(default)]
    pub cache_restores: usize,
    /// Cache temperature classification: `cold`, `warm`, `hot`.
    #[serde(default)]
    pub cache_temperature: String,
    /// Whether a PDG is loaded in memory
    pub pdg_loaded: bool,
    /// Estimated size of the in-memory PDG (nodes × ~200 bytes + edges × ~64 bytes)
    pub pdg_estimated_bytes: usize,
    /// Number of nodes in the search engine index
    pub search_index_nodes: usize,
    /// Overall index health: "healthy", "stale", or "empty"
    pub index_health: String,
}

/// Coverage report of indexed vs source files.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageReport {
    /// Total number of source files discovered
    pub total_source_files: usize,
    /// Number of files currently indexed
    pub indexed_files: usize,
    /// Source files missing from the index
    pub missing_files: Vec<String>,
    /// Index entries whose source files no longer exist
    pub orphaned_entries: Vec<String>,
    /// Percentage of source files covered by the index
    pub coverage_pct: f64,
}
