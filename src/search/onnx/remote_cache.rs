//! Client-side embedding cache for the REMOTE embedding route.
//!
//! The remote route re-embeds every admitted node on every index: the neural
//! checkpoint is keyed on a lexical hash of the whole corpus, so any source
//! change re-embeds all of it, and no remote call ever consulted a cache.
//! Measured on this repository, ~98% of index wall time is that repeated
//! per-node embedding.
//!
//! This module maps the remote provider identity onto the existing, index-scale
//! content-addressed [`GlobalEmbeddingCache`] (spec §6.5 key) rather than adding
//! a second store:
//!
//! | `CacheKey` field | remote-route meaning |
//! |---|---|
//! | `model_digest` | blake3 of endpoint + model + base URL + dimension |
//! | `tokenizer_digest` | blake3 of the active doc/query templates |
//! | `prompt_role_and_version` | document role + template-application version |
//! | `pooling`, `normalization` | [`Pooling::Unspecified`] / [`Normalization::Unspecified`] |
//! | `output_dimensions` | the provider's resolved dimension |
//! | `content_hash` | blake3 of the exact templated input |
//!
//! The two digest slots mean "which model produced this vector" and "which
//! input transform fed it" — exactly what the local route stores there (model
//! file bytes, tokenizer config bytes). A remote endpoint has no such files, so
//! its identity string is the equivalent fact. The dimension is folded into
//! `model_digest` as well as `output_dimensions` so a dimension resolved
//! between runs can never be mistaken for the same space.
//!
//! ## Why the pooling/normalization fields are `Unspecified`
//!
//! A client cannot observe how a remote server aggregates token states or
//! whether it normalizes. Recording a concrete variant would be a guess in a
//! field that is supposed to be a fact. `Unspecified` is honest and is a safe
//! namespace salt — it can only ever separate namespaces, never merge two
//! vector spaces. Correctness does not rest on it: it rests on the endpoint
//! identity being part of the key, and on rows being stored byte-for-byte as
//! the endpoint returned them.
//!
//! Only real endpoint output may be stored. Callers pass the rows that actually
//! landed; skipped and bisected-away inputs are never stored, so a degraded
//! vector can never enter the cache.

use std::borrow::Cow;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::embed::cache::{CacheKey, GlobalEmbeddingCache, Normalization, Pooling, ProbeResult};

/// Prompt role for the DOCUMENT path (the only path this cache serves).
const DOCUMENT_ROLE: u16 = 1;
/// Version of the template substitution applied before embedding. Bumped when
/// an unchanged template starts producing different inputs.
const TEMPLATE_VERSION: u16 = 1;

/// Process-wide counters, surfaced in index progress logs so cache
/// effectiveness is visible and a regression to zero hits is obvious.
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
static STORED: AtomicU64 = AtomicU64::new(0);

/// Cumulative `(hits, misses, stored)` observed by this process.
pub(crate) fn counters() -> (u64, u64, u64) {
    (
        HITS.load(Ordering::Relaxed),
        MISSES.load(Ordering::Relaxed),
        STORED.load(Ordering::Relaxed),
    )
}

/// What a probe found, in the caller's own position space.
pub(crate) struct ProbeOutcome {
    /// Aligned with the caller's inputs; `Some` where a vector was cached.
    pub(crate) cached: Vec<Option<Vec<f32>>>,
    /// Positions that still need an embed request, ascending.
    pub(crate) miss_positions: Vec<usize>,
    /// Keys for `miss_positions`, index-aligned with it.
    miss_keys: Vec<CacheKey>,
}

impl ProbeOutcome {
    /// Every position already cached (no request is needed at all).
    pub(crate) fn is_fully_cached(&self) -> bool {
        self.miss_positions.is_empty()
    }
}

/// A cache handle bound to one remote provider identity and dimension.
pub(crate) struct RemoteEmbedCache {
    store: GlobalEmbeddingCache,
    /// Namespace digest: endpoint + model + base URL + dimension.
    model_digest: [u8; 32],
    /// Template-transform digest.
    tokenizer_digest: [u8; 32],
    dimension: u32,
    identity: String,
}

impl RemoteEmbedCache {
    /// Open the shared store for `identity` at `dimension`.
    ///
    /// Returns `None` — and the caller embeds uncached — when the feature flag
    /// is off or the store cannot be opened. A cache failure must never fail an
    /// index.
    pub(crate) fn open(identity: &str, dimension: usize) -> Option<Self> {
        if dimension == 0 {
            return None;
        }
        if !crate::feature_flags::FeatureFlag::GlobalEmbedCache.is_enabled() {
            return None;
        }
        // Split the identity at its template component so the two digest slots
        // carry the two facts they mean locally: which model, which transform.
        let (model_identity, template) = match identity.split_once("|tpl") {
            Some((model, fingerprint)) => (model, format!("tpl{fingerprint}")),
            None => (identity, String::new()),
        };
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"remote-embed-cache-v1\0");
        hasher.update(model_identity.as_bytes());
        hasher.update(&(dimension as u64).to_le_bytes());
        let model_digest: [u8; 32] = *hasher.finalize().as_bytes();
        let tokenizer_digest: [u8; 32] = blake3::hash(template.as_bytes()).into();

        // Same root as the local route's client-side cache: one store, so a
        // project indexed locally and remotely shares no rows by accident but
        // does share the store's budget and maintenance.
        let root = super::embed_cache_frontend::cache_root();
        let store = match GlobalEmbeddingCache::open(&root) {
            Ok(store) => store,
            Err(error) => {
                tracing::warn!(
                    root = %root.display(),
                    %error,
                    "remote embed cache unavailable; embedding proceeds uncached"
                );
                return None;
            }
        };
        tracing::info!(
            identity,
            dimension,
            root = %root.display(),
            "remote embed cache opened"
        );
        Some(Self {
            store,
            model_digest,
            tokenizer_digest,
            dimension: dimension as u32,
            identity: identity.to_string(),
        })
    }

    fn key(&self, input: &str) -> CacheKey {
        CacheKey {
            model_digest: self.model_digest,
            tokenizer_digest: self.tokenizer_digest,
            prompt_role_and_version: CacheKey::pack_prompt_role_and_version(
                DOCUMENT_ROLE,
                TEMPLATE_VERSION,
            ),
            pooling: Pooling::Unspecified,
            normalization: Normalization::Unspecified,
            output_dimensions: self.dimension,
            content_hash: CacheKey::content_hash(input),
        }
    }

    /// Probe the cache for already-templated inputs.
    ///
    /// `None` means caching is unavailable for this batch and the caller should
    /// embed everything.
    pub(crate) fn probe(&mut self, inputs: &[Cow<'_, str>]) -> Option<ProbeOutcome> {
        if inputs.is_empty() {
            return None;
        }
        let keys: Vec<CacheKey> = inputs.iter().map(|input| self.key(input)).collect();
        let stored = match self.store.probe(&keys) {
            Ok(stored) => stored,
            Err(error) => {
                tracing::warn!(
                    %error,
                    "remote embed cache probe failed; embedding proceeds uncached"
                );
                return None;
            }
        };
        let ProbeResult { hits, misses } = stored;

        let mut cached = vec![None; inputs.len()];
        let mut miss_positions = Vec::with_capacity(misses.len());
        for (position, vector) in hits {
            // Defensive: a row of the wrong width is not a hit for this space.
            if vector.len() == self.dimension as usize {
                cached[position] = Some(vector);
            } else {
                miss_positions.push(position);
            }
        }
        miss_positions.extend(misses);
        miss_positions.sort_unstable();
        miss_positions.dedup();

        let hit_count = cached.iter().filter(|row| row.is_some()).count() as u64;
        HITS.fetch_add(hit_count, Ordering::Relaxed);
        MISSES.fetch_add(miss_positions.len() as u64, Ordering::Relaxed);

        let miss_keys = miss_positions
            .iter()
            .map(|&p| self.key(&inputs[p]))
            .collect();
        Some(ProbeOutcome {
            cached,
            miss_positions,
            miss_keys,
        })
    }

    /// Store the rows that actually landed.
    ///
    /// `rows` is aligned with the caller's batch. Positions holding `None`
    /// (inputs the endpoint skipped or a bisect gave up on) are deliberately
    /// not stored: a degraded vector must never enter the cache. Positions in
    /// `shrunk` carry a vector for a truncated PREFIX of the input (the
    /// oversize-shrink retry path); storing it under the full input's key
    /// would pin the truncated representation forever, so those are skipped
    /// too — the next run re-embeds and, if the endpoint now accepts the whole
    /// document, lands the complete vector.
    pub(crate) fn store_landed(
        &mut self,
        probe: &ProbeOutcome,
        rows: &[Option<Vec<f32>>],
        shrunk: &[usize],
    ) {
        let entries: Vec<(CacheKey, Vec<f32>)> = probe
            .miss_keys
            .iter()
            .zip(probe.miss_positions.iter())
            .filter_map(|(key, &position)| {
                if shrunk.contains(&position) {
                    return None;
                }
                let vector = rows.get(position)?.as_ref()?;
                (vector.len() == self.dimension as usize && !vector.is_empty())
                    .then(|| (key.clone(), vector.clone()))
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        match self.store.put_batch(&entries) {
            Ok(written) => {
                STORED.fetch_add(written as u64, Ordering::Relaxed);
                tracing::debug!(
                    identity = %self.identity,
                    written,
                    total = entries.len(),
                    "remote embed cache batch store complete"
                );
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "remote embed cache store failed; vectors will be recomputed next run"
                );
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    /// Point the shared store at a scratch directory for one test.
    ///
    /// `LEINDEX_HOME` is process-global, so these tests serialize on a mutex
    /// and each restores the previous value.
    struct ScratchHome {
        _guard: std::sync::MutexGuard<'static, ()>,
        previous: Option<String>,
    }

    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    impl ScratchHome {
        fn new(label: &str) -> (Self, std::path::PathBuf) {
            let guard = HOME_LOCK.lock().unwrap_or_else(|error| error.into_inner());
            let previous = std::env::var("LEINDEX_HOME").ok();
            let root = std::env::temp_dir().join(format!(
                "leindex-remote-cache-{label}-{}-{:?}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or_default()
            ));
            std::fs::create_dir_all(&root).expect("scratch home");
            // SAFETY: serialized by HOME_LOCK for these tests.
            unsafe { std::env::set_var("LEINDEX_HOME", &root) };
            (
                Self {
                    _guard: guard,
                    previous,
                },
                root,
            )
        }
    }

    impl Drop for ScratchHome {
        fn drop(&mut self) {
            // SAFETY: serialized by the guard held in `_guard`.
            match &self.previous {
                Some(value) => unsafe { std::env::set_var("LEINDEX_HOME", value) },
                None => unsafe { std::env::remove_var("LEINDEX_HOME") },
            }
        }
    }

    fn inputs(texts: &[&str]) -> Vec<Cow<'static, str>> {
        texts.iter().map(|t| Cow::Owned((*t).to_string())).collect()
    }

    #[test]
    fn test_second_probe_over_the_same_texts_is_a_full_hit() {
        let (_home, _root) = ScratchHome::new("full-hit");
        let mut cache = RemoteEmbedCache::open("remote|custom|http://x/v1||m1|8", 8)
            .expect("cache opens with the feature flag on");

        let batch = inputs(&["alpha", "beta", "gamma"]);
        let first = cache.probe(&batch).expect("probe");
        assert!(
            first.miss_positions == vec![0, 1, 2],
            "a cold cache misses everything"
        );
        assert!(first.cached.iter().all(Option::is_none));

        // Store exactly what the endpoint returned for those positions.
        let rows: Vec<Option<Vec<f32>>> = (0..3).map(|i| Some(vec![i as f32; 8])).collect();
        cache.store_landed(&first, &rows, &[]);

        let second = cache.probe(&batch).expect("probe");
        assert!(
            second.is_fully_cached(),
            "the same texts must be a full hit, so no embed request is issued"
        );
        assert_eq!(second.cached[0].as_deref(), Some(&[0.0f32; 8][..]));
        assert_eq!(second.cached[1].as_deref(), Some(&[1.0f32; 8][..]));
        assert_eq!(second.cached[2].as_deref(), Some(&[2.0f32; 8][..]));
    }

    #[test]
    fn test_rows_that_did_not_land_are_never_cached() {
        let (_home, _root) = ScratchHome::new("partial");
        let mut cache = RemoteEmbedCache::open("remote|custom|http://x/v1||m1|8", 8)
            .expect("cache opens with the feature flag on");

        let batch = inputs(&["kept", "skipped"]);
        let probe = cache.probe(&batch).expect("probe");
        assert_eq!(probe.miss_positions, vec![0, 1]);

        // Position 1 was skipped by the endpoint / given up on by a bisect.
        let rows = vec![Some(vec![1.0f32; 8]), None];
        cache.store_landed(&probe, &rows, &[]);

        let again = cache.probe(&batch).expect("probe");
        assert_eq!(
            again.miss_positions,
            vec![1],
            "only the row that actually landed may be served; the missing one must be re-requested"
        );
        assert!(again.cached[0].is_some());
    }

    /// Codex P2: the oversize-shrink path lands a vector for a truncated
    /// PREFIX of the input into the batch's rows. That row must NOT be stored
    /// under the full input's key: the next run would serve the truncated
    /// vector from the cache even if its endpoint now accepts the whole
    /// document, pinning the degraded representation forever.
    #[test]
    fn test_shrunk_prefix_rows_are_excluded_from_cache_writes() {
        let (_home, _root) = ScratchHome::new("shrunk");
        let mut cache = RemoteEmbedCache::open("remote|custom|http://x/v1||m1|8", 8)
            .expect("cache opens with the feature flag on");

        let batch = inputs(&["full", "whole"]);
        let probe = cache.probe(&batch).expect("probe");
        assert_eq!(probe.miss_positions, vec![0, 1]);

        // Both rows "landed" (the batch returned vectors for them), but row 0
        // came from the shrink-retry path: its vector embeds only a prefix of
        // "full". Row 1 embedded its input whole.
        let rows = vec![Some(vec![9.0f32; 8]), Some(vec![1.0f32; 8])];
        cache.store_landed(&probe, &rows, &[0]);

        let again = cache.probe(&batch).expect("probe");
        assert_eq!(
            again.miss_positions,
            vec![0],
            "a shrunk-prefix vector must never be served under the full input's key"
        );
        assert!(
            again.cached[1].is_some(),
            "the whole-input row is unaffected and must be served"
        );
    }

    #[test]
    fn test_a_different_dimension_is_a_different_namespace() {
        let (_home, _root) = ScratchHome::new("dimension");
        let identity = "remote|custom|http://x/v1||m1|8";
        let mut small = RemoteEmbedCache::open(identity, 8).expect("cache opens");
        let batch = inputs(&["same text"]);
        let probe = small.probe(&batch).expect("probe");
        small.store_landed(&probe, &[Some(vec![1.0f32; 8])], &[]);

        let mut large = RemoteEmbedCache::open(identity, 16).expect("cache opens");
        let probe = large.probe(&batch).expect("probe");
        assert!(
            !probe.is_fully_cached(),
            "a vector from another dimension must never be served"
        );
    }

    #[test]
    fn test_templates_and_models_do_not_share_vectors() {
        let (_home, _root) = ScratchHome::new("identity");
        let batch = inputs(&["same text"]);

        let mut base =
            RemoteEmbedCache::open("remote|custom|http://x/v1||m1|8", 8).expect("cache opens");
        let probe = base.probe(&batch).expect("probe");
        base.store_landed(&probe, &[Some(vec![1.0f32; 8])], &[]);

        for other in [
            // Same endpoint, different served model.
            "remote|custom|http://x/v1||m2|8",
            // Same model, different task template.
            "remote|custom|http://x/v1||m1|tpl0123456789abcdef|8",
            // Different endpoint.
            "remote|custom|http://y/v1||m1|8",
        ] {
            let mut cache = RemoteEmbedCache::open(other, 8).expect("cache opens");
            let probe = cache.probe(&batch).expect("probe");
            assert!(
                !probe.is_fully_cached(),
                "{other} must not reuse another vector space's row"
            );
        }
    }
}
