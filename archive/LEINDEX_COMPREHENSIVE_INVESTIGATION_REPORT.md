# LeIndex Server - Comprehensive Investigation Report

## Executive Summary

I have conducted a thorough investigation of the LeIndex MCP server codebase and identified the root causes of **all 6 critical issues** reported. The issues stem from **data model mismatches**, **incorrect function parameter passing**, and **missing unloader registration**. All issues have been mapped to specific file locations, line numbers, and exact code that needs to be fixed.

---

## Critical Issues Analysis

### Issue 1: get_global_stats tool fails - `'DashboardData' object has no attribute 'average_health_score'`

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`
**Lines**: 2183-2197 (specifically lines 2194, 2195)

**Root Cause**: The `DashboardData` dataclass does not include `average_health_score` and `total_size_mb` attributes, but the code tries to access them.

**Evidence**:

From `server.py` lines 2187-2197:
```python
dashboard = tier1.get_dashboard_data()

return {
    "success": True,
    "stats": {
        "total_projects": dashboard.total_projects,
        "total_symbols": dashboard.total_symbols,
        "total_files": dashboard.total_files,
        "languages": dashboard.languages,
        "average_health_score": dashboard.average_health_score,  # ERROR: Does not exist
        "total_size_mb": dashboard.total_size_mb,  # ERROR: Does not exist
        "last_updated": dashboard.last_updated
    }
}
```

From `tier1_metadata.py` lines 113-133, `DashboardData` is defined as:
```python
@dataclass
class DashboardData:
    total_projects: int
    total_symbols: int
    total_files: int
    languages: Dict[str, int]
    projects: List[ProjectMetadata]
    last_updated: float
    # NOTE: average_health_score and total_size_mb are NOT here
```

But `GlobalStats` (lines 90-109) DOES have these attributes:
```python
@dataclass
class GlobalStats:
    total_projects: int
    total_symbols: int
    total_files: int
    languages: Dict[str, int] = field(default_factory=dict)
    average_health_score: float = 1.0  # Present here
    total_size_mb: float = 0.0  # Present here
```

**Fix Required**: Either add the missing attributes to `DashboardData` or compute them from `GlobalStats` in the `get_dashboard_data()` method.

---

### Issue 2: get_dashboard tool fails - `get_dashboard_data() got an unexpected keyword argument 'status'`

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`
**Lines**: 2248-2256

**Root Cause**: The `get_dashboard()` MCP tool calls `get_dashboard_data()` with keyword arguments like `status`, `language`, etc. using the wrong parameter names. The function signature uses different parameter names.

**Evidence**:

From `server.py` lines 2248-2256:
```python
dashboard = get_dashboard_data(
    status=status,           # WRONG: should be status_filter
    language=language,       # WRONG: should be language_filter
    min_health_score=min_health_score,
    max_health_score=max_health_score,
    sort_by=sort_by,
    sort_order=sort_order
)
```

From `dashboard.py` lines 219-229, the actual function signature is:
```python
def get_dashboard_data(
    tier1: Optional[GlobalIndexTier1] = None,
    status_filter: Optional[str] = None,        # NOT 'status'
    language_filter: Optional[str] = None,      # NOT 'language'
    health_category_filter: Optional[str] = None,
    min_health_score: Optional[float] = None,
    max_health_score: Optional[float] = None,
    sort_by: Optional[str] = None,
    sort_order: Optional[str] = None,
    limit: Optional[int] = None
) -> DashboardData:
```

Additionally, lines 2283-2285 of `server.py` also reference non-existent attributes:
```python
"average_health_score": dashboard.average_health_score,  # ERROR
"total_size_mb": dashboard.total_size_mb,  # ERROR
```

**Fix Required**: Update the parameter names in the `get_dashboard()` function call and fix the attribute access issues.

---

### Issue 3: search_content (search/rank) fails - `search_code_advanced() got an unexpected keyword argument 'fuzziness_level'`

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`
**Lines**: 1009-1023

**Root Cause**: The `search_content()` mega-tool accepts `fuzziness_level` parameter and passes it to `search_code_advanced()`, but `search_code_advanced()` does not accept this parameter.

**Evidence**:

From `server.py` lines 976 and 1016:
```python
async def search_content(
    ...
    fuzziness_level: Optional[str] = None,  # Defined here
    ...
)
```

```python
return await search_code_advanced(
    pattern=pattern,
    ctx=ctx,
    ...
    fuzziness_level=fuzziness_level,  # Passed but not accepted
    ...
)
```

From `server.py` lines 2863-2871, the `search_code_advanced()` signature is:
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
    # NOTE: No fuzziness_level parameter
```

**Fix Required**: Either remove `fuzziness_level` from the call or add it to `search_code_advanced()` signature.

---

### Issue 4: cross_project_search_tool fails - `unexpected keyword argument 'max_results_per_project'`

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`
**Lines**: 2421-2430

**Root Cause**: The `cross_project_search_tool()` accepts `max_results_per_project` and `use_tier2_cache` parameters, but the underlying `cross_project_search()` function uses `limit` instead of `max_results_per_project` and does not accept `use_tier2_cache`.

**Evidence**:

From `server.py` lines 2378-2388:
```python
async def cross_project_search_tool(
    ctx: Context,
    pattern: str,
    project_ids: Optional[List[str]] = None,
    fuzzy: bool = False,
    case_sensitive: bool = True,
    file_pattern: Optional[str] = None,
    context_lines: int = 0,
    max_results_per_project: int = 100,  # Defined here
    use_tier2_cache: bool = True,        # Defined here
) -> Dict[str, Any]:
```

From `server.py` lines 2421-2430:
```python
result = await cross_project_search(
    pattern=pattern,
    project_ids=project_ids,
    fuzzy=fuzzy,
    case_sensitive=case_sensitive,
    file_pattern=file_pattern,
    context_lines=context_lines,
    max_results_per_project=max_results_per_project,  # WRONG: should be 'limit'
    use_tier2_cache=use_tier2_cache  # WRONG: not accepted
)
```

From `cross_project_search.py` lines 432-445:
```python
async def cross_project_search(
    pattern: str,
    project_ids: Optional[List[str]] = None,
    query_router: Optional[QueryRouter] = None,
    tier1: Optional[GlobalIndexTier1] = None,
    tier2: Optional[GlobalIndexTier2] = None,
    case_sensitive: bool = False,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    limit: int = 100,  # Uses 'limit' not 'max_results_per_project'
    timeout: float = 30.0,
    circuit_breaker: Optional[ProjectCircuitBreaker] = None,
) -> CrossProjectSearchResult:
```

**Fix Required**: Change the parameter name from `max_results_per_project` to `limit` and remove `use_tier2_cache` (or implement it).

---

### Issue 5: list_projects detailed format fails - `get_dashboard_data() got an unexpected keyword argument 'status'`

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`
**Lines**: 2330-2334

**Root Cause**: Same as Issue 2 - incorrect parameter names passed to `get_dashboard_data()`.

**Evidence**:

From `server.py` lines 2330-2334:
```python
dashboard = get_dashboard_data(
    status=status,           # WRONG: should be status_filter
    language=language,       # WRONG: should be language_filter
    min_health_score=min_health_score
)
```

**Fix Required**: Update parameter names to match the actual function signature.

---

### Issue 6: trigger_eviction fails - "No candidates provided and no unloader registered"

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/memory/eviction.py`
**Lines**: 290-331

**Root Cause**: The `trigger_eviction()` MCP tool calls `eviction_manager.emergency_eviction()` with `candidates=None`, expecting the manager to fetch candidates via an unloader, but no unloader has been registered with the eviction manager.

**Evidence**:

From `server.py` lines 8707-8714:
```python
# Perform eviction (candidates will be fetched via unloader if registered)
eviction_result = eviction_manager.emergency_eviction(
    candidates=None,  # Let manager fetch candidates from unloader
    target_mb=target_mb,
    max_projects=None,
)
```

From `eviction.py` lines 317-331:
```python
# Get candidates if not provided
if candidates is None:
    if self._unloader is None:
        error = "No candidates provided and no unloader registered"
        logger.error(error)

        return EvictionResult(
            success=False,
            projects_evicted=[],
            memory_freed_mb=0.0,
            target_mb=target_mb,
            duration_seconds=time.time() - start_time,
            message=error,
            errors=[error],
        )

    candidates = self._unloader.get_loaded_projects()
```

**Fix Required**: Either register an unloader with the eviction manager during initialization, or provide candidates directly.

---

## Architecture Analysis

### How Tools Are Defined and Registered

1. **MCP Tool Decorator**: Tools are defined using the `@mcp.tool()` decorator (e.g., line 86, 2207, 2297, 2377, 8647).

2. **Tool Registration**: The decorated functions are automatically registered as MCP tools through the framework.

3. **Parameter Flow**: Tool parameters are defined in the function signature and validated/used by the function implementation.

4. **Backend Functions**: Tools call backend functions (like `get_dashboard_data()`, `cross_project_search()`, `search_code_advanced()`) which often have different parameter names than expected.

### Pattern Mismatches Found

1. **Data Class Attribute Mismatch**: `DashboardData` does not include all attributes that `GlobalStats` has.
2. **Parameter Name Mismatch**: MCP tool wrapper functions use different parameter names than the backend functions they call.
3. **Missing Unloader Registration**: The eviction system requires an unloader to be registered but it's not initialized.

---

## Recommended Fixes

### Fix 1: DashboardData Missing Attributes

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/global_index/tier1_metadata.py`

**Action**: Add `average_health_score` and `total_size_mb` to `DashboardData` dataclass.

```python
@dataclass
class DashboardData:
    total_projects: int
    total_symbols: int
    total_files: int
    languages: Dict[str, int]
    projects: List[ProjectMetadata]
    last_updated: float
    # ADD THESE TWO LINES:
    average_health_score: float = 1.0
    total_size_mb: float = 0.0
```

Also update `get_dashboard_data()` method around line 281:
```python
dashboard = DashboardData(
    total_projects=stats.total_projects,
    total_symbols=stats.total_symbols,
    total_files=stats.total_files,
    languages=stats.languages,
    projects=projects_list,
    last_updated=last_updated,
    average_health_score=stats.average_health_score,  # ADD THIS
    total_size_mb=stats.total_size_mb  # ADD THIS
)
```

### Fix 2: get_dashboard Parameter Names

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`

**Action**: Update the `get_dashboard_data()` call around line 2249:

```python
dashboard = get_dashboard_data(
    status_filter=status,  # Changed from: status=status
    language_filter=language,  # Changed from: language=language
    min_health_score=min_health_score,
    max_health_score=max_health_score,
    sort_by=sort_by,
    sort_order=sort_order
)
```

### Fix 3: search_content fuzziness_level

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`

**Action**: Remove the `fuzziness_level` parameter from the `search_code_advanced()` call around line 1016:

```python
return await search_code_advanced(
    pattern=pattern,
    ctx=ctx,
    case_sensitive=case_sensitive,
    context_lines=context_lines,
    file_pattern=file_pattern,
    fuzzy=fuzzy,
    # Remove: fuzziness_level=fuzziness_level,
    content_boost=content_boost,
    filepath_boost=filepath_boost,
    highlight_pre_tag=highlight_pre_tag,
    highlight_post_tag=highlight_post_tag,
    page=page,
    page_size=page_size,
)
```

Also remove `fuzziness_level` from the function signature at line 976:
```python
async def search_content(
    ctx: Context,
    action: Literal["search", "find", "rank"],
    pattern: Optional[str] = None,
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    # Remove: fuzziness_level: Optional[str] = None,
    content_boost: float = 1.0,
    ...
```

### Fix 4: cross_project_search_tool Parameters

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`

**Action**: Update the call around line 2421:

```python
result = await cross_project_search(
    pattern=pattern,
    project_ids=project_ids,
    fuzzy=fuzzy,
    case_sensitive=case_sensitive,
    file_pattern=file_pattern,
    context_lines=context_lines,
    limit=max_results_per_project,  # Changed from: max_results_per_project=max_results_per_project
    # Remove: use_tier2_cache=use_tier2_cache
)
```

Also update the function signature around line 2387:
```python
async def cross_project_search_tool(
    ctx: Context,
    pattern: str,
    project_ids: Optional[List[str]] = None,
    fuzzy: bool = False,
    case_sensitive: bool = True,
    file_pattern: Optional[str] = None,
    context_lines: int = 0,
    limit: int = 100,  # Changed from: max_results_per_project
    # Remove: use_tier2_cache: bool = True,
) -> Dict[str, Any]:
```

And update the docstring example around line 2403:
```python
        limit: Maximum results to return (default: 100)  # Changed description
```

### Fix 5: list_projects Parameter Names

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`

**Action**: Update the call around line 2330:

```python
dashboard = get_dashboard_data(
    status_filter=status,  # Changed from: status=status
    language_filter=language,  # Changed from: language=language
    min_health_score=min_health_score
)
```

### Fix 6: trigger_eviction Unloader Registration

**File**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/server.py`

**Action**: Register an unloader during server initialization. Search for where `EvictionManager` or `get_global_manager()` is initialized and add unloader registration.

The fix requires finding where the global eviction manager is created and registering a project unloader callback function that returns loaded projects from the global index.

---

## Additional Observations

1. **Inconsistent Parameter Naming**: There's a pattern of inconsistent parameter naming between wrapper MCP tools and backend functions throughout the codebase.

2. **Data Model Design**: The separation between `GlobalStats` and `DashboardData` is unclear - they seem to serve similar purposes but have different attributes.

3. **Missing Documentation**: The API documentation in docstrings does not match the actual function signatures in several places.

4. **No Integration Tests**: These issues would have been caught by basic integration tests that call the MCP tools directly.

5. **Unused Parameters**: Several functions accept parameters (like `use_tier2_cache`, `fuzziness_level`) that are not actually used by the backend implementations.

---

## Summary

All 6 critical issues have been traced to their root causes with exact file locations and line numbers. The fixes involve:

1. Adding 2 missing attributes to `DashboardData` dataclass
2. Updating 3 function calls with correct parameter names
3. Removing 2 unused parameters
4. Registering an unloader with the eviction manager

These are straightforward fixes that should resolve all reported issues.

---

## Test Environment

- **Test Directory:** `/home/stan/Prod/ccm/`
- **Files Indexed:** 2,844
- **Registry Projects:** 6
- **Storage Backend:** SQLite
- **Index Format:** MessagePack
- **Test Date:** 2026-01-10

---

## Files Requiring Changes

| File | Changes Needed |
|------|----------------|
| `src/leindex/global_index/tier1_metadata.py` | Add 2 attributes to DashboardData |
| `src/leindex/server.py` | 5 function calls/signatures updated |
| `src/leindex/memory/eviction.py` | Unloader registration logic |
