//! Evaluation metrics: Recall@k, MRR@10, nDCG@10, per-category, and CIs.
//!
//! These implement the metrics from spec section 9.3. All metric functions
//! operate on ranked results lists that the harness produces, making them
//! independent of the specific retrieval backend.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::corpus::CorpusCase;

// ── Core metric functions ───────────────────────────────────────────────────

/// Compute Recall@k for a single query.
///
/// `retrieved` is the list of retrieved item identifiers (e.g., symbol names
/// or file paths), in rank order. `relevant` is the set of correct items.
///
/// Recall@k = |retrieved\[:k\] ∩ relevant| / |relevant|
///
/// If `relevant` is empty, returns 0.0 (no relevant items to recall).
pub fn recall_at_k(retrieved: &[String], relevant: &[String], k: usize) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let relevant_set: std::collections::HashSet<&String> = relevant.iter().collect();
    let top_k_count = retrieved
        .iter()
        .take(k)
        .filter(|item| relevant_set.contains(*item))
        .count();
    top_k_count as f64 / relevant.len() as f64
}

/// Compute Recall@1 for a single query.
pub fn recall_at_1(retrieved: &[String], relevant: &[String]) -> f64 {
    recall_at_k(retrieved, relevant, 1)
}

/// Compute Recall@5 for a single query.
pub fn recall_at_5(retrieved: &[String], relevant: &[String]) -> f64 {
    recall_at_k(retrieved, relevant, 5)
}

/// Compute Recall@10 for a single query.
pub fn recall_at_10(retrieved: &[String], relevant: &[String]) -> f64 {
    recall_at_k(retrieved, relevant, 10)
}

/// Compute MRR (Mean Reciprocal Rank) for a single query, capped at k.
///
/// Returns 1/rank of the first relevant result within the top-k.
/// If no relevant result is in the top-k, returns 0.0.
///
/// MRR@k = 1 / rank_of_first_relevant (if within k), else 0
pub fn reciprocal_rank_at_k(retrieved: &[String], relevant: &[String], k: usize) -> f64 {
    if relevant.is_empty() || k == 0 {
        return 0.0;
    }
    let relevant_set: std::collections::HashSet<&String> = relevant.iter().collect();
    for (i, item) in retrieved.iter().take(k).enumerate() {
        if relevant_set.contains(item) {
            return 1.0 / (i + 1) as f64;
        }
    }
    0.0
}

/// Compute MRR@10 for a single query.
pub fn reciprocal_rank_at_10(retrieved: &[String], relevant: &[String]) -> f64 {
    reciprocal_rank_at_k(retrieved, relevant, 10)
}

/// Compute nDCG@k (normalized Discounted Cumulative Gain) for a single query.
///
/// Binary relevance: each retrieved item is relevant (1) or not (0).
/// DCG@k = sum over i=1..min(k, len) of rel_i / log2(i + 1)
/// IDCG@k = DCG@k of the ideal ranking (all relevant items first)
/// nDCG@k = DCG@k / IDCG@k
///
/// If there are no relevant items, returns 0.0.
pub fn ndcg_at_k(retrieved: &[String], relevant: &[String], k: usize) -> f64 {
    if relevant.is_empty() || k == 0 {
        return 0.0;
    }
    let relevant_set: std::collections::HashSet<&String> = relevant.iter().collect();

    // Compute DCG@k for the retrieved ranking
    let dcg: f64 = retrieved
        .iter()
        .take(k)
        .enumerate()
        .map(|(i, item)| {
            let rel: f64 = if relevant_set.contains(item) {
                1.0
            } else {
                0.0
            };
            rel / (i as f64 + 2.0).log2()
        })
        .sum();

    // Compute IDCG@k (ideal ranking: all relevant items at the top)
    let ideal_hits = relevant.len().min(k);
    let idcg: f64 = (0..ideal_hits).map(|i| 1.0 / (i as f64 + 2.0).log2()).sum();

    if idcg == 0.0 {
        return 0.0;
    }
    dcg / idcg
}

/// Compute nDCG@10 for a single query.
pub fn ndcg_at_10(retrieved: &[String], relevant: &[String]) -> f64 {
    ndcg_at_k(retrieved, relevant, 10)
}

/// Compute Average Precision (AP) for a single query.
///
/// AP = (1/|relevant|) * sum over k of (Precision@k * rel_k)
///
/// Used for MAP computation.
pub fn average_precision(retrieved: &[String], relevant: &[String]) -> f64 {
    if relevant.is_empty() {
        return 0.0;
    }
    let relevant_set: std::collections::HashSet<&String> = relevant.iter().collect();
    let mut hits = 0;
    let mut sum_precision = 0.0;

    for (i, item) in retrieved.iter().enumerate() {
        if relevant_set.contains(item) {
            hits += 1;
            sum_precision += hits as f64 / (i + 1) as f64;
        }
    }

    sum_precision / relevant.len() as f64
}

// ── Aggregated metrics ──────────────────────────────────────────────────────

/// Metrics for a single evaluation case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseMetrics {
    /// Case ID.
    pub case_id: String,
    /// Category.
    pub category: String,
    /// Recall@1.
    pub recall_1: f64,
    /// Recall@5.
    pub recall_5: f64,
    /// Recall@10.
    pub recall_10: f64,
    /// MRR (reciprocal rank at 10).
    pub mrr_10: f64,
    /// nDCG@10.
    pub ndcg_10: f64,
    /// Average precision.
    pub average_precision: f64,
    /// Whether the query returned zero results (despite having labels).
    pub zero_result: bool,
    /// Relevant-file recall@10 (fraction of relevant files found in top-10).
    pub relevant_file_recall_10: f64,
    /// Relevant-symbol recall@10 (fraction of relevant symbols found in top-10).
    pub relevant_symbol_recall_10: f64,
}

/// Aggregated metrics across all cases or per-category.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AggregateMetrics {
    /// Mean Recall@1.
    pub mean_recall_1: f64,
    /// Mean Recall@5.
    pub mean_recall_5: f64,
    /// Mean Recall@10.
    pub mean_recall_10: f64,
    /// Mean MRR@10.
    pub mean_mrr_10: f64,
    /// Mean nDCG@10.
    pub mean_ndcg_10: f64,
    /// Mean average precision (MAP).
    pub mean_average_precision: f64,
    /// Mean relevant-file recall@10.
    pub mean_relevant_file_recall_10: f64,
    /// Mean relevant-symbol recall@10.
    pub mean_relevant_symbol_recall_10: f64,
    /// Number of zero-result cases.
    pub zero_result_count: usize,
    /// Total number of cases evaluated.
    pub case_count: usize,
    /// 95% confidence interval half-width for MRR@10.
    pub mrr_10_ci95: f64,
    /// 95% confidence interval half-width for Recall@10.
    pub recall_10_ci95: f64,
    /// 95% confidence interval half-width for nDCG@10.
    pub ndcg_10_ci95: f64,
}

/// Per-category metrics table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CategoryMetrics {
    /// Category name.
    pub category: String,
    /// Aggregate metrics for this category.
    pub metrics: AggregateMetrics,
}

/// Full evaluation results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalResults {
    /// Per-case metrics.
    pub per_case: Vec<CaseMetrics>,
    /// Aggregate metrics across all cases.
    pub aggregate: AggregateMetrics,
    /// Per-category aggregate metrics.
    pub per_category: Vec<CategoryMetrics>,
}

/// Compute case-level metrics from a single ranked result list.
///
/// `ranked_symbols` is the retrieved symbol ranking for this case (most
/// relevant first). `ranked_files` is the retrieved file ranking (optional,
/// may be the same list or a separate file-level ranking).
pub fn compute_case_metrics(
    case: &CorpusCase,
    ranked_symbols: &[String],
    ranked_files: &[String],
) -> CaseMetrics {
    // Symbol-level recall and ranking metrics
    let recall_1 = recall_at_1(ranked_symbols, &case.relevant_symbols);
    let recall_5 = recall_at_5(ranked_symbols, &case.relevant_symbols);
    let recall_10 = recall_at_10(ranked_symbols, &case.relevant_symbols);
    let mrr_10 = reciprocal_rank_at_10(ranked_symbols, &case.relevant_symbols);
    let ndcg = ndcg_at_10(ranked_symbols, &case.relevant_symbols);
    let map = average_precision(ranked_symbols, &case.relevant_symbols);

    // File-level recall
    let file_recall_10 = recall_at_10(ranked_files, &case.relevant_files);

    // Symbol-level recall (separate from recall@k since it operates on
    // the file list if relevant_files is populated)
    let symbol_recall_10 = if ranked_files.is_empty() {
        // If no file ranking available, use symbol ranking for file recall too
        recall_at_10(ranked_symbols, &case.relevant_files)
    } else {
        recall_at_10(ranked_symbols, &case.relevant_symbols)
    };

    let zero_result = ranked_symbols.is_empty() && ranked_files.is_empty();

    CaseMetrics {
        case_id: case.id.clone(),
        category: case.category.as_str().to_string(),
        recall_1,
        recall_5,
        recall_10,
        mrr_10,
        ndcg_10: ndcg,
        average_precision: map,
        zero_result,
        relevant_file_recall_10: file_recall_10,
        relevant_symbol_recall_10: symbol_recall_10,
    }
}

/// Aggregate case-level metrics into summary statistics.
pub fn aggregate_metrics(cases: &[CaseMetrics]) -> AggregateMetrics {
    if cases.is_empty() {
        return AggregateMetrics {
            mean_recall_1: 0.0,
            mean_recall_5: 0.0,
            mean_recall_10: 0.0,
            mean_mrr_10: 0.0,
            mean_ndcg_10: 0.0,
            mean_average_precision: 0.0,
            mean_relevant_file_recall_10: 0.0,
            mean_relevant_symbol_recall_10: 0.0,
            zero_result_count: 0,
            case_count: 0,
            mrr_10_ci95: 0.0,
            recall_10_ci95: 0.0,
            ndcg_10_ci95: 0.0,
        };
    }

    let n = cases.len() as f64;

    let mean_recall_1: f64 = cases.iter().map(|c| c.recall_1).sum::<f64>() / n;
    let mean_recall_5: f64 = cases.iter().map(|c| c.recall_5).sum::<f64>() / n;
    let mean_recall_10: f64 = cases.iter().map(|c| c.recall_10).sum::<f64>() / n;
    let mean_mrr: f64 = cases.iter().map(|c| c.mrr_10).sum::<f64>() / n;
    let mean_ndcg: f64 = cases.iter().map(|c| c.ndcg_10).sum::<f64>() / n;
    let mean_map: f64 = cases.iter().map(|c| c.average_precision).sum::<f64>() / n;
    let mean_file_recall: f64 = cases.iter().map(|c| c.relevant_file_recall_10).sum::<f64>() / n;
    let mean_sym_recall: f64 = cases
        .iter()
        .map(|c| c.relevant_symbol_recall_10)
        .sum::<f64>()
        / n;
    let zero_count = cases.iter().filter(|c| c.zero_result).count();

    // Confidence intervals (95% using t ~ 1.96 for large n, conservative for small n)
    let mrr_ci = confidence_interval_95(
        &cases.iter().map(|c| c.mrr_10).collect::<Vec<_>>(),
        mean_mrr,
    );
    let recall_ci = confidence_interval_95(
        &cases.iter().map(|c| c.recall_10).collect::<Vec<_>>(),
        mean_recall_10,
    );
    let ndcg_ci = confidence_interval_95(
        &cases.iter().map(|c| c.ndcg_10).collect::<Vec<_>>(),
        mean_ndcg,
    );

    AggregateMetrics {
        mean_recall_1,
        mean_recall_5,
        mean_recall_10,
        mean_mrr_10: mean_mrr,
        mean_ndcg_10: mean_ndcg,
        mean_average_precision: mean_map,
        mean_relevant_file_recall_10: mean_file_recall,
        mean_relevant_symbol_recall_10: mean_sym_recall,
        zero_result_count: zero_count,
        case_count: cases.len(),
        mrr_10_ci95: mrr_ci,
        recall_10_ci95: recall_ci,
        ndcg_10_ci95: ndcg_ci,
    }
}

/// Compute 95% confidence interval half-width using t-distribution.
///
/// Uses a conservative t-value: 1.96 for n > 30, and exact small-sample
/// t-values for n <= 30. The CI half-width is t * stddev / sqrt(n).
fn confidence_interval_95(values: &[f64], mean: f64) -> f64 {
    let n = values.len();
    if n <= 1 {
        return 0.0;
    }
    let nf = n as f64;
    let variance = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / (nf - 1.0);
    let stddev = variance.sqrt();
    let t = t_value_for_ci95(n);
    t * stddev / nf.sqrt()
}

/// Conservative t-values for 95% CI (two-tailed).
///
/// For n > 30, uses 1.96 (normal approximation).
/// For small n, uses exact t-distribution critical values (df = n-1).
#[allow(clippy::match_overlapping_arm)]
fn t_value_for_ci95(sample_size: usize) -> f64 {
    let df = sample_size.saturating_sub(1);
    // Exact t-values for df degrees of freedom, two-tailed alpha=0.05
    match df {
        0 => 0.0,
        1 => 12.706,
        2 => 4.303,
        3 => 3.182,
        4 => 2.776,
        5 => 2.571,
        6 => 2.447,
        7 => 2.365,
        8 => 2.306,
        9 => 2.262,
        10 => 2.228,
        11..=15 => 2.145,
        16..=20 => 2.093,
        21..=25 => 2.064,
        26..=30 => 2.045,
        _ => 1.960, // Normal approximation for large samples
    }
}

/// Compute per-category metrics from per-case metrics.
pub fn per_category_metrics(cases: &[CaseMetrics]) -> Vec<CategoryMetrics> {
    let mut by_category: HashMap<String, Vec<&CaseMetrics>> = HashMap::new();
    for case in cases {
        by_category
            .entry(case.category.clone())
            .or_default()
            .push(case);
    }

    let mut result: Vec<CategoryMetrics> = by_category
        .into_iter()
        .map(|(category, cat_cases)| {
            let owned: Vec<CaseMetrics> = cat_cases.into_iter().cloned().collect();
            CategoryMetrics {
                category,
                metrics: aggregate_metrics(&owned),
            }
        })
        .collect();

    result.sort_by(|a, b| a.category.cmp(&b.category));
    result
}

/// Compute full evaluation results from per-case metrics.
pub fn compute_eval_results(cases: Vec<CaseMetrics>) -> EvalResults {
    let aggregate = aggregate_metrics(&cases);
    let per_category = per_category_metrics(&cases);
    EvalResults {
        per_case: cases,
        aggregate,
        per_category,
    }
}

/// Compute metrics for all eval-split cases in a corpus given a retrieval
/// function.
///
/// The `retriever` closure takes a corpus case and returns (ranked_symbols,
/// ranked_files). This is the primary entry point for the harness.
pub fn evaluate_corpus<F>(corpus: &super::corpus::Corpus, mut retriever: F) -> EvalResults
where
    F: FnMut(&CorpusCase) -> (Vec<String>, Vec<String>),
{
    let case_metrics: Vec<CaseMetrics> = corpus
        .eval_cases()
        .map(|case| {
            let (symbols, files) = retriever(case);
            compute_case_metrics(case, &symbols, &files)
        })
        .collect();

    compute_eval_results(case_metrics)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recall_at_k_perfect() {
        let retrieved = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let relevant = vec!["a".to_string(), "b".to_string()];
        assert_eq!(recall_at_10(&retrieved, &relevant), 1.0); // Both found in top-10
    }

    #[test]
    fn test_recall_at_k_partial() {
        let retrieved = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let relevant = vec!["a".to_string(), "d".to_string()];
        // Only 1 of 2 relevant items in retrieved
        assert!((recall_at_10(&retrieved, &relevant) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_recall_at_k_miss() {
        let retrieved = vec!["x".to_string(), "y".to_string()];
        let relevant = vec!["a".to_string()];
        assert_eq!(recall_at_10(&retrieved, &relevant), 0.0);
    }

    #[test]
    fn test_recall_at_k_cutoff() {
        let retrieved = vec![
            "x".to_string(),
            "a".to_string(), // At position 1 (0-indexed), so in top-1 but not top-1 rank
        ];
        let relevant = vec!["a".to_string()];
        // recall@1 = 0 (first item is not relevant)
        assert!((recall_at_1(&retrieved, &relevant) - 0.0).abs() < 1e-10);
        // recall@5 = 1 (a is within top-5)
        assert!((recall_at_5(&retrieved, &relevant) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_recall_empty_relevant() {
        let retrieved = vec!["a".to_string()];
        let relevant: Vec<String> = vec![];
        assert_eq!(recall_at_10(&retrieved, &relevant), 0.0);
    }

    #[test]
    fn test_reciprocal_rank_first_position() {
        let retrieved = vec!["a".to_string(), "b".to_string()];
        let relevant = vec!["a".to_string()];
        assert!((reciprocal_rank_at_10(&retrieved, &relevant) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_reciprocal_rank_second_position() {
        let retrieved = vec!["x".to_string(), "a".to_string()];
        let relevant = vec!["a".to_string()];
        assert!((reciprocal_rank_at_10(&retrieved, &relevant) - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_reciprocal_rank_tenth_position() {
        let retrieved: Vec<String> = (0..9)
            .map(|i| format!("distractor-{i}"))
            .chain(std::iter::once("target".to_string()))
            .collect();
        let relevant = vec!["target".to_string()];
        assert!((reciprocal_rank_at_10(&retrieved, &relevant) - 0.1).abs() < 1e-10);
    }

    #[test]
    fn test_reciprocal_rank_not_found() {
        let retrieved = vec!["x".to_string(), "y".to_string()];
        let relevant = vec!["a".to_string()];
        assert!((reciprocal_rank_at_10(&retrieved, &relevant) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_reciprocal_rank_beyond_k() {
        let retrieved: Vec<String> = (0..10)
            .map(|i| format!("distractor-{i}"))
            .chain(std::iter::once("target".to_string()))
            .collect();
        let relevant = vec!["target".to_string()];
        // Target at position 11, beyond k=10
        assert!((reciprocal_rank_at_10(&retrieved, &relevant) - 0.0).abs() < 1e-10);
    }

    #[test]
    fn test_ndcg_perfect_ranking() {
        let retrieved = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let relevant = vec!["a".to_string(), "b".to_string()];
        assert!((ndcg_at_10(&retrieved, &relevant) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_ndcg_imperfect_ranking() {
        let retrieved = vec!["x".to_string(), "a".to_string(), "y".to_string()];
        let relevant = vec!["a".to_string()];
        // DCG = 0/log2(2) + 1/log2(3) + 0 = 1/log2(3)
        // IDCG = 1/log2(2) = 1.0
        // nDCG = (1/log2(3)) / 1.0 ≈ 0.6309
        let result = ndcg_at_10(&retrieved, &relevant);
        assert!((result - (1.0 / 3.0_f64.log2())).abs() < 1e-10);
    }

    #[test]
    fn test_average_precision_single_hit() {
        let retrieved = vec!["a".to_string()];
        let relevant = vec!["a".to_string()];
        assert!((average_precision(&retrieved, &relevant) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_average_precision_two_hits() {
        let retrieved = vec!["a".to_string(), "x".to_string(), "b".to_string()];
        let relevant = vec!["a".to_string(), "b".to_string()];
        // P@1 = 1/1, P@3 = 2/3
        // AP = (1.0 * 1 + 0 * 0 + 0.6667 * 1) / 2 = (1 + 0.6667) / 2 ≈ 0.8333
        let expected = (1.0 + (2.0 / 3.0)) / 2.0;
        let result = average_precision(&retrieved, &relevant);
        assert!((result - expected).abs() < 1e-10);
    }

    #[test]
    fn test_aggregate_metrics_basic() {
        let cases = vec![
            CaseMetrics {
                case_id: "1".to_string(),
                category: "cat_a".to_string(),
                recall_1: 1.0,
                recall_5: 1.0,
                recall_10: 1.0,
                mrr_10: 1.0,
                ndcg_10: 1.0,
                average_precision: 1.0,
                zero_result: false,
                relevant_file_recall_10: 1.0,
                relevant_symbol_recall_10: 1.0,
            },
            CaseMetrics {
                case_id: "2".to_string(),
                category: "cat_a".to_string(),
                recall_1: 0.0,
                recall_5: 0.5,
                recall_10: 0.5,
                mrr_10: 0.5,
                ndcg_10: 0.5,
                average_precision: 0.5,
                zero_result: false,
                relevant_file_recall_10: 0.5,
                relevant_symbol_recall_10: 0.5,
            },
        ];

        let agg = aggregate_metrics(&cases);
        assert_eq!(agg.case_count, 2);
        assert!((agg.mean_recall_1 - 0.5).abs() < 1e-10);
        assert!((agg.mean_mrr_10 - 0.75).abs() < 1e-10);
    }

    #[test]
    fn test_confidence_interval_decreases_with_more_samples() {
        // More samples should produce narrower CI (roughly)
        let small: Vec<f64> = (0..5).map(|i| i as f64).collect();
        let mean_small = small.iter().sum::<f64>() / small.len() as f64;
        let ci_small = confidence_interval_95(&small, mean_small);

        let large: Vec<f64> = (0..100).map(|i| (i % 5) as f64).collect();
        let mean_large = large.iter().sum::<f64>() / large.len() as f64;
        let ci_large = confidence_interval_95(&large, mean_large);

        assert!(ci_large < ci_small, "CI should be smaller with more data");
    }

    #[test]
    fn test_per_category_metrics_split() {
        let cases = vec![
            CaseMetrics {
                case_id: "1".to_string(),
                category: "cat_a".to_string(),
                recall_1: 1.0,
                recall_5: 1.0,
                recall_10: 1.0,
                mrr_10: 1.0,
                ndcg_10: 1.0,
                average_precision: 1.0,
                zero_result: false,
                relevant_file_recall_10: 1.0,
                relevant_symbol_recall_10: 1.0,
            },
            CaseMetrics {
                case_id: "2".to_string(),
                category: "cat_b".to_string(),
                recall_1: 0.0,
                recall_5: 0.0,
                recall_10: 0.0,
                mrr_10: 0.0,
                ndcg_10: 0.0,
                average_precision: 0.0,
                zero_result: true,
                relevant_file_recall_10: 0.0,
                relevant_symbol_recall_10: 0.0,
            },
        ];
        let per_cat = per_category_metrics(&cases);
        assert_eq!(per_cat.len(), 2);
        for cm in &per_cat {
            match cm.category.as_str() {
                "cat_a" => assert!((cm.metrics.mean_recall_10 - 1.0).abs() < 1e-10),
                "cat_b" => assert!((cm.metrics.mean_recall_10 - 0.0).abs() < 1e-10),
                _ => panic!("unexpected category"),
            }
        }
    }

    #[test]
    fn test_known_answer_fixture() {
        // Known answer test for metric correctness (VAL-EVAL-003).
        // Hand-computed expected values.
        let retrieved = vec![
            "correct_1".to_string(),
            "wrong_1".to_string(),
            "correct_2".to_string(),
            "wrong_2".to_string(),
        ];
        let relevant = vec!["correct_1".to_string(), "correct_2".to_string()];

        // Recall@1: 1 of 2 relevant in top-1 = 0.5
        assert!((recall_at_1(&retrieved, &relevant) - 0.5).abs() < 1e-10);
        // Recall@5: 2 of 2 = 1.0
        assert!((recall_at_5(&retrieved, &relevant) - 1.0).abs() < 1e-10);
        // MRR@10: 1/1 = 1.0 (first hit at rank 1)
        assert!((reciprocal_rank_at_10(&retrieved, &relevant) - 1.0).abs() < 1e-10);
        // nDCG@10:
        // DCG = 1/log2(2) + 0 + 1/log2(4) + 0 = 1 + 0.5 = 1.5
        // IDCG = 1/log2(2) + 1/log2(3) = 1 + 0.6309...
        // nDCG = 1.5 / (1 + 1/log2(3))
        let idcg = 1.0 / 2.0_f64.log2() + 1.0 / 3.0_f64.log2();
        let expected_ndcg = 1.5 / idcg;
        assert!((ndcg_at_10(&retrieved, &relevant) - expected_ndcg).abs() < 1e-10);
    }

    #[test]
    fn test_t_value_decreasing() {
        // t-values should decrease as sample size increases (converges to 1.96)
        let t5 = t_value_for_ci95(6);
        let t10 = t_value_for_ci95(11);
        let t100 = t_value_for_ci95(101);
        assert!(t5 > t10);
        assert!(t10 > t100);
        assert!((t100 - 1.960).abs() < 0.01);
    }
}
