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
    /// Request timeout in seconds
    pub timeout_secs: u64,
    /// Maximum retries for failed requests
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

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| RemoteEmbeddingError::HttpClient(e.to_string()))?;

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

        let response = self
            .client
            .post("https://api.openai.com/v1/embeddings")
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .map_err(|e| RemoteEmbeddingError::ApiError(e.to_string()))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| RemoteEmbeddingError::ApiError(e.to_string()))?;

        if !status.is_success() {
            if status.as_u16() == 429 {
                return Err(RemoteEmbeddingError::RateLimitExceeded(
                    "OpenAI".to_string(),
                ));
            }
            return Err(RemoteEmbeddingError::ApiError(format!(
                "API returned {}: {}",
                status, response_text
            )));
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

        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| RemoteEmbeddingError::HttpClient(e.to_string()))?;

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
        let response = self
            .client
            .post(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .header("X-Client-Name", "leindex")
            .json(&request)
            .send()
            .await
            .map_err(|e| RemoteEmbeddingError::ApiError(e.to_string()))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| RemoteEmbeddingError::ApiError(e.to_string()))?;

        if !status.is_success() {
            if status.as_u16() == 429 {
                return Err(RemoteEmbeddingError::RateLimitExceeded(
                    "Cohere".to_string(),
                ));
            }
            return Err(RemoteEmbeddingError::ApiError(format!(
                "API returned {}: {}",
                status, response_text
            )));
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
}

/// Statuses that can be caused by one bad input (oversized/invalid content;
/// 500 is llama-server's oversized-input rejection). 502/503/504 are
/// availability/gateway failures: they fail the batch fast (endpoint-down
/// freeze path) instead of bisecting into many doomed requests.
fn is_bisectable_embedding_status(status: u16) -> bool {
    matches!(status, 400 | 413 | 422 | 500)
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
fn apply_doc_template<'a>(
    template: Option<&str>,
    title: Option<&str>,
    text: &'a str,
) -> std::borrow::Cow<'a, str> {
    let title = title.unwrap_or("none");
    match template {
        None => std::borrow::Cow::Borrowed(text),
        Some(t) => {
            let filled = if t.contains("{title}") {
                t.replace("{title}", title)
            } else {
                t.to_string()
            };
            if filled.contains("{text}") {
                std::borrow::Cow::Owned(filled.replace("{text}", text))
            } else {
                std::borrow::Cow::Owned(format!("{filled} {text}"))
            }
        }
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
        let client = Client::builder()
            .timeout(std::time::Duration::from_secs(config.timeout_secs))
            .build()
            .map_err(|e| RemoteEmbeddingError::HttpClient(e.to_string()))?;
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
        })
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
        let embeddings = self.embed_via_api(vec![input]).await?;
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

    /// Shared document-batch path: template with optional titles, send, and
    /// bisect-retry any batch the endpoint rejects wholesale.
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
        let mut rows = vec![None; inputs.len()];
        let mut pending = vec![(0, inputs.len())];
        let mut skipped = Vec::new();
        let mut first_error = None;

        while let Some((start, end)) = pending.pop() {
            match self.embed_via_api(inputs[start..end].to_vec()).await {
                Ok(embeddings) if embeddings.len() == end - start => {
                    for (index, embedding) in (start..end).zip(embeddings) {
                        rows[index] = Some(embedding);
                    }
                }
                Ok(embeddings) => {
                    return Err(RemoteEmbeddingError::InvalidResponse(format!(
                        "Expected {} embeddings, got {}",
                        end - start,
                        embeddings.len()
                    )));
                }
                Err(RemoteEmbeddingError::HttpStatus { status, message })
                    if is_bisectable_embedding_status(status) =>
                {
                    first_error.get_or_insert((status, message));
                    if end - start == 1 {
                        skipped.push(start);
                    } else {
                        tracing::debug!(
                            start,
                            end,
                            batch_len = end - start,
                            "Bisecting failed remote embedding batch"
                        );
                        let middle = start + (end - start) / 2;
                        pending.push((middle, end));
                        pending.push((start, middle));
                    }
                }
                Err(error) => return Err(error),
            }
        }

        let embedded = rows.iter().filter(|row| row.is_some()).count();
        if !skipped.is_empty() && embedded == 0 {
            let (status, _) = first_error.expect("failed leaves have an HTTP status");
            return Err(RemoteEmbeddingError::HttpStatus {
                status,
                message: format!("all {} inputs failed individually", skipped.len()),
            });
        }
        if !skipped.is_empty() {
            tracing::warn!(skipped_indices = ?skipped, "Skipping remote embedding inputs rejected individually");
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
}

impl CustomEmbeddingProvider {
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
            .map_err(|e| RemoteEmbeddingError::ApiError(e.to_string()))?;

        let status = response.status();
        let response_text = response
            .text()
            .await
            .map_err(|e| RemoteEmbeddingError::ApiError(e.to_string()))?;

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
}

/// Short fingerprint of the active task templates for cache identities.
/// FNV-1a 64-bit: stable, dependency-free, and cheap insurance that a
/// template change never reuses another template's cache namespace.
fn template_fingerprint(doc: Option<&str>, query: Option<&str>) -> Option<String> {
    if doc.is_none() && query.is_none() {
        return None;
    }
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for part in [doc.unwrap_or(""), "\u{0}", query.unwrap_or("")] {
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
        let provider: Arc<dyn RemoteEmbeddingProvider> = match &config.provider {
            RemoteProvider::OpenAI { .. } => Arc::new(OpenAIEmbeddingProvider::new(config)?),
            RemoteProvider::Cohere { .. } => Arc::new(CohereEmbeddingProvider::new(config)?),
            RemoteProvider::Custom { .. } => Arc::new(CustomEmbeddingProvider::new(config)?),
        };

        Ok(Self {
            provider,
            identity_prefix,
            template_fingerprint,
        })
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
