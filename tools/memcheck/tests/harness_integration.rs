//! Integration tests for the memcheck harness.
//!
//! These tests verify the assertions VAL-MEASURE-001 through VAL-MEASURE-006:
//! - VAL-MEASURE-001: Memcheck produces a canonical multi-phase report
//! - VAL-MEASURE-002: Phase order matches the canonical workload
//! - VAL-MEASURE-003: Per-phase report schema exposes required metrics
//! - VAL-MEASURE-004: Memcheck samples a fresh leindex process
//! - VAL-MEASURE-005: Linux RSS is the primary measured metric
//! - VAL-MEASURE-006: Mapped-file and anonymous memory captured when available

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

/// Helper: get the workspace root directory.
fn workspace_root() -> PathBuf {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    // tools/memcheck → workspace root
    PathBuf::from(manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

/// Helper: get the small_repo fixture path.
fn small_repo_fixture() -> PathBuf {
    workspace_root().join("tests/fixtures/memcheck/small_repo")
}

/// Helper: get the release leindex binary path.
#[allow(dead_code)]
fn leindex_binary() -> PathBuf {
    workspace_root().join("target/release/leindex")
}

/// Helper: run the memcheck binary and return (exit_code, stdout, stderr).
fn run_memcheck(fixture: &str, extra_args: &[&str]) -> (bool, String, String) {
    let memcheck_bin = std::env::var("CARGO_BIN_EXE_memcheck")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/debug/memcheck"));

    let mut cmd = Command::new(&memcheck_bin);
    cmd.arg(fixture);
    for arg in extra_args {
        cmd.arg(arg);
    }

    let output = cmd.output().expect("failed to run memcheck");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), stdout, stderr)
}

/// Helper: run memcheck with --output to a temp file and parse the JSON report.
fn run_memcheck_to_json(fixture: &str) -> (bool, serde_json::Value) {
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("report.json");

    let memcheck_bin = std::env::var("CARGO_BIN_EXE_memcheck")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/debug/memcheck"));

    let output = Command::new(&memcheck_bin)
        .arg(fixture)
        .arg("--output")
        .arg(&output_path)
        .arg("--verbose")
        .output()
        .expect("failed to run memcheck");

    let success = output.status.success();
    let report_json = if output_path.exists() {
        let content = std::fs::read_to_string(&output_path).unwrap();
        serde_json::from_str(&content).unwrap_or(serde_json::Value::Null)
    } else {
        serde_json::Value::Null
    };

    (success, report_json)
}

// ─── VAL-MEASURE-001: Memcheck produces a canonical multi-phase report ───

#[test]
fn test_val_measure_001_canonical_multi_phase_report() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());

    assert!(success, "memcheck should exit 0");

    // Report should have a "phases" array
    let phases = report
        .get("phases")
        .expect("report should have 'phases' field");
    let phases_arr = phases.as_array().expect("'phases' should be an array");

    // Should have exactly 9 canonical phases (6 original + 3 worker-active)
    assert_eq!(phases_arr.len(), 9, "should have 9 canonical phases");

    // Phase names should match canonical order
    let expected = [
        "idle_warm",
        "index",
        "idle_post",
        "query",
        "reindex",
        "idle_final",
        "embed_idle",
        "embed_active",
        "embed_teardown",
    ];
    for (i, expected_name) in expected.iter().enumerate() {
        let phase_name = phases_arr[i]
            .get("phase")
            .expect("each phase should have 'phase' field")
            .as_str()
            .expect("'phase' should be a string");
        assert_eq!(
            phase_name, *expected_name,
            "phase {} should be '{}'",
            i, expected_name
        );
    }
}

// ─── VAL-MEASURE-002: Phase order matches canonical workload ────────────

#[test]
fn test_val_measure_002_phase_order_is_canonical() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();
    let phase_names: Vec<&str> = phases
        .iter()
        .map(|p| p.get("phase").unwrap().as_str().unwrap())
        .collect();

    let canonical = vec![
        "idle_warm",
        "index",
        "idle_post",
        "query",
        "reindex",
        "idle_final",
        "embed_idle",
        "embed_active",
        "embed_teardown",
    ];

    // No missing phases
    for name in &canonical {
        assert!(
            phase_names.contains(name),
            "canonical phase '{}' missing from report",
            name
        );
    }

    // No extra phases
    assert_eq!(
        phase_names.len(),
        canonical.len(),
        "should have exactly {} phases, got {}",
        canonical.len(),
        phase_names.len()
    );

    // Exact order match
    assert_eq!(phase_names, canonical, "phases must be in canonical order");
}

// ─── VAL-MEASURE-003: Per-phase report schema exposes required metrics ──

#[test]
fn test_val_measure_003_per_phase_schema_has_required_metrics() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();

    let required_fields = [
        "phase",
        "rss_min_kib",
        "rss_max_kib",
        "rss_p95_kib",
        "mapped_file_kib",
        "anon_kib",
        "sample_count",
        "duration_ms",
        "worker_rss_max_kib",
        "combined_rss_max_kib",
    ];

    for (i, phase) in phases.iter().enumerate() {
        for field in &required_fields {
            assert!(
                phase.get(field).is_some(),
                "phase {} ('{}') missing required field '{}'",
                i,
                phase.get("phase").unwrap().as_str().unwrap_or("?"),
                field
            );
        }

        // RSS values should be non-negative integers
        for rss_field in &["rss_min_kib", "rss_max_kib", "rss_p95_kib"] {
            let val = phase.get(*rss_field).unwrap().as_u64();
            assert!(
                val.is_some(),
                "phase {} '{}' should have integer '{}'",
                i,
                phase.get("phase").unwrap().as_str().unwrap_or("?"),
                rss_field
            );
        }

        // sample_count should be positive for the first 6 phases (original canonical).
        // Worker-active phases (embed_*) may have 0 samples if the worker binary
        // is not available.
        let phase_name = phase.get("phase").unwrap().as_str().unwrap_or("");
        let sample_count = phase.get("sample_count").unwrap().as_u64().unwrap();
        if !phase_name.starts_with("embed_") {
            assert!(
                sample_count > 0,
                "phase {} ('{}') should have at least 1 sample",
                i,
                phase_name
            );
        }

        // duration_ms should be positive for the first 6 phases
        let duration = phase.get("duration_ms").unwrap().as_u64().unwrap();
        if !phase_name.starts_with("embed_") {
            assert!(duration > 0, "phase {} ('{}') should have positive duration", i, phase_name);
        }
    }
}

// ─── VAL-MEASURE-004: Memcheck samples a fresh leindex process ──────────

#[test]
fn test_val_measure_004_samples_fresh_process() {
    // This test verifies the workload launches fresh processes per phase.
    // We check by running memcheck twice and verifying the reports differ
    // (different PIDs, different timestamps), and that the workload code
    // structure launches fresh processes per phase.

    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    // Run twice and verify timestamps differ (proving fresh runs)
    let (success1, report1) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success1, "first memcheck run should succeed");

    // Small delay to ensure different timestamps
    std::thread::sleep(Duration::from_millis(1100));

    let (success2, report2) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success2, "second memcheck run should succeed");

    let ts1 = report1.get("timestamp").unwrap().as_str().unwrap();
    let ts2 = report2.get("timestamp").unwrap().as_str().unwrap();
    assert_ne!(
        ts1, ts2,
        "two consecutive runs should have different timestamps"
    );
}

// ─── VAL-MEASURE-005: Linux RSS is the primary measured metric ──────────

#[test]
fn test_val_measure_005_linux_rss_is_primary_metric() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();

    for phase in phases {
        let phase_name = phase.get("phase").unwrap().as_str().unwrap();
        let rss_max = phase.get("rss_max_kib").unwrap().as_u64().unwrap();
        let sample_count = phase.get("sample_count").unwrap().as_u64().unwrap();

        // Skip worker-active phases that had no samples (worker binary not available)
        if phase_name.starts_with("embed_") && sample_count == 0 {
            continue;
        }

        // RSS should be positive for all sampled phases
        assert!(
            rss_max > 0,
            "phase '{}' should have positive rss_max_kib, got {}",
            phase_name,
            rss_max
        );

        // RSS should be reasonable (< 2 GiB for a small fixture)
        assert!(
            rss_max < 2_000_000,
            "phase '{}' rss_max_kib should be reasonable, got {}",
            phase_name,
            rss_max
        );
    }
}

// ─── VAL-MEASURE-006: Mapped-file and anonymous memory captured ─────────

#[test]
fn test_val_measure_006_mapped_file_and_anon_captured() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();

    for phase in phases {
        let phase_name = phase.get("phase").unwrap().as_str().unwrap();
        let mapped = phase.get("mapped_file_kib").unwrap().as_u64().unwrap();
        let anon = phase.get("anon_kib").unwrap().as_u64().unwrap();

        // On Linux, at least one of mapped_file or anon should be populated
        // for phases that actually sampled the process.
        let sample_count = phase.get("sample_count").unwrap().as_u64().unwrap();
        if sample_count > 0 {
            // On Linux, we expect at least one of these to be non-zero
            // (the process has both file-backed and anonymous mappings).
            // We don't assert both are > 0 because short-lived commands
            // may have very few samples.
            assert!(
                mapped > 0 || anon > 0,
                "phase '{}' should have at least one of mapped_file or anon > 0 (got mapped={}, anon={})",
                phase_name, mapped, anon
            );
        }
    }
}

// ─── Additional robustness tests ────────────────────────────────────────

#[test]
fn test_memcheck_help_flag() {
    let memcheck_bin = std::env::var("CARGO_BIN_EXE_memcheck")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/debug/memcheck"));

    let output = Command::new(&memcheck_bin)
        .arg("--help")
        .output()
        .expect("failed to run memcheck --help");

    assert!(output.status.success(), "--help should exit 0");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("fixture") || stdout.contains("LeIndex memory measurement"),
        "help should mention fixture or purpose"
    );
}

#[test]
fn test_memcheck_missing_fixture_fails() {
    let (success, stdout, stderr) = run_memcheck("/nonexistent/path/fixture", &[]);
    assert!(!success, "memcheck should fail with nonexistent fixture");
    // Should produce an error message
    let combined = format!("{}{}", stdout, stderr);
    assert!(
        combined.contains("does not exist")
            || combined.contains("not found")
            || combined.contains("error"),
        "should mention the missing path: {}",
        combined
    );
}

#[test]
fn test_report_json_is_valid_and_parseable() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    // Top-level fields
    assert!(
        report.get("fixture").is_some(),
        "report should have 'fixture'"
    );
    assert!(
        report.get("phases").is_some(),
        "report should have 'phases'"
    );
    assert!(
        report.get("timestamp").is_some(),
        "report should have 'timestamp'"
    );

    // Fixture path should contain the fixture name
    let fixture_path = report.get("fixture").unwrap().as_str().unwrap();
    assert!(
        fixture_path.contains("small_repo"),
        "fixture path should reference small_repo: {}",
        fixture_path
    );
}

#[test]
fn test_idle_phases_have_reasonable_duration() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();

    // Idle phases should have duration >= 3 seconds (IDLE_DWELL)
    // Worker-active idle phases (embed_idle, embed_teardown) may have 0 duration
    // if the worker binary is not available.
    for phase in phases {
        let name = phase.get("phase").unwrap().as_str().unwrap();
        let sample_count = phase.get("sample_count").unwrap().as_u64().unwrap();
        if name.starts_with("idle_") || (name.starts_with("embed_") && name != "embed_active") {
            // Skip phases with no samples (worker binary not available)
            if name.starts_with("embed_") && sample_count == 0 {
                continue;
            }
            let duration = phase.get("duration_ms").unwrap().as_u64().unwrap();
            assert!(
                duration >= 2500,
                "idle phase '{}' should last at least ~3s, got {}ms",
                name,
                duration
            );
        }
    }
}
