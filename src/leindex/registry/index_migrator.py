"""
Index migration utilities for the meta-registry feature.

This module provides migration capabilities from legacy pickle format to
MessagePack format for code indexes.
"""

import os
import pickle
import shutil
from pathlib import Path
from typing import Any, Optional, Dict, List, Tuple
from dataclasses import dataclass
from enum import Enum
import logging
from datetime import datetime

from .msgpack_serializer import MessagePackSerializer, FormatType, PICKLE_EXT, MSGPACK_EXT
from .directories import get_project_registry_dir

logger = logging.getLogger(__name__)


# ============================================================================
# Migration Status Enumeration
# ============================================================================

class MigrationStatus(Enum):
    """
    Enumeration of migration states.

    Attributes:
        NOT_STARTED: Migration has not been initiated
        IN_PROGRESS: Migration is currently running
        COMPLETED: Migration completed successfully
        FAILED: Migration failed
        ROLLED_BACK: Migration was rolled back
    """
    NOT_STARTED = "not_started"
    IN_PROGRESS = "in_progress"
    COMPLETED = "completed"
    FAILED = "failed"
    ROLLED_BACK = "rolled_back"


# ============================================================================
# Migration Result Data Class
# ============================================================================

@dataclass
class MigrationResult:
    """
    Result of a migration operation.

    Attributes:
        success: Whether the migration was successful
        source_path: Path to the source file
        target_path: Path to the target file
        backup_path: Path to the backup file (if created)
        source_format: Detected format of the source file
        file_count: Number of files in the index
        source_checksum: SHA-256 checksum of source file
        target_checksum: SHA-256 checksum of target file
        checksums_match: Whether the checksums match
        error_message: Error message if migration failed
        duration_seconds: Time taken to migrate in seconds
        timestamp: When the migration occurred
    """
    success: bool
    source_path: Path
    target_path: Path
    backup_path: Optional[Path]
    source_format: FormatType
    file_count: int
    source_checksum: str
    target_checksum: str
    checksums_match: bool
    error_message: Optional[str]
    duration_seconds: float
    timestamp: datetime


# ============================================================================
# Index Migrator
# ============================================================================

class IndexMigrator:
    """
    Migrator for code indexes from pickle to MessagePack format.

    This class provides:
    - Detection of legacy pickle indexes
    - Validation of pickle files
    - Conversion to MessagePack format
    - Backup creation (preserves original pickle)
    - Migration verification (compares counts, checksums)
    - Rollback capability
    - Batch migration for multiple indexes

    The migrator ensures data integrity throughout the migration process
    by computing checksums before and after migration, and providing
    rollback capability if verification fails.

    Attributes:
        serializer: MessagePackSerializer instance for reading/writing
        create_backups: Whether to create backups of original files
        verify_after_migration: Whether to verify migration success
    """

    def __init__(
        self,
        create_backups: bool = True,
        verify_after_migration: bool = True
    ):
        """
        Initialize the IndexMigrator.

        Args:
            create_backups: Whether to create backups of original pickle files
            verify_after_migration: Whether to verify migration by comparing checksums
        """
        self.serializer = MessagePackSerializer()
        self.create_backups = create_backups
        self.verify_after_migration = verify_after_migration
        logger.info(
            f"IndexMigrator initialized (create_backups={create_backups}, "
            f"verify_after_migration={verify_after_migration})"
        )

    # ------------------------------------------------------------------------
    # Legacy Detection
    # ------------------------------------------------------------------------

    def detect_legacy_indexes(
        self,
        project_path: Optional[str | Path] = None,
        scan_global: bool = True
    ) -> List[Path]:
        """
        Detect legacy pickle indexes in the codebase.

        Scans for .pickle files in:
        - Global .leindex_data/ directory (if scan_global=True)
        - Project-specific .leindex/ directories (if project_path provided)

        Args:
            project_path: Optional project path to scan for pickle indexes
            scan_global: Whether to scan the global data directory

        Returns:
            List of paths to detected pickle files

        Examples:
            >>> migrator = IndexMigrator()
            >>> pickle_files = migrator.detect_legacy_indexes(
            ...     project_path="/home/user/myproject",
            ...     scan_global=True
            ... )
            >>> len(pickle_files) > 0
            True
        """
        detected = []

        # Scan global directory
        if scan_global:
            global_dir = Path.home() / ".leindex_data"
            if global_dir.exists():
                detected.extend(self._scan_directory_for_pickles(global_dir))
                logger.info(f"Scanned global directory: {global_dir}")

        # Scan project directory
        if project_path:
            project_path = Path(project_path)
            try:
                project_dir = get_project_registry_dir(project_path)
                if project_dir.exists():
                    detected.extend(self._scan_directory_for_pickles(project_dir))
                    logger.info(f"Scanned project directory: {project_dir}")
            except ValueError as e:
                logger.warning(f"Invalid project path {project_path}: {e}")

        logger.info(f"Detected {len(detected)} legacy pickle indexes")
        return detected

    def _scan_directory_for_pickles(self, directory: Path) -> List[Path]:
        """
        Recursively scan a directory for .pickle files.

        Args:
            directory: Directory to scan

        Returns:
            List of paths to .pickle files
        """
        pickle_files = []
        try:
            for file_path in directory.rglob(f"*{PICKLE_EXT}"):
                if file_path.is_file():
                    pickle_files.append(file_path)
                    logger.debug(f"Found pickle file: {file_path}")
        except (OSError, PermissionError) as e:
            logger.warning(f"Error scanning directory {directory}: {e}")

        return pickle_files

    # ------------------------------------------------------------------------
    # Validation
    # ------------------------------------------------------------------------

    def validate_pickle_file(self, file_path: str | Path) -> Tuple[bool, Optional[str]]:
        """
        Validate a pickle file for migration.

        Checks:
        - File exists and is readable
        - File contains valid pickle data
        - Data structure is compatible with MessagePack

        Args:
            file_path: Path to the pickle file

        Returns:
            Tuple of (is_valid, error_message)

        Examples:
            >>> migrator = IndexMigrator()
            >>> is_valid, error = migrator.validate_pickle_file("index.pickle")
            >>> is_valid
            True
        """
        file_path = Path(file_path)

        # Check file exists
        if not file_path.exists():
            return False, f"File does not exist: {file_path}"

        # Check file is readable
        if not os.access(file_path, os.R_OK):
            return False, f"File is not readable: {file_path}"

        # Try to load pickle data
        try:
            with open(file_path, "rb") as f:
                data = pickle.load(f)

            # Check data is dict-like (expected structure)
            if not isinstance(data, dict):
                return False, f"Data is not dict-like: {type(data)}"

            logger.debug(f"Validated pickle file: {file_path}")
            return True, None

        except (pickle.PickleError, EOFError) as e:
            error_msg = f"Invalid pickle file: {e}"
            logger.error(f"Validation failed for {file_path}: {error_msg}")
            return False, error_msg
        except Exception as e:
            error_msg = f"Unexpected error: {e}"
            logger.error(f"Validation failed for {file_path}: {error_msg}")
            return False, error_msg

    # ------------------------------------------------------------------------
    # Migration
    # ------------------------------------------------------------------------

    def migrate_index(
        self,
        source_path: str | Path,
        target_path: Optional[str | Path] = None,
        create_backup: Optional[bool] = None
    ) -> MigrationResult:
        """
        Migrate a single index from pickle to MessagePack format.

        Process:
        1. Validate source file
        2. Compute source checksum
        3. Create backup (if enabled)
        4. Read source data
        5. Write to target in MessagePack format
        6. Compute target checksum
        7. Verify checksums match (if enabled)
        8. Return result with details

        Args:
            source_path: Path to the source pickle file
            target_path: Path to the target MessagePack file (auto-generated if None)
            create_backup: Whether to create backup (uses instance default if None)

        Returns:
            MigrationResult with details of the migration

        Raises:
            FileNotFoundError: If source file doesn't exist
            ValueError: If source file is invalid

        Examples:
            >>> migrator = IndexMigrator()
            >>> result = migrator.migrate_index("index.pickle")
            >>> result.success
            True
        """
        start_time = datetime.now()
        source_path = Path(source_path)

        # Auto-generate target path if not provided
        if target_path is None:
            target_path = source_path.with_suffix(MSGPACK_EXT)
        else:
            target_path = Path(target_path)

        # Determine backup setting
        if create_backup is None:
            create_backup = self.create_backups

        # Initialize result fields
        backup_path = None
        error_message = None
        success = False
        source_format = FormatType.UNKNOWN

        try:
            # Step 1: Validate source
            logger.info(f"Starting migration: {source_path} -> {target_path}")
            is_valid, validation_error = self.validate_pickle_file(source_path)
            if not is_valid:
                raise ValueError(validation_error)

            # Step 2: Detect source format
            source_format = self.serializer.detect_format(source_path)
            if source_format != FormatType.PICKLE:
                logger.warning(f"Source is not pickle format: {source_format}")

            # Step 3: Compute source checksum
            source_checksum = self.serializer.compute_hash(source_path)
            logger.debug(f"Source checksum: {source_checksum}")

            # Step 4: Create backup
            if create_backup:
                backup_path = self._create_backup(source_path)
                logger.info(f"Created backup: {backup_path}")

            # Step 5: Read source data
            logger.debug(f"Reading source file: {source_path}")
            data = self.serializer.read(source_path)

            # Count files in index
            file_count = self._count_files_in_index(data)
            logger.debug(f"Index contains {file_count} files")

            # Step 6: Write to target
            logger.debug(f"Writing target file: {target_path}")
            self.serializer.write(target_path, data)

            # Step 7: Compute target checksum
            target_checksum = self.serializer.compute_hash(target_path)
            logger.debug(f"Target checksum: {target_checksum}")

            # Step 8: Verify (if enabled)
            checksums_match = True
            if self.verify_after_migration:
                # For pickle->msgpack, data is the same but serialization differs
                # We verify by reading both and comparing the data structures
                source_data = self.serializer.read(source_path)
                target_data = self.serializer.read(target_path)
                checksums_match = self._compare_data_structures(
                    source_data, target_data
                )

                if not checksums_match:
                    raise ValueError("Data verification failed: structures don't match")

            success = True
            logger.info(f"Migration completed successfully: {target_path}")

        except Exception as e:
            error_message = str(e)
            logger.error(f"Migration failed: {error_message}")
            success = False

            # Rollback on failure if backup was created
            if backup_path and backup_path.exists():
                logger.info(f"Rolling back migration using backup: {backup_path}")
                self._rollback_migration(source_path, backup_path)

        # Compute duration
        duration_seconds = (datetime.now() - start_time).total_seconds()

        return MigrationResult(
            success=success,
            source_path=source_path,
            target_path=target_path,
            backup_path=backup_path,
            source_format=source_format,
            file_count=file_count if success else 0,
            source_checksum=source_checksum if success else "",
            target_checksum=target_checksum if success else "",
            checksums_match=checksums_match if success else False,
            error_message=error_message,
            duration_seconds=duration_seconds,
            timestamp=start_time
        )

    def migrate_all(
        self,
        project_path: Optional[str | Path] = None,
        scan_global: bool = True
    ) -> List[MigrationResult]:
        """
        Migrate all detected legacy indexes.

        Args:
            project_path: Optional project path to scan
            scan_global: Whether to scan global directory

        Returns:
            List of MigrationResult objects

        Examples:
            >>> migrator = IndexMigrator()
            >>> results = migrator.migrate_all(
            ...     project_path="/home/user/myproject"
            ... )
            >>> all(r.success for r in results)
            True
        """
        # Detect legacy indexes
        pickle_files = self.detect_legacy_indexes(project_path, scan_global)

        if not pickle_files:
            logger.info("No legacy indexes detected for migration")
            return []

        # Migrate each file
        results = []
        logger.info(f"Migrating {len(pickle_files)} legacy indexes")

        for pickle_file in pickle_files:
            result = self.migrate_index(pickle_file)
            results.append(result)

        # Log summary
        successful = sum(1 for r in results if r.success)
        failed = len(results) - successful
        logger.info(
            f"Migration complete: {successful} succeeded, {failed} failed"
        )

        return results

    # ------------------------------------------------------------------------
    # Backup and Rollback
    # ------------------------------------------------------------------------

    def _create_backup(self, file_path: Path) -> Path:
        """
        Create a backup of a file.

        Args:
            file_path: Path to the file to backup

        Returns:
            Path to the backup file
        """
        timestamp = datetime.now().strftime("%Y%m%d_%H%M%S")
        # Add timestamp before the extension
        backup_path = file_path.with_stem(
            f"{file_path.stem}.backup_{timestamp}"
        )

        shutil.copy2(file_path, backup_path)
        logger.debug(f"Created backup: {backup_path}")
        return backup_path

    def rollback_migration(
        self,
        result: MigrationResult,
        remove_target: bool = True
    ) -> bool:
        """
        Rollback a migration using the backup file.

        Args:
            result: MigrationResult from the migration to rollback
            remove_target: Whether to remove the migrated target file

        Returns:
            True if rollback succeeded, False otherwise

        Examples:
            >>> migrator = IndexMigrator()
            >>> result = migrator.migrate_index("index.pickle")
            >>> migrator.rollback_migration(result)
            True
        """
        if not result.backup_path or not result.backup_path.exists():
            logger.error("No backup file available for rollback")
            return False

        try:
            # Restore from backup
            shutil.copy2(result.backup_path, result.source_path)
            logger.info(f"Restored {result.source_path} from backup")

            # Remove target if requested
            if remove_target and result.target_path.exists():
                result.target_path.unlink()
                logger.info(f"Removed migrated file: {result.target_path}")

            return True

        except Exception as e:
            logger.error(f"Rollback failed: {e}")
            return False

    def _rollback_migration(self, source_path: Path, backup_path: Path) -> bool:
        """
        Internal rollback implementation.

        Args:
            source_path: Original source path
            backup_path: Backup file path

        Returns:
            True if rollback succeeded
        """
        try:
            if backup_path.exists():
                shutil.copy2(backup_path, source_path)
                logger.info(f"Rollback completed: {source_path}")
                return True
        except Exception as e:
            logger.error(f"Rollback failed: {e}")

        return False

    # ------------------------------------------------------------------------
    # Verification
    # ------------------------------------------------------------------------

    def verify_migration(self, result: MigrationResult) -> bool:
        """
        Verify that a migration was successful.

        Checks:
        - Target file exists
        - Checksums match (if verification was enabled during migration)
        - File counts match

        Args:
            result: MigrationResult to verify

        Returns:
            True if verification passes, False otherwise

        Examples:
            >>> migrator = IndexMigrator()
            >>> result = migrator.migrate_index("index.pickle")
            >>> migrator.verify_migration(result)
            True
        """
        if not result.success:
            logger.warning("Cannot verify failed migration")
            return False

        # Check target exists
        if not result.target_path.exists():
            logger.error(f"Target file does not exist: {result.target_path}")
            return False

        # Check checksums match
        if not result.checksums_match:
            logger.error("Checksums do not match")
            return False

        # Verify file count
        if result.file_count == 0:
            logger.error("File count is zero")
            return False

        logger.info(f"Migration verification passed: {result.target_path}")
        return True

    def _count_files_in_index(self, data: Any) -> int:
        """
        Count the number of files in an index data structure.

        Args:
            data: Index data structure

        Returns:
            Number of files
        """
        if isinstance(data, dict):
            # Check for common keys
            for key in ["files", "file_list", "file_infos", "indexed_files"]:
                if key in data:
                    value = data[key]
                    if isinstance(value, (list, dict)):
                        return len(value)

            # Fallback: count top-level keys
            return len(data)

        return 0

    def _compare_data_structures(
        self,
        data1: Any,
        data2: Any
    ) -> bool:
        """
        Compare two data structures for equality.

        Args:
            data1: First data structure
            data2: Second data structure

        Returns:
            True if structures are equal
        """
        if type(data1) != type(data2):
            return False

        if isinstance(data1, dict):
            if set(data1.keys()) != set(data2.keys()):
                return False
            return all(
                self._compare_data_structures(data1[k], data2[k])
                for k in data1.keys()
            )

        elif isinstance(data1, (list, tuple)):
            if len(data1) != len(data2):
                return False
            return all(
                self._compare_data_structures(v1, v2)
                for v1, v2 in zip(data1, data2)
            )

        else:
            return data1 == data2

    # ------------------------------------------------------------------------
    # Status Tracking
    # ------------------------------------------------------------------------

    def get_migration_status(
        self,
        project_path: str | Path
    ) -> Dict[str, Any]:
        """
        Get the migration status for a project.

        Checks:
        - Whether legacy pickle indexes exist
        - Whether MessagePack indexes exist
        - Migration state

        Args:
            project_path: Path to the project

        Returns:
            Dictionary with migration status information

        Examples:
            >>> migrator = IndexMigrator()
            >>> status = migrator.get_migration_status("/home/user/myproject")
            >>> status["has_legacy"]
            True
        """
        project_path = Path(project_path)
        status = {
            "project_path": str(project_path),
            "has_legacy": False,
            "has_msgpack": False,
            "legacy_files": [],
            "msgpack_files": [],
            "migration_needed": False,
            "migration_complete": False
        }

        try:
            project_dir = get_project_registry_dir(project_path)

            if not project_dir.exists():
                status["migration_complete"] = True  # Nothing to migrate
                return status

            # Check for pickle files
            pickle_files = list(project_dir.rglob(f"*{PICKLE_EXT}"))
            status["has_legacy"] = len(pickle_files) > 0
            status["legacy_files"] = [str(p) for p in pickle_files]

            # Check for MessagePack files
            msgpack_files = list(project_dir.rglob(f"*{MSGPACK_EXT}"))
            status["has_msgpack"] = len(msgpack_files) > 0
            status["msgpack_files"] = [str(p) for p in msgpack_files]

            # Determine migration state
            status["migration_needed"] = status["has_legacy"]
            status["migration_complete"] = (
                status["has_msgpack"] and not status["has_legacy"]
            )

        except ValueError as e:
            logger.warning(f"Invalid project path {project_path}: {e}")

        return status
