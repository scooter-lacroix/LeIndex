//! INT8 Quantization for Vector Search
//!
//! This module provides INT8 quantization for efficient vector storage and search.
//! It uses asymmetric quantization where:
//! - Stored vectors are quantized to INT8 (1 byte per dimension)
//! - Query vectors remain as f32 for precision
//!
//! Key features:
//! - 4x memory reduction compared to f32 storage
//! - SIMD-optimized asymmetric distance computation
//! - hnsw_rs compatibility via custom Distance trait
//! - Zero-copy deserialization with bytemuck

pub mod simd;

use bytemuck::{Pod, Zeroable};
use serde::{Deserialize, Serialize};
use std::simd::{f32x16, i32x16, SimdFloat, SimdInt};

pub use simd::*;

/// Quantization parameters for a vector collection
///
/// These parameters are computed during quantization and are required
/// for dequantization and asymmetric distance computation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Zeroable, Pod)]
#[repr(C)]
pub struct QuantizationParams {
    /// Minimum value in the original data (for scaling)
    pub min: f32,
    /// Maximum value in the original data (for scaling)
    pub max: f32,
    /// Scale factor: (max - min) / 255.0
    pub scale: f32,
    /// Zero point: -min / scale
    pub zero_point: f32,
}

impl Default for QuantizationParams {
    fn default() -> Self {
        Self {
            min: 0.0,
            max: 1.0,
            scale: 1.0 / 255.0,
            zero_point: 0.0,
        }
    }
}

impl QuantizationParams {
    /// Create quantization parameters from min/max values
    pub fn from_min_max(min: f32, max: f32) -> Self {
        let scale = (max - min) / 255.0;
        let zero_point = if scale > 0.0 { -min / scale } else { 0.0 };

        Self {
            min,
            max,
            scale,
            zero_point,
        }
    }

    /// Compute parameters from a slice of vectors
    pub fn from_vectors(vectors: &[Vec<f32>]) -> Option<Self> {
        if vectors.is_empty() {
            return None;
        }

        let mut min = f32::INFINITY;
        let mut max = f32::NEG_INFINITY;

        for vec in vectors {
            for &val in vec {
                min = min.min(val);
                max = max.max(val);
            }
        }

        // Add small epsilon to avoid division by zero
        if max - min < 1e-6 {
            max = min + 1e-6;
        }

        Some(Self::from_min_max(min, max))
    }

    /// Quantize a single f32 value to u8
    #[inline]
    pub fn quantize(&self, value: f32) -> u8 {
        let scaled = (value - self.min) / self.scale;
        scaled.clamp(0.0, 255.0) as u8
    }

    /// Dequantize a u8 value back to f32
    #[inline]
    pub fn dequantize(&self, quantized: u8) -> f32 {
        self.min + (quantized as f32) * self.scale
    }
}

/// A quantized vector stored as INT8 values
///
/// This struct provides zero-copy access to quantized vector data
/// and implements the necessary traits for hnsw_rs compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedVector {
    /// Quantized data (1 byte per dimension)
    pub data: Vec<u8>,
    /// Quantization parameters for this vector
    pub params: QuantizationParams,
    /// Original dimension (before quantization)
    pub dimension: usize,
}

impl QuantizedVector {
    /// Create a new quantized vector from raw data
    pub fn new(data: Vec<u8>, params: QuantizationParams, dimension: usize) -> Self {
        Self {
            data,
            params,
            dimension,
        }
    }

    /// Quantize an f32 vector
    pub fn from_f32(vector: &[f32], params: QuantizationParams) -> Self {
        let data: Vec<u8> = vector.iter().map(|&v| params.quantize(v)).collect();
        Self {
            dimension: vector.len(),
            data,
            params,
        }
    }

    /// Dequantize back to f32
    pub fn to_f32(&self) -> Vec<f32> {
        self.data.iter().map(|&v| self.params.dequantize(v)).collect()
    }

    /// Get the quantized data as a slice
    #[inline]
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }

    /// Get the dimension
    #[inline]
    pub fn len(&self) -> usize {
        self.dimension
    }

    /// Check if empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.dimension == 0
    }

    /// Memory usage in bytes
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.data.capacity()
    }
}

/// Trait for computing distances between quantized and f32 vectors
///
/// This trait enables asymmetric distance computation where one vector
/// is quantized (u8) and the other is full precision (f32).
pub trait QuantizedDistance {
    /// Compute asymmetric distance: query (f32) vs stored (quantized u8)
    ///
    /// # Arguments
    /// * `query` - Query vector in f32 (full precision)
    /// * `stored` - Stored quantized vector
    ///
    /// # Returns
    /// Distance value (lower is more similar for L2, higher for cosine)
    fn asymmetric_distance(query: &[f32], stored: &QuantizedVector) -> f32;

    /// Compute batch asymmetric distances
    fn asymmetric_distance_batch(
        query: &[f32],
        stored: &[&QuantizedVector],
    ) -> Vec<f32> {
        stored
            .iter()
            .map(|&v| Self::asymmetric_distance(query, v))
            .collect()
    }
}

/// L2 (Euclidean) distance for asymmetric quantization
pub struct AsymmetricL2;

impl QuantizedDistance for AsymmetricL2 {
    fn asymmetric_distance(query: &[f32], stored: &QuantizedVector) -> f32 {
        assert_eq!(query.len(), stored.dimension);

        let params = &stored.params;
        let mut sum = 0.0;

        // Process in chunks for cache efficiency
        for (i, &q) in query.iter().enumerate() {
            let dequantized = params.dequantize(stored.data[i]);
            let diff = q - dequantized;
            sum += diff * diff;
        }

        sum.sqrt()
    }
}

/// Cosine similarity for asymmetric quantization
pub struct AsymmetricCosine;

impl QuantizedDistance for AsymmetricCosine {
    fn asymmetric_distance(query: &[f32], stored: &QuantizedVector) -> f32 {
        assert_eq!(query.len(), stored.dimension);

        let params = &stored.params;
        let mut dot_product = 0.0;
        let mut query_norm_sq = 0.0;
        let mut stored_norm_sq = 0.0;

        for (i, &q) in query.iter().enumerate() {
            let dequantized = params.dequantize(stored.data[i]);
            dot_product += q * dequantized;
            query_norm_sq += q * q;
            stored_norm_sq += dequantized * dequantized;
        }

        let query_norm = query_norm_sq.sqrt();
        let stored_norm = stored_norm_sq.sqrt();

        if query_norm == 0.0 || stored_norm == 0.0 {
            return 1.0; // Maximum distance for zero vectors
        }

        // Convert similarity to distance: distance = 1 - similarity
        let similarity = dot_product / (query_norm * stored_norm);
        (1.0 - similarity).max(0.0)
    }
}

/// Dot product distance for asymmetric quantization (assumes normalized vectors)
pub struct AsymmetricDot;

impl QuantizedDistance for AsymmetricDot {
    fn asymmetric_distance(query: &[f32], stored: &QuantizedVector) -> f32 {
        assert_eq!(query.len(), stored.dimension);

        let params = &stored.params;
        let mut dot_product = 0.0;

        for (i, &q) in query.iter().enumerate() {
            let dequantized = params.dequantize(stored.data[i]);
            dot_product += q * dequantized;
        }

        // For normalized vectors, distance = 1 - dot_product
        (1.0 - dot_product).max(0.0)
    }
}

/// SIMD-optimized L2 distance using AVX2
pub struct SimdL2;

impl QuantizedDistance for SimdL2 {
    fn asymmetric_distance(query: &[f32], stored: &QuantizedVector) -> f32 {
        assert_eq!(query.len(), stored.dimension);

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                return unsafe { asymmetric_l2_avx2(query, stored) };
            }
        }

        // Fallback to scalar implementation
        AsymmetricL2::asymmetric_distance(query, stored)
    }
}

/// SIMD-optimized Cosine distance using AVX2
pub struct SimdCosine;

impl QuantizedDistance for SimdCosine {
    fn asymmetric_distance(query: &[f32], stored: &QuantizedVector) -> f32 {
        assert_eq!(query.len(), stored.dimension);

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                return unsafe { asymmetric_cosine_avx2(query, stored) };
            }
        }

        // Fallback to scalar implementation
        AsymmetricCosine::asymmetric_distance(query, stored)
    }
}

/// SIMD-optimized Dot product using AVX2
pub struct SimdDot;

impl QuantizedDistance for SimdDot {
    fn asymmetric_distance(query: &[f32], stored: &QuantizedVector) -> f32 {
        assert_eq!(query.len(), stored.dimension);

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                return unsafe { asymmetric_dot_avx2(query, stored) };
            }
        }

        // Fallback to scalar implementation
        AsymmetricDot::asymmetric_distance(query, stored)
    }
}

/// Compute quantization error (RMSE) between original and quantized vectors
pub fn quantization_error(original: &[f32], quantized: &QuantizedVector) -> f32 {
    assert_eq!(original.len(), quantized.dimension);

    let dequantized = quantized.to_f32();
    let sum_sq_diff: f32 = original
        .iter()
        .zip(dequantized.iter())
        .map(|(&o, &d)| (o - d) * (o - d))
        .sum();

    (sum_sq_diff / original.len() as f32).sqrt()
}

/// Batch quantize multiple vectors with shared parameters
pub fn batch_quantize(vectors: &[Vec<f32>]) -> Option<(Vec<QuantizedVector>, QuantizationParams)> {
    let params = QuantizationParams::from_vectors(vectors)?;

    let quantized: Vec<QuantizedVector> = vectors
        .iter()
        .map(|v| QuantizedVector::from_f32(v, params))
        .collect();

    Some((quantized, params))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quantization_params() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        assert_eq!(params.min, 0.0);
        assert_eq!(params.max, 1.0);
        assert!((params.scale - 1.0 / 255.0).abs() < 1e-6);

        // Test quantization round-trip
        let original = 0.5;
        let quantized = params.quantize(original);
        let dequantized = params.dequantize(quantized);
        assert!((dequantized - original).abs() < 0.01);
    }

    #[test]
    fn test_quantization_from_vectors() {
        let vectors = vec![
            vec![0.0, 0.5, 1.0],
            vec![0.2, 0.7, 0.3],
        ];

        let params = QuantizationParams::from_vectors(&vectors).unwrap();
        assert_eq!(params.min, 0.0);
        assert_eq!(params.max, 1.0);
    }

    #[test]
    fn test_quantized_vector_roundtrip() {
        let params = QuantizationParams::from_min_max(-1.0, 1.0);
        let original = vec![-0.5, 0.0, 0.5, 0.9];

        let quantized = QuantizedVector::from_f32(&original, params);
        let dequantized = quantized.to_f32();

        assert_eq!(dequantized.len(), original.len());
        for (o, d) in original.iter().zip(dequantized.iter()) {
            // Quantization error should be small
            assert!((o - d).abs() < 0.01);
        }
    }

    #[test]
    fn test_asymmetric_l2_distance() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let stored = vec![0.5, 0.5, 0.5];
        let query = vec![0.6, 0.6, 0.6];

        let quantized = QuantizedVector::from_f32(&stored, params);
        let distance = AsymmetricL2::asymmetric_distance(&query, &quantized);

        // Distance should be positive
        assert!(distance > 0.0);

        // Compare with direct L2 distance
        let expected: f32 = stored
            .iter()
            .zip(query.iter())
            .map(|(s, q)| (s - q) * (s - q))
            .sum::<f32>()
            .sqrt();

        // Should be close (within quantization error)
        assert!((distance - expected).abs() < 0.1);
    }

    #[test]
    fn test_asymmetric_cosine_distance() {
        let params = QuantizationParams::from_min_max(-1.0, 1.0);
        let stored = vec![1.0, 0.0, 0.0];
        let query = vec![1.0, 0.0, 0.0];

        let quantized = QuantizedVector::from_f32(&stored, params);
        let distance = AsymmetricCosine::asymmetric_distance(&query, &quantized);

        // Identical vectors should have near-zero distance
        assert!(distance < 0.01);

        // Orthogonal vectors should have distance ~1.0
        let query_ortho = vec![0.0, 1.0, 0.0];
        let distance_ortho = AsymmetricCosine::asymmetric_distance(&query_ortho, &quantized);
        assert!((distance_ortho - 1.0).abs() < 0.1);
    }

    #[test]
    fn test_batch_quantize() {
        let vectors = vec![
            vec![0.0, 0.25, 0.5],
            vec![0.75, 1.0, 0.0],
        ];

        let (quantized, params) = batch_quantize(&vectors).unwrap();

        assert_eq!(quantized.len(), 2);
        assert_eq!(params.min, 0.0);
        assert_eq!(params.max, 1.0);
    }

    #[test]
    fn test_quantization_error() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let original = vec![0.1, 0.5, 0.9];

        let quantized = QuantizedVector::from_f32(&original, params);
        let error = quantization_error(&original, &quantized);

        // Error should be small for well-quantized vectors
        assert!(error < 0.01);
    }

    #[test]
    fn test_memory_efficiency() {
        let params = QuantizationParams::from_min_max(0.0, 1.0);
        let original = vec![0.5; 768]; // 768-dim vector

        let quantized = QuantizedVector::from_f32(&original, params);

        // Quantized should use ~1/4 the memory of f32
        let f32_bytes = original.len() * std::mem::size_of::<f32>();
        let quantized_bytes = quantized.data.len();

        assert!(quantized_bytes < f32_bytes / 3);
    }
}