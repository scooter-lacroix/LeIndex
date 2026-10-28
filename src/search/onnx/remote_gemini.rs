//! Native Google Gemini embedding provider (`v1beta`).
//!
//! Moved out of `remote.rs` by pure code motion (Large File Detection gate:
//! `remote.rs` crossed 2000 lines). The type is re-exported from the parent so
//! every existing path keeps resolving.

use super::{
    RemoteEmbeddingConfig, RemoteEmbeddingError, RemoteEmbeddingProvider, RemoteProvider,
    build_http_client,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

/// Inputs per `batchEmbedContents` request when no limit was resolved.
///
/// Google's documented ceiling: the API answers anything larger with
/// `BatchEmbedContentsRequest.requests: at most 100 requests can be in one
/// batch`. Kept here as well as in the `gemini` preset because a `custom` route
/// can still reach this provider, and the ceiling belongs to the PROTOCOL, not
/// to the preset that happens to spell the name.
pub(crate) const DEFAULT_GEMINI_BATCH_LIMIT: usize = 100;

/// Native Google Gemini embedding provider (`v1beta`).
///
/// Why this is not the OpenAI-compatible path: Gemini's native protocol differs
/// from `/v1/embeddings` in three ways that the compat shim cannot express.
///
/// 1. **Shape.** A batch is `POST {base}/models/{model}:batchEmbedContents`
///    with `{"requests":[{"model","content"}]}`; one text is
///    `POST {base}/models/{model}:embedContent` with `{"content"}`.
/// 2. **No index.** `batchEmbedContents` returns `{"embeddings":[...]}` in
///    request order and carries NO per-row `index` field, so the
///    `reorder_embeddings_by_index` contract that every other provider satisfies
///    cannot be applied — order is the contract here.
/// 3. **Task intent.** `taskType` (`RETRIEVAL_DOCUMENT` vs `RETRIEVAL_QUERY`)
///    is how this protocol expresses asymmetric retrieval, where other
///    providers use text prefixes. It is driven by which trait method was
///    called: `embed` is the query path, `embed_batch` is the document path.
///    This is why the model-side templates are NOT applied here — composing
///    both would double-prefix every document.
///
/// `outputDimensionality` carries MRL truncation, so a configured
/// `remote_dimension` smaller than the model's native width is requested from
/// the server rather than sliced client-side.
pub struct GeminiEmbeddingProvider {
    client: Client,
    /// `{base}/models` with any trailing slash removed.
    base_url: String,
    model: String,
    api_key: String,
    /// Requested `outputDimensionality`, when the operator pinned one.
    dimension_override: Option<usize>,
    /// Bootstrap width used only until the first response is measured.
    bootstrap_dimension: usize,
    /// 0 = not yet measured; else the vector length of the first successful
    /// response. Mirrors [`super::CustomEmbeddingProvider`]'s measured width.
    measured: std::sync::atomic::AtomicUsize,
    /// Hard cap on inputs per request (`None` = no cap).
    ///
    /// `batchEmbedContents` rejects any request above the provider's documented
    /// ceiling with a 400 naming it — `at most 100 requests can be in one
    /// batch` — and this path has no bisection to fall back on, so the cap must
    /// be honoured before the request is built rather than recovered from
    /// afterwards.
    batch_limit: Option<usize>,
    /// Retry budget for transient failures (429, 5xx, transport).
    ///
    /// This provider does not share `CustomEmbeddingProvider`'s bisection
    /// machinery, so it must own its retry ladder: without it a single 429
    /// permanently freezes the neural generation.
    max_retries: usize,
}

/// Which side of an asymmetric retrieval pair a request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GeminiTask {
    /// `RETRIEVAL_DOCUMENT` — the corpus side.
    Document,
    /// `RETRIEVAL_QUERY` — the query side.
    Query,
}

impl GeminiTask {
    /// The `taskType` string this protocol expects.
    fn as_str(self) -> &'static str {
        match self {
            Self::Document => "RETRIEVAL_DOCUMENT",
            Self::Query => "RETRIEVAL_QUERY",
        }
    }
}

impl GeminiEmbeddingProvider {
    /// Create a native Gemini embedding provider.
    pub fn new(config: RemoteEmbeddingConfig) -> Result<Self, RemoteEmbeddingError> {
        let model = match &config.provider {
            RemoteProvider::Gemini { model } => model.clone(),
            _ => {
                return Err(RemoteEmbeddingError::ApiError(
                    "Invalid provider".to_string(),
                ));
            }
        };
        // An explicit `base_url` wins so a proxy or a regional endpoint is
        // reachable; otherwise the provider's documented default applies.
        let base_url = config
            .base_url
            .clone()
            .or_else(|| config.provider.default_base_url().map(str::to_string))
            .ok_or_else(|| {
                RemoteEmbeddingError::ApiError("Gemini requires a base URL".to_string())
            })?;
        let api_key = config
            .api_key
            .clone()
            .ok_or_else(|| RemoteEmbeddingError::ApiKeyNotFound("Gemini".to_string()))?;
        let client = build_http_client(&config)?;
        Ok(Self {
            client,
            base_url: base_url.trim_end_matches('/').to_string(),
            model,
            api_key,
            dimension_override: config.remote_dimension,
            bootstrap_dimension: config.provider.default_dimension(),
            measured: std::sync::atomic::AtomicUsize::new(0),
            batch_limit: config.batch_limit.filter(|limit| *limit > 0),
            max_retries: config.max_retries,
        })
    }

    /// Current dimension: measured > override > bootstrap default. Same
    /// precedence as [`super::CustomEmbeddingProvider`]'s current dimension.
    fn current_dimension(&self) -> usize {
        let measured = self.measured.load(std::sync::atomic::Ordering::Relaxed);
        if measured > 0 {
            measured
        } else {
            self.dimension_override.unwrap_or(self.bootstrap_dimension)
        }
    }

    /// Inputs per request to actually send.
    ///
    /// Falls back to [`DEFAULT_GEMINI_BATCH_LIMIT`] rather than to "no limit"
    /// when no cap was resolved: reaching this provider at all means the wire is
    /// Gemini, so the API's ceiling applies whether or not a preset was
    /// consulted (a `custom` route pointed at Gemini has no preset). Sending an
    /// unlimited batch there is a guaranteed 400.
    fn effective_batch_limit(&self) -> usize {
        self.batch_limit
            .unwrap_or(DEFAULT_GEMINI_BATCH_LIMIT)
            .max(1)
    }

    /// POST one native request and return the vectors in request order.
    ///
    /// `texts` must be non-empty and at or below [`Self::batch_limit`]; sizing
    /// is the caller's job because only it knows the whole run.
    ///
    /// Transient failures (429, 5xx, transport) are re-sent on the shared
    /// backoff ladder. Unlike [`super::CustomEmbeddingProvider`] this provider
    /// has no bisection layer above it, so it must own the retry itself:
    /// without it a single rate-limit response froze the whole neural
    /// generation after five consecutive failures.
    async fn embed_native(
        &self,
        texts: Vec<&str>,
        task: GeminiTask,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        let mut backoff_step: usize = 0;
        loop {
            match self.embed_native_once(&texts, task).await {
                Ok(embeddings) => return Ok(embeddings),
                Err(error) => {
                    if !super::is_retryable_embedding_error(&error)
                        || backoff_step >= self.max_retries
                    {
                        return Err(error);
                    }
                    let delay = super::retry_backoff(backoff_step);
                    tracing::warn!(
                        %error,
                        attempt = backoff_step + 1,
                        max_retries = self.max_retries,
                        delay_ms = delay.as_millis() as u64,
                        model = %self.model,
                        "Transient Gemini embedding failure; retrying the same request"
                    );
                    tokio::time::sleep(delay).await;
                    backoff_step += 1;
                }
            }
        }
    }

    /// The `:method` URL and JSON body for one native request.
    ///
    /// The resource name, not the bare model id: the batch API requires
    /// `models/<id>` per request.
    fn native_request(
        &self,
        texts: &[&str],
        task: GeminiTask,
        single: bool,
    ) -> Result<(String, serde_json::Value), RemoteEmbeddingError> {
        let resource = format!("models/{}", self.model);
        let task_type = task.as_str();
        let output_dimensionality = self.dimension_override;
        let (method, body) = if single {
            let body = serde_json::to_value(GeminiSingleRequest {
                content: gemini_content(texts[0]),
                task_type,
                output_dimensionality,
            });
            ("embedContent", body)
        } else {
            let body = serde_json::to_value(GeminiBatchRequestList {
                requests: texts
                    .iter()
                    .map(|text| GeminiBatchRequestRef {
                        model: resource.clone(),
                        content: gemini_content(text),
                        task_type,
                        output_dimensionality,
                    })
                    .collect(),
            });
            ("batchEmbedContents", body)
        };
        let body = body.map_err(|e| RemoteEmbeddingError::InvalidResponse(e.to_string()))?;
        Ok((format!("{}/{resource}:{method}", self.base_url), body))
    }

    /// POST one native request and return the raw response body.
    ///
    /// Exactly one HTTP attempt: the retry ladder lives in the caller.
    async fn send_native_request(
        &self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<String, RemoteEmbeddingError> {
        let response = self
            .client
            .post(url)
            // The native API takes the key as a header, not a Bearer token.
            .header("x-goog-api-key", &self.api_key)
            .header("Content-Type", "application/json")
            .json(body)
            .send()
            .await
            .map_err(|error| RemoteEmbeddingError::Transport(error.to_string()))?;

        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|e| RemoteEmbeddingError::Transport(e.to_string()))?;

        if status.is_success() {
            return Ok(text);
        }
        if status.as_u16() == 429 {
            return Err(RemoteEmbeddingError::RateLimitExceeded(
                "Gemini".to_string(),
            ));
        }
        Err(RemoteEmbeddingError::HttpStatus {
            status: status.as_u16(),
            message: text,
        })
    }

    /// Decode the single or batch native response into vectors, in request
    /// order.
    fn parse_native_embeddings(
        single: bool,
        response_text: &str,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        if single {
            let parsed: GeminiSingleResponse = serde_json::from_str(response_text)
                .map_err(|e| RemoteEmbeddingError::InvalidResponse(e.to_string()))?;
            return Ok(vec![parsed.embedding.values]);
        }
        let parsed: GeminiBatchResponse = serde_json::from_str(response_text)
            .map_err(|e| RemoteEmbeddingError::InvalidResponse(e.to_string()))?;
        Ok(parsed.embeddings.into_iter().map(|e| e.values).collect())
    }

    /// The width a response must match: the measured width once one has been
    /// seen, else the operator's pin.
    ///
    /// Deliberately narrower than [`Self::current_dimension`], which falls back
    /// to the bootstrap default. That default must never be enforced on a
    /// response — see [`Self::enforce_dimensions`].
    fn enforced_dimension(&self) -> Option<usize> {
        let measured = self.measured.load(std::sync::atomic::Ordering::Relaxed);
        if measured > 0 {
            Some(measured)
        } else {
            self.dimension_override
        }
    }

    /// Dimension truth, identical to the OpenAI-compatible path: a known
    /// dimension (measured or pinned) is enforced on every row; otherwise the
    /// first successful length becomes the measurement and the bootstrap
    /// default is never enforced.
    fn enforce_dimensions(&self, embeddings: &[Vec<f32>]) -> Result<(), RemoteEmbeddingError> {
        let expected = match self.enforced_dimension() {
            Some(expected) => expected,
            None => match embeddings.first() {
                Some(first) => first.len(),
                None => return Ok(()),
            },
        };
        for embedding in embeddings {
            if embedding.len() != expected {
                return Err(RemoteEmbeddingError::InvalidDimension {
                    expected,
                    got: embedding.len(),
                });
            }
        }
        Ok(())
    }

    /// The first successful response establishes the measured width; every
    /// later one is checked against it by [`Self::enforce_dimensions`].
    fn record_measured_dimension(&self, embeddings: &[Vec<f32>]) {
        use std::sync::atomic::Ordering;
        if self.measured.load(Ordering::Relaxed) != 0 {
            return;
        }
        let Some(first) = embeddings.first() else {
            return;
        };
        self.measured.store(first.len(), Ordering::Relaxed);
        tracing::info!(
            dimension = first.len(),
            model = %self.model,
            "Measured Gemini embedding dimension from first response"
        );
    }

    /// POST one native request. Exactly one HTTP attempt.
    async fn embed_native_once(
        &self,
        texts: &[&str],
        task: GeminiTask,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        let single = texts.len() == 1;
        let expected = texts.len();
        let (url, body) = self.native_request(texts, task, single)?;
        let response_text = self.send_native_request(&url, &body).await?;
        let embeddings = Self::parse_native_embeddings(single, &response_text)?;

        // Order IS the contract here: there is no `index` to verify against, so
        // a row-count mismatch is the only evidence the response is unusable,
        // and guessing would silently attach vectors to the wrong documents.
        if embeddings.len() != expected {
            return Err(RemoteEmbeddingError::InvalidResponse(format!(
                "expected {expected} embeddings in Gemini response, got {}",
                embeddings.len()
            )));
        }

        self.enforce_dimensions(&embeddings)?;
        self.record_measured_dimension(&embeddings);
        Ok(embeddings)
    }
}

/// `{"embedding":{"values":[...]}}` — the `embedContent` (single text) reply.
///
/// These stay at module scope rather than inside `embed_native_once` because
/// decoding was split into [`GeminiEmbeddingProvider::parse_native_embeddings`].
#[derive(Deserialize)]
struct GeminiValues {
    values: Vec<f32>,
}

#[derive(Deserialize)]
struct GeminiBatchResponse {
    embeddings: Vec<GeminiValues>,
}

#[derive(Deserialize)]
struct GeminiSingleResponse {
    embedding: GeminiValues,
}

#[async_trait]
impl RemoteEmbeddingProvider for GeminiEmbeddingProvider {
    async fn embed(&self, text: &str) -> Result<Vec<f32>, RemoteEmbeddingError> {
        // Single-text calls are the QUERY path.
        let embeddings = self.embed_native(vec![text], GeminiTask::Query).await?;
        embeddings.into_iter().next().ok_or_else(|| {
            RemoteEmbeddingError::InvalidResponse("No embedding returned".to_string())
        })
    }

    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        if texts.is_empty() {
            return Ok(Vec::new());
        }
        // Batch calls are the DOCUMENT path. Split into requests the provider
        // will actually accept: `batchEmbedContents` rejects anything above its
        // documented ceiling with a 400 and there is no bisection below this
        // provider to recover from one, so the split has to happen here.
        let chunk = self.effective_batch_limit();
        let mut rows = Vec::with_capacity(texts.len());
        for window in texts.chunks(chunk) {
            rows.extend(
                self.embed_native(window.to_vec(), GeminiTask::Document)
                    .await?,
            );
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

/// The `batchEmbedContents` body: one wrapping `requests` array.
///
/// Declared at module scope (not inside `embed_native`) because it borrows the
/// per-request entries, which outlive the builder closure.
#[derive(Serialize)]
struct GeminiBatchRequestList<'a> {
    requests: Vec<GeminiBatchRequestRef<'a>>,
}

/// One borrow-carrying entry of a `batchEmbedContents` request.
#[derive(Serialize)]
struct GeminiBatchRequestRef<'a> {
    model: String,
    content: GeminiContentRef<'a>,
    #[serde(rename = "taskType")]
    task_type: &'static str,
    #[serde(
        rename = "outputDimensionality",
        skip_serializing_if = "Option::is_none"
    )]
    output_dimensionality: Option<usize>,
}

/// The single-text (`embedContent`) body: the same fields, one level flatter
/// (no `model`, no wrapping `requests` array).
#[derive(Serialize)]
struct GeminiSingleRequest<'a> {
    content: GeminiContentRef<'a>,
    #[serde(rename = "taskType")]
    task_type: &'static str,
    #[serde(
        rename = "outputDimensionality",
        skip_serializing_if = "Option::is_none"
    )]
    output_dimensionality: Option<usize>,
}

/// The `content` object of a native Gemini request.
#[derive(Serialize)]
struct GeminiContentRef<'a> {
    parts: Vec<GeminiPartRef<'a>>,
}

/// One `parts` entry: the text being embedded.
#[derive(Serialize)]
struct GeminiPartRef<'a> {
    text: &'a str,
}

/// Wrap one text as a single-part Gemini `content`.
///
/// A free function rather than a closure so the returned value borrows only
/// from `text` — a closure could not express that without capturing the input
/// lifetime.
fn gemini_content(text: &str) -> GeminiContentRef<'_> {
    GeminiContentRef {
        parts: vec![GeminiPartRef { text }],
    }
}
