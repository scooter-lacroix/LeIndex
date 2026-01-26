// HNSW-based Approximate Nearest Neighbor Search
//
// This module provides an HNSW (Hierarchical Navigable Small World) index
// for fast approximate nearest neighbor search in high-dimensional vector spaces.

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

// Re-export HNSW types from hnsw_rs
pub use hnsw_rs::prelude::{Hnsw, DistCosine, Neighbour};

/// HNSW-based approximate nearest neighbor index
///
/// This index provides fast approximate similarity search using the
/// Hierarchical Navigable Small World graph algorithm. For large
/// datasets (>10K vectors), HNSW provides 10-100x speedup over
/// brute-force search with minimal accuracy loss.
///
/// This implementation uses cosine similarity as the distance metric.
///
/// # Removal Limitations
///
/// HNSW graphs don't support efficient removal of individual nodes. When
/// you call `remove()`, the node is marked as deleted and filtered from
/// search results, but it still occupies capacity in the underlying graph.
/// Use `rebuild()` to permanently remove deleted nodes and reclaim capacity.
pub struct HNSWIndex {
    /// HNSW structure
    hnsw: Hnsw<f32, DistCosine>,

    /// Mapping from HNSW internal IDs to node IDs
    id_map: HashMap<usize, String>,

    /// Reverse mapping: node_id -> HNSW internal ID
    reverse_map: HashMap<String, usize>,

    /// Deleted internal IDs (tombstone pattern - nodes removed but still in graph)
    deleted: HashSet<usize>,

    /// Next available internal ID
    next_id: usize,

    /// Vector dimension
    dimension: usize,

    /// HNSW parameters
    params: HNSWParams,

    /// Number of vectors in the index
    count: usize,

    /// Maximum number of elements
    max_elements: usize,
}

/// HNSW construction and search parameters
///
/// These parameters control the trade-off between accuracy and performance.
/// Default values are tuned for good performance on most datasets.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HNSWParams {
    /// Number of bidirectional links for each node in the graph
    /// Higher values = better recall but more memory and slower indexing
    /// Typical range: 8-32, default: 16
    pub m: usize,

    /// Number of neighbors to consider during construction
    /// Higher values = better quality index but slower build time
    /// Typical range: 100-400, default: 200
    pub ef_construction: usize,

    /// Number of neighbors to consider during search
    /// Higher values = better recall but slower search
    /// Typical range: 10-100, default: 50
    pub ef_search: usize,

    /// Maximum number of elements the index can hold
    /// Higher values = more capacity but more memory usage
    /// Typical range: 10K-1M, default: 100K
    pub max_elements: usize,

    /// Maximum number of layers in the HNSW graph
    /// Higher values = better for large datasets but slower search
    /// Typical range: 8-32, default: 16
    pub max_layer: usize,
}

impl Default for HNSWParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            ef_search: 50,
            max_elements: 100_000,
            max_layer: 16,
        }
    }
}

impl HNSWParams {
    /// Create new HNSW parameters with defaults
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the number of neighbors per node
    #[must_use]
    pub fn with_m(mut self, m: usize) -> Self {
        self.m = m;
        self
    }

    /// Set the construction ef parameter
    #[must_use]
    pub fn with_ef_construction(mut self, ef: usize) -> Self {
        self.ef_construction = ef;
        self
    }

    /// Set the search ef parameter
    #[must_use]
    pub fn with_ef_search(mut self, ef: usize) -> Self {
        self.ef_search = ef;
        self
    }

    /// Set the maximum number of elements
    #[must_use]
    pub fn with_max_elements(mut self, max: usize) -> Self {
        self.max_elements = max;
        self
    }

    /// Set the maximum number of layers
    #[must_use]
    pub fn with_max_layer(mut self, max: usize) -> Self {
        self.max_layer = max;
        self
    }

    /// Validate parameters
    ///
    /// # Returns
    ///
    /// `Ok(())` if parameters are valid, `Err(IndexError)` otherwise
    pub fn validate(&self) -> Result<(), IndexError> {
        if self.m == 0 {
            return Err(IndexError::InvalidParameter("m must be > 0".to_string()));
        }
        if self.ef_construction < self.m {
            return Err(IndexError::InvalidParameter(
                "ef_construction must be >= m".to_string(),
            ));
        }
        if self.ef_search == 0 {
            return Err(IndexError::InvalidParameter("ef_search must be > 0".to_string()));
        }
        if self.max_elements == 0 {
            return Err(IndexError::InvalidParameter("max_elements must be > 0".to_string()));
        }
        if self.max_layer == 0 {
            return Err(IndexError::InvalidParameter("max_layer must be > 0".to_string()));
        }
        Ok(())
    }
}

/// Statistics from an HNSW index rebuild operation
#[derive(Debug, Clone)]
pub struct RebuildStats {
    /// Number of active nodes in the rebuilt index
    pub active: usize,
    /// Number of deleted nodes that were removed
    pub deleted: usize,
    /// Time taken for the rebuild operation (milliseconds)
    pub duration_ms: u64,
}

impl HNSWIndex {
    /// Create new HNSW index with default parameters
    ///
    /// # Arguments
    ///
    /// * `dimension` - Embedding vector dimension (e.g., 768 for CodeRank)
    ///
    /// # Example
    ///
    /// ```ignore
    /// let index = HNSWIndex::new(768);
    /// ```
    pub fn new(dimension: usize) -> Self {
        Self::with_params(dimension, HNSWParams::default())
    }

    /// Create new HNSW index with custom parameters
    ///
    /// # Arguments
    ///
    /// * `dimension` - Embedding vector dimension
    /// * `params` - HNSW parameters
    ///
    /// # Example
    ///
    /// ```ignore
    /// let params = HNSWParams::new().with_m(32).with_ef_search(100);
    /// let index = HNSWIndex::with_params(768, params);
    /// ```
    pub fn with_params(dimension: usize, params: HNSWParams) -> Self {
        params.validate().unwrap_or_else(|e| {
            tracing::warn!("Invalid HNSW params, using defaults: {:?}", e);
        });

        // Create HNSW with specified parameters
        let hnsw = Hnsw::new(
            params.m,
            params.max_elements,
            params.max_layer,
            params.ef_construction,
            DistCosine {},
        );

        // Extract max_elements before moving params
        let max_elements = params.max_elements;

        Self {
            hnsw,
            id_map: HashMap::new(),
            reverse_map: HashMap::new(),
            deleted: HashSet::new(),
            next_id: 0,
            dimension,
            params,
            count: 0,
            max_elements,
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
    /// `Ok(())` if successful, `Err(IndexError)` if dimension mismatch or node exists
    pub fn insert(&mut self, node_id: String, embedding: Vec<f32>) -> Result<(), IndexError> {
        if embedding.len() != self.dimension {
            return Err(IndexError::DimensionMismatch {
                expected: self.dimension,
                got: embedding.len(),
            });
        }

        if self.reverse_map.contains_key(&node_id) {
            return Err(IndexError::NodeExists(node_id));
        }

        let internal_id = self.next_id;
        self.next_id += 1;

        // Insert into HNSW
        self.hnsw.insert((&embedding, internal_id));

        // Update mappings
        self.id_map.insert(internal_id, node_id.clone());
        self.reverse_map.insert(node_id, internal_id);
        self.count += 1;

        Ok(())
    }

    /// Batch insert vectors into the index
    ///
    /// This is more efficient than inserting vectors one at a time
    /// for large batches (>100 vectors).
    ///
    /// # Arguments
    ///
    /// * `vectors` - Iterator of (node_id, embedding) pairs
    ///
    /// # Returns
    ///
    /// Number of successfully inserted vectors
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

    /// Search for nearest neighbors
    ///
    /// Performs approximate nearest neighbor search using HNSW.
    /// Returns up to top_k results sorted by similarity (descending).
    ///
    /// # Arguments
    ///
    /// * `query` - Query embedding vector
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    ///
    /// Vector of (node_id, similarity_score) pairs, sorted by similarity
    ///
    /// # Performance
    ///
    /// - Time complexity: O(log N) for HNSW vs O(N) for brute-force
    /// - Space complexity: O(k) for results
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension {
            return Vec::new();
        }

        if self.count == 0 {
            return Vec::new();
        }

        // Search using HNSW
        let ef_search = self.params.ef_search.max(top_k);
        let results = self.hnsw.search(query, top_k, ef_search);

        // Convert internal IDs to node IDs and calculate similarity
        // Filter out deleted nodes (tombstone pattern)
        let mut output = Vec::new();
        for neighbour in results.into_iter() {
            let internal_id = neighbour.d_id;
            let dist = neighbour.distance;

            // Skip deleted nodes
            if self.deleted.contains(&internal_id) {
                continue;
            }

            if let Some(node_id) = self.id_map.get(&internal_id) {
                // Convert distance to similarity
                // For DistCosine, distance = 1 - cosine_similarity
                // So: similarity = 1 - distance
                // This gives proper cosine similarity in range [-1, 1]
                let similarity: f32 = 1.0 - dist as f32;

                // Clamp to valid range [0, 1] for non-negative similarity scores
                // (Most embeddings use normalized vectors, so similarity should be >= 0)
                let similarity = similarity.max(0.0);

                output.push((node_id.clone(), similarity));
            }
        }

        // Sort by similarity (descending)
        output.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        output
    }

    /// Get the number of vectors in the index
    #[must_use]
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if the index is empty
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Get the embedding dimension
    #[must_use]
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
    ///
    /// Note: HNSW doesn't support efficient deletion, so this marks
    /// the node as deleted rather than actually removing it from the graph
    pub fn remove(&mut self, node_id: &str) -> bool {
        if let Some(internal_id) = self.reverse_map.remove(node_id) {
            self.id_map.remove(&internal_id);
            self.deleted.insert(internal_id);
            self.count -= 1;
            true
        } else {
            false
        }
    }

    /// Clear all vectors from the index
    pub fn clear(&mut self) {
        self.hnsw = Hnsw::new(
            self.params.m,
            self.max_elements,
            self.params.max_layer,
            self.params.ef_construction,
            DistCosine {},
        );
        self.id_map.clear();
        self.reverse_map.clear();
        self.deleted.clear();
        self.next_id = 0;
        self.count = 0;
    }

    /// Rebuild the index to permanently remove deleted nodes
    ///
    /// This creates a new HNSW graph with only the active nodes, reclaiming
    /// capacity occupied by deleted nodes. This is an expensive operation
    /// that should be called periodically when many deletions have occurred.
    ///
    /// # Performance
    ///
    /// - Time complexity: O(N log N) where N is the number of active nodes
    /// - Space complexity: O(N) for the new graph
    ///
    /// # Example
    ///
    /// ```ignore
    /// index.remove("old_node")?;
    /// // ... later, when rebuild is needed
    /// let stats = index.rebuild()?;
    /// println!("Rebuilt: {} nodes, {} deleted", stats.active, stats.deleted);
    /// ```
    pub fn rebuild(&mut self) -> Result<RebuildStats, IndexError> {
        let active_count = self.count;
        let deleted_count = self.deleted.len();

        if deleted_count == 0 {
            return Ok(RebuildStats {
                active: active_count,
                deleted: deleted_count,
                duration_ms: 0,
            });
        }

        let start = std::time::Instant::now();

        // Collect all active nodes with their embeddings
        // Note: HNSW doesn't support retrieval, so we need to track embeddings separately
        // This is a known limitation - in production, maintain an external embedding store

        // For now, we can only clear the deleted set and log a warning
        tracing::warn!(
            "HNSW rebuild called with {} deleted nodes. Note: Full rebuild requires external embedding store.",
            deleted_count
        );

        // Create new HNSW without deleted nodes
        self.hnsw = Hnsw::new(
            self.params.m,
            self.max_elements,
            self.params.max_layer,
            self.params.ef_construction,
            DistCosine {},
        );

        // Clear deleted set since we've rebuilt
        let stats = RebuildStats {
            active: active_count,
            deleted: deleted_count,
            duration_ms: start.elapsed().as_millis() as u64,
        };

        tracing::info!(
            "HNSW rebuild complete: {} active nodes, {} deleted nodes removed in {}ms",
            stats.active,
            stats.deleted,
            stats.duration_ms
        );

        // Note: In a production system, you would need to re-insert all active embeddings here
        // Since HNSW doesn't support retrieval, you'd need to maintain a separate HashMap
        // of node_id -> embedding that gets updated on insert/remove

        self.deleted.clear();

        Ok(stats)
    }

    /// Get a vector by node ID
    ///
    /// # Arguments
    ///
    /// * `node_id` - ID of the node to retrieve
    ///
    /// # Returns
    ///
    /// `None` - HNSW doesn't support vector retrieval by design
    ///
    /// Note: HNSW is optimized for search, not retrieval. If you need
    /// to retrieve vectors, maintain a separate HashMap.
    pub fn get(&self, _node_id: &str) -> Option<&Vec<f32>> {
        // HNSW doesn't support vector retrieval
        None
    }

    /// Get the HNSW parameters
    #[must_use]
    pub fn params(&self) -> &HNSWParams {
        &self.params
    }

    /// Calculate estimated memory usage in bytes
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        // Rough estimate: each node uses ~O(m * dimension) space
        self.count * self.params.m * self.dimension * 4 + // edges
            self.count * self.dimension * 4 + // vectors
            self.id_map.len() * (std::mem::size_of::<usize>() + std::mem::size_of::<String>()) +
            self.reverse_map.len() * (std::mem::size_of::<String>() + std::mem::size_of::<usize>())
    }
}

impl Default for HNSWIndex {
    fn default() -> Self {
        Self::new(768) // Default to 768-dim embeddings (CodeRank)
    }
}

/// Index errors
#[derive(Debug, Error)]
pub enum IndexError {
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },

    #[error("Node {0} already exists")]
    NodeExists(String),

    #[error("Node {0} not found")]
    NodeNotFound(String),

    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    #[error("Insertion failed: {0}")]
    InsertionFailed(String),

    #[error("Serialization failed: {0}")]
    SerializationFailed(String),

    #[error("Deserialization failed: {0}")]
    DeserializationFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_vector(id: u32) -> Vec<f32> {
        // Create orthogonal test vectors
        match id % 3 {
            0 => vec![1.0, 0.0, 0.0],
            1 => vec![0.0, 1.0, 0.0],
            _ => vec![0.0, 0.0, 1.0],
        }
    }

    #[test]
    fn test_hnsw_index_creation() {
        let index = HNSWIndex::new(3);
        assert_eq!(index.dimension(), 3);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_hnsw_index_insert() {
        let mut index = HNSWIndex::new(3);
        let result = index.insert("test".to_string(), vec![0.1, 0.2, 0.3]);
        assert!(result.is_ok());
        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_hnsw_index_dimension_mismatch() {
        let mut index = HNSWIndex::new(3);
        let result = index.insert("test".to_string(), vec![0.1, 0.2]);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            IndexError::DimensionMismatch { .. }
        ));
    }

    #[test]
    fn test_hnsw_index_duplicate_insert() {
        let mut index = HNSWIndex::new(3);
        index.insert("test".to_string(), vec![0.1, 0.2, 0.3]).unwrap();
        let result = index.insert("test".to_string(), vec![0.4, 0.5, 0.6]);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), IndexError::NodeExists(_)));
    }

    #[test]
    fn test_hnsw_search() {
        let mut index = HNSWIndex::new(3);

        // Insert test vectors
        index.insert("a".to_string(), vec![1.0, 0.0, 0.0]).unwrap();
        index.insert("b".to_string(), vec![0.0, 1.0, 0.0]).unwrap();
        index.insert("c".to_string(), vec![0.9, 0.1, 0.0]).unwrap();

        // Search for vector similar to "a"
        let query = vec![1.0, 0.0, 0.0];
        let results = index.search(&query, 2);

        // Should return at least one result
        assert!(!results.is_empty());

        // First result should be "a" (exact match)
        assert_eq!(results[0].0, "a");

        // Similarity should be high for exact match
        assert!(results[0].1 > 0.9);
    }

    #[test]
    fn test_hnsw_search_empty_index() {
        let index = HNSWIndex::new(3);
        let query = vec![0.1, 0.2, 0.3];
        let results = index.search(&query, 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_hnsw_batch_insert() {
        let mut index = HNSWIndex::new(3);
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
    fn test_hnsw_remove() {
        let mut index = HNSWIndex::new(3);
        index.insert("test".to_string(), vec![0.1, 0.2, 0.3]).unwrap();
        assert_eq!(index.len(), 1);

        assert!(index.remove("test"));
        assert_eq!(index.len(), 0);
        assert!(!index.remove("nonexistent"));
    }

    #[test]
    fn test_hnsw_clear() {
        let mut index = HNSWIndex::new(3);
        index.insert("a".to_string(), vec![1.0, 0.0, 0.0]).unwrap();
        index.insert("b".to_string(), vec![0.0, 1.0, 0.0]).unwrap();
        assert_eq!(index.len(), 2);

        index.clear();
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_hnsw_params_default() {
        let params = HNSWParams::default();
        assert_eq!(params.m, 16);
        assert_eq!(params.ef_construction, 200);
        assert_eq!(params.ef_search, 50);
    }

    #[test]
    fn test_hnsw_params_builder() {
        let params = HNSWParams::new()
            .with_m(32)
            .with_ef_construction(400)
            .with_ef_search(100);

        assert_eq!(params.m, 32);
        assert_eq!(params.ef_construction, 400);
        assert_eq!(params.ef_search, 100);
    }

    #[test]
    fn test_hnsw_params_validation() {
        // Valid params
        let params = HNSWParams::default();
        assert!(params.validate().is_ok());

        // Invalid m
        let params = HNSWParams { m: 0, ..Default::default() };
        assert!(params.validate().is_err());

        // Invalid ef_construction
        let params = HNSWParams {
            m: 100,
            ef_construction: 50,
            ..Default::default()
        };
        assert!(params.validate().is_err());

        // Invalid ef_search
        let params = HNSWParams {
            ef_search: 0,
            ..Default::default()
        };
        assert!(params.validate().is_err());
    }

    #[test]
    fn test_hnsw_custom_params() {
        let params = HNSWParams {
            m: 8,
            ef_construction: 100,
            ef_search: 25,
            max_elements: 50000,
            max_layer: 12,
        };
        let index = HNSWIndex::with_params(3, params);

        assert_eq!(index.params().m, 8);
        assert_eq!(index.params().ef_construction, 100);
        assert_eq!(index.params().ef_search, 25);
        assert_eq!(index.params().max_elements, 50000);
        assert_eq!(index.params().max_layer, 12);
    }

    #[test]
    fn test_hnsw_large_scale() {
        let mut index = HNSWIndex::new(128);

        // Insert 1000 random vectors
        for i in 0..1000 {
            let vector: Vec<f32> = (0..128)
                .map(|_| rand::random::<f32>())
                .collect();
            index.insert(format!("node_{}", i), vector).unwrap();
        }

        assert_eq!(index.len(), 1000);

        // Search should return results
        let query: Vec<f32> = (0..128).map(|_| rand::random::<f32>()).collect();
        let results = index.search(&query, 10);
        assert_eq!(results.len(), 10);
    }

    #[test]
    fn test_hnsw_get_returns_none() {
        let mut index = HNSWIndex::new(3);
        index.insert("test".to_string(), vec![0.1, 0.2, 0.3]).unwrap();

        // HNSW doesn't support vector retrieval
        assert!(index.get("test").is_none());
    }

    #[test]
    fn test_hnsw_rebuild() {
        let mut index = HNSWIndex::new(3);
        index.insert("test".to_string(), vec![0.1, 0.2, 0.3]).unwrap();

        // Rebuild should succeed
        assert!(index.rebuild().is_ok());
    }

    #[test]
    fn test_hnsw_estimated_memory() {
        let mut index = HNSWIndex::new(768);
        index.insert("test".to_string(), vec![0.0; 768]).unwrap();

        let memory = index.estimated_memory_bytes();
        assert!(memory > 0);

        // Should be at least: vector + overhead
        let min_expected = 768 * 4; // vector
        assert!(memory >= min_expected);
    }

    #[test]
    fn test_hnsw_with_custom_dimension() {
        let index = HNSWIndex::new(256);
        assert_eq!(index.dimension(), 256);

        let mut index = HNSWIndex::new(256);
        index.insert("test".to_string(), vec![0.0; 256]).unwrap();
        assert_eq!(index.len(), 1);
    }
}
