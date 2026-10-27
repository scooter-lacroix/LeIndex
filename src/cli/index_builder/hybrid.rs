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
    /// Whole-batch failures since the last successful batch. Reset by any
    /// success, so it measures a sustained outage rather than lifetime noise.
    #[cfg(feature = "remote-embeddings")]
    consecutive_failures: std::cell::Cell<usize>,
    /// Restarts already spent by this generation. Bounded by
    /// [`MAX_GENERATION_RESTARTS`] so an unrevivable endpoint cannot be
    /// restarted in a loop.
    #[cfg(feature = "remote-embeddings")]
    restarts_used: std::cell::Cell<usize>,
}

/// How many whole batches may fail IN A ROW before the generation gives up.
///
/// Each individual request has already spent the transport retry ladder
/// (`RemoteEmbeddingConfig::max_retries`), so reaching this count means the
/// endpoint is genuinely gone, not that one socket hiccuped. The old policy
/// had this at 1: a single reset socket nine minutes into a run froze 70% of
/// the corpus out of the neural layer and there was no way back.
pub(crate) const MAX_CONSECUTIVE_REMOTE_FAILURES: usize = 5;

/// Restarts allowed per generation.
///
/// A restart clears the failure budget, so without a separate cap a service
/// that dies on every start would be restarted forever. One restart absorbs a
/// single OOM or driver reset; exhausting a whole second budget after that is
/// a real, unrecoverable outage.
pub(crate) const MAX_GENERATION_RESTARTS: usize = 1;

/// What to do with a remote batch that produced no usable rows.
#[cfg(feature = "remote-embeddings")]
#[derive(Debug, PartialEq, Eq)]
enum RemoteFailureOutcome {
    /// Skip this batch only; the generation stays on the remote endpoint.
    SkipBatch,
    /// Rows already landed in the neural space, so falling back to a different
    /// embedding space would mix incomparable vectors: freeze instead.
    Freeze,
    /// No remote row exists yet, so a coherent all-local generation is safe.
    FallbackLocal,
}

#[cfg(feature = "remote-embeddings")]
fn remote_failure_outcome(landed: bool, consecutive_failures: usize) -> RemoteFailureOutcome {
    if consecutive_failures < MAX_CONSECUTIVE_REMOTE_FAILURES {
        RemoteFailureOutcome::SkipBatch
    } else if landed {
        RemoteFailureOutcome::Freeze
    } else {
        RemoteFailureOutcome::FallbackLocal
    }
}

impl GenerationNeuralEmbedder {
    pub(crate) fn new(embedder: HybridEmbedder) -> Self {
        Self {
            selected: std::cell::RefCell::new(embedder),
            #[cfg(feature = "remote-embeddings")]
            remote_rows_landed: std::cell::Cell::new(false),
            #[cfg(feature = "remote-embeddings")]
            stopped: std::cell::Cell::new(false),
            #[cfg(feature = "remote-embeddings")]
            consecutive_failures: std::cell::Cell::new(0),
            #[cfg(feature = "remote-embeddings")]
            restarts_used: std::cell::Cell::new(0),
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

    /// Account for one remote batch that produced no usable rows, and return
    /// the rows the caller must emit.
    ///
    /// A single failed batch is survivable: the caller gets `None` rows for
    /// that batch only and the NEXT batch still goes to the remote endpoint, so
    /// a transient outage costs a few documents instead of the rest of the
    /// run. Any successful response clears the count.
    ///
    /// Once [`MAX_CONSECUTIVE_REMOTE_FAILURES`] batches fail in a row the
    /// endpoint is treated as genuinely down, and the escalation prefers
    /// HEALING over abandoning: a managed service (one LeIndex launched) is
    /// restarted and its budget restored, so an OOM or a driver reset costs one
    /// restart instead of the neural layer for every remaining document. Only
    /// an endpoint that cannot be healed — a third-party API, or a managed one
    /// whose restart failed — falls through to the freeze/fallback policy.
    #[cfg(feature = "remote-embeddings")]
    fn on_remote_batch_failed<S: AsRef<str>>(
        &self,
        texts: &[S],
        reason: &'static str,
    ) -> Vec<Option<Vec<f32>>> {
        let failures = self.consecutive_failures.get() + 1;
        self.consecutive_failures.set(failures);
        if failures < MAX_CONSECUTIVE_REMOTE_FAILURES {
            tracing::warn!(
                reason,
                failures,
                budget = MAX_CONSECUTIVE_REMOTE_FAILURES,
                documents = texts.len(),
                "Remote batch produced no rows; skipping this batch and keeping the remote endpoint"
            );
            return vec![None; texts.len()];
        }
        // Budget spent: try to heal the endpoint before giving up on it.
        if self.try_restart_endpoint() {
            self.consecutive_failures.set(0);
            tracing::warn!(
                reason,
                failures,
                documents = texts.len(),
                "Managed endpoint restarted; retrying this batch on the healed endpoint"
            );
            return self.embed_neural_batch_blocking(texts);
        }
        match remote_failure_outcome(self.remote_rows_landed.get(), failures) {
            // Unreachable in practice: `remote_failure_outcome` only reports
            // SkipBatch below the budget, which the early return handled.
            RemoteFailureOutcome::SkipBatch => vec![None; texts.len()],
            RemoteFailureOutcome::Freeze => {
                self.stopped.set(true);
                tracing::error!(
                    reason,
                    failures,
                    "Remote failed repeatedly after accepted rows and could not be restarted; \
                     neural generation frozen for remaining documents"
                );
                vec![None; texts.len()]
            }
            RemoteFailureOutcome::FallbackLocal => {
                self.select_local_fallback();
                self.selected.borrow().embed_neural_batch_blocking(texts)
            }
        }
    }

    /// Restart the managed endpoint, bounded by [`MAX_GENERATION_RESTARTS`].
    ///
    /// Distinct from the provider-level latch: the provider's flag prevents
    /// restarting more than once per outage, while this bounds the total a
    /// generation will spend, so a service that cannot stay up fails fast
    /// instead of cycling stop/start for the rest of the run.
    #[cfg(feature = "remote-embeddings")]
    fn try_restart_endpoint(&self) -> bool {
        if self.restarts_used.get() >= MAX_GENERATION_RESTARTS {
            return false;
        }
        let revived = match &*self.selected.borrow() {
            HybridEmbedder::HybridRemote { remote, .. } => remote.try_revive_endpoint(),
            _ => false,
        };
        if revived {
            self.restarts_used.set(self.restarts_used.get() + 1);
        }
        revived
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
                        self.consecutive_failures.set(0);
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
                Err(error) => tracing::warn!(
                    %error,
                    transient = error.is_transient(),
                    "Remote batch failed; applying the remote failure policy"
                ),
            }
            return self.on_remote_batch_failed(texts, "remote_batch_no_rows");
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
                        // Progress was made, so the dead-endpoint streak is over.
                        self.consecutive_failures.set(0);
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
                Err(error) => tracing::warn!(
                    %error,
                    transient = error.is_transient(),
                    "Remote titled batch failed; applying the remote failure policy"
                ),
            }
            return self.on_remote_batch_failed(texts, "remote_titled_batch_no_rows");
        }
        self.selected.borrow().embed_neural_batch_blocking(texts)
    }

    #[cfg(feature = "remote-embeddings")]
    fn select_local_fallback(&self) {
        // No remote rows exist in this generation: a coherent all-Qwen
        // generation is safe. Construction/worker failures retain TF-IDF.
        let tfidf = {
            let selected = self.selected.borrow();
            selected.tfidf().clone()
        };
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
                    // Keep the test on the generation policy, not the transport
                    // ladder: every attempt fails in milliseconds against a
                    // closed port, and re-sending only spends wall clock.
                    max_retries: 0,
                    ..RemoteEmbeddingConfig::default()
                },
                None,
            )
            .expect("remote construction"),
        );
        // A pinned generation must never fall back to a different embedding
        // space, and the freeze must survive one bad batch — it takes sustained
        // failure to stop, which is the whole point of the budget.
        for attempt in 1..MAX_CONSECUTIVE_REMOTE_FAILURES {
            assert!(
                generation
                    .embed_neural_batch_blocking(&["document"])
                    .iter()
                    .all(Option::is_none)
            );
            assert!(
                !generation.stopped.get(),
                "failure {attempt} of {MAX_CONSECUTIVE_REMOTE_FAILURES} must not freeze"
            );
            assert!(matches!(
                *generation.selected.borrow(),
                HybridEmbedder::HybridRemote { .. }
            ));
        }
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

    /// The failure budget only works if a success clears it. Without the reset,
    /// failures scattered across a long index run accumulate into a freeze even
    /// though the endpoint answered correctly in between.
    #[test]
    fn test_remote_success_resets_the_consecutive_failure_budget() {
        let tfidf = TfIdfEmbedder::build(&[("a.rs".into(), "fn a() {}".into())]);
        let generation = GenerationNeuralEmbedder::new(
            HybridEmbedder::hybrid_remote(
                tfidf,
                RemoteEmbeddingConfig {
                    provider: crate::search::onnx::RemoteProvider::Custom {
                        endpoint: "http://127.0.0.1:1/v1".into(),
                    },
                    max_retries: 0,
                    ..RemoteEmbeddingConfig::default()
                },
                None,
            )
            .expect("remote construction"),
        );
        // One short of the budget, then a success, then one short again: the
        // generation must still be alive because the success reset the count.
        for attempt in 1..MAX_CONSECUTIVE_REMOTE_FAILURES {
            assert!(
                generation
                    .embed_neural_batch_blocking(&["document"])
                    .iter()
                    .all(Option::is_none)
            );
            assert_eq!(generation.consecutive_failures.get(), attempt);
        }
        generation.consecutive_failures.set(0); // stands in for a successful batch
        for attempt in 1..MAX_CONSECUTIVE_REMOTE_FAILURES {
            assert!(
                generation
                    .embed_neural_batch_blocking(&["document"])
                    .iter()
                    .all(Option::is_none)
            );
            assert_eq!(generation.consecutive_failures.get(), attempt);
        }
        assert!(
            !generation.stopped.get(),
            "scattered failures must not accumulate into a freeze"
        );
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

/// Why a bridged remote call failed.
///
/// [`block_on_remote`] flattens errors to `String` for `Display`, which would
/// destroy the one distinction worth carrying to the caller: a transient
/// endpoint blip versus a permanent contract failure. The classification is
/// therefore computed where the typed error still exists (inside the async
/// block) and carried across the thread boundary, so every failure log can name
/// which kind it was.
///
/// It is a DIAGNOSTIC, not a control input. The batch policy
/// (`on_remote_batch_failed`) drives every whole-batch failure through one
/// consecutive-failure budget plus endpoint healing, because a provider can
/// report a permanent-looking status for a restart-fixable fault (llama-server
/// answers an over-long input with HTTP 500). Escalating on the classification
/// instead would freeze a generation over a single bad document.
#[cfg(feature = "remote-embeddings")]
#[derive(Debug)]
pub(crate) enum RemoteBridgeError {
    /// The request failed without saying anything about the embedding space
    /// (dropped connection, gateway/availability status). Retryable.
    Transient(RemoteEmbeddingError),
    /// Contract or environment failure: the endpoint rejected the payload on
    /// its merits, or the bridge itself could not run.
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
/// The future is driven on a SPAWNED thread, never the caller's. That is not
/// incidental: `src/bin/leindex.rs` runs `rt.block_on(cli::main())`, so every
/// CLI subcommand — `leindex setup`, `leindex index`, and the daemon paths —
/// executes inside the outer runtime's context. `Runtime::block_on` panics
/// ("Cannot start a runtime from within a runtime") when called from inside a
/// runtime, so calling it on the caller's thread would abort the setup wizard
/// the moment model discovery ran. The spawn is what makes that impossible.
///
/// The runtime is PROCESS-WIDE, not scratch-per-call, and it lives on its own
/// thread. A runtime owns everything reqwest parks on it — the connection pool
/// included — so a fresh runtime per batch dropped the pool every time and the
/// next batch paid a full TCP connect + TLS handshake for every in-flight
/// request. That is pure overhead on top of the endpoint's own latency, and the
/// larger the corpus the more of it was paid. One long-lived runtime keeps the
/// keep-alive sockets owned by `pool_idle_timeout_for` alive across batches,
/// which is the entire point of enabling that pool for remote endpoints.
///
/// Multi-threaded rather than `new_current_thread`, and the reason is NOT
/// "overlapping requests": `join_all` over non-blocking reqwest futures already
/// progresses every request concurrently on a single-threaded reactor, because
/// the runtime's I/O driver wakes them as their sockets become ready. Worker
/// threads only matter for work that BLOCKS or is `spawn`ed. Two are kept as
/// headroom for exactly that: hyper performs its own connection setup work, and
/// a reactor with no spare worker serialises that against polling. The measured
/// 2.74x at K=4 came from overlapping round trips, which one thread would also
/// deliver — so do not read this thread count as the source of that speedup.
///
/// The future must be `'static` + `Send` (ours hold only the `Arc`-based
/// `GenericRemoteProvider`) and resolve to a [`RemoteBridgeError`], whose
/// transient/permanent split the generation fallback policy needs: flattening
/// it to a `String` here is what made one dropped connection indistinguishable
/// from a contract failure.
#[cfg(feature = "remote-embeddings")]
fn remote_runtime() -> Result<&'static tokio::runtime::Runtime, RemoteBridgeError> {
    static RUNTIME: std::sync::OnceLock<Result<tokio::runtime::Runtime, String>> =
        std::sync::OnceLock::new();
    match RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .thread_name("remote-embed-bridge")
            .build()
            .map_err(|e| e.to_string())
    }) {
        Ok(runtime) => Ok(runtime),
        Err(error) => Err(RemoteBridgeError::Permanent(error.clone())),
    }
}

#[cfg(feature = "remote-embeddings")]
pub(crate) fn block_on_remote<T: Send + 'static>(
    fut: impl std::future::Future<Output = Result<T, RemoteBridgeError>> + Send + 'static,
) -> Result<T, RemoteBridgeError> {
    std::thread::Builder::new()
        .name("remote-embed-bridge-caller".to_string())
        .spawn(move || remote_runtime()?.block_on(fut))
        .map_err(|e| {
            RemoteBridgeError::Permanent(format!("failed to spawn remote-embed bridge thread: {e}"))
        })?
        .join()
        .map_err(|_| {
            RemoteBridgeError::Permanent("remote-embed bridge thread panicked".to_string())
        })?
}

#[cfg(all(test, feature = "remote-embeddings"))]
mod bridge_test {
    use super::*;

    /// The bridge must work from INSIDE a runtime.
    ///
    /// `src/bin/leindex.rs` runs `rt.block_on(cli::main())`, so the setup wizard
    /// and the indexer both call this from within the outer runtime's context.
    /// A bridge that ran `Runtime::block_on` on the caller's thread would panic
    /// with "Cannot start a runtime from within a runtime" here — which is
    /// exactly the regression this test exists to catch.
    #[test]
    fn test_block_on_remote_is_callable_from_inside_a_runtime() {
        let outer = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .expect("outer runtime");
        let value = outer.block_on(async {
            // Called on the runtime's thread, in async context: the shape the
            // CLI actually uses.
            block_on_remote(async { Ok::<u32, RemoteBridgeError>(7) })
                .expect("bridge must not panic")
        });
        assert_eq!(value, 7);
    }

    /// A panicking future must surface as an error, not unwind the caller.
    ///
    /// The bridge runs the future on its own thread, and `join()` converts that
    /// thread's panic into an `Err`. Losing the join would turn a provider bug
    /// into a process abort mid-index.
    #[test]
    fn test_block_on_remote_contains_a_panicking_future() {
        let error = block_on_remote(async {
            panic!("provider blew up");
            #[allow(unreachable_code)]
            Ok::<u32, RemoteBridgeError>(0)
        })
        .expect_err("a panicking future must become an error");
        assert!(
            matches!(error, RemoteBridgeError::Permanent(_)),
            "a panic is not a transient condition; got a transient classification"
        );
    }
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
