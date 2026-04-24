# Project Tracks

This file tracks all major tracks for the project. Each track has its own detailed plan in its respective folder.

---

## [x] Track: LeIndex Performance Optimization - Complete I/O Refactoring
*Link: [./tracks/perf_opt_20260107/](./tracks/perf_opt_20260107/)*

---

## [x] Track: Search Enhancement, Global Index, and Memory Management ✅ COMPLETE
*Link: [./tracks/search_enhance_20260108/](./tracks/search_enhance_20260108/)*

**Description:** Three major enhancements - ALL COMPLETE:
- Task 0: Fixed critical search parameter mismatch bug ✅
- Task 1: Implemented cross-project global index with stale-allowed caching ✅
- Task 2: Implemented advanced hierarchical memory management with LLM-mediated prompting ✅

**Completion Date:** 2026-01-08

**Key Achievements:**
- All 7 phases (0-6) completed successfully
- 750+ tests with 96.8% pass rate
- All performance targets exceeded
- Production-ready code quality
- Comprehensive documentation and examples

---

## [x] Track: Fix Critical MCP Tool Bugs ✅ COMPLETE
*Link: [./tracks/mcp_bugs_fix_20260110/](./tracks/mcp_bugs_fix_20260110/)*

**Description:** Fix 6 critical bugs in the LeIndex MCP server causing runtime errors across core functionality (dashboard, search, cross-project search, project listing, memory eviction).

**Status:** Complete

**Completion Date:** 2026-01-10

**Commits:**
- df9e3c7: Add average_health_score and total_size_mb to DashboardData
- c5b6d6b: Correct parameter names in get_dashboard and list_projects
- eb6cb82: Correct parameter names in search_content and cross_project_search_tool
- 15af0e8: Register eviction unloader with thread-safe implementation

**Test Results:** 909 passed, 18 failed (pre-existing issues)

---

## [x] Track: Timeout Root Cause Fixes with Activity-Based Monitoring ✅ COMPLETE
*Link: [./archive/timeout_fix_20260111/](./archive/timeout_fix_20260111/)*

**Description:** Fixed root causes of 7 timeout-prone MCP operations. All operations now complete successfully without hanging.

**Status:** Complete

**Completion Date:** 2026-01-11

**Commits:**
- 0968507: Fix root causes of 7 timeout-prone MCP operations (all 7 operations fixed)

**Key Achievements:**
- Fixed lock release bug in manage_operations
- Converted blocking operations to async (search_content, manage_memory, manage_temp, detect_orphaned_indexes)
- Added async file I/O for configure_memory
- Implemented symlink loop detection in orphan detector
- Fixed 30+ tests to use async/await patterns
- Resolved 17 DuckDB locking errors in tests
- **Tzar Approved:** Code quality and architecture validated by codex-reviewer
- Test Results: 974 passed, 4 skipped, 0 errors

**Note:** Activity-based timeout enforcement (60s idle detection) was deferred per user direction - root cause fixes were the priority.

**Affected Operations:**
- detect_orphaned_indexes ✅
- search_content (find action) ✅
- manage_operations (list action) ✅
- get_diagnostics (operations, settings types) ✅
- configure_memory ✅
- manage_memory (cleanup action) ✅
- manage_temp (check action) ✅

---

## [x] Track: lerecherche - Search & Analysis Fusion ✅ COMPLETE
*Link: [./tracks/lerecherche_20250125/](./tracks/lerecherche_20250125/)*

**Description:** Implement node-level semantic search with vector-AST synergy for LeIndex Rust Renaissance.

**Status:** Complete

**Completion Date:** 2026-01-26

**Test Results:** 69/69 tests passing ✅

**Key Achievements:**
- All 6 phases (1-6) completed successfully
- Text search with substring/token matching
- Vector search with cosine similarity
- Hybrid scoring combining multiple signals
- PDG context expansion with gravity traversal
- Full indexing pipeline with embedding support
- Natural language query processing
- Intent classification (HowWorks, WhereHandled, Bottlenecks, Semantic, Text)
- Complexity-based ranking for bottleneck queries

**Tzar Review Fixes Applied (18 Issues):**
- ✅ Critical (6): Regex DoS vulnerability, unbounded allocation, missing validation, panic in Default, O(n²) complexity, no dimension validation
- ✅ Important (4): Inefficient HashSet, missing error context, race condition docs, fallback tokenization
- ✅ Edge Cases (2): Unicode normalization, empty terms handling
- ✅ Security (2): Rate limiting and query logging (documented - infrastructure level)
- ✅ Performance (4): String allocations, score calculation, punctuation stripping, regex optimization

**Code Quality:** Production-ready with comprehensive validation, thread safety guarantees, and performance optimizations.

**Commits:**
- e3e905f: fix(lerecherche): Apply all Tzar review fixes for production readiness

**Files Implemented:**
- `src/lib.rs` (22 lines) - Module declarations, exports
- `src/search.rs` (1238 lines) - SearchEngine, text/vector search, natural language queries
- `src/semantic.rs` (140 lines) - PDG context expansion
- `src/ranking.rs` (191 lines) - Hybrid scoring
- `src/vector.rs` (270 lines) - VectorIndex with cosine similarity
- `src/query.rs` (886 lines) - Natural language query processing

**Total:** ~2,747 lines of production Rust code

---

## [~] Track: lepasserelle - Integration & API Layer 🦀 IN PROGRESS
*Link: [./tracks/lepasserelle_20250125/](./tracks/lepasserelle_20250125/)*

**Description:** Pure Rust orchestration, CLI, and MCP server that brings together leparse, legraphe, lerecherche, and lestockage into a unified LeIndex system.

**Status:** ~90% COMPLETE - CLI & MCP Server Complete, Config/Docs Pending ⚠️

**Created:** 2025-01-25

**Type:** Standard Track (part of leindex_rust_refactor_20250125 master track)

**Parent Track:** `leindex_rust_refactor_20250125`

**Overall Progress:**
- **Phase 1:** Remove PyO3, Create Pure Rust Foundation ✅ COMPLETE
- **Phase 2:** Pure Rust MCP Server ✅ COMPLETE (protocol + handlers + serve CLI)
- **Phase 3:** CLI Interface ✅ COMPLETE (index, search, analyze, diagnostics, serve)
- **Phase 4:** Integration Layer ⚠️ PARTIAL (orchestration ✅, config ❌, errors ❌)
- **Phase 5:** Memory Management ✅ COMPLETE (RSS monitoring, spilling, reloading, warming)
- **Phase 6:** Testing & Documentation ⚠️ PARTIAL (72 tests passing, rustdoc complete, user docs pending)

**Key Tasks:**
- [x] Remove all PyO3/Python dependencies
- [x] Implement pure Rust MCP JSON-RPC server
- [x] Create CLI interface (index, search, analyze, diagnostics, serve)
- [x] Build LeIndex orchestration API
- [x] Implement cache spilling and reloading
- [ ] Project configuration (TOML/JSON)
- [ ] Error recovery and detailed error reporting
- [ ] CLI usage examples and MCP protocol docs

**Test Results:**
- 40/40 unit tests passing ✅
- 32/32 integration tests passing ✅
- 72/72 total tests passing ✅
- 0 warnings in build

**New Features (2026-01-27):**
- **CLI Interface:** All commands implemented (index, search, analyze, diagnostics, serve)
- **MCP Server:** `leindex serve` command to start MCP server
- **Cache Management:** Spilling, reloading, warming with 4 strategies
- **One-Line Installer:** `curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash`

**Performance Targets:**
- <60s indexing for 50K files
- <100ms P95 search latency
- 10x memory reduction vs Python (400→32 bytes/node)
- <50ms MCP server response time

---

## [x] Track: lephase - 5-Phase Analysis System Integration ✅ COMPLETE
*Link: [./tracks/lephase_5phase_20260205/](./tracks/lephase_5phase_20260205/)*

**Description:** Integrate Maestro's 5-phase code analysis system into LeIndexer as a new `lephase` crate, leveraging LeIndexer's superior parsing infrastructure while adopting Maestro's token-efficient, LLM-oriented output architecture.

**Status:** COMPLETE - 58/58 tests passing ✅

**Completed:** 2026-02-14

**Type:** Feature Track (New Crate)

**Parent Track:** `leindex_rust_refactor_20250125`

**Phases:**
1. Foundation - Crate Setup (4 tasks)
2. Core Phases 1-3 (5 tasks)
3. Core Phases 4-5 (4 tasks)
4. CLI Integration (4 tasks)
5. MCP Server Integration (3 tasks)
6. Orchestration Engine (5 tasks)
7. Testing & Polish (3 tasks)

**Key Deliverables:**
- New `crates/lephase/` crate with 5-phase analysis
- TokenFormatter with Ultra/Balanced/Verbose modes
- CLI commands: `leindex phase1-5`, `leindex analyze`
- MCP tool: `phase_analysis`
- Orchestration engine with GravityTraversal context

**Reference Documents:**
- [Integration Plan](../docs/5PHASE_INTEGRATION_PLAN.md)
- [Spec](./tracks/lephase_5phase_20260205/spec.md)
- [Plan](./tracks/lephase_5phase_20260205/plan.md)

**Estimated Duration:** 9 weeks

---

## [x] Track: LeIndex Rust Renaissance 🦀 MASTER TRACK ✅ COMPLETE
*Link: [./tracks/leindex_rust_refactor_20250125/](./tracks/leindex_rust_refactor_20250125/)*

**Description:** Transform LeIndex from Python to Rust-based Deep Code Intelligence Engine. Complete greenfield rewrite with integrated code analysis capabilities.

**Status:** COMPLETE - All Core Functionality Production Ready + Documentation ✅

**Created:** 2025-01-25
**Last Updated:** 2026-02-14 (Documentation Complete - README, CLI, MCP, API guides)

**Type:** Master Track (orchestrate-ready with 6 sub-tracks)

**Overall Progress:**
- **Total Tests:** 472/472 passing ✅ (100%)
- **All Crates:** 6/6 production ready (leparse, legraphe, lerecherche, lestockage, lepasserelle, lephase)
- **Integration:** Complete (CLI + MCP server operational)

**Sub-Tracks Status:**
1. ✅ `leparse_20250125` - **COMPLETE** (101 tests) - Core Parsing Engine (zero-copy AST, tree-sitter, 12 languages)
2. ✅ `legraphe_20250125` - **COMPLETE** (47 tests) - Graph Intelligence Core (PDG, gravity-based traversal, cross-project)
3. ✅ `lerecherche_20250125` - **COMPLETE** (99 tests) - Search & Analysis Fusion (HNSW, semantic search, NL queries)
4. ✅ `lestockage_20250125` - **COMPLETE** (59 tests) - Persistent Storage Layer (SQLite, Salsa, Turso config, cross-project)
5. ✅ `lepasserelle_20250125` - **COMPLETE** (79 tests) - Integration & API Layer (CLI, MCP server, cache management)
6. ✅ `lephase_5phase_20260205` - **COMPLETE** (58 tests) - 5-Phase Analysis System (TokenFormatter, GravityTraversal)

**Performance Targets:**
- 10x memory reduction (400→32 bytes per node) ✅
- <60s indexing for 50K files (match Python baseline) ✅
- <100ms P95 search latency ✅
- 20% token efficiency improvement ✅

**Approach:** Greenfield architecture with reference-code-guidance only (no 1:1 copying)

**Key Features Implemented:**
- ✅ Zero-copy AST extraction with tree-sitter
- ✅ Gravity-based traversal for intelligent context expansion
- ✅ Node-level semantic search with vector-AST synergy
- ✅ Cross-project intelligence with global symbol resolution
- ✅ HNSW vector indexing for production-scale search
- ✅ Salsa-based incremental computation (BLAKE3 hashing)
- ✅ Turso/libsql hybrid storage configuration (local-only verified)
- ✅ Pure Rust CLI with 5 commands (index, search, analyze, diagnostics, serve)
- ✅ JSON-RPC 2.0 MCP server for AI assistant integration
- ✅ Cache management (spill/reload/warm strategies)

**Recent Commits:**
- 1ffab39: docs: Update master track plan.md to reflect verified state (2026-01-27)
- 987e91c: Implement Phase 7.4 - Cross-Project Integration Tests
- a30967b: Implement Phase 7.3 - Cross-Project PDG Extension
- afc71ea: Implement Phase 7.2 - Cross-Project Resolution

---

## [x] Track: LeIndex Dashboard Backend 🦀 MASTER TRACK ✅ COMPLETE
*Link: [./tracks/dashboard_backend_20260213/](./tracks/dashboard_backend_20260213/)*

**Description:** Complete Rust backend implementation for LeIndex Dashboard with HTTP/WebSocket API, unique project IDs, global registry, code editing, and real-time sync. Frontend handled separately via `docs/frontend-plan.md`.

**Status:** COMPLETE - All 5 subtracks implemented

**Created:** 2026-02-13
**Completed:** 2026-02-14

**Type:** Master Track (orchestratable with 5 sub-tracks)

**Sub-Tracks Status:**
1. [x] `leunique_id_20260213` - Unique Project ID System ✅
2. [x] `leglobal_20260213` - Global Registry & Discovery ✅
3. [x] `leserve_20260213` - HTTP/WebSocket Server ✅
4. [x] `leedit_20260213` - Code Editing Engine ✅
5. [x] `levalidation_20260213` - Edit Validation ✅

**Total Test Coverage:** ~115 tests passing across all crates

---

## [~] Track: MCP Fix & LeIndex Tool Supremacy
*Link: [./tracks/mcp_fix_tool_supremacy_20260227/](./tracks/mcp_fix_tool_supremacy_20260227/)*

**Description:** Fix MCP stdio double-newline injection bug and placeholder hash embeddings, then expand LeIndex MCP tool surface to completely replace glob/grep/read/edit tools — making LeIndex the preferred toolset for LLM code navigation, understanding, and editing.

**Status:** IN PROGRESS — 22/22 tasks complete (all automated tasks done; 1 user verification remains: A.2)

**Created:** 2026-02-27
**Last Updated:** 2026-02-27

**Type:** Standard Track (Bug Fix + Feature Expansion)

**Parent Track:** `lepasserelle_20250125`

**Phases:**
1. Phase A: MCP Stdio Transport Fix (2 tasks)
2. Phase B: Semantic Embedding Fix (2 tasks)
3. Phase C: Read/Grep/Glob Replacement — 5 new tools + 2 enhancements (8 tasks)
4. Phase D: Context-Aware Editing — 4 new tools + leedit core (6 tasks)
5. Phase E: Integration Testing & Tool Description Polish (4 tasks)

**Total Tasks:** 22 + 5 Maestro verification gates

**Key Deliverables:**
- MCP stdio connection fix (2-char change × 2 lines)
- TF-IDF semantic embedding system replacing placeholder hash
- 5 new MCP tools: `leindex_file_summary`, `leindex_symbol_lookup`, `leindex_project_map`, `leindex_grep_symbols`, `leindex_read_symbol`
- 4 new edit MCP tools: `leindex_edit_preview`, `leindex_edit_apply`, `leindex_rename_symbol`, `leindex_impact_analysis`
- Enhanced `leindex_search` with signatures, doc summaries, caller counts
- Enhanced `leindex_phase_analysis` single-file deep dive
- `leedit` crate placeholder implementations completed
- Token efficiency benchmarks showing 5-10x improvement over standard tools

---

## [ ] Track: LeIndex Remediation — Complete Bug Fix Suite
*Link: [./tracks/leindex_remediation_20260414/](./tracks/leindex_remediation_20260414/)*

**Description:** Complete remediation of 13 critical bugs discovered during Radis_Rust Tzar review. Fixes grep_symbols zero-result bug, broken call graph, fake complexity metrics, and phantom node pollution.

**Status:** NEW

**Created:** 2026-04-14

**Type:** Bug Fix Track (Comprehensive Remediation)

**Scope:** All phases (A: Critical, B: High-Priority, C: System Improvements)

**Testing Approach:** Full TDD (tests first for each fix)

**Validation:** Both internal test fixtures and Radis_Rust integration validation

**Phases:**
1. Phase A: Critical Fixes (4 tasks) — Blocks all downstream quality
2. Phase B: High-Priority Fixes (4 tasks) — Fixes core value prop
3. Phase C: System Improvements (6 tasks) — Quality of life improvements

**Total Tasks:** 13 fixes across 3 phases

**Key Deliverables:**
- grep_symbols always returns results for known symbols (fixes zero-result bug)
- External/phantom nodes filtered from search results by default
- Real cyclomatic complexity from parser instead of parameter-count heuristic
- Rust parser counts branches correctly (if, match, &&, ||, ?)
- Struct instantiation and type reference edges in call graph
- caller_count accurately reflects struct usage
- Search scoring tuned for code vs prose queries
- NodeType::External variant for type-safe filtering
- Re-export deduplication in grep_symbols
- Staleness warnings in tool responses
- Structured zero-result responses with suggestions
- Cached per-file statistics for performance
- include_source parameter for grep_symbols

**Acceptance Criteria:**
- grep_symbols("ColdStoreRepository") returns ≥1 result
- grep_symbols results contain zero "external" phantom nodes
- grep_symbols returns complexity > 5 for complex functions
- caller_count for DeepThoughtManager > 0
- leindex_search with search_mode: "code" returns accurate top results
- All 472 tests passing (no regression)
- >95% code coverage for modified code
- Radis_Rust validation successful

**Dependencies:**
- A.3 depends on A.4
- B.3 depends on B.1 and B.2
- C.1 depends on A.2
- C.2 depends on A.1

**Reference Documents:**
- [Task List](../docs/TODO/LEINDEX_REMEDIATION_TASK_LIST.md)
- [Root Cause Analysis](../docs/TODO/tzar_usage_report.md)
- [Spec](./tracks/leindex_remediation_20260414/spec.md)
- [Plan](./tracks/leindex_remediation_20260414/plan.md)

---

## [x] Track: Version Bump CI Workflow
*Link: [./tracks/bump_version_ci_20260424/](./tracks/bump_version_ci_20260424/)*
- Add `workflow_dispatch` GitHub Action to bump version across Cargo.toml, npm package.json, and PyPI pyproject.toml, committing to master to trigger existing release.yml pipeline
- [Spec](./tracks/bump_version_ci_20260424/spec.md)
- [Plan](./tracks/bump_version_ci_20260424/plan.md)