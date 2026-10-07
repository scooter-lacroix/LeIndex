//! Tests for GenerationWriter: blob staging, dedup, atomic publish, crash safety.
//!
//! Covers VAL-WRITER-001 through VAL-WRITER-005.

use super::super::*;
use super::*;
use crate::storage::cas::CasStore;
use std::fs;
use std::sync::{Arc, Mutex};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

#[allow(dead_code)]
fn make_test_model_identity() -> ModelIdentity {
    ModelIdentity {
        name: "test-model".to_string(),
        digest: "sha256:abcdef".to_string(),
        dimensions: 384,
    }
}

fn open_writer() -> (tempfile::TempDir, Arc<Mutex<CasStore>>, GenerationWriter) {
    let dir = tempfile::tempdir().expect("tempdir");
    let storage_root = dir.path().to_path_buf();
    let cas_root = storage_root.join("cas");
    let store = Arc::new(Mutex::new(CasStore::open(&cas_root).expect("open cas")));
    let writer = GenerationWriter::new(storage_root.clone(), store.clone());
    (dir, store, writer)
}

fn stage_all_layers(writer: &mut GenerationWriter) {
    writer
        .stage(LayerKind::Db, b"db layer payload")
        .expect("stage db");
    writer
        .stage(LayerKind::Tfidf, b"tfidf layer payload")
        .expect("stage tfidf");
    writer
        .stage(LayerKind::Neural, b"neural layer payload")
        .expect("stage neural");
    writer
        .stage(LayerKind::Pdg, b"pdg layer payload")
        .expect("stage pdg");
    writer
        .stage(LayerKind::Symbols, b"symbols layer payload")
        .expect("stage symbols");
}

// ===========================================================================
// VAL-WRITER-001: Writer stages blobs without duplicating identical content
// ===========================================================================

#[test]
fn test_writer_stage_dedup() {
    let (_dir, store, mut writer) = open_writer();

    // Stage the same TF-IDF payload twice with identical bytes.
    writer
        .stage(LayerKind::Tfidf, b"identical tfidf data")
        .expect("stage tfidf first");
    let hash_first = *writer
        .staged_hash(LayerKind::Tfidf)
        .expect("staged hash exists");

    writer
        .stage(LayerKind::Tfidf, b"identical tfidf data")
        .expect("stage tfidf second");
    let hash_second = *writer
        .staged_hash(LayerKind::Tfidf)
        .expect("staged hash exists");

    // Same hash both times.
    assert_eq!(
        hash_first, hash_second,
        "identical bytes must produce same hash"
    );

    // The CAS holds exactly one blob for this hash (dedup).
    {
        let s = store.lock().unwrap();
        assert!(
            s.exists(&hash_first),
            "blob must exist in CAS after staging"
        );
        let blob_count = s.blob_count().expect("blob_count");
        assert_eq!(
            blob_count, 1,
            "dedup: only one blob on disk for identical content"
        );
    }
}

#[test]
fn test_writer_stage_different_layers_different_hashes() {
    let (_dir, _store, mut writer) = open_writer();

    writer.stage(LayerKind::Db, b"db data").expect("stage db");
    writer
        .stage(LayerKind::Neural, b"neural data")
        .expect("stage neural");

    let db_hash = *writer.staged_hash(LayerKind::Db).expect("db hash");
    let neural_hash = *writer.staged_hash(LayerKind::Neural).expect("neural hash");

    assert_ne!(
        db_hash, neural_hash,
        "different content must have different hashes"
    );
}

#[test]
fn test_writer_stage_dedup_across_writers() {
    let (_dir, store, mut writer_a) = {
        let dir = tempfile::tempdir().expect("tempdir");
        let storage_root = dir.path().to_path_buf();
        let cas_root = storage_root.join("cas");
        let store = Arc::new(Mutex::new(CasStore::open(&cas_root).expect("open cas")));
        let writer = GenerationWriter::new(storage_root.clone(), store.clone());
        (dir, store, writer)
    };

    let storage_root = writer_a.storage_root().to_path_buf();
    let cas_root = storage_root.join("cas");

    // Writer A stages a blob.
    writer_a
        .stage(LayerKind::Db, b"shared db content")
        .expect("stage db");

    // Writer B (pointing at same CAS) stages the same blob.
    let store_b = Arc::new(Mutex::new(CasStore::open(&cas_root).expect("reopen cas")));
    let mut writer_b = GenerationWriter::new(storage_root, store_b.clone());
    writer_b
        .stage(LayerKind::Symbols, b"shared db content")
        .expect("stage symbols with same content");

    // Both hashes should be identical.
    let hash_a = *writer_a
        .staged_hash(LayerKind::Db)
        .expect("hash from writer a");
    let hash_b = *writer_b
        .staged_hash(LayerKind::Symbols)
        .expect("hash from writer b");

    assert_eq!(
        hash_a, hash_b,
        "same content across writers must hash identically"
    );

    // Only one blob on disk.
    {
        let s = store.lock().unwrap();
        assert_eq!(
            s.blob_count().unwrap(),
            1,
            "cross-writer dedup: one blob on disk"
        );
    }
}

// ===========================================================================
// VAL-WRITER-002: Writer atomic publish via manifest rename + CURRENT update
// ===========================================================================

#[test]
fn test_writer_atomic_publish() {
    let (dir, _store, mut writer) = open_writer();

    stage_all_layers(&mut writer);

    writer.publish(5).expect("publish");

    let storage_root = dir.path().to_path_buf();

    // CURRENT points to generation 5.
    let current = fs::read_to_string(storage_root.join("CURRENT")).expect("read CURRENT");
    assert_eq!(current.trim(), "5");

    // manifest exists at generations/5/manifest.
    let manifest_path = storage_root.join("generations").join("5").join("manifest");
    assert!(
        manifest_path.exists(),
        "manifest file must exist after publish"
    );

    // .partial file should NOT exist (it was renamed).
    let partial_path = storage_root
        .join("generations")
        .join("5")
        .join("manifest.partial");
    assert!(
        !partial_path.exists(),
        "manifest.partial must not exist after successful publish"
    );

    // Manifest is valid and parseable.
    let manifest_bytes = fs::read(&manifest_path).expect("read manifest bytes");
    let manifest = Manifest::from_bytes(&manifest_bytes).expect("parse manifest");
    assert_eq!(manifest.generation, 5);
    assert_eq!(manifest.layers.len(), 5);
}

#[test]
fn test_writer_publish_creates_generation_directory() {
    let (dir, _store, mut writer) = open_writer();

    stage_all_layers(&mut writer);
    writer.publish(42).expect("publish");

    let generation_dir = dir.path().join("generations").join("42");
    assert!(
        generation_dir.exists(),
        "generation directory must be created"
    );
    assert!(
        generation_dir.join("manifest").exists(),
        "manifest must exist"
    );
}

#[test]
fn test_writer_publish_increments_generation() {
    let (dir, _store, mut writer) = open_writer();

    // Publish gen 1.
    stage_all_layers(&mut writer);
    writer.publish(1).expect("publish 1");
    assert_eq!(
        read_current_generation(dir.path()),
        Some(1),
        "CURRENT points to gen 1"
    );

    // Publish gen 2 with new content.
    writer.finish_publish(); // clear staging
    stage_all_layers(&mut writer);
    writer.publish(2).expect("publish 2");
    assert_eq!(
        read_current_generation(dir.path()),
        Some(2),
        "CURRENT updated to gen 2"
    );

    // Both generations should have valid manifests on disk.
    let m1 = read_generation_manifest(dir.path(), 1).expect("read gen 1 manifest");
    let m2 = read_generation_manifest(dir.path(), 2).expect("read gen 2 manifest");
    assert_eq!(m1.generation, 1);
    assert_eq!(m2.generation, 2);
}

// ===========================================================================
// VAL-WRITER-003: Crash before manifest rename leaves last-good generation intact
// ===========================================================================

#[test]
fn test_crash_before_rename() {
    let (dir, store, mut writer) = open_writer();

    // Publish gen 1 (the "last-good" generation).
    stage_all_layers(&mut writer);
    writer.publish(1).expect("publish gen 1");
    writer.finish_publish();

    // Now simulate starting gen 2 publication but crash BEFORE rename.
    stage_all_layers(&mut writer);

    // Write manifest.partial but do NOT rename or update CURRENT.
    writer
        .write_manifest_partial_for_test(2)
        .expect("write partial");

    // Verify the partial file exists.
    let partial_path = dir
        .path()
        .join("generations")
        .join("2")
        .join("manifest.partial");
    assert!(
        partial_path.exists(),
        "partial file should exist for test setup"
    );

    // manifest (final) should NOT exist.
    let final_path = dir.path().join("generations").join("2").join("manifest");
    assert!(
        !final_path.exists(),
        "manifest must not exist before rename"
    );

    // CURRENT still points to gen 1.
    assert_eq!(
        read_current_generation(dir.path()),
        Some(1),
        "CURRENT must still point to gen 1"
    );

    // Simulate recovery: open a new writer and sweep staging.
    {
        let cas_dir = dir.path().join("cas");
        let store2 = Arc::new(Mutex::new(CasStore::open(&cas_dir).expect("reopen cas")));
        let mut writer2 = GenerationWriter::new(dir.path(), store2.clone());
        writer2.sweep_partial_manifests().expect("sweep");
    }

    // The partial file is gone.
    assert!(!partial_path.exists(), "partial must be swept on recovery");

    // CURRENT generation (1) is still readable.
    let current_gen = read_current_generation(dir.path()).expect("current gen");
    assert_eq!(current_gen, 1);
    let manifest = read_generation_manifest(dir.path(), current_gen).expect("read manifest");
    assert_eq!(manifest.generation, 1);

    // Leasing the current generation works.
    let lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire lease");
    assert_eq!(lease.generation(), 1);
    drop(lease);
}

// ===========================================================================
// VAL-WRITER-004: Crash after manifest rename but before CURRENT update
// ===========================================================================

#[test]
fn test_crash_after_rename_before_current() {
    let (dir, store, mut writer) = open_writer();

    // Publish gen 1 first (last-good).
    stage_all_layers(&mut writer);
    writer.publish(1).expect("publish gen 1");
    writer.finish_publish();

    // Stage and write manifest for gen 2, rename it, but do NOT update CURRENT.
    stage_all_layers(&mut writer);
    writer
        .write_and_rename_manifest_for_test(2)
        .expect("rename without current");

    // gen 2 manifest exists on disk.
    let manifest_path = dir.path().join("generations").join("2").join("manifest");
    assert!(
        manifest_path.exists(),
        "gen 2 manifest should exist after rename"
    );

    // CURRENT still points to gen 1.
    assert_eq!(
        read_current_generation(dir.path()),
        Some(1),
        "CURRENT must still be gen 1 after crash before CURRENT update"
    );

    // Reads from gen 1 succeed.
    let current_gen = read_current_generation(dir.path()).expect("current");
    assert_eq!(current_gen, 1);
    let manifest = read_generation_manifest(dir.path(), current_gen).expect("read gen 1 manifest");
    let lease = GenerationLease::acquire(store.clone(), &manifest).expect("lease gen 1");
    assert_eq!(lease.generation(), 1);
    drop(lease);

    // Recovery sweep handles the orphaned gen 2 manifest gracefully (no error).
    {
        let cas_dir = dir.path().join("cas");
        let store2 = Arc::new(Mutex::new(CasStore::open(&cas_dir).expect("reopen cas")));
        let mut writer2 = GenerationWriter::new(dir.path(), store2);
        writer2.sweep_partial_manifests().expect("sweep");
    }
}

// ===========================================================================
// VAL-WRITER-005: Crash at every publication phase — property test
// ===========================================================================

#[test]
fn test_crash_every_publish_phase() {
    // We simulate a crash at each of the 7 publication phases:
    //   0) staging blob write (simulated by CAS put completing normally)
    //   1) staging fsync (CAS handles internally)
    //   2) blob rename (CAS handles internally)
    //   3) manifest.partial write
    //   4) manifest fsync
    //   5) manifest rename
    //   6) CURRENT write
    //
    // For each kill point, after "restart" the CURRENT-referenced generation
    // must be fully readable and consistent.

    for kill_point in 0..7u8 {
        let (dir, store, mut writer) = open_writer();

        // Publish gen 1 as the last-good generation.
        stage_all_layers(&mut writer);
        writer.publish(1).expect("publish gen 1");
        writer.finish_publish();

        // Stage gen 2 content.
        stage_all_layers(&mut writer);

        // Simulate publish with crash at kill_point.
        let _ = writer.publish_with_simulated_crash(2, kill_point);

        // Verify CURRENT is still pointing to a valid generation.
        let current_gen = read_current_generation(dir.path());
        let gen_n = current_gen.expect("CURRENT must always point to a generation");

        // For kill points 0-5, CURRENT still points to gen 1 (last-good).
        // For kill point 6 (after CURRENT write), the publish completed
        // successfully so CURRENT legitimately points to gen 2. In both
        // cases the pointed-to generation must be fully readable.
        let expected_gen = if kill_point == 6 { 2 } else { 1 };
        assert_eq!(
            gen_n, expected_gen,
            "CURRENT must point to expected generation {} for kill_point {}",
            expected_gen, kill_point
        );

        // The current generation must be fully readable.
        let manifest =
            read_generation_manifest(dir.path(), gen_n).expect("read CURRENT generation manifest");
        assert_eq!(manifest.generation, gen_n);

        // Acquire lease and verify all layers are readable.
        let lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire lease");

        // Verify each layer's blob is retrievable from CAS.
        {
            let s = store.lock().unwrap();
            for hash in lease.layer_hashes() {
                assert!(
                    s.exists(hash),
                    "blob {:?} must exist in CAS for kill_point {}",
                    &hash[..4],
                    kill_point
                );
                // get should succeed (data is intact).
                let _data = s.get(hash).expect("get blob data");
            }
        }

        drop(lease);

        // Recovery sweep should not error.
        {
            let cas_dir = dir.path().join("cas");
            let store2 = Arc::new(Mutex::new(CasStore::open(&cas_dir).expect("reopen cas")));
            let mut writer2 = GenerationWriter::new(dir.path(), store2);
            writer2
                .sweep_partial_manifests()
                .expect("sweep should succeed");

            // After sweep, partial files are gone.
            let partial_path = dir
                .path()
                .join("generations")
                .join("2")
                .join("manifest.partial");
            assert!(
                !partial_path.exists(),
                "partial manifest must be swept for kill_point {}",
                kill_point
            );
        }

        // Current generation still readable after recovery.
        let gen_post = read_current_generation(dir.path()).expect("post-recovery current");
        assert_eq!(
            gen_post, expected_gen,
            "gen_n must still be {} after recovery for kill_point {}",
            expected_gen, kill_point
        );
        let _ = read_generation_manifest(dir.path(), gen_post).expect("post-recovery manifest");
    }
}

// ===========================================================================
// Extra: publish requires all layers staged
// ===========================================================================

#[test]
fn test_publish_requires_all_layers() {
    let (_dir, _store, mut writer) = open_writer();

    // Stage only 3 of 5 layers.
    writer.stage(LayerKind::Db, b"db").expect("stage");
    writer.stage(LayerKind::Tfidf, b"tfidf").expect("stage");
    writer.stage(LayerKind::Neural, b"neural").expect("stage");

    let result = writer.publish(1);
    assert!(
        result.is_err(),
        "publish must fail when not all layers are staged"
    );
}

// ===========================================================================
// Extra: fingerprints are computed from layer data
// ===========================================================================

#[test]
fn test_publish_computes_fingerprints() {
    let (_dir, _store, mut writer) = open_writer();

    stage_all_layers(&mut writer);
    writer.publish(10).expect("publish");

    // Re-read the manifest from the writer to verify fingerprints were set.
    let manifest = writer.last_published_manifest().expect("manifest");
    // Fingerprints should be non-zero (blake3 of real data).
    assert_ne!(
        manifest.graph_fingerprint, [0u8; 32],
        "graph fingerprint must be computed"
    );
    assert_ne!(
        manifest.search_fingerprint, [0u8; 32],
        "search fingerprint must be computed"
    );
}

// ===========================================================================
// Extra: publish writes CURRENT atomically (write-then-rename)
// ===========================================================================

#[test]
fn test_current_file_write_atomic() {
    let (dir, _store, mut writer) = open_writer();

    stage_all_layers(&mut writer);
    writer.publish(7).expect("publish");

    // CURRENT exists and contains just the number.
    let current_path = dir.path().join("CURRENT");
    assert!(current_path.exists());

    // No CURRENT.tmp or CURRENT.partial lingering.
    assert!(!dir.path().join("CURRENT.tmp").exists());
    assert!(!dir.path().join("CURRENT.partial").exists());

    let content = fs::read_to_string(&current_path).expect("read CURRENT");
    assert_eq!(content.trim(), "7");
}
