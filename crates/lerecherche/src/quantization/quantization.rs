//! Quantization Trait and Implementation
//!
//! This module provides the `Quantize` trait for converting between
//! f32 vectors and INT8 quantized vectors using the formula:
//! - s = 254.0 / max(max-min, 1e-9)
//! - b = -min * s - 127.0
//! - q = round(v * s + b) clamped to [-128, 127]

use super::vector::{Int8QuantizedVector, Int8QuantizedVectorMetadata};

/// Trait for quantizing f32 vectors to INT8
pub trait Quantize {
    /// The output type after quantization
    type Output;

    /// Quantize to INT8 format
    fn quantize(&self) -> Self::Output;

    /// Quantize with pre-computed metadata
    fn quantize_with_metadata(&self, metadata: &Int8QuantizedVectorMetadata) -> Self::Output;
}

impl Quantize for [f32] {
    type Output = Int8QuantizedVector;

    fn quantize(&self) -> Self::Output {
        let metadata = Int8QuantizedVectorMetadata::from_vector(self);
        self.quantize_with_metadata(&metadata)
    }

    fn quantize_with_metadata(&self, metadata: &Int8QuantizedVectorMetadata) -> Self::Output {
        let data: Vec<i8> = self
            .iter()
            .map(|&v| {
                let scaled = v * metadata.scale + metadata.bias;
                scaled.round().clamp(-128.0, 127.0) as i8
            })
            .collect();

        Int8QuantizedVector::new(data, *metadata, self.len())
    }
}

impl Quantize for Vec<f32> {
    type Output = Int8QuantizedVector;

    fn quantize(&self) -> Self::Output {
        self.as_slice().quantize()
    }

    fn quantize_with_metadata(&self, metadata: &Int8QuantizedVectorMetadata) -> Self::Output {
        self.as_slice().quantize_with_metadata(metadata)
    }
}

/// Dequantize an INT8 value back to f32 using metadata
#[inline]
pub fn dequantize_value(quantized: i8, metadata: &Int8QuantizedVectorMetadata) -> f32 {
    (quantized as f32 - metadata.bias) / metadata.scale
}

/// Extension trait for dequantizing INT8 data
pub trait Dequantize {
    /// Dequantize back to f32 vector
    fn dequantize(&self) -> Vec<f32>;
}

impl Dequantize for Int8QuantizedVector {
    fn dequantize(&self) -> Vec<f32> {
        self.as_slice()
            .iter()
            .take(self.dimension)
            .map(|&q| dequantize_value(q, &self.metadata))
            .collect()
    }
}

/// Compute quantization error (RMSE) between original and dequantized vectors
pub fn quantization_error(original: &[f32], dequantized: &[f32]) -> f32 {
    assert_eq!(original.len(), dequantized.len());

    let sum_sq_diff: f32 = original
        .iter()
        .zip(dequantized.iter())
        .map(|(&o, &d)| (o - d) * (o - d))
        .sum();

    (sum_sq_diff / original.len() as f32).sqrt()
}

/// Batch quantize multiple vectors with shared parameters
pub fn batch_quantize(
    vectors: &[Vec<f32>],
) -> Option<(Vec<Int8QuantizedVector>, Int8QuantizedVectorMetadata)> {
    if vectors.is_empty() {
        return None;
    }

    // Compute global min/max across all vectors for shared parameters
    let (global_min, global_max, _, _) = vectors.iter().fold(
        (f32::INFINITY, f32::NEG_INFINITY, 0.0f32, 0.0f32),
        |(min, max, sum, sq_sum), vec| {
            vec.iter()
                .fold((min, max, sum, sq_sum), |(min, max, sum, sq_sum), &v| {
                    (min.min(v), max.max(v), sum + v, sq_sum + v * v)
                })
        },
    );

    let scale = 254.0 / (global_max - global_min).max(1e-9);
    let bias = -global_min * scale - 127.0;

    // Compute average sum and squared_sum for metadata
    let total_sum: f32 = vectors.iter().map(|v| v.iter().sum::<f32>()).sum();
    let total_sq_sum: f32 = vectors
        .iter()
        .map(|v| v.iter().map(|&x| x * x).sum::<f32>())
        .sum();
    let n = vectors.len() as f32;

    let metadata = Int8QuantizedVectorMetadata::new(scale, bias, total_sum / n, total_sq_sum / n);

    let quantized: Vec<Int8QuantizedVector> = vectors
        .iter()
        .map(|v| v.quantize_with_metadata(&metadata))
        .collect();

    Some((quantized, metadata))
}
