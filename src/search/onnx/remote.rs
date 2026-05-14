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
}

impl Default for RemoteEmbeddingConfig {
    fn default() -> Self {
        Self {
            provider: RemoteProvider::default(),
            api_key: None,
            timeout_secs: 30,
            max_retries: 3,
            base_url: None,
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
#[async_trait]
pub trait RemoteEmbeddingProvider: Send + Sync {
    /// Generate embeddings for a single text
    async fn embed(&self, text: &str) -> Result<Vec<f32>, RemoteEmbeddingError>;

    /// Generate embeddings for multiple texts (batch)
    async fn embed_batch(&self, texts: Vec<&str>) -> Result<Vec<Vec<f32>>, RemoteEmbeddingError>;

    /// Get the embedding dimension
    fn dimension(&self) -> usize;
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
                ))
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
                ))
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

/// Generic remote embedding provider that wraps specific implementations
#[derive(Clone)]
pub struct GenericRemoteProvider {
    provider: Arc<dyn RemoteEmbeddingProvider>,
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
        let provider: Arc<dyn RemoteEmbeddingProvider> = match &config.provider {
            RemoteProvider::OpenAI { .. } => Arc::new(OpenAIEmbeddingProvider::new(config)?),
            RemoteProvider::Cohere { .. } => Arc::new(CohereEmbeddingProvider::new(config)?),
            RemoteProvider::Custom { .. } => {
                return Err(RemoteEmbeddingError::ApiError(
                    "Custom provider not yet implemented".to_string(),
                ));
            }
        };

        Ok(Self { provider })
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
    fn test_remote_config_openai() {
        let config = RemoteEmbeddingConfig::openai("test-key".to_string(), None);
        assert!(config.api_key.is_some());
    }
}
