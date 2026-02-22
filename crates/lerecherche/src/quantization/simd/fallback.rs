//! Fallback SIMD Implementation using the `wide` crate
//!
//! This implementation works on all platforms but doesn't optimize the i8→f32
//! widening step. It serves as the baseline implementation and correctness reference
//! for platform-specific optimized versions.

use wide::f32x8;
use super::super::vector::Int8QuantizedVector;

/// Compute asymmetric dot product using ADC (fallback implementation)
///
/// This implementation uses the `wide` crate for portable SIMD but performs
/// i8→f32 conversion sequentially, which is the performance bottleneck.
///
/// # Performance
/// - Baseline speed (1x)
/// - Works on all platforms
/// - Safe (no unsafe code)
pub fn dot_product_adc(query: &[f32], qvec: &Int8QuantizedVector, query_sum: f32) -> f32 {
    let n = query.len();
    let q_data = qvec.as_slice();

    let chunks = n / 8;
    let mut dot: f32 = 0.0;

    for c in 0..chunks {
        let offset = c * 8;

        // Load query chunk (f32x8)
        let query_simd = f32x8::from([
            query[offset],
            query[offset + 1],
            query[offset + 2],
            query[offset + 3],
            query[offset + 4],
            query[offset + 5],
            query[offset + 6],
            query[offset + 7],
        ]);

        // Load quantized chunk and widen to f32 (BOTTLENECK: sequential)
        let q_slice = &q_data[offset..offset + 8];
        let q_values: [f32; 8] = [
            q_slice[0] as f32,
            q_slice[1] as f32,
            q_slice[2] as f32,
            q_slice[3] as f32,
            q_slice[4] as f32,
            q_slice[5] as f32,
            q_slice[6] as f32,
            q_slice[7] as f32,
        ];
        let q_simd = f32x8::from(q_values);

        // Accumulate dot product
        dot += (query_simd * q_simd).reduce_add();
    }

    // Handle remainder
    for i in (chunks * 8)..n {
        dot += query[i] * (q_data[i] as f32);
    }

    // Apply ADC correction: (dot - bias * ΣQ) / s
    (dot - qvec.metadata.bias * query_sum) / qvec.metadata.scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quantization::{Quantize, Dequantize};

    #[test]
    fn test_fallback_accuracy() {
        let original = vec![0.1f32, 0.5, -0.2, 0.8, 0.3, 0.9, -0.5, 0.4];
        let query = vec![0.2f32, 0.4, 0.1, 0.7, -0.3, 0.6, 0.2, 0.5];

        let qvec = original.quantize();
        let query_sum: f32 = query.iter().sum();

        let result = dot_product_adc(&query, &qvec, query_sum);

        // Verify against dequantized calculation
        let dequantized = qvec.dequantize();
        let expected: f32 = query.iter().zip(dequantized.iter()).map(|(q, d)| q * d).sum();

        assert!(
            (result - expected).abs() < 1e-4,
            "Fallback result {} doesn't match expected {}",
            result,
            expected
        );
    }

    #[test]
    fn test_remainder_handling() {
        // Test dimensions not divisible by 8
        for dim in [1, 3, 7, 9, 15] {
            let original: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.1).collect();
            let query: Vec<f32> = (0..dim).map(|i| (dim as f32) - (i as f32) * 0.1).collect();

            let qvec = original.quantize();
            let query_sum: f32 = query.iter().sum();

            let result = dot_product_adc(&query, &qvec, query_sum);
            assert!(result.is_finite(), "Failed for dimension {}", dim);
        }
    }
}
