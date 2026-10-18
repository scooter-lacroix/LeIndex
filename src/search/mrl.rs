//! Matryoshka (MRL) slicing policy for EmbeddingGemma2.
//!
//! EmbeddingGemma2 is trained with Matryoshka Representation Learning: any
//! leading-dimension prefix of the 768-dim output stays usable after
//! re-normalization (google/embeddinggemma-2 model card, "Matryoshka
//! Dimension Truncation"). Slicing shrinks the persisted vector store and
//! speeds similarity search at a small quality cost; the card reports
//! near-lossless quality down to 256 dims and substantial degradation at 128.
//!
//! Policy: the slice dimension is a pure function of the node's source span
//! (`NodeInfo::byte_range`), which is persisted in the search snapshot — so
//! the same tier is computed at enrich time, at persist time, and after
//! checkpoint resume without extra state. Short spans are small, dense
//! symbols (getters, consts) that retrieve fine at 256 dims; page-scale
//! spans and synthetic whole-file summaries (`(0,0)`) keep full fidelity.
//!
//! The engine keeps uniform full-dimension rows in memory; slicing is
//! applied when partitioning rows into per-dimension mmap files at publish
//! time, and mirrored on the query side by scoring each store against the
//! correspondingly re-normalized query slice.

use crate::config::MatryoshkaSetting;

/// Slice dimensions supported by the EmbeddingGemma2 MRL training, ascending.
pub const MRL_TIERS: [usize; 4] = [128, 256, 512, 768];

/// Source spans below this many bytes land in the smallest tier (256): short
/// dense symbols whose retrieval signal survives aggressive slicing.
pub const SMALL_SYMBOL_SPAN_BYTES: usize = 512;

/// Source spans below this many bytes land in the middle tier (512); larger
/// spans keep the full 768-dim vector.
pub const MEDIUM_SYMBOL_SPAN_BYTES: usize = 4096;

/// Slice `embedding` to its leading `dim` components and re-normalize to unit
/// length.
///
/// The model card is explicit that slicing a unit-length vector does not
/// preserve unit length: the shortened vector must be L2-normalized again
/// before cosine scoring, or ranking quality degrades silently. Zero-norm
/// inputs (all-zero slice) return the zero vector rather than NaN.
pub fn slice_l2_normalized(embedding: &[f32], dim: usize) -> Vec<f32> {
    let dim = dim.min(embedding.len());
    let slice = &embedding[..dim];
    let magnitude = slice.iter().map(|v| v * v).sum::<f32>().sqrt();
    if magnitude < 1e-9 {
        return vec![0.0; dim];
    }
    slice.iter().map(|v| v / magnitude).collect()
}

/// Dynamic tier for a node's source span under the `Dynamic` setting.
///
/// `(0, 0)` spans are the synthetic per-file summary nodes (see
/// `NodeType::FileSummary`); they represent the whole file's purpose and
/// always keep full fidelity.
pub fn dynamic_tier_for_span(span_bytes: usize) -> usize {
    if span_bytes == 0 {
        768
    } else if span_bytes < SMALL_SYMBOL_SPAN_BYTES {
        256
    } else if span_bytes < MEDIUM_SYMBOL_SPAN_BYTES {
        512
    } else {
        768
    }
}

/// Slice dimension for a document under `setting` (`None` = no slicing).
///
/// `Off` never slices; fixed settings slice everything to their dimension;
/// `Dynamic` resolves per document from the span. Fragment embeddings are
/// deliberately excluded from dynamic slicing by their callers — the
/// fragment mmap's dimension is validated against the native dimension at
/// snapshot install, and fragment layers stay full-fidelity in v1.
pub fn slice_dimension_for(setting: MatryoshkaSetting, span_bytes: usize) -> Option<usize> {
    match setting {
        MatryoshkaSetting::Off => None,
        MatryoshkaSetting::Dynamic => Some(dynamic_tier_for_span(span_bytes)),
        fixed => fixed.fixed_slice(),
    }
}

/// Partition `(node_id, embedding)` rows into per-dimension buckets under
/// `setting`.
///
/// Returns rows grouped by their slice dimension, each row already sliced and
/// re-normalized. Rows whose node id has no span entry are treated as
/// unknown-span and keep the full dimension (safe default: full fidelity).
/// Under `Off` (or an empty input) the single bucket holds the rows
/// untouched, so callers can treat "no slicing" as the degenerate case.
pub fn partition_rows_by_slice_dimension(
    rows: &[(String, Vec<f32>)],
    span_bytes: &std::collections::HashMap<String, usize>,
    setting: MatryoshkaSetting,
) -> Vec<(usize, Vec<(String, Vec<f32>)>)> {
    let mut buckets: Vec<(usize, Vec<(String, Vec<f32>)>)> = Vec::new();
    if rows.is_empty() || setting == MatryoshkaSetting::Off {
        if !rows.is_empty() {
            buckets.push((MRL_TIERS[3], rows.to_vec()));
        }
        return buckets;
    }
    for (node_id, embedding) in rows {
        let span = span_bytes.get(node_id).copied().unwrap_or(usize::MAX);
        let dim = slice_dimension_for(setting, span).unwrap_or(MRL_TIERS[3]);
        match buckets.iter_mut().find(|(d, _)| *d == dim) {
            Some((_, bucket)) => {
                bucket.push((node_id.clone(), slice_l2_normalized(embedding, dim)))
            }
            None => buckets.push((
                dim,
                vec![(node_id.clone(), slice_l2_normalized(embedding, dim))],
            )),
        }
    }
    buckets.sort_by_key(|(dim, _)| *dim);
    buckets
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn unit_vector(dim: usize, peak: usize) -> Vec<f32> {
        let mut v = vec![0.1f32; dim];
        v[peak % dim] = 1.0;
        let m = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        v.iter().map(|x| x / m).collect()
    }

    #[test]
    fn test_sliced_vector_is_unit_length() {
        let v = unit_vector(768, 3);
        let s = slice_l2_normalized(&v, 256);
        assert_eq!(s.len(), 256);
        let m: f32 = s.iter().map(|x| x * x).sum();
        assert!((m - 1.0).abs() < 1e-5, "magnitude {m}");
        // Leading components must be preserved up to the renormalization.
        let scale = s[3] / v[3];
        for (a, b) in s.iter().zip(v.iter()) {
            assert!((a - b * scale).abs() < 1e-6);
        }
    }

    #[test]
    fn test_slice_dim_clamped_to_input() {
        let v = vec![0.5f32; 8];
        let s = slice_l2_normalized(&v, 256);
        assert_eq!(s.len(), 8);
        let m: f32 = s.iter().map(|x| x * x).sum();
        assert!((m - 1.0).abs() < 1e-5);
    }

    #[test]
    fn test_zero_slice_stays_zero_not_nan() {
        let v = vec![0.0f32; 768];
        let s = slice_l2_normalized(&v, 256);
        assert!(s.iter().all(|x| *x == 0.0));
    }

    #[test]
    fn test_span_tiers_follow_thresholds() {
        assert_eq!(dynamic_tier_for_span(0), 768);
        assert_eq!(dynamic_tier_for_span(1), 256);
        assert_eq!(dynamic_tier_for_span(SMALL_SYMBOL_SPAN_BYTES - 1), 256);
        assert_eq!(dynamic_tier_for_span(SMALL_SYMBOL_SPAN_BYTES), 512);
        assert_eq!(dynamic_tier_for_span(MEDIUM_SYMBOL_SPAN_BYTES - 1), 512);
        assert_eq!(dynamic_tier_for_span(MEDIUM_SYMBOL_SPAN_BYTES), 768);
        assert_eq!(dynamic_tier_for_span(usize::MAX), 768);
    }

    #[test]
    fn test_slice_dimension_matrix() {
        assert_eq!(slice_dimension_for(MatryoshkaSetting::Off, 0), None);
        assert_eq!(
            slice_dimension_for(MatryoshkaSetting::Dynamic, 100),
            Some(256)
        );
        assert_eq!(
            slice_dimension_for(MatryoshkaSetting::Dynamic, 0),
            Some(768)
        );
        assert_eq!(
            slice_dimension_for(MatryoshkaSetting::Dim512, 100),
            Some(512)
        );
    }

    #[test]
    fn test_off_partition_is_identity_single_bucket() {
        let rows = vec![("a".to_string(), vec![0.5f32; 768])];
        let buckets =
            partition_rows_by_slice_dimension(&rows, &HashMap::new(), MatryoshkaSetting::Off);
        assert_eq!(buckets.len(), 1);
        assert_eq!(buckets[0].0, 768);
        assert_eq!(buckets[0].1, rows);
        assert!(
            partition_rows_by_slice_dimension(&[], &HashMap::new(), MatryoshkaSetting::Dynamic)
                .is_empty()
        );
    }

    #[test]
    fn test_dynamic_partition_groups_by_tier_and_slices() {
        let rows = vec![
            ("small".to_string(), unit_vector(768, 1)),
            ("big".to_string(), unit_vector(768, 2)),
            ("unknown".to_string(), unit_vector(768, 3)),
        ];
        let spans = HashMap::from([
            ("small".to_string(), 100usize),
            ("big".to_string(), 10_000usize),
        ]);
        let buckets = partition_rows_by_slice_dimension(&rows, &spans, MatryoshkaSetting::Dynamic);
        assert_eq!(buckets.len(), 2);
        assert_eq!(buckets[0].0, 256);
        assert_eq!(buckets[0].1.len(), 1);
        assert_eq!(buckets[0].1[0].0, "small");
        assert_eq!(buckets[0].1[0].1.len(), 256);
        assert_eq!(buckets[1].0, 768);
        // Unknown-span row fell back to full fidelity.
        assert_eq!(buckets[1].1.len(), 2);
        assert!(
            buckets[1]
                .1
                .iter()
                .any(|(id, v)| id == "unknown" && v.len() == 768)
        );
    }
}
