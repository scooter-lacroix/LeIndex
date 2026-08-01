# Warp "Hash Embeddings" — Localized Integration Evaluation for LeIndex

**Date:** 2026-08-01
**Author:** Buffy (Freebuff analysis)
**Scope:** Evaluate the value of incorporating Warp terminal's hash-embedding / codebase-indexing technique into LeIndex as a fully-localized enhancement to the existing TF-IDF / PDG / neural-embedding search stack. Also catalog general LeIndex improvements discovered along the way.
**Constraint:** All functionality must be localized — no stubbing, no remote service calls unless the user explicitly opts in.

---

## 1. Executive Summary

**Verdict: High value, medium effort — recommended as a post-1.10.0 enhancement (or a stretch item inside the current merge plan).**

Warp's "hash embeddings" (their `full_source_code_embedding` module) is not a novel embedding model — it is an **architecture** that combines four transferable ideas LeIndex does not currently have as a cohesive unit:

1. **Content-hash-addressed fragment store** (SHA-256 Merkle tree over files → fragments) that makes incremental embedding idempotent and deduplicated. LeIndex already uses BLAKE3 for file-level change detection but embeds **per PDG node**, recomputing embeddings for unchanged content across runs.
2. **Semantic chunking via tree-sitter** (fragments of ~12 KB / 200 lines, coalesced to whole logical units). LeIndex embeds one blob per PDG node, mixing symbol body + doc context + graph metadata — good but coarse.
3. **Two-stage retrieval: dense candidate recall → cross-encoder rerank** — Warp does candidate retrieval server-side then reranks via a cross-encoder. **LeIndex already has this locally** (HNSW/vector + bge-reranker-base) — this is the part LeIndex is *ahead* on.
4. **Incremental sync with root-hash consistency** (a Merkle tree root hash is passed to retrieval so queries never hit a half-synced index). LeIndex has freshness fingerprints but no tree-level atomicity story for retrieval.

The genuinely valuable, localizable core for LeIndex is **(1) the content-hash fragment store with fragment-level embeddings**, layered on top of the existing TF-IDF/PDG/neural pipeline. It directly addresses the user's goals:

- **"Search more freely"** — today LeIndex can only retrieve whole PDG nodes (function/class bodies). Fragment-level embeddings let the model retrieve *sub-function regions*, doc blocks, and large-method slices, dramatically widening the retrievable unit space (a big Win for conceptual queries against big functions and prose docs).
- **"More accurate results"** — fragment→byte-range mapping returns exact source regions instead of whole-symbol blobs; content-hash dedup removes double-indexed duplicates; reranking stays local.
- **Cost** — content-hash caching means unchanged fragments never get re-embedded on incremental index, cutting the dominant neural cost on iterative development.

**Localization verdict:** Everything needed to replicate the *local* half of Warp's design already exists in LeIndex's dependency tree (tree-sitter, sha2/blake3, bincode, mmap, ONNX worker, cross-encoder reranker). Warp's *remote* half (Voyage/OpenAI embedding APIs over GraphQL) maps 1:1 onto LeIndex's existing local ONNX worker (`qwen3-embed-0.6b`) — no new model, no network, no vendor. A faithful local implementation is feasible with **zero new third-party services**; the `remote-embeddings` feature can remain the optional opt-in path it already is.

**Caveat:** Warp's own retrieval/rerank is server-side, so the OSS client contains no local ANN implementation to copy — LeIndex's own `search` crate is the superior local reference for that half. The copyable surface is the *chunking + hashing + store + sync* layer, which is genuinely novel relative to LeIndex today.

---

## 2. Methodology

- **Skills loaded:** `leindex-code-intelligence`, `analyze`, `research` (deep-research equivalent); `deep-research` is not installed under that exact name — the `research` skill was used instead.
- **LeIndex repo:** `/mnt/WD-SSD/code_index_update/LeIndexer-release-1.8.4` (branch `feat/embed-merge-1.10.0`).
- **Warp repo:** `/mnt/WD-SSD/Prod/work_resources/warp` (branch `master` @ `e089051`).
- **Methods:** source inspection (Warp `crates/ai/src/index/full_source_code_embedding/**`, `app/src/server/server_api/ai.rs`, `crates/warp_graphql_schema/api/schema.graphql`), LeIndex `src/search/**`, `src/cli/index_builder/**`, `src/graph/embedding.rs`, `src/config.rs`, plan doc `docs/plans/embed-merge-1.10.0.md`), web research (Warp docs/blog on Codebase Context), git state audit of both repos.

---

## 3. Repository Sync Status (LeIndex)

Per the user's instruction to bring the local LeIndex copy up to date with the remote **before** evaluating:

- Ran `git fetch origin` on the LeIndex worktree.
- Result: local branch `feat/embed-merge-1.10.0` is **0 commits behind `origin/master`** (checked via `git rev-list --left-right --count origin/master...HEAD` → `0 3`, i.e., 0 remote-only commits; 3 local-only commits ahead of master, which include the embed-merge plan and the feature-boundary fix).
- The worktree holds **uncommitted in-flight changes from the other agent** (config rename `src/cli/neural_config.rs → src/config.rs`, feature-DAG edits in `Cargo.toml`, `src/lib.rs`, `src/search/onnx/client_config.rs`, etc.) that correspond to Tasks 1–2 of `docs/plans/embed-merge-1.10.0.md`.
- **Action taken:** No merge/pull was performed. The remote contains nothing missing locally; merging would risk colliding with the other agent's uncommitted work. The evaluation below is therefore performed against the current (up-to-date-with-remote + in-flight-plan) tree, which is the correct base for post-1.10.0 feature planning. **The other agent's work was left untouched.**
- **Note on this document:** this report (`docs/findings/2026-08-01-hash-embeddings-warp-evaluation.md`) was deliberately **left unstaged** (no `git add`) so it cannot be mistaken for part of the other agent's in-flight changes.

---

## 4. Warp's "Hash Embeddings" — What It Actually Is

### 4.1 Where it lives

| Path | Role |
|---|---|
| `crates/ai/src/index/full_source_code_embedding/mod.rs` | Types: `Fragment`, `ContentHash`, `NodeHash`, `EmbeddingConfig`, error taxonomy |
| `.../merkle_tree/{hash,node,tree}.rs` | SHA-256 content Merkle tree; `MerkleHash` (32-byte), `ContentHash` (leaf=fragment), `NodeHash` (intermediate) |
| `.../chunker.rs` + `chunker/{semantic,naive}.rs` | tree-sitter semantic chunking w/ naive fallback (200 lines, ~12 KB) |
| `.../fragment_metadata.rs` | `leaf_hash → Vec<FragmentMetadata>` (file path + byte range + line range) |
| `.../sync_client.rs`, `.../store_client.rs` | Incremental sync; `StoreClient` trait (remote GraphQL impl in `app/src/server/server_api/ai.rs`) |
| `.../codebase_index.rs` | Retrieval orchestration: `get_relevant_fragments → hashes → metadata → build_fragments → rerank` |
| `crates/warp_graphql_schema/api/schema.graphql` | `GenerateCodeEmbeddingsInput`, `GetRelevantFragmentsInput` (+ server-side rerank) |
| `app/src/server/server_api/ai.rs` | GraphQL client: `generate_code_embeddings`, `get_relevant_fragments`, `rerank_fragments` |

### 4.2 The pipeline (as-built)

1. **Build a file tree** (respecting `.gitignore`, `.warpindexingignore`, `.cursorignore`, `.cursorindexingignore`, `.codeiumignore`; max depth 200; max file limit).
2. **Chunk every file into fragments.** Semantic chunker (tree-sitter, language from filename) splits AST nodes up to `MAX_BYTES_PER_CHUNK = 200 lines × 60 chars ≈ 12 KB`, then *coalesces* small fragments in reverse so a function's name stays with its body. Falls back to naive 200-line chunking on parser failure. `RETRIEVE_FRAGMENT_CONTEXT_LENGTH = 0` (they removed context lines — no quality gain, token cost).
3. **Build a SHA-256 Merkle tree over files→fragments.** Each fragment's content hash is a leaf; internal nodes hash child hashes; root node hash identifies the whole index snapshot.
4. **Sync incrementally.** Diff the tree against the last server-synced root; only *changed* fragments get re-embedded; unchanged fragments reuse embeddings (idempotent by content hash).
5. **Embed fragments (REMOTE in Warp).** `StoreClient::generate_embeddings` sends fragments to Warp's GraphQL API, which embeds with configurable models: `Voyage3_5_512` (default), `VoyageCode3_512`, `Voyage3_5_Lite_512`, or `OpenAiTextSmall3_256` (256-dim).
6. **Retrieve (REMOTE in Warp).** `get_relevant_fragments(query, embedding_config, root_hash, repo_metadata) → Vec<ContentHash>`. Server runs ANN over fragment embeddings; **the client passes the current root hash so retrieval is consistent with the synced index**.
7. **Map hashes → local metadata, build fragments, rerank (REMOTE).** `rerank_fragments(query, fragments)` via a cross-encoder; returns ordered fragments.
8. **Persist snapshots** with a 30-day shelf life; resume via snapshot; priority queue (active session > open session > persisted snapshot).

### 4.3 What "hash" means here — important correction

Web research (Warp docs, engineering blog, RuVector ADR-210 cross-reference) confirms: **Warp does not use feature-hashing or binary/LSH quantization for semantic search.** Its "hash embeddings" terminology refers to the **content-hash addressing of code fragments** (SHA-256) that makes the store deduplicated and incrementally syncable. Dense semantic embeddings are computed by real embedding models (Voyage/OpenAI) — remotely. Any "hash-embedding-as-binary-ANN" framing (Hamming/POPCOUNT, DHE, RaBitQ) is a *different* technique not present in Warp's client.

**Implication for LeIndex:** The transferable asset is the **content-hash fragment store + semantic chunking + tree-consistent incremental sync**, not a new embedding math. LeIndex already has both a dense local embedder (ONNX qwen3) and ANN machinery (HNSW, INT8 ADC, mmap brute-force) that Warp's client lacks.

---

## 5. LeIndex Current Search/Embedding Architecture (Baseline)

| Concern | Current LeIndex implementation | Files |
|---|---|---|
| Tokenizer | `tokenize_code`: camelCase/snake_case/acronym/digit splitting | `src/cli/index_builder/mod.rs` |
| Lexical layer | Inverted index (`token → node_ids`), text match, per-node token cache | `src/search/search/mod.rs` |
| TF-IDF embeddings | 768-dim, stratified IDF vocabulary, L2-normalized; persisted (`tfidf_embedder.bin`) | `src/cli/index_builder/tfidf.rs` |
| Neural embeddings | Local ONNX worker (`leindex-embed` process) `qwen3-embed-0.6b`, 1024-dim, IPC bincode, batch 256, mmap persistence (`embeddings.bin`) | `src/cli/index_builder/hybrid.rs`, `src/search/onnx/*`, `src/search/vector.rs` |
| ANN | `VectorIndex` (brute-force, mmap), `HNSWIndex`, `Int8HnswIndex` (ADC, ~74% memory cut), SIMD dot products | `src/search/{hnsw,vector}.rs`, `src/search/quantization/**` |
| Hybrid scoring | `HybridScorer`: tfidf 0.30 / neural 0.40 / structural 0.15 / text 0.15; `neural_weight` from config (0.3 default, 0.4 in-code) | `src/search/ranking.rs`, `src/search/search/mod.rs` |
| Rerank | **Local** cross-encoder rerank (`bge-reranker-base` via worker), top-80 pool | `src/cli/leindex/query.rs`, `src/cli/index_builder/hybrid.rs` |
| Retrieval routing | `query_route.rs`: ExactSymbol / ExactText / Semantic / DeepPdg; neural candidates unioned into candidate pool (top_k×10, ≥100) | `src/search/query_route.rs`, `src/search/search/mod.rs` |
| PDG conditioning | Per-node enriched content: type/lang, callers/callees counts, complexity, doc context, file-doc for FileSummary nodes | `src/cli/index_builder/mod.rs` |
| Content hashing | BLAKE3 file-level hashing for change detection; work-hoister dedups identical node content *within a run* | `src/cli/index_builder/mod.rs` |
| Freshness | PDG fingerprint (node/edge counts + hash), search snapshot hydration | `src/cli/index_builder/mod.rs` |
| Config | `leindex.toml`: `[neural]`, `[search] search_mode/hybrid/neural_weight/rerank_*`, `[indexing]` | `src/config.rs` (canonical, being finalized in 1.10.0) |

### 5.1 Key gaps relative to Warp's design

1. **No fragment layer.** Everything is indexed per PDG node. A 600-line function is one retrievable blob; a 5-line helper inside it is not separately retrievable. FileSummary nodes help file-level recall but cannot point at a region.
2. **Embedding idempotency is in-memory only.** `WorkHoister` dedups identical content within a single indexing run (`src/cli/index_builder/mod.rs` `work_hoister.lookup/store`). Across runs, unchanged nodes are re-tokenized and re-embedded (or re-restored from mmap, which is per-node-id not per-content-hash — a rename/regeneration invalidates it).
3. **No root-hash-consistent retrieval.** Freshness uses fingerprints, but query-time consistency (never retrieve against a half-synced tree) is not enforced at the same granularity Warp's root-hash contract provides.
4. **No tree-structured snapshot resume.** LeIndex snapshots search metadata, but diffing is file-level; Warp diffs the whole fragment tree in one pass and syncs only deltas.
5. **Ignore-list divergence.** Warp honors `.warpindexingignore`, `.cursorignore`, `.cursorindexingignore`, `.codeiumignore`; LeIndex honors gitignore + project config exclusions.

---

## 6. Evaluation: Value of Incorporating Warp's Approach into LeIndex (Localized)

### 6.1 What to take (high value)

**A. Content-hash fragment store with fragment-level embeddings (the headline item).**

- *What:* Split each PDG node's source (or the raw file) into tree-sitter-semantic fragments ≤ ~12 KB; compute `sha256(fragment)` as the storage key; store `content_hash → (embedding, byte_range, line_range, file_path)`.
- *Why it helps the stated goals:*
  - **Search more freely:** fragment units are ~10–50× smaller than PDG nodes, so conceptual queries can hit *inside* large functions, docs, or enum bodies. Retrieval is no longer limited to "which symbol" but "where in the symbol".
  - **More accurate results:** results map to exact byte ranges; the reranker (already local) operates on smaller, more precise candidates; duplicate identical fragments across files are indexed once.
  - **Dramatically cheaper incremental indexing:** content-hash cache → unchanged fragments never re-embed. This is the single biggest operational win for an agentic coding tool (re-index on every save).
- *Mapping to local stack:* chunker = tree-sitter (already a LeIndex dependency for parsing); hash = sha2 or blake3 (both already present); storage = bincode (already used) or the existing mmap layer; embeddings = existing local ONNX worker; retrieval = existing HNSW/mmap vector index; rerank = existing bge-reranker. **Zero new services.**

**B. Tree-consistent incremental sync (root-hash gating).**

- Reuse LeIndex's existing PDG fingerprint + file hashes to build a lightweight fragment tree, and expose the *root hash* at query time so a conceptual query never reads a mixed-generation index. Cheap, purely local, high correctness value for the MCP server's long-running daemon.

**C. Semantic-chunking discipline (coalescing).**

- LeIndex already strips comment markers and includes doc context — adopting Warp's reverse-coalesce so a fragment keeps name+body together improves embedding coherence. Also adopt `RETRIEVE_FRAGMENT_CONTEXT_LENGTH=0` finding (context lines added tokens without quality — LeIndex's `preceding_doc_context` is the doc analog, already capped at 24 lines).

### 6.2 What NOT to take

- **Warp's remote embedding/rerank service calls** — violates the localization constraint; LeIndex's local worker already replaces them.
- **Warp's `StoreClient` abstraction** — it's a remote-GraphQL seam; LeIndex's `EmbeddingClient`/`HybridEmbedder` is the local equivalent and should stay.
- **Voyage/OpenAI model configs** — no value locally; keep `qwen3-embed-0.6b` + optional `remote-embeddings` feature as the opt-in.
- **Binary/LSH "hash embeddings"** — not what Warp does; LeIndex's INT8 ADC + HNSW is already the right quantization story, and adding Hamming-binary ANN would trade accuracy for little gain given mmap brute-force + HNSW coverage.

### 6.3 Value scoring

| Criterion | Score (1–5) | Rationale |
|---|---|---|
| Improves semantic recall (concept queries) | 5 | Fragment units capture meaning at the right granularity; today's node blobs dilute it |
| Improves precision / result accuracy | 4 | Byte-range-exact results + dedup; rerank on cleaner candidates |
| "Search more freely" (retrievable unit space) | 5 | Sub-symbol, doc, and region retrieval becomes possible |
| Localizable with no new service | 5 | All deps present; worker + reranker already local |
| Operational cost reduction | 5 | Content-hash caching kills re-embedding of unchanged code |
| Effort / risk | 3 (moderate) | New store + chunking pass + migration of embeddings.bin layout; must not break 1.10.0 plan |
| Fit with existing architecture | 4 | Mirrors PDG conditioning; slotting as an additional "fragment index" next to node index is natural |

**Overall: strong buy as a localized enhancement.**

---

## 7. Concrete Integration Proposal (Phased, Localized)

### Phase 0 — Preconditions (must land first)
- Complete `docs/plans/embed-merge-1.10.0.md` (config unification → `src/config.rs`, worker under `src/embed/`, strict feature DAG). The fragment store should compile against the *post-merge* paths.

### Phase 1 — Content-hash fragment store (core)
1. New module `src/search/fragment/` (or `src/cli/index_builder/fragment/`):
   - `Fragment { content_hash: String, content: String, file_path, byte_range, line_range, node_id: Option<String> }`
   - `FragmentStore { content_hash → FragmentMetadata }` persisted via bincode at `.leindex/fragment_store.bin`.
2. Chunker: tree-sitter semantic chunking per file (reuse `parse` crate's language detection), coalescing like Warp; naive fallback.
3. Hashing: `sha256(fragment_content)` (or blake3) as key; keep BLAKE3 file hashes for file-level change detection.
4. Embedding: batch-embed only *new/changed* fragment contents via the existing worker; store `content_hash → embedding`; write a fragment-level mmap `fragments_embeddings.bin` mirroring the existing `embeddings.bin` format (or a v2 of it).
5. Config knobs (`[search]`): `fragment_index_enabled` (default off for 1.10.x, then on), `fragment_max_bytes` (default 12_000), `fragment_index_weight`.

### Phase 2 — Retrieval integration
- `SearchEngine` gains an optional fragment vector index (reuse `VectorIndexImpl`/`MmapVectorIndex`).
- Query path: run existing node-level retrieval **and** fragment-level retrieval; map fragment hits back to owning node (`node_id`) for ranking fusion, but *surface byte ranges* in results.
- Score fusion: `Score` gains an optional `fragment` component; default weight small (e.g., 0.10–0.15) so node-level ranking stays authoritative; reranker gets the unioned candidate pool (top-80 local rerank unchanged).

### Phase 3 — Tree-consistent sync + caching
- Compute a fragment-tree root hash per generation; store with the search snapshot.
- Incremental index: diff file hashes → changed files → re-chunk only those → embed only missing content hashes → update fragment store + root hash.
- Query-time guard: if a generation is mid-build, serve from last complete root (or flag `out_of_sync` like Warp's `out_of_sync_delay`).

### Phase 4 — Validation
- New benchmarks/tests: fragment recall on conceptual queries, dedup efficiency (identical fragments across files), incremental index cost (unchanged file → 0 re-embeds), exact-route non-regression, and mmap memory bounds. Follow AGENTS.md validation (`cargo fmt/clippy/test`, `docs/findings` evidence file).

---

## 8. Risks & Trade-offs

1. **Index size growth.** Fragments multiply the number of embeddings (1 node ≈ several fragments). Mitigation: content-hash dedup (identical code appears once), INT8 quantization (already present), opt-in flag.
2. **Storage format migration.** Existing `embeddings.bin` / `search_snapshot.bin` consumers (hydration, `restore_from_search_snapshot`) assume per-node rows. A fragment index must be additive (new file, new snapshot field) or versioned, never silently repurposed — mirrors the 1.10.0 discipline of rejecting stale generations.
3. **Token/CPU cost of chunking.** tree-sitter parse per file already happens in the parse phase; chunking should reuse the parsed tree to avoid a second full parse.
4. **Ranking fusion risk.** Naive fragment-score injection can drown node-level relevance. Keep fragment weight conservative and validate with the existing search benchmarks (`benches/search_benchmarks.rs`).
5. **Scope collision with 1.10.0 plan.** Do not start implementation until the embed-merge plan's Tasks 1–7 land (config paths, worker ownership, strict features) — otherwise the fragment module will need a second rewrite.

---

## 9. General Improvements to LeIndex (Identified During Review)

Beyond the hash-embedding evaluation, the following were observed. Each is small-to-medium; none require the Warp integration.

### 9.1 Correctness / hygiene
1. **Config default drift:** `src/config.rs` `default_neural_weight()` returns `0.3`, while `HybridScorer::for_code()` and `HybridScoringWeights::default()` use `0.4`. The CLI passes config's 0.3 via `set_neural_weight`, so the effective default differs from the documented scorer defaults. Pick one source of truth (prefer config; document the scorer default as "legacy"). *(Touched by in-flight plan — reconcile after merge.)*
2. **`src/graph/embedding.rs` is dead weight / stale:** `NodeEmbedding` claims "CodeRankEmbed" 768-dim while the real neural model is `qwen3-embed-0.6b` (1024-dim); `EmbeddingCache` uses FIFO eviction with a TODO saying "would use LRU in production" and is not on the search hot path. Either wire it into the fragment store (Phase 1) or delete it.
3. **`Score::new`/`HybridScorer::with_weights` legacy APIs** are deprecated but still referenced in tests; after 1.10.0, consider removing the deprecated shims to shrink surface (AGENTS.md zero-warning policy already keeps them compiling).

### 9.2 Performance
4. **Two tokenizers in the same pipeline:** `tokenize_code` (index_builder) vs the search engine's `split(|c| !is_alphanumeric())` (search/mod.rs `append_nodes`). They intentionally diverge, but a shared normalization helper would reduce surprise and let pre-tokenized tokens be reused (R8 already exists — extend it).
5. **`VectorIndex` (HashMap, brute-force) remains as default in `SearchEngine::new()`** while `MmapVectorIndex`/HNSW are the production paths; consider making `MmapVectorIndex` the default for non-test construction to avoid silent brute-force scans.
6. **Fragment-level dedup cache (this report's core idea) also serves the TF-IDF side:** the `TfIdfEmbedder::embed_tokens` cost is tiny, but the *tokenize+embed* call is repeated per run for unchanged content; a persisted content-hash→tfidf-vector cache would cut indexing CPU as well.

### 9.3 Product / DX
7. **Ignore-list compatibility:** honor `.cursorignore` / `.codeiumignore` / `.warpindexingignore` alongside `.gitignore` (Warp supports all four) — cheap, makes LeIndex a drop-in for cursor users. `.warpindexingignore` is trivially adopted.
8. **Expose "index freshness" as a query-time signal:** Warp surfaces `out_of_sync_delay`; LeIndex could report staleness in MCP `leindex_diagnostics`/search metadata so agents know retrieval reflects generation N vs disk.
9. **`rerank_top_n` default of 80** is well-justified (documented); consider making the reranker pool adaptive (e.g., scale with candidate count) rather than fixed, to bound latency on huge corpora.
10. **`EmbeddingConfig`-style model identity in cache keys** is already done (`search_cache_key_for` includes embed/rerank model) — extend the same discipline to the fragment store's cache key (model + quantization scheme) so a model swap never silently reuses stale fragment embeddings.

### 9.4 Documentation / repo hygiene (non-code)
11. `docs/findings/` currently holds a single dated investigation; adding this report follows the convention. Recommend a `docs/findings/` index (README) as the collection grows.
12. `TASKLIST.md` / `Tracker.md` are heavy manual ledgers; consider consolidating into the plan doc + automated gates once 1.10.0 ships (the AGENTS.md zero-tolerance policy makes CI the authoritative gate anyway).

---

## 10. Recommendation

1. **Adopt the fragment-level content-hash store as a post-1.10.0 feature** (opt-in `fragment_index_enabled`), reusing Warp's *architecture* (semantic chunking + SHA-256 content addressing + root-hash sync) but LeIndex's *local* embed/rerank/ANN stack.
2. **Do not port** Warp's remote service layer, Voyage/OpenAI configs, or binary-LSH framing.
3. **Sequence after `embed-merge-1.10.0` Tasks 1–7** to avoid double rewrites of config/worker paths.
4. Land the low-risk general improvements (config default drift, dead `graph/embedding.rs`, ignore-list compat, cache-key discipline) in the same release cycle.
5. Track as a new plan doc (`docs/plans/fragment-embeddings-1.11.0.md`) with the same rigor as `embed-merge-1.10.0.md` (audit annotations, non-negotiable invariants, stop conditions).

---

## 11. Appendix — Evidence Map

### Warp (hash-embedding architecture)
| Finding | Evidence |
|---|---|
| Fragment = content-hash leaf; SHA-256 Merkle tree | `crates/ai/src/index/full_source_code_embedding/merkle_tree/hash.rs` (`MerkleHash`, `ContentHash::from_content`) |
| Semantic chunking + coalescing, 200 lines/12 KB, naive fallback | `.../chunker.rs`, `.../chunker/semantic.rs`, `.../chunker/naive.rs` |
| No context lines on retrieval | `.../codebase_index.rs:78` `RETRIEVE_FRAGMENT_CONTEXT_LENGTH = 0` |
| Root-hash-consistent retrieval | `.../codebase_index.rs` `last_server_synced_root_node` used in `get_relevant_fragments`; `.../manager.rs` `out_of_sync_delay` |
| Remote embedding models | `.../mod.rs` `EmbeddingConfig::{Voyage3_5_512, VoyageCode3_512, Voyage3_5_Lite_512, OpenAiTextSmall3_256}` |
| Remote store seam (GraphQL) | `.../store_client.rs` trait; `app/src/server/server_api/ai.rs:1377-1395` `GenerateCodeEmbeddings` |
| Schema | `crates/warp_graphql_schema/api/schema.graphql:1341-1358,1557-1573` |
| Ignore-list support | `.../codebase_index.rs` `SUPPORTED_IGNORES` (4 entries) |
| Snapshot shelf life / resume | `.../snapshot.rs` (30-day), `.../priority_queue.rs` (active>open>persisted) |

### LeIndex (baseline)
| Finding | Evidence |
|---|---|
| TF-IDF 768-dim stratified embedder | `src/cli/index_builder/tfidf.rs` |
| Neural 1024-dim qwen3 via worker | `src/cli/index_builder/hybrid.rs` (`NEURAL_EMBEDDING_DIMENSION = 1024`), `src/search/onnx/*` |
| Per-node enriched content + doc context | `src/cli/index_builder/mod.rs` (`enriched_node_content`, `preceding_doc_context`) |
| Work-hoister (in-run dedup only) | `src/cli/index_builder/mod.rs` (`build_indexed_node`, `WorkHoister`) |
| mmap embeddings + snapshot hydration | `src/search/vector.rs` (`MmapEmbeddingIndex`), `src/search/search/mod.rs` (`restore_from_search_snapshot`) |
| Hybrid scoring / neural_weight | `src/search/ranking.rs`, `src/search/search/mod.rs` (`set_neural_weight`) |
| INT8 ADC HNSW + SIMD | `src/search/quantization/**` |
| Local rerank (bge-reranker-base) | `src/cli/leindex/query.rs` (`rerank_cfg`), `src/cli/index_builder/hybrid.rs` (`rerank_blocking`) |
| Config canonical schema | `src/config.rs` (`[search] search_mode/neural_weight/rerank_*`) |
| 1.10.0 in-flight plan | `docs/plans/embed-merge-1.10.0.md` (Tasks 1–2 in progress per git status) |
