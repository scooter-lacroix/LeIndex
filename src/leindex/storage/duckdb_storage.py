"""
DuckDB-based analytical storage backend for LeIndex.

This module implements DuckDB storage for fast analytical queries on file metadata,
symbol tables, and references. DuckDB excels at analytical workloads and can directly
query SQLite databases, Parquet files, and CSV files without data duplication.

Architecture:
- DuckDB for OLAP (Online Analytical Processing) queries
- SQLite for OLTP (Online Transaction Processing) operations
- DuckDB can attach SQLite databases for cross-database queries
"""

import logging
import os
from contextlib import contextmanager
from typing import Any, Dict, List, Optional, Tuple

try:
    import duckdb
except ImportError:
    raise ImportError(
        "DuckDB is not installed. Please install it with: pip install duckdb"
    )

from .storage_interface import StorageInterface, FileMetadataInterface, SearchInterface

logger = logging.getLogger(__name__)


class DuckDBAnalytics:
    """
    DuckDB-based analytical storage backend.

    This class provides fast analytical queries on metadata stored in SQLite.
    DuckDB can directly attach SQLite databases and query them efficiently.

    Key features:
    - Direct querying of SQLite databases without ETL
    - Fast aggregations and analytics on large datasets
    - Columnar storage for analytical workloads
    - Support for complex queries and window functions
    """

    def __init__(self, db_path: str, sqlite_db_path: Optional[str] = None):
        """
        Initialize DuckDB analytics backend.

                Initialize DuckDB analytics backend.

        Creates or connects to a DuckDB database for OLAP operations and optionally
        attaches a SQLite database for cross-database analytical queries.

        Args:
            db_path: Path to DuckDB database file (can be :memory: for in-memory)
            sqlite_db_path: Optional path to SQLite database to attach for queries
        """
        self.db_path = db_path
        self.sqlite_db_path = sqlite_db_path
        self._ensure_db_directory()
        self._init_db()

        logger.info(
            f"DuckDBAnalytics initialized with db_path={db_path}, "
            f"sqlite_db_path={sqlite_db_path}"
        )

    def _ensure_db_directory(self) -> None:
        """
        Ensure the directory for the database exists.

        Creates the directory tree if it doesn't exist and the path is not
        an in-memory database.

        Raises:
            OSError: If directory creation fails.
        """
        if self.db_path != ":memory:":
            db_dir = os.path.dirname(self.db_path)
            if db_dir and not os.path.exists(db_dir):
                os.makedirs(db_dir, exist_ok=True)

    def _init_db(self) -> None:
        """
        Initialize the DuckDB database and attach SQLite if provided.

        Creates DuckDB connection with optimized settings and optionally attaches
        a SQLite database for cross-database queries. Creates analytical views
        for common queries.

        Raises:
            ValueError: If SQLite path is invalid or unsafe.
            Exception: If connection or attachment fails.

        Notes:
            - Connection is closed on initialization failure
            - Analytical views are created only if SQLite is attached
            - Database attachment is idempotent - checks if already attached
        """
        # Create DuckDB connection with unsigned extensions allowed
        # This must be set at connection time, cannot be changed after database is created
        self.conn = duckdb.connect(self.db_path, config={'allow_unsigned_extensions': 'true'})

        try:
            # Configure DuckDB for optimal performance
            self.conn.execute("SET enable_progress_bar = false")
            self.conn.execute("SET enable_object_cache = true")

            # Attach SQLite database if provided
            if self.sqlite_db_path and os.path.exists(self.sqlite_db_path):
                try:
                    # CRITICAL FIX: Check if database is already attached before attempting
                    # This prevents "database already exists" errors when multiple
                    # DuckDBAnalytics instances are created with the same SQLite database
                    attached_dbs = self.conn.execute("SELECT database_name FROM duckdb_databases()").fetchall()
                    attached_names = [row[0] for row in attached_dbs]

                    if 'sqlite_db' in attached_names:
                        logger.info(f"SQLite database 'sqlite_db' already attached, skipping attachment")
                    else:
                        # Validate and sanitize the SQLite path to prevent SQL injection
                        # DuckDB doesn't support parameterized queries for ATTACH
                        if not self._is_safe_path(self.sqlite_db_path):
                            raise ValueError(f"Invalid SQLite database path: {self.sqlite_db_path}")

                        # Escape single quotes in the path to prevent SQL injection
                        safe_path = self.sqlite_db_path.replace("'", "''")
                        self.conn.execute(f"ATTACH '{safe_path}' AS sqlite_db (TYPE sqlite)")
                        logger.info(f"Attached SQLite database: {self.sqlite_db_path}")
                except Exception as e:
                    logger.warning(f"Could not attach SQLite database: {e}")
                    # Re-raise to ensure cleanup in outer try/finally
                    raise

            # Create analytical views if SQLite is attached
            if self.sqlite_db_path:
                self._create_analytical_views()

        except Exception as e:
            # Ensure connection is closed on initialization error
            logger.error(f"Error initializing database, closing connection: {e}")
            self.conn.close()
            raise

    def _is_safe_path(self, path: str) -> bool:
        """
        Validate that a path is safe to use in SQL queries.

        Args:
            path: Path to validate

        Returns:
            True if path is safe, False otherwise
        """
        # Reject paths with potentially dangerous characters
        dangerous_chars = [";", "\x00", "\n", "\r", "\t"]
        if any(char in path for char in dangerous_chars):
            return False

        # Reject empty paths
        if not path or not path.strip():
            return False

        # Reject paths with SQL comment markers
        if "--" in path or "/*" in path or "*/" in path:
            return False

        # Basic path validation - should be a valid absolute or relative path
        # Allow alphanumeric, underscores, hyphens, dots, slashes, and backslashes
        import re
        if not re.match(r'^[a-zA-Z0-9_\-./\\\:]+$', path):
            return False

        return True

    def _create_analytical_views(self) -> None:
        """Create optimized views for analytical queries."""
        try:
            # Create view for file statistics
            # Note: DuckDB doesn't support IF NOT EXISTS with CREATE OR REPLACE VIEW
            # We use CREATE OR REPLACE VIEW directly and catch errors
            self.conn.execute("""
                CREATE OR REPLACE VIEW file_stats AS
                SELECT
                    file_type,
                    extension,
                    COUNT(*) as file_count,
                    COUNT(DISTINCT file_path) as unique_paths,
                    MIN(created_at) as oldest_file,
                    MAX(updated_at) as newest_file
                FROM sqlite_db.files
                GROUP BY file_type, extension
            """)

            # Create view for version statistics
            self.conn.execute("""
                CREATE OR REPLACE VIEW version_stats AS
                SELECT
                    file_path,
                    COUNT(*) as version_count,
                    MIN(timestamp) as first_version,
                    MAX(timestamp) as latest_version,
                    SUM(size) as total_size,
                    AVG(size) as avg_size
                FROM sqlite_db.file_versions
                GROUP BY file_path
            """)

            # Create view for diff statistics
            self.conn.execute("""
                CREATE OR REPLACE VIEW diff_stats AS
                SELECT
                    operation_type,
                    diff_type,
                    COUNT(*) as diff_count,
                    file_path
                FROM sqlite_db.file_diffs
                GROUP BY operation_type, diff_type, file_path
            """)

            logger.info("Created analytical views in DuckDB")
        except Exception as e:
            logger.warning(f"Could not create analytical views: {e}")

    @contextmanager
    def _transaction(self, readonly: bool = False):
        """
        Context manager for transaction handling.

        Args:
            readonly: Whether the transaction is read-only

        Yields:
            DuckDB connection

        Raises:
            Exception: Re-raises any exception, rolling back the transaction
        """
        try:
            if not readonly:
                self.conn.execute("BEGIN TRANSACTION")
            yield self.conn
            if not readonly:
                self.conn.execute("COMMIT")
        except Exception as e:
            if not readonly:
                try:
                    self.conn.execute("ROLLBACK")
                except Exception as rollback_error:
                    logger.error(f"Failed to rollback transaction: {rollback_error}")
            raise

    def query_file_stats(self) -> List[Dict[str, Any]]:
        """
        Query file statistics aggregated by file type and extension.

        Returns:
            List of dictionaries containing file statistics
        """
        try:
            with self._transaction(readonly=True):
                result = self.conn.execute("SELECT * FROM file_stats ORDER BY file_count DESC")
                columns = [desc[0] for desc in result.description]
                return [dict(zip(columns, row)) for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error querying file stats: {e}")
            return []

    def query_version_stats(self, limit: int = 100) -> List[Dict[str, Any]]:
        """
        Query version statistics for files.

        Args:
            limit: Maximum number of results to return

        Returns:
            List of dictionaries containing version statistics
        """
        try:
            result = self.conn.execute(
                "SELECT * FROM version_stats ORDER BY version_count DESC LIMIT ?",
                [limit]
            )
            columns = [desc[0] for desc in result.description]
            return [dict(zip(columns, row)) for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error querying version stats: {e}")
            return []

    def query_files_by_extension(self, extension: str) -> List[Dict[str, Any]]:
        """
        Query all files with a specific extension.

        Args:
            extension: File extension to filter by (e.g., 'py', 'js')

        Returns:
            List of file metadata dictionaries
        """
        try:
            result = self.conn.execute(
                "SELECT * FROM sqlite_db.files WHERE extension = ? ORDER BY file_path",
                [extension]
            )
            columns = [desc[0] for desc in result.description]
            return [dict(zip(columns, row)) for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error querying files by extension: {e}")
            return []

    def query_recent_files(self, hours: int = 24) -> List[Dict[str, Any]]:
        """
        Query files updated within the specified time window.

        Args:
            hours: Number of hours to look back

        Returns:
            List of recently updated file metadata
        """
        try:
            result = self.conn.execute("""
                SELECT *
                FROM sqlite_db.files
                WHERE updated_at >= datetime('now', '-' || ? || ' hours')
                ORDER BY updated_at DESC
            """, [str(hours)])
            columns = [desc[0] for desc in result.description]
            return [dict(zip(columns, row)) for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error querying recent files: {e}")
            return []

    def query_file_history(self, file_path: str) -> Dict[str, Any]:
        """
        Query complete history for a file including versions and diffs.

        Args:
            file_path: Path to the file

        Returns:
            Dictionary containing file history with versions and diffs
        """
        try:
            # Get file metadata
            file_result = self.conn.execute(
                "SELECT * FROM sqlite_db.files WHERE file_path = ?",
                [file_path]
            ).fetchone()

            if not file_result:
                return {}

            columns = [desc[0] for desc in self.conn.description]
            file_info = dict(zip(columns, file_result))

            # Get versions
            versions_result = self.conn.execute(
                "SELECT * FROM sqlite_db.file_versions WHERE file_path = ? ORDER BY timestamp",
                [file_path]
            )
            version_columns = [desc[0] for desc in versions_result.description]
            versions = [dict(zip(version_columns, row)) for row in versions_result.fetchall()]

            # Get diffs
            diffs_result = self.conn.execute(
                "SELECT * FROM sqlite_db.file_diffs WHERE file_path = ? ORDER BY timestamp",
                [file_path]
            )
            diff_columns = [desc[0] for desc in diffs_result.description]
            diffs = [dict(zip(diff_columns, row)) for row in diffs_result.fetchall()]

            return {
                "file_info": file_info,
                "versions": versions,
                "diffs": diffs
            }
        except Exception as e:
            logger.error(f"Error querying file history: {e}")
            return {}

    def query_most_modified_files(self, limit: int = 50) -> List[Dict[str, Any]]:
        """
        Query files with the most modifications (versions + diffs).

        Args:
            limit: Maximum number of results

        Returns:
            List of files with modification counts
        """
        try:
            result = self.conn.execute("""
                SELECT
                    f.file_path,
                    f.file_type,
                    f.extension,
                    COUNT(DISTINCT v.version_id) as version_count,
                    COUNT(DISTINCT d.diff_id) as diff_count,
                    (COUNT(DISTINCT v.version_id) + COUNT(DISTINCT d.diff_id)) as total_changes
                FROM sqlite_db.files f
                LEFT JOIN sqlite_db.file_versions v ON f.file_path = v.file_path
                LEFT JOIN sqlite_db.file_diffs d ON f.file_path = d.file_path
                GROUP BY f.file_path, f.file_type, f.extension
                ORDER BY total_changes DESC
                LIMIT ?
            """, [limit])
            columns = [desc[0] for desc in result.description]
            return [dict(zip(columns, row)) for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error querying most modified files: {e}")
            return []

    def query_directory_summary(self, directory_path: str) -> Dict[str, Any]:
        """
        Query summary statistics for a directory.

        Args:
            directory_path: Path to the directory

        Returns:
            Dictionary containing directory summary
        """
        try:
            # File count by type
            type_result = self.conn.execute("""
                SELECT
                    file_type,
                    COUNT(*) as count
                FROM sqlite_db.files
                WHERE file_path LIKE ?
                GROUP BY file_type
            """, [f"{directory_path}%"])

            file_types = {row[0]: row[1] for row in type_result.fetchall()}

            # Extension distribution
            ext_result = self.conn.execute("""
                SELECT
                    extension,
                    COUNT(*) as count
                FROM sqlite_db.files
                WHERE file_path LIKE ?
                GROUP BY extension
                ORDER BY count DESC
            """, [f"{directory_path}%"])

            extensions = [{"extension": row[0], "count": row[1]} for row in ext_result.fetchall()]

            # Total size from versions
            size_result = self.conn.execute("""
                SELECT
                    COUNT(*) as total_versions,
                    SUM(size) as total_size,
                    AVG(size) as avg_size
                FROM sqlite_db.file_versions
                WHERE file_path LIKE ?
            """, [f"{directory_path}%"])

            size_row = size_result.fetchone()
            size_columns = [desc[0] for desc in size_result.description]
            size_info = dict(zip(size_columns, size_row)) if size_row else {}

            return {
                "directory_path": directory_path,
                "file_types": file_types,
                "extensions": extensions,
                "size_info": size_info
            }
        except Exception as e:
            logger.error(f"Error querying directory summary: {e}")
            return {}

    def export_to_parquet(self, table_name: str, output_path: str) -> bool:
        """
        Export a table or view to Parquet format for external analysis.

        Args:
            table_name: Name of the table or view to export
            output_path: Path where Parquet file should be written

        Returns:
            True if successful, False otherwise
        """
        try:
            # Validate inputs to prevent SQL injection
            # DuckDB doesn't support parameterized queries for COPY
            if not self._is_safe_table_name(table_name):
                raise ValueError(f"Invalid table name: {table_name}")

            if not self._is_safe_path(output_path):
                raise ValueError(f"Invalid output path: {output_path}")

            # Escape single quotes and use validated inputs
            safe_table = table_name.replace("'", "''")
            safe_path = output_path.replace("'", "''")

            self.conn.execute(f"COPY {safe_table} TO '{safe_path}' (FORMAT PARQUET)")
            logger.info(f"Exported {table_name} to {output_path}")
            return True
        except Exception as e:
            logger.error(f"Error exporting to Parquet: {e}")
            return False

    def _is_safe_table_name(self, table_name: str) -> bool:
        """
        Validate that a table name is safe to use in SQL queries.

        Args:
            table_name: Table name to validate

        Returns:
            True if table name is safe, False otherwise
        """
        # Reject empty table names
        if not table_name or not table_name.strip():
            return False

        # Reject dangerous characters
        dangerous_chars = [";", "'", '"', "\x00", "\n", "\r", "\t", "-", ".", " ", "/", "\\"]
        if any(char in table_name for char in dangerous_chars):
            return False

        # Reject SQL comment markers
        if "--" in table_name or "/*" in table_name or "*/" in table_name:
            return False

        # Only allow alphanumeric and underscores
        import re
        if not re.match(r'^[a-zA-Z0-9_]+$', table_name):
            return False

        # Don't allow SQL keywords
        sql_keywords = {'SELECT', 'INSERT', 'UPDATE', 'DELETE', 'DROP', 'ALTER', 'CREATE', 'TRUNCATE'}
        if table_name.upper() in sql_keywords:
            return False

        return True

    def execute_custom_query(self, query: str, params: Optional[List] = None) -> List[Dict[str, Any]]:
        """
        Execute a custom SQL query.

        Args:
            query: SQL query string
            params: Optional query parameters

        Returns:
            List of result dictionaries
        """
        try:
            if params:
                result = self.conn.execute(query, params)
            else:
                result = self.conn.execute(query)

            columns = [desc[0] for desc in result.description]
            return [dict(zip(columns, row)) for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error executing custom query: {e}")
            return []

    def close(self) -> None:
        """Close the DuckDB connection."""
        if hasattr(self, 'conn'):
            self.conn.close()
            logger.info("DuckDB connection closed")


class DuckDBSearch(SearchInterface):
    """
    DuckDB-based search implementation for full-text and analytical queries.

    This leverages DuckDB's powerful query engine for complex searches
    and aggregations across metadata.
    """

    def __init__(self, db_path: str, sqlite_db_path: Optional[str] = None):
        """
        Initialize DuckDB search backend.

                Initialize DuckDB analytics backend.

        Creates or connects to a DuckDB database for OLAP operations and optionally
        attaches a SQLite database for cross-database analytical queries.

        Args:
            db_path: Path to DuckDB database file
            sqlite_db_path: Optional path to SQLite database for metadata queries
        """
        self.db_path = db_path
        self.analytics = DuckDBAnalytics(db_path, sqlite_db_path)
        logger.info(f"DuckDBSearch initialized with db_path={db_path}")

    def search_content(self, query: str, is_regex: bool = False) -> List[Tuple[str, Any]]:
        """
        Search across file content using metadata queries.

        Note: For true full-text search, use SQLiteSearch with FTS enabled.
        This method provides metadata-based filtering.

        Args:
            query: Search query
            is_regex: Whether to treat query as regex (not used in metadata search)

        Returns:
            List of (key, value) tuples
        """
        # DuckDB is not optimized for full-text content search
        # Delegate to SQLite or implement custom logic
        logger.warning("DuckDB is not optimized for full-text search. Use SQLiteSearch with FTS.")
        return []

    def search_file_paths(self, query: str) -> List[str]:
        """
        Search file paths using pattern matching.

        Args:
            query: Search pattern (supports SQL LIKE syntax)

        Returns:
            List of matching file paths
        """
        try:
            result = self.analytics.conn.execute("""
                SELECT file_path
                FROM sqlite_db.files
                WHERE file_path LIKE ?
                ORDER BY file_path
            """, [f"%{query}%"])

            return [row[0] for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error searching file paths: {e}")
            return []

    def search_files(self, query: str) -> List[Dict[str, Any]]:
        """
        Search for files matching the query criteria.

        Args:
            query: Search query (can be file path, extension, or metadata content)

        Returns:
            List of matching file metadata dictionaries
        """
        try:
            result = self.analytics.conn.execute("""
                SELECT *
                FROM sqlite_db.files
                WHERE file_path LIKE ?
                   OR extension LIKE ?
                   OR metadata LIKE ?
                ORDER BY file_path
                LIMIT 1000
            """, [f"%{query}%", f"%{query}%", f"%{query}%"])

            columns = [desc[0] for desc in result.description]
            return [dict(zip(columns, row)) for row in result.fetchall()]
        except Exception as e:
            logger.error(f"Error searching files: {e}")
            return []

    def index_document(self, doc_id: str, document: Dict[str, Any]) -> bool:
        """
        Index a document (delegated to SQLite).

        Args:
            doc_id: Document identifier
            document: Document data

        Returns:
            True if successful
        """
        # DuckDB is not for indexing, delegate to SQLite
        logger.warning("Document indexing should be done via SQLite backend")
        return False

    def index_file(self, file_path: str, content: str) -> None:
        """
        Index a file (delegated to SQLite).

        Args:
            file_path: File path
            content: File content
        """
        # DuckDB is not for indexing, delegate to SQLite
        logger.warning("File indexing should be done via SQLite backend")

    def delete_indexed_file(self, file_path: str) -> None:
        """
        Delete indexed file (delegated to SQLite).

        Args:
            file_path: File path to delete
        """
        # DuckDB is not for indexing, delegate to SQLite
        logger.warning("Index deletion should be done via SQLite backend")

    def close(self) -> None:
        """Close the search backend."""
        self.analytics.close()

    def clear(self) -> bool:
        """Clear operation (delegated to SQLite)."""
        logger.warning("Clear operation should be done via SQLite backend")
        return True


class DuckDBDAL:
    """
    DuckDB Data Access Layer for analytical operations.

    This DAL focuses on OLAP workloads and complements the SQLite DAL
    which handles OLTP operations.
    """

    def __init__(self, db_path: str, sqlite_db_path: Optional[str] = None):
        """
        Initialize DuckDB DAL.

                Initialize DuckDB analytics backend.

        Creates or connects to a DuckDB database for OLAP operations and optionally
        attaches a SQLite database for cross-database analytical queries.

        Args:
            db_path: Path to DuckDB database file
            sqlite_db_path: Optional path to SQLite database for metadata
        """
        self.analytics = DuckDBAnalytics(db_path, sqlite_db_path)
        self.search = DuckDBSearch(db_path, sqlite_db_path)
        logger.info(f"DuckDBDAL initialized with db_path={db_path}")

    @property
    def storage(self) -> Optional[StorageInterface]:
        """DuckDB does not provide general key-value storage."""
        return None

    @property
    def metadata(self) -> Optional[FileMetadataInterface]:
        """DuckDB metadata access is read-only via analytics."""
        return None

    @property
    def search(self) -> Optional[SearchInterface]:
        """Return the DuckDB search interface."""
        return self._search

    @search.setter
    def search(self, value):
        """Set the search interface."""
        self._search = value

    def close(self) -> None:
        """Close the DuckDB DAL."""
        self.analytics.close()
        if hasattr(self, '_search'):
            self._search.close()

    def clear_all(self) -> bool:
        """Clear all data (not applicable for DuckDB)."""
        logger.warning("DuckDB is read-only for metadata, use SQLite for clear operations")
        return True
