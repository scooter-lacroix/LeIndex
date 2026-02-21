//! SIMD-Optimized Kernels for Asymmetric Distance Computation
//!
//! This module provides highly optimized SIMD implementations for computing
//! distances between f32 query vectors and INT8 quantized stored vectors.
//!
//! # Asymmetric Quantization Logic
//!
//! The key insight is that we dequantize on-the-fly during distance computation:
//! ```text
//! dequantized = min + (quantized as f32) * scale
//! distance = f(query, dequantized)
//! ```
//!
//! For L2 distance, we can expand:
//! ```text
//! (query - dequantized)^2 = (query - min - quantized*scale)^2
//! ```
//!
//! This allows SIMD operations to process 8-16 dimensions at once.

use super::{QuantizationParams, QuantizedVector};

/// Number of f32 values per AVX2 register (256 bits / 32 bits = 8)
const AVX2_F32_WIDTH: usize = 8;

/// Number of i32 values per AVX2 register (256 bits / 32 bits = 8)
const AVX2_I32_WIDTH: usize = 8;

/// Number of u8 values we can process (we convert to i32 for computation)
const AVX2_U8_CHUNK: usize = 8;

/// SIMD-optimized asymmetric L2 distance using AVX2
///
/// # Safety
/// Requires AVX2 and FMA support. Caller must verify CPU features.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn asymmetric_l2_avx2(query: &[f32], stored: &QuantizedVector) -> f32 {
    use std::arch::x86_64::*;

    let params = &stored.params;
    let n = query.len();

    // Broadcast scale and min to all lanes
    let scale_vec = _mm256_set1_ps(params.scale);
    let min_vec = _mm256_set1_ps(params.min);

    // Accumulator for sum of squared differences
    let mut sum_vec = _mm256_setzero_ps();

    let mut i = 0;
    // Process 8 dimensions at a time (AVX2 can hold 8 f32s)
    while i + AVX2_F32_WIDTH <= n {
        // Load 8 query values
        let query_vec = _mm256_loadu_ps(query.as_ptr().add(i));

        // Load 8 quantized values, convert to i32 then to f32
        // We load as 8 bytes, zero-extend to 8 i32s, then convert to 8 f32s
        let quantized_bytes = _mm_loadu_si64(stored.data.as_ptr().add(i) as *const _);
        let quantized_i32 = _mm256_cvtepu8_epi32(quantized_bytes);
        let quantized_f32 = _mm256_cvtepi32_ps(quantized_i32);

        // Dequantize: dequantized = min + quantized * scale
        let dequantized = _mm256_fmadd_ps(quantized_f32, scale_vec, min_vec);

        // Compute difference: query - dequantized
        let diff = _mm256_sub_ps(query_vec, dequantized);

        // Square and accumulate
        sum_vec = _mm256_fmadd_ps(diff, diff, sum_vec);

        i += AVX2_F32_WIDTH;
    }

    // Horizontal sum of the 8 lanes
    let mut sum = hsum256_ps_avx(sum_vec);

    // Handle remaining elements
    for j in i..n {
        let dequantized = params.dequantize(stored.data[j]);
        let diff = query[j] - dequantized;
        sum += diff * diff;
    }

    sum.sqrt()
}

/// SIMD-optimized asymmetric cosine distance using AVX2
///
/// Computes: distance = 1 - dot(query, dequantized) / (||query|| * ||dequantized||)
///
/// # Safety
/// Requires AVX2 and FMA support. Caller must verify CPU features.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn asymmetric_cosine_avx2(query: &[f32], stored: &QuantizedVector) -> f32 {
    use std::arch::x86_64::*;

    let params = &stored.params;
    let n = query.len();

    // Broadcast scale and min to all lanes
    let scale_vec = _mm256_set1_ps(params.scale);
    let min_vec = _mm256_set1_ps(params.min);

    // Accumulators
    let mut dot_vec = _mm256_setzero_ps();
    let mut query_norm_vec = _mm256_setzero_ps();
    let mut stored_norm_vec = _mm256_setzero_ps();

    let mut i = 0;
    while i + AVX2_F32_WIDTH <= n {
        // Load query values
        let query_vec = _mm256_loadu_ps(query.as_ptr().add(i));

        // Load and dequantize stored values
        let quantized_bytes = _mm_loadu_si64(stored.data.as_ptr().add(i) as *const _);
        let quantized_i32 = _mm256_cvtepu8_epi32(quantized_bytes);
        let quantized_f32 = _mm256_cvtepi32_ps(quantized_i32);
        let dequantized = _mm256_fmadd_ps(quantized_f32, scale_vec, min_vec);

        // Accumulate dot product
        dot_vec = _mm256_fmadd_ps(query_vec, dequantized, dot_vec);

        // Accumulate norms
        query_norm_vec = _mm256_fmadd_ps(query_vec, query_vec, query_norm_vec);
        stored_norm_vec = _mm256_fmadd_ps(dequantized, dequantized, stored_norm_vec);

        i += AVX2_F32_WIDTH;
    }

    // Horizontal sums
    let mut dot = hsum256_ps_avx(dot_vec);
    let mut query_norm_sq = hsum256_ps_avx(query_norm_vec);
    let mut stored_norm_sq = hsum256_ps_avx(stored_norm_vec);

    // Handle remaining elements
    for j in i..n {
        let dequantized = params.dequantize(stored.data[j]);
        dot += query[j] * dequantized;
        query_norm_sq += query[j] * query[j];
        stored_norm_sq += dequantized * dequantized;
    }

    let query_norm = query_norm_sq.sqrt();
    let stored_norm = stored_norm_sq.sqrt();

    if query_norm == 0.0 || stored_norm == 0.0 {
        return 1.0;
    }

    let similarity = dot / (query_norm * stored_norm);
    (1.0 - similarity).max(0.0)
}

/// SIMD-optimized asymmetric dot product using AVX2
///
/// Assumes vectors are normalized. Computes: distance = 1 - dot(query, dequantized)
///
/// # Safety
/// Requires AVX2 and FMA support. Caller must verify CPU features.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2,fma")]
pub unsafe fn asymmetric_dot_avx2(query: &[f32], stored: &QuantizedVector) -> f32 {
    use std::arch::x86_64::*;

    let params = &stored.params;
    let n = query.len();

    // Broadcast scale and min to all lanes
    let scale_vec = _mm256_set1_ps(params.scale);
    let min_vec = _mm256_set1_ps(params.min);

    // Accumulator for dot product
    let mut dot_vec = _mm256_setzero_ps();

    let mut i = 0;
    while i + AVX2_F32_WIDTH <= n {
        // Load query values
        let query_vec = _mm256_loadu_ps(query.as_ptr().add(i));

        // Load and dequantize stored values
        let quantized_bytes = _mm_loadu_si64(stored.data.as_ptr().add(i) as *const _);
        let quantized_i32 = _mm256_cvtepu8_epi32(quantized_bytes);
        let quantized_f32 = _mm256_cvtepi32_ps(quantized_i32);
        let dequantized = _mm256_fmadd_ps(quantized_f32, scale_vec, min_vec);

        // Accumulate dot product
        dot_vec = _mm256_fmadd_ps(query_vec, dequantized, dot_vec);

        i += AVX2_F32_WIDTH;
    }

    // Horizontal sum
    let mut dot = hsum256_ps_avx(dot_vec);

    // Handle remaining elements
    for j in i..n {
        let dequantized = params.dequantize(stored.data[j]);
        dot += query[j] * dequantized;
    }

    (1.0 - dot).max(0.0)
}

/// Horizontal sum of 8 f32 values in an AVX2 register
///
/// # Safety
/// Requires AVX2 support.
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn hsum256_ps_avx(v: std::arch::x86_64::__m256) -> f32 {
    use std::arch::x86_64::*;

    // Extract high and low 128-bit halves
    let high128 = _mm256_extractf128_ps(v, 1);
    let low128 = _mm256_castps256_ps128(v);

    // Add them together
    let sum128 = _mm_add_ps(low128, high128);

    // Now do horizontal sum of the 4 values
    let sum64 = _mm_add_ps(sum128, _mm_movehl_ps(sum128, sum128));
    let sum32 = _mm_add_ss(sum64, _mm_shuffle_ps(sum64, sum64, 0x55));

    _mm_cvtss_f32(sum32)
}

/// Portable SIMD implementation using std::simd (Rust 1.64+)
///
/// This provides a portable fallback that works on any platform
/// supporting the portable SIMD API.
#[cfg(feature = "portable_simd")]
pub mod portable {
    use super::*;
    use std::simd::{f32x16, i32x16, u8x16, SimdFloat, SimdInt, SimdPartialOrd};

    const LANES: usize = 16;

    /// Portable SIMD L2 distance
    pub fn asymmetric_l2_portable(query: &[f32], stored: &QuantizedVector) -> f32 {
        let params = &stored.params;
        let n = query.len();

        let scale_vec = f32x16::splat(params.scale);
        let min_vec = f32x16::splat(params.min);

        let mut sum_vec = f32x16::splat(0.0);

        let mut i = 0;
        while i + LANES <= n {
            // Load query
            let query_vec = f32x16::from_slice(&query[i..]);

            // Load quantized values as u8, convert to i32 then f32
            let mut quantized_bytes = [0u8; LANES];
            quantized_bytes.copy_from_slice(&stored.data[i..i + LANES]);
            let quantized_u8 = u8x16::from_array(quantized_bytes);

            // Zero-extend u8 to i32 (widen to 32-bit)
            let quantized_i32 = quantized_u8.cast::<i32>();
            let quantized_f32 = quantized_i32.cast::<f32>();

            // Dequantize
            let dequantized = quantized_f32 * scale_vec + min_vec;

            // Compute squared difference
            let diff = query_vec - dequantized;
            sum_vec += diff * diff;

            i += LANES;
        }

        let mut sum = sum_vec.reduce_sum();

        // Handle remaining elements
        for j in i..n {
            let dequantized = params.dequantize(stored.data[j]);
            let diff = query[j] - dequantized;
            sum += diff * diff;
        }

        sum.sqrt()
    }

    /// Portable SIMD cosine distance
    pub fn asymmetric_cosine_portable(query: &[f32], stored: &QuantizedVector) -> f32 {
        let params = &stored.params;
        let n = query.len();

        let scale_vec = f32x16::splat(params.scale);
        let min_vec = f32x16::splat(params.min);

        let mut dot_vec = f32x16::splat(0.0);
        let mut query_norm_vec = f32x16::splat(0.0);
        let mut stored_norm_vec = f32x16::splat(0.0);

        let mut i = 0;
        while i + LANES <= n {
            let query_vec = f32x16::from_slice(&query[i..]);

            let mut quantized_bytes = [0u8; LANES];
            quantized_bytes.copy_from_slice(&stored.data[i..i + LANES]);
            let quantized_u8 = u8x16::from_array(quantized_bytes);

            let quantized_i32 = quantized_u8.cast::<i32>();
            let quantized_f32 = quantized_i32.cast::<f32>();
            let dequantized = quantized_f32 * scale_vec + min_vec;

            dot_vec += query_vec * dequantized;
            query_norm_vec += query_vec * query_vec;
            stored_norm_vec += dequantized * dequantized;

            i += LANES;
        }

        let mut dot = dot_vec.reduce_sum();
        let mut query_norm_sq = query_norm_vec.reduce_sum();
        let mut stored_norm_sq = stored_norm_vec.reduce_sum();

        for j in i..n {
            let dequantized = params.dequantize(stored.data[j]);
            dot += query[j] * dequantized;
            query_norm_sq += query[j] * query[j];
            stored_norm_sq += dequantized * dequantized;
        }

        let query_norm = query_norm_sq.sqrt();
        let stored_norm = stored_norm_sq.sqrt();

        if query_norm == 0.0 || stored_norm == 0.0 {
            return 1.0;
        }

        let similarity = dot / (query_norm * stored_norm);
        (1.0 - similarity).max(0.0)
    }

    /// Portable SIMD dot product
    pub fn asymmetric_dot_portable(query: &[f32], stored: &QuantizedVector) -> f32 {
        let params = &stored.params;
        let n = query.len();

        let scale_vec = f32x16::splat(params.scale);
        let min_vec = f32x16::splat(params.min);

        let mut dot_vec = f32x16::splat(0.0);

        let mut i = 0;
        while i + LANES <= n {
            let query_vec = f32x16::from_slice(&query[i..]);

            let mut quantized_bytes = [0u8; LANES];
            quantized_bytes.copy_from_slice(&stored.data[i..i + LANES]);
            let quantized_u8 = u8x16::from_array(quantized_bytes);

            let quantized_i32 = quantized_u8.cast::<i32>();
            let quantized_f32 = quantized_i32.cast::<f32>();
            let dequantized = quantized_f32 * scale_vec + min_vec;

            dot_vec += query_vec * dequantized;

            i += LANES;
        }

        let mut dot = dot_vec.reduce_sum();

        for j in i..n {
            let dequantized = params.dequantize(stored.data[j]);
            dot += query[j] * dequantized;
        }

        (1.0 - dot).max(0.0)
    }
}

/// Auto-vectorizing scalar implementation
///
/// This version is written to allow the compiler to auto-vectorize
/// and serves as a fallback when SIMD intrinsics aren't available.
pub mod scalar {
    use super::*;

    /// Scalar L2 distance with compiler hints for vectorization
    #[inline]
    pub fn asymmetric_l2_scalar(query: &[f32], stored: &QuantizedVector) -> f32 {
        let params = &stored.params;
        let n = query.len();

        // Process in chunks that fit in cache lines
        const CHUNK_SIZE: usize = 64;

        let mut sum = 0.0f32;

        for chunk_start in (0..n).step_by(CHUNK_SIZE) {
            let chunk_end = (chunk_start + CHUNK_SIZE).min(n);

            // The compiler can vectorize this inner loop
            #[allow(clippy::needless_range_loop)]
            for i in chunk_start..chunk_end {
                let dequantized = params.dequantize(stored.data[i]);
                let diff = query[i] - dequantized;
                sum += diff * diff;
            }
        }

        sum.sqrt()
    }

    /// Scalar cosine distance with compiler hints for vectorization
    #[inline]
    pub fn asymmetric_cosine_scalar(query: &[f32], stored: &QuantizedVector) -> f32 {
        let params = &stored.params;
        let n = query.len();

        let mut dot = 0.0f32;
        let mut query_norm_sq = 0.0f32;
        let mut stored_norm_sq = 0.0f32;

        for i in 0..n {
            let dequantized = params.dequantize(stored.data[i]);
            dot += query[i] * dequantized;
            query_norm_sq += query[i] * query[i];
            stored_norm_sq += dequantized * dequantized;
        }

        let query_norm = query_norm_sq.sqrt();
        let stored_norm = stored_norm_sq.sqrt();

        if query_norm == 0.0 || stored_norm == 0.0 {
            return 1.0;
        }

        let similarity = dot / (query_norm * stored_norm);
        (1.0 - similarity).max(0.0)
    }

    /// Scalar dot product with compiler hints for vectorization
    #[inline]
    pub fn asymmetric_dot_scalar(query: &[f32], stored: &QuantizedVector) -> f32 {
        let params = &stored.params;
        let n = query.len();

        let mut dot = 0.0f32;

        for i in 0..n {
            let dequantized = params.dequantize(stored.data[i]);
            dot += query[i] * dequantized;
        }

        (1.0 - dot).max(0.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{AsymmetricL2, AsymmetricCosine, QuantizationParams, QuantizedVector};

    fn create_test_vector(dimension: usize) -> (Vec<f32>, QuantizedVector) {
        let params = QuantizationParams::from_min_max(-1.0, 1.0);
        let original: Vec<f32> = (0..dimension)
            .map(|i| ((i as f32) / dimension as f32) * 2.0 - 1.0)
            .collect();
        let quantized = QuantizedVector::from_f32(&original, params);
        (original, quantized)
    }

    #[test]
    fn test_simd_l2_matches_scalar() {
        let (query, stored) = create_test_vector(256);

        let scalar_dist = AsymmetricL2::asymmetric_distance(&query, &stored);

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                let simd_dist = unsafe { asymmetric_l2_avx2(&query, &stored) };
                assert!((simd_dist - scalar_dist).abs() < 1e-3,
                    "SIMD L2 distance {} doesn't match scalar {}", simd_dist, scalar_dist);
            }
        }
    }

    #[test]
    fn test_simd_cosine_matches_scalar() {
        let (query, stored) = create_test_vector(256);

        let scalar_dist = AsymmetricCosine::asymmetric_distance(&query, &stored);

        #[cfg(target_arch = "x86_64")]
        {
            if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                let simd_dist = unsafe { asymmetric_cosine_avx2(&query, &stored) };
                assert!((simd_dist - scalar_dist).abs() < 1e-3,
                    "SIMD cosine distance {} doesn't match scalar {}", simd_dist, scalar_dist);
            }
        }
    }

    #[test]
    fn test_simd_with_various_dimensions() {
        for dim in [8, 16, 32, 64, 128, 256, 512, 768, 1024] {
            let (query, stored) = create_test_vector(dim);

            let scalar_l2 = AsymmetricL2::asymmetric_distance(&query, &stored);
            let scalar_cos = AsymmetricCosine::asymmetric_distance(&query, &stored);

            #[cfg(target_arch = "x86_64")]
            {
                if is_x86_feature_detected!("avx2") && is_x86_feature_detected!("fma") {
                    let simd_l2 = unsafe { asymmetric_l2_avx2(&query, &stored) };
                    let simd_cos = unsafe { asymmetric_cosine_avx2(&query, &stored) };

                    assert!((simd_l2 - scalar_l2).abs() < 1e-3,
                        "Dimension {}: SIMD L2 {} != scalar {}", dim, simd_l2, scalar_l2);
                    assert!((simd_cos - scalar_cos).abs() < 1e-3,
                        "Dimension {}: SIMD cosine {} != scalar {}", dim, simd_cos, scalar_cos);
                }
            }
        }
    }

    #[test]
    fn test_scalar_l2() {
        let (query, stored) = create_test_vector(128);
        let dist = scalar::asymmetric_l2_scalar(&query, &stored);
        assert!(dist >= 0.0);
    }

    #[test]
    fn test_scalar_cosine() {
        let (query, stored) = create_test_vector(128);
        let dist = scalar::asymmetric_cosine_scalar(&query, &stored);
        assert!(dist >= 0.0 && dist <= 1.0);
    }
}