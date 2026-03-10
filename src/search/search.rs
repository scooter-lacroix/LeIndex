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

    /// Node embedding (optional, for semantic search)
    pub embedding: Option<Vec<f32>>,

    /// Complexity score (0-100+, higher = more complex)
    pub complexity: u32,
}

/// Pre-computed query data for optimized text scoring
///
/// This struct holds data that is pre-computed once per search to avoid
/// repeated allocations in the hot path. When searching N nodes, this reduces
/// allocations from O(N) to O(1).
struct TextQueryPreprocessed {
    /// Original query string
    query: String,
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
            query: query.to_string(),
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
    /// Result cache for repeated queries
    search_cache: LruCache<String, Vec<SearchResult>>,
}

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
            search_cache: LruCache::new(NonZeroUsize::new(1000).unwrap()),
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
            search_cache: LruCache::new(NonZeroUsize::new(1000).unwrap()),
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
    pub fn index_nodes(&mut self, nodes: Vec<NodeInfo>) {
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

        // Store nodes for text search
        self.nodes = nodes.clone();

        // Build complexity cache for O(1) lookups
        for node in &nodes {
            self.complexity_cache
                .insert(node.node_id.clone(), node.complexity);
        }

        // Build inverted index for O(1) text lookups
        // This maps each token to the set of node IDs containing it
        for node in &nodes {
            // Tokenize content into individual words, splitting on word boundaries
            // This handles punctuation and special characters properly
            for token in node.content.split(|c: char| !c.is_alphanumeric()) {
                let normalized_token: String = token.to_ascii_lowercase();
                // Skip empty tokens and very short ones (< 2 chars) to reduce noise
                if normalized_token.len() >= 2 {
                    self.text_index
                        .entry(normalized_token)
                        .or_default()
                        .insert(node.node_id.clone());
                }
            }
        }

        // Build vector index from embeddings
        for node in nodes {
            if let Some(embedding) = node.embedding {
                if let Err(e) = self.vector_index.insert(node.node_id.clone(), embedding) {
                    tracing::warn!(
                        "Failed to insert embedding for node {}: {:?}",
                        node.node_id,
                        e
                    );
                }
            }
        }
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
                &node.content,
                &node.symbol_name,
                &node.file_path,
            );

            // Get semantic score from vector search results
            let semantic_score = if query.semantic {
                *vector_results.get(&node.node_id).unwrap_or(&0.0)
            } else {
                0.0
            };

            // For now, if no text match and not semantic, skip
            if text_score == 0.0 && !query.semantic && semantic_score == 0.0 {
                continue;
            }

            // Normalize complexity to 0-1 range (divide by 100, not 10)
            let structural_score = (node.complexity as f32 / 100.0).min(1.0);

            // Use custom weights based on query type if provided
            let score = if let Some(qt) = query.query_type {
                match qt {
                    crate::search::ranking::QueryType::Text => {
                        // Prose/Text mode: heavily favor keyword overlap
                        self.scorer.with_weights(0.2, 0.05, 0.75).score(
                            semantic_score,
                            structural_score,
                            text_score,
                        )
                    }
                    crate::search::ranking::QueryType::Semantic => {
                        // Semantic-heavy mode
                        self.scorer.with_weights(0.7, 0.1, 0.2).score(
                            semantic_score,
                            structural_score,
                            text_score,
                        )
                    }
                    crate::search::ranking::QueryType::Structural => {
                        // Structural-heavy mode
                        self.scorer.with_weights(0.3, 0.5, 0.2).score(
                            semantic_score,
                            structural_score,
                            text_score,
                        )
                    }
                }
            } else {
                self.scorer
                    .score(semantic_score, structural_score, text_score)
            };

            if score.overall > 0.0 {
                // Apply relevance threshold if specified
                if let Some(threshold) = query.threshold {
                    if score.overall < threshold {
                        continue;
                    }
                }

                // Extract signature: find the first non-empty, non-placeholder
                // line in node content (skipping the "// name in path" header).
                // Lines like "// [No source code available]" are discarded.
                let signature = node
                    .content
                    .lines()
                    .skip(1) // skip "// name in path" header
                    .map(|l| l.trim())
                    .find(|l| {
                        !l.is_empty() && !l.starts_with("// [No source") && !l.starts_with("// [")
                    })
                    .map(|l| l.to_string());

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

        // Cache results
        self.search_cache.put(cache_key, final_results.clone());

        Ok(final_results)
    }

    /// Optimized text score calculation using pre-computed query data
    ///
    /// This avoids repeated allocations in the hot path by using pre-computed
    /// query tokens and lowercase strings.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(m) where m is content length (query preprocessing is O(1) per call)
    /// - Space complexity: O(t) where t is number of content tokens (vs O(q + t) before)
    fn calculate_text_score_optimized(
        &self,
        precomputed: &TextQueryPreprocessed,
        content: &str,
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

        // Try exact match first (fast path)
        let base_score = if content.eq_ignore_ascii_case(&precomputed.query) {
            1.0
        } else if content
            .to_ascii_lowercase()
            .contains(&precomputed.query_lower)
        {
            0.8
        } else if precomputed.query_tokens.is_empty() {
            0.0
        } else {
            // Count matching tokens without allocating content_tokens HashSet
            let mut matching = 0;
            for token in content.split(|c: char| !c.is_alphanumeric()) {
                let token_lower = token.to_ascii_lowercase();
                if token_lower.len() >= 2 && precomputed.query_tokens.contains(&token_lower) {
                    matching += 1;
                }
            }

            if precomputed.query_tokens.is_empty() {
                0.0
            } else {
                matching as f32 / precomputed.query_tokens.len() as f32
            }
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

        // Convert to SemanticEntry format
        let entries = results
            .into_iter()
            .map(|(node_id, score)| {
                // Determine entry type based on node info
                let entry_type = self
                    .nodes
                    .iter()
                    .find(|n| n.node_id == node_id)
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
        let nodes_size = self.nodes.len() * (std::mem::size_of::<NodeInfo>() + 256); // Approximate content size
        let cache_size = self.complexity_cache.len()
            * (std::mem::size_of::<String>() + std::mem::size_of::<u32>());
        let text_index_size = self
            .text_index
            .values()
            .map(|set| set.len() * std::mem::size_of::<String>())
            .sum::<usize>();

        nodes_size + cache_size + text_index_size + self.vector_index.estimated_memory_bytes()
    }
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
                complexity: 2,
            },
            NodeInfo {
                node_id: "func2".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func2".to_string(),
                language: "rust".to_string(),
                content: "fn func2() { println!(\"world\"); }".to_string(),
                byte_range: (42, 82),
                embedding: Some(vec![0.0, 1.0, 0.0]),
                complexity: 2,
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
}
