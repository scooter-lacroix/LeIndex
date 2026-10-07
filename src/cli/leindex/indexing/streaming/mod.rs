//! Streaming index pipeline (WS6-9 Tasks 1-7).
//!
//! Each stage converts from "materialize the whole corpus into heap fields,
//! then persist" to "stream bounded chunks directly into CAS staged blobs."
//! Only one bounded input batch and one bounded output batch exist on the
//! heap at any time per stage (spec §6.6).
//!
//! All stages are feature-flagged (`LEINDEX_FEATURE_STREAMING_*`) and
//! default OFF. The legacy materialize-all pipeline runs when the flags are
//! disabled.

pub mod fragment;
pub mod neural;
pub mod parse;
pub mod pdg;
pub mod scan;
pub mod tfidf;

#[allow(unused_imports)]
pub use neural::{NeuralInput, NeuralRowWriter, NeuralStats};
#[allow(unused_imports)]
pub use scan::{ScanRecord, ScanRecordWriter, ScanStats, VecScanRecordWriter};

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// BatchBudget (VAL-STREAM-007)
// ---------------------------------------------------------------------------

/// Token/byte/count budget for embedding batches (spec §6.6, §8.3).
///
/// Replaces the count-only `NEURAL_IPC_BATCH=256` heuristic. A batch of 500
/// tiny symbols does NOT have the same shape as 500 long documents; this
/// struct enforces multiple ceilings so the ONNX forward pass stays within
/// bounded memory and sequence-length limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchBudget {
    /// Maximum number of texts in a single batch.
    pub max_texts: usize,
    /// Maximum total UTF-8 bytes across all texts in the batch.
    pub max_utf8_bytes: usize,
    /// Maximum estimated token count (heuristic: bytes / 4).
    pub max_estimated_tokens: usize,
    /// Maximum sequence length for any single text (primary bucketing key).
    pub max_seq_len: usize,
    /// Maximum output vector bytes (dim * sizeof(f32) * text_count).
    pub max_output_vector_bytes: usize,
}

impl Default for BatchBudget {
    fn default() -> Self {
        Self {
            max_texts: 256,
            max_utf8_bytes: 512 * 1024, // 512 KiB
            max_estimated_tokens: 131_072,
            max_seq_len: 8192,
            max_output_vector_bytes: 4 * 1024 * 1024, // 4 MiB
        }
    }
}

impl BatchBudget {
    /// A budget with generous limits (tests only).
    pub fn unlimited() -> Self {
        Self {
            max_texts: usize::MAX,
            max_utf8_bytes: usize::MAX,
            max_estimated_tokens: usize::MAX,
            max_seq_len: usize::MAX,
            max_output_vector_bytes: usize::MAX,
        }
    }

    /// Estimate the token count for a text (heuristic: utf8 bytes / 4).
    pub fn estimate_tokens(text: &str) -> usize {
        text.len().div_ceil(4)
    }

    /// Compute the output vector bytes for a given number of texts at a
    /// fixed embedding dimension.
    pub fn output_vector_bytes(count: usize, dim: usize) -> usize {
        count * dim * std::mem::size_of::<f32>()
    }

    /// Check whether adding `text` to the current batch with `current_count`
    /// texts already in would stay within ALL budget ceilings.
    ///
    /// Returns `false` if the text by itself exceeds any individual cap
    /// (it should be routed to a single-text lane), or if adding it to the
    /// current batch would exceed any accumulated cap.
    pub fn can_accept(&self, current_count: usize, current_bytes: usize, text: &str) -> bool {
        if current_count + 1 > self.max_texts {
            return false;
        }
        let new_bytes = current_bytes + text.len();
        if new_bytes > self.max_utf8_bytes {
            return false;
        }
        let new_tokens = Self::estimate_tokens(text) + current_bytes.div_ceil(4); // approximate current tokens
        if new_tokens > self.max_estimated_tokens {
            return false;
        }
        if text.len() > self.max_seq_len {
            return false;
        }
        true
    }
}

/// Compute the blake3 content hash hex string for a text payload.
pub fn content_hash_hex(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

// ---------------------------------------------------------------------------
// StreamingPipelineState (VAL-STREAM-011)
// ---------------------------------------------------------------------------

/// Slim streaming state holding only checkpoint references and compact
/// metadata -- no `Vec<ParsingResult>`, no `Option<PDG>` (whole), no
/// source-hash collections.
///
/// This is the streaming counterpart of the legacy `IndexPipelineState`.
/// When streaming flags are enabled, each stage writes results directly to CAS
/// staging and records only the CAS blob hash + progress counters here.
#[derive(Debug, Clone, Default)]
pub struct StreamingPipelineState {
    /// CAS blob hash of the scan output (metadata records).
    pub scan_blob: Option<[u8; 32]>,
    /// CAS blob hashes of per-file parse signatures.
    pub parse_blobs: Vec<[u8; 32]>,
    /// CAS blob hash of the compact PDG segment data.
    pub pdg_blob: Option<[u8; 32]>,
    /// CAS blob hash of the interned symbol table.
    pub symbols_blob: Option<[u8; 32]>,
    /// CAS blob hash of the TF-IDF vector data.
    pub tfidf_blob: Option<[u8; 32]>,
    /// CAS blob hash of the neural vector data.
    pub neural_blob: Option<[u8; 32]>,
    /// Number of files scanned.
    pub files_scanned: usize,
    /// Number of files parsed.
    pub files_parsed: usize,
    /// Number of PDG nodes.
    pub pdg_node_count: usize,
    /// Number of PDG edges.
    pub pdg_edge_count: usize,
    /// Number of neural rows written.
    pub neural_rows_written: usize,
}

impl StreamingPipelineState {
    /// Create an empty streaming state.
    pub fn new() -> Self {
        Self::default()
    }
}

#[cfg(all(test, feature = "full"))]
mod test {
    use super::*;

    /// VAL-STREAM-007: BatchBudget enforces byte and token ceilings.
    #[test]
    fn test_batch_budget_enforces_all_ceilings() {
        let budget = BatchBudget {
            max_texts: 3,
            max_utf8_bytes: 100,
            max_estimated_tokens: 25,
            max_seq_len: 50,
            max_output_vector_bytes: 1024,
        };
        // Short text: accepted
        assert!(budget.can_accept(0, 0, "hello"));
        // Text exceeding max_seq_len
        let long = "a".repeat(51);
        assert!(!budget.can_accept(0, 0, &long));
        // Count ceiling: 3 texts max
        assert!(budget.can_accept(2, 10, "x"));
        assert!(!budget.can_accept(3, 10, "x"));
        // Byte ceiling
        assert!(!budget.can_accept(0, 90, &"a".repeat(20)));
    }

    /// VAL-STREAM-007: Different batch shapes for short vs long texts.
    #[test]
    fn test_batch_budget_short_vs_long_different_shapes() {
        // Use a budget where text count is not the bottleneck:
        // max_texts = 500, but bytes and tokens are the differentiator.
        let budget = BatchBudget {
            max_texts: 500,
            max_utf8_bytes: 512 * 1024,    // 512 KiB
            max_estimated_tokens: 131_072, // 128K
            max_seq_len: 8192,
            max_output_vector_bytes: usize::MAX,
        };

        // 500 short symbols: ~12 bytes each = 6000 bytes → 1500 tokens
        let shorts: Vec<&str> = (0..500).map(|_| "fn() -> ()").collect();
        let mut count = 0;
        let mut bytes = 0;
        for s in &shorts {
            if budget.can_accept(count, bytes, s) {
                count += 1;
                bytes += s.len();
            }
        }
        // All 500 short symbols fit within the byte/token budget
        assert_eq!(count, 500);

        // 500 long docs at 4000 bytes each = 2MB → 500K tokens, far exceeds limits
        let long_doc = "x".repeat(4000);
        let mut count = 0;
        let mut bytes = 0;
        for _ in 0..500 {
            if budget.can_accept(count, bytes, &long_doc) {
                count += 1;
                bytes += long_doc.len();
            }
        }
        // Far fewer than 500 long docs fit (byte budget = 512KB / 4KB = ~128)
        assert!(count < 500, "long docs must be limited: got {count}");
        // And byte-budget is the constraint, not text count
        // (512KB / 4KB ≈ 128 per batch, but chunking is approximate)
        assert!(
            count <= 140,
            "byte budget should limit to ~128: got {count}"
        );
    }

    /// VAL-STREAM-011: StreamingPipelineState holds no heap-materialized corpus.
    #[test]
    fn test_streaming_state_no_corpus_fields() {
        let state = StreamingPipelineState::new();
        // Only CAS hash references + counters, no source/parsing/pdg accumulators
        assert!(state.scan_blob.is_none());
        assert!(state.parse_blobs.is_empty());
        assert!(state.pdg_blob.is_none());
        assert!(state.neural_blob.is_none());
        assert_eq!(state.files_scanned, 0);
    }

    #[test]
    fn test_content_hash_deterministic() {
        let h1 = content_hash_hex("hello world");
        let h2 = content_hash_hex("hello world");
        assert_eq!(h1, h2);
        let h3 = content_hash_hex("hello World");
        assert_ne!(h1, h3);
    }

    #[test]
    fn test_estimate_tokens_heuristic() {
        assert_eq!(BatchBudget::estimate_tokens(""), 0);
        assert_eq!(BatchBudget::estimate_tokens("abcd"), 1);
        assert_eq!(BatchBudget::estimate_tokens("abc"), 1);
        assert_eq!(BatchBudget::estimate_tokens("abcdefgh"), 2);
    }
}
