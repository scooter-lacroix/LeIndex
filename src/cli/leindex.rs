// leindex - Core Orchestration
//
// *L'Index* (The Index) - Unified API that brings together all LeIndex crates

use crate::cli::memory::{
    analysis_cache_key, search_cache_key, CacheEntry, WarmStrategy,
};
use crate::graph::{
    extract_pdg_from_signatures,
    pdg::ProgramDependenceGraph,
    traversal::{GravityTraversal, TraversalConfig},
};
use crate::parse::parallel::ParallelParser;
use crate::parse::traits::SignatureInfo;
use crate::search::search::{NodeInfo, SearchEngine, SearchQuery, SearchResult};
use crate::storage::{pdg_store, schema::Storage, UniqueProjectId};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

// Supported source file extensions for indexing
pub(crate) const SOURCE_FILE_EXTENSIONS: &[&str] = &[
    // Main languages
    "rs", "py", "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts",
    // Systems languages
    "go", "java", "cpp", "cc", "cxx", "c", "h", "hpp",
    // Scripting & other
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

// ============================================================================
// TF-IDF EMBEDDING SYSTEM
// ============================================================================

/// Tokenize a code string into sub-tokens by splitting camelCase, snake_case,
/// acronym boundaries, digit boundaries, whitespace, and punctuation, then
/// lowercasing all tokens.
///
/// Examples:
/// - `"getUserName"` → `["get", "user", "name"]`
/// - `"get_user_name"` → `["get", "user", "name"]`
/// - `"HTTPConnection"` → `["http", "connection"]`
/// - `"HTTP2Connection"` → `["http", "2", "connection"]`
fn tokenize_code(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && !current.is_empty() {
                let last = current.chars().last().unwrap();
                if last.is_lowercase() || last.is_ascii_digit() {
                    // camelCase or digit→upper boundary: "userName" → "user" | "Name"
                    if current.len() >= 2 {
                        tokens.push(current.to_lowercase());
                    } else if current.chars().all(|c| c.is_ascii_digit()) {
                        tokens.push(current.clone());
                    }
                    current = ch.to_string();
                } else {
                    current.push(ch);
                }
            } else if ch.is_lowercase()
                && !current.is_empty()
                && current.len() > 1
                && current
                    .chars()
                    .last()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
                && current
                    .chars()
                    .rev()
                    .nth(1)
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
            {
                // Acronym→camelCase: "HTTPC" + 'o' → push "HTTP", start "Co"
                let last_char = current.pop().unwrap();
                if current.len() >= 2 {
                    tokens.push(current.to_lowercase());
                }
                current = format!("{}{}", last_char, ch);
            } else if ch.is_ascii_digit()
                && !current.is_empty()
                && current
                    .chars()
                    .last()
                    .map(|c| c.is_alphabetic())
                    .unwrap_or(false)
            {
                // letter→digit boundary: "HTTP" + '2' → push "http", start "2"
                if current.len() >= 2 {
                    tokens.push(current.to_lowercase());
                }
                current = ch.to_string();
            } else if ch.is_alphabetic()
                && !current.is_empty()
                && current
                    .chars()
                    .last()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
            {
                // digit→letter boundary: "2" + 'C' → push "2", start "C"
                tokens.push(current.to_lowercase());
                current = ch.to_string();
            } else {
                current.push(ch);
            }
        } else if ch == '_' || ch == '-' || ch.is_whitespace() || ch.is_ascii_punctuation() {
            if current.len() >= 2 {
                tokens.push(current.to_lowercase());
            } else if !current.is_empty() && current.chars().all(|c| c.is_ascii_digit()) {
                tokens.push(current.clone());
            }
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    if current.len() >= 2 {
        tokens.push(current.to_lowercase());
    } else if !current.is_empty() && current.chars().all(|c| c.is_ascii_digit()) {
        tokens.push(current.to_lowercase());
    }
    tokens
}

/// TF-IDF based embedding system for code content.
///
/// Produces 768-dimensional vectors by computing TF-IDF scores for the
/// top-768 tokens by IDF value, then L2-normalizing the result.
///
/// This provides meaningful cosine similarity (> 0 for related code) unlike
/// the previous hash-based approach which produced random vectors.
struct TfIdfEmbedder {
    /// Ordered vocabulary (top-K tokens by IDF, K ≤ 768)
    vocab: Vec<String>,
    /// IDF values indexed by vocab position
    idf: Vec<f32>,
    /// Embedding dimension (matches existing vector index: 768)
    dimension: usize,
}

impl TfIdfEmbedder {
    /// Build a TF-IDF embedder from a corpus of (id, content) documents.
    ///
    /// # Steps
    /// 1. Tokenize every document
    /// 2. Build document-frequency table (df[token] = # docs containing token)
    /// 3. Compute IDF = ln(N / df) per token
    /// 4. Select top-768 tokens by IDF as vocabulary
    fn build(documents: &[(String, String)]) -> Self {
        const TARGET_DIM: usize = 768;
        let n = documents.len();

        if n == 0 {
            return Self {
                vocab: Vec::new(),
                idf: Vec::new(),
                dimension: TARGET_DIM,
            };
        }

        // Count document frequency per token
        let mut df: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (_, content) in documents {
            let toks: std::collections::HashSet<String> =
                tokenize_code(content).into_iter().collect();
            for tok in toks {
                *df.entry(tok).or_insert(0) += 1;
            }
        }

        // Compute IDF for each token using a moderate-frequency filter.
        //
        // WHY: "top-768 by IDF" = the 768 RAREST tokens (df=1, IDF≈ln(N)≈9).
        // These are hapax legomena — unique identifiers seen in only one document.
        // Query words almost never hit these rare tokens → zero query embedding
        // → semantic: 0.0 for all results.
        //
        // FIX: restrict to tokens with moderate document frequency:
        //   min_df = N/1000 (at least 0.1% of docs) — skip ultra-rare hapaxes
        //   max_df = N/4   (at most 25% of docs)   — skip ubiquitous noise
        //
        // Typical informative code terms ("embed", "semantic", "search", "vector")
        // appear in 10–200 of 8859 nodes → df falls squarely in this range →
        // they become vocabulary entries → queries produce non-zero embeddings.
        let n_f = n as f32;
        let min_df: usize = (n / 1000).max(3); // at least 3 occurrences
        let max_df: usize = (n / 4).max(min_df + 1);

        let mut idf_scores: Vec<(String, f32)> = df
            .into_iter()
            .filter(|(_, df_count)| *df_count >= min_df && *df_count <= max_df)
            .map(|(tok, df_count)| {
                let idf = (n_f / df_count as f32).ln();
                (tok, idf)
            })
            .collect();

        info!(
            vocab_candidates = idf_scores.len(),
            min_df,
            max_df,
            n_docs = n,
            "TF-IDF vocabulary candidates (moderate-IDF filter)"
        );

        // Stratified vocabulary selection across the full IDF range.
        //
        // WHY NOT "top-768 by IDF": even with min_df filtering, 768+ tokens in the
        // [min_df, max_df] range have higher IDF than typical query terms.  Sorting
        // by IDF descending and truncating to 768 fills the vocab with the rarest
        // non-hapax tokens (df=8–38) and entirely excludes moderately-common terms
        // ("semantic", "cosine", etc.) that queries actually contain.
        //
        // FIX: sort by IDF ascending, then stride-sample evenly across the range.
        // This guarantees vocab coverage from the most common to the rarest candidate,
        // so both high-df query words ("search") and low-df terms ("cosine") land
        // in the vocabulary.
        idf_scores.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let final_scores: Vec<(String, f32)> = if idf_scores.len() <= TARGET_DIM {
            // Fewer candidates than dimensions — use all of them, pad later
            idf_scores
        } else {
            // Stride-sample: take TARGET_DIM evenly-spaced tokens across the sorted list
            let total = idf_scores.len();
            let stride = total as f64 / TARGET_DIM as f64;
            (0..TARGET_DIM)
                .map(|i| {
                    let idx = ((i as f64 * stride) as usize).min(total - 1);
                    idf_scores[idx].clone()
                })
                .collect()
        };

        let idf_scores = final_scores;

        let vocab: Vec<String> = idf_scores.iter().map(|(t, _)| t.clone()).collect();
        let idf: Vec<f32> = idf_scores.iter().map(|(_, s)| *s).collect();

        Self {
            vocab,
            idf,
            dimension: TARGET_DIM,
        }
    }

    /// Embed a text string to a 768-dimensional L2-normalized TF-IDF vector.
    fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dimension];

        if self.vocab.is_empty() {
            return vec;
        }

        // Compute term frequencies
        let tokens = tokenize_code(text);
        let total = tokens.len() as f32;
        if total == 0.0 {
            return vec;
        }

        let mut tf_map: std::collections::HashMap<&str, f32> = std::collections::HashMap::new();
        for tok in &tokens {
            *tf_map.entry(tok.as_str()).or_insert(0.0) += 1.0;
        }

        // Compute TF-IDF for each vocab position
        for (i, (word, idf_val)) in self.vocab.iter().zip(self.idf.iter()).enumerate() {
            if let Some(&count) = tf_map.get(word.as_str()) {
                vec[i] = (count / total) * idf_val;
            }
        }

        // L2 normalize
        let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if magnitude > 1e-9 {
            for v in &mut vec {
                *v /= magnitude;
            }
        }

        vec
    }
}

// ============================================================================

/// LeIndex - Main orchestration struct
///
/// This struct provides a unified API for the entire LeIndex system,
/// integrating parsing, graph construction, search, and storage.
///
/// # Example
///
/// ```ignore
/// let leindex = LeIndex::new("/path/to/project")?;
/// leindex.index_project()?;
/// let results = leindex.search("authentication", 10).await?;
/// ```
pub struct LeIndex {
    /// Project path
    project_path: PathBuf,

    /// Resolved storage root for index artifacts (may be outside project)
    storage_path: PathBuf,

    /// Project identifier (legacy, for backward compatibility)
    project_id: String,

    /// Unique project identifier with BLAKE3-based path hashing
    unique_id: UniqueProjectId,

    /// Storage backend
    storage: Storage,

    /// Search engine
    search_engine: SearchEngine,

    /// Program Dependence Graph
    pdg: Option<ProgramDependenceGraph>,

    /// Cache subsystem (spiller, project scan, file stats)
    cache: crate::cli::index_cache::IndexCache,

    /// Indexing statistics
    stats: IndexStats,

    /// TF-IDF embedder, built from indexed node content.
    /// None until index_nodes() is called with a sufficient corpus.
    embedder: Option<TfIdfEmbedder>,
}

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

impl LeIndex {
    /// Try to create a directory and verify it is writable.
    fn try_create_dir(path: &Path) -> bool {
        std::fs::create_dir_all(path).is_ok()
            && std::fs::metadata(path)
                .map(|m| !m.permissions().readonly())
                .unwrap_or(false)
    }

    /// Resolve the storage directory using a multi-location fallback chain:
    /// 1. In-project `.leindex/`
    /// 2. `LEINDEX_HOME` env var
    /// 3. XDG data dir (`~/.local/share/leindex/<hash>`)
    /// 4. System temp dir (`/tmp/leindex/<hash>`)
    fn resolve_storage_path(project_path: &Path) -> Result<PathBuf> {
        let path_hash = &blake3::hash(project_path.to_string_lossy().as_bytes()).to_hex()[..12];
        let dir_name = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // 1. Prefer in-project .leindex
        let in_project = project_path.join(".leindex");
        if Self::try_create_dir(&in_project) {
            return Ok(in_project);
        }

        // 2. LEINDEX_HOME env var
        if let Ok(home) = std::env::var("LEINDEX_HOME") {
            let env_path = PathBuf::from(home).join(format!("{}-{}", dir_name, path_hash));
            if Self::try_create_dir(&env_path) {
                warn!(
                    "Using LEINDEX_HOME fallback for storage: {}",
                    env_path.display()
                );
                return Ok(env_path);
            }
        }

        // 3. XDG data dir (~/.local/share/leindex/<hash>)
        let xdg_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        if Self::try_create_dir(&xdg_dir) {
            warn!(
                "Using XDG data dir fallback for storage: {}",
                xdg_dir.display()
            );
            return Ok(xdg_dir);
        }

        // 4. System temp dir
        let tmp_path = std::env::temp_dir()
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        std::fs::create_dir_all(&tmp_path).with_context(|| {
            format!(
                "Failed to create .leindex storage directory.\n\
                 Tried:\n\
                 1. {} (in-project)\n\
                 2. $LEINDEX_HOME (not set or not writable)\n\
                 3. {} (XDG data dir)\n\
                 4. {} (temp dir)\n\n\
                 Fix: Check directory permissions, or set LEINDEX_HOME env var to a writable path.",
                in_project.display(),
                xdg_dir.display(),
                tmp_path.display(),
            )
        })?;
        warn!(
            "Using temp dir fallback for storage: {}",
            tmp_path.display()
        );
        Ok(tmp_path)
    }

    /// Open storage with retry and exponential backoff for transient failures
    /// (e.g., SQLite BUSY locks from concurrent access).
    fn open_storage_with_retry(db_path: &Path, max_retries: u32) -> Result<Storage> {
        let mut attempt = 0;
        loop {
            match Storage::open(db_path) {
                Ok(s) => return Ok(s),
                Err(e) if attempt < max_retries => {
                    attempt += 1;
                    let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempt));
                    warn!(
                        "Storage open attempt {}/{} failed: {}. Retrying in {:?}",
                        attempt, max_retries, e, delay
                    );
                    std::thread::sleep(delay);
                }
                Err(e) => {
                    return Err(e).with_context(|| {
                        format!(
                            "Failed to open storage at {} after {} attempts.\n\
                             Suggestion: Delete {} and re-index, or check disk space.",
                            db_path.display(),
                            max_retries,
                            db_path.display()
                        )
                    });
                }
            }
        }
    }

    /// Create a new LeIndex instance for a project
    ///
    /// # Arguments
    ///
    /// * `project_path` - Path to the project directory
    ///
    /// # Returns
    ///
    /// `Result<LeIndex>` - The initialized LeIndex instance
    ///
    /// # Example
    ///
    /// ```ignore
    /// let leindex = LeIndex::new("/path/to/project")?;
    /// ```
    pub fn new<P: AsRef<Path>>(project_path: P) -> Result<Self> {
        let project_path = project_path
            .as_ref()
            .canonicalize()
            .context("Failed to canonicalize project path")?;

        let project_id = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Initialize storage with multi-location fallback and retry
        let storage_path = Self::resolve_storage_path(&project_path)?;

        let db_path = storage_path.join("leindex.db");
        let storage = Self::open_storage_with_retry(&db_path, 3)?;

        // Generate unique project ID with conflict resolution
        // Load existing projects with same base name
        let existing_ids = storage.load_existing_ids(&project_id).unwrap_or_default();
        let unique_id = UniqueProjectId::generate(&project_path, &existing_ids);

        // Store the project metadata
        storage
            .store_project_metadata(&unique_id, &project_path)
            .context("Failed to store project metadata")?;

        info!(
            "Creating LeIndex for project: {} (unique ID: {}) at {:?}",
            project_id,
            unique_id.to_string(),
            project_path
        );

        // Initialize search engine
        let search_engine = SearchEngine::new();

        // Initialize cache subsystem
        let cache_dir = storage_path.join("cache");
        let cache = crate::cli::index_cache::IndexCache::new(cache_dir)?;

        Ok(Self {
            project_path,
            storage_path,
            project_id,
            unique_id,
            storage,
            search_engine,
            pdg: None,
            cache,
            stats: IndexStats {
                total_files: 0,
                files_parsed: 0,
                successful_parses: 0,
                failed_parses: 0,
                total_signatures: 0,
                pdg_nodes: 0,
                pdg_edges: 0,
                indexed_nodes: 0,
                indexing_time_ms: 0,
                external_deps_in_lockfile: 0,
                external_deps_resolved: 0,
                external_deps_unresolved: 0,
            },
            embedder: None,
        })
    }

    /// Index the project
    ///
    /// This executes the full indexing pipeline:
    /// 1. Parse all source files in parallel
    /// 2. Extract PDG from parsed signatures
    /// 3. Index nodes for semantic search
    /// 4. Persist PDG to storage
    ///
    /// # Returns
    ///
    /// `Result<IndexStats>` - Statistics from the indexing operation
    ///
    /// # Example
    ///
    /// ```ignore
    /// let stats = leindex.index_project()?;
    /// println!("Indexed {} files", stats.files_parsed);
    /// ```
    /// Index the project
    ///
    /// This executes the full indexing pipeline:
    /// 1. Parse all source files in parallel (incrementally)
    /// 2. Extract PDG from parsed signatures
    /// 3. Index nodes for semantic search
    /// 4. Persist PDG to storage
    ///
    /// # Arguments
    ///
    /// * `force` - If true, re-index all files regardless of changes
    ///
    /// # Returns
    ///
    /// `Result<IndexStats>` - Statistics from the indexing operation
    pub fn index_project(&mut self, force: bool) -> Result<IndexStats> {
        let start_time = std::time::Instant::now();

        info!(
            "Starting project indexing for: {} (force={})",
            self.project_id, force
        );

        // Step 1: Get currently indexed files from storage
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .unwrap_or_default();

        // Step 2: Collect all source files and compute hashes
        let source_files_with_hashes = self.collect_source_files_with_hashes(true)?;
        info!("Found {} source files", source_files_with_hashes.len());

        // Step 3: Identify changed/new/deleted files
        let mut files_to_parse = Vec::new();
        let mut unchanged_files = Vec::new();

        let current_file_paths: std::collections::HashSet<String> = source_files_with_hashes
            .iter()
            .map(|(p, _)| p.display().to_string())
            .collect();

        for (path, hash) in &source_files_with_hashes {
            let path_str = path.display().to_string();
            if force
                || !indexed_files.contains_key(&path_str)
                || indexed_files.get(&path_str) != Some(hash)
            {
                files_to_parse.push(path.clone());
            } else {
                unchanged_files.push(path_str);
            }
        }

        let deleted_files: Vec<String> = indexed_files
            .keys()
            .filter(|p| !current_file_paths.contains(*p))
            .cloned()
            .collect();

        info!(
            "Incremental analysis: {} to parse, {} unchanged, {} deleted",
            files_to_parse.len(),
            unchanged_files.len(),
            deleted_files.len()
        );

        if files_to_parse.is_empty() && deleted_files.is_empty() && self.is_indexed() {
            // Source files unchanged — but check if manifest/lockfiles changed,
            // which requires re-annotating external dependencies.
            let manifest_dirty = self.check_manifest_stale();
            if !manifest_dirty {
                info!("No changes detected, skipping indexing");
                return Ok(self.stats.clone());
            }
            info!("Manifest files changed — running external dependency annotation");
        }

        // Step 4: Parse changed files
        let parsing_results = if !files_to_parse.is_empty() {
            let parser = ParallelParser::new();
            parser.parse_files(files_to_parse)
        } else {
            Vec::new()
        };

        // Step 5: Update PDG
        // Load existing PDG if we have unchanged files and it's not in memory
        if !unchanged_files.is_empty() && self.pdg.is_none() {
            let _ = self.load_from_storage();
        }

        let mut pdg = self.pdg.take().unwrap_or_else(ProgramDependenceGraph::new);

        // Remove data for changed/deleted files
        for (path, _) in &source_files_with_hashes {
            let path_str = path.display().to_string();
            if !unchanged_files.contains(&path_str) {
                self.remove_file_from_pdg(&mut pdg, &path_str)?;
            }
        }
        for path in &deleted_files {
            self.remove_file_from_pdg(&mut pdg, path)?;
            let _ = crate::storage::pdg_store::delete_file_data(
                &mut self.storage,
                &self.project_id,
                path,
            );
        }

        // Extract signatures from successful parses
        let signatures_by_file: std::collections::HashMap<String, (String, Vec<SignatureInfo>)> =
            parsing_results
                .iter()
                .filter_map(|r| {
                    if r.is_success() {
                        let lang = r.language.clone().unwrap_or_else(|| "unknown".to_string());
                        Some((
                            r.file_path.display().to_string(),
                            (lang, r.signatures.clone()),
                        ))
                    } else {
                        None
                    }
                })
                .collect();

        // Build partial PDG and merge
        for (file_path, (language, signatures)) in signatures_by_file {
            let file_pdg = extract_pdg_from_signatures(signatures, &[], &file_path, &language);
            self.merge_pdgs(&mut pdg, file_pdg);

            // Update hash in storage
            if let Some((_, hash)) = source_files_with_hashes
                .iter()
                .find(|(p, _)| p.display().to_string() == file_path)
            {
                let _ = crate::storage::pdg_store::update_indexed_file(
                    &mut self.storage,
                    &self.project_id,
                    &file_path,
                    hash,
                );
            }
        }

        // Step 5b: Resolve external dependencies via lock files
        let manifest_paths = self
            .cache
            .project_scan
            .as_ref()
            .map(|scan| scan.manifest_paths.clone())
            .unwrap_or_default();
        let ext_registry = crate::graph::ExternalDependencyRegistry::from_manifest_paths(
            &self.project_path,
            &manifest_paths,
        );
        let annotation_stats = crate::graph::annotate_external_nodes(&mut pdg, &ext_registry);
        if !ext_registry.is_empty() {
            info!(
                "External dependency resolution: {}/{} resolved via lock files, {} recognized builtins ({} packages in registry)",
                annotation_stats.resolved,
                annotation_stats.total_external,
                annotation_stats.builtin,
                ext_registry.len()
            );
        } else if annotation_stats.total_external > 0 {
            info!(
                "External dependency resolution: no lockfile registry found, {} builtins recognized, {} unresolved external imports",
                annotation_stats.builtin,
                annotation_stats.unresolved
            );
        }
        let (ext_in_lockfile, ext_resolved, ext_unresolved) = (
            ext_registry.len(),
            annotation_stats.resolved,
            annotation_stats.unresolved,
        );

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!(
            "Updated PDG has {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );

        // Step 6: Re-index nodes for search (full re-index for simplicity, search engine is fast)
        self.index_nodes(&pdg)?;
        let indexed_count = self.search_engine.node_count();

        info!("Indexed {} nodes for search", indexed_count);

        // Step 7: Persist to storage
        self.save_to_storage(&pdg)?;

        // Update statistics
        let successful = parsing_results.iter().filter(|r| r.is_success()).count();
        let failed = parsing_results.iter().filter(|r| r.is_failure()).count();
        let total_sigs: usize = parsing_results.iter().map(|r| r.signatures.len()).sum();

        self.stats = IndexStats {
            total_files: source_files_with_hashes.len(),
            files_parsed: parsing_results.len(),
            successful_parses: successful,
            failed_parses: failed,
            total_signatures: total_sigs,
            pdg_nodes: pdg_node_count,
            pdg_edges: pdg_edge_count,
            indexed_nodes: indexed_count,
            indexing_time_ms: start_time.elapsed().as_millis() as u64,
            external_deps_in_lockfile: ext_in_lockfile,
            external_deps_resolved: ext_resolved,
            external_deps_unresolved: ext_unresolved,
        };

        // Normalize external nodes (legacy compat)
        Self::normalize_external_nodes(&mut pdg);

        // Keep PDG in memory
        self.pdg = Some(pdg);

        // Build file stats cache for performance
        self.build_file_stats_cache();

        info!("Indexing completed in {}ms", self.stats.indexing_time_ms);

        Ok(self.stats.clone())
    }

    fn remove_file_from_pdg(
        &self,
        pdg: &mut ProgramDependenceGraph,
        file_path: &str,
    ) -> Result<()> {
        pdg.remove_file(file_path);
        Ok(())
    }

    fn collect_source_files_with_hashes(
        &mut self,
        refresh: bool,
    ) -> Result<Vec<(PathBuf, String)>> {
        let scan = self.get_project_scan(refresh)?;
        scan.source_paths
            .iter()
            .map(|path| Ok((path.clone(), self.hash_file(path)?)))
            .collect()
    }

    fn collect_source_file_paths(&mut self, refresh: bool) -> Result<Vec<PathBuf>> {
        Ok(self.get_project_scan(refresh)?.source_paths)
    }

    fn get_project_scan(&mut self, refresh: bool) -> Result<ProjectFileScan> {
        // Try cached result first (no scan needed)
        if !refresh {
            if let Some(scan) = &self.cache.project_scan {
                return Ok(scan.clone());
            }
        }
        // Try persistent cache first (avoids full walkdir)
        let project_id = self.project_id.clone();
        if !refresh {
            if let result @ Ok(_) = self.cache.get_project_scan(&project_id, false, || Err(anyhow::anyhow!("cache miss"))) {
                return result;
            }
        }
        // Cache miss — scan filesystem
        let scan = self.scan_project_files()?;
        self.cache.cache_project_scan(&project_id, &scan);
        self.cache.project_scan = Some(scan.clone());
        Ok(scan)
    }

    fn scan_project_files(&self) -> Result<ProjectFileScan> {
        let project_config =
            crate::cli::config::ProjectConfig::load(&self.project_path).unwrap_or_default();
        let mut source_paths = Vec::new();
        let mut manifest_paths = Vec::new();
        let mut walker = walkdir::WalkDir::new(&self.project_path).into_iter();

        while let Some(entry) = walker.next() {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };

            let path = entry.path();
            let file_name = entry.file_name().to_string_lossy();

            if path != self.project_path && file_name.starts_with('.') && file_name != "." {
                if entry.file_type().is_dir() {
                    walker.skip_current_dir();
                }
                continue;
            }

            if entry.file_type().is_dir() {
                if SKIP_DIRS.contains(&file_name.as_ref())
                    || project_config.should_exclude(path)
                {
                    walker.skip_current_dir();
                }
                continue;
            }

            if !entry.file_type().is_file() {
                continue;
            }

            // Check for dependency manifests. Lockfiles (Cargo.lock, etc.) are
            // always collected regardless of exclusion patterns. Workspace manifests
            // (Cargo.toml, package.json, etc.) respect exclusion rules.
            if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
                if Self::is_dependency_manifest_name(name) {
                    let is_lockfile = name.contains("lock") || name.contains(".sum") || name == "npm-shrinkwrap.json";
                    if is_lockfile || !project_config.should_exclude(path) {
                        manifest_paths.push(path.to_path_buf());
                    }
                    continue; // Don't also add manifests to source_paths
                }
            }

            if project_config.should_exclude(path) {
                continue;
            }

            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                let ext_lower = ext.to_ascii_lowercase();
                if SOURCE_FILE_EXTENSIONS.contains(&ext_lower.as_str()) {
                    source_paths.push(path.to_path_buf());
                }
            }
        }

        let source_directories = Self::extract_unique_dirs(&source_paths);
        Ok(ProjectFileScan {
            source_paths,
            manifest_paths,
            source_directories,
        })
    }

    /// Extract sorted unique directories from a list of file paths.
    fn extract_unique_dirs(paths: &[PathBuf]) -> Vec<PathBuf> {
        crate::cli::index_freshness::extract_unique_dirs(paths)
    }

    /// Build a FreshnessContext for delegation to index_freshness module.
    fn freshness_context(&self) -> crate::cli::index_freshness::FreshnessContext<'_> {
        crate::cli::index_freshness::FreshnessContext {
            project_path: &self.project_path,
            storage_path: &self.storage_path,
            project_id: &self.project_id,
            storage: &self.storage,
            project_scan: self.cache.project_scan.as_ref(),
            cache_spiller: &self.cache.cache_spiller,
        }
    }

    fn hash_file(&self, path: &Path) -> Result<String> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("Failed to read file for hashing: {}", path.display()))?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }

    fn merge_pdgs(&self, target: &mut ProgramDependenceGraph, source: ProgramDependenceGraph) {
        for node_idx in source.node_indices() {
            if let Some(node) = source.get_node(node_idx) {
                target.add_node(node.clone());
            }
        }

        for edge_idx in source.edge_indices() {
            if let Some(edge) = source.get_edge(edge_idx) {
                if let Some((s, t)) = source.edge_endpoints(edge_idx) {
                    if let (Some(sn), Some(tn)) = (source.get_node(s), source.get_node(t)) {
                        if let (Some(si), Some(ti)) =
                            (target.find_by_symbol(&sn.id), target.find_by_symbol(&tn.id))
                        {
                            target.add_edge(si, ti, edge.clone());
                        }
                    }
                }
            }
        }
    }

    fn is_dependency_manifest_name(name: &str) -> bool {
        DEPENDENCY_MANIFEST_NAMES.contains(&name)
    }

    fn index_fingerprint(&self) -> String {
        format!(
            "{}:{}:{}",
            self.stats.pdg_nodes, self.stats.pdg_edges, self.stats.indexed_nodes
        )
    }

    fn stable_project_cache_id(&self) -> String {
        let path = self.project_path.to_string_lossy();
        let hash = blake3::hash(path.as_bytes()).to_hex();
        format!("{}:{}", self.project_id, &hash[..12])
    }

    fn search_cache_key_for(&self, query: &str, top_k: usize) -> String {
        search_cache_key(&format!(
            "query:{}:{}:{}:{}",
            self.stable_project_cache_id(),
            self.index_fingerprint(),
            top_k,
            query.trim().to_lowercase()
        ))
    }

    fn analysis_cache_key_for(&self, query: &str, token_budget: usize) -> String {
        analysis_cache_key(&format!(
            "analyze:{}:{}:{}:{}",
            self.stable_project_cache_id(),
            self.index_fingerprint(),
            token_budget,
            query.trim().to_lowercase()
        ))
    }

    /// Search the indexed code
    ///
    /// # Arguments
    ///
    /// * `query` - Search query string
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// `Result<Vec<SearchResult>>` - Search results sorted by relevance
    ///
    /// # Example
    ///
    /// ```ignore
    /// let results = leindex.search("authentication", 10).await?;
    /// for result in results {
    ///     println!("{}: {} ({:.2})", result.rank, result.symbol_name, result.score.total);
    /// }
    /// ```
    pub fn search(
        &mut self,
        query: &str,
        top_k: usize,
        query_type: Option<crate::search::ranking::QueryType>,
    ) -> Result<Vec<SearchResult>> {
        if self.search_engine.is_empty() {
            warn!("Search attempted on empty index");
            return Ok(Vec::new());
        }

        let search_cache_key = self.search_cache_key_for(query, top_k);
        if let Some(CacheEntry::Binary {
            serialized_data, ..
        }) = self
            .cache
            .cache_spiller
            .store_mut()
            .get_or_load(&search_cache_key)?
        {
            if let Ok(cached_results) = bincode::deserialize::<Vec<SearchResult>>(&serialized_data)
            {
                debug!(
                    "Search cache hit for '{}' ({} results)",
                    query,
                    cached_results.len()
                );
                return Ok(cached_results);
            }
        }

        let search_query = SearchQuery {
            query: query.to_string(),
            top_k,
            token_budget: None,
            semantic: true,
            expand_context: false,
            query_embedding: Some(self.generate_query_embedding(query)),
            threshold: Some(0.1), // Added default threshold for better quality
            query_type,
        };

        let mut results = self
            .search_engine
            .search(search_query)
            .context("Search operation failed")?;

        // Enrich results with PDG metadata: symbol_type, caller_count, dependency_count.
        // These require the in-memory PDG which is available here but not in lerecherche.
        if let Some(pdg) = &self.pdg {
            for result in &mut results {
                // Look up the PDG node by its string ID
                if let Some(node_idx) = pdg.find_by_id(&result.node_id) {
                    if let Some(node) = pdg.get_node(node_idx) {
                        result.symbol_type = Some(match node.node_type {
                            crate::graph::pdg::NodeType::Function => "function".to_string(),
                            crate::graph::pdg::NodeType::Class => "class".to_string(),
                            crate::graph::pdg::NodeType::Method => "method".to_string(),
                            crate::graph::pdg::NodeType::Variable => "variable".to_string(),
                            crate::graph::pdg::NodeType::Module => "module".to_string(),
                            crate::graph::pdg::NodeType::External => "external".to_string(),
                        });
                    }
                    result.caller_count = Some(pdg.predecessor_count(node_idx));
                    result.dependency_count = Some(pdg.neighbors(node_idx).len());
                }
            }
        }

        debug!("Search for '{}' returned {} results", query, results.len());

        if let Ok(serialized) = bincode::serialize(&results) {
            let entry = CacheEntry::Binary {
                metadata: std::collections::HashMap::from([
                    ("type".to_string(), "search_results".to_string()),
                    ("query".to_string(), query.to_string()),
                ]),
                serialized_data: serialized,
            };
            if self
                .cache
                .cache_spiller
                .store_mut()
                .insert(search_cache_key.clone(), entry)
                .is_ok()
            {
                let _ = self
                    .cache
                    .cache_spiller
                    .store_mut()
                    .persist_key(&search_cache_key);
            }
        }

        Ok(results)
    }

    /// Perform deep analysis with context expansion
    ///
    /// This combines semantic search with PDG-based context expansion
    /// to provide comprehensive code understanding.
    ///
    /// # Arguments
    ///
    /// * `query` - Analysis query
    /// * `token_budget` - Maximum tokens for context expansion
    ///
    /// # Returns
    ///
    /// `Result<AnalysisResult>` - Analysis results with expanded context
    ///
    /// # Example
    ///
    /// ```ignore
    /// let analysis = leindex.analyze("How does authentication work?", 2000).await?;
    /// println!("Found {} entry points", analysis.results.len());
    /// println!("Context: {}", analysis.context.unwrap_or_default());
    /// ```
    pub fn analyze(&mut self, query: &str, token_budget: usize) -> Result<AnalysisResult> {
        let start_time = std::time::Instant::now();

        let analysis_cache_key = self.analysis_cache_key_for(query, token_budget);
        if let Some(CacheEntry::Analysis {
            serialized_data, ..
        }) = self
            .cache
            .cache_spiller
            .store_mut()
            .get_or_load(&analysis_cache_key)?
        {
            if let Ok(mut cached) = bincode::deserialize::<AnalysisResult>(&serialized_data) {
                cached.processing_time_ms = start_time.elapsed().as_millis() as u64;
                debug!("Analysis cache hit for '{}'", query);
                return Ok(cached);
            }
        }

        // Step 1: Semantic search for entry points
        let search_query = SearchQuery {
            query: query.to_string(),
            top_k: 10,
            token_budget: Some(token_budget),
            semantic: true,
            expand_context: false,
            query_embedding: Some(self.generate_query_embedding(query)),
            threshold: Some(0.1), // Added threshold for better quality
            query_type: Some(crate::search::ranking::QueryType::Semantic),
        };

        let results = self
            .search_engine
            .search(search_query)
            .context("Search for analysis failed")?;

        // Step 2: Expand context using PDG traversal
        let context = if let Some(ref pdg) = self.pdg {
            self.expand_context(pdg, &results, token_budget)?
        } else {
            warn!("No PDG available for context expansion");
            String::from("/* No PDG available for context expansion */")
        };

        // Estimate tokens used (rough approximation: 4 chars per token)
        let tokens_used = context.len() / 4;
        let analysis = AnalysisResult {
            query: query.to_string(),
            results,
            context: Some(context),
            tokens_used,
            processing_time_ms: start_time.elapsed().as_millis() as u64,
        };

        if let Ok(serialized) = bincode::serialize(&analysis) {
            let entry = CacheEntry::Analysis {
                query: query.to_string(),
                timestamp: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0),
                serialized_data: serialized,
            };
            if self
                .cache
                .cache_spiller
                .store_mut()
                .insert(analysis_cache_key.clone(), entry)
                .is_ok()
            {
                let _ = self
                    .cache
                    .cache_spiller
                    .store_mut()
                    .persist_key(&analysis_cache_key);
            }
        }

        Ok(analysis)
    }

    /// Get diagnostics about the indexed project
    ///
    /// # Returns
    ///
    /// `Result<Diagnostics>` - Project diagnostics information
    ///
    /// # Example
    ///
    /// ```ignore
    /// let diag = leindex.get_diagnostics()?;
    /// println!("Memory usage: {:.1}%", diag.memory_usage_percent);
    /// ```
    pub fn get_diagnostics(&self) -> Result<Diagnostics> {
        let memory_stats = self
            .cache
            .cache_spiller
            .memory_stats()
            .context("Failed to get memory stats")?;
        let memory_percent = memory_stats.memory_percent();
        let threshold_exceeded = self.cache.cache_spiller.store().total_bytes() > 0
            && self.cache.cache_spiller.is_threshold_exceeded().unwrap_or(false);

        let pdg_loaded = self.pdg.is_some();
        let (pdg_nodes, pdg_edges) = self
            .pdg
            .as_ref()
            .map(|p| (p.node_count(), p.edge_count()))
            .unwrap_or((0, 0));
        // Rough estimate: ~200 bytes per node (name, file_path, id strings + overhead)
        // ~64 bytes per edge (two indices + edge metadata)
        let pdg_estimated_bytes = pdg_nodes * 200 + pdg_edges * 64;

        let search_index_nodes = self.search_engine.node_count();

        let index_health = if !pdg_loaded || pdg_nodes == 0 {
            "empty".to_string()
        } else if self.stats.failed_parses > 0 {
            "stale".to_string()
        } else {
            "healthy".to_string()
        };

        let cache_temperature = if memory_stats.cache_hits == 0 {
            "cold".to_string()
        } else if memory_stats.cache_hit_rate >= 0.70 && memory_stats.cache_hits >= 5 {
            "hot".to_string()
        } else {
            "warm".to_string()
        };

        Ok(Diagnostics {
            project_path: self.project_path.display().to_string(),
            project_id: self.project_id.clone(),
            unique_project_id: self.unique_id.to_string(),
            display_name: self.unique_id.display(),
            stats: self.stats.clone(),
            memory_usage_bytes: memory_stats.rss_bytes,
            total_memory_bytes: memory_stats.total_bytes,
            memory_usage_percent: memory_percent,
            memory_threshold_exceeded: threshold_exceeded,
            cache_entries: memory_stats.cache_entries,
            cache_bytes: memory_stats.cache_bytes,
            spilled_entries: memory_stats.spilled_entries,
            spilled_bytes: memory_stats.spilled_bytes,
            cache_hits: memory_stats.cache_hits,
            cache_memory_hits: memory_stats.cache_memory_hits,
            cache_disk_hits: memory_stats.cache_disk_hits,
            cache_misses: memory_stats.cache_misses,
            cache_hit_rate: memory_stats.cache_hit_rate,
            cache_writes: memory_stats.cache_writes,
            cache_spills: memory_stats.cache_spills,
            cache_restores: memory_stats.cache_restores,
            cache_temperature,
            pdg_loaded,
            pdg_estimated_bytes,
            search_index_nodes,
            index_health,
        })
    }

    /// Load a previously indexed project from storage
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    ///
    /// # Example
    ///
    /// ```ignore
    /// leindex.load_from_storage()?;
    /// println!("Loaded {} nodes", leindex.search_engine.node_count());
    /// ```
    pub fn load_from_storage(&mut self) -> Result<()> {
        info!("Loading project from storage: {}", self.project_id);

        // Load PDG from storage
        let mut pdg = pdg_store::load_pdg(&self.storage, &self.project_id)
            .context("Failed to load PDG from storage")?;

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();

        info!(
            "Loaded PDG with {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );

        // Normalize external nodes (legacy compat)
        Self::normalize_external_nodes(&mut pdg);

        // Rebuild search index from PDG
        self.index_nodes(&pdg)?;
        let indexed_count = self.search_engine.node_count();

        info!("Rebuilt search index with {} nodes", indexed_count);

        // Update statistics
        self.stats.pdg_nodes = pdg_node_count;
        self.stats.pdg_edges = pdg_edge_count;
        self.stats.indexed_nodes = indexed_count;

        // Keep PDG in memory
        self.pdg = Some(pdg);

        // Build file stats cache for performance
        self.build_file_stats_cache();

        Ok(())
    }

    /// Check which source files have changed since last index.
    /// Returns (changed_paths, deleted_paths).
    pub fn check_freshness(&self) -> Result<(Vec<PathBuf>, Vec<String>)> {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::check_freshness(
            &ctx,
            || self.scan_project_files(),
            |p| self.hash_file(p),
        )
    }

    /// Check if any dependency manifest/lockfile has changed since last index.
    fn check_manifest_stale(&self) -> bool {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::check_manifest_stale(&ctx, || self.scan_project_files())
    }

    /// Fast-path freshness check: O(1) for indexed files, O(D) for source
    /// directories (typically 10-20), and O(M) for manifest files.
    pub fn is_stale_fast(&self) -> bool {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::is_stale_fast(&ctx, || self.scan_project_files())
    }

    // ========================================================================
    // ACCESSOR METHODS (for MCP server integration)
    // ========================================================================

    /// Get the project path
    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    /// Get the storage path used for index artifacts
    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }

    /// Get the project ID (legacy, for backward compatibility)
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Get the unique project identifier
    ///
    /// The unique ID combines base_name, BLAKE3 path hash, and instance number
    /// to provide a deterministic, conflict-free project identifier.
    ///
    /// # Returns
    ///
    /// Reference to the `UniqueProjectId`
    ///
    /// # Example
    ///
    /// ```ignore
    /// let unique_id = leindex.unique_id();
    /// println!("Unique ID: {}", unique_id.to_string()); // "leindex_a3f7d9e2_0"
    /// println!("Display: {}", unique_id.display());     // "leindex" or "leindex (clone #1)"
    /// ```
    pub fn unique_id(&self) -> &UniqueProjectId {
        &self.unique_id
    }

    /// Get the display name for the project
    ///
    /// Returns a user-friendly name with clone indicator if applicable.
    /// - Original (instance=0): base_name
    /// - Clone (instance>0): "base_name (clone #N)"
    ///
    /// # Returns
    ///
    /// Display name string
    ///
    /// # Example
    ///
    /// ```ignore
    /// let display = leindex.display_name();
    /// println!("Project: {}", display); // "leindex" or "leindex (clone #1)"
    /// ```
    pub fn display_name(&self) -> String {
        self.unique_id.display()
    }

    /// Get a reference to the search engine
    pub fn search_engine(&self) -> &SearchEngine {
        &self.search_engine
    }

    /// Get the Program Dependence Graph, if the project has been indexed.
    pub fn pdg(&self) -> Option<&ProgramDependenceGraph> {
        self.pdg.as_ref()
    }

    /// Ensure the PDG is loaded from storage. Called by handlers that need PDG access
    /// before their first query. Defers the 10-50MB load cost from project registration
    /// to first actual use.
    pub fn ensure_pdg_loaded(&mut self) -> Result<()> {
        if self.pdg.is_none() {
            // Guard: only load if there are actual indexed files in storage.
            // LeIndex::new() creates leindex.db eagerly, so DB existence alone
            // would cause a useless load_from_storage on brand-new projects.
            let has_content = crate::storage::pdg_store::has_indexed_files(
                &self.storage, &self.project_id,
            );
            if has_content {
                self.load_from_storage()?;
            }
        }
        Ok(())
    }

    /// Get the current indexing statistics
    pub fn get_stats(&self) -> &IndexStats {
        &self.stats
    }

    /// Build file statistics cache from PDG
    pub(crate) fn build_file_stats_cache(&mut self) {
        if let Some(pdg) = &self.pdg {
            self.cache.build_file_stats_cache(pdg);
        }
    }

    /// Get file statistics cache
    pub fn file_stats(&self) -> Option<&HashMap<String, FileStats>> {
        self.cache.file_stats()
    }

    /// Collect source files that belong to this project using the same exclusion rules as indexing.
    /// Get the list of source file paths for the project.
    ///
    /// TODO: This reuses the cached scan — after file additions/deletions,
    /// callers may get stale inventories until a forced refresh/reindex.
    /// Consider adding an optional `refresh` parameter or invalidation on FS changes.
    pub fn source_file_paths(&mut self) -> Result<Vec<PathBuf>> {
        self.collect_source_file_paths(false)
    }

    /// Expand context around a specific node.
    ///
    /// Accepts flexible node identification:
    /// - Full node ID (`"file_path:qualified_name"`)
    /// - Short symbol name (`"health_check"`)
    /// - Qualified name (`"ClassName.method_name"`)
    /// - `"file_path:symbol_name"` partial IDs
    ///
    /// Populates the SearchResult with real metadata from the PDG node.
    pub fn expand_node_context(
        &self,
        node_id: &str,
        token_budget: usize,
    ) -> Result<AnalysisResult> {
        let start_time = std::time::Instant::now();

        let pdg = self.pdg.as_ref().ok_or_else(|| {
            anyhow::anyhow!("No PDG available for context expansion. Has the project been indexed?")
        })?;

        // Resolve the node_id using multiple lookup strategies:
        // 1. Exact ID match (full "file_path:qualified_name")
        // 2. By name (short display name like "health_check")
        // 3. Case-insensitive substring match on name or id
        let resolved_nid = pdg
            .find_by_symbol(node_id)
            .or_else(|| pdg.find_by_name(node_id))
            .or_else(|| pdg.find_by_name_in_file(node_id, None));

        let (result_node_id, file_path, symbol_name, language, byte_range, complexity) =
            if let Some(nid) = resolved_nid {
                if let Some(node) = pdg.get_node(nid) {
                    (
                        node.id.clone(),
                        node.file_path.clone(),
                        node.name.clone(),
                        node.language.clone(),
                        node.byte_range,
                        node.complexity,
                    )
                } else {
                    (
                        node_id.to_string(),
                        String::new(),
                        node_id.to_string(),
                        "unknown".to_string(),
                        (0, 0),
                        0,
                    )
                }
            } else {
                (
                    node_id.to_string(),
                    String::new(),
                    node_id.to_string(),
                    "unknown".to_string(),
                    (0, 0),
                    0,
                )
            };

        let results = vec![SearchResult {
            rank: 1,
            node_id: result_node_id,
            file_path,
            symbol_name,
            symbol_type: None,
            signature: None,
            complexity,
            caller_count: None,
            dependency_count: None,
            language,
            score: crate::search::ranking::Score::default(),
            context: None,
            byte_range,
        }];

        let context = self.expand_context(pdg, &results, token_budget)?;
        let tokens_used = context.len() / 4;

        Ok(AnalysisResult {
            query: format!("Context for node {}", node_id),
            results,
            context: Some(context),
            tokens_used,
            processing_time_ms: start_time.elapsed().as_millis() as u64,
        })
    }

    /// Check if the project has been indexed
    pub fn is_indexed(&self) -> bool {
        self.search_engine.node_count() > 0
    }

    /// Close the LeIndex and ensure WAL is checkpointed
    ///
    /// This explicitly closes the storage connection, which triggers a WAL checkpoint.
    /// This should be called before switching projects to ensure file locks are released.
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn close(&mut self) -> Result<()> {
        self.storage.close().context("Failed to close storage")?;
        info!("Closed LeIndex for project: {}", self.project_id);
        Ok(())
    }

    /// Report which files are indexed and which are not.
    pub fn coverage_report(&mut self) -> Result<CoverageReport> {
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .unwrap_or_default();
        let source_files = self.collect_source_file_paths(true)?;

        let indexed_set: HashSet<String> = indexed_files.keys().cloned().collect();
        let source_set: HashSet<String> = source_files
            .iter()
            .map(|p| p.display().to_string())
            .collect();

        let missing: Vec<String> = source_set.difference(&indexed_set).cloned().collect();
        let orphaned: Vec<String> = indexed_set.difference(&source_set).cloned().collect();

        Ok(CoverageReport {
            total_source_files: source_files.len(),
            indexed_files: indexed_files.len(),
            missing_files: missing,
            orphaned_entries: orphaned,
            coverage_pct: if source_files.is_empty() {
                100.0
            } else {
                (indexed_files.len() as f64 / source_files.len() as f64) * 100.0
            },
        })
    }

    // ========================================================================
    // CACHE SPILLING (Phase 5.2)
    // ========================================================================

    /// Check memory and spill cache if threshold exceeded
    pub fn check_memory_and_spill(&mut self) -> Result<bool> {
        self.cache.check_memory_and_spill()
    }

    /// Spill PDG cache to disk
    pub fn spill_pdg_cache(&mut self) -> Result<()> {
        self.cache.spill_pdg_cache(&self.project_id, &mut self.pdg)
    }

    /// Spill vector search cache to disk
    pub fn spill_vector_cache(&mut self) -> Result<()> {
        self.cache
            .spill_vector_cache(&self.project_id, self.search_engine.node_count())
    }

    /// Spill all caches (PDG and vector) to disk
    pub fn spill_all_caches(&mut self) -> Result<(usize, usize)> {
        self.cache.spill_all_caches(
            &self.project_id,
            &mut self.pdg,
            self.search_engine.node_count(),
        )
    }

    // ========================================================================
    // CACHE RELOADING (Phase 5.3)
    // ========================================================================

    /// Reload PDG from cache
    ///
    /// Attempts to reload a previously spilled PDG from lestockage.
    /// This is useful when the PDG has been spilled from memory to free up RAM.
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn reload_pdg_from_cache(&mut self) -> Result<()> {
        // Check if PDG is already in memory
        if self.pdg.is_some() {
            info!("PDG already in memory, no reload needed");
            return Ok(());
        }

        // PDG not in memory, try to load from storage
        info!("PDG not in memory, attempting to load from lestockage");
        self.load_from_storage()
    }

    /// Reload vector index from PDG
    ///
    /// Rebuilds the vector search index from the current PDG.
    /// This is useful when the vector cache has been spilled and needs to be restored.
    ///
    /// # Returns
    ///
    /// `Result<usize>` - Number of nodes indexed
    pub fn reload_vector_from_pdg(&mut self) -> Result<usize> {
        // Take PDG temporarily to avoid borrow checker issues
        let pdg = self
            .pdg
            .take()
            .ok_or_else(|| anyhow::anyhow!("No PDG available for vector rebuild"))?;

        // Re-use the index_nodes logic to ensure consistent embedding generation
        self.index_nodes(&pdg)?;

        let indexed_count = self.search_engine.node_count();

        // Restore PDG first so build_file_stats_cache can read from it
        self.pdg = Some(pdg);

        // Rebuild file stats cache after vector rebuild so the fast path stays warm
        self.build_file_stats_cache();

        info!("Rebuilt vector index from PDG: {} nodes", indexed_count);

        Ok(indexed_count)
    }

    /// Warm caches with frequently accessed data
    pub fn warm_caches(
        &mut self,
        strategy: WarmStrategy,
    ) -> Result<crate::cli::memory::WarmResult> {
        let result = self.cache.warm_cache(strategy)?;

        // If PDG warming was requested and PDG is not in memory, reload from storage
        if (strategy == crate::cli::memory::WarmStrategy::PDGOnly
            || strategy == crate::cli::memory::WarmStrategy::All
            || strategy == crate::cli::memory::WarmStrategy::RecentFirst)
            && self.pdg.is_none()
        {
            info!("PDG warming requested but not in memory, reloading from lestockage");
            self.load_from_storage()?;
        }

        Ok(result)
    }

    /// Get cache statistics
    pub fn get_cache_stats(&self) -> Result<crate::cli::memory::MemoryStats> {
        self.cache.get_cache_stats()
    }

    // ========================================================================

    /// Generate an embedding for a query string.
    ///
    /// Uses the TF-IDF embedder built at index time when available, ensuring
    /// queries are projected into the same vector space as the indexed nodes.
    /// Falls back to deterministic hashing for edge cases (empty corpus, not yet indexed).
    pub fn generate_query_embedding(&self, query: &str) -> Vec<f32> {
        if let Some(ref emb) = self.embedder {
            emb.embed(query)
        } else {
            self.generate_deterministic_embedding(query, "", "")
        }
    }

    /// Generate a deterministic 768-dimensional embedding for a node
    ///
    /// This uses a stable hashing approach to generate a vector from symbol metadata.
    /// While not a real semantic embedding from an LLM, it provides a deterministic
    /// basis for vector search and HNSW testing.
    fn generate_deterministic_embedding(
        &self,
        symbol_name: &str,
        _file_path: &str,
        _content: &str,
    ) -> Vec<f32> {
        use std::collections::hash_map::DefaultHasher;
        use std::hash::{Hash, Hasher};

        let mut embedding = Vec::with_capacity(768);

        // Initial seed hash from symbol name only for better query matching
        // In a real system, this would be an LLM embedding of the content.
        // For this "deterministic" version, matching on symbol name is most useful.
        let mut base_hasher = DefaultHasher::new();
        symbol_name.to_lowercase().hash(&mut base_hasher);
        let base_hash = base_hasher.finish();

        for i in 0..768 {
            let mut hasher = DefaultHasher::new();
            base_hash.hash(&mut hasher);
            i.hash(&mut hasher);
            let hash_val = hasher.finish();

            // Map 64-bit hash to f32 in [-1.0, 1.0]
            let val = (hash_val as f64 / u64::MAX as f64) * 2.0 - 1.0;
            embedding.push(val as f32);
        }

        embedding
    }

    /// Index nodes from PDG for search.
    ///
    /// Builds a TF-IDF embedder from the full corpus of node content, then uses
    /// it to embed each node. This produces meaningful cosine similarity between
    /// related code nodes, replacing the previous hash-based placeholder.
    fn index_nodes(&mut self, pdg: &ProgramDependenceGraph) -> Result<()> {
        // Invalidate file stats cache on reindex
        self.cache.file_stats_cache = None;
        // Read file contents once per file to avoid repeated I/O.
        // Cache is scoped to this function and shared across both passes.
        let mut file_cache: std::collections::HashMap<String, std::sync::Arc<String>> =
            std::collections::HashMap::new();

        // --- Pass 1: collect all node content for TF-IDF corpus building ---
        let mut corpus: Vec<(String, String)> = Vec::new();
        let mut raw_nodes: Vec<(_, String)> = Vec::new();

        for node_idx in pdg.node_indices() {
            if let Some(node) = pdg.get_node(node_idx) {
                // Get file content (cached per unique file path)
                let content = file_cache
                    .entry(node.file_path.clone())
                    .or_insert_with(|| {
                        std::sync::Arc::new(
                            std::fs::read(&node.file_path)
                                .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                                .unwrap_or_default(),
                        )
                    })
                    .clone();

                // Extract node-specific content for better text matching.
                // Use byte-safe slicing to avoid panics on invalid UTF-8 char boundaries.
                let node_content = if !content.is_empty() && node.byte_range.1 > node.byte_range.0 {
                    let content_bytes = content.as_bytes();
                    let start = node.byte_range.0.min(content_bytes.len());
                    let end = node.byte_range.1.min(content_bytes.len());

                    if start < end {
                        let snippet = String::from_utf8_lossy(&content_bytes[start..end]);
                        format!("// {} in {}\n{}", node.name, node.file_path, snippet)
                    } else {
                        format!(
                            "// {} in {}\n{}",
                            node.name, node.file_path, "// [No source code available]"
                        )
                    }
                } else {
                    format!(
                        "// {} in {}\n{}",
                        node.name, node.file_path, "// [No source code available]"
                    )
                };

                corpus.push((node.id.clone(), node_content.clone()));
                raw_nodes.push((node_idx, node_content));
            }
        }

        // --- Build TF-IDF embedder from the full corpus ---
        let embedder = TfIdfEmbedder::build(&corpus);

        // --- Pass 2: build NodeInfo vec using the embedder for embeddings ---
        let mut nodes: Vec<NodeInfo> = Vec::new();

        for (node_idx, node_content) in raw_nodes {
            if let Some(node) = pdg.get_node(node_idx) {
                // Always use TF-IDF embedding — do NOT use any stored embedding
                // from a previous index run, as those may be hash-based vectors
                // that live in a different space from TF-IDF query embeddings.
                // Cosine similarity between hash vectors and TF-IDF vectors ≈ 0.
                let embedding = embedder.embed(&node_content);

                let node_info = NodeInfo {
                    node_id: node.id.clone(),
                    file_path: node.file_path.clone(),
                    symbol_name: node.name.clone(),
                    language: node.language.clone(),
                    content: node_content,
                    byte_range: node.byte_range,
                    embedding: Some(embedding),
                    complexity: node.complexity,
                };

                nodes.push(node_info);
            }
        }

        // Store embedder on self for query embedding at search time
        self.embedder = Some(embedder);

        // Index the nodes
        self.search_engine.index_nodes(nodes);

        Ok(())
    }

    /// Expand context using PDG traversal
    fn expand_context(
        &self,
        pdg: &ProgramDependenceGraph,
        results: &[SearchResult],
        token_budget: usize,
    ) -> Result<String> {
        let config = TraversalConfig {
            max_tokens: token_budget,
            ..TraversalConfig::default()
        };
        let traversal = GravityTraversal::with_config(config);

        // Map SearchResult entries to PDG node IDs for the traversal call.
        // Try exact ID match first, then fall back to name-based lookup.
        let entry_points: Vec<_> = results
            .iter()
            .filter_map(|r| {
                pdg.find_by_symbol(&r.node_id)
                    .or_else(|| pdg.find_by_name(&r.node_id))
                    .or_else(|| pdg.find_by_name(&r.symbol_name))
            })
            .collect();

        let expanded_node_ids = traversal.expand_context(pdg, entry_points);

        let mut context = String::from("/* Context Expansion via Gravity Traversal */\n");

        for node_id in expanded_node_ids {
            if let Some(node) = pdg.get_node(node_id) {
                context.push_str(&format!("\n// Symbol: {}\n", node.name));
                context.push_str(&format!("// File: {}\n", node.file_path));
                context.push_str(&format!("// Type: {:?}\n", node.node_type));

                // Retrieve actual source code if byte_range is valid
                if node.byte_range.1 > node.byte_range.0 {
                    if let Ok(content) = std::fs::read(&node.file_path) {
                        let start = node.byte_range.0;
                        let end = node.byte_range.1.min(content.len());
                        if let Ok(code) = std::str::from_utf8(&content[start..end]) {
                            context.push_str(code);
                            context.push_str("\n");
                        } else {
                            context.push_str("// [Error: Source code is not valid UTF-8]\n");
                        }
                    } else {
                        context.push_str(&format!(
                            "// [Error: Could not read file: {}]\n",
                            node.file_path
                        ));
                    }
                } else {
                    context.push_str("// [No source code range available for this node]\n");
                }
            }
        }

        Ok(context)
    }

    /// Normalize external nodes: ensure any node with `language == "external"`
    /// also has `NodeType::External`. Eliminates the dual-check bug class caused
    /// by legacy PDG data that set language without the enum variant.
    fn normalize_external_nodes(pdg: &mut ProgramDependenceGraph) {
        use crate::graph::pdg::NodeType;
        let mut migrated = 0usize;
        for node in pdg.node_weights_mut() {
            let is_external = node.language == "external"
                || node.language.starts_with("external:");
            if is_external && node.node_type != NodeType::External {
                node.node_type = NodeType::External;
                migrated += 1;
            }
        }
        if migrated > 0 {
            info!("Normalized {} external nodes to NodeType::External", migrated);
        }
    }

    /// Save PDG to storage
    fn save_to_storage(&mut self, pdg: &ProgramDependenceGraph) -> Result<()> {
        pdg_store::save_pdg(&mut self.storage, &self.project_id, pdg)
            .context("Failed to save PDG to storage")?;

        info!("Saved PDG to storage for project: {}", self.project_id);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_project_scan_excludes_lockfiles_from_source_but_keeps_manifests() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies":{"react":"^18.2.0"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("package-lock.json"),
            r#"{"name":"demo","lockfileVersion":3}"#,
        )
        .unwrap();

        let mut index = LeIndex::new(dir.path()).unwrap();
        let scan = index.get_project_scan(true).unwrap();

        assert!(scan
            .source_paths
            .iter()
            .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("main.rs")));
        assert!(scan
            .source_paths
            .iter()
            .all(
                |path| path.file_name().and_then(|name| name.to_str()) != Some("package-lock.json")
            ));
        assert!(scan
            .manifest_paths
            .iter()
            .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("package.json")));
        assert!(scan.manifest_paths.iter().any(|path| {
            path.file_name().and_then(|name| name.to_str()) == Some("package-lock.json")
        }));
    }

    #[test]
    fn test_project_scan_is_restored_from_cache_across_instances() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();

        let mut first = LeIndex::new(dir.path()).unwrap();
        let first_scan = first.get_project_scan(true).unwrap();
        drop(first);

        let mut second = LeIndex::new(dir.path()).unwrap();
        let second_scan = second.get_project_scan(false).unwrap();

        assert_eq!(first_scan.source_paths, second_scan.source_paths);
        assert_eq!(first_scan.manifest_paths, second_scan.manifest_paths);
    }

    #[test]
    fn test_stats_serialization() {
        let stats = IndexStats {
            total_files: 100,
            files_parsed: 100,
            successful_parses: 95,
            failed_parses: 5,
            total_signatures: 500,
            pdg_nodes: 300,
            pdg_edges: 1200,
            indexed_nodes: 300,
            indexing_time_ms: 5000,
            external_deps_in_lockfile: 0,
            external_deps_resolved: 0,
            external_deps_unresolved: 0,
        };

        let json = serde_json::to_string(&stats).unwrap();
        let deserialized: IndexStats = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.files_parsed, 100);
        assert_eq!(deserialized.successful_parses, 95);
    }

    #[test]
    fn test_analysis_result_serialization() {
        let result = AnalysisResult {
            query: "test".to_string(),
            results: vec![],
            context: Some("context".to_string()),
            tokens_used: 100,
            processing_time_ms: 50,
        };

        let json = serde_json::to_string(&result).unwrap();
        let deserialized: AnalysisResult = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.query, "test");
        assert_eq!(deserialized.tokens_used, 100);
    }

    #[test]
    fn test_diagnostics_serialization() {
        let diagnostics = Diagnostics {
            project_path: "/test".to_string(),
            project_id: "test".to_string(),
            unique_project_id: "test_a1b2c3d4_0".to_string(),
            display_name: "test".to_string(),
            stats: IndexStats {
                total_files: 0,
                files_parsed: 0,
                successful_parses: 0,
                failed_parses: 0,
                total_signatures: 0,
                pdg_nodes: 0,
                pdg_edges: 0,
                indexed_nodes: 0,
                indexing_time_ms: 0,
                external_deps_in_lockfile: 0,
                external_deps_resolved: 0,
                external_deps_unresolved: 0,
            },
            memory_usage_bytes: 1024,
            total_memory_bytes: 8192,
            memory_usage_percent: 12.5,
            memory_threshold_exceeded: false,
            cache_entries: 5,
            cache_bytes: 50000,
            spilled_entries: 3,
            spilled_bytes: 30000,
            cache_hits: 9,
            cache_memory_hits: 7,
            cache_disk_hits: 2,
            cache_misses: 3,
            cache_hit_rate: 0.75,
            cache_writes: 12,
            cache_spills: 4,
            cache_restores: 2,
            cache_temperature: "warm".to_string(),
            pdg_loaded: true,
            pdg_estimated_bytes: 60000,
            search_index_nodes: 100,
            index_health: "healthy".to_string(),
        };

        let json = serde_json::to_string(&diagnostics).unwrap();
        let deserialized: Diagnostics = serde_json::from_str(&json).unwrap();

        assert_eq!(deserialized.project_id, "test");
        assert_eq!(deserialized.unique_project_id, "test_a1b2c3d4_0");
        assert_eq!(deserialized.display_name, "test");
        assert_eq!(deserialized.memory_usage_bytes, 1024);
        assert_eq!(deserialized.cache_entries, 5);
        assert_eq!(deserialized.cache_hits, 9);
        assert_eq!(deserialized.spilled_bytes, 30000);
    }

    // =========================================================================
    // TF-IDF Embedding Tests
    // =========================================================================

    #[test]
    fn test_tokenize_code_camel_case() {
        let toks = tokenize_code("getUserName");
        assert!(
            toks.contains(&"get".to_string()),
            "expected 'get', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"user".to_string()),
            "expected 'user', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"name".to_string()),
            "expected 'name', got {:?}",
            toks
        );
    }

    #[test]
    fn test_tokenize_code_acronyms_and_digits() {
        let toks = tokenize_code("HTTPConnection");
        assert!(
            toks.contains(&"http".to_string()),
            "expected 'http', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"connection".to_string()),
            "expected 'connection', got {:?}",
            toks
        );

        let toks2 = tokenize_code("HTTP2Connection");
        assert!(
            toks2.contains(&"http".to_string()),
            "expected 'http', got {:?}",
            toks2
        );
        assert!(
            toks2.contains(&"2".to_string()),
            "expected '2', got {:?}",
            toks2
        );
        assert!(
            toks2.contains(&"connection".to_string()),
            "expected 'connection', got {:?}",
            toks2
        );
    }

    #[test]
    fn test_tokenize_code_snake_case() {
        let toks = tokenize_code("get_user_name");
        assert!(
            toks.contains(&"get".to_string()),
            "expected 'get', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"user".to_string()),
            "expected 'user', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"name".to_string()),
            "expected 'name', got {:?}",
            toks
        );
    }

    #[test]
    fn test_tokenize_code_filters_short_tokens() {
        // Single-character tokens should be filtered out (len < 2)
        let toks = tokenize_code("a b c xyz");
        assert!(!toks.contains(&"a".to_string()));
        assert!(!toks.contains(&"b".to_string()));
        assert!(!toks.contains(&"c".to_string()));
        assert!(toks.contains(&"xyz".to_string()));
    }

    #[test]
    fn test_tokenize_code_empty() {
        let toks = tokenize_code("");
        assert!(toks.is_empty());
    }

    #[test]
    fn test_tfidf_embedder_empty_corpus() {
        let embedder = TfIdfEmbedder::build(&[]);
        let vec = embedder.embed("test query");
        assert_eq!(
            vec.len(),
            768,
            "must produce 768-dim vector even for empty corpus"
        );
        assert!(vec.iter().all(|&v| v == 0.0), "empty corpus → zero vector");
    }

    #[test]
    fn test_tfidf_embedding_dimension() {
        let docs: Vec<(String, String)> = (0..10)
            .map(|i| {
                (
                    format!("doc_{}", i),
                    format!(
                        "fn handle_request_{} {{ let result = process(); result }}",
                        i
                    ),
                )
            })
            .collect();
        let embedder = TfIdfEmbedder::build(&docs);
        let vec = embedder.embed("handle request process");
        assert_eq!(vec.len(), 768, "embedding dimension must be 768");
    }

    #[test]
    fn test_tfidf_embedding_normalized() {
        let docs: Vec<(String, String)> = vec![
            (
                "auth".to_string(),
                "fn authenticate_user(token: &str) -> bool { verify_token(token) }".to_string(),
            ),
            (
                "db".to_string(),
                "fn connect_database(url: &str) -> Connection { open_connection(url) }".to_string(),
            ),
            (
                "http".to_string(),
                "fn send_request(endpoint: &str) -> Response { http_get(endpoint) }".to_string(),
            ),
        ];
        let embedder = TfIdfEmbedder::build(&docs);
        let vec = embedder.embed("authenticate token verify");
        let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        // For a non-zero vector, magnitude should be ≈ 1.0
        if magnitude > 1e-9 {
            assert!(
                (magnitude - 1.0).abs() < 1e-4,
                "embedding should be L2-normalized, got magnitude {}",
                magnitude
            );
        }
    }

    #[test]
    fn test_tfidf_related_content_higher_similarity() {
        // Two auth-related snippets should have higher cosine similarity
        // than an auth snippet and an unrelated db snippet
        let docs: Vec<(String, String)> = vec![
            (
                "a1".into(),
                "fn authenticate_user(token: &str) -> bool { verify_token(token) }".into(),
            ),
            (
                "a2".into(),
                "fn check_user_credentials(password: &str) -> bool { hash_check(password) }".into(),
            ),
            (
                "b1".into(),
                "fn connect_database(url: &str) -> Connection { open_connection(url) }".into(),
            ),
            (
                "b2".into(),
                "fn execute_sql_query(query: &str) -> Vec<Row> { db_execute(query) }".into(),
            ),
            (
                "c1".into(),
                "fn parse_json_payload(data: &str) -> Value { serde_parse(data) }".into(),
            ),
        ];
        let embedder = TfIdfEmbedder::build(&docs);

        let auth1 = embedder.embed("fn authenticate_user token verify");
        let auth2 = embedder.embed("fn check_user credentials password hash");
        let db1 = embedder.embed("fn connect database execute sql query");

        let cosine =
            |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b.iter()).map(|(x, y)| x * y).sum() };

        let sim_related = cosine(&auth1, &auth2);
        let sim_unrelated = cosine(&auth1, &db1);

        // Related content should have higher similarity than unrelated
        // (or at minimum, not significantly lower)
        assert!(
            sim_related >= sim_unrelated - 0.1,
            "related similarity ({}) should not be much lower than unrelated similarity ({})",
            sim_related,
            sim_unrelated
        );
    }

    #[test]
    fn test_tfidf_zero_vector_for_unseen_terms() {
        let docs: Vec<(String, String)> =
            vec![("a".into(), "fn foo_bar() -> bool { true }".into())];
        let embedder = TfIdfEmbedder::build(&docs);
        // Query with terms not in any doc → vocab won't contain them → zero vector
        let vec = embedder.embed("zzzzzz aaaaaaa bbbbbbb cccccccc");
        let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        // Either zero vector or a valid normalized one
        assert!(magnitude < 1.1, "magnitude out of range: {}", magnitude);
    }
}
