//! WS4 Task 14: read-path wiring onto leased mmap generations.
//!
//! Verifies (VAL-EQUIV-001/002/003) that the search/symbol/deep-analyze read
//! path hydrates from the generation store's leased mmap layers and returns
//! bit-for-bit identical results to the legacy heap-mirror path, plus the
//! no-stall (mid-index reads serve the prior generation immediately) and
//! no-writer-contention properties (readers never acquire the writer's locks,
//! so they cannot delay publication).

use super::*;
use crate::feature_flags::{
    FeatureFlag, clear_flag_overrides_for_test, lock_flag_tests, set_flag_override_for_test,
};
use crate::graph::pdg::ProgramDependenceGraph;
use crate::storage::cas::CasStore;
use crate::storage::generation::migrate::{
    encode_empty_neural, encode_empty_tfidf, encode_pdg_layer_v2, encode_symbols_layer,
    vacuum_bytes,
};
use crate::storage::generation::{
    GenerationSnapshot, GenerationWriter, LayerKind, read_current_generation,
};
use rusqlite::Connection;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

// ===========================================================================
// Fixture helpers
// ===========================================================================

/// Write a small, deterministic Rust project with distinct symbol names that
/// TF-IDF search can retrieve.
fn write_fixture_project(project: &std::path::Path) {
    std::fs::create_dir_all(project.join("src")).expect("create src");
    std::fs::write(
        project.join("src/lib.rs"),
        "pub mod auth;\npub mod db;\n\npub fn retry_operation<T>(op: impl Fn() -> T) -> T {\n    op()\n}\n\npub fn normalize_path(path: &str) -> String {\n    path.trim().to_string()\n}\n",
    )
    .expect("write lib.rs");
    std::fs::write(
        project.join("src/auth.rs"),
        "pub fn authenticate_user(username: &str, password: &str) -> bool {\n    !username.is_empty() && !password.is_empty()\n}\n\npub fn hash_password(password: &str) -> u64 {\n    password.len() as u64\n}\n\npub fn validate_token(token: &str) -> bool {\n    !token.is_empty()\n}\n\npub fn encrypt_payload(data: &[u8]) -> Vec<u8> {\n    data.to_vec()\n}\n",
    )
    .expect("write auth.rs");
    std::fs::write(
        project.join("src/db.rs"),
        "pub fn connect_database(host: &str, port: u16) -> String {\n    format!(\"{}:{}\", host, port)\n}\n\npub fn fetch_records(query: &str) -> Vec<String> {\n    vec![query.to_string()]\n}\n\npub fn build_connection_string(host: &str, port: u16) -> String {\n    connect_database(host, port)\n}\n\npub fn parse_config(content: &str) -> u64 {\n    content.len() as u64\n}\n",
    )
    .expect("write db.rs");
}

/// Full index + drop, so the live store holds the catalog the generation
/// layers are encoded from.
fn index_project_fixture(project: &std::path::Path) {
    let mut index = LeIndex::new(project).expect("create index");
    index.index_project(true).expect("index project");
}

/// Publish a CAS generation from the live store's catalog. The five layers are
/// encoded with the exact same encoders the migration uses, so the generation
/// is byte-level faithful to the heap-mirror data.
fn publish_live_generation(storage_root: &std::path::Path, generation: u64) {
    let cas = Arc::new(Mutex::new(
        CasStore::open(storage_root.join("cas")).expect("open cas"),
    ));
    let mut writer = GenerationWriter::new(storage_root, cas);
    let db_path = storage_root.join("leindex.db");

    let db_bytes = vacuum_bytes(&db_path).expect("vacuum db");
    let conn = Connection::open(&db_path).expect("open db");
    let pdg_bytes = encode_pdg_layer_v2(&conn).expect("encode pdg");
    let symbols_bytes = encode_symbols_layer(&conn).expect("encode symbols");
    drop(conn);

    writer.stage(LayerKind::Db, &db_bytes).expect("stage db");
    writer
        .stage(LayerKind::Tfidf, &encode_empty_tfidf())
        .expect("stage tfidf");
    writer
        .stage(LayerKind::Neural, &encode_empty_neural())
        .expect("stage neural");
    writer.stage(LayerKind::Pdg, &pdg_bytes).expect("stage pdg");
    writer
        .stage(LayerKind::Symbols, &symbols_bytes)
        .expect("stage symbols");
    writer.publish(generation).expect("publish generation");
}

/// Canonical, order-stable serialization of a PDG so two graphs can be
/// compared byte-for-byte regardless of internal node-id ordering.
fn canonical_pdg(pdg: &ProgramDependenceGraph) -> Vec<u8> {
    use petgraph::visit::{EdgeRef, IntoEdgeReferences};
    let mut nodes: Vec<(usize, String, String, String, String, (usize, usize), u32)> = Vec::new();
    for idx in pdg.node_indices() {
        if let Some(n) = pdg.get_node(idx) {
            nodes.push((
                idx.index(),
                n.id.clone(),
                n.name.clone(),
                format!("{:?}", n.node_type),
                n.file_path.to_string(),
                n.byte_range,
                n.complexity,
            ));
        }
    }
    nodes.sort();
    let mut edges: Vec<(usize, usize, String)> = Vec::new();
    for e in pdg.graph.edge_references() {
        edges.push((
            e.source().index(),
            e.target().index(),
            format!("{:?}", e.weight().edge_type),
        ));
    }
    edges.sort();
    serde_json::to_vec(&(nodes, edges, pdg.node_count(), pdg.edge_count())).unwrap()
}

/// Hydrate a heap-mirror instance. `artifact_dir` is a fresh directory with no
/// legacy artifacts, forcing the same rebuild path the generation instance
/// uses so the comparison isolates the data source (live store vs generation).
fn hydrate_heap_instance(project: &std::path::Path, artifact_dir: &std::path::Path) -> LeIndex {
    let mut idx = LeIndex::new(project).expect("new heap index");
    idx.load_from_storage_inner_at(false, None, artifact_dir.to_path_buf())
        .expect("hydrate heap instance");
    idx
}

/// Hydrate a generation-reader instance through the real flag-gated entry
/// point (`load_from_active_storage`). Requires the caller to have set the
/// `GenerationReaders` override ON (tests hold `FLAG_TEST_LOCK`).
fn hydrate_generation_instance(project: &std::path::Path) -> LeIndex {
    let mut idx = LeIndex::new(project).expect("new generation index");
    let wired = idx
        .try_hydrate_from_generation()
        .expect("hydrate from generation");
    assert!(
        wired,
        "generation read path must engage when the flag is on"
    );
    idx
}

const QUERIES: &[&str] = &[
    "authenticate user",
    "database connection",
    "password hashing",
    "token validation",
    "encrypt payload",
    "fetch records",
    "connection string",
    "parse config",
    "retry operation",
    "normalize path",
    "authentication",
    "password",
    "database",
    "connect",
    "records",
    "config",
    "token",
    "encryption",
    "retry",
    "path normalization",
    "secure login",
    "query builder",
];

// ===========================================================================
// Tests
// ===========================================================================

/// VAL-EQUIV-001/002/003: with the flag on, the search/symbol/deep-analyze
/// read path hydrates from the leased mmap generation and produces byte-level
/// identical output to the legacy heap-mirror path (default off).
#[test]
fn test_read_path_bit_for_bit_equivalence() {
    let _guard = lock_flag_tests();
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_project(dir.path());
    index_project_fixture(dir.path());
    let storage_root = dir.path().join(".leindex");
    publish_live_generation(&storage_root, 1);

    let artifact_dir = tempfile::tempdir().expect("artifact tempdir");
    let mut heap = hydrate_heap_instance(dir.path(), artifact_dir.path());
    assert!(heap.pdg().is_some(), "heap instance must load the pdg");

    // Flag ON for the generation instance only.
    set_flag_override_for_test(FeatureFlag::GenerationReaders, true);
    let mut generation = hydrate_generation_instance(dir.path());
    clear_flag_overrides_for_test();

    assert!(
        generation.generation_snapshot.is_some(),
        "generation instance must retain the leased snapshot"
    );
    assert!(
        !generation.search_engine().is_empty(),
        "generation instance must hydrate a populated search engine"
    );
    assert!(
        generation.pdg().is_some(),
        "generation instance must load the pdg"
    );

    // Search output must be byte-for-byte identical across all queries
    // (anti-cheat section 2.1: no output may differ between the two paths).
    for query in QUERIES {
        let heap_results =
            serde_json::to_vec(&heap.search(query, 10, None).expect("heap search")).unwrap();
        let generation_results = serde_json::to_vec(
            &generation
                .search(query, 10, None)
                .expect("generation search"),
        )
        .unwrap();
        assert_eq!(
            heap_results, generation_results,
            "search output must be bit-for-bit identical for query {query:?}"
        );
    }

    // Symbol / deep-analyze data comes from the PDG: both paths must expose
    // the same graph content byte-for-byte.
    let heap_pdg = canonical_pdg(heap.pdg().unwrap());
    let generation_pdg = canonical_pdg(generation.pdg().unwrap());
    assert_eq!(
        heap_pdg, generation_pdg,
        "PDG (symbol + deep-analyze source) must be bit-identical"
    );
}

/// No-stall test: a search fired mid-index returns immediately by serving the
/// prior (leased) generation while the writer holds the write lock and is
/// mid-publication of the next generation.
#[test]
fn test_no_stall_read_during_index() {
    let _guard = lock_flag_tests();
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_project(dir.path());
    index_project_fixture(dir.path());
    let storage_root = dir.path().join(".leindex");
    publish_live_generation(&storage_root, 1);

    // Writer: holds the cross-process write lock (flock), stages the next
    // generation's manifest WITHOUT swapping CURRENT (mid-publication), then
    // keeps "indexing" for a while before releasing. The reader below begins
    // only after `tx_mid` signals that gen 2's manifest exists but CURRENT is
    // still gen 1 — i.e. genuinely mid-publication.
    let (tx_mid, rx_mid) = mpsc::channel();
    let writer_storage = storage_root.clone();
    let writer = thread::spawn(move || {
        let _write_lock = ProjectWriteLock::acquire(&writer_storage).expect("acquire write lock");
        // Stage + write manifest for gen 2, but leave CURRENT on gen 1.
        let cas = Arc::new(Mutex::new(
            CasStore::open(writer_storage.join("cas")).expect("open cas"),
        ));
        let mut writer = GenerationWriter::new(&writer_storage, cas);
        let db_bytes = vacuum_bytes(&writer_storage.join("leindex.db")).expect("vacuum");
        writer.stage(LayerKind::Db, &db_bytes).expect("stage db");
        writer
            .stage(LayerKind::Tfidf, &encode_empty_tfidf())
            .expect("stage tfidf");
        writer
            .stage(LayerKind::Neural, &encode_empty_neural())
            .expect("stage neural");
        let conn = Connection::open(writer_storage.join("leindex.db")).expect("open db");
        let pdg_bytes = encode_pdg_layer_v2(&conn).expect("encode pdg");
        let symbols_bytes = encode_symbols_layer(&conn).expect("encode symbols");
        drop(conn);
        writer.stage(LayerKind::Pdg, &pdg_bytes).expect("stage pdg");
        writer
            .stage(LayerKind::Symbols, &symbols_bytes)
            .expect("stage symbols");
        writer
            .publish_manifest_only(2)
            .expect("publish manifest only");
        // Mid-publication: manifest for 2 exists, CURRENT is still 1, writer
        // still holds the write lock. Signal the reader, then keep "indexing"
        // (holding the lock / sleeping) so the reader genuinely overlaps it.
        tx_mid.send(()).expect("signal mid-publication");
        thread::sleep(Duration::from_millis(1500));
    });

    // Reader: a search fired mid-publication must return immediately reading
    // gen 1. It must not wait on the writer's write lock (which is held for
    // the full 1500ms sleep after this signal).
    rx_mid.recv().expect("writer is mid-publication");
    set_flag_override_for_test(FeatureFlag::GenerationReaders, true);
    let started = Instant::now();
    let mut reader = hydrate_generation_instance(dir.path());
    let results = reader
        .search("authenticate user", 10, None)
        .expect("search mid-index");
    let read_elapsed = started.elapsed();
    clear_flag_overrides_for_test();

    assert_eq!(
        read_current_generation(&storage_root),
        Some(1),
        "CURRENT still points to the prior generation during publication"
    );
    assert_eq!(
        reader.generation_snapshot.as_ref().unwrap().generation(),
        1,
        "reader served the prior generation"
    );
    assert!(
        !results.is_empty(),
        "search mid-index must return real results from the prior generation"
    );
    assert!(
        read_elapsed < Duration::from_millis(1500),
        "search mid-index must return immediately (writer holds the lock for 1500ms), \
         took {read_elapsed:?}"
    );
    drop(reader);
    writer.join().expect("writer thread");
}

/// No-writer-contention gate: a flock of concurrent generation readers must
/// not delay publication (the read path never acquires the writer's locks).
#[test]
fn test_read_path_no_writer_contention() {
    let _guard = lock_flag_tests();
    let dir = tempfile::tempdir().expect("tempdir");
    write_fixture_project(dir.path());
    index_project_fixture(dir.path());
    let storage_root = dir.path().join(".leindex");
    publish_live_generation(&storage_root, 1);

    // Baseline: publish with no concurrent readers (this becomes the current
    // generation the concurrent readers must serve).
    let baseline = Instant::now();
    publish_live_generation(&storage_root, 2);
    let t_baseline = baseline.elapsed();

    // Solo reader cycle: the yardstick for every timing bound below. Fixed
    // wall-clock limits flake on a loaded or slow machine (and a panic while
    // holding the flag lock used to poison it for unrelated tests); bounds
    // that scale with a measured baseline still catch a reader that blocks
    // the writer, which shows up as a large multiple, not a few milliseconds.
    set_flag_override_for_test(FeatureFlag::GenerationReaders, true);
    let solo = {
        let started = Instant::now();
        let mut idx = hydrate_generation_instance(dir.path());
        idx.search("authenticate user", 10, None)
            .expect("solo reader search");
        started.elapsed()
    };
    let reader_budget = (solo * 25).max(Duration::from_millis(800));
    let writer_slack = (solo * 4).max(Duration::from_millis(400));

    // Concurrent: 8 readers each open the leased snapshot and run a search
    // cycle while the writer publishes the next generation.
    let project = dir.path().to_path_buf();
    let readers: Vec<_> = (0..8)
        .map(|i| {
            let project = project.clone();
            let storage = storage_root.clone();
            thread::spawn(move || {
                // Every reader must see the current generation's content and
                // complete its search cycle promptly.
                let snapshot = GenerationSnapshot::open(&storage).expect("reader open snapshot");
                assert_eq!(
                    snapshot.generation(),
                    2,
                    "reader must serve the current generation"
                );
                let started = Instant::now();
                let mut idx = hydrate_generation_instance(&project);
                let results = idx
                    .search("authenticate user", 10, None)
                    .expect("reader search");
                assert!(
                    !results.is_empty(),
                    "reader {i} must get results from the current generation"
                );
                let elapsed = started.elapsed();
                assert!(
                    elapsed < reader_budget,
                    "reader {i} took {elapsed:?} while writer published \
                     (solo {solo:?}, budget {reader_budget:?})"
                );
            })
        })
        .collect();

    let concurrent = Instant::now();
    publish_live_generation(&storage_root, 3);
    let t_concurrent = concurrent.elapsed();

    // Readers must finish while the override is still ON: each reader's
    // hydration consults the flag, so it must not be cleared mid-flight.
    for (i, reader) in readers.into_iter().enumerate() {
        reader
            .join()
            .unwrap_or_else(|_| panic!("reader {i} panicked"));
    }
    clear_flag_overrides_for_test();

    assert!(
        t_concurrent <= t_baseline + writer_slack,
        "concurrent generation readers must not delay writes: baseline {t_baseline:?}, \
         with readers {t_concurrent:?}, allowed slack {writer_slack:?}"
    );
}
