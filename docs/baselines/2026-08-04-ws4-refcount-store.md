# WS4 CAS Refcount Store — Sidecar (JSON) vs SQLite — Decision

**Task:** WS4 Task 11 (TBD resolution, spec §10 item 1)
**Date:** 2026-08-05
**Status:** DECISION: JSON sidecar (`cas/refs.json`) is the winner and the default

## Context

The CAS refcount store must persist per-blob reference counts. WS4 Task 11
turned the initial in-memory `HashMap` + JSON sidecar into a
`trait RefcountStore` with **two** interchangeable implementations, benchmarked
both on a 10k-blob fixture, and kept the winner:

- **(a) JSON sidecar** — `JsonSidecarStore`: in-memory `HashMap<[u8;32], u64>`
  persisted as `<cas_root>/refs.json` via an atomic write-tmp-fsync-rename.
- **(b) SQLite** — `SqliteRefcountStore`: in-memory `HashMap` persisted as
  `<cas_root>/refs.db` via a single batch `INSERT OR REPLACE` transaction with
  `synchronous = FULL`.

Both preserve the write-barrier semantics of VAL-CAS-016: `persist()` is the
durable commit point; a kill before `persist()` may lose increments but never
produces phantom counts (the in-memory map is the source of truth between
flushes). The blobs themselves are identical — only the persistence
substrate differs.

## Benchmark (10k-blob fixture)

Method: `cargo bench --bench refcount_store --features full -- --warm-up-time 1 --measurement-time 2 --sample-size 10` on x86_64 (AVX2 machine), criterion p50. Fixture = 10,000 distinct blake3 hashes.

| Metric | JSON sidecar | SQLite `refs.db` | Delta | Winner |
|---|---|---|---|---|
| `incr` ×10 000 | 177.3 µs | 176.7 µs | ~0% | tie |
| `decr` ×10 000 | 347.5 µs | 348.3 µs | ~0% | tie |
| `persist` (fsync commit) ×10k | **12.1 ms** | 28.6 ms | 2.36× faster | **JSON** |
| `reopen` (load 10k) | 3.17 ms | 2.88 ms | 10% faster | SQLite |
| resident-memory delta (10k) | **2,664 KiB** | 4,140 KiB | 36% lower | **JSON** |
| on-disk footprint (10k) | **730,002 B** | 1,581,056 B | 2.2× smaller | **JSON** |
| crash-recovery (persist → reopen exact) | PASS | PASS | — | tie |
| crash-recovery (no-persist → no phantom) | PASS | PASS | — | tie |

### Throughput (incr/decr)
The in-memory fast path is identical across backends — both keep the full
`HashMap` in RAM and only differ in how `persist()` writes it out — so
incr/decr throughput is parity (177.3 vs 176.7 µs for 10k incrs).

### fsync / durable-write cost
`persist()` is the hot write path: it is the write barrier invoked on *every*
`GenerationLease` acquire and release and after every CAS GC sweep. JSON
serializes the 10k map once and fsyncs one file (12.1 ms). SQLite replays a
10k-row batch transaction with `synchronous = FULL` (two fsyncs: journal +
database), taking 28.6 ms — **2.36× slower**.

### Crash-recovery
Both pass the identical crash suite (`test_refcount_crash_recovery_after_persist`
and `test_refcount_crash_without_persist`): after a persist+reopen the count is
exact; increments that were never fsynced are lost but never produce a phantom
count. JSON relies on atomic `rename`; SQLite relies on its journal. Both give
a reopenable, never-corrupt artifact. Coverage: `src/storage/cas/refs_test.rs` +
`cas_test.rs` (`test_blob_count_excludes_staging_and_sidecar`).

### Memory
Resident-memory delta while a fuelled+persisted 10k store is live: JSON 2,664
KiB vs SQLite 4,140 KiB. SQLite's extra ~1.5 MiB is its connection + page
cache + journal tail. Even with this trivial corpus (10k blobs), JSON is ~36%
lighter; at the daemon's floor 8 GiB page-cache budget that matters for
long-running read-model hosts.

## Decision

**Adopt the JSON sidecar (`cas/refs.json`) as the default refcount backend.**

It matches SQLite on the in-memory hot path (incr/decr/store), **dominates the
durable-write path** (the dimension that matters most: `persist()` is the write
barrier for every lease acquire/release and every GC), uses **~36% less
resident memory**, and writes **~2.2× less data** to disk. SQLite's single edge
is a ~10% faster *cold reopen* (3.17 vs 2.88 ms) — a one-time process-start
cost with no steady-state value at the refcount scale we actually see (this
repo: 5 blobs; a dense mirror: 10²–10⁴ blobs).

**Implementation action taken:** the losing `SqliteRefcountStore` was deleted
from `src/storage/cas/refs.rs`. `src/storage/cas/refs.rs` now contains a single
implementation (`JsonSidecarStore`) behind `trait RefcountStore`, selected as
`CasStore::open`'s default. `benches/refcount_store.rs` tracks the shipped
winner for regression.

## Rollback / revisit

The `trait RefcountStore` seam remains in place, so a future backend can be
slotted in if need grows beyond JSON's scale (e.g. hundreds of thousands of
tracked blobs where reopen latency dominates). The benchmark is reproducible
via `cargo bench --bench refcount_store --features full`.
