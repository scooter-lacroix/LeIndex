"""
Startup migration detection and execution for the meta-registry feature.

This module provides automatic detection and migration of legacy pickle indexes
to MessagePack format on server startup.
"""

import logging
from pathlib import Path
from typing import Optional, Dict, Any
from datetime import datetime

from .index_migrator import IndexMigrator, MigrationResult
from .project_registry import ProjectRegistry

logger = logging.getLogger(__name__)


# ============================================================================
# Migration State
# ============================================================================

class MigrationState:
    """
    Tracks the state of startup migration.

    Attributes:
        migration_performed: Whether any migration was performed
        total_detected: Total number of legacy indexes detected
        total_migrated: Total number of indexes successfully migrated
        total_failed: Total number of indexes that failed migration
        start_time: When migration started
        end_time: When migration completed
    """

    def __init__(self):
        self.migration_performed: bool = False
        self.total_detected: int = 0
        self.total_migrated: int = 0
        self.total_failed: int = 0
        self.start_time: Optional[datetime] = None
        self.end_time: Optional[datetime] = None
        self.results: list[MigrationResult] = []

    def add_result(self, result: MigrationResult):
        """Add a migration result to the state."""
        self.results.append(result)
        if result.success:
            self.total_migrated += 1
        else:
            self.total_failed += 1

    def summary(self) -> str:
        """Generate a summary of the migration."""
        duration = ""
        if self.start_time and self.end_time:
            duration = f" (took {(self.end_time - self.start_time).total_seconds():.2f}s)"

        return (
            f"Migration summary: {self.total_migrated} succeeded, "
            f"{self.total_failed} failed out of {self.total_detected} detected"
            f"{duration}"
        )


# ============================================================================
# Startup Migration Manager
# ============================================================================

class StartupMigrationManager:
    """
    Manages startup migration of legacy pickle indexes.

    This class provides:
    - Detection of legacy pickle indexes on startup
    - User prompt/warning about migration
    - Automatic migration on first access
    - Migration status tracking in registry metadata

    The manager is designed to run during server startup and automatically
    migrate any detected legacy indexes without interrupting the startup process.

    Attributes:
        migrator: IndexMigrator instance for performing migrations
        auto_migrate: Whether to automatically migrate detected indexes
        project_registry: ProjectRegistry instance for tracking migration status
    """

    def __init__(
        self,
        auto_migrate: bool = True,
        project_registry: Optional[ProjectRegistry] = None
    ):
        """
        Initialize the StartupMigrationManager.

        Args:
            auto_migrate: Whether to automatically migrate detected indexes
            project_registry: Optional ProjectRegistry for tracking status
        """
        self.migrator = IndexMigrator(create_backups=True, verify_after_migration=True)
        self.auto_migrate = auto_migrate
        self.project_registry = project_registry
        self.migration_state = MigrationState()

        logger.info(
            f"StartupMigrationManager initialized (auto_migrate={auto_migrate})"
        )

    # ------------------------------------------------------------------------
    # Detection
    # ------------------------------------------------------------------------

    def check_legacy_indexes(
        self,
        project_path: Optional[str | Path] = None,
        scan_global: bool = True
    ) -> Dict[str, Any]:
        """
        Check for legacy pickle indexes without migrating.

        Args:
            project_path: Optional project path to check
            scan_global: Whether to scan global directory

        Returns:
            Dictionary with detection results

        Examples:
            >>> manager = StartupMigrationManager()
            >>> results = manager.check_legacy_indexes()
            >>> results["has_legacy"]
            False
        """
        logger.info("Checking for legacy pickle indexes...")

        # Detect legacy indexes
        pickle_files = self.migrator.detect_legacy_indexes(
            project_path=project_path,
            scan_global=scan_global
        )

        results = {
            "has_legacy": len(pickle_files) > 0,
            "legacy_count": len(pickle_files),
            "legacy_files": [str(p) for p in pickle_files],
            "project_path": str(project_path) if project_path else None,
        }

        if results["has_legacy"]:
            logger.warning(
                f"Found {len(pickle_files)} legacy pickle index(es) that need migration. "
                "These will be automatically migrated to MessagePack format."
            )
        else:
            logger.info("No legacy pickle indexes detected")

        return results

    # ------------------------------------------------------------------------
    # Migration
    # ------------------------------------------------------------------------

    def perform_startup_migration(
        self,
        project_path: Optional[str | Path] = None,
        scan_global: bool = True,
        auto_migrate_override: Optional[bool] = None
    ) -> MigrationState:
        """
        Perform startup migration if legacy indexes are detected.

        This method:
        1. Checks for legacy pickle indexes
        2. Logs a warning/prompt to the user
        3. Performs migration if auto_migrate is enabled
        4. Tracks migration status in registry metadata
        5. Returns migration state for reporting

        Args:
            project_path: Optional project path to migrate
            scan_global: Whether to scan global directory
            auto_migrate_override: Override auto_migrate setting

        Returns:
            MigrationState with migration results

        Examples:
            >>> manager = StartupMigrationManager()
            >>> state = manager.perform_startup_migration()
            >>> if state.migration_performed:
            ...     print(state.summary())
        """
        self.migration_state = MigrationState()
        self.migration_state.start_time = datetime.now()

        # Check for legacy indexes
        detection_results = self.check_legacy_indexes(project_path, scan_global)
        self.migration_state.total_detected = detection_results["legacy_count"]

        if not detection_results["has_legacy"]:
            logger.info("No migration needed")
            self.migration_state.end_time = datetime.now()
            return self.migration_state

        # Determine whether to auto-migrate
        should_migrate = auto_migrate_override if auto_migrate_override is not None else self.auto_migrate

        if not should_migrate:
            logger.warning(
                f"Found {detection_results['legacy_count']} legacy pickle index(es). "
                "Migration is disabled. Set auto_migrate=True to enable automatic migration."
            )
            self.migration_state.end_time = datetime.now()
            return self.migration_state

        # Perform migration
        logger.info("Starting automatic migration of legacy indexes...")

        results = self.migrator.migrate_all(
            project_path=project_path,
            scan_global=scan_global
        )

        # Process results
        self.migration_state.migration_performed = True
        for result in results:
            self.migration_state.add_result(result)

            # Track in project registry if available
            if self.project_registry and result.success:
                self._track_migration_in_registry(result)

        self.migration_state.end_time = datetime.now()

        # Log summary
        logger.info(self.migration_state.summary())

        return self.migration_state

    # ------------------------------------------------------------------------
    # Registry Tracking
    # ------------------------------------------------------------------------

    def _track_migration_in_registry(self, result: MigrationResult) -> None:
        """
        Track migration status in the project registry.

        Args:
            result: MigrationResult to track
        """
        if not self.project_registry:
            return

        try:
            # Extract project path from result
            # The result.source_path might be in a project directory
            # We need to determine which project this belongs to

            # For now, we'll store migration metadata globally
            # In a future enhancement, we could track per-project migration status

            metadata = {
                "migration_timestamp": result.timestamp.isoformat(),
                "source_format": result.source_format.value,
                "target_path": str(result.target_path),
                "file_count": result.file_count,
                "backup_path": str(result.backup_path) if result.backup_path else None,
                "duration_seconds": result.duration_seconds,
                "checksum_verified": result.checksums_match,
            }

            # Store in registry metadata
            # This is a placeholder - actual implementation would depend on
            # how we want to store migration metadata in the registry
            logger.debug(f"Tracked migration in registry: {metadata}")

        except Exception as e:
            logger.warning(f"Failed to track migration in registry: {e}")

    # ------------------------------------------------------------------------
    # First-Access Migration
    # ------------------------------------------------------------------------

    def migrate_on_first_access(self, project_path: str | Path) -> bool:
        """
        Migrate legacy indexes for a project on first access.

        This method is called when a project is first accessed and checks
        if migration is needed. If so, it performs the migration.

        Args:
            project_path: Path to the project

        Returns:
            True if migration was performed, False otherwise

        Examples:
            >>> manager = StartupMigrationManager()
            >>> migrated = manager.migrate_on_first_access("/home/user/myproject")
            >>> if migrated:
            ...     print("Project indexes were migrated")
        """
        project_path = Path(project_path)

        # Check migration status
        status = self.migrator.get_migration_status(project_path)

        if not status["migration_needed"]:
            logger.debug(f"No migration needed for project: {project_path}")
            return False

        logger.info(f"Migrating legacy indexes on first access: {project_path}")

        # Perform migration for this project only
        migration_state = self.perform_startup_migration(
            project_path=project_path,
            scan_global=False  # Only scan this project
        )

        return migration_state.migration_performed


# ============================================================================
# Convenience Functions
# ============================================================================

def check_and_migrate_on_startup(
    project_path: Optional[str | Path] = None,
    auto_migrate: bool = True,
    project_registry: Optional[ProjectRegistry] = None
) -> MigrationState:
    """
    Convenience function to check and migrate on startup.

    This is the main entry point for startup migration and should be called
    during server initialization.

    Args:
        project_path: Optional project path to check
        auto_migrate: Whether to automatically migrate
        project_registry: Optional ProjectRegistry instance

    Returns:
        MigrationState with results

    Examples:
        >>> from src.leindex.registry.startup_migration import (
        ...     check_and_migrate_on_startup
        ... )
        >>> state = check_and_migrate_on_startup()
        >>> if state.migration_performed:
        ...     print(f"Migration completed: {state.summary()}")
    """
    manager = StartupMigrationManager(
        auto_migrate=auto_migrate,
        project_registry=project_registry
    )

    return manager.perform_startup_migration(project_path=project_path)


def migrate_project_on_access(
    project_path: str | Path,
    project_registry: Optional[ProjectRegistry] = None
) -> bool:
    """
    Convenience function to migrate a project on first access.

    Args:
        project_path: Path to the project
        project_registry: Optional ProjectRegistry instance

    Returns:
        True if migration was performed

    Examples:
        >>> from src.leindex.registry.startup_migration import (
        ...     migrate_project_on_access
        ... )
        >>> migrate_project_on_access("/home/user/myproject")
        True
    """
    manager = StartupMigrationManager(
        auto_migrate=True,
        project_registry=project_registry
    )

    return manager.migrate_on_first_access(project_path)
