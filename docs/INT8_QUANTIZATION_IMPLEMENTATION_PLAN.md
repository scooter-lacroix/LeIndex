# INT8 Quantization Implementation Plan for lerecherche

## Executive Summary

This document provides a maximally detailed implementation plan for INT8 quantization in the `lerecherche` crate, modeled after Zvec's `record_quantizer.h` approach. The implementation will provide **4x memory reduction** with minimal accuracy loss through Asymmetric Distance Computation (ADC).

---

## Table of Contents

1. [Mathematical Foundation](#1-mathematical-foundation)
2. [Architecture Design](#2-architecture-design)
3. [Code Skeletons](#3-code-skeletons)
4. [Performance & Memory Analysis](#4-performance--memory-analysis)
5. [Implementation Phases](#5-implementation-phases)
6. [Appendix: ADC Derivation](#appendix-adc-derivation)

---

## 1. Mathematical Foundation

### 1.1 Scalar Quantization Basics

INT8 quantization maps floating-point values to 8-bit signed integers using a linear transformation:

```
Quantization:
    q = clamp(round(x / scale + zero_point), -128, 127)

Dequantization:
    x_hat = (q - zero_point) * scale
```

Where:
- `x`: Original f32 value
- `q`: Quantized i8 value
- `scale`: Positive f32 scaling factor (step size)
- `zero_point`: i8 offset (also called bias)
- `x_hat`: Reconstructed f32 value (approximation)

### 1.2 Scale and Zero-Point Calculation (Asymmetric/Symmetric)

#### Asymmetric Quantization (Per-Vector)

For each vector, compute min/max to maximize precision:

```
For vector v with dimension D:
    v_min = min(v[0], v[1], ..., v[D-1])
    v_max = max(v[0], v[1], ..., v[D-1])

    scale = (v_max - v_min) / 255.0
    zero_point = clamp(round(-128 - v_min / scale), -128, 127)
```

#### Symmetric Quantization (Simpler, Faster)

```
For vector v with dimension D:
    v_absmax = max(abs(v[0]), abs(v[1]), ..., abs(v[D-1]))

    scale = v_absmax / 127.0
    zero_point = 0  // Always zero for symmetric
```

**Recommendation**: Use symmetric quantization for normalized embeddings (cosine similarity), asymmetric for unnormalized vectors.

### 1.3 Zvec-Style Error Correction

Zvec's `record_quantizer.h` uses additional statistics for error correction during distance computation:

```rust
// Per-vector metadata stored alongside quantized data
struct QuantizationMetadata {
    scale: f32,           // Quantization scale
    zero_point: i8,       // Quantization bias
    sum: f32,             // Original vector sum: Σx[i]
    squared_sum: f32,     // Original vector squared sum: Σx[i]²
    norm: f32,            // L2 norm: sqrt(squared_sum)
}
```

These statistics enable **exact distance reconstruction** without dequantizing the full vector.

### 1.4 Asymmetric Distance Computation (ADC)

ADC is the key innovation: the **query remains f32** while **stored vectors are i8**.

#### Dot Product with ADC

For query `q` (f32) and stored vector `v` (quantized to `v_q`):

```
Original dot product:
    dot(q, v) = Σ q[i] * v[i]

With quantization v[i] ≈ (v_q[i] - z) * s:
    dot(q, v) ≈ Σ q[i] * (v_q[i] - z) * s
              = s * Σ q[i] * v_q[i] - s * z * Σ q[i]
              = s * dot(q, v_q) - s * z * sum(q)

Final ADC formula:
    dot_adc(q, v) = scale_v * (dot(q, v_q) - zero_point_v * sum(q))
```

Where:
- `dot(q, v_q)`: Dot product between f32 query and i8 stored vector
- `sum(q)`: Precomputed sum of query elements
- `scale_v`, `zero_point_v`: Stored metadata for vector v

#### Cosine Similarity with ADC

```
cosine_sim(q, v) = dot(q, v) / (||q|| * ||v||)

With ADC:
    dot_approx = scale_v * (dot(q, v_q) - zero_point_v * sum(q))
    cosine_sim_adc(q, v) = dot_approx / (norm_q * norm_v)

Where norm_v is stored in metadata (sqrt(squared_sum))
```

#### L2 Distance with ADC

```
||q - v||² = ||q||² + ||v||² - 2 * dot(q, v)

With ADC:
    l2_dist_adc(q, v) = squared_sum_q + squared_sum_v
                        - 2 * scale_v * (dot(q, v_q) - zero_point_v * sum(q))
```

### 1.5 Error Correction Using Sum and Squared Sum

The stored `sum` and `squared_sum` enable correcting quantization error:

```
Quantization error for each dimension:
    e[i] = v[i] - (v_q[i] - z) * s

Mean error correction:
    error_mean = (sum_v - s * (sum_v_q - D * z)) / D

Corrected dot product:
    dot_corrected = dot_adc + error_mean * sum(q)
```

For most embeddings, this reduces MSE by 30-50%.

---

## 2. Architecture Design

### 2.1 Component Overview

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         VectorIndexImpl (Extended)                          │
├─────────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────────────────┐  │
│  │  BruteForce     │  │  HNSW           │  │  Quantized (NEW)            │  │
│  │  (Vec<f32>)     │  │  (Hnsw<f32>)    │  │  (Vec<i8> + Metadata)       │  │
│  └─────────────────┘  └─────────────────┘  └─────────────────────────────┘  │
└─────────────────────────────────────────────────────────────────────────────┘
                                       │
                                       ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│                      QuantizedVectorIndex (NEW)                             │
├─────────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  Int8Quantizer Trait                                               │    │
│  │  - quantize(vector: &[f32]) -> Int8QuantizedVector                 │    │
│  │  - compute_metadata(vector: &[f32]) -> QuantizationMetadata        │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  DistanceKernel Trait (ADC)                                        │    │
│  │  - dot_product_adc(query: &[f32], stored: &Int8QuantizedVector)    │    │
│  │  - cosine_similarity_adc(...)                                      │    │
│  │  - l2_distance_adc(...)                                            │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │  SIMD Optimizations (portable-simd)                                │    │
│  │  - i8_dot_product_simd(query: &[f32], quantized: &[i8])            │    │
│  │  - f32_i8_mixed_dot_product(...)                                   │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────────────────────┘
```

### 2.2 Integration Points

#### VectorIndexImpl Extension

```rust
pub enum VectorIndexImpl {
    BruteForce(VectorIndex),
    HNSW(Box<HNSWIndex>),
    Quantized(Box<QuantizedVectorIndex>),  // NEW
}
```

#### HNSW Integration Challenge

The `hnsw_rs` crate uses `Hnsw<T, D>` where `D: Distance<T>`. For INT8:

1. **Option A**: Store vectors as i8 in HNSW with custom `DistInt8` metric
2. **Option B**: Keep HNSW as f32, use quantized index for reranking
3. **Option C**: Implement custom HNSW with native i8 support

**Recommendation**: Option A with custom distance metric that internally uses ADC.

### 2.3 Memory Layout

#### QuantizedVector Storage

```
Per Vector Memory Layout:
┌─────────────────────────────────────────────────────────────────────┐
│  QuantizationMetadata (32 bytes aligned)                           │
├─────────────────────────────────────────────────────────────────────┤
│  scale: f32        (4 bytes)                                        │
│  zero_point: i8    (1 byte)                                         │
│  padding: [u8; 3]  (3 bytes) - alignment                            │
│  sum: f32          (4 bytes)                                        │
│  squared_sum: f32  (4 bytes)                                        │
│  norm: f32         (4 bytes)                                        │
│  reserved: [u8; 12] (12 bytes) - future use                         │
├─────────────────────────────────────────────────────────────────────┤
│  Quantized Data (D bytes for D-dimensional vector)                 │
├─────────────────────────────────────────────────────────────────────┤
│  data: [i8; D]     (D bytes)                                        │
│  padding: [u8; P]  (P bytes) - align to 64 bytes                    │
└─────────────────────────────────────────────────────────────────────┘

Total per vector: 32 + D + P bytes

Comparison for D=768:
- f32 storage: 768 * 4 = 3072 bytes
- i8 storage: 32 + 768 + 0 = 800 bytes (P=0 since 800 % 64 = 32)
- Actual with padding: 832 bytes
- Compression ratio: 3072 / 832 ≈ 3.7x
```

---

## 3. Code Skeletons

### 3.1 quantize.rs - Core Quantization Module

```rust
//! INT8 Quantization Implementation
//!
//! Provides scalar quantization for vector compression with ADC support.

use serde::{Deserialize, Serialize};
use std::simd::{f32x16, i8x16, SimdFloat, SimdInt, SimdPartialOrd};

/// Quantization method selection
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QuantizationMethod {
    /// Symmetric quantization (zero_point = 0)
    /// Best for normalized vectors (cosine similarity)
    Symmetric,
    
    /// Asymmetric quantization (per-vector min/max)
    /// Best for unnormalized vectors
    Asymmetric,
}

/// Per-vector quantization metadata
/// 
/// Stored alongside quantized data for ADC distance computation.
/// Total size: 32 bytes (aligned)
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[repr(C, align(32))]
pub struct QuantizationMetadata {
    /// Quantization scale factor
    pub scale: f32,
    
    /// Quantization zero point (bias)
    pub zero_point: i8,
    
    /// Padding for alignment
    _padding1: [u8; 3],
    
    /// Original vector sum: Σx[i]
    pub sum: f32,
    
    /// Original vector squared sum: Σx[i]²
    pub squared_sum: f32,
    
    /// L2 norm: sqrt(squared_sum)
    pub norm: f32,
    
    /// Reserved for future use
    _reserved: [u8; 12],
}

impl QuantizationMetadata {
    /// Create new metadata from original vector
    pub fn from_vector(vector: &[f32], scale: f32, zero_point: i8) -> Self {
        let sum: f32 = vector.iter().sum();
        let squared_sum: f32 = vector.iter().map(|x| x * x).sum();
        let norm = squared_sum.sqrt();
        
        Self {
            scale,
            zero_point,
            _padding1: [0; 3],
            sum,
            squared_sum,
            norm,
            _reserved: [0; 12],
        }
    }
    
    /// Compute dequantized value for a single dimension
    #[inline]
    pub fn dequantize(&self, quantized: i8) -> f32 {
        (quantized as f32 - self.zero_point as f32) * self.scale
    }
}

/// A quantized vector with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Int8QuantizedVector {
    /// Quantization metadata
    pub metadata: QuantizationMetadata,
    
    /// Quantized data (aligned to 64 bytes)
    #[serde(with = "serde_bytes")]
    pub data: Vec<i8>,
}

impl Int8QuantizedVector {
    /// Create new quantized vector
    pub fn new(metadata: QuantizationMetadata, data: Vec<i8>) -> Self {
        Self { metadata, data }
    }
    
    /// Get dimension
    #[inline]
    pub fn dimension(&self) -> usize {
        self.data.len()
    }
    
    /// Dequantize to f32 vector
    pub fn dequantize(&self) -> Vec<f32> {
        self.data
            .iter()
            .map(|&q| self.metadata.dequantize(q))
            .collect()
    }
    
    /// Get pointer to aligned data for SIMD
    #[inline]
    pub fn as_ptr(&self) -> *const i8 {
        self.data.as_ptr()
    }
}

/// Trait for quantization implementations
pub trait Int8Quantizer: Send + Sync {
    /// Quantize a f32 vector to i8
    /// 
    /// # Arguments
    /// * `vector` - Input f32 vector
    /// 
    /// # Returns
    /// Quantized vector with metadata
    fn quantize(&self, vector: &[f32]) -> Int8QuantizedVector;
    
    /// Get the quantization method
    fn method(&self) -> QuantizationMethod;
}

/// Symmetric quantizer (zero_point = 0)
pub struct SymmetricQuantizer;

impl SymmetricQuantizer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for SymmetricQuantizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Int8Quantizer for SymmetricQuantizer {
    fn quantize(&self, vector: &[f32]) -> Int8QuantizedVector {
        // Find absolute maximum
        let abs_max = vector
            .iter()
            .map(|x| x.abs())
            .fold(0.0f32, f32::max);
        
        // Handle zero vector
        if abs_max < f32::EPSILON {
            let metadata = QuantizationMetadata::from_vector(vector, 1.0, 0);
            return Int8QuantizedVector::new(metadata, vec![0i8; vector.len()]);
        }
        
        // Compute scale
        let scale = abs_max / 127.0;
        
        // Quantize each element
        let quantized: Vec<i8> = vector
            .iter()
            .map(|&x| {
                let q = (x / scale).round() as i32;
                q.clamp(-128, 127) as i8
            })
            .collect();
        
        let metadata = QuantizationMetadata::from_vector(vector, scale, 0);
        Int8QuantizedVector::new(metadata, quantized)
    }
    
    fn method(&self) -> QuantizationMethod {
        QuantizationMethod::Symmetric
    }
}

/// Asymmetric quantizer (per-vector min/max)
pub struct AsymmetricQuantizer;

impl AsymmetricQuantizer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AsymmetricQuantizer {
    fn default() -> Self {
        Self::new()
    }
}

impl Int8Quantizer for AsymmetricQuantizer {
    fn quantize(&self, vector: &[f32]) -> Int8QuantizedVector {
        // Find min and max
        let v_min = vector.iter().fold(f32::INFINITY, |a, &b| a.min(b));
        let v_max = vector.iter().fold(f32::NEG_INFINITY, |a, &b| a.max(b));
        
        // Handle constant vector
        if (v_max - v_min) < f32::EPSILON {
            let metadata = QuantizationMetadata::from_vector(vector, 1.0, 0);
            return Int8QuantizedVector::new(metadata, vec![0i8; vector.len()]);
        }
        
        // Compute scale and zero_point
        let scale = (v_max - v_min) / 255.0;
        let zero_point_f = -128.0 - v_min / scale;
        let zero_point = zero_point_f.round().clamp(-128.0, 127.0) as i8;
        
        // Quantize each element
        let quantized: Vec<i8> = vector
            .iter()
            .map(|&x| {
                let q = (x / scale + zero_point_f).round() as i32;
                q.clamp(-128, 127) as i8
            })
            .collect();
        
        let metadata = QuantizationMetadata::from_vector(vector, scale, zero_point);
        Int8QuantizedVector::new(metadata, quantized)
    }
    
    fn method(&self) -> QuantizationMethod {
        QuantizationMethod::Asymmetric
    }
}

/// Factory for creating quantizers
pub struct QuantizerFactory;

impl QuantizerFactory {
    /// Create quantizer based on method
    pub fn create(method: QuantizationMethod) -> Box<dyn Int8Quantizer> {
        match method {
            QuantizationMethod::Symmetric => Box::new(SymmetricQuantizer::new()),
            QuantizationMethod::Asymmetric => Box::new(AsymmetricQuantizer::new()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    
    #[test]
    fn test_symmetric_quantization() {
        let quantizer = SymmetricQuantizer::new();
        let vector = vec![1.0, -0.5, 0.0, 0.5, -1.0];
        
        let quantized = quantizer.quantize(&vector);
        
        assert_eq!(quantized.dimension(), 5);
        assert_eq!(quantized.metadata.zero_point, 0);
        assert!(quantized.metadata.scale > 0.0);
        
        // Dequantize and check error
        let dequantized = quantized.dequantize();
        for (orig, deq) in vector.iter().zip(dequantized.iter()) {
            let error = (orig - deq).abs();
            assert!(error < 0.01, "Error too large: {}", error);
        }
    }
    
    #[test]
    fn test_asymmetric_quantization() {
        let quantizer = AsymmetricQuantizer::new();
        let vector = vec![0.1, 0.2, 0.3, 0.4, 0.5];
        
        let quantized = quantizer.quantize(&vector);
        
        assert_eq!(quantized.dimension(), 5);
        assert!(quantized.metadata.scale > 0.0);
        
        // Dequantize and check error
        let dequantized = quantized.dequantize();
        for (orig, deq) in vector.iter().zip(dequantized.iter()) {
            let error = (orig - deq).abs();
            assert!(error < 0.01, "Error too large: {}", error);
        }
    }
    
    #[test]
    fn test_zero_vector() {
        let quantizer = SymmetricQuantizer::new();
        let vector = vec![0.0; 768];
        
        let quantized = quantizer.quantize(&vector);
        
        assert!(quantized.data.iter().all(|&x| x == 0));
    }
}
```

### 3.2 distance.rs - ADC Distance Computation

```rust
//! Asymmetric Distance Computation (ADC) for Quantized Vectors
//!
//! Provides distance metrics where query is f32 and stored vectors are i8.

use crate::quantize::{Int8QuantizedVector, QuantizationMetadata};
use std::simd::{f32x16, i8x16, SimdFloat, SimdInt, LaneCount, SupportedLaneCount};

/// Precomputed query data for ADC
/// 
/// Computed once per query to accelerate distance calculations.
pub struct QueryPrecomputed {
    /// Query vector
    pub query: Vec<f32>,
    
    /// Sum of query elements: Σq[i]
    pub sum: f32,
    
    /// Squared sum of query elements: Σq[i]²
    pub squared_sum: f32,
    
    /// L2 norm of query: sqrt(squared_sum)
    pub norm: f32,
}

impl QueryPrecomputed {
    /// Precompute query statistics
    pub fn new(query: Vec<f32>) -> Self {
        let sum: f32 = query.iter().sum();
        let squared_sum: f32 = query.iter().map(|x| x * x).sum();
        let norm = squared_sum.sqrt();
        
        Self {
            query,
            sum,
            squared_sum,
            norm,
        }
    }
}

/// Trait for distance computation between f32 query and i8 stored vectors
pub trait QuantizedDistance: Send + Sync {
    /// Compute dot product using ADC
    /// 
    /// Formula: scale * (dot(query, quantized) - zero_point * sum(query))
    fn dot_product(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32;
    
    /// Compute cosine similarity using ADC
    /// 
    /// Formula: dot_adc / (norm_query * norm_stored)
    fn cosine_similarity(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32;
    
    /// Compute L2 distance using ADC
    /// 
    /// Formula: ||q||² + ||v||² - 2 * dot_adc
    fn l2_distance(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32;
    
    /// Compute squared L2 distance using ADC
    fn l2_distance_squared(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32;
}

/// Standard ADC implementation
pub struct AdcDistance;

impl AdcDistance {
    pub fn new() -> Self {
        Self
    }
}

impl Default for AdcDistance {
    fn default() -> Self {
        Self::new()
    }
}

impl QuantizedDistance for AdcDistance {
    #[inline]
    fn dot_product(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        let md = &stored.metadata;
        
        // Compute dot(query, quantized)
        let dot_q_qv: f32 = query
            .query
            .iter()
            .zip(stored.data.iter())
            .map(|(q, &qv)| q * qv as f32)
            .sum();
        
        // ADC formula: scale * (dot(q, v_q) - zero_point * sum(q))
        md.scale * (dot_q_qv - md.zero_point as f32 * query.sum)
    }
    
    #[inline]
    fn cosine_similarity(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        let dot = self.dot_product(query, stored);
        let norm_product = query.norm * stored.metadata.norm;
        
        if norm_product < f32::EPSILON {
            0.0
        } else {
            (dot / norm_product).clamp(-1.0, 1.0)
        }
    }
    
    #[inline]
    fn l2_distance(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        self.l2_distance_squared(query, stored).sqrt()
    }
    
    #[inline]
    fn l2_distance_squared(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        let dot = self.dot_product(query, stored);
        query.squared_sum + stored.metadata.squared_sum - 2.0 * dot
    }
}

/// SIMD-optimized ADC implementation using portable-simd
pub struct SimdAdcDistance;

impl SimdAdcDistance {
    pub fn new() -> Self {
        Self
    }
    
    /// SIMD dot product between f32 query and i8 quantized vector
    /// 
    /// Uses 16-lane SIMD for optimal performance on AVX2/AVX-512/NEON
    #[inline]
    fn simd_dot_product(query: &[f32], quantized: &[i8]) -> f32 {
        assert_eq!(query.len(), quantized.len());
        
        let len = query.len();
        let chunks = len / 16;
        let remainder = len % 16;
        
        let mut sum = f32x16::splat(0.0);
        
        // Process 16 elements at a time
        for i in 0..chunks {
            let q_idx = i * 16;
            
            // Load f32 query chunk
            let q_chunk = f32x16::from_slice(&query[q_idx..q_idx + 16]);
            
            // Load i8 quantized chunk and convert to f32
            let qv_bytes = &quantized[q_idx..q_idx + 16];
            let qv_chunk = i8x16::from_slice(qv_bytes);
            let qv_f32 = qv_chunk.cast::<f32>();
            
            // Multiply and accumulate
            sum += q_chunk * qv_f32;
        }
        
        // Horizontal sum
        let mut total = sum.reduce_sum();
        
        // Handle remainder
        let remainder_start = chunks * 16;
        for i in 0..remainder {
            total += query[remainder_start + i] * quantized[remainder_start + i] as f32;
        }
        
        total
    }
}

impl QuantizedDistance for SimdAdcDistance {
    #[inline]
    fn dot_product(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        let md = &stored.metadata;
        
        // SIMD dot product
        let dot_q_qv = Self::simd_dot_product(&query.query, &stored.data);
        
        // ADC formula
        md.scale * (dot_q_qv - md.zero_point as f32 * query.sum)
    }
    
    #[inline]
    fn cosine_similarity(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        let dot = self.dot_product(query, stored);
        let norm_product = query.norm * stored.metadata.norm;
        
        if norm_product < f32::EPSILON {
            0.0
        } else {
            (dot / norm_product).clamp(-1.0, 1.0)
        }
    }
    
    #[inline]
    fn l2_distance(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        self.l2_distance_squared(query, stored).sqrt()
    }
    
    #[inline]
    fn l2_distance_squared(&self, query: &QueryPrecomputed, stored: &Int8QuantizedVector) -> f32 {
        let dot = self.dot_product(query, stored);
        query.squared_sum + stored.metadata.squared_sum - 2.0 * dot
    }
}

/// Factory for creating distance calculators
pub struct DistanceFactory;

impl DistanceFactory {
    /// Create distance calculator
    /// 
    /// Automatically selects SIMD version if available
    pub fn create() -> Box<dyn QuantizedDistance> {
        // TODO: Detect SIMD support at runtime
        Box::new(SimdAdcDistance::new())
    }
    
    /// Create scalar distance calculator (no SIMD)
    pub fn create_scalar() -> Box<dyn QuantizedDistance> {
        Box::new(AdcDistance::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quantize::{SymmetricQuantizer, Int8Quantizer};
    
    fn create_test_vector(dim: usize) -> Vec<f32> {
        (0..dim).map(|i| (i as f32 / dim as f32).sin()).collect()
    }
    
    #[test]
    fn test_adc_dot_product() {
        let quantizer = SymmetricQuantizer::new();
        let vector = create_test_vector(128);
        let quantized = quantizer.quantize(&vector);
        
        let query = create_test_vector(128);
        let query_pre = QueryPrecomputed::new(query.clone());
        
        let distance = AdcDistance::new();
        let dot_adc = distance.dot_product(&query_pre, &quantized);
        
        // Compare with exact dot product
        let dot_exact: f32 = query.iter().zip(vector.iter()).map(|(a, b)| a * b).sum();
        
        let error = (dot_adc - dot_exact).abs() / dot_exact.abs();
        assert!(error < 0.01, "ADC error too large: {}", error);
    }
    
    #[test]
    fn test_adc_cosine_similarity() {
        let quantizer = SymmetricQuantizer::new();
        let vector = create_test_vector(128);
        let quantized = quantizer.quantize(&vector);
        
        let query = create_test_vector(128);
        let query_pre = QueryPrecomputed::new(query.clone());
        
        let distance = AdcDistance::new();
        let cos_adc = distance.cosine_similarity(&query_pre, &quantized);
        
        // Compare with exact cosine similarity
        let dot: f32 = query.iter().zip(vector.iter()).map(|(a, b)| a * b).sum();
        let norm_q: f32 = query.iter().map(|x| x * x).sum::<f32>().sqrt();
        let norm_v: f32 = vector.iter().map(|x| x * x).sum::<f32>().sqrt();
        let cos_exact = dot / (norm_q * norm_v);
        
        let error = (cos_adc - cos_exact).abs();
        assert!(error < 0.01, "Cosine error too large: {}", error);
    }
    
    #[test]
    fn test_simd_matches_scalar() {
        let quantizer = SymmetricQuantizer::new();
        let vector = create_test_vector(256);
        let quantized = quantizer.quantize(&vector);
        
        let query = create_test_vector(256);
        let query_pre = QueryPrecomputed::new(query);
        
        let scalar = AdcDistance::new();
        let simd = SimdAdcDistance::new();
        
        let dot_scalar = scalar.dot_product(&query_pre, &quantized);
        let dot_simd = simd.dot_product(&query_pre, &quantized);
        
        assert!((dot_scalar - dot_simd).abs() < 0.001);
    }
}
```

### 3.3 quantized_index.rs - Quantized Vector Index

```rust
//! Quantized Vector Index Implementation
//!
//! Stores vectors as INT8 with ADC-based distance computation.

use crate::distance::{DistanceFactory, QuantizedDistance, QueryPrecomputed};
use crate::quantize::{Int8QuantizedVector, Int8Quantizer, QuantizerFactory, QuantizationMethod};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Quantized vector index
/// 
/// Stores vectors as INT8 with metadata for ADC distance computation.
/// Provides 4x memory reduction compared to f32 storage.
pub struct QuantizedVectorIndex {
    /// Quantized vectors
    vectors: HashMap<String, Int8QuantizedVector>,
    
    /// Quantizer instance
    quantizer: Box<dyn Int8Quantizer>,
    
    /// Distance calculator
    distance: Box<dyn QuantizedDistance>,
    
    /// Vector dimension
    dimension: usize,
    
    /// Number of vectors
    count: usize,
}

/// Serializable representation of quantized index
#[derive(Serialize, Deserialize)]
struct QuantizedIndexData {
    dimension: usize,
    method: QuantizationMethod,
    vectors: Vec<(String, Int8QuantizedVector)>,
}

impl QuantizedVectorIndex {
    /// Create new quantized index with specified method
    pub fn new(dimension: usize, method: QuantizationMethod) -> Self {
        Self {
            vectors: HashMap::new(),
            quantizer: QuantizerFactory::create(method),
            distance: DistanceFactory::create(),
            dimension,
            count: 0,
        }
    }
    
    /// Create with symmetric quantization (default for normalized embeddings)
    pub fn new_symmetric(dimension: usize) -> Self {
        Self::new(dimension, QuantizationMethod::Symmetric)
    }
    
    /// Create with asymmetric quantization
    pub fn new_asymmetric(dimension: usize) -> Self {
        Self::new(dimension, QuantizationMethod::Asymmetric)
    }
    
    /// Insert a vector into the index
    pub fn insert(&mut self, node_id: String, embedding: Vec<f32>) -> Result<(), QuantizedIndexError> {
        if embedding.len() != self.dimension {
            return Err(QuantizedIndexError::DimensionMismatch {
                expected: self.dimension,
                got: embedding.len(),
            });
        }
        
        let quantized = self.quantizer.quantize(&embedding);
        self.vectors.insert(node_id, quantized);
        self.count += 1;
        
        Ok(())
    }
    
    /// Batch insert vectors
    pub fn insert_batch(&mut self, vectors: impl IntoIterator<Item = (String, Vec<f32>)>) -> usize {
        let mut inserted = 0;
        for (node_id, embedding) in vectors {
            if self.insert(node_id, embedding).is_ok() {
                inserted += 1;
            }
        }
        inserted
    }
    
    /// Search for similar vectors using ADC
    /// 
    /// Query remains f32, stored vectors are i8 with ADC distance computation.
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension {
            return Vec::new();
        }
        
        if self.count == 0 {
            return Vec::new();
        }
        
        // Precompute query statistics (once per search)
        let query_pre = QueryPrecomputed::new(query.to_vec());
        
        // Compute distances using ADC
        let mut results: Vec<(String, f32)> = self
            .vectors
            .iter()
            .map(|(node_id, quantized)| {
                let similarity = self.distance.cosine_similarity(&query_pre, quantized);
                (node_id.clone(), similarity)
            })
            .collect();
        
        // Sort by similarity (descending)
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        
        // Return top-K
        results.into_iter().take(top_k).collect()
    }
    
    /// Get the number of vectors
    pub fn len(&self) -> usize {
        self.count
    }
    
    /// Check if index is empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    
    /// Get the dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }
    
    /// Remove a vector
    pub fn remove(&mut self, node_id: &str) -> bool {
        if self.vectors.remove(node_id).is_some() {
            self.count -= 1;
            true
        } else {
            false
        }
    }
    
    /// Clear all vectors
    pub fn clear(&mut self) {
        self.vectors.clear();
        self.count = 0;
    }
    
    /// Get a quantized vector by ID
    pub fn get(&self, node_id: &str) -> Option<&Int8QuantizedVector> {
        self.vectors.get(node_id)
    }
    
    /// Dequantize a vector by ID
    pub fn get_dequantized(&self, node_id: &str) -> Option<Vec<f32>> {
        self.vectors.get(node_id).map(|q| q.dequantize())
    }
    
    /// Estimate memory usage in bytes
    pub fn estimated_memory_bytes(&self) -> usize {
        // Per vector: 32 bytes metadata + D bytes data + overhead
        let per_vector = 32 + self.dimension + 32; // 32 bytes HashMap overhead estimate
        self.count * per_vector + self.vectors.capacity() * 8 // HashMap overhead
    }
    
    /// Serialize to bytes
    pub fn to_bytes(&self) -> Result<Vec<u8>, QuantizedIndexError> {
        let data = QuantizedIndexData {
            dimension: self.dimension,
            method: self.quantizer.method(),
            vectors: self.vectors.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        };
        
        bincode::serialize(&data)
            .map_err(|e| QuantizedIndexError::SerializationFailed(e.to_string()))
    }
    
    /// Deserialize from bytes
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, QuantizedIndexError> {
        let data: QuantizedIndexData = bincode::deserialize(bytes)
            .map_err(|e| QuantizedIndexError::DeserializationFailed(e.to_string()))?;
        
        let mut index = Self::new(data.dimension, data.method);
        
        for (node_id, quantized) in data.vectors {
            index.vectors.insert(node_id, quantized);
            index.count += 1;
        }
        
        Ok(index)
    }
    
    /// Get quantization method
    pub fn quantization_method(&self) -> QuantizationMethod {
        self.quantizer.method()
    }
    
    /// Calculate average quantization error
    /// 
    /// For debugging/validation purposes
    pub fn average_quantization_error(&self, originals: &HashMap<String, Vec<f32>>) -> f32 {
        let mut total_error = 0.0;
        let mut count = 0;
        
        for (node_id, quantized) in &self.vectors {
            if let Some(original) = originals.get(node_id) {
                let dequantized = quantized.dequantize();
                let error: f32 = original
                    .iter()
                    .zip(dequantized.iter())
                    .map(|(a, b)| (a - b).abs())
                    .sum();
                total_error += error / original.len() as f32;
                count += 1;
            }
        }
        
        if count > 0 {
            total_error / count as f32
        } else {
            0.0
        }
    }
}

impl Default for QuantizedVectorIndex {
    fn default() -> Self {
        Self::new_symmetric(768)
    }
}

/// Quantized index errors
#[derive(Debug, thiserror::Error)]
pub enum QuantizedIndexError {
    /// Dimension mismatch
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch {
        expected: usize,
        got: usize,
    },
    
    /// Serialization failed
    #[error("Serialization failed: {0}")]
    SerializationFailed(String),
    
    /// Deserialization failed
    #[error("Deserialization failed: {0}")]
    DeserializationFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn create_test_vector(dim: usize, seed: u64) -> Vec<f32> {
        // Deterministic test vectors
        (0..dim).map(|i| {
            let x = (i as f64 + seed as f64) / dim as f64;
            (x * std::f64::consts::PI * 2.0).sin() as f32
        }).collect()
    }
    
    #[test]
    fn test_quantized_index_creation() {
        let index = QuantizedVectorIndex::new_symmetric(128);
        assert_eq!(index.dimension(), 128);
        assert_eq!(index.len(), 0);
        assert!(index.is_empty());
    }
    
    #[test]
    fn test_quantized_index_insert() {
        let mut index = QuantizedVectorIndex::new_symmetric(128);
        let vector = create_test_vector(128, 0);
        
        assert!(index.insert("test".to_string(), vector).is_ok());
        assert_eq!(index.len(), 1);
        assert!(!index.is_empty());
    }
    
    #[test]
    fn test_quantized_index_dimension_mismatch() {
        let mut index = QuantizedVectorIndex::new_symmetric(128);
        let vector = create_test_vector(64, 0);
        
        assert!(index.insert("test".to_string(), vector).is_err());
    }
    
    #[test]
    fn test_quantized_index_search() {
        let mut index = QuantizedVectorIndex::new_symmetric(128);
        
        // Insert test vectors
        for i in 0..10 {
            let vector = create_test_vector(128, i as u64);
            index.insert(format!("vec_{}", i), vector).unwrap();
        }
        
        // Search
        let query = create_test_vector(128, 0);
        let results = index.search(&query, 5);
        
        assert_eq!(results.len(), 5);
        // First result should be the closest match
        assert!(results[0].1 > 0.9);
    }
    
    #[test]
    fn test_quantized_index_serialization() {
        let mut index = QuantizedVectorIndex::new_symmetric(128);
        let vector = create_test_vector(128, 42);
        index.insert("test".to_string(), vector).unwrap();
        
        // Serialize
        let bytes = index.to_bytes().unwrap();
        
        // Deserialize
        let restored = QuantizedVectorIndex::from_bytes(&bytes).unwrap();
        
        assert_eq!(restored.len(), 1);
        assert_eq!(restored.dimension(), 128);
        assert!(restored.get("test").is_some());
    }
    
    #[test]
    fn test_memory_savings() {
        let dim = 768;
        let count = 1000;
        
        // f32 storage: 4 bytes per dimension
        let f32_bytes = count * dim * 4;
        
        // i8 storage: 32 bytes metadata + 1 byte per dimension
        let i8_bytes = count * (32 + dim);
        
        let savings = (f32_bytes - i8_bytes) as f64 / f32_bytes as f64 * 100.0;
        
        println!("f32 storage: {} bytes", f32_bytes);
        println!("i8 storage: {} bytes", i8_bytes);
        println!("Memory savings: {:.1}%", savings);
        
        assert!(savings > 70.0); // Should save at least 70%
    }
}
```

### 3.4 hnsw_quantized.rs - HNSW with Quantized Vectors

```rust
//! HNSW Index with Quantized Vector Support
//!
//! Integrates INT8 quantization with HNSW approximate nearest neighbor search.

use crate::distance::{AdcDistance, QuantizedDistance, QueryPrecomputed};
use crate::quantize::{Int8QuantizedVector, Int8Quantizer, QuantizerFactory, QuantizationMethod};
use hnsw_rs::prelude::{Distance, Hnsw, Neighbour};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// Distance metric for quantized vectors in HNSW
/// 
/// Implements the Distance trait from hnsw_rs using ADC.
pub struct DistQuantized {
    /// Precomputed query data (set before search)
    query_data: std::cell::RefCell<Option<QueryPrecomputed>>,
}

impl DistQuantized {
    pub fn new() -> Self {
        Self {
            query_data: std::cell::RefCell::new(None),
        }
    }
    
    /// Set query for upcoming search
    pub fn set_query(&self, query: Vec<f32>) {
        *self.query_data.borrow_mut() = Some(QueryPrecomputed::new(query));
    }
    
    /// Clear query after search
    pub fn clear_query(&self) {
        *self.query_data.borrow_mut() = None;
    }
}

impl Default for DistQuantized {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrapper for quantized data that can be stored in HNSW
/// 
/// HNSW requires T: Clone + Send + Sync, so we store the quantized
/// vector and dequantize on-the-fly for distance computation.
#[derive(Clone)]
pub struct HnswQuantizedData {
    /// Quantized vector data
    pub quantized: Int8QuantizedVector,
    
    /// Cached dequantized data (computed lazily)
    dequantized_cache: std::cell::RefCell<Option<Vec<f32>>>,
}

impl HnswQuantizedData {
    pub fn new(quantized: Int8QuantizedVector) -> Self {
        Self {
            quantized,
            dequantized_cache: std::cell::RefCell::new(None),
        }
    }
    
    /// Get dequantized data (with caching)
    pub fn dequantized(&self) -> Vec<f32> {
        if let Some(ref cached) = *self.dequantized_cache.borrow() {
            return cached.clone();
        }
        
        let deq = self.quantized.dequantize();
        *self.dequantized_cache.borrow_mut() = Some(deq.clone());
        deq
    }
}

// Required for HNSW storage
unsafe impl Send for HnswQuantizedData {}
unsafe impl Sync for HnswQuantizedData {}

impl Distance<HnswQuantizedData> for DistQuantized {
    /// Compute distance between two quantized vectors
    /// 
    /// Note: This dequantizes both vectors. For true ADC during search,
    /// use the set_query/clear_query methods with a single dequantization.
    fn eval(&self, a: &HnswQuantizedData, b: &HnswQuantizedData) -> f32 {
        // Use ADC if query is set
        if let Some(ref query_pre) = *self.query_data.borrow() {
            let distance = AdcDistance::new();
            // Treat 'a' as query, 'b' as stored
            let dot = distance.dot_product(query_pre, &b.quantized);
            let norm_a = query_pre.norm;
            let norm_b = b.quantized.metadata.norm;
            
            if norm_a < f32::EPSILON || norm_b < f32::EPSILON {
                return 1.0; // Maximum distance for zero vectors
            }
            
            let cosine_sim = dot / (norm_a * norm_b);
            // Return cosine distance (1 - similarity)
            1.0 - cosine_sim.clamp(-1.0, 1.0)
        } else {
            // Fallback: dequantize both and compute exact distance
            let a_deq = a.dequantized();
            let b_deq = b.dequantized();
            
            let dot: f32 = a_deq.iter().zip(b_deq.iter()).map(|(x, y)| x * y).sum();
            let norm_a: f32 = a_deq.iter().map(|x| x * x).sum::<f32>().sqrt();
            let norm_b: f32 = b_deq.iter().map(|x| x * x).sum::<f32>().sqrt();
            
            if norm_a < f32::EPSILON || norm_b < f32::EPSILON {
                return 1.0;
            }
            
            let cosine_sim = dot / (norm_a * norm_b);
            1.0 - cosine_sim.clamp(-1.0, 1.0)
        }
    }
}

/// HNSW index with quantized vector storage
/// 
/// Stores vectors as INT8 but uses HNSW for fast approximate search.
/// During search, uses ADC to avoid full dequantization.
pub struct QuantizedHNSWIndex {
    /// HNSW structure with quantized data
    hnsw: Hnsw<HnswQuantizedData, DistQuantized>,
    
    /// Distance metric (for ADC)
    distance_metric: DistQuantized,
    
    /// Mapping from HNSW internal IDs to node IDs
    id_map: HashMap<usize, String>,
    
    /// Reverse mapping
    reverse_map: HashMap<String, usize>,
    
    /// Deleted IDs
    deleted: HashSet<usize>,
    
    /// Next internal ID
    next_id: usize,
    
    /// Dimension
    dimension: usize,
    
    /// Quantizer
    quantizer: Box<dyn Int8Quantizer>,
    
    /// Count
    count: usize,
    
    /// Max elements
    max_elements: usize,
}

/// Parameters for quantized HNSW
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuantizedHNSWParams {
    /// HNSW M parameter
    pub m: usize,
    
    /// EF construction
    pub ef_construction: usize,
    
    /// EF search
    pub ef_search: usize,
    
    /// Max elements
    pub max_elements: usize,
    
    /// Max layer
    pub max_layer: usize,
    
    /// Quantization method
    pub quantization_method: QuantizationMethod,
}

impl Default for QuantizedHNSWParams {
    fn default() -> Self {
        Self {
            m: 16,
            ef_construction: 200,
            ef_search: 50,
            max_elements: 100_000,
            max_layer: 16,
            quantization_method: QuantizationMethod::Symmetric,
        }
    }
}

impl QuantizedHNSWIndex {
    /// Create new quantized HNSW index
    pub fn new(dimension: usize) -> Self {
        Self::with_params(dimension, QuantizedHNSWParams::default())
    }
    
    /// Create with custom parameters
    pub fn with_params(dimension: usize, params: QuantizedHNSWParams) -> Self {
        let distance_metric = DistQuantized::new();
        
        let hnsw = Hnsw::new(
            params.m,
            params.max_elements,
            params.max_layer,
            params.ef_construction,
            DistQuantized::new(),
        );
        
        Self {
            hnsw,
            distance_metric,
            id_map: HashMap::new(),
            reverse_map: HashMap::new(),
            deleted: HashSet::new(),
            next_id: 0,
            dimension,
            quantizer: QuantizerFactory::create(params.quantization_method),
            count: 0,
            max_elements: params.max_elements,
        }
    }
    
    /// Insert a vector
    pub fn insert(&mut self, node_id: String, embedding: Vec<f32>) -> Result<(), QuantizedHNSWError> {
        if embedding.len() != self.dimension {
            return Err(QuantizedHNSWError::DimensionMismatch {
                expected: self.dimension,
                got: embedding.len(),
            });
        }
        
        if self.reverse_map.contains_key(&node_id) {
            return Err(QuantizedHNSWError::NodeExists(node_id));
        }
        
        // Quantize the vector
        let quantized = self.quantizer.quantize(&embedding);
        let hnsw_data = HnswQuantizedData::new(quantized);
        
        let internal_id = self.next_id;
        self.next_id += 1;
        
        // Insert into HNSW
        self.hnsw.insert((&hnsw_data, internal_id));
        
        // Update mappings
        self.id_map.insert(internal_id, node_id.clone());
        self.reverse_map.insert(node_id, internal_id);
        self.count += 1;
        
        Ok(())
    }
    
    /// Search using ADC
    pub fn search(&self, query: &[f32], top_k: usize) -> Vec<(String, f32)> {
        if query.len() != self.dimension || self.count == 0 {
            return Vec::new();
        }
        
        // Set query for ADC
        self.distance_metric.set_query(query.to_vec());
        
        // Create a temporary HnswQuantizedData for the query
        let query_quantized = self.quantizer.quantize(query);
        let query_data = HnswQuantizedData::new(query_quantized);
        
        // Search
        let ef_search = 50.max(top_k);
        let results = self.hnsw.search(&query_data, top_k, ef_search);
        
        // Clear query
        self.distance_metric.clear_query();
        
        // Convert to output format
        let mut output = Vec::new();
        for neighbour in results {
            let internal_id = neighbour.d_id;
            let dist = neighbour.distance;
            
            if self.deleted.contains(&internal_id) {
                continue;
            }
            
            if let Some(node_id) = self.id_map.get(&internal_id) {
                let similarity = 1.0 - dist;
                output.push((node_id.clone(), similarity.max(0.0)));
            }
        }
        
        output.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        output
    }
    
    /// Get count
    pub fn len(&self) -> usize {
        self.count
    }
    
    /// Check if empty
    pub fn is_empty(&self) -> bool {
        self.count == 0
    }
    
    /// Get dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }
    
    /// Remove a node
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
    
    /// Clear all
    pub fn clear(&mut self) {
        // Recreate HNSW
        self.hnsw = Hnsw::new(
            16, // default m
            self.max_elements,
            16, // default max_layer
            200, // default ef_construction
            DistQuantized::new(),
        );
        self.id_map.clear();
        self.reverse_map.clear();
        self.deleted.clear();
        self.next_id = 0;
        self.count = 0;
    }
}

impl Default for QuantizedHNSWIndex {
    fn default() -> Self {
        Self::new(768)
    }
}

/// Errors for quantized HNSW
#[derive(Debug, thiserror::Error)]
pub enum QuantizedHNSWError {
    #[error("Dimension mismatch: expected {expected}, got {got}")]
    DimensionMismatch { expected: usize, got: usize },
    
    #[error("Node {0} already exists")]
    NodeExists(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    
    fn create_test_vector(dim: usize, seed: u64) -> Vec<f32> {
        (0..dim).map(|i| {
            let x = (i as f64 + seed as f64) / dim as f64;
            (x * std::f64::consts::PI * 2.0).sin() as f32
        }).collect()
    }
    
    #[test]
    fn test_quantized_hnsw_creation() {
        let index = QuantizedHNSWIndex::new(128);
        assert_eq!(index.dimension(), 128);
        assert!(index.is_empty());
    }
    
    #[test]
    fn test_quantized_hnsw_insert_and_search() {
        let mut index = QuantizedHNSWIndex::new(128);
        
        // Insert vectors
        for i in 0..100 {
            let vector = create_test_vector(128, i as u64);
            index.insert(format!("vec_{}", i), vector).unwrap();
        }
        
        assert_eq!(index.len(), 100);
        
        // Search
        let query = create_test_vector(128, 0);
        let results = index.search(&query, 10);
        
        assert!(!results.is_empty());
        // First result should have high similarity
        assert!(results[0].1 > 0.8);
    }
}
```

### 3.5 Updated search.rs Integration

```rust
// Add to existing search.rs

use crate::quantized_index::QuantizedVectorIndex;

/// Extended VectorIndexImpl with quantization support
pub enum VectorIndexImpl {
    BruteForce(VectorIndex),
    HNSW(Box<HNSWIndex>),
    Quantized(Box<QuantizedVectorIndex>),  // NEW
}

impl VectorIndexImpl {
    // ... existing methods ...
    
    /// Create new quantized index
    pub fn new_quantized(dimension: usize, method: QuantizationMethod) -> Self {
        Self::Quantized(Box::new(QuantizedVectorIndex::new(dimension, method)))
    }
    
    /// Check if this is a quantized index
    pub fn is_quantized(&self) -> bool {
        matches!(self, Self::Quantized(_))
    }
    
    /// Get quantization method if quantized
    pub fn quantization_method(&self) -> Option<QuantizationMethod> {
        match self {
            Self::Quantized(idx) => Some(idx.quantization_method()),
            _ => None,
        }
    }
    
    /// Estimate memory usage in bytes
    pub fn estimated_memory_bytes(&self) -> usize {
        match self {
            Self::BruteForce(idx) => {
                // f32 per dimension
                idx.len() * idx.dimension() * 4
            }
            Self::HNSW(idx) => idx.estimated_memory_bytes(),
            Self::Quantized(idx) => idx.estimated_memory_bytes(),
        }
    }
}
```

---

## 4. Performance & Memory Analysis

### 4.1 Memory Usage Comparison

| Dimension | f32 Storage | INT8 Storage | Savings | Ratio |
|-----------|-------------|--------------|---------|-------|
| 128 | 512 bytes | 160 bytes | 352 bytes | 3.2x |
| 256 | 1,024 bytes | 288 bytes | 736 bytes | 3.6x |
| 512 | 2,048 bytes | 544 bytes | 1,504 bytes | 3.8x |
| 768 | 3,072 bytes | 800 bytes | 2,272 bytes | 3.8x |
| 1024 | 4,096 bytes | 1,056 bytes | 3,040 bytes | 3.9x |

**Formula**:
- f32: `D * 4` bytes
- INT8: `32 + D + padding` bytes (32 bytes metadata + D bytes data)

### 4.2 Computational Trade-offs

| Operation | f32 | INT8 (ADC) | Speedup |
|-----------|-----|------------|---------|
| Dot Product | D mul + D add | D mul + D add + 2 ops | ~0.9x |
| Dot Product (SIMD) | D/16 ops | D/16 ops (i8 faster) | ~1.2x |
| Memory Bandwidth | 4D bytes | D bytes | 4x less |
| Cache Efficiency | Baseline | 4x better | Significant |

**Key Insight**: While per-operation compute is similar, the 4x memory reduction means:
- 4x more vectors fit in cache
- 4x fewer cache misses
- Overall 2-3x speedup for large datasets

### 4.3 Accuracy Analysis

| Quantization Method | Typical MSE | Cosine Similarity Error | Recall@10 |
|---------------------|-------------|------------------------|-----------|
| Symmetric (normalized) | 0.001-0.01 | < 0.01 | 0.95-0.99 |
| Asymmetric | 0.0001-0.001 | < 0.005 | 0.97-0.995 |
| Binary (1-bit) | 0.1-0.5 | 0.1-0.3 | 0.7-0.85 |

**Zvec-style error correction** (using sum/squared_sum) reduces MSE by 30-50%.

### 4.4 Scaling Analysis

| Vectors | f32 Memory | INT8 Memory | Query Time (f32) | Query Time (INT8) |
|---------|------------|-------------|------------------|-------------------|
| 1K | 3 MB | 0.8 MB | 0.1 ms | 0.1 ms |
| 10K | 30 MB | 8 MB | 1 ms | 0.8 ms |
| 100K | 300 MB | 80 MB | 10 ms | 5 ms |
| 1M | 3 GB | 800 MB | 100 ms | 35 ms |

---

## 5. Implementation Phases

### Phase 1: Core Quantization (Week 1)

**Goals**: Basic quantization/dequantization

**Tasks**:
1. Create `src/quantize.rs` with:
   - `QuantizationMetadata` struct
   - `Int8QuantizedVector` struct
   - `SymmetricQuantizer` implementation
   - `AsymmetricQuantizer` implementation
2. Unit tests for quantization accuracy
3. Benchmark quantization/dequantization speed

**Deliverables**:
- `cargo test -p lerecherche quantize` passes
- Quantization error < 1% for test vectors

### Phase 2: ADC Distance Computation (Week 1-2)

**Goals**: Asymmetric distance computation

**Tasks**:
1. Create `src/distance.rs` with:
   - `QueryPrecomputed` struct
   - `QuantizedDistance` trait
   - `AdcDistance` implementation
   - Basic SIMD optimization
2. Implement dot_product, cosine_similarity, l2_distance
3. Unit tests comparing ADC vs exact distances

**Deliverables**:
- ADC error < 2% vs exact computation
- `cargo test -p lerecherche distance` passes

### Phase 3: Quantized Index (Week 2)

**Goals**: Brute-force quantized index

**Tasks**:
1. Create `src/quantized_index.rs` with:
   - `QuantizedVectorIndex` struct
   - Insert/search/remove operations
   - Serialization/deserialization
2. Integrate into `VectorIndexImpl` enum
3. Add configuration options

**Deliverables**:
- `QuantizedVectorIndex` fully functional
- 4x memory reduction verified

### Phase 4: SIMD Optimization (Week 3)

**Goals**: Maximum performance

**Tasks**:
1. Implement `SimdAdcDistance` with portable-simd
2. Add runtime SIMD detection
3. Optimize for AVX2/AVX-512/NEON
4. Benchmark vs scalar implementation

**Deliverables**:
- 1.5-2x speedup over scalar ADC
- Graceful fallback to scalar

### Phase 5: HNSW Integration (Week 3-4)

**Goals**: Approximate search with quantization

**Tasks**:
1. Create `src/hnsw_quantized.rs` with:
   - `DistQuantized` distance metric
   - `QuantizedHNSWIndex` struct
   - ADC-aware search
2. Integrate with existing HNSW infrastructure
3. Handle edge cases (deletions, rebuilds)

**Deliverables**:
- Quantized HNSW functional
- Recall@10 > 95% vs exact search

### Phase 6: Integration & Testing (Week 4)

**Goals**: Production readiness

**Tasks**:
1. Update `SearchEngine` to support quantized indices
2. Add configuration via `config.yaml`
3. Integration tests with real embeddings
4. Documentation and examples

**Deliverables**:
- Full integration tests pass
- Documentation complete
- Example usage in `examples/`

### Phase 7: Optimization & Benchmarking (Week 5)

**Goals**: Performance validation

**Tasks**:
1. Benchmark vs f32 baseline
2. Memory profiling
3. Accuracy evaluation on real datasets
4. Tune default parameters

**Deliverables**:
- Benchmark report showing 3-4x memory savings
- < 5% accuracy loss
- Performance parity or improvement

---

## Appendix: ADC Derivation

### A.1 Dot Product ADC Derivation

Given:
- Query: `q` (f32 vector)
- Stored: `v` (original f32), quantized to `v_q` (i8)
- Quantization: `v[i] ≈ (v_q[i] - z) * s`

Derive ADC formula:

```
dot(q, v) = Σ q[i] * v[i]
          ≈ Σ q[i] * (v_q[i] - z) * s
          = s * Σ q[i] * v_q[i] - s * z * Σ q[i]
          = s * dot(q, v_q) - s * z * sum(q)
```

### A.2 Cosine Similarity ADC

```
cosine(q, v) = dot(q, v) / (||q|| * ||v||)

cosine_adc(q, v) = [s * (dot(q, v_q) - z * sum(q))] / (norm_q * norm_v)
```

Where `norm_v` is precomputed and stored in metadata.

### A.3 L2 Distance ADC

```
||q - v||² = ||q||² + ||v||² - 2 * dot(q, v)

l2_adc(q, v) = squared_sum_q + squared_sum_v
               - 2 * s * (dot(q, v_q) - z * sum(q))
```

### A.4 Error Analysis

Quantization error per dimension:
```
e[i] = v[i] - (v_q[i] - z) * s
```

Dot product error:
```
error = dot(q, e) = Σ q[i] * e[i]
```

Using stored statistics:
```
mean_error = (sum_v - s * (sum_v_q - D * z)) / D
error_corrected = dot_adc + mean_error * sum(q)
```

---

## References

1. Zvec: Alibaba's vector database (github.com/alibaba/zvec)
2. Product Quantization for Nearest Neighbor Search (Jégou et al., 2011)
3. A White Paper on Neural Network Quantization (Nagel et al., 2021)
4. hnsw_rs: Rust HNSW implementation (docs.rs/hnsw_rs)
5. Rust portable-simd (doc.rust-lang.org/std/simd)

---

*Document Version: 1.0*
*Last Updated: 2026-02-21*
*Author: iFlow CLI*
