// Node embedding generation and storage

use serde::{Deserialize, Serialize};

/// Node embedding (768-dimensional vector)
///
/// Uses CodeRankEmbed model for semantic code representations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NodeEmbedding {
    /// 768-dimensional embedding vector
    pub vector: Vec<f32>,

    /// Node ID this embedding represents
    pub node_id: String,

    /// Embedding version/model used
    pub model: String,
}

impl NodeEmbedding {
    /// Create a new embedding from vector
    pub fn new(vector: Vec<f32>, node_id: String) -> Self {
        Self {
            vector,
            node_id,
            model: "CodeRankEmbed".to_string(),
        }
    }

    /// Calculate cosine similarity with another embedding
    pub fn similarity(&self, other: &NodeEmbedding) -> f32 {
        if self.vector.len() != other.vector.len() {
            return 0.0;
        }

        let dot_product: f32 = self.vector.iter()
            .zip(other.vector.iter())
            .map(|(a, b)| a * b)
            .sum();

        let norm_a: f32 = self.vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_b: f32 = other.vector.iter().map(|x| x * x).sum::<f32>().sqrt();

        if norm_a == 0.0 || norm_b == 0.0 {
            0.0
        } else {
            dot_product / (norm_a * norm_b)
        }
    }

    /// Get embedding dimension
    pub fn dimension(&self) -> usize {
        self.vector.len()
    }
}

/// Embedding cache for efficient lookup
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddingCache {
    /// Cached embeddings
    embeddings: Vec<NodeEmbedding>,

    /// Maximum cache size
    max_size: usize,
}

impl EmbeddingCache {
    /// Create a new cache
    pub fn new(max_size: usize) -> Self {
        Self {
            embeddings: Vec::with_capacity(max_size),
            max_size,
        }
    }

    /// Insert an embedding
    pub fn insert(&mut self, embedding: NodeEmbedding) {
        if self.embeddings.len() >= self.max_size {
            // Simple FIFO eviction - would use LRU in production
            self.embeddings.remove(0);
        }
        self.embeddings.push(embedding);
    }

    /// Get embedding by node ID
    pub fn get(&self, node_id: &str) -> Option<&NodeEmbedding> {
        self.embeddings.iter().find(|e| e.node_id == node_id)
    }

    /// Find similar embeddings
    pub fn find_similar(&self, embedding: &NodeEmbedding, top_k: usize) -> Vec<(String, f32)> {
        let mut similarities: Vec<_> = self.embeddings
            .iter()
            .map(|e| (e.node_id.clone(), embedding.similarity(e)))
            .filter(|(_, s)| *s > 0.0)
            .collect();

        similarities.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        similarities.into_iter().take(top_k).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedding_creation() {
        let vector = vec![0.1f32; 768];
        let embedding = NodeEmbedding::new(vector, "test_node".to_string());
        assert_eq!(embedding.dimension(), 768);
    }

    #[test]
    fn test_cosine_similarity() {
        let vec1 = vec![1.0f32, 0.0, 0.0];
        let vec2 = vec![1.0f32, 0.0, 0.0];
        let vec3 = vec![0.0f32, 1.0, 0.0];

        let emb1 = NodeEmbedding::new(vec1, "node1".to_string());
        let emb2 = NodeEmbedding::new(vec2, "node2".to_string());
        let emb3 = NodeEmbedding::new(vec3, "node3".to_string());

        // Same vectors should have similarity 1.0
        assert!((emb1.similarity(&emb2) - 1.0).abs() < 0.001);

        // Orthogonal vectors should have similarity 0.0
        assert!((emb1.similarity(&emb3) - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_cache_operations() {
        let mut cache = EmbeddingCache::new(10);

        let embedding = NodeEmbedding::new(vec![0.1f32; 768], "test".to_string());
        cache.insert(embedding);

        assert!(cache.get("test").is_some());
        assert!(cache.get("nonexistent").is_none());
    }
}
