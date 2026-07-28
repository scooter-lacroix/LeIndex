# Master Remediation Plan — LeIndexer CI Quality Gates

## Summary

Three CI quality gates in `.github/workflows/quality.yml` are failing: **Cyclomatic Complexity** (`lizard -CCN 15`), **Large Files** (`wc -l` > 2000), and **Duplicate Code** (`jscpd --min-lines 6 --threshold 5`, currently 8.66% / 712 clones).

**End state:** all three gates green.

| Workstream | Targets | Net change | Est. effort |
|---|---|---|---|
| Complexity (CCN ≤ 15) | 6 functions | 11 small private helpers extracted across 3 files | S (1–2 days) |
| Large Files (< 2000) | 13 files | 11 files split into ~55 new files; 2 GAPS need plans first | L (2–3 weeks) |
| Duplicate (< 5%) | 7 clusters | ~2,324 lines removed via 1 new `helpers.rs`, 1 new `hnsw_common.rs`, plus intra-file helpers | L (2–3 weeks) |

**Verified facts (reconciled against the working tree):**
- Gate thresholds confirmed in `.github/workflows/quality.yml`: `lizard -CCN 15`, `jscpd --min-lines 6 --threshold 5`, `MAX_LINES=2000`.
- **`indexing.rs` (2156 lines)** in the large-file list is `src/cli/leindex/indexing.rs` — **no split sub-plan was provided. GAP.**
- **`index_builder.rs` (3717 lines)** has a dedupe sub-plan but **no split sub-plan. GAP.**
- **`global_symbols.rs`**: the dedupe agent aborted with "file not found" because it used a wrong path. Actual location: `src/storage/global_symbols.rs` (986 lines — **under** the 2000-line gate; it is a **dup-only** target, not a large-file target). Confirmed heavy `as_str`/`from_str_name` enum repetition + SQL upsert/resolve boilerplate.
- External callers of items being moved, verified: `crate::cli::cli::collect_ort_diagnostics()` (used in `src/cli/mcp/diagnostics_handler.rs:149`), `crate::search::onnx::client::migraphx_cache_path` (used in `src/cli/cli.rs:2006`).

---

## Workstream 1 — Complexity (CCN ≤ 15)

6 functions exceed CCN 15. All fixes are pure private-helper extractions; zero behavior change, zero public-API change.

### 1a. `search_internal` — `src/cli/leindex/query.rs` (CCN 43 → 10)
Extract 3 private helpers (same module):
- `fn enrich_results_with_pdg_metadata(&mut [SearchResult], &Pdg, &Self)` — from lines **163-232** — populates symbol_type/line_number/caller_count/dependency_count. est CCN 14.
- `fn rerank_results(&mut Vec<SearchResult>, query, &Pdg, &Self, &LeIndexConfig.search)` — from lines **233-282** — cross-encoder rerank via `embedder.rerank_blocking`. est CCN 11.
- `fn cache_search_results(&[SearchResult], key, &CacheSpiller)` — from lines **293-312**. est CCN 2.
Orchestrator retains: empty-index early-return, cache hit, query-type routing, threshold/rerank config, SearchQuery construction, pdg `if let`, truncation, caching dispatch (~10 branches).

### 1b. `dashboard_overview` — `src/server/handlers.rs` (CCN 26 → 4)
Extract 3 private helpers (each takes `&Connection`):
- `fn query_codebase_metrics(&Connection) -> ApiResult<Vec<DashboardCodebaseMetricsResponse>>` — from lines **315-395**. est CCN 12.
- `fn query_language_distribution(&Connection) -> ApiResult<Vec<LanguageDistributionResponse>>` — from lines **399-427**. est CCN 5.
- `fn compute_cache_temperature(Option<f64>, i64) -> String` — from lines **437-450**. est CCN 5.
Orchestrator reduces to: lock → 3 helper calls → 3 trivial COUNT queries → build response.

### 1c. `get_codebase` — `src/server/handlers.rs` (CCN 20 → 8)
- `fn row_to_codebase_response(&rusqlite::Row<'_>) -> rusqlite::Result<CodebaseResponse>` — from lines **234-283**. Uses plain `?` (rusqlite::Error); caller adds one `.map_err` to convert to `ApiError`. Mirrors the existing `list_codebases` `query_map` closure pattern. Note: helper CCN is 16 (one per `row.get` `?`); acceptable — the gate measures the function in the report, and this helper is a straight-line field read, not a logic branch.

### 1d. `list_codebases` — `src/server/handlers.rs` (CCN 18 → 4)
- **Reuse the same `row_to_codebase_response` helper from 1c** (it already handles the 11-column read + `last_indexed` fallback). The `query_map` closure body (lines **131-151**) becomes `|row| row_to_codebase_response(row)`. Residual: 4 outer `?` (lock, prepare, query_map, collect).

### 1e. `get_graph` — `src/server/handlers.rs` (CCN 16 → ~6)
- `fn query_graph_nodes(&Connection, &str) -> ApiResult<Vec<GraphNodeResponse>>` — from lines **525-561**.
- `fn query_graph_links(&Connection, &str) -> ApiResult<Vec<GraphLinkResponse>>` — from lines **563-599**.
Each removes 3 branch points (prepare + query_map + collect).

### 1f. `list_projects` — `src/global/registry.rs` (CCN 16 → 4)
- `fn row_to_project_info(&rusqlite::Row<'_>) -> rusqlite::Result<ProjectInfo>` — extract the `query_map` closure body (lines **263-279**). 10 `?` + one `ok_or_else`. **Bonus (optional):** the same helper dedupes the identical row-mapping in `get_project` (lines 306-321) and `find_by_fingerprint` (lines 369-385) — not required for this CCN fix.

**Verification:** `lizard -CCN 15` on `src/`, `src/cli/leindex/query.rs`, `src/global/registry.rs` returns 0 violations.

---

## Workstream 2 — Large Files (< 2000 lines)

13 files over the limit. Each split is a **pure move/re-export refactor**: convert `foo.rs` → `foo/mod.rs`, move line ranges to new sibling files, re-export all previously-`pub` items from `mod.rs` so every external import path stays identical. Multiple `impl Type` blocks across sibling files is idiomatic Rust.

Standard pattern for every split (do not deviate):
1. `git mv foo.rs foo/mod.rs` (or create `foo/` dir + `foo/mod.rs`).
2. Cut the cited line ranges into the new sibling files; each gets `use super::*` (or `use crate::graph::pdg::*` etc.) for the imports it needs.
3. Make moved private items `pub(super)` if siblings/tests need them.
4. Add `mod <sibling>;` declarations + `pub use` / `pub(crate) use` re-exports in `mod.rs` for everything previously reachable at the old path.
5. Tests move with their code (inline `#[cfg(test)] mod test` in the relevant sibling), EXCEPT tests using `file!()` or `super::*`-spanning assertions — those stay in `mod.rs`.

### Per-target splits (with line ranges)

| File (current) | Lines | New modules | Key cuts | Post mod.rs est |
|---|---|---|---|---|
| `src/search/search.rs` | 5809 | 10 new files (see below) | all production code | ~200 (+2401 test module) |
| `src/cli/leindex/setup.rs` | 4104 | `setup/{types,ort,model,tests}.rs` + `setup/mod.rs` | L3073-4104 tests | <700 |
| `src/cli/index_builder.rs` | 3717 | **GAP — needs split plan** | TBD (see 2-GAP1) | <2000 |
| `src/graph/extraction.rs` | 3333 | `extraction/{inheritance,call_edges,cross_file,imports,tests}.rs` + `extraction/mod.rs` | L2412-3333 tests | ~480 |
| `src/cli/mcp/output/render.rs` | 2935 | `render/{tree,search,symbol,analysis,git_status,edit,formatters}.rs` + `render/mod.rs` | L2290-2935 tests | ~390 |
| `src/search/onnx/client.rs` | 2772 | `client/{daemon,config}.rs` + `client/mod.rs` | L26-552, 809-858, 1321-1860 → daemon | ~1050 |
| `src/cli/cli.rs` | 2571 | `cli/{mcp_stdio,mcp_socket,mcp_tools,diagnostics_cmd,setup_cmd,cleanup_cmd}.rs` + `cli/mod.rs` | L1274-1785, 1886-1988, 2032-2257 | ~1549 |
| `crates/leindex-embed/src/runtime.rs` | 2458 | `runtime/{config,onnx_init,embed,rerank,tests}.rs` + `runtime/mod.rs` | L1947-2458 tests | ~490 |
| `src/cli/mcp/server.rs` | 2313 | `mcp/{prompts_resources,socket_transport}.rs` (server.rs stays) | L1154-1512 socket; L2013-2313 prompts | ~1653 |
| `src/graph/external_deps.rs` | 2211 | `external_deps/{types,parsers,builtins,discover}.rs` + `external_deps/mod.rs` | L97-901 parsers | ~960 |
| `src/cli/leindex/indexing.rs` | 2156 | **GAP — needs split plan** | TBD (see 2-GAP2) | <2000 |
| `src/graph/pdg.rs` | 2116 | `pdg/{serialize,tests}.rs` + `pdg/mod.rs` | L480-659 serialize; L1680-2116 tests | ~1490 |
| `src/cli/mcp/output/trim.rs` | 2083 | `trim/tests.rs` (via `#[path]`) + `trim/mod.rs` | L903-2083 tests | 902 |

#### `search.rs` — 10-way split (the big one)
Convert `src/search/search.rs` → `src/search/mod.rs` (the existing `src/search/mod.rs` `pub use search::{...}` re-export at line 33-37 keeps working because `search` becomes a directory). New sibling files under `src/search/`:

| New file | Responsibility | Source ranges |
|---|---|---|
| `indexing.rs` | Content pruning, admission gating, work hoisting (ContentPruner, IndexingAdmissionGate, WorkHoister) | 28-54, 56-266, 268-388 |
| `vector_impl.rs` | MmapVectorIndex, VectorIndexImpl, VectorIndexError | 390-660 |
| `node_info.rs` | NodeInfo + serde compat, CompactTokenIndex, CompactNodeMetadata | 662-1090 |
| `query_types.rs` | SearchQuery, SearchResult, TextQueryPreprocessed, EntryType, SemanticEntry, Error | 853-926, 928-1002, 3365-3406 |
| `int8_quality.rs` | Int8QualityThresholds/Report/Gate/PromotionDecision | 1248-1498 |
| `staged_retrieval.rs` | StagedRetrievalConfig/Metrics, SearchSnapshot, cache consts | 1122-1246, 1153-1155 |
| `engine_index.rs` | SearchEngine write-path methods (new, index_nodes, append_nodes, incremental_reindex, add/remove_node, clear, Default) | 1501-1997, 3357-3363 |
| `engine_snapshot.rs` | search_snapshot, restore_from_search_snapshot | 2043-2210 |
| `engine_search.rs` | search(), search_staged(), compute_score, text score, semantic_search, is_archive_path, estimate_bytes | 2395-3347 |
| `engine_accessors.rs` | SearchEngine struct def + field list + all accessors (node_count, validate_coherence, HNSW/int8 toggles, memory, neural methods) | 1092-1121, 2211-2394, 3068-3347 |

Re-export strategy in new `mod.rs`:
- `pub use` for the 16 items already re-exported from `src/search/mod.rs` (CompactNodeMetadata, ContentPruner, IndexingAdmissionGate, Int8* ×4, NodeInfo, PruningDecision, SearchEngine, SearchQuery, SearchResult, SemanticEntry, StagedRetrievalConfig/Metrics, WorkHoister).
- `pub(crate) use` for SearchSnapshot, TextIndexDelta, EntryType, DEFAULT_EMBEDDING_DIMENSION, MmapVectorIndex, VectorIndexImpl, VectorIndexError, TextQueryPreprocessed.
- The 2401-line test module (L3409-5809) stays in `mod.rs` via `use super::*`. It is borderline for the gate; if it fails, split into `engine_tests.rs` + `pruner_tests.rs` in a follow-up.

### 2-GAP1 — `src/cli/index_builder.rs` (3717) — SPLIT NEEDED, no sub-plan
The dedupe sub-plan (index-builder-intra, Workstream 3) targets this file but does NOT split it below 2000 lines. **A split is required.** Sketch (to be refined before execution):
- `src/cli/index_builder/mod.rs` — `IndexBuilder` struct + ctor + `index_*` orchestration entry points + re-exports.
- `src/cli/index_builder/tfidf.rs` — `TfIdfEmbedder::{build_from_tokens, from_document_frequencies}` (L365-450), HybridEmbedder dispatch, embed/embed_tokens.
- `src/cli/index_builder/scan.rs` — `scan_git_project_files`, `scan_non_git_project_files`, `finalize_project_scan` helper (L1256-1281, 1388-1427).
- `src/cli/index_builder/mmap_load.rs` — `try_load_mmap_embeddings_from_storage`, `try_load_neural_mmap_embeddings_from_storage`, `load_mmap_index` helper (L2398-2424, 2495-2521), `try_load_search_snapshot`.
- `src/cli/index_builder/embed.rs` — `index_nodes_with_embedder_inner` (L1630-1682) + neural EmbedResult handling.
- Tests (whatever their current span) split alongside their unit-under-test.
This split + the Workstream-3 intra-file dedup should be **one combined PR**.

### 2-GAP2 — `src/cli/leindex/indexing.rs` (2156) — NO sub-plan
Verified structure: `impl IndexPipelineState` (L63), free fns `injected_phase_failure`/`add_submodule_summary_nodes`/`progress_*` (L110-205), then a **~1900-line `impl LeIndex` block (L205-2105)** holding the indexing pipeline methods, then `#[cfg(test)] mod tests` (L2117-2156). Only 83 lines over the gate. Minimum viable extraction: lift the test module to `src/cli/leindex/indexing/tests.rs` and pull 1-2 cohesive method groups (e.g. submodule-summary-node construction, git-tree-oid helpers) into siblings. Provide a real plan before starting.

**Verification:** `wc -l` every tracked `.rs`; `find src crates -name '*.rs' | xargs wc -l | sort -rn | awk '$1>2000'` returns empty.

---

## Workstream 3 — Duplicate Code (< 5%)

7 clusters. 6 are low-risk intra-file refactors; 1 (parse-framework) is medium-risk and goes last.

### 3a. `parse-framework` cluster — MEDIUM risk — **own PR, do LAST**
New file `src/parse/helpers.rs` (~100 lines), 5 shared items:
1. `pub fn find_node_by_id(node: &Node, id: usize) -> Option<Node>` — BFS lookup, identical in 14 files.
2. `pub fn clean_call_text(raw: &str) -> String` — strips trailing `(...)`, identical in 9 files.
3. `pub fn add_import(imports: &mut Vec<ImportInfo>, path: &str, alias: Option<String>)` — trim/normalize/push, ~identical in 10 files.
4. `pub fn parse_source(lang_fn: impl Fn() -> Language, source: &[u8], lang_name: &str) -> Result<Node>` — set_language + parse + error boilerplate.
5. `pub struct CfgBuilder<'a>` with `new`/`create_block`/`add_statement_to_block`/`finish`/`handle_if_statement`/`handle_loop_statement` — 55 lines char-for-char identical in 11 files.
`build_cfg_recursive` (language-specific kind dispatch) stays per-file. Call sites to update: all 16 files in `src/parse/` (cpp.rs hub, csharp.rs, java.rs, php.rs, rust.rs, javascript.rs, scala.rs, go.rs, python.rs, ruby.rs, lua.rs, c.rs, bash.rs, kotlin.rs, dart.rs, swift.rs, mod.rs). **Ship Layer 1 first** (mechanical copy-paste: items 1-3 + CfgBuilder core — ~700 lines), measure, then decide on Layer 2 (`parse_source` + trait-impl shrink — ~700 lines, structural). Leave `extract_*_definitions`, `calculate_complexity`, full `build_cfg_recursive` alone — genuinely language-specific.

### 3b. `search-rs-intra` — combine with the `search.rs` split PR (Workstream 2)
Private helpers on `SearchEngine` (land in `engine_search.rs`):
- `fn score_and_collect(node: &NodeInfo, query: &SearchQuery, text_query: &TextQueryPreprocessed, vector_results: &HashMap<String, f32>) -> Option<SearchResult>` — replaces L2546-2598 (`search`) and L2787-2839 (`search_staged`): text_score + tfidf + skip-zero + compute_score + threshold + 16-field SearchResult literal.
- `fn finalize_results(results: Vec<SearchResult>, top_k: usize, cache_key: String, cache: &mut LruCache<String, Vec<SearchResult>>, cache_bytes: &mut usize) -> Vec<SearchResult>` — replaces L2600-2640 and L2843-2881: sort-desc + truncate + rank-assign + byte-budget LRU eviction.
- Test helpers in `mod tests`: `fn test_node(id, content, tfidf, byte_range, complexity) -> NodeInfo` and `fn test_query(query, top_k) -> SearchQuery` — collapse 39 NodeInfo + 24 SearchQuery literals (~278 lines).

### 3c. `extraction-intra` — combine with the `extraction.rs` split PR
Private, top of `extraction/mod.rs`:
- `struct SymbolResolver { exact, last, suffix: HashMap<String, Vec<NodeId>> }` + `fn resolve(&self, name) -> Vec<NodeId>` (exact → suffix 2-3 seg → last). Replaces resolve logic in `extract_call_edges` (L883-983), `resolve_cross_file_call_edges_inner` (L1237-1424), `resolve_flow_target` (L1160-1172), `resolve_cross_file_flow_targets` (L1754-1780), `resolve_import_targets` (L2304-2335).
- `fn expand_call_candidates(call_target, alias_map, caller_ns) -> Vec<String>` — import-alias + self/super/crate/base expansion.
- Optional bonus: `fn check_super_call_evidence(methods: &[&SignatureInfo], other_class: &str) -> f32` for the duplicated super-call block at L671-719 (~25 lines).

### 3d. `index-builder-intra` — combine with the `index_builder.rs` split PR (2-GAP1)
3 private helpers in the new `index_builder/` modules:
1. `TfIdfEmbedder::from_document_frequencies(df, n_docs, node_count, edge_count) -> Self` — unifies `build_from_tokens` (L391-450) and `index_nodes_with_embedder_inner` (L1630-1682). Existing test `test_tfidf_partition_matches_sort_selection` (L2755) verifies equivalence.
2. `fn finalize_project_scan(source_paths, manifest_paths, project_path) -> ProjectFileScan` — unifies `scan_git_project_files` (L1256-1281) + `scan_non_git_project_files` (L1388-1427).
3. `fn load_mmap_index(storage_path, filename, label) -> Option<MmapEmbeddingIndex>` — unifies `try_load_mmap_embeddings_from_storage` (L2495-2521) + `try_load_neural_mmap_embeddings_from_storage` (L2398-2424).

### 3e. `global-symbols-intra` — **PATH CORRECTION** (sub-plan was wrong)
The agent aborted on "file not found". Real path: **`src/storage/global_symbols.rs`** (986 lines — dup-only, under line limit). Confirmed duplication: multiple `enum … { … fn as_str(&self) -> &'static str { … } fn from_str_name(s: &str) -> Option<Self> { … } }` blocks (L60-184 show at least 3 identical enum-impl pairs) + repeated SQL upsert/resolve boilerplate in `GlobalSymbolStore` methods (upsert_symbol L229, upsert_symbols_batch L267, resolve_by_name L327, resolve_by_name_and_type L365, get_symbol L407, get_project_symbols L447, add_external_ref L488, get_outgoing_refs L511, get_incoming_refs L546, add_project_dep L581). Plan before executing: extract a `fn map_row_to_global_symbol(&rusqlite::Row) -> rusqlite::Result<GlobalSymbol>` (mirrors Workstream-1 pattern) and a small `enum_str!` macro (or `const NAME: &[(&str, Variant)]` table + single generic `as_str`/`from_str_name`) for the enum boilerplate. Keep it private to `storage/`.

### 3f. `hnsw-dup`
New file `src/search/hnsw_common.rs` (~60 lines):
- `struct HnswIdMap { id_map, reverse_map, deleted, next_id, count }` + `remove`/`len`/`is_empty`/`alloc_id`/`mark_inserted` + free fn `rank_results(neighbours) -> Vec<(String, f32)>`.
- `struct HnswCommonParams { m, ef_construction, ef_search, max_elements, max_layer }` + Default/builder/`validate()`. `HNSWParams` wraps it `+ quantized: bool`; `Int8HnswParams` wraps it `+ metric: AdcDistanceMetric`.
- **Delete `Int8HnswError`**, reuse `hnsw::IndexError` everywhere (zero external callers; `search.rs` maps both via `.to_string()`).
Update: `src/search/hnsw.rs`, `src/search/quantization/int8_hnsw.rs`, `src/search/mod.rs`, `src/search/search.rs`. `search()` + `estimated_memory_bytes()` stay per-impl (genuinely differ).

**Verification:** `npx jscpd@3.5.10 --min-lines 6 --threshold 5 --output ./jscpd-report .` reports total < 5%.

---

## Cross-Cutting Notes

- **`search.rs` split serves two gates.** Converting it to `mod.rs` clears the large-file gate (5809 → ~200 + siblings), and the `search-rs-intra` dedup helpers (`score_and_collect`, `finalize_results`) land naturally in the new `engine_search.rs`. **Do both in one PR.**
- **`extraction.rs` split serves two gates.** Split clears the large-file gate (3333 → ~480 + siblings) and the `SymbolResolver`/`expand_call_candidates` helpers live in the new `extraction/mod.rs`. **One PR.**
- **`index_builder.rs` serves two gates AND has a planning gap.** Needs both a split (2-GAP1) and the intra-file dedup (3d). Plan the split first, then land split+dedup as **one PR**.
- **`global_symbols.rs` is dup-only.** The sub-plan's "file not found" is a wrong-path bug, not a real gap. Correct path: `src/storage/global_symbols.rs` (986 lines). Do not treat it as a large-file target.
- **Two real large-file planning gaps** need a sub-plan before execution: `index_builder.rs` (sketch provided, 2-GAP1) and `leindex/indexing.rs` (sketch provided, 2-GAP2).
- **Re-export discipline is the only cross-cutting risk** across all splits. Every `pub` item moved to a sibling must be re-exported from the new `mod.rs` so `crate::<module>::<Item>` paths stay valid. The two verified external-path dependencies (`crate::cli::cli::collect_ort_diagnostics` in `diagnostics_handler.rs:149`; `crate::search::onnx::client::migraphx_cache_path` in `cli.rs:2006`) must each get a `pub(crate) use` / `pub use` in their new `mod.rs`.

---

## Sequencing & Merge Order

Recommended: **one PR per file or per cohesive cluster**, smallest/lowest-risk first, so every PR keeps CI green. Items in the same row can go in parallel (different files, no shared state).

| # | PR | Gate(s) | Depends on | Parallelizable |
|---|---|---|---|---|
| 1 | `trim.rs` test-lift | Large | — | yes |
| 2 | `pdg.rs` split (serialize + tests) | Large | — | yes |
| 3 | Complexity batch (handlers.rs ×4 fns, query.rs, registry.rs) | Complexity | — | yes |
| 4 | `global_symbols.rs` dedupe (path-corrected, storage/) | Dup | — | yes |
| 5 | `external_deps.rs` split | Large | — | yes |
| 6 | `runtime.rs` split | Large | — | yes |
| 7 | `mcp/server.rs` split | Large | — | yes |
| 8 | `render.rs` split | Large | — | yes |
| 9 | `cli.rs` split | Large | — | yes |
| 10 | `onnx/client.rs` split | Large | — | yes |
| 11 | `hnsw` dedupe (new `hnsw_common.rs`) | Dup | — | yes |
| 12 | `setup.rs` split | Large | — | yes |
| 13 | **`search.rs` split + search-rs-intra dedupe (combined)** | Large + Dup | — | yes |
| 14 | **`extraction.rs` split + extraction-intra dedupe (combined)** | Large + Dup | — | yes |
| 15 | **`index_builder.rs` split (2-GAP1) + index-builder-intra dedupe (combined)** | Large + Dup | needs split plan first | yes (after plan) |
| 16 | **`leindex/indexing.rs` split (2-GAP2)** | Large | needs split plan first | yes (after plan) |
| 17 | **`parse-framework` dedupe — Layer 1 only** ⚠ highest risk | Dup | do LAST, own PR | no |

**Highest-risk item: the parse-framework cluster (#17).** It touches 16 language-parser files that are on the critical path of every parse/CFG/signature operation. Ship Layer 1 (mechanical `find_node_by_id`/`clean_call_text`/`add_import`/`CfgBuilder` core) only; do not attempt Layer 2 (trait-impl structural merge) until Layer 1 is merged and measured. Recommend as the final PR so any regression is isolated and the other gates are already green.

**Fully independent (can start day 1, in parallel):** rows 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12.
**Combined PRs (split + intra-file dedup share the same touched lines):** rows 13, 14, 15.
**Blocked on a missing plan:** rows 15, 16 (produce the sub-plan, then unblock).
**Serial, last:** row 17.

---

## Verification Strategy (per workstream)

Run the exact CI commands locally before opening each PR.

**Complexity:** `lizard -CCN 15` over `src/` and `crates/` — expect 0 violations. Targeted check: `lizard src/cli/leindex/query.rs src/server/handlers.rs src/global/registry.rs -CCN 15`.

**Large files:** `find src crates -name '*.rs' -not -path '*/target/*' | xargs wc -l | sort -rn | awk '$1>2000 {print}'` — expect empty. Also enforce 1MB size cap (gate checks both).

**Duplicate:** `npx --yes jscpd@3.5.10 --min-lines 6 --threshold 5 --output ./jscpd-report .` then read `./jscpd-report/jscpd-report/json/jscpd-report.json` → `statistics.percentage` must be < 5. Re-run after each dedup PR; the dominant contributor is the parse-framework cluster, so expect the biggest single drop after row 17.

**Cross-cutting (every split PR):** `cargo check --all-targets` + `cargo test` must pass unchanged (pure move/re-export = zero behavior change). Confirm the two verified external paths still resolve: `grep -rn "crate::cli::cli::collect_ort_diagnostics\|crate::search::onnx::client::migraphx_cache_path" src/` returns the same call sites with no compile error.

---

## Appendix — GAP refills (concrete, from current working-tree structure)

The workflow's two split sub-plans failed on a rate-limit (429). Refilled below with the actual current structure of both files (verified by top-level inventory).

### 2-GAP1 refill — `src/cli/index_builder.rs` (3717 → ~1100 mod.rs)

Convert `src/cli/index_builder.rs` → `src/cli/index_builder/mod.rs`:

| Lines | Item(s) | Move to |
|---|---|---|
| 1-131 | imports; `enriched_node_content` + `NodeContentExt` | **stay** (mod.rs) |
| 132-319 | `preceding_doc_context`, `strip_comment_syntax`, `leading_file_doc`, `is_external_node_excluded` | `doc_context.rs` (~190) |
| 320-625 | `TfIdfPersistedState`, `TfIdfEmbedder` struct + full impl | `tfidf.rs` (~305) |
| 627-1140 | `HybridEmbedder` enum, `HybridScoringWeights` + Default + impls | `hybrid.rs` (~515) |
| 1142-1208 | `FileReadCache` | `file_cache.rs` (~67) |
| 1210-1575 | `scan_git_project_files`, `scan_non_git_project_files`, `scan_project_files` | `scan.rs` (~365) |
| 1576-2189 | `index_nodes_with_embedder_inner`, `append_neural_batch`, `index_nodes`, `index_nodes_tfidf_only` | `embed.rs` (~615) |
| 2191-2471 | `search_snapshot_path`, `neural_mmap_embeddings_path`, `try_load_*_mmap_embeddings_from_storage`, `load_mmap_index`, `try_load_search_snapshot`, `Blake3FormatWriter` | `mmap_load.rs` (~280) |
| 2472-3717 | `sanitize_for_prefix`, `clear_query_caches`/`_for_project`, `#[cfg(test)] mod tests` | `util.rs` + `tests.rs` |

`mod.rs` keeps imports + `enriched_node_content`/`NodeContentExt` + `pub use {doc_context::*, tfidf::*, hybrid::*, ...}` for every previously-`pub` item. Est mod.rs ≈ 1100. **Combine with Workstream-3 `index-builder-intra` dedup (3d)** in the same PR — the dedup helpers (`from_document_frequencies`, `finalize_project_scan`, `load_mmap_index`) land naturally in `tfidf.rs` / `scan.rs` / `mmap_load.rs`.

### 2-GAP2 refill — `src/cli/leindex/indexing.rs` (2156 → ~1900 mod.rs)

Only 156 lines over. Minimum viable extraction:

| Lines | Item(s) | Move to |
|---|---|---|
| 1-109 | imports, `IndexPipelineState` struct + impl | **stay** |
| 110-204 | `injected_phase_failure`, `add_submodule_summary_nodes`, `progress_stderr`, `progress_clear` | `progress.rs` (~95, `pub(super)`) |
| 205-2103 | `impl LeIndex` pipeline (scan/parse/pdg/lexical/neural/publish/reindex) | **stay** |
| 2105-2115 | `git_tree_oid` free fn | `progress.rs` (~11) |
| 2117-2156 | `#[cfg(test)] mod tests` | `indexing/tests.rs` via `#[path = "tests.rs"]` (-40) |
| (within impl) | `load_from_storage*` family (`load_from_active_storage`, `load_pdg_from_storage`, `load_from_storage_inner[_at]`, `load_from_mutable_storage`) | `load.rs` (~110) |

Net: 2156 − 95 − 11 − 40 − 110 ≈ **1900**. `mod.rs` adds `mod progress; mod load;` + `#[cfg(test)] #[path = "tests.rs"] mod tests;`. `impl LeIndex` methods in `load.rs` stay `pub(crate)` (module is `pub(crate) mod indexing`, so paths resolve unchanged).
