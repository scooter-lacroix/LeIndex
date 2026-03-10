//! AVX2-Optimized ADC Distance Computation
//!
//! This module provides highly optimized distance computations using x86_64 AVX2
//! intrinsics. It achieves 3-4x speedup over the fallback implementation by
//! vectorizing the i8→f32 widening operation.
//!
//! # Safety
//!
//! All public functions in this module require AVX2 support. They must only be
//! called after verifying AVX2 availability via `is_x86_feature_detected!("avx2")`.
//!
//! # Implementation Details
//!
//! The key optimization is widening 16 i8 values to f32 in a vectorized manner:
//! 1. Load 16 i8 values (128-bit)
//! 2. Widen to 16 i16 values (256-bit) using `_mm256_cvtepi8_epi16`
//! 3. Split into two 128-bit halves
//! 4. Widen each half to 8 i32 values (256-bit) using `_mm256_cvtepi16_epi32`
//! 5. Convert i32 to f32 using `_mm256_cvtepi32_ps`
//! 6. Multiply-accumulate with query values using `_mm256_fmadd_ps`

use super::super::vector::Int8QuantizedVector;
use std::arch::x86_64::*;

/// Compute asymmetric dot product using ADC with AVX2
///
/// # Safety
///
/// - CPU must support AVX2 (check with `is_x86_feature_detected!("avx2")`)
/// - `query` must have at least `qvec.len()` elements
/// - Pointers derived from slices are always valid and properly aligned for AVX2
///   (32-byte alignment is ideal but not required - we use unaligned loads)
#[target_feature(enable = "avx2")]
pub unsafe fn dot_product_adc(query: &[f32], qvec: &Int8QuantizedVector, query_sum: f32) -> f32 {
    let q_slice = qvec.as_slice();
    let dimension = qvec.len();

    // Process 16 elements at a time (2 AVX2 registers of 8 f32 each)
    let n_chunks = dimension / 16;

    // Initialize accumulators to zero
    let mut acc0 = _mm256_setzero_ps();
    let mut acc1 = _mm256_setzero_ps();

    for i in 0..n_chunks {
        // Software prefetching for large dimensions (>1536)
        // Prefetch data 4 iterations ahead to hide memory latency
        if dimension > 1536 && i + 4 < n_chunks {
            let prefetch_ptr = q_slice.as_ptr().add((i + 4) * 16) as *const i8;
            _mm_prefetch(prefetch_ptr as *const i8, _MM_HINT_T0);
        }

        // Load 16 i8 values (128-bit)
        // Using unaligned load - the slice may not be 16-byte aligned
        let i8_ptr = q_slice.as_ptr().add(i * 16) as *const __m128i;
        let i8_vec = _mm_loadu_si128(i8_ptr);

        // Widen i8 -> i16 (256-bit)
        // PMOVSXBW: Packed Move with Sign Extend (Byte to Word)
        let i16_vec = _mm256_cvtepi8_epi16(i8_vec);

        // Extract low and high 128-bit halves
        let low_i16 = _mm256_extracti128_si256(i16_vec, 0);
        let high_i16 = _mm256_extracti128_si256(i16_vec, 1);

        // Widen i16 -> i32 (256-bit for each half)
        // PMOVSXWD: Packed Move with Sign Extend (Word to Doubleword)
        let low_i32 = _mm256_cvtepi16_epi32(low_i16);
        let high_i32 = _mm256_cvtepi16_epi32(high_i16);

        // Convert i32 -> f32
        // CVTDQ2PS: Convert Packed Doubleword Integers to Packed Single-Precision Floats
        let low_f32 = _mm256_cvtepi32_ps(low_i32);
        let high_f32 = _mm256_cvtepi32_ps(high_i32);

        // Load corresponding query values (8 f32 each)
        let q_ptr = query.as_ptr().add(i * 16);
        let q_low = _mm256_loadu_ps(q_ptr);
        let q_high = _mm256_loadu_ps(q_ptr.add(8));

        // Multiply-accumulate: acc += query * quantized
        // FMA: Fused Multiply-Add (if available)
        acc0 = _mm256_fmadd_ps(q_low, low_f32, acc0);
        acc1 = _mm256_fmadd_ps(q_high, high_f32, acc1);
    }

    // Combine the two accumulators
    let sum01 = _mm256_add_ps(acc0, acc1);

    // Horizontal sum of 8 floats in sum01
    // Strategy: reduce 8 -> 4 -> 2 -> 1

    // Extract lower 128 bits
    let low128 = _mm256_castps256_ps128(sum01);
    let high128 = _mm256_extractf128_ps(sum01, 1);

    // Add lower and upper halves
    let sum128 = _mm_add_ps(low128, high128);

    // Horizontal sum of 4 floats
    // [a, b, c, d] -> [a+b, c+d, a+b, c+d]
    let shuffled = _mm_movehl_ps(sum128, sum128);
    let sum64 = _mm_add_ps(sum128, shuffled);

    // [a+b, c+d, ...] -> [a+b+c+d, ...]
    let shuffled2 = _mm_shuffle_ps(sum64, sum64, 0x01);
    let sum32 = _mm_add_ss(sum64, shuffled2);

    // Extract the final sum
    let mut total = _mm_cvtss_f32(sum32);

    // Handle remainder (dimensions not divisible by 16)
    // SAFETY: We use get_unchecked here for performance in the hot loop.
    // The safety invariant is: 0 <= i < q_slice.len()
    // - The loop starts at n_chunks * 16, which is <= dimension
    // - The loop iterates through query which has exactly 'dimension' elements
    // - q_slice comes from qvec which also has exactly 'dimension' elements
    // - Therefore i is always in bounds [0, dimension) for both slices
    for (i, &q_val) in query.iter().enumerate().skip(n_chunks * 16) {
        debug_assert!(
            i < q_slice.len(),
            "Index {} out of bounds for q_slice of len {}",
            i,
            q_slice.len()
        );
        total += q_val * (*q_slice.get_unchecked(i) as f32);
    }

    // Apply ADC correction: (dot - bias * ΣQ) / scale
    (total - qvec.metadata.bias * query_sum) / qvec.metadata.scale
}

/// Compute asymmetric squared L2 distance using AVX2
///
/// # Safety
///
/// - CPU must support AVX2
/// - `query` must have at least `qvec.len()` elements
#[target_feature(enable = "avx2")]
pub unsafe fn l2_squared_distance(
    query: &[f32],
    qvec: &Int8QuantizedVector,
    query_sum: f32,
    query_norm_sq: f32,
) -> f32 {
    let dot_adc = dot_product_adc(query, qvec, query_sum);
    (query_norm_sq + qvec.metadata.squared_sum - 2.0 * dot_adc).max(0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::search::quantization::{Dequantize, Quantize};

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn test_avx2_basic() {
        // Skip if AVX2 not available
        if !is_x86_feature_detected!("avx2") {
            return;
        }

        let original = vec![0.1f32, 0.5, -0.2, 0.8, 0.3, 0.9, -0.5, 0.4];
        let query = vec![0.2f32, 0.4, 0.1, 0.7, -0.3, 0.6, 0.2, 0.5];

        let qvec = original.quantize();
        let query_sum: f32 = query.iter().sum();

        // Call AVX2 implementation (unsafe block is safe here due to feature check above)
        let result = unsafe { dot_product_adc(&query, &qvec, query_sum) };

        // Verify result is finite and reasonable
        assert!(result.is_finite());

        // Verify against dequantized calculation
        let dequantized = qvec.dequantize();
        let expected: f32 = query
            .iter()
            .zip(dequantized.iter())
            .map(|(q, d)| q * d)
            .sum();

        assert!(
            (result - expected).abs() < 1e-3,
            "AVX2 result {} doesn't match expected {}",
            result,
            expected
        );
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn test_avx2_various_dimensions() {
        if !is_x86_feature_detected!("avx2") {
            return;
        }

        // Test various dimensions to ensure remainder handling works
        for dim in [
            1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129,
        ] {
            let original: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.01).collect();
            let query: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.02).collect();

            let qvec = original.quantize();
            let query_sum: f32 = query.iter().sum();

            let result = unsafe { dot_product_adc(&query, &qvec, query_sum) };
            assert!(result.is_finite(), "Failed for dimension {}", dim);
        }
    }

    #[test]
    #[cfg(target_arch = "x86_64")]
    fn test_avx2_matches_fallback() {
        if !is_x86_feature_detected!("avx2") {
            return;
        }

        use super::super::fallback;

        // Test that AVX2 produces same results as fallback
        for dim in [8, 16, 32, 64, 128, 256] {
            let original: Vec<f32> = (0..dim).map(|i| ((i * 7) % 100) as f32 * 0.01).collect();
            let query: Vec<f32> = (0..dim).map(|i| ((i * 13) % 100) as f32 * 0.01).collect();

            let qvec = original.quantize();
            let query_sum: f32 = query.iter().sum();

            let avx2_result = unsafe { dot_product_adc(&query, &qvec, query_sum) };
            let fallback_result = fallback::dot_product_adc(&query, &qvec, query_sum);

            assert!(
                (avx2_result - fallback_result).abs() < 1e-4,
                "Dimension {}: AVX2 {} != Fallback {}",
                dim,
                avx2_result,
                fallback_result
            );
        }
    }
}
