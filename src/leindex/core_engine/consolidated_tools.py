"""
Consolidated MCP Tool Schemas for LeIndex

This module defines the unified schemas for the 9 mega-tools that consolidate
the original 50+ individual MCP tools. Each mega-tool uses action/type-based
routing to preserve all existing functionality while reducing tool selection
overhead.

Mega-Tools Structure:
1. manage_project - Project lifecycle management
2. search_content - Content search and discovery
3. modify_file - File content modifications
4. manage_files - File system operations
5. get_diagnostics - System diagnostics and metrics
6. manage_memory - Memory management
7. manage_operations - Operation tracking and control
8. read_file - File reading strategies
9. manage_temp - Temporary directory management

Design Principles:
- All parameters for all actions are included in the unified schema
- Optional parameters are used for action-specific values
- Literal types enforce valid action/type values at type-check time
- All original docstrings are preserved
- Backward compatibility is maintained for return types
"""

from typing import (
    Any, Dict, List, Optional, Union, Literal,
)
from mcp.server.fastmcp import Context


# =============================================================================
# MEGA-TOOL 1: manage_project
# =============================================================================
# Consolidates: set_project_path, refresh_index, force_reindex, clear_settings, reset_server_state
# Actions: "set_path", "refresh", "reindex", "clear", "reset"
# =============================================================================

ManageProjectAction = Literal["set_path", "refresh", "reindex", "clear", "reset"]


def manage_project(
    ctx: Context,
    action: ManageProjectAction,
    # Parameters for "set_path" action
    path: Optional[str] = None,
    # Parameters for "reindex" action
    clear_cache: bool = True,
) -> Union[str, Dict[str, Any]]:
    """
    Manage project lifecycle operations including setting path, refreshing, and reindexing.

    This mega-tool consolidates all project-level operations into a single interface
    with action-based routing. Each action preserves all parameters from the original
    individual tools.

    Routing Logic:
        - "set_path": Set the base project path for indexing
            * Parameters: path (required)
            * Returns: str with success message or error
            * Original tool: set_project_path()

        - "refresh": Refresh the project index using incremental indexing
            * Parameters: None (uses current project path)
            * Returns: Dict with operation_id, files_processed, metadata_stats
            * Original tool: refresh_index()

        - "reindex": Force a complete re-index of the project
            * Parameters: clear_cache (bool, default: True)
            * Returns: Dict with operation_id, files_processed, cache_cleared
            * Original tool: force_reindex()

        - "clear": Clear all settings and cached data
            * Parameters: None
            * Returns: str confirmation message
            * Original tool: clear_settings()

        - "reset": Completely reset the server state including global variables
            * Parameters: None
            * Returns: str confirmation message
            * Original tool: reset_server_state()

    Args:
        ctx: MCP context providing access to lifespan context
        action: The specific operation to perform (routing key)
        path: Required for "set_path" action - absolute path to project directory
        clear_cache: For "reindex" action - whether to clear all cached data

    Returns:
        Union[str, Dict]: Return type varies by action for backward compatibility

    Examples:
        # Set project path
        result = await manage_project(ctx, "set_path", path="/path/to/project")

        # Refresh with incremental indexing
        result = await manage_project(ctx, "refresh")

        # Force complete reindex without cache clearing
        result = await manage_project(ctx, "reindex", clear_cache=False)

        # Clear all settings
        result = manage_project(ctx, "clear")

        # Reset server state
        result = manage_project(ctx, "reset")
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 2: search_content
# =============================================================================
# Consolidates: search_code_advanced, find_files, rank_search_results
# Actions: "search", "find", "rank"
# =============================================================================

SearchContentAction = Literal["search", "find", "rank"]


def search_content(
    ctx: Context,
    action: SearchContentAction,
    # Common parameters
    pattern: Optional[str] = None,
    # Parameters for "search" action (search_code_advanced)
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    fuzziness_level: Optional[str] = None,
    content_boost: float = 1.0,
    filepath_boost: float = 1.0,
    highlight_pre_tag: str = "<em>",
    highlight_post_tag: str = "</em>",
    page: int = 1,
    page_size: int = 20,
    # Parameters for "rank" action (rank_search_results)
    results: Optional[List[Dict[str, Any]]] = None,
    query: Optional[str] = None,
) -> Union[Dict[str, Any], List[str]]:
    """
    Search and discover content across the project using multiple strategies.

    This mega-tool provides unified access to all content search and discovery
    operations, from advanced semantic search to simple file pattern matching.

    Routing Logic:
        - "search": Advanced code search with multiple backend support
            * Parameters: pattern (required), case_sensitive, context_lines,
              file_pattern, fuzzy, fuzziness_level, content_boost, filepath_boost,
              highlight_pre_tag, highlight_post_tag, page, page_size
            * Returns: Dict with search results or error
            * Original tool: search_code_advanced()

        - "find": Find files matching a glob pattern
            * Parameters: pattern (required, glob pattern like "*.py")
            * Returns: List[str] of matching file paths
            * Original tool: find_files()

        - "rank": Re-rank search results based on query relevance
            * Parameters: results (required, list of search results),
              query (required, search query string)
            * Returns: List[Dict] with ranked results
            * Original tool: rank_search_results()

    Args:
        ctx: MCP context providing access to lifespan context
        action: The specific operation to perform (routing key)
        pattern: Search pattern or file glob pattern (required for search/find)
        case_sensitive: For "search" - whether search is case-sensitive
        context_lines: For "search" - lines of context around matches
        file_pattern: For "search" - glob pattern to filter files
        fuzzy: For "search" - whether to treat pattern as regex
        fuzziness_level: For "search" - ES fuzziness level (e.g., "AUTO", "0", "1", "2")
        content_boost: For "search" - content field boosting factor
        filepath_boost: For "search" - file_path field boosting factor
        highlight_pre_tag: For "search" - HTML tag before highlighted terms
        highlight_post_tag: For "search" - HTML tag after highlighted terms
        page: For "search" - page number for pagination
        page_size: For "search" - results per page
        results: For "rank" - list of search results to rank
        query: For "rank" - query string for ranking

    Returns:
        Union[Dict, List[str], List[Dict]]: Return type varies by action

    Examples:
        # Advanced semantic search
        result = await search_content(ctx, "search", pattern="function foo()",
                                     fuzzy=True, page_size=50)

        # Find all Python files
        result = search_content(ctx, "find", pattern="*.py")

        # Rank search results
        result = search_content(ctx, "rank", results=search_results,
                               query="authentication logic")
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 3: modify_file
# =============================================================================
# Consolidates: write_to_file, apply_diff, insert_content, search_and_replace
# Operations: "write", "diff", "insert", "replace"
# =============================================================================

ModifyFileOperation = Literal["write", "diff", "insert", "replace"]


def modify_file(
    ctx: Context,
    operation: ModifyFileOperation,
    # Common parameters
    path: str,
    # Parameters for "write" operation
    content: Optional[str] = None,
    line_count: Optional[int] = None,
    # Parameters for "diff" operation (apply_diff)
    search: Optional[str] = None,
    replace: Optional[str] = None,
    start_line: Optional[int] = None,
    end_line: Optional[int] = None,
    use_regex: bool = False,
    ignore_case: bool = False,
    # Parameters for "insert" operation
    line: Optional[int] = None,
) -> Dict[str, Any]:
    """
    Modify file content using various strategies with version tracking.

    This mega-tool consolidates all file content modification operations,
    from complete file overwrites to targeted diff-based edits.

    Routing Logic:
        - "write": Write complete content to a file (creates or overwrites)
            * Parameters: path (required), content (required), line_count (required)
            * Returns: Dict with success status and message
            * Original tool: write_to_file()
            * Note: line_count is the number of lines in content

        - "diff": Apply targeted modifications using search/replace
            * Parameters: path (required), search (required), replace (required),
              start_line, end_line, use_regex, ignore_case
            * Returns: Dict with success, replacements_made, lines_modified
            * Original tool: apply_diff()
            * Note: Creates backup file for rollback on error

        - "insert": Insert new content without modifying existing content
            * Parameters: path (required), line (required), content (required)
            * Returns: Dict with success status and message
            * Original tool: insert_content()
            * Note: Use line=0 to append to end of file

        - "replace": Search and replace in file (alias for diff with different defaults)
            * Parameters: path (required), search (required), replace (required),
              start_line, end_line, use_regex, ignore_case
            * Returns: Dict with success status and replacements count
            * Original tool: search_and_replace()

    Args:
        ctx: MCP context providing access to lifespan context
        operation: The specific modification operation (routing key)
        path: Path to the file to modify (relative to project root)
        content: For "write" and "insert" - content to write/insert
        line_count: For "write" - number of lines in content
        search: For "diff" and "replace" - text/pattern to search for
        replace: For "diff" and "replace" - replacement text
        start_line: For "diff" and "replace" - starting line (1-based, inclusive)
        end_line: For "diff" and "replace" - ending line (1-based, inclusive)
        use_regex: For "diff" and "replace" - treat search as regex pattern
        ignore_case: For "diff" and "replace" - case-insensitive matching
        line: For "insert" - line number to insert before (0 = append)

    Returns:
        Dict with operation result including success status and details

    Examples:
        # Write complete file
        result = await modify_file(ctx, "write", path="src/main.py",
                                  content="print('hello')", line_count=1)

        # Apply diff with regex
        result = await modify_file(ctx, "diff", path="config.json",
                                  search=r'"version": "\\d+"',
                                  replace='"version": "2.0"',
                                  use_regex=True)

        # Insert at line 10
        result = await modify_file(ctx, "insert", path="src/main.py",
                                  line=10, content="def new_func():\\n    pass")

        # Append to end
        result = await modify_file(ctx, "insert", path="README.md",
                                  line=0, content="\\n## New Section")

        # Case-insensitive replace
        result = await modify_file(ctx, "replace", path="docs/api.md",
                                  search="todo", replace="TODO",
                                  ignore_case=True)
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 4: manage_files
# =============================================================================
# Consolidates: delete_file, rename_file, revert_file_to_version, get_file_history
# Actions: "delete", "rename", "revert", "history"
# =============================================================================

ManageFilesAction = Literal["delete", "rename", "revert", "history"]


def manage_files(
    ctx: Context,
    action: ManageFilesAction,
    # Common parameters
    file_path: Optional[str] = None,
    # Parameters for "rename" action
    new_file_path: Optional[str] = None,
    # Parameters for "revert" action
    version_id: Optional[str] = None,
    timestamp: Optional[str] = None,
) -> Dict[str, Any]:
    """
    Manage file system operations including delete, rename, revert, and history.

    This mega-tool provides comprehensive file management with full version
    tracking support for all operations.

    Routing Logic:
        - "delete": Delete a file from the filesystem
            * Parameters: file_path (required)
            * Returns: Dict with success status and message
            * Original tool: delete_file()
            * Note: Updates index and removes from metadata store

        - "rename": Rename or move a file
            * Parameters: file_path (required), new_file_path (required)
            * Returns: Dict with success status and message
            * Original tool: rename_file()
            * Note: Creates new version in history

        - "revert": Revert a file to a previous version
            * Parameters: file_path (required), version_id OR timestamp (one required)
            * Returns: Dict with success status and message
            * Original tool: revert_file_to_version()
            * Note: Requires either version_id or timestamp for target version

        - "history": Get the change history for a file
            * Parameters: file_path (required)
            * Returns: Dict with file_path, history list, success status
            * Original tool: get_file_history()
            * Note: Creates initial version if file has no history yet

    Args:
        ctx: MCP context providing access to lifespan context
        action: The specific file management operation (routing key)
        file_path: Path to the file (relative to project root)
        new_file_path: For "rename" - new path for the file
        version_id: For "revert" - specific version ID to revert to
        timestamp: For "revert" - timestamp of version to revert to

    Returns:
        Dict with operation result, success status, and action-specific details

    Examples:
        # Delete a file
        result = await manage_files(ctx, "delete", file_path="old_file.py")

        # Rename/move a file
        result = await manage_files(ctx, "rename",
                                   file_path="src/old.py",
                                   new_file_path="src/new.py")

        # Revert to specific version
        result = await manage_files(ctx, "revert",
                                   file_path="config.json",
                                   version_id="v1.2.3")

        # Revert to timestamp
        result = await manage_files(ctx, "revert",
                                   file_path="main.py",
                                   timestamp="2024-01-15T10:30:00")

        # Get file history
        result = manage_files(ctx, "history", file_path="src/main.py")
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 5: get_diagnostics
# =============================================================================
# Consolidates: get_memory_profile, get_index_statistics, get_backend_health,
#               get_performance_metrics, get_active_operations, get_settings_info,
#               get_ignore_patterns, get_filtering_config, get_ranking_configuration
# Types: "memory", "index", "backend", "performance", "operations",
#        "settings", "ignore", "filtering", "ranking"
# =============================================================================

DiagnosticsType = Literal[
    "memory", "index", "backend", "performance", "operations",
    "settings", "ignore", "filtering", "ranking"
]


def get_diagnostics(
    ctx: Context,
    type: DiagnosticsType,
    # Parameters for "index" type (get_index_statistics)
    force_refresh: bool = False,
    # Parameters for "ranking" type (get_ranking_configuration)
    # No additional parameters needed
) -> Dict[str, Any]:
    """
    Get comprehensive diagnostics and metrics for all system components.

    This mega-tool provides unified access to all diagnostic information
    across memory, index, backends, performance, operations, and configuration.

    Routing Logic:
        - "memory": Get comprehensive memory profiling statistics
            * Parameters: None
            * Returns: Dict with memory snapshot, limits, violations, stats
            * Original tool: get_memory_profile()
            * Includes: process_memory, peak_memory, loaded_files, violations

        - "index": Get comprehensive index statistics from dashboard
            * Parameters: force_refresh (bool, default: False)
            * Returns: Dict with document counts, sizes, backend health
            * Original tool: get_index_statistics()
            * Includes: SQLite, DuckDB, Tantivy, LEANN statistics

        - "backend": Get health status of all backends
            * Parameters: None
            * Returns: Dict with backend names and health status
            * Original tool: get_backend_health()
            * Includes: SQLite, DuckDB, Tantivy, LEANN connection status

        - "performance": Get performance monitoring metrics
            * Parameters: None
            * Returns: Dict with metrics, operation_stats, recent_logs
            * Original tool: get_performance_metrics()
            * Includes: counters, timers, operation statistics

        - "operations": Get status of all active operations
            * Parameters: None
            * Returns: Dict with active_operations, total_count
            * Original tool: get_active_operations()
            * Includes: operation_id, status, progress for each operation

        - "settings": Get information about project settings
            * Parameters: None
            * Returns: Dict with settings_directory, config, stats
            * Original tool: get_settings_info()
            * Includes: path, existence, configuration data

        - "ignore": Get information about loaded ignore patterns
            * Parameters: None
            * Returns: Dict with pattern_sources, all_patterns
            * Original tool: get_ignore_patterns()
            * Includes: .gitignore, .ignore, default excludes

        - "filtering": Get current filtering configuration
            * Parameters: None
            * Returns: Dict with filtering_configuration, performance_settings
            * Original tool: get_filtering_config()
            * Includes: file size limits, directory limits, parallel processing

        - "ranking": Get search ranking configuration
            * Parameters: None
            * Returns: Dict with weights, recency_settings, frequency_settings
            * Original tool: get_ranking_configuration()
            * Includes: semantic, recency, frequency, path importance weights

    Args:
        ctx: MCP context providing access to lifespan context
        type: The specific diagnostic type to retrieve (routing key)
        force_refresh: For "index" type - force cache refresh

    Returns:
        Dict with diagnostic information (structure varies by type)

    Examples:
        # Check memory usage
        result = get_diagnostics(ctx, "memory")

        # Get index statistics (force refresh)
        result = await get_diagnostics(ctx, "index", force_refresh=True)

        # Check backend health
        result = await get_diagnostics(ctx, "backend")

        # Get performance metrics
        result = get_diagnostics(ctx, "performance")

        # List active operations
        result = get_diagnostics(ctx, "operations")

        # View ignore patterns
        result = get_diagnostics(ctx, "ignore")

        # Check ranking configuration
        result = get_diagnostics(ctx, "ranking")
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 6: manage_memory
# =============================================================================
# Consolidates: trigger_memory_cleanup, configure_memory_limits, export_memory_profile
# Actions: "cleanup", "configure", "export"
# =============================================================================

ManageMemoryAction = Literal["cleanup", "configure", "export"]


def manage_memory(
    ctx: Context,
    action: ManageMemoryAction,
    # Parameters for "configure" action
    soft_limit_mb: Optional[float] = None,
    hard_limit_mb: Optional[float] = None,
    max_loaded_files: Optional[int] = None,
    max_cached_queries: Optional[int] = None,
    # Parameters for "export" action
    file_path: Optional[str] = None,
) -> Dict[str, Any]:
    """
    Manage memory usage including cleanup, configuration, and profiling.

    This mega-tool provides comprehensive memory management capabilities
    for monitoring and controlling the indexer's memory footprint.

    Routing Logic:
        - "cleanup": Manually trigger memory cleanup and garbage collection
            * Parameters: None
            * Returns: Dict with cleanup_results, gc_objects_collected, memory_freed_mb
            * Original tool: trigger_memory_cleanup()
            * Includes: before/after stats, current memory, peak memory

        - "configure": Update memory limits configuration
            * Parameters: soft_limit_mb, hard_limit_mb, max_loaded_files,
              max_cached_queries (all optional)
            * Returns: Dict with old_limits, new_limits, success status
            * Original tool: configure_memory_limits()
            * Note: Only provided parameters are updated

        - "export": Export detailed memory profile to a file
            * Parameters: file_path (optional, auto-generated if None)
            * Returns: Dict with file_path, success status
            * Original tool: export_memory_profile()
            * Note: Default path: tempfile/memory_profile_<timestamp>.json

    Args:
        ctx: MCP context providing access to lifespan context
        action: The specific memory management operation (routing key)
        soft_limit_mb: For "configure" - soft memory limit in MB
        hard_limit_mb: For "configure" - hard memory limit in MB
        max_loaded_files: For "configure" - maximum loaded files
        max_cached_queries: For "configure" - maximum cached queries
        file_path: For "export" - destination file path

    Returns:
        Dict with operation result and action-specific data

    Examples:
        # Trigger memory cleanup
        result = manage_memory(ctx, "cleanup")

        # Update only soft limit
        result = manage_memory(ctx, "configure", soft_limit_mb=1024)

        # Update all limits
        result = manage_memory(ctx, "configure",
                             soft_limit_mb=1024,
                             hard_limit_mb=2048,
                             max_loaded_files=200,
                             max_cached_queries=1000)

        # Export to specific file
        result = manage_memory(ctx, "export",
                             file_path="/tmp/memory_profile.json")

        # Export to auto-generated file
        result = manage_memory(ctx, "export")
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 7: manage_operations
# =============================================================================
# Consolidates: get_active_operations, cancel_operation, cleanup_completed_operations
# Actions: "list", "cancel", "cleanup"
# =============================================================================

ManageOperationsAction = Literal["list", "cancel", "cleanup"]


def manage_operations(
    ctx: Context,
    action: ManageOperationsAction,
    # Parameters for "cancel" action
    operation_id: Optional[str] = None,
    reason: str = "Operation cancelled by user",
    # Parameters for "cleanup" action
    max_age_hours: float = 1.0,
    # Parameters for "cancel_all" (extension of cancel)
    cancel_all: bool = False,
) -> Dict[str, Any]:
    """
    Manage tracked operations including listing, cancelling, and cleanup.

    This mega-tool provides comprehensive operation lifecycle management
    for all long-running tasks with progress tracking.

    Routing Logic:
        - "list": Get status of all active operations
            * Parameters: None
            * Returns: Dict with active_operations list, total_count
            * Original tool: get_active_operations()
            * Includes: operation_id, status, progress, elapsed_time

        - "cancel": Cancel a specific operation or all operations
            * Parameters: operation_id (required unless cancel_all=True),
              reason, cancel_all
            * Returns: Dict with success status, message
            * Original tools: cancel_operation(), cancel_all_operations()
            * Note: Set cancel_all=True to cancel all active operations

        - "cleanup": Clean up completed operations older than specified hours
            * Parameters: max_age_hours (default: 1.0)
            * Returns: Dict with operations_cleaned, before/after counts
            * Original tool: cleanup_completed_operations()
            * Note: Removes old completed operations from tracking

    Args:
        ctx: MCP context providing access to lifespan context
        action: The specific operation management task (routing key)
        operation_id: For "cancel" - ID of operation to cancel
        reason: For "cancel" - reason for cancellation
        max_age_hours: For "cleanup" - maximum age in hours to keep
        cancel_all: For "cancel" - if True, cancel all operations

    Returns:
        Dict with operation result and action-specific data

    Examples:
        # List all active operations
        result = manage_operations(ctx, "list")

        # Cancel specific operation
        result = await manage_operations(ctx, "cancel",
                                       operation_id="op-123",
                                       reason="User requested")

        # Cancel all operations
        result = await manage_operations(ctx, "cancel",
                                       cancel_all=True,
                                       reason="Server shutdown")

        # Cleanup operations older than 2 hours
        result = manage_operations(ctx, "cleanup", max_age_hours=2.0)
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 8: read_file
# =============================================================================
# Consolidates: analyze_file_with_smart_reader, read_file_chunks,
#               detect_file_errors, get_file_metadata
# Modes: "smart", "chunks", "detect_errors", "metadata"
# =============================================================================

ReadFileMode = Literal["smart", "chunks", "detect_errors", "metadata"]


def read_file(
    ctx: Context,
    mode: ReadFileMode,
    file_path: str,
    # Parameters for "smart" mode (analyze_file_with_smart_reader)
    include_content: bool = True,
    include_metadata: bool = True,
    include_errors: bool = True,
    include_chunks: bool = False,
    chunk_size: int = 4 * 1024 * 1024,
    # Parameters for "chunks" mode (read_file_chunks)
    max_chunks: Optional[int] = None,
) -> Dict[str, Any]:
    """
    Read files using various strategies optimized for different use cases.

    This mega-tool provides flexible file reading capabilities with
    automatic strategy selection based on file characteristics.

    Routing Logic:
        - "smart": Comprehensive file analysis using SmartFileReader
            * Parameters: file_path (required), include_content,
              include_metadata, include_errors, include_chunks, chunk_size
            * Returns: Dict with comprehensive file analysis
            * Original tool: analyze_file_with_smart_reader()
            * Includes: content, metadata, errors, file_info, reading_strategy

        - "chunks": Read large file in chunks for memory efficiency
            * Parameters: file_path (required), chunk_size (default: 4MB),
              max_chunks (optional limit)
            * Returns: Dict with chunks_read, total_size_read, content_chunks
            * Original tool: read_file_chunks()
            * Note: Ideal for files larger than available memory

        - "detect_errors": Detect and analyze errors in a file
            * Parameters: file_path (required)
            * Returns: Dict with error_analysis, error_count, error_types
            * Original tool: detect_file_errors()
            * Includes: syntax errors, encoding issues, corruption detection

        - "metadata": Get comprehensive file metadata
            * Parameters: file_path (required)
            * Returns: Dict with metadata, file_info, basic_stats
            * Original tool: get_file_metadata()
            * Includes: timestamps, permissions, size, encoding, strategy

    Args:
        ctx: MCP context providing access to lifespan context
        mode: The specific reading mode (routing key)
        file_path: Path to the file (relative to project root)
        include_content: For "smart" - whether to include file content
        include_metadata: For "smart" - whether to include metadata
        include_errors: For "smart" - whether to include error detection
        include_chunks: For "smart" - whether to read in chunks
        chunk_size: For "smart" and "chunks" - size of each chunk in bytes
        max_chunks: For "chunks" - maximum number of chunks to return

    Returns:
        Dict with file data (structure varies by mode)

    Examples:
        # Full smart analysis
        result = read_file(ctx, "smart", file_path="src/main.py",
                         include_content=True, include_errors=True)

        # Read large file in chunks (max 10 chunks)
        result = read_file(ctx, "chunks", file_path="large_data.json",
                          chunk_size=8*1024*1024, max_chunks=10)

        # Detect errors in a file
        result = read_file(ctx, "detect_errors", file_path="config.py")

        # Get file metadata only
        result = read_file(ctx, "metadata", file_path="README.md")
    """
    pass  # Implementation will route to original functions


# =============================================================================
# MEGA-TOOL 9: manage_temp
# =============================================================================
# Consolidates: create_temp_directory, check_temp_directory
# Actions: "create", "check"
# =============================================================================

ManageTempAction = Literal["create", "check"]


def manage_temp(
    ctx: Context,
    action: ManageTempAction,
) -> Dict[str, Any]:
    """
    Manage the temporary directory used for storing index data.

    This mega-tool provides simple operations for creating and checking
    the temporary directory where the indexer stores cached data.

    Routing Logic:
        - "create": Create the temporary directory if it doesn't exist
            * Parameters: None
            * Returns: Dict with temp_directory, existed_before, created, exists_now
            * Original tool: create_temp_directory()
            * Note: Uses consistent temp directory location

        - "check": Check the temporary directory status and contents
            * Parameters: None
            * Returns: Dict with exists, contents, subdirectories
            * Original tool: check_temp_directory()
            * Note: Lists all contents and subdirectories recursively

    Args:
        ctx: MCP context providing access to lifespan context
        action: The specific temp management operation (routing key)

    Returns:
        Dict with temp directory information (structure varies by action)

    Examples:
        # Ensure temp directory exists
        result = manage_temp(ctx, "create")

        # Check temp directory status
        result = manage_temp(ctx, "check")
    """
    pass  # Implementation will route to original functions


# =============================================================================
# ROUTER FUNCTION DEFINITIONS
# =============================================================================
# The following functions will be registered as MCP tools and will route
# to the appropriate original function based on the action/type/operation/mode
# =============================================================================

# Registry mapping for tool routing (for documentation purposes)
MEGA_TOOL_REGISTRY = {
    "manage_project": {
        "actions": {
            "set_path": "set_project_path",
            "refresh": "refresh_index",
            "reindex": "force_reindex",
            "clear": "clear_settings",
            "reset": "reset_server_state",
        },
        "original_tools": [
            "set_project_path",
            "refresh_index",
            "force_reindex",
            "clear_settings",
            "reset_server_state",
        ]
    },
    "search_content": {
        "actions": {
            "search": "search_code_advanced",
            "find": "find_files",
            "rank": "rank_search_results",
        },
        "original_tools": [
            "search_code_advanced",
            "find_files",
            "rank_search_results",
        ]
    },
    "modify_file": {
        "operations": {
            "write": "write_to_file",
            "diff": "apply_diff",
            "insert": "insert_content",
            "replace": "search_and_replace",
        },
        "original_tools": [
            "write_to_file",
            "apply_diff",
            "insert_content",
            "search_and_replace",
        ]
    },
    "manage_files": {
        "actions": {
            "delete": "delete_file",
            "rename": "rename_file",
            "revert": "revert_file_to_version",
            "history": "get_file_history",
        },
        "original_tools": [
            "delete_file",
            "rename_file",
            "revert_file_to_version",
            "get_file_history",
        ]
    },
    "get_diagnostics": {
        "types": {
            "memory": "get_memory_profile",
            "index": "get_index_statistics",
            "backend": "get_backend_health",
            "performance": "get_performance_metrics",
            "operations": "get_active_operations",
            "settings": "get_settings_info",
            "ignore": "get_ignore_patterns",
            "filtering": "get_filtering_config",
            "ranking": "get_ranking_configuration",
        },
        "original_tools": [
            "get_memory_profile",
            "get_index_statistics",
            "get_backend_health",
            "get_performance_metrics",
            "get_active_operations",
            "get_settings_info",
            "get_ignore_patterns",
            "get_filtering_config",
            "get_ranking_configuration",
        ]
    },
    "manage_memory": {
        "actions": {
            "cleanup": "trigger_memory_cleanup",
            "configure": "configure_memory_limits",
            "export": "export_memory_profile",
        },
        "original_tools": [
            "trigger_memory_cleanup",
            "configure_memory_limits",
            "export_memory_profile",
        ]
    },
    "manage_operations": {
        "actions": {
            "list": "get_active_operations",
            "cancel": "cancel_operation",
            "cleanup": "cleanup_completed_operations",
        },
        "original_tools": [
            "get_active_operations",
            "cancel_operation",
            "cancel_all_operations",
            "cleanup_completed_operations",
        ]
    },
    "read_file": {
        "modes": {
            "smart": "analyze_file_with_smart_reader",
            "chunks": "read_file_chunks",
            "detect_errors": "detect_file_errors",
            "metadata": "get_file_metadata",
        },
        "original_tools": [
            "analyze_file_with_smart_reader",
            "read_file_chunks",
            "detect_file_errors",
            "get_file_metadata",
        ]
    },
    "manage_temp": {
        "actions": {
            "create": "create_temp_directory",
            "check": "check_temp_directory",
        },
        "original_tools": [
            "create_temp_directory",
            "check_temp_directory",
        ]
    },
}


# =============================================================================
# IMPLEMENTATION NOTES
# =============================================================================
#
# The actual implementation will follow this pattern for each mega-tool:
#
# @mcp.tool()
# async def manage_project_impl(
#     ctx: Context,
#     action: ManageProjectAction,
#     path: Optional[str] = None,
#     clear_cache: bool = True,
# ) -> Union[str, Dict[str, Any]]:
#     """Implementation router for manage_project mega-tool."""
#     if action == "set_path":
#         if path is None:
#             return {"error": "path parameter is required for set_path action"}
#         return await set_project_path(path, ctx)
#     elif action == "refresh":
#         return await refresh_index(ctx)
#     elif action == "reindex":
#         return await force_reindex(ctx, clear_cache)
#     elif action == "clear":
#         return clear_settings(ctx)
#     elif action == "reset":
#         return reset_server_state(ctx)
#     else:
#         return {"error": f"Unknown action: {action}"}
#
# This pattern will be repeated for all 9 mega-tools, with each router
# function calling the appropriate original function based on the action.
#
# All original functions will remain available as private functions (_*)
# to support the mega-tools without exposing them as individual MCP tools.
# =============================================================================
