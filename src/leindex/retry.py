"""
Centralized Retry Utility

Provides consistent retry logic with exponential backoff and jitter across all backends.
Supports both synchronous and asynchronous operations.

Phase 3: Search Integration, Optimization, and Production Readiness
Spec: conductor/tracks/mcp_consolidation_local_vector_20251230/spec.md
"""

from __future__ import annotations

import asyncio
import logging
import random
import time
from typing import Any, Callable, Optional, TypeVar

T = TypeVar("T")

logger = logging.getLogger(__name__)


class RetryConfig:
    """Configuration for retry behavior."""

    def __init__(
        self,
        max_attempts: int = 3,
        base_delay: float = 0.5,
        max_delay: float = 30.0,
        jitter: bool = True,
        jitter_factor: float = 0.1,
        on_retry: Optional[Callable[[int, float, Exception], None]] = None,
    ):
        """
        Initialize retry configuration.

        Args:
            max_attempts: Maximum number of retry attempts
            base_delay: Base delay between retries (seconds)
            max_delay: Maximum delay between retries (seconds)
            jitter: Whether to add jitter to prevent thundering herd
            jitter_factor: Factor for jitter calculation (0.0-1.0)
            on_retry: Callback called on each retry with (attempt, delay, exception)
        """
        self.max_attempts = max_attempts
        self.base_delay = base_delay
        self.max_delay = max_delay
        self.jitter = jitter
        self.jitter_factor = jitter_factor
        self.on_retry = on_retry

    @classmethod
    def default_sync(cls) -> "RetryConfig":
        """Default configuration for synchronous operations."""
        return cls(max_attempts=3, base_delay=0.5, max_delay=5.0)

    @classmethod
    def default_async(cls) -> "RetryConfig":
        """Default configuration for asynchronous operations."""
        return cls(max_attempts=3, base_delay=0.5, max_delay=30.0)

    @classmethod
    def conservative(cls) -> "RetryConfig":
        """Conservative configuration for critical operations."""
        return cls(max_attempts=5, base_delay=1.0, max_delay=60.0)


def is_recoverable_error(error: Exception) -> bool:
    """
    Determine if an error is recoverable and worth retrying.

    By default, most errors are considered recoverable. Only specific non-recoverable
    errors (like ValueError, TypeError for programming errors) return False.

    Args:
        error: The exception to check

    Returns:
        True if the error is recoverable, False otherwise
    """
    error_type = type(error).__name__
    error_message = str(error).lower()

    # Non-recoverable: programming errors that won't be fixed by retrying
    non_recoverable = [
        "ValueError",
        "TypeError",
        "KeyError",
        "AttributeError",
        "SyntaxError",
        "NameError",
        "IndentationError",
        "ZeroDivisionError",
        "IndexError",
        "ReferenceError",  # Dereferenced weakref
    ]
    if error_type in non_recoverable:
        return False

    # Network/connection errors are generally recoverable
    if "connection" in error_message or "timeout" in error_message:
        return True

    # Some specific exception types are recoverable
    recoverable_exceptions = [
        "ConnectionError",
        "TimeoutError",
        "TemporaryFailure",
        "OperationalError",
        "InternalServerError",
        "ServiceUnavailable",
        "GatewayTimeout",
        "TooManyRequests",
    ]

    if error_type in recoverable_exceptions:
        return True

    # Check error message for common recoverable patterns
    if "service unavailable" in error_message:
        return True
    if "too many requests" in error_message:
        return True

    # Elasticsearch-specific recoverable errors
    if "timeout" in error_message or "service unavailable" in error_message:
        return True

    # SQLite-specific recoverable errors
    if (
        "database is locked" in error_message
        or "database disk image is malformed" in error_message
    ):
        return True

    # By default, assume errors are recoverable (optimistic retry)
    return True


def calculate_retry_delay(
    attempt: int,
    base_delay: float,
    max_delay: float,
    jitter: bool,
    jitter_factor: float,
) -> float:
    """
    Calculate retry delay with exponential backoff and optional jitter.

    Args:
        attempt: Current attempt number (0-based)
        base_delay: Base delay in seconds
        max_delay: Maximum delay in seconds
        jitter: Whether to add jitter
        jitter_factor: Factor for jitter calculation

    Returns:
        Delay in seconds
    """
    delay = min(base_delay * (2**attempt), max_delay)

    if jitter:
        # Add jitter to prevent thundering herd
        # Jitter is ±jitter_factor of the delay
        jitter_range = delay * jitter_factor
        jitter_value = random.uniform(-jitter_range, jitter_range)
        delay = max(0, delay + jitter_value)

    return delay


def retry_sync(
    func: Callable[..., T],
    *,
    config: Optional[RetryConfig] = None,
    is_retryable: Optional[Callable[[Exception], bool]] = None,
    on_success: Optional[Callable[[T, int], None]] = None,
    on_failure: Optional[Callable[[Exception], None]] = None,
) -> T:
    """
    Execute a synchronous function with retry logic.

    Args:
        func: Function to execute
        config: Retry configuration (uses default if None)
        is_retryable: Function to determine if an error is retryable (uses is_recoverable_error if None)
        on_success: Callback called on success with (result, attempts)
        on_failure: Callback called on final failure

    Returns:
        Result of the function

    Raises:
        Exception: Last exception if all retries fail
    """
    if config is None:
        config = RetryConfig.default_sync()

    if is_retryable is None:
        is_retryable = is_recoverable_error

    last_exception: Exception | None = None

    for attempt in range(config.max_attempts):
        try:
            result = func()

            if on_success is not None:
                on_success(result, attempt + 1)

            return result

        except Exception as e:
            last_exception = e

            if not is_retryable(e):
                # Non-recoverable error, fail immediately
                logger.debug(f"Non-recoverable error: {e}")
                if on_failure is not None:
                    on_failure(e)
                raise

            if attempt < config.max_attempts - 1:
                # Calculate delay and wait
                delay = calculate_retry_delay(
                    attempt,
                    config.base_delay,
                    config.max_delay,
                    config.jitter,
                    config.jitter_factor,
                )

                if config.on_retry is not None:
                    config.on_retry(attempt + 1, delay, e)

                logger.debug(
                    f"Attempt {attempt + 1} failed: {e}. Retrying in {delay:.2f}s"
                )
                time.sleep(delay)
            else:
                # All retries exhausted
                logger.error(
                    f"All {config.max_attempts} attempts failed. Last error: {e}"
                )
                if on_failure is not None:
                    on_failure(e)
                raise last_exception


async def retry_async(
    func: Callable[..., Any],
    *,
    config: Optional[RetryConfig] = None,
    is_retryable: Optional[Callable[[Exception], bool]] = None,
    on_success: Optional[Callable[[Any, int], None]] = None,
    on_failure: Optional[Callable[[Exception], None]] = None,
) -> Any:
    """
    Execute an asynchronous function with retry logic.

    Args:
        func: Async function to execute
        config: Retry configuration (uses default if None)
        is_retryable: Function to determine if an error is retryable (uses is_recoverable_error if None)
        on_success: Callback called on success with (result, attempts)
        on_failure: Callback called on final failure

    Returns:
        Result of the async function

    Raises:
        Exception: Last exception if all retries fail
    """
    if config is None:
        config = RetryConfig.default_async()

    if is_retryable is None:
        is_retryable = is_recoverable_error

    last_exception: Exception | None = None

    for attempt in range(config.max_attempts):
        try:
            result = await func()

            if on_success is not None:
                on_success(result, attempt + 1)

            return result

        except Exception as e:
            last_exception = e

            if not is_retryable(e):
                # Non-recoverable error, fail immediately
                logger.debug(f"Non-recoverable error: {e}")
                if on_failure is not None:
                    on_failure(e)
                raise

            if attempt < config.max_attempts - 1:
                # Calculate delay and wait
                delay = calculate_retry_delay(
                    attempt,
                    config.base_delay,
                    config.max_delay,
                    config.jitter,
                    config.jitter_factor,
                )

                if config.on_retry is not None:
                    config.on_retry(attempt + 1, delay, e)

                logger.debug(
                    f"Attempt {attempt + 1} failed: {e}. Retrying in {delay:.2f}s"
                )
                await asyncio.sleep(delay)
            else:
                # All retries exhausted
                logger.error(
                    f"All {config.max_attempts} attempts failed. Last error: {e}"
                )
                if on_failure is not None:
                    on_failure(e)
                raise last_exception

    if on_failure is not None and last_exception is not None:
        on_failure(last_exception)

    raise last_exception


class RetryContext:
    """
    Context manager for retry operations with lifecycle hooks.

    Usage:
        with RetryContext() as ctx:
            ctx.on_retry = lambda attempt, delay, e: logger.warning(f"Retry {attempt}: {e}")
            result = ctx.execute(some_function)
    """

    def __init__(self, config: Optional[RetryConfig] = None):
        self.config = config or RetryConfig.default_sync()
        self.on_retry: Optional[Callable[[int, float, Exception], None]] = None
        self.on_success: Optional[Callable[[Any, int], None]] = None
        self.on_failure: Optional[Callable[[Exception], None]] = None
        self._attempt = 0

    def execute_sync(self, func: Callable[..., T]) -> T:
        """Execute a synchronous function with retry."""
        return retry_sync(
            func,
            config=self.config,
            is_retryable=is_recoverable_error,
            on_success=self.on_success,
            on_failure=self.on_failure,
        )

    async def execute_async(self, func: Callable[..., Any]) -> Any:
        """Execute an asynchronous function with retry."""
        return await retry_async(
            func,
            config=self.config,
            is_retryable=is_recoverable_error,
            on_success=self.on_success,
            on_failure=self.on_failure,
        )

    def __enter__(self) -> "RetryContext":
        return self

    def __exit__(self, exc_type, exc_val, exc_tb) -> None:
        pass


# Convenience function for creating retry contexts
def retry_context(config: Optional[RetryConfig] = None) -> RetryContext:
    """Create a new retry context with optional configuration."""
    return RetryContext(config)
