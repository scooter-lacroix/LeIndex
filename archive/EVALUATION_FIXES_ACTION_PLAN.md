# LeIndex MCP Evaluation Fixes - Detailed Action Plan

**Date:** January 20, 2026  
**Source:** LEINDEX_MCP_EVALUATION_REPORT.md, LEANN_SEMANTIC_SEARCH_EVALUATION.md  
**Status:** In Progress

---

## Overview

This document outlines the implementation plan for all recommendations from the evaluation reports.

---

## Priority High Fixes

### 1. Fix search parameter validation in `search_content`

**Issue:** Search parameters like `content_boost` are accepted but cause validation errors when passed to backend.

**Root Cause Analysis:**
- `search_content` in server.py (line 1226) accepts parameters like `content_boost`, `filepath_boost`
- These are passed to `search_code_advanced` (line 3133) which validates them but has inconsistent handling
- The `SearchOptions` dataclass in `types.py` accepts these but backend implementations may reject unexpected kwargs

**Implementation:**
1. Add parameter validation at the entry point of `search_content`
2. Add bounds checking for all numeric parameters
3. Add descriptive error messages with valid ranges
4. Ensure all parameters are properly filtered before passing to backend

**Files to Modify:**
- `src/leindex/server.py` - Add validation in `search_content` function (around line 1260)

---

### 2. Fix pattern validation error handling in `cross_project_search_tool`

**Issue:** `InvalidPatternError` doesn't properly format error messages, missing `message` attribute access.

**Root Cause Analysis:**
- `InvalidPatternError` class in `cross_project_search.py` (line 125) properly stores message
- The error handling in `cross_project_search_tool` (line 2734-2740) uses `str(e)` which works
- However, when errors are raised with details, the details may not be properly included

**Implementation:**
1. Enhance `InvalidPatternError` to include pattern and reason in string representation
2. Add `pattern` and `reason` as instance attributes for easier access
3. Improve error context in the tool's error handling
4. Add details dict to error response for debugging

**Files to Modify:**
- `src/leindex/global_index/cross_project_search.py` - Enhance `InvalidPatternError` class (line 125)
- `src/leindex/server.py` - Improve error handling in `cross_project_search_tool` (line 2734)

---

### 3. Add more context to error messages

**Issue:** Error messages lack context about what failed and why.

**Implementation:**
1. Create a standardized error response format with:
   - `success`: bool
   - `error`: str (human-readable message)
   - `error_type`: str (exception class name)
   - `error_context`: dict (additional debugging info)
   - `suggestions`: list (possible fixes)
2. Add context to all major error paths in search tools
3. Add parameter echoing in error responses

**Files to Modify:**
- `src/leindex/server.py` - Add helper function and update error handlers
- `src/leindex/global_index/cross_project_search.py` - Improve error details

---

## Priority Medium Fixes

### 4. Expand backend diagnostics details

**Issue:** `get_diagnostics:backend` returns minimal data.

**Current State:** `get_backend_health` (line 8888) only returns `stats.backends` from stats collector.

**Implementation:**
1. Add backend version information
2. Add backend capability flags (supports_regex, supports_fuzzy, etc.)
3. Add backend configuration details
4. Add connection pool status
5. Add recent operation statistics per backend

**Files to Modify:**
- `src/leindex/server.py` - Enhance `get_backend_health` function

---

### 5. Add search operation statistics to performance metrics

**Issue:** Performance metrics track embedding times but not search operation statistics.

**Current State:** `get_performance_metrics` (line 6555) returns generic metrics.

**Implementation:**
1. Add search-specific counters:
   - `search_total`: Total searches performed
   - `search_cache_hits`: Cache hit count
   - `search_cache_misses`: Cache miss count
   - `search_latency_avg`: Average search latency
   - `search_latency_p95`: 95th percentile latency
   - `search_by_backend`: Breakdown by backend used
2. Track search result counts and quality metrics

**Files to Modify:**
- `src/leindex/server.py` - Enhance `get_performance_metrics` function

---

### 6. Document parameter constraints better

**Issue:** Parameter constraints for search aren't documented or validated.

**Implementation:**
1. Add parameter constraint constants at module level
2. Add validation with descriptive error messages
3. Document constraints in docstrings
4. Add constraints info to diagnostics:settings response

**Files to Modify:**
- `src/leindex/server.py` - Add constants and update docstrings

---

## Priority Low Fixes

### 7. Add visualization tools for memory metrics

**Issue:** Memory metrics exist but have no visualization.

**Implementation:**
1. Add ASCII histogram for memory distribution
2. Add trend indicators (↑↓→) for memory growth
3. Add formatted output mode for memory status
4. Add memory timeline with recent snapshots

**Files to Modify:**
- `src/leindex/server.py` - Enhance `get_memory_status` response

---

### 8. Create indexing progress API

**Issue:** No progress API for long indexing operations.

**Implementation:**
1. Add `get_indexing_progress` function that returns:
   - Current phase (scanning, parsing, indexing, finalizing)
   - Files processed / total files
   - Estimated time remaining
   - Current file being processed
2. Hook into existing progress_manager

**Files to Modify:**
- `src/leindex/server.py` - Add new progress API function

---

### 9. Add batch operation support

**Issue:** No batch operation support for multiple file/search operations.

**Implementation:**
1. Add `batch_search` function for multiple patterns
2. Add `batch_read_files` function for multiple files
3. Implement concurrent execution with rate limiting
4. Add aggregated results format

**Files to Modify:**
- `src/leindex/server.py` - Add batch operation functions

---

## Implementation Order

1. **Phase 1 - High Priority (Critical Fixes)**
   - Fix 1: search_content parameter validation
   - Fix 2: InvalidPatternError improvements
   - Fix 3: Error message context

2. **Phase 2 - Medium Priority (Quality Improvements)**
   - Fix 4: Backend diagnostics expansion
   - Fix 5: Search statistics in performance metrics
   - Fix 6: Parameter constraint documentation

3. **Phase 3 - Low Priority (Nice-to-Have)**
   - Fix 7: Memory visualization
   - Fix 8: Indexing progress API
   - Fix 9: Batch operations

---

## Testing Strategy

1. Create test cases for each fix
2. Verify backward compatibility
3. Run existing test suite
4. Manual verification of error messages

---
