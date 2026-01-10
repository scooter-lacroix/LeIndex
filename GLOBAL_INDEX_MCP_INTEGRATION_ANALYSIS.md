# MCP Tool Registration Pattern Analysis - Global Index Integration

## Executive Summary

This document provides a comprehensive analysis of the MCP tool registration pattern in `src/leindex/server.py` and specific recommendations for adding 4 new global index tools:

1. `get_global_stats()` - Return global statistics from GlobalIndexTier1
2. `get_dashboard()` - Return project comparison dashboard data
3. `list_projects()` - List projects with optional filtering
4. `cross_project_search()` - Search across multiple projects

---

## 1. Existing MCP Tool Registration Pattern

### 1.1 Tool Definition Pattern

**Location:** Tools are defined using the `@mcp.tool()` decorator

**Example 1: Simple Tool (backup_registry)**
```python
@mcp.tool()
async def backup_registry(
    ctx: Context,
) -> Dict[str, Any]:
    """
    Create an immediate backup of the project registry.

    Returns the backup file path and metadata.

    Returns:
        Dictionary with backup information

    Example:
        {
            "success": true,
            "backup_path": "/path/to/backup.db",
            "project_count": 5,
            "timestamp": "2025-01-01T12:00:00"
        }
    """
    from .registry import ProjectRegistry, RegistryBackupManager

    try:
        registry = ProjectRegistry()
        backup_manager = RegistryBackupManager()

        # Create backup
        backup_metadata = backup_manager.create_backup(registry=registry)

        return {
            "success": True,
            "backup_path": str(backup_metadata.backup_path),
            "project_count": backup_metadata.project_count,
            "timestamp": backup_metadata.timestamp.isoformat(),
            "backup_size": backup_metadata.backup_size_bytes,
            "checksum": backup_metadata.checksum,
            "message": f"Backup created at {backup_metadata.backup_path}"
        }

    except Exception as e:
        logger.error(f"Error creating registry backup: {e}")
        return {
            "success": False,
            "error": str(e),
        }
```

**Example 2: Mega-Tool with Action Dispatch (get_diagnostics)**
```python
@mcp.tool()
async def get_diagnostics(
    ctx: Context,
    type: Literal[
        "memory",
        "index",
        "backend",
        "performance",
        "operations",
        "settings",
        "ignore",
        "filtering",
        "ranking",
    ],
    force_refresh: bool = False,
) -> Dict[str, Any]:
    """
    Get comprehensive diagnostics and metrics for all system components.

    This mega-tool provides unified access to all diagnostic information.

    Types:
        - "memory": Get comprehensive memory profiling statistics
        - "index": Get comprehensive index statistics (params: force_refresh)
        - "backend": Get health status of all backends
        - "performance": Get performance monitoring metrics
        - "operations": Get status of all active operations
        - "settings": Get information about project settings
        - "ignore": Get information about loaded ignore patterns
        - "filtering": Get current filtering configuration
        - "ranking": Get search ranking configuration

    Examples:
        await get_diagnostics(ctx, "memory")
        await get_diagnostics(ctx, "index", force_refresh=True)
        await get_diagnostics(ctx, "backend")
        await get_diagnostics(ctx, "performance")
    """
    match type:
        case "memory":
            return get_memory_profile()
        case "index":
            return await get_index_statistics(ctx, force_refresh)
        case "backend":
            return await get_backend_health(ctx)
        case "performance":
            return get_performance_metrics()
        case "operations":
            return get_active_operations()
        case "settings":
            return get_settings_info(ctx)
        case "ignore":
            return get_ignore_patterns(ctx)
        case "filtering":
            return get_filtering_config()
        case "ranking":
            return get_ranking_configuration(ctx)
        case _:
            return {"success": False, "error": f"Unknown type: {type}"}
```

**Example 3: Mega-Tool with Multiple Actions (manage_files)**
```python
@mcp.tool()
async def manage_files(
    ctx: Context,
    action: Literal["delete", "rename", "revert", "history"],
    file_path: Optional[str] = None,
    new_file_path: Optional[str] = None,
    version_id: Optional[str] = None,
    timestamp: Optional[str] = None,
) -> Dict[str, Any]:
    """
    Manage file system operations including delete, rename, revert, and history.

    This mega-tool provides comprehensive file management with version tracking.

    Actions:
        - "delete": Delete a file from the filesystem (requires: file_path)
        - "rename": Rename or move a file (requires: file_path, new_file_path)
        - "revert": Revert a file to a previous version (requires: file_path, version_id or timestamp)
        - "history": Get the change history for a file (requires: file_path)

    Examples:
        await manage_files(ctx, "delete", file_path="old_file.py")
        await manage_files(ctx, "rename", file_path="src/old.py", new_file_path="src/new.py")
        await manage_files(ctx, "revert", file_path="config.json", version_id="v1.2.3")
        await manage_files(ctx, "history", file_path="src/main.py")
    """
    match action:
        case "delete":
            if file_path is None:
                return {
                    "success": False,
                    "error": "file_path parameter is required for delete action",
                }
            return await delete_file(file_path, ctx)
        case "rename":
            if file_path is None or new_file_path is None:
                return {
                    "success": False,
                    "error": "file_path and new_file_path required for rename action",
                }
            return await rename_file(file_path, new_file_path, ctx)
        case "revert":
            if file_path is None:
                return {
                    "success": False,
                    "error": "file_path parameter is required for revert action",
                }
            return await revert_file_to_version(file_path, ctx, version_id, timestamp)
        case "history":
            if file_path is None:
                return {
                    "success": False,
                    "error": "file_path parameter is required for history action",
                }
            return get_file_history(file_path, ctx)
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}
```

### 1.2 Tool Registration Location

**File Structure:**
- Line 617: `mcp = FastMCP("LeIndex", lifespan=indexer_lifespan)` - Server initialization
- Lines 619-666: Resources section (`@mcp.resource()` decorators)
- Lines 769-2003: MCP Tools section (`@mcp.tool()` decorators)
  - Lines 775-823: Mega-tool 1: manage_project
  - Lines 826-913: Mega-tool 2: search_content
  - Lines 916-986: Mega-tool 3: manage_file (content modification)
  - Lines 989-1045: Mega-tool 4: manage_files (file operations)
  - Lines 1054-1112: Mega-tool 5: get_diagnostics
  - Lines 1119-1207: Mega-tool 6: manage_memory
  - Lines 1210-1268: Mega-tool 7: read_file
  - Lines 1270-1305: Mega-tool 8: manage_temp
  - Lines 1307-1402: Mega-tool 9: manage_operations
  - Lines 1405-1540: Mega-tool 10: search_content (search)
  - Lines 1542-1624: Meta-registry tools
  - Lines 1954-1998: Tool 7: backup_registry

**Recommended Location:** Add new global index tools after line 2003 (after "END OF META-REGISTRY MCP TOOLS" and before "END OF MEGA-TOOLS" comment).

### 1.3 Tool Function Signature Pattern

**Required Elements:**

1. **Decorator:** `@mcp.tool()` - Registers the function as an MCP tool
2. **Context Parameter:** `ctx: Context` - MCP context for accessing lifespan context
3. **Return Type:** `Dict[str, Any]` or `Union[str, Dict[str, Any]]` - Structured return
4. **Docstring:** Comprehensive documentation with examples
5. **Error Handling:** Try-except blocks with structured error returns

**Function Signature Template:**
```python
@mcp.tool()
async def tool_name(
    ctx: Context,
    required_param: type,
    optional_param: type = default_value,
) -> Dict[str, Any]:
    """
    Brief description of what the tool does.

    Detailed explanation of the tool's purpose and behavior.

    Args:
        required_param: Description of required parameter
        optional_param: Description of optional parameter

    Returns:
        Dictionary with result structure

    Examples:
        tool_name(ctx, "value")
        tool_name(ctx, "value", optional_param="optional")
    """
    try:
        # Implementation
        return {
            "success": True,
            "data": result
        }
    except Exception as e:
        logger.error(f"Error in tool_name: {e}")
        return {
            "success": False,
            "error": str(e)
        }
```

---

## 2. Global Index Module Analysis

### 2.1 Available Global Index Components

**Location:** `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer/src/leindex/global_index/`

**Key Files:**
- `__init__.py` - Public API exports
- `tier1_metadata.py` - GlobalIndexTier1, ProjectMetadata, DashboardData, GlobalStats
- `tier2_cache.py` - GlobalIndexTier2, query caching
- `global_index.py` - GlobalIndex coordinator class
- `dashboard.py` - Dashboard data functions
- `cross_project_search.py` - Cross-project search implementation
- `query_router.py` - Query routing logic
- `monitoring.py` - Monitoring and health checks

### 2.2 Public API Functions (from __init__.py)

```python
# Tier 1 - Metadata
from .tier1_metadata import (
    GlobalIndexTier1,
    ProjectMetadata,
    GlobalStats,
    DashboardData
)

# Dashboard
from .dashboard import (
    get_dashboard_data,
    get_project_comparison,
    get_language_distribution
)

# Cross-Project Search
from .cross_project_search import (
    cross_project_search,
    CrossProjectSearchResult,
    ProjectSearchResult,
    CrossProjectSearchError,
    ProjectNotFoundError,
    AllProjectsFailedError,
    InvalidPatternError
)
```

### 2.3 GlobalIndexTier1 API

**Key Methods:**
```python
class GlobalIndexTier1:
    def __init__(self):
        """Initialize an empty Tier 1 index (thread-safe)."""

    def get_project_metadata(self, project_id: str) -> Optional[ProjectMetadata]:
        """Get metadata for a specific project."""

    def get_dashboard_data(self) -> DashboardData:
        """Get complete dashboard data including all projects and global stats.
        Performance Target: <1ms (P50)"""
```

**DashboardData Structure:**
```python
@dataclass
class DashboardData:
    total_projects: int
    total_symbols: int
    total_files: int
    languages: Dict[str, int]
    average_health_score: float
    total_size_mb: float
    projects: List[ProjectMetadata]
    last_updated: float
```

**ProjectMetadata Structure:**
```python
@dataclass
class ProjectMetadata:
    id: str
    name: str
    path: str
    last_indexed: float
    symbol_count: int
    file_count: int
    languages: Dict[str, int]
    dependencies: List[str]
    health_score: float
    index_status: str
    size_mb: float
```

### 2.4 Dashboard API

**get_dashboard_data() Function:**
```python
def get_dashboard_data(
    status: Optional[str] = None,
    language: Optional[str] = None,
    min_health_score: Optional[float] = None,
    max_health_score: Optional[float] = None,
    sort_by: str = "last_indexed",
    sort_order: str = "descending"
) -> DashboardData:
    """
    Get dashboard data with optional filtering and sorting.

    Args:
        status: Filter by index status ("completed", "building", "error", "partial")
        language: Filter by primary programming language
        min_health_score: Minimum health score (0.0 - 1.0)
        max_health_score: Maximum health score (0.0 - 1.0)
        sort_by: Field to sort by (name, path, last_indexed, file_count, etc.)
        sort_order: Sort order ("ascending" or "descending")

    Returns:
        DashboardData with filtered and sorted projects
    """
```

### 2.5 Cross-Project Search API

**cross_project_search() Function:**
```python
async def cross_project_search(
    pattern: str,
    project_ids: Optional[List[str]] = None,
    fuzzy: bool = False,
    case_sensitive: bool = True,
    file_pattern: Optional[str] = None,
    context_lines: int = 0,
    max_results_per_project: int = 100,
    use_tier2_cache: bool = True
) -> CrossProjectSearchResult:
    """
    Search across multiple projects for a given pattern.

    Args:
        pattern: Search pattern (regex-compatible)
        project_ids: List of project IDs to search (None = all projects)
        fuzzy: Enable fuzzy search
        case_sensitive: Case-sensitive search
        file_pattern: Filter results by file pattern (glob)
        context_lines: Number of context lines to include
        max_results_per_project: Maximum results per project
        use_tier2_cache: Use Tier 2 cache if available

    Returns:
        CrossProjectSearchResult with aggregated results
    """
```

---

## 3. Integration Approach for 4 New Tools

### 3.1 Import Statements (Add to top of server.py)

**Location:** Around line 78-82 (after stats_dashboard import)

```python
# Global Index imports
from .global_index import (
    GlobalIndexTier1,
    DashboardData,
    ProjectMetadata,
    GlobalStats,
    get_dashboard_data,
    get_project_comparison,
    get_language_distribution,
    cross_project_search,
    CrossProjectSearchResult,
    ProjectSearchResult,
    CrossProjectSearchError,
    ProjectNotFoundError,
    AllProjectsFailedError,
    InvalidPatternError
)
from .global_index.global_index import GlobalIndex, GlobalIndexConfig
```

### 3.2 Global Instance Initialization

**Location:** Around line 125 (after stats_collector)

```python
# Global index instance for cross-project operations
global_index: Optional[GlobalIndex] = None


def ensure_global_index() -> GlobalIndex:
    """Ensure global index is initialized."""
    global global_index
    if global_index is None:
        try:
            config = GlobalIndexConfig()
            global_index = GlobalIndex(config=config)
            global_index.subscribe_to_events()
            logger.info("Initialized global index with event subscriptions")
        except Exception as e:
            logger.warning(f"Could not initialize global index: {e}")
            # Create a minimal global index
            global_index = GlobalIndex()
    return global_index
```

**Add to LeIndexContext:** Around line 333

```python
@dataclass
class LeIndexContext:
    # ... existing fields ...
    # Global index
    global_index: Optional[GlobalIndex] = None
```

**Add to indexer_lifespan:** Around line 340

```python
global dal_instance
global result_ranker, api_key_manager, stats_collector, global_index
```

**Initialize in lifespan:** Around line 430

```python
# Initialize global index
try:
    global_index = ensure_global_index()
    context.global_index = global_index
    logger.info("Global index initialized in lifespan context")
except Exception as e:
    logger.warning(f"Could not initialize global index in lifespan: {e}")
    global_index = None
    context.global_index = None
```

### 3.3 Tool Implementation Strategy

#### Option A: Individual Tools (Recommended for Simplicity)

Add 4 separate tools with clear purposes:

```python
# Tool 1: get_global_stats
@mcp.tool()
def get_global_stats(
    ctx: Context,
) -> Dict[str, Any]:
    """
    Get global aggregate statistics across all indexed projects.

    Returns total projects, symbols, files, languages, and health scores.

    Returns:
        Dictionary with global statistics

    Example:
        {
            "total_projects": 5,
            "total_symbols": 50000,
            "total_files": 250,
            "languages": {"Python": 150, "JavaScript": 100},
            "average_health_score": 0.85
        }
    """
    try:
        tier1 = GlobalIndexTier1()
        dashboard = tier1.get_dashboard_data()

        return {
            "success": True,
            "stats": {
                "total_projects": dashboard.total_projects,
                "total_symbols": dashboard.total_symbols,
                "total_files": dashboard.total_files,
                "languages": dashboard.languages,
                "average_health_score": dashboard.average_health_score,
                "total_size_mb": dashboard.total_size_mb,
                "last_updated": dashboard.last_updated
            }
        }
    except Exception as e:
        logger.error(f"Error getting global stats: {e}")
        return {
            "success": False,
            "error": str(e)
        }


# Tool 2: get_dashboard
@mcp.tool()
def get_dashboard(
    ctx: Context,
    status: Optional[str] = None,
    language: Optional[str] = None,
    min_health_score: Optional[float] = None,
    max_health_score: Optional[float] = None,
    sort_by: str = "last_indexed",
    sort_order: str = "descending",
) -> Dict[str, Any]:
    """
    Get project comparison dashboard with optional filtering and sorting.

    Args:
        status: Filter by index status ("completed", "building", "error", "partial")
        language: Filter by primary programming language
        min_health_score: Minimum health score (0.0 - 1.0)
        max_health_score: Maximum health score (0.0 - 1.0)
        sort_by: Field to sort by (name, path, last_indexed, file_count, etc.)
        sort_order: Sort order ("ascending" or "descending")

    Returns:
        Dashboard data with filtered and sorted projects

    Examples:
        get_dashboard(ctx)
        get_dashboard(ctx, status="completed", language="Python")
        get_dashboard(ctx, min_health_score=0.8, sort_by="health_score")
    """
    try:
        dashboard = get_dashboard_data(
            status=status,
            language=language,
            min_health_score=min_health_score,
            max_health_score=max_health_score,
            sort_by=sort_by,
            sort_order=sort_order
        )

        # Convert projects to dicts for JSON serialization
        projects_data = [
            {
                "id": p.id,
                "name": p.name,
                "path": p.path,
                "last_indexed": p.last_indexed,
                "symbol_count": p.symbol_count,
                "file_count": p.file_count,
                "languages": p.languages,
                "dependencies": p.dependencies,
                "health_score": p.health_score,
                "index_status": p.index_status,
                "size_mb": p.size_mb
            }
            for p in dashboard.projects
        ]

        return {
            "success": True,
            "dashboard": {
                "total_projects": dashboard.total_projects,
                "total_symbols": dashboard.total_symbols,
                "total_files": dashboard.total_files,
                "languages": dashboard.languages,
                "average_health_score": dashboard.average_health_score,
                "total_size_mb": dashboard.total_size_mb,
                "last_updated": dashboard.last_updated,
                "projects": projects_data
            }
        }
    except Exception as e:
        logger.error(f"Error getting dashboard: {e}")
        return {
            "success": False,
            "error": str(e)
        }


# Tool 3: list_projects
@mcp.tool()
def list_projects(
    ctx: Context,
    status: Optional[str] = None,
    language: Optional[str] = None,
    min_health_score: Optional[float] = None,
    format: Literal["simple", "detailed"] = "simple",
) -> Dict[str, Any]:
    """
    List projects with optional filtering.

    Args:
        status: Filter by index status ("completed", "building", "error", "partial")
        language: Filter by primary programming language
        min_health_score: Minimum health score (0.0 - 1.0)
        format: Output format - "simple" (name, path, status) or "detailed" (all metadata)

    Returns:
        List of projects matching filters

    Examples:
        list_projects(ctx)
        list_projects(ctx, status="completed")
        list_projects(ctx, language="Python", format="detailed")
    """
    try:
        dashboard = get_dashboard_data(
            status=status,
            language=language,
            min_health_score=min_health_score
        )

        if format == "simple":
            projects = [
                {
                    "id": p.id,
                    "name": p.name,
                    "path": p.path,
                    "status": p.index_status
                }
                for p in dashboard.projects
            ]
        else:  # detailed
            projects = [
                {
                    "id": p.id,
                    "name": p.name,
                    "path": p.path,
                    "last_indexed": p.last_indexed,
                    "symbol_count": p.symbol_count,
                    "file_count": p.file_count,
                    "languages": p.languages,
                    "dependencies": p.dependencies,
                    "health_score": p.health_score,
                    "index_status": p.index_status,
                    "size_mb": p.size_mb
                }
                for p in dashboard.projects
            ]

        return {
            "success": True,
            "count": len(projects),
            "projects": projects
        }
    except Exception as e:
        logger.error(f"Error listing projects: {e}")
        return {
            "success": False,
            "error": str(e)
        }


# Tool 4: cross_project_search
@mcp.tool()
async def cross_project_search_tool(
    ctx: Context,
    pattern: str,
    project_ids: Optional[List[str]] = None,
    fuzzy: bool = False,
    case_sensitive: bool = True,
    file_pattern: Optional[str] = None,
    context_lines: int = 0,
    max_results_per_project: int = 100,
    use_tier2_cache: bool = True,
) -> Dict[str, Any]:
    """
    Search across multiple projects for a given pattern.

    This tool performs federated search across specified projects,
    aggregating and ranking results from all projects.

    Args:
        pattern: Search pattern (regex-compatible)
        project_ids: List of project IDs to search (None = all projects)
        fuzzy: Enable fuzzy search
        case_sensitive: Case-sensitive search
        file_pattern: Filter results by file pattern (glob)
        context_lines: Number of context lines to include
        max_results_per_project: Maximum results per project
        use_tier2_cache: Use Tier 2 cache if available

    Returns:
        Aggregated search results across projects

    Examples:
        await cross_project_search_tool(ctx, "def foo\\(")
        await cross_project_search_tool(ctx, "class User", project_ids=["proj1", "proj2"])
        await cross_project_search_tool(ctx, "TODO", fuzzy=True, case_sensitive=False)
    """
    try:
        result = await cross_project_search(
            pattern=pattern,
            project_ids=project_ids,
            fuzzy=fuzzy,
            case_sensitive=case_sensitive,
            file_pattern=file_pattern,
            context_lines=context_lines,
            max_results_per_project=max_results_per_project,
            use_tier2_cache=use_tier2_cache
        )

        # Convert results to dicts
        projects_results = []
        for proj_result in result.project_results:
            projects_results.append({
                "project_id": proj_result.project_id,
                "success": proj_result.success,
                "error": proj_result.error,
                "result_count": len(proj_result.matches) if proj_result.matches else 0,
                "matches": [
                    {
                        "file_path": m.file_path,
                        "line_number": m.line_number,
                        "line_content": m.line_content,
                        "context_before": m.context_before,
                        "context_after": m.context_after
                    }
                    for m in (proj_result.matches or [])
                ]
            })

        return {
            "success": True,
            "total_results": result.total_results,
            "projects_searched": result.projects_searched,
            "successful_projects": result.successful_projects,
            "failed_projects": result.failed_projects,
            "project_results": projects_results
        }
    except ProjectNotFoundError as e:
        logger.error(f"Project not found: {e}")
        return {
            "success": False,
            "error": f"Project not found: {e.project_id}",
            "error_type": "ProjectNotFoundError"
        }
    except InvalidPatternError as e:
        logger.error(f"Invalid search pattern: {e}")
        return {
            "success": False,
            "error": f"Invalid search pattern: {e.message}",
            "error_type": "InvalidPatternError"
        }
    except AllProjectsFailedError as e:
        logger.error(f"All projects failed: {e}")
        return {
            "success": False,
            "error": f"All projects failed to search: {e.errors}",
            "error_type": "AllProjectsFailedError"
        }
    except CrossProjectSearchError as e:
        logger.error(f"Cross-project search error: {e}")
        return {
            "success": False,
            "error": str(e),
            "error_type": "CrossProjectSearchError"
        }
    except Exception as e:
        logger.error(f"Unexpected error in cross_project_search: {e}")
        return {
            "success": False,
            "error": str(e),
            "error_type": "UnexpectedError"
        }
```

#### Option B: Mega-Tool Approach (Alternative)

Create a single mega-tool with action dispatch:

```python
@mcp.tool()
def manage_global_index(
    ctx: Context,
    action: Literal["get_stats", "get_dashboard", "list_projects", "search"],
    # Dashboard params
    status: Optional[str] = None,
    language: Optional[str] = None,
    min_health_score: Optional[float] = None,
    max_health_score: Optional[float] = None,
    sort_by: str = "last_indexed",
    sort_order: str = "descending",
    format: Literal["simple", "detailed"] = "simple",
    # Search params
    pattern: Optional[str] = None,
    project_ids: Optional[List[str]] = None,
    fuzzy: bool = False,
    case_sensitive: bool = True,
    file_pattern: Optional[str] = None,
    context_lines: int = 0,
    max_results_per_project: int = 100,
    use_tier2_cache: bool = True,
) -> Dict[str, Any]:
    """
    Manage global index operations including stats, dashboard, listing, and search.

    Actions:
        - "get_stats": Get global aggregate statistics
        - "get_dashboard": Get project comparison dashboard with filtering
        - "list_projects": List projects with optional filtering
        - "search": Cross-project search

    Examples:
        manage_global_index(ctx, "get_stats")
        manage_global_index(ctx, "get_dashboard", status="completed")
        manage_global_index(ctx, "list_projects", language="Python")
        await manage_global_index(ctx, "search", pattern="def foo\\(")
    """
    match action:
        case "get_stats":
            return _get_global_stats_impl()
        case "get_dashboard":
            return _get_dashboard_impl(status, language, min_health_score, max_health_score, sort_by, sort_order)
        case "list_projects":
            return _list_projects_impl(status, language, min_health_score, format)
        case "search":
            if pattern is None:
                return {"success": False, "error": "pattern parameter required for search action"}
            return await _cross_project_search_impl(
                pattern, project_ids, fuzzy, case_sensitive,
                file_pattern, context_lines, max_results_per_project, use_tier2_cache
            )
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}
```

**Recommendation:** Use Option A (Individual Tools) because:
1. Clearer tool purposes and namespacing
2. Better discoverability for users
3. Simpler parameter validation per tool
4. Easier to document and maintain
5. Follows existing tool patterns in the codebase

---

## 4. Thread Safety Considerations

### 4.1 GlobalIndexTier1 Thread Safety

**From Code Analysis:**
```python
class GlobalIndexTier1:
    def __init__(self):
        self._lock: threading.Lock = threading.Lock()

    def get_dashboard_data(self) -> DashboardData:
        with self._lock:
            # Thread-safe access
            if self._stats_dirty:
                self._recompute_global_stats_locked()
            projects_list = list(self._projects.values())
```

**Status:** ✅ Thread-safe by design

### 4.2 Cross-Project Search Thread Safety

**From Code Analysis:**
- Creates new GlobalIndexTier1 instance per call
- No shared mutable state between calls
- Async operations properly awaited

**Status:** ✅ Thread-safe (instance per call)

### 4.3 Dashboard Functions Thread Safety

**From Code Analysis:**
```python
def get_dashboard_data(...) -> DashboardData:
    tier1 = GlobalIndexTier1()  # New instance per call
    # All operations are thread-safe
```

**Status:** ✅ Thread-safe (instance per call)

### 4.4 Recommendations

1. **No global state modifications:** Tools should only read from global index
2. **Instance per call:** Create new GlobalIndexTier1 instance in each tool call
3. **Proper error handling:** Catch and log all exceptions
4. **Structured returns:** Always return Dict[str, Any] with success/error fields

---

## 5. Security Considerations

### 5.1 Path Traversal Protection

The global index already has security mechanisms:

```python
from .global_index.security import (
    sanitize_project_path,
    validate_project_id,
    redact_sensitive_data
)
```

**Recommendations:**
1. Validate all project_id inputs
2. Sanitize file patterns in search
3. Redact sensitive data in error messages
4. Use existing security context from global_index module

### 5.2 Pattern Validation

The cross_project_search function includes pattern validation:

```python
def _validate_pattern(pattern: str) -> None:
    """Validate search pattern to prevent catastrophic patterns."""
    # Already implemented in cross_project_search.py
```

**Status:** ✅ Already implemented

### 5.3 Project Access Control

```python
def _validate_project_access(project_id: str) -> None:
    """Ensure user has access to requested project."""
    # Already implemented in cross_project_search.py
```

**Status:** ✅ Already implemented

---

## 6. Performance Considerations

### 6.1 Performance Targets

**From Global Index Specs:**
- `get_dashboard_data()`: <1ms (P50)
- `get_project_metadata()`: O(1) lookup
- `on_project_indexed()`: <5ms synchronous update
- Memory: <10MB for 100 projects

### 6.2 Caching Strategy

**Current Implementation:**
- Tier 1: Always fresh (synchronous updates)
- Tier 2: Stale-allowed query cache (not needed for stats/dashboard)

**Recommendation:**
- No additional caching needed for stats/dashboard (already <1ms)
- Tier 2 cache will be used automatically for cross_project_search if enabled

### 6.3 Monitoring

**From Code:**
```python
from .global_index.monitoring import log_global_index_operation
```

**Recommendation:**
- Add logging to each new tool call
- Monitor performance metrics
- Track error rates

---

## 7. Testing Strategy

### 7.1 Unit Tests Required

1. **get_global_stats()**
   - Test with no projects
   - Test with multiple projects
   - Verify statistics calculation
   - Test error handling

2. **get_dashboard()**
   - Test default (no filters)
   - Test each filter individually
   - Test filter combinations
   - Test sorting options
   - Verify JSON serialization

3. **list_projects()**
   - Test simple format
   - Test detailed format
   - Test filtering
   - Verify project list completeness

4. **cross_project_search_tool()**
   - Test single project search
   - Test multi-project search
   - Test fuzzy search
   - Test error handling (project not found, invalid pattern)
   - Verify result aggregation

### 7.2 Integration Tests Required

1. Test tools with actual project registry
2. Verify event subscriptions work
3. Test concurrent access
4. Verify performance targets

---

## 8. Implementation Checklist

### Phase 1: Setup
- [ ] Add global index imports to server.py (line ~78)
- [ ] Add global_index global variable (line ~125)
- [ ] Add ensure_global_index() function
- [ ] Add global_index to LeIndexContext (line ~333)
- [ ] Initialize global_index in indexer_lifespan

### Phase 2: Tool Implementation
- [ ] Implement get_global_stats() tool
- [ ] Implement get_dashboard() tool
- [ ] Implement list_projects() tool
- [ ] Implement cross_project_search_tool() (async)

### Phase 3: Testing
- [ ] Unit tests for each tool
- [ ] Integration tests with project registry
- [ ] Performance tests (<1ms for dashboard)
- [ ] Thread safety tests
- [ ] Error handling tests

### Phase 4: Documentation
- [ ] Update MCP tool documentation
- [ ] Add usage examples
- [ ] Update API reference
- [ ] Add performance notes

---

## 9. Code Templates

### Template 1: Simple Read-Only Tool

```python
@mcp.tool()
def tool_name(
    ctx: Context,
    required_param: type,
    optional_param: type = default_value,
) -> Dict[str, Any]:
    """
    Brief description.

    Detailed description.

    Args:
        required_param: Description
        optional_param: Description

    Returns:
        Dictionary with result

    Examples:
        tool_name(ctx, "value")
    """
    try:
        # Implementation
        result = perform_operation()

        return {
            "success": True,
            "data": result
        }
    except Exception as e:
        logger.error(f"Error in tool_name: {e}")
        return {
            "success": False,
            "error": str(e)
        }
```

### Template 2: Async Tool

```python
@mcp.tool()
async def async_tool_name(
    ctx: Context,
    required_param: type,
) -> Dict[str, Any]:
    """
    Brief description.

    Detailed description.

    Args:
        required_param: Description

    Returns:
        Dictionary with result

    Examples:
        await async_tool_name(ctx, "value")
    """
    try:
        # Async implementation
        result = await async_operation()

        return {
            "success": True,
            "data": result
        }
    except SpecificException as e:
        logger.error(f"Specific error: {e}")
        return {
            "success": False,
            "error": str(e),
            "error_type": "SpecificException"
        }
    except Exception as e:
        logger.error(f"Unexpected error: {e}")
        return {
            "success": False,
            "error": str(e),
            "error_type": "UnexpectedError"
        }
```

---

## 10. Summary and Recommendations

### 10.1 Recommended Approach

1. **Use Individual Tools** (Option A) for clarity and discoverability
2. **Add imports** at line 78-82 in server.py
3. **Initialize global_index** in lifespan context (around line 430)
4. **Add tools** after line 2003 (after meta-registry tools)
5. **Follow existing patterns** for error handling and return values
6. **Use thread-safe components** (already implemented in global_index)
7. **Add comprehensive logging** for monitoring
8. **Test thoroughly** before deployment

### 10.2 Key Points

✅ **Thread Safety:** All global index components are thread-safe
✅ **Performance:** Dashboard operations target <1ms response time
✅ **Security:** Pattern validation and project access control already implemented
✅ **Error Handling:** Structured error returns with specific exception types
✅ **Monitoring:** Built-in logging and performance tracking

### 10.3 Next Steps

1. Review this analysis with the team
2. Decide on Option A (individual tools) vs Option B (mega-tool)
3. Implement Phase 1 (Setup)
4. Implement Phase 2 (Tools)
5. Implement Phase 3 (Testing)
6. Implement Phase 4 (Documentation)

---

## Appendix: Existing Tool Examples Reference

### Example 1: Simple Registry Tool
**Location:** Lines 1954-1998
**Function:** backup_registry
**Pattern:** Simple async tool with error handling

### Example 2: Mega-Tool with Dispatch
**Location:** Lines 1054-1112
**Function:** get_diagnostics
**Pattern:** Action-based routing with type validation

### Example 3: Multi-Action Mega-Tool
**Location:** Lines 989-1045
**Function:** manage_files
**Pattern:** Multiple actions with parameter validation

### Example 4: Complex Async Tool
**Location:** Lines 1210-1268
**Function:** read_file
**Pattern:** Multiple modes with complex parameter handling
