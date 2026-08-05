//! Tests for CasStore: put/get roundtrip, dedup, corruption, path layout,
//! staging atomicity, refcount lifecycle, and GC.

use super::*;
use crate::storage::cas::blob::{BLOB_HEADER_LEN, BLOB_MAGIC, blob_hash};
use std::collections::HashSet;
use std::fs;
use tempfile::tempdir;

// -------- VAL-CAS-001: put/get roundtrip --------

#[test]
fn test_put_get_roundtrip() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");

    let payloads: &[&[u8]] = &[
        b"",
        b"short ASCII",
        &(0..8192u32).map(|i| (i & 0xFF) as u8).collect::<Vec<u8>>(),
    ];

    for payload in payloads {
        let h = store.put(payload).expect("put");
        let recovered = store.get(&h).expect("get");
        assert_eq!(recovered.as_slice(), *payload);
    }
}

// -------- VAL-CAS-002: put idempotency / deduplication --------

#[test]
fn test_put_idempotency() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");

    let h1 = store.put(b"same content").expect("put 1");
    let h2 = store.put(b"same content").expect("put 2");
    assert_eq!(h1, h2, "same content must hash identically");
    assert_eq!(store.blob_count().unwrap(), 1, "dedup must store once");

    // Staging file cleaned up.
    let staging = dir
        .path()
        .join(STAGING_DIR)
        .join(format!("{}.partial", hash_to_hex(&h1)));
    assert!(
        !staging.exists(),
        "staging partial must be gone after rename"
    );
}

// -------- VAL-CAS-003: hash determinism across stores --------

#[test]
fn test_hash_determinism_cross_store() {
    let dir_a = tempdir().expect("tempdir a");
    let dir_b = tempdir().expect("tempdir b");
    let a = CasStore::open(dir_a.path()).expect("open a");
    let b = CasStore::open(dir_b.path()).expect("open b");

    for payload in [b"deterministic" as &[u8], b"payload", b""] {
        let ha = a.put(payload).expect("put a");
        let hb = b.put(payload).expect("put b");
        assert_eq!(ha, hb);
    }
}

// -------- VAL-CAS-004: corrupt blob rejection --------

#[test]
fn test_corrupt_blob_rejected_get_returns_error() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");

    let payload = b"the quick brown fox";
    let h = store.put(payload).expect("put");
    assert_eq!(store.get(&h).unwrap(), payload);

    // Helper: restore the blob to a clean state before each sub-case.
    // `put` is idempotent (skips if file exists), so we must delete the
    // corrupted file first for the re-put to actually write fresh bytes.
    let reset_blob = || {
        let path = store.blob_path(&h);
        let _ = fs::remove_file(&path);
        store.put(payload).expect("reset blob");
    };

    // Sub-case 1: truncated payload.
    {
        reset_blob();
        let path = store.blob_path(&h);
        let mut bytes = fs::read(&path).expect("read blob");
        bytes.truncate(bytes.len() - 4);
        fs::write(&path, &bytes).expect("write truncated");
        let err = store.get(&h).unwrap_err();
        assert!(
            matches!(
                err,
                CasError::BadBlob(super::blob::BadBlob::PayloadLengthMismatch { .. })
            ),
            "expected payload-length mismatch, got {err:?}"
        );
    }

    // Sub-case 2: flipped magic byte.
    {
        reset_blob();
        let path = store.blob_path(&h);
        let mut bytes = fs::read(&path).expect("read blob");
        bytes[0] ^= 0xFF;
        fs::write(&path, &bytes).expect("write flipped magic");
        let err = store.get(&h).unwrap_err();
        assert!(matches!(
            err,
            CasError::BadBlob(super::blob::BadBlob::BadMagic(_))
        ));
    }

    // Sub-case 3: flipped version byte.
    {
        reset_blob();
        let path = store.blob_path(&h);
        let mut bytes = fs::read(&path).expect("read blob");
        bytes[9] = 0xFE;
        fs::write(&path, &bytes).expect("write flipped version");
        let err = store.get(&h).unwrap_err();
        assert!(matches!(
            err,
            CasError::BadBlob(super::blob::BadBlob::VersionMismatch { .. })
        ));
    }

    // Sub-case 4: flipped payload byte causing hash mismatch.
    {
        reset_blob();
        let path = store.blob_path(&h);
        let mut bytes = fs::read(&path).expect("read blob");
        bytes[BLOB_HEADER_LEN] ^= 0x01;
        fs::write(&path, &bytes).expect("write flipped payload");
        let err = store.get(&h).unwrap_err();
        assert!(matches!(
            err,
            CasError::BadBlob(super::blob::BadBlob::HashMismatch)
        ));
    }
}

// -------- VAL-CAS-005: blob path layout --------

#[test]
fn test_blob_path_layout() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");
    let payload = b"path layout test";
    let h = store.put(payload).expect("put");
    let hex = hash_to_hex(&h);
    let prefix = &hex[0..2];

    // Expected file: <root>/<hh>/<hash>
    let expected = dir.path().join(prefix).join(&hex);
    assert!(expected.exists(), "expected blob at {expected:?}");

    // The blob must not also exist in a different prefix directory.
    let mut blob_files = Vec::new();
    for entry in fs::read_dir(dir.path()).expect("read root") {
        let entry = entry.expect("entry");
        if entry.file_name() == STAGING_DIR || entry.file_name() == "refs.json" {
            continue;
        }
        if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
            for sub in fs::read_dir(entry.path()).expect("read prefix dir") {
                let sub = sub.expect("sub entry");
                blob_files.push(sub.path());
            }
        }
    }
    assert_eq!(blob_files.len(), 1);
    assert_eq!(blob_files[0], expected);
}

// -------- VAL-CAS-006: staging partial not visible --------

#[test]
fn test_staging_partial_not_visible() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");

    let payload = b"atomicity test payload";
    let h = store.put(payload).expect("put");

    // After put returns, blob is at final path and staging is gone.
    assert!(store.blob_path(&h).exists());
    assert!(!store.staging_path(&h).exists());

    // Simulate a crash: move the blob back into staging manually, leave no
    // final-path copy. `get` must then error, not return partial data.
    let final_path = store.blob_path(&h);
    let staging_path = store.staging_path(&h);
    fs::rename(&final_path, &staging_path).expect("move back to staging");
    assert!(matches!(store.get(&h).unwrap_err(), CasError::NotFound));
}

#[test]
fn test_staging_blob_frame_on_disk_has_magic() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");
    let payload = b"inspect";
    store.put(payload).expect("put");
    let h = blob_hash(payload);
    let path = store.blob_path(&h);
    let content = fs::read(&path).expect("read");
    assert_eq!(&content[0..9], BLOB_MAGIC);
}

// -------- VAL-CAS-007: refcount increment --------

#[test]
fn test_refcount_incr() {
    let dir = tempdir().expect("tempdir");
    let mut store = CasStore::open(dir.path()).expect("open");
    let h = store.put(b"refcount incr payload").expect("put");

    assert_eq!(store.refcount(&h), 0);
    store.incr(&h);
    store.incr(&h);
    store.incr(&h);
    assert_eq!(store.refcount(&h), 3);
}

// -------- VAL-CAS-008: refcount decrement + persistence --------

#[test]
fn test_refcount_decr_and_persist_across_reopen() {
    let dir = tempdir().expect("tempdir");
    let h = {
        let mut store = CasStore::open(dir.path()).expect("open");
        let h = store.put(b"decr persist payload").expect("put");
        store.incr(&h);
        store.incr(&h);
        store.incr(&h);
        // Decr from 3 to 0.
        assert_eq!(store.decr(&h).unwrap(), 2);
        assert_eq!(store.decr(&h).unwrap(), 1);
        assert_eq!(store.decr(&h).unwrap(), 0);
        // Decr at 0 must error.
        assert!(matches!(
            store.decr(&h).unwrap_err(),
            CasError::RefcountUnderflow
        ));
        store.persist().expect("persist");
        h
    };

    {
        let store = CasStore::open(dir.path()).expect("reopen");
        assert_eq!(store.refcount(&h), 0);
        // Decr at 0 still errors after reopen.
        let mut s = store;
        assert!(matches!(
            s.decr(&h).unwrap_err(),
            CasError::RefcountUnderflow
        ));
    }
}

// -------- VAL-CAS-009 + VAL-CAS-010: GC at refcount zero --------

#[test]
fn test_gc_refcount_zero_returns_reclaimed_bytes() {
    let dir = tempdir().expect("tempdir");
    let mut store = CasStore::open(dir.path()).expect("open");

    let payload_a = b"blob A";
    let payload_c = b"blob C (refcount=1, must survive)";
    let h_a = store.put(payload_a).expect("put A");
    let h_c = store.put(payload_c).expect("put C");

    // A has refcount 0 by default, C has refcount 1.
    store.incr(&h_c);

    // Persist refcounts before measuring so that refs.json is included in
    // both pre and post measurements (gc() persists as a side effect).
    store.persist().expect("persist before gc");

    // Expected on-disk size for A includes header overhead (VAL-CAS-010).
    let pre_cas_size = dir_size(dir.path());

    let report = store.gc().expect("gc");
    assert_eq!(report.blobs_removed, 1, "only blob A should be collected");
    assert!(
        report.reclaimed_bytes > 0,
        "reclaimed bytes must include header + payload for A"
    );

    // Blob A is gone, C is retained.
    assert!(!store.exists(&h_a));
    assert!(store.exists(&h_c));

    // Reclaim accounting is accurate (du -sb delta).
    let post_cas_size = dir_size(dir.path());
    let size_delta = pre_cas_size.saturating_sub(post_cas_size);
    assert_eq!(
        report.reclaimed_bytes, size_delta,
        "reclaimed bytes must equal the filesystem delta"
    );

    // A is still not recoverable via get.
    assert!(matches!(store.get(&h_a).unwrap_err(), CasError::NotFound));
}

#[test]
fn test_gc_keeps_refcount_positive() {
    let dir = tempdir().expect("tempdir");
    let mut store = CasStore::open(dir.path()).expect("open");

    let h_a = store.put(b"keep me via refcount").expect("put");
    store.incr(&h_a);
    let h_b = store.put(b"collect me").expect("put");
    // h_a refcount 1, h_b refcount 0.

    let report = store.gc().expect("gc");
    assert_eq!(report.blobs_removed, 1);
    assert!(store.exists(&h_a));
    assert!(!store.exists(&h_b));
}

#[test]
fn test_gc_empty_store_noop() {
    let dir = tempdir().expect("tempdir");
    let mut store = CasStore::open(dir.path()).expect("open");
    let report = store.gc().expect("gc");
    assert_eq!(report.blobs_removed, 0);
    assert_eq!(report.reclaimed_bytes, 0);
}

#[test]
fn test_gc_with_pins_retains_unreferenced_but_pinned_blobs() {
    // Even if refcount == 0, the pin set (e.g. generation hashes in future
    // tasks) must keep a blob alive.
    let dir = tempdir().expect("tempdir");
    let mut store = CasStore::open(dir.path()).expect("open");

    let h = store.put(b"pinned blob").expect("put");
    let mut pins = HashSet::new();
    pins.insert(h);

    let report = store.gc_with_pins(&pins).expect("gc");
    assert_eq!(report.blobs_removed, 0);
    assert!(store.exists(&h));
}

// -------- VAL-CAS-018: cross-generation dedup --------

#[test]
fn test_cas_cross_generation_dedup() {
    // Two stores receiving identical payloads must share the same hash, and the
    // second `put` of identical bytes is a no-op at the storage layer.
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");

    let shared_payload = b"shared content across conceptual generations";
    let h1 = store.put(shared_payload).expect("put 1");
    let h2 = store.put(shared_payload).expect("put 2");
    assert_eq!(h1, h2, "identical content -> identical hash");
    assert_eq!(store.blob_count().unwrap(), 1, "one blob on disk, not two");
}

// -------- VAL-CAS-020: open on empty directory --------

#[test]
fn test_cas_open_empty() {
    let dir = tempdir().expect("tempdir");
    let root = dir.path().join("nested").join("cas");
    let store = CasStore::open(&root).expect("open on nonexistent path");
    // Subdirectories are created.
    assert!(root.join(STAGING_DIR).exists());

    // First put works, get on unknown hash returns NotFound (not crash).
    let h = store.put(b"first").expect("put");
    assert_eq!(store.get(&h).unwrap(), b"first");

    let unknown = blob_hash(b"does not exist");
    assert!(matches!(
        store.get(&unknown).unwrap_err(),
        CasError::NotFound
    ));
}

// -------- Extra: encode/decode roundtrip on edge sizes --------

#[test]
fn test_put_empty_payload_roundtrip() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");
    let h = store.put(b"").expect("put empty");
    assert_eq!(store.get(&h).unwrap().as_slice(), b"");
    assert_eq!(store.blob_count().unwrap(), 1);
}

#[test]
fn test_put_multiple_distinct_payloads() {
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");

    let payloads: Vec<Vec<u8>> = (0..10)
        .map(|i| (0..i * 100).map(|j| j as u8).collect())
        .collect();
    let mut hashes = Vec::new();
    for p in &payloads {
        hashes.push(store.put(p).expect("put"));
    }
    // All hashes distinct.
    let unique: HashSet<_> = hashes.iter().copied().collect();
    assert_eq!(unique.len(), payloads.len());
    assert_eq!(store.blob_count().unwrap(), payloads.len());

    // Round-trip each one.
    for (i, h) in hashes.iter().enumerate() {
        assert_eq!(store.get(h).unwrap(), payloads[i]);
    }
}

#[test]
fn test_blob_count_excludes_staging_and_sidecar() {
    let dir = tempdir().expect("tempdir");
    let mut store = CasStore::open(dir.path()).expect("open");

    let h1 = store.put(b"blob one").expect("put");
    store.put(b"blob two").expect("put");
    store.incr(&h1);
    store.persist().expect("persist");

    // refs.json exists, staging dir exists, but only 2 blobs counted.
    assert_eq!(store.blob_count().unwrap(), 2);

    // Leave a stray staging file; it must not be counted.
    let stray = dir
        .path()
        .join(STAGING_DIR)
        .join("deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef.partial");
    fs::write(&stray, b"junk").expect("write stray");
    assert_eq!(store.blob_count().unwrap(), 2);
}

#[test]
fn test_blob_count_with_fanout() {
    // Multiple distinct hashes will naturally fan out across prefix buckets;
    // blob_count must walk them.
    let dir = tempdir().expect("tempdir");
    let store = CasStore::open(dir.path()).expect("open");

    for i in 0..64u8 {
        store.put(&[i; 64]).expect("put");
    }
    assert_eq!(store.blob_count().unwrap(), 64);
}

fn dir_size(path: &Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(path) {
        for entry in entries.flatten() {
            let entry_path = entry.path();
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                total += dir_size(&entry_path);
            } else if let Ok(meta) = entry.metadata() {
                total += meta.len();
            }
        }
    }
    total
}
