"""
Progress Tracking and Cancellation System

This module implements comprehensive progress tracking and cancellation capabilities
for long-running indexing operations, providing real-time events and cleanup.
"""

import asyncio
import threading
import time
import uuid
from dataclasses import dataclass, field
from typing import Dict, List, Optional, Callable, Any
from enum import Enum
from abc import ABC, abstractmethod
import json
import logging
from pathlib import Path


class OperationStatus(Enum):
    """Status of an operation."""
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    CANCELLED = "cancelled"
    FAILED = "failed"
    PAUSED = "paused"


class ProgressEventType(Enum):
    """Types of progress events."""
    STARTED = "started"
    PROGRESS = "progress"
    STAGE_CHANGED = "stage_changed"
    COMPLETED = "completed"
    CANCELLED = "cancelled"
    FAILED = "failed"
    PAUSED = "paused"
    RESUMED = "resumed"
    CLEANUP_STARTED = "cleanup_started"
    CLEANUP_COMPLETED = "cleanup_completed"


@dataclass
class ProgressEvent:
    """Progress event containing operation status and details."""
    operation_id: str
    event_type: ProgressEventType
    timestamp: float
    status: OperationStatus
    current_stage: str
    progress_percent: float
    items_processed: int
    total_items: int
    estimated_remaining_ms: Optional[int]
    rate_per_second: float
    message: str
    details: Dict[str, Any] = field(default_factory=dict)
    metadata: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for JSON serialization."""
        return {
            "operation_id": self.operation_id,
            "event_type": self.event_type.value,
            "timestamp": self.timestamp,
            "status": self.status.value,
            "current_stage": self.current_stage,
            "progress_percent": self.progress_percent,
            "items_processed": self.items_processed,
            "total_items": self.total_items,
            "estimated_remaining_ms": self.estimated_remaining_ms,
            "rate_per_second": self.rate_per_second,
            "message": self.message,
            "details": self.details,
            "metadata": self.metadata
        }


class ProgressEventHandler(ABC):
    """Abstract base class for progress event handlers."""
    
    @abstractmethod
    async def handle_event(self, event: ProgressEvent) -> None:
        """Handle a progress event."""
        pass


class LoggingProgressHandler(ProgressEventHandler):
    """Progress event handler that logs events."""
    
    def __init__(self, logger: Optional[logging.Logger] = None):
        self.logger = logger or logging.getLogger(__name__)
    
    async def handle_event(self, event: ProgressEvent) -> None:
        """Log the progress event."""
        level = logging.INFO
        if event.event_type == ProgressEventType.FAILED:
            level = logging.ERROR
        elif event.event_type == ProgressEventType.CANCELLED:
            level = logging.WARNING
        
        self.logger.log(
            level,
            f"[{event.operation_id}] {event.event_type.value}: {event.message} "
            f"({event.progress_percent:.1f}%, {event.items_processed}/{event.total_items})"
        )


class FileProgressHandler(ProgressEventHandler):
    """Progress event handler that writes events to a file."""
    
    def __init__(self, file_path: Path):
        self.file_path = file_path
        self.file_path.parent.mkdir(parents=True, exist_ok=True)
    
    async def handle_event(self, event: ProgressEvent) -> None:
        """Write the progress event to file."""
        try:
            with open(self.file_path, 'a', encoding='utf-8') as f:
                f.write(json.dumps(event.to_dict()) + '\n')
        except Exception as e:
            # Don't let file writing errors break the operation
            print(f"Error writing progress event to file: {e}")


class CallbackProgressHandler(ProgressEventHandler):
    """Progress event handler that calls a callback function."""
    
    def __init__(self, callback: Callable[[ProgressEvent], None]):
        self.callback = callback
    
    async def handle_event(self, event: ProgressEvent) -> None:
        """Call the callback function with the progress event."""
        try:
            if asyncio.iscoroutinefunction(self.callback):
                await self.callback(event)
            else:
                self.callback(event)
        except Exception as e:
            print(f"Error in progress callback: {e}")


class CancellationToken:
    """Token for cancelling operations."""
    
    def __init__(self):
        self._cancelled = False
        self._cancellation_reason: Optional[str] = None
        self._lock = threading.Lock()
    
    def cancel(self, reason: str = "Operation cancelled"):
        """Cancel the operation."""
        with self._lock:
            self._cancelled = True
            self._cancellation_reason = reason
    
    def is_cancelled(self) -> bool:
        """Check if the operation is cancelled."""
        return self._cancelled
    
    def check_cancelled(self):
        """Check if cancelled and raise exception if so."""
        if self._cancelled:
            raise asyncio.CancelledError(self._cancellation_reason or "Operation cancelled")
    
    @property
    def cancellation_reason(self) -> Optional[str]:
        """Get the cancellation reason."""
        return self._cancellation_reason


class ProgressTracker:
    """Tracks progress of long-running operations with cancellation support."""
    
    def __init__(
        self,
        operation_id: str,
        operation_name: str,
        total_items: int,
        stages: List[str] = None,
        cancellation_token: Optional[CancellationToken] = None
    ):
        self.operation_id = operation_id
        self.operation_name = operation_name
        self.total_items = total_items
        self.stages = stages or ["Processing"]
        self.cancellation_token = cancellation_token or CancellationToken()
        
        # Progress state
        self.current_stage_index = 0
        self.items_processed = 0
        self.start_time = time.time()
        self.last_update_time = self.start_time
        self.status = OperationStatus.PENDING
        
        # Event handlers
        self.event_handlers: List[ProgressEventHandler] = []
        
        # Stage-specific progress
        self.stage_items: Dict[int, int] = {}
        self.stage_processed: Dict[int, int] = {}
        
        # Metadata
        self.metadata: Dict[str, Any] = {}
        self.cleanup_tasks: List[Callable[[], None]] = []
        
        # Thread safety
        self._lock = threading.Lock()
    
    def add_event_handler(self, handler: ProgressEventHandler):
        """Add a progress event handler."""
        self.event_handlers.append(handler)
    
    def add_cleanup_task(self, task: Callable[[], None]):
        """Add a cleanup task to be called on cancellation."""
        self.cleanup_tasks.append(task)
    
    async def start(self):
        """Start the operation."""
        with self._lock:
            self.status = OperationStatus.RUNNING
            self.start_time = time.time()
            self.last_update_time = self.start_time
        
        await self._emit_event(
            ProgressEventType.STARTED,
            f"Started {self.operation_name}",
            details={"stages": self.stages}
        )
    
    async def update_progress(
        self,
        items_processed: int = 1,
        message: str = "",
        stage_index: Optional[int] = None,
        metadata: Optional[Dict[str, Any]] = None
    ):
        """Update progress."""
        # Check for cancellation
        self.cancellation_token.check_cancelled()
        
        with self._lock:
            self.items_processed += items_processed
            if metadata:
                self.metadata.update(metadata)
            
            # Update stage if provided
            if stage_index is not None and stage_index != self.current_stage_index:
                old_stage = self.current_stage
                self.current_stage_index = stage_index
                await self._emit_event(
                    ProgressEventType.STAGE_CHANGED,
                    f"Stage changed from '{old_stage}' to '{self.current_stage}'",
                    details={"old_stage": old_stage, "new_stage": self.current_stage}
                )
        
        await self._emit_event(
            ProgressEventType.PROGRESS,
            message or f"Processed {self.items_processed}/{self.total_items} items"
        )
    
    async def complete(self, message: str = ""):
        """Mark operation as completed."""
        with self._lock:
            self.status = OperationStatus.COMPLETED
            self.items_processed = self.total_items
        
        await self._emit_event(
            ProgressEventType.COMPLETED,
            message or f"Completed {self.operation_name}"
        )
    
    async def cancel(self, reason: str = "Operation cancelled"):
        """Cancel the operation."""
        with self._lock:
            if self.status in [OperationStatus.COMPLETED, OperationStatus.CANCELLED]:
                return
            
            self.status = OperationStatus.CANCELLED
            self.cancellation_token.cancel(reason)
        
        await self._emit_event(
            ProgressEventType.CANCELLED,
            reason,
            details={"reason": reason}
        )
        
        # Run cleanup tasks
        await self._run_cleanup()
    
    async def fail(self, error: Exception, message: str = ""):
        """Mark operation as failed."""
        with self._lock:
            self.status = OperationStatus.FAILED
        
        await self._emit_event(
            ProgressEventType.FAILED,
            message or f"Failed: {str(error)}",
            details={"error": str(error), "error_type": type(error).__name__}
        )
        
        # Run cleanup tasks
        await self._run_cleanup()
    
    async def pause(self, message: str = ""):
        """Pause the operation."""
        with self._lock:
            self.status = OperationStatus.PAUSED
        
        await self._emit_event(
            ProgressEventType.PAUSED,
            message or f"Paused {self.operation_name}"
        )
    
    async def resume(self, message: str = ""):
        """Resume the operation."""
        with self._lock:
            self.status = OperationStatus.RUNNING
        
        await self._emit_event(
            ProgressEventType.RESUMED,
            message or f"Resumed {self.operation_name}"
        )
    
    async def _run_cleanup(self):
        """Run cleanup tasks."""
        if not self.cleanup_tasks:
            return
        
        await self._emit_event(
            ProgressEventType.CLEANUP_STARTED,
            f"Starting cleanup for {self.operation_name}",
            details={"cleanup_tasks_count": len(self.cleanup_tasks)}
        )
        
        for task in self.cleanup_tasks:
            try:
                if asyncio.iscoroutinefunction(task):
                    await task()
                else:
                    task()
            except Exception as e:
                print(f"Error in cleanup task: {e}")
        
        await self._emit_event(
            ProgressEventType.CLEANUP_COMPLETED,
            f"Cleanup completed for {self.operation_name}"
        )
    
    async def _emit_event(
        self,
        event_type: ProgressEventType,
        message: str,
        details: Optional[Dict[str, Any]] = None
    ):
        """Emit a progress event."""
        current_time = time.time()
        elapsed_time = current_time - self.start_time
        
        # Calculate rate and estimated remaining time
        rate_per_second = 0.0
        estimated_remaining_ms = None
        
        if elapsed_time > 0 and self.items_processed > 0:
            rate_per_second = self.items_processed / elapsed_time
            remaining_items = self.total_items - self.items_processed
            if rate_per_second > 0:
                estimated_remaining_ms = int((remaining_items / rate_per_second) * 1000)
        
        progress_percent = (self.items_processed / self.total_items * 100) if self.total_items > 0 else 0
        
        event = ProgressEvent(
            operation_id=self.operation_id,
            event_type=event_type,
            timestamp=current_time,
            status=self.status,
            current_stage=self.current_stage,
            progress_percent=progress_percent,
            items_processed=self.items_processed,
            total_items=self.total_items,
            estimated_remaining_ms=estimated_remaining_ms,
            rate_per_second=rate_per_second,
            message=message,
            details=details or {},
            metadata=self.metadata.copy()
        )
        
        # Send event to all handlers
        for handler in self.event_handlers:
            try:
                await handler.handle_event(event)
            except Exception as e:
                print(f"Error in event handler: {e}")
        
        self.last_update_time = current_time
    
    @property
    def current_stage(self) -> str:
        """Get current stage name."""
        if 0 <= self.current_stage_index < len(self.stages):
            return self.stages[self.current_stage_index]
        return "Unknown"
    
    @property
    def progress_percent(self) -> float:
        """Get current progress percentage."""
        return (self.items_processed / self.total_items * 100) if self.total_items > 0 else 0
    
    @property
    def elapsed_time(self) -> float:
        """Get elapsed time in seconds."""
        return time.time() - self.start_time
    
    def get_status(self) -> Dict[str, Any]:
        """Get current operation status."""
        return {
            "operation_id": self.operation_id,
            "operation_name": self.operation_name,
            "status": self.status.value,
            "current_stage": self.current_stage,
            "progress_percent": self.progress_percent,
            "items_processed": self.items_processed,
            "total_items": self.total_items,
            "elapsed_time": self.elapsed_time,
            "is_cancelled": self.cancellation_token.is_cancelled(),
            "cancellation_reason": self.cancellation_token.cancellation_reason,
            "metadata": self.metadata
        }


class ProgressManager:
    """Manages multiple progress trackers and operations."""
    
    def __init__(self):
        self.trackers: Dict[str, ProgressTracker] = {}
        self.global_handlers: List[ProgressEventHandler] = []
        self._lock = threading.Lock()
    
    def add_global_handler(self, handler: ProgressEventHandler):
        """Add a global event handler for all operations."""
        self.global_handlers.append(handler)
    
    def create_tracker(
        self,
        operation_name: str,
        total_items: int,
        stages: Optional[List[str]] = None,
        operation_id: Optional[str] = None
    ) -> ProgressTracker:
        """Create a new progress tracker."""
        if operation_id is None:
            operation_id = str(uuid.uuid4())
        
        tracker = ProgressTracker(
            operation_id=operation_id,
            operation_name=operation_name,
            total_items=total_items,
            stages=stages
        )
        
        # Add global handlers
        for handler in self.global_handlers:
            tracker.add_event_handler(handler)
        
        with self._lock:
            self.trackers[operation_id] = tracker
        
        return tracker
    
    def get_tracker(self, operation_id: str) -> Optional[ProgressTracker]:
        """Get a progress tracker by ID."""
        return self.trackers.get(operation_id)
    
    async def cancel_operation(self, operation_id: str, reason: str = "Operation cancelled") -> bool:
        """Cancel an operation."""
        tracker = self.get_tracker(operation_id)
        if tracker:
            await tracker.cancel(reason)
            return True
        return False
    
    async def cancel_all_operations(self, reason: str = "All operations cancelled"):
        """Cancel all active operations."""
        with self._lock:
            trackers = list(self.trackers.values())
        
        for tracker in trackers:
            if tracker.status in [OperationStatus.RUNNING, OperationStatus.PAUSED]:
                await tracker.cancel(reason)
    
    def cleanup_completed_operations(self, max_age_seconds: float = 3600):
        """Clean up completed operations older than max_age_seconds."""
        current_time = time.time()
        to_remove = []
        
        with self._lock:
            for operation_id, tracker in self.trackers.items():
                if tracker.status in [OperationStatus.COMPLETED, OperationStatus.CANCELLED, OperationStatus.FAILED]:
                    age = current_time - tracker.start_time
                    if age > max_age_seconds:
                        to_remove.append(operation_id)
        
        for operation_id in to_remove:
            with self._lock:
                del self.trackers[operation_id]
    
    def get_all_operations_status(self) -> List[Dict[str, Any]]:
        """Get status of all operations."""
        with self._lock:
            return [tracker.get_status() for tracker in self.trackers.values()]
    
    def get_active_operations(self) -> List[Dict[str, Any]]:
        """Get status of active operations."""
        with self._lock:
            return [
                tracker.get_status() for tracker in self.trackers.values()
                if tracker.status in [OperationStatus.RUNNING, OperationStatus.PAUSED]
            ]


# Global progress manager instance
progress_manager = ProgressManager()


class ProgressContext:
    """Context manager for progress tracking."""
    
    def __init__(
        self,
        operation_name: str,
        total_items: int,
        stages: Optional[List[str]] = None,
        operation_id: Optional[str] = None,
        manager: Optional[ProgressManager] = None
    ):
        self.manager = manager or progress_manager
        self.tracker = self.manager.create_tracker(
            operation_name=operation_name,
            total_items=total_items,
            stages=stages,
            operation_id=operation_id
        )
    
    async def __aenter__(self) -> ProgressTracker:
        """Enter the context and start tracking."""
        await self.tracker.start()
        return self.tracker
    
    async def __aexit__(self, exc_type, exc_val, exc_tb):
        """Exit the context and handle completion/failure."""
        if exc_type is None:
            await self.tracker.complete()
        elif exc_type == asyncio.CancelledError:
            await self.tracker.cancel(str(exc_val) if exc_val else "Operation cancelled")
        else:
            await self.tracker.fail(exc_val, f"Operation failed: {exc_val}")
        
        return False  # Don't suppress exceptions

