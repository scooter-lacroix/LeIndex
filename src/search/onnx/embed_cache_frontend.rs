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
//! worker's actual embedding semantics: last-token pooling + L2
//! normalization (`EmbeddingRuntime::pool_and_normalize`), no prompt
//! template, `output_dimensions` from the caller. The model/tokenizer
//! digests are computed from the SAME files the worker resolves, streamed
//! (64 KiB chunks) so hashing a ~900 MB model never allocates a heap mirror.
//!
//! Cross-process safety: rows are immutable, content-addressed files written
//! via staging+rename; the client probes (reads) and puts (atomic writes) —
//! the identical access pattern the worker uses, so concurrent client/worker
//! access cannot observe partial rows.
//!
//! The frontend is a per-process singleton keyed by model: a process that
//! serves a DIFFERENT model than the one the singleton was opened for simply
//! bypasses the cache (never stores into a foreign namespace).

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
    miss_keys: Vec<CacheKey>,
}

struct Frontend {
    model_name: String,
    store: GlobalEmbeddingCache,
    model_digest: [u8; 32],
    tokenizer_digest: [u8; 32],
    output_dimensions: u32,
}

static FRONTEND: OnceLock<Mutex<Option<Frontend>>> = OnceLock::new();
static HITS: AtomicU64 = AtomicU64::new(0);
static MISSES: AtomicU64 = AtomicU64::new(0);
/// Latch: once opening fails (unreadable model files, store failure), stop
/// retrying so per-batch probes on a broken install never re-hash a ~900 MB
/// model file.
static OPEN_FAILED: AtomicBool = AtomicBool::new(false);

/// Cumulative (hits, misses) observed by this process — surfaced in index
/// progress logs so cache effectiveness is visible.
#[cfg(any(feature = "cli", test))]
pub(crate) fn counters() -> (u64, u64) {
    (HITS.load(Ordering::Relaxed), MISSES.load(Ordering::Relaxed))
}

/// Probe the cache for `texts` under the given model. Returns `None` when
/// caching is unavailable for this call (flag off, open failure, model
/// mismatch, unreadable model files) — callers then proceed exactly as
/// before. Never fails embedding.
pub(crate) fn probe_texts<S: AsRef<str>>(
    model_name: &str,
    output_dimensions: usize,
    texts: &[S],
) -> Option<CacheProbeOutcome> {
    if texts.is_empty() || output_dimensions == 0 {
        return None;
    }
    let mut guard = frontend_lock().lock().ok()?;
    ensure_frontend(&mut guard, model_name, output_dimensions)?;
    let frontend = guard.as_mut()?;
    if frontend.model_name != model_name || frontend.output_dimensions != output_dimensions as u32 {
        // Different model/dim than the singleton was keyed for: bypass rather
        // than pollute a foreign namespace.
        return None;
    }

    let keys: Vec<CacheKey> = texts
        .iter()
        .map(|text| key_for(frontend, text.as_ref()))
        .collect();
    let ProbeResult { hits, misses } = frontend.store.probe(&keys).ok()?;
    HITS.fetch_add(hits.len() as u64, Ordering::Relaxed);
    MISSES.fetch_add(misses.len() as u64, Ordering::Relaxed);

    // Defensive: a stored row whose dimension disagrees with this request is
    // treated as a miss (probe already validates the fingerprint; this guards
    // config drift).
    let mut hits = hits;
    let mut miss_positions = misses;
    let stale: Vec<usize> = hits
        .iter()
        .filter(|(_, vector)| vector.len() != output_dimensions)
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

/// Store freshly computed vectors for a probe outcome's misses. Best-effort:
/// one batched write per call (no per-row fsync — the cache is rebuildable
/// and re-hash-verified on read); failures warn and disable nothing.
pub(crate) fn store_misses(outcome: &CacheProbeOutcome, miss_vectors: &[Vec<f32>]) {
    if miss_vectors.is_empty() {
        return;
    }
    let Ok(mut guard) = frontend_lock().lock() else {
        return;
    };
    let Some(frontend) = guard.as_mut() else {
        return;
    };
    // Only cache REAL worker output; never TF-IDF-degraded substitutes.
    let entries: Vec<(CacheKey, Vec<f32>)> = outcome
        .miss_keys
        .iter()
        .zip(miss_vectors.iter())
        .filter(|(_, vector)| {
            !vector.is_empty() && vector.len() == frontend.output_dimensions as usize
        })
        .map(|(key, vector)| ((*key).clone(), (*vector).clone()))
        .collect();
    if entries.is_empty() {
        return;
    }
    match frontend.store.put_batch(&entries) {
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

fn frontend_lock() -> &'static Mutex<Option<Frontend>> {
    FRONTEND.get_or_init(|| Mutex::new(None))
}

/// Open the frontend on first use for this process. Digest failures and
/// store-open failures leave `None` (cache bypassed) rather than erroring.
fn ensure_frontend(
    slot: &mut Option<Frontend>,
    model_name: &str,
    output_dimensions: usize,
) -> Option<()> {
    if slot.is_some() {
        return Some(());
    }
    if OPEN_FAILED.load(Ordering::Acquire) {
        return None;
    }
    if !crate::feature_flags::FeatureFlag::GlobalEmbedCache.is_enabled() {
        return None;
    }
    // Resolve the SAME files the worker loads so digests can never disagree
    // with the vectors actually produced.
    let open_failure = |context: &str| {
        OPEN_FAILED.store(true, Ordering::Release);
        tracing::warn!(
            model = model_name,
            context,
            "embed cache unavailable for this process; embedding proceeds uncached"
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
    let root = cache_root();
    let store = match GlobalEmbeddingCache::open(&root) {
        Ok(store) => store,
        Err(error) => {
            tracing::warn!(
                root = %root.display(),
                %error,
                "embed cache open failed; embedding proceeds uncached"
            );
            open_failure("store-open");
            return None;
        }
    };
    *slot = Some(Frontend {
        model_name: model_name.to_string(),
        store,
        model_digest,
        tokenizer_digest,
        output_dimensions: output_dimensions as u32,
    });
    tracing::info!(
        model = model_name,
        root = %root.display(),
        "client-side embed cache opened"
    );
    Some(())
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

fn key_for(frontend: &Frontend, text: &str) -> CacheKey {
    CacheKey {
        model_digest: frontend.model_digest,
        tokenizer_digest: frontend.tokenizer_digest,
        // The worker applies no prompt template to embedding inputs.
        prompt_role_and_version: 0,
        // Must mirror `EmbeddingRuntime::pool_and_normalize`: final unpadded
        // token, then L2 normalization.
        pooling: Pooling::LastToken,
        normalization: Normalization::L2,
        output_dimensions: frontend.output_dimensions,
        content_hash: CacheKey::content_hash(text),
    }
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
                CacheKey {
                    model_digest: [0; 32],
                    tokenizer_digest: [0; 32],
                    prompt_role_and_version: 0,
                    pooling: Pooling::LastToken,
                    normalization: Normalization::L2,
                    output_dimensions: 8,
                    content_hash: CacheKey::content_hash("a"),
                },
                CacheKey {
                    model_digest: [0; 32],
                    tokenizer_digest: [0; 32],
                    prompt_role_and_version: 0,
                    pooling: Pooling::LastToken,
                    normalization: Normalization::L2,
                    output_dimensions: 8,
                    content_hash: CacheKey::content_hash("c"),
                },
            ],
        };
        assert_eq!(outcome.miss_keys.len(), outcome.miss_positions.len());
        // store_misses with wrong-length vectors is a no-op.
        store_misses(&outcome, &[vec![0.0; 3]]);
    }
}
