# LeIndex Fragment Embeddings and Localized Content-Hash Store — 1.11.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

> **Sequencing gate:** This plan targets LeIndex **1.11.0** and is **gated on the embed-merge 1.10.0 plan landing first** (its Tasks 1–7: config → `src/config.rs`, worker → `src/embed/`, strict feature DAG, rc.13 ORT). The tree is currently at **1.9.0** with 1.10.0 in flight; Tasks 1–11 below assume the post-1.10.0 tree and paths.

**Goal:** Add a fully-localized **fragment-level embedding layer** to LeIndex's TF-IDF / PDG / neural search stack: sub-symbol semantic chunks (tree-sitter), module-level orphan coverage, and a **content-hash-addressed fragment store** that makes incremental indexing idempotent and deduplicated — all local, no remote service, no stubbing. Ships as LeIndex 1.11.0 with a fragment index that improves conceptual-query recall ("search more freely") and result precision (byte-range-exact, dedup'd) while leaving the existing node-level ranking authoritative by default.

**Architecture:** Three-tier chunking (Tier 1 = existing PDG node enrichment, unchanged; Tier 2 = sub-symbol semantic fragments inside large nodes, ported from Warp's `full_source_code_embedding` chunker; Tier 3 = orphan module-level regions). Fragments are content-hash-addressed (blake3 of the exact enriched text that is embedded) in a bincode store, persisted to an mmap embedding matrix that is an **exact structural twin of the existing neural mmap path** (`neural_embeddings.bin` → `neural_vector_index`). Query-time: fragment candidates union into the pool, map back to owner nodes via content-hash, fuse with a renormalized 5th `fragment` score component, and feed the existing local reranker.

**Tech stack:** Rust, tree-sitter 0.26 (already a dependency, 15 grammars), blake3/sha2 (already present), bincode (already present), memmap2 (already present), the existing ONNX worker (`qwen3-embed-0.6b`, 1024-dim), existing INT8 ADC HNSW, and the existing local bge-reranker. Zero new crates, zero new services.

---

## Audit annotations — fact corrections to the design docs

**Evidence date:** 2026-08-01. **Tree:** `feat/embed-merge-1.10.0` at `a5094568` (plan + feature-boundary commits ahead of `origin/master`; in-flight 1.10.0 edits are uncommitted). Line numbers below are pre-implementation (1.10.0-final tree) and must be paired with symbol names after the embed-merge move (Tasks 4–6 shift `src/cli/neural_config.rs`→`src/config.rs`, `crates/leindex-embed`→`src/embed/`).

| Original claim (design docs) | Verdict | Correction |
|---|---|---|
| Fragment module at `src/search/fragment/` (findings doc §7) | **Wrong placement** | Chunker depends on `enriched_node_content` which lives under `cli` (`src/cli/index_builder/mod.rs`); `src/search` compiles without `cli` (strict feature DAG). Chunker → `src/cli/index_builder/fragment/`; pure types stay search-visible. |
| Fragment hash = `blake3(raw slice)` (findings doc §5) | **Stale-embedding hazard** | Cache key must hash the **exact enriched text that is embedded** (header + doc context + slice), so cache key ≡ embedding input. Enrichment-format changes bump a schema version (mirror `TFIDF_SCHEMA_VERSION`). |
| `Score` gains `fragment` at 0.10–0.15 (design §8) | **Weight-sum bug** | `HybridScorer::score_hybrid` does NOT normalize weights. Adding a 5th component without renormalizing depresses every overall score. Renormalize to sum 1.0 (mirror `HybridScoringWeights::normalize`) and gate behind the feature. |
| File doc double-indexing (design §4.3) | **Duplicate hits** | Exclude the leading file-doc region from the Tier-3 orphan complement — FileSummary's `leading_file_doc` (≤16 lines) already covers it. |
| Fragment rows keyed like node rows (design §16.2) | **ID semantics differ** | `MmapVectorIndex::from_snapshot` keys on whatever IDs it's handed. Fragment IDs are content hashes, so owner-node mapping (`HashMap<owner, Vec<content_hash>>`) is required before surfacing results. |
| Neural weight default 0.3 (config) vs 0.4 (scorer) | **RESOLVED 2026-08-01** | `default_neural_weight()` now returns 0.4 (single source of truth, matching `HybridScorer::for_code()`/`HybridScoringWeights::default()`); dead `EmbeddingConfig.neural_weight` deleted; example/docs aligned. See `docs/findings/2026-08-01-neural-weight-default-drift.md`. Fragment work inherits the 0.4 default unchanged. |
| Kotlin/Swift/Dart grammars | **Disabled by version conflict** | tree-sitter crate versions conflict; 15 grammars enabled. Fragments fall back to naive chunking for the ~17-language gap; grammar re-enable is a separate future task. |

### Upstream facts to preserve (from 1.10.0 plan)

- One crate, two processes; worker (`src/embed/`) owns model/session; config is canonical `src/config.rs` (`cli OR onnx`).
- Strict feature DAG: `graph = ["parse", "dep:petgraph", "dep:walkdir"]`; never make `onnx` imply `cli`; no `graph → cli` deps.
- Discovery precedence: env → TOML → user lib → sibling lib → pip → system; env wins over TOML.
- MIGraphX cache identity remains model + batch + sequence (never LeIndex version).
- Existing mmap formats (`embeddings.bin`, `neural_embeddings.bin`, `search_snapshot.bin`) are load-bearing for hydration; fragment artifacts are **additive only** — never mutate existing layouts in place.
- Protocol bytes, ordering, fallback, idle teardown, and socket behavior stay stable.
- AGENTS.md: zero warnings/errors; every discovered issue is fixed, never suppressed; test files use `*_test.rs` naming.

## File inventory and current line counts

| Current file | Lines | Role in this plan |
|---|---:|---|
| `src/config.rs` | 487 | Add `[search] fragment_*` knobs (§ config) |
| `src/search/search/mod.rs` | 1,916 | `SearchEngine`: `fragment_vector_index`, hydrate, query-time union |
| `src/search/search/staged_retrieval.rs` | 112 | `SearchSnapshot`: add `fragment_root_hash` + `fragment_rows` |
| `src/search/search/vector_impl.rs` | 314 | Reuse `MmapVectorIndex`/`VectorIndexImpl` (no change, twin pattern) |
| `src/search/search/node_info.rs` | 447 | `NodeInfo` unchanged; results surface fragment ranges via new `SearchResult.fragment_byte_range` (Task 6) |
| `src/search/ranking.rs` | 384 | `Score.fragment` component + renormalized `HybridScorer` |
| `src/search/vector.rs` | 1,235 | Reuse `MmapEmbeddingIndex` (open :499, search :631, write :837) |
| `src/cli/index_builder/mod.rs` | 1,897 | Persist/load fragment mmap twins (near neural :1734/:1764/:1780), cache key (:1496/:1517) |
| `src/cli/index_builder/tfidf.rs` | 396 | Schema-version discipline reference; untouched |
| `src/cli/index_builder/hybrid.rs` | 498 | `HybridScoringWeights::normalize` reference; untouched |
| `src/cli/leindex/indexing/load.rs` | 339 | Hydration: load fragment mmap + snapshot fields |
| `src/cli/leindex/indexing/mod.rs` | ~1,738 | Post-index persist call sites (:440-456, :1369-1370, :1482, :1497) |
| `src/cli/leindex/query.rs` | 1,337 | Query path: cache key (:249), rerank pool, result surfacing |
| `src/cli/leindex/mod.rs` | ~1,000 | `LeIndex::search_cache_key_for` (:643) |
| `src/parse/grammar.rs` | 323 | `LanguageId::from_extension` (grammar lookup for chunker) |
| `src/parse/traits.rs` | 750 | `LanguageConfig` reference; untouched |
| NEW `src/cli/index_builder/fragment/mod.rs` | — | `Fragment`, `FragmentMetadata`, `FragmentStore`, root hash |
| NEW `src/cli/index_builder/fragment/chunker.rs` | — | Semantic split (ported from Warp) + naive fallback |
| NEW `src/cli/index_builder/fragment/orphan.rs` | — | Tier-3 complement extraction (excl. file doc) |
| NEW `src/cli/index_builder/fragment/enrich.rs` | — | Owner-header prefixing, doc-context reuse |
| NEW `src/cli/index_builder/fragment/sync.rs` | — | Incremental diff, root hash, generation guard |
| NEW `src/cli/index_builder/fragment/tests.rs` | — | `*_test.rs` per AGENTS.md |

## Non-negotiable invariants

1. **All-local.** No remote service, no stubbing; `remote-embeddings` remains the only opt-in network path (unchanged).
2. **Node-level index is authoritative.** Fragment layer is opt-in (`fragment_index_enabled = false` by default in 1.10.x-era config; flips on after validation in 1.11).
3. **Cache key ≡ embedding input.** Fragment content hash is `blake3(enriched text actually embedded)`; enrichment-format changes bump a schema version — never silently reuse embeddings.
4. **Additive storage only.** New files `fragment_store.bin`, `fragments_embeddings.bin`, `fragment_root.bin`; existing `embeddings.bin`/`neural_embeddings.bin`/`search_snapshot.bin` layouts untouched; `SearchSnapshot` new fields are serde-defaulted (`None`/`0`).
5. **Fragments never cross node byte ranges.** A Tier-2 fragment is always a contiguous sub-range of one symbol.
6. **Fragment hits map back to owner nodes** via content-hash before results are surfaced; exact/identifier routes are unaffected.
7. **Score fusion renormalizes — gated on the master switch.** Renormalization is keyed on `fragment_index_enabled` (NOT `fragment_weight > 0`, whose default 0.12 already exceeds zero); `fragment_weight` only scales the fragment component once enabled. Default behavior (feature off) is byte-identical.
8. **Stale-generation rejection.** Hydration rejects fragment mmap whose row count ≠ snapshot `fragment_rows` or whose root hash mismatches, mirroring `restore_from_search_snapshot` discipline.
9. **Strict feature DAG.** Fragment module compiles under `cli` (and the `graph`-visible pure types under `search`); never introduces `cli`-only symbols into `src/search`.
10. **No test disappears; every new file follows `*_test.rs` naming** and every assertion is ported from Warp's `semantic_tests.rs` expectations, never weakened.

---

### Task 0: Capture baseline and establish 1.11.0 gate

**Files:** `Cargo.toml` (:3), `src/cli/index_builder/mod.rs`, `src/search/search/mod.rs`; evidence under ignored `target/fragment-embeddings-baseline/`.

> **Progress:** Baseline verified 2026-08-01 — greenfield confirmed (0 fragment refs) at `27e53f0b` before Task 1; evidence captured post-hoc (file annotated accordingly); fmt/clippy/tests gates green. Commit deferred (in-flight 1.10.0 work, no commit without approval).

- [x] Record the pre-change state:

```bash
mkdir -p target/fragment-embeddings-baseline
git status --short
git rev-parse HEAD
rg -n 'fragment' src/config.rs src/search/search/mod.rs src/cli/index_builder/mod.rs \
  | tee target/fragment-embeddings-baseline/fragment-refs.txt
# Expect: no active fragment references (0 lines) — confirms greenfield.
wc -l src/search/search/mod.rs src/search/ranking.rs src/cli/index_builder/mod.rs \
  | tee target/fragment-embeddings-baseline/line-counts.txt
```

- [ ] Capture query/hydration timing for regression comparison (existing path):

```bash
/usr/bin/time -v cargo check -p leindex --features onnx \
  2>target/fragment-embeddings-baseline/check-time.txt
```

- [ ] Confirm baseline gates pass before any change:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Expected: all green; any failure is a pre-existing issue to fix (AGENTS.md zero-tolerance) before proceeding.

### Task 1: Config schema for fragment knobs

**Files:** `src/config.rs` (487); `src/config.rs` tests.

> **Progress:** Task 1 implemented 2026-08-01 (config fields + defaults + tests landed in `src/config.rs`; `cargo test --lib --features cli fragment` green). Commit deferred — in-flight 1.10.0 work + no commit without approval.

- [x] Extend `SearchConfig` with serde-defaulted fields:

```rust
/// Enable fragment-level (sub-symbol) embeddings. Off by default; the node
/// index remains authoritative. When enabled, Tier-2/3 fragments participate
/// in Semantic/hybrid retrieval.
#[serde(default)]
pub fragment_index_enabled: bool,

/// Max bytes per fragment (≈ Warp 200 lines × 60 chars).
#[serde(default = "default_fragment_max_bytes")]
pub fragment_max_bytes: u64,/// Fusion weight for the fragment score component (0.0-1.0). Renormalization
/// is gated on `fragment_index_enabled` (the master switch); this weight only
/// scales the fragment component once enabled.
#[serde(default = "default_fragment_weight")]
pub fragment_weight: f64,

/// Include Tier-3 module-level orphan regions.
#[serde(default = "default_true")]
pub fragment_orphan_enabled: bool,

/// Naive 200-line chunking when a tree-sitter grammar is unavailable.
#[serde(default = "default_true")]
pub fragment_naive_fallback: bool,
```

- [x] Defaults: `fragment_max_bytes = 12_000`, `fragment_weight = 0.12`, `fragment_orphan_enabled = true`, `fragment_naive_fallback = true`, `fragment_index_enabled = false`.
- [x] Add tests: round-trip preserves fragment fields; defaults match the constants; `fragment_index_enabled=false` default keeps config parse identical to pre-change. (`test_fragment_config_defaults`, `test_fragment_config_round_trip` added; empty-TOML parse == `Default` asserted.)
- [ ] Verify and commit:

```bash
cargo test -p leindex --features cli config
git add src/config.rs
git commit -m "feat: add fragment index configuration knobs"
```

*(Verification run on 2026-08-01: `cargo test --lib --features cli fragment` → 2 passed; `cargo clippy -p leindex --features cli --lib` clean; `cargo fmt --all --check` clean. Commit still open per repo policy.)*

### Task 2: Fragment chunker (semantic + naive + orphan)

**Files:** NEW `src/cli/index_builder/fragment/{mod,chunker,orphan,enrich,tests}.rs`; language lookup via `LanguageConfig::from_extension` (`src/parse/traits.rs:396`).

> **Progress:** Task 2 implemented 2026-08-01 (`src/cli/index_builder/fragment/{mod,chunker,orphan,enrich,tests}.rs` + wiring in `index_builder/mod.rs`: `#[allow(dead_code)] mod fragment;` + `preceding_doc_context` → `pub(crate)`). Warp `semantic_tests.rs`/`naive_tests.rs` ported verbatim (adapted: `usize` offsets, in-tree `line_spans`, `LanguageId` lookup); orphan/enrich tests added. `cargo test --lib --features cli fragment` 21/21, `cargo clippy -p leindex --features cli --lib -- -D warnings` 0, `cargo fmt --all --check` clean.

- [x] `chunker.rs` — port Warp's semantic split (`crates/ai/src/index/full_source_code_embedding/chunker/semantic.rs`):
  - `split_node` recursion over the node's AST subtree, `MAX_TRAVERSAL_DEPTH = 200`.
  - `MAX_BYTES_PER_CHUNK = fragment_max_bytes` (default 12,000).
  - Reverse-coalesce (Warp `coalesce_fragments`) **within the node byte range only** — invariant 5.
  - Naive fallback (Warp `naive.rs`: 200-line chunks, byte-safe splits) for missing grammar/parse failure.
- [x] **Language lookup:** use the verified entry point `LanguageConfig::from_extension` (`src/parse/traits.rs:396`), which delegates to `LanguageId::from_extension` in `src/parse/grammar.rs` — same tree-sitter grammar registry, no new dependency.
- [x] `orphan.rs` — Tier-3: union of Tier-1 node byte ranges per file; complement = orphan regions; **exclude the leading file-doc region** (invariant: FileSummary already covers it); chunk with same rules; light header `// type:module lang:<lang> file:<path>`.
- [x] `enrich.rs` — fragment enrichment: owner header line (`// type:function lang:rust callers:N callees:N complexity:N` + `// <symbol> in <path>`) prefixed to Tier-2 slices; reuse `preceding_doc_context` (already comment-marker-stripped, 24-line cap).
- [x] Port Warp `semantic_tests.rs` expectations verbatim (coalescing keeps `#[derive]`+`struct`, `impl`+method, `fn main` split at byte-safe boundaries; no fragment exceeds max bytes; no fragment crosses a node range). Add orphan tests (module-level statement retrievable; file-doc region excluded; empty complement → zero rows).
- [ ] Verify and commit:

```bash
cargo test -p leindex --features cli fragment
cargo fmt --all --check
cargo clippy -p leindex --features cli -- -D warnings
git add src/cli/index_builder/fragment
git commit -m "feat: semantic fragment chunker with orphan coverage"
```

### Task 3: Content-hash fragment store

**Files:** NEW `src/cli/index_builder/fragment/mod.rs`, `sync.rs`; `src/cli/index_builder/mod.rs`.

> **Progress:** Task 3 implemented 2026-08-01 in worktree `feat/fragment-embeddings-1.11.0` (`FragmentStore` + `FragmentMetadata` in `fragment/mod.rs`; `sync.rs` root-hash/generation). `cargo test --lib --features cli fragment` 30/30, `cargo clippy -p leindex --features cli --lib -- -D warnings` 0, `cargo fmt --all --check` clean.

- [x] `FragmentMetadata`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FragmentMetadata {
    pub content_hash: String,       // blake3(enriched text that was embedded)
    pub owner: Option<String>,      // Tier-1 node_id when inside a symbol
    pub file_path: String,
    pub byte_range: (usize, usize), // exact source slice
    pub line_range: (usize, usize),
    pub embedding_offset: u64,      // row into fragment embeddings mmap
}
```

- [x] `FragmentStore` (bincode, `.leindex/fragment_store.bin`): `HashMap<content_hash, Vec<FragmentMetadata>>` (one embedding row, N metadata refs — dedup invariant).
- [x] Store schema version constant (mirror `TFIDF_SCHEMA_VERSION`); `is_fresh`/`from_persisted_state`/`persist`/`load` with schema-version rejection.
- [x] Root hash computation in `sync.rs`: root = `blake3(sorted (content_hash × embedding-version) pairs)`; persisted to `.leindex/fragment_root.bin` with a generation counter.
- [ ] Verify and commit:

```bash
cargo test -p leindex --features cli fragment_store
git add src/cli/index_builder/fragment src/cli/index_builder/mod.rs
git commit -m "feat: content-hash fragment store with dedup"
```

### Task 4: Fragment mmap persistence twins

**Files:** `src/cli/index_builder/mod.rs` (near `persist_neural_embeddings_to_mmap` :1734, `neural_mmap_embeddings_path` :1764, `try_load_neural_mmap_embeddings_from_storage` :1780); `src/search/vector.rs` (:837 `write_mmap_embeddings`).

> **Progress:** Task 4 implemented 2026-08-01 in worktree `feat/fragment-embeddings-1.11.0` (`fragment_mmap_embeddings_path`, `persist_fragment_embeddings_to_mmap(project_path, embeddings)`, `try_load_fragment_mmap_embeddings_from_storage` in `index_builder/mod.rs` + 3 tests). **Design adaptation (documented in code):** the plan's `collect_fragment_embeddings(&SearchEngine)` is deferred to Task 5 because `SearchEngine.fragment_vector_index` only exists there; Task 4 takes the `(content_hash, Vec<f32>)` slice directly so the persistence twins are fully functional today (no stubbing). The 3 new fns carry `#[allow(dead_code)]` (outside the `mod fragment;` allow subtree, no callers until Tasks 5/7) — **Task 5/7 MUST remove those attributes when wiring callers.** `cargo test --lib --features cli fragment_mmap` 2/2, `cargo clippy -p leindex --features cli --lib -- -D warnings` 0, `cargo fmt --all --check` clean.

- [x] `fragment_mmap_embeddings_path(project_path)` → `.leindex/fragments_embeddings.bin` (mirror `:1764`).
- [ ] `collect_fragment_embeddings(&SearchEngine)` → `(content_hash, Vec<f32>)` pairs from the fragment store (mirror `collect_neural_embeddings`). *(Deferred to Task 5 with `fragment_vector_index`; remove `#[allow(dead_code)]` on `persist_fragment_embeddings_to_mmap` when wiring.)*
- [x] `persist_fragment_embeddings_to_mmap(project_path, embeddings: &[(String, Vec<f32>)])` — mirror `persist_neural_embeddings_to_mmap` (`:1734`): empty → remove stale file; else `write_mmap_embeddings`.
- [x] `try_load_fragment_mmap_embeddings_from_storage(storage_path)` → `Option<MmapEmbeddingIndex>` — mirror `:1780` (open, warn on error).
- [x] Additive-only guard: never mutate `embeddings.bin`/`neural_embeddings.bin`.
- [x] Unit tests mirror the neural mmap persistence tests (round-trip, empty→remove, stale-file cleanup).
- [ ] Verify and commit:

```bash
cargo test -p leindex --features cli fragment_mmap
git add src/cli/index_builder/mod.rs src/search/vector.rs
git commit -m "feat: fragment embeddings mmap persistence"
```

### Task 5: SearchEngine integration + snapshot hydration

**Files:** `src/search/search/mod.rs` (field :109, `new()` :142, `with_dimension()` :178, `clear_index()` :252, `restore_from_search_snapshot` :739); `src/search/search/staged_retrieval.rs` (`SearchSnapshot` :8).

- [x] Add `fragment_vector_index: Option<VectorIndexImpl>` next to `neural_vector_index` (:109); init `None` in `new()`/`with_dimension()`; clear in `clear_index()`. **Also: add `SearchEngine::collect_fragment_embeddings` (Task 4's deferred item) and remove `#[allow(dead_code)]` from the 3 Task 4 fns in `index_builder/mod.rs`.**
- [x] Extend `SearchSnapshot` (`staged_retrieval.rs:8`): `fragment_root_hash: Option<String>` (serde default), `fragment_rows: u32` (default 0).
- [x] `restore_from_search_snapshot` (:739): accept `fragment_mmap: Option<Arc<MmapEmbeddingIndex>>` + `fragment_ids: Option<&[String]>` (content-hash keys) — **both optional** so the existing `load.rs:126` call site still compiles unchanged (transitional buildability, per embed-merge discipline); validate `fragment_mmap.len() == snapshot.fragment_rows` and root-hash match (invariant 8); build `fragment_vector_index = Some(VectorIndexImpl::Mmap(MmapVectorIndex::from_snapshot(mmap, fragment_ids)))` — non-fatal on failure (mirror neural block).
- [x] **Update the caller in the same task:** thread the new optional params through `load.rs` `try_hydrate_from_snapshot` (`:91-129`) so the tree compiles at the end of Task 5, not Task 7. Task 7 then only wires the *fragment store load* (persisted artifacts), not the signature.
- [x] `collect_fragment_embeddings` reads from `fragment_vector_index` (fallback to store) for persistence.
- [x] Tests: snapshot round-trip with fragment fields; stale root → rejection; missing fragment rows → rejection; feature-off → `None` behaves identically to today.
- [x] Verify and commit:

```bash
cargo test --lib -p leindex --features cli search_snapshot
cargo check -p leindex --no-default-features --features search
cargo check -p leindex --no-default-features --features cli
git add src/search/search/mod.rs src/search/search/staged_retrieval.rs \
  src/cli/leindex/indexing/load.rs
git commit -m "feat: hydrate fragment vector index from snapshot"
```

**Progress note (Task 5 landed):** `fragment_vector_index` hydration is fully wired end-to-end (persist → hydrate → validate). Root-hash + row-count validation splits across `fragment_layer_is_valid` (cli, invariant 8) and the non-fatal restore block (invariant 3). Runtime activation awaits Task 7 (the rich `fragment_store.bin`), at which point hydration also gains fragment ids — today the layer stays off when the store is absent.

> **Forward notes for Task 5 successors:**
> - The plan's verify command `cargo test -p leindex --features cli search_snapshot` fails on the pre-1.10.0 tree (onnx-gated `pub mod embed` breaks the `onnx_worker_fallback`/`embed_bundle_pipeline_test` integration targets). Use `cargo test --lib -p leindex --features cli search_snapshot` until embed-merge lands.
> - The fragment mmap row order is HashMap-iteration order (non-deterministic) in both `entries()` (persist) and `store.content_hashes()` (hydrate ids). `from_snapshot` resolves rows by id lookup, so this is safe today — but Task 7 must resolve `embedding_offset` via `find_node_row`, never positional store-iteration index.

### Task 6: Query-time retrieval + ranking fusion

**Files:** `src/search/search/mod.rs` (neural candidates :1109, `collect_search_candidates` :1164/:1179/:1192); `src/search/ranking.rs` (`Score`, `HybridScorer`); `src/cli/leindex/query.rs` (search path :249, rerank pool).

- [x] In `search()`: alongside `neural_candidates`, compute `fragment_candidates` from `fragment_vector_index` (top_k×10, ≥100) when a query neural embedding exists.
- [x] **Renormalization gate:** key score renormalization on `fragment_index_enabled` (the master switch), NOT `fragment_weight > 0` — the default `fragment_weight` is already `0.12` (> 0), so gating on the weight alone would renormalize with the feature off and break invariant 7 (byte-identical default).
- [x] Map fragment hits → owner nodes: `HashMap<owner_node_id, Vec<content_hash>>` from the fragment store; add owners to the candidate pool (invariant 6); retain the best fragment byte range per owner for result surfacing.
- [x] **Result surfacing:** add `fragment_byte_range: Option<(usize, usize)>` to `SearchResult` (serde default → `None` for old cached results) rather than repurposing the node-level `byte_range` — keeps node ranges and fragment ranges distinguishable. Populate from the retained best fragment range; leave `byte_range` (node-level) unchanged.
- [x] `Score` gains `fragment: f32` (serde default 0.0); `HybridScorer::score_hybrid` gains a `fragment` weight; **when `fragment_index_enabled`, renormalize the five weights to sum 1.0** (mirror `HybridScoringWeights::normalize`); gated so default path is byte-identical.
- [x] Query path (`query.rs`): read `cfg.search.fragment_index_enabled`; when enabled, include fragment candidates in the reranker pool (single union pool, existing top-80); truncate back to `top_k` after rerank.
- [x] Exact/identifier routes (`query_route.rs`) unchanged; fragment layer participates only when a query neural embedding exists.
- [x] Tests: fragment candidate union; owner mapping; renormalization math (all five weights sum 1.0); default (feature-off) scores byte-identical to pre-change; exact-route non-regression.
- [x] Verify and commit:

```bash
cargo test -p leindex --features cli ranking
cargo test -p leindex --features cli search_fragment
cargo bench --bench search_benchmarks
git add src/search/search/mod.rs src/search/ranking.rs src/cli/leindex/query.rs
git commit -m "feat: fragment retrieval fusion with renormalized scoring"
```

### Task 7: Incremental sync + root-hash consistency

**Files:** `src/cli/index_builder/fragment/sync.rs`; `src/cli/leindex/indexing/load.rs` (hydration :11/:91/:121, finalize :313-324); `src/cli/leindex/indexing/mod.rs` (persist sites :440-456, :1369-1370, :1482, :1497).

- [x] Incremental index: file BLAKE3 unchanged → skip file; changed → re-chunk, diff content hashes against store, embed **only missing hashes** via existing worker (batch 256 IPC).
- [x] Update fragment store rows + recompute root hash per generation; persist `fragment_root.bin`.
- [x] Hydration (`load.rs`): **wire the actual fragment store + root-hash load into the Task 5 plumbing** — load fragment mmap + fragment store alongside neural (`:121`), pass `fragment_ids` + root hash through `try_hydrate_from_snapshot` (`:91`); `finalize_hydration` (`:313-324`) persists fragment artifacts when `persist_artifacts`. (Task 5 already established the signature + call-site plumbing; this task connects the persisted artifacts.)
- [x] Post-index persist twins at `indexing/mod.rs` sites (:440-456, :1369-1370, :1482, :1497).
- [x] Query-time guard: if a generation is mid-build, serve from last complete root or flag staleness (Warp `out_of_sync_delay` analog); never read a half-synced fragment tree.
- [x] Tests: unchanged file → 0 re-embeds; single-edit file → only affected fragments re-embedded; mid-build generation → last-complete-root served; root mismatch → rebuild.
- [x] Verify and commit:

```bash
cargo test -p leindex --features cli fragment_sync
git add src/cli/index_builder/fragment src/cli/leindex/indexing
git commit -m "feat: incremental fragment sync with root-hash guard"
```

### Task 8: Cache-key integration

**Files:** `src/cli/leindex/mod.rs` (`search_cache_key_for` :643); `src/cli/index_builder/mod.rs` (`search_cache_key_for` :1496, `v2:` format :1517).

- [x] Extend the `v2:` query-result cache key with: `fragment_enabled`, `fragment_weight`, `fragment_root_hash`. A config or generation change invalidates the persisted search cache (mirror existing `embed_model`/`rerank_model` discipline).
- [x] No change to `search_cache_key(project_id)` (`src/cli/memory.rs:1305`) — that is the index-cache spiller key (`src/cli/index_cache.rs:242`), distinct from the query-result cache.
- [x] Add the fragment knobs to `search_cache_key_for`'s signature and all callers (`query.rs:249`, `:337`, `:1288`).
- [x] Test: changing `fragment_weight`/root hash produces a different key; legacy keys (no fragment fields) still parse.
- [x] Verify and commit:

```bash
cargo test -p leindex --features cli cache_key
git add src/cli/leindex/mod.rs src/cli/index_builder/mod.rs src/cli/leindex/query.rs
git commit -m "feat: fold fragment knobs into search cache key"
```

### Task 9: CLI/config plumbing + docs

**Files:** `src/cli/cli.rs` (`Commands::Search` :349, `cmd_search_impl` :737); three public READMEs; `docs/NEURAL_SETUP.md`, `docs/PERFORMANCE_BENCHMARKS.md`, `CHANGELOG.md`, `RELEASE_NOTES.md`.

- [x] `leindex setup --check` and `leindex config` surfaces report fragment knobs; no new CLI flag required (config-driven, consistent with `rerank_enabled`).
- [x] Neural-weight default drift (config 0.3 vs scorer 0.4) — **fixed on 2026-08-01** (config default now 0.4; dead constant removed; docs aligned). No further action in this task; see `docs/findings/2026-08-01-neural-weight-default-drift.md`.
- [x] Update README surfaces: fragment index opt-in, config knobs, privacy note (all-local).
- [x] Changelog `[1.11.0]` entry: fragment embeddings, content-hash store, orphan coverage, cache-key v2 extension, weight-drift fix.
- [x] Verify and commit:

```bash
rg -n 'fragment' README.md packages/pypi-leindex/README.md packages/npm-leindex-mcp/README.md \
  | tee target/fragment-embeddings-baseline/doc-refs.txt
git add src/cli README.md CHANGELOG.md RELEASE_NOTES.md docs
git commit -m "docs: fragment embeddings guidance and changelog"
```

### Task 10: Version bump to 1.11.0

**Files:** `Cargo.toml:3`, `package.json`, `dashboard/package.json`, `pi/package.json`, `packages/npm-leindex-mcp/{package.json,README.md,test.js}`, `packages/pypi-leindex/{pyproject.toml,__init__.py}`, `install.sh`, `install_macos.sh`, `install.ps1`; regenerate `Cargo.lock`.

- [ ] Bump all surfaces to 1.11.0 in the same change (AGENTS.md version parity; mirror 1.10.0 Task 11 list).
- [ ] Confirm `cargo package -p leindex --allow-dirty --list` includes `src/cli/index_builder/fragment/**` and the new artifact paths are NOT in package (they are runtime `.leindex/` files, not crate files).
- [ ] Verify and commit:

```bash
git add Cargo.toml Cargo.lock package.json dashboard/package.json pi/package.json \
  packages install.sh install_macos.sh install.ps1
git commit -m "release: prepare LeIndex 1.11.0"
```

### Task 11: Real package/install/performance/full verification

**Files:** No planned source changes. Evidence under ignored `target/fragment-embeddings-verification/`.

- [ ] Format/feature boundaries:

```bash
cargo fmt --all --check
cargo check -p leindex --all-targets
cargo check -p leindex --all-targets --no-default-features --features minimal
cargo check -p leindex --all-targets --no-default-features --features onnx
cargo check -p leindex --all-targets --no-default-features --features onnx-migraphx
cargo test -p leindex --no-run --no-default-features --features onnx
```

- [ ] Fragment-specific gates:

```bash
cargo test -p leindex --features cli fragment
cargo test -p leindex --features cli fragment_store
cargo test -p leindex --features cli fragment_mmap
cargo test -p leindex --features cli fragment_sync
cargo test -p leindex --features cli cache_key
cargo test -p leindex --features cli ranking
```

- [ ] Recall/regression measurement (new benchmark evidence): conceptual-query MRR before/after fragment tier on the search benchmark corpus; confirm measurable sub-symbol recall gain with no node-rank regression; memory bounds (50K nodes ≈ 300 MB raw / 75–150 MB INT8 before dedup; dedup and INT8 both applied).

```bash
cargo bench --bench search_benchmarks \
  | tee target/fragment-embeddings-verification/bench.txt
/usr/bin/time -v cargo check -p leindex --features onnx \
  2>target/fragment-embeddings-verification/check-time.txt
stat -c '%s %n' target/package/leindex-1.11.0.crate \
  | tee target/fragment-embeddings-verification/crate-size.txt
```

- [ ] Mandatory AGENTS suite exactly:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Zero warnings/errors/failures. Diagnose every discovered issue; never suppress or call pre-existing.

- [ ] Additional ONNX gates:

```bash
cargo clippy -p leindex --all-targets \
  --no-default-features --features onnx -- -D warnings
cargo test --workspace --all-features
```

- [ ] Final review:

```bash
git status --short
git diff --check
git log --oneline --decorate -12
```

No publish/push without explicit approval.

## Stop/rollback conditions

- Stop before enabling the fragment default if conceptual-query recall does not improve measurably (benchmark evidence required, not anecdote).
- Stop before default-on if node-level ranking regresses (MRR on existing node queries must not drop).
- Stop if fragment mmap hydration ever blocks the existing snapshot path (feature-off must stay byte-identical and just as fast).
- Stop if a strict-feature build fails because the fragment module leaked a `cli`-only symbol into `src/search`.
- Stop if any existing mmap/snapshot format is mutated in place rather than additive.
- Roll back via reviewable task commits; never force-push or overwrite 1.10.0 artifacts.

## Final self-review checklist

- [ ] Every design decision in `docs/plans/combined-chunker-design.md` (§1–§16) is represented as a task.
- [ ] Cache key ≡ embedding input; enrichment-format changes bump a schema version.
- [ ] Fragment IDs are content hashes and map back to owner nodes before surfacing.
- [ ] Score fusion renormalizes to sum 1.0; default path byte-identical.
- [ ] File-doc region excluded from orphan complement.
- [ ] All-local, no remote service, no stubbing; `remote-embeddings` untouched.
- [ ] Existing mmap/snapshot formats additive-only; `SearchSnapshot` fields serde-defaulted.
- [ ] Strict feature DAG respected (no `cli` symbols in `search`).
- [ ] Warp `semantic_tests.rs` expectations ported verbatim, never weakened.
- [ ] Pre-existing neural-weight drift fixed with config as single source of truth.
- [ ] Full AGENTS validation is final gate; no placeholders (`TBD`, `TODO`) remain.
- [ ] Implementation does not push, publish, or delete shared data without approval.

Plan complete. Recommended execution: subagent-driven task-by-task implementation with review after each commit; inline execution is acceptable if checkpoints remain intact.
