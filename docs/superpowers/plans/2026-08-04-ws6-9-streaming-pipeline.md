# WS6-9: Streaming Index Pipeline

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Convert every indexing stage from "materialize the whole corpus into `IndexPipelineState` heap fields, then persist" to "stream bounded chunks directly into CAS staged blobs via WS4's `GenerationWriter`," stepping through WS5's `BoundedJob`. End state: only one bounded input batch and one bounded output batch on the heap at any time per stage (spec §6.6).

**Architecture:** Each stage becomes a streaming pass: read bounded chunk → process → write directly to CAS staging → drop source/syntax-tree/vectors before next chunk. `IndexPipelineState` shrinks to compact metadata + checkpoint references (no `Vec<ParsingResult>`, no `Option<PDG>` held whole, no `Vec<(String,Vec<f32>)>`).

**Spec refs:** §6 (Streaming Index Architecture), §3.4–3.8 (root-cause anti-patterns).
**Depends on:** SP3a (CAS + GenerationWriter + manifest), SP3b (BoundedJob stepping).
**Tech Stack:** Rust, existing tree-sitter parsers, existing TF-IDF/embedding math.

**Existing infra (reuse, don't reinvent):**
- Tree-sitter parsers (`src/parse/*.rs`), `parse::parallel::ParsingResult`.
- TF-IDF math (`src/cli/index_builder/tfidf.rs`).
- Fragment chunker (`src/cli/index_builder/fragment/chunker.rs`).
- Checkpoints: `ScanCheckpoint`, `ParseCheckpoint`, `PdgCheckpoint`, `LexicalCheckpoint`, `NeuralCheckpoint`.
- `HybridEmbedder` (`src/cli/index_builder/hybrid.rs`).

**Anti-patterns to eliminate (verified in code):**
- `IndexPipelineState.source_files_with_hashes: Vec<(PathBuf,String)>` + `source_file_hashes: HashMap<String,String>` + `parsing_results: Vec<ParsingResult>` + `pdg: Option<PDG>` + `admitted_node_ids: HashSet<String>` — all heap-materialized across phases (§3.4).
- `FileReadCache { HashMap<PathBuf, Arc<Vec<u8>>> }` — source-body cache retained across phases (§6.1 says eliminate).
- `enrich_neural_embeddings(...) -> Vec<(String, Vec<f32>)>` — full row accumulation before persist (§3.6, §6.6).

---

## File Structure

| File | Responsibility |
|---|---|
| `src/cli/leindex/indexing/streaming/scan.rs` | Streaming scan (no source cache) |
| `src/cli/leindex/indexing/streaming/parse.rs` | Bounded parse chunks, per-file persist |
| `src/cli/leindex/indexing/streaming/pdg.rs` | Per-file PDG fragments → CAS adjacency |
| `src/cli/leindex/indexing/streaming/tfidf.rs` | Two-pass external-memory TF-IDF |
| `src/cli/leindex/indexing/streaming/fragment.rs` | Content-hash fragment embeddings (cache probe + direct write) |
| `src/cli/leindex/indexing/streaming/neural.rs` | `NeuralRowWriter` direct staged writes |
| `src/cli/leindex/indexing/mod.rs` | `IndexPipelineState` slim-down; phase loop → stepped |
| `src/cli/index_builder/mod.rs` | Remove/repurpose `FileReadCache` |

---

## Task 1: Streaming scan (eliminate source-body caching)

**Files:** `streaming/scan.rs`, `src/cli/index_builder/mod.rs`

- [ ] **Step 1: Write failing test** — scan walks files lazily, hashes via fixed 64KiB buffer, writes `(path, hash, size, lang, mtime)` records to a CAS-staged scan blob incrementally; asserts **no source body retained** (RSS flat across scan of N files; `FileReadCache` empty).

```rust
pub fn stream_scan(root: &Path, writer: &mut impl ScanRecordWriter, budget: WorkBudget) -> Result<ScanStats> { todo!() }
```

- [ ] **Step 2-4:** TDD. Diff via sorted merge or indexed DB lookup against prior scan; emit bounded changed/deleted work units.
- [ ] **Step 5: RSS assertion** — scan a 1000-file fixture; RSS delta ≤ one chunk size (no source retained). Reuse WS1 sampler.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(indexing): streaming scan, no source-body caching"
```

---

## Task 2: Streaming parse (bounded chunks, drop tree per file)

**Files:** `streaming/parse.rs`

- [ ] **Step 1: Write failing test** — parse bounded by file count AND aggregate bytes; per-file signatures persisted to CAS immediately; syntax tree + source buffer dropped before next file (assert via RSS); oversized files routed to a single-file lane.

- [ ] **Step 2-4:** TDD. A successful parse checkpoint references durable per-file CAS artifacts, not a phase-wide `Vec<ParsingResult>`.
- [ ] **Step 5: RSS assertion** — parse 500 files; RSS stays bounded (no accumulation).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(indexing): streaming parse with per-file persist + tree drop"
```

---

## Task 3: Compact PDG persistence + mmap adjacency

**Files:** `streaming/pdg.rs`

- [ ] **Step 1: Write failing test** — build per-file graph fragments; resolve cross-file edges from compact symbol indices in bounded batches; write node/edge segments + interned symbol table to CAS blobs; assert no whole-PDG clone/serialize cycle.

- [ ] **Step 2-4:** TDD per spec §6.3: stable node IDs, interned strings (tie into WS4 Task 13 interning decision), per-file segments, global symbol lookup table, external edge resolution in bounded batches.
- [ ] **Step 5: Verify no clone** — instrumentation: no `PDG::clone()` during merge (spec §6.3 anti-pattern).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(indexing): compact PDG persistence to CAS, no whole-PDG clone"
```

---

## Task 4: Streaming TF-IDF (two-pass external-memory)

**Files:** `streaming/tfidf.rs`

- [ ] **Step 1: Write failing test** — pass 1 streams admitted docs, updates document frequencies; freeze vocab/IDF; pass 2 streams docs again, tokenizes, writes each row directly to CAS-staged vector storage. Assert no corpus materialization (no `Vec<Vec<f32>>` of all docs).

- [ ] **Step 2-4:** TDD per spec §6.4. Honor WS4 Task 13 sparse-vs-dense decision (write whichever that task selected).
- [ ] **Step 5: Equivalence** — top-K ranking on a fixed query suite identical to current dense implementation (or, if sparse was chosen, equivalent per Task 13's gate).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(indexing): streaming two-pass TF-IDF, direct CAS writes"
```

---

## Task 5: Streaming content-hash/fragment embeddings

**Files:** `streaming/fragment.rs`

- [ ] **Step 1: Write failing test** — stream fragments from changed files; compute content hash (spec §6.5 cache key: model+tokenizer+prompt+pooling+dim+content_hash); probe persistent global cache; queue misses under token/byte budget; write returned vectors directly to CAS-staged rows; **no `HashMap<String, Vec<f32>>` load of all prior fragments** (spec §6.5 anti-pattern — use indexed metadata + mmap rows).

- [ ] **Step 2-4:** TDD. Probe against the global embedding cache (WS10 builds the cross-project cache; here probe the project-scoped portion).
- [ ] **Step 5: RSS assertion** — fragment sync RSS independent of total fragment count.
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(indexing): streaming fragment embeddings, cache-probe + direct write"
```

---

## Task 6: Streaming neural enrichment (NeuralRowWriter)

**Files:** `streaming/neural.rs`, `src/cli/index_builder/mod.rs`

**This is the headline fix for spec §6.6.** Replace `enrich_neural_embeddings(...) -> Vec<(String, Vec<f32>)>`.

- [ ] **Step 1: Write failing test** — new signature writes directly to staged storage; only one bounded input batch + one bounded output batch exist on heap; final rows go to CAS staging, never a phase-wide `Vec`.

```rust
pub trait NeuralRowWriter { fn write_row(&mut self, node_id: &str, vec: &[f32]) -> Result<()>; }
pub fn enrich_neural_embeddings<I, W>(source: &mut I, embedder: &HybridEmbedder, writer: &mut W, budget: BatchBudget) -> Result<NeuralStats>
where I: Iterator<Item = Result<NeuralInput>>, W: NeuralRowWriter { todo!() }

pub struct BatchBudget {
    pub max_texts: usize, pub max_utf8_bytes: usize, pub max_estimated_tokens: usize,
    pub max_seq_len: usize, pub max_output_vector_bytes: usize, pub provider_profile: ProviderProfile,
}
```

- [ ] **Step 2-4:** TDD. `batch_size=500` count-only is insufficient (§6.6) — enforce byte/token/vector budgets. Cancellation between batches (tie to WS5 yield points).
- [ ] **Step 5: RSS assertion** — neural phase RSS independent of corpus node count.
- [ ] **Step 6: Equivalence** — output vectors bit-identical to current `Vec<(String,Vec<f32>)>` path (same model, same batching order).
- [ ] **Step 7: Commit**

```bash
git commit -m "feat(indexing): streaming neural enrichment via NeuralRowWriter (kills Vec accumulation)"
```

---

## Task 7: Slim IndexPipelineState + stepped phase loop

**Files:** `src/cli/leindex/indexing/mod.rs`

- [ ] **Step 1: Write failing test** — `IndexPipelineState` no longer holds `parsing_results: Vec<_>`, `pdg: Option<PDG>` (whole), or source-hash collections; it holds compact metadata + checkpoint references. The phase loop runs each streaming stage via WS5 `BoundedJob::step`.

- [ ] **Step 2-4:** TDD. Each stage streams (Tasks 1-6); the state struct shrinks to durable references.
- [ ] **Step 5: Resume test** — kill mid-pipeline; restart resumes at last checkpoint (§11.2).
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(indexing): slim IndexPipelineState + stepped streaming phases"
```

---

## Task 8: Remove FileReadCache cross-phase retention

**Files:** `src/cli/index_builder/mod.rs`

- [ ] **Step 1:** Audit `FileReadCache` callers (`incremental_reindex_from_watcher`, `run_scan`, `run_neural`, `index_nodes_with_embedder_inner`). Each must re-read per chunk rather than retain across phases.
- [ ] **Step 2:** Either remove `FileReadCache` or reduce it to a per-chunk scratch buffer (dropped each chunk).
- [ ] **Step 3: RSS assertion** — no source-body accumulation across phases (§6.1).
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(indexing): eliminate cross-phase source-body caching"
```

---

## Task 9: Validation + streaming-RSS measurement

- [ ] **Step 1: Validation suite.**
- [ ] **Step 2: Per-stage RSS independence** (WS1 memcheck) — for each stage, RSS delta ≤ chunk size, independent of corpus size. Record `docs/baselines/2026-08-04-ws6-9-streaming-rss.json`.
- [ ] **Step 3: Index wall-time** — no regression vs baseline (§16 performance gate).
- [ ] **Step 4: Quality equivalence** — retrieval metrics identical to pre-streaming (streaming is a refactor, not a semantic change; anti-cheat §2.1).
- [ ] **Step 5: Commit**

```bash
git commit -m "docs(ws6-9): record streaming RSS + wall-time + equivalence"
```

---

## Handoff Summary (fill after execution)

```text
Workstream: WS6-9
Revision: 1.0
Invariant status: streaming is a refactor — retrieval output bit-equivalent (anti-cheat §2.1); no scope/file/node reduction
Files changed: src/cli/leindex/indexing/streaming/*, src/cli/leindex/indexing/mod.rs, src/cli/index_builder/mod.rs
Tests run/results: [fill]
Benchmark artifacts: docs/baselines/2026-08-04-ws6-9-streaming-rss.json
Before: enrich_neural_embeddings returned Vec<(String,Vec<f32>)>; IndexPipelineState held whole PDG + parsing_results | After: [fill]
TBD resolutions: none inline (consumes WS4 Task 13 sparse/interning decisions)
Unverified assumptions: [fill]
Known risks: large refactor — feature-flag each stage
Rollback: per-stage feature flags
Next: SP5 (WS10) global embedding cache consumes streaming fragment writer
```

## Spec-coverage check (§6)

| §6 stage | Task |
|---|---|
| 6.1 scan (no source cache) | 1, 8 |
| 6.2 parse (bounded, drop tree) | 2 |
| 6.3 PDG (compact, interned, mmap) | 3 |
| 6.4 TF-IDF (two-pass external) | 4 |
| 6.5 fragment (cache-probe, direct write) | 5 |
| 6.6 neural (NeuralRowWriter, kill Vec) | 6 |
| 6.7 publication | WS4 Task 7 |
| §3.4-3.8 anti-patterns | 1-8 |

## Forbidden shortcuts (anti-cheat §2.1)

- Do NOT index fewer files/nodes/languages to shrink memory (§2.1 #2).
- Do NOT alter ranking during the streaming refactor (§2.1 — must be bit-equivalent).
- Do NOT retain a heap mirror "just in case" — the CAS+mmap generation is the source of truth.
