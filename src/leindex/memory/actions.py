"""
Memory Action Execution System for LeIndex

This module provides action queuing and execution for memory management.
It handles the execution of memory management actions triggered by threshold
crossing, including garbage collection, cache clearing, and project unloading.

Key Features:
- Action queue with priority handling
- Action executor with retry logic
- Action result tracking and reporting
- Thread-safe implementation
- Comprehensive error handling

Action Types:
- garbage_collection: Run Python garbage collector
- unload_files: Unload cached file contents
- clear_query_cache: Clear query result cache
- unload_projects: Unload specified projects from memory
- emergency_eviction: Emergency eviction of all cached data

Example:
    >>> from leindex.memory.actions import ActionQueue, ActionResult
    >>> queue = ActionQueue()
    >>> queue.enqueue("garbage_collection", priority=5)
    >>> queue.enqueue("unload_projects", priority=10, project_ids=["project1"])
    >>> results = queue.execute_all()
    >>> for result in results:
    ...     print(f"{result.action}: {result.success}")
"""

import gc
import logging
import time
from dataclasses import dataclass, field
from typing import Optional, List, Dict, Any, Callable
from enum import Enum
from threading import Lock
from collections import deque
from abc import ABC, abstractmethod


logger = logging.getLogger(__name__)


# =============================================================================
# Action Type Enum
# =============================================================================
class ActionType(Enum):
    """Types of memory management actions."""
    GARBAGE_COLLECTION = "garbage_collection"
    UNLOAD_FILES = "unload_files"
    CLEAR_QUERY_CACHE = "clear_query_cache"
    UNLOAD_PROJECTS = "unload_projects"
    EMERGENCY_EVICTION = "emergency_eviction"


# =============================================================================
# Action Result Status Enum
# =============================================================================
class ActionResultStatus(Enum):
    """Status of action execution."""
    PENDING = "pending"
    RUNNING = "running"
    SUCCESS = "success"
    PARTIAL = "partial"  # Some actions succeeded, some failed
    FAILED = "failed"
    CANCELLED = "cancelled"


# =============================================================================
# Action Result Data Class
# =============================================================================
@dataclass
class ActionResult:
    """Result of executing a memory management action.

    Attributes:
        action: Type of action that was executed
        status: Final status of the action
        memory_freed_mb: Estimated memory freed in MB
        duration_seconds: Time taken to execute the action
        message: Human-readable result message
        details: Additional details about the execution
        error: Error message if execution failed
        timestamp: Unix timestamp when action completed
    """
    action: ActionType
    status: ActionResultStatus
    memory_freed_mb: float
    duration_seconds: float
    message: str
    details: Dict[str, Any] = field(default_factory=dict)
    error: Optional[str] = None
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> Dict[str, Any]:
        """Convert result to dictionary.

        Returns:
            Dictionary representation of the result
        """
        return {
            "action": self.action.value,
            "status": self.status.value,
            "memory_freed_mb": self.memory_freed_mb,
            "duration_seconds": self.duration_seconds,
            "message": self.message,
            "details": self.details,
            "error": self.error,
            "timestamp": self.timestamp,
        }

    def __str__(self) -> str:
        """Get human-readable string representation."""
        status_emoji = {
            ActionResultStatus.SUCCESS: "✓",
            ActionResultStatus.PARTIAL: "⚠",
            ActionResultStatus.FAILED: "✗",
            ActionResultStatus.CANCELLED: "⊘",
        }.get(self.status, "?")

        return (
            f"{status_emoji} {self.action.value}: {self.status.value.upper()} - "
            f"Freed {self.memory_freed_mb:.1f}MB in {self.duration_seconds:.2f}s"
        )


# =============================================================================
# Action Data Class
# =============================================================================
@dataclass
class Action:
    """A memory management action to be executed.

    Attributes:
        type: Type of action to execute
        priority: Priority for execution (higher = earlier)
        kwargs: Additional arguments for the action
        estimated_freed_mb: Estimated memory to be freed
        enqueued_at: Timestamp when action was enqueued
        id: Unique identifier for the action
    """
    type: ActionType
    priority: int = 5
    kwargs: Dict[str, Any] = field(default_factory=dict)
    estimated_freed_mb: float = 0.0
    enqueued_at: float = field(default_factory=time.time)
    id: Optional[int] = None

    def __lt__(self, other):
        """Compare actions for priority queue (higher priority first)."""
        return self.priority > other.priority


# =============================================================================
# Action Executor Interface
# =============================================================================
class ActionExecutor(ABC):
    """Abstract base class for action executors.

    Each action type has its own executor that knows how to execute
    that specific action and measure its results.
    """

    @abstractmethod
    def execute(self, **kwargs) -> ActionResult:
        """Execute the action.

        Args:
            **kwargs: Action-specific arguments

        Returns:
            ActionResult with execution details
        """
        pass

    @abstractmethod
    def estimate_freed_mb(self, **kwargs) -> float:
        """Estimate how much memory this action will free.

        Args:
            **kwargs: Action-specific arguments

        Returns:
            Estimated memory freed in MB
        """
        pass


# =============================================================================
# Garbage Collection Executor
# =============================================================================
class GarbageCollectionExecutor(ActionExecutor):
    """Executor for garbage collection action."""

    def execute(self, **kwargs) -> ActionResult:
        """Execute Python garbage collection.

        Returns:
            ActionResult with collection details
        """
        start_time = time.time()

        try:
            # Get memory before
            import gc as gc_module
            gc_counts_before = gc_module.get_count()

            # Collect all generations
            collected = gc_module.collect()

            # Get memory after
            gc_counts_after = gc_module.get_count()

            duration = time.time() - start_time

            # Estimate memory freed (heuristic: 1 object ≈ 1KB on average)
            estimated_freed_mb = min(collected * 0.001, 100.0)  # Cap at 100MB

            return ActionResult(
                action=ActionType.GARBAGE_COLLECTION,
                status=ActionResultStatus.SUCCESS,
                memory_freed_mb=estimated_freed_mb,
                duration_seconds=duration,
                message=f"Garbage collection completed: {collected} objects collected",
                details={
                    "objects_collected": collected,
                    "gc_counts_before": gc_counts_before,
                    "gc_counts_after": gc_counts_after,
                },
            )

        except Exception as e:
            duration = time.time() - start_time
            logger.error(f"Garbage collection failed: {e}")

            return ActionResult(
                action=ActionType.GARBAGE_COLLECTION,
                status=ActionResultStatus.FAILED,
                memory_freed_mb=0.0,
                duration_seconds=duration,
                message="Garbage collection failed",
                error=str(e),
            )

    def estimate_freed_mb(self, **kwargs) -> float:
        """Estimate memory freed by garbage collection.

        Returns:
            Estimated memory freed in MB (heuristic)
        """
        # Heuristic: GC typically frees 5-10% of heap
        # We'll be conservative and estimate 5%
        try:
            import psutil
            process = psutil.Process()
            memory_mb = process.memory_info().rss / 1024 / 1024
            return memory_mb * 0.05
        except Exception:
            return 10.0  # Conservative default


# =============================================================================
# Action Queue
# =============================================================================
class ActionQueue:
    """Queue for memory management actions with priority handling.

    This class manages a queue of memory management actions and executes
    them in priority order. It tracks results and provides comprehensive
    error handling.

    Thread Safety:
        All methods are thread-safe and can be called from multiple threads.

    Example:
        >>> queue = ActionQueue()
        >>> queue.enqueue("garbage_collection", priority=5)
        >>> queue.enqueue("unload_projects", priority=10, project_ids=["p1"])
        >>> results = queue.execute_all()
    """

    def __init__(self):
        """Initialize the action queue."""
        self._queue: deque = deque()
        self._lock = Lock()
        self._executors: Dict[ActionType, ActionExecutor] = {}
        self._action_id_counter = 0

        # Register default executors
        self._register_default_executors()

        # Callbacks
        self._before_execute_callbacks: List[Callable[[Action], None]] = []
        self._after_execute_callbacks: List[Callable[[Action, ActionResult], None]] = []

    def _register_default_executors(self) -> None:
        """Register default action executors."""
        self._executors[ActionType.GARBAGE_COLLECTION] = GarbageCollectionExecutor()
        # Other executors will be registered when their implementations are ready

    def register_executor(self, action_type: ActionType, executor: ActionExecutor) -> None:
        """Register a custom executor for an action type.

        Args:
            action_type: Type of action this executor handles
            executor: Executor instance
        """
        with self._lock:
            self._executors[action_type] = executor
            logger.info(f"Registered executor for {action_type.value}")

    def enqueue(
        self,
        action_type: str,
        priority: int = 5,
        **kwargs
    ) -> Optional[int]:
        """Enqueue an action for execution.

        Args:
            action_type: Type of action to enqueue
            priority: Priority level (higher = earlier execution, default: 5)
            **kwargs: Action-specific arguments

        Returns:
            Action ID if enqueued successfully, None on error
        """
        try:
            # Convert string to ActionType
            try:
                action_enum = ActionType(action_type)
            except ValueError:
                logger.error(f"Invalid action type: {action_type}")
                return None

            # Get executor to estimate memory freed
            executor = self._executors.get(action_enum)
            if not executor:
                logger.warning(f"No executor registered for {action_type}")
                estimated_freed = 0.0
            else:
                estimated_freed = executor.estimate_freed_mb(**kwargs)

            # Create action
            with self._lock:
                self._action_id_counter += 1
                action = Action(
                    type=action_enum,
                    priority=priority,
                    kwargs=kwargs,
                    estimated_freed_mb=estimated_freed,
                    id=self._action_id_counter
                )

                # Insert in priority order (higher priority first)
                inserted = False
                for i, existing_action in enumerate(self._queue):
                    if action.priority > existing_action.priority:
                        self._queue.insert(i, action)
                        inserted = True
                        break

                if not inserted:
                    self._queue.append(action)

                logger.info(
                    f"Enqueued action {action.id}: {action_type} "
                    f"(priority={priority}, est_freed={estimated_freed:.1f}MB)"
                )

                return action.id

        except Exception as e:
            logger.error(f"Failed to enqueue action {action_type}: {e}")
            return None

    def dequeue(self) -> Optional[Action]:
        """Dequeue the next action (highest priority first).

        Returns:
            Next action to execute, or None if queue is empty
        """
        with self._lock:
            if not self._queue:
                return None

            action = self._queue.popleft()
            logger.debug(f"Dequeued action {action.id}: {action.type.value}")
            return action

    def peek(self) -> Optional[Action]:
        """Peek at the next action without removing it.

        Returns:
            Next action to execute, or None if queue is empty
        """
        with self._lock:
            if not self._queue:
                return None
            return self._queue[0]

    def execute_next(self) -> Optional[ActionResult]:
        """Execute the next action in the queue.

        Returns:
            ActionResult from execution, or None if queue is empty
        """
        action = self.dequeue()
        if not action:
            return None

        return self._execute_action(action)

    def execute_all(self) -> List[ActionResult]:
        """Execute all actions in the queue.

        Returns:
            List of ActionResults from all executions
        """
        results = []

        while True:
            action = self.dequeue()
            if not action:
                break

            result = self._execute_action(action)
            results.append(result)

        logger.info(f"Executed {len(results)} actions")
        return results

    def _execute_action(self, action: Action) -> ActionResult:
        """Execute a single action.

        Args:
            action: Action to execute

        Returns:
            ActionResult from execution
        """
        # Trigger before callbacks
        for callback in self._before_execute_callbacks:
            try:
                callback(action)
            except Exception as e:
                logger.error(f"Error in before-execute callback: {e}")

        # Get executor
        executor = self._executors.get(action.type)
        if not executor:
            error_msg = f"No executor registered for action type: {action.type.value}"
            logger.error(error_msg)

            result = ActionResult(
                action=action.type,
                status=ActionResultStatus.FAILED,
                memory_freed_mb=0.0,
                duration_seconds=0.0,
                message="Action execution failed",
                error=error_msg,
            )

            # Trigger after callbacks
            for callback in self._after_execute_callbacks:
                try:
                    callback(action, result)
                except Exception as e:
                    logger.error(f"Error in after-execute callback: {e}")

            return result

        # Execute action
        logger.info(f"Executing action {action.id}: {action.type.value}")
        result = executor.execute(**action.kwargs)

        # Trigger after callbacks
        for callback in self._after_execute_callbacks:
            try:
                callback(action, result)
            except Exception as e:
                logger.error(f"Error in after-execute callback: {e}")

        return result

    def get_queue_size(self) -> int:
        """Get the current size of the queue.

        Returns:
            Number of actions in the queue
        """
        with self._lock:
            return len(self._queue)

    def get_queue_summary(self) -> List[Dict[str, Any]]:
        """Get a summary of all actions in the queue.

        Returns:
            List of action summaries
        """
        with self._lock:
            return [
                {
                    "id": action.id,
                    "type": action.type.value,
                    "priority": action.priority,
                    "estimated_freed_mb": action.estimated_freed_mb,
                    "enqueued_at": action.enqueued_at,
                }
                for action in self._queue
            ]

    def clear(self) -> None:
        """Clear all actions from the queue."""
        with self._lock:
            count = len(self._queue)
            self._queue.clear()
            logger.info(f"Cleared {count} actions from queue")

    def register_before_execute_callback(self, callback: Callable[[Action], None]) -> None:
        """Register a callback to be called before each action execution.

        Args:
            callback: Function to call before executing each action
        """
        with self._lock:
            self._before_execute_callbacks.append(callback)

    def register_after_execute_callback(
        self,
        callback: Callable[[Action, ActionResult], None]
    ) -> None:
        """Register a callback to be called after each action execution.

        Args:
            callback: Function to call after executing each action
        """
        with self._lock:
            self._after_execute_callbacks.append(callback)


# =============================================================================
# Convenience Functions
# =============================================================================

# Global action queue instance
_global_queue: Optional[ActionQueue] = None
_global_queue_lock = Lock()


def get_global_queue() -> ActionQueue:
    """Get the global action queue instance.

    Returns:
        Global ActionQueue instance (creates if needed)
    """
    global _global_queue

    with _global_queue_lock:
        if _global_queue is None:
            _global_queue = ActionQueue()

        return _global_queue


def enqueue_action(
    action_type: str,
    priority: int = 5,
    **kwargs
) -> Optional[int]:
    """Enqueue an action in the global queue.

    Args:
        action_type: Type of action to enqueue
        priority: Priority level (higher = earlier execution)
        **kwargs: Action-specific arguments

    Returns:
        Action ID if enqueued successfully, None on error
    """
    queue = get_global_queue()
    return queue.enqueue(action_type, priority, **kwargs)


def execute_all_actions() -> List[ActionResult]:
    """Execute all actions in the global queue.

    Returns:
        List of ActionResults from all executions
    """
    queue = get_global_queue()
    return queue.execute_all()
