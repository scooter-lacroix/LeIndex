"""
Global Index - Coordinator for Tier 1 and Tier 2

This module provides the main GlobalIndex class that coordinates
Tier 1 (materialized metadata) and Tier 2 (stale-allowed query cache).

The GlobalIndex subscribes to project registry events and updates
both tiers accordingly:
- Tier 1: Synchronous metadata update (<5ms)
- Tier 2: Mark affected queries stale (don't rebuild)

Architecture:
    ┌─────────────────────────────────────────┐
    │           GlobalIndex                   │
    ├─────────────────────────────────────────┤
    │  - tier1: GlobalIndexTier1              │  ← Synchronous updates
    │  - tier2: GlobalIndexTier2              │  ← Stale marking
    │  - router: QueryRouter                  │  ← Query routing
    │  - event_bus: EventBus                  │  ← Event subscriptions
    └─────────────────────────────────────────┘
                ↑
                | ProjectIndexedEvent
                |
    ProjectRegistry.on_index_complete()
"""

import logging
import time
from typing import Optional, Set, Any, Dict, List
from dataclasses import dataclass

from .event_bus import EventBus, Event, get_global_event_bus
from .tier1_metadata import GlobalIndexTier1, ProjectMetadata, DashboardData
from .tier2_cache import GlobalIndexTier2, QueryMetadata
from .query_router import QueryRouter
from .events import ProjectIndexedEvent, ProjectDeletedEvent
from .monitoring import (
    get_global_index_monitor,
    log_global_index_operation,
    GlobalIndexError,
    CacheError,
    RoutingError
)


logger = logging.getLogger(__name__)


@dataclass
class GlobalIndexConfig:
    """
    Configuration for the GlobalIndex.

    Attributes:
        tier2_max_size_mb: Maximum cache size for Tier 2 (MB)
        tier2_max_workers: Maximum worker threads for async rebuilds
        enable_tier2_cache: Whether to enable Tier 2 caching
    """
    tier2_max_size_mb: float = 500.0
    tier2_max_workers: int = 2
    enable_tier2_cache: bool = True

    def __post_init__(self):
        """Validate configuration parameters after initialization.

        Raises:
            ValueError: If any configuration parameter is invalid
        """
        # Validate tier2_max_size_mb
        if self.tier2_max_size_mb < 0:
            raise ValueError(
                f"tier2_max_size_mb must be >= 0, got {self.tier2_max_size_mb}"
            )
        if self.tier2_max_size_mb > 100000:
            logger.warning(
                f"tier2_max_size_mb is very large: {self.tier2_max_size_mb}MB. "
                "This may cause memory issues."
            )

        # Validate tier2_max_workers
        if self.tier2_max_workers < 1:
            raise ValueError(
                f"tier2_max_workers must be >= 1, got {self.tier2_max_workers}"
            )
        if self.tier2_max_workers > 100:
            logger.warning(
                f"tier2_max_workers is very large: {self.tier2_max_workers}. "
                "This may cause thread contention."
            )

        # Warn if Tier 2 is disabled
        if not self.enable_tier2_cache:
            logger.warning(
                "Tier 2 cache is disabled. Cross-project queries will be slower."
            )


class GlobalIndex:
    """
    Main coordinator for the global index system.

    This class integrates Tier 1 (metadata) and Tier 2 (query cache)
    with the project registry through event-driven updates.

    Thread Safety:
        This class is thread-safe and can be used in multi-threaded
        MCP server environments. All tier operations are protected
        by their respective locks.

    Example:
        >>> global_index = GlobalIndex()
        >>> global_index.initialize_from_registry(registry)
        >>> dashboard = global_index.get_dashboard_data()
        >>> print(f"Total projects: {dashboard.total_projects}")
    """

    def __init__(
        self,
        config: Optional[GlobalIndexConfig] = None,
        event_bus: Optional[EventBus] = None
    ):
        """
        Initialize the GlobalIndex.

        Args:
            config: Optional configuration
            event_bus: Optional event bus (uses global instance if None)
        """
        self.config = config or GlobalIndexConfig()

        # Initialize tiers
        self.tier1 = GlobalIndexTier1()
        self.tier2 = GlobalIndexTier2(
            max_size_mb=self.config.tier2_max_size_mb,
            max_workers=self.config.tier2_max_workers
        ) if self.config.enable_tier2_cache else None

        # Initialize query router with a dummy project_index_getter
        # This will be replaced when the global index is integrated with the engine
        def _dummy_project_index_getter(project_id: str) -> Any:
            """Placeholder project index getter."""
            # TODO: Replace with actual implementation when integrating with engine
            logger.warning(f"Project index getter not implemented for {project_id}")
            return None

        self.router = QueryRouter(
            tier1=self.tier1,
            tier2=self.tier2,
            project_index_getter=_dummy_project_index_getter
        )

        # Event bus
        self.event_bus = event_bus or get_global_event_bus()

        # Subscription management
        self._unsubscribe_callbacks: List[Any] = []

        # Statistics
        self._stats = {
            'events_received': 0,
            'tier1_updates': 0,
            'tier2_invalidations': 0,
        }
        self._stats_lock: Any = None  # Will be set in __init__
        import threading
        self._stats_lock = threading.Lock()

        # Monitoring
        self.monitor = get_global_index_monitor()

        logger.info(
            f"GlobalIndex initialized with "
            f"Tier2 {'enabled' if self.tier2 else 'disabled'}"
        )

    def subscribe_to_events(self) -> None:
        """
        Subscribe to project registry events.

        This method subscribes the GlobalIndex to all relevant
        events from the project registry. Call this during initialization.

        Example:
            >>> global_index = GlobalIndex()
            >>> global_index.subscribe_to_events()
        """
        # Subscribe to project indexed events
        unsubscribe_indexed = self.event_bus.subscribe(
            "project_indexed",
            self._on_project_indexed_event
        )
        self._unsubscribe_callbacks.append(unsubscribe_indexed)

        # Subscribe to project deleted events
        unsubscribe_deleted = self.event_bus.subscribe(
            "project_deleted",
            self._on_project_deleted_event
        )
        self._unsubscribe_callbacks.append(unsubscribe_deleted)

        logger.info("GlobalIndex: Subscribed to project registry events")

    def unsubscribe_from_events(self) -> None:
        """
        Unsubscribe from all project registry events.

        This method removes all event subscriptions. Call this
        during cleanup or shutdown.
        """
        for unsubscribe in self._unsubscribe_callbacks:
            try:
                unsubscribe()
            except Exception as e:
                logger.error(f"Error unsubscribing: {e}")

        self._unsubscribe_callbacks.clear()
        logger.info("GlobalIndex: Unsubscribed from all events")

    def _on_project_indexed_event(self, event: Event) -> None:
        """
        Handle project indexed event.

        This method is called synchronously when a project indexing completes.
        It updates Tier 1 metadata and marks affected Tier 2 queries as stale.

        Args:
            event: Event object containing project indexing results
        """
        with self._stats_lock:
            self._stats['events_received'] += 1

        start_time = time.time()

        try:
            # Extract data from event
            data = event.data
            project_id = data.get('project_id')
            project_path = data.get('project_path')
            stats = data.get('stats', {})
            status = data.get('status', 'completed')
            error_message = data.get('error_message')
            metadata = data.get('metadata', {})

            if not project_id or not project_path:
                logger.error(
                    f"Invalid project_indexed event: missing project_id or project_path"
                )
                return

            # Create ProjectIndexedEvent
            indexed_event = ProjectIndexedEvent(
                project_id=project_id,
                project_path=project_path,
                timestamp=event.timestamp,
                stats=stats,
                status=status,
                error_message=error_message,
                metadata=metadata
            )

            # Update Tier 1 (synchronous, <5ms target)
            self.tier1.on_project_indexed(indexed_event)

            with self._stats_lock:
                self._stats['tier1_updates'] += 1

            # Mark Tier 2 queries stale (don't rebuild)
            if self.tier2:
                self.tier2.mark_project_stale(project_id)

                with self._stats_lock:
                    self._stats['tier2_invalidations'] += 1

            duration_ms = (time.time() - start_time) * 1000

            # Record metrics
            self.monitor.record_project_indexed()

            # Log structured operation
            log_global_index_operation(
                operation='project_indexed',
                component='global_index',
                status='success',
                duration_ms=duration_ms,
                project_id=project_id,
                project_path=project_path,
                file_count=stats.get('total_files', 0)
            )

            logger.debug(
                f"GlobalIndex: Processed project_indexed event for {project_id} "
                f"in {duration_ms:.2f}ms"
            )

            # Performance target: <5ms for synchronous update
            if duration_ms > 5:
                logger.warning(
                    f"GlobalIndex: Event processing took {duration_ms:.2f}ms, "
                    f"exceeds 5ms target"
                )

        except Exception as e:
            error = GlobalIndexError(
                message=f"Error processing project_indexed event: {e}",
                component='global_index',
                details={'project_id': project_id, 'event_data': data}
            )
            self.monitor.record_error(error)
            logger.error(
                f"GlobalIndex: Error processing project_indexed event: {e}",
                exc_info=True
            )

    def _on_project_deleted_event(self, event: Event) -> None:
        """
        Handle project deleted event.

        This method is called when a project is deleted from the registry.
        It removes metadata from Tier 1 and invalidates all related queries in Tier 2.

        Args:
            event: Event object containing deleted project info
        """
        with self._stats_lock:
            self._stats['events_received'] += 1

        start_time = time.time()

        try:
            # Extract data from event
            data = event.data
            project_id = data.get('project_id')

            if not project_id:
                logger.error(
                    f"Invalid project_deleted event: missing project_id"
                )
                return

            # Remove from Tier 1
            # Note: Tier1 doesn't have a remove method, so we'll need to add it
            # For now, we'll just log it
            logger.info(f"GlobalIndex: Project {project_id} deleted (Tier1 cleanup needed)")

            # Invalidate all Tier 2 queries involving this project
            if self.tier2:
                self.tier2.mark_project_stale(project_id)

                with self._stats_lock:
                    self._stats['tier2_invalidations'] += 1

            duration_ms = (time.time() - start_time) * 1000
            logger.debug(
                f"GlobalIndex: Processed project_deleted event for {project_id} "
                f"in {duration_ms:.2f}ms"
            )

        except Exception as e:
            logger.error(
                f"GlobalIndex: Error processing project_deleted event: {e}",
                exc_info=True
            )

    def initialize_from_registry(self, registry: Any) -> None:
        """
        Initialize GlobalIndex from existing project registry.

        This method loads all existing projects from the registry
        and populates Tier 1 metadata.

        Args:
            registry: ProjectRegistry instance
        """
        logger.info("GlobalIndex: Initializing from registry")

        projects = registry.list_all()

        for project in projects:
            try:
                # Create ProjectIndexedEvent from registry data
                event = ProjectIndexedEvent(
                    project_id=str(project.id),
                    project_path=project.path,
                    timestamp=project.indexed_at.timestamp(),
                    stats=project.stats,
                    status="completed",
                    metadata=project.config
                )

                # Update Tier 1
                self.tier1.on_project_indexed(event)

            except Exception as e:
                logger.error(
                    f"Error loading project {project.id} into GlobalIndex: {e}"
                )

        logger.info(
            f"GlobalIndex: Loaded {len(projects)} projects from registry"
        )

    # Delegate methods to tiers and router

    def get_dashboard_data(self) -> DashboardData:
        """
        Get dashboard data from Tier 1.

        Returns:
            DashboardData with all projects and statistics
        """
        return self.tier1.get_dashboard_data()

    def get_project_metadata(self, project_id: str) -> Optional[ProjectMetadata]:
        """
        Get metadata for a specific project.

        Args:
            project_id: Unique project identifier

        Returns:
            ProjectMetadata if found, None otherwise
        """
        return self.tier1.get_project_metadata(project_id)

    def list_projects(
        self,
        status: Optional[str] = None,
        language: Optional[str] = None,
        min_health_score: Optional[float] = None,
        limit: Optional[int] = None
    ) -> List[ProjectMetadata]:
        """
        List projects with optional filtering.

        Args:
            status: Filter by index status
            language: Filter by primary programming language
            min_health_score: Filter by minimum health score
            limit: Maximum number of projects to return

        Returns:
            List of ProjectMetadata matching filters
        """
        return self.tier1.list_projects(
            status=status,
            language=language,
            min_health_score=min_health_score,
            limit=limit
        )

    def query(
        self,
        query_type: str,
        params: Dict[str, Any]
    ) -> tuple[Any, QueryMetadata]:
        """
        Execute a query through the router.

        Args:
            query_type: Type of query ("tier1", "tier2", "direct", "federated")
            params: Query parameters

        Returns:
            Tuple of (result_data, query_metadata)
        """
        return self.router.query(query_type, params)

    def get_stats(self) -> Dict[str, int]:
        """
        Get global index statistics.

        Returns:
            Dictionary with statistics
        """
        tier1_stats = {
            'project_count': len(self.tier1.list_all_project_ids()),
            'memory_mb': self.tier1.get_memory_usage_mb(),
        }

        tier2_stats = {}
        if self.tier2:
            tier2_stats = self.tier2.stats.copy()

        with self._stats_lock:
            internal_stats = self._stats.copy()

        return {
            **tier1_stats,
            **tier2_stats,
            **internal_stats,
        }

    def shutdown(self) -> None:
        """
        Shutdown the GlobalIndex and cleanup resources.

        This method unsubscribes from events and shuts down
        the Tier 2 rebuild executor.
        """
        logger.info("GlobalIndex: Shutting down")

        # Unsubscribe from events
        self.unsubscribe_from_events()

        # Shutdown Tier 2 executor
        if self.tier2:
            self.tier2.rebuild_executor.shutdown(wait=True)

        logger.info("GlobalIndex: Shutdown complete")
