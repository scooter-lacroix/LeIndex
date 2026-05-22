// ONNX Runtime-based neural embeddings
//
// This module provides the main-daemon side of the ONNX embedding architecture.
// Actual ONNX inference is delegated to the leindex-embed worker process.
// VAL-CPHASE-002: The main crate no longer owns ONNX runtime deps directly.

/// Cross-language semantic chunking for code
///
/// Provides intelligent code chunking that handles language markers,
/// cross-language references, and semantic boundaries for better
/// neural embedding generation.
///
/// This module does not depend on ONNX Runtime directly — it prepares
/// text for embedding but does not run inference.
#[cfg(feature = "onnx")]
pub mod chunking;

/// Worker client for delegating ONNX inference to the leindex-embed process.
///
/// The client manages the worker lifecycle (spawn, reuse, idle teardown)
/// and communicates using the protocol types from the leindex-embed crate.
#[cfg(feature = "onnx")]
pub mod client;

/// Remote embedding providers (OpenAI, Cohere, etc.)
///
/// Provides integration with cloud-based embedding services as an
/// alternative to local ONNX models.
#[cfg(feature = "remote-embeddings")]
pub mod remote;

#[cfg(feature = "onnx")]
pub use chunking::{ChunkConfig, CrossLanguageChunker, SemanticChunk};

#[cfg(feature = "onnx")]
pub use client::{ClientError, EmbedResult, EmbeddingClient};

#[cfg(feature = "remote-embeddings")]
pub use remote::{
    CohereEmbeddingProvider, GenericRemoteProvider, OpenAIEmbeddingProvider, RemoteEmbeddingConfig,
    RemoteEmbeddingError, RemoteProvider,
};
