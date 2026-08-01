# Combined Chunking Strategy — Warp Semantic Chunker × LeIndex PDG Node Enrichment

**Date:** 2026-08-01
**Status:** Design (pre-implementation) — feeds the proposed `fragment-embeddings-1.11.0` plan
**Depends on:** `docs/plans/embed-merge-1.10.0.md` (config/worker paths must land first)
**Related:** `docs/findings/2026-08-01-hash-embeddings-warp-evaluation.md` (§7 integration phases)

---

## 1. Purpose

Design the chunking layer that combines the strengths of two proven approaches:

- **Warp's tree-sitter semantic chunker** — whole-file → semantic fragments with sub-symbol granularity, content-hash dedup, naive fallback.
- **LeIndex's PDG node enrichment** — symbol-level units with graph metadata (callers/callees/complexity), doc-context prose, and per-file summaries.

The goal: **fragment-level retrieval that inherits LeIndex's graph-aware enrichment**, so the model can "search more freely" (hit *inside* big functions, docs, module-level code) while returning *more accurate* results (exact byte ranges, dedup'd identical code) — all local.

---

## 2. Side-by-Side Comparison (as-built, both repos)

| Dimension | Warp semantic chunker | LeIndex PDG enrichment |
|---|---|---|
| Input unit | Whole file | PDG node (function/method/class/var/module) + FileSummary per file |
| Splitting basis | tree-sitter AST (`split_node` recursion, depth ≤ 200) | Symbol boundaries from per-language parsers (`node.byte_range`) |
| Chunk size | `LINES_PER_CHUNK=200` × `AVG_CHAR_PER_LINE=60` ≈ 12 KB, coalesced in reverse | Unbounded (whole node); no sub-node split today |
| Added context | **None** (`RETRIEVE_FRAGMENT_CONTEXT_LENGTH=0` — they removed it) | **Rich**: `// type:… lang:… callers:N callees:N complexity:N` header + `// name in path` + `// review_context:` (24 doc lines, comment markers stripped) + source snippet |
| File-level coverage | All code including module-level statements | FileSummary node (file doc ≤ 16 lines + up to 40 item names); **module-level code between symbols is otherwise invisible** |
| Language resolution | `languages::language_by_filename` (32 langs, embedded grammars via RustEmbed) | `LanguageId::from_extension` → `parser_for_language` (15 tree-sitter grammars; kotlin/swift/dart disabled on version conflict) |
| Hash/dedup | SHA-256 content hash per fragment (content-addressed store) | BLAKE3 per file (change detection); `WorkHoister` in-run content dedup only |
| Fallback | Naive 200-line chunking when parser unavailable/fails | None needed (symbol-based); unknown languages simply produce no nodes |
| Retrieval unit | Fragment with byte + line range, maps back via `FragmentMetadata` | Whole node with `byte_range` |

**Key insight:** the two are **orthogonal axes** — Warp solves *granularity* (sub-symbol), LeIndex solves *semantic context* (graph + docs). A combined chunker should **nest Warp-style fragments inside LeIndex's enriched node units**, plus add an **orphan-region tier** for module-level code neither currently captures well.

---

## 3. Design Goals

1. **Sub-symbol retrievability** — a conceptual query can hit inside a 600-line function, a doc block, or an enum body, not just "which symbol".
2. **Graph-aware fragment text** — fragments carry the owning node's header (type/lang/callers/callees) so vector similarity still benefits from PDG context.
3. **Exact, deduplicated results** — fragments map to precise byte ranges; identical fragments across files are indexed once (content-hash addressing).
4. **Incremental cost collapse** — unchanged fragments are never re-embedded (content-hash cache), making per-save reindex cheap.
5. **No regression risk** — node-level index stays authoritative; fragment layer is opt-in and fuses with conservative weight.
6. **Zero new services** — tree-sitter, sha2/blake3, bincode, mmap, ONNX worker, reranker all already exist.

---

## 4. Architecture: Two-Tier + Orphan Tier

```
                         ┌────────────────────────────────────────────┐
                         │              FILE (source bytes)           │
                         └────────────────────────────────────────────┘
                                        │
                    ┌───────────────────┼───────────────────┐
                    │ (existing)        │ (new)             │ (new)
                    ▼                   ▼                   ▼
         ┌───────────────────┐  ┌──────────────────┐  ┌──────────────────┐
         │  TIER 1 (NODE)    │  │  TIER 2 (FRAG)   │  │  TIER 3 (ORPHAN) │
         │  PDG symbol nodes │  │  sub-symbol frags│  │  module-level    │
         │  + FileSummary    │  │  (semantic, ≤12K)│  │  code/docs       │
         │  enriched content │  │  enriched w/     │  │  (semantic, ≤12K)│
         │  (unchanged)      │  │  owner header    │  │  light header    │
         └───────────────────┘  └──────────────────┘  └──────────────────┘
                                        │  content-hash address (sha256/blake3)
                                        ▼
                         ┌─────────────────────────────┐
                         │  FragmentStore               │
                         │  hash → (owner, byte_range,  │
                         │          line_range, text)   │
                         └─────────────────────────────┘
```

### 4.1 Tier 1 — Node level (existing, unchanged)

Keep `enriched_node_content` exactly as-is (`src/cli/index_builder/mod.rs`): type/lang/callers/callees/complexity header, `// name in path`, `// review_context:` doc prose, source snippet, FileSummary items list. This remains the **authoritative symbol-level index** and the ranking backbone.

### 4.2 Tier 2 — Fragment level (new, inside nodes)

For every Tier-1 node whose `byte_range` span exceeds a threshold, split the node's **source slice** (raw bytes from `node.byte_range`) into semantic fragments:

1. **Language resolution** — reuse `LanguageId::from_extension` (LeIndex) rather than porting Warp's `language_by_filename`; same tree-sitter grammar registry, no new dependency.
2. **Semantic split** — port Warp's `split_node` recursion (depth ≤ 200) over the node's AST subtree, bounding fragments to `MAX_BYTES_PER_CHUNK ≈ 12 KB`.
3. **Boundary rule (deviation from Warp):** fragments must **never cross the node's byte range** — a fragment is always a contiguous sub-range of one symbol.
4. **Coalescing** — reuse Warp's reverse-coalesce so a function's name/attributes stay with its first body block (their test proves `#[derive]` + `struct` and `impl` + method stay together). Coalesce *within* the node, then stop.
5. **Node-sized fast path** — if a node fits under the chunk threshold, emit **one fragment == the node slice** (no fragmentation; zero cost).
6. **Enrichment of fragment text** — prefix each fragment with the **owner node's header line** (`// type:function lang:rust callers:N callees:N complexity:N`) plus `// <symbol> in <path>` so fragment embeddings inherit graph context. Doc context: reuse `preceding_doc_context` for the fragment's start offset (fragments that *are* mostly a doc block already carry their prose).

> **Cache-key contract (resolves §5 staleness):** the fragment content hash is computed over the **exact enriched text that is embedded** (header + doc context + slice), *not* the raw slice. This guarantees cache key ≡ embedding input by construction — a doc/header edit changes the key and forces a correct re-embed, and an unchanged enriched text is never re-embedded. Enrichment inputs must be deterministic at index time (callers/callees counts captured when the node is built, per §14.2), and any change to the *enrichment format itself* (not content) bumps the hash-key schema version, same discipline as `TFIDF_SCHEMA_VERSION`.

### 4.3 Tier 3 — Orphan level (new, module-level coverage)

This is the biggest "search more freely" win — **code and docs between/outside symbols**:

1. Walk each file's tree-sitter tree; compute the **union of Tier-1 node byte ranges**; the complement (top-of-file docs, module-level statements, constants, decorators, free-floating comments) becomes orphan regions.
2. **Exclude the leading file-doc region from the orphan complement** — it is already captured by the Tier-1 FileSummary node's `leading_file_doc` (≤ 16 lines). Without this exclusion the same doc block would be embedded twice with different headers → different content hashes → no dedup → duplicate conceptual hits.
3. Chunk orphan regions with the same semantic chunker (≤ 12 KB, coalesced), each carrying a light header: `// type:module lang:<lang> file:<path>` (no callers/callees — they're not symbols).
4. Large orphan region spanning multiple statements is split by the same rules.

### 4.4 Fragments per node — cardinality & memory

- Average node → 1–3 fragments; giant functions → bounded by `MAX_BYTES_PER_CHUNK` (e.g., 600-line fn ≈ 3–5 fragments).
- Content-hash dedup collapses identical fragments across files (boilerplate, generated code) to **one stored embedding** referenced N times.
- **Memory envelope (pre-quantization):** 50K nodes × ~1.5 avg fragments × 1024-dim f32 ≈ **300 MB** raw fragment embeddings (upper bound ~600 MB if every node fragments into 3); the existing INT8 ADC path (`src/search/quantization/**`, ~74% reduction) brings that to roughly **75–150 MB**. The fragment store is mmap-backed and lazy-loaded (same pattern as `embeddings.bin`), so heap residency stays flat until queried. Exact figures must be validated in the benchmark phase (§12).

---

## 5. Content Addressing & Dedup

- **Fragment key** = hash of the **exact enriched text that is embedded** (owner header + doc context + source slice), per the cache-key contract in §4.2 — so the cache key ≡ embedding input by construction. Use `blake3` for consistency with LeIndex's `read_file_once`.
- **Stability rule:** a code edit changes the slice → new key → re-embed only that fragment. A doc-context edit changes the doc lines → new key → re-embed (correct — the embedding genuinely changed). An *unchanged* fragment (identical enriched text) is never re-embedded. Any change to the enrichment *format* (header layout, doc-line cap) bumps the hash-key schema version rather than silently reusing embeddings.
- **FragmentStore** (bincode, `.leindex/fragment_store.bin`):
  ```rust
  struct FragmentMetadata {
      content_hash: String,          // blake3(enriched text that was embedded)
      owner: Option<String>,         // Tier-1 node_id when inside a symbol
      file_path: String,
      byte_range: (usize, usize),    // exact source slice
      line_range: (usize, usize),    // derived for display
      embedding_offset: u64,         // row into fragment embeddings mmap
  }
  ```
- **Embedding cache**: `content_hash → Vec<f32>` (or row into a fragment-level mmap `fragments_embeddings.bin`, mirroring the existing `embeddings.bin` format; consider a v2 header with `dimension + quantization` fields).
- **Incremental index**: file BLAKE3 unchanged → skip file entirely; file changed → re-chunk, diff content hashes against store, embed **only missing hashes** via existing worker (batch 256 IPC), update rows + root hash.

---

## 6. Language Resolution & Fallback

| Case | Behavior |
|---|---|
| Tree-sitter grammar available (`LanguageId::from_extension` hit) | Semantic chunking for Tier 2/3 |
| Grammar unavailable (e.g., kotlin/swift/dart currently disabled) | **Naive fallback** (port Warp's `naive.rs`: 200-line chunks, byte-safe splits) for Tier 2/3; Tier 1 still works via existing parsers |
| tree-sitter parse fails / traversal depth exceeded | Naive fallback (Warp's exact pattern: `try_semantic → None → naive`) |
| No language at all (unknown extension) | No fragments; node index only (current behavior) |

**Dependency note:** do **not** port Warp's `RustEmbed` grammar bundling — LeIndex already statically links its 15 grammars via `tree-sitter-*` crates; the ~17-language gap (kotlin/swift/dart and markup langs) is a *future* feature (re-enable grammar crates once version conflicts are resolved), not a blocker for the chunker design.

---

## 7. Storage & Snapshot Layout

- New files (additive — never mutate existing `embeddings.bin` / `search_snapshot.bin` layouts):
  - `.leindex/fragment_store.bin` — bincode `FragmentStore`
  - `.leindex/fragments_embeddings.bin` — mmap fragment embedding matrix (v1, mirror of `write_mmap_embeddings`)
  - `.leindex/fragment_root.bin` — root content hash of the fragment tree + generation counter (sync-consistency guard)
- `search_snapshot.bin` gains an optional `fragment_root_hash` field (serde default → `None` for backward compat; hydration rejects stale fragments on mismatch, mirroring `restore_from_search_snapshot` discipline).
- `SearchEngine` gains `Option<VectorIndexImpl>` fragment vector index, loaded lazily from the fragment mmap (same pattern as `neural_vector_index`).

---

## 8. Retrieval & Ranking Fusion

1. Run existing node-level search (Tier 1) **and** fragment-level search (Tier 2+3) against the same query embedding(s).
2. Map fragment hits to owner node for display/dedup, but surface the fragment's `byte_range` + `line_range` in results.
3. `Score` gains optional `fragment: f32` component; default weight **0.10–0.15** (conservative — node ranking stays authoritative). Configurable via `[search] fragment_weight`.

> **Weight renormalization required:** `HybridScorer::score_hybrid` is a plain weighted sum clamped to [0,1] — weights are **not** normalized. Adding a 5th `fragment` component on top of existing weights (which sum to 1.0) would silently depress every overall score and shift global ranking. When `fragment_weight > 0`, renormalize the five weights to sum to 1.0 (mirror `HybridScoringWeights::normalize` in `src/cli/index_builder/hybrid.rs`), and gate the renormalized scorer behind the fragment feature so default behavior stays byte-identical when the feature is off.
4. Reranker pool = union of node + fragment candidates, top-N local cross-encoder rerank (existing `rerank_top_n=80`), preserving the current pipeline.
5. Exact/identifier routes unaffected (fragment layer participates only in `Semantic`/hybrid routes; `query_route.rs` unchanged).

---

## 9. Config Surface (`[search]`, in `src/config.rs`)

```toml
[search]
fragment_index_enabled = false   # opt-in for 1.10.x; flip default on in 1.11 after validation
fragment_max_bytes = 12000       # ≈ Warp 200 lines × 60 chars
fragment_weight = 0.12           # fusion weight for fragment score component
fragment_orphan_enabled = true   # Tier 3 module-level coverage
fragment_naive_fallback = true   # naive chunking when tree-sitter unavailable
```

All knobs default off/neutral so behavior is byte-identical without opt-in.

---

## 10. Incremental Sync & Consistency

- Reuse LeIndex's existing file-level BLAKE3 change detection (`collect_source_files_with_hashes`).
- Extend with a **fragment-tree root hash** per generation: root = hash over sorted (content_hash × embedding-version) pairs. Store with the search snapshot; expose at query time.
- **Query-time guard:** if a generation is mid-build, serve from the last complete root or flag staleness (Warp's `out_of_sync_delay` analog) — never read a half-synced fragment tree.
- Cache-key discipline: include `embed_model` + `quantization` in the fragment cache key (already done for search via `search_cache_key_for`); a model swap must not reuse stale fragment embeddings.

---

## 11. Module Layout & Integration Points (post-1.10.0 paths)

> **Placement note:** this design places the fragment chunker at `src/cli/index_builder/fragment/` (it consumes PDG node output and the enriched-text contract built by `enriched_node_content`, which lives under `cli`). This intentionally differs from the `src/search/fragment/` location sketched in the earlier findings doc: `src/search` is compiled without the `cli` feature (strict feature DAG from the 1.10.0 plan), so the chunker cannot live there if it depends on `enriched_node_content`. The pure types (`Fragment`, `FragmentMetadata`, store format) that `src/search` *does* consume should stay in a search-visible location (e.g., `src/search/search/` or a `search`-gated module) with the enrichment-dependent chunker under `cli`. Final placement is a 1.10.0-plan integration decision; the two docs are now aligned on this rationale.

```
src/cli/index_builder/
  fragment/
    mod.rs            # Fragment, FragmentMetadata, FragmentStore
    chunker.rs        # semantic split (ported from Warp) + naive fallback
    orphan.rs         # Tier 3 complement-region extraction
    enrich.rs         # owner-header prefixing, doc-context reuse
    sync.rs           # incremental diff, root hash, generation guard
    tests.rs          # *_test.rs convention per AGENTS.md
```

Integration seams (all post-`embed-merge-1.10.0` names):
- `src/cli/index_builder/mod.rs` — `build_indexed_node` emits fragment rows alongside node rows; `enrich_neural_embeddings` extended for fragment texts.
- `src/search/search/mod.rs` — `SearchEngine` fragment vector index + `restore_from_search_snapshot` hydration + fusion in `search()`.
- `src/search/vector.rs` — fragment mmap writer (reuse `write_mmap_embeddings` with v2 header).
- `src/config.rs` — §9 knobs.
- `src/search/ranking.rs` — `Score.fragment` component + `HybridScorer` weight.

---

## 12. Validation Plan

1. **Chunker unit tests** (port Warp's `semantic_tests.rs` expectations): coalescing keeps `#[derive]`+`struct`, `impl`+method, `fn main` split at byte-safe boundaries; no fragment exceeds `max_bytes`; no fragment crosses a node byte range.
2. **Orphan tests**: module-level statement between two functions is retrievable; file doc block produces a fragment; empty complement → no Tier-3 rows.
3. **Dedup tests**: identical fragment in 2 files → 1 embedding row, 2 metadata refs.
4. **Incremental tests**: unchanged file → 0 re-embeds; single-edit file → only affected fragments re-embedded.
5. **Regression**: full AGENTS suite (`cargo fmt/clippy/test`), `benches/search_benchmarks.rs`, exact-route non-regression, mmap memory bounds (`docs/findings` evidence file).
6. **Recall measurement**: conceptual-query recall before/after fragment tier (target: measurable MRR gain on sub-symbol queries without node-rank regression).

---

## 13. Risks & Trade-offs

| Risk | Mitigation |
|---|---|
| Index growth (more embeddings) | Content-hash dedup; INT8 quantization (existing); opt-in flag |
| Fragment fusion drowns node ranking | Conservative `fragment_weight` (0.10–0.15); validate with benchmarks |
| Double tree-sitter parse (parse phase + chunker) | Reuse the parsed tree / grammar cache (`GrammarCache`); chunk from node's AST subtree, not a fresh file parse |
| Storage format drift | Additive files only; versioned headers; stale-generation rejection |
| Scope collision with 1.10.0 | Land strictly after embed-merge Tasks 1–7 (config + worker paths) |
| Grammar gap (17 langs) | Naive fallback covers them; grammar re-enable is a separate future task |

---

## 14. Open Questions (for review before implementation)

1. ~~Fragment hash: `blake3(raw slice)` vs `blake3(enriched text)`~~ — **Resolved in §4.2/§5:** hash the exact enriched text that is embedded (cache key ≡ embedding input), with a format-version bump for enrichment-schema changes. The open question is now narrower: confirm the schema-version bump mechanism (mirror `TFIDF_SCHEMA_VERSION`).
2. Should Tier-2 fragments carry the owner's callers/callees counts from *index time* (cheap, mildly stale) or *query time* (fresh, expensive)? Recommend index-time, consistent with existing node enrichment — and note the counts are part of the enriched text, so a count change re-embeds the fragment (correct, deterministic).
3. Orphan Tier: include markdown/prose files (README, docs/) as pure-doc fragments, or keep source-only for 1.11? Recommend source-only first, prose later.
4. Reranker: reuse the single top-80 pool over union, or a dedicated fragment pool? Recommend single pool (simpler, proven).

---

## 15. Recommendation Summary

- **Tier 1 (nodes) + enrichment: unchanged.** It's the ranking backbone and the graph-aware context that makes LeIndex distinctive.
- **Tier 2 (sub-symbol fragments): port Warp's semantic chunker** (split + reverse-coalesce + naive fallback) but bound fragments to node ranges and prefix owner headers.
- **Tier 3 (orphan regions): new, highest recall win** — makes module-level code and file docs retrievable.
- **Content-hash store + incremental sync: the cost win** — unchanged code is never re-embedded.
- Sequence after 1.10.0; opt-in; validated against AGENTS gates and search benchmarks.

---

## 16. Precise Integration Map — Cache Key & Snapshot Hydration

> **Anchors are current-tree (2026-08-01, `feat/embed-merge-1.10.0`) and will shift after the embed-merge move (plan Tasks 4–6).** Pair each anchor with the symbol name when implementing.

A fragment-level mmap index is **an exact structural twin of the existing neural mmap path** (`neural_embeddings.bin` → `neural_vector_index`). The map below mirrors that proven path seam-for-seam, so implementation risk is low and review symmetry is high.

### 16.1 The four seams (summary)

| Seam | Current code (neural analog) | Fragment change |
|---|---|---|
| 1. SearchEngine state | `neural_vector_index: Option<VectorIndexImpl>` (`src/search/search/mod.rs:109`) | add `fragment_vector_index: Option<VectorIndexImpl>`; init in `new()` (`:142`)/`with_dimension()` (`:178`), clear in `clear_index()` (`:252`) |
| 2. Snapshot struct | `SearchSnapshot` (`src/search/search/staged_retrieval.rs:8`) | add `fragment_root_hash: Option<String>` (serde default) + optional `fragment_rows: u32` |
| 3. Persistence load/save | `try_load_neural_mmap_embeddings_from_storage` (`src/cli/index_builder/mod.rs:1780`), `persist_neural_embeddings_to_mmap` (`:1734`) | twin `try_load_fragment_mmap_embeddings_from_storage` + `persist_fragment_embeddings_to_mmap` on `.leindex/fragments_embeddings.bin` |
| 4. Query cache key | `LeIndex::search_cache_key_for` (`src/cli/leindex/mod.rs:643`) → `index_builder::search_cache_key_for` (`:1496`, `v2:` fmt at `:1517`) | fold fragment knobs + root hash into the `v2:` key |

### 16.2 Seam 1 — SearchEngine state (`src/search/search/mod.rs`)

- **Field:** add next to `neural_vector_index: Option<VectorIndexImpl>` at `:109`.
- **Init:** `SearchEngine::new()` (`:142`) and `with_dimension()` (`:178`) set it `None`.
- **Clear:** `clear_index()` (`:252`) also sets it `None` (drop stale fragment mmap on full reindex).
- **Hydrate:** inside `restore_from_search_snapshot` (`:739`), after the neural mmap block, add the same pattern:
  ```rust
  if let Some(mmap) = fragment_mmap.as_ref() {
      // fragment_ids = the fragment store's content-hash key set (see §16.6).
      // Unlike the node path, hits are content hashes, so owner-node mapping
      // is required before results are surfaced.
      match MmapVectorIndex::from_snapshot(std::sync::Arc::clone(mmap), &fragment_ids) {
          Ok(idx) => staged.fragment_vector_index = Some(VectorIndexImpl::Mmap(idx)),
          Err(error) => tracing::warn!(error = %error, "fragment mmap disabled"),
      }
  }
  ```
  Non-fatal on failure, exactly like the neural path.

### 16.3 Seam 2 — Snapshot struct (`src/search/search/staged_retrieval.rs:8`)

- Add `pub(crate) fragment_root_hash: Option<String>` (serde default → `None`; backward-compatible with old snapshots).
- Add `pub(crate) fragment_rows: u32` (default 0) so hydration can validate fragment count like it validates `tfidf_mmap.len() == snapshot.indexed_nodes` (`mod.rs` restore: `:20-33`).
- Hydration rejects a fragment mmap whose row count ≠ snapshot `fragment_rows`.

### 16.4 Seam 3 — Persistence (`src/cli/index_builder/mod.rs`)

Mirror the neural path exactly (files `:1734`, `:1764`, `:1780`, `:1867`):

```rust
// path helper (mirror neural_mmap_embeddings_path at :1765)
project_path.join(".leindex").join("fragments_embeddings.bin")

// persist (mirror persist_neural_embeddings_to_mmap at :1720)
collect_fragment_embeddings(&search_engine)  // (hash, vec) pairs from fragment store
  → write_mmap_embeddings(&path, &rows)

// load (mirror try_load_neural_mmap_embeddings_from_storage at :1780)
MmapEmbeddingIndex::open(&path)  → Arc, warn on error
```

### 16.5 Seam 4 — Query cache key (`src/cli/leindex/mod.rs:643` + `src/cli/index_builder/mod.rs:1496-1525`)

- The wrapper `LeIndex::search_cache_key_for` already folds every result-affecting knob. Extend the `v2:` key (index_builder `:1517`) with:
  - `fragment_enabled` (`cfg.search.fragment_index_enabled`)
  - `fragment_weight`
  - `fragment_root_hash` (from the loaded fragment store)
- A config or generation change therefore invalidates the persisted search cache, matching the existing model-identity discipline (`embed_model`, `rerank_model` already in the key).
- No change needed to `search_cache_key(project_id)` (`src/cli/memory.rs:1305`) — that is the spiller key for the *index* cache (`src/cli/index_cache.rs:242`), distinct from the *query* result cache.

### 16.6 Query-time retrieval (`src/search/search/mod.rs`)

- **Candidate union:** in `search()` (~`:1109-1122`), alongside `neural_candidates`, add:
  ```rust
  let fragment_candidates: HashSet<String> = match (&query.query_neural_embedding, &self.fragment_vector_index) {
      (Some(q_emb), Some(idx)) => idx.search(q_emb, query.top_k.saturating_mul(10).max(100))
          .into_iter().map(|(id, _)| id).collect(),
      _ => HashSet::new(),
  };
  ```
  **Note:** fragment ids are `content_hash` strings, not node ids. Union them via a `HashMap<owner_node_id, Vec<content_hash>>` from the fragment store so `collect_search_candidates` (`:1164`, `:1179`, `:1192`) can map fragment hits back to owner nodes for display/dedup, then surface the fragment byte range in results.
- **Exact route** (`query_route.rs`) is untouched — fragment layer only participates when a query neural embedding exists (Semantic/hybrid).

### 16.7 Hydration call sites (`src/cli/leindex/indexing/load.rs`)

- `load_from_storage_inner_at` (`load.rs:11`); `try_hydrate_from_snapshot` (`:91-129`) loads snapshot + tfidf mmap + neural mmap and calls `restore_from_search_snapshot`. Add fragment mmap load at `:121-124` (same `cfg` gating) and pass it through.
- `finalize_hydration` (`load.rs:313-324`) persists artifacts; add `persist_fragment_embeddings_to_mmap` alongside the neural persist.
- Post-index persist sites in `src/cli/leindex/indexing/mod.rs`: `:440-456`, `:1369-1370`, `:1482`, `:1497` (`persist_neural_mmap`) — add the fragment twin at each.

### 16.8 Implementation checklist (ordered)

1. `staged_retrieval.rs` — snapshot fields (`fragment_root_hash`, `fragment_rows`).
2. `search/mod.rs` — SearchEngine field, init/clear, hydrate, query-time candidate union.
3. `index_builder/mod.rs` — fragment store persist/load twins + `collect_fragment_embeddings`.
4. `load.rs` + `indexing/mod.rs` — load/persist call sites.
5. `leindex/mod.rs:643` + `index_builder/mod.rs:1517` — cache key knobs.
6. `config.rs` — §9 knobs (fragment_index_enabled, fragment_weight, etc.).
7. Benchmarks + AGENTS suite (§12).
