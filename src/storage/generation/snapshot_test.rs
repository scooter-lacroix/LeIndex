//! Tests for [`GenerationSnapshot`]: the leased, immutable read view over the
//! current generation used by the `generation-readers` read path (WS4 Task 14).
//!
//! Covers VAL-EQUIV-* plumbing:
//! - Opens the current generation with a [`GenerationLease`] and produces
//!   layer readers (Neural/Tfidf/Pdg/Symbols) that mmap the CAS blob files
//!   directly (no heap copy).
//! - Materializes the Db layer into a temp-owned SQLite file whose bytes are
//!   bit-identical to the staged generation blob.
//! - Raising a snapshot pins the generation's layer blobs via refcounts.

use crate::storage::cas::CasStore;
use crate::storage::generation::migrate::{
    encode_empty_neural, encode_empty_tfidf, encode_pdg_layer_v2, encode_symbols_layer,
    vacuum_bytes,
};
use crate::storage::generation::{
    GenerationSnapshot, GenerationWriter, LayerKind, Manifest, read_generation_manifest,
};
use rusqlite::Connection;
use std::sync::{Arc, Mutex};

// ===========================================================================
// Helpers
// ===========================================================================

/// Build a minimal catalog with `intel_nodes` / `intel_edges` rows at `path`.
fn write_catalog(path: &std::path::Path) {
    let conn = Connection::open(path).expect("open catalog");
    conn.execute_batch(
        "CREATE TABLE intel_nodes (id INTEGER PRIMARY KEY, node_id TEXT, symbol_name TEXT, \
         language TEXT, node_type TEXT, file_path TEXT, byte_range_start INTEGER, \
         byte_range_end INTEGER, complexity INTEGER, precision INTEGER);\
         CREATE TABLE intel_edges (caller_id INTEGER, callee_id INTEGER, edge_type TEXT, \
         metadata TEXT);\
         CREATE TABLE project_metadata (project_id TEXT, value TEXT);",
    )
    .expect("create tables");
    {
        let mut stmt = conn
            .prepare(
                "INSERT INTO intel_nodes (id, node_id, symbol_name, language, node_type, \
                 file_path, byte_range_start, byte_range_end, complexity, precision) \
                 VALUES (?,?,?,?,?,?,?,?,?,?)",
            )
            .expect("prepare node insert");
        for (id, name, ntype, fpath) in [
            (1i64, "alpha", "function", "/src/a.rs"),
            (2i64, "alpha", "function", "/src/a.rs"),
            (3i64, "beta", "variable", "/src/b.rs"),
        ] {
            stmt.execute(rusqlite::params![
                id,
                format!("{fpath}:{name}"),
                name,
                "rust",
                ntype,
                fpath,
                0i64,
                40i64,
                1i64,
                0i64
            ])
            .expect("insert node");
        }
    }
    conn.execute(
        "INSERT INTO intel_edges VALUES (?,?,?,NULL)",
        rusqlite::params![1i64, 3i64, "call"],
    )
    .expect("insert edge");
    conn.execute(
        "INSERT INTO project_metadata VALUES (?,?)",
        rusqlite::params!["p1", "v1"],
    )
    .expect("insert metadata");
    drop(conn);
}

/// Publish all five layers from `catalog_path` as generation `generation`.
fn publish_generation(
    storage_root: &std::path::Path,
    catalog_path: &std::path::Path,
    generation: u64,
) -> Manifest {
    let cas = Arc::new(Mutex::new(
        CasStore::open(storage_root.join("cas")).expect("open cas"),
    ));
    let mut writer = GenerationWriter::new(storage_root, cas.clone());

    let db_bytes = vacuum_bytes(catalog_path).expect("vacuum catalog");
    let conn = Connection::open(catalog_path).expect("open catalog");
    let pdg_bytes = encode_pdg_layer_v2(&conn).expect("encode pdg");
    let symbols_bytes = encode_symbols_layer(&conn).expect("encode symbols");

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
    writer.publish(generation).expect("publish");

    read_generation_manifest(storage_root, generation).expect("read manifest")
}

// ===========================================================================
// Tests
// ===========================================================================

#[test]
fn snapshot_opens_leased_generation_and_mmaps_layers() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = dir.path().join(".leindex");
    std::fs::create_dir_all(&storage).expect("create storage");
    let catalog_path = storage.join("leindex.db");
    write_catalog(&catalog_path);

    let manifest = publish_generation(&storage, &catalog_path, 1);
    assert_eq!(manifest.generation, 1);

    let snapshot = GenerationSnapshot::open(&storage).expect("open snapshot");
    assert_eq!(snapshot.generation(), 1);
    assert_eq!(snapshot.manifest().generation, 1);
    assert_eq!(snapshot.manifest().layers.keys().len(), 5);

    let neural = snapshot.neural().expect("neural reader");
    assert_eq!(neural.count(), 0);
    assert_eq!(snapshot.tfidf().expect("tfidf reader").num_docs(), 0);
    let pdg = snapshot.pdg().expect("pdg reader");
    assert!(pdg.num_nodes() >= 1, "expected at least one pdg node");
    assert!(pdg.num_edges() >= 1, "expected at least one pdg edge");
    let symbols = snapshot.symbols().expect("symbols reader");
    assert!(symbols.num_symbols() >= 1, "expected at least one symbol");
}

#[test]
fn snapshot_db_layer_is_bit_identical_and_queryable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = dir.path().join(".leindex");
    std::fs::create_dir_all(&storage).expect("create storage");
    let catalog_path = storage.join("leindex.db");
    write_catalog(&catalog_path);

    let staged_bytes = vacuum_bytes(&catalog_path).expect("vacuum");
    publish_generation(&storage, &catalog_path, 1);

    let snapshot = GenerationSnapshot::open(&storage).expect("open snapshot");

    // The materialized Db layer must be byte-for-byte what the generation
    // staged into CAS (VAL-EQUIV-001: no transformation of heap data).
    let materialized = std::fs::read(snapshot.db_path()).expect("read db");
    assert_eq!(materialized, staged_bytes, "Db layer must be bit-identical");

    // And it must be a queryable SQLite catalog with the same rows.
    let read_only = Connection::open(snapshot.db_path()).expect("open materialized db");
    let nodes: i64 = read_only
        .query_row("SELECT COUNT(*) FROM intel_nodes", [], |row| row.get(0))
        .expect("count nodes");
    assert_eq!(nodes, 3, "Db layer must expose all indexed nodes");
}

#[test]
fn snapshot_lease_pins_generation_layer_blobs() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = dir.path().join(".leindex");
    std::fs::create_dir_all(&storage).expect("create storage");
    let catalog_path = storage.join("leindex.db");
    write_catalog(&catalog_path);

    let manifest = publish_generation(&storage, &catalog_path, 1);

    // A fresh CasStore reads the persisted refcount sidecar, so it reflects
    // the live store state at open time.
    let cas = CasStore::open(storage.join("cas")).expect("open cas");

    // Before any snapshot, the layer blobs are unpinned.
    for hash in manifest.layer_hashes() {
        assert_eq!(cas.refcount(&hash), 0, "blob unpinned before lease");
    }

    // Acquiring the snapshot acquires a lease that pins every layer blob.
    let snapshot = GenerationSnapshot::open(&storage).expect("open snapshot");
    // Re-open so the in-memory sidecar reflects the persisted lease pins.
    let pinned_cas = CasStore::open(storage.join("cas")).expect("reopen cas");
    for hash in manifest.layer_hashes() {
        assert_eq!(pinned_cas.refcount(&hash), 1, "blob pinned by lease");
    }

    // Dropping the snapshot releases the pins.
    drop(snapshot);
    let released_cas = CasStore::open(storage.join("cas")).expect("reopen cas");
    for hash in manifest.layer_hashes() {
        assert_eq!(
            released_cas.refcount(&hash),
            0,
            "blob unpinned after lease drop"
        );
    }
}

#[test]
fn snapshot_readers_mmap_the_cas_blob_frame_in_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage = dir.path().join(".leindex");
    std::fs::create_dir_all(&storage).expect("create storage");
    let catalog_path = storage.join("leindex.db");
    write_catalog(&catalog_path);

    publish_generation(&storage, &catalog_path, 1);
    let snapshot = GenerationSnapshot::open(&storage).expect("open snapshot");

    // The Pdg reader opened the CAS blob file in place; the file on disk must
    // still be a valid framed blob (LIDX-BLB1 header + LIDX-PDG1 payload).
    let pdg_hash = snapshot
        .manifest()
        .layers
        .get(&LayerKind::Pdg)
        .copied()
        .expect("pdg hash");
    let blob_path = CasStore::open(storage.join("cas"))
        .expect("cas")
        .blob_path(&pdg_hash);
    let frame = std::fs::read(&blob_path).expect("read blob frame");
    assert_eq!(&frame[..4], b"LIDX");
    let (payload, _hash) =
        crate::storage::cas::blob::extract_payload(&frame).expect("extract payload");
    assert_eq!(
        &payload[..crate::storage::generation::reader::PDG_MAGIC.len()],
        crate::storage::generation::reader::PDG_MAGIC,
        "blob file carries the pdg payload"
    );
}
