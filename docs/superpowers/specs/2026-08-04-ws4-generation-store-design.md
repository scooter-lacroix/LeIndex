# WS4: Immutable mmap Generation Store + Radical Size Reduction

**Date:** 2026-08-04
**Status:** Approved design (pending user review)
**Scope:** Generation storage format, content-addressed blob store, mmap readers, generation leases + atomic publication, byte-bounded retention, and aggressive artifact-size reduction. This is the WS4 portion of sub-plan 3 (`2026-08-04-ws4-5-registry-scheduler.md`).
**Parent spec:** `docs/superpowers/specs/2026-08-04-leindex-resource-architecture-design.md` (§4.3, §6.5, §6.7, §10, §12.2).

## 1. Problem

Spec §4.3 requires concurrent reads during indexing (queries must not stall for the index wall-time). Separately, **measured production disk usage is catastrophic**: a 419-file project occupies 2.5 GiB when its real index is 145 MiB (17× overhead); large projects reach 150 GiB+. Three distinct root causes were verified on this repository's own `.leindex/`:

1. **Jobs never byte-bounded (worst):** 114 historical jobs accumulate 2.0 GiB; each persists a full `pdg.bin` (15.9 MiB) duplicated per job. Spec §10.2 forbids count-only job limits.
2. **Generations are full copies, zero content-addressing:** sha256 proves adjacent generations hold byte-identical `leindex.db` files yet store them as separate 65 MiB copies; `.bin` files (embeddings/neural/search) are copied verbatim into every generation.
3. **Generation retention too loose:** `CURRENT=604` but generations 601, 602, 603 retained though not current, not leased, not rollback-adjacent.

Additionally, the in-process `ProjectRwLock` is a `Mutex<LeIndex>` (LeIndex is `!Sync`), and read-path tools go through the in-memory `search_engine`/`pdg` fields — so even a SQLite connection pool would not yield concurrent reads during indexing.

## 2. Goals and Non-Goals

**Goals:**
- Queries do not stall during indexing (atomic generation publication + lease).
- No 2× RAM during incremental reindex (mmap generations share OS pages by content hash).
- 10–100× disk-footprint reduction across project sizes.
- Zero retrieval-quality regression from overhead removal; quantization gated by WS11.

**Non-goals (deferred):**
- Cross-core read parallelism (true `Sync` LeIndex). Not required: the goal is no-stall, not parallelism.
- Streaming per-stage writers (WS6-9). This spec defines the *format* and *reader*; the writer may initially whole-serialize-then-dedup and is converted stage-by-stage in WS6-9.
- Quantization model selection (WS11 bake-off). This spec defines the *format* that supports quantized payloads and the read path that consumes them; the actual precision is chosen by WS11 quality gates.

## 3. Architecture

A generation is no longer a directory of copied files. It is a **manifest** referencing **content-addressed blobs** in a CAS. Readers mmap blobs zero-copy; the writer streams new blobs and atomically publishes a new manifest.

```
.leindex/
├── cas/                       # content-addressed blob store
│   ├── <2-hex-prefix>/
│   │   └── <blake3>           # immutable, refcounted, mmap'd
│   └── .staging/              # partial blobs during write
├── generations/
│   └── <N>/
│       └── manifest           # layer→hash map + identity + fingerprints
├── jobs/                      # byte-bounded; no pdg.bin
└── CURRENT                    # symlink/pointer to current generation number
```

### 3.1 CAS blob format

```
magic:          b"LIDX-BLB1"        # spec §12.2 explicit magic/version/checksum
version:        u16
content_hash:   blake3             # re-verified on every mmap open
payload_len:    u64
<compact payload>                  # layout defined per layer type
```

Blobs are immutable. Path = `.leindex/cas/<hash[0..2]>/<hash>`. Refcounted; a blob is GC-eligible only when no retained generation references it and refcount = 0.

### 3.2 Generation manifest format

```
magic:              b"LIDX-GEN1"
version:            u16
generation:         u64
model_identity:     { model_digest, tokenizer_digest, prompt_role_and_version,
                      pooling_and_normalization, output_dimensions }   # spec §6.5 cache key
graph_fingerprint:  blake3          # spec §6.7 validation
search_fingerprint: blake3
layers: {
    db:      <cas_hash>,            # SQLite snapshot, VACUUM-normalized
    tfidf:   <cas_hash>,            # sparse rows
    neural:  <cas_hash>,            # quantized vectors + {count, dim, dtype} header
    pdg:     <cas_hash>,            # node/edge segments + interned symbol table
    symbols: <cas_hash>,            # global symbol lookup table
}
```

### 3.3 Reader path (zero-copy, no heap mirror)

1. Open `generations/<N>/manifest`; verify `version` and each blob's `content_hash` (spec §6.7).
2. `mmap` each referenced blob read-only; increment CAS refcount.
3. **Neural:** interpret payload as `{count, dim, dtype: f32|i8|q4, scale, zero_point}` + flat array. Compute dot-products **directly on the quantized domain via SIMD**; dequantize only the final top-k for fusion/reranking. No `HashMap<String, Vec<f32>>` (kills spec §3.7 heap-mirror anti-pattern).
4. **TF-IDF / PDG / symbols:** mmap'd compact segments; symbol strings interned into a mmap'd table (spec §6.3).
5. Hold a `GenerationLease` for the read's duration; drop decrements refcounts.

The reader **never** acquires the writer's `LeIndex` Mutex. It reads the published generation exclusively.

### 3.4 Writer path (atomic publication, crash-safe)

1. Stream new blobs into `.leindex/cas/.staging/<hash>.partial` (WS6-9 makes this per-stage streaming; initial implementation may whole-serialize then dedup).
2. `fsync` blob, `rename` to `.leindex/cas/<prefix>/<hash>` (atomic). **Hash collision = already present = dedup win** → delete staging copy, reference existing blob.
3. Write `generations/<N>/manifest.partial`, `fsync`, `rename` → `manifest` (atomic publication, spec §6.7, §11.1).
4. Atomically update `CURRENT` → `<N>`.
5. Run retention sweep (§3.6).

Crash at any step → `manifest` absent or `CURRENT` unchanged → startup sees last-good generation; `.staging` swept on next start.

### 3.5 GenerationLease

```rust
pub struct GenerationLease { store: Arc<CasStore>, gen: u64 }
impl Drop for GenerationLease { /* decrement blob refcounts; queue GC if zero */ }
```

A read-path tool acquires `lease = registry.lease_generation(project)` before reading and drops it after. Old-generation blobs cannot be GC'd while any lease lives → in-flight reads stay valid (spec §11.4). The `ProjectRegistry` (existing, in the daemon after WS3) hands out leases against the currently-published manifest.

### 3.6 Retention (byte-bounded everywhere)

- **Generations:** keep `current` + `previous` + any with `refcount > 0`. Default cap `max_generations = 2` (current + one rollback). Delete the rest on publication.
- **CAS blobs:** refcounted. GC-eligible when no retained generation references it and `refcount = 0`. GC runs under a maintenance budget (spec §10.2).
- **Jobs:** byte-ceiling per project, default `job_bytes_max = 128 MiB` (configurable). Over budget → delete oldest completed jobs first. **Completed jobs whose generation is published are deleted immediately on publication** (zero resume value). `pdg.bin` no longer per-job — referenced via CAS once published; incomplete jobs keep their own until publication.
- New `leindex retention --report` surfaces generation count, CAS bytes, job bytes, dedup ratio, GC candidates.

## 4. Size-Reduction Strategy (ordered by impact)

| # | Lever | Mechanism | Effect on this repo |
|---|---|---|---|
| 4.1 | CAS content-addressing | Identical blobs collapse to one (sha256-evidenced for DBs) | 488 MiB → ~145 MiB |
| 4.2 | Byte-bounded jobs + drop per-job `pdg.bin` | Jobs hold only resume checkpoints; completed jobs deleted on publish | 2.0 GiB → ~50 MiB |
| 4.3 | Tight generation retention | current + 1 previous + leased only | removes stale gens |
| 4.4 | DB VACUUM-normalize before CAS hash | Maximizes dedup across logical-identical DBs | raises dedup ratio |
| 4.5 | Quantized neural vectors (INT8/Q4) | **Behind WS11 quality gates** (anti-cheat §2.1 #4) | 145 MiB → est. 10–25 MiB |
| 4.6 | Sparse TF-IDF rows | Where equivalence-tested (spec §6.4) | TBD |
| 4.7 | Interned symbol strings | mmap'd table IDs (spec §6.3) | TBD |

**Combined target (this repo):** 2.5 GiB → est. 30–60 MiB before quantization (40–80×); 10–25 MiB after (4.5, WS11-gated). Large projects scale proportionally because the overhead multipliers (jobs × N, generations × full-copy) are eliminated rather than scaled.

4.1–4.4 are pure overhead removal with **no semantic change** and ship in WS4. 4.5–4.7 ship only with WS11 quality-gate evidence.

## 5. Database Layer Decision (resolved)

**Copy-into-CAS with hash-dedup** (chosen over WAL-snapshot). Rationale: the user's own sha256 evidence proves adjacent generations hold byte-identical DBs, which CAS dedup collapses to one blob; the only copies are genuinely-distinct DB states (the information-theoretic minimum). WAL-snapshot rejected because it carries per-generation `-wal`/`-shm` (the exact bloat observed) and cannot form a clean immutable mmap blob.

**Hybrid write path (optimization-now, not later):** WAL during *write* (fast incremental), then `checkpoint` + `VACUUM` (deterministic page normalization) + copy-into-CAS on *publish* (clean immutable read). No `-wal`/`-shm` retained in generation dirs. VACUUM-before-hash is mandatory in WS4 to maximize dedup.

## 6. Quantized Read Path (resolved)

**Quantized-native SIMD dot-products** (chosen over dequantize-on-read). Keep vectors INT8/Q4 in the mmap blob; compute dot-products directly on quantized data via SIMD (INT8 dot ~4× faster than f32); dequantize **only the final top-k** for fusion/reranking. Wins on both RSS (no per-query full-matrix dequant) and speed (SIMD INT8 > f32). Dequantize-on-read is the fallback only if the scoring path cannot be expressed on the quantized domain (quality-gated in WS11). Actual precision (f32 baseline vs INT8 vs Q4) chosen by WS11 bake-off; the format supports all.

## 7. Read/Write Concurrency

- Writer holds the existing `ProjectWriteLock` (cross-process flock) and the `LeIndex` Mutex only for the write path.
- Readers acquire a `GenerationLease` and mmap the published generation; they do **not** touch the `LeIndex` Mutex.
- `LeIndex` remains `!Sync`. No connection pool. No `Sync` migration. (Deferred indefinitely unless profiling shows cross-core read parallelism is needed — it is not required for the no-stall goal.)

## 8. Migration

One-time sweep on first run under new code:
1. Collapse existing `generations/*/{leindex.db, *.bin}` into CAS with dedup.
2. Delete non-current/previous generations.
3. Enforce job byte-ceiling; delete completed jobs whose generation is published.
4. Rewrite a manifest for the current generation; update `CURRENT`.

Reclaim is immediate and large (this repo: ~2.4 GiB → ~150 MiB before any quantization). The sweep is idempotent and crash-safe (writes new manifest beside old layout; swaps `CURRENT` last).

## 9. Acceptance Gates

- No-stall: query latency during indexing ≤ cold-query latency + bounded small epsilon (measured by WS1 memcheck contention phase).
- No 2× RAM: incremental reindex of one file does not double resident mmap pages (CAS hash-sharing verified via `/proc/<pid>/smaps` before/after).
- Footprint: this repo `.leindex/` ≤ 60 MiB before quantization, ≤ 25 MiB after (WS11-gated).
- Crash safety: kill -9 during every publication phase → last-good generation serves reads (spec §13 scenario 9).
- Retention: after 100 reindexes, no monotonic growth in CAS or jobs (spec §13 scenario 13).
- Quality: identical retrieval metrics vs baseline for the 4.1–4.4 overhead-removal changes (no semantic change → must be bit-for-bit equivalent on the read path).

## 10. Open Items

- Exact CAS refcount storage (separate `cas/refs.db` SQLite vs sidecar files) — pick lowest-complexity in implementation.
- Whether `symbols` layer is a separate blob or folded into `pdg` — decide during implementation based on access patterns.
- WS5 (scheduler) is a separate design within SP3; this spec does not cover it.

## 11. Anti-Cheat Compliance (spec §2.1)

- 4.1–4.4 remove pure overhead (duplicate bytes, stale generations, retained completed jobs) with **no change to indexed scope, node count, language coverage, embedding precision, or candidate count**. Read-path output is bit-for-bit equivalent to today's heap-mirror reads.
- 4.5–4.7 (quantization, sparse TF-IDF) ship **only** behind WS11 quality-gate evidence (anti-cheat §2.1 #4). No precision reduction reaches production without passing fused-retrieval equivalence.
- No work is moved to swap, GPU-without-counting, remote service, or mmap-pages-excluded-from-RSS accounting (anti-cheat §2.1 #5–8). Mmap resident pages are counted in all memory ledgers.
