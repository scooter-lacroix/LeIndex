//! INT8 Quantization Module
//!
//! This module provides the foundational types and traits for INT8 quantization
//! in the lerecherche crate. It implements Zvec-style quantization.

pub use super::quantization::{
    batch_quantize, dequantize_value, quantization_error, Dequantize, Quantize,
};
pub use super::vector::{Int8QuantizedVector, Int8QuantizedVectorMetadata, SimdBlock};

#[cfg(test)]
mod integration_tests {
    use super::*;

    #[test]
    fn test_end_to_end_quantization_workflow() {
        let embeddings: Vec<Vec<f32>> = (0..10)
            .map(|i| {
                (0..128)
                    .map(|j| (i as f32 * 0.1 + j as f32 * 0.01).sin() * 0.8)
                    .collect()
            })
            .collect();

        let quantized: Vec<Int8QuantizedVector> = embeddings.iter().map(|e| e.quantize()).collect();

        for qv in &quantized {
            assert_eq!(qv.len(), 128);
        }

        for (original, qv) in embeddings.iter().zip(quantized.iter()) {
            let dequantized = qv.dequantize();
            let error = quantization_error(original, &dequantized);
            assert!(error < 0.01);
        }
    }
}
