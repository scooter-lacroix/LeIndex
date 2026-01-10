"""
Real Memory Usage Tracking for LeIndex

This module provides production-quality memory tracking that measures actual RSS
(Resident Set Size) memory usage, NOT just allocations. It implements background
monitoring, memory breakdown estimation, and growth tracking.

Key Features:
- Real RSS memory measurement using psutil
- Memory breakdown by component (global_index, project_indexes, overhead, other)
- Background monitoring thread with configurable intervals
- Memory growth rate tracking
- Thread-safe implementation
- Historical data retention

Example:
    >>> from leindex.memory.tracker import get_current_usage_mb, check_memory_budget
    >>> usage_mb = get_current_usage_mb()
    >>> status = check_memory_budget()
    >>> print(f"Current: {status.current_mb:.2f}MB / {status.soft_limit_mb}MB")
"""

import gc
import sys
import time
import psutil
import threading
from typing import Optional, Dict, List, Tuple, Any, Callable
from dataclasses import dataclass, field
from collections import deque
from threading import Lock, Event
import logging

from .status import MemoryStatus, MemoryBreakdown
from ..config.global_config import GlobalConfigManager, MemoryConfig

logger = logging.getLogger(__name__)


# =============================================================================
# Memory History Entry
# =============================================================================

@dataclass
class MemoryHistoryEntry:
    """A single entry in memory usage history."""
    timestamp: float
    rss_mb: float
    heap_objects: int
    growth_rate_mb_per_sec: float


# =============================================================================
# Memory Tracker Configuration
# =============================================================================

@dataclass
class MemoryTrackerConfig:
    """Configuration for memory tracker.

    Attributes:
        monitoring_interval_seconds: How often to monitor memory (default: 30s)
        history_retention_hours: How long to keep historical data (default: 24h)
        history_sample_interval: How often to sample history (default: 60s)
        enable_background_monitoring: Enable background monitoring thread
        growth_rate_window_seconds: Window for growth rate calculation (default: 300s)
    """
    monitoring_interval_seconds: float = 30.0
    history_retention_hours: float = 24.0
    history_sample_interval: float = 60.0
    enable_background_monitoring: bool = True
    growth_rate_window_seconds: float = 300.0


# =============================================================================
# Memory Tracker Implementation
# =============================================================================

class MemoryTracker:
    """Real memory usage tracker with RSS measurement and background monitoring.

    This class provides actual RSS memory tracking using psutil, with a
    background monitoring thread that samples memory usage at regular intervals.
    It maintains historical data and calculates memory growth rates.

    Thread Safety:
        All public methods are thread-safe and can be called from multiple threads.

    Example:
        >>> tracker = MemoryTracker()
        >>> usage_mb = tracker.get_current_usage_mb()
        >>> status = tracker.check_memory_budget()
        >>> print(f"Memory: {usage_mb:.2f}MB")
    """

    def __init__(
        self,
        config: Optional[GlobalConfigManager] = None,
        tracker_config: Optional[MemoryTrackerConfig] = None
    ):
        """Initialize the memory tracker.

        Args:
            config: Global configuration manager (optional, uses default if None)
            tracker_config: Tracker-specific configuration (optional, uses defaults if None)
        """
        # Configuration
        self._config_manager = config or GlobalConfigManager()
        self._tracker_config = tracker_config or MemoryTrackerConfig()

        # Get memory limits from config
        self._memory_config = self._config_manager.get_config().memory

        # Initialize process monitoring
        try:
            self._process = psutil.Process()
            self._process_available = True
        except Exception as e:
            logger.warning(f"Could not initialize process monitoring: {e}")
            self._process = None
            self._process_available = False

        # Historical data (thread-safe)
        self._history: deque = deque()
        self._history_lock = Lock()

        # Background monitoring
        self._monitoring = False
        self._monitor_thread: Optional[threading.Thread] = None
        self._shutdown_event = Event()

        # Last check state for growth tracking
        self._last_check_rss_mb = 0.0
        self._last_check_time = 0.0
        self._last_check_lock = Lock()

        # Baseline memory (RSS at startup)
        self._baseline_mb = self._get_current_rss_mb()

        logger.info(
            f"MemoryTracker initialized: baseline={self._baseline_mb:.2f}MB, "
            f"monitoring={'enabled' if self._tracker_config.enable_background_monitoring else 'disabled'}"
        )

        # Start background monitoring if enabled
        if self._tracker_config.enable_background_monitoring:
            self.start_monitoring()

    def _get_current_rss_mb(self) -> float:
        """Get current RSS memory usage in MB.

        This method uses psutil to get the actual RSS (Resident Set Size),
        which represents the actual physical memory used by the process,
        NOT just allocated memory.

        Returns:
            Current RSS memory in MB, or 0.0 if measurement fails
        """
        if not self._process_available or self._process is None:
            logger.warning("Process monitoring not available")
            return 0.0

        try:
            memory_info = self._process.memory_info()
            rss_bytes = memory_info.rss
            rss_mb = rss_bytes / 1024 / 1024  # Convert to MB

            # Validate the value is reasonable
            if rss_mb < 0:
                logger.warning(f"Negative RSS detected: {rss_mb}MB")
                return 0.0
            elif rss_mb > 1000000:  # More than 1TB seems unreasonable
                logger.warning(f"Excessive RSS detected: {rss_mb}MB - possible error")
                return 0.0

            return rss_mb

        except psutil.NoSuchProcess:
            logger.warning("Process no longer exists")
            self._process_available = False
            return 0.0
        except psutil.AccessDenied:
            logger.warning("Access denied for memory monitoring")
            self._process_available = False
            return 0.0
        except Exception as e:
            logger.warning(f"Error getting RSS: {e}")
            return 0.0

    def get_current_usage_mb(self) -> float:
        """Get current RSS memory usage in MB.

        This is the primary method for getting actual memory usage.
        It returns the Resident Set Size (RSS), which represents the
        actual physical memory used by the process.

        Returns:
            Current RSS memory in MB
        """
        return self._get_current_rss_mb()

    def get_growth_rate_mb_per_sec(self) -> float:
        """Calculate memory growth rate since last check - thread-safe.

        Returns:
            Growth rate in MB/second (positive = growing, negative = shrinking)
        """
        # CRITICAL FIX: Must hold lock throughout entire operation to prevent race condition
        with self._last_check_lock:
            current_rss = self._get_current_rss_mb()
            current_time = time.time()

            if self._last_check_time == 0:
                # First check - initialize
                self._last_check_rss_mb = current_rss
                self._last_check_time = current_time
                return 0.0

            time_delta = current_time - self._last_check_time
            if time_delta <= 0:
                # System clock went backwards or no time elapsed
                return 0.0

            memory_delta = current_rss - self._last_check_rss_mb
            growth_rate = memory_delta / time_delta

            # Update last check state (still holding lock)
            self._last_check_rss_mb = current_rss
            self._last_check_time = current_time

            return growth_rate

    def _calculate_breakdown(self, current_rss_mb: float) -> MemoryBreakdown:
        """Calculate memory breakdown by component.

        This estimates how memory is distributed across different components
        based on heap analysis and object tracking.

        Args:
            current_rss_mb: Current RSS memory in MB

        Returns:
            MemoryBreakdown with estimated component usage
        """
        try:
            # Get heap statistics
            gc_objects = len(gc.get_objects())

            # Estimate heap size (sample-based)
            heap_mb = self._estimate_heap_size()

            # Estimate component breakdown based on heuristics
            # In a real implementation, this would use more sophisticated tracking

            # Global index memory (estimated from loaded data structures)
            global_index_mb = heap_mb * 0.25  # ~25% for global index

            # Project indexes memory
            project_indexes_mb = heap_mb * 0.35  # ~35% for project indexes

            # Overhead (Python interpreter, modules, etc.)
            overhead_mb = current_rss_mb * 0.15  # ~15% overhead

            # Other (unaccounted)
            other_mb = current_rss_mb - global_index_mb - project_indexes_mb - overhead_mb
            if other_mb < 0:
                other_mb = 0.0

            return MemoryBreakdown(
                timestamp=time.time(),
                total_mb=current_rss_mb,
                process_rss_mb=current_rss_mb,
                heap_mb=heap_mb,
                global_index_mb=global_index_mb,
                project_indexes_mb=project_indexes_mb,
                overhead_mb=overhead_mb,
                other_mb=other_mb,
                gc_objects=gc_objects
            )

        except Exception as e:
            logger.warning(f"Error calculating breakdown: {e}")
            # Return minimal breakdown on error
            return MemoryBreakdown(
                timestamp=time.time(),
                total_mb=current_rss_mb,
                process_rss_mb=current_rss_mb,
                heap_mb=0.0,
                global_index_mb=0.0,
                project_indexes_mb=0.0,
                overhead_mb=0.0,
                other_mb=current_rss_mb,
                gc_objects=0
            )

    def _estimate_heap_size(self) -> float:
        """Estimate Python heap size in MB.

        Uses sampling to estimate total heap size based on object sizes.

        Returns:
            Estimated heap size in MB
        """
        try:
            objects = gc.get_objects()
            if not objects:
                return 0.0

            # Sample first 1000 objects for performance
            sample_size = min(1000, len(objects))
            sample_objects = objects[:sample_size]

            # Calculate average object size
            total_sample_size = sum(sys.getsizeof(obj) for obj in sample_objects)
            avg_object_size = total_sample_size / sample_size

            # Estimate total heap size
            estimated_heap_bytes = avg_object_size * len(objects)
            heap_mb = estimated_heap_bytes / 1024 / 1024

            return heap_mb

        except Exception as e:
            logger.warning(f"Error estimating heap size: {e}")
            return 0.0

    def check_memory_budget(self) -> MemoryStatus:
        """Check current memory status against budget limits.

        Returns:
            MemoryStatus with current usage and breakdown
        """
        # Get current RSS
        current_mb = self._get_current_rss_mb()

        # Calculate breakdown
        breakdown = self._calculate_breakdown(current_mb)

        # Get limits from config
        total_budget_mb = float(self._memory_config.total_budget_mb)
        global_index_mb = float(self._memory_config.global_index_mb)

        # Calculate soft/hard limits based on thresholds
        soft_limit_mb = total_budget_mb * (self._memory_config.warning_threshold_percent / 100.0)
        hard_limit_mb = total_budget_mb * (self._memory_config.emergency_threshold_percent / 100.0)
        prompt_threshold_mb = total_budget_mb * (self._memory_config.prompt_threshold_percent / 100.0)

        # Calculate usage percentages
        usage_percent = (current_mb / total_budget_mb * 100) if total_budget_mb > 0 else 0.0
        soft_usage_percent = (current_mb / soft_limit_mb * 100) if soft_limit_mb > 0 else 0.0
        hard_usage_percent = (current_mb / hard_limit_mb * 100) if hard_limit_mb > 0 else 0.0

        # Determine status
        if usage_percent >= self._memory_config.emergency_threshold_percent:
            status = "critical"
        elif usage_percent >= self._memory_config.prompt_threshold_percent:
            status = "warning"
        elif usage_percent >= self._memory_config.warning_threshold_percent:
            status = "caution"
        else:
            status = "healthy"

        return MemoryStatus(
            timestamp=time.time(),
            current_mb=current_mb,
            soft_limit_mb=soft_limit_mb,
            hard_limit_mb=hard_limit_mb,
            prompt_threshold_mb=prompt_threshold_mb,
            total_budget_mb=total_budget_mb,
            global_index_mb=global_index_mb,
            usage_percent=usage_percent,
            soft_usage_percent=soft_usage_percent,
            hard_usage_percent=hard_usage_percent,
            status=status,
            breakdown=breakdown,
            growth_rate_mb_per_sec=self.get_growth_rate_mb_per_sec()
        )

    def start_monitoring(self) -> None:
        """Start background memory monitoring thread.

        The monitoring thread will sample memory usage at regular intervals
        and store historical data for analysis.
        """
        if self._monitoring:
            logger.warning("Monitoring already active")
            return

        self._monitoring = True
        self._shutdown_event.clear()

        def monitor_loop():
            """Background monitoring loop."""
            logger.info(
                f"Starting memory monitoring: interval={self._tracker_config.monitoring_interval_seconds}s"
            )

            while not self._shutdown_event.is_set():
                try:
                    # Sample memory usage
                    current_rss = self._get_current_rss_mb()
                    growth_rate = self.get_growth_rate_mb_per_sec()

                    # Get GC stats
                    try:
                        heap_objects = len(gc.get_objects())
                    except Exception:
                        heap_objects = 0

                    # Create history entry
                    entry = MemoryHistoryEntry(
                        timestamp=time.time(),
                        rss_mb=current_rss,
                        heap_objects=heap_objects,
                        growth_rate_mb_per_sec=growth_rate
                    )

                    # Store in history (thread-safe)
                    with self._history_lock:
                        self._history.append(entry)

                        # Cleanup old entries based on retention policy
                        self._cleanup_old_history()

                except Exception as e:
                    logger.error(f"Error in monitoring loop: {e}")

                # Wait for next interval or shutdown
                self._shutdown_event.wait(self._tracker_config.monitoring_interval_seconds)

            logger.info("Memory monitoring stopped")

        # Start daemon thread
        self._monitor_thread = threading.Thread(target=monitor_loop, daemon=True)
        self._monitor_thread.start()

        logger.info("Background monitoring thread started")

    def stop_monitoring(self) -> None:
        """Stop background memory monitoring thread."""
        if not self._monitoring:
            return

        logger.info("Stopping memory monitoring...")
        self._monitoring = False
        self._shutdown_event.set()

        if self._monitor_thread:
            self._monitor_thread.join(timeout=5.0)
            if self._monitor_thread.is_alive():
                logger.warning("Monitor thread did not stop within timeout")

        logger.info("Memory monitoring stopped")

    def _cleanup_old_history(self) -> None:
        """Remove old history entries based on retention policy."""
        retention_seconds = self._tracker_config.history_retention_hours * 3600
        cutoff_time = time.time() - retention_seconds

        # Remove entries older than retention period
        while self._history and self._history[0].timestamp < cutoff_time:
            self._history.popleft()

    def get_history(self, max_entries: Optional[int] = None) -> List[MemoryHistoryEntry]:
        """Get memory usage history.

        Args:
            max_entries: Maximum number of entries to return (None = all)

        Returns:
            List of memory history entries
        """
        with self._history_lock:
            history_list = list(self._history)

        if max_entries is not None:
            history_list = history_list[-max_entries:]

        return history_list

    def get_stats(self) -> Dict[str, Any]:
        """Get comprehensive memory tracker statistics.

        Returns:
            Dictionary with memory statistics
        """
        current_mb = self._get_current_rss_mb()
        growth_rate = self.get_growth_rate_mb_per_sec()
        status = self.check_memory_budget()

        # Calculate statistics from history
        history = self.get_history()
        if history:
            rss_values = [entry.rss_mb for entry in history]
            avg_rss = sum(rss_values) / len(rss_values)
            max_rss = max(rss_values)
            min_rss = min(rss_values)
        else:
            avg_rss = current_mb
            max_rss = current_mb
            min_rss = current_mb

        return {
            "current_mb": current_mb,
            "baseline_mb": self._baseline_mb,
            "growth_mb": current_mb - self._baseline_mb,
            "growth_rate_mb_per_sec": growth_rate,
            "avg_rss_mb": avg_rss,
            "max_rss_mb": max_rss,
            "min_rss_mb": min_rss,
            "monitoring_active": self._monitoring,
            "history_entries": len(history),
            "status": status,
            "tracker_config": {
                "monitoring_interval_seconds": self._tracker_config.monitoring_interval_seconds,
                "history_retention_hours": self._tracker_config.history_retention_hours,
                "enable_background_monitoring": self._tracker_config.enable_background_monitoring,
            }
        }

    def reset_baseline(self) -> None:
        """Reset the baseline memory to current usage."""
        self._baseline_mb = self._get_current_rss_mb()
        logger.info(f"Baseline reset to {self._baseline_mb:.2f}MB")

    def __del__(self):
        """Cleanup on destruction - safe version.

        CRITICAL FIX: __del__ is called during garbage collection when resources
        may already be partially cleaned up. Calling stop_monitoring() here can
        cause crashes because:
        1. The monitoring thread may already be garbage collected
        2. Locks may be in an invalid state
        3. Logger may be shutting down

        The safe approach is to:
        1. Check if monitoring is active before attempting cleanup
        2. Only signal shutdown event without waiting for thread
        3. Silently ignore any errors during garbage collection
        """
        try:
            # Only attempt cleanup if monitoring is active and thread exists
            if self._monitoring and self._monitor_thread is not None:
                # Check if thread is still alive
                if self._monitor_thread.is_alive():
                    # Just signal shutdown, don't wait (avoids blocking in __del__)
                    self._shutdown_event.set()
        except Exception:
            # Silently ignore all errors during garbage collection
            # __del__ is called in undefined state, errors are expected
            pass


# =============================================================================
# Convenience Functions
# =============================================================================

# Global tracker instance
_global_tracker: Optional[MemoryTracker] = None
_global_tracker_lock = Lock()


def get_global_tracker() -> MemoryTracker:
    """Get the global memory tracker instance.

    Returns:
        Global MemoryTracker instance (creates if needed)
    """
    global _global_tracker

    with _global_tracker_lock:
        if _global_tracker is None:
            _global_tracker = MemoryTracker()

        return _global_tracker


def get_current_usage_mb() -> float:
    """Get current RSS memory usage in MB.

    This is a convenience function that uses the global tracker.

    Returns:
        Current RSS memory in MB
    """
    tracker = get_global_tracker()
    return tracker.get_current_usage_mb()


def check_memory_budget() -> MemoryStatus:
    """Check current memory status against budget limits.

    This is a convenience function that uses the global tracker.

    Returns:
        MemoryStatus with current usage and breakdown
    """
    tracker = get_global_tracker()
    return tracker.check_memory_budget()


def start_monitoring() -> None:
    """Start background memory monitoring.

    This is a convenience function that uses the global tracker.
    """
    tracker = get_global_tracker()
    tracker.start_monitoring()


def stop_monitoring() -> None:
    """Stop background memory monitoring.

    This is a convenience function that uses the global tracker.
    """
    tracker = get_global_tracker()
    tracker.stop_monitoring()
