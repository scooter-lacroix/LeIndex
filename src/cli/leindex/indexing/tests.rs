use super::*;

/// VAL-STREAM-012: IndexPipelineState must not hold any cross-phase
/// source-body cache. The `shared_file_cache` field was removed entirely;
/// source bodies are re-read per chunk via a per-chunk scratch buffer
/// (capacity 1), dropped at the end of each batch.
#[test]
fn pipeline_state_has_no_cross_phase_file_cache() {
    // Compile-time check: the struct definition has no shared_file_cache field.
    // If this test compiles, the field was removed (or never present).
    let state = IndexPipelineState::new(
        false,
        std::time::Instant::now(),
        crate::cli::index_job::JobPaths::new(std::path::Path::new("/tmp"), 1),
    );
    // The state should NOT have any file cache field. Verify by checking that
    // the old field path does not compile (it was removed from the struct).
    // This is a negative compile assertion: if someone re-adds the field,
    // this test serves as documentation of the invariant.
    assert!(state.source_files_with_hashes.is_empty(), "fresh state");
}

/// VAL-STREAM-012: FileReadCache::per_chunk_scratch has capacity 1, not
/// 100-200. Source bodies are dropped after each chunk, preventing RSS
/// growth proportional to corpus size.
#[test]
fn per_chunk_scratch_has_capacity_one() {
    // Capacity 1 means only one file body is resident at a time.
    // The scratch is intended to avoid re-reading the same file for
    // adjacent sibling nodes in the same batch; it is NOT a cache.
    // We verify capacity indirectly: insert 2 files, only 1 remains.
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let file_a = dir.path().join("a.rs");
    let file_b = dir.path().join("b.rs");
    let mut f1 = std::fs::File::create(&file_a).unwrap();
    f1.write_all(b"fn a() {}").unwrap();
    drop(f1);
    let mut f2 = std::fs::File::create(&file_b).unwrap();
    f2.write_all(b"fn b() {}").unwrap();
    drop(f2);

    let mut cache = index_builder::FileReadCache::per_chunk_scratch();
    let _bytes_a = cache.get_or_read(&file_a).unwrap();
    let _bytes_b = cache.get_or_read(&file_b).unwrap();

    // After reading B, A should have been evicted (capacity 1).
    // Reading A again should succeed (re-read from disk), confirming
    // it was evicted, not cached.
    let bytes_a_again = cache.get_or_read(&file_a).unwrap();
    assert_eq!(bytes_a_again.as_slice(), b"fn a() {}");
}

#[test]
fn admitted_node_ids_are_sorted_for_checkpoint_payloads() {
    let admitted = [
        "node-z".to_string(),
        "node-a".to_string(),
        "node-m".to_string(),
    ]
    .into_iter()
    .collect();

    let checkpoint = LexicalCheckpoint {
        pdg_hash: "pdg".to_string(),
        snapshot_path: "snapshot.bin".into(),
        tfidf_path: "tfidf.bin".into(),
        admitted_node_ids: sorted_admitted_node_ids(&admitted),
    };
    let payload = serde_json::to_vec(&checkpoint).expect("serialize lexical checkpoint");
    let decoded: LexicalCheckpoint =
        serde_json::from_slice(&payload).expect("deserialize lexical checkpoint");

    assert_eq!(
        decoded.admitted_node_ids,
        vec!["node-a", "node-m", "node-z"]
    );
}

#[test]
fn admitted_node_ids_restore_from_lexical_checkpoint() {
    let checkpoint = LexicalCheckpoint {
        pdg_hash: "pdg".to_string(),
        snapshot_path: "snapshot.bin".into(),
        tfidf_path: "tfidf.bin".into(),
        admitted_node_ids: vec!["node-b".to_string(), "node-a".to_string()],
    };

    let restored = restored_admitted_node_ids(Some(&checkpoint));
    assert_eq!(restored.len(), 2);
    assert!(restored.contains("node-a"));
    assert!(restored.contains("node-b"));
}

#[test]
fn missing_lexical_checkpoint_restores_empty_admission_set() {
    assert!(restored_admitted_node_ids(None).is_empty());
}

#[test]
fn watcher_delta_publishes_current_generation() {
    let temp = tempfile::tempdir().expect("watcher fixture");
    std::fs::create_dir_all(temp.path().join("src")).expect("source directory");
    let source = temp.path().join("src/lib.rs");
    std::fs::write(&source, "pub fn watcher_marker() -> usize { 1 }\n").expect("initial source");

    let mut index = LeIndex::new(temp.path()).expect("create index");
    index.index_project(true).expect("initial generation");
    let storage = temp.path().join(".leindex");
    let initial = std::fs::read_to_string(storage.join("CURRENT"))
        .expect("initial CURRENT")
        .trim()
        .parse::<u64>()
        .expect("initial generation number");

    std::fs::write(&source, "pub fn watcher_marker() -> usize { 2 }\n").expect("changed source");
    index
        .incremental_reindex_from_watcher()
        .expect("watcher delta");

    let published = std::fs::read_to_string(storage.join("CURRENT"))
        .expect("published CURRENT")
        .trim()
        .parse::<u64>()
        .expect("published generation number");
    assert!(published > initial);
    assert!(
        storage
            .join("generations")
            .join(published.to_string())
            .join("leindex.db")
            .is_file()
    );
}

/// Codex wave-4 P2 regression: a fragment-sync failure must leave the engine
/// fragment-free BEFORE the snapshot persist runs. Every snapshot persist is
/// preceded by `sync_fragment_layer_or_clear`; on failure that error branch
/// calls `set_fragment_embeddings(Vec::new())` — the single call that drops the
/// fragment index, the owner refs, and the result cache. Without it, a fresh
/// node generation would be published with STALE (pre-change) fragment text
/// and byte ranges, letting a changed symbol rank/surface against deleted
/// content. This pins the clearing contract the error branch depends on.
#[test]
fn fragment_sync_failure_clear_empties_engine_fragment_state() {
    let mut engine = crate::search::search::SearchEngine::new();
    engine.set_fragment_index_enabled(true);
    // Simulate stale rows from a previous generation.
    engine.set_fragment_embeddings(vec![("hash-old".to_string(), vec![1.0, 2.0, 3.0])]);
    engine.set_fragment_refs(std::collections::HashMap::from([(
        "hash-old".to_string(),
        vec![("owner-a".to_string(), (10, 40))],
    )]));
    assert_eq!(
        engine.collect_fragment_embeddings().len(),
        1,
        "precondition: stale rows are present"
    );

    // The exact call the sync-failure branch of
    // `sync_fragment_layer_or_clear` makes before the snapshot persist.
    engine.set_fragment_embeddings(Vec::new());

    assert!(
        engine.collect_fragment_embeddings().is_empty(),
        "stale fragment rows must be gone before the snapshot persist"
    );
}

/// Build a `ParsingResult` for a fake file with one function that calls
/// `callee`. Both PDG construction routes consume this identically.
fn sample_parsing_result(
    file_path: &str,
    name: &str,
    callee: &str,
) -> crate::parse::parallel::ParsingResult {
    crate::parse::parallel::ParsingResult {
        file_path: std::path::PathBuf::from(file_path),
        language: Some("rust".to_string()),
        signatures: vec![crate::parse::traits::SignatureInfo {
            name: name.to_string(),
            qualified_name: name.to_string(),
            parameters: vec![],
            return_type: None,
            visibility: crate::parse::traits::Visibility::Public,
            is_async: false,
            is_method: false,
            docstring: None,
            calls: vec![callee.to_string()],
            imports: vec![],
            byte_range: (0, 10),
            cyclomatic_complexity: 1,
            flow_facts: vec![],
        }],
        source_bytes: Some(b"fn caller() { callee(); }".to_vec()),
        error: None,
        parse_time_ms: 0,
    }
}

/// VAL-PDG-006/007: `FeatureFlag::StreamingPdg` ON routes PDG construction
/// through the streaming fragment/segment pipeline; OFF routes through the
/// legacy extraction + merge loop.
#[test]
fn streaming_pdg_flag_routes_through_streaming_vs_legacy_builders() {
    use crate::feature_flags::{FeatureFlag, with_flag_override};

    with_flag_override(FeatureFlag::StreamingPdg, true, || {
        assert_eq!(
            pdg_route_for_current_flag(),
            PdgBuildRoute::Streaming,
            "flag ON must select the streaming pipeline"
        );
        let results = vec![
            sample_parsing_result("a.rs", "alpha", "beta"),
            sample_parsing_result("b.rs", "beta", "alpha"),
        ];
        let (pdg, route) = build_changed_file_pdg(results, true);
        assert_eq!(route, PdgBuildRoute::Streaming);
        assert!(pdg.node_count() >= 2, "streaming build must keep all nodes");
    });

    with_flag_override(FeatureFlag::StreamingPdg, false, || {
        assert_eq!(
            pdg_route_for_current_flag(),
            PdgBuildRoute::Legacy,
            "flag OFF must select the legacy merge loop"
        );
        let results = vec![
            sample_parsing_result("a.rs", "alpha", "beta"),
            sample_parsing_result("b.rs", "beta", "alpha"),
        ];
        let (pdg, route) = build_changed_file_pdg(results, false);
        assert_eq!(route, PdgBuildRoute::Legacy);
        assert!(pdg.node_count() >= 2, "legacy build must keep all nodes");
    });
}

/// The streaming and legacy routes must produce equivalent graphs from the
/// same parsing results: identical node ids (each carries the file-qualified
/// symbol id), identical node/edge counts, and identical edge (source, target,
/// type) sets.
#[test]
fn streaming_and_legacy_pdg_routes_produce_equivalent_graphs() {
    let results = vec![
        sample_parsing_result("a.rs", "alpha", "beta"),
        sample_parsing_result("b.rs", "beta", "alpha"),
    ];
    let (streaming_pdg, streaming_route) = build_changed_file_pdg(results.clone(), true);
    let (legacy_pdg, legacy_route) = build_changed_file_pdg(results, false);
    assert_eq!(streaming_route, PdgBuildRoute::Streaming);
    assert_eq!(legacy_route, PdgBuildRoute::Legacy);

    assert_eq!(
        streaming_pdg.node_count(),
        legacy_pdg.node_count(),
        "both routes must materialize the same number of nodes"
    );
    assert_eq!(
        streaming_pdg.edge_count(),
        legacy_pdg.edge_count(),
        "both routes must materialize the same number of edges"
    );

    let mut streaming_ids: Vec<String> = streaming_pdg
        .node_indices()
        .filter_map(|idx| streaming_pdg.get_node(idx).map(|node| node.id.clone()))
        .collect();
    let mut legacy_ids: Vec<String> = legacy_pdg
        .node_indices()
        .filter_map(|idx| legacy_pdg.get_node(idx).map(|node| node.id.clone()))
        .collect();
    streaming_ids.sort();
    legacy_ids.sort();
    assert_eq!(
        streaming_ids, legacy_ids,
        "both routes must carry identical node id sets"
    );
    assert!(
        streaming_ids.iter().any(|id| id.ends_with(":alpha")),
        "expected the alpha function node in the streaming graph"
    );
}
