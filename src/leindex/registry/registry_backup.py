"""
Registry Backup for the meta-registry system.

This module provides backup and restore functionality for the project registry,
ensuring data safety during cleanup operations and providing rollback capability.

Phase 6 Enhancements:
- Filesystem scan recovery for corrupted registry
- Automatic backup rotation (keep 7 days)
- Integration with registry_metadata for tracking last_backup_time
- Startup recovery logic
"""

import shutil
import sqlite3
from pathlib import Path
from typing import Optional, Dict, Any, List, Tuple
from dataclasses import dataclass
from datetime import datetime, timedelta
import logging
import msgpack

from .directories import get_global_registry_dir
from .project_registry import ProjectRegistry

logger = logging.getLogger(__name__)


# ============================================================================
# Data Classes
# ============================================================================

@dataclass
class BackupMetadata:
    """
    Metadata about a registry backup.

    Attributes:
        backup_path: Path to the backup file
        original_path: Path to the original registry database
        timestamp: When the backup was created
        project_count: Number of projects in the registry at backup time
        backup_size_bytes: Size of the backup file in bytes
        checksum: SHA-256 checksum of the backup file
    """
    backup_path: Path
    original_path: Path
    timestamp: datetime
    project_count: int
    backup_size_bytes: int
    checksum: str

    def to_dict(self) -> Dict[str, Any]:
        """
        Convert to dictionary representation.

        Returns:
            Dictionary with all fields
        """
        return {
            "backup_path": str(self.backup_path),
            "original_path": str(self.original_path),
            "timestamp": self.timestamp.isoformat(),
            "project_count": self.project_count,
            "backup_size_bytes": self.backup_size_bytes,
            "checksum": self.checksum,
        }


# ============================================================================
# Registry Backup Manager
# ============================================================================

class RegistryBackupManager:
    """
    Manager for creating and restoring registry backups.

    This class provides:
    - Backup creation with timestamp
    - Backup restoration
    - Backup verification
    - Automatic cleanup of old backups (keep 7 days)
    - Checksum computation for integrity verification
    - Filesystem scan recovery for corrupted registry
    - Startup recovery logic

    Attributes:
        backup_dir: Directory where backups are stored
        max_backups: Maximum number of backups to keep (default: 7)
        backup_interval_hours: Hours between automatic backups (default: 24)
    """

    # Metadata key for tracking last backup time
    METADATA_LAST_BACKUP = "last_backup_time"
    METADATA_LAST_BACKUP_CHECK = "last_backup_check"

    def __init__(
        self,
        backup_dir: Optional[str | Path] = None,
        max_backups: int = 7,
        backup_interval_hours: int = 24
    ):
        """
        Initialize the backup manager.

        Args:
            backup_dir: Directory for storing backups. If None, uses default.
            max_backups: Maximum number of backups to retain (default: 7 days)
            backup_interval_hours: Hours between automatic backups (default: 24)
        """
        if backup_dir is None:
            registry_dir = get_global_registry_dir()
            self.backup_dir = registry_dir / "backups"
        else:
            self.backup_dir = Path(backup_dir)

        self.max_backups = max_backups
        self.backup_interval_hours = backup_interval_hours

        # Ensure backup directory exists
        self.backup_dir.mkdir(parents=True, exist_ok=True)

        logger.info(
            f"RegistryBackupManager initialized (backup_dir={self.backup_dir}, "
            f"max_backups={max_backups}, backup_interval_hours={backup_interval_hours})"
        )

    def create_backup(
        self,
        registry: Optional[ProjectRegistry] = None,
        registry_path: Optional[str | Path] = None
    ) -> BackupMetadata:
        """
        Create a backup of the registry database.

        Args:
            registry: ProjectRegistry instance. If None, registry_path must be provided.
            registry_path: Path to registry database. If None, uses default.

        Returns:
            BackupMetadata with backup details

        Raises:
            FileNotFoundError: If registry database doesn't exist
            IOError: If backup cannot be created
        """
        # Determine registry path
        if registry is not None:
            registry_path = registry.db_path
        elif registry_path is None:
            raise ValueError("Either registry or registry_path must be provided")
        else:
            registry_path = Path(registry_path)

        registry_path = Path(registry_path)

        # Check registry exists
        if not registry_path.exists():
            raise FileNotFoundError(f"Registry database not found: {registry_path}")

        # Generate backup filename with timestamp
        timestamp_str = datetime.now().strftime("%Y%m%d_%H%M%S")
        backup_filename = f"registry_backup_{timestamp_str}.db"
        backup_path = self.backup_dir / backup_filename

        logger.info(f"Creating registry backup: {backup_path}")

        try:
            # Copy the database file
            shutil.copy2(registry_path, backup_path)

            # Get project count
            if registry is not None:
                project_count = registry.count()
            else:
                # Open temporary connection to get count
                conn = sqlite3.connect(registry_path)
                cursor = conn.execute("SELECT COUNT(*) FROM projects")
                project_count = cursor.fetchone()[0]
                conn.close()

            # Get backup size
            backup_size = backup_path.stat().st_size

            # Compute checksum
            checksum = self._compute_checksum(backup_path)

            metadata = BackupMetadata(
                backup_path=backup_path,
                original_path=registry_path,
                timestamp=datetime.now(),
                project_count=project_count,
                backup_size_bytes=backup_size,
                checksum=checksum
            )

            logger.info(
                f"Backup created successfully: {backup_path} "
                f"({project_count} projects, {backup_size} bytes)"
            )

            # Update last backup time in registry metadata
            if registry is not None:
                self._update_last_backup_time(registry)

            # Clean up old backups (rotation)
            self._cleanup_old_backups()

            return metadata

        except Exception as e:
            logger.error(f"Failed to create backup: {e}")
            # Clean up partial backup if it exists
            if backup_path.exists():
                backup_path.unlink()
            raise

    def restore_backup(
        self,
        backup_path: str | Path,
        registry_path: Optional[str | Path] = None,
        verify_before_restore: bool = True
    ) -> bool:
        """
        Restore a registry from backup.

        Args:
            backup_path: Path to the backup file
            registry_path: Path where to restore. If None, uses original path.
            verify_before_restore: Whether to verify backup before restoring

        Returns:
            True if restoration succeeded

        Raises:
            FileNotFoundError: If backup doesn't exist
            ValueError: If backup verification fails
        """
        backup_path = Path(backup_path)

        if not backup_path.exists():
            raise FileNotFoundError(f"Backup file not found: {backup_path}")

        logger.info(f"Restoring registry from backup: {backup_path}")

        # Verify backup if requested
        if verify_before_restore:
            if not self.verify_backup(backup_path):
                raise ValueError(f"Backup verification failed: {backup_path}")

        # Determine registry path
        if registry_path is None:
            # Try to determine from backup metadata
            registry_path = self._get_original_path_from_backup(backup_path)
            if registry_path is None:
                raise ValueError(
                    "Cannot determine registry path. "
                    "Please specify registry_path parameter."
                )

        registry_path = Path(registry_path)

        try:
            # Create backup of current registry before restoring
            if registry_path.exists():
                safety_backup = self._create_safety_backup(registry_path)
                logger.info(f"Created safety backup: {safety_backup}")

            try:
                # Copy backup to registry location
                shutil.copy2(backup_path, registry_path)

                logger.info(f"Registry restored successfully: {registry_path}")
                return True

            except Exception as e:
                # Restore from safety backup if restoration failed
                logger.error(f"Restoration failed: {e}. Attempting rollback...")
                if safety_backup and safety_backup.exists():
                    shutil.copy2(safety_backup, registry_path)
                    logger.info("Rolled back to pre-restoration state")
                raise

        except Exception as e:
            logger.error(f"Failed to restore backup: {e}")
            raise

    def verify_backup(self, backup_path: str | Path) -> bool:
        """
        Verify a backup file for integrity.

        Args:
            backup_path: Path to the backup file

        Returns:
            True if backup is valid

        Raises:
            FileNotFoundError: If backup doesn't exist
        """
        backup_path = Path(backup_path)

        if not backup_path.exists():
            raise FileNotFoundError(f"Backup file not found: {backup_path}")

        logger.info(f"Verifying backup: {backup_path}")

        try:
            # Check file is readable
            if not backup_path.is_file():
                logger.error(f"Backup is not a file: {backup_path}")
                return False

            # Check file size
            if backup_path.stat().st_size == 0:
                logger.error(f"Backup file is empty: {backup_path}")
                return False

            # Try to open as SQLite database
            conn = sqlite3.connect(backup_path)
            cursor = conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
            tables = [row[0] for row in cursor.fetchall()]
            conn.close()

            # Check for required tables
            required_tables = {"projects", "registry_metadata"}
            if not required_tables.issubset(set(tables)):
                logger.error(f"Backup missing required tables: {backup_path}")
                return False

            logger.info(f"Backup verified successfully: {backup_path}")
            return True

        except sqlite3.Error as e:
            logger.error(f"Backup verification failed (SQLite error): {e}")
            return False
        except Exception as e:
            logger.error(f"Backup verification failed: {e}")
            return False

    def list_backups(self) -> list[BackupMetadata]:
        """
        List all available backups.

        Returns:
            List of BackupMetadata objects, sorted by timestamp (newest first)
        """
        backups = []

        for backup_file in self.backup_dir.glob("registry_backup_*.db"):
            try:
                # Extract timestamp from filename
                stem = backup_file.stem  # e.g., "registry_backup_20250101_120000"
                timestamp_str = stem.replace("registry_backup_", "")
                timestamp = datetime.strptime(timestamp_str, "%Y%m%d_%H%M%S")

                # Get file size
                backup_size = backup_file.stat().st_size

                # Compute checksum
                checksum = self._compute_checksum(backup_file)

                # Get project count (from database)
                conn = sqlite3.connect(backup_file)
                cursor = conn.execute("SELECT COUNT(*) FROM projects")
                project_count = cursor.fetchone()[0]
                conn.close()

                metadata = BackupMetadata(
                    backup_path=backup_file,
                    original_path=Path(),  # Unknown
                    timestamp=timestamp,
                    project_count=project_count,
                    backup_size_bytes=backup_size,
                    checksum=checksum
                )
                backups.append(metadata)

            except Exception as e:
                logger.warning(f"Error reading backup {backup_file}: {e}")
                continue

        # Sort by timestamp, newest first
        backups.sort(key=lambda b: b.timestamp, reverse=True)

        logger.info(f"Found {len(backups)} backups")
        return backups

    def delete_backup(self, backup_path: str | Path) -> bool:
        """
        Delete a backup file.

        Args:
            backup_path: Path to the backup to delete

        Returns:
            True if deleted successfully
        """
        backup_path = Path(backup_path)

        if not backup_path.exists():
            logger.warning(f"Backup file not found: {backup_path}")
            return False

        try:
            backup_path.unlink()
            logger.info(f"Deleted backup: {backup_path}")
            return True
        except Exception as e:
            logger.error(f"Failed to delete backup {backup_path}: {e}")
            return False

    # ------------------------------------------------------------------------
    # Private Methods
    # ------------------------------------------------------------------------

    def _compute_checksum(self, file_path: Path) -> str:
        """
        Compute SHA-256 checksum of a file.

        Args:
            file_path: Path to the file

        Returns:
            Hexadecimal checksum string
        """
        import hashlib

        sha256 = hashlib.sha256()
        with open(file_path, "rb") as f:
            for chunk in iter(lambda: f.read(8192), b""):
                sha256.update(chunk)

        return sha256.hexdigest()

    def _cleanup_old_backups(self) -> None:
        """
        Remove old backups, keeping only the most recent max_backups.
        """
        backups = self.list_backups()

        if len(backups) <= self.max_backups:
            return

        # Remove oldest backups
        to_remove = backups[self.max_backups:]
        logger.info(f"Cleaning up {len(to_remove)} old backups")

        for backup in to_remove:
            try:
                self.delete_backup(backup.backup_path)
            except Exception as e:
                logger.warning(f"Failed to delete old backup {backup.backup_path}: {e}")

    def _create_safety_backup(self, registry_path: Path) -> Optional[Path]:
        """
        Create a safety backup before operations.

        Args:
            registry_path: Path to the registry file

        Returns:
            Path to safety backup, or None if creation failed
        """
        timestamp_str = datetime.now().strftime("%Y%m%d_%H%M%S_%f")
        safety_backup = self.backup_dir / f"safety_backup_{timestamp_str}.db"

        try:
            shutil.copy2(registry_path, safety_backup)
            return safety_backup
        except Exception as e:
            logger.error(f"Failed to create safety backup: {e}")
            return None

    def _get_original_path_from_backup(self, backup_path: Path) -> Optional[Path]:
        """
        Try to determine the original registry path from a backup.

        This is a best-effort operation - it may not always be possible
        to determine the original path.

        Args:
            backup_path: Path to the backup file

        Returns:
            Original path if determinable, None otherwise
        """
        # We can't reliably determine this from just the backup file
        # Return None to indicate caller should specify the path
        return None

    # ------------------------------------------------------------------------
    # Phase 6: Backup Time Tracking and Periodic Backup
    # ------------------------------------------------------------------------

    def get_last_backup_time(self, registry: ProjectRegistry) -> Optional[datetime]:
        """
        Get the last backup time from registry metadata.

        Args:
            registry: ProjectRegistry instance

        Returns:
            Last backup datetime, or None if never backed up
        """
        backup_time_str = registry.get_metadata(self.METADATA_LAST_BACKUP)
        if backup_time_str:
            try:
                # Handle if it's a Row object (from sqlite3)
                # Check if it's a sqlite3.Row by checking for 'keys' method
                if hasattr(backup_time_str, 'keys'):
                    backup_time_str = backup_time_str[0]
                return datetime.fromisoformat(str(backup_time_str))
            except (ValueError, TypeError) as e:
                logger.warning(f"Invalid backup time format: {backup_time_str}, error: {e}")
        return None

    def should_create_backup(self, registry: ProjectRegistry) -> bool:
        """
        Check if a backup should be created based on time since last backup.

        Args:
            registry: ProjectRegistry instance

        Returns:
            True if backup should be created (more than backup_interval_hours since last)
        """
        last_backup = self.get_last_backup_time(registry)
        if last_backup is None:
            return True  # Never backed up

        time_since_backup = datetime.now() - last_backup
        return time_since_backup >= timedelta(hours=self.backup_interval_hours)

    def _update_last_backup_time(self, registry: ProjectRegistry) -> None:
        """
        Update the last backup time in registry metadata.

        Args:
            registry: ProjectRegistry instance
        """
        now = datetime.now().isoformat()
        registry.set_metadata(self.METADATA_LAST_BACKUP, now)
        logger.debug(f"Updated last backup time: {now}")

    def update_last_backup_check(self, registry: ProjectRegistry) -> None:
        """
        Update the last backup check time in registry metadata.

        Args:
            registry: ProjectRegistry instance
        """
        now = datetime.now().isoformat()
        registry.set_metadata(self.METADATA_LAST_BACKUP_CHECK, now)
        logger.debug(f"Updated last backup check time: {now}")

    # ------------------------------------------------------------------------
    # Phase 6: Startup Recovery and Corruption Handling
    # ------------------------------------------------------------------------

    def recover_registry(
        self,
        registry_path: Path,
        registry: Optional[ProjectRegistry] = None
    ) -> Tuple[bool, str]:
        """
        Attempt to recover a corrupted registry.

        Recovery strategy:
        1. Try loading the main registry.db
        2. If corrupted, try the most recent backup
        3. If backup also corrupted/missing, perform filesystem scan recovery

        Args:
            registry_path: Path to the registry database
            registry: Optional ProjectRegistry instance (for metadata updates)

        Returns:
            Tuple of (success: bool, message: str)
        """
        logger.info(f"Attempting registry recovery: {registry_path}")

        # Step 1: Check if main registry is valid
        if self._is_registry_valid(registry_path):
            logger.info("Main registry is valid, no recovery needed")
            return True, "Registry is valid"

        # Step 2: Try to restore from most recent backup
        backups = self.list_backups()
        if backups:
            most_recent = backups[0]
            logger.info(f"Attempting recovery from backup: {most_recent.backup_path}")

            if self.verify_backup(most_recent.backup_path):
                try:
                    # Verify backup is good before restoring
                    if self.restore_backup(
                        backup_path=most_recent.backup_path,
                        registry_path=registry_path,
                        verify_before_restore=True
                    ):
                        msg = f"Registry restored from backup ({most_recent.project_count} projects)"
                        logger.info(msg)
                        return True, msg
                except Exception as e:
                    logger.warning(f"Failed to restore from backup: {e}")

        # Step 3: Perform filesystem scan recovery
        logger.warning("All recovery options failed, attempting filesystem scan recovery")
        return self._filesystem_scan_recovery(registry_path, registry)

    def _is_registry_valid(self, registry_path: Path) -> bool:
        """
        Check if a registry database is valid and readable.

        Args:
            registry_path: Path to the registry database

        Returns:
            True if registry is valid
        """
        if not registry_path.exists():
            return False

        try:
            conn = sqlite3.connect(registry_path)
            cursor = conn.execute("SELECT name FROM sqlite_master WHERE type='table'")
            tables = {row[0] for row in cursor.fetchall()}
            conn.close()

            required_tables = {"projects", "registry_metadata"}
            return required_tables.issubset(tables)

        except sqlite3.Error:
            return False

    def _filesystem_scan_recovery(
        self,
        registry_path: Path,
        registry: Optional[ProjectRegistry] = None
    ) -> Tuple[bool, str]:
        """
        Recover registry by scanning for .leindex/index.msgpack files.

        This is a last-resort recovery mechanism that:
        1. Scans for all .leindex directories
        2. Extracts metadata from index.msgpack files
        3. Reconstructs the registry from discovered indexes
        4. Logs warnings about potential metadata loss

        Args:
            registry_path: Path where to create the recovered registry
            registry: Optional ProjectRegistry instance (for direct updates)

        Returns:
            Tuple of (success: bool, message: str)
        """
        logger.warning("Starting filesystem scan recovery - some metadata may be lost")

        # Discover all index.msgpack files
        discovered_projects = self._scan_for_indexes()

        if not discovered_projects:
            msg = "No index files found during filesystem scan"
            logger.error(msg)
            return False, msg

        logger.warning(f"Discovered {len(discovered_projects)} projects during filesystem scan")

        # Create/recreate registry database
        if registry_path.exists():
            registry_path.unlink()

        if registry is None:
            registry = ProjectRegistry(db_path=registry_path)
        else:
            # Reinitialize schema
            registry._ensure_db_exists()

        # Insert discovered projects
        recovered_count = 0
        for project_info in discovered_projects:
            try:
                registry.insert(
                    path=project_info["path"],
                    indexed_at=project_info["indexed_at"],
                    file_count=project_info["file_count"],
                    config=project_info["config"],
                    stats=project_info["stats"],
                    index_location=project_info["index_location"]
                )
                recovered_count += 1
            except Exception as e:
                logger.warning(f"Failed to recover project {project_info['path']}: {e}")

        msg = f"Recovered {recovered_count}/{len(discovered_projects)} projects from filesystem scan"
        logger.warning(msg)

        if recovered_count > 0:
            return True, msg
        else:
            return False, "No projects could be recovered"

    def _scan_for_indexes(self) -> List[Dict[str, Any]]:
        """
        Scan for all .leindex/index.msgpack files.

        Returns:
            List of project info dictionaries extracted from indexes
        """
        discovered = []

        # Scan common project root directories
        scan_roots = [Path("/"), Path.home()]

        for root in scan_roots:
            try:
                # Search for .leindex directories
                for indexer_dir in root.rglob(".leindex"):
                    index_file = indexer_dir / "index.msgpack"
                    if not index_file.exists():
                        continue

                    try:
                        # Extract metadata from index file
                        project_info = self._extract_index_metadata(index_file)
                        if project_info:
                            discovered.append(project_info)
                    except Exception as e:
                        logger.warning(f"Failed to read index {index_file}: {e}")
            except PermissionError:
                # Skip directories we can't access
                continue

        logger.info(f"Found {len(discovered)} index files during filesystem scan")
        return discovered

    def _extract_index_metadata(self, index_file: Path) -> Optional[Dict[str, Any]]:
        """
        Extract project metadata from an index.msgpack file.

        Args:
            index_file: Path to the index.msgpack file

        Returns:
            Dictionary with project metadata, or None if extraction failed
        """
        try:
            with open(index_file, "rb") as f:
                index_data = msgpack.unpackb(f.read(), raw=False)

            # Extract relevant metadata
            # The index structure may vary, so we extract what we can
            project_path = str(index_file.parent.parent)

            # Try to get file count
            file_count = 0
            if isinstance(index_data, dict):
                file_count = index_data.get("file_count", len(index_data.get("files", {})))

            # Try to get indexed timestamp
            indexed_at = datetime.now()
            if isinstance(index_data, dict):
                indexed_at_str = index_data.get("indexed_at")
                if indexed_at_str:
                    try:
                        indexed_at = datetime.fromisoformat(indexed_at_str)
                    except (ValueError, TypeError):
                        pass

            return {
                "path": project_path,
                "indexed_at": indexed_at,
                "file_count": file_count,
                "config": {"recovered": True},  # Mark as recovered
                "stats": {"recovered": True},
                "index_location": str(index_file.parent)
            }

        except Exception as e:
            logger.warning(f"Failed to extract metadata from {index_file}: {e}")
            return None

    # ------------------------------------------------------------------------
    # Phase 6: Non-blocking Backup
    # ------------------------------------------------------------------------

    async def create_backup_async(
        self,
        registry: Optional[ProjectRegistry] = None,
        registry_path: Optional[str | Path] = None
    ) -> BackupMetadata:
        """
        Create a backup asynchronously (non-blocking).

        This method runs the backup in a thread pool to avoid blocking
        the main event loop.

        Args:
            registry: ProjectRegistry instance
            registry_path: Path to registry database

        Returns:
            BackupMetadata with backup details
        """
        import asyncio
        loop = asyncio.get_event_loop()

        return await loop.run_in_executor(
            None,
            lambda: self.create_backup(registry=registry, registry_path=registry_path)
        )
