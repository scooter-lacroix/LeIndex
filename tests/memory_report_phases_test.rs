//! `--memory-report` must actually report phases.
//!
//! Regression: the tracker only accumulated a global peak. `record_phase` —
//! the only call that appended to `MemoryReport::phases` — was never invoked
//! on the CLI indexing path, and `observe_rss(phase)` threw its phase name
//! away. A one-shot `leindex index --memory-report <path>` therefore wrote
//! `"phases": []`, which is the repo's own evidence format (BENCHMARKS.md,
//! tools/memcheck) reporting nothing at all.

#![cfg(all(feature = "cli", target_os = "linux"))]

use std::process::Command;

/// The index phases a one-shot run must report, in pipeline order.
///
/// Mirrors `cli::leindex::IndexPhase` (scan/parse/pdg/lexical/neural) plus the
/// two publication chunks from `scheduler::index_job::IndexPhaseId`. Hardcoded
/// on purpose: this is the externally visible contract, so renaming a phase in
/// the enum must not silently rewrite what the report promises.
const EXPECTED_PHASES: &[&str] = &[
    "scan",
    "parse",
    "pdg",
    "lexical",
    "persist",
    "publish-core",
    "neural",
    "publish-enhanced",
];

fn write_fixture(root: &std::path::Path) {
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub fn alpha(x: u32) -> u32 {\n    x + 1\n}\n\npub struct Widget { pub id: u32 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/util.py"),
        "def helper(value):\n    return value * 2\n",
    )
    .unwrap();
}

#[test]
fn test_one_shot_index_memory_report_has_canonical_phases_in_order() {
    let dir = tempfile::tempdir().unwrap();
    let project = dir.path().join("project");
    let home = dir.path().join("home");
    std::fs::create_dir_all(&project).unwrap();
    std::fs::create_dir_all(&home).unwrap();
    write_fixture(&project);
    let report_path = dir.path().join("report.json");

    let output = Command::new(env!("CARGO_BIN_EXE_leindex"))
        .arg("index")
        .arg("--force")
        .arg("--memory-report")
        .arg(&report_path)
        .arg(&project)
        // Keep the store inside the temp dir and out of any real cache.
        .env("LEINDEX_HOME", &home)
        // No model, no network: this asserts report plumbing, not embeddings.
        .env("LEINDEX_FEATURE_NEURAL_SEARCH", "0")
        .env("LEINDEX_FEATURE_REMOTE_EMBEDDINGS", "0")
        .env("LEINDEX_CLI_SHUTDOWN_DAEMON", "1")
        .output()
        .expect("run leindex index");

    assert!(
        output.status.success(),
        "index failed.\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let raw = std::fs::read_to_string(&report_path).expect("memory report written");
    let report: leindex::cli::memory_report::MemoryReport = serde_json::from_str(&raw)
        .unwrap_or_else(|error| {
            panic!("memory report must be valid JSON ({error}):\n{raw}");
        });

    assert!(
        !report.phases.is_empty(),
        "a one-shot index run reported no phases at all:\n{raw}"
    );

    let names: Vec<&str> = report.phases.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(
        names, EXPECTED_PHASES,
        "memory report phases must be the canonical index phases in pipeline order:\n{raw}"
    );

    // Every phase must carry a real observation, and the process peak must be
    // at least as large as any single phase peak.
    let mut widest_peak = 0;
    for phase in &report.phases {
        assert!(
            phase.peak_rss_bytes > 0,
            "phase '{}' reported a zero peak:\n{raw}",
            phase.name
        );
        assert!(
            phase.sample_count > 0,
            "phase '{}' reported no samples:\n{raw}",
            phase.name
        );
        widest_peak = widest_peak.max(phase.peak_rss_bytes);
    }
    assert!(
        report.peak_rss_bytes >= widest_peak,
        "process peak {} is below a phase peak {widest_peak}:\n{raw}",
        report.peak_rss_bytes
    );
}
