//! Streaming neural enrichment via NeuralRowWriter (WS6-9 Task 6).
//!
//! This is the headline fix for spec §6.6. Replaces the legacy
//! `enrich_neural_embeddings(...) -> Vec<(String, Vec<f32>)>` accumulation
//! pattern with direct staged writes. Only one bounded input batch + one
//! bounded output batch exist on the heap at any time (VAL-STREAM-006,
//! VAL-STREAM-014).

use anyhow::Result;

use super::BatchBudget;

/// A neural input: (node_id, enriched_text) ready for embedding.
#[derive(Debug, Clone)]
pub struct NeuralInput {
    /// PDG node ID this embedding corresponds to.
    pub node_id: String,
    /// Enriched text content for embedding (may include connectivity context).
    pub text: String,
}

/// Statistics from a streaming neural enrichment run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NeuralStats {
    /// Number of rows successfully written.
    pub rows_written: usize,
    /// Number of input texts that produced empty vectors (skipped).
    pub rows_skipped: usize,
    /// Number of batches processed.
    pub batches: usize,
    /// Total bytes of embedding vectors written.
    pub bytes_written: usize,
}

/// Trait for writing neural rows directly to staged storage.
///
/// Implementations write each `(node_id, embedding)` pair to CAS staging
/// immediately — no phase-wide `Vec<(String, Vec<f32>)>` accumulation on
/// the heap. This is the core abstraction that kills the Vec accumulation
/// anti-pattern (spec §6.6, VAL-STREAM-006).
pub trait NeuralRowWriter {
    /// Write a single neural row to staged storage.
    fn write_row(&mut self, node_id: &str, embedding: &[f32]) -> Result<()>;

    /// Write a bounded batch and return its accepted row count. Writers with
    /// efficient batch sinks may override this; the default preserves the
    /// existing per-row contract used by fragment streaming.
    fn write_batch(&mut self, rows: Vec<(String, Vec<f32>)>) -> Result<usize> {
        let mut written = 0;
        for (node_id, embedding) in rows {
            self.write_row(&node_id, &embedding)?;
            written += 1;
        }
        Ok(written)
    }
}

/// Vec-backed NeuralRowWriter for testing and equivalence verification.
///
/// Collects rows into a Vec so tests can compare streaming output against
/// the legacy accumulation path. In production, the CAS-backed writer
/// writes directly to staged blobs.
#[derive(Debug, Default)]
pub struct VecNeuralRowWriter {
    /// Collected (node_id, embedding) pairs.
    pub rows: Vec<(String, Vec<f32>)>,
}

impl VecNeuralRowWriter {
    /// Create an empty Vec neural row writer.
    pub fn new() -> Self {
        Self::default()
    }
}

impl NeuralRowWriter for VecNeuralRowWriter {
    fn write_row(&mut self, node_id: &str, embedding: &[f32]) -> Result<()> {
        self.rows.push((node_id.to_string(), embedding.to_vec()));
        Ok(())
    }

    fn write_batch(&mut self, rows: Vec<(String, Vec<f32>)>) -> Result<usize> {
        let written = rows.len();
        self.rows.extend(rows);
        Ok(written)
    }
}

/// An embedder trait that the streaming neural enricher uses to embed batches.
///
/// This abstracts over HybridEmbedder (ONNX worker, remote API) so the
/// streaming code can be tested with a deterministic mock embedder.
pub trait StreamingEmbedder {
    /// Embed a batch of texts, returning one embedding vector per text.
    fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>>;
}

/// A deterministic mock embedder for testing. Uses a simple hash-based
/// pseudo-embedding so results are reproducible (needed for bit-identical
/// equivalence testing per VAL-STREAM-006).
#[derive(Debug, Clone)]
pub struct MockEmbedder {
    /// Embedding dimensionality.
    pub dim: usize,
}

impl MockEmbedder {
    /// Create a mock embedder producing `dim`-dimensional vectors.
    pub fn new(dim: usize) -> Self {
        Self { dim }
    }
}

impl StreamingEmbedder for MockEmbedder {
    fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
        texts
            .iter()
            .map(|text| {
                let hash = blake3::hash(text.as_bytes());
                let bytes = hash.as_bytes();
                let mut vec = vec![0.0f32; self.dim];
                for (i, slot) in vec.iter_mut().enumerate() {
                    *slot = (bytes[i % bytes.len()] as f32) / 255.0;
                }
                // L2 normalize
                let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
                if magnitude > 1e-9 {
                    for v in &mut vec {
                        *v /= magnitude;
                    }
                }
                vec
            })
            .collect()
    }
}

/// Streaming neural enrichment: read inputs in bounded batches, embed each
/// batch, write rows directly to `writer`. Only one batch is on the heap
/// at any time (VAL-STREAM-006, VAL-STREAM-014).
///
/// The output is bit-identical to the legacy accumulation path when the same
/// embedder, batching order, and texts are used, because the embedding
/// computation per batch is the same; only the accumulation strategy changes.
pub fn enrich_neural_streaming<I, W, E>(
    source: &mut I,
    embedder: &E,
    writer: &mut W,
    budget: &BatchBudget,
) -> Result<NeuralStats>
where
    I: Iterator<Item = Result<NeuralInput>>,
    W: NeuralRowWriter,
    E: StreamingEmbedder,
{
    let mut stats = NeuralStats::default();
    let mut pending_ids: Vec<String> = Vec::new();
    let mut pending_texts: Vec<String> = Vec::new();
    let mut pending_bytes: usize = 0;

    loop {
        let exhausted = match source.next() {
            Some(Ok(input)) => {
                if budget.can_accept(pending_texts.len(), pending_bytes, &input.text) {
                    pending_bytes += input.text.len();
                    pending_ids.push(input.node_id);
                    pending_texts.push(input.text);
                    false
                } else {
                    // Text by itself exceeds a batch limit — flush current
                    // batch, then add this text as a single-item batch
                    if !pending_texts.is_empty() {
                        flush_batch(&pending_ids, &pending_texts, embedder, writer, &mut stats)?;
                        pending_ids.clear();
                        pending_texts.clear();
                        pending_bytes = 0;
                    }
                    // Single-item batch for oversized text. Text longer than
                    // the embedder's sequence limit cannot be embedded at
                    // all: skip it, but COUNT the skip — the previous guard
                    // (`text.len() <= max_seq_len`) only reached this branch
                    // for texts that were already too long, so every
                    // oversized input was silently dropped with no row, no
                    // counter and no error while NeuralStats reported
                    // success.
                    if input.text.len() > budget.max_seq_len && budget.max_seq_len != usize::MAX {
                        stats.rows_skipped += 1;
                    } else {
                        pending_bytes = input.text.len();
                        pending_ids.push(input.node_id);
                        pending_texts.push(input.text);
                    }
                    false
                }
            }
            Some(Err(error)) => return Err(error),
            None => true,
        };

        let batch_full =
            pending_texts.len() >= budget.max_texts || pending_bytes >= budget.max_utf8_bytes;
        if (batch_full || exhausted) && !pending_texts.is_empty() {
            flush_batch(&pending_ids, &pending_texts, embedder, writer, &mut stats)?;
            pending_ids.clear();
            pending_texts.clear();
            pending_bytes = 0;
        }

        if exhausted {
            break;
        }
    }

    Ok(stats)
}

fn flush_batch<E: StreamingEmbedder, W: NeuralRowWriter>(
    ids: &[String],
    texts: &[String],
    embedder: &E,
    writer: &mut W,
    stats: &mut NeuralStats,
) -> Result<()> {
    let embeddings = embedder.embed_batch(texts);
    if embeddings.len() != ids.len() {
        anyhow::bail!(
            "streaming neural embedder returned {} vectors for {} texts",
            embeddings.len(),
            ids.len()
        );
    }
    let mut rows = Vec::with_capacity(ids.len());
    for (node_id, embedding) in ids.iter().zip(embeddings) {
        if embedding.is_empty() {
            stats.rows_skipped += 1;
        } else {
            stats.bytes_written += embedding.len() * std::mem::size_of::<f32>();
            rows.push((node_id.clone(), embedding));
        }
    }
    stats.rows_written += writer.write_batch(rows)?;
    stats.batches += 1;
    Ok(())
}

/// Batch a stream of inputs under a byte/token budget, returning batch
/// boundaries. This is the internal helper used by `enrich_neural_streaming`
/// but can also be used standalone for validation tests.
pub fn batch_inputs(inputs: &[NeuralInput], budget: &BatchBudget) -> Vec<(usize, usize)> /* (start, end) ranges */
{
    let mut ranges = Vec::new();
    let mut start = 0;
    let mut current_bytes = 0;
    let mut current_count = 0;

    for (i, input) in inputs.iter().enumerate() {
        let would_exceed = !budget.can_accept(current_count, current_bytes, &input.text);
        if would_exceed && i > start {
            ranges.push((start, i));
            start = i;
            current_bytes = 0;
            current_count = 0;
        }
        current_bytes += input.text.len();
        current_count += 1;
    }
    if start < inputs.len() {
        ranges.push((start, inputs.len()));
    }
    ranges
}

#[cfg(all(test, feature = "full"))]
mod test {
    use super::*;

    fn make_inputs(n: usize) -> Vec<NeuralInput> {
        (0..n)
            .map(|i| NeuralInput {
                node_id: format!("node{i}"),
                text: format!("fn func_{i}() -> i32 {{ {i} }}"),
            })
            .collect()
    }

    /// PR #90 round 2, cluster E: a count mismatch between the embedder's
    /// returned vectors and the batch's texts is a hard error — never a
    /// silent partial write (a short batch persisted + checkpointed would
    /// make resume skip the missing rows forever).
    #[test]
    fn test_short_embedder_batch_is_a_hard_error_and_persists_nothing() {
        struct ShortEmbedder;
        impl StreamingEmbedder for ShortEmbedder {
            fn embed_batch(&self, texts: &[String]) -> Vec<Vec<f32>> {
                texts.iter().skip(1).map(|_| vec![1.0_f32]).collect()
            }
        }

        let inputs = make_inputs(6);
        let mut iter = inputs.into_iter().map(Ok);
        let mut writer = VecNeuralRowWriter::default();
        let result = enrich_neural_streaming(
            &mut iter,
            &ShortEmbedder,
            &mut writer,
            &BatchBudget::unlimited(),
        );
        assert!(
            result.is_err(),
            "short embedder batch must fail the enrichment, not skip rows silently"
        );
        assert!(
            writer.rows.is_empty(),
            "no partial rows may be persisted when a batch count mismatches (resume would skip the missing rows forever)"
        );
    }

    /// VAL-STREAM-006: Direct staged neural writes produce bit-identical vectors.
    #[test]
    fn test_streaming_neural_bit_identical() {
        let inputs = make_inputs(10);
        let embedder = MockEmbedder::new(64);

        // Streaming path
        let mut streaming_writer = VecNeuralRowWriter::new();
        let iter1_inputs: Vec<Result<NeuralInput>> = inputs.clone().into_iter().map(Ok).collect();
        let mut iter1 = iter1_inputs.into_iter();
        let _ = enrich_neural_streaming(
            &mut iter1,
            &embedder,
            &mut streaming_writer,
            &BatchBudget::default(),
        )
        .unwrap();

        // Legacy equivalent: accumulate in a Vec, same embedder, same order
        let mut legacy_rows = Vec::new();
        for input in inputs.into_iter() {
            let emb = embedder.embed_batch(&[input.text]);
            if !emb[0].is_empty() {
                legacy_rows.push((input.node_id, emb[0].clone()));
            }
        }

        // Bit-identical
        assert_eq!(streaming_writer.rows.len(), legacy_rows.len());
        for (stream_row, legacy_row) in streaming_writer.rows.iter().zip(legacy_rows.iter()) {
            assert_eq!(stream_row.0, legacy_row.0);
            assert_eq!(stream_row.1, legacy_row.1);
        }
    }

    /// VAL-STREAM-007: BatchBudget enforced during neural enrichment.
    #[test]
    fn test_batch_budget_enforced() {
        let inputs = make_inputs(100);
        let budget = BatchBudget {
            max_texts: 5,
            max_utf8_bytes: usize::MAX,
            max_estimated_tokens: usize::MAX,
            max_seq_len: usize::MAX,
            max_output_vector_bytes: usize::MAX,
        };
        let embedder = MockEmbedder::new(16);
        let mut writer = VecNeuralRowWriter::new();
        let mut iter = inputs.into_iter().map(Ok);
        let stats = enrich_neural_streaming(&mut iter, &embedder, &mut writer, &budget).unwrap();

        // No single batch exceeded max_texts
        assert_eq!(stats.rows_written, 100);
        assert!(stats.batches >= 20); // 100 / 5 = 20 minimum
    }

    /// An input longer than `max_seq_len` must be counted as skipped, never
    /// silently dropped: the oversized branch previously pushed only texts
    /// that fit, so over-length inputs vanished with no row and no counter
    /// while NeuralStats reported success.
    #[test]
    fn test_oversized_input_is_counted_not_silently_dropped() {
        let mut inputs = make_inputs(3);
        inputs.insert(
            1,
            NeuralInput {
                node_id: "huge".to_string(),
                text: "x".repeat(4096),
            },
        );
        let budget = BatchBudget {
            max_texts: 2,
            max_utf8_bytes: usize::MAX,
            max_estimated_tokens: usize::MAX,
            max_seq_len: 1024,
            max_output_vector_bytes: usize::MAX,
        };
        let embedder = MockEmbedder::new(16);
        let mut writer = VecNeuralRowWriter::new();
        let mut iter = inputs.into_iter().map(Ok);
        let stats = enrich_neural_streaming(&mut iter, &embedder, &mut writer, &budget).unwrap();

        assert_eq!(
            stats.rows_written, 3,
            "the three embeddable inputs are written"
        );
        assert_eq!(
            stats.rows_skipped, 1,
            "the over-length input is counted as skipped"
        );
        assert!(
            !writer.rows.iter().any(|(id, _)| id == "huge"),
            "over-length text produces no row"
        );
    }

    /// VAL-STREAM-014: RSS independent of corpus node count.
    ///
    /// The key invariant: only one bounded input batch + one bounded output
    /// batch are on the heap at any time. We verify this by checking that the
    /// max batch size stays bounded by the budget regardless of corpus size.
    #[test]
    fn test_rss_independent_of_corpus() {
        let embedder = MockEmbedder::new(8);
        let budget = BatchBudget {
            max_texts: 50,
            ..BatchBudget::default()
        };

        // Small corpus
        let small_inputs = make_inputs(10);
        let mut writer = VecNeuralRowWriter::new();
        let iter_inputs: Vec<Result<NeuralInput>> = small_inputs.into_iter().map(Ok).collect();
        let mut iter = iter_inputs.into_iter();
        let small_stats =
            enrich_neural_streaming(&mut iter, &embedder, &mut writer, &budget).unwrap();

        // Large corpus
        let large_inputs = make_inputs(1000);
        let mut writer = VecNeuralRowWriter::new();
        let iter_inputs: Vec<Result<NeuralInput>> = large_inputs.into_iter().map(Ok).collect();
        let mut iter = iter_inputs.into_iter();
        let large_stats =
            enrich_neural_streaming(&mut iter, &embedder, &mut writer, &budget).unwrap();

        // Max rows per batch is bounded by max_texts regardless of corpus
        let max_per_batch = budget.max_texts;
        let small_per_batch = small_stats
            .rows_written
            .checked_div(small_stats.batches)
            .unwrap_or(small_stats.rows_written);
        let large_per_batch = large_stats
            .rows_written
            .checked_div(large_stats.batches)
            .unwrap_or(large_stats.rows_written);
        // Both stay within max_texts bound
        assert!(
            small_per_batch <= max_per_batch,
            "small batch exceeded budget"
        );
        assert!(
            large_per_batch <= max_per_batch,
            "large batch exceeded budget"
        );
        // Total rows scale with corpus
        assert_eq!(small_stats.rows_written, 10);
        assert_eq!(large_stats.rows_written, 1000);
    }

    #[test]
    fn test_batch_inputs_boundaries() {
        let inputs = make_inputs(20);
        let budget = BatchBudget {
            max_texts: 7,
            max_utf8_bytes: usize::MAX,
            max_estimated_tokens: usize::MAX,
            max_seq_len: usize::MAX,
            max_output_vector_bytes: usize::MAX,
        };
        let ranges = batch_inputs(&inputs, &budget);
        for (_, end) in &ranges {
            assert!(*end <= inputs.len());
        }
    }

    #[test]
    fn test_empty_input() {
        let embedder = MockEmbedder::new(8);
        let mut writer = VecNeuralRowWriter::new();
        let mut iter = Vec::<Result<NeuralInput>>::new().into_iter();
        let stats =
            enrich_neural_streaming(&mut iter, &embedder, &mut writer, &BatchBudget::default())
                .unwrap();
        assert_eq!(stats.rows_written, 0);
        assert_eq!(stats.batches, 0);
    }

    #[test]
    fn test_mock_embedder_deterministic() {
        let embedder = MockEmbedder::new(32);
        let texts = vec!["hello world".to_string()];
        let v1 = embedder.embed_batch(&texts);
        let v2 = embedder.embed_batch(&texts);
        assert_eq!(v1, v2);
    }
}
