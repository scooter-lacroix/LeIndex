// Vector Search Implementation
//
// *Le Vector* (The Vector) - Semantic search with cosine similarity

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Vector index for semantic search
///
/// This index stores node embeddings and provides fast similarity search
/// using cosine similarity. For small to medium datasets, brute-force
/// search is sufficient. For larger datasets, this can be extended with HNSW.
#[derive(Debug, Clone)]
pub struct VectorIndex {
    /// Node ID to embedding mapping
    embeddings: HashMap<String, Vec<f32>>,

    /// Embedding dimension
    dimension: usize,

    /// Number of vectors in the index
    count: usize,
}

impl VectorIndex {
    /// Create a new vector index
    ///
    /// # Arguments
    ///
    /// * `dimension` - The dimension of the embedding vectors (e.g., 768 for CodeRank)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let index = VectorIndex::new(768);
    /// index.insert("func1", vec![0.1, 0.2, ...]);
    /// ```
    pub fn new(dimension: usize) -> Self {
        Self {
            embeddings: HashMap::new(),
            dimension,
            count: 0,
        }
    }

    /// Insert a vector into the index
    ///
    /// # Arguments
    ///
    /// * `node_id` - Unique identifier for the node
    /// * `embedding` - Embedding vector (must match dimension)
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, `Err(Error)` if dimension mismatch
    ///
    /// # Example
    ///
    /// ```ignore
    /// index.insert("my_func", vec![0.1, 0.2, 0.3, ...])?;
    /// ```
    pub fn insert(&mut self, node_id: String, embedding: Vec<f32>) -> Result<(), Error> {
        if embedding.len() != self.dimension {
            return Err(Error::DimensionMismatch {
                expected: self.dimension,
                got: embedding.len(),
            });
        }

        self.embeddings.insert(node_id, embedding);
        self.count += 1;
        Ok(())
    }

    /// Batch insert vectors into the index
    ///
    /// # Arguments
    ///
    /// * `vectors` - Iterator of (node_id, embedding) pairs
    ///
    /// # Returns
    ///
    /// Number of successfully inserted vectors
    ///
    /// # Example
    ///
    /// ```ignore
    /// let vectors = vec![
    ///     ("func1".to_string(), vec![0.1, 0.2, ...]),
    ///     ("func2".to_string(), vec![0.3, 0.4, ...]),
    /// ];
    /// let inserted = index.insert_batch(vectors);
    /// ```
    pub fn insert_batch(
        &mut self,
        vectors: impl IntoIterator<Item = (String, Vec<f32>)>,
    ) -> usize {
        let mut inserted = 0;
        for (node_id, embedding) in vectors {
            if self.insert(node_id, embedding).is_ok() {
                inserted += 1;
            }
        }
        inserted
    }

    /// Search for similar vectors
    ///
    /// Performs cosine similarity search and returns the top-K most similar nodes.
    ///
    /// # Arguments
    ///
    /// * `query` - Query embedding vector
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// Vector of (node_id, similarity_score) pairs, sorted by similarity (descending)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let query = vec![0.1, 0.2, 0.3, ...];
    /// let results = index.search(&query, 10);
    /// for (node_id, score) in results {
    ///     println!("{}: {}", node_id, score);
    /// }
    /// ```
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension {
            return Vec::new();
        }

        // Calculate cosine similarity for all vectors
        let mut results: Vec<(String, f32)> = self
            .embeddings
            .iter()
            .map(|(node_id, embedding)| {
                let similarity = cosine_similarity(query, embedding);
                (node_id.clone(), similarity)
            })
            .collect();

        // Sort by similarity (descending)
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Return top-K
        results.into_iter().take(top_k).collect()
    }

    /// Get the number of vectors in the index
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Get the embedding dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Remove a vector from the index
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to remove
    ///
    /// # Returns
    ///
    /// `true` if the node was found and removed, `false` otherwise
    pub fn remove(&mut self, node_id: &str) -> bool {
        if self.embeddings.remove(node_id).is_some() {
            self.count -= 1;
            true
        } else {
            false
        }
    }

    /// Clear all vectors from the index
    pub fn clear(&mut self) {
        self.embeddings.clear();
        self.count = 0;
    }

    /// Get a vector by node ID
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to retrieve
    ///
    /// # Returns
    ///
    /// `Some(&embedding)` if found, `None` otherwise
    pub fn get(&self, node_id: &str) -> Option<&Vec<f32>> {
        self.embeddings.get(node_id)
    }
}

/// Calculate cosine similarity between two vectors
///
/// Cosine similarity = (A · B) / (||A|| * ||B||)
/// Returns a value between -1.0 and 1.0, where 1.0 is identical.
///
/// # Arguments
///
/// * `a` - First vector
/// * `b` - Second vector
///
/// # Returns
///
/// Cosine similarity score, or 0.0 if either vector is zero-length
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() {
        return 0.0;
    }

    let mut dot_product = 0.0;
    let mut norm_a = 0.0;
    let mut norm_b = 0.0;

    for i in 0..a.len() {
        dot_product += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let norm_a = norm_a.sqrt();
    let norm_b = norm_b.sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot_product / (norm_a * norm_b)
}

/// Vector search errors
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Provided embedding dimension does not match the index dimension
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Expected dimension
        expected: usize,
        /// Actual dimension received
        got: usize,
    },

    /// The index contains no vectors
    #[error("Index is empty")]
    EmptyIndex,

    /// The provided embedding is invalid (e.g., contains NaN or infinite values)
    #[error("Invalid embedding: {0}")]
    InvalidEmbedding(String),
}

/// Search result with node ID and similarity score
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchResult {
    /// Node identifier
    pub node_id: String,

    /// Similarity score (0.0 to 1.0, higher is better)
    pub score: f32,
}

impl SearchResult {
    /// Create a new search result
    pub fn new(node_id: String, score: f32) -> Self {
        Self { node_id, score }
    }
}

impl Default for VectorIndex {
    fn default() -> Self {
        Self::new(768) // Default to 768-dim embeddings (CodeRank)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vector_index_creation() {
        let index = VectorIndex::new(128);
        assert_eq!(index.dimension(), 128);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_vector_index_insert() {
        let mut index = VectorIndex::new(3);
        let result = index.insert("test".to_string(), vec![0.1, 0.2, 0.3]);
        assert!(result.is_ok());
        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_vector_index_dimension_mismatch() {
        let mut index = VectorIndex::new(3);
        let result = index.insert("test".to_string(), vec![0.1, 0.2]);
        assert!(result.is_err());
    }

    #[test]
    fn test_vector_index_search() {
        let mut index = VectorIndex::new(3);
        index.insert("a".to_string(), vec![1.0, 0.0, 0.0]).unwrap();
        index.insert("b".to_string(), vec![0.0, 1.0, 0.0]).unwrap();
        index.insert("c".to_string(), vec![0.9, 0.1, 0.0]).unwrap();

        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 2);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, "a"); // Most similar (identical)
        assert_eq!(results[1].0, "c"); // Second most similar
    }

    #[test]
    fn test_cosine_similarity() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < f32::EPSILON);

        let c = vec![0.0, 1.0, 0.0];
        assert!((cosine_similarity(&a, &c) - 0.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_vector_index_remove() {
        let mut index = VectorIndex::new(3);
        index.insert("test".to_string(), vec![0.1, 0.2, 0.3]).unwrap();
        assert_eq!(index.len(), 1);

        assert!(index.remove("test"));
        assert_eq!(index.len(), 0);
        assert!(!index.remove("nonexistent"));
    }

    #[test]
    fn test_vector_index_batch_insert() {
        let mut index = VectorIndex::new(3);
        let vectors = vec![
            ("a".to_string(), vec![1.0, 0.0, 0.0]),
            ("b".to_string(), vec![0.0, 1.0, 0.0]),
            ("c".to_string(), vec![0.0, 0.0, 1.0]),
        ];

        let inserted = index.insert_batch(vectors);
        assert_eq!(inserted, 3);
        assert_eq!(index.len(), 3);
    }

    #[test]
    fn test_vector_index_get() {
        let mut index = VectorIndex::new(3);
        let embedding = vec![0.1, 0.2, 0.3];
        index.insert("test".to_string(), embedding.clone()).unwrap();

        let retrieved = index.get("test");
        assert!(retrieved.is_some());
        assert_eq!(retrieved.unwrap(), &embedding);

        assert!(index.get("nonexistent").is_none());
    }

    #[test]
    fn test_vector_index_clear() {
        let mut index = VectorIndex::new(3);
        index.insert("a".to_string(), vec![1.0, 0.0, 0.0]).unwrap();
        index.insert("b".to_string(), vec![0.0, 1.0, 0.0]).unwrap();
        assert_eq!(index.len(), 2);

        index.clear();
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_search_with_zero_query() {
        let mut index = VectorIndex::new(3);
        index.insert("test".to_string(), vec![0.1, 0.2, 0.3]).unwrap();

        let results = index.search(&[0.0, 0.0, 0.0], 10);
        // Should still return results, just with 0.0 similarity
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, "test");
    }

    #[test]
    fn test_search_empty_index() {
        let index = VectorIndex::new(3);
        let results = index.search(&[0.1, 0.2, 0.3], 10);
        assert_eq!(results.len(), 0);
    }

    #[test]
    fn test_search_respects_top_k() {
        let mut index = VectorIndex::new(3);
        for i in 0..10 {
            let node_id = format!("node{}", i);
            let embedding = vec![1.0 / (i + 1) as f32, 0.0, 0.0];
            index.insert(node_id, embedding).unwrap();
        }

        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 3);
        assert_eq!(results.len(), 3);
    }
}
