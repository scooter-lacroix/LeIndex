//! Tests for GenerationLease: acquire, drop, refcount management, GC protection.
//!
//! Covers VAL-LEASE-001 through VAL-LEASE-004. VAL-LEASE-005 (no-writer-Mutex)
//! is verified structurally: the lease holds `Arc<Mutex<CasStore>>` which is
//! independent of the LeIndex writer Mutex.

use super::super::*;
use super::*;
use crate::storage::cas::CasStore;
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

fn fixture_manifest_with_hashes(hashes: [[u8; 32]; 5]) -> Manifest {
    let mut layers = HashMap::new();
    layers.insert(LayerKind::Db, hashes[0]);
    layers.insert(LayerKind::Tfidf, hashes[1]);
    layers.insert(LayerKind::Neural, hashes[2]);
    layers.insert(LayerKind::Pdg, hashes[3]);
    layers.insert(LayerKind::Symbols, hashes[4]);

    Manifest {
        version: MANIFEST_VERSION,
        generation: 1,
        model_identity: ModelIdentity {
            name: "test".to_string(),
            digest: "sha256:test".to_string(),
            dimensions: 384,
        },
        graph_fingerprint: [0x11; 32],
        search_fingerprint: [0x22; 32],
        layers,
    }
}

fn setup_store_with_blobs() -> (tempfile::TempDir, Arc<Mutex<CasStore>>, Vec<[u8; 32]>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(Mutex::new(CasStore::open(dir.path()).expect("open cas")));

    // Put 5 distinct blobs (one per layer kind).
    let payloads: &[&[u8]] = &[
        b"db layer blob",
        b"tfidf layer blob",
        b"neural layer blob",
        b"pdg layer blob",
        b"symbols layer blob",
    ];

    let mut hashes = Vec::new();
    {
        let s = store.lock().unwrap();
        for payload in payloads {
            let h = s.put(payload).expect("put");
            hashes.push(h);
        }
    }

    let hash_arr: [[u8; 32]; 5] = [hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]];

    // Return store, the 5 hashes, and a manifest pointing to them.
    // Also return the dir so it stays alive.
    let _manifest = fixture_manifest_with_hashes(hash_arr);
    (dir, store, hashes)
}

// -------- VAL-LEASE-001: lease acquisition increments refcounts --------

#[test]
fn test_lease_acquires_and_increments_refcounts() {
    let (_dir, store, hashes) = setup_store_with_blobs();
    let manifest =
        fixture_manifest_with_hashes([hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]]);

    // Before lease: all refcounts are 0.
    {
        let s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 0);
        }
    }

    let lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire");

    // After acquire: all refcounts are 1.
    {
        let s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 1, "refcount must be incremented on acquire");
        }
    }

    // Lease holds the generation number.
    assert_eq!(lease.generation(), manifest.generation);
}

// -------- VAL-LEASE-002: lease drop decrements refcounts --------

#[test]
fn test_lease_drop_decrements_refcounts() {
    let (_dir, store, hashes) = setup_store_with_blobs();
    let manifest =
        fixture_manifest_with_hashes([hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]]);

    // Before lease: refcount 0.
    {
        let s = store.lock().unwrap();
        assert_eq!(s.refcount(&hashes[0]), 0);
    }

    {
        let _lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire");
        // After acquire: refcount 1.
        let s = store.lock().unwrap();
        assert_eq!(s.refcount(&hashes[0]), 1);
    }
    // After drop: refcount 0 again.
    {
        let s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 0, "refcount must be decremented on drop");
        }
    }
}

// -------- VAL-LEASE-003: lease prevents GC of held blobs --------

#[test]
fn test_lease_prevents_gc() {
    let (_dir, store, hashes) = setup_store_with_blobs();
    let manifest =
        fixture_manifest_with_hashes([hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]]);

    // Acquire lease — this increments refcounts to 1 for all blobs.
    let lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire");

    // GC should NOT collect any blobs (refcounts > 0).
    {
        let mut s = store.lock().unwrap();
        let report = s.gc().expect("gc");
        assert_eq!(
            report.blobs_removed, 0,
            "no blobs collected while lease held"
        );
    }

    // All blobs still exist.
    {
        let s = store.lock().unwrap();
        for h in &hashes {
            assert!(s.exists(h), "blob must survive GC while lease held");
        }
    }

    drop(lease);

    // After drop, refcounts are 0. GC should now collect all blobs.
    {
        let mut s = store.lock().unwrap();
        let report = s.gc().expect("gc after drop");
        assert_eq!(
            report.blobs_removed, 5,
            "all blobs collectible after lease dropped"
        );
    }
}

// -------- VAL-LEASE-004: multiple concurrent leases --------

#[test]
fn test_multiple_leases_same_generation() {
    let (_dir, store, hashes) = setup_store_with_blobs();
    let manifest =
        fixture_manifest_with_hashes([hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]]);

    // Acquire 3 leases on the same generation.
    let lease1 = GenerationLease::acquire(store.clone(), &manifest).expect("acquire 1");
    let lease2 = GenerationLease::acquire(store.clone(), &manifest).expect("acquire 2");
    let lease3 = GenerationLease::acquire(store.clone(), &manifest).expect("acquire 3");

    // After 3 acquires: refcount 3 per blob.
    {
        let s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 3);
        }
    }

    // Drop 2 — refcount goes to 1, GC should not collect.
    drop(lease1);
    drop(lease2);
    {
        let mut s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 1);
        }
        let report = s.gc().expect("gc");
        assert_eq!(report.blobs_removed, 0);
    }

    // Drop the 3rd — refcount 0, GC eligible.
    drop(lease3);
    {
        let mut s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 0);
        }
        let report = s.gc().expect("gc final");
        assert_eq!(report.blobs_removed, 5);
    }
}

// -------- VAL-LEASE-005: lease does not touch writer Mutex (structural) --------
//
// The lease holds `Arc<Mutex<CasStore>>`, which is the CAS store's internal
// refcount mutex. It does NOT acquire `LeIndex`'s writer Mutex or any
// `ProjectWriteLock` (flock). This test verifies a lease can be acquired and
// used without holding any LeIndex lock by operating purely at the CAS level.

#[test]
fn test_lease_no_leindex_lock_required() {
    let (_dir, store, hashes) = setup_store_with_blobs();
    let manifest =
        fixture_manifest_with_hashes([hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]]);

    // Acquire lease without any LeIndex or ProjectWriteLock present.
    let lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire");

    // Access the manifest and layer hashes from the lease.
    assert_eq!(lease.generation(), 1);
    assert_eq!(lease.layer_hashes().len(), 5);

    // Blob data is readable through the CAS store independently.
    {
        let s = store.lock().unwrap();
        let data = s.get(&hashes[0]).expect("get db blob");
        assert_eq!(data, b"db layer blob");
    }

    drop(lease);
}

// -------- Extra: lease provides manifest access --------

#[test]
fn test_lease_provides_manifest_access() {
    let (_dir, store, hashes) = setup_store_with_blobs();
    let manifest =
        fixture_manifest_with_hashes([hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]]);

    let lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire");

    // The lease should give access to the manifest metadata.
    let m = lease.manifest();
    assert_eq!(m.generation, 1);
    assert_eq!(m.version, MANIFEST_VERSION);
    assert_eq!(m.layers.len(), 5);

    drop(lease);
}

// -------- Extra: lease persist after acquire --------

#[test]
fn test_lease_acquire_persists_refcounts() {
    let dir = tempfile::tempdir().expect("tempdir");
    let store = Arc::new(Mutex::new(CasStore::open(dir.path()).expect("open")));

    let mut hashes = Vec::new();
    {
        let s = store.lock().unwrap();
        for i in 0..5u8 {
            hashes.push(s.put(&[i; 32]).expect("put"));
        }
    }

    let manifest =
        fixture_manifest_with_hashes([hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]]);

    {
        let _lease = GenerationLease::acquire(store.clone(), &manifest).expect("acquire");
        // Refcounts are persisted on acquire.
    }
    // After lease dropped, reopen the store and verify refcounts went through
    // persist cycle correctly.
    {
        let s = CasStore::open(dir.path()).expect("reopen");
        for h in &hashes {
            // After lease drop, refcount is back to 0.
            assert_eq!(s.refcount(h), 0);
        }
    }
}

// -------- Integration: read CURRENT + manifest from disk --------

/// Write a storage-root directory with CURRENT + manifest so the lease-from-disk
/// path can be exercised end-to-end.
fn write_generation_to_disk(storage_root: &Path, manifest: &Manifest) -> Vec<[u8; 32]> {
    let gen_dir = storage_root
        .join("generations")
        .join(manifest.generation.to_string());
    std::fs::create_dir_all(&gen_dir).expect("create gen dir");

    // Write manifest.
    let manifest_bytes = manifest.to_bytes().expect("serialize");
    std::fs::write(gen_dir.join("manifest"), &manifest_bytes).expect("write manifest");

    // Write CURRENT.
    std::fs::write(
        storage_root.join("CURRENT"),
        format!("{}\n", manifest.generation),
    )
    .expect("write CURRENT");

    manifest.layer_hashes()
}

fn fixture_manifest(generation: u64, hashes: [[u8; 32]; 5]) -> Manifest {
    let mut layers = HashMap::new();
    layers.insert(LayerKind::Db, hashes[0]);
    layers.insert(LayerKind::Tfidf, hashes[1]);
    layers.insert(LayerKind::Neural, hashes[2]);
    layers.insert(LayerKind::Pdg, hashes[3]);
    layers.insert(LayerKind::Symbols, hashes[4]);

    Manifest {
        version: MANIFEST_VERSION,
        generation,
        model_identity: ModelIdentity {
            name: "test".to_string(),
            digest: "sha256:test".to_string(),
            dimensions: 384,
        },
        graph_fingerprint: [0x11; 32],
        search_fingerprint: [0x22; 32],
        layers,
    }
}

#[test]
fn test_read_current_generation() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("CURRENT"), b"42\n").expect("write");
    assert_eq!(read_current_generation(dir.path()), Some(42));
}

#[test]
fn test_read_current_generation_missing() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert_eq!(read_current_generation(dir.path()), None);
}

#[test]
fn test_read_current_generation_non_numeric() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("CURRENT"), b"gen-abc\n").expect("write");
    assert_eq!(read_current_generation(dir.path()), None);
}

#[test]
fn test_read_generation_manifest_from_disk() {
    let dir = tempfile::tempdir().expect("tempdir");
    let manifest = fixture_manifest(
        7,
        [[0xAA; 32], [0xBB; 32], [0xCC; 32], [0xDD; 32], [0xEE; 32]],
    );
    write_generation_to_disk(dir.path(), &manifest);

    let recovered = read_generation_manifest(dir.path(), 7).expect("read manifest");
    assert_eq!(recovered, manifest);
}

#[test]
fn test_read_generation_manifest_missing_file() {
    let dir = tempfile::tempdir().expect("tempdir");
    let err = read_generation_manifest(dir.path(), 999).unwrap_err();
    assert!(matches!(err, ManifestError::MissingManifest { .. }));
}

#[test]
fn test_lease_acquires_from_disk() {
    // Full integration path: storage root with cas/ + CURRENT + manifest.
    let dir = tempfile::tempdir().expect("tempdir");
    let storage_root = dir.path();

    // Create CAS with 5 layer blobs.
    let cas_dir = storage_root.join("cas");
    let store = CasStore::open(&cas_dir).expect("open cas");
    let payloads: &[&[u8]] = &[b"db", b"tfidf", b"neural", b"pdg", b"symbols"];
    let mut hashes = Vec::new();
    for p in payloads {
        hashes.push(store.put(p).expect("put"));
    }
    drop(store);

    let hash_arr: [[u8; 32]; 5] = [hashes[0], hashes[1], hashes[2], hashes[3], hashes[4]];
    let manifest = fixture_manifest(3, hash_arr);
    write_generation_to_disk(storage_root, &manifest);

    // Read CURRENT + manifest, acquire lease.
    let gen_n = read_current_generation(storage_root).expect("read current");
    assert_eq!(gen_n, 3);

    let m = read_generation_manifest(storage_root, gen_n).expect("read manifest");
    let store = Arc::new(Mutex::new(CasStore::open(&cas_dir).expect("reopen cas")));

    let lease = GenerationLease::acquire(store.clone(), &m).expect("acquire");
    assert_eq!(lease.generation(), 3);

    // Refcounts are incremented.
    {
        let s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 1);
        }
    }

    drop(lease);

    // Refcounts decremented on drop.
    {
        let s = store.lock().unwrap();
        for h in &hashes {
            assert_eq!(s.refcount(h), 0);
        }
    }
}
