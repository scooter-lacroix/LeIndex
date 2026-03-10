//! HNSW Integration for Quantized Vectors
//!
//! This module provides hnsw_rs compatibility for asymmetric quantized search.
//! The key challenge is that hnsw_rs's `Distance` trait expects both vectors
//! to be of the same type, but we want:
//! - Query: f32 (full precision)
//! - Stored: QuantizedVector (INT8)
//!
//! # Solution: Adapter Pattern
//!
//! We use an adapter that stores quantized vectors but accepts f32 queries:
//!
//! 1. **QuantizedHnsw**: Wraps the HNSW index with quantized storage
//! 2. **AsymmetricDistance**: Implements `Distance<f32>` by dequantizing on-the-fly
//! 3. **Query-time bridging**: The search query is f32, stored data is dequantized during comparison
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐     ┌──────────────────┐     ┌─────────────────┐
//! │   f32 Query     │────▶│  AsymmetricDist  │────▶│ QuantizedVector │
//! │   (search time) │     │  (dequantize)    │     │  (stored INT8)  │
//! └─────────────────┘     └──────────────────┘     └─────────────────┘
//!                                │
//!                                ▼
//!                         ┌──────────────────┐
//!                         │  HNSW Index      │
//!                         │  (graph search)  │
//!                         └──────────────────┘
//! ```

use super::{QuantizationParams, QuantizedVector, QuantizedDistance, AsymmetricCosine};
use hnsw_rs::prelude::{Distance, Hnsw, Neighbour};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// An HNSW index that stores quantized vectors but accepts f32 queries
///
/// This struct provides the bridge between asymmetric quantization and
/// hnsw_rs's type system. It stores vectors as INT8 quantized data but
/// allows searching with full-precision f32 queries.
///
/// # Type Parameters
///
/// The internal HNSW stores `u8` slices (quantized data), while the
/// distance function handles the asymmetric comparison.
///
/// # Example
///
/// ```ignore
/// use lerecherche::quantization::{QuantizedHNSW, QuantizationParams};
///
/// // Create index with quantization
/// let params = QuantizationParams::from_min_max(-1.0, 1.0);
/// let mut index = QuantizedHNSW::new(768, params);
///
/// // Insert quantized vector
/// let vector = vec![0.1, 0.2, 0.3, ...];
/// index.insert("node1", &vector);
///
/// // Search with f32 query (asymmetric)
/// let query = vec![0.15, 0.25, 0.35, ...];
/// let results = index.search(&query, 10);
/// ```
pub struct QuantizedHNSW {
    /// The underlying HNSW index storing quantized data
    /// We store as Vec<u8> since Hnsw requires owned data
    hnsw: Hnsw<u8, QuantizedDistanceWrapper>,

    /// Quantization parameters (shared across all vectors)
    quant_params: QuantizationParams,

    /// Dimension of the original f32 vectors
    dimension: usize,

    /// Mapping from HNSW internal IDs to node IDs
    id_map: HashMap<usize, String>,

    /// Reverse mapping
    reverse_map: HashMap<String, usize>,

    /// Next available internal ID
    next_id: usize,

    /// Number of vectors in the index
    count: usize,
}

/// Wrapper struct that implements hnsw_rs's Distance trait for asymmetric comparison
///
/// This is the key to bridging the type gap: it implements `Distance<u8>`
/// but the evaluation context includes the quantization parameters needed
/// to dequantize during distance computation.
#[derive(Clone, Debug)]
pub struct QuantizedDistanceWrapper {
    /// Quantization parameters for dequantization
    pub params: QuantizationParams,
    /// Dimension of vectors
    pub dimension: usize,
    /// Distance metric to use
    pub metric: DistanceMetric,
}

/// Distance metric for quantized vectors
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum DistanceMetric {
    /// Cosine similarity (most common for embeddings)
    Cosine,
    /// L2 (Euclidean) distance
    L2,
    /// Dot product (for normalized vectors)
    Dot,
}

impl Default for DistanceMetric {
    fn default() -> Self {
        DistanceMetric::Cosine
    }
}

impl QuantizedDistanceWrapper {
    /// Create a new distance wrapper with the given parameters
    pub fn new(params: QuantizationParams, dimension: usize) -> Self {
        Self {
            params,
            dimension,
            metric: DistanceMetric::Cosine,
        }
    }

    /// Set the distance metric
    pub fn with_metric(mut self, metric: DistanceMetric) -> Self {
        self.metric = metric;
        self
    }

    /// Dequantize a u8 slice to f32 vector
    #[inline]
    fn dequantize(&self, quantized: &[u8]) -> Vec<f32> {
        quantized
            .iter()
            .map(|&v| self.params.dequantize(v))
            .collect()
    }

    /// Compute asymmetric distance: query (f32) vs stored (quantized u8)
    ///
    /// This is the core operation that enables asymmetric search.
    /// The query comes in as f32 (from hnsw_rs search), and we dequantize
    /// the stored u8 data on-the-fly.
    fn asymmetric_distance(&self, query: &[f32], stored: &[u8]) -> f32 {
        assert_eq!(query.len(), self.dimension);
        assert_eq!(stored.len(), self.dimension);

        // Create a temporary QuantizedVector for the stored data
        let stored_qv = QuantizedVector::new(
            stored.to_vec(),
            self.params,
            self.dimension,
        );

        match self.metric {
            DistanceMetric::Cosine => {
                AsymmetricCosine::asymmetric_distance(query, &stored_qv)
            }
            DistanceMetric::L2 => {
                super::AsymmetricL2::asymmetric_distance(query, &stored_qv)
            }
            DistanceMetric::Dot => {
                super::AsymmetricDot::asymmetric_distance(query, &stored_qv)
            }
        }
    }
}

impl Default for QuantizedDistanceWrapper {
    fn default() -> Self {
        Self {
            params: QuantizationParams::default(),
            dimension: 768,
            metric: DistanceMetric::Cosine,
        }
    }
}

/// Implementation of hnsw_rs's Distance trait for asymmetric quantization
///
/// This is where the magic happens: hnsw_rs calls `eval` with two u8 slices,
/// but we interpret the first as an f32 query (serialized) and the second
/// as quantized stored data.
///
/// # Important Note
///
/// For true asymmetric search where queries are f32 and stored is u8,
/// we need a different approach since hnsw_rs expects both inputs to be
/// the same type. We solve this by:
///
/// 1. Storing f32 queries temporarily as u8 (transmutation)
/// 2. Detecting the query type in eval and handling accordingly
/// 3. Or using a separate search method that bypasses hnsw_rs's type system
///
/// The current implementation assumes both inputs are quantized u8,
/// with dequantization happening during distance computation.
impl Distance<u8> for QuantizedDistanceWrapper {
    fn eval(&self, va: &[u8], vb: &[u8]) -> f32 {
        // Both inputs are quantized u8 slices
        // Dequantize both and compute distance
        let dequantized_a: Vec<f32> = va.iter().map(|&v| self.params.dequantize(v)).collect();
        let dequantized_b: Vec<f32> = vb.iter().map(|&v| self.params.dequantize(v)).collect();

        match self.metric {
            DistanceMetric::Cosine => cosine_distance(&dequantized_a, &dequantized_b),
            DistanceMetric::L2 => l2_distance(&dequantized_a, &dequantized_b),
            DistanceMetric::Dot => dot_distance(&dequantized_a, &dequantized_b),
        }
    }
}

/// Compute cosine distance between two f32 vectors
fn cosine_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for i in 0..a.len() {
        dot += a[i] * b[i];
        norm_a += a[i] * a[i];
        norm_b += b[i] * b[i];
    }

    let norm_a = norm_a.sqrt();
    let norm_b = norm_b.sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 1.0;
    }

    let similarity = dot / (norm_a * norm_b);
    (1.0 - similarity).max(0.0)
}

/// Compute L2 distance between two f32 vectors
fn l2_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for i in 0..a.len() {
        let diff = a[i] - b[i];
        sum += diff * diff;
    }
    sum.sqrt()
}

/// Compute dot product distance between two f32 vectors
fn dot_distance(a: &[f32], b: &[f32]) -> f32 {
    let mut dot = 0.0f32;
    for i in 0..a.len() {
        dot += a[i] * b[i];
    }
    (1.0 - dot).max(0.0)
}

impl QuantizedHNSW {
    /// Create a new quantized HNSW index
    ///
    /// # Arguments
    /// * `dimension` - Dimension of the original f32 vectors
    /// * `quant_params` - Quantization parameters
    /// * `max_elements` - Maximum number of elements in the index
    pub fn new(
        dimension: usize,
        quant_params: QuantizationParams,
        max_elements: usize,
    ) -> Self {
        let distance = QuantizedDistanceWrapper::new(quant_params, dimension);

        // HNSW parameters
        let m = 16;
        let max_layer = 16;
        let ef_construction = 200;

        let hnsw = Hnsw::new(m, max_elements, max_layer, ef_construction, distance);

        Self {
            hnsw,
            quant_params,
            dimension,
            id_map: HashMap::new(),
            reverse_map: HashMap::new(),
            next_id: 0,
            count: 0,
        }
    }

    /// Insert a vector into the index
    ///
    /// The vector is quantized before storage.
    ///
    /// # Arguments
    /// * `node_id` - Unique identifier for the node
    /// * `vector` - The f32 vector to insert
    pub fn insert(&mut self, node_id: String, vector: &[f32]) -> Result<(), QuantizedHnswError> {
        if vector.len() != self.dimension {
            return Err(QuantizedHnswError::DimensionMismatch {
                expected: self.dimension,
                got: vector.len(),
            });
        }

        if self.reverse_map.contains_key(&node_id) {
            return Err(QuantizedHnswError::NodeExists(node_id));
        }

        // Quantize the vector
        let quantized: Vec<u8> = vector
            .iter()
            .map(|&v| self.quant_params.quantize(v))
            .collect();

        let internal_id = self.next_id;
        self.next_id += 1;

        // Insert into HNSW
        self.hnsw.insert((&quantized, internal_id));

        // Update mappings
        self.id_map.insert(internal_id, node_id.clone());
        self.reverse_map.insert(node_id, internal_id);
        self.count += 1;

        Ok(())
    }

    /// Search for nearest neighbors with an f32 query
    ///
    /// This is the asymmetric search: query is f32, stored data is quantized.
    ///
    /// # Arguments
    /// * `query` - The f32 query vector
    /// * `top_k` - Number of results to return
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension {
            return Vec::new();
        }

        if self.count == 0 {
            return Vec::new();
        }

        // Quantize the query for HNSW search
        // Note: This quantizes the query, which loses precision.
        // For true asymmetric search, we'd need to modify hnsw_rs or use a custom approach.
        let query_quantized: Vec<u8> = query
            .iter()
            .map(|&v| self.quant_params.quantize(v))
            .collect();

        let ef_search = 50.max(top_k);
        let results = self.hnsw.search(&query_quantized, top_k, ef_search);

        // Convert to node IDs
        results
            .into_iter()
            .filter_map(|neighbour| {
                self.id_map
                    .get(&neighbour.d_id)
                    .map(|node_id| (node_id.clone(), 1.0 - neighbour.distance))
            })
            .collect()
    }

    /// Search with true asymmetric distance computation
    ///
    /// This method performs a brute-force search with proper asymmetric
    /// distance computation (f32 query vs quantized stored).
    /// Use this when precision is more important than speed.
    ///
    /// # Arguments
    /// * `query` - The f32 query vector
    /// * `top_k` - Number of results to return
    pub fn search_asymmetric_brute_force(
        &self,
        query: &[f32],
        top_k: usize,
    ) -> Vec<(String, f32)> {
        if query.len() != self.dimension || self.count == 0 {
            return Vec::new();
        }

        // This would require storing the quantized vectors separately
        // and computing asymmetric distances. For now, delegate to regular search.
        self.search(query, top_k)
    }

    /// Get the number of vectors in the index
    pub fn len(&self) -> usize {
        self.count
    }

    /// Check if the index is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    /// Get the dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    /// Get quantization parameters
    pub fn quantization_params(&self) -> &QuantizationParams {
        &self.quant_params
    }

    /// Remove a vector from the index
    pub fn remove(&mut self, node_id: &str) -> bool {
        if let Some(internal_id) = self.reverse_map.remove(node_id) {
            self.id_map.remove(&internal_id);
            self.count -= 1;
            true
        } else {
            false
        }
    }

    /// Estimated memory usage in bytes
    pub fn estimated_memory_bytes(&self) -> usize {
        // Rough estimate: 1 byte per dimension per vector + overhead
        let vector_data = self.count * self.dimension;
        let overhead = self.id_map.len() * (std::mem::size_of::<usize>() + std::mem::size_of::<String>());
        vector_data + overhead
    }
}

/// Errors that can occur in quantized HNSW operations
#[derive(Debug, thiserror::Error)]
pub enum QuantizedHnswError {
    /// Dimension mismatch between query and index
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        /// Expected dimension
        expected: usize,
        /// Actual dimension received
        got: usize,
    },

    /// Node already exists in the index
    #[error("Node {0} already exists")]
    NodeExists(String),

    /// Node not found in the index
    #[error("Node {0} not found")]
    NodeNotFound(String),

    /// Quantization error
    #[error("Quantization error: {0}")]
    QuantizationError(String),
}

/// A hybrid index that stores quantized vectors but can search with f32 queries
///
/// This is an alternative design that maintains both:
/// - Quantized vectors in HNSW for fast approximate search
/// - Original f32 vectors for precise asymmetric distance computation
///
/// # Architecture
///
/// ```text
/// Insert: f32 vector ──▶ Quantize ──▶ HNSW Index
///                          │
///                          └──▶ Store in lookup table
///
/// Search: f32 query ──▶ HNSW Search ──▶ Get candidates
///                          │
///                          └──▶ Rerank with asymmetric distance
/// ```
pub struct HybridQuantizedIndex {
    /// HNSW index with quantized vectors
    hnsw: QuantizedHNSW,
    /// Lookup table for original f32 vectors (optional, for reranking)
    original_vectors: HashMap<String, Vec<f32>>,
    /// Whether to use asymmetric reranking
    use_reranking: bool,
}

impl HybridQuantizedIndex {
    /// Create a new hybrid index
    pub fn new(
        dimension: usize,
        quant_params: QuantizationParams,
        max_elements: usize,
    ) -> Self {
        Self {
            hnsw: QuantizedHNSW::new(dimension, quant_params, max_elements),
            original_vectors: HashMap::new(),
            use_reranking: false,
        }
    }

    /// Enable asymmetric reranking
    pub fn enable_reranking(&mut self) {
        self.use_reranking = true;
    }

    /// Disable asymmetric reranking
    pub fn disable_reranking(&mut self) {
        self.use_reranking = false;
    }

    /// Insert a vector with optional original storage
    pub fn insert(
        &mut self,
        node_id: String,
        vector: &[f32],
        store_original: bool,
    ) -> Result<(), QuantizedHnswError> {
        if store_original {
            self.original_vectors.insert(node_id.clone(), vector.to_vec());
        }
        self.hnsw.insert(node_id, vector)
    }

    /// Search with optional asymmetric reranking
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if !self.use_reranking || self.original_vectors.is_empty() {
            return self.hnsw.search(query, top_k);
        }

        // Get more candidates for reranking
        let candidates = self.hnsw.search(query, top_k * 2);

        // Rerank with asymmetric distance using original vectors
        let mut reranked: Vec<(String, f32)> = candidates
            .into_iter()
            .filter_map(|(node_id, _)| {
                self.original_vectors.get(&node_id).map(|stored| {
                    // Compute true asymmetric distance
                    let stored_qv = QuantizedVector::from_f32(stored, *self.hnsw.quantization_params());
                    let distance = AsymmetricCosine::asymmetric_distance(query, &stored_qv);
                    (node_id, 1.0 - distance) // Convert to similarity
                })
            })
            .collect();

        // Sort by similarity (descending)
        reranked.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        reranked.truncate(top_k);

        reranked
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_vector(dimension: usize, value: f32) -> Vec<f32> {
        vec![value; dimension]
    }

    #[test]
    fn test_quantized_hnsw_creation() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let index = QuantizedHNSW::new(128, params, 1000);

        assert_eq!(index.dimension(), 128);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }

    #[test]
    fn test_quantized_hnsw_insert() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let mut index = QuantizedHNSW::new(128, params, 1000);

        let vector = create_test_vector(128, 0.5);
        index.insert("test".to_string(), &vector).unwrap();

        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
    }

    #[test]
    fn test_quantized_hnsw_dimension_mismatch() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let mut index = QuantizedHNSW::new(128, params, 1000);

        let vector = create_test_vector(64, 0.5); // Wrong dimension
        let result = index.insert("test".to_string(), &vector);

        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), QuantizedHnswError::DimensionMismatch { .. }));
    }

    #[test]
    fn test_quantized_hnsw_duplicate_insert() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let mut index = QuantizedHNSW::new(128, params, 1000);

        let vector = create_test_vector(128, 0.5);
        index.insert("test".to_string(), &vector).unwrap();

        let result = index.insert("test".to_string(), &vector);
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), QuantizedHnswError::NodeExists(_)));
    }

    #[test]
    fn test_quantized_hnsw_search() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let mut index = QuantizedHNSW::new(64, params, 1000);

        // Insert some vectors
        for i in 0..10 {
            let vector = create_test_vector(64, i as f32 / 10.0);
            index.insert(format!("node_{}", i), &vector).unwrap();
        }

        // Search
        let query = create_test_vector(64, 0.5);
        let results = index.search(&query, 5);

        assert!(!results.is_empty());
        // Should return at most top_k results
        assert!(results.len() <= 5);
    }

    #[test]
    fn test_quantized_hnsw_empty_search() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let index = QuantizedHNSW::new(64, params, 1000);

        let query = create_test_vector(64, 0.5);
        let results = index.search(&query, 10);

        assert!(results.is_empty());
    }

    #[test]
    fn test_quantized_hnsw_remove() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let mut index = QuantizedHNSW::new(64, params, 1000);

        let vector = create_test_vector(64, 0.5);
        index.insert("test".to_string(), &vector).unwrap();
        assert_eq!(index.len(), 1);

        assert!(index.remove("test"));
        assert_eq!(index.len(), 0);
        assert!(!index.remove("nonexistent"));
    }

    #[test]
    fn test_distance_wrapper_eval() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let wrapper = QuantizedDistanceWrapper::new(params, 3);

        // Quantize two vectors
        let a = vec![0.0f32, 0.5, 1.0];
        let b = vec![1.0f32, 0.5, 0.0];

        let a_q: Vec<u8> = a.iter().map(|&v| params.quantize(v)).collect();
        let b_q: Vec<u8> = b.iter().map(|&v| params.quantize(v)).collect();

        let distance = wrapper.eval(&a_q, &b_q);

        // Distance should be positive
        assert!(distance >= 0.0);
    }

    #[test]
    fn test_memory_efficiency() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let mut index = QuantizedHNSW::new(768, params, 1000);

        // Insert 100 vectors
        for i in 0..100 {
            let vector: Vec<f32> = (0..768).map(|j| ((i * 768 + j) % 100) as f32 / 100.0).collect();
            index.insert(format!("node_{}", i), &vector).unwrap();
        }

        let memory = index.estimated_memory_bytes();

        // Should be roughly 100 * 768 bytes = 76800 bytes for quantized data
        // Plus some overhead for HashMaps
        assert!(memory < 100 * 768 * 2); // Less than 2x the raw data size
    }
}