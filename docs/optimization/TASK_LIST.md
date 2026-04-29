# LeIndexer Optimization - Blocking Task List

**Spec Bible**: SPEC_BIBLE.md  
**Branch**: `feature/unified-crate`  
**Base commit**: `cf2d145`  
**Total tasks**: 28 across 8 phases  

---

## Phase 1: Hygiene & Cleanup (3 tasks)

- [ ] **T01**: Delete stale root artifacts (err2.txt, errors.txt, parse_errors.py, split_handlers.py, truncate_handlers_rs.py, fix_helpers_and_tests.py, extract_tests.py, check*.txt, check_out.json) from project root
- [ ] **T02**: Remove unused imports from `src/cli/mcp/handlers.rs` (E5)
- [ ] **T03**: Consolidate `test_registry_for()` function - move from 8 test modules into single `pub(crate)` fn in `src/cli/mcp/helpers.rs` (B1)

---

## Phase 2: Structural Refactoring (4 tasks)

- [ ] **T04**: Create `dispatch_handler!` macro to replace `ToolHandler` enum's 80-match-arm arms (B3)
- [ ] **T05**: Split `src/edit/mod.rs` (2487 lines) into submodules: `engine.rs`, `command.rs`, `history.rs`, `refactor.rs` (B4)
- [ ] **T06**: Deduplicate `replace_whole_word()` - remove from `edit/mod.rs`, ensure `helpers::` version is used everywhere (B5)
- [ ] **T07**: Replace glob imports (`use super::helpers::*`) with explicit imports in all handler files (E1)

---

## Phase 3: Rewrite Integration (3 tasks) ⚠️ DEPENDENCY CHAIN

- [ ] **T08**: Integrate `pdg_rewrite.rs` - Add `EmbeddingStore` field + `remove_file()` + `add_import_edges()` + `add_inheritance_edges()` to `src/graph/pdg.rs` (1.1)
- [ ] **T09**: Integrate `extraction_rewrite.rs` - Replace `extract_pdg_from_signatures()` body in `src/graph/extraction.rs` with 5-phase pipeline, port tests (1.2) - requires T08
- [ ] **T10**: Integrate `pdg_utils_rewrite.rs` - Replace `src/phase/pdg_utils.rs` with rewrite, update callers to pass `source_bytes`, port tests (1.3) - requires T09

---

## Phase 4: Search Engine Optimization (4 tasks)

- [ ] **T11**: Add `node_id_to_idx: HashMap<String, usize>` to `SearchEngine`, refactor `semantic_search()` to use O(1) lookup (A1)
- [ ] **T12**: Restructure `index_nodes()` to take ownership of `nodes` vec, clone only embeddings (A4)
- [ ] **T13**: Clear `NodeInfo.content` after building inverted index in `extract_text_chunks()` (C1)
- [ ] **T14**: Add `node_tokens: HashMap<String, HashSet<String>>` to cache tokenization, refactor `calculate_text_score_optimized()` (D1)

---

## Phase 5: Validation Pipeline Integration (5 tasks) ⚠️ HARD REQUIREMENT

- [ ] **T15**: Unify `EditChange` types - update `src/validation/edit_change.rs` to use same variants as `src/edit/mod.rs` or create conversion layer (2.1)
- [ ] **T16**: Wire `LogicValidator` into `edit_preview_handler::execute()` - validate before applying, include warnings in preview (2.2)
- [ ] **T17**: Wire `LogicValidator` into `edit_apply_handler::execute()` - reject edits with errors, include warnings in response (2.3)
- [ ] **T18**: Wire `LogicValidator` into `rename_symbol_handler::execute()` - validate rename for conflicts, validate result syntax (2.4)
- [ ] **T19**: Add `validation` field to MCP edit preview/apply responses with `is_valid`, `syntax_errors`, `reference_issues`, `semantic_drift`, `impact_report` (2.5)

---

## Phase 6: Handler Layer Optimization (3 tasks)

- [ ] **T20**: Extract `build_symbol_entry(pdg, nid, opts) -> Value` helper from `grep_symbols_handler::execute()`, use for all three modes (A2)
- [ ] **T21**: Create `HandlerContext` struct with factory methods to reduce preamble boilerplate in 18 handlers (B2)
- [ ] **T22**: Change `ProjectRegistry` from `Mutex<LeIndex>` to `RwLock<LeIndex>` to reduce lock contention on first PDG load (A3)

---

## Phase 7: PDG & Graph Optimization (4 tasks)

- [ ] **T23**: Add `RefCell<Vec<NodeId>>` scratch buffer to `ProgramDependenceGraph`, refactor `bfs_directed()` to reuse (A5)
- [ ] **T24**: Add `name_file_index: HashMap<(String, String), NodeId>` to PDG, implement `find_by_name_in_file()` O(1) (D2)
- [ ] **T25**: Change `TraversalConfig.allowed_edge_types` from `Vec<EdgeType>` to `&'static [EdgeType]` to eliminate heap allocation (D4)
- [ ] **T26**: Use `Arc<str>` for `Node.file_path` to enable string interning, reduce memory (C3)

---

## Phase 8: Optional / Low Priority (2 tasks)

- [ ] **T27**: Optimize `TfIdfEmbedder::build_from_tokens()` with min-heap approach (D3)
- [ ] **T28**: Implement `incremental_reindex()` for `text_index` with delta updates (C2)

---

## Execution Order

Tasks MUST be executed in dependency order. Do not skip phases. Critical path: T08 → T09 → T10 → T15 → T16/T17/T18/T19.

All tasks in Phases 1-2 must complete before Phase 3 begins. Phase 3 must complete before Phase 4. Phase 4 must complete before Phase 5 (validation). Phase 5 must complete before Phase 6. Phase 6 must complete before Phase 7. Phase 8 is optional and can be done anytime after Phase 7.

---

## Verification Checklist

- [ ] All cargo tests pass (`cargo test --workspace`)
- [ ] All 64 MCP handler tests pass
- [ ] `cargo check` passes with no warnings (except allowed list)
- [ ] No new lint errors introduced
- [ ] Validation pipeline exercised by integration tests
- [ ] Rewrite integration tests ported and passing
- [ ] Performance benchmarks show improvement on A1-A5 targets
