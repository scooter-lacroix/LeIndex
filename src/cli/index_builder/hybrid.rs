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

/// Document-side provider selection for one neural generation. Query calls keep
/// their existing behavior. A new instance retries remote on the next generation.
pub(crate) struct GenerationNeuralEmbedder {
    selected: std::cell::RefCell<HybridEmbedder>,
    #[cfg(feature = "remote-embeddings")]
    remote_rows_landed: std::cell::Cell<bool>,
    #[cfg(feature = "remote-embeddings")]
    stopped: std::cell::Cell<bool>,
}

impl GenerationNeuralEmbedder {
    pub(crate) fn new(embedder: HybridEmbedder) -> Self {
        Self {
            selected: std::cell::RefCell::new(embedder),
            #[cfg(feature = "remote-embeddings")]
            remote_rows_landed: std::cell::Cell::new(false),
            #[cfg(feature = "remote-embeddings")]
            stopped: std::cell::Cell::new(false),
        }
    }

    /// Incremental layers may already contain reused rows: their selected
    /// space is pinned before the first request, so local fallback is unsafe.
    pub(crate) fn pinned(embedder: HybridEmbedder) -> Self {
        let generation = Self::new(embedder);
        #[cfg(feature = "remote-embeddings")]
        generation.remote_rows_landed.set(true);
        generation
    }

    pub(crate) fn has_neural(&self) -> bool {
        self.selected.borrow().has_neural()
    }

    pub(crate) fn identity(&self) -> String {
        match &*self.selected.borrow() {
            #[cfg(feature = "remote-embeddings")]
            HybridEmbedder::HybridRemote { remote, .. } => remote.identity(),
            #[cfg(feature = "onnx")]
            HybridEmbedder::HybridLocal { .. } => format!(
                "local|{}|{}",
                crate::config::LeIndexConfig::load_cached()
                    .neural
                    .model_name,
                NEURAL_EMBEDDING_DIMENSION,
            ),
            HybridEmbedder::TfIdfOnly(_) => "unavailable".to_string(),
        }
    }

    pub(crate) fn document_embedding_stopped(&self) -> bool {
        #[cfg(feature = "remote-embeddings")]
        {
            self.stopped.get()
        }
        #[cfg(not(feature = "remote-embeddings"))]
        {
            false
        }
    }

    pub(crate) fn into_embedder(self) -> HybridEmbedder {
        self.selected.into_inner()
    }

    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub(crate) fn embed_neural_batch_blocking<S: AsRef<str>>(
        &self,
        texts: &[S],
    ) -> Vec<Option<Vec<f32>>> {
        if texts.is_empty() {
            return Vec::new();
        }
        #[cfg(feature = "remote-embeddings")]
        if self.stopped.get() {
            return vec![None; texts.len()];
        }
        #[cfg(feature = "remote-embeddings")]
        if matches!(*self.selected.borrow(), HybridEmbedder::HybridRemote { .. }) {
            let remote = match &*self.selected.borrow() {
                HybridEmbedder::HybridRemote { remote, .. } => remote.clone(),
                _ => unreachable!(),
            };
            let owned: Vec<String> = texts.iter().map(|text| text.as_ref().to_string()).collect();
            match block_on_remote(async move {
                remote
                    .embed_batch_optional(owned.iter().map(String::as_str).collect())
                    .await
            }) {
                Ok(rows) if rows.len() == texts.len() => {
                    if rows.iter().flatten().any(|row| !row.is_empty()) {
                        // Any successful remote row pins this generation's space.
                        self.remote_rows_landed.set(true);
                    }
                    return rows;
                }
                Ok(rows) => tracing::warn!(
                    expected = texts.len(),
                    got = rows.len(),
                    "Remote optional batch returned wrong row count"
                ),
                Err(error) => tracing::warn!(
                    %error,
                    "Remote batch failed; applying generation fallback policy"
                ),
            }
            if self.remote_rows_landed.get() {
                self.stopped.set(true);
                tracing::warn!(
                    "Remote failed after accepted rows; neural generation frozen to TF-IDF for remaining documents"
                );
                return vec![None; texts.len()];
            }
            self.select_local_fallback();
            return self.selected.borrow().embed_neural_batch_blocking(texts);
        }
        let embeddings = self.selected.borrow().embed_neural_batch_blocking(texts);
        embeddings
    }

    #[cfg(feature = "remote-embeddings")]
    fn select_local_fallback(&self) {
        // No remote rows exist in this generation: a coherent all-Qwen
        // generation is safe. Construction/worker failures retain TF-IDF.
        let tfidf = self.selected.borrow().tfidf().clone();
        #[cfg(feature = "onnx")]
        let fallback = HybridEmbedder::hybrid_local(
            tfidf.clone(),
            Some(crate::config::LeIndexConfig::load_cached().neural_weight_f32()),
        )
        .unwrap_or_else(|error| {
            tracing::warn!(%error, "Local neural fallback unavailable");
            HybridEmbedder::tfidf_only(tfidf)
        });
        #[cfg(not(feature = "onnx"))]
        let fallback = HybridEmbedder::tfidf_only(tfidf);
        tracing::warn!(
            "Remote failed before any rows; selecting local/TF-IDF backend for the whole neural generation"
        );
        *self.selected.borrow_mut() = fallback;
    }
}

#[cfg(all(test, feature = "remote-embeddings"))]
mod generation_tests {
    use super::*;

    #[test]
    fn test_generation_pinned_remote_failure_never_selects_local() {
        let tfidf = TfIdfEmbedder::build(&[("a.rs".into(), "fn a() {}".into())]);
        let generation = GenerationNeuralEmbedder::pinned(
            HybridEmbedder::hybrid_remote(
                tfidf,
                RemoteEmbeddingConfig {
                    provider: crate::search::onnx::RemoteProvider::Custom {
                        endpoint: "http://127.0.0.1:1/v1".into(),
                    },
                    ..RemoteEmbeddingConfig::default()
                },
                None,
            )
            .expect("remote construction"),
        );
        assert!(
            generation
                .embed_neural_batch_blocking(&["document"])
                .iter()
                .all(Option::is_none)
        );
        assert!(generation.stopped.get());
        assert!(matches!(
            generation.into_embedder(),
            HybridEmbedder::HybridRemote { .. }
        ));
    }

    #[test]
    fn test_generation_remote_pre_success_selects_local_without_remote_rows() {
        let tfidf = TfIdfEmbedder::build(&[("a.rs".into(), "fn a() {}".into())]);
        let generation = GenerationNeuralEmbedder::new(
            HybridEmbedder::hybrid_remote(
                tfidf,
                RemoteEmbeddingConfig {
                    provider: crate::search::onnx::RemoteProvider::Custom {
                        endpoint: "http://127.0.0.1:1/v1".into(),
                    },
                    ..RemoteEmbeddingConfig::default()
                },
                None,
            )
            .expect("remote construction"),
        );
        let response = generation
            .selected
            .borrow()
            .embed_neural_batch_blocking(&["document"]);
        assert!(response.iter().all(Option::is_none));
        assert!(!generation.remote_rows_landed.get());
        // Check the real transition separately from worker IO: local readiness
        // is a runtime gate, not simulated 1024-dimensional evidence.
        generation.select_local_fallback();
        let selected = generation.into_embedder();
        #[cfg(feature = "onnx")]
        {
            assert!(matches!(selected, HybridEmbedder::HybridLocal { .. }));
            assert_eq!(selected.neural_dimension(), Some(1024));
        }
        #[cfg(not(feature = "onnx"))]
        assert!(matches!(selected, HybridEmbedder::TfIdfOnly(_)));
    }
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

    /// Get the configured TF-IDF embedding dimension.
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

    /// Dimension observed from a successful remote response, if available.
    pub fn observed_neural_dimension(&self) -> Option<usize> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { .. } => None,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote, .. } => remote.observed_dimension(),
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

    /// Identity of the neural embedder for the Engram query phrase-book, or
    /// `None` when there is no neural embedder or its identity cannot be
    /// established (the phrase-book is then bypassed, never guessed).
    pub fn engram_identity(&self) -> Option<String> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => neural.engram_identity(NEURAL_EMBEDDING_DIMENSION),
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote, .. } => Some(remote.identity()),
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
                        let vectors = response.into_vectors();
                        if vectors.len() != 1 {
                            return Some(Err(format!(
                                "worker returned {} vectors for one input",
                                vectors.len()
                            )));
                        }
                        // VAL-CPHASE-016: write from flat buffer directly. The decoded
                        // vector count, not response.count, is the storage contract.
                        Some(Ok(vectors
                            .into_iter()
                            .next()
                            .expect("validated vector count")))
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
            use crate::embed::protocol::RerankDocument;
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
                        let vectors = response.into_vectors();
                        if vectors.len() != 1 {
                            return Some(Err(format!(
                                "worker returned {} vectors for one input",
                                vectors.len()
                            )));
                        }
                        // VAL-CPHASE-016: write from flat buffer directly. The decoded
                        // vector count, not response.count, is the storage contract.
                        Some(Ok(vectors
                            .into_iter()
                            .next()
                            .expect("validated vector count")))
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
            Self::HybridRemote { remote, .. } => {
                // Remote requires an async runtime; the CLI query path is
                // synchronous and never runs inside a tokio context, so a
                // scratch single-thread reactor bridges the call. Owned text
                // + owned provider move into the future (async-trait futures
                // borrow their arguments, so the borrows must live inside).
                // Provider errors surface to the caller (query falls back to
                // TF-IDF); they never hard-fail the search.
                let remote = remote.clone();
                let owned_text = text.to_string();
                match block_on_remote(async move { remote.embed(&owned_text).await }) {
                    Ok(embedding) => Some(Ok(embedding)),
                    Err(e) => Some(Err(e)),
                }
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
    pub fn embed_neural_batch_blocking<S: AsRef<str>>(&self, texts: &[S]) -> Vec<Option<Vec<f32>>> {
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
                        let vectors = response.into_vectors();
                        if vectors.len() == texts.len() {
                            vectors.into_iter().map(Some).collect()
                        } else {
                            tracing::warn!(
                                expected = texts.len(),
                                got = vectors.len(),
                                "Neural batch returned wrong vector count, falling back to None for all"
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
            Self::HybridRemote { remote, .. } => {
                if texts.is_empty() {
                    return Vec::new();
                }
                // Same scratch-runtime bridge as embed_neural_blocking; batch
                // API failures degrade to None per text (TF-IDF fallback) so a
                // flapping endpoint never poisons the index (VAL-CPHASE-018).
                let remote = remote.clone();
                let owned: Vec<String> = texts.iter().map(|t| t.as_ref().to_string()).collect();
                match block_on_remote(async move {
                    remote
                        .embed_batch(owned.iter().map(|s| s.as_str()).collect())
                        .await
                }) {
                    Ok(embeddings) if embeddings.len() == texts.len() => {
                        embeddings.into_iter().map(Some).collect()
                    }
                    Ok(embeddings) => {
                        tracing::warn!(
                            expected = texts.len(),
                            got = embeddings.len(),
                            "Remote batch returned wrong vector count, falling back to None for all"
                        );
                        vec![None; texts.len()]
                    }
                    Err(e) => {
                        tracing::warn!(
                            error = %e,
                            "Remote batch embedding degraded to TF-IDF for {} texts",
                            texts.len()
                        );
                        vec![None; texts.len()]
                    }
                }
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
        persisted_identity: Option<(usize, usize, String)>,
    ) -> Result<()> {
        self.tfidf()
            .persist_to_storage(project_path, pdg, persisted_identity)
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
            Self::HybridLocal { neural, .. } => neural.is_ready(),
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

/// Bridge a remote-embeddings future onto the synchronous pipeline.
///
/// The CLI indexing/query paths never run inside a tokio runtime, so a
/// scratch single-thread reactor is safe here (`block_on` panics only when
/// called from within an async context, which cannot happen on these paths).
/// One runtime per call keeps no global state and no background threads.
/// The future must be `'static` + `Send` (ours hold only the `Arc`-based
/// `GenericRemoteProvider`) and resolve to a `Result` whose error is
/// `Display`; every failure mode collapses to `String`.
#[cfg(feature = "remote-embeddings")]
fn block_on_remote<T: Send + 'static, E>(
    fut: impl std::future::Future<Output = Result<T, E>> + Send + 'static,
) -> Result<T, String>
where
    E: std::fmt::Display,
{
    std::thread::Builder::new()
        .name("remote-embed-bridge".to_string())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())
                .and_then(|rt| rt.block_on(fut).map_err(|e| e.to_string()))
        })
        .map_err(|e| format!("failed to spawn remote-embed bridge thread: {e}"))?
        .join()
        .map_err(|_| "remote-embed bridge thread panicked".to_string())?
}
