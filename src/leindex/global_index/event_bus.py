"""
Event Bus for Global Index Integration

This module provides a simple thread-safe event bus for communication between
the project registry and the global index. Events are emitted synchronously
and processed by all registered subscribers.

Thread Safety:
    The event bus is fully thread-safe and can be used in multi-threaded
    MCP server environments. All operations are protected by locks.

Performance:
    Event emission is synchronous and should complete in <1ms to avoid
    blocking the indexing process. Subscribers should perform minimal
    work synchronously and defer heavy processing to background threads.
"""

import logging
import threading
from typing import Callable, Dict, List, Any
from dataclasses import dataclass

logger = logging.getLogger(__name__)


@dataclass
class Event:
    """
    Base class for all events.

    Attributes:
        event_type: Type identifier for the event
        timestamp: Unix timestamp when event was created
        data: Event-specific data
    """
    event_type: str
    timestamp: float
    data: Dict[str, Any]


class EventBus:
    """
    Simple thread-safe event bus for pub/sub messaging.

    This class provides a minimal event bus implementation with:
    - Thread-safe subscription management
    - Synchronous event delivery
    - Automatic subscriber cleanup on errors
    - Type-safe event filtering

    Example:
        >>> bus = EventBus()
        >>> bus.subscribe("project_indexed", lambda event: print(event.data))
        >>> bus.emit(Event("project_indexed", time.time(), {"project_id": "test"}))
    """

    def __init__(self):
        """Initialize an empty event bus."""
        self._subscribers: Dict[str, List[Callable]] = {}
        self._lock: threading.Lock = threading.Lock()
        self._stats = {
            'events_emitted': 0,
            'events_delivered': 0,
            'delivery_errors': 0,
        }
        self._stats_lock: threading.Lock = threading.Lock()

        logger.info("EventBus initialized (thread-safe)")

    def subscribe(self, event_type: str, handler: Callable[[Event], None]) -> Callable[[], None]:
        """
        Subscribe to events of a specific type.

        Args:
            event_type: Type of events to subscribe to
            handler: Callback function that receives Event objects

        Returns:
            Unsubscribe function that removes this subscription

        Example:
            >>> bus = EventBus()
            >>> unsubscribe = bus.subscribe("project_indexed", my_handler)
            >>> unsubscribe()  # Remove subscription
        """
        with self._lock:
            if event_type not in self._subscribers:
                self._subscribers[event_type] = []
            self._subscribers[event_type].append(handler)

        logger.debug(f"Subscribed to event type: {event_type}")

        # Return unsubscribe function
        def unsubscribe() -> None:
            with self._lock:
                if event_type in self._subscribers:
                    try:
                        self._subscribers[event_type].remove(handler)
                        logger.debug(f"Unsubscribed from event type: {event_type}")
                    except ValueError:
                        # Handler already removed
                        pass

        return unsubscribe

    def emit(self, event: Event) -> None:
        """
        Emit an event to all registered subscribers.

        This method is synchronous and will call all subscribers
        in the order they were registered. If a subscriber raises
        an exception, it is logged and the subscriber is removed.

        Args:
            event: Event to emit

        Performance:
            Should complete in <1ms to avoid blocking the caller.
            Subscribers should perform minimal work synchronously.
        """
        with self._stats_lock:
            self._stats['events_emitted'] += 1

        start_time = None

        # Get subscribers for this event type
        with self._lock:
            handlers = self._subscribers.get(event.event_type, []).copy()

        if not handlers:
            logger.debug(f"No subscribers for event type: {event.event_type}")
            return

        # Deliver event to all subscribers
        for handler in handlers:
            try:
                if start_time is None:
                    import time
                    start_time = time.time()

                handler(event)

                with self._stats_lock:
                    self._stats['events_delivered'] += 1

            except Exception as e:
                logger.error(
                    f"Error delivering event to handler: {e}. "
                    f"Removing handler from {event.event_type}"
                )
                with self._stats_lock:
                    self._stats['delivery_errors'] += 1

                # Remove failed handler
                with self._lock:
                    if event.event_type in self._subscribers:
                        try:
                            self._subscribers[event.event_type].remove(handler)
                        except ValueError:
                            pass

        # Log performance
        if start_time is not None:
            import time
            duration_ms = (time.time() - start_time) * 1000
            logger.debug(
                f"EventBus: Delivered {event.event_type} to {len(handlers)} "
                f"subscribers in {duration_ms:.2f}ms"
            )

            # Performance warning
            if duration_ms > 5:
                logger.warning(
                    f"EventBus: Event delivery took {duration_ms:.2f}ms, "
                    f"exceeds 5ms target"
                )

    def get_subscriber_count(self, event_type: str) -> int:
        """
        Get the number of subscribers for an event type.

        Args:
            event_type: Event type to query

        Returns:
            Number of subscribers
        """
        with self._lock:
            return len(self._subscribers.get(event_type, []))

    def get_stats(self) -> Dict[str, int]:
        """
        Get event bus statistics.

        Returns:
            Dictionary with statistics
        """
        with self._stats_lock:
            return self._stats.copy()

    def clear_all_subscribers(self) -> None:
        """
        Remove all subscribers from all event types.

        This is primarily useful for testing and cleanup.
        """
        with self._lock:
            self._subscribers.clear()

        logger.info("EventBus: Cleared all subscribers")


# Global event bus instance for the application
_global_event_bus: EventBus = None
_event_bus_lock = threading.Lock()


def get_global_event_bus() -> EventBus:
    """
    Get the global event bus instance.

    This ensures a single event bus is used across the application,
    which is important for consistent event delivery.

    Returns:
        The global EventBus instance
    """
    global _global_event_bus

    with _event_bus_lock:
        if _global_event_bus is None:
            _global_event_bus = EventBus()
        return _global_event_bus


def reset_global_event_bus() -> None:
    """
    Reset the global event bus.

    This is primarily useful for testing to ensure clean state
    between test runs.
    """
    global _global_event_bus

    with _event_bus_lock:
        _global_event_bus = None

    logger.debug("Global EventBus reset")
