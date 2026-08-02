use super::*;

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
