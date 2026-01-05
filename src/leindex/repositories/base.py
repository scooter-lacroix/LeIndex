"""
Base Repository classes and exceptions.

ARCHITECTURAL FIX (Issue #3 - Missing Repository Pattern):
-------------------------------------------------------
This module provides the base repository abstraction and exception types
for all repository implementations.

Design Patterns:
- Repository Pattern: Encapsulates data access with business logic
- Dependency Inversion: Depends on DALInterface abstraction
- Template Method: Base class defines common workflow
"""

from abc import ABC, abstractmethod
from typing import Optional, Dict, List, Any
import logging

from ..storage.storage_interface import DALInterface

logger = logging.getLogger(__name__)


# ============================================================================
# REPOSITORY EXCEPTIONS - Consistent Error Handling (Issue #4)
# ============================================================================

class RepositoryError(Exception):
    """
    Base exception for all repository errors.

    Provides consistent error handling with context information.
    """

    def __init__(self, message: str, entity: str = "", details: Optional[Dict[str, Any]] = None):
        super().__init__(message)
        self.entity = entity
        self.details = details or {}

    def __str__(self) -> str:
        parts = [super().__str__()]
        if self.entity:
            parts.append(f"Entity: {self.entity}")
        if self.details:
            parts.append(f"Details: {self.details}")
        return " | ".join(parts)


class NotFoundError(RepositoryError):
    """
    Raised when a requested entity is not found in the repository.
    """

    def __init__(self, entity_type: str, identifier: str):
        message = f"{entity_type} with identifier '{identifier}' not found"
        super().__init__(message, entity_type, {"identifier": identifier})
        self.entity_type = entity_type
        self.identifier = identifier


class ValidationError(RepositoryError):
    """
    Raised when input validation fails in the repository.

    This is different from data validation - it's business rule validation.
    """

    def __init__(self, entity: str, field: str, value: Any, reason: str):
        message = f"Validation failed for {entity}.{field}: {reason}"
        super().__init__(message, entity, {"field": field, "value": value})
        self.field = field
        self.value = value


class DuplicateError(RepositoryError):
    """
    Raised when attempting to create a duplicate entity.
    """

    def __init__(self, entity: str, identifier: str):
        message = f"{entity} with identifier '{identifier}' already exists"
        super().__init__(message, entity, {"identifier": identifier})


class OperationError(RepositoryError):
    """
    Raised when a repository operation fails.
    """

    def __init__(self, operation: str, entity: str, reason: str):
        message = f"Failed to {operation} {entity}: {reason}"
        super().__init__(message, entity, {"operation": operation})
        self.operation = operation


# ============================================================================
# BASE REPOSITORY ABSTRACTION
# ============================================================================

class Repository(ABC):
    """
    Abstract base class for all repositories.

    ARCHITECTURAL FIX (Issue #3):
    ----------------------------
    Provides common functionality and enforces consistent behavior
    across all repository implementations.

    Benefits:
    - Common CRUD operations with business logic hooks
    - Consistent error handling
    - Transaction management hooks
    - Logging hooks
    - Validation hooks
    """

    def __init__(self, dal: DALInterface):
        """
        Initialize the repository with a DAL instance.

        Args:
            dal: The Data Access Layer to use for data operations

        Raises:
            ValueError: If dal is None
        """
        if dal is None:
            raise ValueError("DAL instance cannot be None")

        self._dal = dal
        self._logger = logging.getLogger(f"{self.__class__.__module__}.{self.__class__.__name__}")

        # Log repository initialization
        self._logger.debug(f"Initialized {self.__class__.__name__}")

    @property
    def dal(self) -> DALInterface:
        """Get the DAL instance (read-only access)."""
        return self._dal

    # ------------------------------------------------------------------------
    # Lifecycle hooks for subclasses to override
    # ------------------------------------------------------------------------

    def _pre_validate(self, operation: str, data: Dict[str, Any]) -> None:
        """
        Hook for pre-operation validation.

        Subclasses can override to add custom validation logic.

        Args:
            operation: The operation being performed (create, update, delete, etc.)
            data: The data for the operation

        Raises:
            ValidationError: If validation fails
        """
        pass

    def _post_operation(self, operation: str, data: Dict[str, Any], result: Any) -> None:
        """
        Hook for post-operation logic.

        Subclasses can override to add custom post-operation logic
        like caching, event publishing, etc.

        Args:
            operation: The operation that was performed
            data: The data for the operation
            result: The result of the operation
        """
        pass

    def _handle_error(self, operation: str, error: Exception) -> None:
        """
        Hook for error handling.

        Subclasses can override to add custom error handling logic.

        Args:
            operation: The operation that failed
            error: The exception that was raised

        Raises:
            RepositoryError: By default, converts exceptions to RepositoryError
        """
        self._logger.error(f"Error during {operation}: {error}")
        raise OperationError(operation, self.__class__.__name__, str(error))

    # ------------------------------------------------------------------------
    # Common operations
    # ------------------------------------------------------------------------

    def validate_required_fields(self, data: Dict[str, Any], required_fields: List[str]) -> None:
        """
        Validate that all required fields are present and non-empty.

        Args:
            data: The data dictionary to validate
            required_fields: List of required field names

        Raises:
            ValidationError: If any required field is missing or empty
        """
        for field in required_fields:
            if field not in data or data[field] is None or (isinstance(data[field], str) and not data[field].strip()):
                raise ValidationError(
                    self.__class__.__name__,
                    field,
                    data.get(field),
                    f"Required field '{field}' is missing or empty"
                )

    def sanitize_string(self, value: str, max_length: int = 1000) -> str:
        """
        Sanitize a string value for storage.

        Args:
            value: The string to sanitize
            max_length: Maximum allowed length

        Returns:
            Sanitized string

        Raises:
            ValidationError: If the value is too long
        """
        if not isinstance(value, str):
            raise ValidationError(
                self.__class__.__name__,
                "string_value",
                value,
                f"Expected string, got {type(value).__name__}"
            )

        if len(value) > max_length:
            raise ValidationError(
                self.__class__.__name__,
                "string_value",
                value,
                f"String exceeds maximum length of {max_length}"
            )

        return value.strip()

    def validate_file_path(self, file_path: str) -> str:
        """
        Validate and normalize a file path.

        Args:
            file_path: The file path to validate

        Returns:
            Normalized file path

        Raises:
            ValidationError: If the path is invalid
        """

        if not file_path:
            raise ValidationError(
                self.__class__.__name__,
                "file_path",
                file_path,
                "File path cannot be empty"
            )

        # Normalize path separators
        normalized = file_path.replace("\\", "/")

        # Remove leading './' if present
        if normalized.startswith("./"):
            normalized = normalized[2:]

        # Validate for path traversal attempts
        if "../" in normalized or "..\\" in normalized:
            raise ValidationError(
                self.__class__.__name__,
                "file_path",
                file_path,
                "Path traversal detected"
            )

        return normalized

    # ------------------------------------------------------------------------
    # Abstract methods that subclasses must implement
    # ------------------------------------------------------------------------

    @abstractmethod
    def get_by_id(self, identifier: str) -> Optional[Dict[str, Any]]:
        """
        Get an entity by its identifier.

        Args:
            identifier: The unique identifier

        Returns:
            Entity data dictionary, or None if not found

        Raises:
            RepositoryError: On data access errors
        """
        pass

    @abstractmethod
    def list_all(self, limit: Optional[int] = None, offset: int = 0) -> List[Dict[str, Any]]:
        """
        List all entities with pagination.

        Args:
            limit: Maximum number of results to return
            offset: Number of results to skip

        Returns:
            List of entity data dictionaries

        Raises:
            RepositoryError: On data access errors
        """
        pass

    @abstractmethod
    def create(self, data: Dict[str, Any]) -> Dict[str, Any]:
        """
        Create a new entity.

        Args:
            data: Entity data

        Returns:
            Created entity data

        Raises:
            ValidationError: If validation fails
            DuplicateError: If entity already exists
            RepositoryError: On data access errors
        """
        pass

    @abstractmethod
    def update(self, identifier: str, data: Dict[str, Any]) -> Dict[str, Any]:
        """
        Update an existing entity.

        Args:
            identifier: The unique identifier
            data: Updated entity data

        Returns:
            Updated entity data

        Raises:
            NotFoundError: If entity doesn't exist
            ValidationError: If validation fails
            RepositoryError: On data access errors
        """
        pass

    @abstractmethod
    def delete(self, identifier: str) -> bool:
        """
        Delete an entity.

        Args:
            identifier: The unique identifier

        Returns:
            True if deleted, False if not found

        Raises:
            RepositoryError: On data access errors
        """
        pass

    @abstractmethod
    def exists(self, identifier: str) -> bool:
        """
        Check if an entity exists.

        Args:
            identifier: The unique identifier

        Returns:
            True if exists, False otherwise
        """
        pass

    @abstractmethod
    def count(self) -> int:
        """
        Get the total count of entities.

        Returns:
            Number of entities
        """
        pass
