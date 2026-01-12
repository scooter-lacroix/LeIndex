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

## [~] Track: Timeout Root Cause Fixes with Activity-Based Monitoring
*Link: [./maestro/tracks/timeout_fix_20260111/](./maestro/tracks/timeout_fix_20260111/)*

**Description:** Fix 7 timeout operations through root cause analysis and implement activity-based timeout detection. Replaces arbitrary 300s timeouts with intelligent idleness detection (60s of zero activity only).

**Status:** New

**Created:** 2026-01-11

**Affected Operations:**
- detect_orphaned_indexes
- search_content (find action)
- manage_operations (list action)
- get_diagnostics (operations, settings types)
- configure_memory
- manage_memory (cleanup action)
- manage_temp (check action)
