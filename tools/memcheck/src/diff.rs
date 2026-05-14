//! Diff logic — compares memcheck reports against committed baselines and
//! absolute budget ceilings.
//!
//! Regression rules (VAL-MEASURE-010):
//! - A phase fails if its rss_max_kib exceeds (committed baseline + 5%)
//! - A phase also fails if its rss_max_kib exceeds (budget ceiling + 10%)
//! - Both rules are checked; passing one does not excuse failing the other.
//!
//! Missing baselines (VAL-MEASURE-011):
//! - If a per-phase baseline is absent, the baseline rule is skipped but
//!   the absolute ceiling rule is still enforced.

use crate::report::{MemcheckReport, PhaseReport};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Budget file schema — the single source of truth for absolute ceilings
/// (VAL-MEASURE-009).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetFile {
    /// Human-readable description.
    #[serde(default)]
    pub description: String,
    /// Fixture name this budget applies to.
    pub fixture: String,
    /// Regression tolerance percentages.
    pub regression_rules: RegressionRules,
    /// Per-phase ceilings.
    pub phases: std::collections::BTreeMap<String, PhaseBudget>,
}

/// Regression tolerance configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegressionRules {
    /// Baseline tolerance as a percentage (default: 5).
    pub baseline_tolerance_pct: u64,
    /// Ceiling tolerance as a percentage (default: 10).
    pub ceiling_tolerance_pct: u64,
}

/// Per-phase budget ceiling.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PhaseBudget {
    /// Absolute RSS ceiling in KiB.
    pub rss_max_kib: u64,
    /// Human-readable description of the ceiling.
    #[serde(default)]
    pub description: String,
}

/// Result of comparing a single phase against its baseline and ceiling.
#[derive(Debug, Clone)]
pub struct PhaseDiff {
    /// Phase name.
    pub phase: String,
    /// Measured rss_max_kib from the current run.
    pub measured_kib: u64,
    /// Committed baseline rss_max_kib, if available.
    pub baseline_kib: Option<u64>,
    /// Baseline threshold = baseline + baseline_tolerance_pct.
    pub baseline_threshold_kib: Option<u64>,
    /// Whether the baseline rule passed (true if no baseline exists).
    pub baseline_passed: bool,
    /// Absolute ceiling rss_max_kib from budget file, if available.
    pub ceiling_kib: Option<u64>,
    /// Ceiling threshold = ceiling + ceiling_tolerance_pct.
    pub ceiling_threshold_kib: Option<u64>,
    /// Whether the ceiling rule passed (true if no ceiling exists).
    pub ceiling_passed: bool,
    /// Overall pass for this phase.
    pub passed: bool,
}

/// Full diff result across all phases.
#[derive(Debug, Clone)]
pub struct DiffResult {
    /// Per-phase diffs in canonical order.
    pub phases: Vec<PhaseDiff>,
    /// Whether all phases passed.
    pub all_passed: bool,
}

/// Load the budget file from the canonical path.
pub fn load_budget(path: &Path) -> Result<BudgetFile> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read budget file {}", path.display()))?;
    let budget: BudgetFile = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse budget file {}", path.display()))?;
    Ok(budget)
}

/// Load a per-phase baseline from the canonical path.
///
/// Baseline files are at `<baselines_dir>/<fixture>/<phase>.json`.
/// Each contains a `PhaseReport`-compatible JSON object.
pub fn load_baseline(baselines_dir: &Path, fixture: &str, phase: &str) -> Option<PhaseReport> {
    let path = baselines_dir.join(fixture).join(format!("{}.json", phase));
    let content = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Write a per-phase baseline file (VAL-MEASURE-008: overwrites canonical).
pub fn write_baseline(baselines_dir: &Path, fixture: &str, phase: &PhaseReport) -> Result<()> {
    let dir = baselines_dir.join(fixture);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("failed to create baseline dir {}", dir.display()))?;
    let path = dir.join(format!("{}.json", phase.phase));
    let json = serde_json::to_string_pretty(phase)
        .with_context(|| format!("failed to serialize baseline for phase {}", phase.phase))?;
    std::fs::write(&path, json)
        .with_context(|| format!("failed to write baseline {}", path.display()))?;
    Ok(())
}

/// Write all phase reports as baseline files.
pub fn write_all_baselines(
    baselines_dir: &Path,
    fixture: &str,
    phases: &[PhaseReport],
) -> Result<()> {
    for phase in phases {
        write_baseline(baselines_dir, fixture, phase)?;
    }
    Ok(())
}

/// Compare a memcheck report against committed baselines and budget ceilings.
///
/// Returns a `DiffResult` with per-phase pass/fail status.
pub fn diff_report(
    report: &MemcheckReport,
    baselines_dir: &Path,
    budget: &BudgetFile,
) -> DiffResult {
    let fixture_name = extract_fixture_name(&report.fixture);
    let rules = &budget.regression_rules;

    let mut phase_diffs = Vec::with_capacity(report.phases.len());

    for phase in &report.phases {
        let baseline = load_baseline(baselines_dir, fixture_name, &phase.phase);

        // Baseline rule: measured <= baseline + tolerance_pct
        let (baseline_kib, baseline_threshold, baseline_passed) = match &baseline {
            Some(b) => {
                let threshold = apply_pct(b.rss_max_kib, rules.baseline_tolerance_pct);
                let passed = phase.rss_max_kib <= threshold;
                (Some(b.rss_max_kib), Some(threshold), passed)
            }
            None => {
                // No baseline → baseline rule is skipped (passes by default)
                // but ceiling rule still enforced (VAL-MEASURE-011).
                (None, None, true)
            }
        };

        // Ceiling rule: measured <= ceiling + tolerance_pct
        let (ceiling_kib, ceiling_threshold, ceiling_passed) = match budget.phases.get(&phase.phase)
        {
            Some(pb) => {
                let threshold = apply_pct(pb.rss_max_kib, rules.ceiling_tolerance_pct);
                let passed = phase.rss_max_kib <= threshold;
                (Some(pb.rss_max_kib), Some(threshold), passed)
            }
            None => {
                // No ceiling → ceiling rule is skipped
                (None, None, true)
            }
        };

        let passed = baseline_passed && ceiling_passed;

        phase_diffs.push(PhaseDiff {
            phase: phase.phase.clone(),
            measured_kib: phase.rss_max_kib,
            baseline_kib,
            baseline_threshold_kib: baseline_threshold,
            baseline_passed,
            ceiling_kib,
            ceiling_threshold_kib: ceiling_threshold,
            ceiling_passed,
            passed,
        });
    }

    let all_passed = phase_diffs.iter().all(|d| d.passed);

    DiffResult {
        phases: phase_diffs,
        all_passed,
    }
}

/// Format a diff result as a human-readable summary.
pub fn format_diff(diff: &DiffResult) -> String {
    let mut lines = Vec::new();

    lines.push("═══ Memcheck Phase Diff ═══".to_string());
    lines.push(format!(
        "{:<15} {:>12} {:>12} {:>12} {:>8}",
        "Phase", "Measured", "Baseline+5%", "Ceiling+10%", "Status"
    ));
    lines.push("─".repeat(65));

    for pd in &diff.phases {
        let status = if pd.passed { "PASS" } else { "FAIL" };

        let baseline_str = pd
            .baseline_threshold_kib
            .map(|v| format!("{} KiB", v))
            .unwrap_or_else(|| "—".to_string());

        let ceiling_str = pd
            .ceiling_threshold_kib
            .map(|v| format!("{} KiB", v))
            .unwrap_or_else(|| "—".to_string());

        lines.push(format!(
            "{:<15} {:>9} KiB {:>12} {:>12} {:>8}",
            pd.phase, pd.measured_kib, baseline_str, ceiling_str, status
        ));

        // Add detail lines for failures
        if !pd.baseline_passed {
            if let (Some(bl), Some(_thr)) = (pd.baseline_kib, pd.baseline_threshold_kib) {
                lines.push(format!(
                    "  ⚠ baseline regression: {} KiB > baseline({} KiB) + {}%",
                    pd.measured_kib,
                    bl,
                    diff_phases_rules(diff).baseline_tolerance_pct
                ));
            }
        }
        if !pd.ceiling_passed {
            if let (Some(ce), Some(_thr)) = (pd.ceiling_kib, pd.ceiling_threshold_kib) {
                lines.push(format!(
                    "  ⚠ ceiling regression: {} KiB > ceiling({} KiB) + {}%",
                    pd.measured_kib,
                    ce,
                    diff_phases_rules(diff).ceiling_tolerance_pct
                ));
            }
        }
    }

    lines.push("─".repeat(65));
    if diff.all_passed {
        lines.push("Result: ALL PHASES PASSED ✓".to_string());
    } else {
        let failed: Vec<&str> = diff
            .phases
            .iter()
            .filter(|p| !p.passed)
            .map(|p| p.phase.as_str())
            .collect();
        lines.push(format!(
            "Result: FAILED — {} phase(s) regressed: {}",
            failed.len(),
            failed.join(", ")
        ));
    }

    lines.join("\n")
}

/// Helper to extract the fixture directory name from a full path.
fn extract_fixture_name(fixture_path: &str) -> &str {
    Path::new(fixture_path)
        .file_name()
        .map(|n| n.to_str().unwrap_or("unknown"))
        .unwrap_or("unknown")
}

/// Apply a percentage increase to a value.
fn apply_pct(value: u64, pct: u64) -> u64 {
    value + (value * pct / 100)
}

/// Helper to get regression rules from a diff result (for formatting).
fn diff_phases_rules(_diff: &DiffResult) -> RegressionRules {
    // We don't store the rules in DiffResult; return defaults for formatting.
    // The actual rules come from the budget file.
    RegressionRules {
        baseline_tolerance_pct: 5,
        ceiling_tolerance_pct: 10,
    }
}

/// Resolve the canonical paths for baselines and budgets relative to the
/// workspace root.
#[allow(dead_code)]
pub fn resolve_memory_paths(workspace_root: &Path) -> (PathBuf, PathBuf) {
    let baselines_dir = workspace_root.join("docs/memory/baselines");
    let budget_path = workspace_root.join("docs/memory/budgets/current.json");
    (baselines_dir, budget_path)
}

/// Find the workspace root by walking up from a starting directory.
pub fn find_workspace_root(start: &Path) -> Result<PathBuf> {
    let mut dir = start;
    loop {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
                if contents.contains("[workspace]") {
                    return Ok(dir.to_path_buf());
                }
            }
        }
        dir = dir.parent().ok_or_else(|| {
            anyhow::anyhow!("could not find workspace root from {}", start.display())
        })?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workload::CANONICAL_PHASES;

    fn make_budget() -> BudgetFile {
        let mut phases = std::collections::BTreeMap::new();
        phases.insert(
            "idle_warm".to_string(),
            PhaseBudget {
                rss_max_kib: 470000,
                description: "test".to_string(),
            },
        );
        phases.insert(
            "index".to_string(),
            PhaseBudget {
                rss_max_kib: 740000,
                description: "test".to_string(),
            },
        );
        BudgetFile {
            description: "test budget".to_string(),
            fixture: "small_repo".to_string(),
            regression_rules: RegressionRules {
                baseline_tolerance_pct: 5,
                ceiling_tolerance_pct: 10,
            },
            phases,
        }
    }

    fn make_report(phases: &[(&str, u64)]) -> MemcheckReport {
        MemcheckReport {
            fixture: "/path/to/small_repo".to_string(),
            phases: phases
                .iter()
                .map(|(name, rss)| PhaseReport {
                    phase: name.to_string(),
                    rss_min_kib: *rss,
                    rss_max_kib: *rss,
                    rss_p95_kib: *rss,
                    mapped_file_kib: 0,
                    anon_kib: 0,
                    sample_count: 10,
                    duration_ms: 3000,
                })
                .collect(),
            timestamp: "2024-01-01T00:00:00Z".to_string(),
        }
    }

    #[test]
    fn test_apply_pct() {
        assert_eq!(apply_pct(100, 5), 105);
        assert_eq!(apply_pct(100, 10), 110);
        assert_eq!(apply_pct(1000, 5), 1050);
    }

    #[test]
    fn test_extract_fixture_name() {
        assert_eq!(extract_fixture_name("/foo/bar/small_repo"), "small_repo");
        assert_eq!(extract_fixture_name("small_repo"), "small_repo");
        assert_eq!(extract_fixture_name("/a/b/c/"), "c");
    }

    #[test]
    fn test_diff_all_pass_with_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");
        let small_repo_dir = baselines_dir.join("small_repo");
        std::fs::create_dir_all(&small_repo_dir).unwrap();

        // Write baseline: idle_warm = 400000 KiB
        let baseline = PhaseReport {
            phase: "idle_warm".to_string(),
            rss_min_kib: 380000,
            rss_max_kib: 400000,
            rss_p95_kib: 395000,
            mapped_file_kib: 0,
            anon_kib: 0,
            sample_count: 10,
            duration_ms: 3000,
        };
        let json = serde_json::to_string_pretty(&baseline).unwrap();
        std::fs::write(small_repo_dir.join("idle_warm.json"), json).unwrap();

        // Write baseline: index = 700000 KiB
        let baseline2 = PhaseReport {
            phase: "index".to_string(),
            rss_min_kib: 650000,
            rss_max_kib: 700000,
            rss_p95_kib: 690000,
            mapped_file_kib: 0,
            anon_kib: 0,
            sample_count: 10,
            duration_ms: 3000,
        };
        let json2 = serde_json::to_string_pretty(&baseline2).unwrap();
        std::fs::write(small_repo_dir.join("index.json"), json2).unwrap();

        let budget = make_budget();
        let report = make_report(&[("idle_warm", 410000), ("index", 720000)]);

        let result = diff_report(&report, &baselines_dir, &budget);

        // idle_warm: baseline 400000 + 5% = 420000, measured 410000 → PASS
        // idle_warm: ceiling 470000 + 10% = 517000, measured 410000 → PASS
        assert!(result.phases[0].baseline_passed);
        assert!(result.phases[0].ceiling_passed);
        assert!(result.phases[0].passed);

        // index: baseline 700000 + 5% = 735000, measured 720000 → PASS
        // index: ceiling 740000 + 10% = 814000, measured 720000 → PASS
        assert!(result.phases[1].baseline_passed);
        assert!(result.phases[1].ceiling_passed);
        assert!(result.phases[1].passed);

        assert!(result.all_passed);
    }

    #[test]
    fn test_diff_baseline_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");
        let small_repo_dir = baselines_dir.join("small_repo");
        std::fs::create_dir_all(&small_repo_dir).unwrap();

        // Baseline: idle_warm = 400000 KiB
        let baseline = PhaseReport {
            phase: "idle_warm".to_string(),
            rss_min_kib: 380000,
            rss_max_kib: 400000,
            rss_p95_kib: 395000,
            mapped_file_kib: 0,
            anon_kib: 0,
            sample_count: 10,
            duration_ms: 3000,
        };
        let json = serde_json::to_string_pretty(&baseline).unwrap();
        std::fs::write(small_repo_dir.join("idle_warm.json"), json).unwrap();

        let budget = make_budget();
        // Measured 430000 > baseline 400000 + 5% = 420000 → FAIL baseline
        // But 430000 < ceiling 470000 + 10% = 517000 → PASS ceiling
        let report = make_report(&[("idle_warm", 430000)]);

        let result = diff_report(&report, &baselines_dir, &budget);

        assert!(
            !result.phases[0].baseline_passed,
            "should fail baseline rule"
        );
        assert!(result.phases[0].ceiling_passed, "should pass ceiling rule");
        assert!(!result.phases[0].passed, "overall should fail");
        assert!(!result.all_passed);
    }

    #[test]
    fn test_diff_ceiling_regression() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");

        let budget = make_budget();
        // No baseline file → baseline rule skipped
        // Measured 600000 > ceiling 470000 + 10% = 517000 → FAIL ceiling
        let report = make_report(&[("idle_warm", 600000)]);

        let result = diff_report(&report, &baselines_dir, &budget);

        assert!(
            result.phases[0].baseline_passed,
            "no baseline → baseline passes"
        );
        assert!(!result.phases[0].ceiling_passed, "should fail ceiling rule");
        assert!(!result.phases[0].passed, "overall should fail");
        assert!(!result.all_passed);
    }

    #[test]
    fn test_diff_missing_baseline_still_enforces_ceiling() {
        // VAL-MEASURE-011: Missing baseline does not disable ceiling protection
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");

        let budget = make_budget();
        // No baseline, but ceiling exists
        // Measured 520000 > ceiling 470000 + 10% = 517000 → FAIL
        let report = make_report(&[("idle_warm", 520000)]);

        let result = diff_report(&report, &baselines_dir, &budget);

        assert!(result.phases[0].baseline_passed, "no baseline → passes");
        assert!(
            !result.phases[0].ceiling_passed,
            "ceiling should still fail"
        );
        assert!(!result.phases[0].passed);
    }

    #[test]
    fn test_diff_both_rules_fail() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");
        let small_repo_dir = baselines_dir.join("small_repo");
        std::fs::create_dir_all(&small_repo_dir).unwrap();

        let baseline = PhaseReport {
            phase: "idle_warm".to_string(),
            rss_min_kib: 380000,
            rss_max_kib: 400000,
            rss_p95_kib: 395000,
            mapped_file_kib: 0,
            anon_kib: 0,
            sample_count: 10,
            duration_ms: 3000,
        };
        let json = serde_json::to_string_pretty(&baseline).unwrap();
        std::fs::write(small_repo_dir.join("idle_warm.json"), json).unwrap();

        let budget = make_budget();
        // Measured 600000 > baseline 400000 + 5% = 420000 → FAIL
        // Measured 600000 > ceiling 470000 + 10% = 517000 → FAIL
        let report = make_report(&[("idle_warm", 600000)]);

        let result = diff_report(&report, &baselines_dir, &budget);

        assert!(!result.phases[0].baseline_passed);
        assert!(!result.phases[0].ceiling_passed);
        assert!(!result.phases[0].passed);
    }

    #[test]
    fn test_write_and_load_baseline() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");

        let phase = PhaseReport {
            phase: "idle_warm".to_string(),
            rss_min_kib: 380000,
            rss_max_kib: 400000,
            rss_p95_kib: 395000,
            mapped_file_kib: 50000,
            anon_kib: 300000,
            sample_count: 12,
            duration_ms: 3000,
        };

        write_baseline(&baselines_dir, "small_repo", &phase).unwrap();

        let loaded = load_baseline(&baselines_dir, "small_repo", "idle_warm").unwrap();
        assert_eq!(loaded.phase, "idle_warm");
        assert_eq!(loaded.rss_max_kib, 400000);
        assert_eq!(loaded.rss_min_kib, 380000);
        assert_eq!(loaded.sample_count, 12);
    }

    #[test]
    fn test_write_all_baselines() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");

        let phases = CANONICAL_PHASES
            .iter()
            .enumerate()
            .map(|(i, &name)| PhaseReport {
                phase: name.to_string(),
                rss_min_kib: 100000 + i as u64 * 10000,
                rss_max_kib: 200000 + i as u64 * 10000,
                rss_p95_kib: 180000 + i as u64 * 10000,
                mapped_file_kib: 0,
                anon_kib: 0,
                sample_count: 10,
                duration_ms: 3000,
            })
            .collect::<Vec<_>>();

        write_all_baselines(&baselines_dir, "small_repo", &phases).unwrap();

        // Verify all 6 canonical phase files exist
        for phase_name in CANONICAL_PHASES {
            let loaded = load_baseline(&baselines_dir, "small_repo", phase_name);
            assert!(
                loaded.is_some(),
                "baseline for '{}' should exist",
                phase_name
            );
            assert_eq!(loaded.unwrap().phase, *phase_name);
        }
    }

    #[test]
    fn test_format_diff_output() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");
        let small_repo_dir = baselines_dir.join("small_repo");
        std::fs::create_dir_all(&small_repo_dir).unwrap();

        let baseline = PhaseReport {
            phase: "idle_warm".to_string(),
            rss_min_kib: 380000,
            rss_max_kib: 400000,
            rss_p95_kib: 395000,
            mapped_file_kib: 0,
            anon_kib: 0,
            sample_count: 10,
            duration_ms: 3000,
        };
        let json = serde_json::to_string_pretty(&baseline).unwrap();
        std::fs::write(small_repo_dir.join("idle_warm.json"), json).unwrap();

        let budget = make_budget();
        let report = make_report(&[("idle_warm", 410000)]);

        let result = diff_report(&report, &baselines_dir, &budget);
        let output = format_diff(&result);

        assert!(output.contains("PASS"));
        assert!(output.contains("idle_warm"));
        assert!(output.contains("410000"));
    }

    #[test]
    fn test_format_diff_failure_output() {
        let dir = tempfile::tempdir().unwrap();
        let baselines_dir = dir.path().join("baselines");
        let small_repo_dir = baselines_dir.join("small_repo");
        std::fs::create_dir_all(&small_repo_dir).unwrap();

        let baseline = PhaseReport {
            phase: "idle_warm".to_string(),
            rss_min_kib: 380000,
            rss_max_kib: 400000,
            rss_p95_kib: 395000,
            mapped_file_kib: 0,
            anon_kib: 0,
            sample_count: 10,
            duration_ms: 3000,
        };
        let json = serde_json::to_string_pretty(&baseline).unwrap();
        std::fs::write(small_repo_dir.join("idle_warm.json"), json).unwrap();

        let budget = make_budget();
        let report = make_report(&[("idle_warm", 430000)]);

        let result = diff_report(&report, &baselines_dir, &budget);
        let output = format_diff(&result);

        assert!(output.contains("FAIL"));
        assert!(output.contains("baseline regression"));
    }

    #[test]
    fn test_budget_file_serialization() {
        let budget = make_budget();
        let json = serde_json::to_string_pretty(&budget).unwrap();
        let parsed: BudgetFile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.fixture, "small_repo");
        assert_eq!(parsed.regression_rules.baseline_tolerance_pct, 5);
        assert_eq!(parsed.regression_rules.ceiling_tolerance_pct, 10);
        assert!(parsed.phases.contains_key("idle_warm"));
        assert!(parsed.phases.contains_key("index"));
    }
}
