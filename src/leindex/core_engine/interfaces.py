"""
Abstract interfaces for Core Engine components.

ARCHITECTURAL FIX (Issue #2 - Tight Coupling):
----------------------------------------------
This module defines abstract interfaces for VectorBackend and SearchStrategy
to enable dependency injection and decouple CoreEngine from concrete implementations.

By using these interfaces, CoreEngine can work with any implementation that
conforms to the contract, making it testable and flexible.
"""

from abc import ABC, abstractmethod
from typing import List, Optional, Any, Dict, AsyncGenerator
from .types import SearchOptions, SearchResponse, UploadFileOptions, AskResponse, StoreInfo


class IVectorBackend(ABC):
    """
    Abstract interface for vector storage and search backends.

    This interface allows CoreEngine to work with any vector backend
    implementation (mixedbread, Qdrant, Weaviate, etc.) without tight coupling.
    """

    @abstractmethod
    async def list_files(
        self,
        store_id: str,
        path_prefix: Optional[str] = None
    ) -> AsyncGenerator[Any, None]:
        """
        List files in the store.

        Args:
            store_id: The store identifier
            path_prefix: Optional path prefix to filter files

        Yields:
            StoreFile objects
        """
        pass

    @abstractmethod
    async def upload_file(
        self,
        store_id: str,
        file_path: str,
        content: Any,
        options: UploadFileOptions
    ) -> None:
        """
        Upload a file to the store.

        Args:
            store_id: The store identifier
            file_path: Path of the file
            content: File content (str or bytes)
            options: Upload options
        """
        pass

    @abstractmethod
    async def delete_file(self, store_id: str, external_id: str) -> None:
        """
        Delete a file from the store.

        Args:
            store_id: The store identifier
            external_id: External ID of the file to delete
        """
        pass

    @abstractmethod
    async def search(
        self,
        store_ids: List[str],
        query: str,
        options: SearchOptions
    ) -> SearchResponse:
        """
        Search across multiple stores.

        Args:
            store_ids: List of store identifiers to search
            query: Search query
            options: Search options

        Returns:
            SearchResponse with results
        """
        pass

    @abstractmethod
    async def ask(
        self,
        store_ids: List[str],
        question: str,
        options: SearchOptions
    ) -> AskResponse:
        """
        Ask a question using RAG (Retrieval Augmented Generation).

        Args:
            store_ids: List of store identifiers
            question: The question to ask
            options: Search options

        Returns:
            AskResponse with answer and sources
        """
        pass

    @abstractmethod
    async def get_info(self, store_id: str) -> StoreInfo:
        """
        Get store information.

        Args:
            store_id: The store identifier

        Returns:
            StoreInfo with store details
        """
        pass

    @abstractmethod
    async def create_store(self, name: str, description: str = "") -> Any:
        """
        Create a new store.

        Args:
            name: Store name
            description: Store description

        Returns:
            Created store object
        """
        pass

    @property
    @abstractmethod
    def client(self) -> Optional[Any]:
        """Get the underlying client (if available)."""
        pass

    @abstractmethod
    def is_available(self) -> bool:
        """
        Check if the backend is available.

        Returns:
            True if the backend is initialized and ready
        """
        pass


class ISearchStrategy(ABC):
    """
    Abstract interface for search strategies.

    This interface allows CoreEngine to work with different search
    implementations (Zoekt, Ripgrep, grep, etc.) without tight coupling.
    """

    @property
    @abstractmethod
    def name(self) -> str:
        """Get the name of the search strategy."""
        pass

    @abstractmethod
    def is_available(self) -> bool:
        """
        Check if the search strategy is available.

        Returns:
            True if the strategy can be used
        """
        pass

    @abstractmethod
    def search(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False
    ) -> Dict[str, List[tuple]]:
        """
        Execute a search.

        Args:
            pattern: The search pattern
            base_path: The root directory to search in
            case_sensitive: Whether the search is case-sensitive
            context_lines: Number of context lines to show around each match
            file_pattern: Glob pattern to filter files (e.g., "*.py")
            fuzzy: Whether to enable fuzzy search

        Returns:
            A dictionary mapping filenames to lists of (line_number, line_content) tuples

        Raises:
            RuntimeError: If the strategy is not available or search fails
        """
        pass

    @abstractmethod
    def refresh_index(self, base_path: str) -> bool:
        """
        Refresh the search index for the given base path.

        Args:
            base_path: The base directory to re-index

        Returns:
            True if index was refreshed successfully, False otherwise
        """
        pass

    @abstractmethod
    def get_index_info(self) -> Dict[str, Any]:
        """
        Get information about the current search index.

        Returns:
            Dictionary with index information
        """
        pass


class ILegacyBackend(ABC):
    """
    Abstract interface for legacy backends.

    This interface allows CoreEngine to work with legacy DAL implementations
    while supporting the new architecture.
    """

    @property
    @abstractmethod
    def storage(self) -> Optional[Any]:
        """Get the storage interface."""
        pass

    @property
    @abstractmethod
    def metadata(self) -> Optional[Any]:
        """Get the metadata interface."""
        pass

    @property
    @abstractmethod
    def search(self) -> Optional[Any]:
        """Get the search interface."""
        pass

    @abstractmethod
    def close(self) -> None:
        """Close the backend."""
        pass

    @abstractmethod
    def clear_all(self) -> bool:
        """Clear all data."""
        pass
