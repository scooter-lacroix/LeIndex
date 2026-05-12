// ONNX Runtime-based neural embeddings
//
// This module provides true neural embeddings using ONNX Runtime with
// Qwen3 models for cross-language semantic understanding.

/// Cross-language semantic chunking for code
///
/// Provides intelligent code chunking that handles language markers,
/// cross-language references, and semantic boundaries for better
/// neural embedding generation.
#[cfg(feature = "onnx")]
pub mod chunking;

/// Qwen3 embedding provider using ONNX Runtime
///
/// Implements neural embeddings using the Qwen3-Embedding-0.6B model
/// for cross-language code understanding and semantic search.
#[cfg(feature = "onnx")]
pub mod qwen;

/// Qwen3 reranker for quality-aware result refinement
///
/// Provides optional reranking of search results using the Qwen3-Reranker-0.6B
/// model for improved relevance scoring and result ordering.
#[cfg(feature = "onnx")]
pub mod reranker;

/// Remote embedding providers (OpenAI, Cohere, etc.)
///
/// Provides integration with cloud-based embedding services as an
/// alternative to local ONNX models.
#[cfg(feature = "remote-embeddings")]
pub mod remote;

#[cfg(feature = "onnx")]
pub use chunking::{ChunkConfig, CrossLanguageChunker, SemanticChunk};
#[cfg(feature = "onnx")]
pub use qwen::{QwenEmbeddingProvider, QwenEmbeddingProviderError};
#[cfg(feature = "onnx")]
pub use reranker::{QwenReranker, QwenRerankerError};

#[cfg(feature = "remote-embeddings")]
pub use remote::{
    CohereEmbeddingProvider, GenericRemoteProvider, OpenAIEmbeddingProvider,
    RemoteEmbeddingConfig, RemoteEmbeddingError, RemoteProvider,
};
