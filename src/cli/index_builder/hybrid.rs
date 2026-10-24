//! Hybrid embedding backend (unified TF-IDF + neural/remote).

#[cfg(feature = "remote-embeddings")]
use crate::embed::cache::{Normalization, Pooling};
#[cfg(feature = "remote-embeddings")]
use crate::search::onnx::embed_cache_frontend::{CacheProbeOutcome, KeySpec};
#[cfg(feature = "remote-embeddings")]
use crate::search::onnx::remote::{RemoteEmbeddingProvider, is_retryable_embedding_error};
#[cfg(feature = "remote-embeddings")]
use crate::search::onnx::{GenericRemoteProvider, RemoteEmbeddingConfig, RemoteEmbeddingError};

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
    /// Count of CONSECUTIVE batches that failed transiently. Reset to zero by any
    /// batch that lands rows.
    ///
    /// Without a generation-level bound, a dead endpoint costs the full retry
    /// window on *every* remaining batch: `timeout_secs * (max_retries + 1)` —
    /// about 120s with the defaults — multiplied by however many batches are
    /// left. The per-batch retry allowance (`search::onnx::remote`) bounds each
    /// batch; this bounds the generation, so a provider that is simply gone is
    /// declared dead once instead of being retried for every batch in turn.
    #[cfg(feature = "remote-embeddings")]
    transient_failures: std::cell::Cell<usize>,
}

/// How many consecutive transient batch failures a generation tolerates before
/// it stops calling remote entirely and leaves the remaining rows without neural
/// vectors.
///
/// 3 is chosen against the retry cost: with `max_retries = 3` and
/// `timeout_secs = 30` one batch can already consume ~120s, so three in a row is
/// ~6 minutes of a phase that is provably making no progress. A provider that
/// recovers within a batch or two — the case worth surviving, since freezing used
/// to discard tens of thousands of documents over one blip — is unaffected,
/// because any batch that lands a row resets the counter.
#[cfg(feature = "remote-embeddings")]
const MAX_CONSECUTIVE_TRANSIENT_FAILURES: usize = 3;

impl GenerationNeuralEmbedder {
    pub(crate) fn new(embedder: HybridEmbedder) -> Self {
        Self {
            selected: std::cell::RefCell::new(embedder),
            #[cfg(feature = "remote-embeddings")]
            remote_rows_landed: std::cell::Cell::new(false),
            #[cfg(feature = "remote-embeddings")]
            stopped: std::cell::Cell::new(false),
            #[cfg(feature = "remote-embeddings")]
            transient_failures: std::cell::Cell::new(0),
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

    /// A custom endpoint without a pinned dimension only learns it from its
    /// first response, so a fresh generation's pre-request identity carries
    /// the bootstrap default while the checkpoint it saved carries the
    /// measured one: they never match and every run re-embeds. Adopt the
    /// checkpoint's dimension — only when every other identity component is
    /// identical — so resume compares like with like.
    pub(crate) fn adopt_checkpoint_dimension(&self, _checkpoint_model: &str) {
        #[cfg(feature = "remote-embeddings")]
        if let HybridEmbedder::HybridRemote { remote, .. } = &*self.selected.borrow() {
            let Some(dimension) = _checkpoint_model
                .rsplit('|')
                .next()
                .and_then(|suffix| suffix.parse::<usize>().ok())
            else {
                return;
            };
            if remote.measured_dimension().is_none()
                && remote.identity_with_dimension(dimension) == _checkpoint_model
            {
                remote.seed_measured_dimension(dimension);
            }
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

    /// Cache identity for the remote provider, or `None` until its output
    /// dimension is measured. A cold provider reports the bootstrap dimension
    /// while the server may serve another; keying on that writes rows into a
    /// namespace no later run ever hits.
    #[cfg(feature = "remote-embeddings")]
    fn cache_brief(&self, remote: &GenericRemoteProvider) -> Option<CacheBrief> {
        let dimension = remote.measured_dimension()?;
        Some(CacheBrief {
            spec: KeySpec::for_provider(
                &remote.identity_with_dimension(dimension),
                Pooling::Mean,
                Normalization::L2,
                dimension,
            ),
        })
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
            // Remote runs were the one embedding path with no cache: every
            // reindex re-sent the whole corpus even though the store already
            // held those vectors. Probe first and dispatch only misses.
            let brief = self.cache_brief(&remote);
            let probe = brief.and_then(|brief| {
                crate::search::onnx::embed_cache_frontend::probe_provider(&brief.spec, &owned)
            });
            if let Some(outcome) = probe.as_ref() {
                if outcome.miss_positions.is_empty() {
                    tracing::debug!(
                        hits = outcome.hits.len(),
                        "Remote embed cache: every row hit; endpoint not contacted"
                    );
                    self.remote_rows_landed.set(true);
                    return merge_cached_rows(texts.len(), Some(outcome), Vec::new());
                }
            }
            let miss_texts: Vec<String> = match probe.as_ref() {
                Some(outcome) => outcome
                    .miss_positions
                    .iter()
                    .map(|&position| texts[position].as_ref().to_string())
                    .collect(),
                None => owned,
            };
            let expected_rows = miss_texts.len();
            let mut transient = false;
            match block_on_remote(async move {
                let refs: Vec<&str> = miss_texts.iter().map(String::as_str).collect();
                remote
                    .embed_batch_optional(refs)
                    .await
                    .map_err(bridge_error)
            }) {
                Ok(miss_rows) if miss_rows.len() == expected_rows => {
                    if miss_rows.iter().flatten().any(|row| !row.is_empty()) {
                        // Any successful remote row pins this generation's space.
                        self.remote_rows_landed.set(true);
                        // Progress was made, so the dead-endpoint streak is over.
                        self.transient_failures.set(0);
                    }
                    if let (Some(brief), Some(outcome)) = (&brief, probe.as_ref()) {
                        // `computed` is positional with `outcome.miss_keys`, so a
                        // skipped row has to keep its slot rather than be filtered
                        // out here. `store_vectors` drops empty and wrong-dimension
                        // vectors while preserving that alignment — which is what
                        // stops a skipped or shrink-truncated row from ever being
                        // cached under a full-length key.
                        let computed: Vec<Vec<f32>> = miss_rows
                            .iter()
                            .map(|row| row.clone().unwrap_or_default())
                            .collect();
                        crate::search::onnx::embed_cache_frontend::store_provider_misses(
                            &brief.spec,
                            outcome,
                            &computed,
                        );
                    }
                    return merge_cached_rows(texts.len(), probe.as_ref(), miss_rows);
                }
                Ok(miss_rows) => tracing::warn!(
                    expected = expected_rows,
                    got = miss_rows.len(),
                    "Remote optional batch returned wrong row count"
                ),
                Err(error) => {
                    transient = error.is_transient();
                    tracing::warn!(
                        %error,
                        transient,
                        "Remote batch failed; applying generation fallback policy"
                    );
                }
            }
            if self.remote_rows_landed.get() {
                return self.freeze_after_failure(texts.len(), transient);
            }
            self.select_local_fallback();
            return self.selected.borrow().embed_neural_batch_blocking(texts);
        }
        self.selected.borrow().embed_neural_batch_blocking(texts)
    }

    /// Batch embed with per-document titles (relative file paths). Remote
    /// providers apply the titled doc template (EmbeddingGemma2 prompt
    /// training); local backends ignore titles and embed text-only — the
    /// vectors are dimension-identical and mix safely within a generation.
    /// Failure/fallback semantics are identical to
    /// `embed_neural_batch_blocking`.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub(crate) fn embed_neural_batch_blocking_titled<S: AsRef<str>>(
        &self,
        texts: &[S],
        titles: Vec<Option<String>>,
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
            // Same cache path as the untitled variant, but keyed on the
            // TEMPLATED input: the endpoint embeds `template(title, text)`, not
            // `text`, so two nodes with identical snippets under different file
            // paths must not share a row.
            let brief = self.cache_brief(&remote);
            let probe = brief.and_then(|brief| {
                let inputs: Vec<String> = owned
                    .iter()
                    .zip(titles.iter())
                    .map(|(text, title)| {
                        crate::search::onnx::remote::apply_doc_template(
                            remote.doc_template(),
                            title.as_deref(),
                            text,
                        )
                        .into_owned()
                    })
                    .collect();
                crate::search::onnx::embed_cache_frontend::probe_provider_inputs(
                    &brief.spec,
                    &inputs,
                )
            });
            if let Some(outcome) = probe.as_ref() {
                if outcome.miss_positions.is_empty() {
                    tracing::debug!(
                        hits = outcome.hits.len(),
                        "Remote titled embed cache: every row hit; endpoint not contacted"
                    );
                    self.remote_rows_landed.set(true);
                    return merge_cached_rows(texts.len(), Some(outcome), Vec::new());
                }
            }
            let miss_rows_request: Vec<(Option<String>, String)> = match probe.as_ref() {
                Some(outcome) => outcome
                    .miss_positions
                    .iter()
                    .map(|&position| (titles[position].clone(), owned[position].clone()))
                    .collect(),
                None => owned
                    .iter()
                    .zip(titles.iter())
                    .map(|(text, title)| (title.clone(), text.clone()))
                    .collect(),
            };
            let expected_rows = miss_rows_request.len();
            let (miss_titles, miss_texts): (Vec<Option<String>>, Vec<String>) =
                miss_rows_request.into_iter().unzip();
            let mut transient = false;
            match block_on_remote(async move {
                let refs: Vec<&str> = miss_texts.iter().map(String::as_str).collect();
                remote
                    .embed_batch_optional_titled(refs, miss_titles.into_iter().peekable())
                    .await
                    .map_err(bridge_error)
            }) {
                Ok(miss_rows) if miss_rows.len() == expected_rows => {
                    if miss_rows.iter().flatten().any(|row| !row.is_empty()) {
                        // Any successful remote row pins this generation's space.
                        self.remote_rows_landed.set(true);
                    }
                    if let (Some(brief), Some(outcome)) = (&brief, probe.as_ref()) {
                        // Same positional contract as the title-less path above:
                        // keep every slot so `miss_keys` stays aligned and
                        // `store_vectors` stays the single place that decides
                        // what is cacheable.
                        let computed: Vec<Vec<f32>> = miss_rows
                            .iter()
                            .map(|row| row.clone().unwrap_or_default())
                            .collect();
                        crate::search::onnx::embed_cache_frontend::store_provider_misses(
                            &brief.spec,
                            outcome,
                            &computed,
                        );
                    }
                    return merge_cached_rows(texts.len(), probe.as_ref(), miss_rows);
                }
                Ok(miss_rows) => tracing::warn!(
                    expected = expected_rows,
                    got = miss_rows.len(),
                    "Remote optional titled batch returned wrong row count"
                ),
                Err(error) => {
                    transient = error.is_transient();
                    if transient {
                        let failures = self.transient_failures.get() + 1;
                        self.transient_failures.set(failures);
                        tracing::warn!(
                            %error,
                            transient,
                            consecutive_failures = failures,
                            "Remote titled batch failed; applying generation fallback policy"
                        );
                        // A provider that has failed this many batches in a row is
                        // gone, not blipping. Stop rather than spend the full retry
                        // window on every remaining batch.
                        if failures >= MAX_CONSECUTIVE_TRANSIENT_FAILURES {
                            self.stopped.set(true);
                            tracing::warn!(
                                consecutive_failures = failures,
                                threshold = MAX_CONSECUTIVE_TRANSIENT_FAILURES,
                                "Remote endpoint failed {} consecutive batches; stopping remote \
                                 embedding for the rest of this generation. Remaining documents get \
                                 no neural row (semantic search will not retrieve them); fix the \
                                 endpoint and force a re-embed.",
                                failures
                            );
                        }
                    } else {
                        tracing::warn!(
                            %error,
                            transient,
                            "Remote titled batch failed; applying generation fallback policy"
                        );
                    }
                }
            }
            if self.remote_rows_landed.get() {
                return self.freeze_after_failure(texts.len(), transient);
            }
            self.select_local_fallback();
            return self.selected.borrow().embed_neural_batch_blocking(texts);
        }
        self.selected.borrow().embed_neural_batch_blocking(texts)
    }

    /// Policy for a failed remote batch in a generation that ALREADY holds
    /// remote rows (its vector space is pinned, so switching to the local model
    /// mid-generation would silently mix two spaces in one index).
    ///
    /// A transient failure — a dropped connection, a gateway/availability
    /// status — says nothing about the embedding space and the provider has
    /// already retried it with bounded backoff. Freezing here is what discarded
    /// tens of thousands of remaining documents over one blip, so only this
    /// batch's rows come back empty and the generation continues.
    ///
    /// Any other error is a contract failure (dimension/identity mismatch,
    /// malformed response) that would corrupt the index if it continued.
    #[cfg(feature = "remote-embeddings")]
    fn freeze_after_failure(&self, rows: usize, transient: bool) -> Vec<Option<Vec<f32>>> {
        if transient {
            tracing::warn!(
                rows,
                "Remote batch still failing after retries; leaving these rows neural-less instead of freezing the generation"
            );
        } else {
            self.stopped.set(true);
            tracing::warn!(
                "Remote failed after accepted rows; neural generation frozen to TF-IDF for remaining documents"
            );
        }
        vec![None; rows]
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

    /// Seed an unmeasured remote provider with a dimension recovered from persisted rows.
    #[cfg(feature = "remote-embeddings")]
    pub(crate) fn seed_measured_dimension(&self, dimension: usize) {
        if let Self::HybridRemote { remote, .. } = self {
            remote.seed_measured_dimension(dimension);
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
                match block_on_remote(async move {
                    remote.embed(&owned_text).await.map_err(bridge_error)
                }) {
                    Ok(embedding) => Some(Ok(embedding)),
                    Err(e) => Some(Err(e.to_string())),
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
                        .map_err(bridge_error)
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

/// Why a bridged remote call failed, in the terms the generation policy needs.
///
/// [`block_on_remote`] flattens errors to `String` for `Display`, which used to
/// destroy the one distinction the fallback policy cares about: a transient
/// endpoint blip versus a permanent contract failure. The classification is
/// therefore computed where the typed error still exists (inside the async
/// block) and carried across the thread boundary.
#[cfg(feature = "remote-embeddings")]
enum RemoteBridgeError {
    /// The request failed without saying anything about the embedding space
    /// (dropped connection, gateway/availability status). Retryable.
    Transient(RemoteEmbeddingError),
    /// Contract or environment failure: the endpoint rejected the payload on
    /// its merits, or the bridge itself could not run. Continuing would
    /// corrupt the index, so the generation must freeze.
    Permanent(String),
}

#[cfg(feature = "remote-embeddings")]
impl RemoteBridgeError {
    fn is_transient(&self) -> bool {
        matches!(self, Self::Transient(_))
    }
}

#[cfg(feature = "remote-embeddings")]
impl std::fmt::Display for RemoteBridgeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transient(error) => f.write_str(&error.to_string()),
            Self::Permanent(message) => f.write_str(message),
        }
    }
}

/// Classify a provider error for the generation fallback policy.
#[cfg(feature = "remote-embeddings")]
fn bridge_error(error: RemoteEmbeddingError) -> RemoteBridgeError {
    if is_retryable_embedding_error(&error) {
        RemoteBridgeError::Transient(error)
    } else {
        RemoteBridgeError::Permanent(error.to_string())
    }
}

/// Bridge a remote-embeddings future onto the synchronous pipeline.
///
/// The CLI indexing/query paths never run inside a tokio runtime, so a
/// scratch single-thread reactor is safe here (`block_on` panics only when
/// called from within an async context, which cannot happen on these paths).
/// One runtime per call keeps no global state and no background threads.
/// The future must be `'static` + `Send` (ours hold only the `Arc`-based
/// `GenericRemoteProvider`) and resolve to a [`RemoteBridgeError`], whose
/// transient/permanent split the generation fallback policy needs: flattening
/// it to a `String` here is what made one dropped connection indistinguishable
/// from a contract failure.
#[cfg(feature = "remote-embeddings")]
fn block_on_remote<T: Send + 'static>(
    fut: impl std::future::Future<Output = Result<T, RemoteBridgeError>> + Send + 'static,
) -> Result<T, RemoteBridgeError> {
    std::thread::Builder::new()
        .name("remote-embed-bridge".to_string())
        .spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| RemoteBridgeError::Permanent(e.to_string()))
                .and_then(|rt| {
                    rt.block_on(fut)
                        .map_err(|e| RemoteBridgeError::Permanent(e.to_string()))
                })
        })
        .map_err(|e| {
            RemoteBridgeError::Permanent(format!("failed to spawn remote-embed bridge thread: {e}"))
        })?
        .join()
        .map_err(|_| {
            RemoteBridgeError::Permanent("remote-embed bridge thread panicked".to_string())
        })?
}

/// Resolved provider cache identity for one remote batch.
#[derive(Clone, Copy)]
#[cfg(feature = "remote-embeddings")]
struct CacheBrief {
    spec: KeySpec,
}

/// Reassemble one batch in ORIGINAL order: cache hits at their positions,
/// `miss_rows[i]` at `miss_positions[i]`. A skipped row (`None` or an empty
/// vector) stays `None`; it is never turned into `Some(empty)`.
#[cfg(feature = "remote-embeddings")]
fn merge_cached_rows(
    total: usize,
    probe: Option<&CacheProbeOutcome>,
    miss_rows: Vec<Option<Vec<f32>>>,
) -> Vec<Option<Vec<f32>>> {
    let Some(outcome) = probe else {
        return miss_rows;
    };
    debug_assert_eq!(miss_rows.len(), outcome.miss_positions.len());
    let mut merged: Vec<Option<Vec<f32>>> = vec![None; total];
    for (&position, vector) in &outcome.hits {
        merged[position] = Some(vector.clone());
    }
    for (&position, row) in outcome.miss_positions.iter().zip(miss_rows) {
        merged[position] = row.filter(|vector| !vector.is_empty());
    }
    merged
}

#[cfg(all(test, feature = "remote-embeddings"))]
mod merge_tests {
    use super::*;

    #[test]
    fn merge_places_hits_and_misses_at_original_positions() {
        // 0 hit, 1 skipped miss, 2 empty-vector miss, 3 hit, 4 real miss.
        let outcome = CacheProbeOutcome {
            hits: std::collections::HashMap::from([(0, vec![1.0]), (3, vec![3.0])]),
            miss_positions: vec![1, 2, 4],
            miss_keys: Vec::new(),
        };
        let merged = merge_cached_rows(
            5,
            Some(&outcome),
            vec![None, Some(Vec::new()), Some(vec![4.0])],
        );
        assert_eq!(
            merged,
            vec![
                Some(vec![1.0]),
                None,
                None,
                Some(vec![3.0]),
                Some(vec![4.0])
            ]
        );
    }
}
