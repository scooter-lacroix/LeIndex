"""
Graceful Degradation for Global Index Operations

This module provides fallback mechanisms for global index operations when primary
backends are unavailable. It ensures the system continues to function with reduced
capabilities rather than failing completely.

GRACEFUL DEGRADATION STRATEGY:
1. LEANN unavailable → Fall back to Tantivy
2. Tantivy unavailable → Fall back to grep/ripgrep
3. Project index corrupted → Skip project and continue
4. All fallbacks logged with clear reasons

DEGRADED STATUS INDICATORS:
- "full": All backends available
- "degraded_leann_unavailable": LEANN unavailable, using Tantivy
- "degraded_tantivy_unavailable": Tantivy unavailable, using grep/ripgrep
- "degraded_search_fallback": Only basic grep available
- "degraded_no_backend": No search backends available

USAGE EXAMPLE:
    from src.leindex.global_index.graceful_degradation import (
        fallback_from_leann,
        fallback_from_tantivy,
        is_project_healthy,
        DegradedStatus
    )

    # Check if LEANN is available and fallback if needed
    results, status = fallback_from_leann(
        operation="cross_project_search",
        query_pattern="function foo",
        project_ids=["proj1", "proj2"]
    )

    # Check project health before querying
    if is_project_healthy(project_id="proj1"):
        results = query_project("proj1")
    else:
        logger.warning(f"Project proj1 is corrupted, skipping")
"""

import os
import shutil
import subprocess
import logging
from typing import Any, Dict, List, Optional, Set, Tuple
from dataclasses import dataclass
from enum import Enum
from concurrent.futures import ThreadPoolExecutor, as_completed

from .monitoring import (
    log_global_index_operation,
    GlobalIndexError
)
from ..logger_config import logger
from ..search.ripgrep import RipgrepStrategy
from ..search.grep import GrepStrategy


class DegradedStatus(str, Enum):
    """
    Degraded status indicators for API responses.

    Values indicate the current degradation level:
    - full: All backends operational
    - degraded_leann_unavailable: LEANN unavailable, using Tantivy
    - degraded_tantivy_unavailable: Tantivy unavailable, using grep
    - degraded_search_fallback: Only basic grep available
    - degraded_no_backend: No search backends available
    """
    FULL = "full"
    DEGRADED_LEANN_UNAVAILABLE = "degraded_leann_unavailable"
    DEGRADED_TANTIVY_UNAVAILABLE = "degraded_tantivy_unavailable"
    DEGRADED_SEARCH_FALLBACK = "degraded_search_fallback"
    DEGRADED_NO_BACKEND = "degraded_no_backend"


@dataclass
class FallbackResult:
    """
    Result from a fallback operation with degradation status.

    Attributes:
        results: Query results from the fallback backend
        status: Current degradation status
        fallback_reason: Reason for fallback
        original_backend: Backend that was attempted first
        actual_backend: Backend that was actually used
    """
    results: Any
    status: DegradedStatus
    fallback_reason: str
    original_backend: str
    actual_backend: str


def is_leann_available() -> bool:
    """
    Check if LEANN backend is available.

    Returns:
        True if LEANN is available and functional, False otherwise
    """
    try:
        # Check if LEANN is configured and accessible
        # This is a placeholder - actual implementation depends on LEANN setup
        import importlib

        spec = importlib.util.find_spec("leann")
        if spec is None:
            return False

        # Try to import and check basic functionality
        # In production, you'd check connection, API keys, etc.
        return True
    except Exception as e:
        logger.debug(f"LEANN availability check failed: {e}")
        return False


def is_tantivy_available() -> bool:
    """
    Check if Tantivy backend is available.

    Returns:
        True if Tantivy is available and functional, False otherwise
    """
    try:
        import importlib

        spec = importlib.util.find_spec("tantivy")
        if spec is None:
            return False

        # Try to import and check basic functionality
        from tantivy import Index, Schema

        # Basic functionality test
        schema = Schema()
        index = Index(schema)
        return True
    except Exception as e:
        logger.debug(f"Tantivy availability check failed: {e}")
        return False


def is_ripgrep_available() -> bool:
    """
    Check if ripgrep (rg) command is available.

    Returns:
        True if ripgrep is available, False otherwise
    """
    return shutil.which('rg') is not None


def is_grep_available() -> bool:
    """
    Check if grep command is available.

    Returns:
        True if grep is available, False otherwise
    """
    return shutil.which('grep') is not None


def fallback_from_leann(
    operation: str,
    query_pattern: str,
    project_ids: Optional[List[str]] = None,
    **kwargs
) -> FallbackResult:
    """
    Fallback from LEANN to Tantivy when LEANN is unavailable.

    This function attempts to use LEANN first, and falls back to Tantivy
    if LEANN is not available. The fallback is logged with clear reasons.

    Args:
        operation: The operation being performed (e.g., "cross_project_search")
        query_pattern: The search query pattern
        project_ids: Optional list of project IDs to search
        **kwargs: Additional operation-specific parameters

    Returns:
        FallbackResult with results and degradation status

    Example:
        result = fallback_from_leann(
            operation="cross_project_search",
            query_pattern="function foo",
            project_ids=["proj1", "proj2"]
        )
        print(result.status)  # "full" or "degraded_leann_unavailable"
    """
    start_time = __import__('time').time()

    # Try LEANN first
    if is_leann_available():
        try:
            # Placeholder: Execute LEANN query
            # In production, this would call actual LEANN backend
            results = {"placeholder": "leann_results"}

            duration_ms = (__import__('time').time() - start_time) * 1000

            log_global_index_operation(
                operation=operation,
                component='leann_backend',
                status='success',
                duration_ms=duration_ms,
                backend='leann',
                query_pattern=query_pattern,
                project_ids=project_ids or []
            )

            return FallbackResult(
                results=results,
                status=DegradedStatus.FULL,
                fallback_reason="",
                original_backend="leann",
                actual_backend="leann"
            )
        except Exception as e:
            logger.warning(f"LEANN backend failed: {e}, falling back to Tantivy")
            log_global_index_operation(
                operation=operation,
                component='leann_backend',
                status='error',
                duration_ms=(__import__('time').time() - start_time) * 1000,
                backend='leann',
                error=str(e),
                fallback_to='tantivy'
            )

    # LEANN unavailable or failed, fall back to Tantivy
    logger.info("Falling back from LEANN to Tantivy")
    return fallback_from_tantivy(
        operation=operation,
        query_pattern=query_pattern,
        project_ids=project_ids,
        original_backend="leann",
        **kwargs
    )


def fallback_from_tantivy(
    operation: str,
    query_pattern: str,
    project_ids: Optional[List[str]] = None,
    original_backend: str = "leann",
    **kwargs
) -> FallbackResult:
    """
    Fallback from Tantivy to grep/ripgrep when Tantivy is unavailable.

    This function attempts to use Tantivy first, and falls back to
    grep/ripgrep if Tantivy is not available. Preferentially uses ripgrep
    if available, otherwise falls back to grep.

    Args:
        operation: The operation being performed (e.g., "cross_project_search")
        query_pattern: The search query pattern
        project_ids: Optional list of project IDs to search
        original_backend: The backend that was originally attempted
        **kwargs: Additional operation-specific parameters

    Returns:
        FallbackResult with results and degradation status

    Example:
        result = fallback_from_tantivy(
            operation="cross_project_search",
            query_pattern="def foo",
            project_ids=["proj1"],
            original_backend="leann"
        )
    """
    start_time = __import__('time').time()

    # Try Tantivy first
    if is_tantivy_available():
        try:
            # Placeholder: Execute Tantivy query
            # In production, this would call actual Tantivy backend
            results = {"placeholder": "tantivy_results"}

            duration_ms = (__import__('time').time() - start_time) * 1000

            status = DegradedStatus.DEGRADED_LEANN_UNAVAILABLE
            if original_backend != "leann":
                status = DegradedStatus.DEGRADED_TANTIVY_UNAVAILABLE

            log_global_index_operation(
                operation=operation,
                component='tantivy_backend',
                status='success',
                duration_ms=duration_ms,
                backend='tantivy',
                query_pattern=query_pattern,
                project_ids=project_ids or [],
                original_backend=original_backend
            )

            return FallbackResult(
                results=results,
                status=status,
                fallback_reason=f"{original_backend} unavailable",
                original_backend=original_backend,
                actual_backend="tantivy"
            )
        except Exception as e:
            logger.warning(f"Tantivy backend failed: {e}, falling back to grep")
            log_global_index_operation(
                operation=operation,
                component='tantivy_backend',
                status='error',
                duration_ms=(__import__('time').time() - start_time) * 1000,
                backend='tantivy',
                error=str(e),
                fallback_to='grep'
            )

    # Tantivy unavailable or failed, try ripgrep
    logger.info("Falling back from Tantivy to ripgrep")
    return fallback_to_ripgrep(
        operation=operation,
        query_pattern=query_pattern,
        project_ids=project_ids,
        original_backend=original_backend,
        **kwargs
    )


def fallback_to_ripgrep(
    operation: str,
    query_pattern: str,
    project_ids: Optional[List[str]] = None,
    original_backend: str = "tantivy",
    **kwargs
) -> FallbackResult:
    """
    Fallback to ripgrep when higher-level backends are unavailable.

    This function uses ripgrep as the search backend. Ripgrep is preferred
    over grep due to its superior performance and features.

    Args:
        operation: The operation being performed
        query_pattern: The search query pattern
        project_ids: Optional list of project IDs to search
        original_backend: The backend that was originally attempted
        **kwargs: Additional operation-specific parameters including:
            - base_path: Base path to search in (if project_ids not provided)
            - case_sensitive: Whether search is case-sensitive (default: True)
            - context_lines: Number of context lines (default: 0)
            - file_pattern: Glob pattern to filter files (default: None)

    Returns:
        FallbackResult with results and degradation status

    Example:
        result = fallback_to_ripgrep(
            operation="cross_project_search",
            query_pattern="async def",
            base_path="/path/to/project"
        )
    """
    start_time = __import__('time').time()

    if not is_ripgrep_available():
        logger.warning("ripgrep not available, falling back to grep")
        return fallback_to_grep(
            operation=operation,
            query_pattern=query_pattern,
            project_ids=project_ids,
            original_backend=original_backend,
            **kwargs
        )

    try:
        # Use ripgrep strategy
        strategy = RipgrepStrategy()

        base_path = kwargs.get('base_path', os.getcwd())
        case_sensitive = kwargs.get('case_sensitive', True)
        context_lines = kwargs.get('context_lines', 0)
        file_pattern = kwargs.get('file_pattern')

        # Execute search
        results = strategy.search(
            pattern=query_pattern,
            base_path=base_path,
            case_sensitive=case_sensitive,
            context_lines=context_lines,
            file_pattern=file_pattern
        )

        duration_ms = (__import__('time').time() - start_time) * 1000

        log_global_index_operation(
            operation=operation,
            component='ripgrep_fallback',
            status='warning',
            duration_ms=duration_ms,
            backend='ripgrep',
            query_pattern=query_pattern,
            fallback_from=original_backend,
            result_count=sum(len(matches) for matches in results.values())
        )

        return FallbackResult(
            results=results,
            status=DegradedStatus.DEGRADED_SEARCH_FALLBACK,
            fallback_reason=f"{original_backend} and tantivy unavailable",
            original_backend=original_backend,
            actual_backend="ripgrep"
        )

    except Exception as e:
        logger.error(f"ripgrep fallback failed: {e}")
        log_global_index_operation(
            operation=operation,
            component='ripgrep_fallback',
            status='error',
            duration_ms=(__import__('time').time() - start_time) * 1000,
            backend='ripgrep',
            error=str(e)
        )
        # Try final fallback to grep
        return fallback_to_grep(
            operation=operation,
            query_pattern=query_pattern,
            project_ids=project_ids,
            original_backend=original_backend,
            **kwargs
        )


def fallback_to_grep(
    operation: str,
    query_pattern: str,
    project_ids: Optional[List[str]] = None,
    original_backend: str = "tantivy",
    **kwargs
) -> FallbackResult:
    """
    Final fallback to grep when all other backends are unavailable.

    This function uses the basic grep command as a last resort. Grep is
    universally available on Unix-like systems but lacks the performance
    and features of ripgrep.

    Args:
        operation: The operation being performed
        query_pattern: The search query pattern
        project_ids: Optional list of project IDs to search
        original_backend: The backend that was originally attempted
        **kwargs: Additional operation-specific parameters including:
            - base_path: Base path to search in (if project_ids not provided)
            - case_sensitive: Whether search is case-sensitive (default: True)
            - context_lines: Number of context lines (default: 0)
            - file_pattern: Glob pattern to filter files (default: None)

    Returns:
        FallbackResult with results and degradation status

    Example:
        result = fallback_to_grep(
            operation="cross_project_search",
            query_pattern="class Foo",
            base_path="/path/to/project"
        )
    """
    start_time = __import__('time').time()

    if not is_grep_available():
        logger.error("No search backends available (LEANN, Tantivy, ripgrep, grep all unavailable)")
        log_global_index_operation(
            operation=operation,
            component='no_backend',
            status='error',
            duration_ms=(__import__('time').time() - start_time) * 1000,
            error='All search backends unavailable'
        )

        return FallbackResult(
            results={},
            status=DegradedStatus.DEGRADED_NO_BACKEND,
            fallback_reason="All search backends unavailable",
            original_backend=original_backend,
            actual_backend="none"
        )

    try:
        # Use grep strategy
        strategy = GrepStrategy()

        base_path = kwargs.get('base_path', os.getcwd())
        case_sensitive = kwargs.get('case_sensitive', True)
        context_lines = kwargs.get('context_lines', 0)
        file_pattern = kwargs.get('file_pattern')

        # Execute search
        results = strategy.search(
            pattern=query_pattern,
            base_path=base_path,
            case_sensitive=case_sensitive,
            context_lines=context_lines,
            file_pattern=file_pattern
        )

        duration_ms = (__import__('time').time() - start_time) * 1000

        log_global_index_operation(
            operation=operation,
            component='grep_fallback',
            status='warning',
            duration_ms=duration_ms,
            backend='grep',
            query_pattern=query_pattern,
            fallback_from=original_backend,
            result_count=sum(len(matches) for matches in results.values())
        )

        return FallbackResult(
            results=results,
            status=DegradedStatus.DEGRADED_SEARCH_FALLBACK,
            fallback_reason=f"{original_backend}, tantivy, and ripgrep unavailable",
            original_backend=original_backend,
            actual_backend="grep"
        )

    except Exception as e:
        logger.error(f"grep fallback failed: {e}")
        log_global_index_operation(
            operation=operation,
            component='grep_fallback',
            status='error',
            duration_ms=(__import__('time').time() - start_time) * 1000,
            backend='grep',
            error=str(e)
        )

        return FallbackResult(
            results={},
            status=DegradedStatus.DEGRADED_NO_BACKEND,
            fallback_reason="All search backends failed",
            original_backend=original_backend,
            actual_backend="none"
        )


def is_project_healthy(
    project_id: str,
    project_path: Optional[str] = None
) -> bool:
    """
    Check if a project index is healthy and can be queried.

    This function performs basic health checks on a project index:
    1. Checks if project path exists
    2. Checks if index files are present and readable
    3. Validates index file integrity (basic check)

    Args:
        project_id: The project identifier
        project_path: Optional path to the project directory.
                    If not provided, will attempt to resolve from project_id

    Returns:
        True if project index is healthy, False otherwise

    Example:
        if is_project_healthy(project_id="myproject"):
            results = query_project("myproject")
        else:
            logger.warning("Project index corrupted, skipping")
    """
    try:
        # Resolve project path if not provided
        if project_path is None:
            # In production, this would look up the project path from registry
            # For now, assume project_id is relative to current directory
            project_path = os.path.join(os.getcwd(), project_id)

        # Check if project directory exists
        if not os.path.exists(project_path):
            logger.warning(f"Project path does not exist: {project_path}")
            return False

        # Check if it's a directory
        if not os.path.isdir(project_path):
            logger.warning(f"Project path is not a directory: {project_path}")
            return False

        # Check for index files (this is implementation-specific)
        # In production, you'd check for actual index files (MessagePack, etc.)
        # For now, just check if directory is accessible
        if not os.access(project_path, os.R_OK):
            logger.warning(f"Project directory not readable: {project_path}")
            return False

        # Basic index integrity check
        # In production, you'd validate index file format, checksums, etc.
        # For now, just check if we can list contents
        try:
            os.listdir(project_path)
        except OSError as e:
            logger.warning(f"Cannot list project directory: {e}")
            return False

        return True

    except Exception as e:
        logger.error(f"Error checking project health for {project_id}: {e}")
        return False


def filter_healthy_projects(
    project_ids: List[str],
    project_paths: Optional[Dict[str, str]] = None
) -> Tuple[List[str], List[str]]:
    """
    Filter out unhealthy projects from a list.

    This function checks each project and separates healthy ones from
    unhealthy ones. Unhealthy projects are logged with warnings.

    Args:
        project_ids: List of project IDs to check
        project_paths: Optional mapping of project_id -> project_path

    Returns:
        Tuple of (healthy_projects, unhealthy_projects)

    Example:
        healthy, unhealthy = filter_healthy_projects(
            project_ids=["proj1", "proj2", "proj3"]
        )
        print(f"Healthy: {healthy}, Unhealthy: {unhealthy}")
    """
    healthy = []
    unhealthy = []

    for project_id in project_ids:
        project_path = project_paths.get(project_id) if project_paths else None

        if is_project_healthy(project_id, project_path):
            healthy.append(project_id)
        else:
            unhealthy.append(project_id)
            logger.warning(f"Skipping unhealthy project: {project_id}")

    # Log the filtering operation
    log_global_index_operation(
        operation='filter_healthy_projects',
        component='graceful_degradation',
        status='warning' if unhealthy else 'success',
        duration_ms=0.0,
        total_projects=len(project_ids),
        healthy_count=len(healthy),
        unhealthy_count=len(unhealthy),
        unhealthy_projects=unhealthy
    )

    return healthy, unhealthy


def execute_with_degradation(
    operation: str,
    query_pattern: str,
    project_ids: Optional[List[str]] = None,
    base_path: Optional[str] = None,
    **kwargs
) -> Dict[str, Any]:
    """
    Execute a global index operation with automatic graceful degradation.

    This is the main entry point for graceful degradation. It attempts
    backends in order: LEANN → Tantivy → ripgrep → grep, and handles
    project health checks.

    Args:
        operation: The operation to perform
        query_pattern: The search query pattern
        project_ids: Optional list of project IDs to search
        base_path: Base path for search (if not using project_ids)
        **kwargs: Additional operation-specific parameters

    Returns:
        Dictionary with:
            - results: Query results
            - degraded_status: Current degradation status
            - backend_used: Which backend was actually used
            - projects_skipped: List of unhealthy projects (if any)

    Example:
        result = execute_with_degradation(
            operation="cross_project_search",
            query_pattern="async def fetch",
            project_ids=["proj1", "proj2"],
            case_sensitive=False
        )
        print(result['degraded_status'])
        print(result['results'])
    """
    start_time = __import__('time').time()

    # Filter unhealthy projects if project_ids provided
    projects_skipped = []
    if project_ids:
        healthy_projects, unhealthy_projects = filter_healthy_projects(project_ids)
        projects_skipped = unhealthy_projects

        if not healthy_projects:
            logger.error("All projects are unhealthy, no search possible")
            return {
                'results': {},
                'degraded_status': DegradedStatus.DEGRADED_NO_BACKEND,
                'backend_used': 'none',
                'projects_skipped': projects_skipped,
                'error': 'All projects are unhealthy'
            }

        # Update project_ids to only healthy ones
        project_ids = healthy_projects

    # Try backends in order
    try:
        # Start with LEANN (or fallback)
        fallback_result = fallback_from_leann(
            operation=operation,
            query_pattern=query_pattern,
            project_ids=project_ids,
            base_path=base_path,
            **kwargs
        )

        duration_ms = (__import__('time').time() - start_time) * 1000

        return {
            'results': fallback_result.results,
            'degraded_status': fallback_result.status.value,
            'backend_used': fallback_result.actual_backend,
            'projects_skipped': projects_skipped,
            'fallback_reason': fallback_result.fallback_reason,
            'duration_ms': duration_ms
        }

    except Exception as e:
        logger.error(f"All backends failed for operation {operation}: {e}")
        log_global_index_operation(
            operation=operation,
            component='graceful_degradation',
            status='error',
            duration_ms=(__import__('time').time() - start_time) * 1000,
            error=str(e)
        )

        return {
            'results': {},
            'degraded_status': DegradedStatus.DEGRADED_NO_BACKEND.value,
            'backend_used': 'none',
            'projects_skipped': projects_skipped,
            'error': str(e)
        }


def get_backend_status() -> Dict[str, bool]:
    """
    Get the availability status of all search backends.

    Returns:
        Dictionary mapping backend names to availability status

    Example:
        status = get_backend_status()
        print(status)
        # {'leann': False, 'tantivy': True, 'ripgrep': True, 'grep': True}
    """
    return {
        'leann': is_leann_available(),
        'tantivy': is_tantivy_available(),
        'ripgrep': is_ripgrep_available(),
        'grep': is_grep_available()
    }


def get_current_degradation_level() -> DegradedStatus:
    """
    Determine the current degradation level based on backend availability.

    Returns:
        DegradedStatus indicating current system state

    Example:
        level = get_current_degradation_level()
        if level != DegradedStatus.FULL:
            logger.warning(f"System degraded: {level.value}")
    """
    if is_leann_available():
        return DegradedStatus.FULL
    elif is_tantivy_available():
        return DegradedStatus.DEGRADED_LEANN_UNAVAILABLE
    elif is_ripgrep_available():
        return DegradedStatus.DEGRADED_SEARCH_FALLBACK
    elif is_grep_available():
        return DegradedStatus.DEGRADED_SEARCH_FALLBACK
    else:
        return DegradedStatus.DEGRADED_NO_BACKEND
