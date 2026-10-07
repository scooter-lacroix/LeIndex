//! Fused-retrieval evaluation harness with ablation support.
//!
//! The harness indexes the evaluation corpus with a candidate profile, runs
//! queries through the fused retrieval pipeline, and emits JSON metrics.
//! It supports fused ablation: dropping individual retrieval signals (TF-IDF,
//! PDG, dense, fragment, reranker) to measure each signal's contribution.
//!
//! ## Design
//!
//! The harness is designed to evaluate candidates through the FULL fused
//! retrieval path (anti-cheat section 2.1 #13: no public-benchmark-only
//! selection). Public MTEB/CodeSearchNet numbers shortlist only; they do
//! NOT select the production winner.
//!
//! The harness defines a [`RetrievalBackend`] trait that the actual LeIndex
//! search engine will implement. For testing, a [`MockBackend`] provides
//! synthetic rankings.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

use super::corpus::Corpus;
use super::metrics::{self, CaseMetrics, EvalResults};
use super::report;

// ── Fused retrieval profile ─────────────────────────────────────────────────

/// A retrieval signal in the fused pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FusedSignal {
    /// TF-IDF (lexical) signal.
    Tfidf,
    /// Program dependency graph (structural) signal.
    Pdg,
    /// Dense (neural embedding) signal.
    Dense,
    /// Fragment/hash reuse signal.
    Fragment,
    /// Reranker (cross-encoder re-scoring) signal.
    Reranker,
}

impl FusedSignal {
    /// Returns all fused signals.
    pub fn all() -> [FusedSignal; 5] {
        [
            FusedSignal::Tfidf,
            FusedSignal::Pdg,
            FusedSignal::Dense,
            FusedSignal::Fragment,
            FusedSignal::Reranker,
        ]
    }

    /// Returns the string identifier.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Tfidf => "tfidf",
            Self::Pdg => "pdg",
            Self::Dense => "dense",
            Self::Fragment => "fragment",
            Self::Reranker => "reranker",
        }
    }
}

/// Which fused signals to enable or ablate.
///
/// Default is all signals enabled. To ablate, remove specific signals from
/// the set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusedProfile {
    /// Enabled signals (default: all five).
    pub enabled_signals: Vec<FusedSignal>,
    /// Optional weights for each signal (default: equal weight).
    pub signal_weights: HashMap<FusedSignal, f64>,
    /// Name of the embedding model (for identification).
    pub embedding_model: String,
    /// Name of the reranker model (or "none").
    pub reranker_model: String,
}

impl Default for FusedProfile {
    fn default() -> Self {
        Self {
            enabled_signals: FusedSignal::all().to_vec(),
            signal_weights: HashMap::new(),
            embedding_model: "qwen3-embedding-0.6b-fp16".to_string(),
            reranker_model: "qwen3-reranker-0.6b".to_string(),
        }
    }
}

impl FusedProfile {
    /// Create a profile that drops one signal (ablation).
    pub fn ablate(base: &FusedProfile, drop_signal: FusedSignal) -> FusedProfile {
        FusedProfile {
            enabled_signals: base
                .enabled_signals
                .iter()
                .copied()
                .filter(|s| *s != drop_signal)
                .collect(),
            signal_weights: base.signal_weights.clone(),
            embedding_model: base.embedding_model.clone(),
            reranker_model: base.reranker_model.clone(),
        }
    }

    /// Create a profile with no reranker (common ablation).
    pub fn no_reranker() -> FusedProfile {
        FusedProfile {
            enabled_signals: FusedSignal::all()
                .iter()
                .copied()
                .filter(|s| *s != FusedSignal::Reranker)
                .collect(),
            signal_weights: HashMap::new(),
            embedding_model: "qwen3-embedding-0.6b-fp16".to_string(),
            reranker_model: "none".to_string(),
        }
    }

    /// Check if a signal is enabled.
    pub fn is_enabled(&self, signal: FusedSignal) -> bool {
        self.enabled_signals.contains(&signal)
    }

    /// Get the weight for a signal (default 1.0 if not set).
    pub fn weight(&self, signal: FusedSignal) -> f64 {
        self.signal_weights.get(&signal).copied().unwrap_or(1.0)
    }

    /// Returns a label for this profile (for report identification).
    pub fn label(&self) -> String {
        if self.enabled_signals.len() == FusedSignal::all().len() {
            format!("full-fused ({})", self.embedding_model)
        } else {
            let dropped: Vec<&str> = FusedSignal::all()
                .iter()
                .filter(|s| !self.enabled_signals.contains(s))
                .map(|s| s.as_str())
                .collect();
            format!("ablate-{} ({})", dropped.join("-"), self.embedding_model)
        }
    }
}

// ── Retrieval backend trait ─────────────────────────────────────────────────

/// Ranked results for a single query.
#[derive(Debug, Clone)]
pub struct RankedResults {
    /// Ranked symbol identifiers (best first).
    pub ranked_symbols: Vec<String>,
    /// Ranked file paths (best first).
    pub ranked_files: Vec<String>,
    /// Query latency (wall-clock).
    pub latency: Duration,
}

/// Trait abstracting the retrieval backend for the harness.
///
/// The actual LeIndex search engine implements this trait. For testing,
/// [`MockBackend`] provides deterministic synthetic rankings.
pub trait RetrievalBackend {
    /// Index the corpus content.
    ///
    /// The corpus cases contain queries and labels; the backend indexes the
    /// underlying source code that the queries will be evaluated against.
    fn index(&mut self) -> Result<(), BackendError>;

    /// Query for a single case, returning ranked results.
    ///
    /// The profile specifies which fused signals are active. The backend
    /// should only use enabled signals in its fused ranking.
    fn query(&self, query: &str, profile: &FusedProfile) -> Result<RankedResults, BackendError>;

    /// Name of this backend (for reporting).
    fn name(&self) -> &str;
}

/// Error from the retrieval backend.
#[derive(Debug, Clone)]
pub struct BackendError(pub String);

impl std::fmt::Display for BackendError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::error::Error for BackendError {}

// ── Evaluation harness ──────────────────────────────────────────────────────

/// The fused-retrieval evaluation harness.
///
/// Takes a corpus, a retrieval backend, and a fused profile. Indexes the
/// corpus, runs all eval-split cases through the fused pipeline, computes
/// metrics, and emits a JSON report.
pub struct EvalHarness {
    corpus: Corpus,
}

impl EvalHarness {
    /// Create a new harness with the given corpus.
    pub fn new(corpus: Corpus) -> Self {
        Self { corpus }
    }

    /// Create a harness with the default built-in corpus.
    pub fn with_default_corpus() -> Result<Self, super::corpus::CorpusVerificationError> {
        let corpus = super::corpus::load_and_verify_corpus()?;
        Ok(Self { corpus })
    }

    /// Get a reference to the corpus.
    pub fn corpus(&self) -> &Corpus {
        &self.corpus
    }

    /// Run the full evaluation with the given backend and profile.
    ///
    /// Indexes the corpus content, then runs all eval-split cases through
    /// the backend's fused retrieval, computing metrics per case and aggregate.
    pub fn run(
        &self,
        backend: &mut dyn RetrievalBackend,
        profile: &FusedProfile,
    ) -> Result<HarnessReport, HarnessError> {
        // Step 1: Index corpus
        backend.index().map_err(HarnessError::Backend)?;

        // Step 2: Run eval cases
        let mut case_metrics: Vec<CaseMetrics> = Vec::new();
        let mut latencies: Vec<Duration> = Vec::new();

        for case in self.corpus.eval_cases() {
            let results = backend
                .query(&case.query, profile)
                .map_err(HarnessError::Backend)?;

            latencies.push(results.latency);

            case_metrics.push(metrics::compute_case_metrics(
                case,
                &results.ranked_symbols,
                &results.ranked_files,
            ));
        }

        // Step 3: Compute aggregate metrics
        let results = metrics::compute_eval_results(case_metrics);

        // Step 4: Compute latency statistics
        let latency_stats = compute_latency_stats(&latencies);

        Ok(HarnessReport {
            profile: profile.clone(),
            backend_name: backend.name().to_string(),
            results,
            latency: latency_stats,
            corpus_name: "default".to_string(),
            eval_case_count: self.corpus.eval_len(),
        })
    }

    /// Run fused ablation: evaluate with all signals, then separately
    /// ablate each of TF-IDF, PDG, dense, fragment, reranker.
    ///
    /// Returns a [`FusedAblationReport`] with the full profile metrics plus
    /// per-signal ablation metrics.
    pub fn run_ablation(
        &self,
        backend: &mut dyn RetrievalBackend,
        base_profile: &FusedProfile,
    ) -> Result<FusedAblationReport, HarnessError> {
        // Full profile baseline
        let full_report = self.run(backend, base_profile)?;

        // Ablate each signal individually
        let mut ablations: HashMap<FusedSignal, HarnessReport> = HashMap::new();
        for signal in FusedSignal::all() {
            let ablated_profile = FusedProfile::ablate(base_profile, signal);
            let ablated_report = self.run(backend, &ablated_profile)?;
            ablations.insert(signal, ablated_report);
        }

        Ok(FusedAblationReport {
            full: full_report,
            ablations,
            base_profile: base_profile.clone(),
        })
    }
}

/// Latency statistics.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LatencyStats {
    /// p50 latency in milliseconds.
    pub p50_ms: f64,
    /// p95 latency in milliseconds.
    pub p95_ms: f64,
    /// p99 latency in milliseconds.
    pub p99_ms: f64,
    /// Mean latency in milliseconds.
    pub mean_ms: f64,
    /// Number of queries.
    pub count: usize,
}

/// Compute p50/p95/p99 latency from a list of durations.
fn compute_latency_stats(latencies: &[Duration]) -> LatencyStats {
    if latencies.is_empty() {
        return LatencyStats {
            p50_ms: 0.0,
            p95_ms: 0.0,
            p99_ms: 0.0,
            mean_ms: 0.0,
            count: 0,
        };
    }

    let mut sorted: Vec<f64> = latencies.iter().map(|d| d.as_secs_f64() * 1000.0).collect();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let n = sorted.len();
    let pct = |p: f64| -> f64 {
        let idx = ((p / 100.0) * (n as f64 - 1.0)).round() as usize;
        sorted[idx.min(n - 1)]
    };

    let mean: f64 = sorted.iter().sum::<f64>() / n as f64;

    LatencyStats {
        p50_ms: pct(50.0),
        p95_ms: pct(95.0),
        p99_ms: pct(99.0),
        mean_ms: mean,
        count: n,
    }
}

// ── Reports ─────────────────────────────────────────────────────────────────

/// A single harness run report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessReport {
    /// The fused profile used.
    pub profile: FusedProfile,
    /// Backend name.
    pub backend_name: String,
    /// Evaluation results (metrics).
    pub results: EvalResults,
    /// Latency statistics.
    pub latency: LatencyStats,
    /// Corpus name.
    pub corpus_name: String,
    /// Number of eval cases.
    pub eval_case_count: usize,
}

impl HarnessReport {
    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }
}

/// Fused ablation report: per-signal ablation results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FusedAblationReport {
    /// Full profile (all signals enabled) report.
    pub full: HarnessReport,
    /// Per-signal ablation reports.
    pub ablations: HashMap<FusedSignal, HarnessReport>,
    /// The base profile used for ablation.
    pub base_profile: FusedProfile,
}

impl FusedAblationReport {
    /// Serialize to JSON string.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    /// Get the MRR@10 contribution of a signal (full - ablated).
    ///
    /// Positive value means the signal contributes positively.
    pub fn signal_contribution_mrr10(&self, signal: FusedSignal) -> f64 {
        let full_mrr = self.full.results.aggregate.mean_mrr_10;
        let ablated_mrr = self
            .ablations
            .get(&signal)
            .map(|r| r.results.aggregate.mean_mrr_10)
            .unwrap_or(full_mrr);
        full_mrr - ablated_mrr
    }

    /// Generate a concise text summary of the ablation.
    pub fn summary(&self) -> report::AblationSummary {
        let mut signal_impacts: Vec<(String, f64)> = FusedSignal::all()
            .iter()
            .map(|s| (s.as_str().to_string(), self.signal_contribution_mrr10(*s)))
            .collect();
        signal_impacts.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        report::AblationSummary {
            base_model: self.base_profile.embedding_model.clone(),
            full_mrr10: self.full.results.aggregate.mean_mrr_10,
            full_recall10: self.full.results.aggregate.mean_recall_10,
            signal_impacts_mrr10: signal_impacts,
        }
    }
}

/// Error from the harness.
#[derive(Debug)]
pub enum HarnessError {
    /// Backend error.
    Backend(BackendError),
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(e) => write!(f, "Backend error: {e}"),
        }
    }
}

impl std::error::Error for HarnessError {}

// ── Mock backend for testing ────────────────────────────────────────────────

/// A mock retrieval backend for testing the harness.
///
/// Produces deterministic synthetic rankings based on simple string matching
/// and the enabled fused signals. This lets tests verify the harness logic
/// without needing the full LeIndex search engine.
pub struct MockBackend {
    /// Map from query to expected ranked results.
    mock_results: HashMap<String, RankedResults>,
    /// Signal quality factors: if a signal is ablated, quality drops.
    signal_quality: HashMap<FusedSignal, f64>,
}

impl MockBackend {
    /// Create a new mock backend.
    pub fn new() -> Self {
        Self {
            mock_results: HashMap::new(),
            signal_quality: HashMap::new(),
        }
    }

    /// Register a query-to-results mapping.
    pub fn register(&mut self, query: &str, results: RankedResults) {
        self.mock_results.insert(query.to_string(), results);
    }

    /// Set the quality contribution of a signal (0.0 to 1.0).
    ///
    /// When a signal is ablated, the mock simulates quality drop by degrading
    /// the ranking proportional to the quality contribution.
    pub fn set_signal_quality(&mut self, signal: FusedSignal, quality: f64) {
        self.signal_quality.insert(signal, quality);
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl RetrievalBackend for MockBackend {
    fn index(&mut self) -> Result<(), BackendError> {
        Ok(()) // No-op
    }

    fn query(&self, query: &str, profile: &FusedProfile) -> Result<RankedResults, BackendError> {
        let results = self
            .mock_results
            .get(query)
            .cloned()
            .unwrap_or(RankedResults {
                ranked_symbols: Vec::new(),
                ranked_files: Vec::new(),
                latency: Duration::from_micros(100),
            });

        // Simulate quality degradation from ablation: if the reranker is
        // ablated, shuffle results a bit (for testing ablation logic)
        if !profile.is_enabled(FusedSignal::Reranker) && results.ranked_symbols.len() > 1 {
            let reranker_quality = self
                .signal_quality
                .get(&FusedSignal::Reranker)
                .copied()
                .unwrap_or(0.9);
            if reranker_quality < 1.0 {
                // Simulate reranker removal: swap first two results with some probability
                let mut degraded = results.ranked_symbols.clone();
                degraded.swap(0, 1);
                return Ok(RankedResults {
                    ranked_symbols: degraded,
                    ranked_files: results.ranked_files,
                    latency: results.latency,
                });
            }
        }

        Ok(results)
    }

    fn name(&self) -> &str {
        "mock-backend"
    }
}

// ── Corpus evaluation helper ────────────────────────────────────────────────

/// Register mock results for all eval cases in the corpus.
///
/// This helper sets up a mock backend that returns the expected relevant
/// symbols as the ranked results (perfect retrieval), which is useful for
/// testing metric computation.
pub fn register_perfect_mock(backend: &mut MockBackend, corpus: &Corpus) {
    for case in corpus.eval_cases() {
        backend.register(
            &case.query,
            RankedResults {
                ranked_symbols: case.relevant_symbols.clone(),
                ranked_files: case.relevant_files.clone(),
                latency: Duration::from_micros(100),
            },
        );
    }
}

/// Register mock results with some noise (distractors mixed in) for all
/// eval cases.
pub fn register_noisy_mock(backend: &mut MockBackend, corpus: &Corpus, noise_items: &[&str]) {
    for case in corpus.eval_cases() {
        // Put relevant symbols first, then intersperse noise
        let mut ranked = case.relevant_symbols.clone();
        for noise in noise_items {
            ranked.push(noise.to_string());
        }
        backend.register(
            &case.query,
            RankedResults {
                ranked_symbols: ranked,
                ranked_files: case.relevant_files.clone(),
                latency: Duration::from_micros(100),
            },
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eval::corpus::{EvalCategory, Language, Split};

    #[test]
    fn test_fused_profile_default_all_signals() {
        let profile = FusedProfile::default();
        for signal in FusedSignal::all() {
            assert!(profile.is_enabled(signal));
        }
    }

    #[test]
    fn test_fused_profile_ablate_one() {
        let base = FusedProfile::default();
        let ablated = FusedProfile::ablate(&base, FusedSignal::Reranker);
        assert!(!ablated.is_enabled(FusedSignal::Reranker));
        assert!(ablated.is_enabled(FusedSignal::Tfidf));
        assert!(ablated.is_enabled(FusedSignal::Dense));
    }

    #[test]
    fn test_fused_profile_no_reranker() {
        let profile = FusedProfile::no_reranker();
        assert!(!profile.is_enabled(FusedSignal::Reranker));
        assert!(profile.is_enabled(FusedSignal::Tfidf));
        assert!(profile.is_enabled(FusedSignal::Dense));
        assert_eq!(profile.reranker_model, "none");
    }

    #[test]
    fn test_fused_profile_label() {
        let full = FusedProfile::default();
        assert!(full.label().contains("full-fused"));

        let ablated = FusedProfile::ablate(&full, FusedSignal::Reranker);
        assert!(ablated.label().contains("ablate-reranker"));
    }

    #[test]
    fn test_latency_stats_basic() {
        let latencies = vec![
            Duration::from_millis(10),
            Duration::from_millis(20),
            Duration::from_millis(30),
            Duration::from_millis(40),
            Duration::from_millis(50),
            Duration::from_millis(60),
            Duration::from_millis(70),
            Duration::from_millis(80),
            Duration::from_millis(90),
            Duration::from_millis(100),
        ];
        let stats = compute_latency_stats(&latencies);
        assert_eq!(stats.count, 10);
        assert!(stats.p50_ms > 0.0);
        assert!(stats.p95_ms >= stats.p50_ms);
        assert!(stats.p99_ms >= stats.p95_ms);
    }

    #[test]
    fn test_latency_stats_empty() {
        let stats = compute_latency_stats(&[]);
        assert_eq!(stats.count, 0);
        assert_eq!(stats.p50_ms, 0.0);
    }

    #[test]
    fn test_mock_backend_query() {
        let mut backend = MockBackend::new();
        backend.register(
            "hello",
            RankedResults {
                ranked_symbols: vec!["func_a".to_string()],
                ranked_files: vec!["src/a.rs".to_string()],
                latency: Duration::from_micros(50),
            },
        );
        let profile = FusedProfile::default();
        let results = backend.query("hello", &profile).expect("query");
        assert_eq!(results.ranked_symbols, vec!["func_a".to_string()]);
    }

    #[test]
    fn test_mock_backend_unknown_query() {
        let backend = MockBackend::new();
        let profile = FusedProfile::default();
        let results = backend.query("unknown", &profile).expect("query");
        assert!(results.ranked_symbols.is_empty());
    }

    #[test]
    fn test_harness_run_with_perfect_mock() {
        let mut corpus = Corpus::new();
        // Add minimal cases with at least eval+train for one category
        for cat in EvalCategory::all() {
            for i in 0..2 {
                corpus.add_case(crate::eval::corpus::CorpusCase {
                    id: format!("{}_{i}", cat.as_str()),
                    query: format!("query_{cat:?}_{i}"),
                    category: cat,
                    split: if i == 0 { Split::Train } else { Split::Eval },
                    relevant_symbols: vec![format!("sym_{cat:?}_{i}")],
                    relevant_files: vec![format!("src/{cat:?}_{i}.rs")],
                    languages: vec![Language::Rust],
                    expected_position: None,
                    notes: None,
                    hard_negatives: Vec::new(),
                });
            }
        }

        let mut backend = MockBackend::new();
        register_perfect_mock(&mut backend, &corpus);

        let harness = EvalHarness::new(corpus);
        let profile = FusedProfile::default();
        let report = harness.run(&mut backend, &profile).expect("run");

        assert_eq!(report.eval_case_count, 14);
        assert!((report.results.aggregate.mean_recall_1 - 1.0).abs() < 1e-10);
        assert!((report.results.aggregate.mean_mrr_10 - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_harness_run_ablation() {
        let mut corpus = Corpus::new();
        for cat in EvalCategory::all() {
            for i in 0..2 {
                corpus.add_case(crate::eval::corpus::CorpusCase {
                    id: format!("{}_{i}", cat.as_str()),
                    query: format!("query_{cat:?}_{i}"),
                    category: cat,
                    split: if i == 0 { Split::Train } else { Split::Eval },
                    relevant_symbols: vec![format!("sym_{cat:?}_{i}")],
                    relevant_files: vec![format!("src/{cat:?}_{i}.rs")],
                    languages: vec![Language::Rust],
                    expected_position: None,
                    notes: None,
                    hard_negatives: Vec::new(),
                });
            }
        }

        let mut backend = MockBackend::new();
        register_perfect_mock(&mut backend, &corpus);
        // Set reranker to moderate quality so ablation shows some effect
        backend.set_signal_quality(FusedSignal::Reranker, 0.8);

        let harness = EvalHarness::new(corpus);
        let profile = FusedProfile::default();
        let ablation = harness
            .run_ablation(&mut backend, &profile)
            .expect("ablation");

        // Full profile should have perfect MRR
        assert!(
            (ablation.full.results.aggregate.mean_mrr_10 - 1.0).abs() < 1e-10,
            "Perfect mock should give MRR@10 = 1.0"
        );

        // Should have ablations for all 5 signals
        assert_eq!(ablation.ablations.len(), 5);
    }

    #[test]
    fn test_harness_report_json_serialization() {
        let report = HarnessReport {
            profile: FusedProfile::default(),
            backend_name: "test".to_string(),
            results: metrics::compute_eval_results(vec![]),
            latency: LatencyStats {
                p50_ms: 1.0,
                p95_ms: 5.0,
                p99_ms: 10.0,
                mean_ms: 2.0,
                count: 10,
            },
            corpus_name: "test".to_string(),
            eval_case_count: 10,
        };
        let json = report.to_json().expect("json");
        assert!(json.contains("\"backend_name\""));
    }

    #[test]
    fn test_signal_contribution() {
        let mut corpus = super::super::corpus::build_default_corpus();
        let _ = &mut corpus; // use default corpus
        let mut backend = MockBackend::new();
        super::register_perfect_mock(&mut backend, &corpus);
        let harness = EvalHarness::new(corpus);
        let profile = FusedProfile::default();
        let ablation = harness
            .run_ablation(&mut backend, &profile)
            .expect("ablation");

        // All signals should have contribution >= 0 with perfect mock (no degradation)
        for signal in FusedSignal::all() {
            let contribution = ablation.signal_contribution_mrr10(signal);
            assert!(
                contribution.abs() < 1e-10,
                "Signal {signal:?} contribution should be ~0 with perfect mock, got {contribution}"
            );
        }
    }

    #[test]
    fn test_ablation_summary() {
        let corpus = super::super::corpus::build_default_corpus();
        let mut backend = MockBackend::new();
        register_perfect_mock(&mut backend, &corpus);
        let harness = EvalHarness::new(corpus);
        let profile = FusedProfile::default();
        let ablation = harness
            .run_ablation(&mut backend, &profile)
            .expect("ablation");

        let summary = ablation.summary();
        assert!(!summary.base_model.is_empty());
        assert!((summary.full_mrr10 - 1.0).abs() < 1e-10);
        assert_eq!(summary.signal_impacts_mrr10.len(), 5);
    }

    #[test]
    fn test_register_noisy_mock() {
        let mut corpus = super::super::corpus::build_default_corpus();
        let mut backend = MockBackend::new();
        register_noisy_mock(&mut backend, &corpus, &["noise1", "noise2"]);

        // Check a random case was registered with noise
        let case = corpus.eval_cases().next().expect("at least one case");
        let profile = FusedProfile::default();
        let results = backend.query(&case.query, &profile).expect("query");
        assert!(results.ranked_symbols.contains(&"noise1".to_string()));
        assert!(results.ranked_symbols.contains(&"noise2".to_string()));

        let _ = &mut corpus;
    }
}
