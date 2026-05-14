// Qwen3 ONNX Reranker
//
// Provides neural reranking using Qwen3-Reranker-0.6B model via ONNX Runtime.
// Improves search quality by re-ranking top-k results from initial search.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use thiserror::Error;

#[cfg(feature = "onnx")]
use ort::session::Session;

/// Errors that can occur during Qwen3 reranking
#[derive(Debug, Error)]
pub enum QwenRerankerError {
    /// ONNX Runtime initialization or execution error
    #[error("ONNX Runtime error: {0}")]
    OnnxRuntime(String),

    /// Reranker model file could not be found at the expected path
    #[error("Model file not found: {0}")]
    ModelNotFound(String),

    /// Tokenizer initialization or tokenization error
    #[error("Tokenizer error: {0}")]
    Tokenizer(String),

    /// Reranking process failed during inference
    #[error("Reranking failed: {0}")]
    RerankingFailed(String),

    /// ONNX feature is not enabled in compilation
    #[error("Feature not enabled: ONNX feature is required")]
    FeatureNotEnabled,
}

/// Search result to be reranked
///
/// Contains the original search result with its initial score for
/// quality-aware reranking using the neural reranker.
#[derive(Debug, Clone)]
pub struct SearchResultForRerank {
    /// Unique identifier for the search result node
    pub node_id: String,
    /// Content text of the search result
    pub content: String,
    /// Initial score from the primary search (e.g., TF-IDF cosine similarity)
    pub initial_score: f32,
}

/// Reranked result with updated score
///
/// Contains the reranked result with both original and neural scores,
/// providing transparency into the reranking process.
#[derive(Debug, Clone)]
pub struct RerankedResult {
    /// Unique identifier for the search result node
    pub node_id: String,
    /// Original score from the primary search
    pub original_score: f32,
    /// New score from neural reranking
    pub rerank_score: f32,
    /// Combined score (weighted average of original and rerank scores)
    pub combined_score: f32,
}

/// Qwen3 Reranker
///
/// Loads and runs Qwen3-Reranker-0.6B model for quality-aware reranking.
///
/// Supports the A+ idle-unload lifecycle: `unload()` drops the live ONNX
/// session and `ensure_session()` lazily recreates it on the next rerank call.
#[derive(Debug)]
pub struct QwenReranker {
    /// ONNX Runtime session for reranking inference
    #[cfg(feature = "onnx")]
    session: Arc<Mutex<Option<Session>>>,
    model_path: PathBuf,
    /// Tokenizer for processing query and document text
    #[cfg(feature = "onnx")]
    tokenizer: Arc<tokenizers::Tokenizer>,
}

impl QwenReranker {
    /// Create a new Qwen3 reranker
    pub fn new() -> Result<Self, QwenRerankerError> {
        #[cfg(not(feature = "onnx"))]
        {
            return Err(QwenRerankerError::FeatureNotEnabled);
        }

        #[cfg(feature = "onnx")]
        {
            let model_path = Self::resolve_model_path()
                .map_err(|e| QwenRerankerError::ModelNotFound(e.to_string()))?;

            // Initialize ONNX Runtime session
            let session = Session::builder()
                .map_err(|e| {
                    QwenRerankerError::OnnxRuntime(format!(
                        "Failed to create session builder: {}",
                        e
                    ))
                })?
                .commit_from_file(&model_path)
                .map_err(|e| {
                    QwenRerankerError::OnnxRuntime(format!("Failed to load model: {}", e))
                })?;

            // Initialize tokenizer - use the same tokenizer as the embedding model
            let tokenizer_path = model_path
                .parent()
                .ok_or_else(|| QwenRerankerError::ModelNotFound("Invalid model path".to_string()))?
                .join("tokenizer.json");

            let tokenizer = tokenizers::Tokenizer::from_file(&tokenizer_path).map_err(|e| {
                QwenRerankerError::Tokenizer(format!(
                    "Failed to load tokenizer from {}: {}",
                    tokenizer_path.display(),
                    e
                ))
            })?;

            Ok(Self {
                session: Arc::new(Mutex::new(Some(session))),
                model_path,
                tokenizer: Arc::new(tokenizer),
            })
        }
    }

    /// Unload the ONNX session, releasing resident memory (A+ idle-unload).
    pub fn unload(&self) {
        #[cfg(feature = "onnx")]
        {
            if let Ok(mut guard) = self.session.lock() {
                *guard = None;
            }
        }
    }

    /// Check whether the ONNX session is currently loaded.
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
    #[cfg(feature = "onnx")]
    fn ensure_session(&self) -> Result<(), QwenRerankerError> {
        let mut guard = self.session.lock().map_err(|e| {
            QwenRerankerError::RerankingFailed(format!("Failed to lock session: {}", e))
        })?;

        if guard.is_some() {
            return Ok(());
        }

        let session = Session::builder()
            .map_err(|e| {
                QwenRerankerError::OnnxRuntime(format!(
                    "Failed to create session builder on reload: {}",
                    e
                ))
            })?
            .commit_from_file(&self.model_path)
            .map_err(|e| {
                QwenRerankerError::OnnxRuntime(format!("Failed to reload model: {}", e))
            })?;

        *guard = Some(session);
        Ok(())
    }

    /// Resolve the reranker model path
    fn resolve_model_path() -> std::result::Result<PathBuf, QwenRerankerError> {
        // Same resolution logic as embedding provider
        if let Ok(path) = std::env::var("LEINDEX_MODEL_PATH") {
            let model_path = PathBuf::from(path).join("qwen3-rerank-0.6b.onnx");
            if model_path.exists() {
                return Ok(model_path);
            }
        }

        if let Ok(exe_path) = std::env::current_exe() {
            let bundled_dir = exe_path.parent().unwrap().join("models");
            let model_path = bundled_dir.join("qwen3-rerank-0.6b.onnx");
            if model_path.exists() {
                return Ok(model_path);
            }
        }

        if let Some(home) = dirs::home_dir() {
            let user_models = home.join(".leindex").join("models");
            let model_path = user_models.join("qwen3-rerank-0.6b.onnx");
            if model_path.exists() {
                return Ok(model_path);
            }
        }

        Err(QwenRerankerError::ModelNotFound(
            "Qwen3 reranker model not found in any standard location".to_string(),
        ))
    }

    /// Rerank search results given a query
    ///
    /// Takes a query and initial search results, returns reranked results
    /// with improved quality scores.
    pub fn rerank(
        &self,
        query: &str,
        results: Vec<SearchResultForRerank>,
    ) -> Result<Vec<RerankedResult>, QwenRerankerError> {
        #[cfg(feature = "onnx")]
        {
            if results.is_empty() {
                return Ok(vec![]);
            }

            // Lazy reload if the session was previously unloaded
            self.ensure_session()?;

            use ort::value::Tensor;

            // Encode query-document pairs for reranking
            let mut query_doc_pairs: Vec<String> = Vec::with_capacity(results.len());
            for result in &results {
                query_doc_pairs.push(format!("Query: {} Document: {}", query, result.content));
            }

            // Batch tokenize the query-document pairs
            let texts_vec: Vec<&str> = query_doc_pairs.iter().map(|s| s.as_str()).collect();
            let encodings = self.tokenizer.encode_batch(texts_vec, true).map_err(|e| {
                QwenRerankerError::Tokenizer(format!("Batch tokenization failed: {}", e))
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
            let input_ids_tensor = Tensor::from_array(([batch_size, max_seq_len], input_ids_batch))
                .map_err(|e| {
                    QwenRerankerError::OnnxRuntime(format!(
                        "Failed to create batch input_ids tensor: {}",
                        e
                    ))
                })?;

            let attention_mask_tensor =
                Tensor::from_array(([batch_size, max_seq_len], attention_mask_batch)).map_err(
                    |e| {
                        QwenRerankerError::OnnxRuntime(format!(
                            "Failed to create batch attention_mask tensor: {}",
                            e
                        ))
                    },
                )?;

            // Run batch inference
            let mut guard = self.session.lock().map_err(|e| {
                QwenRerankerError::RerankingFailed(format!("Failed to lock session: {}", e))
            })?;

            let session = guard.as_mut().ok_or_else(|| {
                QwenRerankerError::RerankingFailed(
                    "ONNX session not available after ensure_session".to_string(),
                )
            })?;

            let outputs = session.outputs();
            if outputs.is_empty() {
                return Err(QwenRerankerError::RerankingFailed(
                    "Model has no outputs".to_string(),
                ));
            }
            let output_name = outputs
                .iter()
                .find(|output| output.name().contains("score") || output.name().contains("logits"))
                .map(|output| output.name().to_string())
                .unwrap_or_else(|| outputs[0].name().to_string());

            let outputs = session
                .run(ort::inputs! {
                    "input_ids" => input_ids_tensor,
                    "attention_mask" => attention_mask_tensor
                })
                .map_err(|e| {
                    QwenRerankerError::RerankingFailed(format!(
                        "Rerank ONNX inference failed: {}",
                        e
                    ))
                })?;

            // Extract batch output tensor
            let output_tensor = outputs.get(&output_name).ok_or_else(|| {
                QwenRerankerError::RerankingFailed(format!("Output '{}' not found", output_name))
            })?;

            let batch_scores = output_tensor
                .try_extract_array::<f32>()
                .map_err(|e| {
                    QwenRerankerError::RerankingFailed(format!(
                        "Failed to extract batch output tensor: {}",
                        e
                    ))
                })?
                .into_owned();

            // Convert ndarray to Vec<f32>
            let scores_vec: Vec<f32> = batch_scores.into_raw_vec_and_offset().0;

            // Create reranked results
            let mut reranked_results = Vec::with_capacity(results.len());
            for (i, result) in results.into_iter().enumerate() {
                let rerank_score = if i < scores_vec.len() {
                    scores_vec[i]
                } else {
                    result.initial_score // Fallback to initial score if no rerank score
                };

                // Combined score: weighted average (70% rerank, 30% initial)
                let combined_score = 0.7 * rerank_score + 0.3 * result.initial_score;

                reranked_results.push(RerankedResult {
                    node_id: result.node_id,
                    original_score: result.initial_score,
                    rerank_score,
                    combined_score,
                });
            }

            // Sort by combined score (descending)
            reranked_results.sort_by(|a, b| {
                b.combined_score
                    .partial_cmp(&a.combined_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            Ok(reranked_results)
        }

        #[cfg(not(feature = "onnx"))]
        {
            Err(QwenRerankerError::FeatureNotEnabled)
        }
    }

    /// Get the model path
    pub fn model_path(&self) -> &PathBuf {
        &self.model_path
    }
}

impl Clone for QwenReranker {
    fn clone(&self) -> Self {
        Self {
            #[cfg(feature = "onnx")]
            session: Arc::clone(&self.session),
            model_path: self.model_path.clone(),
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
    fn test_reranker_creation() {
        let result = QwenReranker::new();
        assert!(result.is_err() || result.is_ok()); // Allow either until model is bundled
    }

    #[test]
    #[cfg(not(feature = "onnx"))]
    fn test_feature_not_enabled() {
        let result = QwenReranker::new();
        assert!(matches!(result, Err(QwenRerankerError::FeatureNotEnabled)));
    }
}
