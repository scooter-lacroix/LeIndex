"""
Centralized error handling for LeIndex.

This module provides a consistent exception hierarchy for all LeIndex operations,
making error handling more predictable and maintainable across the codebase.
"""

from typing import Optional, Dict, Any
import logging

logger = logging.getLogger(__name__)


class LeIndexError(Exception):
    """
    Base exception for all LeIndex errors.

    All custom exceptions in LeIndex should inherit from this class
    to allow consistent error handling patterns.

    Attributes:
        message: Human-readable error message
        component: Name of the component where the error occurred
        context: Additional context information about the error
    """

    def __init__(
        self,
        message: str,
        component: Optional[str] = None,
        context: Optional[Dict[str, Any]] = None
    ) -> None:
        self.message = message
        self.component = component
        self.context = context or {}
        super().__init__(self.message)

    def __str__(self) -> str:
        if self.component:
            return f"[{self.component}] {self.message}"
        return self.message

    def to_dict(self) -> Dict[str, Any]:
        """Convert exception to dictionary for logging/serialization."""
        return {
            "error_type": self.__class__.__name__,
            "message": self.message,
            "component": self.component,
            "context": self.context
        }


class StorageError(LeIndexError):
    """
    Error in storage operations.

    Raised when storage backend operations fail, including:
    - Database connection failures
    - File I/O errors
    - Index write/read failures
    - Transaction failures
    """

    def __init__(
        self,
        message: str,
        component: str = "storage",
        context: Optional[Dict[str, Any]] = None
    ) -> None:
        super().__init__(message, component, context)


class SearchError(LeIndexError):
    """
    Error in search operations.

    Raised when search backend operations fail, including:
    - Query parsing errors
    - Search execution failures
    - Index corruption
    - Invalid search parameters
    """

    def __init__(
        self,
        message: str,
        component: str = "search",
        context: Optional[Dict[str, Any]] = None
    ) -> None:
        super().__init__(message, component, context)


class ValidationError(LeIndexError):
    """
    Error in validation.

    Raised when input validation fails, including:
    - Invalid file paths
    - Invalid search queries
    - Malformed input data
    - Constraint violations
    """

    def __init__(
        self,
        message: str,
        component: str = "validation",
        context: Optional[Dict[str, Any]] = None
    ) -> None:
        super().__init__(message, component, context)


class ConfigurationError(LeIndexError):
    """
    Error in configuration.

    Raised when configuration is invalid or missing, including:
    - Missing required configuration values
    - Invalid configuration values
    - Configuration file parsing errors
    - Incompatible configuration settings
    """

    def __init__(
        self,
        message: str,
        component: str = "configuration",
        context: Optional[Dict[str, Any]] = None
    ) -> None:
        super().__init__(message, component, context)


class IndexingError(LeIndexError):
    """
    Error in indexing operations.

    Raised when document indexing fails, including:
    - Content extraction failures
    - Document parsing errors
    - Batch processing failures
    - Worker pool errors
    """

    def __init__(
        self,
        message: str,
        component: str = "indexing",
        context: Optional[Dict[str, Any]] = None
    ) -> None:
        super().__init__(message, component, context)


class QueueError(LeIndexError):
    """
    Error in queue operations.

    Raised when task queue operations fail, including:
    - Queue capacity exceeded
    - Task processing failures
    - Worker timeout
    - Backpressure violations
    """

    def __init__(
        self,
        message: str,
        component: str = "queue",
        context: Optional[Dict[str, Any]] = None
    ) -> None:
        super().__init__(message, component, context)


def handle_error(
    error: Exception,
    reraise: bool = False,
    level: str = "error"
) -> None:
    """
    Handle an error with consistent logging behavior.

    Args:
        error: The exception to handle
        reraise: Whether to re-raise the exception after logging
        level: Log level (debug, info, warning, error, critical)

    Example:
        >>> try:
        ...     risky_operation()
        ... except Exception as e:
        ...     handle_error(e, reraise=False)
    """
    # Convert to dict if it's a LeIndexError
    if isinstance(error, LeIndexError):
        error_dict = error.to_dict()
        log_message = f"{error_dict['error_type']}: {error_dict['message']}"
        if error_dict.get('context'):
            log_message += f" | Context: {error_dict['context']}"
    else:
        error_type = type(error).__name__
        error_message = str(error)
        log_message = f"{error_type}: {error_message}"

    # Log at the specified level
    log_func = getattr(logger, level, logger.error)
    log_func(log_message, exc_info=True)

    # Re-raise if requested
    if reraise:
        raise


def wrap_error(
    error: Exception,
    message: str,
    error_class: type = LeIndexError,
    **context
) -> LeIndexError:
    """
    Wrap an exception in a LeIndexError with additional context.

    Args:
        error: The original exception
        message: Additional message explaining the context
        error_class: The LeIndexError subclass to use
        **context: Additional context key-value pairs

    Returns:
        A new LeIndexError instance with the wrapped error

    Example:
        >>> try:
        ...     connect_to_database()
        ... except ConnectionError as e:
        ...     raise wrap_error(e, "Failed to connect to SQLite", StorageError, db_path="/tmp/db.sqlite")
    """
    wrapped_context = {
        "original_error": str(error),
        "original_type": type(error).__name__,
        **context
    }
    return error_class(f"{message}: {error}", context=wrapped_context)
