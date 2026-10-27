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
    /// Maximum number of inputs this provider accepts in ONE request.
    ///
    /// This is a hard protocol limit, not a throughput preference: a request
    /// above it is rejected wholesale with a 4xx that names the cap, so the
    /// only safe batch is one at or below this number. Before this field
    /// existed the batch size was decided purely by the local server's
    /// throughput curve, so a remote generation sent 128 documents to an API
    /// that accepts 100 and every batch failed with
    /// `at most 100 requests can be in one batch`.
    ///
    /// Values are deliberately conservative unless the provider documents an
    /// exact cap — undershooting costs throughput, overshooting costs the whole
    /// batch. An operator can override with `remote_batch_limit`.
    pub batch_limit: usize,
    /// Value for a provider's asymmetric-retrieval "this is a DOCUMENT" field.
    ///
    /// Several OpenAI-compatible providers refuse or silently degrade without
    /// it: Cohere's `input_type` is REQUIRED for embedding models v3 and higher
    /// ("Required for embedding models v3 and higher"), and Voyage's
    /// `input_type: document|query` opts into the retrieval prompt it prepends.
    /// Omitted (skipped on the wire) for providers that do not take the field.
    pub input_type_document: Option<&'static str>,
    /// Value for the same field on the QUERY side.
    pub input_type_query: Option<&'static str>,
    /// Largest number of tokens ONE input may occupy.
    ///
    /// A per-INPUT ceiling. Exceeding it is not a throughput question: the
    /// request is rejected outright, and the client can only recover by
    /// truncating the document (losing its tail) or dropping the row. Knowing
    /// the number up front lets the client truncate deliberately instead of
    /// paying a rejected round trip to discover it.
    pub max_input_tokens: usize,
    /// Largest number of tokens ONE REQUEST may carry in total, across every
    /// input in it.
    ///
    /// This is the limit that row-count caps (`batch_limit`) cannot express, and
    /// conflating the two is what broke hosted EmbeddingGemma: 32 documents of
    /// 882 bytes is ~7k tokens against a 2048-token request window, so every
    /// batch was rejected with `The prompt is too long ... Shorten it and retry`
    /// even though 32 was well under any count cap. Request width must be
    /// packed against this budget, not just against `batch_limit`.
    ///
    /// Values are conservative where the vendor documents no exact total, for
    /// the same reason `batch_limit` undershoots: overshooting costs the whole
    /// request, undershooting only costs a round trip.
    pub request_token_budget: usize,
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
        // Text-only batching requires embedding-001. `gemini-embedding-2`
        // AGGREGATES multiple inputs into one vector and rejects `taskType`
        // outright, so it cannot serve a per-document corpus at all.
        model: "gemini-embedding-001",
        dimension: 3072,
        api_key_env: Some("GEMINI_API_KEY"),
        // Documented hard cap, and the API enforces it with
        // `BatchEmbedContentsRequest.requests: at most 100 requests can be in
        // one batch`. Observed rejecting every 128-document batch.
        batch_limit: 100,
        // Native Gemini expresses this as `taskType`, handled by the provider.
        input_type_document: None,
        input_type_query: None,
        // Documented input limit for embedding-001 is 2048 tokens per text.
        max_input_tokens: 2048,
        // BatchEmbedContents caps the SUM of all inputs against the model's
        // context, and rejects the whole request when it overflows.
        request_token_budget: 8192,
    },
    RemotePreset {
        name: "openai",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.openai.com/v1",
        model: "text-embedding-3-small",
        dimension: 1536,
        api_key_env: Some("OPENAI_API_KEY"),
        // /v1/embeddings accepts 2048 inputs; 1000 keeps one request far from
        // the token ceiling while staying well inside the count ceiling.
        batch_limit: 1000,
        // OpenAI takes no asymmetry field; retrieval shape comes from the
        // model, not the request.
        input_type_document: None,
        input_type_query: None,
        // Documented 8192 tokens per input, same across the 3-* family.
        max_input_tokens: 8191,
        // 300,000 tokens per request is the documented total ceiling.
        request_token_budget: 300_000,
    },
    RemotePreset {
        name: "voyage",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.voyageai.com/v1",
        model: "voyage-code-3",
        dimension: 1024,
        api_key_env: Some("VOYAGE_API_KEY"),
        // Documented maximum of 1000 inputs per request.
        batch_limit: 1000,
        // Voyage prepends its retrieval prompt only when input_type is set.
        input_type_document: Some("document"),
        input_type_query: Some("query"),
        // voyage-code-3 documents a 32,000-token per-input window.
        max_input_tokens: 32_000,
        // The total is bounded by the same 32k window, because one input may
        // legitimately consume all of it.
        request_token_budget: 32_000,
    },
    RemotePreset {
        name: "mistral",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.mistral.ai/v1",
        model: "codestral-embed",
        dimension: 1536,
        api_key_env: Some("MISTRAL_API_KEY"),
        // No documented count cap; bounded by the shared token budget instead.
        batch_limit: 1000,
        input_type_document: None,
        input_type_query: None,
        // codestral-embed documents an 8,192-token per-input window.
        max_input_tokens: 8192,
        // No documented request total; one request is bounded to whatever a
        // single maximum input may carry, which is the safe intersection.
        request_token_budget: 8192,
    },
    RemotePreset {
        name: "jina",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.jina.ai/v1",
        model: "jina-embeddings-v3",
        dimension: 1024,
        api_key_env: Some("JINA_API_KEY"),
        // Documented maximum of 2048 inputs; 1000 leaves token headroom.
        batch_limit: 1000,
        // jina-embeddings-v3 selects a task LoRA; retrieval needs both sides.
        input_type_document: Some("retrieval.passage"),
        input_type_query: Some("retrieval.query"),
        // 8,192 tokens per input.
        max_input_tokens: 8192,
        // No documented request total; bound to one maximum input.
        request_token_budget: 8192,
    },
    RemotePreset {
        name: "together",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.together.xyz/v1",
        model: "BAAI/bge-large-en-v1.5",
        dimension: 1024,
        api_key_env: Some("TOGETHER_API_KEY"),
        batch_limit: 1000,
        input_type_document: None,
        input_type_query: None,
        // BGE-large documents a 512-token per-input window — the tightest here
        // after the embedding models, and the reason a byte-based guess is not
        // good enough (512 tokens is ~2 KB, not 64 KB).
        max_input_tokens: 512,
        // No documented request total; one request is bounded to a single
        // maximum input so a 1000-row batch of long texts cannot overflow.
        request_token_budget: 512,
    },
    RemotePreset {
        name: "nomic",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api-atlas.nomic.ai/v1",
        model: "nomic-embed-text-v1.5",
        dimension: 768,
        api_key_env: Some("NOMIC_API_KEY"),
        // Documented maximum of 1000 inputs.
        batch_limit: 1000,
        input_type_document: None,
        input_type_query: None,
        // 8,192-token per-input window.
        max_input_tokens: 8192,
        // No documented request total; bound to one maximum input.
        request_token_budget: 8192,
    },
    RemotePreset {
        name: "cohere",
        wire: RemoteWire::OpenAiCompat,
        endpoint: "https://api.cohere.ai/v1",
        // Current documented default for /v1/embed. The v2 surface lives at
        // /v2/embed and is a different request shape, so this preset stays on
        // v1 deliberately.
        model: "embed-v5.0-fast",
        dimension: 1024,
        api_key_env: Some("COHERE_API_KEY"),
        // Documented maximum of 96 texts per request — the tightest cap here.
        batch_limit: 96,
        // Cohere: "Required for embedding models v3 and higher." Every current
        // model is v3+, so omitting this is a hard request failure.
        input_type_document: Some("search_document"),
        input_type_query: Some("search_query"),
        // 512 tokens per input for the v3 family.
        max_input_tokens: 512,
        // No documented request total; bound to one maximum input.
        request_token_budget: 512,
    },
];

// Deliberately NOT a preset: `embeddinggemma-2` / `embeddinggemma-300m`.
//
// They already have a dedicated route in `EmbeddingConfig::resolved_remote_with`
// (endpoint, model, dimension, task templates, token budgets), and that route is
// keyed on the provider name OR the model name, so it covers the `custom` +
// `remote_model = "embeddinggemma-2"` case too.
//
// Adding them here would actively break things: `resolved_remote_with` fills
// `batch_limit` from `preset.batch_limit`, so a preset would force a cap onto
// the LOCAL managed llama-server default (where `batch_limit` is currently
// `None`, meaning one big request). That would chunk the local path into small
// serial requests — a throughput regression on the exact path the concurrency
// work leaves byte-identical. The limits belong in the gemma branch, not in a
// preset row.
