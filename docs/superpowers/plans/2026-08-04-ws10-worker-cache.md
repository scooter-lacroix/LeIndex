# WS10: Shared Embedding Worker + Global Embedding Cache

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Add a user-level **content-addressed global embedding cache** that deduplicates identical source across projects/worktrees by full model+content identity, and harden the existing `leindex-embed` worker with token-aware bounded batching, model-digest reporting, and cross-batch cancellation.

**Architecture:** The worker (`src/bin/leindex-embed.rs` → `src/embed/worker_main.rs`) already exists as a single shared process (post-WS3, one per daemon). WS10 adds: (1) a `GlobalEmbeddingCache` keyed by `(model_digest, tokenizer_digest, prompt_role_and_version, pooling_and_normalization, output_dimensions, content_hash)` (spec §6.5), living at user-level (e.g. `~/.leindex/embed-cache/`); (2) worker batching that respects `BatchBudget` (bytes/tokens, not count-only); (3) model/tokenizer/config digest reporting in health.

**Spec refs:** §4.4 (shared worker), §6.5 (content-hash cache key), §10.1 (global cache requirements), §8.3 (GPU policy).
**Depends on:** SP2 (daemon owns one worker client), SP3a (CAS primitives reusable), SP4 (streaming fragment writer probes the cache).
**Tech Stack:** Rust, existing ONNX runtime, existing `embed::protocol`.

**Existing infra (reuse, don't reinvent):**
- `leindex-embed` binary + `worker_main.rs` (socket accept loop, idle timeout, `PR_SET_PDEATHSIG`).
- `embed::protocol` frames (`EmbedRequest`/`EmbedResponse`/`HealthRequest`/`HealthResponse`).
- `embed::runtime` (`RuntimeConfig`, `low_memory_refusal`).
- `embed::runtime_env` (`default_ort_threads`, batch/seq envs).
- WS4 `CasStore` — reuse for the cache's fixed-layout vector rows.

**What does NOT exist:** global cross-project embedding cache; token/byte-aware batching (current is count-only `NEURAL_IPC_BATCH=256`); model-digest reporting in health; explicit content-hash cache-probe RPC.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/embed/cache/mod.rs` | `GlobalEmbeddingCache` (open/probe/put/gc) |
| `src/embed/cache/key.rs` | `CacheKey` (the §6.5 6-tuple) + digest computation |
| `src/embed/cache/store.rs` | Fixed-layout mmap vector rows + metadata (reuse WS4 CAS) |
| `src/embed/protocol.rs` | Add `CacheProbe`/`CacheProbeResponse` msg types + digest fields to `HealthResponse` |
| `src/embed/batching.rs` | `BatchBudget` token/byte estimator + bucketing |
| `src/embed/worker_main.rs` | Wire probe → batch-miss → put loop; cross-batch cancellation |

---

## Task 1: CacheKey (§6.5 6-tuple) + digest computation

**Files:** `src/embed/cache/key.rs`, `src/embed/cache/mod.rs`

- [ ] **Step 1: Write failing test** — same model+content → same key; any field differ → different key; content_hash of identical text equal across projects.

```rust
pub struct CacheKey { pub model_digest: [u8;32], pub tokenizer_digest: [u8;32],
    pub prompt_role_and_version: u32, pub pooling: Pooling, pub normalization: Normalization,
    pub output_dimensions: u32, pub content_hash: [u8;32] }
impl CacheKey { pub fn fingerprint(&self) -> [u8;32] { todo!() } }
```

- [ ] **Step 2-4:** TDD. Digests: blake3 of model bytes, tokenizer bytes. Model upgrade → new namespace (§10.1 "no accidental mixed vectors").
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(embed-cache): §6.5 CacheKey 6-tuple + digest computation"
```

---

## Task 2: GlobalEmbeddingCache (probe/put/gc, mmap rows)

**Files:** `src/embed/cache/store.rs`, `src/embed/cache/mod.rs`

- [ ] **Step 1: Write failing test** — `probe(keys) -> (hits, misses)`; `put(key, vec)`; fixed-layout mmap rows; corruption detection (re-hash on read); byte-budgeted compaction removes unreferenced rows; cross-project dedup (same content_hash under different projects → one row).

- [ ] **Step 2-4:** TDD. Reuse WS4 `CasStore` for row storage where format permits; metadata table (SQLite or sidecar) maps key→row. Project-generation references tracked so GC doesn't drop live rows (§10.1).
- [ ] **Step 5: Privacy check** — no source text stored after hashing unless debug flag (§10.1).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(embed-cache): global mmap cache with probe/put/gc + cross-project dedup"
```

---

## Task 3: BatchBudget + token/byte bucketing (replaces count-only)

**Files:** `src/embed/batching.rs`

- [ ] **Step 1: Write failing test** — `BatchBudget` caps texts, UTF-8 bytes, estimated tokens, max seq len, output vector bytes; bucketing groups similar-length texts to avoid pathological padding (§8.3).

- [ ] **Step 2-4:** TDD. This is the worker-side analog of WS6-9 Task 6's writer budget; share the type. Replaces `NEURAL_IPC_BATCH=256` count-only (§6.6 "500 tiny symbols and 500 long docs have radically different shapes").
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(embed): BatchBudget token/byte bucketing (replaces count-only)"
```

---

## Task 4: Protocol additions (CacheProbe + health digests)

**Files:** `src/embed/protocol.rs`

- [ ] **Step 1: Write failing test** — new `MsgType::CacheProbe`/`CacheProbeResponse`; `HealthResponse` carries model/tokenizer/config digests + measured memory.
- [ ] **Step 2-4:** TDD. Backward-compatible (new fields `#[serde(default)]`).
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(embed-protocol): CacheProbe RPC + model digests in HealthResponse"
```

---

## Task 5: Worker probe → batch-miss → put loop + cross-batch cancel

**Files:** `src/embed/worker_main.rs`

- [ ] **Step 1: Write failing test** — worker receives texts + keys; probes cache; embeds only misses under `BatchBudget`; writes hits+misses back in input order (ordering preserved, §protocol); cancellation flag checked between batches.

- [ ] **Step 2-4:** TDD. The daemon-side client (SP4 streaming fragment writer) sends keys; worker returns full vector set (hits from cache, misses freshly embedded).
- [ ] **Step 5: Crash isolation** — kill worker mid-batch; daemon retries the batch once (idempotent via content_hash) or fails clearly (§11.2). No duplicate workers race to recover.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(embed-worker): probe→batch-miss→put loop + cross-batch cancellation"
```

---

## Task 6: Byte-budgeted compaction + diagnostics

**Files:** `src/embed/cache/mod.rs`, `src/cli/cleanup.rs`

- [ ] **Step 1: Write failing test** — compaction reclaims rows with zero project references; `leindex retention --report` includes cache bytes + hit/miss/eviction telemetry (§10.3).
- [ ] **Step 2-4:** TDD. Every cache has byte accounting, max bytes, entry-size rejection, eviction policy, generation/model invalidation key (§10.3 — count-only prohibited).
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(embed-cache): byte-budgeted compaction + retention telemetry"
```

---

## Task 7: GPU policy + provider profile reporting

**Files:** `src/embed/worker_main.rs`, `src/embed/runtime.rs`

- [ ] **Step 1:** Verify GPU work occurs only for admitted batches (§8.3); worker idle without polling (already via idle timeout — verify). Report VRAM allocations + provider compile caches in health. Static-shape preference where coverage/latency beat dynamic (§8.3).
- [ ] **Step 2: Write failing test** — health reports provider profile + (Linux) VRAM.
- [ ] **Step 3: Commit**

```bash
git commit -m "feat(embed-worker): GPU policy + provider profile in health"
```

---

## Task 8: Validation + cache-effectiveness measurement

- [ ] **Step 1: Validation suite.**
- [ ] **Step 2: Dedup effectiveness** — index two worktrees sharing substantial content; measure cache hit ratio + bytes saved (spec §13 scenario 22). Record `docs/baselines/2026-08-04-ws10-embed-cache.json`.
- [ ] **Step 3: No quality change** — cache hits return identical vectors to fresh embeds (bit-equivalent; anti-cheat §2.1).
- [ ] **Step 4: Commit**

```bash
git commit -m "docs(ws10): record embed-cache dedup effectiveness"
```

---

## Handoff Summary (fill after execution)

```text
Workstream: WS10
Revision: 1.0
Invariant status: cache hits bit-equivalent to fresh embeds (§2.1); no source text stored post-hash (§10.1 privacy); single worker (§4.4)
Files changed: src/embed/{cache,protocol,batching,worker_main,runtime}.rs, src/cli/cleanup.rs
Tests run/results: [fill]
Benchmark artifacts: docs/baselines/2026-08-04-ws10-embed-cache.json
TBD resolutions: none inline
Unverified assumptions: [fill]
Known risks: global cache is user-level shared — corruption detection must be robust
Rollback: feature flag `global-embed-cache` OFF = per-generation embeds
Next: SP6 (WS11) selects the model/precision whose vectors populate this cache
```

## Spec-coverage check

| Spec § | Task |
|---|---|
| §4.4 shared worker (one runtime, no per-project spawn) | exists (SP2); 5 hardens |
| §6.5 cache key 6-tuple | 1 |
| §6.5 probe persistent cache, queue misses, direct write | 5 |
| §6.5 no HashMap load of all prior fragments | WS4 + 5 |
| §10.1 global, content-addressed, transactional, mmap rows, corruption detection, byte compaction, privacy, model namespace | 1, 2, 6 |
| §10.3 byte accounting + telemetry | 6 |
| §8.3 GPU policy + static-shape pref | 7 |
| §11.2 worker crash isolation, no race recovery | 5 |

## Forbidden shortcuts (anti-cheat §2.1)

- Do NOT reduce embedding dimensions/precision to fit cache (§2.1 #4) — precision is WS11-gated.
- Do NOT skip embedding nodes that miss the cache (§2.1 #2).
- Do NOT spawn a second worker for reranking (§4.4 — single worker, single runtime profile).
