// Batch splitting and oversized input handling
//
// VAL-CPHASE-014: Requests exceeding the configured main-side outgoing buffer
// are split into multiple frames keyed by batch identity and re-stitched
// correctly on return.
//
// VAL-CPHASE-015: A single overlarge text is truncated/chunked before IPC
// framing rather than overflowing transport.

use crate::protocol::{BatchId, EmbedRequest, EmbedResponse};

/// Configuration for batch splitting behavior.
#[derive(Debug, Clone)]
pub struct BatchConfig {
    /// Maximum frame size in bytes for outgoing IPC frames.
    pub max_frame_size: usize,
    /// Maximum size in bytes for a single text before truncation.
    pub max_text_size: usize,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_frame_size: 16 * 1024 * 1024, // 16 MiB
            max_text_size: 1024 * 1024,       // 1 MiB
        }
    }
}

/// A sub-batch produced by splitting an oversized request.
#[derive(Debug, Clone)]
pub struct SubBatch {
    /// The batch ID from the original request.
    pub batch_id: BatchId,
    /// Sub-batch index (0-based).
    pub index: usize,
    /// Total number of sub-batches.
    pub total: usize,
    /// The texts in this sub-batch.
    pub request: EmbedRequest,
}

/// Result of splitting an oversized request.
#[derive(Debug)]
pub enum SplitResult {
    /// The request fits in a single frame.
    Single(EmbedRequest),
    /// The request was split into multiple sub-batches.
    Split(Vec<SubBatch>),
}

/// Result of re-stitching split responses.
#[derive(Debug)]
pub struct StitchedResponse {
    /// The combined flat row-major embedding buffer.
    pub vectors: Vec<f32>,
    /// Total number of embeddings.
    pub count: usize,
    /// Dimension of each embedding.
    pub dimension: usize,
}

/// Split an embed request into sub-batches if it exceeds the frame size limit.
///
/// VAL-CPHASE-014: Oversized batches are split into multiple frames keyed
/// by batch identity and re-stitched correctly on return.
pub fn split_request(
    batch_id: BatchId,
    request: EmbedRequest,
    config: &BatchConfig,
) -> SplitResult {
    // First, truncate any oversized individual texts
    let texts: Vec<String> = request
        .texts
        .into_iter()
        .map(|t| truncate_text(t, config.max_text_size))
        .collect();

    if texts.is_empty() {
        return SplitResult::Single(EmbedRequest {
            texts: vec![],
            expected_dim: request.expected_dim,
        });
    }

    // Estimate the serialized size of the full request
    let estimated_size = estimate_request_size(&texts);

    if estimated_size <= config.max_frame_size {
        return SplitResult::Single(EmbedRequest {
            texts,
            expected_dim: request.expected_dim,
        });
    }

    // Split into sub-batches that each fit within the frame size
    let mut sub_batches: Vec<Vec<String>> = Vec::new();
    let mut current_texts = Vec::new();
    let mut current_size = 0usize;

    for text in texts {
        let text_size = text.len() + 16; // overhead estimate per text

        if !current_texts.is_empty() && current_size + text_size > config.max_frame_size {
            // Flush current sub-batch
            sub_batches.push(std::mem::take(&mut current_texts));
            current_size = 0;
        }

        // Guard: if a single text (after truncation to max_text_size) still
        // exceeds max_frame_size, truncate it further to fit. This can happen
        // when max_text_size + overhead > max_frame_size due to misconfiguration.
        let text = if current_texts.is_empty() && text_size > config.max_frame_size {
            let max_content = config.max_frame_size.saturating_sub(16); // subtract overhead
            tracing::warn!(
                original_len = text.len(),
                truncated_to = max_content,
                "single text exceeds max_frame_size, truncating further"
            );
            truncate_text(text, max_content)
        } else {
            text
        };

        // Recalculate text_size to match the actual (possibly truncated) text
        let text_size = text.len() + 16;

        current_texts.push(text);
        current_size += text_size;
    }

    // Flush remaining
    if !current_texts.is_empty() {
        sub_batches.push(current_texts);
    }

    if sub_batches.len() <= 1 {
        let texts = sub_batches.into_iter().next().unwrap_or_default();
        return SplitResult::Single(EmbedRequest {
            texts,
            expected_dim: request.expected_dim,
        });
    }

    let total = sub_batches.len();
    let sub_batches: Vec<SubBatch> = sub_batches
        .into_iter()
        .enumerate()
        .map(|(index, texts)| SubBatch {
            batch_id,
            index,
            total,
            request: EmbedRequest {
                texts,
                expected_dim: request.expected_dim,
            },
        })
        .collect();

    SplitResult::Split(sub_batches)
}

/// Re-stitch multiple sub-batch responses into a single response.
///
/// VAL-CPHASE-014: Split responses are re-stitched correctly, preserving
/// the original batch ordering.
pub fn stitch_responses(responses: Vec<EmbedResponse>) -> StitchedResponse {
    if responses.is_empty() {
        return StitchedResponse {
            vectors: vec![],
            count: 0,
            dimension: 0,
        };
    }

    let dimension = responses[0].dimension;
    let total_elements: usize = responses.iter().map(|r| r.vectors.len()).sum();
    let mut all_vectors = Vec::with_capacity(total_elements);
    let mut total_count = 0;

    for response in responses {
        if response.dimension != dimension {
            tracing::error!(
                expected = dimension,
                actual = response.dimension,
                "dimension mismatch during response stitching"
            );
            continue;
        }
        all_vectors.extend(response.vectors);
        total_count += response.count;
    }

    StitchedResponse {
        vectors: all_vectors,
        count: total_count,
        dimension,
    }
}

/// Truncate a single text to the maximum allowed size.
///
/// VAL-CPHASE-015: A single overlarge text is truncated before IPC framing
/// rather than overflowing transport.
pub fn truncate_text(text: String, max_size: usize) -> String {
    if text.len() <= max_size {
        return text;
    }

    // Truncate at a character boundary
    let mut end = max_size;
    while !text.is_char_boundary(end) && end > 0 {
        end -= 1;
    }

    tracing::warn!(
        original_len = text.len(),
        truncated_len = end,
        "truncated oversized text before IPC framing"
    );

    text[..end].to_string()
}

/// Per-text serialization overhead (length prefix, type tag, etc.)
const PER_TEXT_SERIALIZE_OVERHEAD: usize = 16;
/// Fixed overhead for the frame header and message envelope
const FRAME_HEADER_OVERHEAD: usize = 128;

/// Estimate the serialized size of an embed request.
fn estimate_request_size(texts: &[String]) -> usize {
    // Rough estimate: each text contributes its byte length plus some overhead
    // for the serialization format (length prefixes, type tags, etc.)
    let text_bytes: usize = texts.iter().map(|t| t.len()).sum();
    let overhead = texts.len() * PER_TEXT_SERIALIZE_OVERHEAD + FRAME_HEADER_OVERHEAD;
    text_bytes + overhead
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_request_small_batch() {
        let config = BatchConfig::default();
        let request = EmbedRequest {
            texts: vec!["hello".to_string(), "world".to_string()],
            expected_dim: 4,
        };

        let result = split_request(BatchId::new(1), request, &config);
        match result {
            SplitResult::Single(req) => {
                assert_eq!(req.texts.len(), 2);
                assert_eq!(req.expected_dim, 4);
            }
            SplitResult::Split(_) => panic!("small batch should not be split"),
        }
    }

    #[test]
    fn test_split_request_empty_batch() {
        let config = BatchConfig::default();
        let request = EmbedRequest {
            texts: vec![],
            expected_dim: 4,
        };

        let result = split_request(BatchId::new(1), request, &config);
        match result {
            SplitResult::Single(req) => {
                assert!(req.texts.is_empty());
            }
            SplitResult::Split(_) => panic!("empty batch should not be split"),
        }
    }

    #[test]
    fn test_split_request_oversized_batch() {
        let config = BatchConfig {
            max_frame_size: 200, // Very small to force splitting
            max_text_size: 1024,
        };

        let texts: Vec<String> = (0..20)
            .map(|i| format!("text number {} with some padding", i))
            .collect();
        let request = EmbedRequest {
            texts,
            expected_dim: 4,
        };

        let result = split_request(BatchId::new(1), request, &config);
        match result {
            SplitResult::Single(_) => {
                // Might fit if texts are small enough
            }
            SplitResult::Split(sub_batches) => {
                assert!(sub_batches.len() > 1);
                // All sub-batches should have the same batch ID
                for sb in &sub_batches {
                    assert_eq!(sb.batch_id, BatchId::new(1));
                }
                // Total texts should equal original count
                let total_texts: usize = sub_batches.iter().map(|sb| sb.request.texts.len()).sum();
                assert_eq!(total_texts, 20);
            }
        }
    }

    #[test]
    fn test_split_request_preserves_batch_id() {
        let config = BatchConfig {
            max_frame_size: 100,
            max_text_size: 1024,
        };

        let texts: Vec<String> = (0..10)
            .map(|i| format!("a somewhat longer text number {} here", i))
            .collect();
        let request = EmbedRequest {
            texts,
            expected_dim: 8,
        };

        let batch_id = BatchId::new(42);
        let result = split_request(batch_id, request, &config);

        if let SplitResult::Split(sub_batches) = result {
            for sb in &sub_batches {
                assert_eq!(sb.batch_id, batch_id);
                assert_eq!(sb.request.expected_dim, 8);
            }
        }
    }

    #[test]
    fn test_stitch_responses_empty() {
        let result = stitch_responses(vec![]);
        assert_eq!(result.count, 0);
        assert!(result.vectors.is_empty());
    }

    #[test]
    fn test_stitch_responses_single() {
        let response = EmbedResponse::new(vec![1.0, 2.0, 3.0, 4.0], 1, 4);
        let result = stitch_responses(vec![response]);
        assert_eq!(result.count, 1);
        assert_eq!(result.dimension, 4);
        assert_eq!(result.vectors, vec![1.0, 2.0, 3.0, 4.0]);
    }

    #[test]
    fn test_stitch_responses_multiple() {
        let r1 = EmbedResponse::new(vec![1.0, 2.0, 3.0, 4.0], 1, 4);
        let r2 = EmbedResponse::new(vec![5.0, 6.0, 7.0, 8.0], 1, 4);
        let r3 = EmbedResponse::new(vec![9.0, 10.0, 11.0, 12.0], 1, 4);

        let result = stitch_responses(vec![r1, r2, r3]);
        assert_eq!(result.count, 3);
        assert_eq!(result.dimension, 4);
        assert_eq!(
            result.vectors,
            vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0, 11.0, 12.0]
        );
    }

    #[test]
    fn test_stitch_preserves_ordering() {
        // VAL-CPHASE-013/014: ordering preserved through split and stitch
        let r1 = EmbedResponse::new(vec![1.0, 2.0], 1, 2);
        let r2 = EmbedResponse::new(vec![3.0, 4.0], 1, 2);
        let r3 = EmbedResponse::new(vec![5.0, 6.0], 1, 2);

        let result = stitch_responses(vec![r1, r2, r3]);
        assert_eq!(result.vectors, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
    }

    #[test]
    fn test_truncate_text_within_limit() {
        let result = truncate_text("hello".to_string(), 100);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_text_exceeds_limit() {
        let result = truncate_text("hello world".to_string(), 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_truncate_text_unicode_boundary() {
        let result = truncate_text("héllo wörld".to_string(), 4);
        // "h" is 1 byte, "é" is 2 bytes — so byte 4 is mid-character
        // Should truncate to byte 3 ("hé")
        assert!(result.len() <= 4);
        assert!(result.is_char_boundary(result.len()));
    }

    #[test]
    fn test_truncate_text_exact_boundary() {
        let result = truncate_text("hello".to_string(), 5);
        assert_eq!(result, "hello");
    }

    #[test]
    fn test_estimate_request_size() {
        let texts = vec!["hello".to_string(), "world".to_string()];
        let size = estimate_request_size(&texts);
        assert!(size > 0);
        assert!(size < 1000); // Reasonable for small texts
    }

    #[test]
    fn test_batch_config_default() {
        let config = BatchConfig::default();
        assert_eq!(config.max_frame_size, 16 * 1024 * 1024);
        assert_eq!(config.max_text_size, 1024 * 1024);
    }

    #[test]
    fn test_split_and_stitch_roundtrip() {
        // End-to-end: split a large request, create stub responses, stitch
        let config = BatchConfig {
            max_frame_size: 200,
            max_text_size: 1024,
        };

        let texts: Vec<String> = (0..20)
            .map(|i| format!("text number {} with enough content to matter", i))
            .collect();
        let dim = 4;
        let request = EmbedRequest {
            texts: texts.clone(),
            expected_dim: dim,
        };

        let batch_id = BatchId::new(99);
        let split = split_request(batch_id, request, &config);

        match split {
            SplitResult::Single(req) => {
                // If it fit in one batch, verify all texts are present
                assert_eq!(req.texts.len(), texts.len());
            }
            SplitResult::Split(sub_batches) => {
                // Create stub responses for each sub-batch
                let responses: Vec<EmbedResponse> = sub_batches
                    .iter()
                    .map(|sb| {
                        let count = sb.request.texts.len();
                        EmbedResponse::new(vec![0.0f32; count * dim], count, dim)
                    })
                    .collect();

                // Stitch them back together
                let stitched = stitch_responses(responses);

                // Verify total count matches original text count
                assert_eq!(stitched.count, texts.len());
                assert_eq!(stitched.dimension, dim);
                assert_eq!(stitched.vectors.len(), texts.len() * dim);
            }
        }
    }
}
