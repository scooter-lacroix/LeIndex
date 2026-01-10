"""
Memory Management for LeIndex.

This module provides advanced memory management with:
- Hierarchical YAML configuration (global + project overrides)
- Actual RSS memory tracking (not just allocations)
- Memory threshold actions with LLM-mediated prompting
- Priority-based eviction
- Zero-downtime config reload
- Graceful shutdown with cache persistence
"""

import time
import logging
from typing import Dict, List, Optional, Any, Callable, Tuple
from dataclasses import dataclass, field
from enum import Enum
from pathlib import Path

# Import from existing memory_profiler module
from ..memory_profiler import MemoryProfiler, MemorySnapshot, MemoryLimits

logger = logging.getLogger(__name__)


# ============================================================================
# Data Classes and Enums
# ============================================================================

class MemorySeverity(Enum):
    """Severity levels for memory warnings."""
    LOW = "low"
    MEDIUM = "medium"
    HIGH = "high"
    CRITICAL = "critical"


class MemoryActionType(Enum):
    """Types of memory management actions."""
    CLEANUP = "cleanup"
    SPILL_TO_DISK = "spill_to_disk"
    EVICTION = "eviction"
    GC_TRIGGER = "gc_trigger"
    LIMIT_EXCEEDED = "limit_exceeded"


@dataclass
class MemoryStatus:
    """
    Memory status representation.

    Stub implementation that wraps MemorySnapshot from memory_profiler.
    Full implementation to be completed in Task 4.3.
    """
    timestamp: float
    current_mb: float
    peak_mb: float
    heap_size_mb: float
    gc_objects: int
    active_threads: int
    loaded_files: int
    cached_queries: int
    soft_limit_exceeded: bool = False
    hard_limit_exceeded: bool = False

    @classmethod
    def from_snapshot(cls, snapshot: MemorySnapshot, limits: MemoryLimits) -> "MemoryStatus":
        """Create MemoryStatus from MemorySnapshot."""
        return cls(
            timestamp=snapshot.timestamp,
            current_mb=snapshot.process_memory_mb,
            peak_mb=snapshot.peak_memory_mb,
            heap_size_mb=snapshot.heap_size_mb,
            gc_objects=snapshot.gc_objects,
            active_threads=snapshot.active_threads,
            loaded_files=snapshot.loaded_files,
            cached_queries=snapshot.cached_queries,
            soft_limit_exceeded=snapshot.process_memory_mb > limits.soft_limit_mb,
            hard_limit_exceeded=snapshot.process_memory_mb > limits.hard_limit_mb
        )


@dataclass
class MemoryBreakdown:
    """
    Detailed memory breakdown by category.

    Stub implementation based on MemorySnapshot data.
    Full implementation to be completed in Task 4.3.
    """
    timestamp: float
    total_mb: float
    process_rss_mb: float
    heap_mb: float
    loaded_content_mb: float = 0.0
    query_cache_mb: float = 0.0
    indexes_mb: float = 0.0
    other_mb: float = 0.0

    @classmethod
    def from_snapshot(cls, snapshot: MemorySnapshot) -> "MemoryBreakdown":
        """Create MemoryBreakdown from MemorySnapshot."""
        # Estimate breakdown (stub - full implementation in Task 4.3)
        total_mb = snapshot.process_memory_mb
        heap_mb = snapshot.heap_size_mb

        # Simple estimation - will be refined in Task 4.3
        return cls(
            timestamp=snapshot.timestamp,
            total_mb=total_mb,
            process_rss_mb=total_mb,
            heap_mb=heap_mb,
            loaded_content_mb=heap_mb * 0.4,  # Estimate
            query_cache_mb=heap_mb * 0.2,     # Estimate
            indexes_mb=heap_mb * 0.3,         # Estimate
            other_mb=heap_mb * 0.1            # Estimate
        )


@dataclass
class MemoryWarning:
    """
    Memory warning representation.

    Stub implementation for memory warnings.
    Full implementation to be completed in Task 5.2.
    """
    severity: MemorySeverity
    message: str
    current_mb: float
    limit_mb: float
    timestamp: float = field(default_factory=time.time)
    action_suggested: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert warning to dictionary."""
        return {
            "severity": self.severity.value,
            "message": self.message,
            "current_mb": self.current_mb,
            "limit_mb": self.limit_mb,
            "timestamp": self.timestamp,
            "action_suggested": self.action_suggested
        }


@dataclass
class MemoryAction:
    """
    Memory action representation.

    Stub implementation for memory actions.
    Full implementation to be completed in Task 5.2.
    """
    action_type: MemoryActionType
    description: str
    timestamp: float = field(default_factory=time.time)
    executed: bool = False
    result: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert action to dictionary."""
        return {
            "action_type": self.action_type.value,
            "description": self.description,
            "timestamp": self.timestamp,
            "executed": self.executed,
            "result": self.result
        }


# ============================================================================
# Memory Manager
# ============================================================================

class MemoryManager:
    """
    Main memory management interface.

    Wraps MemoryProfiler from memory_profiler module and provides
    the public API specified in the memory module design.

    Full implementation to be completed in Task 4.3.
    """

    def __init__(self, limits: Optional[MemoryLimits] = None):
        """Initialize MemoryManager with optional memory limits."""
        self._profiler = MemoryProfiler(limits)
        self._warnings: List[MemoryWarning] = []
        self._actions: List[MemoryAction] = []

    def take_snapshot(self, loaded_files: int = 0, cached_queries: int = 0) -> MemorySnapshot:
        """Take a memory snapshot."""
        return self._profiler.take_snapshot(loaded_files, cached_queries)

    def get_status(self) -> MemoryStatus:
        """Get current memory status."""
        snapshot = self.take_snapshot()
        return MemoryStatus.from_snapshot(snapshot, self._profiler.limits)

    def get_breakdown(self) -> MemoryBreakdown:
        """Get detailed memory breakdown."""
        snapshot = self.take_snapshot()
        return MemoryBreakdown.from_snapshot(snapshot)

    def check_limits(self, snapshot: Optional[MemorySnapshot] = None) -> Dict[str, bool]:
        """Check if memory limits are exceeded."""
        if snapshot is None:
            snapshot = self.take_snapshot()
        return self._profiler.check_limits(snapshot)

    def enforce_limits(self, snapshot: Optional[MemorySnapshot] = None) -> Dict[str, Any]:
        """Enforce memory limits and trigger appropriate actions."""
        if snapshot is None:
            snapshot = self.take_snapshot()
        return self._profiler.enforce_limits(snapshot)

    def get_warnings(self) -> List[MemoryWarning]:
        """Get list of memory warnings."""
        return self._warnings.copy()

    def get_actions(self) -> List[MemoryAction]:
        """Get list of memory actions."""
        return self._actions.copy()

    def cleanup(self) -> bool:
        """Trigger cleanup to reduce memory usage."""
        self._profiler._trigger_cleanup()
        action = MemoryAction(
            action_type=MemoryActionType.CLEANUP,
            description="Triggered cleanup callbacks",
            executed=True
        )
        self._actions.append(action)
        return True

    def spill_to_disk(self, key: str, data: Any) -> bool:
        """Spill data to disk."""
        result = self._profiler.spill_to_disk(key, data)
        action = MemoryAction(
            action_type=MemoryActionType.SPILL_TO_DISK,
            description=f"Spilled data for key '{key}'",
            executed=True,
            result="success" if result else "failed"
        )
        self._actions.append(action)
        return result

    def load_from_disk(self, key: str) -> Optional[Any]:
        """Load spilled data from disk."""
        return self._profiler.load_from_disk(key)

    def start_monitoring(self, interval: float = 30.0):
        """Start continuous memory monitoring."""
        self._profiler.start_monitoring(interval)

    def stop_monitoring(self):
        """Stop continuous memory monitoring."""
        self._profiler.stop_monitoring()

    def get_stats(self) -> Dict[str, Any]:
        """Get comprehensive memory statistics."""
        return self._profiler.get_stats()

    def register_cleanup_callback(self, callback: Callable):
        """Register a cleanup callback."""
        self._profiler.register_cleanup_callback(callback)

    def register_spill_callback(self, callback: Callable):
        """Register a spill callback."""
        self._profiler.register_spill_callback(callback)

    def register_limit_exceeded_callback(self, callback: Callable):
        """Register a limit exceeded callback."""
        self._profiler.register_limit_exceeded_callback(callback)


# ============================================================================
# Threshold Manager
# ============================================================================

class ThresholdManager:
    """
    Memory threshold management.

    Stub implementation wrapping MemoryLimits functionality.
    Full implementation to be completed in Task 5.2.
    """

    def __init__(self, limits: Optional[MemoryLimits] = None):
        """Initialize ThresholdManager with optional limits."""
        self.limits = limits or MemoryLimits()
        self._violations: Dict[str, bool] = {}

    def check_thresholds(self, snapshot: MemorySnapshot) -> Dict[str, bool]:
        """Check all thresholds against current snapshot."""
        self._violations = {
            'soft_limit': snapshot.process_memory_mb > self.limits.soft_limit_mb,
            'hard_limit': snapshot.process_memory_mb > self.limits.hard_limit_mb,
            'gc_threshold': snapshot.process_memory_mb > self.limits.gc_threshold_mb,
            'spill_threshold': snapshot.process_memory_mb > self.limits.spill_threshold_mb,
            'max_loaded_files': snapshot.loaded_files > self.limits.max_loaded_files,
            'max_cached_queries': snapshot.cached_queries > self.limits.max_cached_queries
        }
        return self._violations.copy()

    def get_warnings(self) -> List[MemoryWarning]:
        """Get warnings based on current violations."""
        warnings = []
        for threshold, exceeded in self._violations.items():
            if exceeded:
                severity = self._get_severity_for_threshold(threshold)
                message = f"Threshold exceeded: {threshold}"
                warnings.append(MemoryWarning(
                    severity=severity,
                    message=message,
                    current_mb=0.0,  # Will be populated by caller
                    limit_mb=self._get_limit_for_threshold(threshold),
                    action_suggested=self._get_action_for_threshold(threshold)
                ))
        return warnings

    def _get_severity_for_threshold(self, threshold: str) -> MemorySeverity:
        """Get severity level for a threshold."""
        if threshold == 'hard_limit':
            return MemorySeverity.CRITICAL
        elif threshold in ('soft_limit', 'spill_threshold'):
            return MemorySeverity.HIGH
        elif threshold == 'gc_threshold':
            return MemorySeverity.MEDIUM
        return MemorySeverity.LOW

    def _get_limit_for_threshold(self, threshold: str) -> float:
        """Get the limit value for a threshold."""
        limit_map = {
            'soft_limit': self.limits.soft_limit_mb,
            'hard_limit': self.limits.hard_limit_mb,
            'gc_threshold': self.limits.gc_threshold_mb,
            'spill_threshold': self.limits.spill_threshold_mb,
            'max_loaded_files': float(self.limits.max_loaded_files),
            'max_cached_queries': float(self.limits.max_cached_queries)
        }
        return limit_map.get(threshold, 0.0)

    def _get_action_for_threshold(self, threshold: str) -> str:
        """Get suggested action for a threshold."""
        action_map = {
            'soft_limit': 'Trigger cleanup or GC',
            'hard_limit': 'Immediate aggressive cleanup required',
            'gc_threshold': 'Run garbage collection',
            'spill_threshold': 'Spill data to disk',
            'max_loaded_files': 'Unload some loaded files',
            'max_cached_queries': 'Clear query cache'
        }
        return action_map.get(threshold, 'Review memory usage')

    def update_limits(self, **kwargs):
        """Update memory limits."""
        for key, value in kwargs.items():
            if hasattr(self.limits, key):
                setattr(self.limits, key, value)


# ============================================================================
# Memory Action Manager
# ============================================================================

class MemoryActionManager:
    """
    Manages memory-related actions and their execution.

    Stub implementation for memory action management.
    Full implementation to be completed in Task 5.2.
    """

    def __init__(self):
        """Initialize MemoryActionManager."""
        self._actions: List[MemoryAction] = []
        self._pending_actions: List[MemoryAction] = []

    def queue_action(self, action_type: MemoryActionType, description: str) -> MemoryAction:
        """Queue a memory action for execution."""
        action = MemoryAction(
            action_type=action_type,
            description=description
        )
        self._pending_actions.append(action)
        return action

    def execute_action(self, action: MemoryAction, executor: Optional[Callable] = None) -> bool:
        """Execute a memory action."""
        try:
            if executor:
                result = executor()
                action.result = str(result)
            else:
                action.result = "Executed with default handler"

            action.executed = True
            self._actions.append(action)

            if action in self._pending_actions:
                self._pending_actions.remove(action)

            return True
        except Exception as e:
            action.result = f"Error: {e}"
            action.executed = False
            logger.error(f"Failed to execute action {action.action_type}: {e}")
            return False

    def execute_pending(self, executor: Optional[Callable] = None) -> List[bool]:
        """Execute all pending actions."""
        results = []
        for action in list(self._pending_actions):
            results.append(self.execute_action(action, executor))
        return results

    def get_history(self) -> List[MemoryAction]:
        """Get action history."""
        return self._actions.copy()

    def get_pending(self) -> List[MemoryAction]:
        """Get pending actions."""
        return self._pending_actions.copy()


# ============================================================================
# Eviction Manager
# ============================================================================

class EvictionManager:
    """
    Manages priority-based eviction of cached data.

    Stub implementation for eviction management.
    Full implementation to be completed in Task 5.2.
    """

    def __init__(self):
        """Initialize EvictionManager."""
        self._eviction_history: List[Dict[str, Any]] = []
        self._eviction_policies: Dict[str, Callable] = {}

    def register_policy(self, name: str, policy: Callable):
        """Register an eviction policy."""
        self._eviction_policies[name] = policy

    def select_for_eviction(self, candidates: List[Any], policy: str = "lru") -> List[Any]:
        """Select items for eviction based on policy."""
        if policy not in self._eviction_policies:
            logger.warning(f"Unknown eviction policy: {policy}, using default")
            # Default: return first half of candidates
            return candidates[:len(candidates) // 2]

        try:
            return self._eviction_policies[policy](candidates)
        except Exception as e:
            logger.error(f"Eviction policy {policy} failed: {e}")
            return []

    def evict(self, items: List[Any], cleanup_func: Optional[Callable] = None) -> int:
        """Evict items and return count of evicted items."""
        evicted = 0
        for item in items:
            try:
                if cleanup_func:
                    cleanup_func(item)
                evicted += 1
            except Exception as e:
                logger.error(f"Failed to evict item: {e}")

        self._eviction_history.append({
            "timestamp": time.time(),
            "evicted_count": evicted,
            "total_candidates": len(items)
        })

        return evicted

    def get_history(self) -> List[Dict[str, Any]]:
        """Get eviction history."""
        return self._eviction_history.copy()


# ============================================================================
# Imports from Phase 5 Implementation
# ============================================================================

# Import new threshold system
from .thresholds import (
    ThresholdChecker as NewThresholdChecker,
    MemoryWarning as NewMemoryWarning,
    ThresholdLevel,
    check_thresholds as new_check_thresholds,
    get_global_checker,
)

# Import new action system
from .actions import (
    ActionQueue,
    Action,
    ActionResult,
    ActionResultStatus,
    ActionType,
    enqueue_action,
    execute_all_actions,
    get_global_queue,
)

# Import new eviction system
from .eviction import (
    EvictionManager as NewEvictionManager,
    ProjectCandidate,
    ProjectPriority,
    EvictionResult,
    emergency_eviction,
    get_global_manager as get_global_eviction_manager,
)

# Import tracker and status
from .tracker import (
    MemoryTracker,
    get_current_usage_mb,
    check_memory_budget as tracker_check_memory_budget,
    get_global_tracker,
)

from .status import (
    MemoryStatus as NewMemoryStatus,
    MemoryBreakdown as NewMemoryBreakdown,
    MemoryStatusLevel,
)

# ============================================================================
# Public API
# ============================================================================

__all__ = [
    # Legacy API (from memory_profiler)
    "MemoryManager",
    "MemoryStatus",  # Legacy
    "MemoryBreakdown",  # Legacy
    "MemoryWarning",  # Legacy
    "ThresholdManager",
    "MemoryActionManager",
    "EvictionManager",  # Legacy
    "MemorySeverity",
    "MemoryActionType",
    "MemoryAction",
    # New Phase 5 API
    "NewThresholdChecker",
    "NewMemoryWarning",
    "ThresholdLevel",
    "new_check_thresholds",
    "get_global_checker",
    "ActionQueue",
    "Action",
    "ActionResult",
    "ActionResultStatus",
    "ActionType",
    "enqueue_action",
    "execute_all_actions",
    "get_global_queue",
    "NewEvictionManager",
    "ProjectCandidate",
    "ProjectPriority",
    "EvictionResult",
    "emergency_eviction",
    "get_global_eviction_manager",
    "MemoryTracker",
    "get_current_usage_mb",
    "tracker_check_memory_budget",
    "get_global_tracker",
    "NewMemoryStatus",
    "NewMemoryBreakdown",
    "MemoryStatusLevel",
]
