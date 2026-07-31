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
