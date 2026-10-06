// Read-path tests for the PDG store. Post-D7 the write chain (save_pdg,
// delete_pdg, trigram writes) is deleted — graphs persist as generation
// layers and hydrate through them — so these tests cover exactly what
// remains: the legacy SQL read fallback (load_pdg), its trigram fallback,
// pdg_exists, and the indexed_files freshness helpers. Rows are seeded with
// raw SQL, the way a pre-flip legacy store holds them.

use crate::graph::pdg::{EdgeType as PDGEdgeType, NodeType as PDGNodeType};
use crate::storage::pdg_store::{
    delete_file_data, delete_files_data_tx, get_indexed_files, has_indexed_files, load_pdg,
    load_trigram_index, pdg_exists, update_indexed_file, update_indexed_files_tx,
};
use crate::storage::schema::Storage;

/// Seed one node row for `project_id` and return its db id.
fn seed_node(storage: &Storage, project_id: &str, node_id: &str, symbol: &str) -> i64 {
    storage
        .conn()
        .execute(
            "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, complexity, content_hash, byte_range_start, byte_range_end, created_at, updated_at)
             VALUES (?1, 'src/lib.rs', ?2, ?3, ?3, 'rust', 'function', 1, 'seed-hash', 0, 10, 0, 0)",
            rusqlite::params![project_id, node_id, symbol],
        )
        .expect("seed intel_nodes row");
    storage.conn().last_insert_rowid()
}

/// Seed one edge row between two db ids.
fn seed_edge(storage: &Storage, caller_id: i64, callee_id: i64) {
    storage
        .conn()
        .execute(
            "INSERT INTO intel_edges (caller_id, callee_id, edge_type) VALUES (?1, ?2, 'call')",
            rusqlite::params![caller_id, callee_id],
        )
        .expect("seed intel_edges row");
}

#[test]
fn test_load_pdg_roundtrips_legacy_sql_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");

    let caller = seed_node(&storage, "proj", "src/lib.rs:caller", "caller");
    let callee = seed_node(&storage, "proj", "src/lib.rs:callee", "callee");
    seed_edge(&storage, caller, callee);

    let pdg = load_pdg(&storage, "proj").expect("load pdg from legacy rows");
    assert_eq!(pdg.node_count(), 2);
    assert_eq!(pdg.edge_count(), 1);

    let caller_node = pdg
        .find_by_symbol("src/lib.rs:caller")
        .expect("caller node present");
    let node = pdg.get_node(caller_node).expect("caller node data");
    assert_eq!(node.name, "caller");
    assert_eq!(node.node_type, PDGNodeType::Function);
    assert_eq!(node.byte_range, (0, 10));

    let loaded_callee = pdg
        .find_by_symbol("src/lib.rs:callee")
        .expect("callee node present");
    assert!(
        pdg.neighbors(caller_node).contains(&loaded_callee),
        "seeded call edge must load as a caller->callee dependency"
    );
}

#[test]
fn test_load_pdg_marks_legacy_precision_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");

    storage
        .conn()
        .execute(
            "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, complexity, content_hash, byte_range_start, byte_range_end, created_at, updated_at, precision)
             VALUES ('proj', 'src/lib.rs', 'src/lib.rs:main', 'main', 'main', 'rust', 'function', 1, 'seed-hash', 0, 10, 0, 0, 1)",
            [],
        )
        .expect("seed precision node row");

    let pdg = load_pdg(&storage, "proj").expect("load pdg");
    assert!(
        pdg.is_precision_symbol("src/lib.rs:main"),
        "precision=1 rows must hydrate as precision-marked nodes"
    );
}

#[test]
fn test_load_pdg_trigram_fallback_rebuilds_when_row_absent() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");

    seed_node(&storage, "proj", "src/lib.rs:alpha", "alpha");

    assert!(
        load_trigram_index(&storage, "proj").unwrap().is_none(),
        "no persisted trigram row exists"
    );

    // The load-side contract (D5 verify): with no persisted trigram index,
    // load_pdg rebuilds one from the loaded nodes instead of failing.
    let pdg = load_pdg(&storage, "proj").expect("load pdg without trigram row");
    assert!(pdg.node_count() > 0);
}

#[test]
fn test_load_pdg_uses_persisted_trigram_index_when_present() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");

    seed_node(&storage, "proj", "src/lib.rs:alpha", "alpha");

    // Seed a trigram index row directly (the write path that used to create
    // it is deleted; a legacy store may still carry one).
    let mut graph = crate::graph::pdg::ProgramDependenceGraph::new();
    graph.add_node(crate::graph::pdg::Node {
        id: "src/lib.rs:alpha".to_string(),
        node_type: PDGNodeType::Function,
        name: "alpha".to_string(),
        file_path: std::sync::Arc::from("src/lib.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });
    let serialized = graph.trigram_index().serialize();
    storage
        .conn()
        .execute(
            "INSERT INTO trigram_index (project_id, index_data, node_count, trigram_count, updated_at)
             VALUES ('proj', ?1, 1, 1, 0)",
            rusqlite::params![serialized],
        )
        .expect("seed trigram row");

    let pdg = load_pdg(&storage, "proj").expect("load pdg with persisted trigram row");
    assert_eq!(
        pdg.trigram_index().trigram_count(),
        graph.trigram_index().trigram_count(),
        "persisted trigram index must be adopted, not rebuilt to a different shape"
    );
}

#[test]
fn test_pdg_exists_tracks_legacy_rows() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");

    assert!(!pdg_exists(&storage, "proj").unwrap());
    seed_node(&storage, "proj", "src/lib.rs:main", "main");
    assert!(pdg_exists(&storage, "proj").unwrap());
}

#[test]
fn test_indexed_file_freshness_helpers_survive_the_flip() {
    let temp = tempfile::tempdir().expect("tempdir");
    let mut storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");
    let project_id = "proj".to_string();

    assert!(!has_indexed_files(&storage, &project_id));

    update_indexed_file(&mut storage, &project_id, "src/a.rs", "hash-a").unwrap();
    update_indexed_file(&mut storage, &project_id, "src/b.rs", "hash-b").unwrap();
    assert!(has_indexed_files(&storage, &project_id));

    let files = get_indexed_files(&storage, &project_id).unwrap();
    assert_eq!(files.len(), 2);
    assert_eq!(files.get("src/a.rs").map(String::as_str), Some("hash-a"));

    // Batch upsert in a transaction (the freshness write path that remains).
    let tx = storage.conn_mut().transaction().expect("borrow tx");
    update_indexed_files_tx(
        &tx,
        &project_id,
        &[("src/c.rs".to_string(), "hash-c".to_string())],
    )
    .expect("batch upsert");
    tx.commit().expect("commit tx");

    let files = get_indexed_files(&storage, &project_id).unwrap();
    assert_eq!(files.len(), 3, "batched upsert must land");

    // Single-file deletion.
    delete_file_data(&mut storage, &project_id, "src/a.rs").unwrap();
    let files = get_indexed_files(&storage, &project_id).unwrap();
    assert_eq!(files.len(), 2, "deleted freshness record must be gone");
    assert!(!files.contains_key("src/a.rs"));

    // Batch deletion in a transaction.
    let tx = storage.conn_mut().transaction().expect("borrow tx");
    delete_files_data_tx(&tx, &project_id, &["src/b.rs".to_string()]).expect("batch delete");
    tx.commit().expect("commit tx");
    let files = get_indexed_files(&storage, &project_id).unwrap();
    assert_eq!(files.len(), 1, "batched delete must land");
    assert!(files.contains_key("src/c.rs"));
}

/// The edge-metadata decode path: all-null metadata is recognized without
/// parsing and round-trips as an empty `PDGEdgeMetadata`.
#[test]
fn test_load_pdg_decodes_edge_metadata() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");

    let caller = seed_node(&storage, "proj", "src/lib.rs:caller", "caller");
    let callee = seed_node(&storage, "proj", "src/lib.rs:callee", "callee");

    // Rich metadata: call_count 3, confidence 0.5.
    storage
        .conn()
        .execute(
            "INSERT INTO intel_edges (caller_id, callee_id, edge_type, metadata) VALUES (?1, ?2, 'call', ?3)",
            rusqlite::params![
                caller,
                callee,
                r#"{"call_count":3,"variable_name":null,"confidence":0.5,"channel":null,"position":null}"#
            ],
        )
        .expect("seed edge with metadata");

    let pdg = load_pdg(&storage, "proj").expect("load pdg");
    let caller_id = pdg.find_by_symbol("src/lib.rs:caller").unwrap();
    let callee_id = pdg.find_by_symbol("src/lib.rs:callee").unwrap();

    let mut found = None;
    for edge_id in pdg.edge_indices() {
        if pdg.edge_endpoints(edge_id) == Some((caller_id, callee_id)) {
            found = Some(pdg.get_edge(edge_id).expect("edge data").clone());
        }
    }
    let edge = found.expect("seeded edge must load");
    assert_eq!(edge.edge_type, PDGEdgeType::Call);
    let metadata = edge.metadata;
    assert_eq!(metadata.call_count, Some(3));
    assert_eq!(metadata.variable_name, None);
    assert_eq!(metadata.confidence, Some(0.5));
    assert_eq!(metadata.channel, None);
    assert_eq!(metadata.position, None);
}

#[test]
fn test_load_pdg_empty_project_yields_empty_graph() {
    let temp = tempfile::tempdir().expect("tempdir");
    let storage = Storage::open(temp.path().join("leindex.db")).expect("open storage");

    let pdg = load_pdg(&storage, "never-indexed").expect("load pdg for unknown project");
    assert_eq!(pdg.node_count(), 0);
    assert_eq!(pdg.edge_count(), 0);
}
