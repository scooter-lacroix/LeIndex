"""
Zero-Downtime Configuration Reload for LeIndex

This module provides zero-downtime configuration reloading functionality that allows
the MCP server to reload its configuration without requiring a restart, ensuring
continuous availability during configuration updates.

Key Features:
- Signal handling for SIGHUP to trigger config reload
- Atomic config updates with thread-safe operations
- Config validation before applying changes
- Observer pattern for notifying components of config changes
- Automatic rollback on validation failures
- Zero request failures during reload process

Architecture:
    The reload mechanism uses a combination of:
    1. Signal handlers for triggering reloads
    2. Thread-safe atomic updates using threading.Lock
    3. Validation before applying changes
    4. Observer pattern for component notifications
    5. Copy-on-write semantics for config updates

Example:
    >>> from leindex.config.reload import ConfigReloadManager, get_reload_manager
    >>>
    >>> # Get the singleton reload manager
    >>> reload_mgr = get_reload_manager()
    >>>
    >>> # Register an observer callback
    >>> def on_config_change(old_config, new_config):
    ...     print(f"Config updated: {old_config} -> {new_config}")
    >>> reload_mgr.subscribe(on_config_change)
    >>>
    >>> # Trigger a reload (also done via SIGHUP signal)
    >>> success = reload_mgr.reload_config()
    >>> print(f"Reload {'succeeded' if success else 'failed'}")

Thread Safety:
    All operations are thread-safe. Multiple threads can safely call reload_config()
    simultaneously. The manager ensures only one reload operation runs at a time
    and all observers see a consistent config state.

Production Usage:
    The reload manager is automatically initialized when the MCP server starts.
    Send SIGHUP to the server process to trigger a config reload:

    $ kill -HUP <pid>

    Or call reload_config() programmatically from management tools.
"""

import signal
import threading
import copy
import logging
from typing import Callable, Optional, List, Tuple, Any, Dict
from dataclasses import dataclass
from enum import Enum
import time

from .global_config import GlobalConfig, GlobalConfigManager
from .validation import ValidationError

logger = logging.getLogger(__name__)


class ReloadResult(Enum):
    """Result of a configuration reload operation.

    Attributes:
        SUCCESS: Reload completed successfully
        VALIDATION_FAILED: New config failed validation
        FILE_NOT_FOUND: Config file not found
        ALREADY_IN_PROGRESS: Another reload is already in progress
        IO_ERROR: Error reading config file
    """
    SUCCESS = "success"
    VALIDATION_FAILED = "validation_failed"
    FILE_NOT_FOUND = "file_not_found"
    ALREADY_IN_PROGRESS = "already_in_progress"
    IO_ERROR = "io_error"


@dataclass
class ReloadEvent:
    """Represents a configuration reload event.

    Attributes:
        timestamp: When the reload occurred
        result: The result of the reload operation
        old_config: The previous configuration (before reload)
        new_config: The new configuration (after reload, if successful)
        error_message: Error message if reload failed
        duration_ms: Duration of the reload operation in milliseconds
    """
    timestamp: float
    result: ReloadResult
    old_config: Optional[GlobalConfig]
    new_config: Optional[GlobalConfig]
    error_message: Optional[str]
    duration_ms: float

    def __post_init__(self):
        """Validate event data."""
        if self.result == ReloadResult.SUCCESS and self.new_config is None:
            raise ValueError("SUCCESS event must have new_config")
        if self.result != ReloadResult.SUCCESS and self.error_message is None:
            raise ValueError(f"Failed event {self.result} must have error_message")


# Type alias for observer callbacks
ConfigObserver = Callable[[GlobalConfig, GlobalConfig], None]


class ConfigReloadManager:
    """Manages zero-downtime configuration reloading.

    This class provides thread-safe configuration reloading with validation,
    atomic updates, and observer notifications. It ensures that the MCP server
    can update its configuration without dropping requests or causing inconsistencies.

    The manager uses a copy-on-write approach:
    1. Load new config from file
    2. Validate new config
    3. Create atomic copy of new config
    4. Swap config reference atomically
    5. Notify observers of the change

    Thread Safety:
        All public methods are thread-safe. Internal state is protected by locks.
        Multiple concurrent reload operations are serialized.

    Example:
        >>> manager = ConfigReloadManager(config_manager)
        >>> def observer(old, new):
        ...     print(f"Config changed from {old} to {new}")
        >>> manager.subscribe(observer)
        >>> result = manager.reload_config()
        >>> if result == ReloadResult.SUCCESS:
        ...     print("Config reloaded successfully")
    """

    def __init__(
        self,
        config_manager: GlobalConfigManager,
        enable_signal_handler: bool = True
    ):
        """Initialize the configuration reload manager.

        Args:
            config_manager: The GlobalConfigManager instance to reload config from
            enable_signal_handler: Whether to register SIGHUP signal handler

        Note:
            Only one instance should exist per process. The signal handler
            is registered for the main process by default.
        """
        self._config_manager = config_manager

        # Thread-safe state management
        self._lock = threading.RLock()
        self._reload_in_progress = False

        # Observer pattern: list of callbacks to notify on config change
        self._observers: List[ConfigObserver] = []

        # Event history for debugging and monitoring
        self._event_history: List[ReloadEvent] = []
        self._max_history_size = 100

        # Statistics
        self._stats = {
            'total_reloads': 0,
            'successful_reloads': 0,
            'failed_reloads': 0,
            'last_reload_time': None,
            'last_reload_result': None,
        }

        # Register signal handler if requested
        if enable_signal_handler:
            self._setup_signal_handler()

        logger.info("ConfigReloadManager initialized")

    def _setup_signal_handler(self) -> None:
        """Setup SIGHUP signal handler for config reload.

        This allows triggering config reload by sending SIGHUP to the process:
            $ kill -HUP <pid>
        """
        def signal_handler(signum: int, frame) -> None:
            """Handle SIGHUP signal by triggering config reload."""
            logger.info(f"Received signal {signum}, triggering config reload")
            try:
                self.reload_config()
            except Exception as e:
                logger.error(f"Error during signal-triggered reload: {e}", exc_info=True)

        # Register signal handler
        signal.signal(signal.SIGHUP, signal_handler)
        logger.info("Registered SIGHUP signal handler for config reload")

    def subscribe(self, observer: ConfigObserver) -> None:
        """Subscribe to configuration change notifications.

        The observer callback will be called with (old_config, new_config) when
        the configuration is successfully reloaded.

        Args:
            observer: Callback function that accepts (old_config, new_config)

        Example:
            >>> def my_observer(old: GlobalConfig, new: GlobalConfig):
            ...     logger.info(f"Memory budget changed: {old.memory.total_budget_mb} -> {new.memory.total_budget_mb}")
            >>> manager.subscribe(my_observer)
        """
        if not callable(observer):
            raise TypeError("Observer must be callable")

        with self._lock:
            self._observers.append(observer)
            logger.debug(f"Added observer {observer.__name__}, total observers: {len(self._observers)}")

    def unsubscribe(self, observer: ConfigObserver) -> None:
        """Unsubscribe from configuration change notifications.

        Args:
            observer: The callback function to remove

        Returns:
            True if observer was found and removed, False otherwise
        """
        with self._lock:
            try:
                self._observers.remove(observer)
                logger.debug(f"Removed observer {observer.__name__}, remaining: {len(self._observers)}")
                return True
            except ValueError:
                logger.warning(f"Observer {observer.__name__} not found")
                return False

    def reload_config(self) -> ReloadResult:
        """Reload configuration from file with zero downtime.

        This method:
        1. Loads new configuration from file
        2. Validates the new configuration
        3. Atomically updates the config if validation passes
        4. Notifies all observers of the change
        5. Rolls back on validation failure

        Thread Safety:
            Multiple threads can call this simultaneously. Only one reload
            operation runs at a time. Concurrent calls return ALREADY_IN_PROGRESS.

        Returns:
            ReloadResult indicating success or failure reason

        Example:
            >>> result = manager.reload_config()
            >>> if result == ReloadResult.SUCCESS:
            ...     print("Config updated successfully")
            >>> elif result == ReloadResult.VALIDATION_FAILED:
            ...     print("New config failed validation")
        """
        start_time = time.time()
        old_config = None
        new_config = None
        error_message = None

        # Check if reload is already in progress
        with self._lock:
            if self._reload_in_progress:
                logger.debug("Reload already in progress, skipping")
                return ReloadResult.ALREADY_IN_PROGRESS

            self._reload_in_progress = True

        try:
            # Get current config (for rollback and notifications)
            old_config = self._config_manager.get_config()

            # Load new config from file
            logger.info("Loading new configuration from file")
            try:
                new_config = self._config_manager.load_config()
            except FileNotFoundError:
                error_message = "Configuration file not found"
                logger.warning(error_message)
                return ReloadResult.FILE_NOT_FOUND
            except IOError as e:
                error_message = f"Error reading configuration file: {e}"
                logger.error(error_message)
                return ReloadResult.IO_ERROR

            # Validate new config (double-check)
            logger.debug("Validating new configuration")
            config_dict = self._config_manager.to_dict_persistent(new_config)
            try:
                self._config_manager.validator.validate_config(config_dict)
            except ValidationError as e:
                error_message = f"Configuration validation failed: {e}"
                logger.error(error_message)
                return ReloadResult.VALIDATION_FAILED

            # Atomic config swap (copy-on-write)
            logger.debug("Performing atomic config swap")
            with self._lock:
                # Create immutable copies for observers
                old_config_copy = copy.deepcopy(old_config)
                new_config_copy = copy.deepcopy(new_config)

                # Atomically update the config cache using public method
                self._config_manager.update_config_cache(new_config)

            # Notify observers (outside lock to avoid deadlock)
            logger.debug(f"Notifying {len(self._observers)} observers")
            self._notify_observers(old_config_copy, new_config_copy)

            # Update statistics
            duration_ms = (time.time() - start_time) * 1000
            with self._lock:
                self._stats['total_reloads'] += 1
                self._stats['successful_reloads'] += 1
                self._stats['last_reload_time'] = time.time()
                self._stats['last_reload_result'] = ReloadResult.SUCCESS

                # Record event in history
                event = ReloadEvent(
                    timestamp=time.time(),
                    result=ReloadResult.SUCCESS,
                    old_config=old_config_copy,
                    new_config=new_config_copy,
                    error_message=None,
                    duration_ms=duration_ms
                )
                self._add_event_to_history(event)

            logger.info(f"Config reloaded successfully in {duration_ms:.2f}ms")
            return ReloadResult.SUCCESS

        except Exception as e:
            # Unexpected error - rollback and log
            error_message = f"Unexpected error during reload: {e}"
            logger.error(error_message, exc_info=True)

            # Rollback: ensure old config is still in place
            with self._lock:
                if old_config is not None:
                    self._config_manager.update_config_cache(old_config)

            duration_ms = (time.time() - start_time) * 1000
            with self._lock:
                self._stats['total_reloads'] += 1
                self._stats['failed_reloads'] += 1
                self._stats['last_reload_time'] = time.time()
                self._stats['last_reload_result'] = ReloadResult.VALIDATION_FAILED

                # Record failed event
                event = ReloadEvent(
                    timestamp=time.time(),
                    result=ReloadResult.VALIDATION_FAILED,
                    old_config=old_config,
                    new_config=None,
                    error_message=error_message,
                    duration_ms=duration_ms
                )
                self._add_event_to_history(event)

            return ReloadResult.VALIDATION_FAILED

        finally:
            # Always clear the reload-in-progress flag
            with self._lock:
                self._reload_in_progress = False

    def _notify_observers(
        self,
        old_config: GlobalConfig,
        new_config: GlobalConfig
    ) -> None:
        """Notify all observers of configuration change.

        Args:
            old_config: Previous configuration (immutable copy)
            new_config: New configuration (immutable copy)

        Note:
            Observers are called outside the lock to prevent deadlocks.
            Each observer is called in a try-except to ensure one failing
            observer doesn't prevent others from being notified.
        """
        # Get observer list (copy to avoid modification during iteration)
        with self._lock:
            observers = self._observers.copy()

        for observer in observers:
            try:
                observer(old_config, new_config)
                logger.debug(f"Observer {observer.__name__} notified successfully")
            except Exception as e:
                logger.error(
                    f"Observer {observer.__name__} raised exception: {e}",
                    exc_info=True
                )

    def _add_event_to_history(self, event: ReloadEvent) -> None:
        """Add event to history with size limit.

        Args:
            event: The reload event to add
        """
        with self._lock:
            self._event_history.append(event)
            # Keep only recent events
            if len(self._event_history) > self._max_history_size:
                self._event_history.pop(0)

    def get_current_config(self) -> GlobalConfig:
        """Get the current configuration.

        Returns:
            Current GlobalConfig instance

        Example:
            >>> config = manager.get_current_config()
            >>> print(config.memory.total_budget_mb)
        """
        return self._config_manager.get_config()

    def get_stats(self) -> Dict[str, Any]:
        """Get reload statistics.

        Returns:
            Dictionary with reload statistics

        Example:
            >>> stats = manager.get_stats()
            >>> print(f"Success rate: {stats['successful_reloads'] / stats['total_reloads']:.1%}")
        """
        with self._lock:
            return self._stats.copy()

    def get_event_history(self, limit: Optional[int] = None) -> List[ReloadEvent]:
        """Get reload event history.

        Args:
            limit: Maximum number of events to return (most recent first)
                   If None, returns all events

        Returns:
            List of reload events, most recent first

        Example:
            >>> events = manager.get_event_history(limit=10)
            >>> for event in events:
            ...     print(f"{event.result} at {event.timestamp}")
        """
        with self._lock:
            history = self._event_history.copy()

        if limit is not None:
            history = history[-limit:]

        # Return most recent first
        return list(reversed(history))

    def clear_history(self) -> None:
        """Clear the event history.

        This is useful for testing or for long-running processes where
        historical data is no longer needed.
        """
        with self._lock:
            self._event_history.clear()
            logger.debug("Event history cleared")

    def get_observer_count(self) -> int:
        """Get the number of registered observers.

        Returns:
            Number of observers

        Example:
            >>> count = manager.get_observer_count()
            >>> print(f"Registered observers: {count}")
        """
        with self._lock:
            return len(self._observers)

    def is_reload_in_progress(self) -> bool:
        """Check if a reload operation is currently in progress.

        Returns:
            True if a reload is in progress, False otherwise

        Example:
            >>> if manager.is_reload_in_progress():
            ...     print("Reload in progress, please wait")
        """
        with self._lock:
            return self._reload_in_progress


# Global singleton instance
_reload_manager_instance: Optional[ConfigReloadManager] = None
_reload_manager_lock = threading.Lock()


def get_reload_manager() -> Optional[ConfigReloadManager]:
    """Get the global ConfigReloadManager singleton instance.

    Returns:
        The global ConfigReloadManager instance, or None if not initialized

    Example:
        >>> from leindex.config.reload import get_reload_manager
        >>> manager = get_reload_manager()
        >>> if manager:
        ...     manager.reload_config()
    """
    global _reload_manager_instance

    with _reload_manager_lock:
        return _reload_manager_instance


def initialize_reload_manager(
    config_manager: GlobalConfigManager,
    enable_signal_handler: bool = True
) -> ConfigReloadManager:
    """Initialize the global ConfigReloadManager singleton.

    This function should be called once during application startup.

    Args:
        config_manager: The GlobalConfigManager instance
        enable_signal_handler: Whether to enable SIGHUP signal handler

    Returns:
        The initialized ConfigReloadManager instance

    Example:
        >>> from leindex.config import GlobalConfigManager
        >>> from leindex.config.reload import initialize_reload_manager
        >>>
        >>> config_mgr = GlobalConfigManager()
        >>> reload_mgr = initialize_reload_manager(config_mgr)
        >>> print(f"Reload manager initialized with {reload_mgr.get_observer_count()} observers")
    """
    global _reload_manager_instance

    with _reload_manager_lock:
        if _reload_manager_instance is not None:
            logger.warning("ConfigReloadManager already initialized, returning existing instance")
            return _reload_manager_instance

        _reload_manager_instance = ConfigReloadManager(
            config_manager=config_manager,
            enable_signal_handler=enable_signal_handler
        )
        logger.info("Global ConfigReloadManager initialized")

        return _reload_manager_instance


def reload_config() -> ReloadResult:
    """Convenience function to trigger config reload using the global manager.

    This is a shorthand for:
        >>> get_reload_manager().reload_config()

    Returns:
        ReloadResult indicating success or failure

    Raises:
        RuntimeError: If reload manager has not been initialized

    Example:
        >>> from leindex.config.reload import reload_config
        >>> result = reload_config()
        >>> if result == ReloadResult.SUCCESS:
        ...     print("Config reloaded")
    """
    manager = get_reload_manager()
    if manager is None:
        raise RuntimeError("ConfigReloadManager not initialized. Call initialize_reload_manager() first.")

    return manager.reload_config()
