"""
Global Index for LeIndex.

This module provides cross-project search and comparison dashboard functionality
with a two-tier hybrid architecture:
- Tier 1: Materialized metadata (always fresh, <1ms response)
- Tier 2: Stale-allowed query cache (serves stale immediately, rebuilds async)

The global index enables:
- Cross-project semantic and lexical search
- Project comparison dashboard
- Global aggregate statistics
- Dependency tracking across projects
"""

from .events import (
    ProjectIndexedEvent,
    ProjectUpdatedEvent,
    ProjectDeletedEvent
)
from .tier1_metadata import (
    GlobalIndexTier1,
    ProjectMetadata,
    GlobalStats,
    DashboardData
)
from .tier2_cache import (
    GlobalIndexTier2,
    CachedQuery,
    QueryMetadata
)
from .query_router import (
    QueryRouter,
    QueryResult
)
from .dashboard import (
    get_dashboard_data,
    get_project_comparison,
    get_language_distribution
)
from .cross_project_search import (
    cross_project_search,
    CrossProjectSearchResult,
    ProjectSearchResult,
    CrossProjectSearchError,
    ProjectNotFoundError,
    AllProjectsFailedError,
    InvalidPatternError
)
from .graceful_degradation import (
    DegradedStatus,
    FallbackResult,
    is_leann_available,
    is_tantivy_available,
    is_ripgrep_available,
    is_grep_available,
    fallback_from_leann,
    fallback_from_tantivy,
    fallback_to_ripgrep,
    fallback_to_grep,
    is_project_healthy,
    filter_healthy_projects,
    execute_with_degradation,
    get_backend_status,
    get_current_degradation_level
)

__all__ = [
    # Events
    "ProjectIndexedEvent",
    "ProjectUpdatedEvent",
    "ProjectDeletedEvent",

    # Tier 1
    "GlobalIndexTier1",
    "ProjectMetadata",
    "GlobalStats",
    "DashboardData",

    # Tier 2
    "GlobalIndexTier2",
    "CachedQuery",
    "QueryMetadata",

    # Query Router
    "QueryRouter",
    "QueryResult",

    # Dashboard
    "get_dashboard_data",
    "get_project_comparison",
    "get_language_distribution",

    # Cross-Project Search
    "cross_project_search",
    "CrossProjectSearchResult",
    "ProjectSearchResult",
    "CrossProjectSearchError",
    "ProjectNotFoundError",
    "AllProjectsFailedError",
    "InvalidPatternError",

    # Graceful Degradation
    "DegradedStatus",
    "FallbackResult",
    "is_leann_available",
    "is_tantivy_available",
    "is_ripgrep_available",
    "is_grep_available",
    "fallback_from_leann",
    "fallback_from_tantivy",
    "fallback_to_ripgrep",
    "fallback_to_grep",
    "is_project_healthy",
    "filter_healthy_projects",
    "execute_with_degradation",
    "get_backend_status",
    "get_current_degradation_level",
]
