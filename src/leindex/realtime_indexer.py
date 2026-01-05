"""
DEPRECATED: RabbitMQ-based Realtime Indexer

This module is deprecated and will be removed in a future version.
Please use the new asyncio-based implementation in async_indexer.py instead.

The new implementation provides:
- No external dependencies (no RabbitMQ required)
- Better performance with native Python asyncio
- Simplified deployment and operations
- Same features with cleaner architecture

Migration guide:
Replace:
  from leindex.realtime_indexer import RealtimeIndexer, RabbitMQProducer, RabbitMQConsumer
With:
  from leindex.async_indexer import AsyncRealtimeIndexer
"""

import warnings
warnings.warn(
    "RabbitMQ-based RealtimeIndexer is deprecated. Use AsyncRealtimeIndexer from async_indexer.py instead.",
    DeprecationWarning,
    stacklevel=2
)

try:
    import pika
except ImportError:
    pika = None
    warnings.warn(
        "pika (RabbitMQ client) is no longer a dependency. Please use AsyncRealtimeIndexer instead.",
        ImportWarning,
        stacklevel=2
    )

import threading
import json
import time
import heapq
from collections import defaultdict, deque
from datetime import datetime
from enum import Enum
from typing import Dict, Any, List, Optional, Literal, Deque, Tuple
from dataclasses import dataclass, field
from elasticsearch import Elasticsearch
from .content_extractor import ContentExtractor
from .constants import ES_INDEX_NAME
from .logger_config import logger # Import the centralized logger

IndexingOperation = Dict[str, Any]


# ============================================================================
# PRODUCT.MD ALIGNMENT - Improved Real-Time Indexing with Performance Optimization
# ============================================================================

class IndexingPriority(Enum):
    """
    Priority levels for indexing operations.

    PRODUCT.MD REQUIREMENT:
    -----------------------
    "Improved Real-Time Indexing: Optimize the efficiency and reliability
    of the asynchronous indexing pipeline."
    """
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
class PrioritizedIndexingOperation:
    """
    A prioritized indexing operation for use with heapq.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Prioritized Indexing Queue: Implement actual priority queue for
    CRITICAL/HIGH/NORMAL/LOW priority files"

    Lower priority value = higher priority (CRITICAL=0, HIGH=1, NORMAL=2, LOW=3)
    Uses timestamp as tiebreaker for FIFO ordering within same priority.
    """
    # Priority tuple for heap ordering (lower = higher priority)
    sort_key: Tuple[int, float] = field(compare=True)

    # Operation data (not included in comparison)
    file_path: str = field(compare=False)
    operation_type: Literal["index", "delete", "update"] = field(compare=False)
    timestamp: str = field(compare=False)

    # Optional metadata
    metadata: Dict[str, Any] = field(default_factory=dict, compare=False)
    retry_count: int = field(default=0, compare=False)

    @classmethod
    def create(
        cls,
        file_path: str,
        operation_type: Literal["index", "delete", "update"],
        priority: IndexingPriority = IndexingPriority.NORMAL,
        timestamp: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> 'PrioritizedIndexingOperation':
        """
        Create a prioritized operation.

        Args:
            file_path: Path to the file
            operation_type: Type of operation (index, delete, update)
            priority: Priority level
            timestamp: ISO format timestamp (uses now if None)
            metadata: Optional metadata dict

        Returns:
            PrioritizedIndexingOperation
        """
        if timestamp is None:
            timestamp = datetime.now().isoformat()

        # Create sort key: (priority, timestamp)
        # Lower priority value = higher priority
        # Timestamp ensures FIFO ordering within same priority
        ts_float = datetime.fromisoformat(timestamp).timestamp()
        sort_key = (priority.numeric_value, ts_float)

        return cls(
            sort_key=sort_key,
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
            "type": self.operation_type,
            "file_path": self.file_path,
            "timestamp": self.timestamp,
            "priority": self.priority.value,
            "metadata": self.metadata,
        }

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'PrioritizedIndexingOperation':
        """Create from dictionary (for deserialization)."""
        priority = IndexingPriority.from_string(data.get("priority", "normal"))
        return cls.create(
            file_path=data.get("file_path", ""),
            operation_type=data.get("type", "index"),
            priority=priority,
            timestamp=data.get("timestamp"),
            metadata=data.get("metadata", {})
        )

    def increment_retry(self) -> 'PrioritizedIndexingOperation':
        """Return a copy with incremented retry count."""
        return PrioritizedIndexingOperation(
            sort_key=self.sort_key,
            file_path=self.file_path,
            operation_type=self.operation_type,
            timestamp=self.timestamp,
            metadata=self.metadata,
            retry_count=self.retry_count + 1
        )


class PrioritizedIndexingQueue:
    """
    Thread-safe priority queue for indexing operations.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Prioritized Indexing Queue: Implement actual priority queue for
    CRITICAL/HIGH/NORMAL/LOW priority files"

    Features:
    - Heap-based priority queue with O(log n) push/pop
    - Thread-safe operations
    - Priority levels: CRITICAL > HIGH > NORMAL > LOW
    - FIFO ordering within same priority
    - Configurable capacity limits
    - Statistics tracking
    """

    def __init__(
        self,
        max_size: int = 10000,
        max_per_priority: Optional[int] = None
    ):
        """
        Initialize the priority queue.

        Args:
            max_size: Maximum total queue size (soft limit)
            max_per_priority: Maximum items per priority level (soft limit)
        """
        self._heap: List[PrioritizedIndexingOperation] = []
        self._lock = threading.Lock()
        self._max_size = max_size
        self._max_per_priority = max_per_priority
        self._priority_counts: Dict[IndexingPriority, int] = defaultdict(int)
        self._total_added = 0
        self._total_popped = 0

    def push(
        self,
        file_path: str,
        operation_type: Literal["index", "delete", "update"],
        priority: IndexingPriority = IndexingPriority.NORMAL,
        timestamp: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> bool:
        """
        Push an operation onto the queue.

        Args:
            file_path: Path to the file
            operation_type: Type of operation
            priority: Priority level
            timestamp: ISO format timestamp
            metadata: Optional metadata

        Returns:
            True if added, False if queue is full
        """
        with self._lock:
            # Check capacity limits
            if len(self._heap) >= self._max_size:
                # Drop LOW priority items if queue is full
                if self._priority_counts[IndexingPriority.LOW] > 0:
                    self._drop_low_priority_items(count=1)
                else:
                    logger.warning(
                        f"Priority queue full ({len(self._heap)} >= {self._max_size}), "
                        f"dropping operation: {operation_type} for {file_path}"
                    )
                    return False

            # Check per-priority limit
            if self._max_per_priority:
                if self._priority_counts[priority] >= self._max_per_priority:
                    logger.warning(
                        f"Priority {priority.value} queue full "
                        f"({self._priority_counts[priority]} >= {self._max_per_priority}), "
                        f"dropping operation: {operation_type} for {file_path}"
                    )
                    return False

            # Create and push operation
            operation = PrioritizedIndexingOperation.create(
                file_path=file_path,
                operation_type=operation_type,
                priority=priority,
                timestamp=timestamp,
                metadata=metadata
            )

            heapq.heappush(self._heap, operation)
            self._priority_counts[priority] += 1
            self._total_added += 1

            logger.debug(
                f"Queued {priority.value} priority operation: {operation_type} for {file_path}",
                extra={'component': 'PrioritizedIndexingQueue', 'action': 'push', 'priority': priority.value}
            )

            return True

    def pop(self, block: bool = False, timeout: Optional[float] = None) -> Optional[PrioritizedIndexingOperation]:
        """
        Pop the highest priority operation from the queue.

        Args:
            block: Whether to block if queue is empty (not fully implemented)
            timeout: Maximum time to wait (requires block=True)

        Returns:
            PrioritizedIndexingOperation or None if empty
        """
        with self._lock:
            if not self._heap:
                if block:
                    # Simple spin-wait implementation
                    # For production, consider using threading.Condition
                    start = time.time()
                    while not self._heap and (timeout is None or time.time() - start < timeout):
                        time.sleep(0.01)
                    if not self._heap:
                        return None

                return None

            operation = heapq.heappop(self._heap)
            self._priority_counts[operation.priority] -= 1
            self._total_popped += 1

            logger.debug(
                f"Dequeued {operation.priority.value} priority operation: "
                f"{operation.operation_type} for {operation.file_path}",
                extra={'component': 'PrioritizedIndexingQueue', 'action': 'pop', 'priority': operation.priority.value}
            )

            return operation

    def peek(self) -> Optional[PrioritizedIndexingOperation]:
        """
        Peek at the highest priority operation without removing it.

        Returns:
            PrioritizedIndexingOperation or None if empty
        """
        with self._lock:
            if self._heap:
                return self._heap[0]
            return None

    def clear(self):
        """Clear all items from the queue."""
        with self._lock:
            dropped = len(self._heap)
            self._heap.clear()
            for priority in self._priority_counts:
                self._priority_counts[priority] = 0
            logger.info(
                f"Cleared {dropped} items from priority queue",
                extra={'component': 'PrioritizedIndexingQueue', 'action': 'clear', 'dropped': dropped}
            )

    def _drop_low_priority_items(self, count: int = 1):
        """Drop the lowest priority items from the queue."""
        dropped = 0
        new_heap = []

        for op in self._heap:
            if dropped < count and op.priority == IndexingPriority.LOW:
                self._priority_counts[op.priority] -= 1
                dropped += 1
            else:
                new_heap.append(op)

        heapq.heapify(new_heap)
        self._heap = new_heap

        logger.debug(
            f"Dropped {dropped} LOW priority items from queue",
            extra={'component': 'PrioritizedIndexingQueue', 'action': 'drop_low_priority', 'count': dropped}
        )

    def get_stats(self) -> Dict[str, Any]:
        """
        Get queue statistics.

        Returns:
            Dictionary with queue statistics
        """
        with self._lock:
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
                "utilization_percent": round(len(self._heap) / self._max_size * 100, 2) if self._max_size > 0 else 0,
            }

    def get_items_by_priority(self, priority: IndexingPriority) -> List[PrioritizedIndexingOperation]:
        """
        Get all operations of a specific priority level (without removing them).

        Args:
            priority: The priority level to filter by

        Returns:
            List of operations with the specified priority
        """
        with self._lock:
            return [op for op in self._heap if op.priority == priority]

    def remove_by_path(self, file_path: str) -> int:
        """
        Remove all operations for a specific file path.

        Args:
            file_path: The file path to remove operations for

        Returns:
            Number of operations removed
        """
        with self._lock:
            original_size = len(self._heap)
            new_heap = []

            for op in self._heap:
                if op.file_path != file_path:
                    new_heap.append(op)
                else:
                    self._priority_counts[op.priority] -= 1

            heapq.heapify(new_heap)
            self._heap = new_heap

            removed = original_size - len(self._heap)
            if removed > 0:
                logger.debug(
                    f"Removed {removed} operations for {file_path} from queue",
                    extra={'component': 'PrioritizedIndexingQueue', 'action': 'remove_by_path', 'file_path': file_path, 'count': removed}
                )

            return removed


class BatchIndexer:
    """
    Batches indexing operations for improved throughput.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Improved Real-Time Indexing: Optimize the efficiency"

    Batching improves performance by:
    1. Reducing round-trips to Elasticsearch
    2. Leveraging bulk indexing API
    3. Spreading fixed overhead across multiple operations
    """

    DEFAULT_BATCH_SIZE = 50
    DEFAULT_BATCH_TIMEOUT = 5.0  # seconds
    MAX_BATCH_SIZE = 500

    def __init__(
        self,
        es_client: Elasticsearch,
        index_name: str,
        batch_size: int = DEFAULT_BATCH_SIZE,
        batch_timeout: float = DEFAULT_BATCH_TIMEOUT
    ):
        self.es_client = es_client
        self.index_name = index_name
        self.batch_size = min(batch_size, self.MAX_BATCH_SIZE)
        self.batch_timeout = batch_timeout
        self._batch: List[Dict[str, Any]] = []
        self._batch_lock = threading.Lock()
        self._last_flush = time.time()

    def add_operation(self, operation: Dict[str, Any]) -> bool:
        """
        Add an operation to the current batch.

        Returns True if batch was flushed, False otherwise.
        """
        with self._batch_lock:
            self._batch.append(operation)

            # Check if we should flush
            current_time = time.time()
            should_flush = (
                len(self._batch) >= self.batch_size or
                (current_time - self._last_flush) >= self.batch_timeout
            )

            if should_flush:
                return self._flush()

            return False

    def _flush(self) -> bool:
        """Flush the current batch to Elasticsearch."""
        if not self._batch:
            return True

        batch_copy = self._batch.copy()
        self._batch.clear()
        self._last_flush = time.time()

        # Prepare bulk operations
        bulk_ops = []
        for op in batch_copy:
            op_type = op.get("type", "index")
            file_path = op.get("file_path", "")

            if op_type in ("index", "update") and "document" in op:
                # Index or update operation
                bulk_ops.append({
                    "index": {
                        "_index": self.index_name,
                        "_id": file_path
                    }
                })
                bulk_ops.append(op["document"])
            elif op_type == "delete":
                # Delete operation
                bulk_ops.append({
                    "delete": {
                        "_index": self.index_name,
                        "_id": file_path
                    }
                })

        if not bulk_ops:
            return True

        try:
            # Execute bulk operation
            response = self.es_client.bulk(body=bulk_ops, request_timeout=30)

            # Check for errors
            if response.get("errors"):
                logger.warning(
                    f"Bulk indexing completed with errors. "
                    f"Took: {response.get('took', 0)}ms",
                    extra={'component': 'BatchIndexer', 'action': 'bulk_with_errors'}
                )
            else:
                logger.info(
                    f"Successfully bulk indexed {len(batch_copy)} operations. "
                    f"Took: {response.get('took', 0)}ms",
                    extra={'component': 'BatchIndexer', 'action': 'bulk_success', 'count': len(batch_copy)}
                )

            return not response.get("errors", False)

        except Exception as e:
            logger.error(
                f"Error during bulk indexing: {e}",
                extra={'component': 'BatchIndexer', 'action': 'bulk_error', 'error': str(e)}
            )
            return False

    def flush(self) -> bool:
        """Manually flush any pending operations."""
        with self._batch_lock:
            return self._flush()

    def get_pending_count(self) -> int:
        """Get the number of pending operations in the batch."""
        with self._batch_lock:
            return len(self._batch)


class BackpressureController:
    """
    Implements backpressure control for the indexing pipeline.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Improved Real-Time Indexing: Optimize the efficiency and reliability"

    Backpressure prevents overwhelming the system by:
    1. Monitoring queue depth and processing latency
    2. Throttling new operations when overloaded
    3. Prioritizing critical operations
    4. Providing feedback to producers
    """

    DEFAULT_QUEUE_THRESHOLD = 1000
    DEFAULT_LATENCY_THRESHOLD_MS = 5000
    DEFAULT_RECOVERY_FACTOR = 0.8

    def __init__(
        self,
        queue_threshold: int = DEFAULT_QUEUE_THRESHOLD,
        latency_threshold_ms: int = DEFAULT_LATENCY_THRESHOLD_MS,
        recovery_factor: float = DEFAULT_RECOVERY_FACTOR
    ):
        self.queue_threshold = queue_threshold
        self.latency_threshold_ms = latency_threshold_ms
        self.recovery_factor = recovery_factor
        self._queue_depths: Dict[str, int] = defaultdict(int)
        self._processing_latencies: Deque[float] = deque(maxlen=100)
        self._lock = threading.Lock()

    def record_queue_depth(self, queue_name: str, depth: int):
        """Record the current depth of a queue."""
        with self._lock:
            self._queue_depths[queue_name] = depth

    def record_processing_latency(self, latency_ms: float):
        """Record a processing latency measurement."""
        with self._lock:
            self._processing_latencies.append(latency_ms)

    def should_throttle(self) -> bool:
        """
        Determine if new operations should be throttled.

        Returns True if backpressure should be applied.
        """
        with self._lock:
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

    def get_status(self) -> Dict[str, Any]:
        """Get current backpressure status."""
        with self._lock:
            avg_latency = (
                sum(self._processing_latencies) / len(self._processing_latencies)
                if self._processing_latencies
                else 0
            )
            return {
                "queue_depths": dict(self._queue_depths),
                "avg_processing_latency_ms": avg_latency,
                "should_throttle": self.should_throttle(),
                "queue_threshold": self.queue_threshold,
                "latency_threshold_ms": self.latency_threshold_ms
            }

class RabbitMQProducer:
    """
    A RabbitMQ producer for sending indexing requests.
    """
    def __init__(self, host: str, port: int, exchange: str, routing_key: str):
        self.host = host
        self.port = port
        self.exchange = exchange
        self.routing_key = routing_key
        self.connection: Optional[pika.BlockingConnection] = None
        self.channel: Optional[pika.channel.Channel] = None
        self._connect()
        logger.info(f"RabbitMQProducer initialized for {host}:{port}, exchange='{exchange}', routing_key='{routing_key}'",
                    extra={'component': 'RabbitMQProducer', 'host': host, 'port': port, 'exchange': exchange, 'routing_key': routing_key})

    def _connect(self):
        """Establishes a connection to RabbitMQ."""
        try:
            # Add heartbeat and connection timeout to fix pika connection issues
            credentials = pika.PlainCredentials('guest', 'guest')
            parameters = pika.ConnectionParameters(
                host=self.host,
                port=self.port,
                virtual_host='/',
                credentials=credentials,
                heartbeat=600,  # 10 minute heartbeat
                blocked_connection_timeout=300,  # 5 minute blocked connection timeout
                connection_attempts=3,
                retry_delay=5
            )
            self.connection = pika.BlockingConnection(parameters)
            self.channel = self.connection.channel()
            self.channel.exchange_declare(exchange=self.exchange, exchange_type='topic', durable=True)
            logger.info("Successfully connected to RabbitMQ and declared exchange.",
                        extra={'component': 'RabbitMQProducer', 'action': 'connect_success'})
        except pika.exceptions.AMQPConnectionError as e:
            logger.error(f"Failed to connect to RabbitMQ at {self.host}:{self.port}: {e}",
                        extra={'component': 'RabbitMQProducer', 'action': 'connect_failure', 'host': self.host, 'port': self.port, 'error': str(e)})
            self.connection = None
            self.channel = None
        except Exception as e:
            logger.error(f"An unexpected error occurred during RabbitMQ connection: {e}",
                        extra={'component': 'RabbitMQProducer', 'action': 'connect_unexpected_error', 'error': str(e)})
            self.connection = None
            self.channel = None

    def publish(self, message: Dict[str, Any]):
        """Publishes a message to the RabbitMQ exchange."""
        if not self.channel or not self.connection or self.connection.is_closed:
            logger.warning("RabbitMQ connection lost, attempting to reconnect...",
                           extra={'component': 'RabbitMQProducer', 'action': 'reconnect_attempt'})
            self._connect()
            if not self.channel:
                logger.error("Failed to reconnect to RabbitMQ, message not sent.",
                            extra={'component': 'RabbitMQProducer', 'action': 'publish_failed_reconnect'})
                return

        try:
            self.channel.basic_publish(
                exchange=self.exchange,
                routing_key=self.routing_key,
                body=json.dumps(message),
                properties=pika.BasicProperties(
                    delivery_mode=pika.spec.PERSISTENT_DELIVERY_MODE # Make message persistent
                )
            )
            logger.debug(f"Published message to RabbitMQ: {message.get('type')} for {message.get('file_path')}",
                         extra={'component': 'RabbitMQProducer', 'action': 'publish_success', 'message_type': message.get('type'), 'file_path': message.get('file_path')})
        except pika.exceptions.AMQPConnectionError as e:
            logger.error(f"Lost connection to RabbitMQ while publishing: {e}. Message not sent.",
                        extra={'component': 'RabbitMQProducer', 'action': 'publish_connection_error', 'error': str(e)})
            self.connection = None # Mark for reconnection
        except Exception as e:
            logger.error(f"Error publishing message to RabbitMQ: {e}",
                        extra={'component': 'RabbitMQProducer', 'action': 'publish_unexpected_error', 'error': str(e)})

    def close(self):
        """Closes the RabbitMQ connection."""
        if self.connection and self.connection.is_open:
            self.connection.close()
            logger.info("RabbitMQProducer connection closed.",
                        extra={'component': 'RabbitMQProducer', 'action': 'close_connection'})

class RabbitMQConsumer:
    """
    A RabbitMQ consumer for receiving and processing indexing requests.
    Runs in a dedicated thread.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Improved Real-Time Indexing: Optimize the efficiency and reliability
    of the asynchronous indexing pipeline."

    Enhanced with:
    - BatchIndexer for bulk operations
    - BackpressureController for load management
    - Processing latency tracking
    """
    def __init__(
        self,
        es_client: Elasticsearch,
        base_path: str,
        host: str,
        port: int,
        queue_name: str,
        exchange: str,
        routing_key: str,
        enable_batching: bool = True,
        batch_size: int = 50,
        enable_backpressure: bool = True
    ):
        self.es_client = es_client
        self.base_path = base_path
        self.host = host
        self.port = port
        self.queue_name = queue_name
        self.exchange = exchange
        self.routing_key = routing_key
        self.connection: Optional[pika.BlockingConnection] = None
        self.channel: Optional[pika.channel.Channel] = None
        self._stop_event = threading.Event()
        self._worker_thread: Optional[threading.Thread] = None
        self.index_name = ES_INDEX_NAME
        self.content_extractor = ContentExtractor(base_path)

        # PRODUCT.MD ALIGNMENT: Enable batch processing
        self.enable_batching = enable_batching
        self.batch_indexer = BatchIndexer(es_client, ES_INDEX_NAME, batch_size=batch_size) if enable_batching else None

        # PRODUCT.MD ALIGNMENT: Enable backpressure control
        self.enable_backpressure = enable_backpressure
        self.backpressure = BackpressureController() if enable_backpressure else None

        self._processing_count = 0
        self._processing_times: Deque[float] = deque(maxlen=100)

        # Message retry tracking to prevent infinite NACK loops
        self._message_retry_counts: Dict[str, int] = {}
        self._max_message_retries = 3

        logger.info(
            f"RabbitMQConsumer initialized for {host}:{port}, queue='{queue_name}' "
            f"(batching={enable_batching}, backpressure={enable_backpressure})",
            extra={'component': 'RabbitMQConsumer', 'host': host, 'port': port, 'queue': queue_name}
        )

    def _connect(self):
        """Establishes a connection to RabbitMQ and declares queue/exchange."""
        try:
            # Add heartbeat and connection timeout to fix pika connection issues
            credentials = pika.PlainCredentials('guest', 'guest')
            parameters = pika.ConnectionParameters(
                host=self.host,
                port=self.port,
                virtual_host='/',
                credentials=credentials,
                heartbeat=600,  # 10 minute heartbeat
                blocked_connection_timeout=300,  # 5 minute blocked connection timeout
                connection_attempts=3,
                retry_delay=5
            )
            self.connection = pika.BlockingConnection(parameters)
            self.channel = self.connection.channel()
            self.channel.exchange_declare(exchange=self.exchange, exchange_type='topic', durable=True)
            result = self.channel.queue_declare(queue=self.queue_name, durable=True)
            self.channel.queue_bind(exchange=self.exchange, queue=self.queue_name, routing_key=self.routing_key)
            logger.info(f"Successfully connected to RabbitMQ, declared queue '{self.queue_name}' and bound to exchange '{self.exchange}'.",
                        extra={'component': 'RabbitMQConsumer', 'action': 'connect_success', 'queue': self.queue_name, 'exchange': self.exchange})
            return True
        except pika.exceptions.AMQPConnectionError as e:
            logger.error(f"Failed to connect to RabbitMQ at {self.host}:{self.port}: {e}",
                        extra={'component': 'RabbitMQConsumer', 'action': 'connect_failure', 'host': self.host, 'port': self.port, 'error': str(e)})
            self.connection = None
            self.channel = None
            return False
        except Exception as e:
            logger.error(f"An unexpected error occurred during RabbitMQ consumer connection: {e}",
                        extra={'component': 'RabbitMQConsumer', 'action': 'connect_unexpected_error', 'error': str(e)})
            self.connection = None
            self.channel = None
            return False

    def _extract_content_and_metadata(self, file_path: str) -> Optional[Dict[str, Any]]:
        """
        Extracts content and basic metadata from a file using the ContentExtractor.

        CRITICAL FIX: Validate that file_path is within base_path before reading
        to prevent path traversal attacks. Also adds comprehensive error handling
        for file access issues.
        """
        # CRITICAL FIX: Validate file_path is within base_path to prevent path traversal
        import os
        from pathlib import Path

        # Constants for file validation
        MAX_FILE_SIZE_MB = 100  # Maximum file size to process (100MB)
        BINARY_FILE_SIGNATURES = {
            b'\x00\x00\x00\x18\x66\x74\x79\x70': 'mp4',  # MP4 video
            b'\x00\x00\x00\x14\x66\x74\x79\x70': 'mov',  # MOV video
            b'\x00\x00\x00\x20\x66\x74\x79\x70': 'm4v',  # M4V video
            b'\x25\x50\x44\x46': 'pdf',  # PDF (handled separately)
            b'\x50\x4B\x03\x04': 'zip',  # ZIP archive
            b'\x50\x4B\x05\x06': 'zip',  # Empty ZIP archive
            b'\x50\x4B\x07\x08': 'zip',  # Spanned ZIP archive
            b'\x1F\x8B\x08': 'gz',  # GZIP archive
            b'\x42\x5A\x68': 'bz2',  # BZIP2 archive
            b'\xFD\x37\x7A\x58\x5A\x00': 'xz',  # XZ archive
            b'\x75\x73\x74\x61\x72': 'tar',  # TAR archive
            b'\x52\x61\x72\x21': 'rar',  # RAR archive
            b'\x37\x7A\xBC\xAF\x27\x1C': '7z',  # 7Z archive
            b'\x49\x49\x2A\x00': 'tiff',  # TIFF (little-endian)
            b'\x4D\x4D\x00\x2A': 'tiff',  # TIFF (big-endian)
            b'\x42\x4D': 'bmp',  # BMP image
            b'\x47\x49\x46\x38': 'gif',  # GIF image
            b'\x89\x50\x4E\x47\x0D\x0A\x1A\x0A': 'png',  # PNG image
            b'\xFF\xD8\xFF': 'jpg',  # JPEG image
            b'\x00\x00\x01\x00': 'ico',  # ICO icon
        }

        try:
            # Resolve both paths to their absolute real paths
            # This resolves symlinks and relative path components
            real_file_path = Path(file_path).resolve()
            real_base_path = Path(self.base_path).resolve()

            # Check if the real file path is within the real base path
            try:
                real_file_path.relative_to(real_base_path)
                # Path is safe, proceed with further validation
            except ValueError:
                # relative_to raises ValueError if path is not within base
                logger.error(
                    f"SECURITY VIOLATION: File path {file_path} is not within base path {self.base_path}. "
                    f"Resolved file path: {real_file_path}, Resolved base path: {real_base_path}",
                    extra={'component': 'RabbitMQConsumer', 'action': 'path_traversal_blocked', 'file_path': file_path}
                )
                return None

            # CRITICAL FIX: Check file existence and accessibility before extraction
            if not real_file_path.exists():
                logger.warning(f"File does not exist: {real_file_path}",
                              extra={'component': 'RabbitMQConsumer', 'action': 'file_not_found', 'file_path': file_path})
                return None

            if not real_file_path.is_file():
                logger.warning(f"Path is not a file: {real_file_path}",
                              extra={'component': 'RabbitMQConsumer', 'action': 'not_a_file', 'file_path': file_path})
                return None

            # CRITICAL FIX: Check file size before processing
            try:
                file_size = real_file_path.stat().st_size
                max_size_bytes = MAX_FILE_SIZE_MB * 1024 * 1024
                if file_size > max_size_bytes:
                    logger.warning(
                        f"File {real_file_path} exceeds size limit ({file_size / (1024*1024):.2f}MB > {MAX_FILE_SIZE_MB}MB). Skipping.",
                        extra={'component': 'RabbitMQConsumer', 'action': 'file_too_large', 'file_path': file_path, 'file_size': file_size}
                    )
                    return None
            except OSError as e:
                logger.error(f"Error getting file size for {real_file_path}: {e}",
                           extra={'component': 'RabbitMQConsumer', 'action': 'file_size_error', 'error': str(e)})
                return None

            # CRITICAL FIX: Check file permissions
            if not os.access(real_file_path, os.R_OK):
                logger.warning(f"Permission denied: cannot read file {real_file_path}",
                              extra={'component': 'RabbitMQConsumer', 'action': 'permission_denied', 'file_path': file_path})
                return None

            # CRITICAL FIX: Detect binary files (except PDF and Office docs which we handle)
            file_extension = real_file_path.suffix.lower()
            skip_binary_check_extensions = {'.pdf', '.docx', '.xlsx', '.pptx', '.odt'}

            if file_extension not in skip_binary_check_extensions:
                try:
                    with open(real_file_path, 'rb') as f:
                        header = f.read(16)

                    for signature, format_name in BINARY_FILE_SIGNATURES.items():
                        if header.startswith(signature):
                            logger.info(
                                f"Detected binary file ({format_name}): {real_file_path}. Skipping content extraction.",
                                extra={'component': 'RabbitMQConsumer', 'action': 'binary_file_skipped', 'file_path': file_path, 'format': format_name}
                            )
                            return None

                    # Additional check: if null bytes are present, likely binary
                    if b'\x00' in header[:8]:
                        logger.info(
                            f"File appears to be binary (contains null bytes): {real_file_path}. Skipping.",
                            extra={'component': 'RabbitMQConsumer', 'action': 'binary_null_bytes_skipped', 'file_path': file_path}
                        )
                        return None

                except PermissionError as e:
                    logger.error(f"Permission denied when checking file header for {real_file_path}: {e}",
                               extra={'component': 'RabbitMQConsumer', 'action': 'header_check_permission_error', 'error': str(e)})
                    return None
                except OSError as e:
                    # File might be locked or have other I/O issues
                    if "locked" in str(e).lower() or "used by another process" in str(e).lower():
                        logger.warning(f"File is locked or in use: {real_file_path}. Will retry later.",
                                      extra={'component': 'RabbitMQConsumer', 'action': 'file_locked', 'file_path': file_path})
                        return None
                    logger.error(f"Error reading file header for {real_file_path}: {e}",
                               extra={'component': 'RabbitMQConsumer', 'action': 'header_read_error', 'error': str(e)})
                    return None

        except (OSError, ValueError) as e:
            logger.error(f"Error validating file path {file_path} against base {self.base_path}: {e}",
                       extra={'component': 'RabbitMQConsumer', 'action': 'path_validation_error', 'error': str(e)})
            return None

        # All validations passed, proceed with content extraction
        try:
            return self.content_extractor.extract_content(file_path)
        except PermissionError as e:
            logger.error(f"Permission error extracting content from {file_path}: {e}",
                       extra={'component': 'RabbitMQConsumer', 'action': 'extraction_permission_error', 'error': str(e)})
            return None
        except OSError as e:
            if "locked" in str(e).lower() or "used by another process" in str(e).lower():
                logger.warning(f"File is locked during content extraction: {file_path}.",
                              extra={'component': 'RabbitMQConsumer', 'action': 'extraction_file_locked', 'file_path': file_path})
            else:
                logger.error(f"OS error extracting content from {file_path}: {e}",
                           extra={'component': 'RabbitMQConsumer', 'action': 'extraction_os_error', 'error': str(e)})
            return None

    def _index_document(self, file_path: str, document: Dict[str, Any]):
        """
        Indexes or updates a document in Elasticsearch.
        Uses file_path as the document ID for idempotency.
        """
        try:
            response = self.es_client.index(
                index=self.index_name,
                id=file_path,
                document=document,
                op_type='index',
                request_timeout=10
            )
            logger.info(f"Indexed/Updated document for {file_path}: {response['result']}",
                        extra={'component': 'Elasticsearch', 'action': 'index_document', 'file_path': file_path, 'result': response['result']})
            return True
        except ConnectionError as e:
            logger.error(f"Elasticsearch connection error while indexing {file_path}: {e}. Retrying...",
                        extra={'component': 'Elasticsearch', 'action': 'index_document_connection_error', 'file_path': file_path, 'error': str(e)})
            # In a real system, implement a retry mechanism (e.g., exponential backoff)
            return False
        except Exception as e:
            logger.error(f"Error indexing document for {file_path}: {e}",
                        extra={'component': 'Elasticsearch', 'action': 'index_document_unexpected_error', 'file_path': file_path, 'error': str(e)})
            return False

    def _delete_document(self, file_path: str):
        """
        Deletes a document from Elasticsearch.
        """
        try:
            response = self.es_client.delete(
                index=self.index_name,
                id=file_path,
                ignore=[404],
                request_timeout=10
            )
            if response['result'] == 'deleted':
                logger.info(f"Deleted document for {file_path}",
                            extra={'component': 'Elasticsearch', 'action': 'delete_document', 'file_path': file_path, 'result': response['result']})
                return True
            elif response['result'] == 'not_found':
                logger.warning(f"Document for {file_path} not found in Elasticsearch (already deleted or never existed).",
                               extra={'component': 'Elasticsearch', 'action': 'delete_document_not_found', 'file_path': file_path})
                return True # Consider it successful if it's already gone
            return False
        except ConnectionError as e:
            logger.error(f"Elasticsearch connection error while deleting {file_path}: {e}. Retrying...",
                        extra={'component': 'Elasticsearch', 'action': 'delete_document_connection_error', 'file_path': file_path, 'error': str(e)})
            return False
        except Exception as e:
            logger.error(f"Error deleting document for {file_path}: {e}",
                        extra={'component': 'Elasticsearch', 'action': 'delete_document_unexpected_error', 'file_path': file_path, 'error': str(e)})
            return False

    def _process_message(self, ch, method, properties, body):
        """
        Callback function to process received messages.

        PRODUCT.MD ALIGNMENT:
        ---------------------
        "Improved Real-Time Indexing: Optimize the efficiency and reliability"

        Enhanced with:
        - Batch processing for better throughput
        - Backpressure checking before processing
        - Latency tracking for monitoring
        - Retry tracking to prevent infinite NACK loops
        """
        start_time = time.time()
        try:
            operation: IndexingOperation = json.loads(body.decode('utf-8'))
            op_type = operation.get("type")
            file_path = operation.get("file_path")

            # CRITICAL FIX: Check if this message has exceeded max retries
            retry_count = self._message_retry_counts.get(file_path, 0)
            if retry_count >= self._max_message_retries:
                logger.error(
                    f"Message for {file_path} exceeded max retries ({self._max_message_retries}), rejecting permanently.",
                    extra={'component': 'RabbitMQConsumer', 'action': 'max_retries_exceeded', 'file_path': file_path, 'retry_count': retry_count}
                )
                ch.basic_reject(delivery_tag=method.delivery_tag, requeue=False)
                self._message_retry_counts.pop(file_path, None)
                return

            # PRODUCT.MD ALIGNMENT: Check backpressure before processing
            if self.backpressure and self.backpressure.should_throttle():
                # Under backpressure, delay processing
                time.sleep(0.1)
                logger.debug(
                    f"Backpressure delay applied before processing {file_path}",
                    extra={'component': 'RabbitMQConsumer', 'action': 'backpressure_delay', 'file_path': file_path}
                )

            logger.info(f"Received message: {op_type} for {file_path}",
                        extra={'component': 'RabbitMQConsumer', 'action': 'message_received', 'op_type': op_type, 'file_path': file_path})

            success = False
            if op_type == "index" or op_type == "update":
                document_data = self._extract_content_and_metadata(file_path)
                if document_data:
                    # PRODUCT.MD ALIGNMENT: Use batch processing if enabled
                    if self.enable_batching and self.batch_indexer:
                        operation["document"] = document_data
                        self.batch_indexer.add_operation(operation)
                        success = True
                    else:
                        # Fall back to direct indexing
                        success = self._index_document(file_path, document_data)
                else:
                    logger.error(f"Failed to extract content for {file_path}, skipping indexing.",
                                extra={'component': 'RabbitMQConsumer', 'action': 'extract_content_failed', 'file_path': file_path})
            elif op_type == "delete":
                success = self._delete_document(file_path)
            else:
                logger.warning(f"Unknown indexing operation type: {op_type}",
                               extra={'component': 'RabbitMQConsumer', 'action': 'unknown_op_type', 'op_type': op_type})

            # PRODUCT.MD ALIGNMENT: Track processing latency
            processing_time = (time.time() - start_time) * 1000  # Convert to ms
            self._processing_times.append(processing_time)
            if self.backpressure:
                self.backpressure.record_processing_latency(processing_time)

            if success:
                ch.basic_ack(delivery_tag=method.delivery_tag)
                # CRITICAL FIX: Clear retry count on successful processing
                if file_path in self._message_retry_counts:
                    self._message_retry_counts.pop(file_path, None)
                logger.info(f"Acknowledged message for {file_path} (took {processing_time:.0f}ms)",
                            extra={'component': 'RabbitMQConsumer', 'action': 'message_acknowledged', 'file_path': file_path, 'latency_ms': processing_time})
            else:
                # CRITICAL FIX: Increment retry count and requeue message if processing failed
                self._message_retry_counts[file_path] = retry_count + 1
                ch.basic_nack(delivery_tag=method.delivery_tag, requeue=True)
                logger.warning(f"NACKed message for {file_path}, requeued. Retry count: {retry_count + 1}/{self._max_message_retries}",
                               extra={'component': 'RabbitMQConsumer', 'action': 'message_nacked', 'file_path': file_path, 'retry_count': retry_count + 1})

        except json.JSONDecodeError as e:
            logger.error(f"Error decoding message body: {e}. Message: {body.decode('utf-8')}. Rejecting message.",
                        extra={'component': 'RabbitMQConsumer', 'action': 'json_decode_error', 'error': str(e), 'message_body': body.decode('utf-8')})
            ch.basic_reject(delivery_tag=method.delivery_tag, requeue=False) # Don't requeue malformed messages
        except Exception as e:
            logger.error(f"Unhandled error processing message: {e}. Message: {body.decode('utf-8')}. Requeuing message.",
                        extra={'component': 'RabbitMQConsumer', 'action': 'unhandled_processing_error', 'error': str(e), 'message_body': body.decode('utf-8')})
            ch.basic_nack(delivery_tag=method.delivery_tag, requeue=True)

    def _worker(self):
        """
        Worker thread function to consume messages.

        CRITICAL FIX: ALL connection setup and consumer registration happens
        in this worker thread. Pika's BlockingConnection is NOT thread-safe,
        so we cannot share channels/connections between threads.

        This worker thread:
        1. Establishes its own connection
        2. Registers the consumer
        3. Calls start_consuming()
        """
        logger.info("RabbitMQConsumer worker thread started.",
                    extra={'component': 'RabbitMQConsumer', 'action': 'worker_start'})

        while not self._stop_event.is_set():
            # Always establish connection and register consumer in this thread
            # Pika's BlockingConnection is NOT thread-safe
            if not self._connect():
                logger.warning("Failed to connect to RabbitMQ, retrying in 5s...",
                               extra={'component': 'RabbitMQConsumer', 'action': 'worker_connect_failed'})
                time.sleep(5)
                continue

            # Register consumer in this thread (required by Pika's threading model)
            try:
                self.channel.basic_qos(prefetch_count=10)
                self.channel.basic_consume(
                    queue=self.queue_name,
                    on_message_callback=self._process_message,
                    auto_ack=False
                )
                logger.info(
                    f"Consumer registered on queue '{self.queue_name}' in worker thread",
                    extra={'component': 'RabbitMQConsumer', 'action': 'consumer_registered', 'queue': self.queue_name}
                )
            except Exception as e:
                logger.error(
                    f"Failed to register consumer: {e}",
                    extra={'component': 'RabbitMQConsumer', 'action': 'consumer_register_failed', 'error': str(e)}
                )
                time.sleep(5)
                continue

            try:
                # Start consuming messages. This call blocks until a message is received
                # or the connection is closed.
                self.channel.start_consuming()
            except pika.exceptions.AMQPConnectionError as e:
                logger.error(f"RabbitMQ connection error during consuming: {e}. Attempting to reconnect...",
                            extra={'component': 'RabbitMQConsumer', 'action': 'worker_consume_connection_error', 'error': str(e)})
                self.connection = None  # Mark for reconnection
                time.sleep(5)
            except pika.exceptions.AMQPChannelError as e:
                logger.error(f"RabbitMQ channel error: {e}. Attempting to reconnect...",
                            extra={'component': 'RabbitMQConsumer', 'action': 'worker_channel_error', 'error': str(e)})
                self.connection = None  # Mark for reconnection
                time.sleep(5)
            except Exception as e:
                logger.error(f"Unhandled error in RabbitMQConsumer worker: {e}",
                            extra={'component': 'RabbitMQConsumer', 'action': 'worker_unhandled_error', 'error': str(e)})
                time.sleep(1)  # Prevent busy-loop on persistent errors

        logger.info("RabbitMQConsumer worker thread stopped.",
                    extra={'component': 'RabbitMQConsumer', 'action': 'worker_stop'})

    def start(self):
        """
        Starts the consumer worker thread.

        CRITICAL FIX: Do NOT establish connection or register consumer in the main thread.
        Pika's BlockingConnection is NOT thread-safe, so ALL connection setup and
        consumer registration must happen in the worker thread.

        This method simply starts the worker thread and returns immediately.
        """
        if self._worker_thread is None or not self._worker_thread.is_alive():
            self._stop_event.clear()
            self._worker_thread = threading.Thread(target=self._worker, daemon=True)
            self._worker_thread.start()
            logger.info("RabbitMQConsumer worker thread initiated.",
                        extra={'component': 'RabbitMQConsumer', 'action': 'worker_started'})
        else:
            logger.warning("RabbitMQConsumer worker thread is already running.",
                          extra={'component': 'RabbitMQConsumer', 'action': 'worker_already_running'})

    def stop(self):
        """
        Stops the consumer worker thread and closes connection.

        PRODUCT.MD ALIGNMENT:
        ---------------------
        Flushes any pending batch operations before stopping to ensure
        all queued operations are completed.
        """
        if self._worker_thread and self._worker_thread.is_alive():
            logger.info("Signaling RabbitMQConsumer worker thread to stop...")

            # PRODUCT.MD ALIGNMENT: Flush any pending batch operations with timeout
            if self.enable_batching and self.batch_indexer:
                pending = self.batch_indexer.get_pending_count()
                if pending > 0:
                    logger.info(f"Flushing {pending} pending batch operations before stop...")
                    flush_complete = threading.Event()

                    def do_flush():
                        try:
                            self.batch_indexer.flush()
                        finally:
                            flush_complete.set()

                    flush_thread = threading.Thread(target=do_flush, daemon=True)
                    flush_thread.start()
                    flush_complete.wait(timeout=10)  # Max 10 second flush

                    if not flush_complete.is_set():
                        logger.warning("Flush operation timed out during stop, forcing shutdown")

            self._stop_event.set()

            # First stop consuming to prevent new messages
            if self.channel:
                try:
                    # Cancel all consumers first
                    for consumer_tag in getattr(self.channel, '_consumer_tags', []):
                        try:
                            self.channel.basic_cancel(consumer_tag)
                        except Exception as e:
                            logger.debug(f"Error canceling consumer {consumer_tag}: {e}")

                    # Then stop consuming
                    self.channel.stop_consuming()
                except Exception as e:
                    logger.warning(f"Error stopping RabbitMQ channel consuming: {e}")

            # Wait for worker thread to finish before closing connection
            self._worker_thread.join(timeout=5)

            # Now close the connection
            if self.connection and not self.connection.is_closed:
                try:
                    self.connection.close()
                except Exception as e:
                    logger.warning(f"Error closing RabbitMQ connection: {e}")

            # Final check on thread status
            if self._worker_thread.is_alive():
                logger.warning("RabbitMQConsumer worker thread did not stop gracefully.")
            else:
                logger.info("RabbitMQConsumer worker thread stopped successfully.")
        else:
            logger.info("RabbitMQConsumer worker thread is not running.")

    def get_status(self) -> Dict[str, Any]:
        """
        Get the current status of the consumer.

        PRODUCT.MD ALIGNMENT:
        ---------------------
        Provides visibility into batch processing and backpressure state.
        """
        status = {
            "is_running": self._worker_thread and self._worker_thread.is_alive(),
            "queue_name": self.queue_name,
            "enable_batching": self.enable_batching,
            "enable_backpressure": self.enable_backpressure
        }

        # Add batch indexer status
        if self.batch_indexer:
            status["batch_pending"] = self.batch_indexer.get_pending_count()

        # Add backpressure status
        if self.backpressure:
            status["backpressure"] = self.backpressure.get_status()

        # Add processing statistics
        if self._processing_times:
            status["avg_processing_latency_ms"] = sum(self._processing_times) / len(self._processing_times)
            status["max_processing_latency_ms"] = max(self._processing_times)
            status["min_processing_latency_ms"] = min(self._processing_times)

        return status

class RealtimeIndexer:
    """
    Handles real-time indexing of file changes using RabbitMQ.
    Acts as a producer to send messages to the queue.
    """
    def __init__(self, es_client: Elasticsearch, base_path: str, producer: RabbitMQProducer, consumer: RabbitMQConsumer):
        self.es_client = es_client
        self.base_path = base_path
        self.producer = producer
        self.consumer = consumer
        self.index_name = ES_INDEX_NAME
        logger.info(f"RealtimeIndexer initialized with RabbitMQ producer and consumer for index: {self.index_name}")

    def start(self):
        """Starts the RabbitMQ consumer worker thread."""
        self.consumer.start()
        logger.info("RealtimeIndexer (RabbitMQ consumer) started.")

    def stop(self):
        """Stops the RabbitMQ consumer worker thread."""
        self.consumer.stop()
        self.producer.close() # Close producer connection on stop
        logger.info("RealtimeIndexer (RabbitMQ consumer) stopped and producer connection closed.")

    def enqueue_change(self, file_path: str, change_type: Literal["index", "delete", "update"]):
        """
        Enqueues a file change operation to RabbitMQ.
        'index' and 'update' are treated similarly for upserting.
        """
        operation = {
            "type": change_type,
            "file_path": file_path,
            "timestamp": datetime.now().isoformat()
        }
        self.producer.publish(operation)
        logger.debug(f"Enqueued change to RabbitMQ: {change_type} for {file_path}")

    def wait_for_completion(self, timeout: Optional[float] = None):
        """
        Note: For RabbitMQ, 'wait_for_completion' is not directly equivalent to an in-memory queue's join().
        Messages are asynchronous. This method can be used to wait for a short period
        to allow some messages to be processed, but it won't guarantee all messages
        sent *before* this call are processed, especially if the consumer is in a separate process.
        For testing, you might need to implement a more sophisticated check or
        rely on logs/Elasticsearch state.
        """
        logger.warning("wait_for_completion for RabbitMQ is an approximation. It waits for a short period.")
        time.sleep(timeout if timeout is not None else 2) # Wait for 2 seconds by default
        logger.info("Approximated wait for RabbitMQ messages completed.")