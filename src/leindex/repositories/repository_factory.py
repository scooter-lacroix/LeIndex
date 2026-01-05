"""
Repository Factory for creating repository instances.

ARCHITECTURAL FIX (Issue #3 - Missing Repository Pattern):
-------------------------------------------------------
This factory provides a centralized way to create repository instances
with proper DAL injection and configuration.

Usage:
    dal = get_dal_instance()
    factory = RepositoryFactory(dal)

    file_repo = factory.create_file_repository()
    search_repo = factory.create_search_repository()
    index_repo = factory.create_index_repository()
"""

import logging
from typing import Optional

from .file_repository import FileRepository
from .search_repository import SearchRepository
from .index_repository import IndexRepository
from ..storage.storage_interface import DALInterface

logger = logging.getLogger(__name__)


class RepositoryFactory:
    """
    Factory for creating repository instances with proper configuration.

    ARCHITECTURAL FIX (Issue #3):
    ----------------------------
    Centralizes repository creation with consistent configuration
    and dependency injection.
    """

    def __init__(self, dal: DALInterface, enable_audit: bool = True, enable_analytics: bool = True):
        """
        Initialize the repository factory.

        Args:
            dal: Data Access Layer instance to inject into repositories
            enable_audit: Whether to enable audit logging
            enable_analytics: Whether to enable search analytics
        """
        if dal is None:
            raise ValueError("DAL instance cannot be None")

        self._dal = dal
        self._enable_audit = enable_audit
        self._enable_analytics = enable_analytics

        self._logger = logging.getLogger(f"{self.__class__.__module__}.{self.__class__.__name__}")

    @property
    def dal(self) -> DALInterface:
        """Get the DAL instance."""
        return self._dal

    def create_file_repository(self, enable_audit: Optional[bool] = None) -> FileRepository:
        """
        Create a FileRepository instance.

        Args:
            enable_audit: Override default audit setting

        Returns:
            FileRepository instance

        Raises:
            ValueError: If DAL doesn't support metadata operations
        """
        if not self._dal.metadata:
            raise ValueError("DAL must support metadata operations for FileRepository")

        audit_enabled = enable_audit if enable_audit is not None else self._enable_audit

        repo = FileRepository(self._dal, enable_audit=audit_enabled)
        self._logger.debug(f"Created FileRepository with audit={audit_enabled}")

        return repo

    def create_search_repository(self, enable_analytics: Optional[bool] = None) -> SearchRepository:
        """
        Create a SearchRepository instance.

        Args:
            enable_analytics: Override default analytics setting

        Returns:
            SearchRepository instance

        Raises:
            ValueError: If DAL doesn't support search operations
        """
        if not self._dal.search:
            raise ValueError("DAL must support search operations for SearchRepository")

        analytics_enabled = enable_analytics if enable_analytics is not None else self._enable_analytics

        repo = SearchRepository(self._dal, enable_analytics=analytics_enabled)
        self._logger.debug(f"Created SearchRepository with analytics={analytics_enabled}")

        return repo

    def create_index_repository(self) -> IndexRepository:
        """
        Create an IndexRepository instance.

        Returns:
            IndexRepository instance

        Raises:
            ValueError: If DAL doesn't support metadata operations
        """
        if not self._dal.metadata:
            raise ValueError("DAL must support metadata operations for IndexRepository")

        repo = IndexRepository(self._dal)
        self._logger.debug("Created IndexRepository")

        return repo

    def create_all_repositories(self) -> dict:
        """
        Create all available repositories based on DAL capabilities.

        Returns:
            Dictionary with repository names as keys and instances as values

        Example:
            {
                "file": FileRepository(...),
                "search": SearchRepository(...),
                "index": IndexRepository(...)
            }
        """
        repositories = {}

        try:
            repositories["file"] = self.create_file_repository()
        except ValueError:
            self._logger.debug("FileRepository not available - DAL doesn't support metadata")

        try:
            repositories["search"] = self.create_search_repository()
        except ValueError:
            self._logger.debug("SearchRepository not available - DAL doesn't support search")

        try:
            repositories["index"] = self.create_index_repository()
        except ValueError:
            self._logger.debug("IndexRepository not available - DAL doesn't support metadata")

        self._logger.info(f"Created repositories: {list(repositories.keys())}")

        return repositories
