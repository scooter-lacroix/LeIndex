// Remote Embedding Providers
//
// Provides integration with cloud-based embedding services like OpenAI,
// Cohere, and other API-based embedding providers as an alternative to local ONNX models.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;

/// Endpoint model discovery (`models_url` / `list_models`) and its tests.
#[path = "remote_models.rs"]
mod remote_models;

/// Served-model modality heuristic, re-exported for the setup picker, which
/// filters a listing to the embedding models a user actually wants to choose
/// from without ever hiding the unfiltered list.
pub(crate) use remote_models::looks_like_embedding_model;

/// Error and provider-selector types (pure code motion; see the module docs).
#[path = "remote_types.rs"]
mod remote_types;
pub use remote_types::{RemoteEmbeddingError, RemoteProvider};

/// Native Gemini provider (pure code motion; see the module docs).
#[path = "remote_gemini.rs"]
mod remote_gemini;
pub use remote_gemini::GeminiEmbeddingProvider;

/// Task-template rendering (`{text}` / `{title}`) and its tests.
#[path = "remote_template.rs"]
mod remote_template;
pub(crate) use remote_template::{apply_doc_template, apply_template};

/// Hosted providers (OpenAI, Cohere) and the retry ladder only they use.
#[path = "remote_cloud.rs"]
mod remote_cloud;
pub use remote_cloud::{CohereEmbeddingProvider, OpenAIEmbeddingProvider};

/// Request shaping (token estimate, truncation, request width).
#[path = "remote_budget.rs"]
mod remote_budget;
pub(crate) use remote_budget::{
    endpoint_is_local, remote_max_in_flight, request_width_for, truncate_to_token_budget,
};

/// Exponential backoff with a cap: 250ms, 500ms, 1s, 2s, then 2s.
///
/// Deliberately small. A reset socket or a saturated queue clears in well under
/// a second; anything larger just hides a dead endpoint behind a longer stall.
///
/// Lives here rather than beside its callers because both the hosted retry
/// ladder (`remote_cloud`) and the custom/Gemini ladders use it.
pub(crate) fn retry_backoff(attempt: usize) -> std::time::Duration {
    const BASE_MS: u64 = 250;
    const CAP_MS: u64 = 2_000;
    std::time::Duration::from_millis(BASE_MS.saturating_mul(1u64 << attempt.min(3)).min(CAP_MS))
}

/// Remote embedding provider configuration
#[derive(Debug, Clone)]
pub struct RemoteEmbeddingConfig {
    /// Provider type
    pub provider: RemoteProvider,
    /// API key (for providers that require authentication)
    pub api_key: Option<String>,
    /// Request timeout in seconds.
    ///
    /// This is a dead-endpoint detector, not a batch-size ceiling. It must be
    /// long enough that a `max_texts`-sized batch of longest-legal documents
    /// always finishes: budgeting a request against a deadline it cannot meet
    /// converts a slow-but-correct server into a permanent, silent fallback to
    /// TF-IDF (measured: a 256-document batch against llama-server blew a 30s
    /// deadline mid-generation, freezing 70% of the corpus out of the neural
    /// layer). Prefer shrinking the batch, not this.
    pub timeout_secs: u64,
    /// Retries per request for failures the transport can heal on its own:
    /// connect errors, reset/closed sockets, truncated bodies, and
    /// availability statuses. Definitive rejections — a 4xx naming a bad input,
    /// or a 5xx whose answer is fixed for identical bytes (see
    /// [`is_retryable_status`]) — are never retried; they are bisected instead.
    ///
    /// This does NOT bound total wall clock. A remote endpoint additionally
    /// caps each attempt at `timeout_secs`, so a dead third-party host fails
    /// within `(max_retries + 1) * timeout_secs`. A local endpoint has no
    /// whole-request deadline (see [`Self::is_local_endpoint`]): attempts are
    /// capped only by connect timeout and TCP keepalive, so what stops a dead
    /// local server is the caller's sustained-failure budget, not a deadline.
    ///
    /// `0` means "never re-send a FAILED request". It does not disable
    /// truncating an oversized document and re-sending the shorter text: that is
    /// a deterministic reformulation bounded by [`MAX_OVERSIZE_SHRINKS`], not a
    /// retry, and suppressing it would drop a row the endpoint can actually
    /// serve.
    pub max_retries: usize,
    /// Base URL for custom providers
    pub base_url: Option<String>,
    /// Model name sent as the "model" field of the /v1/embeddings payload
    /// (custom providers; llama-server accepts the served model name or alias).
    pub model: Option<String>,
    /// Dimension override for custom providers. When unset, the dimension is
    /// measured from the first successful embed response.
    pub remote_dimension: Option<usize>,
    /// Template applied to DOCUMENT texts before embedding (`{text}`
    /// placeholder; a prefix-only template may omit it). `None` = raw text.
    pub doc_template: Option<String>,
    /// Template applied to QUERY texts (asymmetric retrieval prefixes).
    /// Same `{text}` contract. `None` = raw text.
    pub query_template: Option<String>,
    /// Name of the local `[services.<name>]` entry this endpoint belongs to,
    /// when LeIndex launched it. `None` for a third-party endpoint.
    ///
    /// This is what makes a managed endpoint self-healing: LeIndex owns the
    /// process (via `auto_start`), so a sustained outage can be fixed by
    /// restarting it instead of abandoning the neural layer. Set by the index
    /// builder, which is the only place that knows the endpoint's origin.
    pub service_name: Option<String>,
    /// Hard cap on inputs per request, or `None` for no cap.
    ///
    /// Distinct from the caller's `BatchBudget`: that decides how many texts a
    /// batch SHOULD carry for throughput, this decides how many the endpoint
    /// will ACCEPT. The two were conflated, so the throughput-tuned 128 went
    /// straight into a 100-cap API and every batch was rejected with
    /// `at most 100 requests can be in one batch`. Filled from the provider
    /// preset and overridable via `remote_batch_limit`.
    ///
    /// A cap above the endpoint's real limit is still fatal to the batch, and
    /// a cap below it only costs throughput — which is why the presets
    /// undershoot rather than gamble.
    pub batch_limit: Option<usize>,
    /// Value for the provider's `input_type` field on DOCUMENT requests.
    ///
    /// Cohere states `input_type` is "Required for embedding models v3 and
    /// higher", so omitting it is a hard failure there; Voyage and Jina use it
    /// to opt into their retrieval prompts / task LoRAs, where omitting it
    /// silently costs accuracy instead. `None` omits the field from the wire.
    pub input_type_document: Option<String>,
    /// Value for the same field on QUERY requests.
    pub input_type_query: Option<String>,
    /// Token ceiling for ONE input, or `None` for no limit.
    ///
    /// Distinct from `batch_limit`, which counts DOCUMENTS. A single input can
    /// exceed a model's window on its own, and the endpoint then rejects the
    /// whole request; knowing the ceiling lets the client truncate that document
    /// deliberately instead of losing the round trip.
    pub max_input_tokens: Option<usize>,
    /// Token ceiling for ONE REQUEST (all inputs summed), or `None` for no
    /// limit.
    ///
    /// This is the constraint `batch_limit` cannot express, and it is what
    /// request width must actually be packed against. Hosted EmbeddingGemma
    /// rejects anything over 2,048 tokens TOTAL, so a 32-document batch of
    /// ~882 B documents (~7k tokens) fails even though 32 is under the count
    /// cap. Without this the only way to discover the ceiling is a rejection.
    pub request_token_budget: Option<usize>,
}

impl Default for RemoteEmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: RemoteProvider::default(),
            api_key: None,
            timeout_secs: 30,
            max_retries: 3,
            base_url: None,
            model: None,
            remote_dimension: None,
            doc_template: None,
            query_template: None,
            service_name: None,
            batch_limit: None,
            input_type_document: None,
            input_type_query: None,
            max_input_tokens: None,
            request_token_budget: None,
        }
    }
}

impl RemoteEmbeddingConfig {
    /// Create configuration for OpenAI embeddings
    pub fn openai(api_key: String, model: Option<String>) -> Self {
        Self {
            provider: RemoteProvider::OpenAI {
                model: model.unwrap_or_else(|| "text-embedding-3-small".to_string()),
            },
            api_key: Some(api_key),
            ..Default::default()
        }
    }

    /// Create configuration for Cohere embeddings
    pub fn cohere(api_key: String, model: Option<String>) -> Self {
        Self {
            provider: RemoteProvider::Cohere {
                model: model.unwrap_or_else(|| "embed-english-v3.0".to_string()),
            },
            api_key: Some(api_key),
            base_url: Some("https://api.cohere.ai/v1".to_string()),
            ..Default::default()
        }
    }

    /// Create configuration for custom HTTP endpoint
    pub fn custom(endpoint: String, api_key: Option<String>) -> Self {
        Self {
            provider: RemoteProvider::Custom { endpoint },
            api_key,
            ..Default::default()
        }
    }

    /// Build a remote config from a resolved `[indexing.embeddings]` route.
    ///
    /// The ONE place that decides which [`RemoteProvider`] variant a resolved
    /// config maps to. Before this existed, every construction site
    /// (`index_builder::remote`, `leindex::indexing::load`, the setup smoke
    /// test) hardcoded `RemoteProvider::Custom`, so a config naming `gemini`
    /// silently ran the OpenAI-compatible path: the preset's endpoint and model
    /// were honored but the wire format and `taskType` were not. Consolidating
    /// it here means a new provider cannot be forgotten at a call site.
    ///
    /// `endpoint` is the resolved endpoint ladder's winner; it becomes the
    /// variant's identity for the OpenAI-compatible path and the override base
    /// URL for Gemini (needed so a proxy or regional host still works).
    pub fn from_resolved(
        resolved: &crate::config::ResolvedRemote,
        api_key: Option<String>,
        service_name: Option<String>,
    ) -> Self {
        let model = resolved.model.clone().unwrap_or_default();
        let provider = match resolved.wire {
            crate::config::RemoteWire::Gemini => RemoteProvider::Gemini { model },
            crate::config::RemoteWire::OpenAiCompat => RemoteProvider::Custom {
                endpoint: resolved.endpoint.clone(),
            },
        };
        Self {
            provider,
            api_key,
            model: resolved.model.clone(),
            remote_dimension: resolved.remote_dimension,
            doc_template: resolved.doc_template.clone(),
            query_template: resolved.query_template.clone(),
            // Only Gemini consumes `base_url` (an override for its default);
            // the OpenAI-compatible path already carries the endpoint in the
            // variant, and setting both would key the identity on the wrong
            // string.
            base_url: matches!(resolved.wire, crate::config::RemoteWire::Gemini)
                .then(|| resolved.endpoint.clone()),
            service_name,
            batch_limit: resolved.batch_limit,
            input_type_document: resolved.input_type_document.clone(),
            input_type_query: resolved.input_type_query.clone(),
            max_input_tokens: resolved.max_input_tokens,
            request_token_budget: resolved.request_token_budget,
            ..Self::default()
        }
    }

    // `models_url` and `list_models` live in `remote_models.rs`.

    /// True when the endpoint cannot leave this machine — the case where a
    /// whole-request deadline buys nothing and costs correctness.
    pub(crate) fn is_local_endpoint(&self) -> bool {
        let endpoint = match &self.provider {
            RemoteProvider::Custom { endpoint } => Some(endpoint.as_str()),
            _ => None,
        };
        let Some(endpoint) = endpoint.or(self.base_url.as_deref()) else {
            return false;
        };
        let authority = endpoint
            .split_once("://")
            .map_or(endpoint, |(_, rest)| rest)
            .split('/')
            .next()
            .unwrap_or_default();
        // Strip the PORT before matching. An endpoint is written as
        // `host:port`, so comparing the whole authority only ever matched a
        // host with no port at all: `localhost:11434` and `[::1]:8766` fell
        // through to the remote branch and silently regained the wall-clock
        // deadline this predicate exists to remove. Only bare `127.x` happened
        // to work, and only because of the `starts_with` fallback below.
        let host = if let Some(bracketed) = authority.strip_prefix('[') {
            // `[::1]:8766` -> `::1`
            bracketed.split(']').next().unwrap_or_default()
        } else {
            // `localhost:11434` -> `localhost`
            authority.split(':').next().unwrap_or_default()
        };
        matches!(host, "localhost" | "127.0.0.1" | "0.0.0.0" | "::1") || host.starts_with("127.")
    }
}

/// How long an idle keep-alive socket may be reused, per endpoint class.
///
/// Local endpoints keep `ZERO`. llama-server (and single-model
/// OpenAI-compatible servers generally) drop a kept-alive connection as soon as
/// client and server disagree about liveness, and reqwest then surfaces the NEXT
/// request on that dead socket as `error sending request for url (...)` — a
/// transport failure with no server answer behind it. Measured on a 523-batch
/// index run: one stale-socket reuse aborted the batch 9 minutes in. A fresh
/// connection per batch costs a localhost handshake — sub-millisecond — against
/// a request that takes hundreds of milliseconds anyway.
///
/// A REMOTE endpoint is the opposite trade. A TLS handshake to another host is
/// 2-4 round trips plus real CPU, paid once per batch: 7,495 rows at 128
/// texts/batch is ~59 batches, so reuse removes ~59 handshakes from the critical
/// path. The stale-socket hazard that forces `ZERO` locally does not carry over —
/// a general HTTP server honours its keep-alive contract, and a reset socket is
/// absorbed by `with_transient_retry` (`Transport` is retryable), so the failure
/// degrades to one retry instead of a frozen run.
fn pool_idle_timeout_for(config: &RemoteEmbeddingConfig) -> std::time::Duration {
    if config.is_local_endpoint() {
        std::time::Duration::ZERO
    } else {
        // Comfortably longer than one batch's compute so consecutive batches
        // reuse the socket, short enough that a finished run releases its
        // connections promptly.
        std::time::Duration::from_secs(90)
    }
}

/// Idle sockets kept per host.
///
/// `0` for local endpoints, matching the disabled idle timeout above. Remote
/// endpoints keep enough for the neural path's in-flight batches: a single idle
/// socket would funnel every request back onto one connection and throw away
/// reuse as soon as two batches overlap.
fn pool_max_idle_per_host_for(config: &RemoteEmbeddingConfig) -> usize {
    if config.is_local_endpoint() { 0 } else { 4 }
}

/// Build the HTTP client every provider shares.
///
/// Connection lifetime is decided by [`pool_idle_timeout_for`]: disabled for
/// local endpoints (stale-socket hazard above), enabled for remote ones
/// (handshake cost dominates). Everything else here is shared.
fn build_http_client(config: &RemoteEmbeddingConfig) -> Result<Client, RemoteEmbeddingError> {
    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let mut builder = Client::builder()
        .connect_timeout(timeout)
        .pool_idle_timeout(pool_idle_timeout_for(config))
        .pool_max_idle_per_host(pool_max_idle_per_host_for(config));
    // Local endpoints carry no whole-request deadline: the response arrives
    // when the server has finished the batch, and the batch size is already
    // bounded by `BatchBudget`. A wall-clock cap here is a false-negative
    // generator — it cannot tell a dead endpoint from a big-but-valid request.
    // Remote APIs keep theirs: an unresponsive third-party host must not hang
    // an entire index run.
    if !config.is_local_endpoint() {
        builder = builder.timeout(timeout);
    } else {
        // Liveness without a deadline: the kernel keeps probing the connection
        // while the server is still computing, so a host that vanished
        // mid-batch surfaces as a socket error instead of a request that never
        // returns. These are the bare `Duration` setters reqwest 0.12 exposes
        // (`TcpKeepalive` is the 0.13 API); the builder rejects an idle time
        // with no interval, so both are always set together.
        builder = builder
            .tcp_keepalive(timeout)
            .tcp_keepalive_interval(timeout)
            .tcp_keepalive_retries(3);
    }
    builder
        .build()
        .map_err(|e| RemoteEmbeddingError::HttpClient(e.to_string()))
}

/// Trait for remote embedding providers
// `async_trait` (0.1.91) unconditionally stamps a bare `#[must_use]` on every
// generated trait method whose return type is `Pin<Box<dyn Future>>`, which is
// already `#[must_use]`; clippy 1.99's `double_must_use` flags that expansion.
// The duplicate is emitted by the macro, not by this declaration, so it is
// allowed here at the single trait that triggers it.
#[allow(clippy::double_must_use)]
#[async_trait]
pub trait RemoteEmbeddingProvider: Send + Sync {
    /// Generate embeddings for a single text
    async fn embed(&self, text: &str) -> Result<Vec<f32>, RemoteEmbeddingError>;

    /// Generate embeddings for multiple texts (batch)
    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError>;

    /// Generate a document batch while preserving missing input rows.
    async fn embed_batch_optional(
        &self,
        texts: Vec<&str>,
    ) -> Result<Vec<Option<Vec<f32>>>, RemoteEmbeddingError> {
        self.embed_batch(texts)
            .await
            .map(|embeddings| embeddings.into_iter().map(Some).collect())
    }

    /// Generate a document batch with per-document titles (EmbeddingGemma2
    /// document inputs are `title: {title} | text: {text}`). Providers that
    /// own a document template should override this; the default forwards to
    /// [`Self::embed_batch`] and ignores titles (title-less templates). Rows
    /// are full vectors — skipped rows are empty (same contract as
    /// [`Self::embed_batch`]).
    async fn embed_batch_titled(
        &self,
        texts: Vec<&str>,
        titles: std::iter::Peekable<std::vec::IntoIter<Option<String>>>,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        let rows = self.embed_batch_optional_titled(texts, titles).await?;
        Ok(rows
            .into_iter()
            .map(|row| row.unwrap_or_default())
            .collect())
    }

    /// Titled counterpart of [`Self::embed_batch_optional`]: skipped rows
    /// stay `None` instead of collapsing to empty vectors. Providers that do
    /// not override it degrade to the untitled batch (titles ignored).
    async fn embed_batch_optional_titled(
        &self,
        texts: Vec<&str>,
        titles: std::iter::Peekable<std::vec::IntoIter<Option<String>>>,
    ) -> Result<Vec<Option<Vec<f32>>>, RemoteEmbeddingError> {
        let _ = titles;
        self.embed_batch_optional(texts).await
    }

    /// Get the embedding dimension
    fn dimension(&self) -> usize;

    /// Runtime-true dimension when the provider knows better than the legacy
    /// `default_dimension()` table: the configured override or a dimension
    /// measured from a real response. `None` = no better information (the
    /// legacy default stays authoritative and the identity string is stable).
    fn measured_dimension(&self) -> Option<usize> {
        None
    }

    /// Dimension observed in a successful response; unlike
    /// `measured_dimension`, this never reports a configured override.
    fn observed_dimension(&self) -> Option<usize> {
        None
    }

    /// Adopt a dimension recovered from persisted rows while still unmeasured,
    /// so identity matches the one those rows were saved under (a custom
    /// endpoint only measures at its first response). No-op by default.
    fn seed_measured_dimension(&self, _dimension: usize) {}

    /// Downcast hook for the one provider that keeps a client-side embedding
    /// cache. Default `None` for every other backend.
    fn as_custom(&self) -> Option<&CustomEmbeddingProvider> {
        None
    }
}

/// Reorder embedding vectors by their response index, verifying that the
/// responses form an exact permutation of `0..expected`.
///
/// OpenAI-compatible endpoints do not guarantee that `data` elements are
/// returned in input order. A missing index, duplicate index, out-of-range
/// index, or length mismatch violates the contract and is rejected as
/// `RemoteEmbeddingError::InvalidResponse` (Codex review round-3 P1).
pub(crate) fn reorder_embeddings_by_index(
    entries: Vec<(usize, Vec<f32>)>,
    expected: usize,
) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
    if entries.len() != expected {
        return Err(RemoteEmbeddingError::InvalidResponse(format!(
            "expected {expected} embeddings in response, got {}",
            entries.len()
        )));
    }
    let mut ordered: Vec<Option<Vec<f32>>> = (0..expected).map(|_| None).collect();
    for (idx, vec) in entries {
        if idx >= expected {
            return Err(RemoteEmbeddingError::InvalidResponse(format!(
                "embedding index {idx} out of range 0..{expected}"
            )));
        }
        if ordered[idx].is_some() {
            return Err(RemoteEmbeddingError::InvalidResponse(format!(
                "duplicate embedding index {idx}"
            )));
        }
        ordered[idx] = Some(vec);
    }
    ordered
        .into_iter()
        .enumerate()
        .map(|(idx, opt)| {
            opt.ok_or_else(|| {
                RemoteEmbeddingError::InvalidResponse(format!("missing embedding for index {idx}"))
            })
        })
        .collect()
}

/// OpenAI-compatible embedding provider for user-configured endpoints
/// (llama-server, vLLM, sglang, Ollama, or any `/v1/embeddings` service).
///
/// Dimension truth (spec 2026-10-09 B1.3): the legacy default (1536 for
/// custom endpoints) is only a bootstrap value used before the first
/// successful response — it is never enforced. The first successful embed
/// response's vector length becomes the measured dimension, which then wins
/// over the `remote_dimension` override; afterwards any response with a
/// different length is rejected (`InvalidDimension`) so a mid-index drift
/// cannot poison the neural mmap with ragged rows.
pub struct CustomEmbeddingProvider {
    client: Client,
    endpoint: String,
    model: Option<String>,
    api_key: Option<String>,
    dimension_override: Option<usize>,
    bootstrap_dimension: usize,
    /// Template applied to single-text (`embed`) calls — the QUERY path.
    /// `None` = raw text (back-compat).
    query_template: Option<String>,
    /// Template applied to batch (`embed_batch`) calls — the DOCUMENT path.
    /// `None` = raw text (back-compat).
    doc_template: Option<String>,
    /// 0 = not yet measured; else the vector length observed in the first
    /// successful response.
    measured: std::sync::atomic::AtomicUsize,
    /// Transport retry budget; carried because this provider destructures the
    /// config instead of holding it.
    ///
    /// Bounded by the request timeout, never unbounded.
    max_retries: usize,
    /// Hard cap on inputs per request, from the provider preset (or
    /// `remote_batch_limit`). `None`/`0` = one request per batch.
    ///
    /// Held on the STRUCT, not read from the config at call time, because this
    /// provider destructures the config and never keeps it — which is exactly
    /// why the cap was silently dropped: the config had it, the Gemini provider
    /// chunked on it, and this path had no way to see it. A capped endpoint
    /// received the throughput-tuned 128, was rejected wholesale, and recovered
    /// only by bisecting into ~2n-1 doomed requests.
    batch_limit: Option<usize>,
    /// Token ceiling for ONE input, or `None`. Carried on the struct for the
    /// same reason as `batch_limit`: this provider destructures the config, so
    /// a limit left in the config is a limit this path cannot see.
    max_input_tokens: Option<usize>,
    /// Token ceiling for ONE REQUEST (all inputs summed), or `None`.
    ///
    /// Request width is packed against this. Counting documents alone is not
    /// sufficient: a handful of long documents can overflow a model's window
    /// while sitting far under any document cap, and the endpoint then rejects
    /// the entire request.
    request_token_budget: Option<usize>,
    /// Embedding-cache namespace, injected by
    /// [`GenericRemoteProvider::from_config`] from the SAME identity the
    /// checkpoint uses (endpoint + model + base URL + template fingerprint).
    /// `None` = uncached. Never recomputed here: a second copy of the identity
    /// formula is a second thing that can drift out of agreement with the
    /// checkpoint, and the template fingerprint is only known to the wrapper.
    cache_namespace: std::sync::Mutex<Option<String>>,
    /// The content-addressed store, opened on first use. A `Mutex` because the
    /// store needs `&mut` to probe and write while the batch path takes
    /// `&self`.
    cache: std::sync::Mutex<Option<super::remote_cache::RemoteEmbedCache>>,
    /// Latch: a failed open is not retried per batch. Without it, an
    /// unreadable store turns every batch into another failed open attempt.
    cache_open_failed: std::sync::atomic::AtomicBool,
    /// Managed service this endpoint belongs to, when LeIndex launched it.
    ///
    /// `Some(name)` is what makes self-healing possible: LeIndex owns the
    /// process (via `[services.<name>]` + `auto_start`), so when the endpoint
    /// stays down across a whole failure budget it can be restarted rather
    /// than abandoned. `None` for a third-party endpoint — OpenAI, OpenRouter,
    /// Cohere, Google — where there is no process here to heal.
    supervised_service: Option<String>,
    /// Latches once a restart has been attempted for an outage. Exactly one
    /// restart per outage: a restart that does not fix the endpoint must not
    /// turn every subsequent batch into another full stop/start cycle.
    restart_attempted: std::sync::atomic::AtomicBool,
    /// `input_type` values for the two sides, when the provider takes one.
    ///
    /// Sent as a single `"input_type"` field chosen per call site: document
    /// batches use `document`, single-text queries use `query`. `None` omits
    /// the field, which is what endpoints like llama-server expect.
    input_type_document: Option<String>,
    input_type_query: Option<String>,
}

/// Statuses that can be caused by one bad input (oversized/invalid content;
/// 500 is llama-server's oversized-input rejection). 502/503/504 are
/// availability/gateway failures: they fail the batch fast (endpoint-down
/// freeze path) instead of bisecting into many doomed requests.
fn is_bisectable_embedding_status(status: u16) -> bool {
    matches!(status, 400 | 413 | 422 | 500)
}

/// Maximum number of halvings applied to one oversized input before it is
/// given up on (64 KiB -> 1 KiB after six).
pub(crate) const MAX_OVERSIZE_SHRINKS: usize = 6;

/// Whether a bisectable rejection means "this ONE input exceeds the server's
/// per-input token limit".
///
/// Same verdict as [`is_oversized_input_rejection`], kept as the narrower
/// entry point for the truncation path: it also accepts HTTP 413, which
/// generic OpenAI-compatible servers use for a body over the payload limit.
///
/// A 400 whose message names a length problem is also a size verdict, not a
/// content verdict: hosted proxies answer an over-window request with 400 and
/// `The prompt is too long ...`, and treating that as unclassifiable dropped
/// the row instead of truncating it.
pub(crate) fn is_oversize_rejection(status: u16, message: &str) -> bool {
    status == 413
        || is_oversized_input_rejection(status, message)
        || (status == 400 && is_too_long_message(message))
}

/// Whether `message` says the input is too long for the model's window.
///
/// Three provider dialects say the same thing, and only the first was
/// recognized before:
///
/// * llama-server (HTTP 500): `input (N tokens) is too large to process.
///   increase the physical batch size`.
/// * Hosted OpenAI-compatible proxies (HTTP 400): `The prompt is too long for
///   <model> on the routes that remain. Shorten it and retry.`
/// * OpenAI itself (HTTP 400): `maximum context length is N tokens`.
///
/// Matching only the llama-server wording meant a 400 from a hosted proxy was
/// classified as a document-level verdict and skipped outright, so the shrink
/// path never ran and the row was silently dropped. The phrases are matched
/// case-insensitively on meaning, not on one exact sentence.
pub(crate) fn is_too_long_message(message: &str) -> bool {
    let lowered = message.to_ascii_lowercase();
    lowered.contains("too large to process")
        || lowered.contains("prompt is too long")
        || lowered.contains("maximum context length")
        || lowered.contains("reduce the length of the messages")
}

/// The first half of `text` (by bytes), cut back to a char boundary so the
/// endpoint never receives a partial UTF-8 sequence.
pub(crate) fn halve_on_char_boundary(text: &str) -> &str {
    let mut end = text.len() / 2;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

/// Whether re-sending the SAME batch could plausibly succeed.
///
/// True only for failures that say "not right now": the endpoint never
/// produced a response, or answered with a gateway/availability/timeout/rate
/// limit status. `HTTP 500` is deliberately NOT retryable — llama-server uses
/// it to reject one oversized input, so the batch loop bisects it instead, and
/// retrying would multiply a deterministic failure.
///
/// Everything else is a contract failure (dimension/identity mismatch,
/// malformed body, an input the endpoint rejects on its own merits). Those are
/// permanent for this batch and must surface immediately.
pub(crate) fn is_retryable_embedding_error(error: &RemoteEmbeddingError) -> bool {
    match error {
        RemoteEmbeddingError::Transport(_) | RemoteEmbeddingError::RateLimitExceeded(_) => true,
        RemoteEmbeddingError::HttpStatus { status, .. } => {
            matches!(status, 408 | 502 | 503 | 504)
        }
        _ => false,
    }
}

/// Remaining retry allowance for ONE logical batch request.
///
/// A logical request fans out into many HTTP attempts: the initial batch, every
/// bisection sub-batch, and every oversize-shrink attempt. Giving each of those
/// its own `max_retries` budget means a single dead endpoint costs
/// `attempts * (max_retries + 1) * timeout_secs` — the cost grows with how far
/// the batch has to bisect, which is exactly backwards: the sicker the endpoint,
/// the longer the stall. This struct is created once per logical batch and shared
/// across its attempts, so the total wait per batch stays bounded by
/// `(max_retries + 1) * timeout_secs` (plus backoff) no matter how it subdivides.
/// A counter that must be readable across `await` points, so `Cell` is out
/// (`!Sync` would make these futures `!Send`) and a locked type is overkill.
type RetryCounter = std::sync::atomic::AtomicUsize;

struct RetryAllowance<'a> {
    retries_left: &'a RetryCounter,
}

impl<'a> RetryAllowance<'a> {
    fn new(retries_left: &'a RetryCounter) -> Self {
        Self { retries_left }
    }

    /// Consume one retry. Returns `false` when the batch's allowance is spent.
    ///
    /// `Ordering::Relaxed` is sufficient: the counter is private to one logical
    /// batch running on one task, so it needs atomicity, not ordering.
    ///
    /// Written as an explicit compare-exchange loop rather than
    /// `AtomicUsize::try_update`, which is only stable since Rust 1.95 — above
    /// this crate's MSRV. `compare_exchange_weak` is free to spuriously fail, so
    /// the loop re-reads and retries; a spurious failure simply means one extra
    /// iteration, never a miscounted retry.
    fn take(&self) -> bool {
        let mut current = self.retries_left.load(std::sync::atomic::Ordering::Relaxed);
        loop {
            if current == 0 {
                return false;
            }
            match self.retries_left.compare_exchange_weak(
                current,
                current - 1,
                std::sync::atomic::Ordering::Relaxed,
                std::sync::atomic::Ordering::Relaxed,
            ) {
                Ok(_) => return true,
                Err(observed) => current = observed,
            }
        }
    }

    fn remaining(&self) -> usize {
        self.retries_left.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Accumulated state of one bisect-retrying remote batch.
///
/// Split out of [`CustomEmbeddingProvider::embed_pending`] so the bisection
/// loop, the single-input verdict and the final fallback policy each read on
/// their own. The fields are the loop's whole working set: which ranges are
/// still queued, which inputs the endpoint rejected individually, and the
/// error that stopped the run early (if any).
struct PendingOutcome<'rows, 'batch, 'allowance> {
    /// Write-through target, indexed by the ORIGINAL position in the batch.
    rows: &'rows mut [Option<Vec<f32>>],
    /// Map from a slice-local position back to that original position.
    indices: &'batch [usize],
    /// The batch-wide retry budget, shared with every sub-request and oversize
    /// shrink. Held here so the single-input verdict can shrink-and-retry
    /// without the caller threading it through.
    allowance: &'allowance RetryAllowance<'allowance>,
    /// Ranges still to send. Popped from the tail, so a bisection's halves are
    /// depth-first and a landed sub-batch is never re-sent.
    pending_slices: Vec<(usize, usize)>,
    /// Single inputs the endpoint rejected for a document-level reason.
    skipped: Vec<usize>,
    /// Single inputs rejected as larger than the server's per-input limit.
    oversized: Vec<usize>,
    /// First HTTP status/message behind `skipped`, used to report a batch where
    /// nothing at all landed.
    first_error: Option<(u16, String)>,
    /// Set when the run must stop but already-embedded vectors are kept.
    aborted_mid_flight: Option<RemoteEmbeddingError>,
}

impl<'rows, 'batch, 'allowance> PendingOutcome<'rows, 'batch, 'allowance> {
    fn new(
        rows: &'rows mut [Option<Vec<f32>>],
        indices: &'batch [usize],
        allowance: &'allowance RetryAllowance<'allowance>,
    ) -> Self {
        Self {
            pending_slices: vec![(0, indices.len())],
            rows,
            indices,
            allowance,
            skipped: Vec::new(),
            oversized: Vec::new(),
            first_error: None,
            aborted_mid_flight: None,
        }
    }

    /// Write a fully-answered range's vectors into their original positions.
    fn land(&mut self, start: usize, end: usize, embeddings: Vec<Vec<f32>>) {
        for (index, embedding) in (start..end).zip(embeddings) {
            self.rows[self.indices[index]] = Some(embedding);
        }
    }

    /// Split a rejected range in half and requeue both halves.
    ///
    /// Bisection is NOT a retry: each split halves the range, so depth is
    /// `<= log2(batch)` and the total request count is `<= 2n-1` for an n-input
    /// batch. It needs no budget.
    fn bisect(&mut self, start: usize, end: usize, status: u16, message: String) {
        self.first_error.get_or_insert((status, message));
        tracing::debug!(
            start,
            end,
            batch_len = end - start,
            "Bisecting failed remote embedding batch"
        );
        let middle = start + (end - start) / 2;
        self.pending_slices.push((middle, end));
        self.pending_slices.push((start, middle));
    }

    /// Stop the run, keeping whatever already landed.
    fn abort(&mut self, error: RemoteEmbeddingError) {
        self.aborted_mid_flight = Some(error);
        self.pending_slices.clear();
    }

    /// Apply the batch's fallback policy once the queue is exhausted.
    fn finish(self) -> Result<(), RemoteEmbeddingError> {
        let Self {
            rows,
            indices,
            skipped,
            oversized,
            first_error,
            aborted_mid_flight,
            ..
        } = self;
        let embedded = indices
            .iter()
            .filter(|&&position| rows[position].is_some())
            .count();
        if let Some(error) = aborted_mid_flight {
            if embedded == 0 {
                return Err(error);
            }
            tracing::warn!(
                %error,
                embedded,
                total = indices.len(),
                "Remote batch aborted mid-flight; keeping already-embedded rows and leaving the rest empty"
            );
            return Ok(());
        }
        if !skipped.is_empty() && embedded == 0 {
            let (status, _) = first_error.expect("failed leaves have an HTTP status");
            return Err(RemoteEmbeddingError::HttpStatus {
                status,
                message: format!("all {} inputs failed individually", skipped.len()),
            });
        }
        if !oversized.is_empty() {
            // Every one of these was refused as an individual DOCUMENT for
            // exceeding the server's per-input token limit. That is not an
            // endpoint outage, so it must not surface as an `Err`: doing so
            // would spend the caller's sustained-failure budget on documents
            // the server will never accept, which is exactly how oversized
            // inputs used to freeze the whole neural layer. The rows stay
            // `None` (those documents get no neural vector) and the batch is
            // reported as handled.
            tracing::warn!(
                oversized = oversized.len(),
                total = indices.len(),
                "Skipping documents that exceed the server's per-input token limit; \
                 raise the service's -ub (logical batch) to admit them"
            );
        }
        if !skipped.is_empty() {
            tracing::warn!(skipped_indices = ?skipped, "Skipping remote embedding inputs rejected individually");
        }
        Ok(())
    }
}

/// Run one batch request, re-sending transient failures while the batch's
/// shared [`RetryAllowance`] permits.
///
/// The allowance is shared with every other attempt of the same logical batch
/// (bisection sub-batches and oversize shrinks), so the whole batch — not each
/// sub-attempt — is bounded by `max_retries` retries. A permanent failure is
/// returned at once: retrying it would only multiply a deterministic error.
async fn with_transient_retry<T, F, Fut>(
    allowance: &RetryAllowance<'_>,
    mut send: F,
) -> Result<T, RemoteEmbeddingError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, RemoteEmbeddingError>>,
{
    let mut backoff_step: usize = 0;
    loop {
        match send().await {
            Ok(value) => return Ok(value),
            Err(error) => {
                // `is_retryable_embedding_error` already excludes every
                // input-caused rejection (400/413/422/500 are bisectable, never
                // retryable), so nothing here can re-send a batch the endpoint
                // rejected on its merits. That matters because the budget is
                // SHARED and the caller treats a spent budget as an endpoint
                // outage: spending it on an identical-input failure would
                // starve the bisection of the only signal it has, turning a
                // document-level rejection into a batch-level freeze.
                if !is_retryable_embedding_error(&error) || !allowance.take() {
                    return Err(error);
                }
                let delay = retry_backoff(backoff_step);
                tracing::warn!(
                    %error,
                    retries_left = allowance.remaining(),
                    delay_ms = delay.as_millis() as u64,
                    "Transient remote embedding failure; retrying the same batch"
                );
                tokio::time::sleep(delay).await;
                backoff_step = backoff_step.saturating_add(1);
            }
        }
    }
}

/// True when llama-server refused a single input for exceeding its per-input
/// token limit (`-ub`), rather than failing to serve the request.
///
/// llama-server answers such an input with HTTP 500 whose body names the limit:
/// `input (N tokens) is too large to process. increase the physical batch
/// size`. That is a statement about the DOCUMENT, not about the endpoint, and
/// the two must be told apart: an unclassifiable 500 is an outage that should
/// spend the caller's sustained-failure budget, while an oversized document
/// should simply be skipped. Conflating them is what let oversized inputs alone
/// burn the budget and freeze the neural layer (92,605 rejections in one run).
pub(crate) fn is_oversized_input_rejection(status: u16, message: &str) -> bool {
    status == 500
        && message
            .to_ascii_lowercase()
            .contains("too large to process")
}

impl CustomEmbeddingProvider {
    /// Create a custom OpenAI-compatible embedding provider.
    pub fn new(config: RemoteEmbeddingConfig) -> Result<Self, RemoteEmbeddingError> {
        let (endpoint, bootstrap_dimension) = match &config.provider {
            RemoteProvider::Custom { endpoint } => {
                (endpoint.clone(), config.provider.default_dimension())
            }
            _ => {
                return Err(RemoteEmbeddingError::ApiError(
                    "Invalid provider".to_string(),
                ));
            }
        };
        let client = build_http_client(&config)?;
        Ok(Self {
            client,
            endpoint,
            model: config.model.clone(),
            api_key: config.api_key.clone(),
            dimension_override: config.remote_dimension,
            query_template: config.query_template.clone(),
            doc_template: config.doc_template.clone(),
            bootstrap_dimension,
            measured: std::sync::atomic::AtomicUsize::new(0),
            max_retries: config.max_retries,
            // Carried onto the struct because this provider never retains the
            // config — see the field's own doc comment.
            batch_limit: config.batch_limit.filter(|limit| *limit > 0),
            cache_namespace: std::sync::Mutex::new(None),
            cache: std::sync::Mutex::new(None),
            cache_open_failed: std::sync::atomic::AtomicBool::new(false),
            // `from_config` injects the managed service name (it is the only
            // caller that knows the config); direct construction has none.
            supervised_service: config.service_name.clone(),
            restart_attempted: std::sync::atomic::AtomicBool::new(false),
            input_type_document: config.input_type_document.clone(),
            input_type_query: config.input_type_query.clone(),
            max_input_tokens: config.max_input_tokens.filter(|limit| *limit > 0),
            request_token_budget: config.request_token_budget.filter(|limit| *limit > 0),
        })
    }

    /// Restart the managed service behind this endpoint, at most once.
    ///
    /// Only a service LeIndex launched (`[services.<name>]` with
    /// `service_name` set by the index builder) can be healed here. The caller
    /// has already spent a whole batch-failure budget over many seconds, so
    /// this is not a reaction to one slow batch — it is the difference between
    /// "the endpoint is busy" and "the endpoint is gone".
    ///
    /// `restart_attempted` latches the attempt: an endpoint that cannot be
    /// revived (bad model path, corrupt GGUF, no VRAM) fails once instead of
    /// cycling the server stop/start behind every subsequent batch.
    ///
    /// Returns `true` only when the restart actually succeeded. Everything
    /// else leaves the caller's own fallback policy in charge.
    #[cfg(feature = "remote-embeddings")]
    pub(crate) fn try_revive_endpoint(&self) -> bool {
        use std::sync::atomic::Ordering;
        let Some(name) = self.supervised_service.as_deref() else {
            // Third-party endpoint: no local process to heal.
            return false;
        };
        if self.restart_attempted.swap(true, Ordering::AcqRel) {
            tracing::warn!(
                service = %name,
                "Managed embedding endpoint is still down after its restart; \
                 not restarting again this run"
            );
            return false;
        }
        let services = &crate::config::LeIndexConfig::load_cached().services;
        let Some(config) = services.get(name) else {
            tracing::warn!(
                service = %name,
                "Managed service is absent from config; cannot restart it"
            );
            return false;
        };
        tracing::warn!(
            service = %name,
            endpoint = %self.endpoint,
            "Embedding endpoint spent its whole retry budget; restarting the managed service"
        );
        // `restart` waits for HTTP health before returning, so `Ok` means the
        // port answers again, not merely that a process was spawned.
        match crate::services::restart(name, config) {
            Ok(_) => {
                tracing::warn!(
                    service = %name,
                    "Managed embedding service restarted; resuming neural embedding"
                );
                true
            }
            Err(error) => {
                tracing::error!(
                    service = %name,
                    %error,
                    "Managed embedding service restart failed"
                );
                false
            }
        }
    }

    /// Current dimension: measured > override > legacy bootstrap default.
    fn current_dimension(&self) -> usize {
        let measured = self.measured.load(std::sync::atomic::Ordering::Relaxed);
        if measured > 0 {
            measured
        } else {
            self.dimension_override.unwrap_or(self.bootstrap_dimension)
        }
    }
}

#[async_trait]
impl RemoteEmbeddingProvider for CustomEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, RemoteEmbeddingError> {
        // Single-text calls are the QUERY path: apply the query template.
        let input = apply_template(self.query_template.as_deref(), text);
        // One text is one logical batch, so the allowance covers just this call.
        let retries_left = RetryCounter::new(self.max_retries);
        let allowance = RetryAllowance::new(&retries_left);
        let embeddings = with_transient_retry(&allowance, || {
            self.embed_via_api_for(vec![input.clone()], self.input_type_query.as_deref())
        })
        .await?;
        embeddings.into_iter().next().ok_or_else(|| {
            RemoteEmbeddingError::InvalidResponse("No embedding returned".to_string())
        })
    }

    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        // Batch calls are the DOCUMENT path: apply the doc template (no
        // titles — every {title} placeholder resolves to `none`).
        let titles = std::iter::repeat_n(None, texts.len())
            .collect::<Vec<Option<String>>>()
            .into_iter()
            .peekable();
        let rows = self.embed_batch_optional_titled(texts, titles).await?;
        Ok(rows
            .into_iter()
            .map(|row| row.unwrap_or_default())
            .collect())
    }

    async fn embed_batch_optional(
        &self,
        texts: Vec<&str>,
    ) -> Result<Vec<Option<Vec<f32>>>, RemoteEmbeddingError> {
        // Legacy (title-less) documents: every title resolves to `none`.
        let titles = std::iter::repeat_n(None, texts.len())
            .collect::<Vec<Option<String>>>()
            .into_iter()
            .peekable();
        self.embed_batch_optional_titled(texts, titles).await
    }

    async fn embed_batch_titled(
        &self,
        texts: Vec<&str>,
        titles: std::iter::Peekable<std::vec::IntoIter<Option<String>>>,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        let rows = self.embed_batch_optional_titled(texts, titles).await?;
        Ok(rows
            .into_iter()
            .map(|row| row.unwrap_or_default())
            .collect())
    }

    /// Shared document-batch path: template with optional titles, probe the
    /// embedding cache, send only the misses, and bisect-retry any batch the
    /// endpoint rejects wholesale. Rows that land are stored back in the cache;
    /// rows the endpoint skipped are not.
    async fn embed_batch_optional_titled(
        &self,
        texts: Vec<&str>,
        titles: std::iter::Peekable<std::vec::IntoIter<Option<String>>>,
    ) -> Result<Vec<Option<Vec<f32>>>, RemoteEmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        let inputs: Vec<_> = texts
            .iter()
            .zip(titles)
            .map(|(text, title)| {
                apply_doc_template(self.doc_template.as_deref(), title.as_deref(), text)
            })
            .collect();
        // Enforce the endpoint's per-INPUT token ceiling up front.
        //
        // One document can exceed a model's window on its own, and the request
        // carrying it is then rejected wholesale. Truncating deliberately is
        // strictly better than the alternative the shrink path offers later:
        // it costs no rejected round trip, and the truncation is ours to
        // choose rather than reactive. `estimate_tokens` is deliberately an
        // over-estimate, so this trims slightly early rather than too late.
        //
        // Truncation keeps the HEAD of the document, which is what a code
        // embedding should see: signatures, imports and the enclosing
        // definition come before the body. A tail-truncated symbol would keep
        // the closing braces and lose the thing being defined.
        let inputs: Vec<_> = match self.max_input_tokens.filter(|limit| *limit > 0) {
            Some(limit) => inputs
                .into_iter()
                .map(|input| truncate_to_token_budget(input, limit))
                .collect(),
            None => inputs,
        };
        let total = inputs.len();
        // Index of every input still needing an embed request.
        let mut pending_indices: Vec<usize> = (0..total).collect();
        // Cached rows, in original position space.
        let mut rows = vec![None; total];
        let probe = self.probe_cache(&inputs);

        if let Some(probe) = &probe {
            for (position, row) in probe.cached.iter().enumerate() {
                rows[position] = row.clone();
            }
            if probe.is_fully_cached() {
                tracing::debug!(total, "remote batch fully served from the embed cache");
                return Ok(rows);
            }
            pending_indices = probe.miss_positions.clone();
        }

        // Only the misses are sent. The embedding is identical whether the text
        // travels in a batch of 500 or alone, so shrinking the request set is a
        // pure win: fewer tokens, no re-embedding of unchanged documents.
        let pending_inputs: Vec<_> = pending_indices
            .iter()
            .map(|&index| inputs[index].clone())
            .collect();
        // Enforce the endpoint's hard per-request input cap.
        //
        // `batch_limit` is filled from the provider preset (or `remote_batch_limit`)
        // and was never read on this path — only Gemini chunked. The caller's
        // `BatchBudget` is throughput-tuned (128) and knows nothing about an
        // endpoint that accepts 64, so a custom OpenAI-compatible endpoint was
        // sent 128 inputs, rejected wholesale, and recovered only by bisecting
        // the batch into ~2n-1 doomed requests. Chunking here is the fix: cap
        // crossed, the response is rejected outright, never bisected.
        //
        // A missing or zero limit keeps today's single-request behaviour for
        // LOCAL endpoints (the byte-identical safety net for the managed
        // llama-server). For remote endpoints, where the round trip dominates,
        // a missing cap falls back to `REMOTE_CHUNK` so the batch still splits
        // into several overlapping requests instead of collapsing into one
        // serial request; a configured cap below that width still wins, since
        // the endpoint rejects anything above its documented limit.
        //
        // Width is also packed against `request_token_budget`: the document cap
        // alone cannot keep a request inside a model's window, and a rejected
        // request costs the whole batch. Width is therefore re-derived at each
        // step rather than computed once, because documents differ in size — a
        // fixed stride would either overflow on a run of long documents or
        // waste the budget on short ones.
        let slices = self.plan_request_slices(&pending_inputs);
        let max_in_flight = remote_max_in_flight(&self.endpoint);
        if max_in_flight <= 1 || slices.len() <= 1 {
            // Serial arm — byte-identical to today's behaviour for local endpoints.
            self.embed_slices_serial(&slices, &pending_inputs, &mut rows, &pending_indices)
                .await?;
        } else {
            // Each worker embeds ONE slice and returns its rows.
            //
            // Waves of `max_in_flight` are what actually enforce the endpoint
            // width. The previous `join_all` over EVERY slice fired the whole
            // corpus at the endpoint at once — `max_in_flight` was only ever
            // read as a `<= 1` on/off gate, so the "4 in flight" this file
            // documents was not enforced anywhere. Against a hosted endpoint
            // that is an unbounded burst the server queues (or 429s) on its own
            // terms, and local timing then measures those queues rather than
            // the client.
            //
            // A wave, not a semaphore: `buffer_unordered` would express the
            // same width but its stream borrows `slices` across the await, and
            // the `async_trait` boxed future below cannot satisfy that
            // lifetime. Chunking the borrowed slice list keeps every future
            // owned and equivalent in effect.
            self.embed_slices_concurrent(
                &slices,
                &pending_inputs,
                &mut rows,
                &pending_indices,
                max_in_flight,
            )
            .await?;
        }

        if let Some(probe) = &probe {
            self.store_cache(probe, &rows);
        }
        Ok(rows)
    }

    fn dimension(&self) -> usize {
        self.current_dimension()
    }

    fn measured_dimension(&self) -> Option<usize> {
        let measured = self.measured.load(std::sync::atomic::Ordering::Relaxed);
        if measured > 0 {
            Some(measured)
        } else {
            self.dimension_override
        }
    }

    fn observed_dimension(&self) -> Option<usize> {
        let measured = self.measured.load(std::sync::atomic::Ordering::Relaxed);
        (measured > 0).then_some(measured)
    }

    fn seed_measured_dimension(&self, dimension: usize) {
        // Fills an unmeasured provider only; a real response always wins.
        let _ = self.measured.compare_exchange(
            0,
            dimension,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        );
    }

    fn as_custom(&self) -> Option<&CustomEmbeddingProvider> {
        Some(self)
    }
}

impl CustomEmbeddingProvider {
    /// Pack the pending inputs into per-request slices.
    ///
    /// Width is re-derived at each step rather than computed once, because
    /// documents differ in size — a fixed stride would either overflow on a run
    /// of long documents or waste the budget on short ones.
    fn plan_request_slices(
        &self,
        pending_inputs: &[std::borrow::Cow<'_, str>],
    ) -> Vec<(usize, usize)> {
        let local = endpoint_is_local(&self.endpoint);
        let mut slices: Vec<(usize, usize)> = Vec::new();
        let mut start = 0usize;
        while start < pending_inputs.len() {
            let width = request_width_for(
                &pending_inputs[start..],
                self.batch_limit,
                self.request_token_budget,
                local,
            )
            .max(1);
            let end = (start + width).min(pending_inputs.len());
            slices.push((start, end));
            start = end;
        }
        slices
    }

    /// Send every slice in turn on one connection.
    async fn embed_slices_serial(
        &self,
        slices: &[(usize, usize)],
        pending_inputs: &[std::borrow::Cow<'_, str>],
        rows: &mut [Option<Vec<f32>>],
        pending_indices: &[usize],
    ) -> Result<(), RemoteEmbeddingError> {
        for &(start, end) in slices {
            self.embed_pending(
                &pending_inputs[start..end],
                rows,
                &pending_indices[start..end],
            )
            .await?;
        }
        Ok(())
    }

    /// Send slices in waves of `max_in_flight`, merging each wave's rows back
    /// into their original positions.
    ///
    /// The scratch is sized to the SLICE, not to the whole batch: indices are
    /// rebased to slice-local positions before the call, so a worker never
    /// allocates or scans everything. [`Self::embed_pending`] writes through
    /// the indices it is given, so local indices are all it needs; the merge
    /// here translates them back.
    async fn embed_slices_concurrent(
        &self,
        slices: &[(usize, usize)],
        pending_inputs: &[std::borrow::Cow<'_, str>],
        rows: &mut [Option<Vec<f32>>],
        pending_indices: &[usize],
        max_in_flight: usize,
    ) -> Result<(), RemoteEmbeddingError> {
        for wave in slices.chunks(max_in_flight) {
            let futures = wave.iter().map(|&(start, end)| async move {
                let width = end - start;
                let mut worker_rows = vec![None; width];
                let local: Vec<usize> = (0..width).collect();
                self.embed_pending(&pending_inputs[start..end], &mut worker_rows, &local)
                    .await
                    .map(|()| (start, worker_rows))
            });
            for result in futures::future::join_all(futures).await {
                let (start, worker_rows) = result?;
                for (offset, row) in worker_rows.into_iter().enumerate() {
                    if let Some(row) = row {
                        rows[pending_indices[start + offset]] = Some(row);
                    }
                }
            }
        }
        Ok(())
    }
    /// Attach the embedding-cache namespace (the checkpoint identity minus its
    /// dimension component). Called once by
    /// [`GenericRemoteProvider::from_config`]; `None` disables caching.
    pub(crate) fn set_cache_namespace(&self, namespace: Option<String>) {
        // Set before the first batch runs, so the uncontended path is the read.
        let mut slot = match self.cache_namespace.lock() {
            Ok(slot) => slot,
            Err(poisoned) => poisoned.into_inner(),
        };
        *slot = namespace;
    }

    /// Re-send one input the endpoint rejected as too large, halving it until
    /// it fits (at most [`MAX_OVERSIZE_SHRINKS`] times).
    ///
    /// `Ok(None)` = still rejected after the last shrink, or rejected for a
    /// reason other than size: the caller skips this single row. Non-bisectable
    /// errors (transport, dimension, malformed body) propagate unchanged.
    ///
    /// `allowance` is the enclosing logical batch's shared retry budget and is
    /// consulted only for TRANSIENT re-sends. Truncation itself is bounded
    /// deterministically by [`MAX_OVERSIZE_SHRINKS`], exactly like bisection, so
    /// it must never be gated on that budget: `max_retries == 0` disables
    /// re-sending a failed request, not reformulating a request to fit a
    /// documented size limit. Gating it there silently drops every oversized
    /// document on a retries-off config, which leaves the node without a vector
    /// and — when it is the batch's only input — looks like a dead endpoint.
    async fn embed_shrinking_oversized(
        &self,
        input: &str,
        allowance: &RetryAllowance<'_>,
    ) -> Result<Option<Vec<f32>>, RemoteEmbeddingError> {
        let mut current = input;
        for attempt in 1..=MAX_OVERSIZE_SHRINKS {
            current = halve_on_char_boundary(current);
            if current.is_empty() {
                return Ok(None);
            }
            match with_transient_retry(allowance, || {
                self.embed_via_api(vec![std::borrow::Cow::Borrowed(current)])
            })
            .await
            {
                Ok(embeddings) => {
                    tracing::warn!(
                        attempt,
                        kept_bytes = current.len(),
                        original_bytes = input.len(),
                        "Oversized remote embedding input truncated to fit the endpoint"
                    );
                    return Ok(embeddings.into_iter().next());
                }
                Err(RemoteEmbeddingError::HttpStatus { status, message })
                    if is_bisectable_embedding_status(status) =>
                {
                    // The classifier that ENTERS the shrink loop must be the one
                    // that decides to keep going, so compare the status it was
                    // matched on and leave the message to the 500-only predicate
                    // that distinguishes "too large" from other 500s.
                    if !is_oversize_rejection(status, &message) {
                        return Ok(None);
                    }
                }
                Err(error) => return Err(error),
            }
        }
        Ok(None)
    }

    /// Bisect-retry `pending` and write results into `rows` at `indices`.
    ///
    /// Returns `Ok(())` when every pending input landed, `Err` when none did
    /// (so the caller can apply its fallback policy). A partial outcome is
    /// `Ok(())` with the remaining positions left `None`.
    async fn embed_pending(
        &self,
        pending: &[std::borrow::Cow<'_, str>],
        rows: &mut [Option<Vec<f32>>],
        indices: &[usize],
    ) -> Result<(), RemoteEmbeddingError> {
        // ONE allowance for the whole logical batch. Bisection can fan a single
        // request out into hundreds of sub-requests; giving each its own
        // `max_retries` budget makes a dead endpoint cost
        // `sub_batches * (max_retries + 1) * timeout_secs`, i.e. the stall grows
        // the harder the batch fails. Shared here, the batch is bounded no matter
        // how far it subdivides.
        let retries_left = RetryCounter::new(self.max_retries);
        let allowance = RetryAllowance::new(&retries_left);
        let mut outcome = PendingOutcome::new(rows, indices, &allowance);

        while let Some((start, end)) = outcome.pending_slices.pop() {
            // Retry the SUB-batch, not the whole batch: a transient failure
            // deep in a bisection must not restart work already landed above.
            match with_transient_retry(&allowance, || {
                self.embed_via_api(pending[start..end].to_vec())
            })
            .await
            {
                Ok(embeddings) if embeddings.len() == end - start => {
                    outcome.land(start, end, embeddings);
                }
                Ok(embeddings) => {
                    // Wrong row count: the endpoint answered, but not usable
                    // for this slice. Retrying cannot fix it, so stop
                    // bisecting and keep whatever already landed.
                    outcome.abort(RemoteEmbeddingError::InvalidResponse(format!(
                        "Expected {} embeddings, got {}",
                        end - start,
                        embeddings.len()
                    )));
                    break;
                }
                Err(RemoteEmbeddingError::HttpStatus { status, message })
                    if is_bisectable_embedding_status(status) =>
                {
                    if end - start == 1 {
                        self.handle_single_rejection(
                            &mut outcome,
                            pending[start].as_ref(),
                            status,
                            message,
                            start,
                        )
                        .await?;
                    } else {
                        outcome.bisect(start, end, status, message);
                    }
                }
                Err(error) => {
                    // The endpoint went away mid-batch. Abandoning the queue
                    // is unavoidable for the REMAINING slices, but the slices
                    // already embedded are real vectors that were paid for:
                    // dropping them here is what made a transient blip cost a
                    // whole batch of documents.
                    outcome.abort(error);
                    break;
                }
            }
        }

        outcome.finish()
    }

    /// Resolve one rejected single input: shrink an oversize document and retry
    /// it, or record it as skipped.
    ///
    /// A lone input the endpoint calls too large is shrunk and re-sent rather
    /// than dropped: losing it would leave the node without a vector, and if it
    /// is the only input of the batch it would also look like a dead endpoint
    /// and freeze the generation. Every other single-input rejection is a
    /// document-level verdict: record it and move on, because only an
    /// unclassifiable status counts toward the caller's sustained-failure
    /// budget.
    async fn handle_single_rejection(
        &self,
        outcome: &mut PendingOutcome<'_, '_, '_>,
        text: &str,
        status: u16,
        message: String,
        start: usize,
    ) -> Result<(), RemoteEmbeddingError> {
        if is_oversize_rejection(status, &message) {
            match self
                .embed_shrinking_oversized(text, outcome.allowance)
                .await?
            {
                Some(embedding) => outcome.rows[outcome.indices[start]] = Some(embedding),
                None => outcome.oversized.push(start),
            }
        } else {
            outcome.skipped.push(start);
            outcome.first_error.get_or_insert((status, message));
        }
        Ok(())
    }

    /// Probe the remote embedding cache, opening it once per process.
    ///
    /// Every failure path returns `None`, which means "embed everything, no
    /// cache" — a cache problem must never fail an index.
    fn probe_cache(
        &self,
        inputs: &[std::borrow::Cow<'_, str>],
    ) -> Option<super::remote_cache::ProbeOutcome> {
        // Copy the namespace out before taking the cache lock: the two locks are
        // never held at once, so nothing here can deadlock against the setter.
        let namespace = {
            let slot = self
                .cache_namespace
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            slot.clone()?
        };
        // The dimension is the final identity component: a measured (or
        // checkpoint-adopted) dimension makes warm runs hit, while an
        // unmeasured provider correctly misses instead of guessing a width.
        let dimension = self.measured_dimension()?;
        let mut guard = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if guard.is_none() {
            if self
                .cache_open_failed
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return None;
            }
            match super::remote_cache::RemoteEmbedCache::open(&namespace, dimension) {
                Some(cache) => *guard = Some(cache),
                None => {
                    // Flag off, or the store cannot be opened: latch so the
                    // next batch does not pay for another failed open.
                    self.cache_open_failed
                        .store(true, std::sync::atomic::Ordering::Release);
                    return None;
                }
            }
        }
        let cache = guard.as_mut()?;
        cache.probe(inputs)
    }

    /// Store the rows that actually landed. Best effort; never fails a batch.
    fn store_cache(&self, probe: &super::remote_cache::ProbeOutcome, rows: &[Option<Vec<f32>>]) {
        let mut guard = self.cache.lock().unwrap_or_else(|error| error.into_inner());
        if let Some(cache) = guard.as_mut() {
            cache.store_landed(probe, rows);
        }
    }

    /// POST one `/embeddings` request with already-templated inputs and
    /// enforce the dimension-truth contract (shared by query + document
    /// paths so both always agree on dimension semantics).
    async fn embed_via_api(
        &self,
        inputs: Vec<std::borrow::Cow<'_, str>>,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        self.embed_via_api_for(inputs, self.input_type_document.as_deref())
            .await
    }

    /// [`Self::embed_via_api`] with an explicit `input_type`, so the query path
    /// can send the provider's query-side value instead of its document one.
    ///
    /// `input_type` is omitted from the wire entirely when `None`: providers
    /// that do not define the field (and every local llama-server) reject or
    /// ignore it, so it must never be sent speculatively.
    async fn embed_via_api_for(
        &self,
        inputs: Vec<std::borrow::Cow<'_, str>>,
        input_type: Option<&str>,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        #[derive(Serialize)]
        struct CustomRequest<'a> {
            model: String,
            input: Vec<&'a str>,
            #[serde(skip_serializing_if = "Option::is_none")]
            input_type: Option<&'a str>,
        }

        #[derive(Deserialize)]
        struct CustomResponse {
            data: Vec<CustomEmbedding>,
        }

        // `index` is required (no serde default): a missing index on an
        // OpenAI-compatible endpoint is a contract violation and hard-fails
        // rather than guessing array order (Codex review round-3 P1).
        #[derive(Deserialize)]
        struct CustomEmbedding {
            index: usize,
            embedding: Vec<f32>,
        }

        let request = CustomRequest {
            model: self.model.clone().unwrap_or_default(),
            input: inputs.iter().map(|i| i.as_ref()).collect(),
            input_type,
        };
        let expected = inputs.len();

        let url = format!("{}/embeddings", self.endpoint.trim_end_matches('/'));
        // Exactly ONE attempt. Retrying is `with_transient_retry`'s job, driven
        // by the enclosing batch's shared `RetryAllowance`: retrying here as
        // well would give every bisection sub-batch and every shrink attempt a
        // fresh full `max_retries` budget, which is the unbounded-stall bug the
        // allowance exists to prevent.
        let mut request_builder = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request);
        if let Some(api_key) = &self.api_key {
            request_builder = request_builder.header("Authorization", format!("Bearer {api_key}"));
        }
        let response = request_builder
            .send()
            .await
            .map_err(|error| RemoteEmbeddingError::Transport(error.to_string()))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| RemoteEmbeddingError::Transport(e.to_string()))?;

        if !status.is_success() {
            if status.as_u16() == 429 {
                return Err(RemoteEmbeddingError::RateLimitExceeded(
                    "custom".to_string(),
                ));
            }
            return Err(RemoteEmbeddingError::HttpStatus {
                status: status.as_u16(),
                message: response_text,
            });
        }

        let parsed: CustomResponse = serde_json::from_str(&response_text)
            .map_err(|e| RemoteEmbeddingError::InvalidResponse(e.to_string()))?;

        let embeddings = reorder_embeddings_by_index(
            parsed
                .data
                .into_iter()
                .map(|e| (e.index, e.embedding))
                .collect(),
            expected,
        )?;

        // Dimension truth: with a measurement or override already known every
        // vector must match it; otherwise the first successful length becomes
        // the measurement (bootstrap default is never enforced).
        let known = {
            let measured = self.measured.load(std::sync::atomic::Ordering::Relaxed);
            if measured > 0 {
                Some(measured)
            } else {
                self.dimension_override
            }
        };
        if let Some(expected) = known.or_else(|| embeddings.first().map(Vec::len)) {
            for embedding in &embeddings {
                if embedding.len() != expected {
                    return Err(RemoteEmbeddingError::InvalidDimension {
                        expected,
                        got: embedding.len(),
                    });
                }
            }
        }
        if let Some(first) = embeddings
            .first()
            .filter(|_| self.measured.load(std::sync::atomic::Ordering::Relaxed) == 0)
        {
            self.measured
                .store(first.len(), std::sync::atomic::Ordering::Relaxed);
            tracing::info!(
                dimension = first.len(),
                endpoint = %self.endpoint,
                "Measured remote embedding dimension from first response"
            );
        }
        Ok(embeddings)
    }
}

/// Generic remote embedding provider that wraps specific implementations
#[derive(Clone)]
pub struct GenericRemoteProvider {
    provider: Arc<dyn RemoteEmbeddingProvider>,
    /// Identity prefix (provider, model, endpoint). Never contains
    /// credentials; the embedding dimension is appended at call time so a
    /// measured runtime dimension (custom endpoints) lands in the identity.
    identity_prefix: String,
    /// Fingerprint of the active task templates (spec v2 P1): the same
    /// endpoint with different templates produces DIFFERENT vectors, so the
    /// caches keyed on this identity (Engram phrase-book, embedding caches)
    /// must not share entries across template changes. `None` = raw text.
    template_fingerprint: Option<String>,
    /// The document template this provider applies before sending. Exposed via
    /// [`Self::doc_template`] so the embedding cache can key rows on the exact
    /// string the endpoint embeds rather than the pre-template text.
    doc_template: Option<String>,
}

/// Version of the *semantics* applied by [`apply_doc_template`] and
/// [`apply_template`], folded into [`template_fingerprint`].
///
/// The fingerprint hashes the template STRING. That is not enough: the same
/// string can mean different inputs to the model as the substitution rules
/// change, and this is not hypothetical — builds before document titles were
/// threaded embedded a literal `{title}` that is now replaced per document.
/// Existing installations with a custom template containing `{title}` would
/// otherwise keep their old vectors, because neither the template nor the
/// lexical hash changed, and silently mix two vector spaces in one store.
///
/// Bump this whenever the substituted result of an unchanged template changes.
/// 1 = pre-title-substitution. 2 = `{title}` is replaced (with `none` when
/// absent) and a template with no `{text}` placeholder gets the text appended.
const TEMPLATE_SEMANTICS_VERSION: u32 = 2;

/// Short fingerprint of the active task templates for cache identities.
/// FNV-1a 64-bit: stable, dependency-free, and cheap insurance that a
/// template change never reuses another template's cache namespace.
///
/// The [`TEMPLATE_SEMANTICS_VERSION`] is hashed first so a semantics change
/// with an unchanged template string also lands in a fresh namespace.
fn template_fingerprint(doc: Option<&str>, query: Option<&str>) -> Option<String> {
    if doc.is_none() && query.is_none() {
        return None;
    }
    let version = TEMPLATE_SEMANTICS_VERSION.to_string();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    // Each part is LENGTH-PREFIXED before it is folded in. Joining parts with a
    // bare separator — which is what this did — is not injective: a document
    // template ending in a separator and a query template starting with one are
    // indistinguishable from the split one byte to the left, so two different
    // template pairs produce the same fingerprint and reuse each other's cached
    // vectors, in a different vector space. The length makes the encoding
    // injective without needing an escape character at all.
    for part in [version.as_str(), doc.unwrap_or(""), query.unwrap_or("")] {
        for byte in part.len().to_le_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0010_0000_01b3);
        }
        for byte in part.as_bytes() {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0010_0000_01b3);
        }
    }
    Some(format!("tpl{hash:016x}"))
}

impl std::fmt::Debug for GenericRemoteProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GenericRemoteProvider")
            .field("provider", &"<RemoteEmbeddingProvider>")
            .finish()
    }
}

impl GenericRemoteProvider {
    /// Create a remote provider from configuration
    pub fn from_config(config: RemoteEmbeddingConfig) -> Result<Self, RemoteEmbeddingError> {
        let (kind, model) = match &config.provider {
            RemoteProvider::OpenAI { model } => ("openai", model.as_str()),
            RemoteProvider::Cohere { model } => ("cohere", model.as_str()),
            RemoteProvider::Gemini { model } => ("gemini", model.as_str()),
            RemoteProvider::Custom { endpoint } => ("custom", endpoint.as_str()),
        };
        // The configured model name is part of the identity for custom
        // endpoints: the endpoint alone does not identify the vector space,
        // and a model swap behind the same URL would otherwise reuse another
        // model's checkpoint silently. OpenAI/Cohere already carry their model
        // name in the `kind` slot above; for custom endpoints the endpoint is
        // the kind slot, so the model is appended when set. When unset, the
        // endpoint itself is the identity marker (backward compatible).
        let identity_prefix = if matches!(&config.provider, RemoteProvider::Custom { .. }) {
            match config.model.as_deref() {
                Some(model_name) if !model_name.trim().is_empty() => {
                    format!(
                        "remote|{kind}|{model}|{}|m{model_name}",
                        config.base_url.as_deref().unwrap_or("")
                    )
                }
                _ => format!(
                    "remote|{kind}|{model}|{}",
                    config.base_url.as_deref().unwrap_or("")
                ),
            }
        } else {
            format!(
                "remote|{kind}|{model}|{}",
                config.base_url.as_deref().unwrap_or("")
            )
        };
        let template_fingerprint = template_fingerprint(
            config.doc_template.as_deref(),
            config.query_template.as_deref(),
        );
        // Captured before the config moves into the provider: after that this
        // struct is the only place the caller can learn which template the
        // endpoint will apply, and the embed cache must key on the templated
        // string, never on the raw one.
        let doc_template = config.doc_template.clone();
        let provider: Arc<dyn RemoteEmbeddingProvider> = match &config.provider {
            RemoteProvider::OpenAI { .. } => Arc::new(OpenAIEmbeddingProvider::new(config)?),
            RemoteProvider::Cohere { .. } => Arc::new(CohereEmbeddingProvider::new(config)?),
            RemoteProvider::Gemini { .. } => Arc::new(GeminiEmbeddingProvider::new(config)?),
            RemoteProvider::Custom { .. } => Arc::new(CustomEmbeddingProvider::new(config)?),
        };

        // Inject the cache namespace. Custom endpoints are the route that can
        // re-embed an unchanged corpus, so they get one; the namespace is the
        // same identity (minus dimension) the checkpoint uses, so a model or
        // template change can never reuse another space's vectors.
        let backend: &dyn RemoteEmbeddingProvider = provider.as_ref();
        if let Some(custom) = backend.as_custom() {
            custom.set_cache_namespace(Some(identity_prefix.clone()));
        }

        Ok(Self {
            provider,
            identity_prefix,
            template_fingerprint,
            doc_template,
        })
    }

    /// The document template applied to batch inputs before they are sent, if
    /// any. `None` = raw text.
    pub fn doc_template(&self) -> Option<&str> {
        self.doc_template.as_deref()
    }

    /// One restart attempt per outage, and only for a service this process
    /// owns.
    ///
    /// The caller spends a whole failure budget over many seconds before
    /// reaching here, so this is not a knee-jerk reaction to one slow batch:
    /// it is the difference between "the endpoint is busy" and "the endpoint
    /// is gone". `restart_attempted` on the custom provider latches the
    /// attempt, so an endpoint that cannot be revived (bad model path, corrupt
    /// GGUF, no VRAM) fails once instead of cycling the server stop/start
    /// behind every subsequent batch.
    ///
    /// Returns `true` only when a restart actually succeeded. Anything else —
    /// a third-party endpoint with no local process, a managed one that cannot
    /// be restarted — keeps the caller's own fallback policy in charge.
    #[cfg(feature = "remote-embeddings")]
    pub(crate) fn try_revive_endpoint(&self) -> bool {
        let Some(custom) = self.provider.as_custom() else {
            // Nothing local to restart (OpenAI, OpenRouter, Cohere, Google...).
            return false;
        };
        custom.try_revive_endpoint()
    }

    /// Stable identity of this provider for embedding caches (no secrets).
    /// The trailing dimension component is appended per call so a custom
    /// provider's measured runtime dimension is part of the identity. The
    /// optional template fingerprint separates vector spaces across task-prefix
    /// changes.
    pub fn identity(&self) -> String {
        self.identity_with_dimension(self.provider.dimension())
    }

    /// Identity as it would read if the provider's dimension were `dimension`
    /// (used to match a checkpoint saved under a since-measured dimension).
    pub fn identity_with_dimension(&self, dimension: usize) -> String {
        let template = self
            .template_fingerprint
            .as_deref()
            .map(|fingerprint| format!("|{fingerprint}"))
            .unwrap_or_default();
        format!("{}{template}|{dimension}", self.identity_prefix)
    }

    /// Dimension observed in the first successful remote response, if any.
    pub fn observed_dimension(&self) -> Option<usize> {
        self.provider.observed_dimension()
    }

    /// Adopt a dimension recovered from persisted rows while still unmeasured.
    pub fn seed_measured_dimension(&self, dimension: usize) {
        self.provider.seed_measured_dimension(dimension);
    }
}

#[async_trait]
impl RemoteEmbeddingProvider for GenericRemoteProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, RemoteEmbeddingError> {
        self.provider.embed(text).await
    }

    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        self.provider.embed_batch(texts).await
    }

    async fn embed_batch_optional(
        &self,
        texts: Vec<&str>,
    ) -> Result<Vec<Option<Vec<f32>>>, RemoteEmbeddingError> {
        self.provider.embed_batch_optional(texts).await
    }

    async fn embed_batch_titled(
        &self,
        texts: Vec<&str>,
        titles: std::iter::Peekable<std::vec::IntoIter<Option<String>>>,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        self.provider.embed_batch_titled(texts, titles).await
    }

    async fn embed_batch_optional_titled(
        &self,
        texts: Vec<&str>,
        titles: std::iter::Peekable<std::vec::IntoIter<Option<String>>>,
    ) -> Result<Vec<Option<Vec<f32>>>, RemoteEmbeddingError> {
        self.provider
            .embed_batch_optional_titled(texts, titles)
            .await
    }

    fn dimension(&self) -> usize {
        self.provider.dimension()
    }

    fn measured_dimension(&self) -> Option<usize> {
        self.provider.measured_dimension()
    }

    fn observed_dimension(&self) -> Option<usize> {
        self.provider.observed_dimension()
    }

    fn seed_measured_dimension(&self, dimension: usize) {
        self.provider.seed_measured_dimension(dimension);
    }
}

#[cfg(test)]
#[path = "remote_test.rs"]
mod tests;
