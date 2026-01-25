// lerecherche - Search & Analysis Fusion
//
// *La Recherche* (The Search) - Node-level semantic search with vector-AST synergy

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

pub mod search;
pub mod semantic;
pub mod ranking;

pub use search::{SearchEngine, SearchResult, SearchQuery, SemanticEntry};
pub use semantic::SemanticProcessor;
pub use ranking::{HybridScorer, Score};

/// Search library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
