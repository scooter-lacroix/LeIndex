use lephase::{run_phase_analysis, DocsMode, PhaseOptions, PhaseSelection};
use std::fs;
use tempfile::tempdir;

fn write_file(path: &std::path::Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, content).expect("write file");
}

#[test]
fn cold_then_warm_run_uses_summary_cache() {
    let dir = tempdir().expect("tempdir");
    write_file(
        &dir.path().join("src/lib.rs"),
        "pub fn add(a:i32,b:i32)->i32{a+b}\n",
    );

    let options = PhaseOptions {
        root: dir.path().to_path_buf(),
        ..PhaseOptions::default()
    };

    let first = run_phase_analysis(options.clone(), PhaseSelection::All).expect("first run");
    assert!(!first.cache_hit, "cold run should not hit summary cache");

    let second = run_phase_analysis(options, PhaseSelection::All).expect("second run");
    assert!(second.cache_hit, "warm run should reuse summary cache");
    assert_eq!(second.changed_files, 0);
}

#[test]
fn incremental_change_invalidates_generation_cache() {
    let dir = tempdir().expect("tempdir");
    let source = dir.path().join("src/lib.rs");
    write_file(&source, "pub fn add(a:i32,b:i32)->i32{a+b}\n");

    let options = PhaseOptions {
        root: dir.path().to_path_buf(),
        ..PhaseOptions::default()
    };

    let _ = run_phase_analysis(options.clone(), PhaseSelection::All).expect("initial run");

    write_file(
        &source,
        "pub fn add(a:i32,b:i32)->i32{a+b}\npub fn sub(a:i32,b:i32)->i32{a-b}\n",
    );

    let updated = run_phase_analysis(options, PhaseSelection::All).expect("updated run");
    assert!(updated.changed_files >= 1);
    assert!(
        !updated.cache_hit,
        "generation change should miss summary cache"
    );
}

#[test]
fn docs_analysis_requires_explicit_opt_in() {
    let dir = tempdir().expect("tempdir");
    write_file(
        &dir.path().join("src/lib.rs"),
        "pub fn ping()->bool{true}\n",
    );
    write_file(
        &dir.path().join("README.md"),
        "# Header\nTODO: add docs validation\n",
    );

    let without_docs = run_phase_analysis(
        PhaseOptions {
            root: dir.path().to_path_buf(),
            include_docs: false,
            docs_mode: DocsMode::All,
            ..PhaseOptions::default()
        },
        PhaseSelection::Single(1),
    )
    .expect("run without docs");

    assert!(
        !without_docs.formatted_output.contains("docs:"),
        "docs output must be absent when include_docs=false"
    );

    let with_docs = run_phase_analysis(
        PhaseOptions {
            root: dir.path().to_path_buf(),
            include_docs: true,
            docs_mode: DocsMode::Markdown,
            ..PhaseOptions::default()
        },
        PhaseSelection::Single(1),
    )
    .expect("run with docs");

    assert!(
        with_docs.formatted_output.contains("docs:"),
        "docs output should appear when docs are explicitly enabled"
    );
}

#[test]
fn phase4_and_phase5_generate_hotspots_and_recommendations() {
    let dir = tempdir().expect("tempdir");
    write_file(
        &dir.path().join("src/lib.rs"),
        "pub fn auth_error_path(x:i32)->i32{ if x>10 {x+1} else {x-1} }\n",
    );

    let report = run_phase_analysis(
        PhaseOptions {
            root: dir.path().to_path_buf(),
            top_n: 5,
            ..PhaseOptions::default()
        },
        PhaseSelection::All,
    )
    .expect("phase analysis");

    let phase4 = report.phase4.expect("phase4 summary");
    let phase5 = report.phase5.expect("phase5 summary");

    assert!(
        !phase4.hotspots.is_empty(),
        "phase4 should produce hotspots"
    );
    assert!(
        !phase5.recommendations.is_empty(),
        "phase5 should produce recommendations"
    );
}
