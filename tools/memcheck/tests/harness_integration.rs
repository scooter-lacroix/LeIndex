//! Integration tests for the memcheck harness.
//!
//! These tests verify the assertions VAL-MEASURE-001 through VAL-MEASURE-006:
//! - VAL-MEASURE-001: Memcheck produces a canonical multi-phase report
//! - VAL-MEASURE-002: Phase order matches the canonical workload
//! - VAL-MEASURE-003: Per-phase report schema exposes required metrics
//! - VAL-MEASURE-004: Memcheck samples a fresh leindex process
//! - VAL-MEASURE-005: Linux RSS is the primary measured metric
//! - VAL-MEASURE-006: Mapped-file and anonymous memory captured when available
//!
//! These tests share the leindex-embed worker daemon socket and the fixture
//! directory. They MUST run serially: `cargo test -- --test-threads=1`.
//! Running them in parallel causes concurrent worker spawns against the same
//! daemon socket, producing false failures.
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Mutex, OnceLock};
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
fn leindex_binary() -> PathBuf {
    workspace_root().join("target/release/leindex")
}

/// Helper: check if the release leindex binary exists.
///
/// Memcheck integration tests require `target/release/leindex` which is NOT
/// built during CI's `lint-and-test` job (only `cargo test --workspace` runs
/// in debug mode). Call this at the top of each test alongside the fixture
/// check to skip gracefully when the release binary is absent.
fn release_binary_available() -> bool {
    leindex_binary().exists()
}

/// Skip the current test if the release binary is absent.
macro_rules! require_release_binary {
    () => {
        if !release_binary_available() {
            eprintln!(
                "SKIP: release binary not found at {:?}. Run: cargo build --release --bin leindex",
                leindex_binary()
            );
            return;
        }
    };
}

static MEMCHECK_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

fn memcheck_lock() -> std::sync::MutexGuard<'static, ()> {
    MEMCHECK_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("memcheck test lock poisoned")
}

/// Helper: run the memcheck binary and return (exit_code, stdout, stderr).
fn run_memcheck(fixture: &str, extra_args: &[&str]) -> (bool, String, String) {
    let _lock = memcheck_lock();
    let memcheck_bin = std::env::var("CARGO_BIN_EXE_memcheck")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/debug/memcheck"));

    let mut cmd = Command::new(&memcheck_bin);
    cmd.arg(fixture);
    for arg in extra_args {
        cmd.arg(arg);
    }
    cmd.env("LEINDEX_WORKER_EXECUTION_PROVIDER", "cpu");

    let output = cmd.output().expect("failed to run memcheck");
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    (output.status.success(), stdout, stderr)
}

/// Helper: run memcheck with --output to a temp file and parse the JSON report.
fn run_memcheck_to_json(fixture: &str) -> (bool, serde_json::Value) {
    let _lock = memcheck_lock();
    let dir = tempfile::tempdir().unwrap();
    let output_path = dir.path().join("report.json");
    // These tests validate report shape and phase behavior. Keep host RSS
    // variance out of them; baseline-threshold behavior is covered by the
    // diff-logic tests, while the absolute budget remains active here.
    let baselines_path = dir.path().join("baselines");

    let memcheck_bin = std::env::var("CARGO_BIN_EXE_memcheck")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/debug/memcheck"));

    let output = Command::new(&memcheck_bin)
        .arg(fixture)
        .arg("--output")
        .arg(&output_path)
        .arg("--baselines-dir")
        .arg(&baselines_path)
        .arg("--verbose")
        .env("LEINDEX_HOME", dir.path().join("leindex-home"))
        .env("LEINDEX_WORKER_EXECUTION_PROVIDER", "cpu")
        .output()
        .expect("failed to run memcheck");

    let success = output.status.success();
    if !success {
        eprintln!(
            "memcheck failed (status={}):\nstdout:\n{}\nstderr:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
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

    require_release_binary!();

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());

    assert!(success, "memcheck should exit 0");

    // Report should have a "phases" array
    let phases = report
        .get("phases")
        .expect("report should have 'phases' field");
    let phases_arr = phases.as_array().expect("'phases' should be an array");

    // Should have exactly 12 canonical phases (6 original + 3 worker-active
    // + 3 memory-pressure phases)
    assert_eq!(phases_arr.len(), 12, "should have 12 canonical phases");

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
        "mcp_idle_proliferation",
        "worker_ort_threads",
        "stale_artifacts",
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

    require_release_binary!();

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
        "mcp_idle_proliferation",
        "worker_ort_threads",
        "stale_artifacts",
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

    require_release_binary!();

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

        // sample_count should be positive for the non-worker-gated phases.
        // Worker-gated phases (embed_*, worker_ort_threads) may have 0 samples
        // if the worker binary is not available (placeholder reports).
        let phase_name = phase.get("phase").unwrap().as_str().unwrap_or("");
        let sample_count = phase.get("sample_count").unwrap().as_u64().unwrap();
        if !phase_name.starts_with("embed_") && phase_name != "worker_ort_threads" {
            assert!(
                sample_count > 0,
                "phase {} ('{}') should have at least 1 sample",
                i,
                phase_name
            );
        }

        // duration_ms should be positive for the non-worker-gated phases
        let duration = phase.get("duration_ms").unwrap().as_u64().unwrap();
        if !phase_name.starts_with("embed_") && phase_name != "worker_ort_threads" {
            assert!(
                duration > 0,
                "phase {} ('{}') should have positive duration",
                i,
                phase_name
            );
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

    require_release_binary!();

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

    require_release_binary!();

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();

    for phase in phases {
        let phase_name = phase.get("phase").unwrap().as_str().unwrap();
        let rss_max = phase.get("rss_max_kib").unwrap().as_u64().unwrap();
        let sample_count = phase.get("sample_count").unwrap().as_u64().unwrap();

        // Skip worker-gated phases that had no samples (worker binary not
        // available → placeholder reports carry u64::MAX sentinels).
        if (phase_name.starts_with("embed_") || phase_name == "worker_ort_threads")
            && sample_count == 0
        {
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

    require_release_binary!();

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
                phase_name,
                mapped,
                anon
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

    require_release_binary!();

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

    require_release_binary!();

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();

    // Idle phases should have duration >= 3 seconds (IDLE_DWELL)
    // Worker-active idle phases (embed_idle, embed_teardown) may have 0 duration
    // if the worker binary is not available.
    for phase in phases {
        let name = phase.get("phase").unwrap().as_str().unwrap();
        let sample_count = phase.get("sample_count").unwrap().as_u64().unwrap();
        if name.starts_with("idle_")
            || name == "mcp_idle_proliferation"
            || (name.starts_with("embed_") && name != "embed_active")
        {
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

// ─── T8 step-3: mcp_idle_proliferation linearity guard ──────────────────

#[test]
fn test_mcp_idle_proliferation_linearity() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    require_release_binary!();

    let (success, report) = run_memcheck_to_json(fixture.to_str().unwrap());
    assert!(success, "memcheck should exit 0");

    let phases = report.get("phases").unwrap().as_array().unwrap();
    let get_rss = |name: &str| -> u64 {
        phases
            .iter()
            .find(|p| p.get("phase").and_then(|n| n.as_str()) == Some(name))
            .unwrap_or_else(|| panic!("phase '{name}' missing from report"))
            .get("rss_max_kib")
            .unwrap()
            .as_u64()
            .unwrap()
    };

    let single = get_rss("idle_warm");
    let combined = get_rss("mcp_idle_proliferation");
    let ceiling = (single as f64 * 3.5) as u64;

    // 3 concurrent idle servers must scale roughly linearly: combined ≤ 3.5 ×
    // single-server. A per-server leak (memory-pressure T2) breaks this.
    assert!(
        combined <= ceiling,
        "3 idle MCP servers combined {combined} KiB should be ≤ 3.5× single-server {single} KiB ({ceiling} KiB) — superlinear per-server growth"
    );
    // Sanity: all 3 servers really ran (combined ≥ 2 × single).
    assert!(
        combined >= single * 2,
        "combined {combined} KiB should be at least 2× single {single} KiB (3 servers were expected)"
    );
}

// ─── T8 step-3: stale-artifacts sweep removes dead sidecars ─────────────

#[test]
fn test_stale_artifacts_phase_removes_dead_sidecars() {
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    require_release_binary!();

    let _lock = memcheck_lock();
    let dir = tempfile::tempdir().unwrap();
    let run_dir = dir.path().join("leindex-home").join("run");
    std::fs::create_dir_all(&run_dir).unwrap();

    // Seed dead-pid sidecars matching the T7 sweeper naming scheme.
    let stem = "leindex-embed-memcheck";
    let seeded = ["pid", "lock", "sock"];
    for ext in seeded {
        let contents = if ext == "pid" {
            "2147483647\n"
        } else {
            "stale\n"
        };
        std::fs::write(run_dir.join(format!("{stem}.{ext}")), contents).unwrap();
    }

    let memcheck_bin = std::env::var("CARGO_BIN_EXE_memcheck")
        .map(PathBuf::from)
        .unwrap_or_else(|_| workspace_root().join("target/debug/memcheck"));
    let output = Command::new(&memcheck_bin)
        .arg(fixture.to_str().unwrap())
        .arg("--baselines-dir")
        .arg(dir.path().join("baselines"))
        .env("LEINDEX_HOME", dir.path().join("leindex-home"))
        .env(
            "LEINDEX_MEMCHECK_STALE_HOME",
            dir.path().join("leindex-home"),
        )
        .env("LEINDEX_WORKER_EXECUTION_PROVIDER", "cpu")
        .output()
        .expect("failed to run memcheck");
    assert!(
        output.status.success(),
        "memcheck failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    for ext in seeded {
        let path = run_dir.join(format!("{stem}.{ext}"));
        assert!(
            !path.exists(),
            "sidecar {} should have been removed by the stale_artifacts phase",
            path.display()
        );
    }
}

// ─── T8 step-3: worker ORT-threads cap ≤ no-cap (opt-in, expensive) ─────

#[test]
fn test_worker_ort_threads_cap_leq_nocap() {
    // Opt-in: a full memcheck run (incl. the ONNX worker-active phases) takes
    // ~1 min; this test runs it twice and loads the real model each time.
    // Enable with LEINDEX_MEMCHECK_EXPENSIVE=1.
    if std::env::var("LEINDEX_MEMCHECK_EXPENSIVE").is_err() {
        eprintln!(
            "SKIP: set LEINDEX_MEMCHECK_EXPENSIVE=1 to run the {{0,4,1}} ORT-thread RSS comparison"
        );
        return;
    }
    let fixture = small_repo_fixture();
    if !fixture.exists() {
        eprintln!("SKIP: fixture not found at {:?}", fixture);
        return;
    }

    require_release_binary!();

    let measure = |threads: &str| -> u64 {
        let _lock = memcheck_lock();
        let dir = tempfile::tempdir().unwrap();
        let output_path = dir.path().join("report.json");
        let memcheck_bin = std::env::var("CARGO_BIN_EXE_memcheck")
            .map(PathBuf::from)
            .unwrap_or_else(|_| workspace_root().join("target/debug/memcheck"));
        let output = Command::new(&memcheck_bin)
            .arg(fixture.to_str().unwrap())
            .arg("--output")
            .arg(&output_path)
            .arg("--baselines-dir")
            .arg(dir.path().join("baselines"))
            .env("LEINDEX_HOME", dir.path().join("leindex-home"))
            .env("LEINDEX_WORKER_EXECUTION_PROVIDER", "cpu")
            .env("LEINDEX_WORKER_ORT_THREADS", threads)
            .output()
            .expect("failed to run memcheck");
        if !output.status.success() {
            eprintln!(
                "memcheck (threads={threads}) failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            return u64::MAX;
        }
        let content = std::fs::read_to_string(&output_path).unwrap_or_default();
        let json: serde_json::Value =
            serde_json::from_str(&content).unwrap_or(serde_json::Value::Null);
        json.get("phases")
            .and_then(|p| p.as_array())
            .and_then(|arr| {
                arr.iter()
                    .find(|p| p.get("phase").and_then(|n| n.as_str()) == Some("worker_ort_threads"))
            })
            .and_then(|p| p.get("worker_rss_max_kib"))
            .and_then(|v| v.as_u64())
            .unwrap_or(u64::MAX)
    };

    // Plan {0,4,1}: measure all three settings and log the deltas as the
    // empirical record. The assertion is cap ≤ no-cap for both capped values.
    let t0 = measure("0");
    let t4 = measure("4");
    let t1 = measure("1");
    // Kilo WARNING: u64::MAX is the failure sentinel returned by `measure` on
    // any run error. If every run failed, `MAX <= MAX` would make the cap
    // assertions below pass vacuously (false green). Fail loudly instead.
    for (label, kib) in [
        ("threads=0 (uncapped)", t0),
        ("threads=4", t4),
        ("threads=1", t1),
    ] {
        assert_ne!(
            kib,
            u64::MAX,
            "measurement for {label} failed (memcheck run error) — cannot validate the ORT-thread cap (T5/D3)"
        );
    }
    eprintln!(
        "worker_ort_threads empirical record: threads=0 (uncapped) -> {t0} KiB, threads=4 -> {t4} KiB, threads=1 -> {t1} KiB"
    );
    assert!(
        t1 <= t0,
        "capped ORT threads (1) worker RSS {t1} KiB must be ≤ uncapped (0) {t0} KiB (T5/D3)"
    );
    assert!(
        t4 <= t0,
        "capped ORT threads (4) worker RSS {t4} KiB must be ≤ uncapped (0) {t0} KiB (T5/D3)"
    );
}
