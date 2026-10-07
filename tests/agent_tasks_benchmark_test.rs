//! Integration test that generates the W6 agent-task benchmark documentation.
//!
//! When this test runs, it executes the deterministic agent-task suite
//! (internal fixtures + the vendored CoSQA external benchmark) and writes the
//! markdown reports under docs/baselines/, mirroring the ws11 bake-off
//! pattern.
//!
//! These reports are the evidence artifacts for the W6 roadmap wave:
//! - Deterministic agent-task suite over ≥3 fixture repositories
//! - Ground-truth Recall@10 / MRR@10 / nDCG@10 via the LeIndex lexical path
//! - Token cost (chars/4) and tool-call count vs a naive-tools baseline
//! - Doc-section tasks measuring the docs tier
//! - External validation on CoSQA (ACL 2021) real web queries

#![cfg(feature = "full")]

use leindex::eval::{agent_tasks, external_suite};
use std::fs;
use std::path::Path;

#[test]
fn test_agent_tasks_report_generated() {
    let report = agent_tasks::run_suite();
    let md = agent_tasks::generate_markdown(&report);

    let path = Path::new("docs/baselines/2026-08-20-w6-agent-tasks.md");
    fs::create_dir_all(path.parent().unwrap()).expect("create dir");
    fs::write(path, &md).expect("write agent-task markdown");

    // The suite must cover three repositories and both task categories.
    assert!(report.per_repo.len() >= 3, "need ≥3 fixture repos");
    assert!(
        report.task_rows.len() >= 24,
        "need ≥24 tasks, got {}",
        report.task_rows.len()
    );
    for repo in ["leindex-self-mirror", "polyglot-checkout", "docs-corpus"] {
        assert!(md.contains(repo), "report missing repo {repo}");
    }
    assert!(
        md.contains("doc_section"),
        "report missing docs-tier category"
    );

    // Gate: the LeIndex lexical path must beat the naive-tools baseline on
    // aggregate retrieval quality on the deterministic suite.
    let (le, naive) = &report.aggregate;
    assert!(
        le.recall10 > naive.recall10,
        "leindex recall@10 ({:.3}) must beat naive ({:.3})",
        le.recall10,
        naive.recall10
    );
    assert!(
        le.mrr10 >= naive.mrr10,
        "leindex MRR@10 ({:.3}) must be ≥ naive ({:.3})",
        le.mrr10,
        naive.mrr10
    );

    // Gate: one search call returning snippets must cost fewer tokens than
    // the naive read-whole-files workflow. This is the token-savings claim.
    assert!(
        le.avg_tokens < naive.avg_tokens,
        "leindex avg tokens ({:.0}) must be < naive ({:.0})",
        le.avg_tokens,
        naive.avg_tokens
    );

    // Gate: the docs tier must be measured and win its category — doc
    // sections must be retrievable by natural-language queries.
    let doc_tier = report
        .per_category
        .iter()
        .find(|(c, _, _)| c == "doc_section")
        .expect("doc_section category present");
    assert!(
        doc_tier.1.recall10 > doc_tier.2.recall10,
        "docs-tier leindex recall@10 ({:.3}) must beat naive ({:.3})",
        doc_tier.1.recall10,
        doc_tier.2.recall10
    );
}

#[test]
fn test_agent_tasks_report_is_deterministic() {
    let a = agent_tasks::run_suite();
    let b = agent_tasks::run_suite();
    assert_eq!(
        format!("{:.6}", a.aggregate.0.recall10),
        format!("{:.6}", b.aggregate.0.recall10)
    );
    assert_eq!(
        format!("{:.6}", a.aggregate.1.mrr10),
        format!("{:.6}", b.aggregate.1.mrr10)
    );
}

#[test]
fn test_cosqa_external_report_generated() {
    let report = external_suite::run_cosqa().expect("cosqa subset runs");
    let md = external_suite::generate_cosqa_markdown(&report);

    let path = Path::new("docs/baselines/2026-08-20-w6-cosqa-external.md");
    fs::create_dir_all(path.parent().unwrap()).expect("create dir");
    fs::write(path, &md).expect("write cosqa markdown");

    assert!(report.records >= 60, "vendored subset shrank");
    assert!(md.contains("CoSQA"), "report must attribute the source");
    assert!(md.contains("leindex") && md.contains("naive"));

    // Gate: on real human-annotated web queries, the LeIndex lexical path
    // (IDF-weighted + identifier signal) must not lose to raw token-overlap
    // counting — the floor comparison for the lexical signal.
    assert!(
        report.leindex.mrr10 >= report.naive.mrr10,
        "cosqa leindex MRR@10 ({:.3}) must be ≥ naive ({:.3})",
        report.leindex.mrr10,
        report.naive.mrr10
    );
}
