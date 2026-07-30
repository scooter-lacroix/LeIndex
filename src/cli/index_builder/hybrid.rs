//! Hybrid embedding backend (unified TF-IDF + neural/remote).

use super::*;

// ============================================================================
// HYBRID EMBEDDING BACKEND (Unified TF-IDF + Neural/Remote)
// ============================================================================

/// Neural embedding dimension for the Qwen3-Embedding-0.6B ONNX model.
///
/// This dimension is used for local ONNX-backed neural embeddings.
/// The value must match the output dimension of the deployed model.
#[cfg(feature = "onnx")]
pub(crate) const NEURAL_EMBEDDING_DIMENSION: usize = 1024;

/// Hybrid embedding backend that always uses TF-IDF as the base signal and
/// combines configured neural/remote embeddings for semantic retrieval.
#[derive(Debug, Clone)]
pub enum HybridEmbedder {
    /// TF-IDF only (base signal always available)
    TfIdfOnly(TfIdfEmbedder),

    /// TF-IDF + Local ONNX neural embeddings (via worker process)
    #[cfg(feature = "onnx")]
    HybridLocal {
        /// TF-IDF embedder for keyword-based search
        tfidf: TfIdfEmbedder,
        /// Worker client for neural embedding via leindex-embed process
        neural: EmbeddingClient,
        /// Weight for neural embeddings in hybrid scoring (0.0-1.0)
        neural_weight: f32,
    },

    /// TF-IDF + Remote embeddings (OpenAI, Cohere, custom)
    #[cfg(feature = "remote-embeddings")]
    HybridRemote {
        /// TF-IDF embedder for keyword-based search
        tfidf: TfIdfEmbedder,
        /// Remote embedding provider for semantic search
        remote: GenericRemoteProvider,
        /// Weight for remote embeddings in hybrid scoring (0.0-1.0)
        remote_weight: f32,
    },
}

/// Scoring weights for hybrid embedding combination
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HybridScoringWeights {
    /// Weight for TF-IDF signal (0.0-1.0)
    pub tfidf: f32,
    /// Weight for neural/remote signal (0.0-1.0)
    pub neural: f32,
    /// Weight for structural signal (0.0-1.0)
    pub structural: f32,
    /// Weight for text match signal (0.0-1.0)
    pub text_match: f32,
}

impl Default for HybridScoringWeights {
    fn default() -> Self {
        Self {
            tfidf: 0.30,
            neural: 0.40,
            structural: 0.15,
            text_match: 0.15,
        }
    }
}

impl HybridScoringWeights {
    /// Create weights when neural embedding is unavailable
    pub fn without_neural() -> Self {
        Self {
            tfidf: 0.60,
            neural: 0.00,
            structural: 0.20,
            text_match: 0.20,
        }
    }

    /// Normalize weights to sum to 1.0
    pub fn normalize(&self) -> Self {
        let sum = self.tfidf + self.neural + self.structural + self.text_match;
        if sum == 0.0 {
            return Self::default();
        }
        Self {
            tfidf: self.tfidf / sum,
            neural: self.neural / sum,
            structural: self.structural / sum,
            text_match: self.text_match / sum,
        }
    }
}

impl HybridEmbedder {
    /// Create an explicit TF-IDF-only embedder for disabled/terminal-failure paths.
    pub fn tfidf_only(embedder: TfIdfEmbedder) -> Self {
        Self::TfIdfOnly(embedder)
    }

    /// Create a hybrid embedder with local ONNX neural embeddings via worker
    #[cfg(feature = "onnx")]
    pub fn hybrid_local(tfidf: TfIdfEmbedder, neural_weight: Option<f32>) -> Result<Self, String> {
        Ok(Self::HybridLocal {
            tfidf,
            neural: EmbeddingClient::new(),
            neural_weight: neural_weight.unwrap_or(0.40),
        })
    }

    /// Create a hybrid embedder with remote embeddings
    #[cfg(feature = "remote-embeddings")]
    pub fn hybrid_remote(
        tfidf: TfIdfEmbedder,
        remote_config: RemoteEmbeddingConfig,
        remote_weight: Option<f32>,
    ) -> Result<Self, RemoteEmbeddingError> {
        let remote = GenericRemoteProvider::from_config(remote_config)?;
        Ok(Self::HybridRemote {
            tfidf,
            remote,
            remote_weight: remote_weight.unwrap_or(0.40),
        })
    }

    /// Get the TF-IDF embedder (always available)
    pub fn tfidf(&self) -> &TfIdfEmbedder {
        match self {
            Self::TfIdfOnly(embedder) => embedder,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { tfidf, .. } => tfidf,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { tfidf, .. } => tfidf,
        }
    }

    /// Get the TF-IDF embedder mutably (always available)
    pub fn tfidf_mut(&mut self) -> &mut TfIdfEmbedder {
        match self {
            Self::TfIdfOnly(embedder) => embedder,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { tfidf, .. } => tfidf,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { tfidf, .. } => tfidf,
        }
    }

    /// Get the TF-IDF dimension (always 768)
    pub fn tfidf_dimension(&self) -> usize {
        self.tfidf().dimension()
    }

    /// Get the neural/remote dimension (if available)
    pub fn neural_dimension(&self) -> Option<usize> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { .. } => Some(NEURAL_EMBEDDING_DIMENSION),
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote, .. } => Some(remote.dimension()),
        }
    }

    /// Check if neural/remote enhancement is available
    pub fn has_neural(&self) -> bool {
        match self {
            Self::TfIdfOnly(_) => false,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { .. } => true,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => true,
        }
    }

    /// If a GPU provider was requested but the worker fell back to CPU, return
    /// a reason the caller can act on (skip neural enrichment → TF-IDF only).
    ///
    /// Honors the user's intent: a deliberate `cpu` (or `auto`) configuration
    /// always returns `None` so the CPU neural path stays fully operational;
    /// only an explicit `migraphx`/`cuda`/`rocm` request that actually degraded
    /// to CPU is flagged. See [`EmbeddingClient::cpu_fallback_reason`].
    #[cfg(feature = "onnx")]
    pub fn cpu_fallback_reason(&self) -> Option<String> {
        match self {
            Self::HybridLocal { neural, .. } => neural.cpu_fallback_reason(),
            _ => None,
        }
    }

    /// Whether neural inference is already ready for a query.
    ///
    pub fn neural_ready(&self) -> bool {
        match self {
            Self::TfIdfOnly(_) => false,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => neural.is_ready(),
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => true,
        }
    }

    /// Report the readiness state used by MCP retrieval metadata.
    pub fn neural_status(&self) -> &'static str {
        match self {
            Self::TfIdfOnly(_) => "absent",
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => match neural.availability() {
                crate::search::WorkerAvailability::Ready => "ready",
                crate::search::WorkerAvailability::Initializing(_) => "initializing",
                crate::search::WorkerAvailability::Failed(_) => "failed",
                crate::search::WorkerAvailability::Absent => "absent",
            },
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => "ready",
        }
    }

    /// Get the neural weight for scoring
    pub fn neural_weight(&self) -> f32 {
        match self {
            Self::TfIdfOnly(_) => 0.0,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural_weight, .. } => *neural_weight,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote_weight, .. } => *remote_weight,
        }
    }

    /// Get recommended scoring weights
    pub fn scoring_weights(&self) -> HybridScoringWeights {
        if self.has_neural() {
            HybridScoringWeights::default()
        } else {
            HybridScoringWeights::without_neural()
        }
    }

    /// Generate TF-IDF embedding for pre-tokenized content (always available)
    pub fn embed_tfidf(&self, tokens: &[String]) -> Vec<f32> {
        self.tfidf().embed_tokens(tokens)
    }

    /// Generate neural/remote embedding for text (if available)
    ///
    /// Uses `embed_with_fallback` for retry-once semantics:
    /// - VAL-CPHASE-017: Retries once on worker failure
    /// - VAL-CPHASE-018: Falls back to TF-IDF for the affected batch after second failure
    /// - VAL-CPHASE-019: Emits actionable warning on fallback
    /// - VAL-CPHASE-020: Worker failure does not crash the main daemon
    /// - VAL-CPHASE-021: Fresh worker can be spawned after fallback
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub async fn embed_neural_async(&self, text: &str) -> Option<Result<Vec<f32>, String>> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                // Clone shares the worker handle via Arc (EmbeddingClient::clone is cheap).
                // Required because spawn_blocking requires ownership.
                let neural = neural.clone();
                let texts = vec![text.to_string()];
                let result = task::spawn_blocking(move || {
                    neural.embed_with_fallback(&texts, NEURAL_EMBEDDING_DIMENSION)
                })
                .await
                .ok()?;
                match result {
                    EmbedResult::Success(response) => {
                        match response.into_vectors().into_iter().next() {
                            // VAL-CPHASE-016: write from flat buffer directly. Presence of the
                            // vector (not response.count) is the real non-empty contract.
                            Some(v) => Some(Ok(v)),
                            None => Some(Err("worker returned empty response".to_string())),
                        }
                    }
                    EmbedResult::Fallback { batch_id, error } => {
                        // VAL-CPHASE-018/019: Fallback already logged actionable warning.
                        tracing::warn!(
                            batch_id = %batch_id,
                            error = %error,
                            "Neural embedding degraded to TF-IDF for node (async path)"
                        );
                        None
                    }
                }
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote, .. } => Some(
                remote
                    .embed(text)
                    .await
                    .map_err(|e| format!("Remote embedding failed: {}", e)),
            ),
        }
    }

    /// Generate neural/remote embedding for text (blocking wrapper for sync contexts)
    ///
    /// Uses `embed_with_fallback` for retry-once semantics:
    /// - VAL-CPHASE-017: Retries once on worker failure
    /// - VAL-CPHASE-018: Falls back to TF-IDF for the affected batch after second failure
    /// - VAL-CPHASE-019: Emits actionable warning on fallback
    /// - VAL-CPHASE-020: Worker failure does not crash the main daemon
    /// - VAL-CPHASE-021: Fresh worker can be spawned after fallback
    ///
    /// Blocking cross-encoder rerank of candidate documents via the worker's
    /// on-demand reranker (bge-reranker-base). Takes (id, content, initial_score)
    /// tuples and returns (id, combined_score) ranked by the cross-encoder.
    /// Returns None when no neural worker is available (TfIdfOnly, or a
    /// no-onnx build) — caller keeps the original ordering. (VAL-RERANK.)
    pub fn rerank_blocking(
        &self,
        query: &str,
        docs: Vec<(String, String, f32)>,
    ) -> Option<Result<Vec<(String, f32)>, String>> {
        #[cfg(feature = "onnx")]
        #[allow(clippy::needless_return)]
        // return is required so the cfg(not(onnx)) fallthrough block can follow
        {
            use leindex_embed::protocol::RerankDocument;
            return match self {
                Self::TfIdfOnly(_) => None,
                Self::HybridLocal { neural, .. } => {
                    let documents: Vec<RerankDocument> = docs
                        .into_iter()
                        .map(|(id, content, initial_score)| RerankDocument {
                            id,
                            content,
                            initial_score,
                        })
                        .collect();
                    match neural.rerank(query, documents) {
                        Ok(resp) => Some(Ok(resp
                            .results
                            .into_iter()
                            .map(|r| (r.id, r.combined_score))
                            .collect())),
                        Err(e) => Some(Err(e.to_string())),
                    }
                }
                // Remote-only embedders provide embeddings, not a local reranker;
                // reranking is a local-ONNX capability. (Required for match
                // exhaustiveness under the onnx+remote-embeddings feature combo.)
                #[cfg(feature = "remote-embeddings")]
                Self::HybridRemote { .. } => None,
            };
        }
        #[cfg(not(feature = "onnx"))]
        {
            let _ = (query, docs);
            None
        }
    }

    /// Embed a single text with the neural embedder (blocking). Returns `None`
    /// for the TF-IDF-only variant; for the hybrid-local variant returns the
    /// embedding result, with fallback applied by `embed_with_fallback`.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub fn embed_neural_blocking(&self, text: &str) -> Option<Result<Vec<f32>, String>> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                let texts = vec![text.to_string()];
                let result = neural.embed_with_fallback(&texts, NEURAL_EMBEDDING_DIMENSION);
                match result {
                    EmbedResult::Success(response) => {
                        match response.into_vectors().into_iter().next() {
                            // VAL-CPHASE-016: write from flat buffer directly. Presence of the
                            // vector (not response.count) is the real non-empty contract.
                            Some(v) => Some(Ok(v)),
                            None => Some(Err("worker returned empty response".to_string())),
                        }
                    }
                    EmbedResult::Fallback { batch_id, error } => {
                        // VAL-CPHASE-018/019: Fallback already logged actionable warning.
                        // Return None so the caller falls back to TF-IDF for this batch.
                        tracing::warn!(
                            batch_id = %batch_id,
                            error = %error,
                            "Neural embedding degraded to TF-IDF for node"
                        );
                        None
                    }
                }
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => {
                // Remote requires async runtime, this is a blocking wrapper
                // In practice, the indexing pipeline should use the async version
                Some(Err("Remote embeddings require async runtime".to_string()))
            }
        }
    }

    /// Generate neural/remote embeddings for a batch of texts (blocking wrapper).
    ///
    /// Returns `Vec<Option<Vec<f32>>>` — one entry per input text.
    /// `Some(vec)` on success, `None` only when the provider is unavailable or
    /// the affected request enters the explicit fallback path.
    ///
    /// This batches all texts into a single IPC call to the ONNX worker,
    /// reducing N round-trips to 1 per chunk.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub fn embed_neural_batch_blocking(&self, texts: &[String]) -> Vec<Option<Vec<f32>>> {
        match self {
            Self::TfIdfOnly(_) => vec![None; texts.len()],
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                if texts.is_empty() {
                    return Vec::new();
                }
                let result = neural.embed_with_fallback(texts, NEURAL_EMBEDDING_DIMENSION);
                match result {
                    EmbedResult::Success(response) => {
                        if response.count == texts.len() {
                            response.into_vectors().into_iter().map(Some).collect()
                        } else {
                            tracing::warn!(
                                expected = texts.len(),
                                got = response.count,
                                "Neural batch returned wrong count, falling back to None for all"
                            );
                            vec![None; texts.len()]
                        }
                    }
                    EmbedResult::Fallback { batch_id, error } => {
                        tracing::warn!(
                            batch_id = %batch_id,
                            error = %error,
                            "Neural batch embedding degraded to TF-IDF for {} texts",
                            texts.len()
                        );
                        vec![None; texts.len()]
                    }
                }
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => {
                // Remote requires async runtime; not supported in blocking context
                vec![None; texts.len()]
            }
        }
    }

    /// Persist the TF-IDF embedder to storage
    ///
    /// Delegates to the inner TfIdfEmbedder's persist_to_storage method
    pub fn persist_to_storage(
        &self,
        project_path: &Path,
        pdg: &ProgramDependenceGraph,
    ) -> Result<()> {
        self.tfidf().persist_to_storage(project_path, pdg)
    }

    /// Unload the ONNX session if the hybrid backend uses one (A+ idle-unload).
    ///
    /// After an indexing batch completes, calling this drops the live ONNX
    /// session so it does not remain resident indefinitely (VAL-APLUS-024).
    /// With the worker architecture, this signals the worker to shut down.
    pub fn unload_onnx(&self) {
        match self {
            Self::TfIdfOnly(_) => {}
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                // Kill the worker process; the client can spawn a fresh one later.
                neural.kill_worker();
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => {}
        }
    }

    /// Check whether the ONNX session is currently loaded.
    #[must_use]
    pub fn is_onnx_loaded(&self) -> bool {
        match self {
            Self::TfIdfOnly(_) => false,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { .. } => {
                // With the worker architecture, "loaded" means the worker process
                // is running. This will be properly tracked in the runtime lifecycle
                // feature. For now, return false as the worker is spawned on demand.
                false
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => false,
        }
    }
}

impl Default for HybridEmbedder {
    fn default() -> Self {
        Self::TfIdfOnly(TfIdfEmbedder::build_from_tokens(&[]))
    }
}
