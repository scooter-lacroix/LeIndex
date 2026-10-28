//! Hosted OpenAI and Cohere providers, plus the retry ladder only they use.
//!
//! Moved out of `remote.rs` by pure code motion (Large File Detection gate:
//! `remote.rs` crossed 2000 lines). `build_http_client`, `retry_backoff`,
//! `RemoteEmbeddingConfig` and the provider trait all stay in the parent; the
//! type re-exports below keep every existing path resolving.

use super::{
    RemoteEmbeddingConfig, RemoteEmbeddingError, RemoteEmbeddingProvider, RemoteProvider,
    build_http_client, reorder_embeddings_by_index, retry_backoff,
};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

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
        let embeddings = self
            .embed_with_input_type(vec![text], self.config.input_type_query.as_deref())
            .await?;
        embeddings.into_iter().next().ok_or_else(|| {
            RemoteEmbeddingError::InvalidResponse("No embedding returned".to_string())
        })
    }

    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        self.embed_with_input_type(texts, self.config.input_type_document.as_deref())
            .await
    }

    fn dimension(&self) -> usize {
        self.config.provider.default_dimension()
    }
}

impl CohereEmbeddingProvider {
    /// POST `/embed` for one batch with an explicit `input_type`.
    ///
    /// Cohere's `input_type` selects the retrieval side the vectors are tuned
    /// for, so the document and query paths MUST send different values. This
    /// used to hardcode `search_document` on both, which put queries in the
    /// document subspace and quietly degraded recall.
    ///
    /// `None` omits the field: only an operator-supplied `input_type: none`-ish
    /// config can produce that, and inventing a value is worse than letting the
    /// API reject the request loudly.
    async fn embed_with_input_type(
        &self,
        texts: Vec<&str>,
        input_type: Option<&str>,
    ) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
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
            #[serde(skip_serializing_if = "Option::is_none")]
            input_type: Option<&'a str>,
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
            input_type,
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
}
