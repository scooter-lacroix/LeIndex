// lerecherche - Search & Analysis Fusion
//
// *La Recherche* (The Search) - Node-level semantic search with vector-AST synergy

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod search;
pub mod semantic;
pub mod ranking;
pub mod vector;
pub mod query;
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
