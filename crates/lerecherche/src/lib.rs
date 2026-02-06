//! lerecherche - Search & Analysis Fusion
//!
//! *La Recherche* (The Search) - Node-level semantic search with vector-AST synergy

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Hierarchical Navigable Small World (HNSW) implementation for vector search.
pub mod hnsw;
/// Query parsing and intent detection.
pub mod query;
/// Hybrid ranking and scoring algorithms.
pub mod ranking;
/// Unified search engine combining keyword and semantic search.
pub mod search;
/// Semantic analysis and embedding generation.
pub mod semantic;
/// Tiered HNSW + Turso vector indexing.
pub mod tiered;
/// Vector storage and indexing.
pub mod vector;

pub use hnsw::{HNSWIndex, HNSWParams, IndexError};
pub use query::{ParsedQuery, QueryIntent, QueryParser};
pub use ranking::{HybridScorer, Score};
pub use search::{NodeInfo, SearchEngine, SearchQuery, SearchResult, SemanticEntry};
pub use semantic::SemanticProcessor;
pub use tiered::{TieredHnswConfig, TieredHnswIndex, DEFAULT_HOT_VECTOR_MEMORY_BYTES};
pub use vector::VectorIndex;

/// Search library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
