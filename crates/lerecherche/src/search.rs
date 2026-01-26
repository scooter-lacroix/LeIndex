// Core search engine implementation

use crate::ranking::{Score, HybridScorer};
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
}

impl SearchEngine {
    /// Create a new search engine
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            scorer: HybridScorer::new(),
        }
    }

    /// Index nodes for searching
    pub fn index_nodes(&mut self, nodes: Vec<NodeInfo>) {
        self.nodes = nodes;
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
    pub async fn semantic_search(
        &self,
        _query: &str,
        _top_k: usize,
    ) -> Result<Vec<SemanticEntry>, Error> {
        // Placeholder - will use LEANN backend
        Ok(Vec::new())
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
}
