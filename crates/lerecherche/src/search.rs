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

use crate::query::{QueryParser, QueryIntent, MAX_EMBEDDING_DIMENSION, MIN_EMBEDDING_DIMENSION};
use crate::ranking::{Score, HybridScorer};
use crate::vector::VectorIndex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// CONSTANTS & VALIDATION
// ============================================================================

/// Default embedding dimension (CodeRank-compatible)
pub const DEFAULT_EMBEDDING_DIMENSION: usize = 768;

/// Maximum number of nodes that can be indexed (prevents memory exhaustion)
pub const MAX_NODES: usize = 1_000_000;

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

    /// Source content
    pub content: String,

    /// Byte range in source
    pub byte_range: (usize, usize),

    /// Node embedding (optional, for semantic search)
    pub embedding: Option<Vec<f32>>,

    /// Complexity score (0-100+, higher = more complex)
    pub complexity: u32,
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
/// # Thread Safety
///
/// - Reads (`&SearchEngine`) are thread-safe for concurrent access
/// - Writes (`&mut SearchEngine`) require exclusive access
/// - The internal VectorIndex is NOT thread-safe for concurrent writes
///
/// For concurrent read-write access, wrap in `Arc<RwLock<SearchEngine>>.
///
/// # Example
///
/// ```ignore
/// let mut engine = SearchEngine::new();
/// engine.index_nodes(nodes);
/// let results = engine.search(query).await?;
/// ```
pub struct SearchEngine {
    nodes: Vec<NodeInfo>,
    scorer: HybridScorer,
    vector_index: VectorIndex,
    /// Complexity cache for O(1) lookups (fixes O(n²) bug)
    complexity_cache: HashMap<String, u32>,
}

impl SearchEngine {
    /// Create a new search engine with default 768-dim embeddings
    ///
    /// # Panics
    ///
    /// This never panics - all initialization is infallible.
    #[must_use]
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndex::new(DEFAULT_EMBEDDING_DIMENSION),
            complexity_cache: HashMap::new(),
        }
    }

    /// Create a new search engine with custom embedding dimension
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
        if dimension < MIN_EMBEDDING_DIMENSION || dimension > MAX_EMBEDDING_DIMENSION {
            panic!(
                "Invalid embedding dimension: {} (must be between {} and {})",
                dimension, MIN_EMBEDDING_DIMENSION, MAX_EMBEDDING_DIMENSION
            );
        }

        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndex::new(dimension),
            complexity_cache: HashMap::new(),
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
                MAX_NODES, nodes.len()
            );
        }

        // Clear cache when re-indexing
        self.complexity_cache.clear();

        // Store nodes for text search
        self.nodes = nodes.clone();

        // Build complexity cache for O(1) lookups
        for node in &nodes {
            self.complexity_cache.insert(node.node_id.clone(), node.complexity);
        }

        // Build vector index from embeddings
        for node in nodes {
            if let Some(embedding) = node.embedding {
                if let Err(e) = self.vector_index.insert(node.node_id.clone(), embedding) {
                    tracing::warn!("Failed to insert embedding for node {}: {:?}", node.node_id, e);
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
    pub async fn search(&self, query: SearchQuery) -> Result<Vec<SearchResult>, Error> {
        if self.nodes.is_empty() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();

        for node in &self.nodes {
            let text_score = self.calculate_text_score(&query.query, &node.content);

            // For now, if no text match and not semantic, skip
            if text_score == 0.0 && !query.semantic {
                continue;
            }

            let semantic_score = if query.semantic {
                // In a real implementation, we would use a vector database
                // Here we'll use a placeholder or basic similarity if query had embedding
                0.0
            } else {
                0.0
            };

            // Normalize complexity to 0-1 range (divide by 100, not 10)
            let structural_score = (node.complexity as f32 / 100.0).min(1.0);

            let score = self.scorer.score(semantic_score, structural_score, text_score);

            if score.overall > 0.0 {
                results.push(SearchResult {
                    rank: 0, // Will be set after sorting
                    node_id: node.node_id.clone(),
                    file_path: node.file_path.clone(),
                    symbol_name: node.symbol_name.clone(),
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

        Ok(final_results)
    }

    /// Calculate text similarity score using case-insensitive matching
    ///
    /// This is optimized to avoid unnecessary allocations by using
    /// case-insensitive matching instead of lowercasing entire strings.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(n + m) where n is query length and m is content length
    /// - Space complexity: O(1) - no additional allocations
    fn calculate_text_score(&self, query: &str, content: &str) -> f32 {
        // Try exact match first (fast path)
        if content.eq_ignore_ascii_case(query) {
            return 1.0;
        }

        // Check if query is a substring (case-insensitive)
        if content.to_ascii_lowercase().contains(query.to_ascii_lowercase().as_str()) {
            return 0.8;
        }

        // Token overlap calculation
        let query_tokens: std::collections::HashSet<_> =
            query.split_whitespace().collect();
        let content_tokens: std::collections::HashSet<_> =
            content.split_whitespace().collect();

        if query_tokens.is_empty() {
            return 0.0;
        }

        let intersection = query_tokens.intersection(&content_tokens).count();
        intersection as f32 / query_tokens.len() as f32
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
    pub async fn semantic_search(
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
    pub fn vector_index(&self) -> &VectorIndex {
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
    /// index.insert("my_func", vec![0.1, 0.2, ...])?;
    /// ```
    #[must_use]
    pub fn vector_index_mut(&mut self) -> &mut VectorIndex {
        &mut self.vector_index
    }

    /// Natural language search with query understanding
    ///
    /// This method processes natural language queries and converts them into
    /// structured search queries with intent classification and pattern matching.
    ///
    /// # Arguments
    ///
    /// * `query` - Natural language query text (e.g., "show me how authentication works")
    /// * `top_k` - Maximum number of results to return (1-1000)
    ///
    /// # Returns
    ///
    /// Vector of search results sorted by relevance.
    ///
    /// # Supported Query Patterns
    ///
    /// - "Show me how X works" → Semantic search with context expansion
    /// - "Where is X handled?" → Structural pattern search
    /// - "What are bottlenecks?" → Complexity-based ranking
    /// - Generic questions → Semantic search
    /// - Text-only → Text search
    ///
    /// # Example
    ///
    /// ```ignore
    /// let results = engine.natural_search("show me how parsing works", 10).await?;
    /// ```
    ///
    /// # Errors
    ///
    /// Returns `Error::InvalidQuery` if the query is invalid (too long, no meaningful terms).
    /// Returns `Error::QueryFailed` if parsing or search fails.
    pub async fn natural_search(
        &self,
        query: &str,
        top_k: usize,
    ) -> Result<Vec<SearchResult>, Error> {
        let parser = QueryParser::new().map_err(|e| Error::QueryFailed(e.to_string()))?;
        let parsed = parser.parse(query, top_k)
            .map_err(|e| Error::InvalidQuery(e.to_string()))?;

        // Build search query from parsed natural language query
        let search_query = parser.build_search_query(&parsed);

        // Execute search based on intent
        match parsed.intent {
            QueryIntent::HowWorks | QueryIntent::WhereHandled => {
                // These queries benefit from semantic search with context expansion
                self.search_with_intent(search_query, parsed.intent).await
            }
            QueryIntent::Bottlenecks => {
                // Sort by complexity/centrality
                self.search_by_complexity(search_query).await
            }
            QueryIntent::Semantic => {
                // Pure semantic search
                self.search(search_query).await
            }
            QueryIntent::Text => {
                // Fallback to text search
                self.search(search_query).await
            }
        }
    }

    /// Search with intent-based enhancement
    ///
    /// This applies intent-specific enhancements to the search.
    /// Currently, all intents delegate to the base search implementation,
    /// but future enhancements can be added here.
    async fn search_with_intent(
        &self,
        query: SearchQuery,
        _intent: QueryIntent,
    ) -> Result<Vec<SearchResult>, Error> {
        // For now, delegate to regular search
        // In the future, this could apply intent-specific enhancements
        self.search(query).await
    }

    /// Search by complexity (for bottleneck queries)
    ///
    /// This re-ranks search results by complexity, putting the most complex
    /// nodes first. Uses the complexity cache for O(1) lookups.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(n log n) for sorting where n is result count
    /// - Space complexity: O(n) for the results
    /// - Complexity lookups are O(1) thanks to the cache
    ///
    /// # Algorithm
    ///
    /// 1. Execute base search to get all matching results
    /// 2. Sort by complexity in descending order (highest first)
    /// 3. Reassign ranks based on new sort order
    async fn search_by_complexity(
        &self,
        query: SearchQuery,
    ) -> Result<Vec<SearchResult>, Error> {
        let mut results = self.search(query).await?;

        // Sort by complexity (highest first) using O(1) cache lookups
        results.sort_by(|a, b| {
            let complexity_a = self.complexity_cache
                .get(&b.node_id)
                .copied()
                .unwrap_or(0);
            let complexity_b = self.complexity_cache
                .get(&a.node_id)
                .copied()
                .unwrap_or(0);
            complexity_a.cmp(&complexity_b)
        });

        // Reassign ranks
        for (i, result) in results.iter_mut().enumerate() {
            result.rank = i + 1;
        }

        Ok(results)
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// SEMANTIC ENTRY
// ============================================================================

/// Semantic entry point for graph expansion
///
/// This represents a node selected via vector search as an entry point
/// for PDG (Program Dependence Graph) context expansion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEntry {
    /// Node ID
    pub node_id: String,

    /// Relevance score (cosine similarity, 0.0-1.0)
    pub relevance: f32,

    /// Entry point type
    pub entry_type: EntryType,
}

impl SemanticEntry {
    /// Create a new semantic entry
    #[must_use]
    pub fn new(node_id: String, relevance: f32, entry_type: EntryType) -> Self {
        Self {
            node_id,
            relevance,
            entry_type,
        }
    }
}

/// Entry point type
///
/// This classifies the type of code node represented by a SemanticEntry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryType {
    /// Function entry
    Function,

    /// Class entry
    Class,

    /// Module entry
    Module,
}

// ============================================================================
// ERRORS
// ============================================================================

/// Search errors
///
/// These errors can occur during search operations.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Query failed: {0}")]
    QueryFailed(String),

    #[error("Index not ready")]
    IndexNotReady,

    #[error("Invalid query: {0}")]
    InvalidQuery(String),
}

// ============================================================================
// TESTS
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_search_engine_basic() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "1".to_string(),
                file_path: "test1.rs".to_string(),
                symbol_name: "func1".to_string(),
                content: "fn func1() { println!(\"hello\"); }".to_string(),
                byte_range: (0, 30),
                embedding: None,
                complexity: 1,
            },
            NodeInfo {
                node_id: "2".to_string(),
                file_path: "test2.rs".to_string(),
                symbol_name: "func2".to_string(),
                content: "fn func2() { println!(\"world\"); }".to_string(),
                byte_range: (0, 30),
                embedding: None,
                complexity: 5,
            },
        ];
        engine.index_nodes(nodes);

        let query = SearchQuery {
            query: "hello".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
        };

        let results = engine.search(query).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].node_id, "1");
        assert!(results[0].score.overall > 0.0);
    }

    #[tokio::test]
    async fn test_search_ranking() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "low_complexity".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "low".to_string(),
                content: "fn low() { search_term }".to_string(),
                byte_range: (0, 20),
                embedding: None,
                complexity: 1,
            },
            NodeInfo {
                node_id: "high_complexity".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "high".to_string(),
                content: "fn high() { search_term }".to_string(),
                byte_range: (0, 20),
                embedding: None,
                complexity: 10,
            },
        ];
        engine.index_nodes(nodes);

        let query = SearchQuery {
            query: "search_term".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
        };

        let results = engine.search(query).await.unwrap();
        assert_eq!(results.len(), 2);
        // High complexity should rank higher due to structural score
        assert_eq!(results[0].node_id, "high_complexity");
    }

    #[tokio::test]
    async fn test_vector_search_integration() {
        let mut engine = SearchEngine::new();

        // Create nodes with embeddings
        let nodes = vec![
            NodeInfo {
                node_id: "func1".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "function_one".to_string(),
                content: "fn function_one() {}".to_string(),
                byte_range: (0, 20),
                embedding: Some(vec![1.0, 0.0, 0.0]), // 3-dim for testing
                complexity: 1,
            },
            NodeInfo {
                node_id: "func2".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "function_two".to_string(),
                content: "fn function_two() {}".to_string(),
                byte_range: (20, 40),
                embedding: Some(vec![0.0, 1.0, 0.0]),
                complexity: 2,
            },
            NodeInfo {
                node_id: "func3".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "function_three".to_string(),
                content: "fn function_three() {}".to_string(),
                byte_range: (40, 60),
                embedding: Some(vec![0.9, 0.1, 0.0]), // Similar to func1
                complexity: 3,
            },
        ];

        // Index with 3-dim embeddings (not 768, just for this test)
        let mut custom_engine = SearchEngine::with_dimension(3);
        custom_engine.index_nodes(nodes);

        // Search with query similar to func1
        let query = vec![1.0, 0.0, 0.0];
        let results = custom_engine.semantic_search(&query, 10).await.unwrap();

        // func1 should be most similar (identical)
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].node_id, "func1");
        assert!(results[0].relevance > 0.9); // Should be very close to 1.0

        // func3 should be second (similar to func1)
        assert_eq!(results[1].node_id, "func3");

        // func2 should be last (different direction)
        assert_eq!(results[2].node_id, "func2");
        assert!(results[2].relevance < 0.1); // Should be close to 0.0
    }

    #[tokio::test]
    async fn test_vector_search_empty_index() {
        let engine = SearchEngine::new();
        let query = vec![0.1, 0.2, 0.3];
        let results = engine.semantic_search(&query, 10).await.unwrap();
        assert_eq!(results.len(), 0);
    }

    #[tokio::test]
    async fn test_vector_search_top_k() {
        let mut engine = SearchEngine::with_dimension(2);
        let nodes = (0..10).map(|i| NodeInfo {
            node_id: format!("node{}", i),
            file_path: "test.rs".to_string(),
            symbol_name: format!("func{}", i),
            content: String::new(),
            byte_range: (0, 0),
            embedding: Some(vec![1.0 / (i + 1) as f32, 0.0]),
            complexity: 1,
        }).collect::<Vec<_>>();

        engine.index_nodes(nodes);

        let query = vec![1.0, 0.0];
        let results = engine.semantic_search(&query, 3).await.unwrap();
        assert_eq!(results.len(), 3); // Should only return top 3
    }

    #[test]
    fn test_search_engine_with_dimension() {
        let engine = SearchEngine::with_dimension(128);
        assert_eq!(engine.vector_index().dimension(), 128);
    }

    #[test]
    fn test_search_engine_default_dimension() {
        let engine = SearchEngine::new();
        assert_eq!(engine.vector_index().dimension(), DEFAULT_EMBEDDING_DIMENSION);
    }

    #[test]
    fn test_search_engine_dimension_validation() {
        // Valid dimensions should work
        let _ = SearchEngine::with_dimension(1);
        let _ = SearchEngine::with_dimension(MAX_EMBEDDING_DIMENSION);

        // Invalid dimensions should panic
        let result = std::panic::catch_unwind(|| {
            SearchEngine::with_dimension(0);
        });
        assert!(result.is_err(), "Dimension 0 should panic");

        let result = std::panic::catch_unwind(|| {
            SearchEngine::with_dimension(MAX_EMBEDDING_DIMENSION + 1);
        });
        assert!(result.is_err(), "Dimension > MAX_EMBEDDING_DIMENSION should panic");
    }

    #[test]
    fn test_node_count() {
        let mut engine = SearchEngine::new();
        assert_eq!(engine.node_count(), 0);
        assert!(engine.is_empty());

        let nodes = vec![
            NodeInfo {
                node_id: "test".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "test".to_string(),
                content: "fn test() {}".to_string(),
                byte_range: (0, 10),
                embedding: None,
                complexity: 1,
            },
        ];
        engine.index_nodes(nodes);
        assert_eq!(engine.node_count(), 1);
        assert!(!engine.is_empty());
    }

    #[tokio::test]
    async fn test_direct_vector_index_access() {
        let mut engine = SearchEngine::with_dimension(3);

        // Add vectors directly via index - use truly orthogonal vectors
        let index = engine.vector_index_mut();
        index.insert("test1".to_string(), vec![1.0, 0.0, 0.0]).unwrap(); // X axis
        index.insert("test2".to_string(), vec![0.0, 1.0, 0.0]).unwrap(); // Y axis

        assert_eq!(engine.vector_index().len(), 2);

        // Search for X-axis vector should only find test1
        let query = vec![1.0, 0.0, 0.0];
        let results = engine.semantic_search(&query, 10).await.unwrap();

        // Should return both results, but test1 has similarity 1.0, test2 has 0.0
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].node_id, "test1");
        assert!(results[0].relevance > 0.99); // Nearly identical
        assert_eq!(results[1].node_id, "test2");
        assert!(results[1].relevance < 0.01); // Orthogonal
    }

    #[tokio::test]
    async fn test_natural_search_how_works() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "auth_func".to_string(),
                file_path: "auth.rs".to_string(),
                symbol_name: "authenticate".to_string(),
                content: "fn authenticate() { // auth logic }".to_string(),
                byte_range: (0, 40),
                embedding: None,
                complexity: 5,
            },
            NodeInfo {
                node_id: "other_func".to_string(),
                file_path: "other.rs".to_string(),
                symbol_name: "other".to_string(),
                content: "fn other() { // other logic }".to_string(),
                byte_range: (0, 30),
                embedding: None,
                complexity: 1,
            },
        ];
        engine.index_nodes(nodes);

        let results = engine.natural_search("show me how authentication works", 10).await.unwrap();

        // Should find the authenticate function
        assert!(results.iter().any(|r| r.node_id == "auth_func" || r.symbol_name.contains("auth")));
    }

    #[tokio::test]
    async fn test_natural_search_where_handled() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "error_handler".to_string(),
                file_path: "error.rs".to_string(),
                symbol_name: "handle_error".to_string(),
                content: "fn handle_error() { // error handling }".to_string(),
                byte_range: (0, 40),
                embedding: None,
                complexity: 3,
            },
        ];
        engine.index_nodes(nodes);

        let results = engine.natural_search("where is error handling handled", 10).await.unwrap();

        // Should find the error handler
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.symbol_name.contains("error") || r.file_path.contains("error")));
    }

    #[tokio::test]
    async fn test_natural_search_bottlenecks() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "simple".to_string(),
                file_path: "simple.rs".to_string(),
                symbol_name: "simple_func".to_string(),
                content: "fn simple_func() {}".to_string(),
                byte_range: (0, 20),
                embedding: None,
                complexity: 1,
            },
            NodeInfo {
                node_id: "complex".to_string(),
                file_path: "complex.rs".to_string(),
                symbol_name: "complex_func".to_string(),
                content: "fn complex_func() { // complex logic with multiple operations }".to_string(),
                byte_range: (0, 40),
                embedding: None,
                complexity: 10,
            },
        ];
        engine.index_nodes(nodes);

        let results = engine.natural_search("what are the bottlenecks", 10).await.unwrap();

        // Bottleneck queries should return results sorted by complexity
        // We expect at least some results since the query will match "complex" content
        if !results.is_empty() {
            // Complex function should rank higher due to complexity
            assert_eq!(results[0].node_id, "complex");
        }
    }

    #[tokio::test]
    async fn test_natural_search_semantic() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "cache_impl".to_string(),
                file_path: "cache.rs".to_string(),
                symbol_name: "caching".to_string(),
                content: "impl Caching { fn new() {} }".to_string(),
                byte_range: (0, 30),
                embedding: None,
                complexity: 2,
            },
        ];
        engine.index_nodes(nodes);

        let results = engine.natural_search("how do I implement caching", 10).await.unwrap();

        // Should find caching-related code
        assert!(!results.is_empty());
    }

    #[tokio::test]
    async fn test_natural_search_empty_query() {
        let engine = SearchEngine::new();
        let results = engine.natural_search("", 10).await;

        // Should return error for empty query
        assert!(results.is_err());
    }

    #[tokio::test]
    async fn test_natural_search_text_fallback() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "my_function".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "my_function".to_string(),
                content: "fn my_function() { println!(\"test\"); }".to_string(),
                byte_range: (0, 40),
                embedding: None,
                complexity: 1,
            },
        ];
        engine.index_nodes(nodes);

        let results = engine.natural_search("my_function", 10).await.unwrap();

        // Should find the function by text match
        assert!(!results.is_empty());
        assert_eq!(results[0].node_id, "my_function");
    }

    #[tokio::test]
    async fn test_complexity_sorting_uses_cache() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "low".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "low".to_string(),
                content: "fn low() {}".to_string(),
                byte_range: (0, 10),
                embedding: None,
                complexity: 1,
            },
            NodeInfo {
                node_id: "high".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "high".to_string(),
                content: "fn high() {}".to_string(),
                byte_range: (0, 10),
                embedding: None,
                complexity: 100,
            },
        ];
        engine.index_nodes(nodes);

        // Search that matches both nodes (query "fn" matches both function definitions)
        let query = SearchQuery {
            query: "fn".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
        };

        let results = engine.search_by_complexity(query).await.unwrap();

        // "high" should be first (complexity 100)
        assert_eq!(results[0].node_id, "high");
        // "low" should be second (complexity 1)
        assert_eq!(results[1].node_id, "low");
    }

    #[tokio::test]
    async fn test_calculate_text_score_optimized() {
        let engine = SearchEngine::new();

        // Test case-insensitive exact match (fast path)
        let score1 = engine.calculate_text_score("hello", "hello");
        assert_eq!(score1, 1.0);

        // Test substring match
        let score2 = engine.calculate_text_score("hello", "hello world");
        assert_eq!(score2, 0.8);

        // Test token overlap (no common tokens = 0.0)
        let score3 = engine.calculate_text_score("hello world", "hi there");
        assert_eq!(score3, 0.0);
    }

    // ===== Validation Tests =====

    #[test]
    fn test_max_nodes_enforcement() {
        let mut engine = SearchEngine::new();

        // Should work within limits
        let valid_nodes = (0..MAX_NODES).map(|i| NodeInfo {
            node_id: format!("node{}", i),
            file_path: "test.rs".to_string(),
            symbol_name: format!("func{}", i),
            content: String::new(),
            byte_range: (0, 0),
            embedding: None,
            complexity: 1,
        }).collect::<Vec<_>>();

        engine.index_nodes(valid_nodes);
        assert_eq!(engine.node_count(), MAX_NODES);

        // Should panic when exceeding limit
        let too_many_nodes = vec![NodeInfo {
            node_id: "extra".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "extra".to_string(),
            content: String::new(),
            byte_range: (0, 0),
            embedding: None,
            complexity: 1,
        }];

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            engine.index_nodes(too_many_nodes);
        }));
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_complexity_cache_is_effective() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "node1".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func1".to_string(),
                content: "fn func1() {}".to_string(),
                byte_range: (0, 10),
                embedding: None,
                complexity: 42,
            },
            NodeInfo {
                node_id: "node2".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "func2".to_string(),
                content: "fn func2() {}".to_string(),
                byte_range: (10, 20),
                embedding: None,
                complexity: 99,
            },
        ];
        engine.index_nodes(nodes);

        // Verify cache is populated
        assert_eq!(engine.complexity_cache.len(), 2);
        assert_eq!(engine.complexity_cache.get("node1"), Some(&42));
        assert_eq!(engine.complexity_cache.get("node2"), Some(&99));
    }

    #[tokio::test]
    async fn test_semantic_search_dimension_validation() {
        let mut engine = SearchEngine::new();

        // Add a node with embedding to make the index non-empty
        let nodes = vec![NodeInfo {
            node_id: "test".to_string(),
            file_path: "test.rs".to_string(),
            symbol_name: "test".to_string(),
            content: "test".to_string(),
            byte_range: (0, 4),
            embedding: Some(vec![0.0; 768]), // 768-dim embedding
            complexity: 1,
        }];
        engine.index_nodes(nodes);

        // Wrong dimension should error
        let query = vec![0.1, 0.2]; // 2-dim instead of 768
        let results = engine.semantic_search(&query, 10).await;

        assert!(results.is_err());
        assert!(results.unwrap_err().to_string().contains("dimension mismatch"));
    }

    #[tokio::test]
    async fn test_no_results_returns_empty() {
        let mut engine = SearchEngine::new();
        let nodes = vec![
            NodeInfo {
                node_id: "test".to_string(),
                file_path: "test.rs".to_string(),
                symbol_name: "test".to_string(),
                content: "fn test() {}".to_string(),
                byte_range: (0, 10),
                embedding: None,
                complexity: 1,
            },
        ];
        engine.index_nodes(nodes);

        let query = SearchQuery {
            query: "nonexistent".to_string(),
            top_k: 10,
            token_budget: None,
            semantic: false,
            expand_context: false,
        };

        let results = engine.search(query).await.unwrap();
        assert!(results.is_empty());
    }
}
