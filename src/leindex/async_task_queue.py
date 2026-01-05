"""
Async Task Queue Implementation

This module implements an asyncio-based task queue to replace RabbitMQ dependency.
Provides bounded queues, worker pools, task cancellation, and progress tracking.

Key Features:
- Bounded asyncio.Queue with priority support
- Worker pool for concurrent task processing
- Graceful shutdown and cancellation
- Progress tracking integration
- Backpressure control
"""

import asyncio
import heapq
import time
from collections import defaultdict, deque
from dataclasses import dataclass, field
from datetime import datetime
from enum import Enum
from typing import (
    Any,
    Dict,
    List,
    Optional,
    Tuple,
    TYPE_CHECKING,
    Deque
)

from .logger_config import logger
from .progress_tracker import (
    ProgressTracker,
    progress_manager
)
from .constants import (
    QUEUE_MAX_SIZE,
    QUEUE_POP_TIMEOUT,
    QUEUE_BACKPRESSURE_THRESHOLD,
    QUEUE_BACKPRESSURE_LATENCY_THRESHOLD_MS,
    QUEUE_BACKPRESSURE_RECOVERY_FACTOR,
)
from .error_handling import QueueError

if TYPE_CHECKING:
    pass


class IndexingPriority(Enum):
    """
    Priority levels for indexing operations.

    Attributes:
        CRITICAL: User-initiated, immediate attention needed
        HIGH: Active file changes
        NORMAL: Standard background indexing
        LOW: Bulk/batch operations
    """

    """Priority levels for indexing operations."""
    CRITICAL = "critical"     # User-initiated, immediate attention needed
    HIGH = "high"            # Active file changes
    NORMAL = "normal"         # Standard background indexing
    LOW = "low"              # Bulk/batch operations

    @property
    def numeric_value(self) -> int:
        """Get numeric priority value (lower = higher priority)."""
        return {
            IndexingPriority.CRITICAL: 0,
            IndexingPriority.HIGH: 1,
            IndexingPriority.NORMAL: 2,
            IndexingPriority.LOW: 3,
        }[self]

    @classmethod
    def from_string(cls, value: str) -> 'IndexingPriority':
        """Convert string to IndexingPriority, defaulting to NORMAL."""
        try:
            return cls(value.lower())
        except (ValueError, AttributeError):
            return cls.NORMAL


@dataclass(order=True)
class PrioritizedTask:
    """
    A prioritized task for use with heapq.

    Lower priority value = higher priority (CRITICAL=0, HIGH=1, NORMAL=2, LOW=3).
    Uses timestamp as tiebreaker for FIFO ordering within same priority.

    Attributes:
        sort_key: Priority tuple for heap ordering (priority, timestamp)
        task_id: Unique task identifier
        file_path: Path to the file
        operation_type: Type of operation (index, delete, update)
        timestamp: ISO format timestamp
        metadata: Optional metadata dictionary
        retry_count: Number of retry attempts
    """

    """
    A prioritized task for use with heapq.

    Lower priority value = higher priority (CRITICAL=0, HIGH=1, NORMAL=2, LOW=3)
    Uses timestamp as tiebreaker for FIFO ordering within same priority.
    """
    # Priority tuple for heap ordering (lower = higher priority)
    sort_key: Tuple[int, float] = field(compare=True)

    # Task data (not included in comparison)
    task_id: str = field(compare=False)
    file_path: str = field(compare=False)
    operation_type: str = field(compare=False)
    timestamp: str = field(compare=False)

    # Optional metadata
    metadata: Dict[str, Any] = field(default_factory=dict, compare=False)
    retry_count: int = field(default=0, compare=False)

    @classmethod
    def create(
        cls,
        task_id: str,
        file_path: str,
        operation_type: str,
        priority: IndexingPriority = IndexingPriority.NORMAL,
        timestamp: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'PrioritizedTask':
        """
        Create a prioritized task.

        Args:
            task_id: Unique task identifier
            file_path: Path to the file
            operation_type: Type of operation (index, delete, update)
            priority: Priority level
            timestamp: ISO format timestamp (uses now if None)
            metadata: Optional metadata dict

        Returns:
            PrioritizedTask
        """
        if timestamp is None:
            timestamp = datetime.now().isoformat()

        # Create sort key: (priority, timestamp)
        ts_float = datetime.fromisoformat(timestamp).timestamp()
        sort_key = (priority.numeric_value, ts_float)

        return cls(
            sort_key=sort_key,
            task_id=task_id,
            file_path=file_path,
            operation_type=operation_type,
            timestamp=timestamp,
            metadata=metadata or {}
        )

    @property
    def priority(self) -> IndexingPriority:
        """Get the IndexingPriority enum value."""
        return IndexingPriority(list(IndexingPriority)[self.sort_key[0]])

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "task_id": self.task_id,
            "type": self.operation_type,
            "file_path": self.file_path,
            "timestamp": self.timestamp,
            "priority": self.priority.value,
            "metadata": self.metadata,
        }

    def increment_retry(self) -> 'PrioritizedTask':
        """Return a copy with incremented retry count."""
        return PrioritizedTask(
            sort_key=self.sort_key,
            task_id=self.task_id,
            file_path=self.file_path,
            operation_type=self.operation_type,
            timestamp=self.timestamp,
            metadata=self.metadata,
            retry_count=self.retry_count + 1
        )


class AsyncBoundedQueue:
    """
    Thread-safe bounded priority queue using asyncio.

    Features:
    - Heap-based priority queue with O(log n) push/pop
    - Bounded capacity with backpressure
    - FIFO ordering within same priority
    - Statistics tracking

    Attributes:
        _heap: Internal heap storage for tasks
        _max_size: Maximum total queue size
        _max_per_priority: Maximum items per priority level
        _priority_counts: Count of tasks per priority
    """

    """
    Thread-safe bounded priority queue using asyncio.

    Features:
    - Heap-based priority queue with O(log n) push/pop
    - Bounded capacity with backpressure
    - FIFO ordering within same priority
    - Statistics tracking
    """

    def __init__(
        self,
        max_size: int = 10000,
        max_per_priority: Optional[int] = None,
        max_memory_bytes: int = 100 * 1024 * 1024  # 100MB default
    ):
        """
        Initialize the bounded priority queue.

        Args:
            max_size: Maximum total queue size (hard limit)
            max_per_priority: Maximum items per priority level (soft limit)
            max_memory_bytes: Maximum memory usage in bytes (hard limit)
        """
        self._heap: List[PrioritizedTask] = []
        self._max_size = max_size
        self._max_per_priority = max_per_priority
        self._max_memory_bytes = max_memory_bytes
        self._priority_counts: Dict[IndexingPriority, int] = defaultdict(int)
        self._total_added = 0
        self._total_popped = 0
        self._lock = asyncio.Lock()
        self._not_empty = asyncio.Condition(lock=self._lock)

    async def push(
        self,
        task_id: str,
        file_path: str,
        operation_type: str,
        priority: IndexingPriority = IndexingPriority.NORMAL,
        timestamp: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> bool:
        """
        Push a task onto the queue.

        Args:
            task_id: Unique task identifier
            file_path: Path to the file
            operation_type: Type of operation
            priority: Priority level
            timestamp: ISO format timestamp
            metadata: Optional metadata

        Returns:
            True if added, False if queue is at hard limit
        """
        async with self._lock:
            # Check capacity limits (hard limits)
            current_size = len(self._heap)
            current_memory = self._estimate_memory_usage()

            # Enforce hard limits - try to make room first
            if current_size >= self._max_size or current_memory >= self._max_memory_bytes:
                if not await self._make_room():
                    logger.warning(
                        f"Queue at hard limit (size={current_size}/{self._max_size}, "
                        f"memory={current_memory}/{self._max_memory_bytes} bytes), "
                        f"dropping task: {operation_type} for {file_path}",
                        extra={'component': 'AsyncBoundedQueue', 'action': 'hard_limit_reached',
                               'queue_size': current_size, 'memory_bytes': current_memory}
                    )
                    return False

            # Check per-priority limit (soft limit)
            if self._max_per_priority:
                if self._priority_counts[priority] >= self._max_per_priority:
                    logger.warning(
                        f"Priority {priority.value} queue full "
                        f"({self._priority_counts[priority]} >= {self._max_per_priority}), "
                        f"dropping task: {operation_type} for {file_path}",
                        extra={'component': 'AsyncBoundedQueue', 'action': 'priority_full'}
                    )
                    return False

            # Create and push task
            task = PrioritizedTask.create(
                task_id=task_id,
                file_path=file_path,
                operation_type=operation_type,
                priority=priority,
                timestamp=timestamp,
                metadata=metadata
            )

            heapq.heappush(self._heap, task)
            self._priority_counts[priority] += 1
            self._total_added += 1

            # Notify waiting consumers
            self._not_empty.notify()

            logger.debug(
                f"Queued {priority.value} priority task: {operation_type} for {file_path}",
                extra={'component': 'AsyncBoundedQueue', 'action': 'push', 'priority': priority.value}
            )

            return True

    async def pop(self, block: bool = True, timeout: Optional[float] = None) -> Optional[PrioritizedTask]:
        """
        Pop the highest priority task from the queue.

        Args:
            block: Whether to block if queue is empty
            timeout: Maximum time to wait (requires block=True)

        Returns:
            PrioritizedTask or None if empty/timeout
        """
        async with self._not_empty:
            if not self._heap:
                if not block:
                    return None

                # Wait for task with timeout
                try:
                    await asyncio.wait_for(
                        self._not_empty.wait(),
                        timeout=timeout
                    )
                except asyncio.TimeoutError:
                    return None

                if not self._heap:
                    return None

            task = heapq.heappop(self._heap)
            self._priority_counts[task.priority] -= 1
            self._total_popped += 1

            logger.debug(
                f"Dequeued {task.priority.value} priority task: "
                f"{task.operation_type} for {task.file_path}",
                extra={'component': 'AsyncBoundedQueue', 'action': 'pop', 'priority': task.priority.value}
            )

            return task

    async def peek(self) -> Optional[PrioritizedTask]:
        """
        Peek at the highest priority task without removing it.

        Returns:
            PrioritizedTask or None if empty
        """
        async with self._lock:
            if self._heap:
                return self._heap[0]
            return None

    async def clear(self) -> None:
        """Clear all items from the queue."""
        async with self._lock:
            dropped = len(self._heap)
            self._heap.clear()
            for priority in self._priority_counts:
                self._priority_counts[priority] = 0
            logger.info(
                f"Cleared {dropped} items from priority queue",
                extra={'component': 'AsyncBoundedQueue', 'action': 'clear', 'dropped': dropped}
            )

    async def _drop_low_priority_items(self, count: int = 1) -> None:
        """Drop the lowest priority items from the queue."""
        await self._drop_by_priority(IndexingPriority.LOW, count)

    async def _make_room(self) -> bool:
        """
        Try to make room by dropping low-priority items.

        Returns:
            True if room was made, False if no room could be made
        """
        # Try dropping LOW priority items first
        if self._priority_counts[IndexingPriority.LOW] > 0:
            await self._drop_low_priority_items(count=1)
            return True

        # Then try NORMAL priority items
        if self._priority_counts[IndexingPriority.NORMAL] > 0:
            await self._drop_by_priority(IndexingPriority.NORMAL, count=1)
            logger.warning(
                "Dropping NORMAL priority tasks to make room",
                extra={'component': 'AsyncBoundedQueue', 'action': 'drop_normal_priority'}
            )
            return True

        # Then try HIGH priority items
        if self._priority_counts[IndexingPriority.HIGH] > 0:
            await self._drop_by_priority(IndexingPriority.HIGH, count=1)
            logger.warning(
                "Dropping HIGH priority tasks to make room",
                extra={'component': 'AsyncBoundedQueue', 'action': 'drop_high_priority'}
            )
            return True

        # Cannot make room - queue is full of CRITICAL tasks
        return False

    async def _drop_by_priority(self, priority: IndexingPriority, count: int = 1):
        """
        Drop items of specific priority.

        Args:
            priority: The priority level to drop
            count: Number of items to drop
        """
        dropped = 0
        new_heap = []

        for task in self._heap:
            if dropped < count and task.priority == priority:
                self._priority_counts[task.priority] -= 1
                dropped += 1
            else:
                new_heap.append(task)

        heapq.heapify(new_heap)
        self._heap = new_heap

        logger.debug(
            f"Dropped {dropped} {priority.value} priority items from queue",
            extra={'component': 'AsyncBoundedQueue', 'action': 'drop_by_priority',
                   'priority': priority.value, 'count': dropped}
        )

    def _estimate_memory_usage(self) -> int:
        """
        Estimate memory usage in bytes.

        Uses a heuristic based on task count. Each task is estimated at ~1KB
        including the task object, metadata, and overhead.

        Returns:
            Estimated memory usage in bytes
        """
        # Base estimate: ~1KB per task (includes object overhead, strings, metadata dict)
        bytes_per_task = 1024
        return len(self._heap) * bytes_per_task

    async def get_stats(self) -> Dict[str, Any]:
        """
        Get queue statistics.

        Returns:
            Dictionary with queue statistics
        """
        async with self._lock:
            memory_usage = self._estimate_memory_usage()
            return {
                "queue_size": len(self._heap),
                "total_added": self._total_added,
                "total_popped": self._total_popped,
                "priority_counts": {
                    priority.value: self._priority_counts[priority]
                    for priority in IndexingPriority
                },
                "max_size": self._max_size,
                "max_per_priority": self._max_per_priority,
                "max_memory_bytes": self._max_memory_bytes,
                "estimated_memory_bytes": memory_usage,
                "memory_utilization_percent": round(memory_usage / self._max_memory_bytes * 100, 2) if self._max_memory_bytes > 0 else 0,
                "utilization_percent": round(len(self._heap) / self._max_size * 100, 2) if self._max_size > 0 else 0,
            }

    async def remove_by_path(self, file_path: str) -> int:
        """
        Remove all tasks for a specific file path.

        Args:
            file_path: The file path to remove tasks for

        Returns:
            Number of tasks removed
        """
        async with self._lock:
            original_size = len(self._heap)
            new_heap = []

            for task in self._heap:
                if task.file_path != file_path:
                    new_heap.append(task)
                else:
                    self._priority_counts[task.priority] -= 1

            heapq.heapify(new_heap)
            self._heap = new_heap

            removed = original_size - len(self._heap)
            if removed > 0:
                logger.debug(
                    f"Removed {removed} tasks for {file_path} from queue",
                    extra={'component': 'AsyncBoundedQueue', 'action': 'remove_by_path', 'file_path': file_path, 'count': removed}
                )

            return removed


class AsyncTaskProcessor:
    """
    Processes tasks from an async queue with a worker pool.

    Features:
    - Configurable worker pool size
    - Graceful shutdown
    - Task cancellation support
    - Error handling and retry logic
    - Progress tracking integration

    Attributes:
        queue: The bounded queue to process tasks from
        worker_count: Number of worker tasks
        max_retries: Maximum number of retries for failed tasks
        progress_tracker: Optional progress tracker for reporting
    """

    """
    Processes tasks from an async queue with a worker pool.

    Features:
    - Configurable worker pool size
    - Graceful shutdown
    - Task cancellation support
    - Error handling and retry logic
    - Progress tracking integration
    """

    def __init__(
        self,
        queue: AsyncBoundedQueue,
        worker_count: int = 4,
        max_retries: int = 3,
        progress_tracker: Optional[ProgressTracker] = None
    ):
        """
        Initialize the task processor.

        Args:
            queue: The bounded queue to process tasks from
            worker_count: Number of worker tasks
            max_retries: Maximum number of retries for failed tasks
            progress_tracker: Optional progress tracker for reporting
        """
        self.queue = queue
        self.worker_count = worker_count
        self.max_retries = max_retries
        self.progress_tracker = progress_tracker or progress_manager

        self._workers: List[asyncio.Task] = []
        self._running = False
        self._shutdown_event = asyncio.Event()
        self._task_counter = 0
        self._lock = asyncio.Lock()
        self._processing_stats = {
            "total_processed": 0,
            "total_failed": 0,
            "total_retried": 0,
        }

    async def start(self) -> None:
        """Start the worker pool."""
        if self._running:
            logger.warning("Task processor already running",
                          extra={'component': 'AsyncTaskProcessor', 'action': 'already_running'})
            return

        self._running = True
        self._shutdown_event.clear()

        # Create worker tasks
        for i in range(self.worker_count):
            worker = asyncio.create_task(self._worker(f"worker-{i}"))
            self._workers.append(worker)

        logger.info(f"Started {self.worker_count} worker tasks",
                    extra={'component': 'AsyncTaskProcessor', 'action': 'started', 'worker_count': self.worker_count})

    async def stop(self, timeout: float = 30.0):
        """
        Stop the worker pool gracefully.

        Args:
            timeout: Maximum time to wait for workers to finish
        """
        if not self._running:
            return

        logger.info("Stopping task processor...",
                    extra={'component': 'AsyncTaskProcessor', 'action': 'stopping'})

        # Signal shutdown
        self._running = False
        self._shutdown_event.set()

        # Wait for workers to finish with timeout
        if self._workers:
            try:
                await asyncio.wait_for(
                    asyncio.gather(*self._workers, return_exceptions=True),
                    timeout=timeout
                )
            except asyncio.TimeoutError:
                logger.warning("Worker shutdown timed out, cancelling workers",
                              extra={'component': 'AsyncTaskProcessor', 'action': 'shutdown_timeout'})
                # Cancel all workers
                for worker in self._workers:
                    if not worker.done():
                        worker.cancel()

                # Wait for cancellation to complete
                try:
                    await asyncio.wait_for(
                        asyncio.gather(*self._workers, return_exceptions=True),
                        timeout=5.0
                    )
                except asyncio.TimeoutError:
                    logger.error("Worker cancellation timed out",
                                 extra={'component': 'AsyncTaskProcessor', 'action': 'cancel_timeout'})

        # Clear workers list only after all are stopped
        self._workers.clear()
        logger.info("Task processor stopped",
                    extra={'component': 'AsyncTaskProcessor', 'action': 'stopped'})

    async def _worker(self, worker_name: str) -> None:
        """
        Worker coroutine that processes tasks from the queue.

        Args:
            worker_name: Name of the worker for logging
        """
        logger.debug(f"{worker_name} started",
                     extra={'component': 'AsyncTaskProcessor', 'action': 'worker_start', 'worker': worker_name})

        while self._running:
            try:
                # Get task from queue with timeout
                task = await self.queue.pop(block=True, timeout=QUEUE_POP_TIMEOUT)

                if task is None:
                    # Timeout occurred, check if we should continue
                    continue

                # Process the task
                await self._process_task(task, worker_name)

            except asyncio.CancelledError:
                logger.debug(f"{worker_name} cancelled",
                             extra={'component': 'AsyncTaskProcessor', 'action': 'worker_cancelled', 'worker': worker_name})
                break
            except Exception as e:
                logger.error(f"{worker_name} error: {e}",
                             extra={'component': 'AsyncTaskProcessor', 'action': 'worker_error', 'worker': worker_name, 'error': str(e)})
                # Continue processing other tasks

        logger.debug(f"{worker_name} stopped",
                     extra={'component': 'AsyncTaskProcessor', 'action': 'worker_stop', 'worker': worker_name})

    async def _process_task(self, task: PrioritizedTask, worker_name: str) -> None:
        """
        Process a single task.

        This method should be overridden in subclasses for specific processing logic.
        The base implementation just logs the task.

        Args:
            task: The task to process
            worker_name: Name of the worker processing the task
        """
        start_time = time.time()

        try:
            logger.info(f"{worker_name} processing task: {task.operation_type} for {task.file_path}",
                        extra={'component': 'AsyncTaskProcessor', 'action': 'processing', 'worker': worker_name, 'task_id': task.task_id})

            # Simulate processing (override in subclass)
            await asyncio.sleep(0.1)

            processing_time = time.time() - start_time
            async with self._lock:
                self._processing_stats["total_processed"] += 1

            logger.info(f"{worker_name} completed task: {task.task_id} in {processing_time:.2f}s",
                        extra={'component': 'AsyncTaskProcessor', 'action': 'completed', 'worker': worker_name, 'task_id': task.task_id, 'duration': processing_time})

        except Exception as e:
            processing_time = time.time() - start_time
            logger.error(f"{worker_name} failed to process task {task.task_id}: {e}",
                         extra={'component': 'AsyncTaskProcessor', 'action': 'failed', 'worker': worker_name, 'task_id': task.task_id, 'error': str(e)})

            async with self._lock:
                self._processing_stats["total_failed"] += 1

            # Retry logic if under max retries
            if task.retry_count < self.max_retries:
                retry_task = task.increment_retry()
                await self.queue.push(
                    task_id=retry_task.task_id,
                    file_path=retry_task.file_path,
                    operation_type=retry_task.operation_type,
                    priority=retry_task.priority,
                    timestamp=retry_task.timestamp,
                    metadata=retry_task.metadata
                )
                async with self._lock:
                    self._processing_stats["total_retried"] += 1
                logger.info(f"Retried task {task.task_id} (attempt {retry_task.retry_count}/{self.max_retries})",
                            extra={'component': 'AsyncTaskProcessor', 'action': 'retried', 'task_id': task.task_id, 'retry_count': retry_task.retry_count})

    async def get_stats(self) -> Dict[str, Any]:
        """
        Get processing statistics.

        Returns:
            Dictionary with processing statistics
        """
        queue_stats = await self.queue.get_stats()

        async with self._lock:
            return {
                **queue_stats,
                "worker_count": self.worker_count,
                "is_running": self._running,
                "total_processed": self._processing_stats["total_processed"],
                "total_failed": self._processing_stats["total_failed"],
                "total_retried": self._processing_stats["total_retried"],
            }

    async def wait_for_completion(self, timeout: Optional[float] = None):
        """
        Wait for all tasks in the queue to be processed.

        Args:
            timeout: Maximum time to wait (None = wait indefinitely)
        """
        start_time = time.time()

        while True:
            stats = await self.get_stats()
            queue_size = stats["queue_size"]

            if queue_size == 0:
                logger.info("All tasks processed",
                            extra={'component': 'AsyncTaskProcessor', 'action': 'all_completed'})
                break

            if timeout and (time.time() - start_time) >= timeout:
                logger.warning(f"Wait for completion timed out with {queue_size} tasks remaining",
                              extra={'component': 'AsyncTaskProcessor', 'action': 'wait_timeout', 'remaining': queue_size})
                break

            # Wait a bit before checking again
            await asyncio.sleep(0.5)


class BackpressureController:
    """
    Implements backpressure control for the async indexing pipeline.

    Backpressure prevents overwhelming the system by:
    1. Monitoring queue depth and processing latency
    2. Throttling new operations when overloaded
    3. Prioritizing critical operations
    4. Providing feedback to producers

    Attributes:
        queue_threshold: Queue depth threshold for backpressure
        latency_threshold_ms: Processing latency threshold in milliseconds
        recovery_factor: Recovery threshold for lifting backpressure
    """

    def __init__(
        self,
        queue_threshold: int = QUEUE_BACKPRESSURE_THRESHOLD,
        latency_threshold_ms: int = QUEUE_BACKPRESSURE_LATENCY_THRESHOLD_MS,
        recovery_factor: float = QUEUE_BACKPRESSURE_RECOVERY_FACTOR
    ) -> None:
        """
        Initialize the backpressure controller.

        Args:
            queue_threshold: Queue depth threshold (default: 1000)
            latency_threshold_ms: Latency threshold in milliseconds (default: 5000)
            recovery_factor: Recovery factor 0.0-1.0 (default: 0.8)

        Note:
            Backpressure is activated when either queue depth OR latency
            exceeds their respective thresholds. It's deactivated when both
            metrics drop below threshold * recovery_factor.
        """
        self.queue_threshold = queue_threshold
        self.latency_threshold_ms = latency_threshold_ms
        self.recovery_factor = recovery_factor
        self._queue_depths: Dict[str, int] = defaultdict(int)
        self._processing_latencies: Deque[float] = deque(maxlen=100)
        self._lock = asyncio.Lock()

    async def record_queue_depth(self, queue_name: str, depth: int) -> None:
        """
        Record the current depth of a queue.

        Args:
            queue_name: Name of the queue
            depth: Current queue depth
        """
        async with self._lock:
            self._queue_depths[queue_name] = depth

    async def record_processing_latency(self, latency_ms: float) -> None:
        """
        Record a processing latency measurement.

        Args:
            latency_ms: Processing latency in milliseconds
        """
        async with self._lock:
            self._processing_latencies.append(latency_ms)

    async def should_throttle(self) -> bool:
        """
        Determine if new operations should be throttled.

        Returns:
            True if backpressure should be applied, False otherwise

        Note:
            Throttling occurs when queue depth OR latency exceeds threshold.
            Uses separate checks to provide clear logging for each condition.
        """
        async with self._lock:
            # Check queue depths
            max_depth = max(self._queue_depths.values()) if self._queue_depths else 0
            if max_depth > self.queue_threshold:
                logger.warning(
                    f"Backpressure: Queue depth ({max_depth}) exceeds threshold ({self.queue_threshold})",
                    extra={'component': 'BackpressureController', 'action': 'throttle_queue', 'depth': max_depth}
                )
                return True

            # Check average processing latency
            if self._processing_latencies:
                avg_latency = sum(self._processing_latencies) / len(self._processing_latencies)
                if avg_latency > self.latency_threshold_ms:
                    logger.warning(
                        f"Backpressure: Average latency ({avg_latency:.0f}ms) exceeds threshold ({self.latency_threshold_ms}ms)",
                        extra={'component': 'BackpressureController', 'action': 'throttle_latency', 'latency': avg_latency}
                    )
                    return True

            return False

    async def get_status(self) -> Dict[str, Any]:
        """
        Get current backpressure status.

        Returns:
            Dictionary containing:
                - queue_depths: Dictionary of queue depths by name
                - avg_processing_latency_ms: Average latency in milliseconds
                - should_throttle: Whether backpressure is active
                - queue_threshold: Configured queue threshold
                - latency_threshold_ms: Configured latency threshold
        """
        async with self._lock:
            avg_latency = (
                sum(self._processing_latencies) / len(self._processing_latencies)
                if self._processing_latencies
                else 0
            )
            return {
                "queue_depths": dict(self._queue_depths),
                "avg_processing_latency_ms": avg_latency,
                "should_throttle": await self.should_throttle(),
                "queue_threshold": self.queue_threshold,
                "latency_threshold_ms": self.latency_threshold_ms
            }
