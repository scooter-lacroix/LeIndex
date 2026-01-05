"""
Storage interface for code index backends.

This module defines the interface that all storage backends must implement
to ensure consistent API across different storage implementations.

ARCHITECTURAL FIX (Issue #1 - Interface Segregation Violation):
---------------------------------------------------------------
The original design violated the Interface Segregation Principle (ISP) by
forcing all implementations to support all sub-interfaces even when not needed.

FIX IMPLEMENTED:
1. Created a modular interface system with CapabilityProvider protocol
2. Made DALInterface a composition-based interface that can be partially implemented
3. Each sub-interface (StorageInterface, FileMetadataInterface, SearchInterface)
   can now be implemented independently
4. Added OptionalComponentMixin for optional interface methods
5. DALInterface now supports dynamic capability checking via supports_capability()
6. Added HasCapabilities protocol for runtime capability introspection

This allows implementations to:
- Only implement the interfaces they actually need
- Compose multiple interfaces together without forcing all methods
- Check at runtime if a specific capability is supported
- Provide clear error messages when unsupported capabilities are accessed
"""

from abc import ABC, abstractmethod
from typing import Dict, List, Optional, Any, Iterator, Tuple, Protocol, runtime_checkable, Set


@runtime_checkable
class CapabilityProvider(Protocol):
    """
    Protocol for objects that can declare their capabilities at runtime.

    This enables dynamic capability checking without requiring all interfaces
    to be implemented by every DAL implementation.
    """
    def supports_capability(self, capability: str) -> bool:
        """Check if a specific capability is supported."""
        ...

    def get_supported_capabilities(self) -> Set[str]:
        """Get all capabilities supported by this provider."""
        ...


class OptionalComponentMixin:
    """
    Mixin for optional interface methods that may not be supported by all implementations.

    This provides default implementations that raise NotImplementedError
    with clear error messages indicating which method is not supported.
    """

    def _raise_not_supported(self, method_name: str, interface_name: str) -> None:
        """Raise a clear error when an unsupported method is called."""
        raise NotImplementedError(
            f"Method '{method_name}' from {interface_name} is not supported by this implementation. "
            f"Check supports_capability() before calling this method."
        )


class StorageInterface(ABC):
    """Abstract base class for generic key-value storage backends."""

    @abstractmethod
    def put(self, key: str, value: Any) -> bool:
        """Store a key-value pair.

        Args:
            key: The key to store
            value: The value to store

        Returns:
            True if successful, False otherwise

        Raises:
            StorageError: If the operation fails
        """
        pass

    @abstractmethod
    def get(self, key: str) -> Optional[Any]:
        """Retrieve a value by key.

        Args:
            key: The key to retrieve

        Returns:
            The value if found, None otherwise

        Raises:
            StorageError: If the operation fails
        """
        pass

    @abstractmethod
    def delete(self, key: str) -> bool:
        """Delete a key-value pair.

        Args:
            key: The key to delete

        Returns:
            True if successful, False otherwise

        Raises:
            StorageError: If the operation fails
        """
        pass

    @abstractmethod
    def exists(self, key: str) -> bool:
        """Check if a key exists.

        Args:
            key: The key to check

        Returns:
            True if key exists, False otherwise
        """
        pass

    @abstractmethod
    def keys(self, pattern: Optional[str] = None) -> Iterator[str]:
        """Iterate over keys, optionally filtered by pattern.

        Args:
            pattern: Optional pattern to filter keys

        Yields:
            Keys matching the pattern
        """
        pass

    @abstractmethod
    def items(self, pattern: Optional[str] = None) -> Iterator[Tuple[str, Any]]:
        """Iterate over key-value pairs, optionally filtered by pattern.

        Args:
            pattern: Optional pattern to filter keys

        Yields:
            Key-value pairs matching the pattern
        """
        pass

    @abstractmethod
    def clear(self) -> bool:
        """Clear all data.

        Returns:
            True if successful, False otherwise
        """
        pass

    @abstractmethod
    def size(self) -> int:
        """Get the number of stored items.

        Returns:
            Number of items in storage
        """
        pass

    @abstractmethod
    def close(self) -> None:
        """Close the storage backend and release resources."""
        pass

    @abstractmethod
    def flush(self) -> bool:
        """Flush any pending operations to persistent storage.

        Returns:
            True if successful, False otherwise
        """
        pass

    # Optional file content methods - not all storage backends need these
    def save_file_content(self, file_path: str, content: str) -> None:
        """Save file content to storage.

        Args:
            file_path: The path of the file to save
            content: The content to save

        Raises:
            IOError: If the file cannot be written
            NotImplementedError: If this backend doesn't support file content storage
        """
        raise NotImplementedError(
            f"{self.__class__.__name__} does not support save_file_content(). "
            "Use a different storage backend or implement this method."
        )

    def get_file_content(self, file_path: str) -> Optional[str]:
        """Retrieve file content from storage.

        Args:
            file_path: The path of the file to retrieve

        Returns:
            The file content if found, None otherwise

        Raises:
            NotImplementedError: If this backend doesn't support file content storage
        """
        raise NotImplementedError(
            f"{self.__class__.__name__} does not support get_file_content(). "
            "Use a different storage backend or implement this method."
        )

    def delete_file_content(self, file_path: str) -> None:
        """Delete file content from storage.

        Args:
            file_path: The path of the file to delete

        Raises:
            NotImplementedError: If this backend doesn't support file content storage
        """
        raise NotImplementedError(
            f"{self.__class__.__name__} does not support delete_file_content(). "
            "Use a different storage backend or implement this method."
        )


class FileMetadataInterface(ABC):
    """Abstract interface for file metadata storage, including versions and diffs."""

    @abstractmethod
    def add_file(self, file_path: str, file_type: str, extension: str,
                 metadata: Optional[Dict[str, Any]] = None) -> bool:
        """Add a file's metadata to the index.

        Args:
            file_path: Path to the file
            file_type: Type of the file (e.g., 'file', 'directory')
            extension: File extension
            metadata: Optional metadata dictionary

        Returns:
            True if successful, False otherwise
        """
        pass

    @abstractmethod
    def remove_file(self, file_path: str) -> bool:
        """Remove a file's metadata from the index.

        Args:
            file_path: Path to the file to remove

        Returns:
            True if successful, False otherwise
        """
        pass

    @abstractmethod
    def get_file_info(self, file_path: str) -> Optional[Dict[str, Any]]:
        """Get metadata about a file.

        Args:
            file_path: Path to the file

        Returns:
            File information dictionary if found, None otherwise
        """
        pass

    @abstractmethod
    def get_directory_structure(self, directory_path: str = "") -> Dict[str, Any]:
        """Get the directory structure based on stored file metadata.

        Args:
            directory_path: Optional directory path to get structure for

        Returns:
            Dictionary representing the directory structure
        """
        pass

    @abstractmethod
    def get_all_files(self) -> List[Tuple[str, Dict[str, Any]]]:
        """Get all files' metadata in the index.

        Returns:
            List of tuples (file_path, file_info)
        """
        pass

    # Version tracking methods - optional for some implementations
    @abstractmethod
    def insert_file_version(self, version_id: str, file_path: str, content: str,
                           hash: str, timestamp: str, size: int) -> bool:
        """Inserts a new file version.

        Args:
            version_id: Unique ID for the file version
            file_path: Path of the file
            content: Content of the file version
            hash: Hash of the file content
            timestamp: Timestamp of the version
            size: Size of the file content in bytes

        Returns:
            True if successful, False otherwise
        """
        pass

    @abstractmethod
    def get_file_version(self, version_id: str) -> Optional[Dict]:
        """Retrieves a file version by its ID.

        Args:
            version_id: The ID of the file version to retrieve

        Returns:
            A dictionary containing file version data if found, None otherwise
        """
        pass

    @abstractmethod
    def get_file_versions_for_path(self, file_path: str) -> List[Dict]:
        """Retrieves all versions for a given file path, ordered by timestamp.

        Args:
            file_path: The path of the file

        Returns:
            A list of dictionaries, each containing file version data
        """
        pass

    # Diff tracking methods - optional for some implementations
    @abstractmethod
    def insert_file_diff(self, diff_id: str, file_path: str,
                        previous_version_id: Optional[str], current_version_id: str,
                        diff_content: str, diff_type: str, operation_type: str,
                        operation_details: Optional[str], timestamp: str) -> bool:
        """Inserts a new file diff.

        Args:
            diff_id: Unique ID for the diff
            file_path: Path of the file
            previous_version_id: ID of the previous version (if applicable)
            current_version_id: ID of the current version
            diff_content: The content of the diff
            diff_type: Type of diff (e.g., 'unified', 'json')
            operation_type: Type of operation (e.g., 'modify', 'create', 'delete', 'rename')
            operation_details: Additional details about the operation
            timestamp: Timestamp of the diff

        Returns:
            True if successful, False otherwise
        """
        pass

    @abstractmethod
    def get_file_diffs_for_path(self, file_path: str) -> List[Dict]:
        """Retrieves all diffs for a given file path.

        Args:
            file_path: The path of the file

        Returns:
            A list of dictionaries, each containing file diff data
        """
        pass

    # Additional metadata methods
    @abstractmethod
    def save_file_metadata(self, file_path: str, metadata: Dict[str, Any]) -> None:
        """Save file metadata to storage.

        Args:
            file_path: The path of the file
            metadata: The metadata dictionary to save

        Raises:
            IOError: If the metadata cannot be written
        """
        pass

    @abstractmethod
    def get_file_metadata(self, file_path: str) -> Optional[Dict[str, Any]]:
        """Retrieve file metadata from storage.

        Args:
            file_path: The path of the file

        Returns:
            The metadata dictionary if found, None otherwise
        """
        pass

    @abstractmethod
    def delete_file_metadata(self, file_path: str) -> None:
        """Delete file metadata from storage.

        Args:
            file_path: The path of the file
        """
        pass

    @abstractmethod
    def get_all_file_paths(self) -> List[str]:
        """Get all file paths in the storage.

        Returns:
            List of all file paths
        """
        pass

    @abstractmethod
    def clear(self) -> bool:
        """Clear all file metadata.

        Returns:
            True if successful, False otherwise
        """
        pass

    @abstractmethod
    def size(self) -> int:
        """Get the number of files in the storage.

        Returns:
            Number of files
        """
        pass

    @abstractmethod
    def close(self) -> None:
        """Close the storage backend."""
        pass

    @abstractmethod
    def flush(self) -> bool:
        """Flush any pending operations.

        Returns:
            True if successful, False otherwise
        """
        pass


class SearchInterface(ABC):
    """Abstract interface for full-text search capabilities."""

    @abstractmethod
    def search_content(self, query: str) -> List[Tuple[str, Any]]:
        """Search across file content.

        Args:
            query: The search query string

        Returns:
            A list of (key, value) tuples matching the query
        """
        pass

    @abstractmethod
    def search_file_paths(self, query: str) -> List[str]:
        """Search across file paths.

        Args:
            query: The search query string

        Returns:
            A list of file paths matching the query
        """
        pass

    @abstractmethod
    def index_document(self, doc_id: str, document: Dict[str, Any]) -> bool:
        """Index a document for search.

        Args:
            doc_id: Unique identifier for the document
            document: Document data to index

        Returns:
            True if successful, False otherwise
        """
        pass

    @abstractmethod
    def index_file(self, file_path: str, content: str) -> None:
        """Index a file for search.

        Args:
            file_path: Path of the file to index
            content: Content of the file to index

        Raises:
            IOError: If the file cannot be indexed
        """
        pass

    @abstractmethod
    def delete_indexed_file(self, file_path: str) -> None:
        """Delete a file from the search index.

        Args:
            file_path: Path of the file to delete from index
        """
        pass

    @abstractmethod
    def search_files(self, query: str) -> List[Dict[str, Any]]:
        """Search for files matching the query.

        Args:
            query: The search query string

        Returns:
            A list of dictionaries containing file search results
        """
        pass

    @abstractmethod
    def close(self) -> None:
        """Close the search backend."""
        pass

    @abstractmethod
    def clear(self) -> bool:
        """Clear the search index.

        Returns:
            True if successful, False otherwise
        """
        pass


# Standard capability identifiers for runtime checking
class Capabilities:
    """Standard capability identifiers for DAL implementations."""

    # Storage capabilities
    KEY_VALUE = "key_value_storage"
    FILE_CONTENT = "file_content_storage"

    # Metadata capabilities
    FILE_METADATA = "file_metadata"
    VERSION_TRACKING = "version_tracking"
    DIFF_TRACKING = "diff_tracking"

    # Search capabilities
    FULLTEXT_SEARCH = "fulltext_search"
    INDEXING = "document_indexing"

    # Transaction capabilities
    TRANSACTIONS = "transactions"
    TWO_PHASE_COMMIT = "two_phase_commit"

    # Query capabilities
    PATTERN_MATCHING = "pattern_matching"
    REGEX_SEARCH = "regex_search"
    FUZZY_SEARCH = "fuzzy_search"


class DALInterface(ABC, CapabilityProvider):
    """
    Abstract base class for the Data Access Layer (DAL).

    ARCHITECTURAL FIX (Issue #1):
    ----------------------------
    This interface now supports composition over inheritance. Instead of forcing
    all implementations to support all sub-interfaces, implementations can:

    1. Implement only the interfaces they need
    2. Use composition to combine multiple implementations
    3. Declare their capabilities at runtime via supports_capability()
    4. Return None for interfaces they don't support

    Example usage:
        # Check if a capability is supported before using it
        if dal.supports_capability(Capabilities.VERSION_TRACKING):
            versions = dal.metadata.get_file_versions_for_path(path)

        # Get all supported capabilities
        caps = dal.get_supported_capabilities()
    """

    # Capability constants for easy access
    CAP = Capabilities

    @property
    @abstractmethod
    def storage(self) -> Optional[StorageInterface]:
        """
        Returns the generic key-value storage interface.

        Returns None if this implementation doesn't support key-value storage.
        """
        pass

    @property
    @abstractmethod
    def metadata(self) -> Optional[FileMetadataInterface]:
        """
        Returns the file metadata storage interface.

        Returns None if this implementation doesn't support metadata storage.
        """
        pass

    @property
    @abstractmethod
    def search(self) -> Optional[SearchInterface]:
        """
        Returns the full-text search interface.

        Returns None if this implementation doesn't support search.
        """
        pass

    @abstractmethod
    def close(self) -> None:
        """
        Closes all underlying storage backends and releases resources.
        """
        pass

    @abstractmethod
    def clear_all(self) -> bool:
        """
        Clears all data from all underlying storage backends.

        Returns:
            True if successful, False otherwise
        """
        pass

    # Default capability checking implementation
    def supports_capability(self, capability: str) -> bool:
        """
        Check if a specific capability is supported by this DAL implementation.

        Default implementation checks which interfaces are not None.
        Subclasses can override for more granular capability reporting.

        Args:
            capability: The capability identifier (use Capabilities class constants)

        Returns:
            True if the capability is supported, False otherwise
        """
        # Map capabilities to interface checks
        capability_map = {
            self.CAP.KEY_VALUE: lambda: self.storage is not None,
            self.CAP.FILE_CONTENT: lambda: self.storage is not None,
            self.CAP.FILE_METADATA: lambda: self.metadata is not None,
            self.CAP.VERSION_TRACKING: lambda: self.metadata is not None,
            self.CAP.DIFF_TRACKING: lambda: self.metadata is not None,
            self.CAP.FULLTEXT_SEARCH: lambda: self.search is not None,
            self.CAP.INDEXING: lambda: self.search is not None,
        }

        checker = capability_map.get(capability)
        if checker:
            return checker()
        return False

    def get_supported_capabilities(self) -> Set[str]:
        """
        Get all capabilities supported by this DAL implementation.

        Returns:
            Set of capability identifiers that are supported
        """
        all_caps = {
            self.CAP.KEY_VALUE, self.CAP.FILE_CONTENT,
            self.CAP.FILE_METADATA, self.CAP.VERSION_TRACKING, self.CAP.DIFF_TRACKING,
            self.CAP.FULLTEXT_SEARCH, self.CAP.INDEXING,
        }
        return {cap for cap in all_caps if self.supports_capability(cap)}

    def require_capability(self, capability: str) -> None:
        """
        Raise an error if a capability is not supported.

        Useful for validating required capabilities before operations.

        Args:
            capability: The capability identifier to check

        Raises:
            NotImplementedError: If the capability is not supported
        """
        if not self.supports_capability(capability):
            raise NotImplementedError(
                f"This DAL implementation does not support the '{capability}' capability. "
                f"Supported capabilities: {self.get_supported_capabilities()}"
            )


# Custom exception types for consistent error handling
class StorageError(Exception):
    """Base exception for storage-related errors."""

    def __init__(self, message: str, operation: str = "", details: Optional[Dict[str, Any]] = None):
        super().__init__(message)
        self.operation = operation
        self.details = details or {}


class NotFoundError(StorageError):
    """Raised when a requested resource is not found."""

    pass


class ValidationError(StorageError):
    """Raised when input validation fails."""

    pass


class ConfigurationError(StorageError):
    """Raised when there's a configuration error."""

    pass
