# Track: search_bug_fix_20260117

## Overview

Fix two critical bugs in the LeIndex MCP server that prevent search functionality from working:

1. **Bug #1**: `search_content` passes unsupported parameters (`content_boost`, `filepath_boost`, `highlight_pre_tag`, `highlight_post_tag`) to `search_code_advanced()` which doesn't accept them
2. **Bug #2**: `cross_project_search_tool` accesses non-existent `.message` attribute on `InvalidPatternError`, causing `AttributeError`

## Root Cause Analysis

### Bug #1: Parameter Mismatch
- **Location**: `src/leindex/server.py` lines 1267-1280
- **Issue**: `search_content` tool accepts 4 boosting/highlighting parameters but `search_code_advanced` (line 3133) doesn't define them
- **Impact**: All `search_content` calls fail with "unexpected keyword argument" error

### Bug #2: Missing Attribute
- **Location**: `src/leindex/server.py` line 2738
- **Issue**: `InvalidPatternError` hierarchy (→ `GlobalIndexError` → `Exception`) doesn't define `.message` attribute
- **Impact**: Cross-project search fails with `AttributeError` on invalid patterns

## Fix Strategy

### Fix #1: Extend `search_code_advanced` Signature
Add the 4 missing parameters to `search_code_advanced()`:
- `content_boost: float = 1.0`
- `filepath_boost: float = 1.0`
- `highlight_pre_tag: str = "<em>"`
- `highlight_post_tag: str = "</em>"`

Pass these through to the backend search implementation.

### Fix #2: Replace `e.message` with `str(e)`
Change line 2738 from:
```python
"error": f"Invalid search pattern: {e.message}",
```
to:
```python
"error": f"Invalid search pattern: {str(e)}",
```

## Acceptance Criteria

1. `search_content` tool with `action="search"` works without errors
2. `content_boost` and `filepath_boost` parameters are accepted and passed through
3. Highlighting tags are properly formatted in search results
4. `cross_project_search_tool` handles `InvalidPatternError` gracefully
5. Error messages for invalid patterns display correctly

## Out of Scope

- Implementing actual boosting logic in backends (Tantivy already supports this)
- UI/UX changes for error display
- Performance optimization of search results

## Files Affected

| File | Lines | Issue |
|------|-------|-------|
| `src/leindex/server.py` | 3133-3142 | Add missing parameters to `search_code_advanced` signature |
| `src/leindex/server.py` | 2738 | Replace `e.message` with `str(e)` |
