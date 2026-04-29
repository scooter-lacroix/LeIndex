# LeIndexer Optimization - Provenance & Audit Trail

**Analyst**: Droid (AI software engineering agent)  
**Date**: 2026-04-27  
**Branch**: `feature/unified-crate`  
**Base commit**: `cf2d145` (fix: restore 6 handlers to original logic + migrate 17 missing tests)  

---

## Analysis Request

User requested: "comprehensive optimization analysis of the entire codebase" with instructions to be "thorough and meticulous." The scope included performance, memory, code quality, structural optimization, and integration of orphan rewrite files. User explicitly required that the validation pipeline be completed and integrated (not deferred).

---

## Methodology

### 1. Initial Exploration

- Used `leindex`'s own analysis tools (deep_analyze, file_summary, project_map, phase_analysis) to understand codebase structure, complexity hotspots, and inter-module dependencies.
- Complemented with `ripgrep` for targeted text searches, `cargo check` and `cargo test` to validate current state.
- Identified 81+ source files for deep inspection, prioritizing files with high complexity and centrality.

### 2. Deep Structural Analysis

For each critical subsystem, performed:
- **File-level analysis**: `leindex_file_summary()` to understand symbol inventory, complexity scores, cross-file dependencies.
- **Symbol-level analysis**: `leindex_read_symbol()` to examine exact implementations of key functions (e.g., `semantic_search`, `index_nodes`, `bfs_directed`, `calculate_text_score_optimized`).
- **Call graph analysis**: `leindex_symbol_lookup()` to trace callers/callees and impact radius.
- **Full-content reading**: Used `Read` tool to examine entire files when necessary to understand context and design rationale.

### 3. Validation Against Source

Every potential finding was cross-referenced with actual implementation:
- Validated that `is_common_method()` uses a const `&[&str]` array with O(1) `.contains()` lookup, contrary to initial suspicion of O(N²) pattern matching.
- Confirmed `looks_like_abstract_base()` iterates two 3-element const arrays - already optimal.
- Verified that the validation module (`src/validation/`) has zero call sites outside its own tests, confirming it is completely disconnected.

### 4. Orphan Rewrite File Analysis

Read all three orphan rewrite files in full:
- `pdg_rewrite.rs` (613 lines) - compared feature-by-feature with current `pdg.rs`.
- `extraction_rewrite.rs` (1,213 lines) - extracted 5-phase pipeline, compared data flow/inheritance models.
- `pdg_utils_rewrite.rs` (369 lines) - analyzed merge/relink algorithms and configuration.

For each, produced a table of improvements and determined integration strategy (merge vs replace).

### 5. Dependency Mapping & Phasing

Constructed a dependency graph of all validated findings, then grouped into 8 implementation phases based on:
- Hygiene tasks (low risk, no dependencies) → Phase 1
- Structural refactoring (enables later work) → Phase 2
- Rewrite integration (strict dependency chain) → Phase 3
- Search engine optimizations (internal to SearchEngine) → Phase 4
- Validation pipeline (requires EditChange unification first) → Phase 5
- Handler layer (depends on earlier structural changes) → Phase 6
- PDG/Graph (largely independent) → Phase 7
- Optional/low priority → Phase 8

### 6. Task Breakdown

Each finding was decomposed into atomic, verifiable tasks with explicit acceptance criteria. The resulting 28-task blocking list is designed to be executed in strict dependency order.

---

## Tools Used

- **leindex deep analysis**: `leindex___leindex_deep_analyze`, `leindex___leindex_phase_analysis`, `leindex___leindex_project_map`
- **Symbol navigation**: `leindex___leindex_read_symbol`, `leindex___leindex_symbol_lookup`, `leindex___leindex_file_summary`
- **Text search**: `leindex___leindex_text_search`, `Grep`
- **File reading**: `Read` (with absolute paths)
- **Build/test**: `cargo check`, `cargo test --workspace`
- **Terminal**: `Execute` (for document generation, file operations)
- **Version control**: `git status`, `git diff`, `git log`

---

## Findings Summary

### Validated Findings: 23

**Tier A - Critical (5)**:
- A1: `semantic_search()` O(N) linear scan → add `node_id_to_idx`
- A2: `grep_symbols_handler` triple duplication → extract `build_symbol_entry`
- A3: First-request PDG load under Mutex → RwLock
- A4: `index_nodes()` clones entire node Vec → take ownership
- A5: `bfs_directed` allocates intermediate Vecs → scratch buffer

**Tier B - High Duplication (5)**:
- B1: `test_registry_for` in 8 tests → consolidate
- B2: Handler preamble boilerplate → `HandlerContext`
- B3: `ToolHandler` 80-match-arm → macro
- B4: `edit/mod.rs` monolith → split submodules
- B5: Duplicate `replace_whole_word` → deduplicate

**Tier C - High Memory (3)**:
- C1: `NodeInfo` stores full source → clear after indexing
- C2: `text_index` rebuilt from scratch → incremental (deferred to Phase 8)
- C3: No string interning for file paths → `Arc<str>`
- C4: Coarse-grained memory spill → deferred (requires new serialization)

**Tier D - Medium Algorithmic (4)**:
- D1: `calculate_text_score_optimized` re-tokenizes → cache tokens
- D2: `find_by_name_in_file` linear scan → `name_file_index`
- D3: `TfIdfEmbedder::build_from_tokens` O(N log N) → min-heap (low priority)
- D4: `TraversalConfig.allowed_edge_types` heap alloc → static slice

**Tier E - Low Structural (5)**:
- E1: Glob imports in handlers → explicit
- E2: Stale artifacts in project root → delete
- E3: Orphan rewrite files → integration planned (see Section 1)
- E4: Validation layer duplication → integration planned (see Section 2)
- E5: Unused imports in `handlers.rs` → remove

---

## Rejected Findings: 1

### R1: PDG heuristic string matching O(N²) bottleneck

**Initial hypothesis**: Functions like `is_common_method()` and `looks_like_abstract_base()` use linear scans over hardcoded arrays, causing O(N²) behavior when called repeatedly.

**Validation**: 
- Read `src/graph/pdg.rs` lines 168-185 (`is_common_method`): uses `const COMMON_METHODS: &[&str] = &["new", "clone", "init", ...]` (30 elements) and calls `.contains()` which is binary search O(log n) ≈ O(1) for 30 items.
- Read `src/validation/syntax.rs` lines 78-95 (`looks_like_abstract_base`): iterates two 3-element const arrays. Constant time.
- No quadratic behavior detected. The O(N²) concern was based on misreading linear scan as unbounded; with fixed-size const arrays, it's effectively O(1).

**Decision**: REJECTED - not a bottleneck. No action taken.

---

## Orphan Rewrite Evaluation

### pdg_rewrite.rs
**Integration value**: HIGH  
**Plan**: Merge `EmbeddingStore` and helper methods into existing `pdg.rs`. Do not replace entire file.

### extraction_rewrite.rs
**Integration value**: CRITICAL  
**Plan**: Replace extraction logic in `src/graph/extraction.rs` with 5-phase pipeline. Fixes O(N²) clique generation and multi-line import parsing.

### pdg_utils_rewrite.rs
**Integration value**: HIGH  
**Plan**: Replace entire `src/phase/pdg_utils.rs` with rewrite. Requires extraction_rewrite first.

---

## Validation Pipeline Assessment

**State**: Complete but dead code. All components (`LogicValidator`, `SyntaxValidator`, `ReferenceChecker`, `SemanticDriftAnalyzer`, `ImpactAnalyzer`) are implemented and tested, but `LogicValidator` is never instantiated outside tests.

**Integration complexity**: HIGH  
- Requires unifying two incompatible `EditChange` types.
- Requires adding `LogicValidator` creation method to `LeIndex`.
- Requires wiring into three MCP edit handlers with careful error handling.
- Requires extending MCP response schemas to include validation results.

**User directive**: "MUST be completed, validated, and thoroughly integrated" - this is a hard requirement, not optional.

---

## Artifacts Produced

1. **SPEC_BIBLE.md** - Authoritative specification with full findings, dependency graph, 8 implementation phases, 28-task blocking list.
2. **TASK_LIST.md** - Executable checklist with verification criteria.
3. **HANDOFF.md** - Context for developer picking up the work, including risk areas, rollback procedures, file reference.
4. **PROVENANCE.md** - This document: audit trail, methodology, tools, findings count, rejected rationale.

All files located in: `/mnt/WD-SSD/code_index_update/LeIndexer/docs/optimization/`

---

## Baseline Metrics

- **Commit**: cf2d145
- **Test status**: All tests pass (64 MCP tests confirmed)
- **Compilation**: `cargo check` clean
- **Source lines analyzed**: ~16,000 in MCP handler layer alone; 81+ files total
- **Analysis duration**: Multi-session deep investigation

Recommend re-running benchmarks after each phase to track gains.

---

## Signature

Analyst: Droid (AI agent)  
Review: Pending user approval of spec bible and task list  
Execution: Ready to proceed upon approval
