//! Predeclared acceptance gates and variance bands (spec section 9.4).
//!
//! ## Design Principle
//!
//! Gates are committed and published BEFORE any candidate model is evaluated.
//! This prevents cherry-picking tolerances after seeing results, which would
//! undermine the statistical validity of the evaluation.
//!
//! ## Variance Bands
//!
//! Variance bands are derived from baseline (FP16 Qwen3) repeated runs (5x).
//! The observed standard deviation sets the allowed noise envelope. A candidate
//! is only penalized for regression beyond the variance band, not for noise
//! within it.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::Duration;

// ── Gates ───────────────────────────────────────────────────────────────────

/// Predeclared acceptance gates for model evaluation (spec section 9.4).
///
/// These thresholds are committed BEFORE any candidate model is evaluated.
/// A candidate that violates any gate fails acceptance.
///
/// Variance bands are computed from baseline (FP16 Qwen3) repeated runs.
/// The allowed regression is the gate threshold PLUS the variance band width,
/// so a candidate is penalized only for regression beyond statistical noise.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Gates {
    /// Maximum allowed aggregate MRR@10 regression (absolute delta).
    /// 0.0 = no regression allowed beyond variance band.
    pub aggregate_mrr10_max_regression: f64,

    /// Maximum allowed protected-category regression in percentage points.
    /// 1.0 = one percentage point. Categories flagged as "protected" are
    /// those where per-category regression matters most (e.g., hard negatives,
    /// changed/deleted/freshness).
    pub protected_category_max_regression_pp: f64,

    /// Maximum allowed p95 latency regression as a percentage.
    /// 0.0 = no latency regression allowed.
    /// 10.0 = candidate p95 may be up to 10% slower than baseline.
    pub p95_latency_max_regression_pct: f64,

    /// Maximum allowed index wall-time regression as a percentage.
    /// 0.0 = no wall-time regression allowed.
    pub wall_time_max_regression_pct: f64,

    /// If true, any new zero-result case on a labeled query is an automatic
    /// gate failure (spec section 9.4: "No new zero-result cases").
    pub zero_result_forbidden: bool,

    /// Variance bands derived from baseline repeated runs.
    /// Map from metric name to observed standard deviation.
    pub variance_bands: VarianceBands,

    /// Timestamp recording when gates were committed (for audit trail).
    pub committed_at: chrono::DateTime<chrono::Utc>,
}

/// Variance bands derived from baseline (FP16 Qwen3) repeated runs.
///
/// Each entry maps a metric key to its observed standard deviation across
/// repeated baseline runs. The gate allows regression up to the gate threshold
/// plus the variance band width, so noise within the band is not penalized.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct VarianceBands {
    /// Stddev of aggregate MRR@10 across baseline runs.
    pub aggregate_mrr10_stddev: f64,

    /// Stddev of aggregate Recall@10 across baseline runs.
    pub aggregate_recall10_stddev: f64,

    /// Per-category MRR@10 stddevs.
    pub per_category_mrr10_stddev: HashMap<String, f64>,

    /// Stddev of p95 latency (in milliseconds).
    pub p95_latency_stddev_ms: f64,

    /// Stddev of index wall-time (in seconds).
    pub wall_time_stddev_secs: f64,

    /// Number of baseline runs used to compute these bands.
    pub baseline_run_count: usize,
}

impl Default for Gates {
    fn default() -> Self {
        Self {
            // Section 9.4: "No statistically meaningful aggregate MRR@10 regression"
            // Default to zero allowed regression beyond variance band.
            aggregate_mrr10_max_regression: 0.0,
            // Section 9.4: "No protected category regression > 1pp"
            protected_category_max_regression_pp: 1.0,
            // Section 9.4: "Search p95 no worse than baseline"
            p95_latency_max_regression_pct: 0.0,
            // Section 9.4: "Index wall time no worse than baseline"
            wall_time_max_regression_pct: 0.0,
            // Section 9.4: "No new zero-result cases"
            zero_result_forbidden: true,
            variance_bands: VarianceBands::default(),
            committed_at: chrono::Utc::now(),
        }
    }
}

impl Gates {
    /// Create gates with variance bands computed from baseline repeated runs.
    ///
    /// Per spec section 9.4: "Exact gates should use baseline variance from
    /// repeated runs."
    pub fn with_variance_bands(mut self, bands: VarianceBands) -> Self {
        self.variance_bands = bands;
        self
    }

    /// Check whether a candidate's results pass all acceptance gates.
    ///
    /// Returns `Ok(())` if the candidate passes all gates, or `Err(GateFailure)`
    /// with a description of which gate was violated and by how much.
    ///
    /// The variance band is ADDED to the gate threshold before comparison,
    /// so regression within the band is not penalized (it could be noise).
    pub fn check(&self, results: &GateCandidateResults) -> Result<(), GateFailure> {
        // Aggregate MRR@10 regression check
        let mrr_threshold =
            self.aggregate_mrr10_max_regression + self.variance_bands.aggregate_mrr10_stddev;
        if results.aggregate_mrr10_regression > mrr_threshold {
            return Err(GateFailure {
                gate: GateType::AggregateMrr10Regression,
                threshold: mrr_threshold,
                observed: results.aggregate_mrr10_regression,
                message: format!(
                    "Aggregate MRR@10 regression {:.6} exceeds allowed {:.6} \
                     (gate {:.6} + variance band {:.6})",
                    results.aggregate_mrr10_regression,
                    mrr_threshold,
                    self.aggregate_mrr10_max_regression,
                    self.variance_bands.aggregate_mrr10_stddev,
                ),
            });
        }

        // Protected-category regression check
        let protected_threshold = self.protected_category_max_regression_pp / 100.0; // Convert pp to fraction
        for (category, regression) in &results.per_category_mrr10_regression {
            if self.is_protected_category(category) {
                let band = self
                    .variance_bands
                    .per_category_mrr10_stddev
                    .get(category)
                    .copied()
                    .unwrap_or(0.0);
                let allowed = protected_threshold + band;
                if *regression > allowed {
                    return Err(GateFailure {
                        gate: GateType::ProtectedCategoryRegression(category.clone()),
                        threshold: allowed,
                        observed: *regression,
                        message: format!(
                            "Protected category '{}' MRR@10 regression {:.4} ({}pp) \
                             exceeds allowed {:.4} ({:.1}pp + band {:.4})",
                            category,
                            *regression,
                            *regression * 100.0,
                            allowed,
                            self.protected_category_max_regression_pp,
                            band,
                        ),
                    });
                }
            }
        }

        // p95 latency regression check
        if results.p95_latency_regression_pct > self.p95_latency_max_regression_pct {
            return Err(GateFailure {
                gate: GateType::P95LatencyRegression,
                threshold: self.p95_latency_max_regression_pct,
                observed: results.p95_latency_regression_pct,
                message: format!(
                    "p95 latency regression {:.2}% exceeds allowed {:.2}%",
                    results.p95_latency_regression_pct, self.p95_latency_max_regression_pct,
                ),
            });
        }

        // Wall-time regression check
        if results.wall_time_regression_pct > self.wall_time_max_regression_pct {
            return Err(GateFailure {
                gate: GateType::WallTimeRegression,
                threshold: self.wall_time_max_regression_pct,
                observed: results.wall_time_regression_pct,
                message: format!(
                    "Index wall-time regression {:.2}% exceeds allowed {:.2}%",
                    results.wall_time_regression_pct, self.wall_time_max_regression_pct,
                ),
            });
        }

        // Zero-result check
        if self.zero_result_forbidden && results.new_zero_result_count > 0 {
            return Err(GateFailure {
                gate: GateType::ZeroResult,
                threshold: 0.0,
                observed: results.new_zero_result_count as f64,
                message: format!(
                    "Candidate produced {} new zero-result case(s); \
                     zero-result cases are forbidden (section 9.4)",
                    results.new_zero_result_count,
                ),
            });
        }

        Ok(())
    }

    /// Returns true if the given category is a "protected" category.
    ///
    /// Protected categories are ones where per-category regression is
    /// especially important to catch (spec section 9.4). Currently:
    /// - `hard_negatives` (syntactic near-misses must not degrade)
    /// - `changed_deleted_freshness` (freshness-sensitive queries must not degrade)
    /// - `same_name_different_behavior` (disambiguation must not degrade)
    ///
    /// Additional categories can be added to the protected set by extending
    /// this list.
    fn is_protected_category(&self, category: &str) -> bool {
        matches!(
            category,
            "hard_negatives" | "changed_deleted_freshness" | "same_name_different_behavior"
        )
    }
}

// ── Gate check result types ─────────────────────────────────────────────────

/// Which gate was violated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum GateType {
    /// Aggregate MRR@10 regression.
    AggregateMrr10Regression,
    /// Protected-category regression (contains category name).
    ProtectedCategoryRegression(String),
    /// p95 latency regression.
    P95LatencyRegression,
    /// Index wall-time regression.
    WallTimeRegression,
    /// Zero-result on a labeled query.
    ZeroResult,
}

/// A candidate's evaluation results checked against gates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateCandidateResults {
    /// Aggregate MRR@10 regression (baseline - candidate, positive = regression).
    pub aggregate_mrr10_regression: f64,

    /// Per-category MRR@10 regression (positive = regression).
    pub per_category_mrr10_regression: HashMap<String, f64>,

    /// p95 latency regression percentage (positive = slower).
    pub p95_latency_regression_pct: f64,

    /// Index wall-time regression percentage (positive = slower).
    pub wall_time_regression_pct: f64,

    /// Count of queries that returned zero results but had labeled answers.
    pub new_zero_result_count: usize,
}

/// A gate failure.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateFailure {
    /// Which gate was violated.
    pub gate: GateType,
    /// The threshold (gate + variance band) that was exceeded.
    pub threshold: f64,
    /// The observed value.
    pub observed: f64,
    /// Human-readable description.
    pub message: String,
}

impl std::fmt::Display for GateFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for GateFailure {}

// ── Baseline variance computation ───────────────────────────────────────────

/// Raw metrics from a single baseline run, used to compute variance bands.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BaselineRunMetrics {
    /// Aggregate MRR@10 for this run.
    pub aggregate_mrr10: f64,
    /// Aggregate Recall@10 for this run.
    pub aggregate_recall10: f64,
    /// Per-category MRR@10 for this run.
    pub per_category_mrr10: HashMap<String, f64>,
    /// p95 latency in milliseconds.
    pub p95_latency_ms: f64,
    /// Index wall-time in seconds.
    pub wall_time_secs: f64,
}

/// Compute variance bands from baseline repeated runs.
///
/// Takes a vector of per-run metrics (typically 5 runs of FP16 Qwen3) and
/// computes the standard deviation for each metric. These bands are then
/// attached to the [`Gates`] struct to set allowed noise envelopes.
pub fn compute_variance_bands(runs: &[BaselineRunMetrics]) -> VarianceBands {
    if runs.is_empty() {
        return VarianceBands::default();
    }

    let n = runs.len() as f64;

    // Compute mean MRR@10
    let mrr_mean = runs.iter().map(|r| r.aggregate_mrr10).sum::<f64>() / n;
    let mrr_variance = runs
        .iter()
        .map(|r| (r.aggregate_mrr10 - mrr_mean).powi(2))
        .sum::<f64>()
        / n;
    let mrr_stddev = mrr_variance.sqrt();

    // Compute mean Recall@10
    let recall_mean = runs.iter().map(|r| r.aggregate_recall10).sum::<f64>() / n;
    let recall_variance = runs
        .iter()
        .map(|r| (r.aggregate_recall10 - recall_mean).powi(2))
        .sum::<f64>()
        / n;
    let recall_stddev = recall_variance.sqrt();

    // Compute per-category MRR@10 stddevs
    let mut per_category_stddev: HashMap<String, f64> = HashMap::new();
    let category_names: std::collections::HashSet<&String> = runs
        .iter()
        .flat_map(|r| r.per_category_mrr10.keys())
        .collect();
    for cat in category_names {
        let values: Vec<f64> = runs
            .iter()
            .filter_map(|r| r.per_category_mrr10.get(cat))
            .copied()
            .collect();
        if values.len() > 1 {
            let mean = values.iter().sum::<f64>() / values.len() as f64;
            let variance =
                values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
            per_category_stddev.insert(cat.clone(), variance.sqrt());
        }
    }

    // Compute p95 latency stddev
    let p95_mean = runs.iter().map(|r| r.p95_latency_ms).sum::<f64>() / n;
    let p95_variance = runs
        .iter()
        .map(|r| (r.p95_latency_ms - p95_mean).powi(2))
        .sum::<f64>()
        / n;
    let p95_stddev = p95_variance.sqrt();

    // Compute wall-time stddev
    let wall_mean = runs.iter().map(|r| r.wall_time_secs).sum::<f64>() / n;
    let wall_variance = runs
        .iter()
        .map(|r| (r.wall_time_secs - wall_mean).powi(2))
        .sum::<f64>()
        / n;
    let wall_stddev = wall_variance.sqrt();

    VarianceBands {
        aggregate_mrr10_stddev: mrr_stddev,
        aggregate_recall10_stddev: recall_stddev,
        per_category_mrr10_stddev: per_category_stddev,
        p95_latency_stddev_ms: p95_stddev,
        wall_time_stddev_secs: wall_stddev,
        baseline_run_count: runs.len(),
    }
}

/// Run a timed computation and return the elapsed duration.
///
/// This is a utility for measuring wall time during baseline/candidate runs.
pub fn time_execution<F, R>(f: F) -> (R, Duration)
where
    F: FnOnce() -> R,
{
    let start = std::time::Instant::now();
    let result = f();
    (result, start.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_gates_default_values() {
        let gates = Gates::default();
        assert_eq!(gates.aggregate_mrr10_max_regression, 0.0);
        assert_eq!(gates.protected_category_max_regression_pp, 1.0);
        assert_eq!(gates.p95_latency_max_regression_pct, 0.0);
        assert_eq!(gates.wall_time_max_regression_pct, 0.0);
        assert!(gates.zero_result_forbidden);
    }

    #[test]
    fn test_gates_serialize_roundtrip() {
        let gates = Gates::default();
        let json = serde_json::to_string(&gates).expect("serialize gates");
        let deserialized: Gates = serde_json::from_str(&json).expect("deserialize gates");
        assert_eq!(
            deserialized.aggregate_mrr10_max_regression,
            gates.aggregate_mrr10_max_regression
        );
        assert_eq!(
            deserialized.protected_category_max_regression_pp,
            gates.protected_category_max_regression_pp
        );
    }

    #[test]
    fn test_candidate_passing_all_gates() {
        let gates = Gates::default();
        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.001, // tiny regression
            per_category_mrr10_regression: HashMap::new(),
            p95_latency_regression_pct: 0.5,
            wall_time_regression_pct: 0.3,
            new_zero_result_count: 0,
        };
        // With no variance bands, the threshold is 0.0, so any positive
        // MRR regression fails. Let's use 0.0 for a guaranteed pass.
        let results_pass = GateCandidateResults {
            aggregate_mrr10_regression: 0.0,
            per_category_mrr10_regression: HashMap::new(),
            p95_latency_regression_pct: 0.0,
            wall_time_regression_pct: 0.0,
            new_zero_result_count: 0,
        };
        assert!(gates.check(&results_pass).is_ok());
        // Check that tiny regression fails without variance band
        assert!(gates.check(&results).is_err());
    }

    #[test]
    fn test_mrr_regression_fails_gate() {
        let gates = Gates::default();
        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.05, // 5% regression
            per_category_mrr10_regression: HashMap::new(),
            p95_latency_regression_pct: 0.0,
            wall_time_regression_pct: 0.0,
            new_zero_result_count: 0,
        };
        let failure = gates.check(&results).expect_err("should fail");
        assert_eq!(failure.gate, GateType::AggregateMrr10Regression);
    }

    #[test]
    fn test_protected_category_regression_fails() {
        let gates = Gates::default();
        let mut per_cat = HashMap::new();
        // 2pp regression on hard_negatives (a protected category)
        per_cat.insert("hard_negatives".to_string(), 0.02);
        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.0,
            per_category_mrr10_regression: per_cat,
            p95_latency_regression_pct: 0.0,
            wall_time_regression_pct: 0.0,
            new_zero_result_count: 0,
        };
        let failure = gates.check(&results).expect_err("should fail");
        assert!(matches!(
            failure.gate,
            GateType::ProtectedCategoryRegression(ref c) if c == "hard_negatives"
        ));
    }

    #[test]
    fn test_non_protected_category_regression_passes() {
        let gates = Gates::default();
        let mut per_cat = HashMap::new();
        // 2pp regression on NL-to-symbol (not a protected category)
        per_cat.insert("nl_to_symbol".to_string(), 0.02);
        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.0,
            per_category_mrr10_regression: per_cat,
            p95_latency_regression_pct: 0.0,
            wall_time_regression_pct: 0.0,
            new_zero_result_count: 0,
        };
        assert!(gates.check(&results).is_ok());
    }

    #[test]
    fn test_zero_result_fails() {
        let gates = Gates::default();
        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.0,
            per_category_mrr10_regression: HashMap::new(),
            p95_latency_regression_pct: 0.0,
            wall_time_regression_pct: 0.0,
            new_zero_result_count: 1,
        };
        let failure = gates.check(&results).expect_err("should fail");
        assert!(matches!(failure.gate, GateType::ZeroResult));
    }

    #[test]
    fn test_variance_band_allows_noise() {
        // With a variance band, small regressions within the band are OK.
        let gates = Gates {
            aggregate_mrr10_max_regression: 0.0,
            ..Gates::default()
        }
        .with_variance_bands(VarianceBands {
            aggregate_mrr10_stddev: 0.01,
            ..VarianceBands::default()
        });

        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.005, // Within 0.01 band
            per_category_mrr10_regression: HashMap::new(),
            p95_latency_regression_pct: 0.0,
            wall_time_regression_pct: 0.0,
            new_zero_result_count: 0,
        };
        assert!(gates.check(&results).is_ok());

        // Just outside the band
        let results_outside = GateCandidateResults {
            aggregate_mrr10_regression: 0.015, // Beyond 0.01 band
            ..results
        };
        assert!(gates.check(&results_outside).is_err());
    }

    #[test]
    fn test_latency_regression_fails() {
        let gates = Gates::default();
        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.0,
            per_category_mrr10_regression: HashMap::new(),
            p95_latency_regression_pct: 10.0,
            wall_time_regression_pct: 0.0,
            new_zero_result_count: 0,
        };
        let failure = gates.check(&results).expect_err("should fail");
        assert_eq!(failure.gate, GateType::P95LatencyRegression);
    }

    #[test]
    fn test_wall_time_regression_fails() {
        let gates = Gates::default();
        let results = GateCandidateResults {
            aggregate_mrr10_regression: 0.0,
            per_category_mrr10_regression: HashMap::new(),
            p95_latency_regression_pct: 0.0,
            wall_time_regression_pct: 15.0,
            new_zero_result_count: 0,
        };
        let failure = gates.check(&results).expect_err("should fail");
        assert_eq!(failure.gate, GateType::WallTimeRegression);
    }

    #[test]
    fn test_compute_variance_bands_from_runs() {
        let runs = vec![
            BaselineRunMetrics {
                aggregate_mrr10: 0.80,
                aggregate_recall10: 0.85,
                per_category_mrr10: HashMap::new(),
                p95_latency_ms: 100.0,
                wall_time_secs: 10.0,
            },
            BaselineRunMetrics {
                aggregate_mrr10: 0.82,
                aggregate_recall10: 0.87,
                per_category_mrr10: HashMap::new(),
                p95_latency_ms: 105.0,
                wall_time_secs: 10.5,
            },
            BaselineRunMetrics {
                aggregate_mrr10: 0.81,
                aggregate_recall10: 0.86,
                per_category_mrr10: HashMap::new(),
                p95_latency_ms: 102.0,
                wall_time_secs: 10.2,
            },
            BaselineRunMetrics {
                aggregate_mrr10: 0.795,
                aggregate_recall10: 0.845,
                per_category_mrr10: HashMap::new(),
                p95_latency_ms: 98.0,
                wall_time_secs: 9.9,
            },
            BaselineRunMetrics {
                aggregate_mrr10: 0.805,
                aggregate_recall10: 0.855,
                per_category_mrr10: HashMap::new(),
                p95_latency_ms: 101.0,
                wall_time_secs: 10.1,
            },
        ];

        let bands = compute_variance_bands(&runs);
        assert_eq!(bands.baseline_run_count, 5);
        // Stddev should be small but positive for non-identical values
        assert!(bands.aggregate_mrr10_stddev > 0.0);
        assert!(bands.aggregate_mrr10_stddev < 0.02); // Should be small
        assert!(bands.p95_latency_stddev_ms > 0.0);
    }

    #[test]
    fn test_compute_variance_bands_empty() {
        let bands = compute_variance_bands(&[]);
        assert_eq!(bands.baseline_run_count, 0);
        assert_eq!(bands.aggregate_mrr10_stddev, 0.0);
    }

    #[test]
    fn test_protected_categories() {
        let gates = Gates::default();
        assert!(gates.is_protected_category("hard_negatives"));
        assert!(gates.is_protected_category("changed_deleted_freshness"));
        assert!(gates.is_protected_category("same_name_different_behavior"));
        assert!(!gates.is_protected_category("nl_to_symbol"));
        assert!(!gates.is_protected_category("concept_to_impl"));
    }

    #[test]
    fn test_time_execution() {
        let (_result, elapsed) = time_execution(|| {
            std::thread::sleep(std::time::Duration::from_millis(10));
            42
        });
        assert_eq!(_result, 42);
        assert!(elapsed >= std::time::Duration::from_millis(8));
    }
}
