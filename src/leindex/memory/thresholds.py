"""
Memory Threshold Detection and Actions for LeIndex

This module provides threshold detection and action triggering for memory management.
It monitors memory usage against configured thresholds and triggers appropriate actions
including warnings, LLM prompts, and emergency eviction.

Key Features:
- Multi-level threshold detection (80%, 93%, 98%)
- Warning generation with heuristic-based recommendations
- Action queuing and execution
- Integration with LLM via MCP context (not direct calls)
- Thread-safe implementation

Threshold Levels:
- 80% (warning_threshold): Log warning only, no action
- 93% (prompt_threshold): Return warning to LLM for user prompt
- 98% (emergency_threshold): Automatic emergency eviction

Example:
    >>> from leindex.memory.thresholds import check_thresholds, MemoryWarning
    >>> warning = check_thresholds(current_memory_mb=2500, total_budget_mb=3072)
    >>> if warning:
    ...     print(f"Warning: {warning.message}")
    ...     print(f"Recommended: {warning.recommendations}")
"""

import logging
import time
from dataclasses import dataclass, field
from typing import Optional, List, Dict, Any, Callable
from enum import Enum
from threading import Lock

from .status import MemoryStatus
from ..config.global_config import GlobalConfigManager, MemoryConfig


logger = logging.getLogger(__name__)


# =============================================================================
# Threshold Level Enum
# =============================================================================
class ThresholdLevel(Enum):
    """Memory threshold levels."""
    HEALTHY = "healthy"
    CAUTION = "caution"  # 80% - log warning only
    WARNING = "warning"  # 93% - prompt LLM
    CRITICAL = "critical"  # 98% - emergency eviction


# =============================================================================
# Memory Warning Data Class
# =============================================================================
@dataclass
class MemoryWarning:
    """Warning generated when a memory threshold is crossed.

    Attributes:
        threshold_percent: The threshold percentage that was crossed (80, 93, 98)
        level: Severity level (caution/warning/critical)
        action: Recommended action to take
        urgency: Urgency level (low/medium/high/emergency)
        message: Human-readable warning message
        recommendations: List of specific recommendations
        available_actions: List of available actions with estimates
        current_mb: Current memory usage in MB
        threshold_mb: Threshold value in MB
        timestamp: Unix timestamp when warning was generated
    """
    threshold_percent: float
    level: ThresholdLevel
    action: str
    urgency: str
    message: str
    recommendations: List[str] = field(default_factory=list)
    available_actions: List[Dict[str, Any]] = field(default_factory=list)
    current_mb: float = 0.0
    threshold_mb: float = 0.0
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> Dict[str, Any]:
        """Convert warning to dictionary for MCP context.

        Returns:
            Dictionary representation of the warning
        """
        return {
            "threshold_percent": self.threshold_percent,
            "level": self.level.value,
            "action": self.action,
            "urgency": self.urgency,
            "message": self.message,
            "recommendations": self.recommendations,
            "available_actions": self.available_actions,
            "current_mb": self.current_mb,
            "threshold_mb": self.threshold_mb,
            "timestamp": self.timestamp,
        }

    def __str__(self) -> str:
        """Get human-readable string representation."""
        return (
            f"{self.level.value.upper()} - {self.message}\n"
            f"Current: {self.current_mb:.1f}MB / Threshold: {self.threshold_mb:.1f}MB "
            f"({self.threshold_percent:.0f}%)\n"
            f"Recommendations: {', '.join(self.recommendations[:3])}"
        )


# =============================================================================
# Threshold Checker
# =============================================================================
class ThresholdChecker:
    """Checks memory usage against thresholds and generates warnings.

    This class monitors memory usage and generates warnings when thresholds
    are crossed. It implements the three-tier threshold system:
    - 80%: Caution - log warning only
    - 93%: Warning - prompt LLM via MCP context
    - 98%: Critical - automatic emergency eviction

    Thread Safety:
        All methods are thread-safe and can be called from multiple threads.

    Example:
        >>> checker = ThresholdChecker()
        >>> status = check_memory_budget()  # from tracker
        >>> warning = checker.check_thresholds(status)
        >>> if warning:
        ...     # Handle warning (log, return to LLM, or trigger eviction)
    """

    def __init__(self, config: Optional[GlobalConfigManager] = None):
        """Initialize the threshold checker.

        Args:
            config: Global configuration manager (optional, uses default if None)
        """
        self._config_manager = config or GlobalConfigManager()
        self._memory_config = self._config_manager.get_config().memory
        self._lock = Lock()

        # Callbacks for threshold events
        self._caution_callbacks: List[Callable[[MemoryWarning], None]] = []
        self._warning_callbacks: List[Callable[[MemoryWarning], None]] = []
        self._critical_callbacks: List[Callable[[MemoryWarning], None]] = []

    def check_thresholds(self, status: MemoryStatus) -> Optional[MemoryWarning]:
        """Check memory status against thresholds and generate warning if needed.

        Args:
            status: Current memory status from tracker

        Returns:
            MemoryWarning if threshold crossed, None otherwise
        """
        with self._lock:
            # Get threshold percentages from config
            warning_threshold = self._memory_config.warning_threshold_percent  # 80%
            prompt_threshold = self._memory_config.prompt_threshold_percent  # 93%
            emergency_threshold = self._memory_config.emergency_threshold_percent  # 98%

            # Determine which threshold was crossed (if any)
            if status.usage_percent >= emergency_threshold:
                return self._generate_critical_warning(status, emergency_threshold)
            elif status.usage_percent >= prompt_threshold:
                return self._generate_warning_warning(status, prompt_threshold)
            elif status.usage_percent >= warning_threshold:
                return self._generate_caution_warning(status, warning_threshold)
            else:
                # No threshold crossed
                return None

    def _generate_caution_warning(
        self,
        status: MemoryStatus,
        threshold_percent: float
    ) -> MemoryWarning:
        """Generate caution-level warning (80% threshold).

        At caution level, we log a warning but don't trigger any actions.
        This is an early warning system.

        Args:
            status: Current memory status
            threshold_percent: Threshold percentage (80)

        Returns:
            MemoryWarning with caution-level details
        """
        threshold_mb = status.total_budget_mb * (threshold_percent / 100.0)

        # Generate recommendations
        recommendations = self._generate_recommendations(status, ThresholdLevel.CAUTION)

        # Get available actions
        available_actions = self._get_available_actions(status)

        warning = MemoryWarning(
            threshold_percent=threshold_percent,
            level=ThresholdLevel.CAUTION,
            action="monitor",
            urgency="low",
            message=(
                f"Memory usage approaching caution threshold ({threshold_percent}%). "
                f"Current usage: {status.usage_percent:.1f}% ({status.current_mb:.1f}MB). "
                f"Monitor memory growth and be prepared to take action if usage continues to rise."
            ),
            recommendations=recommendations,
            available_actions=available_actions,
            current_mb=status.current_mb,
            threshold_mb=threshold_mb,
            timestamp=time.time()
        )

        # Log warning
        logger.warning(f"Memory threshold caution: {status.usage_percent:.1f}% ({status.current_mb:.1f}MB)")

        # Trigger callbacks
        for callback in self._caution_callbacks:
            try:
                callback(warning)
            except Exception as e:
                logger.error(f"Error in caution callback: {e}")

        return warning

    def _generate_warning_warning(
        self,
        status: MemoryStatus,
        threshold_percent: float
    ) -> MemoryWarning:
        """Generate warning-level alert (93% threshold).

        At warning level, we return the warning to the LLM via MCP context.
        The LLM will then prompt the user to select an action.

        CRITICAL: We do NOT call the LLM directly. We return the warning
        through MCP context and let the LLM integration layer handle it.

        Args:
            status: Current memory status
            threshold_percent: Threshold percentage (93)

        Returns:
            MemoryWarning with warning-level details
        """
        threshold_mb = status.total_budget_mb * (threshold_percent / 100.0)

        # Generate recommendations
        recommendations = self._generate_recommendations(status, ThresholdLevel.WARNING)

        # Get available actions with estimates
        available_actions = self._get_available_actions(status)

        warning = MemoryWarning(
            threshold_percent=threshold_percent,
            level=ThresholdLevel.WARNING,
            action="prompt_user",
            urgency="medium",
            message=(
                f"Memory usage at warning threshold ({threshold_percent}%). "
                f"Current usage: {status.usage_percent:.1f}% ({status.current_mb:.1f}MB). "
                f"Immediate attention required. Please select an action below."
            ),
            recommendations=recommendations,
            available_actions=available_actions,
            current_mb=status.current_mb,
            threshold_mb=threshold_mb,
            timestamp=time.time()
        )

        # Log warning
        logger.warning(
            f"Memory threshold warning: {status.usage_percent:.1f}% ({status.current_mb:.1f}MB) - "
            f"Prompting user for action selection"
        )

        # Trigger callbacks
        for callback in self._warning_callbacks:
            try:
                callback(warning)
            except Exception as e:
                logger.error(f"Error in warning callback: {e}")

        return warning

    def _generate_critical_warning(
        self,
        status: MemoryStatus,
        threshold_percent: float
    ) -> MemoryWarning:
        """Generate critical-level alert (98% threshold).

        At critical level, we trigger automatic emergency eviction.
        This is an automated response to prevent OOM.

        Args:
            status: Current memory status
            threshold_percent: Threshold percentage (98)

        Returns:
            MemoryWarning with critical-level details
        """
        threshold_mb = status.total_budget_mb * (threshold_percent / 100.0)

        # Generate recommendations
        recommendations = self._generate_recommendations(status, ThresholdLevel.CRITICAL)

        # Get available actions
        available_actions = self._get_available_actions(status)

        warning = MemoryWarning(
            threshold_percent=threshold_percent,
            level=ThresholdLevel.CRITICAL,
            action="emergency_eviction",
            urgency="emergency",
            message=(
                f"EMERGENCY: Memory usage at critical threshold ({threshold_percent}%). "
                f"Current usage: {status.usage_percent:.1f}% ({status.current_mb:.1f}MB). "
                f"Triggering automatic emergency eviction to prevent OOM."
            ),
            recommendations=recommendations,
            available_actions=available_actions,
            current_mb=status.current_mb,
            threshold_mb=threshold_mb,
            timestamp=time.time()
        )

        # Log critical warning
        logger.critical(
            f"Memory threshold critical: {status.usage_percent:.1f}% ({status.current_mb:.1f}MB) - "
            f"Triggering emergency eviction"
        )

        # Trigger callbacks
        for callback in self._critical_callbacks:
            try:
                callback(warning)
            except Exception as e:
                logger.error(f"Error in critical callback: {e}")

        return warning

    def _generate_recommendations(
        self,
        status: MemoryStatus,
        level: ThresholdLevel
    ) -> List[str]:
        """Generate heuristic-based recommendations.

        Args:
            status: Current memory status
            level: Threshold level

        Returns:
            List of recommendation strings
        """
        recommendations = []

        # Base recommendations by level
        if level == ThresholdLevel.CRITICAL:
            recommendations.append("IMMEDIATE: Trigger emergency eviction")
            recommendations.append("Unload all non-critical cached data")
            recommendations.append("Trigger aggressive garbage collection")
            recommendations.append("Consider restarting the process if eviction fails")
        elif level == ThresholdLevel.WARNING:
            recommendations.append("Trigger garbage collection")
            recommendations.append("Unload least recently used data")
            recommendations.append("Consider spilling cache to disk")
            recommendations.append("Review project memory allocations")
        elif level == ThresholdLevel.CAUTION:
            recommendations.append("Monitor memory growth rate")
            recommendations.append("Review cached data and query cache")
            recommendations.append("Consider cleanup if growth continues")

        # Add breakdown-specific recommendations
        if status.breakdown:
            total_mb = status.breakdown.total_mb
            if status.breakdown.global_index_mb > total_mb * 0.4:
                recommendations.append(
                    f"Global index using {status.breakdown.global_index_mb:.1f}MB "
                    f"({status.breakdown.global_index_mb/total_mb*100:.1f}%) - consider optimization"
                )
            if status.breakdown.project_indexes_mb > total_mb * 0.5:
                recommendations.append(
                    f"Project indexes using {status.breakdown.project_indexes_mb:.1f}MB "
                    f"({status.breakdown.project_indexes_mb/total_mb*100:.1f}%) - consider eviction"
                )
            if status.breakdown.gc_objects > 100000:
                recommendations.append(
                    f"High object count ({status.breakdown.gc_objects:,}) - GC may help"
                )

        # Add growth rate recommendations
        if status.growth_rate_mb_per_sec > 1.0:
            recommendations.append(
                f"High memory growth rate ({status.growth_rate_mb_per_sec:.2f} MB/s) - "
                f"identify and stop memory-intensive operations"
            )
        elif status.growth_rate_mb_per_sec > 0.1:
            recommendations.append(
                f"Moderate memory growth ({status.growth_rate_mb_per_sec:.2f} MB/s) - "
                f"monitor closely"
            )

        return recommendations

    def _get_available_actions(self, status: MemoryStatus) -> List[Dict[str, Any]]:
        """Get available actions with memory estimates.

        Args:
            status: Current memory status

        Returns:
            List of action dictionaries with name, description, and estimated_freed_mb
        """
        actions = []

        # Estimate memory that can be freed by each action
        # These are heuristic estimates based on typical usage patterns

        # Garbage collection
        actions.append({
            "name": "garbage_collection",
            "display_name": "Trigger Garbage Collection",
            "description": "Run Python garbage collector to free unreachable objects",
            "estimated_freed_mb": max(10.0, status.current_mb * 0.05),  # ~5% or 10MB minimum
            "urgency": "low",
            "side_effects": "May cause brief pause",
        })

        # Unload file contents
        if status.breakdown and status.breakdown.loaded_files_mb:
            estimated_freed = status.breakdown.loaded_files_mb * 0.7  # Unload 70%
            actions.append({
                "name": "unload_files",
                "display_name": "Unload File Contents",
                "description": f"Unload ~70% of cached file contents (~{estimated_freed:.1f}MB)",
                "estimated_freed_mb": estimated_freed,
                "urgency": "medium",
                "side_effects": "Files will need to be re-read on next access",
            })

        # Clear query cache
        if status.breakdown and status.breakdown.query_cache_mb:
            estimated_freed = status.breakdown.query_cache_mb
            actions.append({
                "name": "clear_query_cache",
                "display_name": "Clear Query Cache",
                "description": f"Clear query cache (~{estimated_freed:.1f}MB)",
                "estimated_freed_mb": estimated_freed,
                "urgency": "low",
                "side_effects": "Queries will be slower until cache warms up",
            })

        # Unload projects
        if status.breakdown and status.breakdown.project_indexes_mb:
            # Estimate 3 projects can be unloaded
            estimated_per_project = status.breakdown.project_indexes_mb * 0.3
            actions.append({
                "name": "unload_projects",
                "display_name": "Unload Least Recently Used Projects",
                "description": f"Unload up to 3 LRU projects (~{estimated_per_project:.1f}MB each)",
                "estimated_freed_mb": estimated_per_project * 3,
                "urgency": "medium",
                "side_effects": "Projects will need to be re-loaded on next access",
            })

        # Emergency eviction (unload all)
        actions.append({
            "name": "emergency_eviction",
            "display_name": "EMERGENCY: Evict All Cached Data",
            "description": "Unload all non-critical data (~50% of current usage)",
            "estimated_freed_mb": status.current_mb * 0.5,
            "urgency": "emergency",
            "side_effects": "Significant performance degradation until caches warm up",
        })

        # Sort by estimated memory freed (descending)
        actions.sort(key=lambda a: a["estimated_freed_mb"], reverse=True)

        return actions

    def register_caution_callback(self, callback: Callable[[MemoryWarning], None]) -> None:
        """Register a callback for caution-level warnings.

        Args:
            callback: Function to call when caution threshold is crossed
        """
        with self._lock:
            self._caution_callbacks.append(callback)

    def register_warning_callback(self, callback: Callable[[MemoryWarning], None]) -> None:
        """Register a callback for warning-level alerts.

        Args:
            callback: Function to call when warning threshold is crossed
        """
        with self._lock:
            self._warning_callbacks.append(callback)

    def register_critical_callback(self, callback: Callable[[MemoryWarning], None]) -> None:
        """Register a callback for critical-level alerts.

        Args:
            callback: Function to call when critical threshold is crossed
        """
        with self._lock:
            self._critical_callbacks.append(callback)


# =============================================================================
# Convenience Functions
# =============================================================================

# Global threshold checker instance
_global_checker: Optional[ThresholdChecker] = None
_global_checker_lock = Lock()


def get_global_checker() -> ThresholdChecker:
    """Get the global threshold checker instance.

    Returns:
        Global ThresholdChecker instance (creates if needed)
    """
    global _global_checker

    with _global_checker_lock:
        if _global_checker is None:
            _global_checker = ThresholdChecker()

        return _global_checker


def check_thresholds(status: MemoryStatus) -> Optional[MemoryWarning]:
    """Check memory status against thresholds.

    This is a convenience function that uses the global checker.

    Args:
        status: Current memory status from tracker

    Returns:
        MemoryWarning if threshold crossed, None otherwise
    """
    checker = get_global_checker()
    return checker.check_thresholds(status)
