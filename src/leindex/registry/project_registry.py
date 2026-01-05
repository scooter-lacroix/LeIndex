"""
Project Registry for managing indexed projects.

This module provides a SQLite-based registry for tracking indexed projects,
their metadata, configurations, and statistics.
"""

import sqlite3
import hashlib
import json
from pathlib import Path
from typing import Optional, Dict, Any, List
from dataclasses import dataclass, asdict
from datetime import datetime
import logging

from .directories import get_registry_db_path

logger = logging.getLogger(__name__)


# ============================================================================
# Data Classes
# ============================================================================

@dataclass
class ProjectInfo:
    """
    Information about an indexed project.

    Attributes:
        id: Database ID of the project
        path: Absolute path to the project
        path_hash: SHA-256 hash of the absolute path
        indexed_at: Timestamp when the project was last indexed
        file_count: Number of files in the index
        config: Project configuration dictionary
        stats: Index statistics dictionary
        index_location: Path to the index data
    """
    id: Optional[int]
    path: str
    path_hash: str
    indexed_at: datetime
    file_count: int
    config: Dict[str, Any]
    stats: Dict[str, Any]
    index_location: str

    def to_dict(self) -> Dict[str, Any]:
        """
        Convert to dictionary representation.

        Returns:
            Dictionary with all fields
        """
        data = asdict(self)
        # Convert datetime to ISO string
        data['indexed_at'] = self.indexed_at.isoformat()
        # Remove id if None
        if data['id'] is None:
            del data['id']
        return data

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'ProjectInfo':
        """
        Create ProjectInfo from dictionary.

        Args:
            data: Dictionary with project info

        Returns:
            ProjectInfo instance
        """
        # Convert ISO string back to datetime
        if isinstance(data.get('indexed_at'), str):
            data['indexed_at'] = datetime.fromisoformat(data['indexed_at'])

        # Ensure required fields
        if 'config' not in data:
            data['config'] = {}
        if 'stats' not in data:
            data['stats'] = {}

        return cls(**data)


# ============================================================================
# Registry Exceptions
# ============================================================================

class RegistryError(Exception):
    """Base exception for registry errors."""
    pass


class ProjectNotFoundError(RegistryError):
    """Raised when a project is not found in the registry."""

    def __init__(self, project_path: str):
        self.project_path = project_path
        super().__init__(f"Project not found: {project_path}")


class DuplicateProjectError(RegistryError):
    """Raised when attempting to register a duplicate project."""

    def __init__(self, project_path: str):
        self.project_path = project_path
        super().__init__(f"Project already exists: {project_path}")


# ============================================================================
# Project Registry
# ============================================================================

class ProjectRegistry:
    """
    SQLite-based registry for managing indexed projects.

    This class provides CRUD operations for project metadata, including:
    - Insert new projects
    - Update existing projects
    - Delete projects
    - Query projects by path or hash
    - List all projects

    The registry uses SQLite with:
    - WAL mode for better concurrency
    - FULL synchronous for durability
    - SHA-256 path hashing for efficient lookups

    Attributes:
        db_path: Path to the SQLite database file
    """

    # SQL Schema definitions
    SQL_CREATE_PROJECTS_TABLE = """
        CREATE TABLE IF NOT EXISTS projects (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            path TEXT NOT NULL UNIQUE,
            path_hash TEXT UNIQUE NOT NULL,
            indexed_at TIMESTAMP NOT NULL,
            file_count INTEGER NOT NULL,
            config JSON NOT NULL,
            stats JSON NOT NULL,
            index_location TEXT NOT NULL,
            created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );
    """

    SQL_CREATE_METADATA_TABLE = """
        CREATE TABLE IF NOT EXISTS registry_metadata (
            key TEXT PRIMARY KEY,
            value TEXT NOT NULL,
            updated_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
        );
    """

    SQL_CREATE_INDEXES = [
        "CREATE INDEX IF NOT EXISTS idx_projects_path_hash ON projects(path_hash);",
        "CREATE INDEX IF NOT EXISTS idx_projects_indexed_at ON projects(indexed_at);",
    ]

    def __init__(self, db_path: Optional[str | Path] = None):
        """
        Initialize the project registry.

        Args:
            db_path: Path to the registry database. If None, uses default location.
        """
        if db_path is None:
            db_path = get_registry_db_path()
        else:
            db_path = Path(db_path)

        self.db_path = Path(db_path)
        self._ensure_db_exists()
        logger.info(f"ProjectRegistry initialized with db_path: {self.db_path}")

    # ------------------------------------------------------------------------
    # Database Management
    # ------------------------------------------------------------------------

    def _ensure_db_exists(self) -> None:
        """Ensure the database file and schema exist."""
        # Ensure parent directory exists
        self.db_path.parent.mkdir(parents=True, exist_ok=True)

        # Initialize schema
        with self._get_connection() as conn:
            # Create tables
            conn.execute(self.SQL_CREATE_PROJECTS_TABLE)
            conn.execute(self.SQL_CREATE_METADATA_TABLE)

            # Create indexes
            for index_sql in self.SQL_CREATE_INDEXES:
                conn.execute(index_sql)

            conn.commit()

            # CRITICAL: Set PRAGMA settings for durability and consistency
            # PRAGMA journal_mode = WAL: Use Write-Ahead Logging for better concurrency
            conn.execute("PRAGMA journal_mode=WAL;")

            # PRAGMA synchronous = FULL: Ensure all writes are synced to disk
            conn.execute("PRAGMA synchronous=FULL;")

            # PRAGMA foreign_keys = ON: Enforce foreign key constraints
            conn.execute("PRAGMA foreign_keys=ON;")
            conn.commit()

            logger.debug(f"Database schema initialized: {self.db_path}")

    def _get_connection(self) -> sqlite3.Connection:
        """
        Get a database connection with proper configuration.

        Returns:
            SQLite connection object
        """
        conn = sqlite3.connect(self.db_path)
        conn.row_factory = sqlite3.Row  # Enable column access by name
        return conn

    @staticmethod
    def _hash_path(path: str) -> str:
        """
        Compute SHA-256 hash of a path.

        Args:
            path: Absolute path string

        Returns:
            Hexadecimal SHA-256 hash

        Examples:
            >>> ProjectRegistry._hash_path("/home/user/project")
            'a1b2c3d4e5f6...'
        """
        sha256 = hashlib.sha256()
        sha256.update(path.encode('utf-8'))
        return sha256.hexdigest()

    # ------------------------------------------------------------------------
    # CRUD Operations
    # ------------------------------------------------------------------------

    def insert(
        self,
        path: str,
        indexed_at: datetime,
        file_count: int,
        config: Dict[str, Any],
        stats: Dict[str, Any],
        index_location: str
    ) -> ProjectInfo:
        """
        Insert a new project into the registry.

        Args:
            path: Absolute path to the project
            indexed_at: When the project was indexed
            file_count: Number of files in the index
            config: Project configuration
            stats: Index statistics
            index_location: Path to index data

        Returns:
            Created ProjectInfo

        Raises:
            DuplicateProjectError: If project already exists
            RegistryError: On database errors
        """
        path_hash = self._hash_path(path)

        try:
            with self._get_connection() as conn:
                cursor = conn.execute(
                    """
                    INSERT INTO projects (
                        path, path_hash, indexed_at, file_count,
                        config, stats, index_location
                    ) VALUES (?, ?, ?, ?, ?, ?, ?)
                    """,
                    (
                        path,
                        path_hash,
                        indexed_at.isoformat(),
                        file_count,
                        json.dumps(config),
                        json.dumps(stats),
                        index_location
                    )
                )
                conn.commit()

                project_id = cursor.lastrowid
                logger.info(f"Inserted project: {path} (id={project_id})")

                return ProjectInfo(
                    id=project_id,
                    path=path,
                    path_hash=path_hash,
                    indexed_at=indexed_at,
                    file_count=file_count,
                    config=config,
                    stats=stats,
                    index_location=index_location
                )

        except sqlite3.IntegrityError as e:
            if "UNIQUE constraint failed" in str(e):
                raise DuplicateProjectError(path) from e
            raise RegistryError(f"Failed to insert project: {e}") from e

    def update(
        self,
        path: str,
        indexed_at: Optional[datetime] = None,
        file_count: Optional[int] = None,
        config: Optional[Dict[str, Any]] = None,
        stats: Optional[Dict[str, Any]] = None,
        index_location: Optional[str] = None
    ) -> ProjectInfo:
        """
        Update an existing project in the registry.

        Only the fields that are provided (not None) will be updated.

        Args:
            path: Absolute path to the project
            indexed_at: New indexed_at timestamp
            file_count: New file count
            config: New configuration
            stats: New statistics
            index_location: New index location

        Returns:
            Updated ProjectInfo

        Raises:
            ProjectNotFoundError: If project doesn't exist
            RegistryError: On database errors
        """
        # Build update query dynamically based on provided fields
        updates = []
        params = []

        if indexed_at is not None:
            updates.append("indexed_at = ?")
            params.append(indexed_at.isoformat())

        if file_count is not None:
            updates.append("file_count = ?")
            params.append(file_count)

        if config is not None:
            updates.append("config = ?")
            params.append(json.dumps(config))

        if stats is not None:
            updates.append("stats = ?")
            params.append(json.dumps(stats))

        if index_location is not None:
            updates.append("index_location = ?")
            params.append(index_location)

        if not updates:
            raise ValueError("At least one field must be provided for update")

        updates.append("updated_at = CURRENT_TIMESTAMP")
        params.append(path)  # WHERE clause parameter

        query = f"UPDATE projects SET {', '.join(updates)} WHERE path = ?"

        try:
            with self._get_connection() as conn:
                cursor = conn.execute(query, params)
                conn.commit()

                if cursor.rowcount == 0:
                    raise ProjectNotFoundError(path)

                logger.info(f"Updated project: {path}")

                # Fetch and return updated project
                return self.get_by_path(path)

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to update project: {e}") from e

    def delete(self, path: str) -> bool:
        """
        Delete a project from the registry.

        Args:
            path: Absolute path to the project

        Returns:
            True if deleted, False if not found

        Raises:
            RegistryError: On database errors
        """
        try:
            with self._get_connection() as conn:
                cursor = conn.execute("DELETE FROM projects WHERE path = ?", (path,))
                conn.commit()

                deleted = cursor.rowcount > 0
                if deleted:
                    logger.info(f"Deleted project: {path}")
                else:
                    logger.warning(f"Project not found for deletion: {path}")

                return deleted

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to delete project: {e}") from e

    def get_by_path(self, path: str) -> ProjectInfo:
        """
        Get a project by its path.

        Args:
            path: Absolute path to the project

        Returns:
            ProjectInfo

        Raises:
            ProjectNotFoundError: If project doesn't exist
        """
        try:
            with self._get_connection() as conn:
                cursor = conn.execute(
                    "SELECT * FROM projects WHERE path = ?",
                    (path,)
                )
                row = cursor.fetchone()

                if row is None:
                    raise ProjectNotFoundError(path)

                return self._row_to_project_info(row)

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to get project: {e}") from e

    def get_by_path_hash(self, path_hash: str) -> Optional[ProjectInfo]:
        """
        Get a project by its path hash.

        Args:
            path_hash: SHA-256 hash of the project path

        Returns:
            ProjectInfo or None if not found
        """
        try:
            with self._get_connection() as conn:
                cursor = conn.execute(
                    "SELECT * FROM projects WHERE path_hash = ?",
                    (path_hash,)
                )
                row = cursor.fetchone()

                if row is None:
                    return None

                return self._row_to_project_info(row)

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to get project by hash: {e}") from e

    def get_by_id(self, project_id: int) -> Optional[ProjectInfo]:
        """
        Get a project by its database ID.

        Args:
            project_id: Database ID of the project

        Returns:
            ProjectInfo or None if not found
        """
        try:
            with self._get_connection() as conn:
                cursor = conn.execute(
                    "SELECT * FROM projects WHERE id = ?",
                    (project_id,)
                )
                row = cursor.fetchone()

                if row is None:
                    return None

                return self._row_to_project_info(row)

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to get project by ID: {e}") from e

    def list_all(
        self,
        limit: Optional[int] = None,
        offset: int = 0,
        order_by: str = "indexed_at",
        descending: bool = True
    ) -> List[ProjectInfo]:
        """
        List all projects in the registry.

        Args:
            limit: Maximum number of results
            offset: Number of results to skip
            order_by: Column to order by (default: indexed_at)
            descending: Whether to sort in descending order

        Returns:
            List of ProjectInfo
        """
        try:
            direction = "DESC" if descending else "ASC"
            query = f"SELECT * FROM projects ORDER BY {order_by} {direction}"

            if limit is not None:
                query += f" LIMIT {limit} OFFSET {offset}"

            with self._get_connection() as conn:
                cursor = conn.execute(query)
                rows = cursor.fetchall()

                return [self._row_to_project_info(row) for row in rows]

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to list projects: {e}") from e

    def count(self) -> int:
        """
        Get the total number of projects in the registry.

        Returns:
            Number of projects
        """
        try:
            with self._get_connection() as conn:
                cursor = conn.execute("SELECT COUNT(*) FROM projects")
                return cursor.fetchone()[0]

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to count projects: {e}") from e

    def exists(self, path: str) -> bool:
        """
        Check if a project exists in the registry.

        Args:
            path: Absolute path to the project

        Returns:
            True if project exists, False otherwise
        """
        try:
            with self._get_connection() as conn:
                cursor = conn.execute(
                    "SELECT 1 FROM projects WHERE path = ? LIMIT 1",
                    (path,)
                )
                return cursor.fetchone() is not None

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to check project existence: {e}") from e

    # ------------------------------------------------------------------------
    # Metadata Operations
    # ------------------------------------------------------------------------

    def set_metadata(self, key: str, value: str) -> None:
        """
        Set a metadata key-value pair.

        Args:
            key: Metadata key
            value: Metadata value

        Raises:
            RegistryError: On database errors
        """
        try:
            with self._get_connection() as conn:
                conn.execute(
                    """
                    INSERT OR REPLACE INTO registry_metadata (key, value, updated_at)
                    VALUES (?, ?, CURRENT_TIMESTAMP)
                    """,
                    (key, value)
                )
                conn.commit()

                logger.debug(f"Set metadata: {key}")

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to set metadata: {e}") from e

    def get_metadata(self, key: str) -> Optional[str]:
        """
        Get a metadata value by key.

        Args:
            key: Metadata key

        Returns:
            Metadata value or None if not found
        """
        try:
            with self._get_connection() as conn:
                cursor = conn.execute(
                    "SELECT value FROM registry_metadata WHERE key = ?",
                    (key,)
                )
                row = cursor.fetchone()

                return row[0] if row else None

        except sqlite3.Error as e:
            raise RegistryError(f"Failed to get metadata: {e}") from e

    # ------------------------------------------------------------------------
    # Utility Methods
    # ------------------------------------------------------------------------

    @staticmethod
    def _row_to_project_info(row: sqlite3.Row) -> ProjectInfo:
        """
        Convert a database row to ProjectInfo.

        Args:
            row: SQLite row object

        Returns:
            ProjectInfo instance
        """
        return ProjectInfo(
            id=row["id"],
            path=row["path"],
            path_hash=row["path_hash"],
            indexed_at=datetime.fromisoformat(row["indexed_at"]),
            file_count=row["file_count"],
            config=json.loads(row["config"]),
            stats=json.loads(row["stats"]),
            index_location=row["index_location"]
        )

    def close(self) -> None:
        """
        Close the registry and perform cleanup.

        Note: SQLite connections are managed per-operation, so this is
        primarily for resource cleanup in tests or explicit shutdown.
        """
        # Checkpoint WAL to ensure all changes are committed
        try:
            with self._get_connection() as conn:
                conn.execute("PRAGMA wal_checkpoint(TRUNCATE);")
                conn.commit()

            logger.info(f"Registry closed: {self.db_path}")

        except sqlite3.Error as e:
            logger.warning(f"Error during WAL checkpoint: {e}")
