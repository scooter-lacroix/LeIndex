# Project Tracks

This file tracks all major tracks for the project. Each track has its own detailed plan in its respective folder.

---

## [x] Track: LeIndex Performance Optimization - Complete I/O Refactoring
*Link: [./maestro/tracks/perf_opt_20260107/](./maestro/tracks/perf_opt_20260107/)*

---

## [x] Track: Search Enhancement, Global Index, and Memory Management ✅ COMPLETE
*Link: [./maestro/tracks/search_enhance_20260108/](./maestro/tracks/search_enhance_20260108/)*

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
*Link: [./maestro/tracks/mcp_bugs_fix_20260110/](./maestro/tracks/mcp_bugs_fix_20260110/)*

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
*Link: [./maestro/archive/timeout_fix_20260111/](./maestro/archive/timeout_fix_20260111/)*

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

## [~] Track: LeIndex Rust Renaissance 🦀 MASTER TRACK
*Link: [./maestro/tracks/leindex_rust_refactor_20250125/](./maestro/tracks/leindex_rust_refactor_20250125/)*

**Description:** Transform LeIndex from Python to Rust-based Deep Code Intelligence Engine. Complete greenfield rewrite with integrated code analysis capabilities.

**Status:** Orchestration In Progress

**Created:** 2025-01-25

**Type:** Master Track (orchestrate-ready with 5 sub-tracks)

**Sub-Tracks:**
1. `leparse_20250125` - Core Parsing Engine (zero-copy AST, tree-sitter, 17+ languages)
2. `legraphe_20250125` - Graph Intelligence Core (PDG, gravity-based traversal)
3. `lerecherche_20250125` - Search & Analysis Fusion (node-level embeddings, semantic entry points)
4. `lestockage_20250125` - Persistent Storage Layer (SQLite, Salsa incremental computation)
5. `lepasserelle_20250125` - Bridge & Integration (PyO3, MCP tools, memory management)

**Performance Targets:**
- 10x memory reduction (400→32 bytes per node)
- <60s indexing for 50K files (match Python baseline)
- <100ms P95 search latency
- 20% token efficiency improvement

**Approach:** Greenfield architecture with reference-code-guidance only (no 1:1 copying)

**Key Features:**
- Zero-copy AST extraction with tree-sitter
- Gravity-based traversal for intelligent context expansion
- Node-level semantic search with vector-AST synergy
- Cross-project intelligence with global symbol resolution
- Salsa-based incremental computation (node-level hashing)
- Unified MCP tool: `leindex_deep_analyze`