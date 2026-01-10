"""
Memory Status Data Classes for LeIndex

This module provides data classes for representing memory status and breakdown
information. These classes are used throughout the memory management system to
provide consistent, type-safe representations of memory state.

Key Classes:
- MemoryStatus: Current memory status with limits and usage percentages
- MemoryBreakdown: Detailed breakdown of memory usage by component

Example:
    >>> from leindex.memory.status import MemoryStatus, MemoryBreakdown
    >>> breakdown = MemoryBreakdown(
    ...     timestamp=time.time(),
    ...     total_mb=1024.0,
    ...     process_rss_mb=1024.0,
    ...     heap_mb=512.0,
    ...     global_index_mb=256.0,
    ...     project_indexes_mb=256.0,
    ...     overhead_mb=128.0,
    ...     other_mb=128.0,
    ...     gc_objects=50000
    ... )
    >>> print(f"Total: {breakdown.total_mb:.2f}MB")
"""

import time
from dataclasses import dataclass, field
from typing import Optional, Dict, Any
from enum import Enum


# =============================================================================
# Memory Status Enum
# =============================================================================

class MemoryStatusLevel(Enum):
    """Memory status levels for quick assessment."""
    HEALTHY = "healthy"
    CAUTION = "caution"
    WARNING = "warning"
    CRITICAL = "critical"


# =============================================================================
# Memory Breakdown Data Class
# =============================================================================

@dataclass
class MemoryBreakdown:
    """Detailed breakdown of memory usage by component.

    This class provides a detailed breakdown of how memory is being used
    across different components of the system. It includes both measured
    values (RSS, heap) and estimated component breakdown.

    Attributes:
        timestamp: Unix timestamp when breakdown was calculated
        total_mb: Total RSS memory in MB
        process_rss_mb: Process RSS memory in MB (same as total_mb)
        heap_mb: Estimated Python heap size in MB
        global_index_mb: Estimated memory for global index in MB
        project_indexes_mb: Estimated memory for project indexes in MB
        overhead_mb: Estimated overhead (interpreter, modules, etc.) in MB
        other_mb: Unaccounted memory in MB
        gc_objects: Number of objects tracked by garbage collector
        loaded_files_mb: Optional: memory for loaded file contents
        query_cache_mb: Optional: memory for query cache

    Example:
        >>> breakdown = MemoryBreakdown(
        ...     timestamp=time.time(),
        ...     total_mb=1024.0,
        ...     process_rss_mb=1024.0,
        ...     heap_mb=512.0,
        ...     global_index_mb=256.0,
        ...     project_indexes_mb=256.0,
        ...     overhead_mb=128.0,
        ...     other_mb=128.0,
        ...     gc_objects=50000
        ... )
        >>> print(f"Global index: {breakdown.global_index_mb:.2f}MB")
    """
    timestamp: float
    total_mb: float
    process_rss_mb: float
    heap_mb: float
    global_index_mb: float
    project_indexes_mb: float
    overhead_mb: float
    other_mb: float
    gc_objects: int
    loaded_files_mb: Optional[float] = None
    query_cache_mb: Optional[float] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert breakdown to dictionary.

        Returns:
            Dictionary representation of the breakdown
        """
        return {
            "timestamp": self.timestamp,
            "total_mb": self.total_mb,
            "process_rss_mb": self.process_rss_mb,
            "heap_mb": self.heap_mb,
            "global_index_mb": self.global_index_mb,
            "project_indexes_mb": self.project_indexes_mb,
            "overhead_mb": self.overhead_mb,
            "other_mb": self.other_mb,
            "gc_objects": self.gc_objects,
            "loaded_files_mb": self.loaded_files_mb,
            "query_cache_mb": self.query_cache_mb,
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "MemoryBreakdown":
        """Create breakdown from dictionary.

        Args:
            data: Dictionary representation of breakdown

        Returns:
            MemoryBreakdown instance
        """
        return cls(
            timestamp=data.get("timestamp", time.time()),
            total_mb=data.get("total_mb", 0.0),
            process_rss_mb=data.get("process_rss_mb", 0.0),
            heap_mb=data.get("heap_mb", 0.0),
            global_index_mb=data.get("global_index_mb", 0.0),
            project_indexes_mb=data.get("project_indexes_mb", 0.0),
            overhead_mb=data.get("overhead_mb", 0.0),
            other_mb=data.get("other_mb", 0.0),
            gc_objects=data.get("gc_objects", 0),
            loaded_files_mb=data.get("loaded_files_mb"),
            query_cache_mb=data.get("query_cache_mb"),
        )

    def get_percentage_breakdown(self) -> Dict[str, float]:
        """Get breakdown as percentages of total memory.

        Returns:
            Dictionary with component percentages
        """
        if self.total_mb <= 0:
            return {
                "heap_percent": 0.0,
                "global_index_percent": 0.0,
                "project_indexes_percent": 0.0,
                "overhead_percent": 0.0,
                "other_percent": 0.0,
            }

        return {
            "heap_percent": (self.heap_mb / self.total_mb) * 100,
            "global_index_percent": (self.global_index_mb / self.total_mb) * 100,
            "project_indexes_percent": (self.project_indexes_mb / self.total_mb) * 100,
            "overhead_percent": (self.overhead_mb / self.total_mb) * 100,
            "other_percent": (self.other_mb / self.total_mb) * 100,
        }


# =============================================================================
# Memory Status Data Class
# =============================================================================

@dataclass
class MemoryStatus:
    """Current memory status with limits and usage percentages.

    This class provides a comprehensive view of current memory status including
    usage, limits, and thresholds. It includes both soft and hard limits, as
    well as the LLM prompt threshold.

    Attributes:
        timestamp: Unix timestamp when status was captured
        current_mb: Current RSS memory usage in MB
        soft_limit_mb: Soft limit threshold in MB
        hard_limit_mb: Hard limit threshold in MB
        prompt_threshold_mb: LLM prompt threshold in MB
        total_budget_mb: Total memory budget in MB
        global_index_mb: Global index memory allocation in MB
        usage_percent: Usage as percentage of total budget
        soft_usage_percent: Usage as percentage of soft limit
        hard_usage_percent: Usage as percentage of hard limit
        status: Status level (healthy/caution/warning/critical)
        breakdown: Optional detailed memory breakdown
        growth_rate_mb_per_sec: Memory growth rate in MB/second
        recommendations: Optional list of action recommendations

    Example:
        >>> status = MemoryStatus(
        ...     timestamp=time.time(),
        ...     current_mb=2048.0,
        ...     soft_limit_mb=2457.6,  # 80% of 3072
        ...     hard_limit_mb=3000.0,  # 98% of 3072
        ...     prompt_threshold_mb=2856.0,  # 93% of 3072
        ...     total_budget_mb=3072.0,
        ...     global_index_mb=512.0,
        ...     usage_percent=66.7,
        ...     soft_usage_percent=83.3,
        ...     hard_usage_percent=68.3,
        ...     status=MemoryStatusLevel.CAUTION
        ... )
        >>> print(f"Status: {status.status.value}")
    """
    timestamp: float
    current_mb: float
    soft_limit_mb: float
    hard_limit_mb: float
    prompt_threshold_mb: float
    total_budget_mb: float
    global_index_mb: float
    usage_percent: float
    soft_usage_percent: float
    hard_usage_percent: float
    status: str
    breakdown: Optional[MemoryBreakdown] = None
    growth_rate_mb_per_sec: float = 0.0
    recommendations: Optional[list[str]] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert status to dictionary.

        Returns:
            Dictionary representation of the status
        """
        result = {
            "timestamp": self.timestamp,
            "current_mb": self.current_mb,
            "soft_limit_mb": self.soft_limit_mb,
            "hard_limit_mb": self.hard_limit_mb,
            "prompt_threshold_mb": self.prompt_threshold_mb,
            "total_budget_mb": self.total_budget_mb,
            "global_index_mb": self.global_index_mb,
            "usage_percent": self.usage_percent,
            "soft_usage_percent": self.soft_usage_percent,
            "hard_usage_percent": self.hard_usage_percent,
            "status": self.status,
            "growth_rate_mb_per_sec": self.growth_rate_mb_per_sec,
            "recommendations": self.recommendations or [],
        }

        if self.breakdown:
            result["breakdown"] = self.breakdown.to_dict()

        return result

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "MemoryStatus":
        """Create status from dictionary.

        Args:
            data: Dictionary representation of status

        Returns:
            MemoryStatus instance
        """
        breakdown_data = data.get("breakdown")
        breakdown = MemoryBreakdown.from_dict(breakdown_data) if breakdown_data else None

        return cls(
            timestamp=data.get("timestamp", time.time()),
            current_mb=data.get("current_mb", 0.0),
            soft_limit_mb=data.get("soft_limit_mb", 0.0),
            hard_limit_mb=data.get("hard_limit_mb", 0.0),
            prompt_threshold_mb=data.get("prompt_threshold_mb", 0.0),
            total_budget_mb=data.get("total_budget_mb", 0.0),
            global_index_mb=data.get("global_index_mb", 0.0),
            usage_percent=data.get("usage_percent", 0.0),
            soft_usage_percent=data.get("soft_usage_percent", 0.0),
            hard_usage_percent=data.get("hard_usage_percent", 0.0),
            status=data.get("status", "unknown"),
            breakdown=breakdown,
            growth_rate_mb_per_sec=data.get("growth_rate_mb_per_sec", 0.0),
            recommendations=data.get("recommendations"),
        )

    def is_healthy(self) -> bool:
        """Check if memory status is healthy.

        Returns:
            True if status is healthy or caution
        """
        return self.status in (MemoryStatusLevel.HEALTHY.value, MemoryStatusLevel.CAUTION.value)

    def is_warning(self) -> bool:
        """Check if memory status is at warning level.

        Returns:
            True if status is warning
        """
        return self.status == MemoryStatusLevel.WARNING.value

    def is_critical(self) -> bool:
        """Check if memory status is critical.

        Returns:
            True if status is critical
        """
        return self.status == MemoryStatusLevel.CRITICAL.value

    def exceeds_soft_limit(self) -> bool:
        """Check if current usage exceeds soft limit.

        Returns:
            True if current_mb > soft_limit_mb
        """
        return self.current_mb > self.soft_limit_mb

    def exceeds_hard_limit(self) -> bool:
        """Check if current usage exceeds hard limit.

        Returns:
            True if current_mb > hard_limit_mb
        """
        return self.current_mb > self.hard_limit_mb

    def exceeds_prompt_threshold(self) -> bool:
        """Check if current usage exceeds prompt threshold.

        Returns:
            True if current_mb > prompt_threshold_mb
        """
        return self.current_mb > self.prompt_threshold_mb

    def get_available_mb(self) -> float:
        """Get available memory before hard limit.

        Returns:
            Available memory in MB
        """
        return max(0.0, self.hard_limit_mb - self.current_mb)

    def get_utilization(self) -> str:
        """Get human-readable utilization string.

        Returns:
            String like "66.7% (2048.0/3072.0 MB)"
        """
        return f"{self.usage_percent:.1f}% ({self.current_mb:.1f}/{self.total_budget_mb:.1f} MB)"

    def get_summary(self) -> str:
        """Get summary of memory status.

        Returns:
            Human-readable summary string
        """
        status_emoji = {
            MemoryStatusLevel.HEALTHY.value: "✓",
            MemoryStatusLevel.CAUTION.value: "⚠",
            MemoryStatusLevel.WARNING.value: "⚠⚠",
            MemoryStatusLevel.CRITICAL.value: "✗",
        }.get(self.status, "?")

        return (
            f"{status_emoji} Memory: {self.status.upper()} - "
            f"{self.get_utilization()} - "
            f"Growth: {self.growth_rate_mb_per_sec:+.2f} MB/s"
        )


# =============================================================================
# Factory Functions
# =============================================================================

def create_memory_status_from_measurements(
    current_mb: float,
    total_budget_mb: float,
    global_index_mb: float,
    warning_threshold_percent: float,
    prompt_threshold_percent: float,
    emergency_threshold_percent: float,
    breakdown: Optional[MemoryBreakdown] = None,
    growth_rate_mb_per_sec: float = 0.0,
) -> MemoryStatus:
    """Create MemoryStatus from raw measurements.

    This factory function calculates all derived values (limits, percentages,
    status) from raw measurements.

    Args:
        current_mb: Current RSS memory in MB
        total_budget_mb: Total memory budget in MB
        global_index_mb: Global index allocation in MB
        warning_threshold_percent: Warning threshold (e.g., 80 for 80%)
        prompt_threshold_percent: LLM prompt threshold (e.g., 93 for 93%)
        emergency_threshold_percent: Emergency threshold (e.g., 98 for 98%)
        breakdown: Optional memory breakdown
        growth_rate_mb_per_sec: Memory growth rate in MB/second

    Returns:
        MemoryStatus with all fields populated
    """
    # Calculate limits
    soft_limit_mb = total_budget_mb * (warning_threshold_percent / 100.0)
    hard_limit_mb = total_budget_mb * (emergency_threshold_percent / 100.0)
    prompt_threshold_mb = total_budget_mb * (prompt_threshold_percent / 100.0)

    # Calculate percentages
    usage_percent = (current_mb / total_budget_mb * 100) if total_budget_mb > 0 else 0.0
    soft_usage_percent = (current_mb / soft_limit_mb * 100) if soft_limit_mb > 0 else 0.0
    hard_usage_percent = (current_mb / hard_limit_mb * 100) if hard_limit_mb > 0 else 0.0

    # Determine status
    if usage_percent >= emergency_threshold_percent:
        status = MemoryStatusLevel.CRITICAL.value
    elif usage_percent >= prompt_threshold_percent:
        status = MemoryStatusLevel.WARNING.value
    elif usage_percent >= warning_threshold_percent:
        status = MemoryStatusLevel.CAUTION.value
    else:
        status = MemoryStatusLevel.HEALTHY.value

    # Generate recommendations
    recommendations = _generate_recommendations(status, usage_percent, breakdown)

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
        growth_rate_mb_per_sec=growth_rate_mb_per_sec,
        recommendations=recommendations,
    )


def _generate_recommendations(
    status: str,
    usage_percent: float,
    breakdown: Optional[MemoryBreakdown],
) -> list[str]:
    """Generate action recommendations based on memory status.

    Args:
        status: Current memory status level
        usage_percent: Current usage percentage
        breakdown: Optional memory breakdown

    Returns:
        List of recommendation strings
    """
    recommendations = []

    if status == MemoryStatusLevel.CRITICAL.value:
        recommendations.append("IMMEDIATE ACTION REQUIRED")
        recommendations.append("Trigger aggressive garbage collection")
        recommendations.append("Unload all non-critical cached data")
        recommendations.append("Consider restarting the process")
    elif status == MemoryStatusLevel.WARNING.value:
        recommendations.append("Memory usage approaching critical levels")
        recommendations.append("Trigger garbage collection")
        recommendations.append("Unload least recently used data")
        recommendations.append("Consider spilling cache to disk")
    elif status == MemoryStatusLevel.CAUTION.value:
        recommendations.append("Memory usage elevated")
        recommendations.append("Monitor memory growth rate")
        recommendations.append("Consider cleanup if growth continues")

    # Add breakdown-specific recommendations
    if breakdown:
        if breakdown.global_index_mb > breakdown.total_mb * 0.4:
            recommendations.append("Global index using significant memory - consider optimization")
        if breakdown.project_indexes_mb > breakdown.total_mb * 0.5:
            recommendations.append("Project indexes using significant memory - consider eviction")

    return recommendations
