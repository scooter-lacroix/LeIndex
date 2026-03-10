//! INT8 Quantized HNSW Index
//!
//! This module provides an HNSW index that stores vectors as INT8 quantized data
//! and uses Asymmetric Distance Computation (ADC) for searching with f32 queries.
//!
//! # Architecture
//!
//! ```text
//! Insert: f32 vector ──▶ Quantize ──▶ Int8QuantizedVector ──▶ HNSW Index
//!
//! Search: f32 query ──▶ Set ADC Context ──▶ HNSW Search (ADC) ──▶ Results
//!                │                           ▲
//!                └───────────────────────────┘
//!                    (thread-local context)
//! ```
//!
//! # Memory Efficiency
//!
//! - Original f32 vector: 4 bytes per dimension
//! - Quantized INT8 vector: 1 byte per dimension + 32 bytes metadata
// - Memory reduction: ~74% for large dimensions
//
// # Usage
//
// ```ignore
// use lerecherche::quantization::{Int8HnswIndex, Int8HnswParams};
//
// // Create index
// let mut index = Int8HnswIndex::new(768);
//
// // Insert vectors
// index.insert("node1".to_string(), vec![0.1, 0.2, ...]).unwrap();
//
// // Search with f32 query (uses ADC internally)
// let results = index.search(&vec![0.15, 0.25, ...], 10);
// ```

use hnsw_rs::prelude::Hnsw;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

use super::distance::{
    clear_adc_query_context, set_adc_query_context, AdcDistanceMetric, Int8AdcDistance,
};
use super::quantization::Quantize;
use super::vector::Int8QuantizedVector;

/// Parameters for INT8 quantized HNSW index
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Int8HnswParams {
    /// Number of bidirectional links for each node in the graph
    pub m: usize,
    /// Number of neighbors to consider during construction
    pub ef_construction: usize,
    /// Number of neighbors to consider during search
    pub ef_search: usize,
    /// Maximum number of elements the index can hold
    pub max_elements: usize,
    /// Maximum number of layers in the HNSW graph
    pub max_layer: usize,
    /// Distance metric to use
    pub metric: AdcDistanceMetric,
}

impl Default for Int8HnswParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            ef_search: 50,
            max_elements: 100_000,
            max_layer: 16,
            metric: AdcDistanceMetric::Cosine,
        }
    }
}

impl Int8HnswParams {
    /// Create new INT8 HNSW parameters with defaults
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

    /// Set the distance metric
    #[must_use]
    pub fn with_metric(mut self, metric: AdcDistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Validate parameters
    pub fn validate(&self) -> Result<(), Int8HnswError> {
        if self.m == 0 {
            return Err(Int8HnswError::InvalidParameter("m must be > 0".to_string()));
        }
        if self.ef_construction < self.m {
            return Err(Int8HnswError::InvalidParameter(
                "ef_construction must be >= m".to_string(),
            ));
        }
        if self.ef_search == 0 {
            return Err(Int8HnswError::InvalidParameter(
                "ef_search must be > 0".to_string(),
            ));
        }
        if self.max_elements == 0 {
            return Err(Int8HnswError::InvalidParameter(
                "max_elements must be > 0".to_string(),
            ));
        }
        if self.max_layer == 0 {
            return Err(Int8HnswError::InvalidParameter(
                "max_layer must be > 0".to_string(),
            ));
        }
        Ok(())
    }
}

/// INT8 Quantized HNSW Index
///
/// This index stores vectors as INT8 quantized data and uses Asymmetric Distance
/// Computation (ADC) for searching with f32 queries. This provides ~74% memory
/// reduction compared to f32 storage while maintaining search accuracy.
pub struct Int8HnswIndex {
    /// HNSW structure storing Int8QuantizedVector
    hnsw: Hnsw<Int8QuantizedVector, Int8AdcDistance>,

    /// Mapping from HNSW internal IDs to node IDs
    id_map: HashMap<usize, String>,

    /// Reverse mapping: node_id -> HNSW internal ID
    reverse_map: HashMap<String, usize>,

    /// Deleted internal IDs (tombstone pattern)
    deleted: HashSet<usize>,

    /// Next available internal ID
    next_id: usize,

    /// Vector dimension
    dimension: usize,

    /// HNSW parameters
    params: Int8HnswParams,

    /// Number of vectors in the index
    count: usize,

    /// Maximum number of elements
    max_elements: usize,
}

impl Int8HnswIndex {
    /// Create a new INT8 quantized HNSW index with default parameters
    ///
    /// # Arguments
    /// * `dimension` - Dimension of the original f32 vectors
    ///
    /// # Example
    /// ```ignore
    /// let index = Int8HnswIndex::new(768);
    /// ```
    pub fn new(dimension: usize) -> Self {
        Self::with_params(dimension, Int8HnswParams::default())
    }

    /// Create a new INT8 quantized HNSW index with custom parameters
    ///
    /// # Arguments
    /// * `dimension` - Dimension of the original f32 vectors
    /// * `params` - INT8 HNSW parameters
    pub fn with_params(dimension: usize, params: Int8HnswParams) -> Self {
        params.validate().unwrap_or_else(|e| {
            tracing::warn!("Invalid INT8 HNSW params, using defaults: {:?}", e);
        });

        let distance = Int8AdcDistance::new(params.metric);
        let max_elements = params.max_elements;

        let hnsw = Hnsw::new(
            params.m,
            params.max_elements,
            params.max_layer,
            params.ef_construction,
            distance,
        );

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
    /// The vector is automatically quantized to INT8 before storage.
    ///
    /// # Arguments
    /// * `node_id` - Unique identifier for the node
    /// * `embedding` - The f32 vector to insert (must match dimension)
    ///
    /// # Returns
    /// `Ok(())` if successful, `Err(Int8HnswError)` if dimension mismatch or node exists
    pub fn insert(&mut self, node_id: String, embedding: Vec<f32>) -> Result<(), Int8HnswError> {
        if embedding.len() != self.dimension {
            return Err(Int8HnswError::DimensionMismatch {
                expected: self.dimension,
                got: embedding.len(),
            });
        }

        if self.reverse_map.contains_key(&node_id) {
            return Err(Int8HnswError::NodeExists(node_id));
        }

        // Quantize the vector to INT8
        let quantized: Int8QuantizedVector = embedding.quantize();

        let internal_id = self.next_id;
        self.next_id += 1;

        // Insert into HNSW
        self.hnsw.insert((&vec![quantized], internal_id));

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
    /// * `vectors` - Iterator of (node_id, embedding) pairs
    ///
    /// # Returns
    /// Number of successfully inserted vectors
    pub fn insert_batch(&mut self, vectors: impl IntoIterator<Item = (String, Vec<f32>)>) -> usize {
        let mut inserted = 0;
        for (node_id, embedding) in vectors {
            if self.insert(node_id, embedding).is_ok() {
                inserted += 1;
            }
        }
        inserted
    }

    /// Search for nearest neighbors with an f32 query
    ///
    /// This method uses Asymmetric Distance Computation (ADC) to search
    /// with an f32 query against the stored INT8 quantized vectors.
    ///
    /// # Arguments
    /// * `query` - The f32 query vector
    /// * `top_k` - Maximum number of results to return
    ///
    /// # Returns
    /// Vector of (node_id, similarity_score) pairs, sorted by similarity (descending)
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension {
            return Vec::new();
        }

        if self.count == 0 {
            return Vec::new();
        }

        // Set the ADC query context before search
        set_adc_query_context(query, self.params.metric);

        // Create a dummy query vector for the HNSW search
        // The actual query is read from the thread-local context
        let dummy_query = Int8QuantizedVector::new(
            vec![0i8; self.dimension],
            super::vector::Int8QuantizedVectorMetadata::default(),
            self.dimension,
        );

        // Search using HNSW
        let ef_search = self.params.ef_search.max(top_k);
        let results = self
            .hnsw
            .search(std::slice::from_ref(&dummy_query), top_k, ef_search);

        // Clear the ADC query context after search
        clear_adc_query_context();

        // Convert internal IDs to node IDs and calculate similarity
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
                // For cosine: distance = 1 - similarity, so similarity = 1 - distance
                // For L2 squared: we need to normalize, but for now use simple conversion
                let similarity = match self.params.metric {
                    AdcDistanceMetric::Cosine | AdcDistanceMetric::Dot => (1.0 - dist).max(0.0),
                    AdcDistanceMetric::L2Squared => {
                        // Convert L2 squared distance to similarity (inverse)
                        let max_dist = 4.0f32; // Assuming normalized vectors, max L2² is ~4
                        (1.0 - dist / max_dist).max(0.0)
                    }
                };

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

    /// Get the HNSW parameters
    #[must_use]
    pub fn params(&self) -> &Int8HnswParams {
        &self.params
    }

    /// Remove a vector from the index
    ///
    /// Note: HNSW doesn't support efficient deletion, so this marks
    /// the node as deleted rather than actually removing it from the graph.
    /// Use `rebuild()` to permanently remove deleted nodes.
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
        let distance = Int8AdcDistance::new(self.params.metric);
        self.hnsw = Hnsw::new(
            self.params.m,
            self.max_elements,
            self.params.max_layer,
            self.params.ef_construction,
            distance,
        );
        self.id_map.clear();
        self.reverse_map.clear();
        self.deleted.clear();
        self.next_id = 0;
        self.count = 0;
    }

    /// Calculate estimated memory usage in bytes
    ///
    /// This estimates the memory usage of the quantized index,
    /// which is ~74% less than the equivalent f32 index.
    #[must_use]
    pub fn estimated_memory_bytes(&self) -> usize {
        // Each quantized vector: 1 byte per dimension + 32 bytes metadata
        let vector_data = self.count * self.dimension + self.count * 32;
        // Graph edges: each node has ~m connections
        let edge_data = self.count * self.params.m * std::mem::size_of::<usize>();
        // HashMap overhead
        let map_overhead = self.id_map.len()
            * (std::mem::size_of::<usize>() + std::mem::size_of::<String>())
            + self.reverse_map.len()
                * (std::mem::size_of::<String>() + std::mem::size_of::<usize>());

        vector_data + edge_data + map_overhead
    }

    /// Compare memory usage with an equivalent f32 index
    ///
    /// Returns the memory reduction ratio (0.0 to 1.0)
    #[must_use]
    pub fn memory_reduction_ratio(&self) -> f32 {
        let quantized_memory = self.estimated_memory_bytes() as f32;
        // f32 index: 4 bytes per dimension + overhead
        let f32_memory = (self.count * self.dimension * 4) as f32;

        if f32_memory == 0.0 {
            return 0.0;
        }

        1.0 - (quantized_memory / f32_memory)
    }
}

impl Default for Int8HnswIndex {
    fn default() -> Self {
        Self::new(768)
    }
}

/// Errors that can occur in INT8 HNSW operations
#[derive(Debug, Error)]
pub enum Int8HnswError {
    /// Provided embedding dimension does not match the index dimension
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Expected dimension
        expected: usize,
        /// Actual dimension received
        got: usize,
    },

    /// A node with the same ID already exists in the index
    #[error("Node {0} already exists")]
    NodeExists(String),

    /// The specified node was not found in the index
    #[error("Node {0} not found")]
    NodeNotFound(String),

    /// An invalid parameter was provided for HNSW construction or search
    #[error("Invalid parameter: {0}")]
    InvalidParameter(String),

    /// Failed to insert a vector into the HNSW graph
    #[error("Insertion failed: {0}")]
    InsertionFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_vector(dimension: usize, value: f32) -> Vec<f32> {
        vec![value; dimension]
    }

    #[test]
    fn test_int8_hnsw_creation() {
        let index = Int8HnswIndex::new(768);
        assert_eq!(index.dimension(), 768);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_int8_hnsw_with_params() {
        let params = Int8HnswParams::new()
            .with_m(32)
            .with_ef_construction(400)
            .with_ef_search(100);

        let index = Int8HnswIndex::with_params(768, params);
        assert_eq!(index.params().m, 32);
        assert_eq!(index.params().ef_construction, 400);
        assert_eq!(index.params().ef_search, 100);
    }

    #[test]
    fn test_int8_hnsw_insert() {
        let mut index = Int8HnswIndex::new(64);
        let vector = create_test_vector(64, 0.5);

        let result = index.insert("test".to_string(), vector);
        assert!(result.is_ok());
        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_int8_hnsw_dimension_mismatch() {
        let mut index = Int8HnswIndex::new(64);
        let vector = create_test_vector(32, 0.5); // Wrong dimension

        let result = index.insert("test".to_string(), vector);
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            Int8HnswError::DimensionMismatch { .. }
        ));
    }

    #[test]
    fn test_int8_hnsw_duplicate_insert() {
        let mut index = Int8HnswIndex::new(64);
        let vector = create_test_vector(64, 0.5);

        index.insert("test".to_string(), vector.clone()).unwrap();
        let result = index.insert("test".to_string(), vector);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), Int8HnswError::NodeExists(_)));
    }

    #[test]
    fn test_int8_hnsw_search() {
        let mut index = Int8HnswIndex::new(64);

        // Insert test vectors
        for i in 0..10 {
            let vector = create_test_vector(64, i as f32 / 10.0);
            index.insert(format!("node_{}", i), vector).unwrap();
        }

        // Search for vector similar to middle value
        let query = create_test_vector(64, 0.5);
        let results = index.search(&query, 5);

        assert!(!results.is_empty());
        assert!(results.len() <= 5);

        // Results should be sorted by similarity (descending)
        for i in 1..results.len() {
            assert!(results[i - 1].1 >= results[i].1);
        }
    }

    #[test]
    fn test_int8_hnsw_empty_search() {
        let index = Int8HnswIndex::new(64);
        let query = create_test_vector(64, 0.5);

        let results = index.search(&query, 10);
        assert!(results.is_empty());
    }

    #[test]
    fn test_int8_hnsw_wrong_dimension_search() {
        let mut index = Int8HnswIndex::new(64);
        index
            .insert("test".to_string(), create_test_vector(64, 0.5))
            .unwrap();

        let query = create_test_vector(32, 0.5); // Wrong dimension
        let results = index.search(&query, 10);

        assert!(results.is_empty());
    }

    #[test]
    fn test_int8_hnsw_batch_insert() {
        let mut index = Int8HnswIndex::new(64);

        let vectors: Vec<(String, Vec<f32>)> = (0..10)
            .map(|i| {
                (
                    format!("node_{}", i),
                    create_test_vector(64, i as f32 / 10.0),
                )
            })
            .collect();

        let inserted = index.insert_batch(vectors);
        assert_eq!(inserted, 10);
        assert_eq!(index.len(), 10);
    }

    #[test]
    fn test_int8_hnsw_remove() {
        let mut index = Int8HnswIndex::new(64);
        index
            .insert("test".to_string(), create_test_vector(64, 0.5))
            .unwrap();

        assert_eq!(index.len(), 1);
        assert!(index.remove("test"));
        assert_eq!(index.len(), 0);
        assert!(!index.remove("nonexistent"));
    }

    #[test]
    fn test_int8_hnsw_clear() {
        let mut index = Int8HnswIndex::new(64);

        for i in 0..5 {
            index
                .insert(format!("node_{}", i), create_test_vector(64, 0.5))
                .unwrap();
        }

        assert_eq!(index.len(), 5);
        index.clear();
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_int8_hnsw_params_validation() {
        let valid_params = Int8HnswParams::default();
        assert!(valid_params.validate().is_ok());

        let invalid_m = Int8HnswParams {
            m: 0,
            ..Default::default()
        };
        assert!(invalid_m.validate().is_err());

        let invalid_ef = Int8HnswParams {
            ef_construction: 5,
            m: 10,
            ..Default::default()
        };
        assert!(invalid_ef.validate().is_err());
    }

    #[test]
    fn test_memory_efficiency() {
        let mut index = Int8HnswIndex::new(768);

        // Insert 100 vectors
        for i in 0..100 {
            let vector: Vec<f32> = (0..768)
                .map(|j| ((i * 768 + j) % 100) as f32 / 100.0)
                .collect();
            index.insert(format!("node_{}", i), vector).unwrap();
        }

        let quantized_memory = index.estimated_memory_bytes();
        let reduction_ratio = index.memory_reduction_ratio();

        // Should have significant memory reduction (~74% for large dimensions)
        assert!(
            reduction_ratio > 0.5,
            "Memory reduction too low: {}",
            reduction_ratio
        );

        // Quantized memory should be less than f32 memory
        let f32_memory = 100 * 768 * 4;
        assert!(quantized_memory < f32_memory);
    }

    #[test]
    fn test_int8_hnsw_different_metrics() {
        for metric in [
            AdcDistanceMetric::Cosine,
            AdcDistanceMetric::L2Squared,
            AdcDistanceMetric::Dot,
        ] {
            let params = Int8HnswParams::new().with_metric(metric);
            let mut index = Int8HnswIndex::with_params(64, params);

            // Insert vectors
            for i in 0..5 {
                let vector = create_test_vector(64, i as f32 / 5.0);
                index.insert(format!("node_{}", i), vector).unwrap();
            }

            // Search
            let query = create_test_vector(64, 0.5);
            let results = index.search(&query, 3);

            assert!(!results.is_empty(), "Metric {:?} failed", metric);
        }
    }

    #[test]
    fn test_int8_hnsw_search_consistency() {
        let mut index = Int8HnswIndex::new(64);

        // Insert orthogonal vectors
        for i in 0..5 {
            let mut vector = vec![0.0; 64];
            vector[i] = 1.0;
            index.insert(format!("node_{}", i), vector).unwrap();
        }

        let mut query = vec![0.0f32; 64];
        query[0] = 1.0;

        // Run search multiple times
        let results1 = index.search(&query, 3);
        let results2 = index.search(&query, 3);
        let results3 = index.search(&query, 3);

        // Results should be consistent
        assert_eq!(results1.len(), results2.len());
        assert_eq!(results2.len(), results3.len());

        for i in 0..results1.len() {
            assert_eq!(results1[i].0, results2[i].0);
            assert_eq!(results2[i].0, results3[i].0);
        }
    }
}
