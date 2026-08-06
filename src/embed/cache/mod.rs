//! Global content-addressed embedding cache (WS10 Tasks 1-3, 6).
//!
//! Provides `CacheKey` (the spec §6.5 6-tuple) and `GlobalEmbeddingCache`
//! (mmap vector rows with probe, put, gc, and cross-project dedup).
//! Byte-budgeted compaction with telemetry counters (spec §10.3).
//!
//! Feature-flagged behind `LEINDEX_FEATURE_GLOBAL_EMBED_CACHE`.

pub mod key;
pub mod store;

pub use key::{CacheKey, Normalization, Pooling};
pub use store::{
    CacheCompactionReport, CacheConfig, CacheError, CacheStatsReport, CacheTelemetry,
    GlobalEmbeddingCache, ProbeResult, ProjectRefs,
};
