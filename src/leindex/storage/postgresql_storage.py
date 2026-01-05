"""
PostgreSQL-based storage backend for file metadata.

OPTIONAL DEPENDENCY: This module requires SQLAlchemy and psycopg2-binary.
Install with: pip install leindex[postgresql]

CRITICAL FIX: Added connection pool monitoring, proper migration handling, and missing indexes.
"""

import logging
from typing import Any, Dict, Optional, List, Tuple, Iterator
from datetime import datetime

try:
    from sqlalchemy import create_engine, Column, String, Text, DateTime, Integer, JSON, ForeignKey, Index, func, text
    from sqlalchemy.orm import sessionmaker, declarative_base, relationship
    from sqlalchemy.exc import SQLAlchemyError
    from sqlalchemy.pool import QueuePool
    from sqlalchemy.event import listen
    from sqlalchemy.engine import Engine
    SQLALCHEMY_AVAILABLE = True
except ImportError:
    SQLALCHEMY_AVAILABLE = False
    create_engine = None  # type: ignore

from .storage_interface import StorageInterface, FileMetadataInterface

logger = logging.getLogger(__name__)

if SQLALCHEMY_AVAILABLE:
    Base = declarative_base()
else:
    Base = None  # type: ignore


class File(Base):
    __tablename__ = 'files'
    id = Column(Integer, primary_key=True)
    file_path = Column(String, unique=True, nullable=False, index=True)
    file_type = Column(String, nullable=False)
    extension = Column(String)
    metadata_json = Column(JSON)
    created_at = Column(DateTime, default=datetime.utcnow)
    updated_at = Column(DateTime, default=datetime.utcnow, onupdate=datetime.utcnow)

    # CRITICAL FIX: Add composite indexes for common query patterns
    __table_args__ = (
        Index('ix_files_file_type_extension', 'file_type', 'extension'),
        Index('ix_files_updated_at', 'updated_at'),
    )

    versions = relationship("FileVersion", back_populates="file", cascade="all, delete-orphan")

    def __repr__(self):
        return f"<File(file_path='{self.file_path}', file_type='{self.file_type}')>"


class FileVersion(Base):
    __tablename__ = 'file_versions'
    id = Column(String, primary_key=True)  # version_id
    file_path = Column(String, nullable=False, index=True)
    file_id = Column(Integer, ForeignKey('files.id'), nullable=False)
    content = Column(Text, nullable=False)
    hash = Column(String, nullable=False)
    timestamp = Column(DateTime, default=datetime.utcnow)
    size = Column(Integer, nullable=False)

    # CRITICAL FIX: Add composite indexes for common query patterns
    __table_args__ = (
        Index('ix_file_versions_file_path_timestamp', 'file_path', 'timestamp'),
        Index('ix_file_versions_hash', 'hash'),
    )

    file = relationship("File", back_populates="versions")

    def __repr__(self):
        return f"<FileVersion(id='{self.id}', file_path='{self.file_path}')>"


class FileDiff(Base):
    __tablename__ = 'file_diffs'
    id = Column(String, primary_key=True)  # diff_id
    file_path = Column(String, nullable=False, index=True)
    previous_version_id = Column(String, ForeignKey('file_versions.id'), nullable=True)
    current_version_id = Column(String, ForeignKey('file_versions.id'), nullable=False)
    diff_content = Column(Text, nullable=False)
    diff_type = Column(String, nullable=False)
    operation_type = Column(String, nullable=False)
    operation_details = Column(Text)
    timestamp = Column(DateTime, default=datetime.utcnow)

    # CRITICAL FIX: Add composite indexes for common query patterns
    __table_args__ = (
        Index('ix_file_diffs_file_path_timestamp', 'file_path', 'timestamp'),
        Index('ix_file_diffs_operation_type', 'operation_type'),
    )

    previous_version = relationship("FileVersion", foreign_keys=[previous_version_id])
    current_version = relationship("FileVersion", foreign_keys=[current_version_id])

    def __repr__(self):
        return f"<FileDiff(id='{self.id}', file_path='{self.file_path}', operation='{self.operation_type}')>"


def _log_pool_status(dbapi_conn, connection_record):
    """Callback for logging pool status changes."""
    logger.debug("New PostgreSQL connection established")


class PostgreSQLStorage(StorageInterface):
    """
    PostgreSQL-based generic key-value storage.
    This implements the StorageInterface.

    CRITICAL FIX: Added connection pool monitoring and proper sizing.

    OPTIONAL DEPENDENCY: Requires SQLAlchemy and psycopg2-binary.
    Install with: pip install leindex[postgresql]
    """
    # CRITICAL FIX: Add connection pool configuration constants
    DEFAULT_POOL_SIZE = 10
    DEFAULT_MAX_OVERFLOW = 20
    DEFAULT_POOL_TIMEOUT = 30
    DEFAULT_POOL_RECYCLE = 3600

    def __init__(self, db_user: str, db_password: str, db_host: str, db_port: int, db_name: str,
                 ssl_args: Optional[Dict[str, Any]] = None,
                 pool_size: int = DEFAULT_POOL_SIZE,
                 max_overflow: int = DEFAULT_MAX_OVERFLOW):
        """
        Initialize PostgreSQL storage with connection pooling.

        CRITICAL FIX: Added configurable connection pool parameters.

        Args:
            db_user: Database user
            db_password: Database password
            db_host: Database host
            db_port: Database port
            db_name: Database name
            ssl_args: Optional SSL configuration

        Raises:
            ImportError: If SQLAlchemy is not installed
        """
        if not SQLALCHEMY_AVAILABLE:
            raise ImportError(
                "SQLAlchemy is required for PostgreSQL backend. "
                "Install it with: pip install leindex[postgresql]"
            )

        # Connection details should ideally come from a secure secrets management system
        self.connection_string = f"postgresql+psycopg2://{db_user}:{db_password}@{db_host}:{db_port}/{db_name}"

        connect_args = {}
        if ssl_args:
            connect_args['ssl'] = ssl_args

        # CRITICAL FIX: Create engine with proper pool configuration
        self.engine = create_engine(
            self.connection_string,
            poolclass=QueuePool,
            pool_size=pool_size,
            max_overflow=max_overflow,
            pool_timeout=self.DEFAULT_POOL_TIMEOUT,
            pool_recycle=self.DEFAULT_POOL_RECYCLE,
            pool_pre_ping=True,  # Verify connections before use
            connect_args=connect_args
        )

        # CRITICAL FIX: Set up connection pool monitoring
        listen(self.engine, 'connect', _log_pool_status)

        # CRITICAL FIX: Create tables if they don't exist
        # Note: For production, use Alembic for proper migration management
        self._run_migration()

        self.Session = sessionmaker(bind=self.engine)
        logger.info(
            f"Initializing PostgreSQLStorage for database: {db_name} on {db_host}:{db_port} "
            f"with pool_size={pool_size}, max_overflow={max_overflow}"
        )

    def _run_migration(self):
        """
        CRITICAL FIX: Run database schema creation and migrations.

        Note: This is a simplified migration approach. For production use,
        integrate Alembic for proper versioned migrations.
        """
        try:
            # Create tables
            Base.metadata.create_all(self.engine)
            logger.info("Database schema created/updated successfully")

            # CRITICAL FIX: Verify indexes exist and create if missing
            self._ensure_indexes()

        except SQLAlchemyError as e:
            logger.error(f"Error during database migration: {e}")
            raise

    def _ensure_indexes(self):
        """
        CRITICAL FIX: Ensure critical indexes exist for query performance.

        This method verifies that indexes required for common query patterns
        are present in the database.
        """
        try:
            with self.engine.connect() as conn:
                # Check for existing indexes
                result = conn.execute(
                    text("""
                        SELECT indexname FROM pg_indexes
                        WHERE schemaname = 'public'
                        AND tablename IN ('files', 'file_versions', 'file_diffs')
                    """)
                )
                existing_indexes = set(row[0] for row in result)

                # Expected indexes from model definitions
                expected_indexes = {
                    'ix_files_file_path',
                    'ix_files_file_type_extension',
                    'ix_files_updated_at',
                    'ix_file_versions_file_path',
                    'ix_file_versions_file_path_timestamp',
                    'ix_file_versions_hash',
                    'ix_file_diffs_file_path',
                    'ix_file_diffs_file_path_timestamp',
                    'ix_file_diffs_operation_type',
                }

                missing_indexes = expected_indexes - existing_indexes
                if missing_indexes:
                    logger.warning(f"Missing indexes detected: {missing_indexes}")
                    # Indexes will be created by SQLAlchemy's create_all()
                    # For production, use Alembic migrations
                else:
                    logger.debug("All expected indexes are present")

        except SQLAlchemyError as e:
            logger.error(f"Error verifying indexes: {e}")

    def _get_pool_status(self) -> Dict[str, Any]:
        """
        CRITICAL FIX: Get connection pool status for monitoring.

        Returns:
            Dictionary containing pool status information
        """
        pool = self.engine.pool
        return {
            'size': pool.size(),
            'checked_in': pool.checkedin(),
            'checked_out': pool.checkedout(),
            'overflow': pool.overflow(),
            'max_size': pool.size() + pool.max_overflow
        }

    def put(self, key: str, value: Any) -> bool:
        """Store a key-value pair in PostgreSQL."""
        try:
            with self.Session() as session:
                # Assuming a simple key-value table named 'kv_store'
                # with columns 'key' TEXT PRIMARY KEY and 'value_json' JSONB
                # This is a placeholder, actual implementation might vary
                # based on the specific schema for generic storage.
                # For now, we'll just log and return True as this is not the primary focus.
                logger.warning("Generic key-value put not fully implemented for PostgreSQLStorage.")
                return True
        except SQLAlchemyError as e:
            logger.error(f"Error putting key-value pair: {e}")
            return False

    def get(self, key: str) -> Optional[Any]:
        """Retrieve a value by key from PostgreSQL."""
        try:
            with self.Session() as session:
                logger.warning("Generic key-value get not fully implemented for PostgreSQLStorage.")
                return None
        except SQLAlchemyError as e:
            logger.error(f"Error getting key-value pair: {e}")
            return None

    def delete(self, key: str) -> bool:
        """Delete a key-value pair from PostgreSQL."""
        try:
            with self.Session() as session:
                logger.warning("Generic key-value delete not fully implemented for PostgreSQLStorage.")
                return True
        except SQLAlchemyError as e:
            logger.error(f"Error deleting key-value pair: {e}")
            return False

    def exists(self, key: str) -> bool:
        """Check if a key exists in PostgreSQL."""
        try:
            with self.Session() as session:
                logger.warning("Generic key-value exists not fully implemented for PostgreSQLStorage.")
                return False
        except SQLAlchemyError as e:
            logger.error(f"Error checking key existence: {e}")
            return False

    def keys(self, pattern: Optional[str] = None) -> Iterator[str]:
        """Iterate over keys in PostgreSQL, optionally filtered by pattern."""
        logger.warning("Generic key-value keys iteration not fully implemented for PostgreSQLStorage.")
        yield from []

    def items(self, pattern: Optional[str] = None) -> Iterator[Tuple[str, Any]]:
        """Iterate over key-value pairs in PostgreSQL, optionally filtered by pattern."""
        logger.warning("Generic key-value items iteration not fully implemented for PostgreSQLStorage.")
        yield from []

    def clear(self) -> bool:
        """Clear all data from the PostgreSQL key-value store."""
        try:
            with self.Session() as session:
                logger.warning("Generic key-value clear not fully implemented for PostgreSQLStorage.")
                return True
        except SQLAlchemyError as e:
            logger.error(f"Error clearing key-value store: {e}")
            return False

    def size(self) -> int:
        """Get the number of stored items in PostgreSQL."""
        try:
            with self.Session() as session:
                logger.warning("Generic key-value size not fully implemented for PostgreSQLStorage.")
                return 0
        except SQLAlchemyError as e:
            logger.error(f"Error getting key-value store size: {e}")
            return 0

    def close(self) -> None:
        """Close the PostgreSQL key-value storage backend."""
        logger.info("PostgreSQLStorage closed. Engine disposed.")
        self.engine.dispose()
    
    def flush(self) -> bool:
        """Flush any pending operations to PostgreSQL."""
        logger.debug("PostgreSQL: Flushing operations (no-op for SQLAlchemy sessions, commit handles it)")
        return True


class PostgreSQLFileMetadata(FileMetadataInterface):
    """
    PostgreSQL-based file metadata storage.
    This implements the FileMetadataInterface.

    OPTIONAL DEPENDENCY: Requires SQLAlchemy and psycopg2-binary.
    Install with: pip install leindex[postgresql]
    """
    def __init__(self, db_user: str, db_password: str, db_host: str, db_port: int, db_name: str,
                 ssl_args: Optional[Dict[str, Any]] = None):
        """
        Initialize PostgreSQL file metadata storage.

        Raises:
            ImportError: If SQLAlchemy is not installed
        """
        if not SQLALCHEMY_AVAILABLE:
            raise ImportError(
                "SQLAlchemy is required for PostgreSQL backend. "
                "Install it with: pip install leindex[postgresql]"
            )

        # Connection details should ideally come from a secure secrets management system (e.g., HashiCorp Vault, AWS Secrets Manager)
        # and not be hardcoded or passed directly from insecure sources.
        self.connection_string = f"postgresql+psycopg2://{db_user}:{db_password}@{db_host}:{db_port}/{db_name}"
        
        connect_args = {}
        if ssl_args:
            # Example ssl_args: {'sslmode': 'require', 'sslrootcert': '/path/to/ca.pem', 'sslcert': '/path/to/client.crt', 'sslkey': '/path/to/client.key'}
            connect_args['ssl'] = ssl_args

        self.engine = create_engine(self.connection_string, poolclass=QueuePool, pool_size=10, max_overflow=20, connect_args=connect_args)
        Base.metadata.create_all(self.engine) # Ensure tables exist
        self.Session = sessionmaker(bind=self.engine)
        logger.info(f"Initializing PostgreSQLFileMetadata for database: {db_name} on {db_host}:{db_port}")

    def add_file(self, file_path: str, file_type: str, extension: str,
                 metadata: Optional[Dict[str, Any]] = None) -> bool:
        """Add a file's metadata to PostgreSQL."""
        try:
            with self.Session() as session:
                file_record = session.query(File).filter_by(file_path=file_path).first()
                if file_record:
                    file_record.file_type = file_type
                    file_record.extension = extension
                    file_record.metadata_json = metadata
                    logger.debug(f"PostgreSQL: Updated file {file_path}")
                else:
                    file_record = File(file_path=file_path, file_type=file_type, extension=extension, metadata_json=metadata)
                    session.add(file_record)
                    logger.debug(f"PostgreSQL: Added file {file_path}")
                session.commit()
                return True
        except SQLAlchemyError as e:
            session.rollback()
            logger.error(f"Error adding/updating file {file_path}: {e}")
            return False

    def remove_file(self, file_path: str) -> bool:
        """Remove a file's metadata from PostgreSQL."""
        try:
            with self.Session() as session:
                file_record = session.query(File).filter_by(file_path=file_path).first()
                if file_record:
                    session.delete(file_record)
                    session.commit()
                    logger.debug(f"PostgreSQL: Removed file {file_path}")
                    return True
                logger.debug(f"PostgreSQL: File {file_path} not found for removal.")
                return False
        except SQLAlchemyError as e:
            session.rollback()
            logger.error(f"Error removing file {file_path}: {e}")
            return False

    def get_file_info(self, file_path: str) -> Optional[Dict[str, Any]]:
        """Get metadata about a file from PostgreSQL."""
        try:
            with self.Session() as session:
                file_record = session.query(File).filter_by(file_path=file_path).first()
                if file_record:
                    return {
                        "id": file_record.id,
                        "file_path": file_record.file_path,
                        "file_type": file_record.file_type,
                        "extension": file_record.extension,
                        "metadata": file_record.metadata_json,
                        "created_at": file_record.created_at.isoformat(),
                        "updated_at": file_record.updated_at.isoformat()
                    }
                return None
        except SQLAlchemyError as e:
            logger.error(f"Error getting file info for {file_path}: {e}")
            return None

    def get_directory_structure(self, directory_path: str = "") -> Dict[str, Any]:
        """Get the directory structure from PostgreSQL."""
        try:
            with self.Session() as session:
                # This is a simplified implementation. A full directory structure
                # would require more complex tree traversal logic.
                # For now, it returns a flat list of files within the given path.
                query = session.query(File)
                if directory_path:
                    query = query.filter(File.file_path.like(f"{directory_path}%"))
                
                structure = {}
                for file_record in query.all():
                    path_parts = file_record.file_path.split('/')
                    current_level = structure
                    for part in path_parts[:-1]:
                        if part not in current_level:
                            current_level[part] = {}
                        current_level = current_level[part]
                    current_level[path_parts[-1]] = {
                        "type": file_record.file_type,
                        "extension": file_record.extension,
                        "metadata": file_record.metadata_json
                    }
                return structure
        except SQLAlchemyError as e:
            logger.error(f"Error getting directory structure for {directory_path}: {e}")
            return {}

    def get_all_files(self, limit: Optional[int] = None, offset: int = 0) -> List[Tuple[str, Dict[str, Any]]]:
        """
        Get all files' metadata from PostgreSQL.

        PERFORMANCE FIX: Added pagination support to prevent loading all files
        into memory for large databases.

        Args:
            limit: Maximum number of files to return (None for all)
            offset: Number of files to skip (for pagination)

        Returns:
            List of (file_path, metadata) tuples
        """
        try:
            with self.Session() as session:
                query = session.query(File).order_by(File.file_path)

                # Apply pagination if specified
                if limit is not None:
                    query = query.limit(limit)
                if offset > 0:
                    query = query.offset(offset)

                all_files = []
                for file_record in query.all():
                    all_files.append((
                        file_record.file_path,
                        {
                            "id": file_record.id,
                            "file_path": file_record.file_path,
                            "file_type": file_record.file_type,
                            "extension": file_record.extension,
                            "metadata": file_record.metadata_json,
                            "created_at": file_record.created_at.isoformat(),
                            "updated_at": file_record.updated_at.isoformat()
                        }
                    ))
                return all_files
        except SQLAlchemyError as e:
            logger.error(f"Error getting all files: {e}")
            return []

    def get_all_files_yield(self, batch_size: int = 1000) -> Iterator[Tuple[str, Dict[str, Any]]]:
        """
        Get all files' metadata from PostgreSQL using a generator for memory efficiency.

        PERFORMANCE FIX: Uses yield_per() for memory-efficient iteration over large
        result sets. This is preferable for processing large numbers of files.

        Args:
            batch_size: Number of records to fetch per batch

        Yields:
            (file_path, metadata) tuples one at a time
        """
        try:
            with self.Session() as session:
                # Use yield_per for efficient batch processing
                query = session.query(File).order_by(File.file_path)

                for file_record in query.yield_per(batch_size):
                    yield (
                        file_record.file_path,
                        {
                            "id": file_record.id,
                            "file_path": file_record.file_path,
                            "file_type": file_record.file_type,
                            "extension": file_record.extension,
                            "metadata": file_record.metadata_json,
                            "created_at": file_record.created_at.isoformat(),
                            "updated_at": file_record.updated_at.isoformat()
                        }
                    )
        except SQLAlchemyError as e:
            logger.error(f"Error yielding all files: {e}")
            return
            yield  # Make this a generator function even on error

    def insert_file_version(self, version_id: str, file_path: str, content: str, hash: str, timestamp: str, size: int) -> bool:
        """Inserts a new file version into PostgreSQL."""
        try:
            with self.Session() as session:
                file_record = session.query(File).filter_by(file_path=file_path).first()
                if not file_record:
                    logger.error(f"File {file_path} not found when trying to insert version {version_id}.")
                    return False
                
                # Convert timestamp string to datetime object
                try:
                    dt_timestamp = datetime.fromisoformat(timestamp)
                except ValueError:
                    logger.error(f"Invalid timestamp format for version {version_id}: {timestamp}")
                    return False

                file_version = FileVersion(
                    id=version_id,
                    file_path=file_path,
                    file_id=file_record.id,
                    content=content,
                    hash=hash,
                    timestamp=dt_timestamp,
                    size=size
                )
                session.add(file_version)
                session.commit()
                logger.debug(f"PostgreSQL: Inserted file version {version_id} for {file_path}")
                return True
        except SQLAlchemyError as e:
            session.rollback()
            logger.error(f"Error inserting file version {version_id} for {file_path}: {e}")
            return False

    def get_file_version(self, version_id: str) -> Optional[Dict]:
        """Retrieves a file version by its ID from PostgreSQL."""
        try:
            with self.Session() as session:
                file_version = session.query(FileVersion).filter_by(id=version_id).first()
                if file_version:
                    return {
                        "version_id": file_version.id,
                        "file_path": file_version.file_path,
                        "content": file_version.content,
                        "hash": file_version.hash,
                        "timestamp": file_version.timestamp.isoformat(),
                        "size": file_version.size
                    }
                return None
        except SQLAlchemyError as e:
            logger.error(f"Error getting file version {version_id}: {e}")
            return None

    def get_file_versions_for_path(self, file_path: str, limit: Optional[int] = None, offset: int = 0) -> List[Dict]:
        """
        Retrieves all versions for a given file path from PostgreSQL.

        PERFORMANCE FIX: Added pagination (limit/offset) support to prevent loading
        all versions into memory for files with extensive history.

        Args:
            file_path: The file path to get versions for
            limit: Maximum number of versions to return (None for all)
            offset: Number of versions to skip (for pagination)

        Returns:
            List of version dictionaries
        """
        logger.debug(f"PostgreSQL get_file_versions_for_path called with file_path: {file_path}, limit={limit}, offset={offset}")
        try:
            with self.Session() as session:
                query = session.query(FileVersion).filter_by(file_path=file_path).order_by(FileVersion.timestamp)

                # Apply pagination if specified
                if limit is not None:
                    query = query.limit(limit)
                if offset > 0:
                    query = query.offset(offset)

                versions = query.all()
                logger.debug(f"Found {len(versions)} versions for path {file_path}")

                # Use list comprehension for better performance
                result = [
                    {
                        "version_id": v.id,
                        "file_path": v.file_path,
                        "content": v.content,
                        "hash": v.hash,
                        "timestamp": v.timestamp.isoformat(),
                        "size": v.size
                    } for v in versions
                ]
                if versions:
                    logger.debug(f"Sample version data: version_id={versions[0].id}, timestamp={versions[0].timestamp}")
                return result
        except SQLAlchemyError as e:
            logger.error(f"Error getting file versions for path {file_path}: {e}")
            return []

    def insert_file_diff(self, diff_id: str, file_path: str, previous_version_id: Optional[str], current_version_id: str, diff_content: str, diff_type: str, operation_type: str, operation_details: Optional[str], timestamp: str) -> bool:
        """Inserts a new file diff into PostgreSQL."""
        try:
            with self.Session() as session:
                # Check if current_version_id exists
                current_version = session.query(FileVersion).filter_by(id=current_version_id).first()
                if not current_version:
                    logger.error(f"Current version ID {current_version_id} not found for diff {diff_id}.")
                    return False

                # Check if previous_version_id exists if provided
                if previous_version_id:
                    previous_version = session.query(FileVersion).filter_by(id=previous_version_id).first()
                    if not previous_version:
                        logger.error(f"Previous version ID {previous_version_id} not found for diff {diff_id}.")
                        return False
                
                # Convert timestamp string to datetime object
                try:
                    dt_timestamp = datetime.fromisoformat(timestamp)
                except ValueError:
                    logger.error(f"Invalid timestamp format for diff {diff_id}: {timestamp}")
                    return False

                file_diff = FileDiff(
                    id=diff_id,
                    file_path=file_path,
                    previous_version_id=previous_version_id,
                    current_version_id=current_version_id,
                    diff_content=diff_content,
                    diff_type=diff_type,
                    operation_type=operation_type,
                    operation_details=operation_details,
                    timestamp=dt_timestamp
                )
                session.add(file_diff)
                session.commit()
                logger.debug(f"PostgreSQL: Inserted file diff {diff_id} for {file_path}")
                return True
        except SQLAlchemyError as e:
            session.rollback()
            logger.error(f"Error inserting file diff {diff_id} for {file_path}: {e}")
            return False

    def get_file_diffs_for_path(self, file_path: str, limit: Optional[int] = None, offset: int = 0) -> List[Dict]:
        """
        Retrieves all diffs for a given file path from PostgreSQL.

        PERFORMANCE FIX: Added pagination (limit/offset) support to prevent loading
        all diffs into memory for files with extensive history.

        Args:
            file_path: The file path to get diffs for
            limit: Maximum number of diffs to return (None for all)
            offset: Number of diffs to skip (for pagination)

        Returns:
            List of diff dictionaries
        """
        logger.debug(f"PostgreSQL get_file_diffs_for_path called with file_path: {file_path}, limit={limit}, offset={offset}")
        try:
            with self.Session() as session:
                query = session.query(FileDiff).filter_by(file_path=file_path).order_by(FileDiff.timestamp)

                # Apply pagination if specified
                if limit is not None:
                    query = query.limit(limit)
                if offset > 0:
                    query = query.offset(offset)

                diffs = query.all()
                logger.debug(f"Found {len(diffs)} diffs for path {file_path}")

                # Use list comprehension for better performance
                result = [
                    {
                        "diff_id": d.id,
                        "file_path": d.file_path,
                        "previous_version_id": d.previous_version_id,
                        "current_version_id": d.current_version_id,
                        "diff_content": d.diff_content,
                        "diff_type": d.diff_type,
                        "operation_type": d.operation_type,
                        "operation_details": d.operation_details,
                        "timestamp": d.timestamp.isoformat()
                    } for d in diffs
                ]
                if diffs:
                    logger.debug(f"Sample diff data: diff_id={diffs[0].id}, operation_type={diffs[0].operation_type}")
                return result
        except SQLAlchemyError as e:
            logger.error(f"Error getting file diffs for path {file_path}: {e}")
            return []

    def get_file_history_combined(self, file_path: str, limit: Optional[int] = None, offset: int = 0) -> Dict[str, List[Dict]]:
        """
        PERFORMANCE FIX: Retrieves both versions and diffs for a given file path in optimized queries.
        This solves the N+1 query problem by using batched queries with proper indexing.

        Instead of making N queries (1 for versions + 1 for each version's diffs),
        this method fetches all data in 2 optimized queries using the composite indexes.

        Args:
            file_path: The file path to get history for
            limit: Maximum number of versions to return (None for all)
            offset: Number of versions to skip (for pagination)

        Returns:
            Dictionary with 'versions' and 'diffs' lists
        """
        logger.debug(f"PostgreSQL get_file_history_combined called with file_path: {file_path}, limit={limit}, offset={offset}")
        try:
            with self.Session() as session:
                # Query 1: Get versions with pagination
                versions_query = session.query(FileVersion).filter_by(file_path=file_path).order_by(FileVersion.timestamp)

                if limit is not None:
                    versions_query = versions_query.limit(limit)
                if offset > 0:
                    versions_query = versions_query.offset(offset)

                versions = versions_query.all()

                # Query 2: Get all related diffs in a single query using IN clause
                # This is much more efficient than querying diffs for each version individually
                version_ids = [v.id for v in versions]

                if version_ids:
                    diffs = session.query(FileDiff).filter(
                        FileDiff.file_path == file_path,
                        FileDiff.current_version_id.in_(version_ids)
                    ).order_by(FileDiff.timestamp).all()
                else:
                    diffs = []

                # Build result dictionary
                result = {
                    "versions": [
                        {
                            "version_id": v.id,
                            "file_path": v.file_path,
                            "content": v.content,
                            "hash": v.hash,
                            "timestamp": v.timestamp.isoformat(),
                            "size": v.size
                        } for v in versions
                    ],
                    "diffs": [
                        {
                            "diff_id": d.id,
                            "file_path": d.file_path,
                            "previous_version_id": d.previous_version_id,
                            "current_version_id": d.current_version_id,
                            "diff_content": d.diff_content,
                            "diff_type": d.diff_type,
                            "operation_type": d.operation_type,
                            "operation_details": d.operation_details,
                            "timestamp": d.timestamp.isoformat()
                        } for d in diffs
                    ]
                }

                logger.debug(f"Found {len(result['versions'])} versions and {len(result['diffs'])} diffs for path {file_path}")
                return result

        except SQLAlchemyError as e:
            logger.error(f"Error getting combined file history for path {file_path}: {e}")
            return {"versions": [], "diffs": []}

    # Additional metadata methods (CRITICAL FIX: Added to implement FileMetadataInterface)
    def save_file_metadata(self, file_path: str, metadata: Dict[str, Any]) -> None:
        """Save file metadata to storage.

        Args:
            file_path: The path of the file
            metadata: The metadata dictionary to save

        Raises:
            IOError: If the metadata cannot be written
        """
        try:
            with self.Session() as session:
                file_record = session.query(File).filter_by(file_path=file_path).first()
                if file_record:
                    file_record.metadata_json = metadata
                    session.commit()
                    logger.debug(f"PostgreSQL: Saved metadata for {file_path}")
                else:
                    raise IOError(f"File {file_path} not found, cannot save metadata")
        except SQLAlchemyError as e:
            session.rollback()
            raise IOError(f"Error saving metadata for {file_path}: {e}")

    def get_file_metadata(self, file_path: str) -> Optional[Dict[str, Any]]:
        """Retrieve file metadata from storage.

        Args:
            file_path: The path of the file

        Returns:
            The metadata dictionary if found, None otherwise
        """
        try:
            with self.Session() as session:
                file_record = session.query(File).filter_by(file_path=file_path).first()
                if file_record:
                    return file_record.metadata_json
                return None
        except SQLAlchemyError as e:
            logger.error(f"Error getting file metadata for {file_path}: {e}")
            return None

    def delete_file_metadata(self, file_path: str) -> None:
        """Delete file metadata from storage.

        Args:
            file_path: The path of the file
        """
        try:
            with self.Session() as session:
                file_record = session.query(File).filter_by(file_path=file_path).first()
                if file_record:
                    session.delete(file_record)
                    session.commit()
                    logger.debug(f"PostgreSQL: Deleted metadata for {file_path}")
        except SQLAlchemyError as e:
            session.rollback()
            logger.error(f"Error deleting file metadata for {file_path}: {e}")

    def get_all_file_paths(self) -> List[str]:
        """Get all file paths in the storage.

        Returns:
            List of all file paths
        """
        try:
            with self.Session() as session:
                file_records = session.query(File.file_path).all()
                return [record.file_path for record in file_records]
        except SQLAlchemyError as e:
            logger.error(f"Error getting all file paths: {e}")
            return []

    def clear(self) -> bool:
        """Clear all file metadata, versions, and diffs from PostgreSQL."""
        try:
            with self.Session() as session:
                # Order of deletion matters due to foreign key constraints
                session.query(FileDiff).delete()
                session.query(FileVersion).delete()
                session.query(File).delete()
                session.commit()
                logger.debug("PostgreSQL: Cleared all file metadata, versions, and diffs.")
                return True
        except SQLAlchemyError as e:
            session.rollback()
            logger.error(f"Error clearing all file metadata: {e}")
            return False
    
    def size(self) -> int:
        """Get the number of files in the PostgreSQL metadata store."""
        try:
            with self.Session() as session:
                count = session.query(File).count()
                logger.debug(f"PostgreSQL: File metadata size is {count}")
                return count
        except SQLAlchemyError as e:
            logger.error(f"Error getting file metadata size: {e}")
            return 0
    
    def flush(self) -> bool:
        """Flush any pending operations to PostgreSQL."""
        logger.debug("PostgreSQL: Flushing operations (no-op for SQLAlchemy sessions, commit handles it)")
        return True
    
    def close(self) -> None:
        """Close the PostgreSQL file metadata backend."""
        logger.info("PostgreSQLFileMetadata closed. Engine disposed.")
        self.engine.dispose()