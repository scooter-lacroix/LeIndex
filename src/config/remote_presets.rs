//! Remote embedding provider presets.
//!
//! Extracted from `config.rs` by pure code motion (Large File Detection gate:
//! `config.rs` crossed 2000 lines). Re-exported from `config` so
//! `config::REMOTE_PRESETS`, `config::RemotePreset` and `config::RemoteWire`
//! keep their paths.

/// Wire family a remote provider's endpoint speaks.
///
/// Two families cover every supported provider, and the split is not cosmetic:
/// it decides which request type the provider is built from. `OpenAiCompat` is
/// the `/v1/embeddings` contract (`{"model","input"}` out,
/// `{"data":[{"index","embedding"}]}` back) that Gemini's compat surface,
/// Voyage, Mistral, Jina, Together and Nomic all implement, so those presets
/// are pure data on the existing [`crate::search::onnx::CustomEmbeddingProvider`]
/// path. `Gemini` is Google's native `v1beta/models/{model}:batchEmbedContents`,
/// whose response carries no per-row `index` and whose `taskType` field is the
/// asymmetric-retrieval lever — neither of which the compat contract can
/// express, so it needs its own implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteWire {
    /// OpenAI-compatible `/v1/embeddings`.
    OpenAiCompat,
    /// Native Google `v1beta` `embedContent` / `batchEmbedContents`.
    Gemini,
}

/// One known remote embedding provider: the defaults a preset fills in.
///
/// Presets exist so a provider name alone reaches a working route:
/// `remote_provider = "gemini"` supplies the endpoint, model, native dimension
/// and conventional API-key variable. Every field is only a default — an
/// explicitly configured `remote_endpoint` / `remote_model` /
/// `remote_dimension` / `remote_api_key_env` always wins (see
/// [`super::EmbeddingConfig::resolved_remote_with`]).
#[derive(Debug, Clone, Copy)]
pub struct RemotePreset {
    /// Config spelling: the `remote_provider` value and `--embedding <name>`.
    pub name: &'static str,
    /// Wire family the endpoint speaks; selects the provider implementation.
    pub wire: RemoteWire,
    /// Default base URL, including any version path segment.
    pub endpoint: &'static str,
    /// Default model identifier.
    pub model: &'static str,
    /// Native (untruncated) embedding dimension.
    ///
    /// This is a BOOTSTRAP value, consulted only before the first successful
    /// response: the length a provider actually returns is measured and wins
    /// over this number, and any later response of a different length is
    /// rejected rather than written. A stale constant therefore degrades to a
    /// wrong starting guess, never to a corrupt vector space.
    pub dimension: usize,
    /// Environment variable conventionally holding this provider's API key.
    pub api_key_env: Option<&'static str>,
}

/// Every provider `leindex setup` can configure, one row each.
///
/// Dimensions are each model's native (untruncated) width, so an unset
/// `remote_dimension` boots at full fidelity instead of the generic 1536 that
/// [`crate::search::onnx::RemoteProvider::default_dimension`] falls back to for
/// anything it does not recognize.
pub const REMOTE_PRESETS: &[RemotePreset] = &[
    RemotePreset {
        name: "gemini",
        wire: RemoteWire::Gemini,
        // Native v1beta surface, not the `/v1beta/openai/` compat shim.
        endpoint: "https://generativelanguage.googleapis.com/v1beta",
        model: "gemini-embedding-001",
        dimension: 3072,
        api_key_env: Some("GEMINI_API_KEY"),
    },
    RemotePreset {
        name: "openai",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.openai.com/v1",
        model: "text-embedding-3-small",
        dimension: 1536,
        api_key_env: Some("OPENAI_API_KEY"),
    },
    RemotePreset {
        name: "voyage",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.voyageai.com/v1",
        model: "voyage-code-3",
        dimension: 1024,
        api_key_env: Some("VOYAGE_API_KEY"),
    },
    RemotePreset {
        name: "mistral",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.mistral.ai/v1",
        model: "codestral-embed",
        dimension: 1536,
        api_key_env: Some("MISTRAL_API_KEY"),
    },
    RemotePreset {
        name: "jina",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.jina.ai/v1",
        model: "jina-embeddings-v3",
        dimension: 1024,
        api_key_env: Some("JINA_API_KEY"),
    },
    RemotePreset {
        name: "together",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.together.xyz/v1",
        model: "BAAI/bge-large-en-v1.5",
        dimension: 1024,
        api_key_env: Some("TOGETHER_API_KEY"),
    },
    RemotePreset {
        name: "nomic",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api-atlas.nomic.ai/v1",
        model: "nomic-embed-text-v1.5",
        dimension: 768,
        api_key_env: Some("NOMIC_API_KEY"),
    },
    RemotePreset {
        name: "cohere",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.cohere.ai/v1",
        model: "embed-english-v3.0",
        dimension: 1024,
        api_key_env: Some("COHERE_API_KEY"),
    },
];
