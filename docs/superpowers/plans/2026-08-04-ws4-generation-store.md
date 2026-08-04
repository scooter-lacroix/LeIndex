# WS4: Immutable mmap Generation Store + Radical Size Reduction

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace per-generation full-copy directories with a content-addressed blob store (CAS) + mmap'd immutable generation manifests; add generation leases + atomic publication; enforce byte-bounded retention; cut `.leindex/` footprint 40–80× (this repo: 2.5 GiB → 30–60 MiB) before quantization. No-stall reads during indexing; no 2× RAM on incremental reindex.

**Architecture:** Generation = manifest referencing CAS blobs by blake3 hash. Readers mmap blobs zero-copy (quantized-native SIMD dot-products for neural); hold a `GenerationLease`. Writer streams blobs, dedups by hash, atomically publishes manifest + updates `CURRENT`. `LeIndex` stays `!Sync`; readers never touch the writer Mutex.

**Spec:** `docs/superpowers/specs/2026-08-04-ws4-generation-store-design.md` (read it first).
**Parent:** `docs/superpowers/specs/2026-08-04-leindex-resource-architecture-design.md` (§4.3, §6.5, §6.7, §10, §12.2).
**Tech Stack:** Rust, blake3, memmap2 (or equivalent), std::os::unix::io, SQLite (VACUUM/checkpoint).

**Existing infra (DO NOT reinvent):**
- `ProjectRegistry` (`src/cli/registry.rs`) — multi-project, LRU, `evict_idle_engines`. Lives in daemon post-WS3.
- `ProjectWriteLock` (`src/cli/leindex/mod.rs:98`) — cross-process flock; readers already skip it.
- `PublishedGeneration` (`src/cli/index_job.rs`) — `{generation, storage_path, health}`. Becomes the manifest pointer.
- `MemoryCapGuard::current_rss_mb()` (`src/cli/memory_cap.rs:15`) — reuse for admission accounting.
- `leindex cleanup` (`src/cli/cleanup.rs`) — extend for CAS/GC.
- Existing `.leindex/generations/<N>/` layout — migration source.

**TBD-resolution requirement (user-mandated):** Tasks 11–14 resolve EVERY open/TBD item from spec §4.6, §4.7, §10. The plan is not complete until each has a measured decision recorded in `docs/baselines/`.

---

## File Structure (created/modified)

| File | Responsibility |
|---|---|
| `src/storage/cas/mod.rs` | CAS store: put/get/refcount/GC |
| `src/storage/cas/blob.rs` | Blob format (magic/version/hash/payload), mmap reader |
| `src/storage/cas/refs.rs` | Refcount storage (decision in Task 11) |
| `src/storage/generation/mod.rs` | Manifest types, read/validate |
| `src/storage/generation/lease.rs` | `GenerationLease` refcount guard |
| `src/storage/generation/manifest.rs` | Manifest serialize/deserialize, atomic publish |
| `src/storage/generation/reader.rs` | Zero-copy layer readers (neural SIMD, tfidf, pdg, symbols) |
| `src/storage/generation/writer.rs` | Blob staging, dedup, publish (initial: whole-serialize; WS6-9: streaming) |
| `src/storage/generation/retention.rs` | Generation + CAS + job retention sweeps |
| `src/storage/generation/migrate.rs` | One-time legacy→CAS migration |
| `src/cli/leindex/indexing/mod.rs` | Wire publication into `publish_generation` |
| `src/cli/registry.rs` | `lease_generation(project)` API |
| `src/cli/cleanup.rs` | `leindex retention --report`, CAS GC command |
| Modify: search read-path handlers | Acquire lease; read from mmap generation instead of heap mirror |

---

## Task 1: CAS blob format + store (put/get, no refcount yet)

**Files:** `src/storage/cas/mod.rs`, `src/storage/cas/blob.rs`, `src/storage/mod.rs` (add `pub mod cas;`)

- [ ] **Step 1: Write failing test** — `put(bytes) -> hash`; `get(hash) -> bytes`; idempotent (same content → same hash, stored once).

```rust
// src/storage/cas/blob.rs
use blake3;
pub const BLOB_MAGIC: &[u8; 8] = b"LIDX-BLB1";

pub fn blob_hash(payload: &[u8]) -> [u8; 32] { blake3::hash(payload).into() }
pub fn encode_blob(payload: &[u8]) -> Vec<u8> { todo!() } // magic+version+hash+len+payload
pub fn validate_blob(bytes: &[u8]) -> Result<[u8;32], BadBlob> { todo!() } // re-hash, compare

#[cfg(test)]
mod test {
    #[test]
    fn test_put_get_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let store = CasStore::open(dir.path()).unwrap();
        let h = store.put(b"hello world").unwrap();
        assert_eq!(store.get(&h).unwrap(), b"hello world");
        // idempotent
        let h2 = store.put(b"hello world").unwrap();
        assert_eq!(h, h2);
        assert_eq!(store.blob_count(), 1);
    }
    #[test]
    fn test_corrupt_blob_rejected() { /* truncate, assert validate_blob errors */ }
}
```

- [ ] **Step 2: Verify fail → Step 3: Implement** — `CasStore::put` writes `.staging/<hash>.partial`, fsync, rename to `cas/<prefix>/<hash>`; if target exists, delete staging (dedup). Path layout `<hash[0..2]>/<hash>`.
- [ ] **Step 4: Verify pass → Step 5: Commit**

```bash
git add src/storage/cas/ src/storage/mod.rs
git commit -m "feat(cas): content-addressed blob store with hash-dedup put/get"
```

---

## Task 2: CAS refcount + GC

**Files:** `src/storage/cas/refs.rs`, `src/storage/cas/mod.rs`

- [ ] **Step 1: Write failing test** — `incr(hash)`, `decr(hash)`; blob GC-eligible only at refcount 0 + no generation reference; `gc()` removes eligible blobs and returns reclaimed bytes.

- [ ] **Step 2-4:** TDD. `refs.rs` decision deferred to Task 11 (sidecar vs SQLite) — start with in-memory `HashMap<Hash, u64>` + a JSON sidecar `cas/refs.json` for persistence (simplest); Task 11 measures and may swap.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(cas): refcount + GC for unreferenced blobs"
```

---

## Task 3: Generation manifest types + serialize

**Files:** `src/storage/generation/mod.rs`, `src/storage/generation/manifest.rs`, `src/storage/mod.rs`

- [ ] **Step 1: Write failing test** — manifest round-trips (serialize → deserialize equal); validates `graph_fingerprint`/`search_fingerprint`; rejects version mismatch.

```rust
pub const MANIFEST_MAGIC: &[u8; 8] = b"LIDX-GEN1";
pub struct Manifest {
    pub version: u16, pub generation: u64,
    pub model_identity: ModelIdentity,
    pub graph_fingerprint: [u8;32], pub search_fingerprint: [u8;32],
    pub layers: HashMap<LayerKind, [u8;32]>,  // layer → CAS hash
}
pub enum LayerKind { Db, Tfidf, Neural, Pdg, Symbols }
```

- [ ] **Step 2-4:** TDD. Use bincode for compactness (matches `embed::protocol` pattern).
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(generation): manifest format with layer→hash map + fingerprints"
```

---

## Task 4: GenerationLease + ProjectRegistry API

**Files:** `src/storage/generation/lease.rs`, `src/cli/registry.rs`

- [ ] **Step 1: Write failing test** — `lease_generation(project)` returns a `GenerationLease` for the current manifest; dropping it decrements refcounts; blobs not GC'd while lease held.

```rust
pub struct GenerationLease { store: Arc<CasStore>, hashes: Vec<[u8;32]>, gen: u64 }
impl Drop for GenerationLease { /* decr each hash */ }

impl ProjectRegistry {
    pub async fn lease_generation(&self, project: &Path) -> Result<GenerationLease> { todo!() }
}
```

- [ ] **Step 2-4:** TDD. Registry tracks current manifest per project (initially read from `CURRENT` + manifest file).
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(generation): GenerationLease refcount guard + registry API"
```

---

## Task 5: Zero-copy neural reader (quantized-native SIMD)

**Files:** `src/storage/generation/reader.rs`

- [ ] **Step 1: Write failing test** — mmap a neural blob (`{count, dim, dtype, scale, zero_point}` + flat array); read vector i; compute dot-product on f32 data; assert correct. Then INT8 blob: dot-product on i8 with scale/zero_point, assert matches dequantized-then-dotted within epsilon.

```rust
pub struct NeuralReader { mmap: Mmap, header: NeuralHeader, base: *const u8 }
pub enum Dtype { F32, Int8, Q4 }
impl NeuralReader {
    pub unsafe fn vector(&self, i: usize) -> VectorView;       // zero-copy view
    pub fn dot(&self, i: usize, query: &[f32]) -> f32;          // SIMD, dtype-aware
}
```

- [ ] **Step 2-4:** TDD f32 path first (correctness baseline), then INT8 SIMD path (wide-i32 accumulator with scale/zero_point). Q4 deferred to WS11 (model-gated).
- [ ] **Step 5: Benchmark** — add a micro-bench (`benches/neural_dot.rs`): f32 vs INT8 dot on 10k vectors × 1024 dim. INT8 must be ≥1.5× faster (else fall back to f32 + record in `docs/baselines/`).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(generation): zero-copy quantized-native SIMD neural reader"
```

---

## Task 6: Zero-copy TF-IDF / PDG / symbols readers

**Files:** `src/storage/generation/reader.rs` (extend)

- [ ] **Step 1: Write failing test** per layer — mmap + structured view + a sample read returns expected data.
- [ ] **Step 2-5:** TDD each layer. Symbol strings via mmap'd interning table (Task 13 finalizes format).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(generation): zero-copy tfidf/pdg/symbols readers"
```

---

## Task 7: Writer — blob staging, dedup, atomic publish

**Files:** `src/storage/generation/writer.rs`, `src/storage/generation/manifest.rs`

- [ ] **Step 1: Write failing test** — `GenerationWriter::stage(layer, bytes)`; `publish(N)` writes manifest.partial → fsync → rename → update CURRENT; crash simulation (delete manifest.partial mid-write) leaves last-good generation intact.

- [ ] **Step 2-4:** TDD. Staging dedups via CAS put (same hash → no dup). Atomic publish: write `generations/<N>/manifest.partial`, fsync, rename to `manifest`, atomically write `CURRENT` contents `<N>`.
- [ ] **Step 5: Crash test** — property test: kill at each step, verify `CURRENT`-referenced generation always readable.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(generation): crash-safe atomic publication (stage/dedup/publish)"
```

---

## Task 8: DB layer — checkpoint + VACUUM + copy-into-CAS

**Files:** `src/storage/generation/writer.rs` (extend), `src/cli/leindex/indexing/mod.rs`

- [ ] **Step 1: Write failing test** — given a `leindex.db`, produce a VACUUM-normalized immutable copy in CAS; two DBs with identical logical content but different page layouts hash equal after VACUUM (the dedup guarantee).

```rust
pub fn db_to_cas(conn: &Storage, cas: &CasStore) -> Result<[u8;32]> {
    // 1. checkpoint(WAL) into the DB
    // 2. VACUUM INTO a normalized temp file (deterministic page layout)
    // 3. cas.put(normalized_bytes)
}
```

- [ ] **Step 2-4:** TDD. Use `VACUUM INTO` for a normalized copy without disturbing the live DB. Prove dedup: run two sequential no-op reindexes, assert the DB blob hash is identical (collapses the user's observed 4→2 duplication).
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(generation): VACUUM-normalized DB copy-into-CAS (max dedup)"
```

---

## Task 9: Retention — generations, CAS, jobs

**Files:** `src/storage/generation/retention.rs`, `src/cli/cleanup.rs`

- [ ] **Step 1: Write failing test** — after publish, only `current + previous + leased(refcount>0)` generations remain; CAS blobs with no retained reference and refcount 0 are GC'd; jobs over `job_bytes_max` (default 128 MiB) deleted oldest-first; **completed jobs whose generation is published deleted immediately**.

```rust
pub fn retain_after_publish(store: &CasStore, gens_dir: &Path, jobs_dir: &Path, cfg: RetentionConfig) -> RetentionReport
pub struct RetentionConfig { pub max_generations: usize, pub job_bytes_max: u64 }
impl Default for RetentionConfig { /* max_generations: 2, job_bytes_max: 128 MiB */ }
```

- [ ] **Step 2-4:** TDD. Reuse `du`-style byte walk for jobs.
- [ ] **Step 5: Add `leindex retention --report`** — prints generation count, CAS bytes, job bytes, dedup ratio, GC candidates. Wire into `cleanup.rs`.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(generation): byte-bounded retention (gens current+1, CAS GC, jobs 128MiB)"
```

---

## Task 10: Legacy → CAS migration sweep

**Files:** `src/storage/generation/migrate.rs`, wired into first-run path in `LeIndex::new`/load.

- [ ] **Step 1: Write failing test** — given a legacy `.leindex/` (full-copy generations + 2 GiB jobs), migration produces: current+previous manifests referencing CAS blobs; stale generations deleted; jobs byte-bounded; completed jobs removed. Assert this-repo-shaped fixture: 2.5 GiB → ≤ 200 MiB.
- [ ] **Step 2-4:** TDD. Idempotent + crash-safe (writes new manifest beside old, swaps CURRENT last). Migration is a no-op if CAS already populated.
- [ ] **Step 5: Run on this repo's actual `.leindex/`** — record before/after to `docs/baselines/2026-08-04-ws4-migration.json`.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(generation): one-time legacy→CAS migration sweep"
```

---

## Task 11 (TBD resolution): CAS refcount storage — sidecar vs SQLite

**Resolves spec §10 item 1.**

- [ ] **Step 1: Implement both** behind a trait `RefcountStore` — (a) JSON sidecar `cas/refs.json`, (b) `cas/refs.db` SQLite.
- [ ] **Step 2: Benchmark** on 10k-blob fixture: incr/decr throughput, fsync cost, crash-recovery, memory. Write `docs/baselines/2026-08-04-ws4-refcount-store.md`.
- [ ] **Step 3: DECIDE** — pick the winner by lowest-overhead + crash-safety. Default to the winner; delete the loser.
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(cas): refcount store decision (sidecar vs SQLite) — <winner>"
```

---

## Task 12 (TBD resolution): Neural quantization read-path parity

**Resolves spec §4.5 + §6 read-path.** (Precision *selection* remains WS11; this resolves whether the read path handles INT8 correctly.)

- [ ] **Step 1: Write failing test** — INT8-quantized neural blob: dot-product via SIMD path vs dequantize-then-f32-dot, within 1e-4 relative epsilon across a fixture of 1000 vectors.
- [ ] **Step 2: Benchmark** — `benches/neural_dot.rs` extended: f32 vs INT8 SIMD. Record p50/p95. **Gate: INT8 ≥ 1.5× faster than f32** at equivalent recall (recall measured in WS11, but the SPEED gate is set here).
- [ ] **Step 3: DECIDE** — if INT8 SIMD wins speed and the epsilon holds, INT8 read path is production-ready (pending WS11 recall gate). Record decision in `docs/baselines/2026-08-04-ws4-neural-quant-readpath.md`.
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(generation): INT8 neural read-path parity + speed gate"
```

---

## Task 13 (TBD resolution): Sparse TF-IDF + symbol interning

**Resolves spec §4.6 + §4.7.**

- [ ] **Step 1: Measure dense-vs-sparse TF-IDF** — instrument current dense TF-IDF storage size on this repo; build sparse equivalent; measure size delta + retrieval-ranking equivalence (same top-10 results on a fixed query suite). Write `docs/baselines/2026-08-04-ws4-tfidf-sparse.md`.
- [ ] **Step 2: DECIDE sparse** — adopt sparse iff size shrinks ≥30% AND ranking is identical on the suite. Record decision.
- [ ] **Step 3: Measure symbol-string duplication** — count repeated `String` allocations across PDG node ids/paths/names on this repo; estimate interning savings.
- [ ] **Step 4: DECIDE interning** — adopt mmap'd interning table iff duplication ≥20% of PDG blob size. Record in `docs/baselines/2026-08-04-ws4-symbol-interning.md`. Also resolves §10 item 2 (whether `symbols` is a separate blob).
- [ ] **Step 5: Implement whichever passed their gate** (may be both, one, or neither).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(generation): sparse TF-IDF + symbol interning (gated by measurement)"
```

---

## Task 14: Wire read-path handlers to leased mmap generations

**Files:** search/symbol/deep-analyze handlers in `src/cli/mcp/` (the consumers of `search_engine`/`pdg`).

- [ ] **Step 1: Write failing test** — a search tool call acquires a `GenerationLease`, reads from the mmap generation, returns identical results to the current heap-mirror path (bit-for-bit equivalence test on a fixed corpus — anti-cheat §2.1).
- [ ] **Step 2-4:** TDD. Feature-flagged (`generation-readers`) so legacy heap-mirror path stays default until WS12 rollout; both produce identical output.
- [ ] **Step 5: No-stall test** — start a long index, fire a search mid-index, assert it returns immediately (≤ cold-query + epsilon) reading the prior generation.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(generation): read-path handlers use leased mmap generations (feature-flagged)"
```

---

## Task 15: Full validation + acceptance

- [ ] **Step 1: Validation suite**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Footprint gate** (this repo) — before/after `du -sh .leindex/`. Target: ≤ 60 MiB pre-quantization. Record `docs/baselines/2026-08-04-ws4-footprint.json`.
- [ ] **Step 3: No-2×-RAM gate** — WS1 memcheck: incremental one-file reindex; `/proc/<pid>/smaps` shows shared-pages double-counted, not resident pages doubled.
- [ ] **Step 4: Crash gate** — spec §13 scenario 9: kill -9 at every publication phase; last-good generation serves.
- [ ] **Step 5: 100-reindex monotonic-growth gate** — spec §13 scenario 13: no growth in CAS or jobs.
- [ ] **Step 6: TBD-completeness audit** — verify Tasks 11/12/13 each have a recorded decision file. Plan is incomplete if any is missing.
- [ ] **Step 7: Commit baselines**

```bash
git add docs/baselines/2026-08-04-ws4-*.json docs/baselines/2026-08-04-ws4-*.md
git commit -m "docs(ws4): record footprint/no-stall/crash/TBD-decision evidence"
```

---

## Handoff Summary (fill after execution)

```text
Workstream: WS4
Revision: 1.0
Invariant status: anti-cheat §2.1 — 4.1-4.4 are pure overhead removal (bit-equivalent read path); quantization (4.5) gated by WS11; mmap resident pages counted in ledgers
Files changed: src/storage/{cas,generation}/*, src/cli/{registry,leindex/indexing/mod,cleanup}.rs, read-path handlers
Tests run/results: [fill]
Benchmark artifacts: docs/baselines/2026-08-04-ws4-{footprint,no-stall,crash,refcount-store,neural-quant-readpath,tfidf-sparse,symbol-interning,migration}.*
Before footprint: 2.5 GiB (this repo, 419 files) | After: [fill, target ≤60 MiB]
Before quality: [baseline] | After: [must be bit-equivalent for 4.1-4.4; WS11-gated for 4.5]
TBD resolutions: Task 11 (refcount store)=[fill], Task 12 (INT8 readpath)=[fill], Task 13 (sparse/interning)=[fill]
Unverified assumptions: [fill]
Known risks: migration sweep touches user data — ship behind a backup warning
Rollback: feature flag `generation-readers` OFF = legacy heap-mirror + full-copy generations
Next workstream prerequisites: WS5 scheduler needs lease API; WS6-9 streaming writer needs CAS+manifest format; WS11 needs INT8 read path + quantized format
```

---

## Spec-coverage check

| Spec § | Task |
|---|---|
| §3.1 CAS blob format | 1 |
| §3.2 manifest format | 3 |
| §3.3 zero-copy reader (SIMD) | 5, 6 |
| §3.4 atomic publication | 7 |
| §3.5 GenerationLease | 4 |
| §3.6 retention (gens/CAS/jobs) | 9 |
| §4.1 CAS content-addressing | 1, 8 |
| §4.2 byte-bounded jobs + no per-job pdg.bin | 9 |
| §4.3 tight gen retention | 9 |
| §4.4 VACUUM-normalize | 8 |
| §4.5 quantized neural (WS11-gated) | 12 |
| §4.6 sparse TF-IDF (TBD→resolved) | 13 |
| §4.7 symbol interning (TBD→resolved) | 13 |
| §5 DB copy-into-CAS + hybrid write | 8 |
| §6 quantized-native SIMD | 5, 12 |
| §7 concurrency (readers skip Mutex) | 14 |
| §8 migration | 10 |
| §9 acceptance gates | 15 |
| §10 open items (refcount store, symbols blob) | 11, 13 |

## Forbidden shortcuts (anti-cheat §2.1)

- Do NOT quantize (4.5) without WS11 quality evidence — INT8 read path ships but production precision is WS11-gated.
- Do NOT exclude mmap resident pages from memory ledgers (§2.1 #7).
- Do NOT reduce indexed scope to shrink footprint (§2.1 #2).
- Do NOT weaken bit-equivalence of the 4.1–4.4 read path — it must match today's heap-mirror output.
- Do NOT leave a TBD unresolved at plan completion (user mandate).
