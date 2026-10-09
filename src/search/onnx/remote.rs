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

    /// Get the embedding dimension
    fn dimension(&self) -> usize;

    /// Runtime-true dimension when the provider knows better than the legacy
    /// `default_dimension()` table: the configured override or a dimension
    /// measured from a real response. `None` = no better information (the
    /// legacy default stays authoritative and the identity string is stable).
    fn measured_dimension(&self) -> Option<usize> {
        None
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

        #[derive(Deserialize)]
        struct OpenAIEmbedding {
            embedding: Vec<f32>,
        }

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

        Ok(openai_response
            .data
            .into_iter()
            .map(|e| e.embedding)
            .collect())
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
    /// 0 = not yet measured; else the vector length observed in the first
    /// successful response.
    measured: std::sync::atomic::AtomicUsize,
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
        let embeddings = self.embed_batch(vec![text]).await?;
        embeddings.into_iter().next().ok_or_else(|| {
            RemoteEmbeddingError::InvalidResponse("No embedding returned".to_string())
        })
    }

    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError> {
        #[derive(Serialize)]
        struct CustomRequest<'a> {
            model: String,
            input: Vec<&'a str>,
        }

        #[derive(Deserialize)]
        struct CustomResponse {
            data: Vec<CustomEmbedding>,
        }

        #[derive(Deserialize)]
        struct CustomEmbedding {
            embedding: Vec<f32>,
        }

        let request = CustomRequest {
            model: self.model.clone().unwrap_or_default(),
            input: texts,
        };

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
            return Err(RemoteEmbeddingError::ApiError(format!(
                "API returned {}: {}",
                status, response_text
            )));
        }

        let parsed: CustomResponse = serde_json::from_str(&response_text)
            .map_err(|e| RemoteEmbeddingError::InvalidResponse(e.to_string()))?;

        let embeddings: Vec<Vec<f32>> = parsed.data.into_iter().map(|e| e.embedding).collect();

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
        if let Some(expected) = known {
            for embedding in &embeddings {
                if embedding.len() != expected {
                    return Err(RemoteEmbeddingError::InvalidDimension {
                        expected,
                        got: embedding.len(),
                    });
                }
            }
        } else if let Some(first) = embeddings.first() {
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
}

/// Generic remote embedding provider that wraps specific implementations
#[derive(Clone)]
pub struct GenericRemoteProvider {
    provider: Arc<dyn RemoteEmbeddingProvider>,
    /// Identity prefix (provider, model, endpoint). Never contains
    /// credentials; the embedding dimension is appended at call time so a
    /// measured runtime dimension (custom endpoints) lands in the identity.
    identity_prefix: String,
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
        let identity_prefix = format!(
            "remote|{kind}|{model}|{}",
            config.base_url.as_deref().unwrap_or("")
        );
        let provider: Arc<dyn RemoteEmbeddingProvider> = match &config.provider {
            RemoteProvider::OpenAI { .. } => Arc::new(OpenAIEmbeddingProvider::new(config)?),
            RemoteProvider::Cohere { .. } => Arc::new(CohereEmbeddingProvider::new(config)?),
            RemoteProvider::Custom { .. } => Arc::new(CustomEmbeddingProvider::new(config)?),
        };

        Ok(Self {
            provider,
            identity_prefix,
        })
    }

    /// Stable identity of this provider for embedding caches (no secrets).
    /// The trailing dimension component is appended per call so a custom
    /// provider's measured runtime dimension is part of the identity.
    pub fn identity(&self) -> String {
        format!("{}|{}", self.identity_prefix, self.provider.dimension())
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

    fn dimension(&self) -> usize {
        self.provider.dimension()
    }

    fn measured_dimension(&self) -> Option<usize> {
        self.provider.measured_dimension()
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
    fn test_remote_config_openai() {
        let config = RemoteEmbeddingConfig::openai("test-key".to_string(), None);
        assert!(config.api_key.is_some());
    }
}
