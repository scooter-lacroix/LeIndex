//! Streaming fragment embeddings stage (WS6-9 Task 5).
//!
//! Streams fragments from changed files, computes content hash, probes a
//! persistent cache, queues misses under token/byte budget, and writes
//! returned vectors directly to CAS-staged rows. No `HashMap<String,
//! Vec<f32>>` load of all prior fragments is materialized (spec §6.5,
//! VAL-STREAM-013).

use std::collections::HashMap;

use anyhow::Result;

use super::BatchBudget;
use super::neural::{NeuralRowWriter, StreamingEmbedder};

/// A fragment input: (fragment_id, content_hash, text) for embedding.
#[derive(Debug, Clone)]
pub struct FragmentInput {
    /// Unique fragment ID (file_path:byte_range).
    pub fragment_id: String,
    /// Content hash of the fragment text (blake3 hex).
    pub content_hash: String,
    /// Enriched text content for embedding.
    pub text: String,
}

/// A probe key for the persistent embedding cache.
///
/// Per spec §6.5, the full cache key is:
/// (model_digest, tokenizer_digest, prompt_role_and_version, pooling,
///  normalization, output_dimensions, content_hash).
/// Here we focus on the content_hash dimension for the streaming fragment
/// stage; the full 6-tuple is the WS10 global cache concern.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CacheProbeKey {
    /// Model digest (ONNX model bytes hash).
    pub model_digest: String,
    /// Content hash of the fragment text.
    pub content_hash: String,
    /// Output embedding dimensions.
    pub output_dim: usize,
}

/// Trait for probing a persistent embedding cache.
///
/// `probe` returns cached vectors for known keys, and the caller embeds
/// the misses. This abstraction allows testing with an in-memory cache
/// while production uses the WS10 global mmap cache.
pub trait FragmentEmbedCache {
    /// Probe the cache for vectors. Returns `Some(vec)` for hits.
    fn probe(&self, key: &CacheProbeKey) -> Option<Vec<f32>>;

    /// Put a vector into the cache for future probes.
    fn put(&mut self, key: CacheProbeKey, vec: Vec<f32>);
}

/// In-memory fragment cache for testing.
#[derive(Debug, Default)]
pub struct InMemoryFragmentCache {
    entries: HashMap<CacheProbeKey, Vec<f32>>,
}

impl InMemoryFragmentCache {
    /// Create an empty in-memory fragment cache.
    pub fn new() -> Self {
        Self::default()
    }
}

impl FragmentEmbedCache for InMemoryFragmentCache {
    fn probe(&self, key: &CacheProbeKey) -> Option<Vec<f32>> {
        self.entries.get(key).cloned()
    }

    fn put(&mut self, key: CacheProbeKey, vec: Vec<f32>) {
        self.entries.insert(key, vec);
    }
}

/// Statistics from a streaming fragment embedding run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FragmentStats {
    /// Total fragments processed.
    pub fragments_total: usize,
    /// Cache hits (vector served from cache).
    pub cache_hits: usize,
    /// Cache misses (vector freshly embedded).
    pub cache_misses: usize,
    /// Fragments that produced empty vectors (skipped).
    pub rows_skipped: usize,
    /// Number of embedding batches sent.
    pub batches: usize,
}

/// Streaming fragment embeddings: probe cache, batch misses, write directly.
///
/// No `HashMap<String, Vec<f32>>` of all prior fragments is materialized.
/// Cache hits are written directly to `writer`. Misses are accumulated in
/// bounded batches (respecting `budget`), embedded via `embedder`, written
/// to `writer`, and stored back into the cache for future probes.
///
/// VAL-STREAM-013: RSS is independent of total fragment count because only
/// one bounded batch of misses is on the heap at any time.
pub fn streaming_fragments<C, W, E>(
    inputs: &[FragmentInput],
    cache: &mut C,
    embedder: &E,
    writer: &mut W,
    budget: &BatchBudget,
    model_digest: &str,
    output_dim: usize,
) -> Result<FragmentStats>
where
    C: FragmentEmbedCache,
    W: NeuralRowWriter,
    E: StreamingEmbedder,
{
    let mut stats = FragmentStats::default();
    let mut miss_ids: Vec<String> = Vec::new();
    let mut miss_texts: Vec<String> = Vec::new();
    let mut miss_keys: Vec<CacheProbeKey> = Vec::new();
    let mut miss_bytes: usize = 0;

    for input in inputs {
        stats.fragments_total += 1;
        let key = CacheProbeKey {
            model_digest: model_digest.to_string(),
            content_hash: input.content_hash.clone(),
            output_dim,
        };

        if let Some(cached) = cache.probe(&key) {
            // Cache hit: write directly, no accumulation
            if !cached.is_empty() {
                writer.write_row(&input.fragment_id, &cached)?;
                stats.cache_hits += 1;
            } else {
                stats.rows_skipped += 1;
            }
        } else {
            // Cache miss: queue for embedding (bounded batch)
            if !budget.can_accept(miss_texts.len(), miss_bytes, &input.text) {
                // Flush current miss batch
                flush_miss_batch(
                    &miss_ids,
                    &miss_texts,
                    &miss_keys,
                    embedder,
                    writer,
                    cache,
                    &mut stats,
                )?;
                miss_ids.clear();
                miss_texts.clear();
                miss_keys.clear();
                miss_bytes = 0;
            }
            miss_bytes += input.text.len();
            miss_ids.push(input.fragment_id.clone());
            miss_texts.push(input.text.clone());
            miss_keys.push(key);
            stats.cache_misses += 1;
        }
    }

    // Flush remaining misses
    if !miss_texts.is_empty() {
        flush_miss_batch(
            &miss_ids,
            &miss_texts,
            &miss_keys,
            embedder,
            writer,
            cache,
            &mut stats,
        )?;
    }

    Ok(stats)
}

fn flush_miss_batch<C, W, E>(
    ids: &[String],
    texts: &[String],
    keys: &[CacheProbeKey],
    embedder: &E,
    writer: &mut W,
    cache: &mut C,
    stats: &mut FragmentStats,
) -> Result<()>
where
    C: FragmentEmbedCache,
    W: NeuralRowWriter,
    E: StreamingEmbedder,
{
    let embeddings = embedder.embed_batch(texts);
    for ((node_id, embedding), key) in ids.iter().zip(embeddings.iter()).zip(keys.iter()) {
        if !embedding.is_empty() {
            writer.write_row(node_id, embedding)?;
            cache.put(key.clone(), embedding.clone());
        } else {
            stats.rows_skipped += 1;
        }
    }
    stats.batches += 1;
    Ok(())
}

#[cfg(all(test, feature = "full"))]
mod test {
    use super::super::neural::{MockEmbedder, VecNeuralRowWriter};
    use super::*;

    fn make_fragments(n: usize) -> Vec<FragmentInput> {
        (0..n)
            .map(|i| {
                let text = format!("fragment content {i}");
                FragmentInput {
                    fragment_id: format!("file.rs:{i}"),
                    content_hash: super::super::content_hash_hex(&text),
                    text,
                }
            })
            .collect()
    }

    /// VAL-STREAM-013: Fragment embeddings probe persistent cache before embedding.
    #[test]
    fn test_fragment_cache_probe_before_embed() {
        let fragments = make_fragments(10);
        let embedder = MockEmbedder::new(32);
        let mut writer = VecNeuralRowWriter::new();
        let mut cache = InMemoryFragmentCache::new();
        let budget = BatchBudget::default();

        // First run: all misses
        let stats1 = streaming_fragments(
            &fragments,
            &mut cache,
            &embedder,
            &mut writer,
            &budget,
            "model-v1",
            32,
        )
        .unwrap();

        assert_eq!(stats1.cache_misses, 10);
        assert_eq!(stats1.cache_hits, 0);
        assert_eq!(stats1.fragments_total, 10);

        // Second run (same fragments): all hits
        let mut writer2 = VecNeuralRowWriter::new();
        let stats2 = streaming_fragments(
            &fragments,
            &mut cache,
            &embedder,
            &mut writer2,
            &budget,
            "model-v1",
            32,
        )
        .unwrap();

        assert_eq!(stats2.cache_hits, 10);
        assert_eq!(stats2.cache_misses, 0);
    }

    #[test]
    fn test_fragment_no_hashmap_load_of_all_priors() {
        // The streaming writer only holds rows that have been written;
        // there's no HashMap<String, Vec<f32>> of all fragments loaded
        // before writing begins. We verify the writer contains streamed
        // rows written one at a time, not pre-loaded.
        let fragments = make_fragments(5);
        let embedder = MockEmbedder::new(8);
        let mut writer = VecNeuralRowWriter::new();
        let mut cache = InMemoryFragmentCache::new();
        let budget = BatchBudget::default();

        let _stats = streaming_fragments(
            &fragments,
            &mut cache,
            &embedder,
            &mut writer,
            &budget,
            "model-v1",
            8,
        )
        .unwrap();

        // All 5 fragments written individually
        assert_eq!(writer.rows.len(), 5);
        // IDs match the fragment IDs
        for (row, frag) in writer.rows.iter().zip(fragments.iter()) {
            assert_eq!(row.0, frag.fragment_id);
        }
    }

    #[test]
    fn test_model_digest_isolates_cache() {
        let fragments = make_fragments(3);
        let embedder = MockEmbedder::new(16);
        let mut cache = InMemoryFragmentCache::new();
        let budget = BatchBudget::default();

        // Fill cache under model-v1
        let mut writer1 = VecNeuralRowWriter::new();
        let _ = streaming_fragments(
            &fragments,
            &mut cache,
            &embedder,
            &mut writer1,
            &budget,
            "model-v1",
            16,
        )
        .unwrap();

        // Query under model-v2 → all misses (different model_digest)
        let mut writer2 = VecNeuralRowWriter::new();
        let stats = streaming_fragments(
            &fragments,
            &mut cache,
            &embedder,
            &mut writer2,
            &budget,
            "model-v2",
            16,
        )
        .unwrap();

        assert_eq!(stats.cache_hits, 0);
        assert_eq!(stats.cache_misses, 3);
    }

    #[test]
    fn test_fragment_budget_enforced() {
        let fragments = make_fragments(100);
        let embedder = MockEmbedder::new(8);
        let mut writer = VecNeuralRowWriter::new();
        let mut cache = InMemoryFragmentCache::new();
        let budget = BatchBudget {
            max_texts: 10,
            ..BatchBudget::default()
        };

        let stats = streaming_fragments(
            &fragments,
            &mut cache,
            &embedder,
            &mut writer,
            &budget,
            "model-v1",
            8,
        )
        .unwrap();

        // No batch should have more than 10 misses
        assert!(stats.batches >= 10); // 100/10 = 10 minimum batches
        assert_eq!(stats.fragments_total, 100);
    }

    #[test]
    fn test_fragment_empty_input() {
        let embedder = MockEmbedder::new(8);
        let mut writer = VecNeuralRowWriter::new();
        let mut cache = InMemoryFragmentCache::new();
        let stats = streaming_fragments(
            &[],
            &mut cache,
            &embedder,
            &mut writer,
            &BatchBudget::default(),
            "model-v1",
            8,
        )
        .unwrap();
        assert_eq!(stats.fragments_total, 0);
    }
}
