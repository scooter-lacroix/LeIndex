# LeIndex Search Functionality Bug Report

**Date:** 2026-01-17  
**Status:** Open  
**Severity:** Critical - Search tools are non-functional

---

## Summary

Two critical bugs in the LeIndex MCP server prevent the search functionality from working:

1. **Bug #1:** `search_content` tool passes unsupported parameters to `search_code_advanced()`
2. **Bug #2:** `cross_project_search_tool` accesses non-existent `.message` attribute on `InvalidPatternError`

---

## Bug #1: Parameter Mismatch in `search_content`

### Error Message
```
Error executing tool search_content: search_code_advanced() got an unexpected keyword argument 'content_boost'
```

### Root Cause

The MCP tool `search_content` in [server.py#L1226-L1280](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py#L1226-L1280) accepts parameters that are not supported by the underlying `search_code_advanced()` function.

**`search_content` accepts these parameters (lines 1235-1238):**
```python
content_boost: float = 1.0,
filepath_boost: float = 1.0,
highlight_pre_tag: str = "<em>",
highlight_post_tag: str = "</em>",
```

**But `search_code_advanced` signature (lines 3133-3141) only accepts:**
```python
async def search_code_advanced(
    pattern: str,
    ctx: Context,
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    page: int = 1,
    page_size: int = 5,
) -> Dict[str, Any]:
```

**The call at lines 1267-1280 passes all parameters including unsupported ones:**
```python
return await search_code_advanced(
    pattern=pattern,
    ctx=ctx,
    case_sensitive=case_sensitive,
    context_lines=context_lines,
    file_pattern=file_pattern,
    fuzzy=fuzzy,
    content_boost=content_boost,        # ❌ NOT SUPPORTED
    filepath_boost=filepath_boost,      # ❌ NOT SUPPORTED
    highlight_pre_tag=highlight_pre_tag,  # ❌ NOT SUPPORTED
    highlight_post_tag=highlight_post_tag,  # ❌ NOT SUPPORTED
    page=page,
    page_size=page_size,
)
```

### Fix Required

**Option A (Recommended): Remove unsupported parameters from the call**

Edit [server.py#L1267-L1280](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py#L1267-L1280):

```python
return await search_code_advanced(
    pattern=pattern,
    ctx=ctx,
    case_sensitive=case_sensitive,
    context_lines=context_lines,
    file_pattern=file_pattern,
    fuzzy=fuzzy,
    page=page,
    page_size=page_size,
)
```

Also remove or document the unused parameters from the tool signature (lines 1235-1238).

**Option B: Extend `search_code_advanced` to support additional parameters**

If the additional parameters are intended for future use, add them to `search_code_advanced()`:

```python
async def search_code_advanced(
    pattern: str,
    ctx: Context,
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    page: int = 1,
    page_size: int = 5,
    content_boost: float = 1.0,           # Add
    filepath_boost: float = 1.0,          # Add
    highlight_pre_tag: str = "<em>",      # Add
    highlight_post_tag: str = "</em>",    # Add
) -> Dict[str, Any]:
```

---

## Bug #2: Missing `.message` Attribute on `InvalidPatternError`

### Error Message
```
Error executing tool cross_project_search_tool: 'InvalidPatternError' object has no attribute 'message'
```

### Root Cause

The exception handler in `cross_project_search_tool` at [server.py#L2734-L2739](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py#L2734-L2739) tries to access `e.message`:

```python
except InvalidPatternError as e:
    logger.error(f"Invalid search pattern: {e}")
    return {
        "success": False,
        "error": f"Invalid search pattern: {e.message}",  # ❌ .message doesn't exist
        "error_type": "InvalidPatternError"
    }
```

**However, `InvalidPatternError` does NOT have a `.message` attribute.**

Looking at the class hierarchy:
- `InvalidPatternError` extends `CrossProjectSearchError` ([cross_project_search.py#L125-145](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/global_index/cross_project_search.py#L125-L145))
- `CrossProjectSearchError` extends `GlobalIndexError` ([monitoring.py#L79-104](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/global_index/monitoring.py#L79-L104))
- `GlobalIndexError` calls `super().__init__(message)` but does NOT store `message` as an attribute

The message is only accessible via `str(e)` or `e.args[0]`.

### Fix Required

Edit [server.py#L2738](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py#L2738):

```python
except InvalidPatternError as e:
    logger.error(f"Invalid search pattern: {e}")
    return {
        "success": False,
        "error": f"Invalid search pattern: {str(e)}",  # ✅ Use str(e) instead of e.message
        "error_type": "InvalidPatternError"
    }
```

**Alternative**: Add a `message` property to `GlobalIndexError`:

```python
class GlobalIndexError(Exception):
    def __init__(self, message: str, component: str, details: Optional[Dict[str, Any]] = None):
        super().__init__(message)
        self._message = message  # Store as private attribute
        self.component = component
        self.details = details or {}
        self.timestamp = datetime.now().isoformat()
    
    @property
    def message(self) -> str:
        return self._message
```

---

## Affected Files

| File | Lines | Issue |
|------|-------|-------|
| `src/leindex/server.py` | 1267-1280 | Passes unsupported params to `search_code_advanced()` |
| `src/leindex/server.py` | 2738 | Accesses `.message` which doesn't exist |

---

## Testing Recommendations

After applying fixes, verify with:

```python
# Test search_content
await search_content(ctx, "search", pattern="def main", fuzzy=True)

# Test cross_project_search_tool with invalid pattern
await cross_project_search_tool(ctx, pattern="[invalid(regex")
```

---

## Note on Existing Documentation

This issue has been previously documented in:
- `CRITICAL_BUG_FIX_TASK1.1.md` 
- `LEINDEXER_CODEBASE_ANALYSIS.md`
- `LEINDEX_COMPREHENSIVE_INVESTIGATION_REPORT.md`

However, the fixes were never applied to the actual source code.

---

## Maestro Track Analysis

### Track: `search_enhance_20260108`

The [spec.md](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/maestro/tracks/search_enhance_20260108/spec.md) and [PROGRESS_SUMMARY.md](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/maestro/tracks/search_enhance_20260108/PROGRESS_SUMMARY.md) indicate:

| Item | Spec Decision | Status |
|------|---------------|--------|
| `content_boost` | **Remove** from call | ❌ Not applied |
| `filepath_boost` | **Remove** from call | ❌ Not applied |
| `highlight_pre_tag` | **Remove** from call | ❌ Not applied |
| `highlight_post_tag` | **Remove** from call | ❌ Not applied |
| Phase 1 (Search Fix) | Marked "COMPLETE" | ❌ False - code unchanged |

**Finding:** The spec chose to remove these parameters rather than implement them, but the fix was never actually committed.

### The Parameters SHOULD Be Implemented

The Tantivy backend at [tantivy_storage.py#L847-868](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/storage/tantivy_storage.py#L847-L868) **already supports** these parameters:

```python
def search_content(
    self,
    query: str,
    is_sqlite_pattern: bool = False,
    fuzziness: Optional[str] = None,
    content_boost: float = 1.0,          # ✅ Supported
    file_path_boost: float = 1.0,        # ✅ Supported  
    highlight_pre_tags: Optional[List[str]] = None,  # ✅ Supported
    highlight_post_tags: Optional[List[str]] = None  # ✅ Supported
) -> List[Tuple[str, Any]]:
```

**Recommendation:** Instead of removing the parameters, extend `search_code_advanced()` to pass them through to Tantivy.

---

## Proposed Implementation

### Fix #1: Extend `search_code_advanced` Signature

Edit [server.py#L3133-3142](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py#L3133-L3142):

```python
async def search_code_advanced(
    pattern: str,
    ctx: Context,
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    page: int = 1,
    page_size: int = 5,
    # New parameters to expose Tantivy capabilities
    content_boost: float = 1.0,
    filepath_boost: float = 1.0,
    highlight_pre_tag: str = "<em>",
    highlight_post_tag: str = "</em>",
) -> Dict[str, Any]:
```

Then pass these to the Tantivy backend when available.

### Fix #2: Fix `InvalidPatternError` Handler

Edit [server.py#L2738](file:///mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py#L2738):

```python
"error": f"Invalid search pattern: {str(e)}",  # Use str(e) not e.message
```

### Fix #3: Optimize Search Token Efficiency

The current `page_size` default of 5 in `search_code_advanced` is already optimized for token efficiency. Additionally:

1. **Truncate long lines** - Add `max_line_length` parameter (default 200)
2. **Limit context** - Cap `context_lines` at 3 in tool schema
3. **Add result summary mode** - Return counts only without content for initial queries

### Token-Efficient Search Response Format

```python
# Compact response format
{
    "success": True,
    "count": 47,
    "showing": 5,
    "page": 1,
    "matches": [
        {"f": "src/auth.py", "l": 42, "c": "def authenticate(token):"},
        # ... truncated lines, short keys
    ]
}
```

---

## Implementation Priority

| Priority | Fix | Effort | Impact |
|----------|-----|--------|--------|
| 🔴 P0 | Remove unsupported params OR extend signature | 30 min | Unblocks all search |
| 🔴 P0 | Fix `.message` → `str(e)` | 5 min | Unblocks cross-project search |
| 🟡 P1 | Pass boost params to Tantivy | 2 hrs | Better search relevance |
| 🟢 P2 | Add token-efficient response mode | 4 hrs | Reduces LLM context usage |
