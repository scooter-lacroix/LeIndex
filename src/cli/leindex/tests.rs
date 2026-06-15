use super::*;
use tempfile::tempdir;

#[test]
fn test_project_scan_excludes_lockfiles_from_source_but_keeps_manifests() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        dir.path().join("package.json"),
        r#"{"dependencies":{"react":"^18.2.0"}}"#,
    )
    .unwrap();
    std::fs::write(
        dir.path().join("package-lock.json"),
        r#"{"name":"demo","lockfileVersion":3}"#,
    )
    .unwrap();

    let mut index = LeIndex::new(dir.path()).unwrap();
    let scan = index.get_project_scan(true).unwrap();

    assert!(scan
        .source_paths
        .iter()
        .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("main.rs")));
    assert!(scan
        .source_paths
        .iter()
        .all(|path| path.file_name().and_then(|name| name.to_str()) != Some("package-lock.json")));
    assert!(scan
        .manifest_paths
        .iter()
        .any(|path| path.file_name().and_then(|name| name.to_str()) == Some("package.json")));
    assert!(scan.manifest_paths.iter().any(|path| {
        path.file_name().and_then(|name| name.to_str()) == Some("package-lock.json")
    }));
}

#[test]
fn test_project_scan_is_restored_from_cache_across_instances() {
    let dir = tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let mut first = LeIndex::new(dir.path()).unwrap();
    let first_scan = first.get_project_scan(true).unwrap();
    drop(first);

    let mut second = LeIndex::new(dir.path()).unwrap();
    let second_scan = second.get_project_scan(false).unwrap();

    assert_eq!(first_scan.source_paths, second_scan.source_paths);
    assert_eq!(first_scan.manifest_paths, second_scan.manifest_paths);
}

#[test]
fn test_stats_serialization() {
    let stats = IndexStats {
        total_files: 100,
        files_parsed: 100,
        successful_parses: 95,
        failed_parses: 5,
        total_signatures: 500,
        pdg_nodes: 300,
        pdg_edges: 1200,
        indexed_nodes: 300,
        indexing_time_ms: 5000,
        external_deps_in_lockfile: 0,
        external_deps_resolved: 0,
        external_deps_unresolved: 0,
        external_deps_total: 0,
        external_deps_builtin: 0,
    };

    let json = serde_json::to_string(&stats).unwrap();
    let deserialized: IndexStats = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.files_parsed, 100);
    assert_eq!(deserialized.successful_parses, 95);
}

#[test]
fn test_analysis_result_serialization() {
    let result = AnalysisResult {
        query: "test".to_string(),
        results: vec![],
        context: Some("context".to_string()),
        tokens_used: 100,
        processing_time_ms: 50,
    };

    let json = serde_json::to_string(&result).unwrap();
    let deserialized: AnalysisResult = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.query, "test");
    assert_eq!(deserialized.tokens_used, 100);
}

#[test]
fn test_diagnostics_serialization() {
    let diagnostics = Diagnostics {
        project_path: "/test".to_string(),
        project_id: "test".to_string(),
        unique_project_id: "test_a1b2c3d4_0".to_string(),
        display_name: "test".to_string(),
        stats: IndexStats {
            total_files: 0,
            files_parsed: 0,
            successful_parses: 0,
            failed_parses: 0,
            total_signatures: 0,
            pdg_nodes: 0,
            pdg_edges: 0,
            indexed_nodes: 0,
            indexing_time_ms: 0,
            external_deps_in_lockfile: 0,
            external_deps_resolved: 0,
            external_deps_unresolved: 0,
            external_deps_total: 0,
            external_deps_builtin: 0,
        },
        memory_usage_bytes: 1024,
        total_memory_bytes: 8192,
        memory_usage_percent: 12.5,
        memory_threshold_exceeded: false,
        cache_entries: 5,
        cache_bytes: 50000,
        spilled_entries: 3,
        spilled_bytes: 30000,
        cache_hits: 9,
        cache_memory_hits: 7,
        cache_disk_hits: 2,
        cache_misses: 3,
        cache_hit_rate: 0.75,
        cache_writes: 12,
        cache_spills: 4,
        cache_restores: 2,
        cache_temperature: "warm".to_string(),
        pdg_loaded: true,
        pdg_estimated_bytes: 60000,
        search_index_nodes: 100,
        index_health: "healthy".to_string(),
        pdg_nodes: 500,
        pdg_edges: 800,
        embedding_model: "tfidf_only".to_string(),
    };

    let json = serde_json::to_string(&diagnostics).unwrap();
    let deserialized: Diagnostics = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.project_id, "test");
    assert_eq!(deserialized.unique_project_id, "test_a1b2c3d4_0");
    assert_eq!(deserialized.display_name, "test");
    assert_eq!(deserialized.memory_usage_bytes, 1024);
    assert_eq!(deserialized.cache_entries, 5);
    assert_eq!(deserialized.cache_hits, 9);
    assert_eq!(deserialized.spilled_bytes, 30000);
}
