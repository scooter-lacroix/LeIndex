//! Tests for the CAS refcount store (winner of the WS4 Task 11 decision):
//! incr/decr persistence, underflow, GC, and crash-recovery.
//!
//! Every behavioral test runs once per supported [`RefcountStore`] backend.
//! Only the JSON sidecar winner remains in `refs.rs`.

use super::*;
use crate::storage::cas::blob::blob_hash;
use std::fs;
use tempfile::tempdir;

/// Run a behavioral test against a fresh JSON-sidecar store.
fn run(body: impl FnOnce(JsonSidecarStore)) {
    let dir = tempdir().expect("tempdir");
    let store = JsonSidecarStore::open(dir.path()).expect("open refcount store");
    body(store);
}

#[test]
fn test_refcount_incr_accumulates() {
    run(|mut store| {
        let h = blob_hash(b"some payload");
        assert_eq!(store.refcount(&h), 0);
        assert_eq!(store.incr(&h), 1);
        assert_eq!(store.incr(&h), 2);
        assert_eq!(store.incr(&h), 3);
        assert_eq!(store.refcount(&h), 3);
    });
}

#[test]
fn test_refcount_decr_basic() {
    run(|mut store| {
        let h = blob_hash(b"payload");
        store.incr(&h);
        store.incr(&h);
        store.incr(&h);
        assert_eq!(store.decr(&h).unwrap(), 2);
        assert_eq!(store.decr(&h).unwrap(), 1);
        assert_eq!(store.decr(&h).unwrap(), 0);
    });
}

#[test]
fn test_refcount_decr_underflow_at_zero() {
    run(|mut store| {
        let h = blob_hash(b"underflow test");
        let err = store.decr(&h).unwrap_err();
        assert!(matches!(err, CasError::RefcountUnderflow));
    });
}

#[test]
fn test_refcount_two_decrs_from_one() {
    run(|mut store| {
        let h = blob_hash(b"two decrs");
        store.incr(&h);
        assert_eq!(store.decr(&h).unwrap(), 0);
        let err = store.decr(&h).unwrap_err();
        assert!(matches!(err, CasError::RefcountUnderflow));
    });
}

#[test]
fn test_refcount_persist_across_reopen() {
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"persistent");

    {
        let mut store = JsonSidecarStore::open(dir.path()).expect("open");
        store.incr(&h);
        store.incr(&h);
        store.incr(&h);
        store.decr(&h).unwrap();
        assert_eq!(store.refcount(&h), 2);
        store.persist().expect("persist");
    }
    {
        let store = JsonSidecarStore::open(dir.path()).expect("reopen");
        assert_eq!(store.refcount(&h), 2);
    }
}

#[test]
fn test_refcount_persist_zero_after_decrement_to_zero() {
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"down to zero");

    {
        let mut store = JsonSidecarStore::open(dir.path()).expect("open");
        store.incr(&h);
        store.incr(&h);
        store.incr(&h);
        store.persist().expect("persist before");

        // Decr from 3 to 0.
        assert_eq!(store.decr(&h).unwrap(), 2);
        assert_eq!(store.decr(&h).unwrap(), 1);
        assert_eq!(store.decr(&h).unwrap(), 0);
        store.persist().expect("persist after");
        assert_eq!(store.refcount(&h), 0);
    }
    {
        let mut store = JsonSidecarStore::open(dir.path()).expect("reopen");
        // Refcount 0 is persisted explicitly.
        assert_eq!(store.refcount(&h), 0);
        // Decr at 0 should still error.
        assert!(matches!(
            store.decr(&h).unwrap_err(),
            CasError::RefcountUnderflow
        ));
    }
}

#[test]
fn test_refcount_crash_recovery_after_persist() {
    // Per VAL-CAS-016: incr x5, persist, kill -> reopen and refcount is 5.
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"crash recovery persist");

    {
        let mut store = JsonSidecarStore::open(dir.path()).expect("open");
        for _ in 0..5 {
            store.incr(&h);
        }
        store.persist().expect("persist before simulated crash");
    }
    {
        let store = JsonSidecarStore::open(dir.path()).expect("reopen");
        assert_eq!(store.refcount(&h), 5);
    }
}

#[test]
fn test_refcount_crash_without_persist() {
    // Per VAL-CAS-016: incr x3 without fsync -> reopen and refcount <= 3
    // (lost increments acceptable; phantom counts are not).
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"crash recovery no persist");

    {
        let mut store = JsonSidecarStore::open(dir.path()).expect("open");
        // Do a dummy persist so the file exists with some other hash,
        // increasing the chance of a phantom-count regression being caught.
        let other = blob_hash(b"different");
        store.incr(&other);
        store.persist().expect("persist other hash");
    }
    {
        let mut store = JsonSidecarStore::open(dir.path()).expect("reopen");
        // Now incr h in-memory but do not persist.
        store.incr(&h);
        store.incr(&h);
        store.incr(&h);
        assert_eq!(store.refcount(&h), 3);
        // Drop without persist.
    }
    {
        let store = JsonSidecarStore::open(dir.path()).expect("reopen after crash");
        // h may be 0 (never persisted), but must be <= 3.
        assert!(
            store.refcount(&h) <= 3,
            "phantom refcounts after crash-not-fsynced"
        );
    }
}

#[test]
fn test_refcount_zero_refcount_hashes() {
    run(|mut store| {
        let a = blob_hash(b"a");
        let b = blob_hash(b"b");
        let c = blob_hash(b"c");
        store.incr(&a);
        store.incr(&b);
        store.incr(&b);
        store.incr(&c);
        store.decr(&c).unwrap(); // c -> 0

        let zeroes: HashSet<_> = store.zero_refcount_hashes().into_iter().collect();
        assert_eq!(zeroes.len(), 1);
        assert!(zeroes.contains(&c));
    });
}

#[test]
fn test_refcount_tracked_and_iter() {
    run(|mut store| {
        let a = blob_hash(b"a");
        let b = blob_hash(b"b");
        store.incr(&a);
        store.incr(&b);
        store.incr(&b);
        let tracked = store.tracked_hashes();
        assert!(tracked.contains(&a) && tracked.contains(&b));
        let pairs = store.iter();
        assert_eq!(pairs.len(), 2);
        assert!(pairs.contains(&(a, 1)));
        assert!(pairs.contains(&(b, 2)));
    });
}

#[test]
fn test_refcount_reopen_missing_sidecar() {
    // No sidecar file present -> empty store.
    let dir = tempdir().expect("tempdir");
    let store = JsonSidecarStore::open(dir.path()).expect("open");
    let h = blob_hash(b"absent");
    assert_eq!(store.refcount(&h), 0);
    assert!(!store.contains(&h));
}

#[test]
fn test_refcount_reopen_corrupt_sidecar() {
    // Corrupt sidecar should not panic; store starts empty.
    let dir = tempdir().expect("tempdir");
    fs::write(dir.path().join(REFS_SIDECAR), b"!!!not valid json!!!").expect("write corrupt");
    let store = JsonSidecarStore::open(dir.path()).expect("open");
    assert_eq!(store.tracked_hashes().len(), 0);
}
