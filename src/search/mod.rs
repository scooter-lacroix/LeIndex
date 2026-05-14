//! lerecherche - Search & Analysis Fusion
//!
//! *La Recherche* (The Search) - Node-level semantic search with vector-AST synergy

#![warn(missing_docs)]
#![warn(unused_extern_crates)]

/// Hierarchical Navigable Small World (HNSW) implementation for vector search.
pub mod hnsw;
/// INT8 Quantization system.
pub mod quantization;
/// Query parsing and intent detection.
pub mod query;
/// Hybrid ranking and scoring algorithms.
pub mod ranking;
/// Unified search engine combining keyword and semantic search.
#[allow(clippy::module_inception)]
pub mod search;
/// Semantic analysis and embedding generation.
pub mod semantic;
/// Vector storage and indexing.
pub mod vector;

/// ONNX Runtime neural embeddings (R15 - optional feature)
#[cfg(feature = "onnx")]
pub mod onnx;

pub use hnsw::{HNSWIndex, HNSWParams, IndexError};
pub use query::{ParsedQuery, QueryIntent, QueryParser};
pub use ranking::{HybridScorer, Score};
pub use search::{
    CompactNodeMetadata, CompactTokenIndex, ContentPruner, IndexingAdmissionGate,
    Int8PromotionDecision, Int8QualityGate, Int8QualityReport, Int8QualityThresholds, NodeInfo,
    PruningDecision, SearchEngine, SearchQuery, SearchResult, SemanticEntry, StagedRetrievalConfig,
    StagedRetrievalMetrics, WorkHoister,
};
pub use semantic::SemanticProcessor;
pub use vector::VectorIndex;

#[cfg(feature = "onnx")]
pub use onnx::EmbeddingClient;

#[cfg(all(feature = "remote-embeddings", feature = "onnx"))]
pub use onnx::{
    GenericRemoteProvider, OpenAIEmbeddingProvider, RemoteEmbeddingConfig, RemoteEmbeddingError,
    RemoteProvider,
};

/// Search library initialization
pub fn init() {
    let _ = tracing::subscriber::set_default(tracing::subscriber::NoSubscriber::default());
}
