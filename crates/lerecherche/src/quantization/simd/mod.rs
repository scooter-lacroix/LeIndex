//! SIMD Distance Computation with Runtime Feature Detection
//!
//! This module provides optimized SIMD implementations for distance computations
//! between f32 query vectors and INT8 quantized stored vectors using Asymmetric
//! Distance Computation (ADC).
//!
//! # Implementation Strategy
//!
//! The module uses runtime CPU feature detection to automatically select the best
//! available implementation:
//!
//! | Platform | Implementation | Speedup |
//! |----------|---------------|---------|
//! | x86_64 with AVX2 | Native AVX2 intrinsics | 3-4x |
//! | Other platforms | `wide` crate fallback | 1x (baseline) |
//!
//! # Usage
//!
//! Simply call the public functions - the optimal implementation is selected
//! automatically at first use:
//!
//! ```ignore
//! use lerecherche::quantization::simd::dot_product_adc;
//!
//! let result = dot_product_adc(&query, &quantized_vector, query_sum);
//! ```

use super::vector::Int8QuantizedVector;
use std::sync::OnceLock;

// Platform-specific modules
pub mod fallback;

#[cfg(target_arch = "x86_64")]
pub mod x86_avx2;

/// Function pointer type for ADC dot product implementations
pub type DotProductFn = fn(&[f32], &Int8QuantizedVector, f32) -> f32;

/// Cached function pointer to the best available implementation
static DOT_PRODUCT_ADC_IMPL: OnceLock<DotProductFn> = OnceLock::new();

/// Safe wrapper for AVX2 implementation that handles the unsafe block
#[cfg(target_arch = "x86_64")]
fn avx2_dot_product_adc_safe(query: &[f32], qvec: &Int8QuantizedVector, query_sum: f32) -> f32 {
    // SAFETY: We only call this function after verifying AVX2 is available
    unsafe { x86_avx2::dot_product_adc(query, qvec, query_sum) }
}

/// Get the best available dot product implementation for this CPU
///
/// This function performs runtime feature detection on first call and caches
/// the result for subsequent calls.
fn get_best_dot_product_impl() -> DotProductFn {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return avx2_dot_product_adc_safe;
        }
    }

    // Default fallback (works on all platforms)
    fallback::dot_product_adc
}

/// Compute asymmetric dot product using ADC with the best available SIMD implementation
///
/// # Arguments
/// * `query` - The f32 query vector
/// * `qvec` - The INT8 quantized stored vector
/// * `query_sum` - Precomputed sum of query values (ΣQ_i)
///
/// # Returns
/// The ADC-corrected dot product: (Σ(Q_i * q_i) - bias * ΣQ_i) / scale
///
/// # Implementation
/// Automatically selects the fastest implementation based on CPU capabilities:
/// - AVX2 on x86_64 (3-4x speedup)
/// - Portable fallback on other platforms
pub fn dot_product_adc(query: &[f32], qvec: &Int8QuantizedVector, query_sum: f32) -> f32 {
    let implementation = DOT_PRODUCT_ADC_IMPL.get_or_init(get_best_dot_product_impl);
    implementation(query, qvec, query_sum)
}

/// Compute asymmetric squared L2 distance using ADC
///
/// # Formula
/// ```text
/// ||Q - D||² = ||Q||² + ||D||² - 2 * dot_product_adc(Q, D)
/// ```
pub fn l2_squared_adc(
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
    use crate::quantization::{Quantize, Dequantize};

    #[test]
    fn test_dot_product_adc_basic() {
        let original = vec![0.1f32, 0.5, -0.2, 0.8];
        let query = vec![0.2f32, 0.4, 0.1, 0.7];

        let qvec = original.quantize();
        let query_sum: f32 = query.iter().sum();

        let result = dot_product_adc(&query, &qvec, query_sum);

        // Should produce a finite result
        assert!(result.is_finite());

        // Verify against manual calculation
        let dequantized = qvec.dequantize();
        let expected_dot: f32 = query.iter().zip(dequantized.iter()).map(|(q, d)| q * d).sum();
        assert!((result - expected_dot).abs() < 1e-3);
    }

    #[test]
    fn test_various_dimensions() {
        for dim in [1, 7, 8, 9, 15, 16, 17, 31, 32, 33, 63, 64, 65, 127, 128, 129] {
            let original: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.01).collect();
            let query: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.02).collect();

            let qvec = original.quantize();
            let query_sum: f32 = query.iter().sum();

            // Should not panic on any dimension
            let result = dot_product_adc(&query, &qvec, query_sum);
            assert!(result.is_finite(), "Failed for dimension {}", dim);
        }
    }
}
