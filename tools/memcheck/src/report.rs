//! Report types and serialization for memcheck output.
//!
//! Each phase report exposes the metrics required by VAL-MEASURE-003:
//! RSS min/max/p95, mapped-file vs anonymous memory, sample count, and
//! duration.

use crate::workload::CANONICAL_PHASES;
use serde::{Deserialize, Serialize};

/// Per-phase memory report (VAL-MEASURE-003).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PhaseReport {
    /// Phase name (e.g., "idle_warm", "index").
    pub phase: String,
    /// Minimum RSS observed during the phase, in KiB.
    pub rss_min_kib: u64,
    /// Maximum RSS observed during the phase, in KiB.
    pub rss_max_kib: u64,
    /// 95th percentile RSS, in KiB.
    pub rss_p95_kib: u64,
    /// Mapped-file memory in KiB (Linux, 0 if unavailable).
    pub mapped_file_kib: u64,
    /// Anonymous memory in KiB (Linux, 0 if unavailable).
    pub anon_kib: u64,
    /// Number of samples collected.
    pub sample_count: usize,
    /// Phase duration in milliseconds.
    pub duration_ms: u64,
}

/// Full memcheck report containing all phases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemcheckReport {
    /// Fixture path that was measured.
    pub fixture: String,
    /// Per-phase reports in canonical order.
    pub phases: Vec<PhaseReport>,
    /// Timestamp of the report.
    pub timestamp: String,
}

impl MemcheckReport {
    /// Get a phase report by name.
    #[allow(dead_code)]
    pub fn get_phase(&self, name: &str) -> Option<&PhaseReport> {
        self.phases.iter().find(|p| p.phase == name)
    }

    /// Validate that the report contains all canonical phases in order.
    #[allow(dead_code)]
    pub fn validate_canonical_phases(&self) -> Result<(), String> {
        if self.phases.len() != CANONICAL_PHASES.len() {
            return Err(format!(
                "expected {} phases, got {}",
                CANONICAL_PHASES.len(),
                self.phases.len()
            ));
        }
        for (i, expected) in CANONICAL_PHASES.iter().enumerate() {
            if self.phases[i].phase != *expected {
                return Err(format!(
                    "phase {}: expected '{}', got '{}'",
                    i, expected, self.phases[i].phase
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_phase(name: &str) -> PhaseReport {
        PhaseReport {
            phase: name.to_string(),
            rss_min_kib: 100,
            rss_max_kib: 200,
            rss_p95_kib: 180,
            mapped_file_kib: 50,
            anon_kib: 100,
            sample_count: 5,
            duration_ms: 1000,
        }
    }

    #[test]
    fn test_phase_report_serialization() {
        let report = PhaseReport {
            phase: "index".to_string(),
            rss_min_kib: 100000,
            rss_max_kib: 200000,
            rss_p95_kib: 180000,
            mapped_file_kib: 50000,
            anon_kib: 150000,
            sample_count: 12,
            duration_ms: 3000,
        };

        let json = serde_json::to_string(&report).unwrap();
        let deserialized: PhaseReport = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, report);
    }

    #[test]
    fn test_memcheck_report_get_phase() {
        let report = MemcheckReport {
            fixture: "/test".to_string(),
            phases: vec![make_phase("idle_warm"), make_phase("index")],
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };

        assert!(report.get_phase("idle_warm").is_some());
        assert!(report.get_phase("index").is_some());
        assert!(report.get_phase("nonexistent").is_none());
    }

    #[test]
    fn test_phase_report_json_fields() {
        let report = PhaseReport {
            phase: "query".to_string(),
            rss_min_kib: 50000,
            rss_max_kib: 80000,
            rss_p95_kib: 75000,
            mapped_file_kib: 10000,
            anon_kib: 60000,
            sample_count: 8,
            duration_ms: 2000,
        };

        let json = serde_json::to_value(&report).unwrap();
        // Verify all required fields are present (VAL-MEASURE-003)
        for field in &[
            "phase",
            "rss_min_kib",
            "rss_max_kib",
            "rss_p95_kib",
            "mapped_file_kib",
            "anon_kib",
            "sample_count",
            "duration_ms",
        ] {
            assert!(json.get(field).is_some(), "missing field: {}", field);
        }
    }

    #[test]
    fn test_validate_canonical_phases_success() {
        let report = MemcheckReport {
            fixture: "/test".to_string(),
            phases: CANONICAL_PHASES
                .iter()
                .map(|&name| make_phase(name))
                .collect(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };
        assert!(report.validate_canonical_phases().is_ok());
    }

    #[test]
    fn test_validate_canonical_phases_wrong_count() {
        let report = MemcheckReport {
            fixture: "/test".to_string(),
            phases: vec![make_phase("idle_warm")],
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };
        let err = report.validate_canonical_phases().unwrap_err();
        assert!(err.contains("expected 6 phases"));
    }

    #[test]
    fn test_validate_canonical_phases_wrong_order() {
        let mut phases: Vec<PhaseReport> = CANONICAL_PHASES
            .iter()
            .map(|&name| make_phase(name))
            .collect();
        phases.swap(0, 1); // swap idle_warm and index
        let report = MemcheckReport {
            fixture: "/test".to_string(),
            phases,
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        };
        let err = report.validate_canonical_phases().unwrap_err();
        assert!(err.contains("expected 'idle_warm'"));
    }
}
