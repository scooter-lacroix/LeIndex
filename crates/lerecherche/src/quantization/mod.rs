//! INT8 Quantization for Vector Search
//!
//! This module provides high-performance INT8 quantization for vector storage and search,
//! modeled after Zvec's record_quantizer.h. It uses Asymmetric Distance Computation (ADC)
//! to maintain precision while reducing memory footprint by ~74%.

#[allow(clippy::module_inception)]
pub mod quantization;
pub mod vector;

// SIMD module with runtime feature detection
pub mod simd;
pub use simd::{dot_product_adc, l2_squared_adc};

/// Distance metric implementations for Asymmetric Distance Computation
pub mod distance;
pub mod int8;
pub mod int8_hnsw;

pub use distance::*;
pub use int8_hnsw::*;
pub use quantization::*;
pub use vector::*;

/// Serialization support for quantized vectors
pub mod serialization;
