"""
Global Index Tier 1 - Materialized Metadata

This module implements the Tier 1 global index, which maintains always-fresh
project metadata in memory. Tier 1 is updated synchronously when projects are
indexed and provides O(1) lookups for project metadata and statistics.

Key Characteristics:
- Always fresh (synchronous updates on project index events)
- Fast access (<1ms for dashboard queries)
- Low memory footprint (<10MB for 100 projects)
- Simple in-memory storage (no persistence needed)

Performance Targets:
- get_dashboard_data(): <1ms (P50)
- get_project_metadata(): O(1) lookup
- on_project_indexed(): <5ms synchronous update
- Memory: <10MB for 100 projects

Architecture:
    ┌─────────────────────────────────────────┐
    │         GlobalIndexTier1                │
    ├─────────────────────────────────────────┤
    │  projects: Dict[str, ProjectMetadata]   │  ← O(1) lookup by project_id
    │  global_stats: GlobalStats              │  ← Auto-recomputed on updates
    │  last_updated: float                    │  ← Timestamp of last update
    └─────────────────────────────────────────┘
                ↑
                | ProjectIndexedEvent
                |
    ProjectRegistry.on_index_complete()
"""

import time
import logging
import threading
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Any
from collections import defaultdict

from .events import ProjectIndexedEvent


logger = logging.getLogger(__name__)


@dataclass
class ProjectMetadata:
    """
    Metadata for a single indexed project.

    This dataclass contains all essential information about a project
    that is needed for dashboard display and project comparisons.

    Attributes:
        id: Unique project identifier (typically a slug or UUID)
        name: Human-readable project name
        path: Absolute file system path to the project
        last_indexed: Unix timestamp of last successful index
        symbol_count: Total number of code symbols indexed
        file_count: Total number of source files indexed
        languages: Dictionary mapping language names to file counts
        dependencies: List of project IDs this project depends on
        health_score: Project health score (0.0 = poor, 1.0 = excellent)
        index_status: Current indexing status
        size_mb: Estimated index size in memory (MB)
    """
    id: str
    name: str
    path: str
    last_indexed: float
    symbol_count: int
    file_count: int
    languages: Dict[str, int] = field(default_factory=dict)
    dependencies: List[str] = field(default_factory=list)
    health_score: float = 1.0
    index_status: str = "completed"
    size_mb: float = 0.0

    def __post_init__(self):
        """Validate metadata after initialization."""
        if not 0.0 <= self.health_score <= 1.0:
            raise ValueError(f"health_score must be between 0.0 and 1.0, got {self.health_score}")

        if self.index_status not in ["building", "completed", "error", "partial"]:
            raise ValueError(f"Invalid index_status: {self.index_status}")


@dataclass
class GlobalStats:
    """
    Aggregated statistics across all indexed projects.

    These stats are auto-recomputed whenever a project is indexed or updated.

    Attributes:
        total_projects: Total number of registered projects
        total_symbols: Sum of all symbols across all projects
        total_files: Sum of all files across all projects
        languages: Dictionary mapping language names to total file counts
        average_health_score: Mean health score across all projects
        total_size_mb: Total memory usage of all project indexes (MB)
    """
    total_projects: int
    total_symbols: int
    total_files: int
    languages: Dict[str, int] = field(default_factory=dict)
    average_health_score: float = 1.0
    total_size_mb: float = 0.0


@dataclass
class DashboardData:
    """
    Complete data for the project comparison dashboard.

    This is the primary return type for Tier 1 queries, providing
    all information needed to render the dashboard UI.

    Attributes:
        total_projects: Total number of projects
        total_symbols: Total symbols across all projects
        total_files: Total files across all projects
        languages: Aggregated language statistics
        projects: List of all project metadata
        last_updated: Unix timestamp of last update
        average_health_score: Mean health score across all projects
        total_size_mb: Total memory usage of all project indexes (MB)
    """
    total_projects: int
    total_symbols: int
    total_files: int
    languages: Dict[str, int]
    projects: List[ProjectMetadata]
    last_updated: float
    average_health_score: float = 1.0
    total_size_mb: float = 0.0


class GlobalIndexTier1:
    """
    Tier 1 Global Index - Materialized Metadata.

    This class maintains in-memory metadata for all indexed projects,
    providing fast access to project statistics and dashboard data.

    The tier is updated synchronously when projects are indexed,
    ensuring data is always fresh and consistent.

    Thread Safety:
        This class is thread-safe and can be used in multi-threaded
        MCP server environments. All shared state access is protected
        by an internal lock.

    Example:
        >>> tier1 = GlobalIndexTier1()
        >>> event = ProjectIndexedEvent(
        ...     project_id="my_project",
        ...     project_path="/path/to/project",
        ...     stats={"symbols": 1000, "files": 50}
        ... )
        >>> tier1.on_project_indexed(event)
        >>> dashboard = tier1.get_dashboard_data()
        >>> print(f"Total projects: {dashboard.total_projects}")
    """

    def __init__(self):
        """Initialize an empty Tier 1 index."""
        self._projects: Dict[str, ProjectMetadata] = {}
        self._global_stats: Optional[GlobalStats] = None
        self._last_updated: float = 0.0
        self._stats_dirty: bool = True  # Flag indicating stats need recomputation
        self._lock: threading.Lock = threading.Lock()

        logger.info("GlobalIndexTier1 initialized (thread-safe)")

    def on_project_indexed(self, event: ProjectIndexedEvent) -> None:
        """
        Handle a ProjectIndexedEvent by updating metadata synchronously.

        This method is called synchronously when a project indexing completes.
        It updates the project metadata and marks global stats for recomputation.

        Args:
            event: ProjectIndexedEvent containing indexing results

        Raises:
            ValueError: If event data is invalid
        """
        start_time = time.time()

        try:
            # Extract statistics from event
            stats = event.stats or {}
            symbol_count = stats.get("symbols", 0)
            file_count = stats.get("files", 0)
            languages = stats.get("languages", {})
            size_mb = stats.get("size_mb", 0.0)

            # Calculate health score (could be enhanced with more metrics)
            health_score = self._calculate_health_score(event.status, stats)

            # Create or update project metadata
            metadata = ProjectMetadata(
                id=event.project_id,
                name=self._extract_project_name(event.project_path),
                path=event.project_path,
                last_indexed=event.timestamp,
                symbol_count=symbol_count,
                file_count=file_count,
                languages=languages,
                dependencies=stats.get("dependencies", []),
                health_score=health_score,
                index_status=event.status,
                size_mb=size_mb
            )

            # Store metadata with lock protection
            with self._lock:
                self._projects[event.project_id] = metadata
                self._stats_dirty = True
                self._last_updated = event.timestamp

            duration_ms = (time.time() - start_time) * 1000
            logger.debug(
                f"Tier1: Updated metadata for {event.project_id} in {duration_ms:.2f}ms"
            )

            # Performance target: <5ms for synchronous update
            if duration_ms > 5:
                logger.warning(
                    f"Tier1: Update took {duration_ms:.2f}ms, exceeds 5ms target"
                )

        except Exception as e:
            logger.error(f"Tier1: Failed to update metadata for {event.project_id}: {e}")
            raise

    def get_project_metadata(self, project_id: str) -> Optional[ProjectMetadata]:
        """
        Get metadata for a specific project.

        Args:
            project_id: Unique project identifier

        Returns:
            ProjectMetadata if found, None otherwise
        """
        with self._lock:
            return self._projects.get(project_id)

    def get_dashboard_data(self) -> DashboardData:
        """
        Get complete dashboard data including all projects and global stats.

        This method returns all data needed to render the project comparison
        dashboard. Global stats are recomputed if dirty.

        Performance Target: <1ms (P50)

        Returns:
            DashboardData containing all projects and aggregated statistics
        """
        start_time = time.time()

        with self._lock:
            # Recompute stats if dirty
            if self._stats_dirty:
                self._recompute_global_stats_locked()
                self._stats_dirty = False

            stats = self._global_stats or GlobalStats(
                total_projects=0,
                total_symbols=0,
                total_files=0,
                languages={},
                average_health_score=1.0,
                total_size_mb=0.0
            )

            # Create a copy of projects list to avoid holding lock during iteration
            projects_list = list(self._projects.values())
            last_updated = self._last_updated

        dashboard = DashboardData(
            total_projects=stats.total_projects,
            total_symbols=stats.total_symbols,
            total_files=stats.total_files,
            languages=stats.languages,
            projects=projects_list,
            last_updated=last_updated,
            average_health_score=stats.average_health_score,
            total_size_mb=stats.total_size_mb
        )

        duration_ms = (time.time() - start_time) * 1000
        logger.debug(f"Tier1: get_dashboard_data() took {duration_ms:.2f}ms")

        # Performance target: <1ms
        if duration_ms > 1:
            logger.warning(
                f"Tier1: get_dashboard_data() took {duration_ms:.2f}ms, exceeds 1ms target"
            )

        return dashboard

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
            status: Filter by index status ("building", "completed", "error")
            language: Filter by primary programming language
            min_health_score: Filter by minimum health score
            limit: Maximum number of projects to return

        Returns:
            List of ProjectMetadata matching filters
        """
        with self._lock:
            projects = list(self._projects.values())

        # Apply filters outside lock to minimize contention
        if status:
            projects = [p for p in projects if p.index_status == status]

        if language:
            projects = [p for p in projects if language.lower() in (lang.lower() for lang in p.languages.keys())]

        if min_health_score is not None:
            projects = [p for p in projects if p.health_score >= min_health_score]

        # Apply limit
        if limit:
            projects = projects[:limit]

        return projects

    def _recompute_global_stats_locked(self) -> None:
        """
        Recompute global statistics from all project metadata.

        This is called lazily when stats are needed and marked dirty.
        Must be called while holding the lock.
        """
        if not self._projects:
            self._global_stats = GlobalStats(
                total_projects=0,
                total_symbols=0,
                total_files=0,
                languages={},
                average_health_score=1.0,
                total_size_mb=0.0
            )
            return

        total_symbols = sum(p.symbol_count for p in self._projects.values())
        total_files = sum(p.file_count for p in self._projects.values())
        total_size_mb = sum(p.size_mb for p in self._projects.values())

        # Aggregate languages across all projects
        languages = defaultdict(int)
        for project in self._projects.values():
            for lang, count in project.languages.items():
                languages[lang] += count

        # Calculate average health score
        avg_health = sum(p.health_score for p in self._projects.values()) / len(self._projects)

        self._global_stats = GlobalStats(
            total_projects=len(self._projects),
            total_symbols=total_symbols,
            total_files=total_files,
            languages=dict(languages),
            average_health_score=avg_health,
            total_size_mb=total_size_mb
        )

        logger.debug(
            f"Tier1: Recomputed global stats for {len(self._projects)} projects"
        )

    def _calculate_health_score(self, status: str, stats: Dict[str, Any]) -> float:
        """
        Calculate health score for a project.

        Health score is based on:
        - Index status (completed = 1.0, error = 0.0, building = 0.5)
        - Index size (could indicate issues if too large/small)
        - Could be extended with more metrics

        Args:
            status: Index status
            stats: Indexing statistics

        Returns:
            Health score between 0.0 and 1.0
        """
        if status == "completed":
            base_score = 1.0
        elif status == "building":
            base_score = 0.5
        elif status == "partial":
            base_score = 0.7
        else:  # error
            base_score = 0.0

        # Could add more sophisticated scoring here
        return base_score

    def _extract_project_name(self, project_path: str) -> str:
        """
        Extract a human-readable project name from the project path.

        Args:
            project_path: Absolute path to the project

        Returns:
            Project name (basename of path)
        """
        import os
        return os.path.basename(project_path)

    def get_memory_usage_mb(self) -> float:
        """
        Estimate memory usage of Tier 1 index.

        This is a rough estimate based on the number of projects and
        average metadata size. For accurate measurement, use
        memory profiling tools.

        Returns:
            Estimated memory usage in MB
        """
        with self._lock:
            # Rough estimate: ~1KB per project for metadata
            # (This is conservative; actual usage is likely lower)
            estimated_bytes = len(self._projects) * 1024
            return estimated_bytes / (1024 * 1024)

    def list_all_project_ids(self) -> set:
        """
        Get all registered project IDs.

        Returns:
            Set of project IDs
        """
        with self._lock:
            return set(self._projects.keys())
