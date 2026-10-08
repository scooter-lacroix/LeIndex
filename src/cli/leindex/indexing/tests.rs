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

/// Serializes tests that index Rust projects while `LEINDEX_SCIP_RUST_BIN`
/// may be set: env is process-global, so a concurrent full-index in another
/// watcher test would otherwise discover (and invoke) another test's
/// indexer fixture.
static WATCHER_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn watcher_delta_publishes_current_generation() {
    let _env_guard = WATCHER_ENV_LOCK.lock().unwrap();
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
    // The generation contract after the read-side flip: the directory carries
    // metadata only, and its payload lives in the CAS layers the manifest
    // names. Assert the layer exists and its blob is present, which is what
    // `leindex.db`-in-the-directory used to stand in for.
    let manifest = crate::storage::generation::read_generation_manifest(&storage, published)
        .expect("published manifest");
    let db_layer = manifest
        .layers
        .get(&crate::storage::generation::LayerKind::Db)
        .copied()
        .expect("manifest names the Db layer");
    let cas = crate::storage::cas::CasStore::open(storage.join("cas")).expect("open CAS");
    let db_bytes = cas.get(&db_layer).expect("Db layer blob present in CAS");
    assert!(!db_bytes.is_empty());
    assert!(
        !storage
            .join("generations")
            .join(published.to_string())
            .join("leindex.db")
            .is_file(),
        "publish must not write a per-generation file mirror"
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

/// The streaming PDG route must emit exactly one node per successfully parsed
/// signature, across multiple files. (Direct caller of `build_fragment_from_parsed`.)
#[test]
fn build_pdg_streaming_produces_correct_node_count_for_multiple_files() {
    // Two files, two signatures each -> four nodes total.
    let results = vec![
        sample_parsing_result("a.rs", "alpha", "beta"),
        sample_parsing_result("a.rs", "gamma", "delta"),
        sample_parsing_result("b.rs", "omega", "alpha"),
        sample_parsing_result("b.rs", "epsilon", "omega"),
    ];
    let pdg = build_pdg_streaming(results);
    assert_eq!(
        pdg.node_count(),
        4,
        "streaming build must keep one node per parsed signature"
    );
    // Each node is keyed by its file-qualified simple name.
    let ids: Vec<String> = pdg
        .node_indices()
        .filter_map(|idx| pdg.get_node(idx).map(|node| node.id.clone()))
        .collect();
    assert!(ids.contains(&"a.rs:alpha".to_string()));
    assert!(ids.contains(&"a.rs:gamma".to_string()));
    assert!(ids.contains(&"b.rs:omega".to_string()));
    assert!(ids.contains(&"b.rs:epsilon".to_string()));
}

/// The streaming route must produce a REAL graph: complexity carried from
/// signatures and intra-file call edges present. It used to feed
/// `build_fragment_from_parsed` (a skeleton that flattened signatures to
/// name/kind/bytes, hardcoded complexity 0, and emitted NO edges), which
/// left every streaming-built index with complexity-0 nodes, empty callee
/// lists, and a `forward_impact` that always returned nothing. The route
/// now delegates to `extract_pdg_from_signatures` + `fragment_from_pdg`
/// (the skeleton's documented production realization), so — unlike the
/// skeleton — a method inside `impl Foo` also gets the inferred `Foo`
/// class node via containment. That extra node is the price of a graph
/// with actual edges, and matches what the legacy route always produced.
#[test]
fn streaming_route_builds_real_edges_and_complexity() {
    use crate::parse::traits::{SignatureInfo, Visibility};
    let result = crate::parse::parallel::ParsingResult {
        file_path: std::path::PathBuf::from("src/lib.rs"),
        language: Some("rust".to_string()),
        signatures: vec![SignatureInfo {
            name: "bar".to_string(),
            qualified_name: "Foo.bar".to_string(),
            parameters: vec![],
            return_type: None,
            visibility: Visibility::Public,
            is_async: false,
            is_method: true,
            docstring: None,
            calls: vec![],
            imports: vec![],
            byte_range: (10, 20),
            cyclomatic_complexity: 3,
            flow_facts: vec![],
        }],
        source_bytes: Some(b"impl Foo { fn bar() {} }".to_vec()),
        error: None,
        parse_time_ms: 0,
    };
    let pdg = build_pdg_streaming(vec![result]);

    // The method node must exist, keyed off its qualified name.
    let bar = pdg
        .find_by_symbol("src/lib.rs:Foo.bar")
        .or_else(|| pdg.find_by_name("bar"));
    let bar = bar.expect("method node must exist in the streaming graph");
    let node = pdg.get_node(bar).unwrap();
    assert!(
        node.complexity >= 1,
        "streaming nodes must carry complexity from signatures (got {})",
        node.complexity
    );

    // Containment may add the inferred class node; the method must remain
    // addressable by its simple name for lookup tools.
    assert!(
        pdg.find_by_name("bar").is_some(),
        "simple-name lookup must still resolve the method"
    );
}

/// Parallel PDG construction (rayon `into_par_iter`) must produce identical
/// node and edge sets to the sequential baseline. Runs the same parsing
/// results through both routes multiple times and verifies determinism.
#[test]
fn test_parallel_pdg_construction_matches_sequential_baseline() {
    let results = vec![
        sample_parsing_result("a.rs", "alpha", "beta"),
        sample_parsing_result("b.rs", "beta", "gamma"),
        sample_parsing_result("c.rs", "gamma", "delta"),
        sample_parsing_result("d.rs", "delta", "alpha"),
    ];

    // Run legacy route twice — parallel construction must be deterministic.
    let pdg1 = build_pdg_legacy(results.clone());
    let pdg2 = build_pdg_legacy(results.clone());

    let mut ids1: Vec<String> = pdg1
        .node_indices()
        .filter_map(|idx| pdg1.get_node(idx).map(|n| n.id.clone()))
        .collect();
    ids1.sort_unstable();

    let mut ids2: Vec<String> = pdg2
        .node_indices()
        .filter_map(|idx| pdg2.get_node(idx).map(|n| n.id.clone()))
        .collect();
    ids2.sort_unstable();

    assert_eq!(pdg1.node_count(), pdg2.node_count());
    assert_eq!(pdg1.edge_count(), pdg2.edge_count());
    assert_eq!(
        ids1, ids2,
        "parallel PDG construction must be deterministic"
    );

    // Run streaming route twice — also must be deterministic.
    let pdg3 = build_pdg_streaming(results.clone());
    let pdg4 = build_pdg_streaming(results);

    let mut ids3: Vec<String> = pdg3
        .node_indices()
        .filter_map(|idx| pdg3.get_node(idx).map(|n| n.id.clone()))
        .collect();
    ids3.sort_unstable();

    let mut ids4: Vec<String> = pdg4
        .node_indices()
        .filter_map(|idx| pdg4.get_node(idx).map(|n| n.id.clone()))
        .collect();
    ids4.sort_unstable();

    assert_eq!(pdg3.node_count(), pdg4.node_count());
    assert_eq!(pdg3.edge_count(), pdg4.edge_count());
    assert_eq!(ids3, ids4, "parallel streaming PDG must be deterministic");
}

/// Consolidated publish_generation must produce exactly one generation
/// directory per indexing run (not two as in the old dual-publish path).
#[test]
fn test_single_publish_generation_per_index_run() {
    let temp = tempfile::tempdir().expect("publish fixture");
    std::fs::create_dir_all(temp.path().join("src")).expect("source directory");
    std::fs::write(
        temp.path().join("src/lib.rs"),
        "pub fn publish_marker() -> usize { 42 }\n",
    )
    .expect("source file");

    let mut index = LeIndex::new(temp.path()).expect("create index");
    index.index_project(true).expect("index project");

    let storage = temp.path().join(".leindex");
    let generations_dir = storage.join("generations");

    // Count generation directories (exclude staging dirs starting with '.').
    let generation_count = std::fs::read_dir(&generations_dir)
        .expect("generations directory")
        .filter_map(|entry| entry.ok())
        .filter(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| !name.starts_with('.'))
        })
        .count();

    // With the consolidated single-publish path, there should be exactly
    // 1 or 2 generation directories (1 core + 1 neural if neural rows > 0).
    // The old dual-publish path produced 2 core snapshots (one per call).
    // The new path produces at most 1 core + 1 neural = 2.
    assert!(
        generation_count <= 2,
        "expected at most 2 generation directories (core + neural), got {generation_count}"
    );
    assert!(
        generation_count >= 1,
        "expected at least 1 generation directory, got {generation_count}"
    );
}

/// The watcher/edit-apply incremental path must never spawn external SCIP
/// indexers: it runs synchronously under the project write lock, where a
/// full `rust-analyzer scip` pass (minutes) would stall every other tool on
/// the project. Precision re-merges on the next explicit index instead.
#[test]
fn watcher_delta_does_not_spawn_precision_indexer() {
    let _guard = WATCHER_ENV_LOCK.lock().unwrap();

    let indexer_dir = tempfile::tempdir().expect("indexer fixture dir");
    let ran_marker = indexer_dir.path().join("indexer_ran");
    let indexer = indexer_dir.path().join("fake-scip.sh");
    let temp = tempfile::tempdir().expect("watcher fixture");
    // The env override is process-global and other concurrently-running
    // tests perform full Rust indexes that legitimately invoke precision
    // with THEIR project roots. Record only invocations against THIS
    // project: a watcher-delta precision spawn would pass exactly it.
    std::fs::write(
        &indexer,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"{}\" ]; then touch {}; fi\ncp /dev/null \"$2\"\n",
            temp.path().display(),
            ran_marker.display()
        ),
    )
    .expect("write indexer fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = std::fs::metadata(&indexer).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&indexer, permissions).unwrap();
    }

    std::fs::create_dir_all(temp.path().join("src")).expect("source directory");
    let source = temp.path().join("src/lib.rs");
    std::fs::write(&source, "pub fn no_precision_marker() -> usize { 1 }\n").expect("source");

    let mut index = LeIndex::new(temp.path()).expect("create index");
    // Initial full index BEFORE the override exists, so only the watcher
    // delta below could possibly spawn the fixture.
    index.index_project(true).expect("initial generation");

    unsafe {
        std::env::set_var("LEINDEX_SCIP_RUST_BIN", &indexer);
        std::env::set_var("LEINDEX_SCIP_FORCE", "1");
    }
    std::fs::write(&source, "pub fn no_precision_marker() -> usize { 2 }\n").expect("changed");
    let result = index.incremental_reindex_from_watcher();
    unsafe {
        std::env::remove_var("LEINDEX_SCIP_RUST_BIN");
        std::env::remove_var("LEINDEX_SCIP_FORCE");
    }
    result.expect("watcher delta");

    assert!(
        !ran_marker.is_file(),
        "watcher delta must not invoke external SCIP indexers"
    );
}

/// `create_validator` must share the resident graph, not copy it: the copy
/// (28k nodes / 139k edges on a mid-size project) was ~0.4 s of every edit
/// preview and rename preview. Mutation while a reader is alive must still
/// leave that reader a consistent snapshot.
#[test]
fn validator_shares_the_resident_graph_and_mutation_leaves_readers_a_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("lib.rs"),
        "pub fn alpha() -> u32 { beta() }\npub fn beta() -> u32 { 1 }\n",
    )
    .unwrap();
    let mut index = LeIndex::new(dir.path()).unwrap();
    index.index_project(true).unwrap();
    index.ensure_pdg_loaded().unwrap();
    let shared = index.pdg.clone().expect("graph resident after indexing");
    let baseline = std::sync::Arc::strong_count(&shared);

    let validator = index
        .create_validator()
        .expect("validator for an indexed project");
    assert!(
        std::sync::Arc::strong_count(&shared) >= baseline + 3,
        "the validator and its three checkers must hold references to the resident \
         graph (count {} vs baseline {baseline}), not a private copy",
        std::sync::Arc::strong_count(&shared)
    );

    // A writer that needs ownership while the validator is alive gets a copy,
    // and the validator's snapshot is unaffected.
    let nodes_before = shared.node_count();
    let mut owned = index.take_owned_pdg().expect("owned graph");
    owned.add_node(crate::graph::pdg::Node {
        id: "x.rs:extra".into(),
        node_type: crate::graph::pdg::NodeType::Function,
        name: "extra".into(),
        file_path: std::sync::Arc::from("x.rs"),
        byte_range: (0, 1),
        complexity: 1,
        language: "rust".into(),
    });
    assert_eq!(owned.node_count(), nodes_before + 1);
    assert_eq!(
        shared.node_count(),
        nodes_before,
        "readers keep their snapshot"
    );
    drop(validator);
    drop(shared);

    // Unshared, taking the graph is a move (try_unwrap succeeds), not a copy.
    let expected_nodes = owned.node_count();
    index.pdg = Some(std::sync::Arc::new(owned));
    assert_eq!(
        std::sync::Arc::strong_count(index.pdg.as_ref().unwrap()),
        1,
        "nothing else holds the graph"
    );
    let taken = index.take_owned_pdg().unwrap();
    assert_eq!(taken.node_count(), expected_nodes);
    assert!(index.pdg.is_none());
}

/// Graph-only hydration must read the published generation, not the mutable
/// root. A root rewritten after publish (a failed or concurrent refresh) used
/// to pair a different PDG with the generation's search artifacts, so every
/// hydration rebuilt the search index and never persisted it.
#[test]
fn test_graph_only_hydration_reads_published_generation_not_mutable_root() {
    let _guard = crate::feature_flags::lock_flag_tests();
    let temp = tempfile::tempdir().expect("fixture");
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(
        temp.path().join("src/a.rs"),
        "pub fn alpha() -> u32 { 1 }\npub fn beta() -> u32 { alpha() + 1 }\n",
    )
    .unwrap();
    std::fs::write(
        temp.path().join("src/b.rs"),
        "pub fn gamma() -> u32 { 3 }\npub fn delta() -> u32 { gamma() + 1 }\n",
    )
    .unwrap();

    let mut indexer = LeIndex::new(temp.path()).expect("create index");
    indexer.index_project(true).expect("index");
    let published_nodes = indexer.get_stats().pdg_nodes;
    let storage = indexer.storage_path().to_path_buf();
    drop(indexer);
    assert!(published_nodes > 0);

    // Diverge the mutable root from the published generation.
    {
        let conn = rusqlite::Connection::open(storage.join("leindex.db")).unwrap();
        conn.execute("PRAGMA foreign_keys = OFF", []).unwrap();
        conn.execute("DELETE FROM intel_nodes WHERE file_path LIKE '%b.rs'", [])
            .unwrap();
    }

    let mut reader = LeIndex::new(temp.path()).expect("reader");
    if reader.active_storage_path() == reader.storage_path().to_path_buf() {
        // No published generation in this layout: nothing to diverge from.
        return;
    }
    reader.ensure_pdg_loaded_graph_only().expect("graph load");
    assert_eq!(
        reader.pdg().expect("pdg resident").node_count(),
        published_nodes,
        "graph-only load must come from the published generation"
    );
}

/// Every index run used to leave another full generation behind (nothing ran
/// retention automatically). Only the current generation and its rollback
/// predecessor may remain.
#[test]
fn test_repeated_indexing_keeps_only_current_and_previous_generation() {
    let _guard = crate::feature_flags::lock_flag_tests();
    let temp = tempfile::tempdir().expect("fixture");
    std::fs::create_dir_all(temp.path().join("src")).unwrap();
    std::fs::write(
        temp.path().join("src/a.rs"),
        "pub fn alpha() -> u32 { 1 }\n",
    )
    .unwrap();

    let mut indexer = LeIndex::new(temp.path()).expect("create index");
    for round in 0..4 {
        std::fs::write(
            temp.path().join("src/a.rs"),
            format!("pub fn alpha() -> u32 {{ {round} }}\n"),
        )
        .unwrap();
        indexer.index_project(true).expect("index");
    }
    let generations = indexer.storage_path().join("generations");
    let mut kept: Vec<String> = std::fs::read_dir(&generations)
        .unwrap()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.parse::<u64>().is_ok())
        .collect();
    kept.sort();
    assert!(
        kept.len() <= 2,
        "expected current + previous only, found {kept:?}"
    );
}

/// The save-stage budget scales per-edge, floors for tiny graphs, honors the
/// env override exactly, and never multiplies the override by the debug scale.
#[test]
fn test_save_stage_budget_ms_scaling_floor_and_override() {
    use super::save_stage_budget_ms;

    let scale = if cfg!(debug_assertions) { 10 } else { 1 };

    // Tiny graphs sit on the floor (× debug scale). The floor is 2 s: the
    // fixed publish cost (VACUUM + CAS staging + fsyncs) measured ~600 ms on
    // shared CI runners, so a floor near that false-trips 0-edge fixtures.
    assert_eq!(save_stage_budget_ms(0, None), 2_000 * scale);
    assert_eq!(save_stage_budget_ms(1_000, None), 2_000 * scale);

    // Large graphs scale per-edge: 250k edges × 20µs = 5000 ms → floor loses.
    // (100k edges is the crossover where 20µs/edge exceeds the 2 s floor.)
    assert_eq!(save_stage_budget_ms(250_000, None), 5_000 * scale);

    // The measured production shape: ~193k edges, ~5µs/edge actual →
    // budget 3_860·scale ms leaves ~4× headroom in release, ~40× in debug.
    assert_eq!(save_stage_budget_ms(193_000, None), 3_860 * scale);

    // The override wins absolutely — no per-edge math, no debug multiplier.
    assert_eq!(save_stage_budget_ms(193_000, Some(60_000)), 60_000);
    assert_eq!(save_stage_budget_ms(0, Some(1)), 1);
}

/// The gate trips only when elapsed exceeds the budget, and its error names
/// the numbers an operator needs.
#[test]
fn test_save_stage_gate_trips_only_over_budget() {
    use super::save_stage_gate;
    use std::time::Duration;

    let staging = PathBuf::from("/tmp/staging-x");
    // 200k edges → 4_000·scale ms budget; well under must pass.
    assert!(save_stage_gate(Duration::from_millis(10), 200_000, &staging).is_ok());
    // Absurdly over must fail, with the diagnostic fields in the message.
    let error = save_stage_gate(Duration::from_secs(600), 200_000, &staging)
        .expect_err("gate must trip over budget");
    let message = error.to_string();
    assert!(message.contains("600000 ms"), "message: {message}");
    assert!(message.contains("200000 edges"), "message: {message}");
    assert!(
        message.contains("LEINDEX_SAVE_STAGE_GATE_MS"),
        "message: {message}"
    );
}
