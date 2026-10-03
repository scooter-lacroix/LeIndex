//! WS4 Task 11: winning CAS refcount store regression benchmark.
//!
//! On a 10,000-hash fixture, measures incr/decr throughput, `persist`
//! (fsync) cost, reopen latency, and resident-memory delta for the JSON
//! sidecar backend chosen by the Task 11 decision (see BENCHMARKS.md
//! Section 9 "CAS engineering decisions" for the digested outcome).
//!
//! Run: `cargo bench --bench refcount_store --features full`
//!
//! The Task 11 sidecar-vs-SQLite comparison and its full numbers live in the
//! decision file; this benchmark tracks the shipped winner so a regression in
//! the refcount hot path is caught by CI.

use criterion::{Criterion, criterion_group, criterion_main};
use leindex::storage::cas::blob::blob_hash;
use leindex::storage::cas::refs::{JsonSidecarStore, RefcountStore};
use std::path::Path;
use tempfile::tempdir;

/// Number of distinct blob hashes in the fixture (per Task 11: a 10k-blob
/// fixture).
const N_HASHES: usize = 10_000;

/// Build the 10k-hash fixture.
fn make_hashes() -> Vec<[u8; 32]> {
    (0..N_HASHES)
        .map(|i| blob_hash(format!("fixture-blob-{i}").as_bytes()))
        .collect()
}

/// Resident-set size in KiB, read from `/proc/self/statm` (Linux).
fn rss_kb() -> Option<u64> {
    let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
    let mut fields = statm.split_whitespace();
    fields.next()?; // total virtual size (pages)
    let resident_pages = fields.next()?.parse::<u64>().ok()?;
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) } as u64;
    Some(resident_pages * page_size / 1024)
}

/// Open a fresh JSON store at a temp dir and fuel it with `hashes`.
fn fuel(hashes: &[[u8; 32]]) -> (tempfile::TempDir, JsonSidecarStore) {
    let dir = tempdir().expect("tempdir");
    let mut store = JsonSidecarStore::open(dir.path()).expect("open");
    for h in hashes {
        store.incr(h);
    }
    (dir, store)
}

/// Resident-memory delta (RSS while a fuelled+persisted store is live vs the
/// process baseline). Reads `/proc/self/statm`.
fn memory_report(hashes: &[[u8; 32]]) {
    let baseline = rss_kb().unwrap_or(0);
    let peak = {
        let (_dir, store) = fuel(hashes);
        let _ = store.persist();
        rss_kb().unwrap_or(0)
    };
    eprintln!(
        "[memory] 10k-blob resident delta — json: {} KiB",
        peak.saturating_sub(baseline)
    );
}

fn bench_incr(c: &mut Criterion, hashes: &[[u8; 32]]) {
    let mut group = c.benchmark_group("incr_10k");
    group.bench_function("json_sidecar", |b| {
        let (_dir, mut store) = fuel(hashes);
        b.iter(|| {
            for h in hashes {
                std::hint::black_box(store.incr(h));
            }
        });
    });
    group.finish();
}

fn bench_decr(c: &mut Criterion, hashes: &[[u8; 32]]) {
    let mut group = c.benchmark_group("decr_10k");
    // Each hash is fuelled to count 1; every iteration decrements it back to
    // 0 and restores it to 1 so the fed store never underflows across runs.
    group.bench_function("json_sidecar", |b| {
        let (_dir, mut store) = fuel(hashes);
        b.iter(|| {
            for h in hashes {
                std::hint::black_box(store.decr(h).expect("decr"));
                store.incr(h);
            }
        });
    });
    group.finish();
}

fn bench_persist(c: &mut Criterion, hashes: &[[u8; 32]]) {
    let mut group = c.benchmark_group("persist_10k");
    group.bench_function("json_sidecar", |b| {
        let (_dir, store) = fuel(hashes);
        b.iter(|| {
            let _: () = store.persist().expect("persist");
            std::hint::black_box(())
        });
    });
    group.finish();
}

fn bench_reopen(c: &mut Criterion, root: &Path) {
    let mut group = c.benchmark_group("reopen_10k");
    group.bench_function("json_sidecar", |b| {
        b.iter(|| std::hint::black_box(JsonSidecarStore::open(root).expect("open")));
    });
    group.finish();
}

fn bench_all(c: &mut Criterion) {
    let hashes = make_hashes();
    memory_report(&hashes);

    bench_incr(c, &hashes);
    bench_decr(c, &hashes);
    bench_persist(c, &hashes);

    // Reopen: persist a fuelled 10k store and reopen it.
    let reopen_dir = tempdir().expect("tempdir");
    {
        let mut store = JsonSidecarStore::open(reopen_dir.path()).expect("open");
        for h in &hashes {
            store.incr(h);
        }
        store.persist().expect("persist");
    }
    bench_reopen(c, reopen_dir.path());
}

criterion_group!(name = refcount_store; config = Criterion::default().sample_size(10); targets = bench_all);
criterion_main!(refcount_store);
