"""
Factory for Data Access Layer (DAL) instances.
This module provides a central point for creating and configuring
the SQLite + DuckDB DAL implementation.

CORRECT ARCHITECTURE:
- Vector Search: LEANN (HNSW)
- Full-Text Search: Tantivy (Lucene)
- Metadata Storage: SQLite (OLTP)
- Analytics: DuckDB (OLAP)
- Async Processing: asyncio (no message broker)

FORBIDDEN SYSTEMS (NOT USED):
- PostgreSQL (replaced by SQLite + DuckDB)
- Elasticsearch (replaced by Tantivy)
- RabbitMQ (replaced by asyncio)
"""

import os
from typing import Optional, Dict, Any

from ..config_manager import ConfigManager
from .storage_interface import DALInterface, StorageInterface, FileMetadataInterface, SearchInterface
from .sqlite_storage import SQLiteDAL, SQLiteStorage, SQLiteFileMetadata, SQLiteSearch
from .duckdb_storage import DuckDBDAL
from ..logger_config import setup_logging

logger = setup_logging()


class SQLiteDuckDBDAL(DALInterface):
    """
    The LeIndex DAL implementation using SQLite for transactional metadata
    and DuckDB for analytical queries.

    This is the ONLY backend for LeIndex:
    - SQLite: Fast OLTP operations for metadata, versions, diffs
    - DuckDB: Fast OLAP queries for analytics and reporting
    - No external dependencies (both are embeddable)
    - Zero setup required
    """

    def __init__(self, sqlite_db_path: str, duckdb_db_path: Optional[str] = None,
                 enable_fts: bool = True):
        """
        Initialize SQLite + DuckDB DAL.

        Args:
            sqlite_db_path: Path to SQLite database file
            duckdb_db_path: Path to DuckDB database file (defaults to sqlite_db_path + .duckdb)
            enable_fts: Enable full-text search in SQLite

        Raises:
            ValueError: If sqlite_db_path is empty or invalid
            IOError: If directory creation fails, database is not writable, or insufficient disk space
        """
        # Validate sqlite_db_path
        if not sqlite_db_path:
            raise ValueError("sqlite_db_path cannot be empty")

        # Create directory if needed
        db_dir = os.path.dirname(sqlite_db_path)
        if db_dir and not os.path.exists(db_dir):
            try:
                os.makedirs(db_dir, exist_ok=True)
                logger.info(f"Created database directory: {db_dir}")
            except OSError as e:
                raise IOError(f"Failed to create database directory {db_dir}: {e}")

        # Check write permissions if file exists
        if os.path.exists(sqlite_db_path) and not os.access(sqlite_db_path, os.W_OK):
            raise IOError(f"SQLite database not writable: {sqlite_db_path}")

        # Check disk space (require at least 100MB)
        try:
            stat = os.statvfs(os.path.dirname(sqlite_db_path) if os.path.dirname(sqlite_db_path) else ".")
            available_space = stat.f_bavail * stat.f_frsize
            if available_space < 100 * 1024 * 1024:  # 100MB minimum
                raise IOError(f"Insufficient disk space for database. At least 100MB required, {available_space // (1024*1024)}MB available")
        except (OSError, AttributeError) as e:
            logger.warning(f"Could not check disk space: {e}")

        if duckdb_db_path is None:
            duckdb_db_path = sqlite_db_path + ".duckdb"

        self._metadata_backend = SQLiteFileMetadata(sqlite_db_path)
        self._storage_backend = SQLiteStorage(sqlite_db_path)
        self._search_backend = SQLiteSearch(sqlite_db_path, enable_fts=enable_fts)
        self._analytics_backend = DuckDBDAL(duckdb_db_path, sqlite_db_path)

        logger.info(
            f"SQLiteDuckDBDAL initialized: sqlite={sqlite_db_path}, "
            f"duckdb={duckdb_db_path}, fts={enable_fts}"
        )

    @property
    def storage(self) -> StorageInterface:
        return self._storage_backend

    @property
    def metadata(self) -> FileMetadataInterface:
        return self._metadata_backend

    @property
    def search(self) -> SearchInterface:
        return self._search_backend

    @property
    def analytics(self) -> DuckDBDAL:
        """Return the DuckDB analytics backend."""
        return self._analytics_backend

    def close(self) -> None:
        """Close all underlying storage backends."""
        if hasattr(self, '_storage_backend'):
            self._storage_backend.close()
        if hasattr(self, '_metadata_backend'):
            self._metadata_backend.close()
        if hasattr(self, '_search_backend'):
            self._search_backend.close()
        if hasattr(self, '_analytics_backend'):
            self._analytics_backend.close()

    def clear_all(self) -> bool:
        """Clear all data from all underlying storage backends."""
        metadata_cleared = self._metadata_backend.clear()
        storage_cleared = self._storage_backend.clear()
        search_cleared = self._search_backend.clear()
        # DuckDB analytics is read-only, no need to clear
        return metadata_cleared and storage_cleared and search_cleared


def get_dal_instance() -> DALInterface:
    """
    Factory function to get the SQLite + DuckDB DAL instance.

    This is the ONLY backend for LeIndex. All other backends have been removed.

    Args:
        No direct config argument, settings are loaded from ConfigManager and environment variables.

    Returns:
        An instance of SQLiteDuckDBDAL.

    Raises:
        ValueError: If required configuration is missing.
    """
    # Initialize ConfigManager to get application-wide DAL settings
    config_manager = ConfigManager()
    dal_settings = config_manager.get_dal_settings()

    # Get database path from settings or environment
    # Default backend is SQLite + DuckDB
    sqlite_db_path = dal_settings.get("db_path", os.path.join("data", "code_index.db"))
    duckdb_db_path = dal_settings.get("duckdb_db_path")
    enable_fts = dal_settings.get("sqlite_enable_fts", True)

    return SQLiteDuckDBDAL(sqlite_db_path, duckdb_db_path, enable_fts=enable_fts)
