"""
Monitoring and Observability Module

This module provides comprehensive monitoring, metrics collection, and
health check capabilities for the LeIndex system.

MONITORING FEATURES:
- Metrics collection (counters, gauges, histograms)
- Performance monitoring (latency, throughput, errors)
- Health check endpoints
- Structured logging integration
- Alerting thresholds

METRICS TYPES:
- Counter: Monotonically increasing value (e.g., requests processed)
- Gauge: Point-in-time value (e.g., current queue size)
- Histogram: Distribution of values (e.g., request latency)
- Summary: Statistics (count, sum, min, max, avg, percentiles)

USAGE EXAMPLE:
    metrics = MetricsRegistry()
    counter = metrics.counter('files_indexed', 'Number of files indexed')
    counter.inc()

    histogram = metrics.histogram('index_latency_ms', 'Indexing latency')
    with histogram.time():
        # Do indexing work
        pass
"""

import time
import threading
from typing import Dict, Any, Optional, Callable, List, Tuple
from dataclasses import dataclass, field
from datetime import datetime, timedelta
from collections import defaultdict, deque
import statistics

from .logger_config import logger


@dataclass
class MetricValue:
    """Represents a single metric measurement."""
    timestamp: float
    value: float
    labels: Dict[str, str] = field(default_factory=dict)


@dataclass
class Counter:
    """
    A counter is a cumulative metric that represents a single monotonically increasing counter.

    Use counters for things like:
    - Number of requests processed
    - Number of errors encountered
    - Number of files indexed
    """
    name: str
    description: str
    _value: float = 0.0
    _created_at: float = field(default_factory=time.time)

    def inc(self, amount: float = 1.0) -> None:
        """Increment the counter by the given amount."""
        if amount < 0:
            raise ValueError("Counter increment must be non-negative")
        self._value += amount

    def dec(self, amount: float = 1.0) -> None:
        """Decrement the counter by the given amount."""
        if amount < 0:
            raise ValueError("Counter decrement must be non-negative")
        self._value -= amount

    def get(self) -> float:
        """Get the current counter value."""
        return self._value

    def reset(self) -> None:
        """Reset the counter to zero."""
        self._value = 0.0


@dataclass
class Gauge:
    """
    A gauge is a metric that represents a single numerical value that can arbitrarily go up and down.

    Use gauges for things like:
    - Current queue size
    - Current memory usage
    - Current number of active connections
    """
    name: str
    description: str
    _value: float = 0.0
    _created_at: float = field(default_factory=time.time)

    def set(self, value: float) -> None:
        """Set the gauge to the given value."""
        self._value = value

    def inc(self, amount: float = 1.0) -> None:
        """Increment the gauge by the given amount."""
        self._value += amount

    def dec(self, amount: float = 1.0) -> None:
        """Decrement the gauge by the given amount."""
        self._value -= amount

    def get(self) -> float:
        """Get the current gauge value."""
        return self._value


@dataclass
class Histogram:
    """
    A histogram samples observations (usually things like request durations or response sizes).

    CONFIGURATION:
    - buckets: Pre-defined buckets for histogram (default: exponential)
    - max_samples: Maximum number of samples to keep (default: 10000)

    STATISTICS:
    - count: Total number of observations
    - sum: Sum of all observations
    - min: Minimum observation
    - max: Maximum observation
    - avg: Average observation
    - p50, p95, p99: Percentiles
    """
    name: str
    description: str
    buckets: List[float] = field(default_factory=lambda: [
        0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0
    ])
    max_samples: int = 10000
    _samples: deque = field(default_factory=lambda: deque(maxlen=10000))
    _count: float = 0.0
    _sum: float = 0.0
    _lock: threading.Lock = field(default_factory=threading.Lock)

    def observe(self, value: float) -> None:
        """Observe a value."""
        with self._lock:
            self._samples.append(value)
            self._count += 1
            self._sum += value

    def time(self) -> Callable:
        """
        Context manager for timing operations.

        Example:
            with histogram.time():
                # Do some work
                pass
        """
        class Timer:
            def __init__(self, histogram: 'Histogram'):
                self.histogram = histogram
                self.start_time = None

            def __enter__(self):
                self.start_time = time.time()
                return self

            def __exit__(self, exc_type, exc_val, exc_tb):
                elapsed = time.time() - self.start_time
                self.histogram.observe(elapsed)

        return Timer(self)

    def get_stats(self) -> Dict[str, Any]:
        """Get histogram statistics."""
        with self._lock:
            if not self._samples:
                return {
                    'count': 0,
                    'sum': 0.0,
                    'min': 0.0,
                    'max': 0.0,
                    'avg': 0.0,
                    'p50': 0.0,
                    'p95': 0.0,
                    'p99': 0.0
                }

            samples_list = list(self._samples)
            return {
                'count': self._count,
                'sum': self._sum,
                'min': min(samples_list),
                'max': max(samples_list),
                'avg': statistics.mean(samples_list),
                'p50': statistics.median(samples_list),
                'p95': statistics.quantiles(samples_list, n=20)[18] if len(samples_list) > 1 else samples_list[0],
                'p99': statistics.quantiles(samples_list, n=100)[98] if len(samples_list) > 1 else samples_list[0]
            }


class MetricsRegistry:
    """
    Central registry for all metrics.

    THREAD SAFETY: All operations are thread-safe via locks.

    USAGE:
        registry = MetricsRegistry()

        # Create metrics
        counter = registry.counter('files_indexed', 'Number of files indexed')
        gauge = registry.gauge('queue_size', 'Current queue size')
        histogram = registry.histogram('index_latency_ms', 'Indexing latency')

        # Use metrics
        counter.inc()
        gauge.set(10)
        with histogram.time():
            # Do work
            pass

        # Get all metrics
        all_metrics = registry.get_all_metrics()
    """

    def __init__(self):
        """Initialize the metrics registry."""
        self._counters: Dict[str, Counter] = {}
        self._gauges: Dict[str, Gauge] = {}
        self._histograms: Dict[str, Histogram] = {}
        self._lock = threading.Lock()

    def counter(self, name: str, description: str) -> Counter:
        """Get or create a counter metric."""
        with self._lock:
            if name not in self._counters:
                self._counters[name] = Counter(name=name, description=description)
            return self._counters[name]

    def gauge(self, name: str, description: str) -> Gauge:
        """Get or create a gauge metric."""
        with self._lock:
            if name not in self._gauges:
                self._gauges[name] = Gauge(name=name, description=description)
            return self._gauges[name]

    def histogram(self, name: str, description: str) -> Histogram:
        """Get or create a histogram metric."""
        with self._lock:
            if name not in self._histograms:
                self._histograms[name] = Histogram(name=name, description=description)
            return self._histograms[name]

    def get_all_metrics(self) -> Dict[str, Any]:
        """Get all metrics as a dictionary."""
        with self._lock:
            metrics = {}

            for name, counter in self._counters.items():
                metrics[name] = {
                    'type': 'counter',
                    'description': counter.description,
                    'value': counter.get()
                }

            for name, gauge in self._gauges.items():
                metrics[name] = {
                    'type': 'gauge',
                    'description': gauge.description,
                    'value': gauge.get()
                }

            for name, histogram in self._histograms.items():
                metrics[name] = {
                    'type': 'histogram',
                    'description': histogram.description,
                    **histogram.get_stats()
                }

            return metrics

    def reset_all(self) -> None:
        """Reset all metrics (mainly for testing)."""
        with self._lock:
            for counter in self._counters.values():
                counter.reset()
            for gauge in self._gauges.values():
                gauge.set(0)
            # Histograms don't support reset


class HealthChecker:
    """
    Health check system for monitoring system health.

    HEALTH CHECKS:
    - Database connectivity
    - File system accessibility
    - Memory usage
    - Cache health
    - Queue status

    Each health check returns:
    - healthy: True/False
    - message: Human-readable status
    - timestamp: When the check was performed
    - details: Additional diagnostic information
    """

    def __init__(self):
        """Initialize the health checker."""
        self._checks: Dict[str, Callable] = {}
        self._results: Dict[str, Dict[str, Any]] = {}
        self._last_check: float = 0
        self._check_interval: float = 30.0  # Seconds
        self._lock = threading.Lock()

    def register_check(self, name: str, check_func: Callable[[], Dict[str, Any]]) -> None:
        """
        Register a health check function.

        Args:
            name: Name of the health check
            check_func: Function that returns a dict with keys:
                - healthy (bool): Whether the check passed
                - message (str): Human-readable status
                - details (dict, optional): Additional information

        Example:
            def check_database():
                try:
                    # Try to connect
                    db.connect()
                    return {'healthy': True, 'message': 'Database OK'}
                except Exception as e:
                    return {'healthy': False, 'message': f'Database error: {e}'}

            health_checker.register_check('database', check_database)
        """
        with self._lock:
            self._checks[name] = check_func

    def run_checks(self, force: bool = False) -> Dict[str, Any]:
        """
        Run all registered health checks.

        Args:
            force: Force checks to run even if within check interval

        Returns:
            Dictionary with overall health status and individual check results
        """
        current_time = time.time()

        with self._lock:
            # Check if we should skip (unless forced)
            if not force and (current_time - self._last_check) < self._check_interval:
                return self._results

            self._last_check = current_time
            overall_healthy = True
            check_results = {}

            # Run each check
            for name, check_func in self._checks.items():
                try:
                    result = check_func()
                    result['timestamp'] = current_time
                    check_results[name] = result

                    if not result.get('healthy', False):
                        overall_healthy = False

                except Exception as e:
                    check_results[name] = {
                        'healthy': False,
                        'message': f'Check failed with exception: {e}',
                        'timestamp': current_time
                    }
                    overall_healthy = False

            self._results = {
                'healthy': overall_healthy,
                'timestamp': current_time,
                'checks': check_results
            }

            return self._results

    def is_healthy(self) -> bool:
        """Quick check if overall system is healthy."""
        result = self.run_checks()
        return result.get('healthy', True)


class PerformanceMonitor:
    """
    Monitor performance metrics for the indexing system.

    TRACKED METRICS:
    - Indexing throughput (files/second)
    - Average indexing latency
    - Cache hit rate
    - Error rate
    - Memory usage
    """

    def __init__(self, metrics_registry: MetricsRegistry):
        """
        Initialize the performance monitor.

        Args:
            metrics_registry: Metrics registry to record metrics to
        """
        self.metrics = metrics_registry
        self._start_time = time.time()

        # Create metrics
        self.files_indexed = self.metrics.counter(
            'performance_files_indexed',
            'Total number of files indexed'
        )
        self.index_errors = self.metrics.counter(
            'performance_index_errors',
            'Total number of indexing errors'
        )
        self.cache_hits = self.metrics.counter(
            'performance_cache_hits',
            'Total number of cache hits'
        )
        self.cache_misses = self.metrics.counter(
            'performance_cache_misses',
            'Total number of cache misses'
        )
        self.index_latency = self.metrics.histogram(
            'performance_index_latency_seconds',
            'Time taken to index a file'
        )
        self.queue_size = self.metrics.gauge(
            'performance_queue_size',
            'Current number of items in the indexing queue'
        )
        self.memory_usage_mb = self.metrics.gauge(
            'performance_memory_usage_mb',
            'Current memory usage in MB'
        )

    def record_index(self, latency_seconds: float, success: bool = True) -> None:
        """
        Record an indexing operation.

        Args:
            latency_seconds: Time taken for the operation
            success: Whether the operation succeeded
        """
        self.index_latency.observe(latency_seconds)

        if success:
            self.files_indexed.inc()
        else:
            self.index_errors.inc()

    def get_cache_hit_rate(self) -> float:
        """Calculate cache hit rate as percentage."""
        hits = self.cache_hits.get()
        misses = self.cache_misses.get()
        total = hits + misses

        if total == 0:
            return 0.0

        return (hits / total) * 100

    def get_throughput(self) -> float:
        """
        Calculate indexing throughput (files/second).

        Returns:
            Files indexed per second since start
        """
        elapsed = time.time() - self._start_time
        if elapsed == 0:
            return 0.0

        return self.files_indexed.get() / elapsed

    def get_summary(self) -> Dict[str, Any]:
        """Get a summary of performance metrics."""
        return {
            'uptime_seconds': time.time() - self._start_time,
            'files_indexed': self.files_indexed.get(),
            'index_errors': self.index_errors.get(),
            'cache_hit_rate': f"{self.get_cache_hit_rate():.2f}%",
            'throughput_files_per_sec': f"{self.get_throughput():.2f}",
            'avg_latency_seconds': self._calculate_average_latency(),
            'error_rate': f"{self._calculate_error_rate():.2f}%"
        }

    def _calculate_average_latency(self) -> float:
        """Calculate average indexing latency."""
        stats = self.index_latency.get_stats()
        return stats['avg']

    def _calculate_error_rate(self) -> float:
        """Calculate error rate as percentage."""
        total = self.files_indexed.get() + self.index_errors.get()
        if total == 0:
            return 0.0

        return (self.index_errors.get() / total) * 100


# Global instances for easy access
_global_metrics = MetricsRegistry()
_global_health_checker = HealthChecker()
_global_performance_monitor = PerformanceMonitor(_global_metrics)


def get_metrics_registry() -> MetricsRegistry:
    """Get the global metrics registry."""
    return _global_metrics


def get_health_checker() -> HealthChecker:
    """Get the global health checker."""
    return _global_health_checker


def get_performance_monitor() -> PerformanceMonitor:
    """Get the global performance monitor."""
    return _global_performance_monitor
