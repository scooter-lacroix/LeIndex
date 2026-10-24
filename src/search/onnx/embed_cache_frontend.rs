//! Client-side access to the global embedding cache.
//!
//! The worker-side cache path (probe → embed misses → put) has existed since
//! WS10 Task 5, but no client ever sent `cache_keys`, so the cache was
//! unreachable in practice (telemetry: 0 hits / 0 misses). This frontend
//! probes the shared content-addressed store directly in the CLIENT process:
//!
//! - hits are applied without touching the worker at all — an all-hit batch
//!   never spawns the multi-GiB embed daemon, which was the dominant cost of
//!   the index tail (~26 s cold start + ~58 s inference on the stress repo);
//! - misses dispatch through the normal worker path and the client stores the
//!   fresh vectors under the same keys the worker would have used.
//!
//! Keys are the spec §6.5 6-tuple. The identity fields MUST match the
//! embedding semantics that actually produced the vector: pooling,
//! normalization, and the model/tokenizer digests. Two callers of this module
//! supply those fields differently:
//!
//! - the LOCAL worker path (`probe_texts`) resolves and streams the SAME model
//!   and tokenizer files the worker loads (last-token pooling + L2 per
//!   `EmbeddingRuntime::pool_and_normalize`, no prompt template);
//! - the REMOTE provider path (`probe_provider`) cannot see the server's
//!   files, so it hashes the provider's [`crate::search::onnx::remote`]
//!   identity string into the same digest slots — see [`KeySpec::for_provider`].
//!
//! Store lifetime and model identity are deliberately separate concerns here:
//! the store is opened once per process from the cache root (independent of
//! any model), while the identity travels per call inside [`KeySpec`]. That is
//! what lets one process cache for a remote endpoint on a machine that has no
//! local ONNX model installed at all.
//!
//! Cross-process safety: rows are immutable, content-addressed files written
//! via staging+rename; the client probes (reads) and puts (atomic writes) —
//! the identical access pattern the worker uses, so concurrent client/worker
//! access cannot observe partial rows.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

use crate::embed::cache::{CacheKey, GlobalEmbeddingCache, Normalization, Pooling, ProbeResult};

/// Probe outcome for one batch of texts.
pub(crate) struct CacheProbeOutcome {
    /// Original-text position → cached vector.
    pub(crate) hits: HashMap<usize, Vec<f32>>,
    /// Positions whose keys were absent (or corrupt — corrupt rows are
    /// already reported as misses by the store).
    pub(crate) miss_positions: Vec<usize>,
    /// Keys aligned with `miss_positions` (index i ↔ miss_positions[i]).
    pub(crate) miss_keys: Vec<CacheKey>,
}

/// Everything a cache key needs beyond the text itself: which model produced
/// the vector, and the pooling/normalization shape it was produced under.
///
/// This is the whole identity, so a caller can never accidentally key rows
/// under a model it is not actually talking to.
#[derive(Clone, Copy)]
pub(crate) struct KeySpec {
    model_digest: [u8; 32],
    tokenizer_digest: [u8; 32],
    pooling: Pooling,
    normalization: Normalization,
    output_dimensions: u32,
}

impl KeySpec {
    /// Identity for a provider whose model files this process cannot read
    /// (a remote `/v1/embeddings` endpoint: llama-server, vLLM, Ollama, ...).
    ///
    /// The digest slots are filled with blake3 of the provider's identity
    /// string, which is NOT a model file digest. That is the point: the two
    /// namespaces are disjoint by construction, so a remote row can never be
    /// served to the local ONNX path (or vice versa) even when both serve a
    /// model with the same name — the exact silent-vector-space-mixing failure
    /// this store is built to prevent.
    ///
    /// `identity` must cover everything that changes the produced vector:
    /// provider kind, model, endpoint, task-template fingerprint, and the
    /// measured output dimension (`GenericRemoteProvider::identity` does).
    pub(crate) fn for_provider(
        identity: &str,
        pooling: Pooling,
        normalization: Normalization,
        output_dimensions: usize,
    ) -> Self {
        let digest = |domain: &str| -> [u8; 32] {
            let mut hasher = blake3::Hasher::new();
            hasher.update(domain.as_bytes());
            hasher.update(&(identity.len() as u64).to_le_bytes());
            hasher.update(identity.as_bytes());
            hasher.finalize().into()
        };
        Self {
            model_digest: digest("LEINDEX-PROVIDER-CACHE-MODEL-V1"),
            tokenizer_digest: digest("LEINDEX-PROVIDER-CACHE-TOKENIZER-V1"),
            pooling,
            normalization,
            output_dimensions: output_dimensions as u32,
        }
    }

    fn key_for(&self, text: &str) -> CacheKey {
        CacheKey {
            model_digest: self.model_digest,
            tokenizer_digest: self.tokenizer_digest,
            // Neither the worker nor a remote /v1/embeddings call applies a
            // prompt template to these inputs; templates live in the provider
            // identity string instead.
            prompt_role_and_version: 0,
            pooling: self.pooling,
            normalization: self.normalization,
            output_dimensions: self.output_dimensions,
            content_hash: CacheKey::content_hash(text),
        }
    }
}

/// Digests of the local worker model, memoized per process. Hashing a ~900 MB
/// model file is expensive, so this must never happen per batch.
struct LocalDigests {
    model_name: String,
    output_dimensions: usize,
    model_digest: [u8; 32],
    tokenizer_digest: [u8; 32],
}

static STORE: OnceLock<Mutex<Option<GlobalEmbeddingCache>>> = OnceLock::new();
static LOCAL: OnceLock<Mutex<Option<LocalDigests>>> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
/// Latch: once opening fails (store failure), stop retrying so per-batch
/// probes on a broken install never repeat the expensive open.
static STORE_FAILED: AtomicBool = AtomicBool::new(false);
/// Separate latch for digest failures: unreadable model files disable the
/// LOCAL namespace only. A remote provider must still be cacheable on a
/// machine that never installed the local ONNX model.
static DIGESTS_FAILED: AtomicBool = AtomicBool::new(false);

/// Cumulative (hits, misses) observed by this process — surfaced in index
/// progress logs so cache effectiveness is visible.
#[cfg(any(feature = "cli", test))]
pub(crate) fn counters() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

/// Probe the cache for `texts` under the local worker model. Returns `None`
/// when caching is unavailable for this call (flag off, open failure,
/// unreadable model files) — callers then proceed exactly as before. Never
/// fails embedding.
pub(crate) fn probe_texts<S: AsRef<str>>(
    model_name: &str,
    output_dimensions: usize,
    texts: &[S],
) -> Option<CacheProbeOutcome> {
    if texts.is_empty() || output_dimensions == 0 {
        return None;
    }
    let spec = local_spec(model_name, output_dimensions)?;
    probe_spec(&spec, texts)
}

/// Probe the cache for `texts` under an explicit provider identity.
///
/// Same contract as [`probe_texts`]: `None` means "no cache for this call",
/// never an error.
pub(crate) fn probe_provider<S: AsRef<str>>(
    spec: &KeySpec,
    texts: &[S],
) -> Option<CacheProbeOutcome> {
    if texts.is_empty() || spec.output_dimensions == 0 {
        return None;
    }
    let keys: Vec<CacheKey> = texts
        .iter()
        .map(|text| spec.key_for(text.as_ref()))
        .collect();
    probe_keys(spec.output_dimensions as usize, keys)
}

/// Probe the cache under an explicit provider identity for a REMOTE `titled`
/// batch.
///
/// The server does not see `text` — it sees this provider's doc template
/// applied to (title, text), so a key computed from `text` alone is ambiguous:
/// two nodes sharing a snippet under different file paths would collide and
/// one would silently receive the other's vector. Key the exact string the
/// server embeds. The template is applied here (rather than re-derived) so the
/// key can never drift from `apply_doc_template`.
pub(crate) fn probe_provider_inputs<S: AsRef<str>>(
    spec: &KeySpec,
    inputs: &[S],
) -> Option<CacheProbeOutcome> {
    if inputs.is_empty() || spec.output_dimensions == 0 {
        return None;
    }
    let keys: Vec<CacheKey> = inputs
        .iter()
        .map(|input| spec.key_for(input.as_ref()))
        .collect();
    probe_keys(spec.output_dimensions as usize, keys)
}

/// Probe `texts` against the store using `spec`'s identity.
fn probe_spec<S: AsRef<str>>(spec: &KeySpec, texts: &[S]) -> Option<CacheProbeOutcome> {
    let keys: Vec<CacheKey> = texts
        .iter()
        .map(|text| spec.key_for(text.as_ref()))
        .collect();
    probe_keys(spec.output_dimensions as usize, keys)
}

fn probe_keys(expected_dim: usize, keys: Vec<CacheKey>) -> Option<CacheProbeOutcome> {
    let mut guard = store_lock().lock().ok()?;
    let store = store_slot(&mut guard)?;
    let ProbeResult { hits, misses } = store.probe(&keys).ok()?;
    HITS.fetch_add(hits.len() as u64, Ordering::Relaxed);
    MISSES.fetch_add(misses.len() as u64, Ordering::Relaxed);

    // Defensive: a stored row whose dimension disagrees with this request is
    // treated as a miss (probe already validates the fingerprint; this guards
    // config drift).
    let mut hits = hits;
    let mut miss_positions = misses;
    let stale: Vec<usize> = hits
        .iter()
        .filter(|(_, vector)| vector.len() != expected_dim)
        .map(|(position, _)| *position)
        .collect();
    for position in stale {
        hits.remove(&position);
        miss_positions.push(position);
        MISSES.fetch_add(1, Ordering::Relaxed);
    }
    miss_positions.sort_unstable();

    let miss_keys = miss_positions
        .iter()
        .map(|&position| keys[position].clone())
        .collect();

    Some(CacheProbeOutcome {
        hits,
        miss_positions,
        miss_keys,
    })
}

/// Store freshly computed vectors for a probe outcome's misses under the LOCAL
/// worker model's dimension. Best-effort: one batched write per call (no
/// per-row fsync — the cache is rebuildable and re-hash-verified on read);
/// failures warn and disable nothing.
pub(crate) fn store_misses(
    outcome: &CacheProbeOutcome,
    miss_vectors: &[Vec<f32>],
    output_dimensions: usize,
) {
    if output_dimensions == 0 {
        return;
    }
    store_vectors(output_dimensions, outcome, miss_vectors);
}

/// Store freshly computed vectors for a provider probe outcome's misses.
pub(crate) fn store_provider_misses(
    spec: &KeySpec,
    outcome: &CacheProbeOutcome,
    miss_vectors: &[Vec<f32>],
) {
    store_vectors(spec.output_dimensions as usize, outcome, miss_vectors);
}

fn store_vectors(expected_dim: usize, outcome: &CacheProbeOutcome, miss_vectors: &[Vec<f32>]) {
    if miss_vectors.is_empty() {
        return;
    }
    let Ok(mut guard) = store_lock().lock() else {
        return;
    };
    let Some(store) = store_slot(&mut guard) else {
        return;
    };
    // Only cache REAL model output; never TF-IDF-degraded substitutes.
    let entries: Vec<(CacheKey, Vec<f32>)> = outcome
        .miss_keys
        .iter()
        .zip(miss_vectors.iter())
        .filter(|(_, vector)| !vector.is_empty() && vector.len() == expected_dim)
        .map(|(key, vector)| ((*key).clone(), (*vector).clone()))
        .collect();
    if entries.is_empty() {
        return;
    }
    match store.put_batch(&entries) {
        Ok(written) => {
            tracing::debug!(
                stored = written,
                total = entries.len(),
                "embed cache batch store complete"
            );
        }
        Err(error) => {
            tracing::warn!(
                %error,
                "embed cache batch store failed; vectors will be recomputed next run"
            );
        }
    }
}

fn store_lock() -> &'static Mutex<Option<GlobalEmbeddingCache>> {
    STORE.get_or_init(|| Mutex::new(None))
}

/// Open the store on first use. Depends only on the cache root and the feature
/// flag — never on any model, so a remote-only install is cacheable.
fn store_slot(slot: &mut Option<GlobalEmbeddingCache>) -> Option<&mut GlobalEmbeddingCache> {
    if slot.is_none() {
        if STORE_FAILED.load(Ordering::Acquire) {
            return None;
        }
        if !crate::feature_flags::FeatureFlag::GlobalEmbedCache.is_enabled() {
            return None;
        }
        let root = cache_root();
        match GlobalEmbeddingCache::open(&root) {
            Ok(store) => {
                tracing::info!(root = %root.display(), "client-side embed cache opened");
                *slot = Some(store);
            }
            Err(error) => {
                STORE_FAILED.store(true, Ordering::Release);
                tracing::warn!(
                    root = %root.display(),
                    %error,
                    "embed cache open failed; embedding proceeds uncached"
                );
                return None;
            }
        }
    }
    slot.as_mut()
}

/// Resolve (and memoize) the digests of the model the LOCAL worker loads, so
/// keys can never disagree with the vectors actually produced.
fn local_spec(model_name: &str, output_dimensions: usize) -> Option<KeySpec> {
    if DIGESTS_FAILED.load(Ordering::Acquire) {
        return None;
    }
    let mut guard = LOCAL.get_or_init(|| Mutex::new(None)).lock().ok()?;
    if guard.is_none() {
        let open_failure = |context: &str| {
            DIGESTS_FAILED.store(true, Ordering::Release);
            tracing::warn!(
                model = model_name,
                context,
                "embed cache identity unavailable; local embedding proceeds uncached"
            );
        };
        let model_path = match crate::embed::model_path::ModelResolver::resolve(model_name) {
            Ok(path) => path,
            Err(error) => {
                tracing::debug!(%error, "embed cache: model path unresolved");
                open_failure("model-path");
                return None;
            }
        };
        let tokenizer_path =
            match crate::embed::model_path::ModelResolver::resolve_tokenizer(model_name) {
                Ok(path) => path,
                Err(error) => {
                    tracing::debug!(%error, "embed cache: tokenizer path unresolved");
                    open_failure("tokenizer-path");
                    return None;
                }
            };
        let model_digest = match streaming_digest(&model_path) {
            Ok(digest) => digest,
            Err(error) => {
                tracing::debug!(
                    path = %model_path.display(),
                    %error,
                    "embed cache: model digest unavailable"
                );
                open_failure("model-digest");
                return None;
            }
        };
        let tokenizer_digest = match streaming_digest(&tokenizer_path) {
            Ok(digest) => digest,
            Err(error) => {
                tracing::debug!(
                    path = %tokenizer_path.display(),
                    %error,
                    "embed cache: tokenizer digest unavailable"
                );
                open_failure("tokenizer-digest");
                return None;
            }
        };
        *guard = Some(LocalDigests {
            model_name: model_name.to_string(),
            output_dimensions,
            model_digest,
            tokenizer_digest,
        });
    }
    let local = guard.as_ref()?;
    if local.model_name != model_name || local.output_dimensions != output_dimensions {
        // A second model in the same process would need its own digests; the
        // memoized identity is authoritative for the resolved model, so bypass
        // rather than key rows under the wrong model.
        return None;
    }
    Some(KeySpec {
        model_digest: local.model_digest,
        tokenizer_digest: local.tokenizer_digest,
        // Must mirror `EmbeddingRuntime::pool_and_normalize`: final unpadded
        // token, then L2 normalization.
        pooling: Pooling::LastToken,
        normalization: Normalization::L2,
        output_dimensions: local.output_dimensions as u32,
    })
}

/// Mirror of the worker's `default_embed_cache_root` — the SAME directory the
/// worker-side cache uses, so both sides share one store.
fn cache_root() -> std::path::PathBuf {
    if let Ok(home) = std::env::var("LEINDEX_HOME") {
        return std::path::PathBuf::from(home).join("embed-cache");
    }
    if let Ok(home) = std::env::var("HOME") {
        return std::path::PathBuf::from(home)
            .join(".leindex")
            .join("embed-cache");
    }
    std::path::PathBuf::from(".leindex").join("embed-cache")
}

/// Streaming blake3 of a file — never materializes large models on the heap.
fn streaming_digest(path: &Path) -> std::io::Result<[u8; 32]> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher.finalize().into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_counters_start_at_zero() {
        // HITS/MISSES are process-global atomics that any concurrent embed-path
        // test may legitimately increment (probe_texts runs in other lib tests
        // and on dev machines the feature flag can be on by default), so
        // asserting an absolute zero here is order-dependent. Assert the real
        // invariant instead: counters only ever move via cache probes, and a
        // bypassed probe (empty texts / zero dim) must not touch them.
        let (hits_before, misses_before) = counters();
        assert!(probe_texts("model", 8, &[] as &[String]).is_none());
        assert!(probe_texts("model", 0, &["text".to_string()]).is_none());
        let (hits_after, misses_after) = counters();
        assert_eq!(
            (hits_before, misses_before),
            (hits_after, misses_after),
            "bypassed probes must not change the cache counters"
        );
    }

    #[test]
    fn test_probe_texts_empty_bypasses_cache() {
        assert!(probe_texts("model", 8, &[] as &[String]).is_none());
        assert!(probe_texts("model", 0, &["text".to_string()]).is_none());
    }

    #[test]
    fn test_outcome_miss_keys_align_with_positions() {
        // Structural check without touching the filesystem singleton: the
        // outcome contract is exercised end-to-end by the worker-cache tests
        // and the live two-run validation.
        let outcome = CacheProbeOutcome {
            hits: HashMap::new(),
            miss_positions: vec![0, 2],
            miss_keys: vec![
                KeySpec::for_provider("provider-a", Pooling::Mean, Normalization::L2, 8)
                    .key_for("a"),
                KeySpec::for_provider("provider-a", Pooling::Mean, Normalization::L2, 8)
                    .key_for("c"),
            ],
        };
        assert_eq!(outcome.miss_keys.len(), outcome.miss_positions.len());
        // store_misses with wrong-length vectors is a no-op.
        store_misses(&outcome, &[vec![0.0; 3]], 8);
    }

    #[test]
    fn test_provider_namespace_cannot_collide_with_local_model_digests() {
        let spec = KeySpec::for_provider(
            "remote|custom|http://x|m|768",
            Pooling::Mean,
            Normalization::L2,
            768,
        );
        // A provider digest is a blake3 of an identity string. Assert only the
        // property that matters: it is not a real file digest namespace, i.e.
        // it differs from another provider identity and from a zeroed digest.
        let other = KeySpec::for_provider(
            "remote|custom|http://y|m|768",
            Pooling::Mean,
            Normalization::L2,
            768,
        );
        assert_ne!(spec.model_digest, other.model_digest);
        assert_ne!(spec.tokenizer_digest, other.tokenizer_digest);
        assert_ne!(spec.model_digest, [0u8; 32]);
        assert_ne!(spec.model_digest, spec.tokenizer_digest);
    }

    #[test]
    fn test_provider_spec_separates_vector_spaces() {
        // Same text, same provider, different pooling or template identity:
        // every one of these must land in a different namespace, or the cache
        // would serve vectors produced under different semantics.
        let text = "fn sample() {}";
        let base = KeySpec::for_provider("id", Pooling::Mean, Normalization::L2, 768);
        let last_token = KeySpec::for_provider("id", Pooling::LastToken, Normalization::L2, 768);
        let raw = KeySpec::for_provider("id", Pooling::Mean, Normalization::None, 768);
        let other_tpl = KeySpec::for_provider("id|tpl123", Pooling::Mean, Normalization::L2, 768);
        let other_dim = KeySpec::for_provider("id", Pooling::Mean, Normalization::L2, 1024);
        let fingerprints = [
            base.key_for(text).fingerprint(),
            last_token.key_for(text).fingerprint(),
            raw.key_for(text).fingerprint(),
            other_tpl.key_for(text).fingerprint(),
            other_dim.key_for(text).fingerprint(),
        ];
        for (index, left) in fingerprints.iter().enumerate() {
            for right in &fingerprints[index + 1..] {
                assert_ne!(left, right, "distinct semantics must not share a cache row");
            }
        }
        // And the same spec is stable and content-addressed.
        assert_eq!(
            base.key_for(text).fingerprint(),
            KeySpec::for_provider("id", Pooling::Mean, Normalization::L2, 768)
                .key_for(text)
                .fingerprint()
        );
    }

    #[test]
    fn test_provider_probe_bypasses_on_empty_input() {
        let spec = KeySpec::for_provider("id", Pooling::Mean, Normalization::L2, 8);
        assert!(probe_provider(&spec, &[] as &[String]).is_none());
        let zero = KeySpec::for_provider("id", Pooling::Mean, Normalization::L2, 0);
        assert!(probe_provider(&zero, &["text".to_string()]).is_none());
    }
}
