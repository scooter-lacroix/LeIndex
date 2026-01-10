"""
Global Index Monitoring Module

This module provides monitoring, metrics collection, and health checks
specifically for the Global Index system.

MONITORING FEATURES:
- Structured JSON logging for all global index operations
- Metrics collection (cache hit rate, query latency, memory usage)
- Health checks for Tier 1, Tier 2, and Query Router
- Error categorization (global_index_error, cache_error, routing_error)

INTEGRATION:
- Uses base monitoring module (src/leindex/monitoring.py)
- Integrates with global index components
- Provides structured logging for observability

USAGE EXAMPLE:
    from src.leindex.global_index.monitoring import (
        get_global_index_monitor,
        log_global_index_operation
    )

    monitor = get_global_index_monitor()
    monitor.record_cache_hit()
    monitor.record_query_latency(0.045)

    log_global_index_operation(
        operation="cross_project_search",
        project_ids=["proj1", "proj2"],
        result_count=42,
        duration_ms=45.2
    )
"""

import json
import time
import threading
from typing import Dict, Any, Optional, List, Iterator
from dataclasses import dataclass, field, asdict
from datetime import datetime
from contextlib import contextmanager

from ..monitoring import (
    MetricsRegistry,
    HealthChecker,
    Histogram,
    Counter,
    Gauge
)
from ..logger_config import logger


@dataclass
class GlobalIndexLogEntry:
    """
    Structured log entry for global index operations.

    FIELDS:
    - timestamp: ISO 8601 timestamp
    - operation: Type of operation performed
    - component: Component that performed the operation
    - status: Success/failure status
    - duration_ms: Operation duration in milliseconds
    - metadata: Additional operation-specific data
    """
    timestamp: str
    operation: str
    component: str
    status: str
    duration_ms: float
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_json(self) -> str:
        """Convert log entry to JSON string."""
        return json.dumps(asdict(self))


class GlobalIndexError(Exception):
    """Base exception for global index errors."""

    def __init__(self, message: str, component: str, details: Optional[Dict[str, Any]] = None):
        """
        Initialize global index error.

        Args:
            message: Error message
            component: Component where error occurred
            details: Additional error details
        """
        super().__init__(message)
        self.component = component
        self.details = details or {}
        self.timestamp = datetime.now().isoformat()

    def to_dict(self) -> Dict[str, Any]:
        """Convert error to dictionary for logging."""
        return {
            'error_type': 'global_index_error',
            'message': str(self),
            'component': self.component,
            'details': self.details,
            'timestamp': self.timestamp
        }


class CacheError(GlobalIndexError):
    """Exception for cache-related errors."""

    def __init__(self, message: str, details: Optional[Dict[str, Any]] = None):
        super().__init__(message, component='tier2_cache', details=details)

    def to_dict(self) -> Dict[str, Any]:
        """Convert error to dictionary for logging."""
        base = super().to_dict()
        base['error_type'] = 'cache_error'
        return base


class RoutingError(GlobalIndexError):
    """Exception for query routing errors."""

    def __init__(self, message: str, details: Optional[Dict[str, Any]] = None):
        super().__init__(message, component='query_router', details=details)

    def to_dict(self) -> Dict[str, Any]:
        """Convert error to dictionary for logging."""
        base = super().to_dict()
        base['error_type'] = 'routing_error'
        return base


class GlobalIndexMonitor:
    """
    Monitor for global index operations.

    COLLECTED METRICS:
    - Cache hit rate: Percentage of cache hits vs misses
    - Query latency: P50, P95, P99 latencies in milliseconds
    - Memory usage: Memory used by global index components
    - Operation counts: Count of operations by type
    - Error counts: Count of errors by category

    USAGE:
        monitor = GlobalIndexMonitor(metrics_registry)

        # Record cache operations
        monitor.record_cache_hit()
        monitor.record_cache_miss()

        # Record query latency
        with monitor.measure_query_latency():
            # Perform query
            pass

        # Get current metrics
        metrics = monitor.get_metrics()
    """

    def __init__(self, metrics_registry: MetricsRegistry):
        """
        Initialize the global index monitor.

        Args:
            metrics_registry: Metrics registry to record metrics to
        """
        self.metrics = metrics_registry

        # Cache metrics
        self.cache_hits = self.metrics.counter(
            'global_index_cache_hits',
            'Number of cache hits in Tier 2 cache'
        )
        self.cache_misses = self.metrics.counter(
            'global_index_cache_misses',
            'Number of cache misses in Tier 2 cache'
        )
        self.cache_evictions = self.metrics.counter(
            'global_index_cache_evictions',
            'Number of cache evictions from LRU'
        )

        # Query metrics
        self.query_latency = self.metrics.histogram(
            'global_index_query_latency_ms',
            'Query latency for global index queries'
        )
        self.tier1_queries = self.metrics.counter(
            'global_index_tier1_queries',
            'Number of Tier 1 (metadata) queries'
        )
        self.tier2_queries = self.metrics.counter(
            'global_index_tier2_queries',
            'Number of Tier 2 (cached) queries'
        )
        self.direct_queries = self.metrics.counter(
            'global_index_direct_queries',
            'Number of direct project queries'
        )
        self.federated_queries = self.metrics.counter(
            'global_index_federated_queries',
            'Number of federated queries across multiple projects'
        )

        # Memory metrics
        self.memory_usage_mb = self.metrics.gauge(
            'global_index_memory_mb',
            'Memory usage of global index in MB'
        )
        self.tier1_memory_mb = self.metrics.gauge(
            'global_index_tier1_memory_mb',
            'Memory usage of Tier 1 metadata in MB'
        )
        self.tier2_memory_mb = self.metrics.gauge(
            'global_index_tier2_memory_mb',
            'Memory usage of Tier 2 cache in MB'
        )

        # Error metrics
        self.global_index_errors = self.metrics.counter(
            'global_index_errors_total',
            'Total number of global index errors'
        )
        self.cache_errors = self.metrics.counter(
            'global_index_cache_errors',
            'Number of cache-related errors'
        )
        self.routing_errors = self.metrics.counter(
            'global_index_routing_errors',
            'Number of query routing errors'
        )

        # Operation metrics
        self.project_indexed = self.metrics.counter(
            'global_index_projects_indexed',
            'Number of projects indexed in global index'
        )
        self.project_stale_marked = self.metrics.counter(
            'global_index_projects_stale_marked',
            'Number of projects marked as stale'
        )
        self.cross_project_searches = self.metrics.counter(
            'global_index_cross_project_searches',
            'Number of cross-project searches performed'
        )

    def record_cache_hit(self) -> None:
        """Record a cache hit."""
        self.cache_hits.inc()

    def record_cache_miss(self) -> None:
        """Record a cache miss."""
        self.cache_misses.inc()

    def record_cache_eviction(self) -> None:
        """Record a cache eviction."""
        self.cache_evictions.inc()

    def get_cache_hit_rate(self) -> float:
        """
        Calculate cache hit rate as percentage.

        Returns:
            Cache hit rate percentage (0-100)
        """
        hits = self.cache_hits.get()
        misses = self.cache_misses.get()
        total = hits + misses

        if total == 0:
            return 0.0

        return (hits / total) * 100

    @contextmanager
    def measure_query_latency(self) -> Iterator[None]:
        """
        Context manager for measuring query latency.

        Example:
            with monitor.measure_query_latency():
                # Perform query
                results = perform_query()
        """
        start_time = time.time()
        try:
            yield
        finally:
            elapsed_ms = (time.time() - start_time) * 1000
            self.query_latency.observe(elapsed_ms)

    def record_tier1_query(self) -> None:
        """Record a Tier 1 (metadata) query."""
        self.tier1_queries.inc()

    def record_tier2_query(self) -> None:
        """Record a Tier 2 (cached) query."""
        self.tier2_queries.inc()

    def record_direct_query(self) -> None:
        """Record a direct project query."""
        self.direct_queries.inc()

    def record_federated_query(self) -> None:
        """Record a federated query."""
        self.federated_queries.inc()

    def update_memory_usage(self, total_mb: float, tier1_mb: float, tier2_mb: float) -> None:
        """
        Update memory usage metrics.

        Args:
            total_mb: Total memory usage in MB
            tier1_mb: Tier 1 memory usage in MB
            tier2_mb: Tier 2 memory usage in MB
        """
        self.memory_usage_mb.set(total_mb)
        self.tier1_memory_mb.set(tier1_mb)
        self.tier2_memory_mb.set(tier2_mb)

    def record_error(self, error: GlobalIndexError) -> None:
        """
        Record a global index error.

        Args:
            error: The error that occurred
        """
        self.global_index_errors.inc()

        if isinstance(error, CacheError):
            self.cache_errors.inc()
        elif isinstance(error, RoutingError):
            self.routing_errors.inc()

        # Log the error with structured format
        logger.error(json.dumps(error.to_dict()))

    def record_project_indexed(self) -> None:
        """Record a project being indexed."""
        self.project_indexed.inc()

    def record_project_stale_marked(self) -> None:
        """Record a project being marked as stale."""
        self.project_stale_marked.inc()

    def record_cross_project_search(self) -> None:
        """Record a cross-project search."""
        self.cross_project_searches.inc()

    def get_query_latency_percentiles(self) -> Dict[str, float]:
        """
        Get query latency percentiles.

        Returns:
            Dictionary with p50, p95, p99 latencies in milliseconds
        """
        stats = self.query_latency.get_stats()
        return {
            'p50': stats.get('p50', 0.0),
            'p95': stats.get('p95', 0.0),
            'p99': stats.get('p99', 0.0)
        }

    def get_metrics(self) -> Dict[str, Any]:
        """
        Get all global index metrics.

        Returns:
            Dictionary with all metrics
        """
        return {
            'cache': {
                'hit_rate': f"{self.get_cache_hit_rate():.2f}%",
                'hits': self.cache_hits.get(),
                'misses': self.cache_misses.get(),
                'evictions': self.cache_evictions.get()
            },
            'queries': {
                'latency_p50_ms': self.get_query_latency_percentiles()['p50'],
                'latency_p95_ms': self.get_query_latency_percentiles()['p95'],
                'latency_p99_ms': self.get_query_latency_percentiles()['p99'],
                'tier1_count': self.tier1_queries.get(),
                'tier2_count': self.tier2_queries.get(),
                'direct_count': self.direct_queries.get(),
                'federated_count': self.federated_queries.get()
            },
            'memory': {
                'total_mb': self.memory_usage_mb.get(),
                'tier1_mb': self.tier1_memory_mb.get(),
                'tier2_mb': self.tier2_memory_mb.get()
            },
            'errors': {
                'total': self.global_index_errors.get(),
                'cache_errors': self.cache_errors.get(),
                'routing_errors': self.routing_errors.get()
            },
            'operations': {
                'projects_indexed': self.project_indexed.get(),
                'projects_stale_marked': self.project_stale_marked.get(),
                'cross_project_searches': self.cross_project_searches.get()
            }
        }


class GlobalIndexHealthChecker:
    """
    Health checker for global index components.

    HEALTH CHECKS:
    - Tier 1 Health: Metadata storage health
    - Tier 2 Health: Cache health
    - Query Router Health: Routing logic health
    - Memory Health: Memory usage within limits
    """

    def __init__(self, health_checker: HealthChecker):
        """
        Initialize the global index health checker.

        Args:
            health_checker: Base health checker to register checks with
        """
        self.health_checker = health_checker
        self._register_checks()

    def _register_checks(self) -> None:
        """Register all health checks."""
        self.health_checker.register_check(
            'global_index_tier1',
            self._check_tier1_health
        )
        self.health_checker.register_check(
            'global_index_tier2',
            self._check_tier2_health
        )
        self.health_checker.register_check(
            'global_index_router',
            self._check_router_health
        )
        self.health_checker.register_check(
            'global_index_memory',
            self._check_memory_health
        )

    def _check_tier1_health(self) -> Dict[str, Any]:
        """
        Check Tier 1 (metadata) health.

        Returns:
            Health check result
        """
        try:
            # Import here to avoid circular dependency
            from .tier1_metadata import GlobalIndexTier1

            tier1 = GlobalIndexTier1()

            # Check if we can access metadata
            project_count = len(tier1._projects)

            return {
                'healthy': True,
                'message': f'Tier 1 healthy with {project_count} projects',
                'details': {
                    'project_count': project_count
                }
            }
        except Exception as e:
            return {
                'healthy': False,
                'message': f'Tier 1 health check failed: {e}',
                'details': {
                    'error': str(e)
                }
            }

    def _check_tier2_health(self) -> Dict[str, Any]:
        """
        Check Tier 2 (cache) health.

        Returns:
            Health check result
        """
        try:
            # Import here to avoid circular dependency
            from .tier2_cache import GlobalIndexTier2

            tier2 = GlobalIndexTier2()

            # Check cache stats
            cache_size = len(tier2._cache._cache_data)
            rebuilding_count = len(tier2._cache._rebuilding_keys)

            return {
                'healthy': True,
                'message': f'Tier 2 healthy with {cache_size} cached queries',
                'details': {
                    'cache_size': cache_size,
                    'rebuilding_count': rebuilding_count
                }
            }
        except Exception as e:
            return {
                'healthy': False,
                'message': f'Tier 2 health check failed: {e}',
                'details': {
                    'error': str(e)
                }
            }

    def _check_router_health(self) -> Dict[str, Any]:
        """
        Check query router health.

        Returns:
            Health check result
        """
        try:
            # Import here to avoid circular dependency
            from .query_router import QueryRouter

            router = QueryRouter()

            # Check if router is initialized
            if router._tier1 is None or router._tier2 is None:
                return {
                    'healthy': False,
                    'message': 'Query router not properly initialized',
                    'details': {
                        'tier1_initialized': router._tier1 is not None,
                        'tier2_initialized': router._tier2 is not None
                    }
                }

            return {
                'healthy': True,
                'message': 'Query router healthy',
                'details': {}
            }
        except Exception as e:
            return {
                'healthy': False,
                'message': f'Query router health check failed: {e}',
                'details': {
                    'error': str(e)
                }
            }

    def _check_memory_health(self) -> Dict[str, Any]:
        """
        Check memory usage health.

        Returns:
            Health check result
        """
        try:
            import psutil

            process = psutil.Process()
            memory_info = process.memory_info()
            rss_mb = memory_info.rss / 1024 / 1024

            # Define memory threshold (500MB)
            threshold_mb = 500
            usage_percent = (rss_mb / threshold_mb) * 100

            if rss_mb > threshold_mb:
                return {
                    'healthy': False,
                    'message': f'Memory usage ({rss_mb:.2f}MB) exceeds threshold ({threshold_mb}MB)',
                    'details': {
                        'rss_mb': rss_mb,
                        'threshold_mb': threshold_mb,
                        'usage_percent': usage_percent
                    }
                }

            return {
                'healthy': True,
                'message': f'Memory usage within limits ({rss_mb:.2f}MB)',
                'details': {
                    'rss_mb': rss_mb,
                    'threshold_mb': threshold_mb,
                    'usage_percent': usage_percent
                }
            }
        except Exception as e:
            return {
                'healthy': False,
                'message': f'Memory health check failed: {e}',
                'details': {
                    'error': str(e)
                }
            }


def log_global_index_operation(
    operation: str,
    component: str,
    status: str = "success",
    duration_ms: float = 0.0,
    **metadata
) -> None:
    """
    Log a global index operation in structured JSON format.

    Args:
        operation: Type of operation (e.g., 'cross_project_search', 'cache_query')
        component: Component performing the operation
        status: Operation status ('success', 'error', 'warning')
        duration_ms: Operation duration in milliseconds
        **metadata: Additional operation-specific metadata

    Example:
        log_global_index_operation(
            operation='cross_project_search',
            component='tier2_cache',
            status='success',
            duration_ms=45.2,
            project_ids=['proj1', 'proj2'],
            result_count=42
        )
    """
    entry = GlobalIndexLogEntry(
        timestamp=datetime.now().isoformat(),
        operation=operation,
        component=component,
        status=status,
        duration_ms=duration_ms,
        metadata=metadata
    )

    # Log at appropriate level based on status
    log_json = entry.to_json()
    if status == 'error':
        logger.error(f"[GLOBAL_INDEX] {log_json}")
    elif status == 'warning':
        logger.warning(f"[GLOBAL_INDEX] {log_json}")
    else:
        logger.info(f"[GLOBAL_INDEX] {log_json}")


# Global instances
_global_index_monitor: Optional[GlobalIndexMonitor] = None
_global_index_health_checker: Optional[GlobalIndexHealthChecker] = None
_lock = threading.Lock()


def get_global_index_monitor() -> GlobalIndexMonitor:
    """
    Get the global global index monitor instance.

    Returns:
        Global index monitor instance
    """
    global _global_index_monitor

    with _lock:
        if _global_index_monitor is None:
            from ..monitoring import get_metrics_registry
            _global_index_monitor = GlobalIndexMonitor(get_metrics_registry())

        return _global_index_monitor


def get_global_index_health_checker() -> GlobalIndexHealthChecker:
    """
    Get the global global index health checker instance.

    Returns:
        Global index health checker instance
    """
    global _global_index_health_checker

    with _lock:
        if _global_index_health_checker is None:
            from ..monitoring import get_health_checker
            _global_index_health_checker = GlobalIndexHealthChecker(get_health_checker())

        return _global_index_health_checker
