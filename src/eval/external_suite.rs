//! External benchmark suite: CoSQA retrieval (vendored, real human annotations).
//!
//! Complements the synthetic agent-task suite ([`super::agent_tasks`]) with an
//! authoritative public benchmark: CoSQA (ACL 2021) web queries paired with
//! human-annotated Python code. A deterministic 63-record subset of the
//! official `cosqa-retrieval-test-500.json` split is vendored under
//! `src/eval/corpus/cosqa/` (C-UDA 1.0 license; see the README there).
//!
//! Every record contributes one code document (id = `idx`) and one query whose
//! single ground truth is its paired document. The same two deterministic
//! backends as the agent-task suite run over this real data:
//!
//! - LeIndex: production TF-IDF lexical signal + identifier name-match boost.
//! - Naive: raw token-overlap counting over code text with whole-document
//!   "reads" (the grep+Read workflow).
//!
//! With one relevant document per query, Recall@10 equals hit@10 and MRR@10
//! captures ranking quality. Reports land in `docs/baselines/` via the
//! ws11-style integration test; methodology (including the wider external
//! landscape: CodeSearchNet, CodeXGLUE, CodeQueries, CoSQA+) lives in
//! `docs/baselines/AGENT_TASKS_METHODOLOGY.md`.

use crate::cli::index_builder::TfIdfEmbedder;
use serde::{Deserialize, Serialize};

use super::metrics;

/// The vendored CoSQA subset, embedded at compile time for hermetic runs.
const COSQA_SUBSET_JSON: &str = include_str!("corpus/cosqa/cosqa_retrieval_test_subset.json");

/// One vendored CoSQA record: a web query and its annotated Python code.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CosqaRecord {
    /// Upstream record identifier (ground-truth document id).
    pub idx: String,
    /// The natural-language web query.
    pub doc: String,
    /// The annotated Python code.
    pub code: String,
}

/// Parse the vendored subset.
pub fn load_cosqa_subset() -> Result<Vec<CosqaRecord>, String> {
    serde_json::from_str(COSQA_SUBSET_JSON).map_err(|e| format!("vendored CoSQA subset: {e}"))
}

/// A code document derived from a CoSQA record.
#[derive(Debug, Clone)]
pub struct CosqaDoc {
    /// Ground-truth id (upstream `idx`).
    pub id: String,
    /// Python function name extracted from the code, when present.
    pub function_name: Option<String>,
    /// The code body.
    pub code: String,
}

/// Extract the leading `def name(` identifier from Python source.
fn python_function_name(code: &str) -> Option<String> {
    code.lines().find_map(|line| {
        let trimmed = line.trim_start();
        let rest = trimmed.strip_prefix("def ")?;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    })
}

/// Build the document pool from the vendored records.
pub fn cosqa_docs(records: &[CosqaRecord]) -> Vec<CosqaDoc> {
    records
        .iter()
        .map(|r| CosqaDoc {
            id: r.idx.clone(),
            function_name: python_function_name(&r.code),
            code: r.code.clone(),
        })
        .collect()
}

/// Metrics for one backend over the CoSQA subset.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CosqaMetrics {
    /// Mean Recall@10 (hit@10 with single-relevant queries).
    pub recall10: f64,
    /// Mean MRR@10.
    pub mrr10: f64,
    /// Mean nDCG@10.
    pub ndcg10: f64,
    /// Mean tokens per query (chars/4).
    pub avg_tokens: f64,
    /// Mean tool calls per query.
    pub avg_tool_calls: f64,
    /// Number of queries.
    pub queries: usize,
}

/// Full CoSQA comparison report.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CosqaReport {
    /// Vendored subset size.
    pub records: usize,
    /// LeIndex backend metrics.
    pub leindex: CosqaMetrics,
    /// Naive baseline metrics.
    pub naive: CosqaMetrics,
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// Run the CoSQA subset through both backends.
pub fn run_cosqa() -> Result<CosqaReport, String> {
    let records = load_cosqa_subset()?;
    let docs = cosqa_docs(&records);

    // LeIndex side: index enriched docs with the production TF-IDF signal.
    let corpus: Vec<(String, String)> = docs
        .iter()
        .map(|d| {
            (
                d.id.clone(),
                format!("{} {}", d.function_name.clone().unwrap_or_default(), d.code),
            )
        })
        .collect();
    let embedder = TfIdfEmbedder::build(&corpus);
    let doc_vecs: Vec<(String, Vec<f32>)> = corpus
        .iter()
        .map(|(id, content)| (id.clone(), embedder.embed(content)))
        .collect();

    let mut le_hits = Vec::new();
    let mut naive_hits = Vec::new();
    for record in &records {
        let relevant = [record.idx.clone()];

        // LeIndex: composite score mirroring production ranking — semantic
        // cosine + text query-coverage + structural identifier signal.
        let query_vec = embedder.embed(&record.doc);
        let query_tokens: Vec<String> = record
            .doc
            .split(|c: char| !(c.is_ascii_alphanumeric()))
            .filter(|t| !t.is_empty())
            .map(|t| t.to_ascii_lowercase())
            .collect();
        let mut scored: Vec<(String, f64)> = doc_vecs
            .iter()
            .map(|(id, vec)| {
                let doc = docs.iter().find(|d| &d.id == id).expect("doc by id");
                let name_signal = doc
                    .function_name
                    .as_ref()
                    .map(|name| agent_name_signal(name, &query_tokens))
                    .unwrap_or(0.0);
                let doc_tokens: std::collections::HashSet<String> = tokenize(&doc.code);
                let coverage = if query_tokens.is_empty() {
                    0.0
                } else {
                    query_tokens
                        .iter()
                        .filter(|q| doc_tokens.contains(*q))
                        .count() as f64
                        / query_tokens.len() as f64
                };
                (
                    id.clone(),
                    0.45 * cosine(&query_vec, vec) as f64 + 0.35 * coverage + 0.20 * name_signal,
                )
            })
            .collect();
        scored.sort_by(|a, b| {
            b.1.partial_cmp(&a.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });
        let le_ranked: Vec<String> = scored.iter().take(10).map(|(id, _)| id.clone()).collect();
        le_hits.push((
            metrics::recall_at_10(&le_ranked, &relevant),
            metrics::reciprocal_rank_at_10(&le_ranked, &relevant),
            metrics::ndcg_at_10(&le_ranked, &relevant),
            le_ranked.len() * 160 / 4,
            1_usize,
        ));

        // Naive: token overlap over code text; read the top-3 whole docs.
        let query_set: Vec<String> = query_tokens;
        let mut naive_scored: Vec<(f64, String)> = docs
            .iter()
            .map(|d| {
                let doc_tokens: std::collections::HashSet<String> = tokenize(&d.code);
                let hits = query_set.iter().filter(|q| doc_tokens.contains(*q)).count() as f64;
                (hits, d.id.clone())
            })
            .collect();
        naive_scored.sort_by(|a, b| {
            b.0.partial_cmp(&a.0)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.1.cmp(&b.1))
        });
        let read: Vec<&CosqaDoc> = naive_scored
            .iter()
            .filter(|(score, _)| *score > 0.0)
            .take(3)
            .filter_map(|(_, id)| docs.iter().find(|d| d.id == *id))
            .collect();
        let naive_ranked: Vec<String> = read.iter().map(|d| d.id.clone()).collect();
        let payload: usize = read.iter().map(|d| d.code.len()).sum();
        naive_hits.push((
            metrics::recall_at_10(&naive_ranked, &relevant),
            metrics::reciprocal_rank_at_10(&naive_ranked, &relevant),
            metrics::ndcg_at_10(&naive_ranked, &relevant),
            payload / 4,
            2 + read.len(),
        ));
    }

    let aggregate = |rows: &Vec<(f64, f64, f64, usize, usize)>| CosqaMetrics {
        recall10: rows.iter().map(|r| r.0).sum::<f64>() / rows.len() as f64,
        mrr10: rows.iter().map(|r| r.1).sum::<f64>() / rows.len() as f64,
        ndcg10: rows.iter().map(|r| r.2).sum::<f64>() / rows.len() as f64,
        avg_tokens: rows.iter().map(|r| r.3).sum::<usize>() as f64 / rows.len() as f64,
        avg_tool_calls: rows.iter().map(|r| r.4).sum::<usize>() as f64 / rows.len() as f64,
        queries: rows.len(),
    };

    Ok(CosqaReport {
        records: records.len(),
        leindex: aggregate(&le_hits),
        naive: aggregate(&naive_hits),
    })
}

/// Lowercase alphanumeric tokenization (deliberately simple, shared shape
/// with the agent-task naive tokenizer).
fn tokenize(text: &str) -> std::collections::HashSet<String> {
    text.split(|c: char| !(c.is_ascii_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect()
}

/// Identifier name-match signal for a bare function name (see
/// `agent_tasks::name_match_signal`; duplicated minimally to keep the
/// external suite self-contained against doc-id formatting).
fn agent_name_signal(name: &str, query_tokens: &[String]) -> f64 {
    let name_tokens: Vec<String> = name
        .split(|c: char| !(c.is_ascii_alphanumeric()))
        .filter(|t| !t.is_empty())
        .map(|t| t.to_ascii_lowercase())
        .collect();
    if query_tokens.is_empty() || name_tokens.is_empty() {
        return 0.0;
    }
    let name_lower = name.to_ascii_lowercase();
    let query_joined = query_tokens.join("");
    if name_lower == query_joined {
        return 1.0;
    }
    if name_lower.contains(&query_joined) || query_joined.contains(&name_lower) {
        return 0.7;
    }
    let hits = query_tokens
        .iter()
        .filter(|q| name_tokens.iter().any(|n| n == *q))
        .count();
    if hits == query_tokens.len() { 0.4 } else { 0.0 }
}

/// Render the CoSQA comparison as markdown for `docs/baselines/`.
pub fn generate_cosqa_markdown(report: &CosqaReport) -> String {
    let mut md = String::new();
    md.push_str("# W6 External Benchmark — CoSQA (vendored subset)\n\n");
    md.push_str(&format!(
        "Source: CoSQA (ACL 2021, Huang et al.) official `cosqa-retrieval-test-500.json` split; deterministic every-8th-record subset ({} records) vendored under C-UDA 1.0.\n\n",
        report.records
    ));
    md.push_str("Real web queries with human-annotated Python code; single relevant document per query, so Recall@10 = hit@10.\n\n");
    md.push_str("| backend | recall@10 | MRR@10 | nDCG@10 | avg tokens/query | avg tool calls |\n");
    md.push_str("|---|---|---|---|---|---|\n");
    for (name, m) in [("leindex", &report.leindex), ("naive", &report.naive)] {
        md.push_str(&format!(
            "| {} | {:.3} | {:.3} | {:.3} | {:.0} | {:.1} |\n",
            name, m.recall10, m.mrr10, m.ndcg10, m.avg_tokens, m.avg_tool_calls
        ));
    }
    md.push_str("\nNote: both backends are lexical-only (no neural embeddings), so this measures the lexical signal's floor on real queries, not the deployed hybrid ceiling.\n");
    md
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vendored_subset_parses() {
        let records = load_cosqa_subset().expect("subset parses");
        assert!(records.len() >= 60, "expected ~63 records");
        assert!(
            records
                .iter()
                .all(|r| !r.idx.is_empty() && !r.doc.is_empty() && !r.code.is_empty())
        );
    }

    #[test]
    fn test_python_function_name_extraction() {
        assert_eq!(
            python_function_name("def _process_and_sort(s, force_ascii):\n    pass"),
            Some("_process_and_sort".to_string())
        );
        assert_eq!(python_function_name("# comment only"), None);
    }

    #[test]
    fn test_cosqa_run_is_deterministic_and_scores() {
        let a = run_cosqa().expect("run a");
        let b = run_cosqa().expect("run b");
        assert_eq!(
            format!("{:.6}", a.leindex.mrr10),
            format!("{:.6}", b.leindex.mrr10)
        );
        assert_eq!(
            format!("{:.6}", a.naive.mrr10),
            format!("{:.6}", b.naive.mrr10)
        );
        assert!(
            a.leindex.mrr10 > 0.0,
            "lexical signal must retrieve something"
        );
        assert!(a.naive.avg_tool_calls > 1.0);
    }
}
