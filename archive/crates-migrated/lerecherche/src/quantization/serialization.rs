//! Serialization for INT8 Quantized Vectors
//!
//! This module provides compact binary serialization for `Int8QuantizedVector`
//! using `bincode`, achieving ~74% memory reduction compared to f32 storage.
//!
//! # Serialization Format
//!
//! The binary format consists of:
//! - Vector length (usize, 8 bytes on 64-bit systems)
//! - Quantized data (1 byte per dimension)
//! - Metadata (scale, bias, sum, squared_sum as f32, 16 bytes)
//!
//! # Platform Compatibility
//!
//! ⚠️ **Endianness Warning**: The serialized format uses platform-native endianness.
//! This means:
//!
//! - ✅ Data serialized on x86_64 (little-endian) can be read on AArch64 (little-endian)
//! - ❌ Data serialized on big-endian systems cannot be read on little-endian systems
//!
//! For cross-platform compatibility:
//! - Ensure serialization and deserialization occur on the same endianness
//! - Or convert endianness before/after serialization
//!
//! # Example
//!
//! ```
//! use lerecherche::quantization::{Quantize, serialization};
//!
//! let vector = vec![0.1f32, 0.5, -0.2, 0.8];
//! let quantized = vector.quantize();
//!
//! // Serialize
//! let bytes = serialization::to_bytes(&quantized).unwrap();
//!
//! // Deserialize
//! let decoded = serialization::from_bytes(&bytes).unwrap();
//! ```

use crate::quantization::vector::Int8QuantizedVector;
use thiserror::Error;

/// Errors that can occur during serialization/deserialization of quantized vectors
#[derive(Debug, Error)]
pub enum SerializationError {
    /// Failed to serialize the vector to binary
    #[error("Serialization failed: {0}")]
    Serialization(String),
    /// Failed to deserialize the vector from binary
    #[error("Deserialization failed: {0}")]
    Deserialization(String),
}

/// Serialize quantized vector to compact binary format
pub fn to_bytes(vector: &Int8QuantizedVector) -> Result<Vec<u8>, SerializationError> {
    bincode::serialize(vector).map_err(|e| SerializationError::Serialization(e.to_string()))
}

/// Deserialize quantized vector from compact binary format
pub fn from_bytes(bytes: &[u8]) -> Result<Int8QuantizedVector, SerializationError> {
    bincode::deserialize(bytes).map_err(|e| SerializationError::Deserialization(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quantization::vector::Int8QuantizedVectorMetadata;

    #[test]
    fn test_serialization_roundtrip() {
        // Create a test vector
        let metadata = Int8QuantizedVectorMetadata::new(1.0, 0.0, 10.0, 100.0);
        let data = vec![1, 2, 3, 4, 5];
        let qv = Int8QuantizedVector::new(data, metadata, 5);

        // Serialize
        let bytes = to_bytes(&qv).unwrap();

        // Deserialize
        let decoded = from_bytes(&bytes).unwrap();

        assert_eq!(qv, decoded);
    }

    #[test]
    fn test_serialization_size() {
        // Create a test vector with 64 dimensions (2 blocks)
        let metadata = Int8QuantizedVectorMetadata::new(1.0, 0.0, 10.0, 100.0);
        let data: Vec<i8> = (0..64).map(|i| i as i8).collect();
        let qv = Int8QuantizedVector::new(data, metadata, 64);

        let bytes = to_bytes(&qv).unwrap();

        // Expected size:
        // - Blocks: 8 bytes (len) + 64 bytes (data) = 72 bytes
        // - Metadata: 16 bytes (4 floats, padding skipped)
        // - Dimension: 8 bytes (usize)
        // Total: 72 + 16 + 8 = 96 bytes

        // Allow for minor platform differences or bincode variations, but strictly < 112 (size with padding)
        assert!(
            bytes.len() < 112,
            "Serialized size {} indicates padding was NOT skipped (expected ~96 bytes)",
            bytes.len()
        );

        // Exact check for 64-bit bincode standard
        if std::mem::size_of::<usize>() == 8 {
            assert_eq!(bytes.len(), 96, "Unexpected serialized size");
        }
    }
}
