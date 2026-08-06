//! Concise and machine-readable report generation for evaluation results.
//!
//! Provides summary types for ablation reports and helper functions for
//! generating JSON exports.

use serde::{Deserialize, Serialize};

/// Concise ablation summary: which signals contribute most to fused quality.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AblationSummary {
    /// Base model name used for evaluation.
    pub base_model: String,
    /// MRR@10 with all signals enabled.
    pub full_mrr10: f64,
    /// Recall@10 with all signals enabled.
    pub full_recall10: f64,
    /// Per-signal MRR@10 impact (signal name, impact value).
    /// Sorted by impact descending.
    pub signal_impacts_mrr10: Vec<(String, f64)>,
}

impl std::fmt::Display for AblationSummary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Ablation Summary ({})", self.base_model)?;
        writeln!(f, "  Full MRR@10:      {:.4}", self.full_mrr10)?;
        writeln!(f, "  Full Recall@10:   {:.4}", self.full_recall10)?;
        writeln!(f, "  Signal impacts (MRR@10 contribution):")?;
        for (signal, impact) in &self.signal_impacts_mrr10 {
            writeln!(f, "    {signal:<12} {impact:+.4}")?;
        }
        Ok(())
    }
}

/// Bake-off candidate row for the overall comparison table.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CandidateRow {
    /// Candidate identifier (e.g., "qwen3-int8", "embedding-gemma-300m").
    pub candidate: String,
    /// Embedding model name.
    pub embedding_model: String,
    /// Reranker policy: "keep", "replace", "remove".
    pub reranker_policy: String,
    /// MRR@10 score.
    pub mrr_10: f64,
    /// Recall@10 score.
    pub recall_10: f64,
    /// nDCG@10 score.
    pub ndcg_10: f64,
    /// Per-category results where protected categories passed.
    pub protected_categories_passed: bool,
    /// Whether this candidate passed all gates.
    pub gates_passed: bool,
    /// Host RSS in MiB.
    pub host_rss_mib: f64,
    /// GPU VRAM in MiB (0 if CPU-only).
    pub gpu_vram_mib: f64,
    /// p95 latency in ms.
    pub p95_latency_ms: f64,
}

/// Bake-off report: full comparison table across all candidates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BakeoffReport {
    /// Baseline candidate identifier.
    pub baseline_candidate: String,
    /// All candidate rows.
    pub rows: Vec<CandidateRow>,
    /// Winner candidate identifier (set after applying gates).
    pub winner: Option<String>,
}

impl BakeoffReport {
    /// Create a new empty bake-off report.
    pub fn new(baseline: &str) -> Self {
        Self {
            baseline_candidate: baseline.to_string(),
            rows: Vec::new(),
            winner: None,
        }
    }

    /// Add a candidate row.
    pub fn add(&mut self, row: CandidateRow) {
        self.rows.push(row);
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Reranker ablation decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankerDecision {
    /// Decision: "keep", "replace", or "remove".
    pub decision: String,
    /// Rationale (with evidence reference).
    pub rationale: String,
    /// Configuration tested.
    pub configs: Vec<RerankerConfig>,
}

/// Reranker ablation configuration row.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RerankerConfig {
    /// Configuration name.
    pub name: String,
    /// MRR@10 score.
    pub mrr_10: f64,
    /// Recall@10 score.
    pub recall_10: f64,
    /// Memory cost in MiB.
    pub memory_mib: f64,
    /// Whether this config passed gates.
    pub passed: bool,
}

impl RerankerDecision {
    /// Create a new empty reranker decision.
    pub fn new(decision: &str, rationale: &str) -> Self {
        Self {
            decision: decision.to_string(),
            rationale: rationale.to_string(),
            configs: Vec::new(),
        }
    }

    /// Add a configuration row.
    pub fn add_config(&mut self, config: RerankerConfig) {
        self.configs.push(config);
    }

    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_ablation_summary_display() {
        let summary = AblationSummary {
            base_model: "test-model".to_string(),
            full_mrr10: 0.85,
            full_recall10: 0.90,
            signal_impacts_mrr10: vec![("reranker".to_string(), 0.05), ("dense".to_string(), 0.02)],
        };
        let s = format!("{summary}");
        assert!(s.contains("test-model"));
        assert!(s.contains("0.8500"));
        assert!(s.contains("reranker"));
    }

    #[test]
    fn test_bakeoff_report_json() {
        let mut report = BakeoffReport::new("baseline");
        report.add(CandidateRow {
            candidate: "test-candidate".to_string(),
            embedding_model: "test-embedding".to_string(),
            reranker_policy: "keep".to_string(),
            mrr_10: 0.85,
            recall_10: 0.90,
            ndcg_10: 0.88,
            protected_categories_passed: true,
            gates_passed: true,
            host_rss_mib: 300.0,
            gpu_vram_mib: 0.0,
            p95_latency_ms: 5.0,
        });

        let json = report.to_json().expect("json");
        assert!(json.contains("\"baseline_candidate\""));
        assert!(json.contains("test-candidate"));
    }

    #[test]
    fn test_reranker_decision_json() {
        let mut decision =
            RerankerDecision::new("keep", "Reranker contributes +5pp MRR@10 within budget");
        decision.add_config(RerankerConfig {
            name: "baseline-qwen3-reranker".to_string(),
            mrr_10: 0.85,
            recall_10: 0.90,
            memory_mib: 1190.0,
            passed: true,
        });
        let json = decision.to_json().expect("json");
        assert!(json.contains("\"decision\""));
        assert!(json.contains("keep"));
    }
}
