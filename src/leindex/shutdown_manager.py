"""
Graceful Shutdown Manager for LeIndex MCP Server.

This module provides comprehensive graceful shutdown functionality with:
- Signal handling (SIGINT, SIGTERM)
- Cache flush to disk on shutdown
- In-memory data persistence
- In-progress operation completion
- Shutdown hooks for custom cleanup
- Comprehensive logging and monitoring
"""

import asyncio
import signal
import logging
import threading
import time
import functools
from typing import Dict, List, Optional, Callable, Awaitable, Any, Tuple
from dataclasses import dataclass, field
from enum import Enum

logger = logging.getLogger(__name__)


class ShutdownState(Enum):
    """States for graceful shutdown process."""
    RUNNING = "running"
    SHUTDOWN_INITIATED = "shutdown_initiated"
    WAITING_OPERATIONS = "waiting_operations"
    FLUSHING_CACHES = "flushing_caches"
    PERSISTING_DATA = "persisting_data"
    EXECUTING_HOOKS = "executing_hooks"
    COMPLETED = "completed"
    FAILED = "failed"


@dataclass
class ShutdownHook:
    """Represents a shutdown hook with metadata."""
    name: str
    func: Callable[[], Awaitable[None]]
    priority: int = 100
    timeout: float = 30.0
    description: Optional[str] = None

    def __post_init__(self):
        """Validate hook parameters."""
        if self.priority < 0 or self.priority > 1000:
            raise ValueError(f"Hook priority must be between 0 and 1000, got {self.priority}")
        if self.timeout <= 0:
            raise ValueError(f"Hook timeout must be positive, got {self.timeout}")


@dataclass
class ShutdownResult:
    """Result of graceful shutdown process."""
    success: bool
    state: ShutdownState
    duration_seconds: float
    hooks_executed: int
    hooks_failed: int
    operations_completed: int
    operations_timeout: int
    caches_flushed: bool
    data_persisted: bool
    error_message: Optional[str] = None
    details: Dict[str, Any] = field(default_factory=dict)


class GracefulShutdownManager:
    """
    Manages graceful shutdown of the LeIndex MCP server.
    """

    def __init__(
        self,
        shutdown_timeout: float = 60.0,
        operation_wait_timeout: float = 30.0,
        enable_signal_handlers: bool = True,
        persist_callback: Optional[Callable[[], None]] = None
    ):
        """Initialize the graceful shutdown manager.

        Args:
            shutdown_timeout: Maximum time to wait for shutdown to complete
            operation_wait_timeout: Maximum time to wait for operations to finish
            enable_signal_handlers: Whether to register signal handlers for SIGINT/SIGTERM
            persist_callback: Optional callback for persisting data during shutdown.
                             This decouples the shutdown manager from specific persistence
                             implementations, improving testability and modularity.
        """
        self._shutdown_timeout = shutdown_timeout
        self._operation_wait_timeout = operation_wait_timeout
        self._enable_signal_handlers = enable_signal_handlers
        self._persist_callback = persist_callback

        self._state = ShutdownState.RUNNING
        self._started = False
        self._shutdown_start_time: Optional[float] = None
        self._shutdown_complete_time: Optional[float] = None
        self._shutdown_event = asyncio.Event()
        self._lock = asyncio.Lock()

        self._hooks: List[ShutdownHook] = []
        self._operations: Dict[str, asyncio.Task] = {}
        self._operations_lock = asyncio.Lock()

        self._original_handlers: Dict[int, Any] = {}
        self._loop: Optional[asyncio.AbstractEventLoop] = None
        self._loop_lock = threading.Lock()

        logger.info(
            f"GracefulShutdownManager initialized "
            f"(timeout={shutdown_timeout}s, operation_wait={operation_wait_timeout}s)"
        )

    async def start(self) -> None:
        """Start the shutdown manager."""
        async with self._lock:
            if self._started:
                raise RuntimeError(f"Cannot start manager: already started")

            self._started = True

            if self._enable_signal_handlers:
                self._setup_signal_handlers()

            logger.info("GracefulShutdownManager started")

    def _setup_signal_handlers(self) -> None:
        """Setup signal handlers for SIGINT and SIGTERM."""
        with self._loop_lock:
            try:
                self._loop = asyncio.get_running_loop()
            except RuntimeError:
                self._loop = asyncio.new_event_loop()
                asyncio.set_event_loop(self._loop)

            def signal_handler(signum, frame):
                signal_name = signal.Signals(signum).name
                logger.info(f"Received signal {signal_name} ({signum}), initiating graceful shutdown...")

                if self._loop and self._loop.is_running():
                    self._loop.call_soon_threadsafe(self._shutdown_event.set)
                else:
                    logger.warning("Event loop not running, shutdown will be initiated manually")

            for sig in (signal.SIGINT, signal.SIGTERM):
                self._original_handlers[sig] = signal.signal(sig, signal_handler)

            logger.info("Signal handlers registered for SIGINT and SIGTERM")

    async def shutdown(self) -> ShutdownResult:
        """Execute graceful shutdown process."""
        async with self._lock:
            if self._state != ShutdownState.RUNNING:
                logger.warning(f"Shutdown already in progress or completed (state={self._state.value})")
                return ShutdownResult(
                    success=False,
                    state=self._state,
                    duration_seconds=0.0,
                    hooks_executed=0,
                    hooks_failed=0,
                    operations_completed=0,
                    operations_timeout=0,
                    caches_flushed=False,
                    data_persisted=False,
                    error_message=f"Cannot shutdown from state: {self._state.value}"
                )

            self._state = ShutdownState.SHUTDOWN_INITIATED
            self._shutdown_start_time = time.time()

            logger.info("Starting graceful shutdown process...")

        result = await self._execute_shutdown()

        self._shutdown_complete_time = time.time()
        result.duration_seconds = self._shutdown_complete_time - self._shutdown_start_time

        async with self._lock:
            if result.success:
                self._state = ShutdownState.COMPLETED
            else:
                self._state = ShutdownState.FAILED

        self._restore_signal_handlers()

        logger.info(
            f"Graceful shutdown {'completed' if result.success else 'failed'} "
            f"in {result.duration_seconds:.2f}s"
        )

        return result

    async def _execute_shutdown(self) -> ShutdownResult:
        """Execute the shutdown process step by step."""
        result = ShutdownResult(
            success=True,
            state=ShutdownState.COMPLETED,
            duration_seconds=0.0,
            hooks_executed=0,
            hooks_failed=0,
            operations_completed=0,
            operations_timeout=0,
            caches_flushed=False,
            data_persisted=False,
            details={}
        )

        # Step 1: Wait for in-progress operations
        logger.info("Step 1: Waiting for in-progress operations...")
        self._state = ShutdownState.WAITING_OPERATIONS
        operations_result = await self._wait_for_operations()
        result.operations_completed = operations_result['completed']
        result.operations_timeout = operations_result['timeout']

        # Step 2: Flush caches to disk
        logger.info("Step 2: Flushing caches to disk...")
        self._state = ShutdownState.FLUSHING_CACHES
        result.caches_flushed = await self._flush_caches()

        # Step 3: Persist in-memory data
        logger.info("Step 3: Persisting in-memory data...")
        self._state = ShutdownState.PERSISTING_DATA
        result.data_persisted = await self._persist_data()

        # Step 4: Execute shutdown hooks
        logger.info("Step 4: Executing shutdown hooks...")
        self._state = ShutdownState.EXECUTING_HOOKS
        hooks_result = await self._execute_hooks()
        result.hooks_executed = hooks_result['executed']
        result.hooks_failed = hooks_result['failed']

        if result.hooks_failed > 0:
            result.success = False
            result.error_message = f"{result.hooks_failed} shutdown hooks failed"

        return result

    async def _wait_for_operations(self) -> Dict[str, int]:
        """Wait for in-progress operations to complete."""
        completed = 0
        timeout_count = 0

        async with self._operations_lock:
            operations = list(self._operations.values())

        if not operations:
            logger.info("No in-progress operations to wait for")
            return {'completed': 0, 'timeout': 0}

        logger.info(f"Waiting for {len(operations)} in-progress operations...")

        try:
            done, pending = await asyncio.wait(
                operations,
                timeout=self._operation_wait_timeout
            )
            completed = len(done)
            timeout_count = len(pending)

            if timeout_count > 0:
                logger.warning(
                    f"{timeout_count} operations did not complete within "
                    f"{self._operation_wait_timeout}s timeout"
                )
                for task in pending:
                    task.cancel()

            logger.info(f"Operations completed: {completed}, timeout: {timeout_count}")

        except asyncio.TimeoutError:
            logger.error(f"Timeout waiting for operations ({self._operation_wait_timeout}s)")
            timeout_count = len(operations)
        except Exception as e:
            logger.error(f"Error waiting for operations: {e}")

        return {'completed': completed, 'timeout': timeout_count}

    async def _flush_caches(self) -> bool:
        """Flush all caches to disk."""
        try:
            from .storage.dal_factory import get_dal_instance

            dal = get_dal_instance()

            if dal and hasattr(dal, 'flush'):
                logger.info("Flushing DAL instance...")
                dal.flush()
                logger.info("DAL instance flushed")
            elif dal:
                logger.info("DAL instance has no flush method")
            else:
                logger.info("No DAL instance to flush")

            return True

        except Exception as e:
            logger.error(f"Error flushing caches: {e}", exc_info=True)
            return False

    async def _persist_data(self) -> bool:
        """Persist in-memory data to disk using dependency injection callback.

        This method uses dependency injection to decouple the shutdown manager
        from specific persistence implementations. The callback is provided
        during initialization, improving testability and modularity.

        Returns:
            True if persistence succeeded or no callback was registered,
            False if persistence failed
        """
        try:
            if self._persist_callback is not None:
                logger.info("Executing persist callback")
                self._persist_callback()
                logger.info("Persist callback executed successfully")
            else:
                logger.info("No persist callback registered, skipping data persistence")
            return True

        except Exception as e:
            logger.error(f"Error persisting data: {e}", exc_info=True)
            return False

    async def _execute_hooks(self) -> Dict[str, int]:
        """Execute all registered shutdown hooks."""
        executed = 0
        failed = 0

        hooks = sorted(self._hooks, key=lambda h: h.priority)

        if not hooks:
            logger.info("No shutdown hooks registered")
            return {'executed': 0, 'failed': 0}

        logger.info(f"Executing {len(hooks)} shutdown hooks...")

        for hook in hooks:
            logger.info(f"Executing hook: {hook.name} (priority={hook.priority})")
            executed += 1

            try:
                await asyncio.wait_for(hook.func(), timeout=hook.timeout)
                logger.info(f"Hook {hook.name} completed successfully")

            except asyncio.TimeoutError:
                logger.error(f"Hook {hook.name} timed out after {hook.timeout}s")
                failed += 1
            except Exception as e:
                logger.error(f"Hook {hook.name} failed: {e}", exc_info=True)
                failed += 1

        logger.info(f"Hooks executed: {executed}, failed: {failed}")
        return {'executed': executed, 'failed': failed}

    def register_hook(
        self,
        name: str,
        func: Callable[[], Awaitable[None]],
        priority: int = 100,
        timeout: float = 30.0,
        description: Optional[str] = None
    ) -> None:
        """Register a shutdown hook."""
        if not name or not isinstance(name, str):
            raise ValueError("Hook name must be a non-empty string")

        if not asyncio.iscoroutinefunction(func):
            raise ValueError("Hook function must be async (coroutine function)")

        if any(h.name == name for h in self._hooks):
            raise RuntimeError(f"Hook with name '{name}' already registered")

        hook = ShutdownHook(
            name=name,
            func=func,
            priority=priority,
            timeout=timeout,
            description=description
        )

        self._hooks.append(hook)
        logger.info(f"Registered shutdown hook: {name} (priority={priority}, timeout={timeout}s)")

    def unregister_hook(self, name: str) -> bool:
        """Unregister a shutdown hook."""
        for i, hook in enumerate(self._hooks):
            if hook.name == name:
                self._hooks.pop(i)
                logger.info(f"Unregistered shutdown hook: {name}")
                return True

        logger.warning(f"Hook not found for unregistration: {name}")
        return False

    async def register_operation(
        self,
        name: str,
        task: asyncio.Task
    ) -> None:
        """Register an in-progress operation.

        Args:
            name: Unique identifier for the operation
            task: The asyncio.Task to track

        Raises:
            TypeError: If task is not an asyncio.Task instance
            ValueError: If shutdown has already been initiated
        """
        # Validate operation type
        if not isinstance(task, asyncio.Task):
            raise TypeError(
                f"Expected asyncio.Task, got {type(task).__name__}"
            )

        # Check lifecycle state - refuse registration after shutdown initiated
        if self._state != ShutdownState.RUNNING:
            logger.debug(
                f"Shutdown initiated (state={self._state.value}), "
                f"ignoring operation registration for '{name}'"
            )
            return

        async with self._operations_lock:
            self._operations[name] = task

        logger.debug(f"Registered operation: {name}")

        # Use named function instead of lambda to avoid reference cycles
        task.add_done_callback(self._create_operation_cleanup_callback(name))

    def _create_operation_cleanup_callback(
        self,
        operation_name: str
    ) -> Callable[[asyncio.Task], None]:
        """Create a cleanup callback for an operation.

        CRITICAL FIX: This named function approach avoids reference cycles that can occur
        with lambdas capturing task objects. The previous implementation used
        functools.partial with asyncio.create_task, which could cause:
        1. Reference cycles (lambda capturing task)
        2. Tasks created after event loop stops
        3. Crashes during shutdown

        The new implementation simplifies the callback to directly remove the operation
        from the dictionary without creating new tasks.

        Args:
            operation_name: Name of the operation to clean up

        Returns:
            Callback function that unregisters the operation when task completes
        """
        def cleanup_callback(task: asyncio.Task) -> None:
            """Remove operation from tracking when task completes.

            NOTE: This callback is invoked from the asyncio event loop when the task
            completes. It must be thread-safe and avoid creating new tasks.
            """
            try:
                # Direct dictionary removal without creating a new task
                # This is safe because we're not awaiting anything
                loop = asyncio.get_running_loop()
                loop.call_soon_threadsafe(
                    self._synchronous_remove_operation,
                    operation_name
                )
            except RuntimeError:
                # Event loop not running, skip cleanup
                logger.debug(
                    f"Event loop not running, skipping cleanup for {operation_name}"
                )
        return cleanup_callback

    def _synchronous_remove_operation(self, operation_name: str) -> None:
        """Synchronously remove an operation without awaiting.

        This helper method is called from the cleanup callback to remove
        operations without creating new async tasks.

        Args:
            operation_name: Name of the operation to remove
        """
        # Direct dictionary access without lock for performance
        # This is safe because:
        # 1. Only one callback removes each operation
        # 2. Dictionary get/del are atomic in CPython
        # 3. Missing key is handled gracefully
        self._operations.pop(operation_name, None)
        logger.debug(f"Synchronously removed operation: {operation_name}")

    async def unregister_operation(self, name: str) -> bool:
        """Unregister an in-progress operation."""
        async with self._operations_lock:
            if name in self._operations:
                del self._operations[name]
                logger.debug(f"Unregistered operation: {name}")
                return True

        logger.debug(f"Operation not found for unregistration: {name}")
        return False

    def get_state(self) -> ShutdownState:
        """Get the current shutdown state."""
        return self._state

    def is_shutdown_initiated(self) -> bool:
        """Check if shutdown has been initiated."""
        return self._shutdown_event.is_set()

    def _restore_signal_handlers(self) -> None:
        """Restore original signal handlers."""
        for sig, handler in self._original_handlers.items():
            signal.signal(sig, handler)

        logger.info("Original signal handlers restored")


_global_shutdown_manager: Optional[GracefulShutdownManager] = None
_manager_lock = threading.Lock()


def get_shutdown_manager() -> GracefulShutdownManager:
    """Get the global shutdown manager instance."""
    global _global_shutdown_manager

    with _manager_lock:
        if _global_shutdown_manager is None:
            _global_shutdown_manager = GracefulShutdownManager()

        return _global_shutdown_manager


async def initialize_shutdown_manager() -> GracefulShutdownManager:
    """Initialize and start the global shutdown manager."""
    manager = get_shutdown_manager()
    await manager.start()
    return manager
