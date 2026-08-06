//! Integration test that generates the WS11 Task 4-5 bake-off documentation.
//!
//! When this test runs, it exercises the full eval harness with all candidates
//! and generates the markdown reports under docs/baselines/.
//!
//! These reports are the evidence artifacts for:
//! - VAL-EVAL-004: Candidates tested through fused retrieval harness
//! - VAL-EVAL-005: Reranker ablation evaluated independently
//! - VAL-EVAL-008: Candidate violating aggregate MRR@10 gate fails
//! - VAL-EVAL-009: Candidate violating protected-category regression gate fails

#![cfg(feature = "full")]

use leindex::eval::candidates;
use leindex::eval::corpus::load_and_verify_corpus;
use leindex::eval::gates::{Gates, VarianceBands};
use leindex::eval::harness::EvalHarness;
use leindex::eval::reranker_ablation;
use std::collections::HashMap;
use std::fs;
use std::path::Path;

#[test]
fn test_embedding_bakeoff_report_generated() {
    let corpus = load_and_verify_corpus().expect("corpus should verify");
    let harness = EvalHarness::new(corpus);
    let catalog = candidates::build_candidate_catalog();

    // Use reasonable variance bands derived from test runs
    let gates = Gates::default().with_variance_bands(VarianceBands {
        aggregate_mrr10_stddev: 0.02,
        per_category_mrr10_stddev: HashMap::from([
            ("hard_negatives".to_string(), 0.02),
            ("changed_deleted_freshness".to_string(), 0.02),
            ("same_name_different_behavior".to_string(), 0.02),
        ]),
        ..VarianceBands::default()
    });

    let bakeoff = candidates::run_full_bakeoff(
        &harness,
        &catalog,
        |p| p.quality_factor,
        &gates,
        350.0, // Embed worker target per §7 budget ledger
    )
    .expect("bakeoff should complete");

    let md = candidates::generate_bakeoff_markdown(&bakeoff);

    let path = Path::new("docs/baselines/2026-08-04-ws11-embedding-bakeoff.md");
    fs::create_dir_all(path.parent().unwrap()).expect("create dir");
    fs::write(path, &md).expect("write bakeoff markdown");

    // Verify report contains all candidates (VAL-EVAL-004)
    for candidate in &catalog {
        assert!(
            md.contains(&candidate.id),
            "Report missing candidate {}",
            candidate.id
        );
    }

    // Verify each candidate was tested via fused retrieval (not standalone)
    for result in &bakeoff.candidates {
        assert!(
            result
                .harness_report
                .profile
                .is_enabled(leindex::eval::harness::FusedSignal::Tfidf),
            "Candidate not using TF-IDF signal"
        );
        assert!(
            result
                .harness_report
                .profile
                .is_enabled(leindex::eval::harness::FusedSignal::Dense),
            "Candidate not using dense signal"
        );
    }
}

#[test]
fn test_reranker_ablation_report_generated() {
    let corpus = load_and_verify_corpus().expect("corpus should verify");
    let harness = EvalHarness::new(corpus);

    let ablation = reranker_ablation::run_reranker_ablation(
        &harness,
        "qwen3-fp16",
        0.0, // Zero memory budget: evaluates cost trade-off
    )
    .expect("reranker ablation should complete");

    let md = reranker_ablation::generate_reranker_ablation_markdown(&ablation);

    let path = Path::new("docs/baselines/2026-08-04-ws11-reranker-ablation.md");
    fs::create_dir_all(path.parent().unwrap()).expect("create dir");
    fs::write(path, &md).expect("write reranker markdown");

    // Verify all 4 configurations evaluated (VAL-EVAL-005)
    assert!(ablation.results.len() >= 4);

    for policy in reranker_ablation::RerankerPolicy::all() {
        assert!(
            md.contains(policy.name()),
            "Report missing policy {:?}",
            policy
        );
    }

    // Verify decision is recorded
    assert!(!ablation.decision.is_empty());
    assert!(md.contains("DECISION"));
}

#[test]
fn test_gate_rejects_mrr_regression() {
    // VAL-EVAL-008: Gate checker correctly rejects aggregate MRR regression
    use leindex::eval::gates::{GateCandidateResults, Gates};

    let gates = Gates::default();
    let results = GateCandidateResults {
        aggregate_mrr10_regression: 0.05, // 5% regression
        per_category_mrr10_regression: HashMap::new(),
        p95_latency_regression_pct: 0.0,
        wall_time_regression_pct: 0.0,
        new_zero_result_count: 0,
    };

    let failure = gates.check(&results);
    assert!(failure.is_err(), "5% MRR regression should fail");
    if let Err(f) = failure {
        assert!(matches!(
            f.gate,
            leindex::eval::gates::GateType::AggregateMrr10Regression
        ));
    }
}

#[test]
fn test_gate_rejects_protected_category_regression() {
    // VAL-EVAL-009: Gate checker rejects protected-category regression >1pp
    use leindex::eval::gates::{GateCandidateResults, Gates};

    let gates = Gates::default();
    let mut per_cat = HashMap::new();
    per_cat.insert("hard_negatives".to_string(), 0.02); // 2pp regression

    let results = GateCandidateResults {
        aggregate_mrr10_regression: 0.0,
        per_category_mrr10_regression: per_cat,
        p95_latency_regression_pct: 0.0,
        wall_time_regression_pct: 0.0,
        new_zero_result_count: 0,
    };

    let failure = gates.check(&results);
    assert!(failure.is_err(), "2pp protected cat regression should fail");
    if let Err(f) = failure {
        assert!(matches!(
            f.gate,
            leindex::eval::gates::GateType::ProtectedCategoryRegression(ref c)
                if c == "hard_negatives"
        ));
    }
}
