"""
Async Indexer Implementation

This module provides an asyncio-based indexing pipeline to replace RabbitMQ.
Implements file change tracking, content extraction, and indexing with async processing.

Key Features:
- Async task queue with priority support
- Worker pool for concurrent processing
- Batching for improved throughput
- Backpressure control
- Graceful shutdown
"""

import asyncio
import time
from datetime import datetime
from typing import Dict, Any, List, Optional, Literal, Union

from .async_task_queue import (
    AsyncBoundedQueue,
    AsyncTaskProcessor,
    IndexingPriority,
    PrioritizedTask,
    BackpressureController
)
from .content_extractor import ContentExtractor
from .logger_config import logger
from .constants import (
    ASYNC_INDEXER_DEFAULT_BATCH_SIZE,
    ASYNC_INDEXER_MAX_BATCH_SIZE,
    ASYNC_INDEXER_BATCH_TIMEOUT,
    ASYNC_INDEXER_DEFAULT_WORKER_COUNT,
    ASYNC_INDEXER_MAX_RETRIES,
    ASYNC_INDEXER_BACKPRESSURE_DELAY,
    ASYNC_INDEXER_SHUTDOWN_FLUSH_TIMEOUT,
    ASYNC_INDEXER_MAX_PROCESSING_TIMES,
    ASYNC_INDEXER_MAX_EXTRACTION_RETRIES,
    ASYNC_INDEXER_EXTRACTION_RETRY_DELAY,
    QUEUE_MAX_SIZE,
    QUEUE_POP_TIMEOUT,
)
from .error_handling import IndexingError


class AsyncBatchIndexer:
    """
    Batches indexing operations for improved throughput.

    Batching improves performance by:
    1. Reducing round-trips to storage
    2. Leveraging bulk operations
    3. Spreading fixed overhead across multiple operations

    Attributes:
        storage_backend: Storage backend for persistence
        batch_size: Target batch size before flush
        batch_timeout: Maximum seconds to wait before auto-flush
    """

    """
    Batches indexing operations for improved throughput.

    Batching improves performance by:
    1. Reducing round-trips to storage
    2. Leveraging bulk operations
    3. Spreading fixed overhead across multiple operations

    Attributes:
        storage_backend: Storage backend for indexing operations
        batch_size: Current batch size (capped at MAX_BATCH_SIZE)
        batch_timeout: Timeout in seconds before forcing a flush
        _batch: List of pending operations
        _batch_lock: Async lock for batch operations
        _last_flush: Timestamp of last flush
        _flush_event: Event for signaling flush completion
    """

    def __init__(
        self,
        storage_backend: Any,  # Will be injected at runtime
        batch_size: int = ASYNC_INDEXER_DEFAULT_BATCH_SIZE,
        batch_timeout: float = ASYNC_INDEXER_BATCH_TIMEOUT
    ) -> None:
        """
        Initialize the batch indexer.

        Args:
            storage_backend: Storage backend instance for indexing
            batch_size: Target batch size before flushing (default: 50)
            batch_timeout: Maximum seconds to wait before flushing (default: 5.0)

        Note:
            Actual batch_size is capped at ASYNC_INDEXER_MAX_BATCH_SIZE (500)
            to prevent unbounded memory growth.
        """
        self.storage_backend = storage_backend
        self.batch_size = min(batch_size, ASYNC_INDEXER_MAX_BATCH_SIZE)
        self.batch_timeout = batch_timeout
        self._batch: List[Dict[str, Any]] = []
        self._batch_lock = asyncio.Lock()
        self._last_flush = time.time()
        self._flush_event = asyncio.Event()

    async def add_operation(self, operation: Dict[str, Any]) -> bool:
        """
        Add an operation to the current batch.

        Args:
            operation: Dictionary containing operation details with keys:
                - type (str): Operation type (index, update, delete)
                - file_path (str): Path to the file
                - timestamp (str): ISO format timestamp
                - priority (str): Priority level
                - document (dict, optional): Document data for index/update
                - metadata (dict, optional): Additional metadata

        Returns:
            True if batch was flushed, False otherwise

        Raises:
            IndexingError: If batch is at maximum capacity and cannot accept more operations
        """
        async with self._batch_lock:
            # Enforce MAX_BATCH_SIZE to prevent unbounded memory growth
            if len(self._batch) >= ASYNC_INDEXER_MAX_BATCH_SIZE:
                # Force flush before adding more
                logger.warning(
                    f"Batch at max capacity ({ASYNC_INDEXER_MAX_BATCH_SIZE}), forcing flush",
                    extra={'component': 'AsyncBatchIndexer', 'action': 'force_flush', 'size': len(self._batch)}
                )
                await self._flush()

            # Check if we're still at capacity after flush attempt
            if len(self._batch) >= ASYNC_INDEXER_MAX_BATCH_SIZE:
                logger.error(
                    f"Batch overflow, rejecting operation (batch size: {len(self._batch)})",
                    extra={'component': 'AsyncBatchIndexer', 'action': 'overflow_reject', 'size': len(self._batch)}
                )
                raise IndexingError(
                    f"Batch at maximum capacity ({ASYNC_INDEXER_MAX_BATCH_SIZE}), cannot accept more operations",
                    context={'current_size': len(self._batch), 'operation': operation}
                )

            self._batch.append(operation)

            # Check if we should flush
            current_time = time.time()
            should_flush = (
                len(self._batch) >= self.batch_size or
                (current_time - self._last_flush) >= self.batch_timeout
            )

            if should_flush:
                return await self._flush()

            return False

    async def _flush(self) -> bool:
        """Flush the current batch to storage."""
        if not self._batch:
            return True

        batch_copy = self._batch.copy()
        self._batch.clear()
        self._last_flush = time.time()
        self._flush_event.set()

        try:
            # Delegate to storage backend for bulk operations
            if hasattr(self.storage_backend, 'bulk_index'):
                await self.storage_backend.bulk_index(batch_copy)
            else:
                # Fallback to individual operations
                for op in batch_copy:
                    op_type = op.get("type", "index")
                    if op_type in ("index", "update") and "document" in op:
                        await self.storage_backend.index_document(
                            op.get("file_path"),
                            op["document"]
                        )
                    elif op_type == "delete":
                        await self.storage_backend.delete_document(
                            op.get("file_path")
                        )

            logger.info(
                f"Successfully bulk indexed {len(batch_copy)} operations",
                extra={'component': 'AsyncBatchIndexer', 'action': 'bulk_success', 'count': len(batch_copy)}
            )

            return True

        except Exception as e:
            logger.error(
                f"Error during bulk indexing: {e}",
                extra={'component': 'AsyncBatchIndexer', 'action': 'bulk_error', 'error': str(e)}
            )
            return False

    async def flush(self) -> bool:
        """Manually flush any pending operations."""
        async with self._batch_lock:
            return await self._flush()

    async def get_pending_count(self) -> int:
        """Get the number of pending operations in the batch."""
        async with self._batch_lock:
            return len(self._batch)


class AsyncIndexingProcessor(AsyncTaskProcessor):
    """
    Specialized task processor for indexing operations.

    Extends AsyncTaskProcessor with:
    - Content extraction
    - Storage backend integration
    - Batch processing
    - Backpressure control

    Attributes:
        storage_backend: Storage backend for indexing
        base_path: Base path for file operations
        content_extractor: Content extraction utility
        batch_indexer: Optional batch processor
        backpressure: Optional backpressure controller
    """

    """
    Specialized task processor for indexing operations.

    Extends AsyncTaskProcessor with:
    - Content extraction
    - Storage backend integration
    - Batch processing
    - Backpressure control
    """

    def __init__(
        self,
        queue: AsyncBoundedQueue,
        storage_backend: Any,
        base_path: str,
        worker_count: int = ASYNC_INDEXER_DEFAULT_WORKER_COUNT,
        max_retries: int = ASYNC_INDEXER_MAX_RETRIES,
        enable_batching: bool = True,
        batch_size: int = ASYNC_INDEXER_DEFAULT_BATCH_SIZE,
        enable_backpressure: bool = True
    ) -> None:
        """
        Initialize the async indexing processor.

        Args:
            queue: Bounded queue for task management
            storage_backend: Storage backend for indexing operations
            base_path: Base path for file operations
            worker_count: Number of worker tasks (default: 4)
            max_retries: Maximum retry attempts for failed tasks (default: 3)
            enable_batching: Enable batch processing (default: True)
            batch_size: Batch size for bulk operations (default: 50)
            enable_backpressure: Enable backpressure control (default: True)
        """
        super().__init__(queue, worker_count, max_retries)
        self.storage_backend = storage_backend
        self.base_path = base_path
        self.content_extractor = ContentExtractor(base_path)

        # Batching configuration
        self.enable_batching = enable_batching
        self.batch_indexer = AsyncBatchIndexer(storage_backend, batch_size) if enable_batching else None

        # Backpressure configuration
        self.enable_backpressure = enable_backpressure
        self.backpressure = BackpressureController() if enable_backpressure else None

        # Processing statistics
        self._processing_times: List[float] = []

    async def _process_task(self, task: PrioritizedTask, worker_name: str) -> None:
        """
        Process a single indexing task.

        Args:
            task: The task to process
            worker_name: Name of the worker processing the task
        """
        start_time = time.time()

        try:
            # Check backpressure before processing
            if self.backpressure and await self.backpressure.should_throttle():
                # Under backpressure, drop LOW priority tasks instead of sleeping
                # This prevents deadlock where all workers are sleeping
                if task.priority == IndexingPriority.LOW:
                    logger.debug(
                        f"Dropping LOW priority task under backpressure: {task.file_path}",
                        extra={'component': 'AsyncIndexingProcessor', 'action': 'drop_low_priority', 'file_path': task.file_path}
                    )
                    return
                # For HIGH/CRITICAL priorities, continue processing without delay
                logger.debug(
                    f"Processing {task.priority.value} priority task under backpressure: {task.file_path}",
                    extra={'component': 'AsyncIndexingProcessor', 'action': 'backpressure_continue', 'file_path': task.file_path, 'priority': task.priority.value}
                )

            logger.info(f"{worker_name} processing: {task.operation_type} for {task.file_path}",
                        extra={'component': 'AsyncIndexingProcessor', 'action': 'processing', 'worker': worker_name, 'task_id': task.task_id})

            success = False

            if task.operation_type in ("index", "update"):
                # Extract content and metadata
                try:
                    document_data = await self._extract_content_async(task.file_path)
                except (FileNotFoundError, PermissionError) as perm_error:
                    # Permanent errors - don't retry
                    logger.error(
                        f"Permanent error extracting content for {task.file_path}: {perm_error}",
                        extra={'component': 'AsyncIndexingProcessor', 'action': 'extract_permanent_error',
                               'file_path': task.file_path, 'error': str(perm_error)}
                    )
                    document_data = None

                if document_data:
                    # Use batch processing if enabled
                    if self.enable_batching and self.batch_indexer:
                        operation = {
                            "type": task.operation_type,
                            "file_path": task.file_path,
                            "timestamp": task.timestamp,
                            "priority": task.priority.value,
                            "document": document_data,
                            "metadata": task.metadata
                        }
                        await self.batch_indexer.add_operation(operation)
                        success = True
                    else:
                        # Direct indexing
                        success = await self._index_document_async(task.file_path, document_data)
                else:
                    logger.error(f"Failed to extract content for {task.file_path}",
                                 extra={'component': 'AsyncIndexingProcessor', 'action': 'extract_failed', 'file_path': task.file_path})

            elif task.operation_type == "delete":
                success = await self._delete_document_async(task.file_path)

            # Calculate processing time
            processing_time = (time.time() - start_time) * 1000  # Convert to ms

            # Acquire lock BEFORE updating any shared state (fixes race condition)
            async with self._lock:
                # Update processing statistics
                if success:
                    self._processing_stats["total_processed"] += 1
                else:
                    self._processing_stats["total_failed"] += 1

                # Update processing times list (thread-safe)
                self._processing_times.append(processing_time)

                # Trim list to prevent unbounded memory growth
                if len(self._processing_times) > ASYNC_INDEXER_MAX_PROCESSING_TIMES:
                    self._processing_times = self._processing_times[-ASYNC_INDEXER_MAX_PROCESSING_TIMES:]

            # Update backpressure controller (has its own internal lock)
            if self.backpressure:
                await self.backpressure.record_processing_latency(processing_time)

            logger.info(f"{worker_name} completed {task.task_id} in {processing_time:.0f}ms",
                        extra={'component': 'AsyncIndexingProcessor', 'action': 'completed', 'worker': worker_name, 'task_id': task.task_id, 'success': success})

        except asyncio.CancelledError:
            logger.debug(f"{worker_name} task {task.task_id} cancelled",
                         extra={'component': 'AsyncIndexingProcessor', 'action': 'cancelled', 'worker': worker_name, 'task_id': task.task_id})
            raise
        except Exception as e:
            processing_time = (time.time() - start_time) * 1000
            logger.error(f"{worker_name} error processing {task.task_id}: {e}",
                         extra={'component': 'AsyncIndexingProcessor', 'action': 'error', 'worker': worker_name, 'task_id': task.task_id, 'error': str(e)})

            async with self._lock:
                self._processing_stats["total_failed"] += 1

            # Retry logic
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
                            extra={'component': 'AsyncIndexingProcessor', 'action': 'retried', 'task_id': task.task_id, 'retry_count': retry_task.retry_count})

    async def _extract_content_async(
        self,
        file_path: str,
        retry_count: int = 0
    ) -> Optional[Dict[str, Any]]:
        """
        Extract content and metadata from a file asynchronously.

        Includes retry mechanism for transient failures. Permanent errors
        (FileNotFoundError, PermissionError) are raised immediately.

        Args:
            file_path: Path to the file
            retry_count: Current retry attempt (internal use)

        Returns:
            Document data dict or None if extraction failed after retries

        Raises:
            FileNotFoundError: If file doesn't exist (permanent error)
            PermissionError: If permission denied (permanent error)

        Note:
            Transient errors are retried up to ASYNC_INDEXER_MAX_EXTRACTION_RETRIES times
            with exponential backoff (ASYNC_INDEXER_EXTRACTION_RETRY_DELAY * retry_count).
        """
        # Run the blocking content extraction in a thread pool
        loop = asyncio.get_event_loop()
        try:
            return await loop.run_in_executor(
                None,
                self.content_extractor.extract_content,
                file_path
            )
        except FileNotFoundError:
            logger.error(f"File not found: {file_path}",
                         extra={'component': 'AsyncIndexingProcessor', 'action': 'file_not_found', 'file_path': file_path})
            raise  # Don't retry missing files - permanent error
        except PermissionError:
            logger.error(f"Permission denied: {file_path}",
                         extra={'component': 'AsyncIndexingProcessor', 'action': 'permission_denied', 'file_path': file_path})
            raise  # Don't retry permission errors - permanent error
        except Exception as e:
            # Transient error - retry with exponential backoff
            if retry_count < ASYNC_INDEXER_MAX_EXTRACTION_RETRIES:
                logger.warning(
                    f"Retrying ({retry_count + 1}/{ASYNC_INDEXER_MAX_EXTRACTION_RETRIES}) for {file_path}: {e}",
                    extra={'component': 'AsyncIndexingProcessor', 'action': 'retry_extract',
                           'file_path': file_path, 'retry_count': retry_count + 1, 'error': str(e)}
                )
                await asyncio.sleep(ASYNC_INDEXER_EXTRACTION_RETRY_DELAY * (retry_count + 1))
                return await self._extract_content_async(file_path, retry_count + 1)
            else:
                logger.error(
                    f"Failed after {ASYNC_INDEXER_MAX_EXTRACTION_RETRIES} retries for {file_path}: {e}",
                    extra={'component': 'AsyncIndexingProcessor', 'action': 'extract_failed_final',
                           'file_path': file_path, 'error': str(e)}
                )
                return None

    async def _index_document_async(self, file_path: str, document: Dict[str, Any]) -> bool:
        """
        Index a document in storage asynchronously.

        Args:
            file_path: Path to the file
            document: Document data

        Returns:
            True if successful, False otherwise
        """
        try:
            # Check if storage backend has async method
            if hasattr(self.storage_backend, 'index_document_async'):
                await self.storage_backend.index_document_async(file_path, document)
            elif hasattr(self.storage_backend, 'index_document'):
                # Run sync method in executor
                loop = asyncio.get_event_loop()
                await loop.run_in_executor(
                    None,
                    self.storage_backend.index_document,
                    file_path,
                    document
                )
            else:
                logger.error("Storage backend has no index_document method",
                             extra={'component': 'AsyncIndexingProcessor', 'action': 'no_index_method'})
                return False

            logger.info(f"Indexed document for {file_path}",
                        extra={'component': 'AsyncIndexingProcessor', 'action': 'indexed', 'file_path': file_path})
            return True

        except Exception as e:
            logger.error(f"Error indexing document for {file_path}: {e}",
                         extra={'component': 'AsyncIndexingProcessor', 'action': 'index_error', 'file_path': file_path, 'error': str(e)})
            return False

    async def _delete_document_async(self, file_path: str) -> bool:
        """
        Delete a document from storage asynchronously.

        Args:
            file_path: Path to the file

        Returns:
            True if successful, False otherwise
        """
        try:
            # Check if storage backend has async method
            if hasattr(self.storage_backend, 'delete_document_async'):
                await self.storage_backend.delete_document_async(file_path)
            elif hasattr(self.storage_backend, 'delete_document'):
                # Run sync method in executor
                loop = asyncio.get_event_loop()
                await loop.run_in_executor(
                    None,
                    self.storage_backend.delete_document,
                    file_path
                )
            else:
                logger.error("Storage backend has no delete_document method",
                             extra={'component': 'AsyncIndexingProcessor', 'action': 'no_delete_method'})
                return False

            logger.info(f"Deleted document for {file_path}",
                        extra={'component': 'AsyncIndexingProcessor', 'action': 'deleted', 'file_path': file_path})
            return True

        except Exception as e:
            logger.error(f"Error deleting document for {file_path}: {e}",
                         extra={'component': 'AsyncIndexingProcessor', 'action': 'delete_error', 'file_path': file_path, 'error': str(e)})
            return False

    async def get_stats(self) -> Dict[str, Any]:
        """Get comprehensive processing statistics."""
        stats = await super().get_stats()

        # Add batch indexer status
        if self.batch_indexer:
            stats["batch_pending"] = await self.batch_indexer.get_pending_count()

        # Add backpressure status
        if self.backpressure:
            stats["backpressure"] = await self.backpressure.get_status()

        # Add processing latency statistics
        if self._processing_times:
            stats["avg_processing_latency_ms"] = sum(self._processing_times) / len(self._processing_times)
            stats["max_processing_latency_ms"] = max(self._processing_times)
            stats["min_processing_latency_ms"] = min(self._processing_times)

        return stats

    async def stop(self, timeout: float = 30.0) -> None:
        """
        Stop the processor with batch flush.

        Args:
            timeout: Maximum time to wait for shutdown (default: 30.0 seconds)

        Note:
            Pending batch operations are flushed with a timeout of
            ASYNC_INDEXER_SHUTDOWN_FLUSH_TIMEOUT (10 seconds).
        """
        # Flush any pending batch operations
        if self.enable_batching and self.batch_indexer:
            pending = await self.batch_indexer.get_pending_count()
            if pending > 0:
                logger.info(f"Flushing {pending} pending batch operations before stop...",
                            extra={'component': 'AsyncIndexingProcessor', 'action': 'flushing_batch', 'pending': pending})

                try:
                    await asyncio.wait_for(self.batch_indexer.flush(), timeout=ASYNC_INDEXER_SHUTDOWN_FLUSH_TIMEOUT)
                except asyncio.TimeoutError:
                    logger.warning("Batch flush timed out during shutdown",
                                  extra={'component': 'AsyncIndexingProcessor', 'action': 'flush_timeout'})

        # Stop the processor
        await super().stop(timeout)


class AsyncRealtimeIndexer:
    """
    Async implementation of real-time file indexing.

    Replaces RabbitMQ-based RealtimeIndexer with pure asyncio implementation.

    Attributes:
        base_path: Base path for file operations
        queue: Async bounded task queue
        processor: Task processor for indexing
    """

    """
    Async implementation of real-time file indexing.

    Replaces RabbitMQ-based RealtimeIndexer with pure asyncio implementation.
    """

    def __init__(
        self,
        storage_backend: Any,
        base_path: str,
        max_queue_size: int = QUEUE_MAX_SIZE,
        worker_count: int = ASYNC_INDEXER_DEFAULT_WORKER_COUNT,
        enable_batching: bool = True,
        batch_size: int = ASYNC_INDEXER_DEFAULT_BATCH_SIZE,
        enable_backpressure: bool = True
    ) -> None:
        """
        Initialize the async real-time indexer.

        Args:
            storage_backend: Storage backend for indexing
            base_path: Base path for file operations
            max_queue_size: Maximum queue size (default: 10000)
            worker_count: Number of worker tasks (default: 4)
            enable_batching: Enable batch processing (default: True)
            batch_size: Batch size for bulk operations (default: 50)
            enable_backpressure: Enable backpressure control (default: True)
        """
        self.base_path = base_path
        self.queue = AsyncBoundedQueue(max_size=max_queue_size)
        self.processor = AsyncIndexingProcessor(
            queue=self.queue,
            storage_backend=storage_backend,
            base_path=base_path,
            worker_count=worker_count,
            enable_batching=enable_batching,
            batch_size=batch_size,
            enable_backpressure=enable_backpressure
        )
        self._task_counter = 0
        self._lock = asyncio.Lock()

        logger.info(f"AsyncRealtimeIndexer initialized with {worker_count} workers",
                    extra={'component': 'AsyncRealtimeIndexer', 'action': 'initialized', 'worker_count': worker_count})

    async def start(self) -> None:
        """Start the indexing processor."""
        await self.processor.start()
        logger.info("AsyncRealtimeIndexer started",
                    extra={'component': 'AsyncRealtimeIndexer', 'action': 'started'})

    async def stop(self) -> None:
        """Stop the indexing processor."""
        await self.processor.stop()
        logger.info("AsyncRealtimeIndexer stopped",
                    extra={'component': 'AsyncRealtimeIndexer', 'action': 'stopped'})

    async def enqueue_change(
        self,
        file_path: str,
        change_type: Literal["index", "delete", "update"],
        priority: IndexingPriority = IndexingPriority.NORMAL
    ) -> None:
        """
        Enqueue a file change operation.

        Args:
            file_path: Path to the file
            change_type: Type of change (index, delete, update)
            priority: Priority level for the operation
        """
        # Generate unique task ID
        async with self._lock:
            self._task_counter += 1
            task_id = f"task_{self._task_counter}"

        # Add to queue
        success = await self.queue.push(
            task_id=task_id,
            file_path=file_path,
            operation_type=change_type,
            priority=priority,
            timestamp=datetime.now().isoformat()
        )

        if success:
            logger.debug(f"Enqueued {priority.value} priority {change_type} for {file_path}",
                         extra={'component': 'AsyncRealtimeIndexer', 'action': 'enqueued', 'file_path': file_path, 'change_type': change_type, 'priority': priority.value})
        else:
            logger.warning(f"Failed to enqueue {change_type} for {file_path} (queue full)",
                           extra={'component': 'AsyncRealtimeIndexer', 'action': 'enqueue_failed', 'file_path': file_path, 'change_type': change_type})

    async def wait_for_completion(self, timeout: Optional[float] = None):
        """
        Wait for all queued tasks to complete.

        Args:
            timeout: Maximum time to wait (None = wait indefinitely)
        """
        await self.processor.wait_for_completion(timeout)

    async def get_stats(self) -> Dict[str, Any]:
        """Get indexer statistics."""
        return await self.processor.get_stats()
