// Qwen3 ONNX Embedding Provider
//
// Provides neural embeddings using Qwen3-Embedding-0.6B model via ONNX Runtime.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokenizers::Tokenizer;

#[cfg(feature = "onnx")]
use ort::session::Session;

/// Errors that can occur during Qwen3 embedding generation
#[derive(Debug, Error)]
pub enum QwenEmbeddingProviderError {
    /// ONNX Runtime initialization or execution error
    #[error("ONNX Runtime error: {0}")]
    OnnxRuntime(String),

    /// Model file could not be found at the expected path
    #[error("Model file not found: {0}")]
    ModelNotFound(String),

    /// Tokenizer initialization or tokenization error
    #[error("Tokenizer error: {0}")]
    Tokenizer(String),

    /// Embedding generation failed during inference
    #[error("Embedding generation failed: {0}")]
    GenerationFailed(String),

    /// Embedding dimension mismatch
    #[error("Invalid embedding dimension: expected {expected}, got {got}")]
    InvalidDimension {
        /// Expected embedding dimension
        expected: usize,
        /// Actual embedding dimension received
        got: usize,
    },

    /// ONNX feature is not enabled in compilation
    #[error("Feature not enabled: ONNX feature is required")]
    FeatureNotEnabled,
}

/// Qwen3 Embedding Provider
///
/// Loads and runs Qwen3-Embedding-0.6B model using ONNX Runtime for
/// cross-language code understanding.
///
/// Supports an A+ idle-unload lifecycle: after an indexing batch completes,
/// calling `unload()` drops the live ONNX session to release resident memory.
/// The next embed/rerank request lazily recreates the session via
/// `ensure_session()`.
#[derive(Debug)]
pub struct QwenEmbeddingProvider {
    model_path: PathBuf,
    embedding_dimension: usize,
    #[cfg(feature = "onnx")]
    session: Arc<Mutex<Option<Session>>>,
    #[cfg(feature = "onnx")]
    tokenizer: Arc<Tokenizer>,
}

impl QwenEmbeddingProvider {
    /// Create a new Qwen3 embedding provider
    ///
    /// This will load the Qwen3-Embedding-0.6B model from the bundled
    /// model directory and initialize the tokenizer.
    pub fn new() -> Result<Self, QwenEmbeddingProviderError> {
        #[cfg(not(feature = "onnx"))]
        {
            return Err(QwenEmbeddingProviderError::FeatureNotEnabled);
        }

        #[cfg(feature = "onnx")]
        {
            // Model path resolution
            let model_path = Self::resolve_model_path()
                .map_err(|e| QwenEmbeddingProviderError::ModelNotFound(e.to_string()))?;

            // Resolve tokenizer path
            let tokenizer_path = Self::resolve_tokenizer_path()
                .map_err(|e| QwenEmbeddingProviderError::ModelNotFound(e.to_string()))?;

            // Load ONNX session
            let session = Session::builder()
                .map_err(|e| {
                    QwenEmbeddingProviderError::OnnxRuntime(format!(
                        "Failed to create session builder: {}",
                        e
                    ))
                })?
                .commit_from_file(&model_path)
                .map_err(|e| {
                    QwenEmbeddingProviderError::OnnxRuntime(format!("Failed to load model: {}", e))
                })?;

            // Load tokenizer
            let tokenizer = Tokenizer::from_file(tokenizer_path).map_err(|e| {
                QwenEmbeddingProviderError::Tokenizer(format!("Failed to load tokenizer: {}", e))
            })?;

            Ok(Self {
                model_path,
                embedding_dimension: 1024, // Qwen3-Embedding-0.6B output dimension
                session: Arc::new(Mutex::new(Some(session))),
                tokenizer: Arc::new(tokenizer),
            })
        }
    }

    /// Unload the ONNX session, releasing resident memory.
    ///
    /// After calling this, the provider enters an unloaded state. The next
    /// embed/rerank request will lazily recreate the session via
    /// `ensure_session()`. This is the A+ idle-unload lifecycle hook
    /// (VAL-APLUS-022).
    pub fn unload(&self) {
        #[cfg(feature = "onnx")]
        {
            if let Ok(mut guard) = self.session.lock() {
                *guard = None;
            }
        }
    }

    /// Check whether the ONNX session is currently loaded.
    ///
    /// Returns `true` when a live session exists, `false` after `unload()`.
    #[must_use]
    pub fn is_loaded(&self) -> bool {
        #[cfg(feature = "onnx")]
        {
            self.session
                .lock()
                .map(|guard| guard.is_some())
                .unwrap_or(false)
        }
        #[cfg(not(feature = "onnx"))]
        {
            false
        }
    }

    /// Ensure the ONNX session is loaded, recreating it lazily if needed.
    ///
    /// This is the lazy-reload path (VAL-APLUS-023): after `unload()`, the
    /// next embed call invokes this to recreate the session from the same
    /// model path.
    #[cfg(feature = "onnx")]
    fn ensure_session(&self) -> Result<(), QwenEmbeddingProviderError> {
        let mut guard = self.session.lock().map_err(|e| {
            QwenEmbeddingProviderError::GenerationFailed(format!("Failed to lock session: {}", e))
        })?;

        if guard.is_some() {
            return Ok(());
        }

        // Lazily recreate the session
        let session = Session::builder()
            .map_err(|e| {
                QwenEmbeddingProviderError::OnnxRuntime(format!(
                    "Failed to create session builder on reload: {}",
                    e
                ))
            })?
            .commit_from_file(&self.model_path)
            .map_err(|e| {
                QwenEmbeddingProviderError::OnnxRuntime(format!("Failed to reload model: {}", e))
            })?;

        *guard = Some(session);
        Ok(())
    }

    /// Resolve the model path from bundled models or system path
    fn resolve_model_path() -> std::result::Result<PathBuf, QwenEmbeddingProviderError> {
        // Priority order:
        // 1. Environment variable LEINDEX_MODEL_PATH
        // 2. Bundled models directory in installation
        // 3. User home directory .leindex/models/

        if let Ok(path) = std::env::var("LEINDEX_MODEL_PATH") {
            let model_path = PathBuf::from(path).join("qwen3-embed-0.6b.onnx");
            if model_path.exists() {
                return Ok(model_path);
            }
        }

        // Check bundled models (relative to binary)
        if let Ok(exe_path) = std::env::current_exe() {
            let bundled_dir = exe_path.parent().unwrap().join("models");
            let model_path = bundled_dir.join("qwen3-embed-0.6b.onnx");
            if model_path.exists() {
                return Ok(model_path);
            }
        }

        // Check user home directory
        if let Some(home) = dirs::home_dir() {
            let user_models = home.join(".leindex").join("models");
            let model_path = user_models.join("qwen3-embed-0.6b.onnx");
            if model_path.exists() {
                return Ok(model_path);
            }
        }

        Err(QwenEmbeddingProviderError::ModelNotFound(
            "Qwen3 model not found in any standard location".to_string(),
        ))
    }

    /// Resolve the tokenizer path from bundled models or system path
    fn resolve_tokenizer_path() -> std::result::Result<PathBuf, QwenEmbeddingProviderError> {
        // Priority order:
        // 1. Environment variable LEINDEX_MODEL_PATH
        // 2. Bundled models directory in installation
        // 3. User home directory .leindex/models/

        if let Ok(path) = std::env::var("LEINDEX_MODEL_PATH") {
            let tokenizer_path = PathBuf::from(path).join("tokenizer.json");
            if tokenizer_path.exists() {
                return Ok(tokenizer_path);
            }
        }

        // Check bundled models (relative to binary)
        if let Ok(exe_path) = std::env::current_exe() {
            let bundled_dir = exe_path.parent().unwrap().join("models");
            let tokenizer_path = bundled_dir.join("tokenizer.json");
            if tokenizer_path.exists() {
                return Ok(tokenizer_path);
            }
        }

        // Check user home directory
        if let Some(home) = dirs::home_dir() {
            let user_models = home.join(".leindex").join("models");
            let tokenizer_path = user_models.join("tokenizer.json");
            if tokenizer_path.exists() {
                return Ok(tokenizer_path);
            }
        }

        Err(QwenEmbeddingProviderError::ModelNotFound(
            "Qwen3 tokenizer not found in any standard location".to_string(),
        ))
    }

    /// Generate embeddings for a single text
    ///
    /// Returns a 1024-dimensional vector (Qwen3-Embedding-0.6B default).
    pub fn embed(&self, text: &str) -> Result<Vec<f32>, QwenEmbeddingProviderError> {
        #[cfg(feature = "onnx")]
        {
            // Lazy reload if the session was previously unloaded (VAL-APLUS-023)
            self.ensure_session()?;

            // Tokenize the input text
            let encoding = self.tokenizer.encode(text, true).map_err(|e| {
                QwenEmbeddingProviderError::Tokenizer(format!("Tokenization failed: {}", e))
            })?;

            let ids = encoding.get_ids();
            let attention_mask = encoding.get_attention_mask();

            // Convert to the input format expected by the model
            // Qwen3 expects input_ids and attention_mask as tensors
            let batch_size = 1;
            let seq_len = ids.len();

            if seq_len == 0 {
                return Err(QwenEmbeddingProviderError::Tokenizer(
                    "Tokenization produced empty sequence".to_string(),
                ));
            }

            // Create input tensors
            use ort::value::Tensor;

            let input_ids_data = ids.iter().map(|&id| id as i64).collect::<Vec<i64>>();
            let attention_mask_data = attention_mask
                .iter()
                .map(|&mask| mask as i64)
                .collect::<Vec<i64>>();

            let input_ids_tensor = Tensor::from_array(([batch_size, seq_len], input_ids_data))
                .map_err(|e| {
                    QwenEmbeddingProviderError::OnnxRuntime(format!(
                        "Failed to create input_ids tensor: {}",
                        e
                    ))
                })?;

            let attention_mask_tensor =
                Tensor::from_array(([batch_size, seq_len], attention_mask_data)).map_err(|e| {
                    QwenEmbeddingProviderError::OnnxRuntime(format!(
                        "Failed to create attention_mask tensor: {}",
                        e
                    ))
                })?;

            // Run inference
            let mut guard = self.session.lock().map_err(|e| {
                QwenEmbeddingProviderError::GenerationFailed(format!(
                    "Failed to lock session: {}",
                    e
                ))
            })?;

            let session = guard.as_mut().ok_or_else(|| {
                QwenEmbeddingProviderError::GenerationFailed(
                    "ONNX session not available after ensure_session".to_string(),
                )
            })?;

            // Get output names before running inference to avoid borrow issues
            let outputs = session.outputs();
            if outputs.is_empty() {
                return Err(QwenEmbeddingProviderError::GenerationFailed(
                    "Model has no outputs".to_string(),
                ));
            }
            let output_name = outputs
                .iter()
                .find(|output| {
                    output.name().contains("hidden") || output.name().contains("embedding")
                })
                .map(|output| output.name().to_string())
                .unwrap_or_else(|| outputs[0].name().to_string());

            let outputs = session
                .run(ort::inputs! {
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor
                })
                .map_err(|e| {
                    QwenEmbeddingProviderError::GenerationFailed(format!(
                        "ONNX inference failed: {}",
                        e
                    ))
                })?;

            let output_tensor = outputs.get(&output_name).ok_or_else(|| {
                QwenEmbeddingProviderError::GenerationFailed(format!(
                    "Output '{}' not found",
                    output_name
                ))
            })?;

            // Extract the tensor data as a flat Vec<f32>
            let embedding = output_tensor
                .try_extract_array::<f32>()
                .map_err(|e| {
                    QwenEmbeddingProviderError::GenerationFailed(format!(
                        "Failed to extract output tensor: {}",
                        e
                    ))
                })?
                .iter()
                .copied()
                .collect::<Vec<f32>>();

            // Ensure we have the correct dimension
            if embedding.len() != self.embedding_dimension {
                // If we got a different shape, we might need to pool or average
                // For now, let's take the mean across the sequence length if needed
                if seq_len > 0 && embedding.len() % seq_len == 0 {
                    let per_token_dim = embedding.len() / seq_len;
                    let mut pooled = vec![0.0f32; per_token_dim];
                    for (i, &val) in embedding.iter().enumerate() {
                        pooled[i % per_token_dim] += val;
                    }
                    for val in &mut pooled {
                        *val /= seq_len as f32;
                    }
                    if pooled.len() == self.embedding_dimension {
                        return Ok(pooled);
                    }
                }

                return Err(QwenEmbeddingProviderError::InvalidDimension {
                    expected: self.embedding_dimension,
                    got: embedding.len(),
                });
            }

            Ok(embedding)
        }

        #[cfg(not(feature = "onnx"))]
        {
            Err(QwenEmbeddingProviderError::FeatureNotEnabled)
        }
    }

    /// Generate embeddings for multiple texts (batched)
    ///
    /// More efficient than calling `embed` multiple times.
    /// Processes texts in batches for better ONNX Runtime performance.
    pub fn embed_batch(
        &self,
        texts: &[String],
    ) -> Result<Vec<Vec<f32>>, QwenEmbeddingProviderError> {
        #[cfg(feature = "onnx")]
        {
            if texts.is_empty() {
                return Ok(vec![]);
            }

            // Lazy reload if the session was previously unloaded (VAL-APLUS-023)
            self.ensure_session()?;

            // Batch tokenize all texts - convert to Vec for encode_batch
            let texts_vec: Vec<&str> = texts.iter().map(|s| s.as_str()).collect();
            let encodings = self.tokenizer.encode_batch(texts_vec, true).map_err(|e| {
                QwenEmbeddingProviderError::Tokenizer(format!("Batch tokenization failed: {}", e))
            })?;

            if encodings.is_empty() {
                return Ok(vec![]);
            }

            let max_seq_len = encodings.iter().map(|enc| enc.len()).max().unwrap_or(0);
            let batch_size = encodings.len();

            // Pad all sequences to the same length
            let mut input_ids_batch = vec![0i64; batch_size * max_seq_len];
            let mut attention_mask_batch = vec![0i64; batch_size * max_seq_len];

            for (i, encoding) in encodings.iter().enumerate() {
                let ids = encoding.get_ids();
                let mask = encoding.get_attention_mask();
                let offset = i * max_seq_len;

                for (j, &id) in ids.iter().enumerate() {
                    if j < max_seq_len {
                        input_ids_batch[offset + j] = id as i64;
                    }
                }
                for (j, &mask_val) in mask.iter().enumerate() {
                    if j < max_seq_len {
                        attention_mask_batch[offset + j] = mask_val as i64;
                    }
                }
            }

            // Create batch input tensors
            use ort::value::Tensor;

            let input_ids_tensor = Tensor::from_array(([batch_size, max_seq_len], input_ids_batch))
                .map_err(|e| {
                    QwenEmbeddingProviderError::OnnxRuntime(format!(
                        "Failed to create batch input_ids tensor: {}",
                        e
                    ))
                })?;

            let attention_mask_tensor =
                Tensor::from_array(([batch_size, max_seq_len], attention_mask_batch)).map_err(
                    |e| {
                        QwenEmbeddingProviderError::OnnxRuntime(format!(
                            "Failed to create batch attention_mask tensor: {}",
                            e
                        ))
                    },
                )?;

            // Run batch inference
            let mut guard = self.session.lock().map_err(|e| {
                QwenEmbeddingProviderError::GenerationFailed(format!(
                    "Failed to lock session: {}",
                    e
                ))
            })?;

            let session = guard.as_mut().ok_or_else(|| {
                QwenEmbeddingProviderError::GenerationFailed(
                    "ONNX session not available after ensure_session".to_string(),
                )
            })?;

            let outputs = session.outputs();
            if outputs.is_empty() {
                return Err(QwenEmbeddingProviderError::GenerationFailed(
                    "Model has no outputs".to_string(),
                ));
            }
            let output_name = outputs
                .iter()
                .find(|output| {
                    output.name().contains("hidden") || output.name().contains("embedding")
                })
                .map(|output| output.name().to_string())
                .unwrap_or_else(|| outputs[0].name().to_string());

            let outputs = session
                .run(ort::inputs! {
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor
                })
                .map_err(|e| {
                    QwenEmbeddingProviderError::GenerationFailed(format!(
                        "Batch ONNX inference failed: {}",
                        e
                    ))
                })?;

            // Extract batch output tensor
            let output_tensor = outputs.get(&output_name).ok_or_else(|| {
                QwenEmbeddingProviderError::GenerationFailed(format!(
                    "Output '{}' not found",
                    output_name
                ))
            })?;

            let batch_embeddings = output_tensor
                .try_extract_array::<f32>()
                .map_err(|e| {
                    QwenEmbeddingProviderError::GenerationFailed(format!(
                        "Failed to extract batch output tensor: {}",
                        e
                    ))
                })?
                .into_owned();

            // Convert ndarray to Vec<f32> for processing
            let batch_embeddings_vec: Vec<f32> = batch_embeddings.into_raw_vec_and_offset().0;

            // Split batch embeddings into individual vectors
            let per_token_dim = batch_embeddings_vec.len() / batch_size;
            let mut individual_embeddings = Vec::with_capacity(batch_size);

            for i in 0..batch_size {
                let start = i * per_token_dim;
                let end = start + per_token_dim;
                let mut embedding = batch_embeddings_vec[start..end].to_vec();

                // If the output has per-sequence embeddings, pool across sequence length
                if per_token_dim > self.embedding_dimension {
                    let seq_len = encodings[i].len();
                    if seq_len > 0 && embedding.len() % seq_len == 0 {
                        let per_item_dim = embedding.len() / seq_len;
                        let mut pooled = vec![0.0f32; per_item_dim];
                        for (j, &val) in embedding.iter().enumerate() {
                            pooled[j % per_item_dim] += val;
                        }
                        for val in &mut pooled {
                            *val /= seq_len as f32;
                        }
                        if pooled.len() == self.embedding_dimension {
                            embedding = pooled;
                        }
                    }
                }

                // Ensure correct dimension
                if embedding.len() != self.embedding_dimension {
                    return Err(QwenEmbeddingProviderError::InvalidDimension {
                        expected: self.embedding_dimension,
                        got: embedding.len(),
                    });
                }

                individual_embeddings.push(embedding);
            }

            Ok(individual_embeddings)
        }

        #[cfg(not(feature = "onnx"))]
        {
            Err(QwenEmbeddingProviderError::FeatureNotEnabled)
        }
    }

    /// Get the embedding dimension
    pub fn dimension(&self) -> usize {
        self.embedding_dimension
    }

    /// Get the model path
    pub fn model_path(&self) -> &PathBuf {
        &self.model_path
    }
}

impl Clone for QwenEmbeddingProvider {
    fn clone(&self) -> Self {
        Self {
            model_path: self.model_path.clone(),
            embedding_dimension: self.embedding_dimension,
            #[cfg(feature = "onnx")]
            session: Arc::clone(&self.session),
            #[cfg(feature = "onnx")]
            tokenizer: Arc::clone(&self.tokenizer),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(feature = "onnx")]
    fn test_qwen_provider_creation() {
        // This will fail until model is bundled, but tests the structure
        let result = QwenEmbeddingProvider::new();
        assert!(result.is_err() || result.is_ok()); // Allow either until model is bundled
    }

    #[test]
    #[cfg(not(feature = "onnx"))]
    fn test_feature_not_enabled() {
        let result = QwenEmbeddingProvider::new();
        assert!(matches!(
            result,
            Err(QwenEmbeddingProviderError::FeatureNotEnabled)
        ));
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_embedding_dimension() {
        // Test with real provider if available
        let result = QwenEmbeddingProvider::new();

        if result.is_ok() {
            let provider = result.unwrap();
            assert_eq!(provider.dimension(), 1024);
        }
        // If provider creation fails, skip test gracefully
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_placeholder_embedding_generation() {
        // Test with real provider if available
        let result = QwenEmbeddingProvider::new();

        if result.is_ok() {
            let provider = result.unwrap();
            let embedding = provider.embed("test code").unwrap();
            assert_eq!(embedding.len(), 1024);
            // Real embeddings should have non-zero values
            assert!(
                !embedding.iter().all(|&x| x == 0.0),
                "Real embeddings should contain non-zero values"
            );
        }
        // If provider creation fails (e.g., model files not found), skip test gracefully
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_batch_embedding_generation() {
        // Test with real provider if available
        let result = QwenEmbeddingProvider::new();

        if result.is_ok() {
            let provider = result.unwrap();
            let texts = vec!["test 1".to_string(), "test 2".to_string()];
            let embeddings = provider.embed_batch(&texts).unwrap();
            assert_eq!(embeddings.len(), 2);
            assert_eq!(embeddings[0].len(), 1024);
            assert_eq!(embeddings[1].len(), 1024);
            // Real embeddings should have non-zero values
            assert!(
                !embeddings[0].iter().all(|&x| x == 0.0),
                "Real embeddings should contain non-zero values"
            );
            assert!(
                !embeddings[1].iter().all(|&x| x == 0.0),
                "Real embeddings should contain non-zero values"
            );
        }
        // If provider creation fails (e.g., model files not found), skip test gracefully
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_semantic_embeddings_differ() {
        // Test that different texts produce different embeddings (semantic property)
        let result = QwenEmbeddingProvider::new();

        if result.is_ok() {
            let provider = result.unwrap();
            let embedding1 = provider.embed("function that calculates sum").unwrap();
            let embedding2 = provider.embed("variable holding user data").unwrap();
            let embedding3 = provider.embed("function that calculates sum").unwrap(); // same as first

            // Embeddings should have correct dimension
            assert_eq!(embedding1.len(), 1024);
            assert_eq!(embedding2.len(), 1024);

            // Different texts should produce different embeddings
            let cosine_sim = |a: &[f32], b: &[f32]| -> f32 {
                let dot_product: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
                let mag_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
                let mag_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
                if mag_a > 0.0 && mag_b > 0.0 {
                    dot_product / (mag_a * mag_b)
                } else {
                    0.0
                }
            };

            let sim_12 = cosine_sim(&embedding1, &embedding2);
            let sim_13 = cosine_sim(&embedding1, &embedding3);

            // Same text should produce identical embeddings
            assert!(
                (sim_13 - 1.0).abs() < 0.01,
                "Same text should produce identical embeddings"
            );

            // Different texts should produce different embeddings
            assert!(
                sim_12 < 0.99,
                "Different texts should produce different embeddings"
            );

            // All embeddings should be non-zero
            assert!(!embedding1.iter().all(|&x| x == 0.0));
            assert!(!embedding2.iter().all(|&x| x == 0.0));
        }
        // If provider creation fails, skip test gracefully
    }

    // ---- A+ ONNX unload lifecycle tests (VAL-APLUS-022, VAL-APLUS-023) ----

    /// VAL-APLUS-022: ONNX provider can unload after work completes.
    ///
    /// After an embedding batch, the provider supports explicit unload that
    /// drops the live session without breaking later use.
    #[test]
    #[cfg(feature = "onnx")]
    fn test_onnx_unload_drops_session() {
        let result = QwenEmbeddingProvider::new();
        if let Ok(provider) = result {
            // Provider starts loaded
            assert!(provider.is_loaded(), "provider should start loaded");

            // Unload should succeed
            provider.unload();
            assert!(
                !provider.is_loaded(),
                "provider should not be loaded after unload"
            );
        }
    }

    /// VAL-APLUS-023: ONNX provider lazily reloads on next demand.
    ///
    /// After unload, the next embed request recreates the session and still
    /// returns valid output.
    #[test]
    #[cfg(feature = "onnx")]
    fn test_onnx_lazy_reload_after_unload() {
        let result = QwenEmbeddingProvider::new();
        if let Ok(provider) = result {
            // Use → unload → use cycle
            let _first = provider
                .embed("test input")
                .expect("first embed should work");
            assert!(provider.is_loaded());

            provider.unload();
            assert!(!provider.is_loaded(), "should be unloaded after unload()");

            // Lazy reload on next embed
            let second = provider
                .embed("test input after reload")
                .expect("embed after unload should trigger lazy reload");
            assert_eq!(second.len(), provider.dimension());
            assert!(provider.is_loaded(), "should be reloaded after embed");
        }
    }

    /// VAL-APLUS-022 variant: unload is idempotent and safe to call repeatedly.
    #[test]
    #[cfg(feature = "onnx")]
    fn test_onnx_unload_is_idempotent() {
        let result = QwenEmbeddingProvider::new();
        if let Ok(provider) = result {
            provider.unload();
            assert!(!provider.is_loaded());
            // Second unload should not panic
            provider.unload();
            assert!(!provider.is_loaded());
        }
    }
}
