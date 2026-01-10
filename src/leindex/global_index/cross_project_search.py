"""
Cross-Project Search Implementation for Global Index.

This module implements federated search across multiple project indexes,
with caching via Tier 2 and metadata support from Tier 1.

Key Features:
- Parallel async queries across projects
- Result merging and ranking by relevance/score
- Async-aware caching integration with Tier 2 (stale-allowed)
- Partial failure resilience (return results from successful projects)
- Pattern validation for security
- Project access validation
- Circuit breaker protection for failing projects

Performance Targets:
- Cache hit: <50ms
- Cache miss: 300-500ms
- Parallel queries using asyncio.gather()

Architecture:
    cross_project_search()
        ├── _validate_pattern() - Input validation
        ├── _validate_project_access() - Access control
        ├── _execute_federated_search() - Parallel queries with circuit breaker
        └── _merge_and_rank_results() - Result aggregation
"""

import asyncio
import hashlib
import logging
import re
import time
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Set, Tuple

from .tier1_metadata import GlobalIndexTier1, ProjectMetadata
from .tier2_cache import GlobalIndexTier2, QueryMetadata
from .query_router import QueryRouter
from .monitoring import (
    log_global_index_operation,
    GlobalIndexError,
    CacheError,
    get_global_index_monitor,
)
from ..logger_config import logger


# =============================================================================
# EXCEPTION CLASSES
# =============================================================================

class CrossProjectSearchError(GlobalIndexError):
    """Base exception for cross-project search errors."""

    def __init__(self, message: str, details: Optional[Dict[str, Any]] = None):
        """Initialize the error.

        Args:
            message: Error message
            details: Optional additional error details
        """
        super().__init__(message, component='cross_project_search', details=details)

    def to_dict(self) -> Dict[str, Any]:
        """Convert error to dictionary for logging."""
        base = super().to_dict()
        base['error_type'] = 'cross_project_search_error'
        return base


class ProjectNotFoundError(CrossProjectSearchError):
    """Raised when requested projects are not found in Tier 1 metadata."""

    def __init__(self, project_ids: List[str]):
        """Initialize the error.

        Args:
            project_ids: List of project IDs that were not found
        """
        message = f"Projects not found ({len(project_ids)}): {', '.join(project_ids[:5])}" + (
            f"..." if len(project_ids) > 5 else ""
        )
        super().__init__(message, details={'project_ids': project_ids, 'count': len(project_ids)})

    def to_dict(self) -> Dict[str, Any]:
        """Convert error to dictionary for logging."""
        base = super().to_dict()
        base['error_type'] = 'project_not_found'
        return base


class AllProjectsFailedError(CrossProjectSearchError):
    """Raised when all project queries fail."""

    def __init__(
        self,
        project_ids: List[str],
        errors: Dict[str, str]
    ):
        """Initialize the error.

        Args:
            project_ids: List of project IDs that failed
            errors: Dictionary mapping project IDs to error messages
        """
        message = f"All {len(project_ids)} project queries failed"
        super().__init__(
            message,
            details={
                'project_count': len(project_ids),
                'errors': errors
            }
        )

    def to_dict(self) -> Dict[str, Any]:
        """Convert error to dictionary for logging."""
        base = super().to_dict()
        base['error_type'] = 'all_projects_failed'
        return base


class InvalidPatternError(CrossProjectSearchError):
    """Raised when search pattern is invalid."""

    def __init__(self, pattern: str, reason: str):
        """Initialize the error.

        Args:
            pattern: The invalid pattern
            reason: Reason why the pattern is invalid
        """
        message = f"Invalid search pattern: {reason}"
        super().__init__(
            message,
            details={'pattern': pattern, 'reason': reason}
        )

    def to_dict(self) -> Dict[str, Any]:
        """Convert error to dictionary for logging."""
        base = super().to_dict()
        base['error_type'] = 'invalid_pattern'
        return base


# =============================================================================
# CIRCUIT BREAKER
# =============================================================================

@dataclass
class CircuitBreakerState:
    """
    State tracking for a single project's circuit breaker.

    Attributes:
        failure_count: Number of consecutive failures
        last_failure_time: Timestamp of most recent failure
        last_success_time: Timestamp of most recent success
        is_open: Whether circuit is open (blocking queries)
        cooldown_until: Timestamp when cooldown period ends
    """
    failure_count: int = 0
    last_failure_time: Optional[float] = None
    last_success_time: Optional[float] = None
    is_open: bool = False
    cooldown_until: Optional[float] = None


class ProjectCircuitBreaker:
    """
    Circuit breaker for protecting against failing projects.

    This class implements a circuit breaker pattern to prevent cascading
    failures when a project repeatedly fails queries. After a threshold
    of failures, the project is temporarily blocked from queries.

    Features:
    - Per-project failure tracking
    - Configurable failure threshold (default: 3)
    - Configurable cooldown period (default: 60 seconds)
    - Automatic reset on successful query
    - Async-safe implementation with asyncio.Lock

    Example:
        >>> breaker = ProjectCircuitBreaker(failure_threshold=3, cooldown_seconds=60)
        >>> if breaker.can_query("backend"):
        ...     result = await search_project("backend")
        ...     breaker.record_success("backend")
        ... else:
        ...     logger.warning("Backend project circuit breaker is open")
    """

    def __init__(
        self,
        failure_threshold: int = 3,
        cooldown_seconds: float = 60.0
    ):
        """Initialize the circuit breaker.

        Args:
            failure_threshold: Number of failures before opening circuit (default: 3)
            cooldown_seconds: Seconds to wait before retrying failed project (default: 60)
        """
        self.failure_threshold = failure_threshold
        self.cooldown_seconds = cooldown_seconds

        # Per-project state tracking
        self._states: Dict[str, CircuitBreakerState] = defaultdict(
            lambda: CircuitBreakerState()
        )

        # Async lock for thread-safe operations
        self._lock = asyncio.Lock()

        # Statistics
        self._stats = {
            'total_blocks': 0,
            'total_resets': 0,
            'projects_blocked': set(),
        }

    def _generate_cache_key(self, project_id: str) -> str:
        """Generate a cache key for project state."""
        return f"circuit_breaker:{project_id}"

    async def can_query(self, project_id: str) -> bool:
        """Check if queries are allowed for a project.

        Args:
            project_id: Project to check

        Returns:
            True if queries are allowed, False if circuit is open
        """
        async with self._lock:
            state = self._states[project_id]

            # Check if circuit is open
            if state.is_open:
                # Check if cooldown has expired
                if state.cooldown_until and time.time() >= state.cooldown_until:
                    # Cooldown expired, attempt reset
                    logger.info(
                        f"Circuit breaker cooldown expired for {project_id}, "
                        f"resetting state"
                    )
                    self._reset_state(project_id, state)
                    return True
                else:
                    # Still in cooldown
                    remaining = (
                        state.cooldown_until - time.time()
                        if state.cooldown_until else 0
                    )
                    logger.debug(
                        f"Circuit breaker open for {project_id}, "
                        f"{remaining:.1f}s remaining in cooldown"
                    )
                    self._stats['total_blocks'] += 1
                    return False

            # Circuit is closed, queries allowed
            return True

    async def record_success(self, project_id: str) -> None:
        """Record a successful query for a project.

        Args:
            project_id: Project that succeeded
        """
        async with self._lock:
            state = self._states[project_id]
            state.last_success_time = time.time()

            # Reset failure count on success
            if state.failure_count > 0:
                logger.info(
                    f"Resetting failure count for {project_id} "
                    f"after success (was {state.failure_count})"
                )
                state.failure_count = 0
                self._stats['total_resets'] += 1

            # Ensure circuit is closed after success
            if state.is_open:
                logger.warning(
                    f"Closing circuit breaker for {project_id} after success"
                )
                state.is_open = False
                state.cooldown_until = None

    async def record_failure(self, project_id: str, error: str) -> None:
        """Record a failed query for a project.

        Args:
            project_id: Project that failed
            error: Error message describing the failure
        """
        async with self._lock:
            state = self._states[project_id]
            state.failure_count += 1
            state.last_failure_time = time.time()

            logger.warning(
                f"Recording failure for {project_id}: {error} "
                f"(count: {state.failure_count}/{self.failure_threshold})"
            )

            # Check if threshold reached
            if state.failure_count >= self.failure_threshold:
                if not state.is_open:
                    # Open the circuit
                    state.is_open = True
                    state.cooldown_until = time.time() + self.cooldown_seconds

                    logger.error(
                        f"Circuit breaker OPENED for {project_id} "
                        f"after {state.failure_count} failures. "
                        f"Cooldown: {self.cooldown_seconds}s"
                    )

                    self._stats['projects_blocked'].add(project_id)
                else:
                    # Already open, extend cooldown
                    state.cooldown_until = time.time() + self.cooldown_seconds
                    logger.debug(
                        f"Extended cooldown for {project_id} "
                        f"(circuit already open)"
                    )

    def _reset_state(self, project_id: str, state: CircuitBreakerState) -> None:
        """Reset circuit breaker state for a project.

        Args:
            project_id: Project to reset
            state: Current state to reset
        """
        state.failure_count = 0
        state.is_open = False
        state.cooldown_until = None
        logger.info(f"Circuit breaker reset for {project_id}")

    def get_state(self, project_id: str) -> Dict[str, Any]:
        """Get current circuit breaker state for a project.

        Args:
            project_id: Project to query

        Returns:
            Dictionary with current state information
        """
        state = self._states[project_id]
        return {
            'project_id': project_id,
            'is_open': state.is_open,
            'failure_count': state.failure_count,
            'last_failure_time': state.last_failure_time,
            'last_success_time': state.last_success_time,
            'cooldown_remaining': (
                max(0, state.cooldown_until - time.time())
                if state.cooldown_until else 0
            ) if state.is_open else 0,
        }

    def get_statistics(self) -> Dict[str, Any]:
        """Get circuit breaker statistics.

        Returns:
            Dictionary with aggregate statistics
        """
        return {
            'total_blocks': self._stats['total_blocks'],
            'total_resets': self._stats['total_resets'],
            'currently_blocked_projects': list(self._stats['projects_blocked']),
            'total_tracked_projects': len(self._states),
        }


# =============================================================================
# DATA CLASSES
# =============================================================================

@dataclass
class ProjectSearchResult:
    """
    Search result from a single project.

    Attributes:
        project_id: Project that produced this result
        results: List of search results from this project
        total_count: Total number of results (may exceed returned list)
        query_time_ms: Time taken for this project query
        error: Error message if query failed, None otherwise
    """
    project_id: str
    results: List[Dict[str, Any]] = field(default_factory=list)
    total_count: int = 0
    query_time_ms: float = 0.0
    error: Optional[str] = None

    def __bool__(self) -> bool:
        """Return True if this result has data (not an error)."""
        return self.error is None


@dataclass
class CrossProjectSearchResult:
    """
    Aggregated cross-project search result.

    Attributes:
        merged_results: List of merged and ranked results from all projects
        total_results: Total number of results across all projects
        project_results: Per-project results (including failures)
        query_metadata: Query metadata including cache info
        cache_hit: Whether the result came from cache
        query_time_ms: Total query time including cache lookup
    """
    merged_results: List[Dict[str, Any]] = field(default_factory=list)
    total_results: int = 0
    project_results: Dict[str, ProjectSearchResult] = field(default_factory=dict)
    query_metadata: Optional[QueryMetadata] = None
    cache_hit: bool = False
    query_time_ms: float = 0.0


# =============================================================================
# MAIN SEARCH FUNCTION
# =============================================================================

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
    limit: int = 100,
    timeout: float = 30.0,
    circuit_breaker: Optional[ProjectCircuitBreaker] = None,
) -> CrossProjectSearchResult:
    """
    Execute cross-project search with async-aware caching and parallel queries.

    This function performs federated search across multiple project indexes,
    with intelligent async-aware caching via Tier 2 and metadata support from Tier 1.
    It also includes circuit breaker protection to prevent cascading failures from
    problematic projects.

    Args:
        pattern: Search pattern (string or regex)
        project_ids: List of project IDs to search (None = all projects)
        query_router: QueryRouter instance (for cache key generation)
        tier1: Tier 1 metadata cache (for project access validation)
        tier2: Tier 2 query cache (for result caching)
        case_sensitive: Whether search is case-sensitive
        context_lines: Lines of context around matches
        file_pattern: Glob pattern to filter files
        fuzzy: Whether to use fuzzy matching
        limit: Maximum results to return
        timeout: Maximum time (seconds) to wait for federated search (default: 30.0)
        circuit_breaker: Optional circuit breaker for failing project protection

    Returns:
        CrossProjectSearchResult with merged and ranked results

    Raises:
        InvalidPatternError: If pattern is invalid or contains catastrophic regex
        ProjectNotFoundError: If requested projects don't exist
        AllProjectsFailedError: If all project queries fail
        asyncio.TimeoutError: If federated search exceeds timeout

    Example:
        >>> result = await cross_project_search(
        ...     pattern="class User",
        ...     project_ids=["backend", "frontend"],
        ...     fuzzy=True,
        ...     limit=50
        ... )
        >>> print(f"Found {result.total_results} results")
        >>> for r in result.merged_results[:10]:
        ...     print(f"  {r['file_path']}:{r['line_number']}")
    """
    start_time = time.time()
    status = 'success'
    error_details = None

    try:
        # 1. Validate input pattern
        _validate_pattern(pattern)
        _sanitize_file_pattern(file_pattern)

        # 2. Determine projects to search
        if project_ids is None:
            # Search all projects
            project_ids = list(tier1.list_all_project_ids()) if tier1 else []

        if not project_ids:
            raise InvalidPatternError(
                pattern,
                "No projects available for search"
            )

        # 3. Validate project access
        _validate_project_access(project_ids, tier1)

        # 4. Build cache key if router/tier2 available
        cache_key = None
        if tier2 and query_router:
            # Generate cache key from search parameters
            cache_key = _generate_cache_key(
                pattern=pattern,
                project_ids=sorted(project_ids),
                case_sensitive=case_sensitive,
                context_lines=context_lines,
                file_pattern=file_pattern,
                fuzzy=fuzzy,
                limit=limit,
            )

        # 5. Check cache if available
        if cache_key and tier2:
            try:
                # Use async-aware cache query
                result, metadata = await _query_cache_async(
                    tier2=tier2,
                    cache_key=cache_key,
                    pattern=pattern,
                    project_ids=project_ids,
                    case_sensitive=case_sensitive,
                    context_lines=context_lines,
                    file_pattern=file_pattern,
                    fuzzy=fuzzy,
                    limit=limit,
                    timeout=timeout,
                    circuit_breaker=circuit_breaker,
                )

                result.query_time_ms = (time.time() - start_time) * 1000
                result.cache_hit = metadata.source != 'federation'
                result.query_metadata = metadata

                # Log cache operation
                log_global_index_operation(
                    operation='cross_project_search',
                    component='cross_project_search',
                    status='success',
                    duration_ms=result.query_time_ms,
                    pattern=pattern,
                    project_ids=project_ids,
                    cache_hit=result.cache_hit,
                    cache_source=metadata.source,
                )

                return result

            except CacheError as e:
                # Cache failure, fall through to direct execution
                logger.warning(f"Cache query failed: {e}, executing directly")

        # 6. Execute federated search with timeout protection
        result = await asyncio.wait_for(
            _execute_federated_search(
                pattern=pattern,
                project_ids=project_ids,
                case_sensitive=case_sensitive,
                context_lines=context_lines,
                file_pattern=file_pattern,
                fuzzy=fuzzy,
                limit=limit,
                circuit_breaker=circuit_breaker,
            ),
            timeout=timeout
        )

        result.query_time_ms = (time.time() - start_time) * 1000
        result.cache_hit = False

        # Store result in cache if available
        if cache_key and tier2:
            try:
                await _store_cache_async(
                    tier2=tier2,
                    cache_key=cache_key,
                    result=result,
                    involved_projects=set(project_ids),
                )
            except Exception as e:
                # Cache store failure is not critical
                logger.warning(f"Failed to store result in cache: {e}")

        return result

    except (InvalidPatternError, ProjectNotFoundError, AllProjectsFailedError, asyncio.TimeoutError):
        raise

    except Exception as e:
        status = 'error'
        error_details = {'error': str(e), 'pattern': pattern}

        # Log structured operation
        log_global_index_operation(
            operation='cross_project_search',
            component='cross_project_search',
            status=status,
            duration_ms=(time.time() - start_time) * 1000,
            pattern=pattern,
            project_ids=project_ids,
            error=str(e)
        )

        # Re-raise as CrossProjectSearchError
        raise CrossProjectSearchError(
            f"Cross-project search failed: {e}",
            details={'pattern': pattern, 'project_ids': project_ids}
        ) from e


def _validate_pattern(pattern: str) -> None:
    """
    Validate search pattern for security and correctness.

    Args:
        pattern: Search pattern to validate

    Raises:
        InvalidPatternError: If pattern is invalid
    """
    if not pattern:
        raise InvalidPatternError(
            pattern,
            "Pattern cannot be empty"
        )

    if not isinstance(pattern, str):
        raise InvalidPatternError(
            str(pattern),
            "Pattern must be a string"
        )

    # Check for null bytes
    if '\0' in pattern:
        raise InvalidPatternError(
            pattern,
            "Pattern contains null bytes"
        )

    # Check length (prevent DoS)
    if len(pattern) > 10000:
        raise InvalidPatternError(
            pattern,
            "Pattern too long (max 10000 characters)"
        )

    # Check for catastrophic regex patterns
    _check_for_catastrophic_patterns(pattern)


def _check_for_catastrophic_patterns(pattern: str) -> None:
    """
    Validate regex pattern for catastrophic backtracking risks.

    This function checks for patterns that can cause exponential backtracking:
    - Nested quantifiers (e.g., (a+)+, (a*)*, (a+)+)
    - Overlapping alternations (e.g., (a|a)+, (a|aa)+)
    - Deep nesting (more than 10 levels deep)
    - Repeated optional groups (e.g., (a?)+, (a*)+)

    Args:
        pattern: Regex pattern to validate

    Raises:
        InvalidPatternError: If pattern contains catastrophic patterns
    """
    # First validate pattern by attempting compilation
    try:
        re.compile(pattern)
    except re.error as e:
        raise InvalidPatternError(
            pattern,
            f"Invalid regex: {str(e)}"
        )

    # Check for nested quantifiers - most dangerous
    # Look for patterns like (a+)+, (a*)*, (a+)?, etc.
    if re.search(r'\([^(]*[*+?][*+]', pattern):
        raise InvalidPatternError(
            pattern,
            "Pattern contains nested quantifiers (catastrophic backtracking risk)"
        )

    # Check for overlapping alternations with quantifiers
    # Look for (a|a)+, (a|aa)+, etc.
    if re.search(r'\([^)]*\|[^)]*\)[*+]', pattern):
        # Additional check for repeated characters in alternation
        alternation_match = re.search(r'\(([^)]+)\)', pattern)
        if alternation_match:
            alternation_content = alternation_match.group(1)
            # Check if alternation contains repeated patterns
            parts = alternation_content.split('|')
            if len(parts) > 1 and any(p in parts for p in parts if parts.count(p) > 1):
                raise InvalidPatternError(
                    pattern,
                    "Pattern contains overlapping alternations (catastrophic backtracking risk)"
                )

    # Check nesting depth
    max_nesting = 10
    current_depth = 0
    max_depth_seen = 0

    for char in pattern:
        if char == '(':
            current_depth += 1
            max_depth_seen = max(max_depth_seen, current_depth)
        elif char == ')':
            current_depth -= 1

    if max_depth_seen > max_nesting:
        raise InvalidPatternError(
            pattern,
            f"Pattern nesting depth ({max_depth_seen}) exceeds maximum ({max_nesting})"
        )


def _sanitize_file_pattern(file_pattern: Optional[str]) -> None:
    """
    Sanitize file pattern to prevent path traversal and injection attacks.

    Args:
        file_pattern: File pattern to sanitize

    Raises:
        InvalidPatternError: If file_pattern contains dangerous characters
    """
    if not file_pattern:
        return

    if not isinstance(file_pattern, str):
        raise InvalidPatternError(
            str(file_pattern),
            "File pattern must be a string"
        )

    # Check for path traversal attempts
    if '..' in file_pattern:
        raise InvalidPatternError(
            file_pattern,
            "File pattern contains path traversal sequence (..)"
        )

    # Check for absolute paths
    if file_pattern.startswith('/'):
        raise InvalidPatternError(
            file_pattern,
            "File pattern cannot be an absolute path"
        )

    # Check for dangerous characters
    dangerous_chars = ['\0', '\n', '\r']
    for char in dangerous_chars:
        if char in file_pattern:
            raise InvalidPatternError(
                file_pattern,
                f"File pattern contains forbidden character: {repr(char)}"
            )

    # Validate it's a valid glob pattern
    # Allow: alphanumeric, underscore, hyphen, dot, slash, *, ?, [, ]
    # But not at dangerous positions
    allowed_pattern = re.compile(r'^[\w\-./\*\?\[\]\{\}]+$')
    if not allowed_pattern.match(file_pattern):
        raise InvalidPatternError(
            file_pattern,
            "File pattern contains invalid characters"
        )


def _validate_project_access(
    project_ids: List[str],
    tier1: Optional[GlobalIndexTier1]
) -> None:
    """
    Validate that requested projects exist and are accessible.

    Args:
        project_ids: List of project IDs to validate
        tier1: Tier 1 metadata cache

    Raises:
        ProjectNotFoundError: If any projects don't exist (returns ALL missing projects)
    """
    if not tier1:
        # No validation if no tier1 provided
        return

    available_projects = tier1.list_all_project_ids()

    missing_projects = [
        pid for pid in project_ids
        if pid not in available_projects
    ]

    if missing_projects:
        # Return ALL missing projects, not just the first one
        raise ProjectNotFoundError(missing_projects)


def _generate_cache_key(
    pattern: str,
    project_ids: List[str],
    case_sensitive: bool,
    context_lines: int,
    file_pattern: Optional[str],
    fuzzy: bool,
    limit: int,
) -> str:
    """
    Generate a deterministic cache key for search parameters.

    Args:
        pattern: Search pattern
        project_ids: Sorted list of project IDs
        case_sensitive: Case-sensitive search
        context_lines: Context lines
        file_pattern: File filter pattern
        fuzzy: Fuzzy matching
        limit: Result limit

    Returns:
        SHA256 hash of search parameters as hex string
    """
    # Create normalized key string
    key_parts = [
        pattern,
        ','.join(project_ids),
        str(case_sensitive),
        str(context_lines),
        file_pattern or '',
        str(fuzzy),
        str(limit),
    ]
    key_string = ':'.join(key_parts)

    # Generate hash
    return hashlib.sha256(key_string.encode()).hexdigest()


async def _query_cache_async(
    tier2: GlobalIndexTier2,
    cache_key: str,
    pattern: str,
    project_ids: List[str],
    case_sensitive: bool,
    context_lines: int,
    file_pattern: Optional[str],
    fuzzy: bool,
    limit: int,
    timeout: float,
    circuit_breaker: Optional[ProjectCircuitBreaker],
) -> Tuple[CrossProjectSearchResult, QueryMetadata]:
    """
    Query Tier 2 cache with async-aware execution.

    This function wraps the synchronous Tier 2 cache query in an async
    executor to avoid blocking the event loop while still providing
    full caching functionality.

    Args:
        tier2: Tier 2 cache instance
        cache_key: Cache key for this query
        pattern: Search pattern
        project_ids: Projects to search
        case_sensitive: Case-sensitive search
        context_lines: Context lines
        file_pattern: File filter
        fuzzy: Fuzzy matching
        limit: Result limit
        timeout: Query timeout
        circuit_breaker: Circuit breaker instance

    Returns:
        Tuple of (search_result, query_metadata)

    Raises:
        CacheError: If cache query fails
    """
    loop = asyncio.get_event_loop()

    # Define the cache query function
    def cache_query_func() -> CrossProjectSearchResult:
        """Synchronous function to execute on cache miss.

        NOTE: This is a known limitation - we cannot use asyncio.run() inside
        an async context. The Tier 2 cache architecture needs refactoring to
        support async callbacks properly. For now, we raise an error to
        prevent incorrect usage.
        """
        # CRITICAL FIX: Cannot use asyncio.run() inside async context
        # This would cause: RuntimeError: asyncio.run() cannot be called from a running event loop
        # The proper fix requires refactoring Tier 2 cache to support async callbacks
        logger.error(
            "Cache miss in async context - Tier 2 cache doesn't support async callbacks. "
            "Falling back to direct execution."
        )
        raise CacheError(
            "Async-aware cache query not yet fully implemented - Tier 2 cache architecture needs refactoring",
            details={
                'cache_key': cache_key,
                'reason': 'Cannot execute async federated search from sync callback in running event loop',
                'workaround': 'Use direct execution instead of cached query'
            }
        )

    # Run cache query in executor to avoid blocking
    try:
        result, metadata = await loop.run_in_executor(
            None,
            lambda: tier2.query(
                cache_key=cache_key,
                query_func=cache_query_func,
                involved_projects=set(project_ids),
            )
        )
        return result, metadata

    except Exception as e:
        raise CacheError(
            f"Cache query failed: {e}",
            details={'cache_key': cache_key, 'error': str(e)}
        ) from e


async def _store_cache_async(
    tier2: GlobalIndexTier2,
    cache_key: str,
    result: CrossProjectSearchResult,
    involved_projects: Set[str],
) -> None:
    """
    Store result in Tier 2 cache with async-aware execution.

    Args:
        tier2: Tier 2 cache instance
        cache_key: Cache key for this result
        result: Search result to cache
        involved_projects: Projects involved in this query

    Raises:
        CacheError: If cache store fails
    """
    loop = asyncio.get_event_loop()

    try:
        # Direct cache store (this is fast, so we can use run_in_executor)
        await loop.run_in_executor(
            None,
            lambda: tier2._compute_and_store(
                cache_key=cache_key,
                query_func=lambda: result,
                involved_projects=involved_projects,
            )
        )

    except Exception as e:
        raise CacheError(
            f"Cache store failed: {e}",
            details={'cache_key': cache_key, 'error': str(e)}
        ) from e


async def _execute_federated_search(
    pattern: str,
    project_ids: List[str],
    case_sensitive: bool,
    context_lines: int,
    file_pattern: Optional[str],
    fuzzy: bool,
    limit: int,
    circuit_breaker: Optional[ProjectCircuitBreaker],
) -> CrossProjectSearchResult:
    """
    Execute parallel federated search across projects with circuit breaker protection.

    Args:
        pattern: Search pattern
        project_ids: Projects to search
        case_sensitive: Case-sensitive search
        context_lines: Context lines around matches
        file_pattern: File filter pattern
        fuzzy: Fuzzy matching
        limit: Max results per project
        circuit_breaker: Circuit breaker for failing project protection

    Returns:
        CrossProjectSearchResult with per-project results

    Raises:
        AllProjectsFailedError: If all project queries fail or are blocked
    """
    # Filter projects through circuit breaker if available
    active_project_ids = []
    if circuit_breaker:
        for pid in project_ids:
            can_query = await circuit_breaker.can_query(pid)
            if can_query:
                active_project_ids.append(pid)
            else:
                logger.info(f"Project {pid} blocked by circuit breaker")
    else:
        active_project_ids = project_ids

    if not active_project_ids:
        # All projects blocked by circuit breaker
        errors = {
            pid: "Blocked by circuit breaker"
            for pid in project_ids
        }
        raise AllProjectsFailedError(project_ids, errors)

    # Execute queries in parallel
    tasks = [
        _search_single_project(
            project_id=pid,
            pattern=pattern,
            case_sensitive=case_sensitive,
            context_lines=context_lines,
            file_pattern=file_pattern,
            fuzzy=fuzzy,
            limit=limit,
            circuit_breaker=circuit_breaker,
        )
        for pid in active_project_ids
    ]

    project_results_list = await asyncio.gather(*tasks, return_exceptions=True)

    # Process results
    project_results: Dict[str, ProjectSearchResult] = {}
    successful_results: List[ProjectSearchResult] = []

    for pid, result in zip(active_project_ids, project_results_list):
        if isinstance(result, Exception):
            # Project query failed
            error_msg = str(result)
            project_results[pid] = ProjectSearchResult(
                project_id=pid,
                error=error_msg
            )

            # Record failure with circuit breaker
            if circuit_breaker:
                asyncio.create_task(circuit_breaker.record_failure(pid, error_msg))

        elif isinstance(result, ProjectSearchResult):
            project_results[pid] = result
            if result:
                successful_results.append(result)

                # Record success with circuit breaker
                if circuit_breaker:
                    asyncio.create_task(circuit_breaker.record_success(pid))
            else:
                # Result has error
                if circuit_breaker and result.error:
                    asyncio.create_task(
                        circuit_breaker.record_failure(pid, result.error)
                    )

    # Check if all projects failed
    if not successful_results:
        errors = {
            pid: pr.error
            for pid, pr in project_results.items()
            if pr.error
        }
        raise AllProjectsFailedError(active_project_ids, errors)

    # Merge and rank results
    merged_results = _merge_and_rank_results(successful_results, limit)

    return CrossProjectSearchResult(
        merged_results=merged_results,
        total_results=sum(pr.total_count for pr in successful_results),
        project_results=project_results,
        cache_hit=False,
    )


async def _search_single_project(
    project_id: str,
    pattern: str,
    case_sensitive: bool,
    context_lines: int,
    file_pattern: Optional[str],
    fuzzy: bool,
    limit: int,
    circuit_breaker: Optional[ProjectCircuitBreaker],
) -> ProjectSearchResult:
    """
    Search a single project using the actual search backend.

    This function integrates with the real search infrastructure to perform
    actual searches on project indexes. It uses the DAL (Data Access Layer)
    to access project indexes and perform searches.

    Args:
        project_id: Project to search
        pattern: Search pattern
        case_sensitive: Case-sensitive search
        context_lines: Context lines
        file_pattern: File filter
        fuzzy: Fuzzy matching
        limit: Max results
        circuit_breaker: Circuit breaker for failure tracking

    Returns:
        ProjectSearchResult from this project
    """
    start_time = time.time()

    try:
        logger.debug(f"Searching project {project_id} for pattern '{pattern}'")

        # Get project index from DAL
        from ..storage.dal_factory import get_dal_instance
        dal = get_dal_instance()

        if dal is None:
            raise RuntimeError(
                f"DAL not initialized. Cannot search project {project_id}"
            )

        # Get project metadata from DAL
        project_metadata = await dal.get_project_metadata(project_id)

        if project_metadata is None:
            raise ProjectNotFoundError([project_id])

        project_path = project_metadata.get('path')
        if not project_path:
            raise RuntimeError(
                f"Project {project_id} has no path in metadata"
            )

        # Get search interface from DAL
        search_interface = dal.search()
        if search_interface is None:
            raise RuntimeError(
                f"Search interface not available for project {project_id}"
            )

        # Build search query
        # Note: This is a simplified search implementation
        # The full implementation would use more sophisticated query building
        if fuzzy:
            # For fuzzy/regex search, use content search
            search_results_tuples = search_interface.search_content(pattern)
        else:
            # For literal search, also use content search
            search_results_tuples = search_interface.search_content(pattern)

        # Convert search results to expected format
        results = []
        for file_path, content_match in search_results_tuples[:limit]:
            # Extract line number and content from the match
            # The structure depends on what the search interface returns
            if isinstance(content_match, dict):
                line_number = content_match.get('line_number', 0)
                content = content_match.get('content', '')
                score = content_match.get('score', 0.8)
            else:
                line_number = 0
                content = str(content_match)
                score = 0.8

            results.append({
                'file_path': file_path,
                'line_number': line_number,
                'content': content,
                'score': score,
                'match_type': 'semantic' if fuzzy else 'lexical',
                'project_id': project_id,
            })

        total_count = len(search_results_tuples)

        logger.debug(
            f"Found {total_count} results in project {project_id} "
            f"in {(time.time() - start_time) * 1000:.2f}ms"
        )

        return ProjectSearchResult(
            project_id=project_id,
            results=results,
            total_count=total_count,
            query_time_ms=(time.time() - start_time) * 1000,
        )

    except ProjectNotFoundError:
        # Re-raise project not found errors
        raise

    except Exception as e:
        # Log error and return error result
        logger.error(
            f"Error searching project {project_id}: {e}",
            exc_info=True
        )
        return ProjectSearchResult(
            project_id=project_id,
            error=f"Search failed: {str(e)}"
        )


def _merge_and_rank_results(
    project_results: List[ProjectSearchResult],
    limit: int,
) -> List[Dict[str, Any]]:
    """
    Merge and rank results from multiple projects.

    Args:
        project_results: List of successful project results
        limit: Maximum results to return

    Returns:
        Merged and ranked list of results
    """
    # Collect all results with project_id annotation
    all_results: List[Dict[str, Any]] = []

    for pr in project_results:
        for result in pr.results:
            # Annotate with project_id
            annotated_result = {
                **result,
                'project_id': pr.project_id,
            }
            all_results.append(annotated_result)

    # Sort by score (descending)
    all_results.sort(key=lambda r: r.get('score', 0.0), reverse=True)

    # Apply limit
    return all_results[:limit]
