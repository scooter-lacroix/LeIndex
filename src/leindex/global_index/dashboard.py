"""
Global Index Dashboard Module

This module provides the project comparison dashboard functionality for LeIndex.
The dashboard aggregates project metadata from Tier 1 and provides filtering,
sorting, and comparison capabilities.

PERFORMANCE TARGETS:
- get_dashboard_data(): <1ms response time
- All filtering/sorting: <1ms additional overhead
- Memory overhead: <1MB for dashboard state

DASHBOARD FEATURES:
- Project summary statistics (count, symbols, files, languages)
- Per-project details (name, path, last indexed, counts, health score)
- Filtering by status, language, health score
- Sorting by any field (name, path, last indexed, file count, health score)
- Structured logging for observability

USAGE EXAMPLE:
    from src.leindex.global_index.dashboard import get_dashboard_data

    # Get all projects with statistics
    dashboard = get_dashboard_data()

    # Get filtered and sorted projects
    dashboard = get_dashboard_data(
        status="completed",
        language="Python",
        min_health_score=0.8,
        sort_by="health_score",
        sort_order="descending"
    )

    print(f"Total projects: {dashboard.total_projects}")
    print(f"Total symbols: {dashboard.total_symbols}")
    for project in dashboard.projects:
        print(f"  {project.name}: {project.health_score:.2f}")
"""

import time
import logging
from typing import Dict, List, Optional, Any, Tuple
from dataclasses import dataclass
from enum import Enum

from .tier1_metadata import (
    GlobalIndexTier1,
    DashboardData,
    ProjectMetadata,
    GlobalStats
)
from .monitoring import log_global_index_operation


logger = logging.getLogger(__name__)


class SortField(Enum):
    """
    Fields available for sorting dashboard data.

    Values:
        name: Sort by project name (alphabetical)
        path: Sort by project path (alphabetical)
        last_indexed: Sort by last indexed timestamp (most recent first)
        file_count: Sort by number of files (largest first)
        symbol_count: Sort by number of symbols (largest first)
        health_score: Sort by health score (highest first)
        size_mb: Sort by index size in MB (largest first)
        language_count: Sort by number of languages (most diverse first)
    """
    NAME = "name"
    PATH = "path"
    LAST_INDEXED = "last_indexed"
    FILE_COUNT = "file_count"
    SYMBOL_COUNT = "symbol_count"
    HEALTH_SCORE = "health_score"
    SIZE_MB = "size_mb"
    LANGUAGE_COUNT = "language_count"


class SortOrder(Enum):
    """
    Sort order options.

    Values:
        ASC: Ascending order (A-Z, 0-9, low to high)
        DESC: Descending order (Z-A, 9-0, high to low)
    """
    ASC = "ascending"
    DESC = "descending"


class IndexStatus(Enum):
    """
    Project indexing status values.

    Values:
        BUILDING: Project is currently being indexed
        COMPLETED: Project indexing completed successfully
        ERROR: Project indexing failed with errors
        PARTIAL: Project indexing partially completed
    """
    BUILDING = "building"
    COMPLETED = "completed"
    ERROR = "error"
    PARTIAL = "partial"


class HealthCategory(Enum):
    """
    Health score categories for filtering.

    Values:
        HEALTHY: Health score >= 0.8 (good)
        WARNING: Health score 0.5 - 0.79 (acceptable)
        CRITICAL: Health score < 0.5 (needs attention)
    """
    HEALTHY = "healthy"
    WARNING = "warning"
    CRITICAL = "critical"


@dataclass
class DashboardFilter:
    """
    Filter criteria for dashboard queries.

    Attributes:
        status: Filter by index status (completed, indexing, error)
        language: Filter by programming language
        health_category: Filter by health category
        min_health_score: Filter by minimum health score (0.0 - 1.0)
        max_health_score: Filter by maximum health score (0.0 - 1.0)
        min_file_count: Filter by minimum file count
        max_file_count: Filter by maximum file count
        min_symbol_count: Filter by minimum symbol count
        max_symbol_count: Filter by maximum symbol count
        project_id_prefix: Filter by project ID prefix
    """
    status: Optional[str] = None
    language: Optional[str] = None
    health_category: Optional[str] = None
    min_health_score: Optional[float] = None
    max_health_score: Optional[float] = None
    min_file_count: Optional[int] = None
    max_file_count: Optional[int] = None
    min_symbol_count: Optional[int] = None
    max_symbol_count: Optional[int] = None
    project_id_prefix: Optional[str] = None

    def is_empty(self) -> bool:
        """
        Check if filter has any active criteria.

        Returns:
            True if no filter criteria are set, False otherwise
        """
        return all([
            self.status is None,
            self.language is None,
            self.health_category is None,
            self.min_health_score is None,
            self.max_health_score is None,
            self.min_file_count is None,
            self.max_file_count is None,
            self.min_symbol_count is None,
            self.max_symbol_count is None,
            self.project_id_prefix is None
        ])

    def to_dict(self) -> Dict[str, Any]:
        """
        Convert filter to dictionary for logging.

        Returns:
            Dictionary representation of filter
        """
        return {
            'status': self.status,
            'language': self.language,
            'health_category': self.health_category,
            'min_health_score': self.min_health_score,
            'max_health_score': self.max_health_score,
            'min_file_count': self.min_file_count,
            'max_file_count': self.max_file_count,
            'min_symbol_count': self.min_symbol_count,
            'max_symbol_count': self.max_symbol_count,
            'project_id_prefix': self.project_id_prefix
        }


@dataclass
class DashboardSort:
    """
    Sort configuration for dashboard queries.

    Attributes:
        field: Field to sort by
        order: Sort order (ascending or descending)
    """
    field: SortField = SortField.NAME
    order: SortOrder = SortOrder.ASC

    def to_dict(self) -> Dict[str, str]:
        """
        Convert sort to dictionary for logging.

        Returns:
            Dictionary representation of sort
        """
        return {
            'field': self.field.value,
            'order': self.order.value
        }


def get_dashboard_data(
    tier1: Optional[GlobalIndexTier1] = None,
    status_filter: Optional[str] = None,
    language_filter: Optional[str] = None,
    health_category_filter: Optional[str] = None,
    min_health_score: Optional[float] = None,
    max_health_score: Optional[float] = None,
    sort_by: Optional[str] = None,
    sort_order: Optional[str] = None,
    limit: Optional[int] = None
) -> DashboardData:
    """
    Get dashboard data with optional filtering and sorting.

    This is the main entry point for dashboard queries. It retrieves
    project metadata from Tier 1, applies filters, sorts results, and
    returns complete dashboard data including aggregated statistics.

    Performance Target: <1ms (P50)

    Args:
        tier1: Optional GlobalIndexTier1 instance (uses singleton if None)
        status_filter: Filter by index status ("completed", "indexing", "error")
        language_filter: Filter by programming language (case-insensitive)
        health_category_filter: Filter by health category ("healthy", "warning", "critical")
        min_health_score: Filter by minimum health score (0.0 - 1.0)
        max_health_score: Filter by maximum health score (0.0 - 1.0)
        sort_by: Field to sort by ("name", "path", "last_indexed", "file_count",
                 "symbol_count", "health_score", "size_mb", "language_count")
        sort_order: Sort order ("ascending" or "descending")
        limit: Maximum number of projects to return

    Returns:
        DashboardData with filtered and sorted projects

    Raises:
        ValueError: If filter or sort parameters are invalid

    Example:
        # Get all completed Python projects with high health score, sorted by name
        dashboard = get_dashboard_data(
            status_filter="completed",
            language_filter="Python",
            min_health_score=0.8,
            sort_by="name",
            sort_order="ascending"
        )
    """
    start_time = time.time()

    # Get or create Tier 1 instance
    if tier1 is None:
        tier1 = GlobalIndexTier1()

    # Validate parameters
    _validate_filter_parameters(status_filter, health_category_filter, min_health_score, max_health_score)
    _validate_sort_parameters(sort_by, sort_order)

    # Build filter and sort objects
    dashboard_filter = DashboardFilter(
        status=status_filter,
        language=language_filter,
        health_category=health_category_filter,
        min_health_score=min_health_score,
        max_health_score=max_health_score
    )

    dashboard_sort = None
    if sort_by is not None:
        field = SortField(sort_by)
        order = SortOrder(sort_order) if sort_order else SortOrder.ASC
        dashboard_sort = DashboardSort(field=field, order=order)

    # Get raw dashboard data from Tier 1
    dashboard = tier1.get_dashboard_data()

    # Apply filters
    if not dashboard_filter.is_empty():
        dashboard.projects = _apply_filters(
            dashboard.projects,
            dashboard_filter
        )

    # Apply sorting
    if dashboard_sort is not None:
        dashboard.projects = _apply_sort(
            dashboard.projects,
            dashboard_sort
        )

    # Apply limit
    if limit is not None and limit > 0:
        dashboard.projects = dashboard.projects[:limit]

    # Recalculate totals based on filtered results
    dashboard.total_projects = len(dashboard.projects)
    dashboard.total_symbols = sum(p.symbol_count for p in dashboard.projects)
    dashboard.total_files = sum(p.file_count for p in dashboard.projects)

    # Recalculate health score and size for filtered results
    if dashboard.projects:
        dashboard.average_health_score = (
            sum(p.health_score for p in dashboard.projects) / len(dashboard.projects)
        )
        dashboard.total_size_mb = sum(p.size_mb for p in dashboard.projects)
    else:
        dashboard.average_health_score = 1.0
        dashboard.total_size_mb = 0.0

    # Log operation
    duration_ms = (time.time() - start_time) * 1000
    log_global_index_operation(
        operation='get_dashboard_data',
        component='dashboard',
        status='success',
        duration_ms=duration_ms,
        filter=dashboard_filter.to_dict() if not dashboard_filter.is_empty() else None,
        sort=dashboard_sort.to_dict() if dashboard_sort else None,
        result_count=len(dashboard.projects)
    )

    # Performance warning
    if duration_ms > 1:
        logger.warning(
            f"Dashboard query took {duration_ms:.2f}ms, exceeds 1ms target"
        )

    return dashboard


def _validate_filter_parameters(
    status_filter: Optional[str] = None,
    health_category_filter: Optional[str] = None,
    min_health_score: Optional[float] = None,
    max_health_score: Optional[float] = None
) -> None:
    """
    Validate filter parameters.

    Args:
        status_filter: Status filter value
        health_category_filter: Health category filter value
        min_health_score: Minimum health score value
        max_health_score: Maximum health score value

    Raises:
        ValueError: If any parameter is invalid
    """
    if status_filter is not None:
        valid_statuses = [s.value for s in IndexStatus]
        if status_filter not in valid_statuses:
            raise ValueError(
                f"Invalid status_filter: {status_filter}. "
                f"Must be one of {valid_statuses}"
            )

    if health_category_filter is not None:
        valid_categories = [c.value for c in HealthCategory]
        if health_category_filter not in valid_categories:
            raise ValueError(
                f"Invalid health_category_filter: {health_category_filter}. "
                f"Must be one of {valid_categories}"
            )

    if min_health_score is not None:
        if not 0.0 <= min_health_score <= 1.0:
            raise ValueError(
                f"min_health_score must be between 0.0 and 1.0, got {min_health_score}"
            )

    if max_health_score is not None:
        if not 0.0 <= max_health_score <= 1.0:
            raise ValueError(
                f"max_health_score must be between 0.0 and 1.0, got {max_health_score}"
            )

    if min_health_score is not None and max_health_score is not None:
        if min_health_score > max_health_score:
            raise ValueError(
                f"min_health_score ({min_health_score}) cannot be greater than "
                f"max_health_score ({max_health_score})"
            )


def _validate_sort_parameters(
    sort_by: Optional[str] = None,
    sort_order: Optional[str] = None
) -> None:
    """
    Validate sort parameters.

    Args:
        sort_by: Sort field value
        sort_order: Sort order value

    Raises:
        ValueError: If any parameter is invalid
    """
    if sort_by is not None:
        valid_fields = [f.value for f in SortField]
        if sort_by not in valid_fields:
            raise ValueError(
                f"Invalid sort_by: {sort_by}. "
                f"Must be one of {valid_fields}"
            )

    if sort_order is not None:
        valid_orders = [o.value for o in SortOrder]
        if sort_order not in valid_orders:
            raise ValueError(
                f"Invalid sort_order: {sort_order}. "
                f"Must be one of {valid_orders}"
            )


def _apply_filters(
    projects: List[ProjectMetadata],
    filter_obj: DashboardFilter
) -> List[ProjectMetadata]:
    """
    Apply filters to a list of projects.

    Args:
        projects: List of projects to filter
        filter_obj: Filter criteria

    Returns:
        Filtered list of projects
    """
    filtered = projects

    # Status filter
    if filter_obj.status:
        filtered = [
            p for p in filtered
            if p.index_status == filter_obj.status
        ]

    # Language filter (case-insensitive)
    if filter_obj.language:
        lang_lower = filter_obj.language.lower()
        filtered = [
            p for p in filtered
            if any(lang_lower in lang.lower() for lang in p.languages.keys())
        ]

    # Health category filter
    if filter_obj.health_category:
        if filter_obj.health_category == HealthCategory.HEALTHY.value:
            filtered = [p for p in filtered if p.health_score >= 0.8]
        elif filter_obj.health_category == HealthCategory.WARNING.value:
            filtered = [p for p in filtered if 0.5 <= p.health_score < 0.8]
        elif filter_obj.health_category == HealthCategory.CRITICAL.value:
            filtered = [p for p in filtered if p.health_score < 0.5]

    # Min health score filter
    if filter_obj.min_health_score is not None:
        filtered = [
            p for p in filtered
            if p.health_score >= filter_obj.min_health_score
        ]

    # Max health score filter
    if filter_obj.max_health_score is not None:
        filtered = [
            p for p in filtered
            if p.health_score <= filter_obj.max_health_score
        ]

    # Min file count filter
    if filter_obj.min_file_count is not None:
        filtered = [
            p for p in filtered
            if p.file_count >= filter_obj.min_file_count
        ]

    # Max file count filter
    if filter_obj.max_file_count is not None:
        filtered = [
            p for p in filtered
            if p.file_count <= filter_obj.max_file_count
        ]

    # Min symbol count filter
    if filter_obj.min_symbol_count is not None:
        filtered = [
            p for p in filtered
            if p.symbol_count >= filter_obj.min_symbol_count
        ]

    # Max symbol count filter
    if filter_obj.max_symbol_count is not None:
        filtered = [
            p for p in filtered
            if p.symbol_count <= filter_obj.max_symbol_count
        ]

    # Project ID prefix filter
    if filter_obj.project_id_prefix:
        filtered = [
            p for p in filtered
            if p.id.startswith(filter_obj.project_id_prefix)
        ]

    return filtered


def _apply_sort(
    projects: List[ProjectMetadata],
    sort_obj: DashboardSort
) -> List[ProjectMetadata]:
    """
    Apply sorting to a list of projects.

    Args:
        projects: List of projects to sort
        sort_obj: Sort configuration

    Returns:
        Sorted list of projects
    """
    # Get sort key function
    key_func = _get_sort_key(sort_obj.field)

    # Sort projects
    reverse = sort_obj.order == SortOrder.DESC
    sorted_projects = sorted(projects, key=key_func, reverse=reverse)

    return sorted_projects


def _get_sort_key(field: SortField):
    """
    Get a sort key function for the specified field.

    Args:
        field: Field to sort by

    Returns:
        Function that extracts sort key from ProjectMetadata
    """
    if field == SortField.NAME:
        return lambda p: p.name.lower()
    elif field == SortField.PATH:
        return lambda p: p.path.lower()
    elif field == SortField.LAST_INDEXED:
        return lambda p: p.last_indexed
    elif field == SortField.FILE_COUNT:
        return lambda p: p.file_count
    elif field == SortField.SYMBOL_COUNT:
        return lambda p: p.symbol_count
    elif field == SortField.HEALTH_SCORE:
        return lambda p: p.health_score
    elif field == SortField.SIZE_MB:
        return lambda p: p.size_mb
    elif field == SortField.LANGUAGE_COUNT:
        return lambda p: len(p.languages)
    else:
        # Default to name
        return lambda p: p.name.lower()


def get_project_comparison(
    tier1: Optional[GlobalIndexTier1] = None,
    project_ids: Optional[List[str]] = None
) -> Dict[str, Any]:
    """
    Get detailed comparison data for specific projects.

    This function provides detailed metrics for comparing multiple projects,
    including language distribution, size metrics, and health scores.

    Args:
        tier1: Optional GlobalIndexTier1 instance (uses singleton if None)
        project_ids: List of project IDs to compare (None = all projects)

    Returns:
        Dictionary with comparison data including per-project metrics

    Example:
        comparison = get_project_comparison(
            project_ids=["project_a", "project_b"]
        )
        print(f"Project A health: {comparison['projects']['project_a']['health_score']}")
    """
    start_time = time.time()

    if tier1 is None:
        tier1 = GlobalIndexTier1()

    # Get all projects or specific projects
    if project_ids:
        projects = []
        for pid in project_ids:
            metadata = tier1.get_project_metadata(pid)
            if metadata:
                projects.append(metadata)
    else:
        dashboard = tier1.get_dashboard_data()
        projects = dashboard.projects

    # Build comparison data
    comparison = {
        'project_count': len(projects),
        'projects': {},
        'aggregated': {
            'total_symbols': sum(p.symbol_count for p in projects),
            'total_files': sum(p.file_count for p in projects),
            'total_size_mb': sum(p.size_mb for p in projects),
            'average_health_score': sum(p.health_score for p in projects) / len(projects) if projects else 0.0,
            'languages': {}
        }
    }

    # Add per-project details
    for project in projects:
        comparison['projects'][project.id] = {
            'name': project.name,
            'path': project.path,
            'last_indexed': project.last_indexed,
            'symbol_count': project.symbol_count,
            'file_count': project.file_count,
            'languages': project.languages,
            'health_score': project.health_score,
            'index_status': project.index_status,
            'size_mb': project.size_mb,
            'language_count': len(project.languages)
        }

        # Aggregate languages
        for lang, count in project.languages.items():
            if lang not in comparison['aggregated']['languages']:
                comparison['aggregated']['languages'][lang] = 0
            comparison['aggregated']['languages'][lang] += count

    # Log operation
    duration_ms = (time.time() - start_time) * 1000
    log_global_index_operation(
        operation='get_project_comparison',
        component='dashboard',
        status='success',
        duration_ms=duration_ms,
        project_count=len(projects)
    )

    return comparison


def get_language_distribution(
    tier1: Optional[GlobalIndexTier1] = None,
    status_filter: Optional[str] = None
) -> Dict[str, Any]:
    """
    Get language distribution across all indexed projects.

    This function provides statistics on which programming languages
    are used across the codebase, with optional filtering by status.

    Args:
        tier1: Optional GlobalIndexTier1 instance (uses singleton if None)
        status_filter: Optional filter by index status

    Returns:
        Dictionary with language distribution statistics

    Example:
        dist = get_language_distribution(status_filter="completed")
        for lang, stats in dist['languages'].items():
            print(f"{lang}: {stats['file_count']} files")
    """
    start_time = time.time()

    if tier1 is None:
        tier1 = GlobalIndexTier1()

    # Get dashboard data
    dashboard = get_dashboard_data(
        tier1=tier1,
        status_filter=status_filter
    )

    # Calculate language distribution
    language_stats = {}
    for project in dashboard.projects:
        for lang, count in project.languages.items():
            if lang not in language_stats:
                language_stats[lang] = {
                    'file_count': 0,
                    'project_count': 0
                }
            language_stats[lang]['file_count'] += count
            language_stats[lang]['project_count'] += 1

    # Sort by file count
    sorted_languages = dict(
        sorted(
            language_stats.items(),
            key=lambda x: x[1]['file_count'],
            reverse=True
        )
    )

    # Log operation
    duration_ms = (time.time() - start_time) * 1000
    log_global_index_operation(
        operation='get_language_distribution',
        component='dashboard',
        status='success',
        duration_ms=duration_ms,
        language_count=len(sorted_languages)
    )

    return {
        'language_count': len(sorted_languages),
        'languages': sorted_languages,
        'total_files': dashboard.total_files
    }
