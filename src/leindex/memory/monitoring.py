"""
Memory Monitoring System for LeIndex

This module provides comprehensive monitoring and metrics for memory management
operations. It implements structured JSON logging, metrics emission, health checks,
error categorization, and periodic profiling snapshots.

Key Features:
- Structured JSON logging for all memory operations
- Real-time metrics emission (memory_rss_mb, memory_usage_percent, eviction_count)
- Health check system for memory manager
- Categorized error handling (memory_error, threshold_error, eviction_error)
- Periodic memory profiling snapshots (default: every 30 seconds)
- MCP tool integration for metrics exposure
- Thread-safe circular buffer for snapshot storage
- Integration with existing tracker and eviction modules

Example:
    >>> from leindex.memory.monitoring import MemoryMonitor, get_monitor
    >>> monitor = get_monitor()
    >>> metrics = await monitor.get_metrics()
    >>> health = await monitor.health_check()
    >>> print(f"Status: {health['status']}")
"""

import gc
import json
import time
import asyncio
import threading
from typing import Optional, Dict, Any, List, Callable
from dataclasses import dataclass, field, asdict
from collections import deque
from threading import Lock, Event
from enum import Enum
import logging

from .tracker import MemoryTracker, get_global_tracker
from .eviction import EvictionManager, get_global_manager
from .status import MemoryStatus, MemoryBreakdown
from ..logger_config import logger


# =============================================================================
# Error Categories
# =============================================================================

class MemoryError(Exception):
    """Base exception for memory-related errors.

    All memory-related exceptions inherit from this base class,
    allowing for broad exception catching when needed.
    """
    pass


class ThresholdError(MemoryError):
    """Exception raised when memory thresholds are exceeded.

    This is raised when memory usage crosses warning, prompt,
    or emergency thresholds.
    """

    def __init__(self, message: str, threshold_type: str, current_mb: float, threshold_mb: float):
        """Initialize threshold error.

        Args:
            message: Error message
            threshold_type: Type of threshold (warning/prompt/emergency)
            current_mb: Current memory usage in MB
            threshold_mb: Threshold value in MB
        """
        super().__init__(message)
        self.threshold_type = threshold_type
        self.current_mb = current_mb
        self.threshold_mb = threshold_mb


class EvictionError(MemoryError):
    """Exception raised when eviction operations fail.

    This is raised when project eviction fails or doesn't free
    the expected amount of memory.
    """

    def __init__(self, message: str, target_mb: float, freed_mb: float, errors: List[str]):
        """Initialize eviction error.

        Args:
            message: Error message
            target_mb: Target memory to free in MB
            freed_mb: Actual memory freed in MB
            errors: List of error messages
        """
        super().__init__(message)
        self.target_mb = target_mb
        self.freed_mb = freed_mb
        self.errors = errors


class MonitoringError(MemoryError):
    """Exception raised when monitoring operations fail.

    This is raised when snapshot creation, metrics collection,
    or health checks fail.
    """
    pass


# =============================================================================
# Health Status Enum
# =============================================================================

class HealthStatus(Enum):
    """Health status levels for memory manager."""
    HEALTHY = "healthy"
    WARNING = "warning"
    CRITICAL = "critical"


# =============================================================================
# Memory Snapshot Data Class
# =============================================================================

@dataclass
class MemorySnapshot:
    """A snapshot of memory usage at a point in time.

    Attributes:
        timestamp: Unix timestamp when snapshot was taken
        rss_mb: Current RSS memory in MB
        heap_objects: Number of objects in Python heap
        usage_percent: Memory usage as percentage of budget
        status: Memory status level
        growth_rate_mb_per_sec: Memory growth rate
        eviction_count: Total number of evictions performed
        metadata: Additional metadata
    """
    timestamp: float
    rss_mb: float
    heap_objects: int
    usage_percent: float
    status: str
    growth_rate_mb_per_sec: float
    eviction_count: int = 0
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert snapshot to dictionary.

        Returns:
            Dictionary representation of the snapshot
        """
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "MemorySnapshot":
        """Create snapshot from dictionary.

        Args:
            data: Dictionary representation of snapshot

        Returns:
            MemorySnapshot instance
        """
        return cls(**data)


# =============================================================================
# Structured Logger
# =============================================================================

class StructuredLogger:
    """Structured JSON logger for memory operations.

    This class provides structured logging with consistent field naming
    and JSON formatting for all memory-related events.
    """

    def __init__(self, component: str = "memory_monitor"):
        """Initialize structured logger.

        Args:
            component: Component name for log entries
        """
        self.component = component
        self.logger = logger

    def log_memory_event(
        self,
        event_type: str,
        level: str = "info",
        **kwargs
    ) -> None:
        """Log a memory event with structured JSON format.

        Args:
            event_type: Type of event (e.g., "threshold_exceeded", "eviction_triggered")
            level: Log level (debug/info/warning/error/critical)
            **kwargs: Additional event-specific fields
        """
        log_entry = {
            "timestamp": time.time(),
            "component": self.component,
            "event_type": event_type,
            **kwargs
        }

        # Log at appropriate level
        log_func = getattr(self.logger, level.lower(), self.logger.info)
        log_func(f"memory_event: {json.dumps(log_entry)}", extra={"structured": log_entry})

    def log_threshold_crossing(
        self,
        threshold_type: str,
        current_mb: float,
        threshold_mb: float,
        usage_percent: float
    ) -> None:
        """Log a threshold crossing event.

        Args:
            threshold_type: Type of threshold crossed
            current_mb: Current memory usage
            threshold_mb: Threshold value
            usage_percent: Usage percentage
        """
        self.log_memory_event(
            "threshold_crossed",
            level="warning",
            threshold_type=threshold_type,
            current_mb=current_mb,
            threshold_mb=threshold_mb,
            usage_percent=usage_percent
        )

    def log_eviction_event(
        self,
        projects_evicted: List[str],
        memory_freed_mb: float,
        target_mb: float,
        duration_seconds: float
    ) -> None:
        """Log an eviction event.

        Args:
            projects_evicted: List of evicted project IDs
            memory_freed_mb: Memory freed in MB
            target_mb: Target memory to free in MB
            duration_seconds: Duration of eviction
        """
        self.log_memory_event(
            "eviction_completed",
            level="info",
            projects_count=len(projects_evicted),
            projects_evicted=projects_evicted,
            memory_freed_mb=memory_freed_mb,
            target_mb=target_mb,
            duration_seconds=duration_seconds
        )

    def log_error(
        self,
        error_type: str,
        error_message: str,
        **kwargs
    ) -> None:
        """Log an error event.

        Args:
            error_type: Type of error (memory_error/threshold_error/eviction_error)
            error_message: Error message
            **kwargs: Additional error context
        """
        self.log_memory_event(
            "error",
            level="error",
            error_type=error_type,
            error_message=error_message,
            **kwargs
        )


# =============================================================================
# Memory Metrics Collector
# =============================================================================

@dataclass
class MemoryMetrics:
    """Real-time memory metrics.

    Attributes:
        timestamp: Unix timestamp when metrics were collected
        memory_rss_mb: Current RSS memory in MB
        memory_usage_percent: Memory usage as percentage of budget
        eviction_count: Total number of evictions performed
        memory_freed_total_mb: Total memory freed by evictions
        growth_rate_mb_per_sec: Current memory growth rate
        status: Current memory status
        threshold_crossings: Count of threshold crossings by type
    """
    timestamp: float
    memory_rss_mb: float
    memory_usage_percent: float
    eviction_count: int
    memory_freed_total_mb: float
    growth_rate_mb_per_sec: float
    status: str
    threshold_crossings: Dict[str, int] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert metrics to dictionary.

        Returns:
            Dictionary representation of metrics
        """
        return asdict(self)


class MemoryMetricsCollector:
    """Collector for real-time memory metrics.

    This class collects and aggregates metrics from the memory tracker
    and eviction manager.
    """

    def __init__(
        self,
        tracker: Optional[MemoryTracker] = None,
        eviction_manager: Optional[EvictionManager] = None
    ):
        """Initialize metrics collector.

        Args:
            tracker: Memory tracker instance (uses global if None)
            eviction_manager: Eviction manager instance (uses global if None)
        """
        self._tracker = tracker or get_global_tracker()
        self._eviction_manager = eviction_manager or get_global_manager()
        self._structured_logger = StructuredLogger(component="metrics_collector")

        # Threshold crossing tracking
        self._threshold_crossings: Dict[str, int] = {
            "warning": 0,
            "prompt": 0,
            "emergency": 0,
        }
        self._crossings_lock = Lock()

        # Last known status to detect crossings
        self._last_status: Optional[str] = None

    async def collect_metrics(self) -> MemoryMetrics:
        """Collect current memory metrics.

        Returns:
            MemoryMetrics with current values
        """
        try:
            # Get current memory status
            memory_status = self._tracker.check_memory_budget()

            # Get eviction statistics
            eviction_stats = self._eviction_manager.get_statistics()

            # Detect threshold crossings
            current_status = memory_status.status
            self._detect_threshold_crossing(current_status, memory_status)

            # Create metrics
            metrics = MemoryMetrics(
                timestamp=time.time(),
                memory_rss_mb=memory_status.current_mb,
                memory_usage_percent=memory_status.usage_percent,
                eviction_count=eviction_stats.get("total_evictions", 0),
                memory_freed_total_mb=eviction_stats.get("total_memory_freed_mb", 0.0),
                growth_rate_mb_per_sec=memory_status.growth_rate_mb_per_sec,
                status=current_status,
                threshold_crossings=self._get_threshold_crossings(),
            )

            return metrics

        except Exception as e:
            self._structured_logger.log_error(
                "monitoring_error",
                f"Failed to collect metrics: {e}"
            )
            raise MonitoringError(f"Metrics collection failed: {e}")

    def _detect_threshold_crossing(self, current_status: str, memory_status: MemoryStatus) -> None:
        """Detect and log threshold crossings.

        Args:
            current_status: Current memory status
            memory_status: Memory status details
        """
        with self._crossings_lock:
            # Check if this is a new status (crossing)
            if self._last_status and self._last_status != current_status:
                # Determine which threshold was crossed
                if current_status == "warning":
                    self._threshold_crossings["prompt"] += 1
                    self._structured_logger.log_threshold_crossing(
                        "prompt",
                        memory_status.current_mb,
                        memory_status.prompt_threshold_mb,
                        memory_status.usage_percent
                    )
                elif current_status == "critical":
                    self._threshold_crossings["emergency"] += 1
                    self._structured_logger.log_threshold_crossing(
                        "emergency",
                        memory_status.current_mb,
                        memory_status.hard_limit_mb,
                        memory_status.usage_percent
                    )
                elif current_status == "caution" and self._last_status == "healthy":
                    self._threshold_crossings["warning"] += 1
                    self._structured_logger.log_threshold_crossing(
                        "warning",
                        memory_status.current_mb,
                        memory_status.soft_limit_mb,
                        memory_status.usage_percent
                    )

            self._last_status = current_status

    def _get_threshold_crossings(self) -> Dict[str, int]:
        """Get thread-safe copy of threshold crossings.

        Returns:
            Dictionary of threshold crossing counts
        """
        with self._crossings_lock:
            return self._threshold_crossings.copy()

    def reset_crossings(self) -> None:
        """Reset threshold crossing counters."""
        with self._crossings_lock:
            for key in self._threshold_crossings:
                self._threshold_crossings[key] = 0


# =============================================================================
# Memory Profiler
# =============================================================================

class MemoryProfiler:
    """Periodic memory profiling with snapshot collection.

    This class takes snapshots of memory usage at regular intervals
    and stores them in a circular buffer for efficient storage.
    """

    def __init__(
        self,
        tracker: Optional[MemoryTracker] = None,
        interval_seconds: float = 30.0,
        max_snapshots: int = 2880  # 24 hours at 30s intervals
    ):
        """Initialize memory profiler.

        Args:
            tracker: Memory tracker instance (uses global if None)
            interval_seconds: Snapshot interval in seconds (default: 30s)
            max_snapshots: Maximum snapshots to keep (default: 2880 for 24h)
        """
        self._tracker = tracker or get_global_tracker()
        self._interval_seconds = interval_seconds
        self._max_snapshots = max_snapshots

        # Circular buffer for snapshots (thread-safe)
        self._snapshots: deque = deque(maxlen=max_snapshots)
        self._snapshots_lock = Lock()

        # Background profiling
        self._profiling = False
        self._profile_task: Optional[asyncio.Task] = None
        self._shutdown_event = Event()

        # Statistics
        self._total_snapshots = 0
        self._structured_logger = StructuredLogger(component="memory_profiler")

    async def start_profiling(self) -> None:
        """Start background profiling thread."""
        if self._profiling:
            logger.warning("Profiling already active")
            return

        self._profiling = True
        self._shutdown_event.clear()

        logger.info(
            f"Starting memory profiling: interval={self._interval_seconds}s, "
            f"max_snapshots={self._max_snapshots}"
        )

        # Run profiling loop
        await self._profiling_loop()

    def start_profiling_sync(self) -> None:
        """Start profiling in a background thread (synchronous version)."""
        if self._profiling:
            logger.warning("Profiling already active")
            return

        self._profiling = True
        self._shutdown_event.clear()

        def profile_loop():
            """Synchronous profiling loop."""
            logger.info(
                f"Starting memory profiling (thread): interval={self._interval_seconds}s"
            )

            while not self._shutdown_event.is_set():
                try:
                    # Take snapshot
                    snapshot = self._take_snapshot()

                    # Store snapshot
                    with self._snapshots_lock:
                        self._snapshots.append(snapshot)
                        self._total_snapshots += 1

                    # Log snapshot event
                    self._structured_logger.log_memory_event(
                        "snapshot_taken",
                        level="debug",
                        snapshot_count=len(self._snapshots),
                        rss_mb=snapshot.rss_mb,
                        usage_percent=snapshot.usage_percent
                    )

                except Exception as e:
                    self._structured_logger.log_error(
                        "monitoring_error",
                        f"Failed to take snapshot: {e}"
                    )

                # Wait for next interval or shutdown
                self._shutdown_event.wait(timeout=self._interval_seconds)

            logger.info("Memory profiling stopped")

        # Start daemon thread
        profile_thread = threading.Thread(target=profile_loop, daemon=True)
        profile_thread.start()

        logger.info("Memory profiling thread started")

    def _take_snapshot(self) -> MemorySnapshot:
        """Take a memory snapshot.

        Returns:
            MemorySnapshot with current memory state
        """
        try:
            # Get current memory status
            memory_status = self._tracker.check_memory_budget()

            # Get heap object count
            try:
                heap_objects = len(gc.get_objects())
            except Exception:
                heap_objects = 0

            # Get eviction count
            from .eviction import get_global_manager
            eviction_manager = get_global_manager()
            eviction_stats = eviction_manager.get_statistics()
            eviction_count = eviction_stats.get("total_evictions", 0)

            # Create snapshot
            snapshot = MemorySnapshot(
                timestamp=time.time(),
                rss_mb=memory_status.current_mb,
                heap_objects=heap_objects,
                usage_percent=memory_status.usage_percent,
                status=memory_status.status,
                growth_rate_mb_per_sec=memory_status.growth_rate_mb_per_sec,
                eviction_count=eviction_count,
                metadata={
                    "soft_limit_mb": memory_status.soft_limit_mb,
                    "hard_limit_mb": memory_status.hard_limit_mb,
                }
            )

            return snapshot

        except Exception as e:
            self._structured_logger.log_error(
                "monitoring_error",
                f"Failed to take snapshot: {e}"
            )
            # Return minimal snapshot on error
            return MemorySnapshot(
                timestamp=time.time(),
                rss_mb=0.0,
                heap_objects=0,
                usage_percent=0.0,
                status="unknown",
                growth_rate_mb_per_sec=0.0,
            )

    async def _profiling_loop(self) -> None:
        """Async profiling loop."""
        while self._profiling and not self._shutdown_event.is_set():
            try:
                # Take snapshot
                snapshot = self._take_snapshot()

                # Store snapshot
                with self._snapshots_lock:
                    self._snapshots.append(snapshot)
                    self._total_snapshots += 1

                # Log snapshot event
                self._structured_logger.log_memory_event(
                    "snapshot_taken",
                    level="debug",
                    snapshot_count=len(self._snapshots),
                    rss_mb=snapshot.rss_mb,
                    usage_percent=snapshot.usage_percent
                )

            except Exception as e:
                self._structured_logger.log_error(
                    "monitoring_error",
                    f"Failed to take snapshot: {e}"
                )

            # Wait for next interval
            await asyncio.sleep(self._interval_seconds)

    def stop_profiling(self) -> None:
        """Stop background profiling."""
        if not self._profiling:
            return

        logger.info("Stopping memory profiling...")
        self._profiling = False
        self._shutdown_event.set()

        logger.info("Memory profiling stopped")

    def get_snapshots(
        self,
        max_snapshots: Optional[int] = None
    ) -> List[MemorySnapshot]:
        """Get memory snapshots.

        Args:
            max_snapshots: Maximum number of snapshots to return (None = all)

        Returns:
            List of memory snapshots
        """
        with self._snapshots_lock:
            snapshots_list = list(self._snapshots)

        if max_snapshots is not None:
            snapshots_list = snapshots_list[-max_snapshots:]

        return snapshots_list

    def get_latest_snapshot(self) -> Optional[MemorySnapshot]:
        """Get the most recent snapshot.

        Returns:
            Latest snapshot or None if no snapshots available
        """
        with self._snapshots_lock:
            if self._snapshots:
                return self._snapshots[-1]
            return None

    def get_statistics(self) -> Dict[str, Any]:
        """Get profiler statistics.

        Returns:
            Dictionary with profiler statistics
        """
        with self._snapshots_lock:
            snapshots = list(self._snapshots)

        if not snapshots:
            return {
                "total_snapshots_taken": self._total_snapshots,
                "current_snapshot_count": 0,
                "profiling_active": self._profiling,
            }

        # Calculate statistics
        rss_values = [s.rss_mb for s in snapshots]
        usage_values = [s.usage_percent for s in snapshots]

        return {
            "total_snapshots_taken": self._total_snapshots,
            "current_snapshot_count": len(snapshots),
            "profiling_active": self._profiling,
            "interval_seconds": self._interval_seconds,
            "max_snapshots": self._max_snapshots,
            "rss_mb": {
                "min": min(rss_values),
                "max": max(rss_values),
                "avg": sum(rss_values) / len(rss_values),
                "current": snapshots[-1].rss_mb,
            },
            "usage_percent": {
                "min": min(usage_values),
                "max": max(usage_values),
                "avg": sum(usage_values) / len(usage_values),
                "current": snapshots[-1].usage_percent,
            },
            "oldest_snapshot_time": snapshots[0].timestamp,
            "newest_snapshot_time": snapshots[-1].timestamp,
        }


# =============================================================================
# Memory Health Checker
# =============================================================================

class MemoryHealthChecker:
    """Health check system for memory manager.

    This class performs comprehensive health checks on the memory
    management system.
    """

    def __init__(
        self,
        tracker: Optional[MemoryTracker] = None,
        eviction_manager: Optional[EvictionManager] = None
    ):
        """Initialize health checker.

        Args:
            tracker: Memory tracker instance (uses global if None)
            eviction_manager: Eviction manager instance (uses global if None)
        """
        self._tracker = tracker or get_global_tracker()
        self._eviction_manager = eviction_manager or get_global_manager()
        self._structured_logger = StructuredLogger(component="health_checker")

    async def health_check(self) -> Dict[str, Any]:
        """Perform comprehensive health check.

        Returns:
            Dictionary with health check results
        """
        try:
            # Get current memory status
            memory_status = self._tracker.check_memory_budget()

            # Determine overall health status
            if memory_status.status == "critical":
                overall_status = HealthStatus.CRITICAL
            elif memory_status.status in ("warning", "caution"):
                overall_status = HealthStatus.WARNING
            else:
                overall_status = HealthStatus.HEALTHY

            # Perform individual checks
            checks = {
                "memory_usage": self._check_memory_usage(memory_status),
                "memory_growth": self._check_memory_growth(memory_status),
                "eviction_system": self._check_eviction_system(),
                "tracker_status": self._check_tracker_status(),
            }

            # Count failed checks
            failed_checks = sum(1 for check in checks.values() if not check["healthy"])

            # Determine if any critical issues
            critical_issues = [
                check for check in checks.values()
                if not check["healthy"] and check.get("severity") == "critical"
            ]

            health_result = {
                "status": overall_status.value,
                "timestamp": time.time(),
                "checks": checks,
                "failed_checks": failed_checks,
                "critical_issues": len(critical_issues),
                "memory_status": memory_status.to_dict(),
            }

            # Log health check event
            self._structured_logger.log_memory_event(
                "health_check",
                level="info" if overall_status == HealthStatus.HEALTHY else "warning",
                status=overall_status.value,
                failed_checks=failed_checks,
                critical_issues=len(critical_issues),
            )

            return health_result

        except Exception as e:
            self._structured_logger.log_error(
                "monitoring_error",
                f"Health check failed: {e}"
            )
            raise MonitoringError(f"Health check failed: {e}")

    def _check_memory_usage(self, memory_status: MemoryStatus) -> Dict[str, Any]:
        """Check memory usage health.

        Args:
            memory_status: Current memory status

        Returns:
            Check result dictionary
        """
        if memory_status.is_critical():
            return {
                "healthy": False,
                "severity": "critical",
                "message": "Memory usage at critical level",
                "current_mb": memory_status.current_mb,
                "hard_limit_mb": memory_status.hard_limit_mb,
                "usage_percent": memory_status.usage_percent,
            }
        elif memory_status.is_warning():
            return {
                "healthy": False,
                "severity": "warning",
                "message": "Memory usage elevated",
                "current_mb": memory_status.current_mb,
                "prompt_threshold_mb": memory_status.prompt_threshold_mb,
                "usage_percent": memory_status.usage_percent,
            }
        else:
            return {
                "healthy": True,
                "message": "Memory usage within acceptable limits",
                "current_mb": memory_status.current_mb,
                "usage_percent": memory_status.usage_percent,
            }

    def _check_memory_growth(self, memory_status: MemoryStatus) -> Dict[str, Any]:
        """Check memory growth rate health.

        Args:
            memory_status: Current memory status

        Returns:
            Check result dictionary
        """
        growth_rate = memory_status.growth_rate_mb_per_sec

        # Growth rate thresholds (MB per second)
        critical_growth = 10.0  # > 10 MB/s is critical
        warning_growth = 5.0   # > 5 MB/s is warning

        if growth_rate > critical_growth:
            return {
                "healthy": False,
                "severity": "critical",
                "message": f"Memory growing very rapidly: {growth_rate:.2f} MB/s",
                "growth_rate_mb_per_sec": growth_rate,
            }
        elif growth_rate > warning_growth:
            return {
                "healthy": False,
                "severity": "warning",
                "message": f"Memory growing rapidly: {growth_rate:.2f} MB/s",
                "growth_rate_mb_per_sec": growth_rate,
            }
        else:
            return {
                "healthy": True,
                "message": f"Memory growth rate acceptable: {growth_rate:.2f} MB/s",
                "growth_rate_mb_per_sec": growth_rate,
            }

    def _check_eviction_system(self) -> Dict[str, Any]:
        """Check eviction system health.

        Returns:
            Check result dictionary
        """
        try:
            stats = self._eviction_manager.get_statistics()

            # Check if eviction system has been used
            total_evictions = stats.get("total_evictions", 0)

            if total_evictions > 0:
                # Calculate average memory freed per eviction
                total_freed = stats.get("total_memory_freed_mb", 0.0)
                avg_freed = total_freed / total_evictions if total_evictions > 0 else 0.0

                return {
                    "healthy": True,
                    "message": "Eviction system operational",
                    "total_evictions": total_evictions,
                    "total_memory_freed_mb": total_freed,
                    "avg_memory_freed_mb": avg_freed,
                }
            else:
                return {
                    "healthy": True,
                    "message": "Eviction system ready (no evictions performed)",
                    "total_evictions": 0,
                }

        except Exception as e:
            return {
                "healthy": False,
                "severity": "warning",
                "message": f"Eviction system check failed: {e}",
                "error": str(e),
            }

    def _check_tracker_status(self) -> Dict[str, Any]:
        """Check memory tracker status.

        Returns:
            Check result dictionary
        """
        try:
            stats = self._tracker.get_stats()

            monitoring_active = stats.get("monitoring_active", False)

            return {
                "healthy": True,
                "message": "Memory tracker operational",
                "monitoring_active": monitoring_active,
                "current_mb": stats.get("current_mb", 0.0),
                "history_entries": stats.get("history_entries", 0),
            }

        except Exception as e:
            return {
                "healthy": False,
                "severity": "warning",
                "message": f"Memory tracker check failed: {e}",
                "error": str(e),
            }


# =============================================================================
# Main Memory Monitor
# =============================================================================

class MemoryMonitor:
    """Main memory monitoring system.

    This class integrates all monitoring components:
    - Structured logging
    - Metrics collection
    - Health checking
    - Profiling snapshots

    Example:
        >>> monitor = MemoryMonitor()
        >>> await monitor.start()
        >>> metrics = await monitor.get_metrics()
        >>> health = await monitor.health_check()
        >>> await monitor.stop()
    """

    def __init__(
        self,
        tracker: Optional[MemoryTracker] = None,
        eviction_manager: Optional[EvictionManager] = None,
        profiling_interval_seconds: float = 30.0,
        max_snapshots: int = 2880
    ):
        """Initialize memory monitor.

        Args:
            tracker: Memory tracker instance (uses global if None)
            eviction_manager: Eviction manager instance (uses global if None)
            profiling_interval_seconds: Snapshot interval in seconds (default: 30s)
            max_snapshots: Maximum snapshots to keep (default: 2880 for 24h)
        """
        self._tracker = tracker or get_global_tracker()
        self._eviction_manager = eviction_manager or get_global_manager()

        # Initialize components
        self._structured_logger = StructuredLogger(component="memory_monitor")
        self._metrics_collector = MemoryMetricsCollector(self._tracker, self._eviction_manager)
        self._profiler = MemoryProfiler(
            self._tracker,
            profiling_interval_seconds,
            max_snapshots
        )
        self._health_checker = MemoryHealthChecker(self._tracker, self._eviction_manager)

        # Monitoring state
        self._running = False

    async def start(self) -> None:
        """Start monitoring system."""
        if self._running:
            logger.warning("Memory monitor already running")
            return

        logger.info("Starting memory monitor...")

        # Start profiling
        self._profiler.start_profiling_sync()

        self._running = True

        self._structured_logger.log_memory_event(
            "monitor_started",
            level="info",
            profiling_interval_seconds=self._profiler._interval_seconds
        )

        logger.info("Memory monitor started")

    def start_sync(self) -> None:
        """Start monitoring system (synchronous version)."""
        if self._running:
            logger.warning("Memory monitor already running")
            return

        logger.info("Starting memory monitor...")

        # Start profiling
        self._profiler.start_profiling_sync()

        self._running = True

        self._structured_logger.log_memory_event(
            "monitor_started",
            level="info",
            profiling_interval_seconds=self._profiler._interval_seconds
        )

        logger.info("Memory monitor started")

    async def stop(self) -> None:
        """Stop monitoring system."""
        if not self._running:
            return

        logger.info("Stopping memory monitor...")

        # Stop profiling
        self._profiler.stop_profiling()

        self._running = False

        self._structured_logger.log_memory_event(
            "monitor_stopped",
            level="info"
        )

        logger.info("Memory monitor stopped")

    def stop_sync(self) -> None:
        """Stop monitoring system (synchronous version)."""
        if not self._running:
            return

        logger.info("Stopping memory monitor...")

        # Stop profiling
        self._profiler.stop_profiling()

        self._running = False

        self._structured_logger.log_memory_event(
            "monitor_stopped",
            level="info"
        )

        logger.info("Memory monitor stopped")

    async def get_metrics(self) -> Dict[str, Any]:
        """Get current memory metrics.

        Returns:
            Dictionary with current metrics
        """
        try:
            # Collect metrics
            metrics = await self._metrics_collector.collect_metrics()

            # Add profiler statistics
            profiler_stats = self._profiler.get_statistics()

            return {
                "metrics": metrics.to_dict(),
                "profiler": profiler_stats,
                "monitor_running": self._running,
            }

        except Exception as e:
            self._structured_logger.log_error(
                "monitoring_error",
                f"Failed to get metrics: {e}"
            )
            raise MonitoringError(f"Failed to get metrics: {e}")

    def get_metrics_sync(self) -> Dict[str, Any]:
        """Get current memory metrics (synchronous version).

        Returns:
            Dictionary with current metrics
        """
        try:
            # Get memory status
            memory_status = self._tracker.check_memory_budget()

            # Get eviction statistics
            eviction_stats = self._eviction_manager.get_statistics()

            # Get profiler statistics
            profiler_stats = self._profiler.get_statistics()

            # Create metrics
            metrics = {
                "timestamp": time.time(),
                "memory_rss_mb": memory_status.current_mb,
                "memory_usage_percent": memory_status.usage_percent,
                "eviction_count": eviction_stats.get("total_evictions", 0),
                "memory_freed_total_mb": eviction_stats.get("total_memory_freed_mb", 0.0),
                "growth_rate_mb_per_sec": memory_status.growth_rate_mb_per_sec,
                "status": memory_status.status,
                "threshold_crossings": self._metrics_collector._get_threshold_crossings(),
            }

            return {
                "metrics": metrics,
                "profiler": profiler_stats,
                "monitor_running": self._running,
            }

        except Exception as e:
            self._structured_logger.log_error(
                "monitoring_error",
                f"Failed to get metrics: {e}"
            )
            raise MonitoringError(f"Failed to get metrics: {e}")

    async def health_check(self) -> Dict[str, Any]:
        """Perform health check.

        Returns:
            Dictionary with health check results
        """
        return await self._health_checker.health_check()

    def health_check_sync(self) -> Dict[str, Any]:
        """Perform health check (synchronous version).

        Returns:
            Dictionary with health check results
        """
        # Use sync version of internal checks
        try:
            # Get current memory status
            memory_status = self._tracker.check_memory_budget()

            # Determine overall health status
            if memory_status.status == "critical":
                overall_status = HealthStatus.CRITICAL
            elif memory_status.status in ("warning", "caution"):
                overall_status = HealthStatus.WARNING
            else:
                overall_status = HealthStatus.HEALTHY

            # Simplified checks
            checks = {
                "memory_usage": {
                    "healthy": memory_status.is_healthy(),
                    "status": memory_status.status,
                    "usage_percent": memory_status.usage_percent,
                },
                "tracker_status": {
                    "healthy": True,
                    "monitoring_active": self._tracker.get_stats().get("monitoring_active", False),
                },
            }

            return {
                "status": overall_status.value,
                "timestamp": time.time(),
                "checks": checks,
                "failed_checks": sum(1 for c in checks.values() if not c["healthy"]),
                "critical_issues": 0,
            }

        except Exception as e:
            self._structured_logger.log_error(
                "monitoring_error",
                f"Health check failed: {e}"
            )
            raise MonitoringError(f"Health check failed: {e}")

    def get_snapshots(
        self,
        max_snapshots: Optional[int] = None
    ) -> List[MemorySnapshot]:
        """Get memory profiling snapshots.

        Args:
            max_snapshots: Maximum number of snapshots to return

        Returns:
            List of memory snapshots
        """
        return self._profiler.get_snapshots(max_snapshots)

    def get_latest_snapshot(self) -> Optional[MemorySnapshot]:
        """Get the most recent snapshot.

        Returns:
            Latest snapshot or None
        """
        return self._profiler.get_latest_snapshot()


# =============================================================================
# Global Monitor Instance
# =============================================================================

_global_monitor: Optional[MemoryMonitor] = None
_global_monitor_lock = Lock()


def get_monitor() -> MemoryMonitor:
    """Get the global memory monitor instance.

    Returns:
        Global MemoryMonitor instance (creates if needed)
    """
    global _global_monitor

    with _global_monitor_lock:
        if _global_monitor is None:
            _global_monitor = MemoryMonitor()

        return _global_monitor


def start_monitoring() -> None:
    """Start global memory monitoring."""
    monitor = get_monitor()
    monitor.start_sync()


def stop_monitoring() -> None:
    """Stop global memory monitoring."""
    global _global_monitor

    if _global_monitor is not None:
        _global_monitor.stop_sync()


async def get_metrics() -> Dict[str, Any]:
    """Get metrics from global monitor.

    Returns:
        Dictionary with current metrics
    """
    monitor = get_monitor()
    return await monitor.get_metrics()


def get_metrics_sync() -> Dict[str, Any]:
    """Get metrics from global monitor (synchronous).

    Returns:
        Dictionary with current metrics
    """
    monitor = get_monitor()
    return monitor.get_metrics_sync()


async def health_check() -> Dict[str, Any]:
    """Perform health check using global monitor.

    Returns:
        Dictionary with health check results
    """
    monitor = get_monitor()
    return await monitor.health_check()


def health_check_sync() -> Dict[str, Any]:
    """Perform health check using global monitor (synchronous).

    Returns:
        Dictionary with health check results
    """
    monitor = get_monitor()
    return monitor.health_check_sync()
