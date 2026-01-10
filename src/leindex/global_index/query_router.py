"""
Query Router for Global Index.

Routes queries to appropriate tier based on query type and parameters:
- Tier 1: Metadata/dashboard queries (<1ms, always fresh)
- Direct: Project-specific queries (no caching)
- Tier 2: Cross-project queries (stale-allowed cache)
- Federation: Uncacheable queries (direct federation)
"""

import hashlib
import json
import logging
import time
from dataclasses import dataclass, field
from datetime import date, datetime
from typing import Any, Callable, Dict, List, Optional, Set, Tuple

from leindex.global_index.tier1_metadata import GlobalIndexTier1
from leindex.global_index.tier2_cache import GlobalIndexTier2, QueryMetadata
from leindex.global_index.monitoring import (
    get_global_index_monitor,
    log_global_index_operation,
    RoutingError
)

logger = logging.getLogger(__name__)


@dataclass
class QueryResult:
    """Result from a routed query with metadata."""
    data: Any
    metadata: QueryMetadata
    source: str  # 'tier1', 'direct', 'tier2_fresh', 'tier2_stale', 'federation'


class QueryRouter:
    """
    Route queries to appropriate tier based on query type and parameters.

    Routing Logic:
    1. Metadata/dashboard → Tier 1 (instant, always fresh)
    2. Project-specific → Direct to project index (no caching)
    3. Cross-project → Tier 2 cache (stale-allowed)
    4. Uncacheable → Direct federation
    """

    # Query types that go to Tier 1
    TIER1_QUERY_TYPES = {
        'dashboard',
        'project_list',
        'project_metadata',
        'global_stats',
        'project_health',
    }

    # Query types that go to Tier 2 (cacheable cross-project)
    TIER2_QUERY_TYPES = {
        'cross_project_search',
        'dependency_graph',
        'find_pattern',
        'aggregate_exports',
    }

    def __init__(
        self,
        tier1: GlobalIndexTier1,
        tier2: GlobalIndexTier2,
        project_index_getter: Callable[[str], Any]
    ):
        """
        Initialize query router.

        Args:
            tier1: Tier 1 metadata cache
            tier2: Tier 2 query cache
            project_index_getter: Function to get project index by ID
        """
        self.tier1 = tier1
        self.tier2 = tier2
        self.project_index_getter = project_index_getter
        self.monitor = get_global_index_monitor()

    def query(
        self,
        query_type: str,
        params: Dict[str, Any]
    ) -> QueryResult:
        """
        Route query to appropriate tier.

        Args:
            query_type: Type of query (dashboard, cross_project_search, etc.)
            params: Query parameters

        Returns:
            QueryResult with data and metadata

        Raises:
            ValueError: If query_type is unknown or params contain callables
        """
        start_time = time.time()
        status = 'success'
        error_details = None

        try:
            # Route 1: Metadata/dashboard → Tier 1
            if query_type in self.TIER1_QUERY_TYPES:
                self.monitor.record_tier1_query()
                result = self._tier1_query(query_type, params)

            # Route 2: Project-specific → Direct to project
            elif params.get('project_id') and query_type != 'cross_project':
                self.monitor.record_direct_query()
                result = self._direct_project_query(query_type, params)

            # Route 3: Cross-project → Tier 2 cache
            elif query_type in self.TIER2_QUERY_TYPES:
                self.monitor.record_tier2_query()
                result = self._tier2_query(query_type, params)

            # Route 4: Uncacheable → Direct federation
            else:
                self.monitor.record_federated_query()
                result = self._federated_query(query_type, params)

            duration_ms = (time.time() - start_time) * 1000

            # Log structured operation
            log_global_index_operation(
                operation=query_type,
                component='query_router',
                status=status,
                duration_ms=duration_ms,
                query_type=query_type,
                result_source=result.source
            )

            return result

        except Exception as e:
            duration_ms = (time.time() - start_time) * 1000
            status = 'error'
            error_details = {'error': str(e), 'query_type': query_type}

            error = RoutingError(
                message=f"Query routing failed: {e}",
                details={'query_type': query_type, 'params': params}
            )
            self.monitor.record_error(error)

            # Log structured operation
            log_global_index_operation(
                operation=query_type,
                component='query_router',
                status=status,
                duration_ms=duration_ms,
                query_type=query_type,
                error=str(e)
            )

            raise

    def _tier1_query(self, query_type: str, params: Dict[str, Any]) -> QueryResult:
        """
        Route to Tier 1 metadata cache.

        Args:
            query_type: Query type
            params: Query parameters

        Returns:
            QueryResult from Tier 1
        """
        start_time = __import__('time').time()

        if query_type == 'dashboard':
            data = self.tier1.get_dashboard_data()
        elif query_type == 'project_list':
            data = self.tier1.list_projects(
                status=params.get('status'),
                language=params.get('language'),
                min_health_score=params.get('min_health_score'),
                limit=params.get('limit')
            )
        elif query_type == 'project_metadata':
            data = self.tier1.get_project_metadata(
                project_id=params['project_id']
            )
        elif query_type == 'global_stats':
            # Get dashboard data which contains projects list
            dashboard = self.tier1.get_dashboard_data()
            # Calculate average health score from projects
            if dashboard.projects:
                avg_health = sum(p.health_score for p in dashboard.projects) / len(dashboard.projects)
            else:
                avg_health = 1.0
            data = {
                'total_projects': dashboard.total_projects,
                'total_symbols': dashboard.total_symbols,
                'total_files': dashboard.total_files,
                'languages': dashboard.languages,
                'average_health_score': avg_health
            }
        elif query_type == 'project_health':
            # Get project metadata and extract health
            metadata = self.tier1.get_project_metadata(
                project_id=params['project_id']
            )
            if metadata:
                data = {'health_score': metadata.health_score}
            else:
                data = None
        else:
            raise ValueError(f"Unknown Tier 1 query type: {query_type}")

        elapsed_ms = (__import__('time').time() - start_time) * 1000

        return QueryResult(
            data=data,
            metadata=QueryMetadata(
                is_stale=False,
                staleness_age_seconds=0.0,
                rebuild_in_progress=False,
                last_updated=__import__('time').time(),
                source='tier1'
            ),
            source='tier1'
        )

    def _direct_project_query(
        self,
        query_type: str,
        params: Dict[str, Any]
    ) -> QueryResult:
        """
        Route directly to project index (no caching).

        Args:
            query_type: Query type
            params: Query parameters (must include project_id)

        Returns:
            QueryResult from project index
        """
        project_id = params.get('project_id')
        if not project_id:
            raise ValueError("project_id required for direct queries")

        # Get project index
        project_index = self.project_index_getter(project_id)
        if not project_index:
            raise ValueError(f"Project not found: {project_id}")

        # Execute query on project index
        # (This will be implemented when integrating with actual project indexes)
        data = self._execute_project_query(project_index, query_type, params)

        return QueryResult(
            data=data,
            metadata=QueryMetadata(
                is_stale=False,
                staleness_age_seconds=0.0,
                rebuild_in_progress=False,
                last_updated=__import__('time').time(),
                source='direct'
            ),
            source='direct'
        )

    def _tier2_query(self, query_type: str, params: Dict[str, Any]) -> QueryResult:
        """
        Route to Tier 2 cache with stale-allowed reads.

        Args:
            query_type: Query type
            params: Query parameters

        Returns:
            QueryResult from Tier 2 cache
        """
        # Build cache key
        cache_key = self._build_cache_key(query_type, params)

        # Determine involved projects
        involved_projects = self._get_involved_projects(query_type, params)

        # Define query function for cache miss
        def query_func() -> Any:
            return self._execute_federated_query(query_type, params)

        # Query Tier 2 cache
        data, metadata = self.tier2.query(
            cache_key=cache_key,
            query_func=query_func,
            involved_projects=involved_projects
        )

        # Determine source string
        source = metadata.source

        return QueryResult(
            data=data,
            metadata=metadata,
            source=source
        )

    def _federated_query(
        self,
        query_type: str,
        params: Dict[str, Any]
    ) -> QueryResult:
        """
        Execute federated query across multiple project indexes.

        Args:
            query_type: Query type
            params: Query parameters

        Returns:
            QueryResult from federation
        """
        # Execute federated query
        data = self._execute_federated_query(query_type, params)

        return QueryResult(
            data=data,
            metadata=QueryMetadata(
                is_stale=False,
                staleness_age_seconds=0.0,
                rebuild_in_progress=False,
                last_updated=__import__('time').time(),
                source='federation'
            ),
            source='federation'
        )

    def _build_cache_key(self, query_type: str, params: Dict[str, Any]) -> str:
        """
        Build deterministic cache key handling edge cases.

        Args:
            query_type: Type of query
            params: Query parameters

        Returns:
            Cache key string

        Raises:
            ValueError: If params contain callables (cannot be cached)
        """
        normalized = self._normalize_params(params)
        param_str = json.dumps(normalized, sort_keys=True, default=str)
        hash_val = hashlib.sha256(param_str.encode()).hexdigest()[:16]
        return f"{query_type}:{hash_val}"

    def _normalize_params(self, params: Dict[str, Any]) -> Dict[str, Any]:
        """
        Normalize params for consistent cache keys.

        Handles:
        - Sets → Sorted lists
        - Datetimes → ISO format strings
        - Nested dicts/lists → Recursive normalization
        - Callables → Raise error

        Args:
            params: Parameters to normalize

        Returns:
            Normalized parameters

        Raises:
            ValueError: If params contain callables
        """
        def normalize_value(val) -> Any:
            # Handle sets
            if isinstance(val, set):
                return sorted(list(val))

            # Handle datetimes
            elif isinstance(val, (datetime, date)):
                return val.isoformat()

            # Handle dicts
            elif isinstance(val, dict):
                return {
                    k: normalize_value(v)
                    for k, v in sorted(val.items())
                }

            # Handle lists
            elif isinstance(val, list):
                return [normalize_value(v) for v in val]

            # Handle callables (cannot cache) - must check before __dict__
            # because lambda functions have __dict__ but should be rejected
            elif callable(val) and not isinstance(val, type):
                raise ValueError(
                    f"Cannot cache params with callable: {val}. "
                    "Callables cannot be serialized for cache keys."
                )

            # Handle objects with __dict__
            elif hasattr(val, '__dict__'):
                return normalize_value(val.__dict__)

            # Primitive types
            else:
                return val

        # Normalize all params
        return {
            k: normalize_value(v)
            for k, v in sorted(params.items())
        }

    def _get_involved_projects(
        self,
        query_type: str,
        params: Dict[str, Any]
    ) -> Set[str]:
        """
        Get set of project IDs involved in a query.

        Args:
            query_type: Type of query
            params: Query parameters

        Returns:
            Set of project IDs
        """
        if query_type == 'cross_project_search':
            project_ids = params.get('project_ids')
            if project_ids:
                return set(project_ids) if isinstance(project_ids, list) else {project_ids}
            # If no project_ids specified, search all projects
            return self.tier1.list_all_project_ids()

        elif query_type in ('dependency_graph', 'find_pattern', 'aggregate_exports'):
            project_id = params.get('project_id')
            if project_id:
                return {project_id}
            # Query all projects
            return self.tier1.list_all_project_ids()

        else:
            return set()

    def _execute_project_query(
        self,
        project_index: Any,
        query_type: str,
        params: Dict[str, Any]
    ) -> Any:
        """
        Execute query on a single project index.

        Args:
            project_index: Project index to query
            query_type: Type of query
            params: Query parameters

        Returns:
            Query results

        Note:
            This is a placeholder. Will be implemented when integrating
            with actual project index implementations.
        """
        # Placeholder: Will be implemented in Task 2.4
        # when integrating with actual project indexes
        logger.warning(
            f"Project query not yet implemented: {query_type}. "
            "Returning placeholder data."
        )
        return {"placeholder": True, "query_type": query_type}

    def _execute_federated_query(
        self,
        query_type: str,
        params: Dict[str, Any]
    ) -> Any:
        """
        Execute federated query across multiple project indexes.

        Args:
            query_type: Type of query
            params: Query parameters

        Returns:
            Merged query results

        Note:
            This is a placeholder. Will be implemented in Task 2.4
            when integrating with actual project indexes.
        """
        # Placeholder: Will be implemented in Task 2.4
        logger.warning(
            f"Federated query not yet implemented: {query_type}. "
            "Returning placeholder data."
        )
        return {"placeholder": True, "query_type": query_type}
