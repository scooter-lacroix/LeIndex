// Core search engine implementation
//
// # Thread Safety
//
// `SearchEngine` is NOT thread-safe for concurrent writes. However:
// - `&SearchEngine` (shared reference) can be safely used for concurrent reads
// - `&mut SearchEngine` requires exclusive access for writes
// - VectorIndex uses internal HashMap which is not thread-safe
//
// For concurrent access, wrap in `Arc<RwLock<SearchEngine>>`.

use crate::search::hnsw::{HNSWIndex, HNSWParams};
use crate::search::quantization::int8_hnsw::{Int8HnswIndex, Int8HnswParams};
use crate::search::query::{MAX_EMBEDDING_DIMENSION, MIN_EMBEDDING_DIMENSION};
use crate::search::ranking::{HybridScorer, Score};
use crate::search::vector::VectorIndex;
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::num::NonZeroUsize;

// ============================================================================
// CONSTANTS & VALIDATION
// ============================================================================

/// Default embedding dimension (CodeRank-compatible)
pub const DEFAULT_EMBEDDING_DIMENSION: usize = 768;

/// Maximum number of nodes that can be indexed (prevents memory exhaustion)
pub const MAX_NODES: usize = 1_000_000;

// ============================================================================
// VECTOR INDEX IMPLEMENTATION
// ============================================================================

/// Vector index implementation
///
/// Enum that wraps either the brute-force VectorIndex or the HNSW-based HNSWIndex.
/// This allows switching between implementations at runtime.
pub enum VectorIndexImpl {
    /// Brute-force vector index (exact search)
    BruteForce(VectorIndex),

    /// HNSW-based approximate nearest neighbor index
    HNSW(Box<HNSWIndex>),
    /// INT8 quantized HNSW-based approximate nearest neighbor index
    HNSWQuantized(Box<Int8HnswIndex>),
}

impl VectorIndexImpl {
    /// Get the number of vectors in the index
    #[must_use]
    pub fn len(&self) -> usize {
        match self {
            Self::BruteForce(idx) => idx.len(),
            Self::HNSW(idx) => idx.len(),
            Self::HNSWQuantized(idx) => idx.len(),
        }
    }

    /// Check if the index is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        match self {
            Self::BruteForce(idx) => idx.is_empty(),
            Self::HNSW(idx) => idx.is_empty(),
            Self::HNSWQuantized(idx) => idx.is_empty(),
        }
    }

    /// Get the embedding dimension
    #[must_use]
    pub fn dimension(&self) -> usize {
        match self {
            Self::BruteForce(idx) => idx.dimension(),
            Self::HNSW(idx) => idx.dimension(),
            Self::HNSWQuantized(idx) => idx.dimension(),
        }
    }

    /// Search for similar vectors
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        match self {
            Self::BruteForce(idx) => idx.search(query, top_k),
            Self::HNSW(idx) => idx.search(query, top_k),
            Self::HNSWQuantized(idx) => idx.search(query, top_k),
        }
    }

    /// Insert a vector into the index
    pub fn insert(&mut self, node_id: String, vector: Vec<f32>) -> Result<(), VectorIndexError> {
        match self {
            Self::BruteForce(idx) => idx
                .insert(node_id, vector)
                .map_err(|e| VectorIndexError::InsertionFailed(e.to_string())),
            Self::HNSW(idx) => idx
                .insert(node_id, vector)
                .map_err(|e| VectorIndexError::InsertionFailed(e.to_string())),
            Self::HNSWQuantized(idx) => idx
                .insert(node_id, vector)
                .map_err(|e| VectorIndexError::InsertionFailed(e.to_string())),
        }
    }

    /// Clear all vectors from the index
    pub fn clear(&mut self) {
        match self {
            Self::BruteForce(idx) => idx.clear(),
            Self::HNSW(idx) => idx.clear(),
            Self::HNSWQuantized(idx) => idx.clear(),
        }
    }

    /// Remove a vector from the index by node ID.
    ///
    /// Returns `true` if the node was found and removed, `false` otherwise.
    /// For HNSW indexes, removal is lazy (marks as deleted); use `rebuild()` to reclaim memory.
    pub fn remove(&mut self, node_id: &str) -> bool {
        match self {
            Self::BruteForce(idx) => idx.remove(node_id),
            Self::HNSW(idx) => idx.remove(node_id),
            Self::HNSWQuantized(idx) => idx.remove(node_id),
        }
    }

    /// Check if HNSW is enabled
    #[must_use]
    pub fn is_hnsw_enabled(&self) -> bool {
        matches!(self, Self::HNSW(_) | Self::HNSWQuantized(_))
    }

    /// Get estimated memory usage in bytes
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        match self {
            Self::BruteForce(idx) => (*idx).estimated_memory_bytes(),
            Self::HNSW(idx) => (*idx).estimated_memory_bytes(),
            Self::HNSWQuantized(idx) => (*idx).estimated_memory_bytes(),
        }
    }
}

/// Vector index errors
#[derive(Debug, thiserror::Error)]
pub enum VectorIndexError {
    /// Failed to insert a vector into the index
    #[error("Insertion failed: {0}")]
    InsertionFailed(String),

    /// General index operation failure
    #[error("Index operation failed: {0}")]
    IndexOperationFailed(String),
}

// ============================================================================
// NODE INFORMATION
// ============================================================================

/// Node information for indexing
///
/// This represents a single code node (function, class, module) that can be
/// indexed and searched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeInfo {
    /// Unique node ID
    pub node_id: String,

    /// File path
    pub file_path: String,

    /// Symbol name
    pub symbol_name: String,

    /// Programming language
    pub language: String,

    /// Source content
    pub content: String,

    /// Byte range in source
    pub byte_range: (usize, usize),

    /// TF-IDF embedding (always present, 768-dim, for hybrid search)
    #[serde(default)]
    pub tfidf_embedding: Vec<f32>,

    /// Neural/remote embedding (optional enhancement, for hybrid search)
    pub neural_embedding: Option<Vec<f32>>,

    /// Legacy embedding field (points to tfidf_embedding for backward compatibility)
    #[serde(default)]
    pub embedding: Option<Vec<f32>>,

    /// Complexity score (0-100+, higher = more complex)
    pub complexity: u32,

    /// Cached signature extracted from content (for search results)
    /// This is extracted before content is cleared during T13 optimization
    pub signature: Option<String>,

    /// Pre-tokenized search tokens (lowercased, filtered by length >= 2).
    ///
    /// When `Some`, these tokens are used directly for the inverted index
    /// instead of re-tokenizing from `content`. This enables callers that
    /// already have tokenized content (e.g., `index_builder`) to skip the
    /// redundant split+lowercase pass.
    ///
    /// Backward-compatible: `None` falls back to `content.split()` tokenization.
    #[serde(default)]
    pub pre_tokenized: Option<Vec<String>>,
}

/// Pre-computed query data for optimized text scoring
///
/// This struct holds data that is pre-computed once per search to avoid
/// repeated allocations in the hot path. When searching N nodes, this reduces
/// allocations from O(N) to O(1).
struct TextQueryPreprocessed {
    /// Lowercase query for case-insensitive matching
    query_lower: String,
    /// Query tokens for overlap calculation
    query_tokens: HashSet<String>,
}

impl TextQueryPreprocessed {
    /// Create pre-computed query data
    fn from_query(query: &str) -> Self {
        let query_lower = query.to_ascii_lowercase();
        // Tokenize using the same logic as the content indexing
        let query_tokens: HashSet<_> = query
            .split(|c: char| !c.is_alphanumeric())
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| s.len() >= 2)
            .collect();

        Self {
            query_lower,
            query_tokens,
        }
    }
}

// ============================================================================
// SEARCH QUERY
// ============================================================================

/// Search query
///
/// This represents a search request with all parameters needed to execute
/// a search operation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Query text
    pub query: String,

    /// Maximum results to return (validated by QueryParser)
    pub top_k: usize,

    /// Token budget for context expansion (validated by QueryParser)
    pub token_budget: Option<usize>,

    /// Whether to use semantic search
    pub semantic: bool,

    /// Whether to expand context using graph traversal
    pub expand_context: bool,

    /// Optional query embedding for semantic search
    pub query_embedding: Option<Vec<f32>>,

    /// Minimum relevance threshold (0.0-1.0)
    pub threshold: Option<f32>,

    /// Query type for adaptive ranking
    pub query_type: Option<crate::search::ranking::QueryType>,
}

// ============================================================================
// SEARCH RESULT
// ============================================================================

/// Search result
///
/// This represents a single search result with all relevant metadata.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Result rank (1-based)
    pub rank: usize,

    /// Node ID
    pub node_id: String,

    /// File path
    pub file_path: String,

    /// Symbol name
    pub symbol_name: String,

    /// Symbol type: function | method | class | variable | module
    ///
    /// Populated by `LeIndex::search()` from PDG node type.
    /// `None` when the node is not in the PDG (e.g., external refs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol_type: Option<String>,

    /// First line of the symbol's source (declaration / signature).
    ///
    /// Extracted from `node.content` — the second line after the
    /// `// name in path` header comment, trimmed of leading whitespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,

    /// Cyclomatic complexity score of the symbol.
    pub complexity: u32,

    /// Number of call-sites that invoke this symbol (direct callers in PDG).
    ///
    /// Populated by `LeIndex::search()`. `None` if PDG is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caller_count: Option<usize>,

    /// Number of symbols this symbol depends on (outgoing PDG edges).
    ///
    /// Populated by `LeIndex::search()`. `None` if PDG is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dependency_count: Option<usize>,

    /// Programming language
    pub language: String,

    /// Relevance score
    pub score: Score,

    /// Expanded context (if requested)
    pub context: Option<String>,

    /// Byte range in source
    pub byte_range: (usize, usize),
}

// ============================================================================
// SEARCH ENGINE
// ============================================================================

/// Search engine combining vector and graph search
///
/// This is the main entry point for search operations. It combines:
/// - Text-based search for keyword matching
/// - Vector-based semantic search for similarity
/// - Hybrid scoring combining multiple signals
///
/// Supports both brute-force and HNSW vector search backends.
///
/// # Thread Safety
///
/// - Reads (`&SearchEngine`) are thread-safe for concurrent access
/// - Writes (`&mut SearchEngine`) require exclusive access
/// - The internal VectorIndexImpl is NOT thread-safe for concurrent writes
///
/// For concurrent read-write access, wrap in `Arc<RwLock<SearchEngine>>`.
///
/// # Example
///
/// ```ignore
/// let mut engine = SearchEngine::new();
/// engine.index_nodes(nodes);
/// let results = engine.search(query)?;
/// ```
pub struct SearchEngine {
    nodes: Vec<NodeInfo>,
    scorer: HybridScorer,
    vector_index: VectorIndexImpl,
    /// Complexity cache for O(1) lookups (fixes O(n²) bug)
    complexity_cache: HashMap<String, u32>,
    /// Inverted index for O(1) text lookups: token -> set of node IDs
    /// This allows sub-linear text search instead of O(N) scan
    text_index: HashMap<String, HashSet<String>>,
    /// Node ID to index mapping for O(1) node lookups (fixes A1)
    /// Populated during index_nodes() and maintained on updates
    node_id_to_idx: HashMap<String, usize>,
    /// Per-node token cache: node_id -> set of normalized tokens
    /// Populated during index_nodes() to avoid re-tokenization in scoring
    node_tokens: HashMap<String, HashSet<String>>,
    /// Result cache for repeated queries (A+ Section 8.1: bounded by entries and bytes)
    search_cache: LruCache<String, Vec<SearchResult>>,
    /// Tracked byte estimate for the search cache
    search_cache_bytes: usize,
}

// A+ Search cache budget constants (Section 8.1)
/// Maximum entries in the search cache.
const SEARCH_CACHE_MAX_ENTRIES: usize = 256;
/// Maximum total bytes for the search cache.
const SEARCH_CACHE_MAX_BYTES: usize = 16 * 1024 * 1024; // 16 MiB

impl SearchEngine {
    /// Create a new search engine with default 768-dim embeddings
    ///
    /// Uses brute-force vector search by default.
    ///
    /// # Panics
    ///
    /// This never panics - all initialization is infallible.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndexImpl::BruteForce(VectorIndex::new(
                DEFAULT_EMBEDDING_DIMENSION,
            )),
            complexity_cache: HashMap::new(),
            text_index: HashMap::new(),
            node_id_to_idx: HashMap::new(),
            node_tokens: HashMap::new(),
            search_cache: LruCache::new(NonZeroUsize::new(SEARCH_CACHE_MAX_ENTRIES).unwrap()),
            search_cache_bytes: 0,
        }
    }

    /// Create a new search engine with custom embedding dimension
    ///
    /// Uses brute-force vector search by default.
    ///
    /// # Arguments
    ///
    /// * `dimension` - Embedding vector dimension (1-10000)
    ///
    /// # Panics
    ///
    /// Panics if dimension is 0 or exceeds MAX_EMBEDDING_DIMENSION.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let engine = SearchEngine::with_dimension(128);
    /// ```
    #[must_use]
    pub fn with_dimension(dimension: usize) -> Self {
        // Validate dimension at construction time
        if !(MIN_EMBEDDING_DIMENSION..=MAX_EMBEDDING_DIMENSION).contains(&dimension) {
            panic!(
                "Invalid embedding dimension: {} (must be between {} and {})",
                dimension, MIN_EMBEDDING_DIMENSION, MAX_EMBEDDING_DIMENSION
            );
        }

        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndexImpl::BruteForce(VectorIndex::new(dimension)),
            complexity_cache: HashMap::new(),
            text_index: HashMap::new(),
            node_id_to_idx: HashMap::new(),
            node_tokens: HashMap::new(),
            search_cache: LruCache::new(NonZeroUsize::new(SEARCH_CACHE_MAX_ENTRIES).unwrap()),
            search_cache_bytes: 0,
        }
    }

    /// Index nodes for searching
    ///
    /// This builds the internal indexes needed for search:
    /// - Text search index (stored in self.nodes)
    /// - Vector index (built from embeddings)
    /// - Complexity cache (for O(1) complexity lookups)
    ///
    /// # Arguments
    ///
    /// * `nodes` - Vector of nodes to index
    ///
    /// # Performance
    ///
    /// - Time complexity: O(n) where n is number of nodes
    /// - Space complexity: O(n) for storage + O(n) for embeddings
    ///
    /// # Panics
    ///
    /// Panics if node count exceeds MAX_NODES (prevents memory exhaustion).
    pub fn index_nodes(&mut self, mut nodes: Vec<NodeInfo>) {
        if nodes.len() > MAX_NODES {
            panic!(
                "Cannot index more than {} nodes (provided: {})",
                MAX_NODES,
                nodes.len()
            );
        }

        // Clear cache when re-indexing
        self.complexity_cache.clear();
        self.text_index.clear();
        self.search_cache.clear();
        self.search_cache_bytes = 0;
        self.node_id_to_idx.clear();
        self.node_tokens.clear();
        self.vector_index.clear();

        // Build node_id_to_idx for O(1) node lookups (A1 optimization)
        // Build complexity cache, inverted index, and token cache before taking ownership
        for (idx, node) in nodes.iter().enumerate() {
            self.node_id_to_idx.insert(node.node_id.clone(), idx);
            self.complexity_cache
                .insert(node.node_id.clone(), node.complexity);

            // Build inverted index for O(1) text lookups
            // This maps each token to the set of node IDs containing it
            // Also build per-node token cache for scoring (T14 optimization)
            //
            // R8: Use pre-tokenized tokens when available to skip re-tokenization.
            // Falls back to content-based tokenization for backward compatibility.
            let mut tokens = HashSet::new();
            if let Some(pre_tok) = &node.pre_tokenized {
                // Use pre-computed tokens directly (already lowercased, filtered >= 2 chars)
                for token in pre_tok {
                    self.text_index
                        .entry(token.clone())
                        .or_default()
                        .insert(node.node_id.clone());
                    tokens.insert(token.clone());
                }
            } else {
                for token in node.content.split(|c: char| !c.is_alphanumeric()) {
                    let normalized_token: String = token.to_ascii_lowercase();
                    // Skip empty tokens and very short ones (< 2 chars) to reduce noise
                    if normalized_token.len() >= 2 {
                        self.text_index
                            .entry(normalized_token.clone())
                            .or_default()
                            .insert(node.node_id.clone());
                        tokens.insert(normalized_token);
                    }
                }
            }
            self.node_tokens.insert(node.node_id.clone(), tokens);
        }

        // Build vector index from TF-IDF embeddings — clone only embeddings (A4 optimization)
        // All other node content is moved via ownership, avoiding a full Vec clone
        for node in nodes.iter_mut() {
            // Use tfidf_embedding (always present) instead of optional embedding
            if !node.tfidf_embedding.is_empty() {
                if let Err(e) = self
                    .vector_index
                    .insert(node.node_id.clone(), node.tfidf_embedding.clone())
                {
                    tracing::warn!(
                        "Failed to insert TF-IDF embedding for node {}: {:?}",
                        node.node_id,
                        e
                    );
                }
            }

            // For backward compatibility, set embedding to tfidf_embedding
            node.embedding = if !node.tfidf_embedding.is_empty() {
                Some(node.tfidf_embedding.clone())
            } else {
                None
            };
        }

        // Move nodes into storage — no clone needed since indexes are already built
        self.nodes = nodes;

        // Extract signatures before clearing content (for search results)
        // This must happen before T13 optimization clears the content
        for node in &mut self.nodes {
            node.signature = Self::extract_signature_from_content(&node.content);
        }

        // Free content memory after all indexes are built (T13 optimization)
        // The inverted index (text_index) already captures all tokens,
        // and the Storage layer retains original source files on disk.
        // This reduces memory by ~15MB at 5K nodes.
        for node in &mut self.nodes {
            node.content.clear();
        }
    }

    /// Extract signature from node content.
    ///
    /// Returns the first non-empty, non-comment line after the header.
    pub fn extract_signature_from_content(content: &str) -> Option<String> {
        content
            .lines()
            .skip(1) // skip "// name in path" header
            .map(|l| l.trim())
            .find(|l| !l.is_empty() && !l.starts_with("// [No source") && !l.starts_with("// ["))
            .map(|l| l.to_string())
    }

    /// Apply an incremental delta update to the text index.
    ///
    /// This removes and adds/updates nodes without rebuilding the entire index,
    /// making it significantly faster than `index_nodes()` for small changes.
    ///
    /// # Arguments
    ///
    /// * `delta` - The delta describing nodes to remove and add/update.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(K) where K is the number of changed nodes
    /// - Full rebuild is O(N) — incremental is faster when K << N
    ///
    /// # Panics
    ///
    /// Panics if the total node count after the update exceeds `MAX_NODES`.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let delta = TextIndexDelta {
    ///     removed_node_ids: vec!["old_func".to_string()],
    ///     updated_nodes: vec![new_node],
    /// };
    /// engine.incremental_reindex(delta);
    /// ```
    pub fn incremental_reindex(&mut self, delta: TextIndexDelta) {
        // Invalidate search cache — results may change
        self.search_cache.clear();
        self.search_cache_bytes = 0;

        // Phase 1: Remove nodes
        for node_id in &delta.removed_node_ids {
            self.remove_node_from_index(node_id);
        }

        // Phase 2: Add/update nodes
        for node in delta.updated_nodes {
            self.add_node_to_index(node);
        }

        // Verify we don't exceed limits
        if self.nodes.len() > MAX_NODES {
            panic!(
                "Cannot index more than {} nodes (current: {})",
                MAX_NODES,
                self.nodes.len()
            );
        }
    }

    /// Remove a single node from all index structures.
    ///
    /// This is O(T) where T is the number of unique tokens in the removed node.
    fn remove_node_from_index(&mut self, node_id: &str) {
        // Remove from node_id_to_idx
        let Some(removed_idx) = self.node_id_to_idx.remove(node_id) else {
            return; // Node not in index, nothing to do
        };

        // Remove from text_index: for each token the node contributed to,
        // remove the node_id from the token's set. Clean up empty sets.
        if let Some(tokens) = self.node_tokens.remove(node_id) {
            for token in tokens {
                if let Entry::Occupied(mut entry) = self.text_index.entry(token) {
                    entry.get_mut().remove(node_id);
                    if entry.get().is_empty() {
                        entry.remove();
                    }
                }
            }
        }

        // Remove from complexity_cache
        self.complexity_cache.remove(node_id);

        // Remove from vector_index
        self.vector_index.remove(node_id);

        // Remove from nodes Vec and fix indices
        // Swap-remove for O(1) removal, then fix the swapped node's index
        if removed_idx < self.nodes.len() {
            self.nodes.swap_remove(removed_idx);
            // If we didn't remove the last element, the swapped element needs
            // its index updated in node_id_to_idx
            if removed_idx < self.nodes.len() {
                let swapped_id = self.nodes[removed_idx].node_id.clone();
                self.node_id_to_idx.insert(swapped_id, removed_idx);
            }
        }
    }

    /// Add or update a single node in all index structures.
    ///
    /// If the node already exists (same `node_id`), it is removed first, then
    /// re-added with the new data.
    fn add_node_to_index(&mut self, mut node: NodeInfo) {
        // If node already exists, remove the old version first
        if self.node_id_to_idx.contains_key(&node.node_id) {
            self.remove_node_from_index(&node.node_id);
        }

        let node_id = node.node_id.clone();
        let new_idx = self.nodes.len();

        // Build inverted index entries and token cache for this node
        //
        // R8: Use pre-tokenized tokens when available to skip re-tokenization.
        // Falls back to content-based tokenization for backward compatibility.
        let mut tokens = HashSet::new();
        if let Some(pre_tok) = &node.pre_tokenized {
            for token in pre_tok {
                self.text_index
                    .entry(token.clone())
                    .or_default()
                    .insert(node_id.clone());
                tokens.insert(token.clone());
            }
        } else {
            for token in node.content.split(|c: char| !c.is_alphanumeric()) {
                let normalized_token: String = token.to_ascii_lowercase();
                if normalized_token.len() >= 2 {
                    self.text_index
                        .entry(normalized_token.clone())
                        .or_default()
                        .insert(node_id.clone());
                    tokens.insert(normalized_token);
                }
            }
        }
        self.node_tokens.insert(node_id.clone(), tokens);

        // Update node_id_to_idx
        self.node_id_to_idx.insert(node_id.clone(), new_idx);

        // Update complexity_cache
        self.complexity_cache
            .insert(node_id.clone(), node.complexity);

        // Insert TF-IDF embedding into vector index (always present)
        if !node.tfidf_embedding.is_empty() {
            if let Err(e) = self
                .vector_index
                .insert(node_id.clone(), node.tfidf_embedding.clone())
            {
                tracing::warn!(
                    "Failed to insert TF-IDF embedding for node {}: {:?}",
                    node_id,
                    e
                );
            }
        }

        // For backward compatibility, set embedding to tfidf_embedding
        node.embedding = if !node.tfidf_embedding.is_empty() {
            Some(node.tfidf_embedding.clone())
        } else {
            None
        };

        // Extract signature before clearing content (same as index_nodes does)
        node.signature = Self::extract_signature_from_content(&node.content);

        // Clear content to save memory (same as index_nodes does)
        node.content.clear();

        // Add to nodes Vec
        self.nodes.push(node);
    }

    /// Get the number of indexed nodes
    ///
    /// # Returns
    ///
    /// The number of nodes currently indexed.
    #[must_use]
    pub fn node_count(&self) -> usize {
        self.nodes.len()
    }

    /// Collect all (node_id, embedding) pairs from the indexed nodes.
    ///
    /// Returns only nodes that have an embedding. Used by the mmap
    /// persistence layer to write embeddings to disk.
    pub fn collect_embeddings(&self) -> Vec<(String, Vec<f32>)> {
        self.nodes
            .iter()
            .filter_map(|n| {
                n.embedding
                    .as_ref()
                    .map(|emb| (n.node_id.clone(), emb.clone()))
            })
            .collect()
    }

    /// Check if the index is empty
    ///
    /// # Returns
    ///
    /// `true` if no nodes are indexed, `false` otherwise.
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Execute a search query
    ///
    /// This performs a hybrid search combining:
    /// - Text matching (substring + token overlap)
    /// - Semantic similarity (if embeddings available)
    /// - Structural relevance (complexity-based)
    ///
    /// # Arguments
    ///
    /// * `query` - Search query with all parameters
    ///
    /// # Returns
    ///
    /// Vector of search results sorted by relevance (highest first).
    ///
    /// # Performance
    ///
    /// - Time complexity: O(n) where n is number of nodes
    /// - Space complexity: O(k) where k is top_k (results)
    ///
    /// # Errors
    ///
    /// Returns `Error::QueryFailed` if the search operation fails.
    pub fn search(&mut self, query: SearchQuery) -> Result<Vec<SearchResult>, Error> {
        if self.nodes.is_empty() {
            return Ok(Vec::new());
        }

        // Check cache first
        let cache_key = format!(
            "{}:{}:{:?}:{}",
            query.query, query.top_k, query.threshold, query.semantic
        );
        if let Some(cached) = self.search_cache.get(&cache_key) {
            return Ok(cached.clone());
        }

        let mut results = Vec::new();

        // Pre-compute vector search if semantic search is requested
        let vector_results: std::collections::HashMap<String, f32> = if query.semantic {
            // Use provided query embedding if available
            let embedding = if let Some(emb) = query.query_embedding {
                Some(emb)
            } else {
                // Fallback: find if there's a node with an embedding we can use
                // This is legacy behavior, should be avoided
                self.nodes
                    .iter()
                    .find_map(|n| n.embedding.as_ref())
                    .cloned()
            };

            if let Some(emb) = embedding {
                self.vector_index
                    .search(&emb, query.top_k)
                    .into_iter()
                    .collect()
            } else {
                std::collections::HashMap::new()
            }
        } else {
            std::collections::HashMap::new()
        };

        // Pre-compute query data for optimized text scoring
        // This reduces allocations from O(N) to O(1) per search
        let text_query = TextQueryPreprocessed::from_query(&query.query);

        // Use inverted index to filter candidates - only check nodes that contain query terms
        // This reduces search complexity from O(N) to O(M) where M is number of matching nodes
        let candidates = if text_query.query_tokens.is_empty() {
            // No query tokens, check all nodes
            self.nodes.iter().collect::<Vec<_>>()
        } else {
            // Build candidate set using inverted index - O(1) per token lookup
            let mut candidate_ids: HashSet<&str> = HashSet::new();

            for token in &text_query.query_tokens {
                if let Some(node_ids) = self.text_index.get(token) {
                    for node_id in node_ids {
                        candidate_ids.insert(node_id.as_str());
                    }
                }
            }

            // If no matches in inverted index, return empty results early
            if candidate_ids.is_empty() && !query.semantic {
                return Ok(Vec::new());
            }

            // Convert candidate IDs to node references
            if candidate_ids.is_empty() {
                // If no text matches, but we have semantic search, check all nodes
                // (Optimization: we could limit to just the vector search hits)
                self.nodes.iter().collect()
            } else {
                // We have text matches. If we also have semantic results, we must include them
                // even if they don't match keywords.
                if vector_results.is_empty() {
                    self.nodes
                        .iter()
                        .filter(|node| candidate_ids.contains(node.node_id.as_str()))
                        .collect()
                } else {
                    // Union of text matches and semantic matches
                    self.nodes
                        .iter()
                        .filter(|node| {
                            candidate_ids.contains(node.node_id.as_str())
                                || vector_results.contains_key(&node.node_id)
                        })
                        .collect()
                }
            }
        };

        for node in candidates {
            let text_score = self.calculate_text_score_optimized(
                &text_query,
                &node.node_id,
                &node.symbol_name,
                &node.file_path,
            );

            // Get TF-IDF score from vector search results
            let tfidf_score = if query.semantic {
                *vector_results.get(&node.node_id).unwrap_or(&0.0)
            } else {
                0.0
            };

            // For now, if no text match and not semantic, skip
            if text_score == 0.0 && !query.semantic && tfidf_score == 0.0 {
                continue;
            }

            // Normalize complexity to 0-1 range (divide by 100, not 10)
            let structural_score = (node.complexity as f32 / 100.0).min(1.0);

            // Neural score is 0.0 in this context (vector index only has TF-IDF embeddings)
            let neural_score = 0.0;

            // Use custom weights based on query type if provided
            let score = if let Some(qt) = query.query_type {
                match qt {
                    crate::search::ranking::QueryType::Text => {
                        // Prose/Text mode: heavily favor keyword overlap
                        self.scorer
                            .with_weights_hybrid(0.2, 0.05, 0.05, 0.7)
                            .score_hybrid(tfidf_score, neural_score, structural_score, text_score)
                    }
                    crate::search::ranking::QueryType::Semantic => {
                        // Semantic-heavy mode
                        self.scorer
                            .with_weights_hybrid(0.7, 0.1, 0.1, 0.1)
                            .score_hybrid(tfidf_score, neural_score, structural_score, text_score)
                    }
                    crate::search::ranking::QueryType::Structural => {
                        // Structural-heavy mode
                        self.scorer
                            .with_weights_hybrid(0.3, 0.0, 0.5, 0.2)
                            .score_hybrid(tfidf_score, neural_score, structural_score, text_score)
                    }
                }
            } else {
                // Default hybrid scoring
                self.scorer
                    .score_hybrid(tfidf_score, neural_score, structural_score, text_score)
            };

            if score.overall > 0.0 {
                // Apply relevance threshold if specified
                if let Some(threshold) = query.threshold {
                    if score.overall < threshold {
                        continue;
                    }
                }

                // Use cached signature (extracted before content was cleared)
                let signature = node.signature.clone();

                results.push(SearchResult {
                    rank: 0, // Will be set after sorting
                    node_id: node.node_id.clone(),
                    file_path: node.file_path.clone(),
                    symbol_name: node.symbol_name.clone(),
                    symbol_type: None, // enriched by LeIndex::search()
                    signature,
                    complexity: node.complexity,
                    caller_count: None,     // enriched by LeIndex::search()
                    dependency_count: None, // enriched by LeIndex::search()
                    language: node.language.clone(),
                    score,
                    context: None,
                    byte_range: node.byte_range,
                });
            }
        }

        // Sort by score (descending)
        results.sort_by(|a, b| {
            b.score
                .overall
                .partial_cmp(&a.score.overall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // Take top_k
        let top_k = results.into_iter().take(query.top_k).collect::<Vec<_>>();

        let mut final_results = top_k;
        for (i, result) in final_results.iter_mut().enumerate() {
            result.rank = i + 1;
        }

        // Cache results with byte-budget enforcement (A+ Section 8.1)
        {
            let results_bytes = Self::estimate_search_results_bytes(&final_results);
            // If replacing an existing entry, subtract its bytes first
            if let Some(existing) = self.search_cache.get(&cache_key) {
                self.search_cache_bytes = self
                    .search_cache_bytes
                    .saturating_sub(Self::estimate_search_results_bytes(existing));
            }
            // Evict until there is room
            while self.search_cache_bytes + results_bytes > SEARCH_CACHE_MAX_BYTES
                && !self.search_cache.is_empty()
            {
                if let Some((_, evicted)) = self.search_cache.pop_lru() {
                    self.search_cache_bytes = self
                        .search_cache_bytes
                        .saturating_sub(Self::estimate_search_results_bytes(&evicted));
                }
            }
            self.search_cache_bytes += results_bytes;
            self.search_cache.put(cache_key, final_results.clone());
        }

        Ok(final_results)
    }

    /// Optimized text score calculation using cached node tokens and pre-computed query data
    ///
    /// Uses the node_tokens HashMap for O(1) token overlap calculation instead of
    /// iterating over the inverted index per query token. Tokens are cached during
    /// index_nodes() — no re-tokenization in the scoring hot path.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(min(q, t)) where q = query tokens, t = node tokens (set intersection)
    /// - Space complexity: O(1) — no allocations per call
    fn calculate_text_score_optimized(
        &self,
        precomputed: &TextQueryPreprocessed,
        node_id: &str,
        symbol_name: &str,
        file_path: &str,
    ) -> f32 {
        // Boost matches in symbol name
        let symbol_boost = if symbol_name
            .to_ascii_lowercase()
            .contains(&precomputed.query_lower)
        {
            0.5
        } else {
            0.0
        };

        // Penalty for test-related files to address Limitation 4
        let test_penalty = if file_path.to_ascii_lowercase().contains("test")
            || symbol_name.to_ascii_lowercase().contains("test")
        {
            0.3
        } else {
            0.0
        };

        // Use cached node tokens for overlap calculation (T14 optimization)
        // Tokens were cached during index_nodes() — no re-tokenization needed.
        // This avoids iterating over each query token and checking the inverted index,
        // replacing it with a single set intersection on pre-cached per-node tokens.
        let base_score = if precomputed.query_tokens.is_empty() {
            // No meaningful tokens in query
            0.0
        } else if let Some(node_tokens) = self.node_tokens.get(node_id) {
            // Count overlap between query tokens and cached node tokens
            let matching = precomputed.query_tokens.intersection(node_tokens).count();
            matching as f32 / precomputed.query_tokens.len() as f32
        } else {
            0.0
        };

        ((base_score + symbol_boost) - test_penalty).clamp(0.0, 1.0)
    }

    /// Semantic search for entry points
    ///
    /// This method performs vector similarity search using cosine similarity.
    /// For now, it requires pre-computed embeddings in the indexed nodes.
    ///
    /// # Arguments
    ///
    /// * `query_embedding` - Query embedding vector (must match index dimension)
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// Vector of semantic entries sorted by similarity score
    ///
    /// # Example
    ///
    /// ```ignore
    /// let query_embedding = vec![0.1, 0.2, 0.3, ...]; // 768-dim vector
    /// let results = engine.semantic_search(&query_embedding, 10).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Error::QueryFailed` if dimension mismatch or search fails.
    pub fn semantic_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SemanticEntry>, Error> {
        // Return early if index is empty (no need to validate dimensions in this case)
        if self.vector_index.is_empty() {
            return Ok(Vec::new());
        }

        // Validate embedding dimension (only needed when we actually have embeddings)
        if query_embedding.len() != self.vector_index.dimension() {
            return Err(Error::QueryFailed(format!(
                "Embedding dimension mismatch: expected {}, got {}",
                self.vector_index.dimension(),
                query_embedding.len()
            )));
        }

        // Perform vector similarity search
        let results = self.vector_index.search(query_embedding, top_k);

        // Convert to SemanticEntry format using O(1) HashMap lookup
        let entries = results
            .into_iter()
            .map(|(node_id, score)| {
                // O(1) lookup via node_id_to_idx instead of O(N) linear scan
                let entry_type = self
                    .node_id_to_idx
                    .get(&node_id)
                    .and_then(|&idx| self.nodes.get(idx))
                    .map(|_| EntryType::Function)
                    .unwrap_or(EntryType::Function);

                SemanticEntry {
                    node_id,
                    relevance: score,
                    entry_type,
                }
            })
            .collect();

        Ok(entries)
    }

    /// Get the vector index for direct access
    ///
    /// This provides access to the underlying vector index for advanced use cases.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let dimension = engine.vector_index().dimension();
    /// let count = engine.vector_index().len();
    /// ```
    #[must_use]
    pub fn vector_index(&self) -> &VectorIndexImpl {
        &self.vector_index
    }

    /// Get mutable access to the vector index
    ///
    /// This allows direct manipulation of the vector index.
    ///
    /// # Thread Safety
    ///
    /// **WARNING:** This method requires `&mut self` which ensures exclusive access.
    /// Never call this concurrently with any other method on the same instance.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let index = engine.vector_index_mut();
    /// index.insert("new_node", embedding)?;
    /// ```
    pub fn vector_index_mut(&mut self) -> &mut VectorIndexImpl {
        &mut self.vector_index
    }

    /// Enable HNSW for faster approximate search
    ///
    /// This converts the vector index from brute-force to HNSW-based.
    /// Existing indexed vectors are **NOT** automatically migrated - you must
    /// re-index your data after enabling HNSW.
    ///
    /// # Arguments
    ///
    /// * `params` - Optional HNSW parameters (uses defaults if None)
    ///
    /// # Example
    ///
    /// ```ignore
    /// engine.enable_hnsw(None);
    /// engine.index_nodes(nodes); // Re-index with HNSW
    /// ```
    pub fn enable_hnsw(&mut self, params: Option<HNSWParams>) {
        let dimension = self.vector_index.dimension();
        let params = params.unwrap_or_default();
        self.vector_index =
            VectorIndexImpl::HNSW(Box::new(HNSWIndex::with_params(dimension, params)));
    }

    /// Check if HNSW is currently enabled
    #[must_use]
    pub fn is_hnsw_enabled(&self) -> bool {
        matches!(
            self.vector_index,
            VectorIndexImpl::HNSW(_) | VectorIndexImpl::HNSWQuantized(_)
        )
    }

    /// Disable HNSW and switch back to brute-force search
    ///
    /// This clears the current vector index and creates a new brute-force index.
    /// You'll need to re-index your data after disabling HNSW.
    pub fn disable_hnsw(&mut self) {
        let dimension = self.vector_index.dimension();
        self.vector_index = VectorIndexImpl::BruteForce(VectorIndex::new(dimension));
    }

    /// Create a new search engine with HNSW enabled
    ///
    /// # Arguments
    ///
    /// * `dimension` - Embedding vector dimension
    /// * `params` - HNSW parameters
    ///
    /// # Example
    ///
    /// ```ignore
    /// let engine = SearchEngine::with_hnsw(128, HNSWParams::default());
    /// ```
    #[must_use]
    pub fn with_hnsw(dimension: usize, params: HNSWParams) -> Self {
        let mut engine = Self::with_dimension(dimension);
        engine.enable_hnsw(Some(params));
        engine
    }

    /// Enable INT8 quantized HNSW for memory-efficient search
    ///
    /// This provides ~74% memory reduction compared to f32 HNSW while
    /// maintaining search accuracy through asymmetric distance computation.
    ///
    /// # Arguments
    ///
    /// * `params` - Optional INT8 HNSW parameters (uses defaults if None)
    ///
    /// # Example
    ///
    /// ```ignore
    /// engine.enable_int8_hnsw(None);
    /// engine.index_nodes(nodes); // Re-index with INT8 quantization
    /// ```
    pub fn enable_int8_hnsw(&mut self, params: Option<Int8HnswParams>) {
        let dimension = self.vector_index.dimension();
        let params = params.unwrap_or_default();
        self.vector_index =
            VectorIndexImpl::HNSWQuantized(Box::new(Int8HnswIndex::with_params(dimension, params)));
    }

    /// Check if the current index is quantized
    #[must_use]
    pub fn is_quantized(&self) -> bool {
        matches!(self.vector_index, VectorIndexImpl::HNSWQuantized(_))
    }

    /// Estimate memory usage in bytes
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        // Rough estimate based on implementation
        // Content is cleared after indexing (T13), so no +256 content estimate needed
        let nodes_size = self.nodes.len() * std::mem::size_of::<NodeInfo>();
        let cache_size = self.complexity_cache.len()
            * (std::mem::size_of::<String>() + std::mem::size_of::<u32>());
        let text_index_size = self
            .text_index
            .values()
            .map(|set| set.len() * std::mem::size_of::<String>())
            .sum::<usize>();

        nodes_size + cache_size + text_index_size + self.vector_index.estimated_memory_bytes()
    }

    /// Estimate byte size of a slice of search results for cache accounting.
    fn estimate_search_results_bytes(results: &[SearchResult]) -> usize {
        results
            .iter()
            .map(|r| {
                r.node_id.len()
                    + r.file_path.len()
                    + r.symbol_name.len()
                    + r.symbol_type.as_ref().map_or(0, |s| s.len())
                    + r.signature.as_ref().map_or(0, |s| s.len())
                    + r.language.len()
                    + r.context.as_ref().map_or(0, |c| c.len())
                    + 128 // overhead estimate for rank, score, complexity, byte_range, etc.
            })
            .sum()
    }
}

/// Delta update for the text index.
///
/// Describes which nodes to remove and which to add/update, enabling
/// incremental reindexing without rebuilding the entire index from scratch.
///
/// # Performance
///
/// Incremental updates are O(K) where K is the number of changed nodes,
/// compared to O(N) for a full `index_nodes()` rebuild.
///
/// # Example
///
/// ```ignore
/// let delta = TextIndexDelta {
///     removed_node_ids: vec!["old_func".to_string()],
///     updated_nodes: vec![updated_node_info],
/// };
/// engine.incremental_reindex(delta);
/// ```
#[derive(Debug, Default)]
pub struct TextIndexDelta {
    /// Node IDs to remove from the index.
    pub removed_node_ids: Vec<String>,
    /// New or updated nodes to add to the index.
    /// Nodes whose `node_id` already exists will be replaced in-place.
    pub updated_nodes: Vec<NodeInfo>,
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Entry type for semantic search results
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryType {
    /// Function entry point
    Function,
    /// Method entry point
    Method,
    /// Class/struct entry point
    Class,
    /// Module-level entry point
    Module,
}

/// Semantic entry for entry point detection
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEntry {
    /// Node ID
    pub node_id: String,
    /// Relevance score
    pub relevance: f32,
    /// Entry type
    pub entry_type: EntryType,
}

/// Search errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Query execution failed
    #[error("Query failed: {0}")]
    QueryFailed(String),

    /// Index is empty
    #[error("Index is empty")]
    EmptyIndex,

    /// Dimension mismatch
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Expected dimension
        expected: usize,
        /// Actual dimension received
        got: usize,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_nodes() -> Vec<NodeInfo> {
        vec![
            NodeInfo {
                node_id: "func1".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func1".to_string(),
                language: "rust".to_string(),
                content: "fn func1() { println!(\"hello\"); }".to_string(),
                byte_range: (0, 40),
                embedding: Some(vec![1.0, 0.0, 0.0]),
                tfidf_embedding: vec![1.0, 0.0, 0.0],
                neural_embedding: None,
                complexity: 2,
                signature: None,
                pre_tokenized: None,
            },
            NodeInfo {
                node_id: "func2".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func2".to_string(),
                language: "rust".to_string(),
                content: "fn func2() { println!(\"world\"); }".to_string(),
                byte_range: (42, 82),
                embedding: Some(vec![0.0, 1.0, 0.0]),
                tfidf_embedding: vec![0.0, 1.0, 0.0],
                neural_embedding: None,
                complexity: 2,
                signature: None,
                pre_tokenized: None,
            },
        ]
    }

    #[test]
    fn test_search_engine_creation() {
        let engine = SearchEngine::new();
        assert_eq!(engine.node_count(), 0);
        assert!(engine.is_empty());
    }

    #[test]
    fn test_index_nodes() {
        let mut engine = SearchEngine::new();
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);
        assert_eq!(engine.node_count(), 2);
        assert!(!engine.is_empty());
    }

    #[test]
    fn test_search_empty_index() {
        let mut engine = SearchEngine::new();
        let query = SearchQuery {
            query: "test".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_semantic_search_empty_index() {
        let engine = SearchEngine::new();
        let results = engine.semantic_search(&[0.1, 0.2, 0.3], 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_search_with_results() {
        let mut engine = SearchEngine::new();
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        let query = SearchQuery {
            query: "func1".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "func1");
    }

    #[test]
    fn test_semantic_search() {
        let mut engine = SearchEngine::with_dimension(3);
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        // Search with query vector similar to func1
        let results = engine.semantic_search(&[1.0, 0.0, 0.0], 1).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "func1");
    }

    #[test]
    fn test_dimension_validation() {
        let engine = SearchEngine::with_dimension(128);
        assert_eq!(engine.vector_index().dimension(), 128);
    }

    #[test]
    fn test_dimension_mismatch_error() {
        let mut engine = SearchEngine::with_dimension(3);
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        // Try searching with wrong dimension
        let result = engine.semantic_search(&[0.1, 0.2], 10);
        assert!(result.is_err());
    }

    #[test]
    fn test_hnsw_enable() {
        let mut engine = SearchEngine::with_dimension(128);
        engine.enable_hnsw(None);
        assert!(engine.vector_index().is_hnsw_enabled());
    }

    #[test]
    fn test_top_k_limit() {
        let mut engine = SearchEngine::new();
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        let query = SearchQuery {
            query: "fn".to_string(),
            top_k: 1,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn test_relevance_threshold() {
        let mut engine = SearchEngine::new();
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        let query = SearchQuery {
            query: "nonexistent".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: Some(0.5),
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn test_node_id_to_idx_populated() {
        let mut engine = SearchEngine::new();
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        // Verify node_id_to_idx is populated with correct indices
        assert_eq!(engine.node_id_to_idx.len(), 2);
        assert_eq!(engine.node_id_to_idx.get("func1"), Some(&0));
        assert_eq!(engine.node_id_to_idx.get("func2"), Some(&1));
    }

    #[test]
    fn test_node_id_to_idx_o1_lookup_in_semantic_search() {
        let mut engine = SearchEngine::with_dimension(3);
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        // Verify semantic_search uses node_id_to_idx for O(1) lookup
        // by checking that results are still correct after optimization
        let results = engine.semantic_search(&[1.0, 0.0, 0.0], 10).unwrap();
        assert!(!results.is_empty());

        // The top result should be func1 (closest to query vector)
        assert_eq!(results[0].node_id, "func1");
        assert_eq!(results[0].entry_type, EntryType::Function);

        // Verify all results have correct entry type
        for entry in &results {
            assert_eq!(entry.entry_type, EntryType::Function);
        }
    }

    #[test]
    fn test_node_id_to_idx_cleared_on_reindex() {
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());
        assert_eq!(engine.node_id_to_idx.len(), 2);

        // Re-index with different nodes - should clear and repopulate
        engine.index_nodes(vec![NodeInfo {
            node_id: "new_func".to_string(),
            file_path: "new.rs".to_string(),
            symbol_name: "new_func".to_string(),
            language: "rust".to_string(),
            content: "fn new_func() {}".to_string(),
            byte_range: (0, 18),
            embedding: None,
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }]);
        assert_eq!(engine.node_id_to_idx.len(), 1);
        assert_eq!(engine.node_id_to_idx.get("new_func"), Some(&0));
        assert_eq!(engine.node_id_to_idx.get("func1"), None);
    }

    #[test]
    fn test_content_cleared_after_indexing() {
        // T13: Verify that NodeInfo.content is cleared after index_nodes()
        // to reduce memory footprint. The inverted index (text_index) preserves
        // all token information for search.
        let mut engine = SearchEngine::new();
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        // Content should be empty (cleared) for all nodes
        for node in &engine.nodes {
            assert!(
                node.content.is_empty(),
                "Node {} content should be cleared after indexing, but got: {:?}",
                node.node_id,
                node.content
            );
        }

        // But text search should still work via inverted index
        let query = SearchQuery {
            query: "func1".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(
            !results.is_empty(),
            "Search should still find results via inverted index after content cleared"
        );
        assert_eq!(results[0].node_id, "func1");

        // Also verify text_index is populated
        assert!(
            !engine.text_index.is_empty(),
            "text_index should be populated"
        );
        assert!(
            engine.text_index.contains_key("func1"),
            "text_index should contain 'func1' token"
        );
        assert!(
            engine.text_index.contains_key("func2"),
            "text_index should contain 'func2' token"
        );
    }

    #[test]
    fn test_node_tokens_populated() {
        // T14: Verify that node_tokens cache is populated during index_nodes()
        let mut engine = SearchEngine::new();
        let nodes = create_test_nodes();
        engine.index_nodes(nodes);

        // node_tokens should have an entry for each node
        assert_eq!(engine.node_tokens.len(), 2);
        assert!(engine.node_tokens.contains_key("func1"));
        assert!(engine.node_tokens.contains_key("func2"));

        // Verify tokens contain expected normalized content
        let func1_tokens = engine.node_tokens.get("func1").unwrap();
        assert!(
            func1_tokens.contains("func1"),
            "func1 tokens should contain 'func1', got: {:?}",
            func1_tokens
        );

        let func2_tokens = engine.node_tokens.get("func2").unwrap();
        assert!(
            func2_tokens.contains("func2"),
            "func2 tokens should contain 'func2', got: {:?}",
            func2_tokens
        );
    }

    #[test]
    fn test_node_tokens_cleared_on_reindex() {
        // T14: Verify node_tokens is cleared when re-indexing
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());
        assert_eq!(engine.node_tokens.len(), 2);

        // Re-index with different nodes
        engine.index_nodes(vec![NodeInfo {
            node_id: "new_func".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "new_func".to_string(),
            language: "rust".to_string(),
            content: "fn new_func() {}".to_string(),
            byte_range: (0, 18),
            embedding: None,
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }]);
        assert_eq!(engine.node_tokens.len(), 1);
        assert!(engine.node_tokens.contains_key("new_func"));
        assert!(!engine.node_tokens.contains_key("func1"));
    }

    #[test]
    fn test_node_tokens_used_in_scoring() {
        // T14: Verify that scoring uses cached tokens (no re-tokenization)
        // by checking that search results are correct after content is cleared.
        // This implicitly tests that calculate_text_score_optimized uses node_tokens.
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        // Content is cleared (T13), but tokens are cached (T14)
        for node in &engine.nodes {
            assert!(node.content.is_empty());
        }

        // Search for a term that appears in content — should still find it via cached tokens
        let query = SearchQuery {
            query: "println hello".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();

        // Should find results since "println" and "hello" appear in node content and tokens are cached
        assert!(
            !results.is_empty(),
            "Search should find results using cached node_tokens even after content is cleared"
        );
        // func1 contains both "println" and "hello", should be top result
        assert_eq!(results[0].node_id, "func1");
    }

    // ----------------------------------------------------------------
    // T28: Incremental reindex tests
    // ----------------------------------------------------------------

    #[test]
    fn test_incremental_reindex_add_nodes() {
        // T28: Adding nodes via incremental_reindex should update all indexes
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());
        assert_eq!(engine.node_count(), 2);

        let delta = TextIndexDelta {
            removed_node_ids: vec![],
            updated_nodes: vec![NodeInfo {
                node_id: "func3".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func3".to_string(),
                language: "rust".to_string(),
                content: "fn func3() { db_query(); }".to_string(),
                byte_range: (100, 130),
                embedding: Some(vec![0.0, 0.0, 1.0]),
                tfidf_embedding: vec![0.0, 0.0, 1.0],
                neural_embedding: None,
                complexity: 3,
                signature: None,
                pre_tokenized: None,
            }],
        };
        engine.incremental_reindex(delta);

        // Should now have 3 nodes
        assert_eq!(engine.node_count(), 3);
        assert_eq!(engine.node_id_to_idx.len(), 3);
        assert_eq!(engine.node_tokens.len(), 3);
        assert_eq!(engine.complexity_cache.len(), 3);

        // Search should find the new node
        let query = SearchQuery {
            query: "func3".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "func3");

        // text_index should contain "func3" token
        assert!(engine.text_index.contains_key("func3"));
        // "db" and "query" tokens should also be indexed
        assert!(engine.text_index.contains_key("query"));
    }

    #[test]
    fn test_incremental_reindex_remove_nodes() {
        // T28: Removing nodes via incremental_reindex should clean up all indexes
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());
        assert_eq!(engine.node_count(), 2);

        let delta = TextIndexDelta {
            removed_node_ids: vec!["func1".to_string()],
            updated_nodes: vec![],
        };
        engine.incremental_reindex(delta);

        // Should now have 1 node
        assert_eq!(engine.node_count(), 1);
        assert_eq!(engine.node_id_to_idx.len(), 1);
        assert!(!engine.node_id_to_idx.contains_key("func1"));
        assert!(engine.node_id_to_idx.contains_key("func2"));

        // func1's tokens should be removed from text_index
        // "func1" token should no longer map to func1
        if let Some(ids) = engine.text_index.get("func1") {
            assert!(
                !ids.contains("func1"),
                "func1 should be removed from text_index"
            );
        }

        // node_tokens should not contain func1
        assert!(!engine.node_tokens.contains_key("func1"));
        assert!(engine.node_tokens.contains_key("func2"));

        // Search for func1 should not find it
        let query = SearchQuery {
            query: "func1".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(
            results.is_empty(),
            "func1 should not be found after removal"
        );
    }

    #[test]
    fn test_incremental_reindex_update_existing_node() {
        // T28: Updating an existing node should replace it correctly
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        // Update func1 with new content
        let delta = TextIndexDelta {
            removed_node_ids: vec![],
            updated_nodes: vec![NodeInfo {
                node_id: "func1".to_string(),
                file_path: "updated.rs".to_string(),
                symbol_name: "func1_renamed".to_string(),
                language: "rust".to_string(),
                content: "fn func1_renamed() { new_logic(); }".to_string(),
                byte_range: (0, 35),
                embedding: Some(vec![0.5, 0.5, 0.0]),
                tfidf_embedding: vec![0.5, 0.5, 0.0],
                neural_embedding: None,
                complexity: 5,
                signature: None,
                pre_tokenized: None,
            }],
        };
        engine.incremental_reindex(delta);

        // Should still have 2 nodes
        assert_eq!(engine.node_count(), 2);

        // Complexity cache should reflect the update
        assert_eq!(engine.complexity_cache.get("func1"), Some(&5));

        // New tokens should be indexed
        assert!(engine.node_tokens.get("func1").unwrap().contains("logic"));
        assert!(engine.text_index.contains_key("logic"));

        // Search for new content should work
        let query = SearchQuery {
            query: "new_logic".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "func1");
    }

    #[test]
    fn test_incremental_reindex_combined_add_remove() {
        // T28: Combined add and remove in one delta
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        let delta = TextIndexDelta {
            removed_node_ids: vec!["func1".to_string()],
            updated_nodes: vec![
                NodeInfo {
                    node_id: "func3".to_string(),
                    file_path: "new.rs".to_string(),
                    symbol_name: "func3".to_string(),
                    language: "rust".to_string(),
                    content: "fn func3() {}".to_string(),
                    byte_range: (0, 14),
                    embedding: None,
                    tfidf_embedding: vec![],
                    neural_embedding: None,
                    complexity: 1,
                    signature: None,
                    pre_tokenized: None,
                },
                NodeInfo {
                    node_id: "func4".to_string(),
                    file_path: "new.rs".to_string(),
                    symbol_name: "func4".to_string(),
                    language: "rust".to_string(),
                    content: "fn func4() { helper(); }".to_string(),
                    byte_range: (15, 40),
                    embedding: None,
                    tfidf_embedding: vec![],
                    neural_embedding: None,
                    complexity: 2,
                    signature: None,
                    pre_tokenized: None,
                },
            ],
        };
        engine.incremental_reindex(delta);

        // Should have func2 (original) + func3 + func4 = 3 nodes
        assert_eq!(engine.node_count(), 3);
        assert_eq!(engine.node_id_to_idx.len(), 3);

        // func1 should be gone
        assert!(!engine.node_id_to_idx.contains_key("func1"));
        // func2, func3, func4 should exist
        assert!(engine.node_id_to_idx.contains_key("func2"));
        assert!(engine.node_id_to_idx.contains_key("func3"));
        assert!(engine.node_id_to_idx.contains_key("func4"));

        // Search for func2 should still work
        let query = SearchQuery {
            query: "func2".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "func2");
    }

    #[test]
    fn test_incremental_reindex_empty_delta() {
        // T28: Empty delta should not change anything
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        let delta = TextIndexDelta {
            removed_node_ids: vec![],
            updated_nodes: vec![],
        };
        engine.incremental_reindex(delta);

        assert_eq!(engine.node_count(), 2);
        assert_eq!(engine.node_id_to_idx.len(), 2);
    }

    #[test]
    fn test_incremental_reindex_removes_empty_token_sets() {
        // T28: When removing the last node for a token, the token entry should be removed
        let mut engine = SearchEngine::new();
        engine.index_nodes(vec![
            NodeInfo {
                node_id: "unique1".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "unique1".to_string(),
                language: "rust".to_string(),
                content: "fn unique1() { zebra(); }".to_string(),
                byte_range: (0, 25),
                embedding: None,
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            },
            NodeInfo {
                node_id: "unique2".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "unique2".to_string(),
                language: "rust".to_string(),
                content: "fn unique2() { apple(); }".to_string(),
                byte_range: (26, 52),
                embedding: None,
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            },
        ]);

        // "zebra" token should exist and map to unique1 only
        assert!(engine.text_index.contains_key("zebra"));

        // Remove unique1 — "zebra" token set should be cleaned up entirely
        let delta = TextIndexDelta {
            removed_node_ids: vec!["unique1".to_string()],
            updated_nodes: vec![],
        };
        engine.incremental_reindex(delta);

        // "zebra" token should no longer exist in text_index (no remaining nodes have it)
        assert!(
            !engine.text_index.contains_key("zebra"),
            "Token with no remaining nodes should be removed from text_index"
        );

        // "apple" should still exist
        assert!(engine.text_index.contains_key("apple"));
    }

    #[test]
    fn test_incremental_reindex_correctness_vs_full_rebuild() {
        // T28: Incremental reindex should produce identical results to a full rebuild
        let mut engine_inc = SearchEngine::new();
        let mut engine_full = SearchEngine::new();

        // Start with same initial nodes
        let initial = create_test_nodes();
        engine_inc.index_nodes(initial.clone());
        engine_full.index_nodes(initial);

        // Apply delta incrementally
        let delta = TextIndexDelta {
            removed_node_ids: vec!["func1".to_string()],
            updated_nodes: vec![NodeInfo {
                node_id: "func3".to_string(),
                file_path: "new.rs".to_string(),
                symbol_name: "func3".to_string(),
                language: "rust".to_string(),
                content: "fn func3() { compute(); }".to_string(),
                byte_range: (0, 25),
                embedding: Some(vec![1.0, 1.0, 0.0]),
                tfidf_embedding: vec![1.0, 1.0, 0.0],
                neural_embedding: None,
                complexity: 4,
                signature: None,
                pre_tokenized: None,
            }],
        };
        engine_inc.incremental_reindex(delta);

        // Apply same changes via full rebuild
        engine_full.index_nodes(vec![
            NodeInfo {
                node_id: "func2".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func2".to_string(),
                language: "rust".to_string(),
                content: "fn func2() { println!(\"world\"); }".to_string(),
                byte_range: (42, 82),
                embedding: Some(vec![0.0, 1.0, 0.0]),
                tfidf_embedding: vec![0.0, 1.0, 0.0],
                neural_embedding: None,
                complexity: 2,
                signature: None,
                pre_tokenized: None,
            },
            NodeInfo {
                node_id: "func3".to_string(),
                file_path: "new.rs".to_string(),
                symbol_name: "func3".to_string(),
                language: "rust".to_string(),
                content: "fn func3() { compute(); }".to_string(),
                byte_range: (0, 25),
                embedding: Some(vec![1.0, 1.0, 0.0]),
                tfidf_embedding: vec![1.0, 1.0, 0.0],
                neural_embedding: None,
                complexity: 4,
                signature: None,
                pre_tokenized: None,
            },
        ]);

        // Both engines should have same node count
        assert_eq!(engine_inc.node_count(), engine_full.node_count());

        // Both should have same node_ids
        let inc_ids: std::collections::BTreeSet<_> =
            engine_inc.nodes.iter().map(|n| n.node_id.clone()).collect();
        let full_ids: std::collections::BTreeSet<_> = engine_full
            .nodes
            .iter()
            .map(|n| n.node_id.clone())
            .collect();
        assert_eq!(inc_ids, full_ids);

        // Search should produce same results
        let query = SearchQuery {
            query: "func2".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let inc_results = engine_inc.search(query.clone()).unwrap();
        let full_results = engine_full.search(query).unwrap();
        assert_eq!(inc_results.len(), full_results.len());
        if !inc_results.is_empty() {
            assert_eq!(inc_results[0].node_id, full_results[0].node_id);
        }

        // Semantic search should also produce same results
        let inc_sem = engine_inc.semantic_search(&[1.0, 1.0, 0.0], 10).unwrap();
        let full_sem = engine_full.semantic_search(&[1.0, 1.0, 0.0], 10).unwrap();
        assert_eq!(inc_sem.len(), full_sem.len());
        if !inc_sem.is_empty() {
            assert_eq!(inc_sem[0].node_id, full_sem[0].node_id);
        }
    }

    #[test]
    fn test_incremental_reindex_semantic_search_after_update() {
        // T28: Semantic search should work correctly after incremental update
        let mut engine = SearchEngine::with_dimension(3);
        engine.index_nodes(create_test_nodes());

        // Add a new node with a distinct embedding
        let delta = TextIndexDelta {
            removed_node_ids: vec![],
            updated_nodes: vec![NodeInfo {
                node_id: "func3".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func3".to_string(),
                language: "rust".to_string(),
                content: "fn func3() {}".to_string(),
                byte_range: (0, 14),
                embedding: Some(vec![0.1, 0.1, 0.9]),
                tfidf_embedding: vec![0.1, 0.1, 0.9],
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            }],
        };
        engine.incremental_reindex(delta);

        // Search for vec close to func3's embedding
        let results = engine.semantic_search(&[0.1, 0.1, 0.9], 1).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "func3");
    }

    #[test]
    fn test_incremental_reindex_node_id_to_idx_consistency() {
        // T28: node_id_to_idx should be consistent after multiple incremental updates
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        // Add func3
        engine.incremental_reindex(TextIndexDelta {
            removed_node_ids: vec![],
            updated_nodes: vec![NodeInfo {
                node_id: "func3".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func3".to_string(),
                language: "rust".to_string(),
                content: "fn func3() {}".to_string(),
                byte_range: (0, 14),
                embedding: None,
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            }],
        });

        // Remove func1 (swap-remove may swap func3 into func1's slot)
        engine.incremental_reindex(TextIndexDelta {
            removed_node_ids: vec!["func1".to_string()],
            updated_nodes: vec![],
        });

        // Verify all indices are consistent
        assert_eq!(engine.node_id_to_idx.len(), engine.nodes.len());
        for (idx, node) in engine.nodes.iter().enumerate() {
            assert_eq!(
                engine.node_id_to_idx.get(&node.node_id),
                Some(&idx),
                "node_id_to_idx mismatch for node {}",
                node.node_id
            );
        }
    }

    #[test]
    fn test_incremental_reindex_removes_nonexistent_node() {
        // T28: Removing a node that doesn't exist should be a no-op
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        let delta = TextIndexDelta {
            removed_node_ids: vec!["nonexistent".to_string()],
            updated_nodes: vec![],
        };
        engine.incremental_reindex(delta);

        assert_eq!(engine.node_count(), 2);
        assert_eq!(engine.node_id_to_idx.len(), 2);
    }

    #[test]
    fn test_incremental_reindex_content_cleared() {
        // T28: Content should be cleared on newly added nodes (same as index_nodes)
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        engine.incremental_reindex(TextIndexDelta {
            removed_node_ids: vec![],
            updated_nodes: vec![NodeInfo {
                node_id: "func3".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func3".to_string(),
                language: "rust".to_string(),
                content: "fn func3() { important_content(); }".to_string(),
                byte_range: (0, 40),
                embedding: None,
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: 3,
                signature: None,
                pre_tokenized: None,
            }],
        });

        // Content should be cleared for all nodes
        for node in &engine.nodes {
            assert!(
                node.content.is_empty(),
                "Node {} content should be cleared, got: {:?}",
                node.node_id,
                node.content
            );
        }

        // But func3 tokens should still be searchable
        assert!(engine
            .node_tokens
            .get("func3")
            .unwrap()
            .contains("important"));
    }

    // ----------------------------------------------------------------
    // R8: Pre-tokenized search engine tests
    // ----------------------------------------------------------------

    #[test]
    fn test_pre_tokenized_produces_identical_search_results() {
        // R8: NodeInfo with pre_tokenized = Some(...) should produce identical
        // search results to the re-tokenization path.
        let content = "fn calculate_total(price: f64, tax: f64) -> f64 { price + tax }";

        // Compute search tokens the same way index_builder does
        let search_tokens: Vec<String> = content
            .split(|c: char| !c.is_alphanumeric())
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| s.len() >= 2)
            .collect();

        // Engine with pre-tokenized tokens
        let mut engine_pre = SearchEngine::new();
        engine_pre.index_nodes(vec![NodeInfo {
            node_id: "calc_total".to_string(),
            file_path: "math.rs".to_string(),
            symbol_name: "calculate_total".to_string(),
            language: "rust".to_string(),
            content: content.to_string(),
            byte_range: (0, content.len()),
            embedding: None,
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 3,
            signature: None,
            pre_tokenized: Some(search_tokens),
        }]);

        // Engine with re-tokenization (pre_tokenized = None)
        let mut engine_fallback = SearchEngine::new();
        engine_fallback.index_nodes(vec![NodeInfo {
            node_id: "calc_total".to_string(),
            file_path: "math.rs".to_string(),
            symbol_name: "calculate_total".to_string(),
            language: "rust".to_string(),
            content: content.to_string(),
            byte_range: (0, content.len()),
            embedding: None,
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 3,
            signature: None,
            pre_tokenized: None,
        }]);

        // Both inverted indexes should be identical
        assert_eq!(
            engine_pre.text_index, engine_fallback.text_index,
            "Pre-tokenized and fallback should produce identical text_index"
        );
        assert_eq!(
            engine_pre.node_tokens, engine_fallback.node_tokens,
            "Pre-tokenized and fallback should produce identical node_tokens"
        );

        // Search for "calculate" should find the node in both
        let query = SearchQuery {
            query: "calculate".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results_pre = engine_pre.search(query.clone()).unwrap();
        let results_fallback = engine_fallback.search(query).unwrap();
        assert_eq!(results_pre.len(), results_fallback.len());
        assert!(!results_pre.is_empty());
        assert_eq!(results_pre[0].node_id, results_fallback[0].node_id);
    }

    #[test]
    fn test_pre_tokenized_none_falls_back_to_content() {
        // R8: NodeInfo with pre_tokenized = None should use content-based
        // tokenization (backward compatibility).
        let mut engine = SearchEngine::new();
        engine.index_nodes(vec![NodeInfo {
            node_id: "backward_compat".to_string(),
            file_path: "compat.rs".to_string(),
            symbol_name: "legacy_func".to_string(),
            language: "rust".to_string(),
            content: "fn legacy_func() { return 42; }".to_string(),
            byte_range: (0, 30),
            embedding: None,
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 1,
            signature: None,
            pre_tokenized: None,
        }]);

        // Should still find via content-based tokenization
        assert!(engine.text_index.contains_key("legacy"));
        assert!(engine.text_index.contains_key("func"));
        assert!(engine.node_tokens.contains_key("backward_compat"));

        let query = SearchQuery {
            query: "legacy".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "backward_compat");
    }

    #[test]
    fn test_pre_tokenized_and_content_produce_same_inverted_index() {
        // R8: Both paths produce the same inverted index for the same content.
        let content = "pub async fn handle_http_request(req: Request) -> Response { ... }";

        let tokens: Vec<String> = content
            .split(|c: char| !c.is_alphanumeric())
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| s.len() >= 2)
            .collect();

        // Verify our manual tokenization produces expected tokens
        assert!(tokens.contains(&"handle".to_string()));
        assert!(tokens.contains(&"http".to_string()));
        assert!(tokens.contains(&"request".to_string()));
        assert!(tokens.contains(&"response".to_string()));

        // Engine A: pre-tokenized
        let mut engine_a = SearchEngine::new();
        engine_a.index_nodes(vec![NodeInfo {
            node_id: "handler".to_string(),
            file_path: "server.rs".to_string(),
            symbol_name: "handle_http_request".to_string(),
            language: "rust".to_string(),
            content: content.to_string(),
            byte_range: (0, content.len()),
            embedding: None,
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 5,
            signature: None,
            pre_tokenized: Some(tokens),
        }]);

        // Engine B: content-based
        let mut engine_b = SearchEngine::new();
        engine_b.index_nodes(vec![NodeInfo {
            node_id: "handler".to_string(),
            file_path: "server.rs".to_string(),
            symbol_name: "handle_http_request".to_string(),
            language: "rust".to_string(),
            content: content.to_string(),
            byte_range: (0, content.len()),
            embedding: None,
            tfidf_embedding: vec![],
            neural_embedding: None,
            complexity: 5,
            signature: None,
            pre_tokenized: None,
        }]);

        // Both should have identical text_index entries
        for token in &["handle", "http", "request", "response", "pub", "async"] {
            assert_eq!(
                engine_a.text_index.get(*token),
                engine_b.text_index.get(*token),
                "Mismatch for token '{}': pre_tokenized={:?}, content={:?}",
                token,
                engine_a.text_index.get(*token),
                engine_b.text_index.get(*token)
            );
        }
    }

    #[test]
    fn test_pre_tokenized_incremental_reindex() {
        // R8: Pre-tokenized tokens should work correctly with incremental reindex.
        let mut engine = SearchEngine::new();
        engine.index_nodes(create_test_nodes());

        let new_content = "fn compute_metrics(data: &[f64]) -> Metrics { ... }";
        let tokens: Vec<String> = new_content
            .split(|c: char| !c.is_alphanumeric())
            .map(|s| s.to_ascii_lowercase())
            .filter(|s| s.len() >= 2)
            .collect();

        let delta = TextIndexDelta {
            removed_node_ids: vec![],
            updated_nodes: vec![NodeInfo {
                node_id: "metrics".to_string(),
                file_path: "metrics.rs".to_string(),
                symbol_name: "compute_metrics".to_string(),
                language: "rust".to_string(),
                content: new_content.to_string(),
                byte_range: (0, new_content.len()),
                embedding: None,
                tfidf_embedding: vec![],
                neural_embedding: None,
                complexity: 4,
                signature: None,
                pre_tokenized: Some(tokens),
            }],
        };
        engine.incremental_reindex(delta);

        // Should have 3 nodes now
        assert_eq!(engine.node_count(), 3);

        // Pre-tokenized tokens should be in the inverted index
        assert!(engine.text_index.contains_key("compute"));
        assert!(engine.text_index.contains_key("metrics"));
        assert!(engine.text_index.contains_key("data"));

        // Search should find the new node
        let query = SearchQuery {
            query: "compute metrics".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "metrics");
    }

    // A+ VAL-APLUS-014: Search cache is hard-capped without semantic regression
    #[test]
    fn test_search_cache_hard_capped() {
        let mut engine = SearchEngine::new();

        // Index some nodes
        let nodes: Vec<NodeInfo> = (0..50)
            .map(|i| NodeInfo {
                node_id: format!("node_{}", i),
                file_path: format!("file_{}.rs", i),
                symbol_name: format!("symbol_{}", i),
                language: "rust".to_string(),
                content: format!("fn symbol_{}() {{}}", i),
                byte_range: (0, 16),
                tfidf_embedding: vec![0.0; 768],
                neural_embedding: None,
                embedding: None,
                complexity: 1,
                signature: None,
                pre_tokenized: None,
            })
            .collect();
        engine.index_nodes(nodes);

        // Run many searches to fill the cache
        for i in 0..300 {
            let query = SearchQuery {
                query: format!("query_{}", i),
                top_k: 10,
                token_budget: None,
                semantic: false,
                expand_context: false,
                query_embedding: None,
                threshold: None,
                query_type: None,
            };
            let _ = engine.search(query);
        }

        // Cache should not exceed entry limit
        assert!(
            engine.search_cache.len() <= SEARCH_CACHE_MAX_ENTRIES,
            "search cache entries ({}) should not exceed max ({})",
            engine.search_cache.len(),
            SEARCH_CACHE_MAX_ENTRIES
        );

        // Cache bytes should not exceed byte limit
        assert!(
            engine.search_cache_bytes <= SEARCH_CACHE_MAX_BYTES,
            "search cache bytes ({}) should not exceed max ({})",
            engine.search_cache_bytes,
            SEARCH_CACHE_MAX_BYTES
        );

        // Verify search still returns correct results (no semantic regression)
        let query = SearchQuery {
            query: "symbol_0".to_string(),
            top_k: 5,
            token_budget: None,
            semantic: false,
            expand_context: false,
            query_embedding: None,
            threshold: None,
            query_type: None,
        };
        let results = engine.search(query).unwrap();
        assert!(!results.is_empty(), "search should still return results");
    }
}
