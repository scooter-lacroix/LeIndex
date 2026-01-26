// Core search engine implementation

use crate::ranking::{Score, HybridScorer};
use crate::vector::VectorIndex;
use serde::{Deserialize, Serialize};

/// Node information for indexing
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

    /// Node embedding
    pub embedding: Option<Vec<f32>>,

    /// Complexity score
    pub complexity: u32,
}

/// Search query
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchQuery {
    /// Query text
    pub query: String,

    /// Maximum results to return
    pub top_k: usize,

    /// Token budget for context expansion
    pub token_budget: Option<usize>,

    /// Whether to use semantic search
    pub semantic: bool,

    /// Whether to expand context using graph traversal
    pub expand_context: bool,
}

/// Search result
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Result rank
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

/// Search engine combining vector and graph search
pub struct SearchEngine {
    nodes: Vec<NodeInfo>,
    scorer: HybridScorer,
    vector_index: VectorIndex,
}

impl SearchEngine {
    /// Create a new search engine
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndex::new(768), // Default 768-dim embeddings
        }
    }

    /// Create a new search engine with custom embedding dimension
    pub fn with_dimension(dimension: usize) -> Self {
        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
            vector_index: VectorIndex::new(dimension),
        }
    }

    /// Index nodes for searching
    pub fn index_nodes(&mut self, nodes: Vec<NodeInfo>) {
        // Store nodes for text search
        self.nodes = nodes.clone();

        // Build vector index from embeddings
        for node in nodes {
            if let Some(embedding) = node.embedding {
                let _ = self.vector_index.insert(node.node_id.clone(), embedding);
            }
        }
    }

    /// Execute a search query
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

            let structural_score = (node.complexity as f32 / 10.0).min(1.0);

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

        // Sort by score
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

    fn calculate_text_score(&self, query: &str, content: &str) -> f32 {
        let query_lower = query.to_lowercase();
        let content_lower = content.to_lowercase();

        if content_lower.contains(&query_lower) {
            return 1.0;
        }

        // Basic token overlap
        let query_tokens: std::collections::HashSet<_> = query_lower.split_whitespace().collect();
        let content_tokens: std::collections::HashSet<_> = content_lower.split_whitespace().collect();

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
    pub async fn semantic_search(
        &self,
        query_embedding: &[f32],
        top_k: usize,
    ) -> Result<Vec<SemanticEntry>, Error> {
        if self.vector_index.is_empty() {
            return Ok(Vec::new());
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
    pub fn vector_index(&self) -> &VectorIndex {
        &self.vector_index
    }

    /// Get mutable access to the vector index
    ///
    /// This allows direct manipulation of the vector index.
    pub fn vector_index_mut(&mut self) -> &mut VectorIndex {
        &mut self.vector_index
    }
}

impl Default for SearchEngine {
    fn default() -> Self {
        Self::new()
    }
}

/// Semantic entry point for graph expansion
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SemanticEntry {
    /// Node ID
    pub node_id: String,

    /// Relevance score
    pub relevance: f32,

    /// Entry point type
    pub entry_type: EntryType,
}

/// Make SemanticEntry accessible from semantic module
impl SemanticEntry {
    /// Create a new semantic entry
    pub fn new(node_id: String, relevance: f32, entry_type: EntryType) -> Self {
        Self {
            node_id,
            relevance,
            entry_type,
        }
    }
}

/// Entry point type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EntryType {
    /// Function entry
    Function,

    /// Class entry
    Class,

    /// Module entry
    Module,
}

/// Search errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Query failed: {0}")]
    QueryFailed(String),

    #[error("Index not ready")]
    IndexNotReady,

    #[error("Invalid query: {0}")]
    InvalidQuery(String),
}

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
        assert_eq!(engine.vector_index().dimension(), 768);
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
}
