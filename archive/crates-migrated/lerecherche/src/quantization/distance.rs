//! Distance Trait Implementation for INT8 Quantized Vectors with ADC
//!
//! This module provides the `Distance` trait implementation for `Int8QuantizedVector`
//! using Asymmetric Distance Computation (ADC). The key challenge is that hnsw_rs's
//! `Distance` trait expects both vectors to be of the same type, but we want:
//! - Query: f32 (full precision)
//! - Stored: Int8QuantizedVector (INT8)
//!
//! # Solution: Thread-Local Query Context
//!
//! We use thread-local storage to store the current f32 query and its metadata
//! (sum, norm_sq) during a search operation. The `Distance` implementation for
//! `Int8QuantizedVector` reads from this thread-local context to perform ADC
//! against the stored INT8 data.
//!
//! # Thread Safety
//!
//! ⚠️ **IMPORTANT**: The ADC context uses thread-local storage and is **NOT** `Send` or `Sync`.
//!
//! ## Single-Threaded Search Only
//!
//! The current implementation is safe for **single-threaded searches** because:
//!
//! 1. `Int8HnswIndex::search()` uses `hnsw.search()` (single-threaded)
//! 2. The ADC context is set before search: `set_adc_query_context(query)`
//! 3. The search executes entirely within the calling thread
//! 4. The ADC context is cleared after search: `clear_adc_query_context()`
//!
//! ## Thread-Local Isolation
//!
//! Each thread has its own independent copy of `ADC_QUERY_CONTEXT`. This means:
//! - Multiple threads can search concurrently using separate `Int8HnswIndex` instances
//! - Each thread's context is isolated from others
//! - No data races can occur between threads
//!
//! ## Limitations
//!
//! - ❌ **DO NOT** use `hnsw.parallel_search()` - it spawns threads that won't have the context
//! - ❌ **DO NOT** share an `Int8HnswIndex` across threads for concurrent searches
//! - ✅ **DO** create separate `Int8HnswIndex` instances per thread
//! - ✅ **DO** use single-threaded `search()` method

use super::simd::{dot_product_adc, l2_squared_adc};
use super::vector::Int8QuantizedVector;
use hnsw_rs::prelude::Distance;
use serde::{Deserialize, Serialize};
use std::cell::RefCell;

// Thread-local storage for ADC query context
thread_local! {
    /// Thread-local storage for the current f32 query during ADC search.
    ///
    /// # Thread Safety
    ///
    /// This context is thread-local, meaning each thread has its own independent
    /// copy. This is safe for single-threaded searches because:
    ///
    /// 1. `Int8HnswIndex::search()` uses single-threaded `hnsw.search()`
    /// 2. The context is set at search start and cleared before returning
    /// 3. No sharing occurs between threads
    ///
    /// # Limitations
    ///
    /// ⚠️ **DO NOT** use with `hnsw.parallel_search()` - spawned threads won't
    /// have access to this context, causing incorrect distance calculations.
    ///
    /// ⚠️ **DO NOT** share `Int8HnswIndex` across threads for concurrent searches.
    ///
    /// # Implementation Note
    ///
    /// Using `RefCell` allows mutable access within a single thread. This is safe
    /// because thread-local data cannot be accessed from other threads.
    static ADC_QUERY_CONTEXT: RefCell<Option<AdcQueryContext>> = const { RefCell::new(None) };
}

/// Query context for Asymmetric Distance Computation
#[derive(Debug, Clone)]
pub struct AdcQueryContext {
    /// The f32 query vector
    pub query: Vec<f32>,
    /// Sum of query values: Σq[i]
    pub sum: f32,
    /// Sum of squared query values: Σq[i]²
    pub norm_sq: f32,
    /// Distance metric to use
    pub metric: AdcDistanceMetric,
}

/// Distance metric for ADC computation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AdcDistanceMetric {
    /// Cosine similarity (converted to distance)
    #[default]
    Cosine,
    /// L2 squared distance
    L2Squared,
    /// Dot product (converted to distance)
    Dot,
}

impl AdcQueryContext {
    /// Create a new query context from an f32 query vector
    pub fn new(query: Vec<f32>, metric: AdcDistanceMetric) -> Self {
        let sum: f32 = query.iter().sum();
        let norm_sq: f32 = query.iter().map(|&x| x * x).sum();
        Self {
            query,
            sum,
            norm_sq,
            metric,
        }
    }

    /// Create a new query context with default metric (Cosine)
    pub fn with_query(query: Vec<f32>) -> Self {
        Self::new(query, AdcDistanceMetric::default())
    }
}

/// Set the thread-local ADC query context
pub fn set_adc_query_context(query: &[f32], metric: AdcDistanceMetric) {
    ADC_QUERY_CONTEXT.with(|ctx| {
        *ctx.borrow_mut() = Some(AdcQueryContext::new(query.to_vec(), metric));
    });
}

/// Clear the thread-local ADC query context
pub fn clear_adc_query_context() {
    ADC_QUERY_CONTEXT.with(|ctx| {
        *ctx.borrow_mut() = None;
    });
}

/// Get a reference to the current ADC query context
pub fn with_adc_query_context<F, R>(f: F) -> Option<R>
where
    F: FnOnce(&AdcQueryContext) -> R,
{
    ADC_QUERY_CONTEXT.with(|ctx| ctx.borrow().as_ref().map(f))
}

/// Check if an ADC query context is currently set
pub fn has_adc_query_context() -> bool {
    ADC_QUERY_CONTEXT.with(|ctx| ctx.borrow().is_some())
}

fn asymmetric_cosine_distance(query_ctx: &AdcQueryContext, qvec: &Int8QuantizedVector) -> f32 {
    let dot_adc = dot_product_adc(&query_ctx.query, qvec, query_ctx.sum);
    let query_norm = query_ctx.norm_sq.sqrt();
    let stored_norm = qvec.metadata.norm();

    if query_norm == 0.0 || stored_norm == 0.0 {
        return 1.0;
    }

    let cosine_sim = dot_adc / (query_norm * stored_norm);
    (1.0 - cosine_sim).max(0.0)
}

fn asymmetric_l2_squared_distance(query_ctx: &AdcQueryContext, qvec: &Int8QuantizedVector) -> f32 {
    l2_squared_adc(&query_ctx.query, qvec, query_ctx.sum, query_ctx.norm_sq)
}

fn asymmetric_dot_distance(query_ctx: &AdcQueryContext, qvec: &Int8QuantizedVector) -> f32 {
    let dot_adc = dot_product_adc(&query_ctx.query, qvec, query_ctx.sum);
    ((1.0 - dot_adc) / 2.0f32).max(0.0f32)
}

impl Distance<Int8QuantizedVector> for AdcDistanceMetric {
    fn eval(&self, query: &[Int8QuantizedVector], stored: &[Int8QuantizedVector]) -> f32 {
        if let Some(ctx) = with_adc_query_context(|ctx| ctx.clone()) {
            match ctx.metric {
                AdcDistanceMetric::Cosine => asymmetric_cosine_distance(&ctx, &stored[0]),
                AdcDistanceMetric::L2Squared => asymmetric_l2_squared_distance(&ctx, &stored[0]),
                AdcDistanceMetric::Dot => asymmetric_dot_distance(&ctx, &stored[0]),
            }
        } else {
            // Symmetric fallback: INT8 vs INT8 (used during insertion)
            let v1 = &query[0];
            let v2 = &stored[0];
            match self {
                AdcDistanceMetric::Cosine => {
                    let d1 = v1.to_f32();
                    let d2 = v2.to_f32();
                    let mut dot = 0.0;
                    let mut n1 = 0.0;
                    let mut n2 = 0.0;
                    for i in 0..d1.len() {
                        dot += d1[i] * d2[i];
                        n1 += d1[i] * d1[i];
                        n2 += d2[i] * d2[i];
                    }
                    if n1 == 0.0 || n2 == 0.0 {
                        return 1.0;
                    }
                    (1.0 - dot / (n1.sqrt() * n2.sqrt())).max(0.0)
                }
                AdcDistanceMetric::L2Squared => {
                    let d1 = v1.to_f32();
                    let d2 = v2.to_f32();
                    d1.iter()
                        .zip(d2.iter())
                        .map(|(a, b)| (a - b) * (a - b))
                        .sum()
                }
                AdcDistanceMetric::Dot => {
                    let d1 = v1.to_f32();
                    let d2 = v2.to_f32();
                    (1.0 - d1.iter().zip(d2.iter()).map(|(a, b)| a * b).sum::<f32>()).max(0.0)
                }
            }
        }
    }
}

/// Wrapper struct that implements Distance<Int8QuantizedVector> with ADC
#[derive(Debug, Clone, Copy, Default)]
pub struct Int8AdcDistance {
    /// The distance metric to use
    pub metric: AdcDistanceMetric,
}

impl Int8AdcDistance {
    /// Create a new Int8AdcDistance with the given metric
    pub fn new(metric: AdcDistanceMetric) -> Self {
        Self { metric }
    }
    /// Create a new Int8AdcDistance with Cosine metric
    pub fn cosine() -> Self {
        Self::new(AdcDistanceMetric::Cosine)
    }
    /// Create a new Int8AdcDistance with L2 squared metric
    pub fn l2_squared() -> Self {
        Self::new(AdcDistanceMetric::L2Squared)
    }
    /// Create a new Int8AdcDistance with Dot metric
    pub fn dot() -> Self {
        Self::new(AdcDistanceMetric::Dot)
    }
}

impl Distance<Int8QuantizedVector> for Int8AdcDistance {
    fn eval(&self, query: &[Int8QuantizedVector], stored: &[Int8QuantizedVector]) -> f32 {
        self.metric.eval(query, stored)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quantization::Quantize;

    #[test]
    fn test_adc_query_context_creation() {
        let query = vec![0.1, 0.2, 0.3, 0.4];
        let ctx = AdcQueryContext::with_query(query.clone());
        assert_eq!(ctx.query, query);
        assert!((ctx.sum - 1.0).abs() < 1e-6);
        assert!((ctx.norm_sq - 0.3).abs() < 1e-6);
    }

    #[test]
    fn test_set_and_clear_adc_context() {
        assert!(!has_adc_query_context());
        let query = vec![0.1, 0.2, 0.3];
        set_adc_query_context(&query, AdcDistanceMetric::Cosine);
        assert!(has_adc_query_context());
        with_adc_query_context(|ctx| {
            assert_eq!(ctx.query, vec![0.1, 0.2, 0.3]);
        });
        clear_adc_query_context();
        assert!(!has_adc_query_context());
    }

    #[test]
    fn test_int8_adc_distance_trait() {
        let query = vec![1.0, 0.0, 0.0, 0.0];
        set_adc_query_context(&query, AdcDistanceMetric::Cosine);
        let stored_f32 = vec![0.5, 0.5, 0.5, 0.5];
        let stored = stored_f32.quantize();
        let distance_fn = Int8AdcDistance::cosine();
        let distance =
            distance_fn.eval(std::slice::from_ref(&stored), std::slice::from_ref(&stored));
        assert!(distance >= 0.0 && distance <= 1.0);
        clear_adc_query_context();
    }
}
