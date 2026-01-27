//! lerecherche - Search & Analysis Fusion
//!
//! *La Recherche* (The Search) - Node-level semantic search with vector-AST synergy

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Unified search engine combining keyword and semantic search.
pub mod search;
/// Semantic analysis and embedding generation.
pub mod semantic;
/// Hybrid ranking and scoring algorithms.
pub mod ranking;
/// Vector storage and indexing.
pub mod vector;
/// Query parsing and intent detection.
pub mod query;
/// Hierarchical Navigable Small World (HNSW) implementation for vector search.
pub mod hnsw;

pub use search::{SearchEngine, SearchResult, SearchQuery, SemanticEntry, NodeInfo};
pub use semantic::SemanticProcessor;
pub use ranking::{HybridScorer, Score};
pub use vector::VectorIndex;
pub use query::{QueryParser, ParsedQuery, QueryIntent};
pub use hnsw::{HNSWIndex, HNSWParams, IndexError};

/// Search library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
