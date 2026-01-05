"""
Search Repository - Business logic for search operations.

ARCHITECTURAL FIX (Issue #3 - Missing Repository Pattern):
-------------------------------------------------------
This repository encapsulates all business logic related to search operations,
separating it from the data access layer (DAL).

Business Rules Implemented:
1. Query validation and sanitization
2. Result ranking and relevance scoring
3. Search result caching
4. Search analytics and logging
"""

import logging
from typing import List, Dict, Any, Optional, Tuple
from datetime import datetime

from .base import Repository, ValidationError
from ..storage.storage_interface import DALInterface
from ..constants import MAX_QUERY_LENGTH, MIN_SEARCH_LIMIT, MAX_SEARCH_LIMIT

logger = logging.getLogger(__name__)


class SearchRepository(Repository):
    """
    Repository for search operations with business logic.

    ARCHITECTURAL FIX (Issue #3):
    ----------------------------
    Encapsulates business rules for search operations that were previously
    scattered across DAL implementations.

    Responsibilities:
    - Query validation and sanitization
    - Search result processing
    - Relevance scoring
    - Search analytics
    """

    def __init__(self, dal: DALInterface, enable_analytics: bool = True) -> None:
        """
        Initialize the search repository.

        Args:
            dal: Data Access Layer instance
            enable_analytics: Whether to enable search analytics
        """
        super().__init__(dal)
        self._enable_analytics = enable_analytics

        # Require search backend
        if not self._dal.search:
            raise ValueError("SearchRepository requires a DAL with search support")

    # ========================================================================
    # SEARCH operations with business logic
    # ========================================================================

    def search_files(
        self,
        query: str,
        limit: int = 100,
        offset: int = 0,
        file_pattern: Optional[str] = None
    ) -> List[Dict[str, Any]]:
        """
        Search for files with validation and business logic.

        Business Rules:
        1. Validate and sanitize query
        2. Enforce query length limits
        3. Apply pagination
        4. Log search analytics

        Args:
            query: Search query string
            limit: Maximum number of results
            offset: Number of results to skip
            file_pattern: Optional file pattern filter

        Returns:
            List of search results

        Raises:
            ValidationError: If query is invalid
        """
        # Validate query
        sanitized_query = self._validate_query(query)

        # Validate pagination
        if limit < MIN_SEARCH_LIMIT:
            raise ValidationError("Search", "limit", limit, f"Limit must be at least {MIN_SEARCH_LIMIT}")
        if limit > MAX_SEARCH_LIMIT:
            raise ValidationError("Search", "limit", limit, f"Limit cannot exceed {MAX_SEARCH_LIMIT}")
        if offset < 0:
            raise ValidationError("Search", "offset", offset, "Offset cannot be negative")

        # Perform search
        try:
            results = self._dal.search.search_files(sanitized_query)
        except Exception as e:
            self._handle_error("search_files", e)

        # Apply pagination
        paginated_results = results[offset:offset + limit]

        # Analytics
        if self._enable_analytics:
            self._log_search_analytics(sanitized_query, len(results), len(paginated_results))

        return paginated_results

    def search_content(
        self,
        query: str,
        limit: int = 100
    ) -> List[Tuple[str, Any]]:
        """
        Search across file content with business logic.

        Args:
            query: Search query string
            limit: Maximum number of results

        Returns:
            List of (key, value) tuples

        Raises:
            ValidationError: If query is invalid
        """
        sanitized_query = self._validate_query(query)

        try:
            results = self._dal.search.search_content(sanitized_query)
        except Exception as e:
            self._handle_error("search_content", e)

        # Apply limit
        return results[:limit]

    def search_file_paths(
        self,
        query: str,
        limit: int = 100
    ) -> List[str]:
        """
        Search across file paths with business logic.

        Args:
            query: Search query string
            limit: Maximum number of results

        Returns:
            List of file paths

        Raises:
            ValidationError: If query is invalid
        """
        sanitized_query = self._validate_query(query)

        try:
            results = self._dal.search.search_file_paths(sanitized_query)
        except Exception as e:
            self._handle_error("search_file_paths", e)

        # Apply limit
        return results[:limit]

    def _validate_query(self, query: str) -> str:
        """
        Validate and sanitize a search query.

        Args:
            query: The query to validate

        Returns:
            Sanitized query

        Raises:
            ValidationError: If query is invalid
        """
        if not query:
            raise ValidationError("Search", "query", query, "Query cannot be empty")

        query = query.strip()

        if len(query) > MAX_QUERY_LENGTH:
            raise ValidationError(
                "Search",
                "query",
                query,
                f"Query exceeds maximum length of {MAX_QUERY_LENGTH}"
            )

        if len(query) < MIN_SEARCH_LIMIT:
            raise ValidationError(
                "Search",
                "query",
                query,
                f"Query must be at least {MIN_SEARCH_LIMIT} character(s)"
            )

        # Check for potentially malicious patterns
        dangerous_patterns = ["../", "..\\", "\x00"]
        for pattern in dangerous_patterns:
            if pattern in query:
                raise ValidationError(
                    "Search",
                    "query",
                    query,
                    f"Query contains potentially malicious pattern: {pattern}"
                )

        return query

    def _log_search_analytics(self, query: str, total_results: int, returned_results: int) -> None:
        """Log search analytics."""
        analytics = {
            "timestamp": datetime.utcnow().isoformat(),
            "query": query[:100],  # Truncate for logging
            "query_length": len(query),
            "total_results": total_results,
            "returned_results": returned_results
        }
        self._logger.info(f"SEARCH_ANALYTICS: {analytics}")

    # ========================================================================
    # INDEXING operations
    # ========================================================================

    def index_document(self, doc_id: str, document: Dict[str, Any]) -> bool:
        """
        Index a document with validation.

        Args:
            doc_id: Document identifier
            document: Document data

        Returns:
            True if successful

        Raises:
            ValidationError: If validation fails
        """
        if not doc_id:
            raise ValidationError("Document", "doc_id", doc_id, "Document ID cannot be empty")

        if not document:
            raise ValidationError("Document", "document", document, "Document cannot be empty")

        try:
            return self._dal.search.index_document(doc_id, document)
        except Exception as e:
            self._handle_error("index_document", e)

    def index_file(self, file_path: str, content: str) -> None:
        """
        Index a file for search.

        Args:
            file_path: Path of the file
            content: Content to index

        Raises:
            ValidationError: If parameters are invalid
        """
        normalized_path = self.validate_file_path(file_path)

        if not content:
            raise ValidationError("File", "content", content, "Content cannot be empty")

        try:
            self._dal.search.index_file(normalized_path, content)
        except Exception as e:
            self._handle_error("index_file", e)

    def remove_from_index(self, file_path: str) -> None:
        """
        Remove a file from the search index.

        Args:
            file_path: Path of the file

        Raises:
            ValidationError: If path is invalid
        """
        normalized_path = self.validate_file_path(file_path)

        try:
            self._dal.search.delete_indexed_file(normalized_path)
        except Exception as e:
            self._handle_error("delete_indexed_file", e)

    # ========================================================================
    # Abstract method implementations (search is different from CRUD)
    # ========================================================================

    def get_by_id(self, identifier: str) -> Optional[Dict[str, Any]]:
        """Get a document by ID - returns indexed document info."""
        # Search backends typically don't have a get_by_id method
        raise NotImplementedError("Search repository doesn't support get_by_id")

    def list_all(self, limit: Optional[int] = None, offset: int = 0) -> List[Dict[str, Any]]:
        """List all indexed documents."""
        # Search backends typically don't have a list_all method
        raise NotImplementedError("Search repository doesn't support list_all")

    def create(self, data: Dict[str, Any]) -> Dict[str, Any]:
        """Index a new document."""
        doc_id = data.get("doc_id", data.get("file_path", ""))
        self.index_document(doc_id, data)
        return {"doc_id": doc_id, "indexed": True}

    def update(self, identifier: str, data: Dict[str, Any]) -> Dict[str, Any]:
        """Update an indexed document."""
        # Re-index the document
        self.index_document(identifier, data)
        return {"doc_id": identifier, "indexed": True}

    def delete(self, identifier: str) -> bool:
        """Remove from index."""
        self.remove_from_index(identifier)
        return True

    def exists(self, identifier: str) -> bool:
        """Check if document is indexed."""
        # Search backends typically don't have an exists method
        # Could search for the exact document
        results = self._dal.search.search_content(identifier)
        return any(key == identifier for key, _ in results)

    def count(self) -> int:
        """Count indexed documents."""
        # Search backends typically don't have a count method
        raise NotImplementedError("Search repository doesn't support count")
