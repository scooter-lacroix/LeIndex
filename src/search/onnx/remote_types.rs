//! Error and provider-selector types for the remote embedding route.
//!
//! Moved out of `remote.rs` by pure code motion (Large File Detection gate:
//! `remote.rs` crossed 2000 lines). Re-exported from the parent so every
//! existing path keeps resolving.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur during remote embedding generation
#[derive(Debug, Error)]
pub enum RemoteEmbeddingError {
    /// HTTP client error
    #[error("HTTP client error: {0}")]
    HttpClient(String),

    /// Transport-level failure: the request never produced a usable response
    /// (connection refused/reset, dropped mid-response, DNS failure, timeout).
    ///
    /// This is transient by nature and says nothing about the embedding space,
    /// so callers retry it. A reachable-but-unhappy endpoint reports
    /// [`Self::HttpStatus`] instead, which is the signal for a permanent
    /// freeze. Lumping the two together as `ApiError` is what made one dropped
    /// connection discard every remaining document in a generation.
    #[error("transport failure: {0}")]
    Transport(String),

    /// API request failed
    #[error("API request failed: {0}")]
    ApiError(String),

    /// HTTP request returned an unsuccessful status.
    #[error("API returned HTTP {status}: {message}")]
    HttpStatus {
        /// HTTP status code returned by the remote endpoint.
        status: u16,
        /// Error message body returned with the status.
        message: String,
    },

    /// Invalid API response
    #[error("Invalid API response: {0}")]
    InvalidResponse(String),

    /// API key not configured
    #[error("API key not configured for provider: {0}")]
    ApiKeyNotFound(String),

    /// Rate limit exceeded
    #[error("Rate limit exceeded for provider: {0}")]
    RateLimitExceeded(String),

    /// Invalid embedding dimension
    #[error("Invalid embedding dimension: expected {expected}, got {got}")]
    InvalidDimension {
        /// Expected embedding dimension from the provider
        expected: usize,
        /// Actual embedding dimension received
        got: usize,
    },

    /// Feature not enabled
    #[error("Feature not enabled: remote-embeddings feature is required")]
    FeatureNotEnabled,
}

/// Remote embedding provider types
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RemoteProvider {
    /// OpenAI embeddings (text-embedding-3-small, text-embedding-3-large)
    OpenAI {
        /// Model name (e.g., "text-embedding-3-small")
        model: String,
    },
    /// Cohere embeddings (embed-english-v3.0, embed-multilingual-v3.0)
    Cohere {
        /// Model name (e.g., "embed-english-v3.0")
        model: String,
    },
    /// Google Gemini embeddings, native `v1beta` surface
    /// (`gemini-embedding-001`, `text-embedding-004`).
    ///
    /// Distinct from the OpenAI-compatible variant because the native protocol
    /// is not `/v1/embeddings`: requests go to
    /// `models/{model}:batchEmbedContents` (or `:embedContent` for one text),
    /// `taskType` carries the asymmetric doc/query intent instead of a template,
    /// `outputDimensionality` carries the MRL truncation, and responses are
    /// ordered by request position with no per-row `index` to reorder by. See
    /// [`super::GeminiEmbeddingProvider`].
    Gemini {
        /// Model name (e.g., "gemini-embedding-001")
        model: String,
    },
    /// Custom HTTP endpoint
    Custom {
        /// Custom HTTP endpoint URL
        endpoint: String,
    },
}

impl Default for RemoteProvider {
    fn default() -> Self {
        Self::OpenAI {
            model: "text-embedding-3-small".to_string(),
        }
    }
}

impl RemoteProvider {
    /// Get the default embedding dimension for this provider
    pub fn default_dimension(&self) -> usize {
        match self {
            Self::OpenAI { model } => match model.as_str() {
                "text-embedding-3-small" => 1536,
                "text-embedding-3-large" => 3072,
                _ => 1536,
            },
            Self::Cohere { model } => match model.as_str() {
                "embed-english-v3.0" => 1024,
                "embed-multilingual-v3.0" => 1024,
                _ => 1024,
            },
            // Native (untruncated) widths. `gemini-embedding-001` and
            // `text-embedding-004` both default to 768 when the request omits
            // `outputDimensionality`; 001 can emit up to 3072 on request. The
            // unset-override default is the 3072 maximum because the indexer
            // requests full width unless MRL truncation is explicitly
            // configured — bootstrapping at 768 would describe a narrower
            // vector than the endpoint actually returns. Both are bootstrap
            // guesses: the first response's length is measured and wins.
            Self::Gemini { model } => match model.as_str() {
                "text-embedding-004" | "embedding-001" => 768,
                _ => 3072,
            },
            Self::Custom { .. } => 1536, // Default assumption for custom endpoints
        }
    }

    /// Default API base URL for providers whose endpoint is implied by the
    /// provider itself. `None` means the endpoint must be supplied (a custom
    /// endpoint has no default; OpenAI/Cohere historically hardcoded theirs).
    pub fn default_base_url(&self) -> Option<&'static str> {
        match self {
            Self::OpenAI { .. } => Some("https://api.openai.com/v1"),
            Self::Cohere { .. } => Some("https://api.cohere.ai/v1"),
            // The native v1beta surface, not the `/v1beta/openai/` compat shim:
            // the compat path would force `CustomEmbeddingProvider` and lose
            // `taskType`.
            Self::Gemini { .. } => Some("https://generativelanguage.googleapis.com/v1beta"),
            Self::Custom { .. } => None,
        }
    }
}
