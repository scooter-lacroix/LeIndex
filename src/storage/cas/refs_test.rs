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

// ---------------------------------------------------------------------------
// Overlapping handles and crashed owners
// ---------------------------------------------------------------------------

fn sidecar_count(dir: &std::path::Path, hash: &[u8; 32]) -> u64 {
    super::read_sidecar(&dir.join(REFS_SIDECAR))
        .counts
        .get(&super::hash_to_hex(hash))
        .copied()
        .unwrap_or(0)
}

#[test]
fn test_refcount_overlapping_handles_do_not_lose_updates() {
    // Lease A persists 1, lease B (opened before that persist) persists its own
    // increment, then A drops first. Whole-map replacement used to write A's
    // stale view (0) here while B was still live, and B later wrote 1 after
    // both were gone.
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"overlapping leases");

    let mut a = JsonSidecarStore::open(dir.path()).unwrap();
    let mut b = JsonSidecarStore::open(dir.path()).unwrap();

    a.incr(&h);
    a.persist().unwrap();
    b.incr(&h);
    b.persist().unwrap();
    assert_eq!(sidecar_count(dir.path(), &h), 2, "both leases are recorded");

    a.decr(&h).unwrap();
    a.persist().unwrap();
    assert_eq!(
        sidecar_count(dir.path(), &h),
        1,
        "A releasing must leave B's lease in place"
    );

    b.decr(&h).unwrap();
    b.persist().unwrap();
    assert_eq!(
        sidecar_count(dir.path(), &h),
        0,
        "no phantom lease once both are released"
    );
}

#[test]
fn test_refcount_concurrent_persists_from_many_handles_sum_exactly() {
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"concurrent leases");
    let root = dir.path().to_path_buf();

    let threads: Vec<_> = (0..8)
        .map(|_| {
            let root = root.clone();
            std::thread::spawn(move || {
                let mut store = JsonSidecarStore::open(&root).unwrap();
                for _ in 0..25 {
                    store.incr(&h);
                    store.persist().unwrap();
                }
            })
        })
        .collect();
    for thread in threads {
        thread.join().unwrap();
    }

    assert_eq!(sidecar_count(dir.path(), &h), 200);
}

#[test]
fn test_refcount_reload_sees_other_handles_leases() {
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"reload sees leases");

    let observer = JsonSidecarStore::open(dir.path()).unwrap();
    let mut leaser = JsonSidecarStore::open(dir.path()).unwrap();
    leaser.incr(&h);
    leaser.persist().unwrap();

    assert_eq!(observer.refcount(&h), 0, "stale until reloaded");
    observer.reload().unwrap();
    assert_eq!(observer.refcount(&h), 1);
}

#[test]
fn test_refcount_reload_keeps_unpersisted_local_changes() {
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"reload keeps local");

    let mut other = JsonSidecarStore::open(dir.path()).unwrap();
    let mut local = JsonSidecarStore::open(dir.path()).unwrap();
    other.incr(&h);
    other.persist().unwrap();
    local.incr(&h);
    local.reload().unwrap();
    assert_eq!(local.refcount(&h), 2, "other's persisted + local pending");
}

/// A pid that is guaranteed not to be running: a child that has been reaped.
#[cfg(unix)]
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true").spawn().expect("spawn");
    let pid = child.id();
    child.wait().expect("wait");
    pid
}

/// Write a full sidecar (counts + owners) directly, bypassing the store.
fn write_sidecar(
    dir: &std::path::Path,
    counts: HashMap<[u8; 32], u64>,
    owners: BTreeMap<String, super::OwnerHolds>,
) {
    let file = super::SidecarFile {
        counts: counts
            .iter()
            .map(|(hash, count)| (super::hash_to_hex(hash), *count))
            .collect(),
        owners,
    };
    std::fs::write(
        dir.join(REFS_SIDECAR),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();
}

#[cfg(unix)]
#[test]
fn test_refcount_counts_of_a_crashed_owner_are_reclaimed_on_open() {
    // A process that persisted a lease and was then killed leaves positive
    // counts behind. Without owner metadata they pinned blobs forever.
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"crashed owner");
    let survivor = blob_hash(b"live owner");

    // A live handle in this process holds one count on `survivor`.
    let mut live = JsonSidecarStore::open(dir.path()).unwrap();
    live.incr(&survivor);
    live.persist().unwrap();

    // The crashed owner held two counts on `h` and one on `survivor`.
    let pid = dead_pid();
    let mut file = super::read_sidecar(&dir.path().join(REFS_SIDECAR));
    let mut holds = super::OwnerHolds::default();
    holds.blobs.insert(super::hash_to_hex(&h), 2);
    holds.blobs.insert(super::hash_to_hex(&survivor), 1);
    file.owners.insert(format!("{pid}.1.0"), holds);
    *file.counts.entry(super::hash_to_hex(&h)).or_insert(0) += 2;
    *file
        .counts
        .entry(super::hash_to_hex(&survivor))
        .or_insert(0) += 1;
    std::fs::write(
        dir.path().join(REFS_SIDECAR),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar_count(dir.path(), &h), 2);
    assert_eq!(sidecar_count(dir.path(), &survivor), 2);

    let reopened = JsonSidecarStore::open(dir.path()).unwrap();
    assert_eq!(reopened.refcount(&h), 0, "crashed owner's counts reclaimed");
    assert_eq!(
        reopened.refcount(&survivor),
        1,
        "only the dead owner's share is reclaimed; the live lease stays"
    );
    assert_eq!(sidecar_count(dir.path(), &h), 0, "reclaim is persisted");
    assert!(
        !super::read_sidecar(&dir.path().join(REFS_SIDECAR))
            .owners
            .contains_key(&format!("{pid}.1.0")),
        "dead owner's entry is removed"
    );
}

#[test]
fn test_refcount_released_leases_leave_no_owner_entry() {
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"ledger cleanup");
    let mut store = JsonSidecarStore::open(dir.path()).unwrap();
    store.incr(&h);
    store.persist().unwrap();
    let owner_entries = || {
        super::read_sidecar(&dir.path().join(REFS_SIDECAR))
            .owners
            .len()
    };
    assert_eq!(owner_entries(), 1);
    store.decr(&h).unwrap();
    store.persist().unwrap();
    assert_eq!(
        owner_entries(),
        0,
        "a handle holding nothing keeps no owner entry"
    );
}

#[test]
fn test_refcount_release_transitions_count_and_holding_together() {
    // Two handles each hold one lease on the same blob; A releases. The
    // shared count (2 -> 1) and A's owner holdings must transition in ONE
    // atomic sidecar write: with the earlier two-file layout, a crash
    // between the count write and the ledger write left A's stale holdings
    // behind, and recovery subtracted them from the already-decremented
    // count — freeing the blob while B was still reading it. Both live in
    // one file now, so an observed sidecar can never show the intermediate
    // state.
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"atomic release");

    let mut a = JsonSidecarStore::open(dir.path()).unwrap();
    let mut b = JsonSidecarStore::open(dir.path()).unwrap();
    a.incr(&h);
    a.persist().unwrap();
    b.incr(&h);
    b.persist().unwrap();
    assert_eq!(sidecar_count(dir.path(), &h), 2);
    assert_eq!(
        super::read_sidecar(&dir.path().join(REFS_SIDECAR))
            .owners
            .len(),
        2
    );

    a.decr(&h).unwrap();
    a.persist().unwrap();

    let file = super::read_sidecar(&dir.path().join(REFS_SIDECAR));
    assert_eq!(
        file.counts.get(&super::hash_to_hex(&h)).copied(),
        Some(1),
        "count is decremented"
    );
    assert_eq!(
        file.owners.len(),
        1,
        "and A's holdings are dropped in the same write"
    );

    // Reopening (recovery) must not subtract anything further.
    let reopened = JsonSidecarStore::open(dir.path()).unwrap();
    assert_eq!(
        reopened.refcount(&h),
        1,
        "B's live lease survives A's release and a recovery pass"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn test_refcount_recycled_own_pid_with_new_start_time_is_reclaimed() {
    // A restarted process frequently receives its predecessor's pid (in
    // containers, often pid 1). The predecessor's holdings must be judged by
    // start time, not by "this pid is mine, so it is alive" — otherwise they
    // pin blobs and generations forever.
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"recycled pid");
    let our_pid = std::process::id();
    let wrong_start = super::process_start_ticks(our_pid).unwrap_or(0) + 1;

    let mut counts = HashMap::new();
    counts.insert(h, 1u64);
    let mut holds = super::OwnerHolds::default();
    holds.blobs.insert(super::hash_to_hex(&h), 1);
    let mut owners = BTreeMap::new();
    owners.insert(format!("{our_pid}.{wrong_start}.0"), holds);
    write_sidecar(dir.path(), counts, owners);

    let reopened = JsonSidecarStore::open(dir.path()).unwrap();
    assert_eq!(
        reopened.refcount(&h),
        0,
        "a same-pid owner with a different start time is dead"
    );
}

#[test]
fn test_refcount_legacy_flat_sidecar_is_still_readable() {
    // Sidecars written before owner tracking moved into refs.json are flat
    // hex → count maps; they must keep opening.
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"legacy flat");
    let flat = format!("{{\"{}\": 3}}", super::hash_to_hex(&h));
    fs::write(dir.path().join(REFS_SIDECAR), flat).unwrap();

    let store = JsonSidecarStore::open(dir.path()).unwrap();
    assert_eq!(store.refcount(&h), 3);
}

#[cfg(unix)]
#[test]
fn test_refcount_legacy_owner_ledger_dir_is_absorbed() {
    // Intermediate v2.0.0 builds kept per-owner ledgers in refs.owners/.
    // Opening a store written by one must fold them into the sidecar and
    // remove the directory, dead owner included.
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"legacy ledger");
    let pid = dead_pid();

    let mut store = JsonSidecarStore::open(dir.path()).unwrap();
    store.incr(&h);
    store.persist().unwrap();

    let owners = dir.path().join(REFS_OWNERS_DIR);
    fs::create_dir_all(&owners).unwrap();
    let mut ledger = HashMap::new();
    ledger.insert(h, 1u64);
    super::write_counts_atomic(&owners.join(format!("{pid}.1.0.json")), &ledger).unwrap();
    // Bump the count for the crashed owner in the sidecar's own format
    // (writing a flat hex map would destroy the owners map).
    let mut file = super::read_sidecar(&dir.path().join(REFS_SIDECAR));
    *file.counts.entry(super::hash_to_hex(&h)).or_insert(0) += 1;
    std::fs::write(
        dir.path().join(REFS_SIDECAR),
        serde_json::to_vec_pretty(&file).unwrap(),
    )
    .unwrap();

    let reopened = JsonSidecarStore::open(dir.path()).unwrap();
    assert!(
        !owners.exists(),
        "legacy ledger dir is removed after absorption"
    );
    assert_eq!(
        reopened.refcount(&h),
        1,
        "only the live handle's count remains"
    );
}

#[test]
fn test_generation_holds_are_tracked_by_identity_across_handles() {
    let dir = tempdir().expect("tempdir");

    let mut leaser = JsonSidecarStore::open(dir.path()).unwrap();
    leaser.record_generation_hold(7);
    leaser.record_generation_hold(7);
    leaser.persist().unwrap();

    let observer = JsonSidecarStore::open(dir.path()).unwrap();
    assert!(
        observer.held_generations().contains(&7),
        "a persisted generation hold is visible to a fresh handle"
    );

    leaser.release_generation_hold(7);
    leaser.persist().unwrap();
    assert!(
        observer.held_generations().contains(&7),
        "one hold remains after a single release of two"
    );

    leaser.release_generation_hold(7);
    leaser.persist().unwrap();
    assert!(
        observer.held_generations().is_empty(),
        "the generation is no longer held once every lease is released"
    );
}

#[cfg(unix)]
#[test]
fn test_generation_hold_of_a_crashed_owner_is_reclaimed() {
    let dir = tempdir().expect("tempdir");
    let pid = dead_pid();

    let mut holds = super::OwnerHolds::default();
    holds.generations.insert("9".to_string(), 1);
    let mut owners = BTreeMap::new();
    owners.insert(format!("{pid}.1.0"), holds);
    write_sidecar(dir.path(), HashMap::new(), owners);

    let reopened = JsonSidecarStore::open(dir.path()).unwrap();
    assert!(
        reopened.held_generations().is_empty(),
        "a crashed owner's generation hold must not pin the generation forever"
    );
}

#[test]
fn test_refcount_remove_keeps_entry_another_handle_still_counts() {
    let dir = tempdir().expect("tempdir");
    let h = blob_hash(b"remove vs lease");

    let mut gc = JsonSidecarStore::open(dir.path()).unwrap();
    let mut reader = JsonSidecarStore::open(dir.path()).unwrap();
    reader.incr(&h);
    reader.persist().unwrap();

    // GC (stale view: count 0) drops the entry; the merge must not discard the
    // reader's live count.
    gc.remove(&h);
    gc.persist().unwrap();
    assert_eq!(sidecar_count(dir.path(), &h), 1);
}
