//! INT8 quantization quality thresholds, reports, and promotion gating.

use super::*;

/// These thresholds come from the binding spec §6.7.
#[derive(Debug, Clone)]
pub struct Int8QualityThresholds {
    /// Maximum allowed NDCG@10 drop (fraction, e.g. 0.01 = 1%).
    pub ndcg10_max_drop: f64,
    /// Maximum allowed p50 latency increase (fraction, e.g. 0.05 = 5%).
    pub p50_max_increase: f64,
    /// Maximum allowed p99 latency increase (fraction, e.g. 0.10 = 10%).
    pub p99_max_increase: f64,
}

impl Default for Int8QualityThresholds {
    fn default() -> Self {
        Self {
            ndcg10_max_drop: 0.01,
            p50_max_increase: 0.05,
            p99_max_increase: 0.10,
        }
    }
}

/// Result of comparing INT8 search quality against an FP32 baseline.
///
/// Each field reports the measured metric and whether it passed the
/// corresponding threshold.
#[derive(Debug, Clone)]
pub struct Int8QualityReport {
    /// NDCG@10 measured on the FP32 baseline.
    pub baseline_ndcg10: f64,
    /// NDCG@10 measured on the INT8 path.
    pub int8_ndcg10: f64,
    /// NDCG@10 drop (baseline − INT8). Positive means INT8 is worse.
    pub ndcg10_drop: f64,
    /// Whether the NDCG@10 drop is within the allowed threshold.
    pub ndcg10_passed: bool,

    /// p50 latency on the FP32 baseline (nanoseconds).
    pub baseline_p50_ns: u64,
    /// p50 latency on the INT8 path (nanoseconds).
    pub int8_p50_ns: u64,
    /// p50 latency increase fraction.
    pub p50_increase: f64,
    /// Whether p50 latency is within the allowed threshold.
    pub p50_passed: bool,

    /// p99 latency on the FP32 baseline (nanoseconds).
    pub baseline_p99_ns: u64,
    /// p99 latency on the INT8 path (nanoseconds).
    pub int8_p99_ns: u64,
    /// p99 latency increase fraction.
    pub p99_increase: f64,
    /// Whether p99 latency is within the allowed threshold.
    pub p99_passed: bool,

    /// Overall pass: `true` only when all three thresholds pass.
    pub overall_passed: bool,
}

/// INT8 quality gate.
///
/// Evaluates whether INT8 quantized search meets the quality and latency
/// thresholds required for promotion. The gate is explicit: INT8 is **not**
/// promoted unless all thresholds pass (VAL-BPHASE-030).
///
/// The FP32 comparison path remains available regardless of the gate outcome
/// (VAL-BPHASE-034).
///
/// # Usage
///
/// ```ignore
/// let gate = Int8QualityGate::new(Int8QualityThresholds::default());
/// let report = gate.evaluate(&baseline_results, &int8_results, &baseline_latencies, &int8_latencies);
/// if report.overall_passed {
///     // Safe to promote INT8
/// } else {
///     // Keep FP32 as default
/// }
/// ```
#[derive(Debug, Clone)]
pub struct Int8QualityGate {
    thresholds: Int8QualityThresholds,
}

impl Int8QualityGate {
    /// Create a new quality gate with the given thresholds.
    pub fn new(thresholds: Int8QualityThresholds) -> Self {
        Self { thresholds }
    }

    /// Create a gate with default thresholds from the spec.
    pub fn with_default_thresholds() -> Self {
        Self::new(Int8QualityThresholds::default())
    }

    /// Return the configured thresholds.
    pub fn thresholds(&self) -> &Int8QualityThresholds {
        &self.thresholds
    }

    /// Compute NDCG@10 given a ranked list of returned IDs and a set of
    /// relevant (ground-truth) IDs.
    ///
    /// `returned_ids` is ordered by rank (position 0 = rank 1).
    /// `relevant_ids` is the set of IDs that are relevant for the query.
    pub fn ndcg_at_10(returned_ids: &[String], relevant_ids: &HashSet<String>) -> f64 {
        // DCG@10
        let k = 10.min(returned_ids.len());
        let mut dcg: f64 = 0.0;
        for (i, id) in returned_ids.iter().take(k).enumerate() {
            if relevant_ids.contains(id) {
                let rank = (i + 1) as f64;
                dcg += 1.0 / (rank + 1.0).log2();
            }
        }

        // Ideal DCG@10
        let ideal_k = 10.min(relevant_ids.len());
        let mut idcg: f64 = 0.0;
        for i in 0..ideal_k {
            let rank = (i + 1) as f64;
            idcg += 1.0 / (rank + 1.0).log2();
        }

        if idcg == 0.0 { 0.0 } else { dcg / idcg }
    }

    /// Compute a latency percentile from a sorted list of nanosecond samples.
    ///
    /// `samples` must be sorted in ascending order.
    /// `percentile` is in [0.0, 1.0] (e.g. 0.5 for p50, 0.99 for p99).
    pub fn latency_percentile(sorted_samples: &[u64], percentile: f64) -> u64 {
        if sorted_samples.is_empty() {
            return 0;
        }
        let idx = ((sorted_samples.len() as f64) * percentile) as usize;
        let idx = idx.min(sorted_samples.len() - 1);
        sorted_samples[idx]
    }

    /// Evaluate the INT8 quality gate against measured metrics.
    ///
    /// Returns an [`Int8QualityReport`] indicating whether each threshold
    /// passed and whether the overall gate passed.
    pub fn evaluate(
        &self,
        baseline_ndcg10: f64,
        int8_ndcg10: f64,
        baseline_latencies_ns: &[u64],
        int8_latencies_ns: &[u64],
    ) -> Int8QualityReport {
        let ndcg10_drop = baseline_ndcg10 - int8_ndcg10;
        let ndcg10_passed = ndcg10_drop <= self.thresholds.ndcg10_max_drop;

        let mut baseline_sorted = baseline_latencies_ns.to_vec();
        baseline_sorted.sort();
        let mut int8_sorted = int8_latencies_ns.to_vec();
        int8_sorted.sort();

        let baseline_p50 = Self::latency_percentile(&baseline_sorted, 0.50);
        let int8_p50 = Self::latency_percentile(&int8_sorted, 0.50);
        let p50_increase = if baseline_p50 == 0 {
            0.0
        } else {
            (int8_p50 as f64 - baseline_p50 as f64) / baseline_p50 as f64
        };
        let p50_passed = p50_increase <= self.thresholds.p50_max_increase;

        let baseline_p99 = Self::latency_percentile(&baseline_sorted, 0.99);
        let int8_p99 = Self::latency_percentile(&int8_sorted, 0.99);
        let p99_increase = if baseline_p99 == 0 {
            0.0
        } else {
            (int8_p99 as f64 - baseline_p99 as f64) / baseline_p99 as f64
        };
        let p99_passed = p99_increase <= self.thresholds.p99_max_increase;

        let overall_passed = ndcg10_passed && p50_passed && p99_passed;

        Int8QualityReport {
            baseline_ndcg10,
            int8_ndcg10,
            ndcg10_drop,
            ndcg10_passed,
            baseline_p50_ns: baseline_p50,
            int8_p50_ns: int8_p50,
            p50_increase,
            p50_passed,
            baseline_p99_ns: baseline_p99,
            int8_p99_ns: int8_p99,
            p99_increase,
            p99_passed,
            overall_passed,
        }
    }
}

impl Default for Int8QualityGate {
    fn default() -> Self {
        Self::with_default_thresholds()
    }
}

/// Promotion decision for INT8 as the default search path.
///
/// The system does not silently promote INT8 — the decision is explicit and
/// observable (VAL-BPHASE-030).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Int8PromotionDecision {
    /// INT8 may be promoted as the default path.
    Promote,
    /// INT8 must not be promoted; FP32 remains the default.
    /// Contains a human-readable reason.
    Block(String),
}

impl Int8QualityReport {
    /// Convert the report into a promotion decision.
    ///
    /// Returns [`Int8PromotionDecision::Promote`] only when all thresholds
    /// pass. Otherwise returns [`Int8PromotionDecision::Block`] with a
    /// description of which thresholds failed.
    pub fn promotion_decision(&self) -> Int8PromotionDecision {
        if self.overall_passed {
            Int8PromotionDecision::Promote
        } else {
            let mut reasons = Vec::new();
            if !self.ndcg10_passed {
                reasons.push(format!(
                    "NDCG@10 drop {:.4} > {:.4}",
                    self.ndcg10_drop, 0.01
                ));
            }
            if !self.p50_passed {
                reasons.push(format!(
                    "p50 latency increase {:.2}% > {:.2}%",
                    self.p50_increase * 100.0,
                    5.0
                ));
            }
            if !self.p99_passed {
                reasons.push(format!(
                    "p99 latency increase {:.2}% > {:.2}%",
                    self.p99_increase * 100.0,
                    10.0
                ));
            }
            Int8PromotionDecision::Block(reasons.join("; "))
        }
    }
}
