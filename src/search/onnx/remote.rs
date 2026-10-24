// Remote Embedding Providers
//
// Provides integration with cloud-based embedding services like OpenAI,
// Cohere, and other API-based embedding providers as an alternative to local ONNX models.

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
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
            Self::Custom { .. } => 1536, // Default assumption for custom endpoints
        }
    }
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
    /// `0` means "never re-send anything, ever" — including the truncation
    /// re-send of an oversized document. Each input then reaches exactly one
    /// terminal verdict.
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

/// Build the HTTP client every provider shares.
///
/// `pool_idle_timeout(Duration::ZERO)` disables idle-socket reuse on purpose.
/// llama-server (and single-model OpenAI-compatible servers generally) drop a
/// kept-alive connection as soon as client and server disagree about liveness,
/// and reqwest then surfaces the NEXT request on that dead socket as
/// `error sending request for url (...)` — a transport failure with no server
/// answer behind it. Measured on a 523-batch index run: one stale-socket reuse
/// aborted the batch 9 minutes in. A fresh connection per batch costs a
/// localhost handshake against a request that takes hundreds of milliseconds
/// anyway.
fn build_http_client(config: &RemoteEmbeddingConfig) -> Result<Client, RemoteEmbeddingError> {
    let timeout = std::time::Duration::from_secs(config.timeout_secs);
    let mut builder = Client::builder()
        .connect_timeout(timeout)
        .pool_idle_timeout(std::time::Duration::ZERO);
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

/// Availability statuses that mean "the endpoint is busy or restarting", not
/// "your input is wrong" — re-sending the identical bytes can plausibly work.
///
/// A definitive rejection (400/413/422 naming content, 401 naming credentials)
/// is deliberately absent: those fail identically forever, so retrying only
/// delays the bisect that would have isolated the offending document.
fn is_retryable_status(status: u16) -> bool {
    // 500 is deliberately absent: llama-server answers an input over its
    // per-input token limit with HTTP 500, and every other provider's 500 is
    // equally deterministic for identical bytes. Re-sending it would multiply
    // the failure and starve the bisection that narrows it to the one bad
    // document. A 500 is BISECTED instead (see `is_bisectable_embedding_status`).
    matches!(status, 408 | 425 | 429 | 502 | 503 | 504 | 507 | 509 | 529)
}

/// Exponential backoff with a cap: 250ms, 500ms, 1s, 2s, then 2s.
///
/// Deliberately small. A reset socket or a saturated queue clears in well under
/// a second; anything larger just hides a dead endpoint behind a longer stall.
fn retry_backoff(attempt: usize) -> std::time::Duration {
    const BASE_MS: u64 = 250;
    const CAP_MS: u64 = 2_000;
    std::time::Duration::from_millis(BASE_MS.saturating_mul(1u64 << attempt.min(3)).min(CAP_MS))
}

/// Send one request, retrying transient failures up to `max_retries` times.
///
/// This is the ladder `RemoteEmbeddingConfig::max_retries` always described and
/// never had: before it existed, a single reset socket became a permanent
/// neural-generation freeze (the caller's `stopped` latch fires on the first
/// whole-batch error and never clears).
///
/// `build` is a closure, not a prepared `Request`, because every attempt needs
/// its own body. When the budget is exhausted the last response is returned
/// verbatim rather than converted to an error: the caller's status handling
/// (availability fast-fail, oversized-input bisection) owns the verdict, so
/// `max_retries == 0` reproduces the pre-retry behaviour exactly.
async fn send_with_retry<F>(
    max_retries: usize,
    mut build: F,
) -> Result<reqwest::Response, RemoteEmbeddingError>
where
    F: FnMut() -> reqwest::RequestBuilder,
{
    let mut attempt = 0;
    loop {
        let reason = match build().send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                if attempt >= max_retries || !is_retryable_status(status) {
                    return Ok(response);
                }
                format!("HTTP {status}")
            }
            Err(error) => {
                if attempt >= max_retries {
                    return Err(RemoteEmbeddingError::Transport(error.to_string()));
                }
                error.to_string()
            }
        };
        let delay = retry_backoff(attempt);
        tracing::warn!(
            attempt = attempt + 1,
            max_retries,
            delay_ms = delay.as_millis() as u64,
            %reason,
            "Remote embedding request failed transiently; retrying"
        );
        tokio::time::sleep(delay).await;
        attempt += 1;
    }
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

/// OpenAI embedding provider implementation
pub struct OpenAIEmbeddingProvider {
    client: Client,
    config: RemoteEmbeddingConfig,
}

impl OpenAIEmbeddingProvider {
    /// Create a new OpenAI embedding provider
    pub fn new(config: RemoteEmbeddingConfig) -> Result<Self, RemoteEmbeddingError> {
        if config.api_key.is_none() {
            return Err(RemoteEmbeddingError::ApiKeyNotFound("OpenAI".to_string()));
        }
        let client = build_http_client(&config)?;
        Ok(Self { client, config })
    }
}

#[async_trait]
impl RemoteEmbeddingProvider for OpenAIEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, RemoteEmbeddingError> {
        let embeddings = self.embed_batch(vec![text]).await?;
        embeddings.into_iter().next().ok_or_else(|| {
            RemoteEmbeddingError::InvalidResponse("No embedding returned".to_string())
        })
    }

    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        let model_name = match &self.config.provider {
            RemoteProvider::OpenAI { model } => model.clone(),
            _ => {
                return Err(RemoteEmbeddingError::ApiError(
                    "Invalid provider".to_string(),
                ));
            }
        };

        #[derive(Serialize)]
        struct OpenAIRequest<'a> {
            model: String,
            input: Vec<&'a str>,
            encoding_format: String,
        }

        #[derive(Deserialize)]
        struct OpenAIResponse {
            data: Vec<OpenAIEmbedding>,
        }

        // `index` is required: OpenAI-compatible endpoints may return `data`
        // out of request order, and a missing index is a contract violation.
        #[derive(Deserialize)]
        struct OpenAIEmbedding {
            index: usize,
            embedding: Vec<f32>,
        }

        let texts_len = texts.len();
        let request = OpenAIRequest {
            model: model_name,
            input: texts,
            encoding_format: "float".to_string(),
        };

        let api_key = self.config.api_key.as_ref().unwrap();

        let response = send_with_retry(self.config.max_retries, || {
            self.client
                .post("https://api.openai.com/v1/embeddings")
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&request)
        })
        .await?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| RemoteEmbeddingError::Transport(e.to_string()))?;

        if !status.is_success() {
            if status.as_u16() == 429 {
                return Err(RemoteEmbeddingError::RateLimitExceeded(
                    "OpenAI".to_string(),
                ));
            }
            return Err(RemoteEmbeddingError::HttpStatus {
                status: status.as_u16(),
                message: format!("OpenAI: {response_text}"),
            });
        }

        let openai_response: OpenAIResponse = serde_json::from_str(&response_text)
            .map_err(|e| RemoteEmbeddingError::InvalidResponse(e.to_string()))?;

        reorder_embeddings_by_index(
            openai_response
                .data
                .into_iter()
                .map(|e| (e.index, e.embedding))
                .collect(),
            texts_len,
        )
    }

    fn dimension(&self) -> usize {
        self.config.provider.default_dimension()
    }
}

/// Cohere embedding provider implementation
pub struct CohereEmbeddingProvider {
    client: Client,
    config: RemoteEmbeddingConfig,
}

impl CohereEmbeddingProvider {
    /// Create a new Cohere embedding provider
    pub fn new(config: RemoteEmbeddingConfig) -> Result<Self, RemoteEmbeddingError> {
        if config.api_key.is_none() {
            return Err(RemoteEmbeddingError::ApiKeyNotFound("Cohere".to_string()));
        }

        let client = build_http_client(&config)?;

        Ok(Self { client, config })
    }
}

#[async_trait]
impl RemoteEmbeddingProvider for CohereEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, RemoteEmbeddingError> {
        let embeddings = self.embed_batch(vec![text]).await?;
        embeddings.into_iter().next().ok_or_else(|| {
            RemoteEmbeddingError::InvalidResponse("No embedding returned".to_string())
        })
    }

    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        let model_name = match &self.config.provider {
            RemoteProvider::Cohere { model } => model.clone(),
            _ => {
                return Err(RemoteEmbeddingError::ApiError(
                    "Invalid provider".to_string(),
                ));
            }
        };

        let base_url = self.config.base_url.as_ref().unwrap();

        #[derive(Serialize)]
        struct CohereRequest<'a> {
            model: String,
            texts: Vec<&'a str>,
            input_type: String,
        }

        #[derive(Deserialize)]
        struct CohereResponse {
            embeddings: Vec<CohereEmbedding>,
        }

        #[derive(Deserialize)]
        struct CohereEmbedding {
            embedding: Vec<f32>,
        }

        let request = CohereRequest {
            model: model_name,
            texts,
            input_type: "search_document".to_string(),
        };

        let api_key = self.config.api_key.as_ref().unwrap();

        let url = format!("{}/embed", base_url);
        let response = send_with_retry(self.config.max_retries, || {
            self.client
                .post(&url)
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .header("X-Client-Name", "leindex")
                .json(&request)
        })
        .await?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| RemoteEmbeddingError::Transport(e.to_string()))?;

        if !status.is_success() {
            if status.as_u16() == 429 {
                return Err(RemoteEmbeddingError::RateLimitExceeded(
                    "Cohere".to_string(),
                ));
            }
            return Err(RemoteEmbeddingError::HttpStatus {
                status: status.as_u16(),
                message: format!("Cohere: {response_text}"),
            });
        }

        let cohere_response: CohereResponse = serde_json::from_str(&response_text)
            .map_err(|e| RemoteEmbeddingError::InvalidResponse(e.to_string()))?;

        Ok(cohere_response
            .embeddings
            .into_iter()
            .map(|e| e.embedding)
            .collect())
    }

    fn dimension(&self) -> usize {
        self.config.provider.default_dimension()
    }
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
const MAX_OVERSIZE_SHRINKS: usize = 6;

/// Whether a bisectable rejection means "this ONE input exceeds the server's
/// per-input token limit".
///
/// Same verdict as [`is_oversized_input_rejection`], kept as the narrower
/// entry point for the truncation path: it also accepts HTTP 413, which
/// generic OpenAI-compatible servers use for a body over the payload limit.
pub(crate) fn is_oversize_rejection(status: u16, message: &str) -> bool {
    status == 413 || is_oversized_input_rejection(status, message)
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

    /// The shared budget has no left, so a later attempt reports why it stopped.
    fn spent(&self) -> bool {
        self.retries_left.load(std::sync::atomic::Ordering::Relaxed) == 0
    }

    fn remaining(&self) -> usize {
        self.retries_left.load(std::sync::atomic::Ordering::Relaxed)
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
                // SHARED: spending it on an identical-input failure would starve
                // the bisection, and `allowance.spent()` makes the bisection skip
                // the remaining rows — handing the caller a silent skip instead
                // of the endpoint failure it is required to propagate.
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

/// Apply a task template to `text`: replace every `{text}` placeholder with
/// the text (prefix-only templates — no placeholder — prepend + space).
fn apply_template<'a>(template: Option<&str>, text: &'a str) -> std::borrow::Cow<'a, str> {
    match template {
        None => std::borrow::Cow::Borrowed(text),
        Some(t) if t.contains("{text}") => std::borrow::Cow::Owned(t.replace("{text}", text)),
        Some(t) => std::borrow::Cow::Owned(format!("{t} {text}")),
    }
}

/// Apply the DOCUMENT task template with a title.
///
/// EmbeddingGemma2 document inputs are `title: {title} | text: {text}`; the
/// model card mandates `title: none` when no title is available, so callers
/// without a title (queries aside, the title-less doc paths, fragments)
/// pass `None` and every `{title}` placeholder resolves to `none`.
///
/// Substitution is a SINGLE LEFT-TO-RIGHT PASS over the template, and neither
/// substituted value is ever rescanned. Replacing `{title}` and then searching
/// the already-filled string for `{text}` — which is what this used to do —
/// let a title containing the literal `{text}` swallow the document body: a
/// source file legitimately named `{text}.rs` produces exactly that title, and
/// its embedding would be built from the body twice and no title at all.
pub(crate) fn apply_doc_template<'a>(
    template: Option<&str>,
    title: Option<&str>,
    text: &'a str,
) -> std::borrow::Cow<'a, str> {
    let title = title.unwrap_or("none");
    let Some(template) = template else {
        return std::borrow::Cow::Borrowed(text);
    };
    if !template.contains("{text}") && !template.contains("{title}") {
        // Prefix-only template: prepend and separate.
        return std::borrow::Cow::Owned(format!("{template} {text}"));
    }
    let mut out = String::with_capacity(template.len() + text.len() + title.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        if let Some(tail) = after.strip_prefix("{title}") {
            out.push_str(title);
            rest = tail;
        } else if let Some(tail) = after.strip_prefix("{text}") {
            out.push_str(text);
            rest = tail;
        } else {
            // A literal brace that starts no placeholder.
            out.push('{');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    // A template with `{title}` but no `{text}` still needs the body appended,
    // matching the historical behaviour.
    if !template.contains("{text}") {
        out.push(' ');
        out.push_str(text);
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod template_tests {
    use super::apply_doc_template;

    #[test]
    fn test_title_containing_a_placeholder_does_not_swallow_the_body() {
        // A source file named `{text}.rs` yields exactly this title. The old
        // two-pass replace substituted the BODY into the title slot.
        let out = apply_doc_template(
            Some("title: {title} | text: {text}"),
            Some("{text}.rs"),
            "BODY",
        );
        assert_eq!(
            out, "title: {text}.rs | text: BODY",
            "a title must be inserted verbatim and never rescanned for placeholders"
        );
        assert!(
            !out.contains("text: BODY |"),
            "the body must not replace the title"
        );
    }

    #[test]
    fn test_missing_title_resolves_to_none() {
        assert_eq!(
            apply_doc_template(Some("title: {title} | text: {text}"), None, "B"),
            "title: none | text: B"
        );
    }

    #[test]
    fn test_prefix_only_template_appends_the_body() {
        assert_eq!(
            apply_doc_template(Some("search_document:"), None, "B"),
            "search_document: B"
        );
    }

    #[test]
    fn test_no_template_returns_the_raw_text() {
        assert_eq!(apply_doc_template(None, Some("t"), "B"), "B");
    }
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
            cache_namespace: std::sync::Mutex::new(None),
            cache: std::sync::Mutex::new(None),
            cache_open_failed: std::sync::atomic::AtomicBool::new(false),
            // `from_config` injects the managed service name (it is the only
            // caller that knows the config); direct construction has none.
            supervised_service: config.service_name.clone(),
            restart_attempted: std::sync::atomic::AtomicBool::new(false),
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
        let embeddings =
            with_transient_retry(&allowance, || self.embed_via_api(vec![input.clone()])).await?;
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
        self.embed_pending(&pending_inputs, &mut rows, &pending_indices)
            .await?;

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
    /// `allowance` is the enclosing logical batch's shared retry budget, so the
    /// shrink attempts cannot each start a fresh retry cycle. Truncation is a
    /// RE-SEND, so it also obeys `max_retries == 0` as "never re-send" and
    /// declines immediately: a caller that has turned retries off expects each
    /// input to reach exactly one terminal verdict.
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
            if allowance.spent() {
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
                    if !is_oversized_input_rejection(status, &message) {
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
        let mut pending_slices = vec![(0, pending.len())];
        let mut skipped = Vec::new();
        let mut oversized = Vec::new();
        let mut first_error = None;
        let mut aborted_mid_flight = None;
        // ONE allowance for the whole logical batch. Bisection can fan a single
        // request out into hundreds of sub-requests; giving each its own
        // `max_retries` budget makes a dead endpoint cost
        // `sub_batches * (max_retries + 1) * timeout_secs`, i.e. the stall grows
        // the harder the batch fails. Shared here, the batch is bounded no matter
        // how far it subdivides.
        let retries_left = RetryCounter::new(self.max_retries);
        let allowance = RetryAllowance::new(&retries_left);

        while let Some((start, end)) = pending_slices.pop() {
            // Retry the SUB-batch, not the whole batch: a transient failure
            // deep in a bisection must not restart work already landed above.
            match with_transient_retry(&allowance, || {
                self.embed_via_api(pending[start..end].to_vec())
            })
            .await
            {
                Ok(embeddings) if embeddings.len() == end - start => {
                    for (index, embedding) in (start..end).zip(embeddings) {
                        rows[indices[index]] = Some(embedding);
                    }
                }
                Ok(embeddings) => {
                    // Wrong row count: the endpoint answered, but not usable
                    // for this slice. Retrying cannot fix it, so stop
                    // bisecting and keep whatever already landed.
                    aborted_mid_flight = Some(RemoteEmbeddingError::InvalidResponse(format!(
                        "Expected {} embeddings, got {}",
                        end - start,
                        embeddings.len()
                    )));
                    pending_slices.clear();
                    break;
                }
                Err(RemoteEmbeddingError::HttpStatus { status, message })
                    if is_bisectable_embedding_status(status) =>
                {
                    if end - start == 1 {
                        // A lone input the endpoint calls too large is shrunk
                        // and re-sent rather than dropped: losing it would leave
                        // the node without a vector, and if it is the only input
                        // of the batch it would also look like a dead endpoint
                        // and freeze the generation. Every other single-input
                        // rejection is a document-level verdict: record it and
                        // move on, because only an unclassifiable status counts
                        // toward the caller's sustained-failure budget.
                        if is_oversize_rejection(status, &message) {
                            match self
                                .embed_shrinking_oversized(pending[start].as_ref(), &allowance)
                                .await?
                            {
                                Some(embedding) => rows[indices[start]] = Some(embedding),
                                None => oversized.push(start),
                            }
                        } else {
                            skipped.push(start);
                            first_error.get_or_insert((status, message));
                        }
                    } else {
                        first_error.get_or_insert((status, message));
                        tracing::debug!(
                            start,
                            end,
                            batch_len = end - start,
                            "Bisecting failed remote embedding batch"
                        );
                        // Bisection is NOT a retry: each split halves the range,
                        // so depth is <= log2(batch) and the total request count
                        // is <= 2n-1 for an n-input batch. It needs no budget.
                        let middle = start + (end - start) / 2;
                        pending_slices.push((middle, end));
                        pending_slices.push((start, middle));
                    }
                }
                Err(error) => {
                    // The endpoint went away mid-batch. Abandoning the queue
                    // is unavoidable for the REMAINING slices, but the slices
                    // already embedded are real vectors that were paid for:
                    // dropping them here is what made a transient blip cost a
                    // whole batch of documents.
                    aborted_mid_flight = Some(error);
                    pending_slices.clear();
                    break;
                }
            }
        }

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
        #[derive(Serialize)]
        struct CustomRequest<'a> {
            model: String,
            input: Vec<&'a str>,
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
mod tests {
    use super::*;

    #[test]
    fn test_remote_provider_default_dimension() {
        let provider = RemoteProvider::OpenAI {
            model: "text-embedding-3-small".to_string(),
        };
        assert_eq!(provider.default_dimension(), 1536);
    }

    #[test]
    fn test_template_fingerprint_covers_substitution_semantics() {
        // A template-string-only hash is not enough: `apply_doc_template`
        // started replacing `{title}` without the template changing, so every
        // installation holding a `{title}` template kept its old vectors in
        // the current namespace while new rows were embedded from different
        // input — two vector spaces under one identity. Guard the property:
        // the fingerprint must not be reproducible from the template text
        // alone (the pre-fix formula), which is what folding in
        // TEMPLATE_SEMANTICS_VERSION buys.
        let doc = "title: {title} | text: {text}";
        let mut lexical: u64 = 0xcbf2_9ce4_8422_2325;
        for part in [doc, "\u{0}", ""] {
            for byte in part.as_bytes() {
                lexical ^= u64::from(*byte);
                lexical = lexical.wrapping_mul(0x0010_0000_01b3);
            }
        }
        assert_ne!(
            template_fingerprint(Some(doc), None),
            Some(format!("tpl{lexical:016x}")),
            "the fingerprint must vary with substitution semantics, not only the text"
        );
    }

    #[test]
    fn test_template_fingerprint_cannot_collide_across_part_boundaries() {
        // `("a\0", "b")` and `("a", "\0b")` are different template pairs — the
        // document template ends in a NUL vs the query template begins with one
        // — but a bare-separator join is not injective: both encode to the same
        // byte stream `a\0\0b`, so they shared a fingerprint and therefore a
        // cache namespace. Length-prefixing each part makes the encoding
        // injective, whatever bytes the templates contain.
        let left = template_fingerprint(Some("a\u{0}"), Some("b"));
        let right = template_fingerprint(Some("a"), Some("\u{0}b"));
        assert!(left.is_some() && right.is_some());
        assert_ne!(
            left, right,
            "templates that differ only in where a separator falls must not share a fingerprint"
        );
    }

    #[test]
    fn test_remote_identity_names_model_and_never_leaks_credentials() {
        let secret = "sk-super-secret-value";
        let config = RemoteEmbeddingConfig::openai(secret.to_string(), None);
        let provider = GenericRemoteProvider::from_config(config).unwrap();
        let identity = provider.identity();
        assert!(identity.starts_with("remote|openai|text-embedding-3-small|"));
        assert!(identity.ends_with(&format!("|{}", provider.dimension())));
        assert!(!identity.contains(secret));

        let other = GenericRemoteProvider::from_config(RemoteEmbeddingConfig {
            provider: RemoteProvider::OpenAI {
                model: "text-embedding-3-large".to_string(),
            },
            api_key: Some(secret.to_string()),
            ..Default::default()
        })
        .unwrap();
        assert_ne!(identity, other.identity());
    }

    #[test]
    fn test_custom_remote_identity_includes_model_without_changing_legacy_identity() {
        let endpoint = "http://127.0.0.1:8080/v1/embeddings".to_string();
        let base_url = Some("http://127.0.0.1:8080/v1".to_string());
        let config = |model: Option<&str>| RemoteEmbeddingConfig {
            provider: RemoteProvider::Custom {
                endpoint: endpoint.clone(),
            },
            base_url: base_url.clone(),
            model: model.map(str::to_string),
            remote_dimension: Some(768),
            ..Default::default()
        };

        let model_a = GenericRemoteProvider::from_config(config(Some("model-a"))).unwrap();
        let model_b = GenericRemoteProvider::from_config(config(Some("model-b"))).unwrap();
        assert_ne!(model_a.identity(), model_b.identity());
        assert!(model_a.identity().contains("|mmodel-a|"));

        let legacy = GenericRemoteProvider::from_config(config(None)).unwrap();
        assert_eq!(
            legacy.identity(),
            format!(
                "remote|custom|{endpoint}|{}|768",
                base_url.as_deref().unwrap()
            )
        );
    }

    #[test]
    fn test_remote_config_openai() {
        let config = RemoteEmbeddingConfig::openai("test-key".to_string(), None);
        assert!(config.api_key.is_some());
    }

    #[test]
    fn test_reorder_embeddings_identity() {
        let entries = vec![(0, vec![1.0]), (1, vec![2.0]), (2, vec![3.0])];
        let result = reorder_embeddings_by_index(entries, 3).unwrap();
        assert_eq!(result, vec![vec![1.0], vec![2.0], vec![3.0]]);
    }

    #[test]
    fn test_reorder_embeddings_shuffled() {
        let entries = vec![(2, vec![3.0]), (0, vec![1.0]), (1, vec![2.0])];
        let result = reorder_embeddings_by_index(entries, 3).unwrap();
        assert_eq!(result, vec![vec![1.0], vec![2.0], vec![3.0]]);
    }

    #[test]
    fn test_reorder_embeddings_duplicate_index() {
        let entries = vec![(0, vec![1.0]), (0, vec![2.0])];
        let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
        assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
        assert!(err.to_string().contains("duplicate"));
    }

    #[test]
    fn test_reorder_embeddings_gap_missing_index() {
        // Length 2, but indices are 0 and 2 (missing 1)
        let entries = vec![(0, vec![1.0]), (2, vec![3.0])];
        let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
        assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
    }

    #[test]
    fn test_reorder_embeddings_out_of_range_extra() {
        let entries = vec![(0, vec![1.0]), (3, vec![4.0])];
        let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
        assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
        assert!(err.to_string().contains("out of range"));
    }

    #[test]
    fn test_reorder_embeddings_length_mismatch() {
        let entries = vec![(0, vec![1.0])];
        let err = reorder_embeddings_by_index(entries, 2).unwrap_err();
        assert!(matches!(err, RemoteEmbeddingError::InvalidResponse(_)));
        assert!(err.to_string().contains("expected 2"));
    }
}
