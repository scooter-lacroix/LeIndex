//! Core Types for INT8 Quantization
//!
//! This module provides the foundational types for INT8 quantization with SIMD support:
//! - SimdBlock: 32-byte aligned storage for SIMD operations
//! - Int8QuantizedVectorMetadata: Configuration for quantization parameters
//! - Int8QuantizedVector: INT8 quantized vector with metadata

use serde::{Deserialize, Serialize};

/// SIMD block size for 32-byte aligned operations (AVX2 compatible)
pub const SIMD_BLOCK_SIZE: usize = 32;

/// Number of i8 values in a SimdBlock (32 bytes / 1 byte = 32)
pub const SIMD_LANES: usize = 32;

/// A 32-byte aligned block for SIMD operations
///
/// This struct provides aligned storage for INT8 quantized data,
/// enabling efficient SIMD operations without unaligned loads.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[repr(C, align(32))]
pub struct SimdBlock {
    /// Aligned storage for 32 INT8 values
    pub data: [i8; SIMD_LANES],
}

impl SimdBlock {
    /// Create a new SimdBlock with all zeros
    #[inline]
    pub fn zeros() -> Self {
        Self {
            data: [0i8; SIMD_LANES],
        }
    }

    /// Create a new SimdBlock from a slice of i8 values
    ///
    /// # Panics
    /// Panics if the slice length is not exactly SIMD_LANES (32)
    #[inline]
    pub fn from_slice(slice: &[i8]) -> Self {
        assert_eq!(
            slice.len(),
            SIMD_LANES,
            "Slice must have exactly {} elements",
            SIMD_LANES
        );
        let mut data = [0i8; SIMD_LANES];
        data.copy_from_slice(slice);
        Self { data }
    }

    /// Get the number of elements in the block
    #[inline]
    pub fn len(&self) -> usize {
        SIMD_LANES
    }

    /// Check if the block is empty (always false)
    #[inline]
    pub fn is_empty(&self) -> bool {
        false
    }

    /// Get a slice view of the data
    #[inline]
    pub fn as_slice(&self) -> &[i8] {
        &self.data
    }

    /// Get a mutable slice view of the data
    #[inline]
    pub fn as_mut_slice(&mut self) -> &mut [i8] {
        &mut self.data
    }
}

impl Default for SimdBlock {
    fn default() -> Self {
        Self::zeros()
    }
}

impl AsRef<[i8]> for SimdBlock {
    fn as_ref(&self) -> &[i8] {
        &self.data
    }
}

impl AsMut<[i8]> for SimdBlock {
    fn as_mut(&mut self) -> &mut [i8] {
        &mut self.data
    }
}

/// Configuration for INT8 quantization
///
/// This struct stores the parameters needed for asymmetric quantization
/// in the range [-128, 127]. The quantization formula is:
/// - s = 254.0 / max(max-min, 1e-9)
/// - b = -min * s - 127.0
/// - q = round(v * s + b) clamped to [-128, 127]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[repr(C, align(32))]
pub struct Int8QuantizedVectorMetadata {
    /// Scale factor for quantization/dequantization
    pub scale: f32,
    /// Bias (zero-point) for quantization/dequantization
    pub bias: f32,
    /// Sum of original vector values: Σx[i]
    pub sum: f32,
    /// Sum of squared original vector values: Σx[i]²
    pub squared_sum: f32,
    /// Padding to ensure 32-byte alignment (16 bytes)
    /// Padding to ensure 32-byte alignment (16 bytes)
    #[serde(skip)]
    pub _padding: [u8; 16],
}

impl Int8QuantizedVectorMetadata {
    /// Create new config from computed values
    #[inline]
    pub fn new(scale: f32, bias: f32, sum: f32, squared_sum: f32) -> Self {
        Self {
            scale,
            bias,
            sum,
            squared_sum,
            _padding: [0; 16],
        }
    }

    /// Compute metadata from an f32 vector
    ///
    /// Uses the quantization formula:
    /// - s = 254.0 / max(max-min, 1e-9)
    /// - b = -min * s - 127.0
    ///
    /// # Panics
    ///
    /// Panics if the vector contains NaN or infinite values.
    pub fn from_vector(vector: &[f32]) -> Self {
        // Validate all inputs are finite
        for (idx, &value) in vector.iter().enumerate() {
            if !value.is_finite() {
                panic!(
                    "Cannot quantize non-finite value ({}) at index {}. Vector must contain only finite f32 values.",
                    value, idx
                );
            }
        }

        let (min, max, sum, squared_sum) = vector.iter().fold(
            (f32::INFINITY, f32::NEG_INFINITY, 0.0f32, 0.0f32),
            |(min, max, sum, sq_sum), &v| (min.min(v), max.max(v), sum + v, sq_sum + v * v),
        );

        let scale = 254.0 / (max - min).max(1e-9);
        let bias = -min * scale - 127.0;

        Self::new(scale, bias, sum, squared_sum)
    }

    /// Quantize a single f32 value to i8
    #[inline]
    pub fn quantize(&self, value: f32) -> i8 {
        let scaled = value * self.scale + self.bias;
        scaled.round().clamp(-128.0, 127.0) as i8
    }

    /// Dequantize a single i8 value back to f32
    #[inline]
    pub fn dequantize(&self, quantized: i8) -> f32 {
        (quantized as f32 - self.bias) / self.scale
    }

    /// Get the L2 norm of the original vector
    #[inline]
    pub fn norm(&self) -> f32 {
        self.squared_sum.sqrt()
    }

    /// Compute the squared L2 norm
    #[inline]
    pub fn norm_squared(&self) -> f32 {
        self.squared_sum
    }
}

impl Default for Int8QuantizedVectorMetadata {
    fn default() -> Self {
        Self {
            scale: 1.0,
            bias: 0.0,
            sum: 0.0,
            squared_sum: 0.0,
            _padding: [0; 16],
        }
    }
}

/// A quantized vector stored as INT8 values with 32-byte aligned blocks
///
/// This struct provides 4x memory reduction compared to f32 storage
/// while maintaining precision through Zvec-style error correction metadata.
/// Data is stored in 32-byte aligned SimdBlocks for efficient SIMD operations.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Int8QuantizedVector {
    /// Quantized INT8 data stored in 32-byte aligned blocks
    pub blocks: Vec<SimdBlock>,
    /// Quantization metadata with 32-byte alignment
    pub metadata: Int8QuantizedVectorMetadata,
    /// Original vector dimension
    pub dimension: usize,
}

impl Int8QuantizedVector {
    /// Create a new quantized vector from raw INT8 data and config
    ///
    /// Data will be padded and stored in SimdBlocks
    pub fn new(data: Vec<i8>, metadata: Int8QuantizedVectorMetadata, dimension: usize) -> Self {
        // Pad data to multiple of SIMD_LANES
        let mut padded_data = data;
        let remainder = padded_data.len() % SIMD_LANES;
        if remainder != 0 {
            padded_data.extend(std::iter::repeat(0i8).take(SIMD_LANES - remainder));
        }

        // Convert to blocks
        let blocks: Vec<SimdBlock> = padded_data
            .chunks(SIMD_LANES)
            .map(SimdBlock::from_slice)
            .collect();

        Self {
            blocks,
            metadata,
            dimension,
        }
    }

    /// Create a new quantized vector from pre-formed blocks
    #[inline]
    pub fn from_blocks(
        blocks: Vec<SimdBlock>,
        metadata: Int8QuantizedVectorMetadata,
        dimension: usize,
    ) -> Self {
        Self {
            blocks,
            metadata,
            dimension,
        }
    }

    /// Get the quantized data as a flat slice of i8
    ///
    /// # Safety
    ///
    /// This function uses `unsafe` to transmute `&[SimdBlock]` to `&[i8]`. This is sound because:
    ///
    /// 1. **Fixed Layout**: `SimdBlock` is `#[repr(C)]` with a fixed memory layout containing exactly
    ///    `SIMD_LANES` (32) contiguous `i8` values, totaling 32 bytes.
    ///
    /// 2. **Type Compatibility**: `i8` is the basic element type stored in `SimdBlock.data`, so
    ///    transmuting from `*const SimdBlock` to `*const i8` is valid.
    ///
    /// 3. **Bounded Pointer Arithmetic**: The slice length is calculated as `blocks.len() * SIMD_LANES`,
    ///    which exactly matches the total number of `i8` elements across all blocks.
    ///
    /// 4. **No Mutable Aliasing**: This function takes `&self` and returns `&[i8]`, ensuring no
    ///    mutable references exist during the lifetime of the returned slice.
    ///
    /// 5. **Lifetime Safety**: The returned slice's lifetime is tied to `&self`, preventing use-after-free.
    ///
    /// # Alignment
    ///
    /// The returned slice is aligned to at least 1 byte (i8 alignment). Note that while the underlying
    /// `SimdBlock` data is 32-byte aligned, the slice view doesn't preserve that alignment guarantee.
    /// For SIMD operations requiring 32-byte alignment, use `self.blocks` directly.
    #[inline]
    pub fn as_slice(&self) -> &[i8] {
        unsafe {
            std::slice::from_raw_parts(
                self.blocks.as_ptr() as *const i8,
                self.blocks.len() * SIMD_LANES,
            )
        }
    }

    /// Get the number of blocks
    #[inline]
    pub fn num_blocks(&self) -> usize {
        self.blocks.len()
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

    /// Memory usage in bytes (including metadata)
    pub fn memory_bytes(&self) -> usize {
        std::mem::size_of::<Self>() + self.blocks.len() * std::mem::size_of::<SimdBlock>()
    }

    /// Dequantize back to f32 vector
    pub fn to_f32(&self) -> Vec<f32> {
        self.as_slice()
            .iter()
            .take(self.dimension)
            .map(|&q| self.metadata.dequantize(q))
            .collect()
    }
}

/// Compute the number of SimdBlocks needed for a given dimension
#[inline]
pub fn blocks_for_dimension(dimension: usize) -> usize {
    dimension.div_ceil(SIMD_LANES)
}

/// Pad a dimension to the next multiple of SIMD_LANES
#[inline]
pub fn padded_dimension(dimension: usize) -> usize {
    blocks_for_dimension(dimension) * SIMD_LANES
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quantization::quantization::{Dequantize, Quantize};

    #[test]
    fn test_simd_block_alignment() {
        // Verify 32-byte alignment
        assert_eq!(
            std::mem::align_of::<SimdBlock>(),
            32,
            "SimdBlock must have 32-byte alignment"
        );

        // Verify total size is 32 bytes
        assert_eq!(
            std::mem::size_of::<SimdBlock>(),
            32,
            "SimdBlock must be 32 bytes"
        );
    }

    #[test]
    fn test_simd_block_creation() {
        let block = SimdBlock::zeros();
        assert_eq!(block.len(), 32);
        assert!(!block.is_empty());
        assert!(block.data.iter().all(|&x| x == 0));

        let data: Vec<i8> = (0..32).map(|i| i as i8).collect();
        let block2 = SimdBlock::from_slice(&data);
        assert_eq!(block2.data[0], 0);
        assert_eq!(block2.data[31], 31);
    }

    #[test]
    fn test_int8_quantized_vector_metadata_alignment() {
        // Verify 32-byte alignment
        assert_eq!(
            std::mem::align_of::<Int8QuantizedVectorMetadata>(),
            32,
            "Int8QuantizedVectorMetadata must have 32-byte alignment"
        );

        // Verify total size is 32 bytes
        assert_eq!(
            std::mem::size_of::<Int8QuantizedVectorMetadata>(),
            32,
            "Int8QuantizedVectorMetadata must be 32 bytes"
        );
    }

    #[test]
    fn test_metadata_from_vector() {
        let vector = vec![0.0f32, 0.5, 1.0];
        let metadata = Int8QuantizedVectorMetadata::from_vector(&vector);

        // Expected: s = 254.0 / (1.0 - 0.0) = 254.0
        // Expected: b = -0.0 * 254.0 - 127.0 = -127.0
        assert!((metadata.scale - 254.0).abs() < 1e-6);
        assert!((metadata.bias - (-127.0)).abs() < 1e-6);
        assert!((metadata.sum - 1.5).abs() < 1e-6);
        assert!((metadata.squared_sum - 1.25).abs() < 1e-6);
    }

    #[test]
    fn test_int8_quantized_vector_creation() {
        let data: Vec<i8> = (0..64).map(|i| (i % 256) as i8).collect();
        let metadata = Int8QuantizedVectorMetadata::default();
        let qv = Int8QuantizedVector::new(data, metadata, 64);

        assert_eq!(qv.len(), 64);
        assert_eq!(qv.num_blocks(), 2);
    }

    #[test]
    #[should_panic(expected = "non-finite value (NaN)")]
    fn test_rejects_nan_values() {
        let vector = vec![0.1f32, f32::NAN, 0.3];
        let _metadata = Int8QuantizedVectorMetadata::from_vector(&vector);
    }

    #[test]
    #[should_panic(expected = "non-finite value (inf)")]
    fn test_rejects_positive_infinity() {
        let vector = vec![0.1f32, f32::INFINITY, 0.3];
        let _metadata = Int8QuantizedVectorMetadata::from_vector(&vector);
    }

    #[test]
    #[should_panic(expected = "non-finite value (-inf)")]
    fn test_rejects_negative_infinity() {
        let vector = vec![0.1f32, f32::NEG_INFINITY, 0.3];
        let _metadata = Int8QuantizedVectorMetadata::from_vector(&vector);
    }

    #[test]
    fn test_accepts_finite_values() {
        // Test various finite values including extremes
        let vector = vec![
            f32::MAX,
            f32::MIN,
            0.0,
            -0.0,
            1e-38,  // Very small positive
            -1e-38, // Very small negative
            1e38,   // Large positive
            -1e38,  // Large negative
        ];
        // Should not panic
        let _metadata = Int8QuantizedVectorMetadata::from_vector(&vector);
    }

    #[test]
    fn test_as_slice_safety() {
        let data: Vec<i8> = (0..64).map(|i| i as i8).collect();
        let metadata = Int8QuantizedVectorMetadata::default();
        let qv = Int8QuantizedVector::new(data.clone(), metadata, 64);

        // Verify as_slice returns correct data
        let slice = qv.as_slice();
        assert_eq!(slice.len(), 64);
        assert_eq!(&slice[..64], &data[..64]);

        // Verify we can call as_slice multiple times
        let slice2 = qv.as_slice();
        assert_eq!(slice, slice2);

        // Verify the slice is read-only
        let first_val = slice[0];
        assert_eq!(first_val, data[0]);
    }

    #[test]
    fn test_empty_vector_handling() {
        // Empty vectors should produce empty quantized vectors
        let empty: Vec<f32> = vec![];
        let qv = empty.quantize();

        assert_eq!(qv.len(), 0);
        assert_eq!(qv.as_slice().len(), 0);
        assert_eq!(qv.num_blocks(), 0);
    }

    #[test]
    fn test_large_dimension_vectors() {
        // Test various large dimensions commonly used in embeddings
        let dimensions = vec![1000, 2048, 4096, 8192, 10000];

        for dim in dimensions {
            let vector: Vec<f32> = (0..dim).map(|i| (i % 100) as f32 * 0.01).collect();
            let quantized = vector.quantize();
            let dequantized = quantized.dequantize();

            // Verify roundtrip preserves dimension
            assert_eq!(dequantized.len(), dim, "Dimension mismatch for {}", dim);

            // Verify quantization error is reasonable (< 1% RMSE for these test values)
            let mse: f32 = vector
                .iter()
                .zip(dequantized.iter())
                .map(|(o, d)| (o - d).powi(2))
                .sum::<f32>()
                / dim as f32;
            let rmse = mse.sqrt();

            assert!(rmse < 0.01, "RMSE too high for dimension {}: {}", dim, rmse);
        }
    }

    #[test]
    fn test_very_small_dimension_vectors() {
        // Test edge cases for small dimensions
        for dim in 1..=32 {
            let vector: Vec<f32> = (0..dim).map(|i| (i as f32) * 0.1 - 0.5).collect();
            let quantized = vector.quantize();
            let dequantized = quantized.dequantize();

            assert_eq!(dequantized.len(), dim);
        }
    }

    #[test]
    fn test_uniform_vector_quantization() {
        // Uniform vectors (all same value) should quantize without issues
        let uniform = vec![0.5f32; 100];
        let quantized = uniform.quantize();
        let dequantized = quantized.dequantize();

        // All dequantized values should be very close to original
        for (i, &val) in dequantized.iter().enumerate() {
            assert!(
                (val - 0.5).abs() < 0.01,
                "Uniform vector dequantization failed at index {}: got {}",
                i,
                val
            );
        }
    }

    #[test]
    fn test_zero_vector_quantization() {
        // Zero vector should quantize correctly (scale handles division by zero)
        let zeros = vec![0.0f32; 128];
        let quantized = zeros.quantize();
        let dequantized = quantized.dequantize();

        assert_eq!(dequantized.len(), 128);
        // All values should be close to zero
        for val in &dequantized {
            assert!(
                val.abs() < 0.01f32,
                "Zero vector dequantized to non-zero: {}",
                val
            );
        }
    }
}
