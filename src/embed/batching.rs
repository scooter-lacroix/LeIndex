//! BatchBudget — token/byte/count budget for embedding batches (WS10 Task 3).
//!
//! Replaces the count-only `NEURAL_IPC_BATCH=256` heuristic. As spec §6.6
//! states: "500 tiny symbols and 500 long docs have radically different
//! shapes." [`BatchBudget`] enforces multiple ceilings so the ONNX forward
//! pass stays within bounded memory and sequence-length limits, and
//! [`bucket_texts`] groups similar-length texts into batches to avoid
//! pathological padding overhead (spec §8.3).

use serde::{Deserialize, Serialize};

/// Token/byte/count budget for embedding batches (spec §6.6, §8.3).
///
/// The budget caps each batch on five dimensions:
/// - Maximum text count
/// - Maximum total UTF-8 bytes
/// - Maximum estimated token count
/// - Maximum sequence length for any single text
/// - Maximum output vector bytes (`dim * sizeof(f32) * count`)
///
/// [`BatchBudget::bucket`] groups similar-length texts into separate batches
/// so that the ONNX forward pass does not waste compute on padding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchBudget {
    /// Maximum number of texts in a single batch.
    pub max_texts: usize,
    /// Maximum total UTF-8 bytes across all texts in the batch.
    pub max_utf8_bytes: usize,
    /// Maximum estimated token count across all texts (heuristic: bytes / 4).
    pub max_estimated_tokens: usize,
    /// Maximum sequence length (in bytes) for any single text. This is the
    /// primary bucketing key: texts longer than this get their own batch.
    pub max_seq_len: usize,
    /// Maximum output vector bytes (dim * sizeof(f32) * count).
    pub max_output_vector_bytes: usize,
}

impl Default for BatchBudget {
    fn default() -> Self {
        Self {
            max_texts: 256,
            max_utf8_bytes: 512 * 1024,    // 512 KiB
            max_estimated_tokens: 131_072, // 128K
            max_seq_len: 8192,
            max_output_vector_bytes: 4 * 1024 * 1024, // 4 MiB
        }
    }
}

/// A single batch produced by [`BatchBudget::bucket`].
#[derive(Debug, Clone)]
pub struct Batch {
    /// Indices of the texts in this batch (into the original input).
    pub indices: Vec<usize>,
}

impl Batch {
    /// Number of texts in this batch.
    pub fn len(&self) -> usize {
        self.indices.len()
    }

    /// Whether this batch is empty.
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }
}

impl BatchBudget {
    /// A budget with generous limits (tests only).
    #[cfg(test)]
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
    /// texts and `current_bytes` already in would stay within ALL budget
    /// ceilings.
    ///
    /// Returns `false` if the text by itself exceeds any individual cap, or
    /// if adding it to the current batch would exceed any accumulated cap.
    pub fn can_accept(&self, current_count: usize, current_bytes: usize, text: &str) -> bool {
        if current_count + 1 > self.max_texts {
            return false;
        }
        let new_bytes = current_bytes + text.len();
        if new_bytes > self.max_utf8_bytes {
            return false;
        }
        let current_tokens = current_bytes.div_ceil(4);
        let new_tokens = current_tokens + Self::estimate_tokens(text);
        if new_tokens > self.max_estimated_tokens {
            return false;
        }
        if text.len() > self.max_seq_len {
            return false;
        }
        true
    }

    /// Check whether adding a text of `text_bytes` size would keep the output
    /// vector bytes within budget, given `current_count` texts already at
    /// `dim` dimensions.
    pub fn can_accept_vector_bytes(&self, current_count: usize, dim: usize) -> bool {
        let new_bytes = Self::output_vector_bytes(current_count + 1, dim);
        new_bytes <= self.max_output_vector_bytes
    }

    /// Group texts into batches that respect the budget ceilings and group
    /// similar-length texts together to minimize padding waste (spec §8.3).
    ///
    /// The algorithm:
    /// 1. Sort text indices by byte length (ascending).
    /// 2. Greedily fill batches until adding the next text would exceed any
    ///    budget cap OR would break the length-similarity invariant.
    /// 3. Start a new batch.
    ///
    /// The length-similarity invariant: within a batch, the ratio of the
    /// longest text to the shortest text is at most 3x. This avoids mixing
    /// 12-byte function names with 500-byte documents in the same ONNX
    /// forward pass (which would pad all short texts to 500 tokens).
    ///
    /// `dim` is the embedding dimension, used for the output vector byte cap.
    pub fn bucket(&self, texts: &[&str], dim: usize) -> Vec<Batch> {
        if texts.is_empty() {
            return Vec::new();
        }

        // Sort indices by text length (ascending). This groups similar-length
        // texts so padding waste is minimized.
        let mut sorted: Vec<usize> = (0..texts.len()).collect();
        sorted.sort_by_key(|&i| texts[i].len());

        let mut batches = Vec::new();
        let mut current_batch: Vec<usize> = Vec::new();
        let mut current_bytes = 0usize;
        let mut batch_min_len: usize = 0;
        let mut batch_max_len: usize = 0;

        for &idx in &sorted {
            let text = texts[idx];

            let can_accept_count = self.can_accept(current_batch.len(), current_bytes, text);
            let can_accept_vec = self.can_accept_vector_bytes(current_batch.len(), dim);

            // Length-similarity: the new text must not break the 3x ratio
            // between batch_min_len and the new text length (or vice versa).
            let length_compatible = if current_batch.is_empty() {
                true
            } else {
                let new_len = text.len();
                let new_min = batch_min_len.min(new_len);
                let new_max = batch_max_len.max(new_len);
                // Allow 0-length texts to coexist; otherwise enforce 3x ratio.
                // Use a minimum floor of 32 bytes to avoid excessive fragmentation
                // when many tiny texts are present.
                let floor = new_min.max(32);
                new_max <= floor * 3
            };

            if !current_batch.is_empty()
                && (!can_accept_count || !can_accept_vec || !length_compatible)
            {
                // Flush current batch.
                batches.push(Batch {
                    indices: std::mem::take(&mut current_batch),
                });
                current_bytes = 0;
                // Reset min/max — will be set below when the text is added.
            }

            if current_batch.is_empty() {
                batch_min_len = text.len();
                batch_max_len = text.len();
            }

            current_batch.push(idx);
            current_bytes += text.len();
            batch_min_len = batch_min_len.min(text.len());
            batch_max_len = batch_max_len.max(text.len());
        }

        if !current_batch.is_empty() {
            batches.push(Batch {
                indices: current_batch,
            });
        }

        batches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VAL-CACHE-006: BatchBudget caps texts, bytes, tokens, seq len, output vector bytes.
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

    /// VAL-CACHE-006: Different batch shapes for short vs long texts.
    #[test]
    fn test_bucket_separates_short_and_long_texts() {
        // Use a budget where text count is not the bottleneck but byte
        // and token caps force separation.
        let budget = BatchBudget {
            max_texts: 500,
            max_utf8_bytes: 1024,
            max_estimated_tokens: 256,
            max_seq_len: 512,
            max_output_vector_bytes: usize::MAX,
        };

        let short_text = "fn()";
        let long_doc = &"a".repeat(500);

        let texts = vec![short_text, short_text, long_doc, short_text, short_text];
        let batches = budget.bucket(&texts, 4);

        // The long document and the short texts should NOT be in the same
        // batch (they have radically different shapes).
        let long_batch_idx = batches.iter().position(|b| b.indices.contains(&2)).unwrap();
        let long_batch = &batches[long_batch_idx];
        // The batch containing the long doc should not contain the short texts.
        assert!(long_batch.indices.contains(&2));
        // The batch may contain only the long doc, or the long doc with
        // similar-length texts, but not with the short texts.
        for &idx in &long_batch.indices {
            if idx != 2 {
                // Other texts in this batch should be of similar length
                assert!(
                    texts[idx].len() > 100,
                    "long batch should not contain very short texts"
                );
            }
        }
    }

    /// VAL-CACHE-006: 500 tiny symbols do NOT have the same shape as 500 long docs.
    #[test]
    fn test_500_tiny_vs_500_long_different_shapes() {
        let budget = BatchBudget {
            max_texts: 500,
            max_utf8_bytes: 512 * 1024,
            max_estimated_tokens: 131_072,
            max_seq_len: 8192,
            max_output_vector_bytes: usize::MAX,
        };

        // 500 tiny symbols: ~12 bytes each
        let tiny_texts: Vec<&str> = (0..500).map(|_| "fn() -> ()").collect();
        let tiny_batches = budget.bucket(&tiny_texts, 4);
        assert_eq!(
            tiny_batches.len(),
            1,
            "500 tiny texts should fit in one batch"
        );

        // 500 long documents: 2KiB each
        let long_texts: Vec<String> = (0..500)
            .map(|i| format!("doc {i} {}", "x".repeat(2000)))
            .collect();
        let long_refs: Vec<&str> = long_texts.iter().map(|s| s.as_str()).collect();
        let long_batches = budget.bucket(&long_refs, 4);
        assert!(
            long_batches.len() > 1,
            "500 long docs should need multiple batches"
        );
    }

    /// Bucketing groups similar-length texts.
    #[test]
    fn test_bucket_groups_similar_lengths() {
        let budget = BatchBudget::default();

        // Mix of short, medium, and long texts.
        let shorts: Vec<String> = (0..10).map(|i| format!("fn_{i}()")).collect();
        let mediums: Vec<String> = (0..10)
            .map(|i| format!("fn function_{}() {{ /* {} */ }}", i, "x".repeat(100)))
            .collect();
        let longs: Vec<String> = (0..5)
            .map(|i| format!("fn l{}_long() {{ /* {} */ }}", i, "y".repeat(500)))
            .collect();

        let mut all: Vec<String> = Vec::new();
        all.extend(shorts.iter().cloned());
        all.extend(mediums.iter().cloned());
        all.extend(longs.iter().cloned());

        let refs: Vec<&str> = all.iter().map(|s| s.as_str()).collect();
        let batches = budget.bucket(&refs, 4);

        // Each batch should be internally homogeneous: the ratio of longest
        // to shortest text (with a 32-byte floor) should be at most 3x.
        for batch in &batches {
            if batch.len() <= 1 {
                continue;
            }
            let lengths: Vec<usize> = batch.indices.iter().map(|&i| all[i].len()).collect();
            let min_len = *lengths.iter().min().unwrap();
            let max_len = *lengths.iter().max().unwrap();
            let floor = min_len.max(32);
            assert!(
                max_len <= floor * 3,
                "batch has texts of lengths [{min_len}..{max_len}], too spread for bucketing"
            );
        }
    }

    #[test]
    fn test_empty_input() {
        let budget = BatchBudget::default();
        let batches = budget.bucket(&[], 4);
        assert!(batches.is_empty());
    }

    #[test]
    fn test_single_text() {
        let budget = BatchBudget::default();
        let batches = budget.bucket(&["hello"], 4);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 1);
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(BatchBudget::estimate_tokens("abcd"), 1);
        assert_eq!(BatchBudget::estimate_tokens("abc"), 1);
        assert_eq!(BatchBudget::estimate_tokens("abcde"), 2);
        assert_eq!(BatchBudget::estimate_tokens(""), 0);
    }

    #[test]
    fn test_output_vector_bytes() {
        // 10 texts at 1024 dim
        let bytes = BatchBudget::output_vector_bytes(10, 1024);
        assert_eq!(bytes, 10 * 1024 * 4);
    }

    #[test]
    fn test_can_accept_vector_bytes() {
        let budget = BatchBudget {
            max_output_vector_bytes: 100,
            max_texts: usize::MAX,
            max_utf8_bytes: usize::MAX,
            max_estimated_tokens: usize::MAX,
            max_seq_len: usize::MAX,
        };
        // dim=4, so each vector is 16 bytes. 6 texts = 96 bytes (ok). 7 = 112 (over).
        assert!(budget.can_accept_vector_bytes(5, 4));
        assert!(!budget.can_accept_vector_bytes(6, 4));
    }

    #[test]
    fn test_all_input_in_single_batch_when_budget_allows() {
        let budget = BatchBudget::default();
        let texts: Vec<&str> = (0..10).map(|_| "short").collect();
        let batches = budget.bucket(&texts, 4);
        assert_eq!(batches.len(), 1);
        assert_eq!(batches[0].len(), 10);
    }

    #[test]
    fn test_bucket_conservation_no_lost_texts() {
        let budget = BatchBudget {
            max_texts: 3,
            max_utf8_bytes: 50,
            max_estimated_tokens: 12,
            max_seq_len: 20,
            max_output_vector_bytes: usize::MAX,
        };
        let texts: Vec<&str> = vec!["a", "bb", "ccc", "dddd", "eeeee", "f"];
        let batches = budget.bucket(&texts, 4);

        // Every input text must appear in exactly one batch.
        let mut all_indices: Vec<usize> = batches
            .iter()
            .flat_map(|b| b.indices.iter().copied())
            .collect();
        all_indices.sort_unstable();
        let expected: Vec<usize> = (0..texts.len()).collect();
        // Note: texts exceeding max_seq_len are still bucketed (they get their
        // own batch) — the bucketing algorithm does not reject them.
        assert_eq!(
            all_indices, expected,
            "all input indices must be present exactly once"
        );
    }
}
