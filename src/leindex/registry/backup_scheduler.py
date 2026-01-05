"""
Periodic Backup Scheduler for the meta-registry system.

This module provides automatic periodic backup functionality with:
- Startup backup check (backup if >24h since last backup)
- Background periodic backup task (24-hour intervals)
- Graceful shutdown handling
- Signal handlers for SIGTERM and SIGINT
"""

import asyncio
import signal
import logging
from typing import Optional, Tuple

from .project_registry import ProjectRegistry
from .registry_backup import RegistryBackupManager

logger = logging.getLogger(__name__)


class BackupScheduler:
    """
    Scheduler for automatic periodic backups of the project registry.

    This class manages:
    - Startup backup check (creates backup if needed on startup)
    - Background periodic backup task (24-hour intervals)
    - Graceful shutdown handling

    Attributes:
        backup_manager: RegistryBackupManager instance
        backup_interval_hours: Hours between automatic backups
        _backup_task: Background task for periodic backups
        _shutdown_event: Event for signaling shutdown
    """

    def __init__(
        self,
        backup_manager: Optional[RegistryBackupManager] = None,
        backup_interval_hours: int = 24
    ):
        """
        Initialize the backup scheduler.

        Args:
            backup_manager: RegistryBackupManager instance. If None, creates default.
            backup_interval_hours: Hours between automatic backups
        """
        if backup_manager is None:
            backup_manager = RegistryBackupManager()

        self.backup_manager = backup_manager
        self.backup_interval_hours = backup_interval_hours
        self._backup_task: Optional[asyncio.Task] = None
        self._shutdown_event = asyncio.Event()
        self._registry: Optional[ProjectRegistry] = None

        logger.info(
            f"BackupScheduler initialized (interval={backup_interval_hours}h)"
        )

    async def startup_backup_check(self, registry: ProjectRegistry) -> Tuple[bool, str]:
        """
        Check if backup is needed on startup and create if necessary.

        Args:
            registry: ProjectRegistry instance

        Returns:
            Tuple of (backup_created: bool, message: str)
        """
        self._registry = registry

        # Update last backup check time
        self.backup_manager.update_last_backup_check(registry)

        # Check if backup is needed
        if self.backup_manager.should_create_backup(registry):
            logger.info("Startup backup check: backup needed (more than 24h since last backup)")

            try:
                # Create backup asynchronously (non-blocking)
                await self._create_backup_async(registry)
                return True, "Startup backup created successfully"
            except Exception as e:
                logger.error(f"Failed to create startup backup: {e}")
                return False, f"Failed to create startup backup: {e}"
        else:
            last_backup = self.backup_manager.get_last_backup_time(registry)
            if last_backup:
                time_ago = (asyncio.get_event_loop().time() - last_backup.timestamp()) / 3600
                msg = f"Startup backup check: no backup needed (last backup {time_ago:.1f}h ago)"
            else:
                msg = "Startup backup check: no backup needed (no previous backup)"
            logger.info(msg)
            return False, msg

    def start_periodic_backup(self, registry: ProjectRegistry) -> None:
        """
        Start the background periodic backup task.

        Args:
            registry: ProjectRegistry instance
        """
        self._registry = registry

        if self._backup_task is not None and not self._backup_task.done():
            logger.warning("Periodic backup task already running")
            return

        logger.info(
            f"Starting periodic backup task (interval={self.backup_interval_hours}h)"
        )

        # Create background task
        self._backup_task = asyncio.create_task(
            self._periodic_backup_loop(registry),
            name="periodic_backup"
        )

    async def stop_periodic_backup(self) -> None:
        """
        Stop the background periodic backup task gracefully.

        Waits for the current backup to complete if in progress.
        """
        logger.info("Stopping periodic backup task...")

        # Signal shutdown
        self._shutdown_event.set()

        # Wait for task to complete (with timeout)
        if self._backup_task and not self._backup_task.done():
            try:
                await asyncio.wait_for(self._backup_task, timeout=60)
                logger.info("Periodic backup task stopped gracefully")
            except asyncio.TimeoutError:
                logger.warning("Periodic backup task did not stop within timeout, cancelling")
                self._backup_task.cancel()
                try:
                    await self._backup_task
                except asyncio.CancelledError:
                    logger.info("Periodic backup task cancelled")
            except Exception as e:
                logger.error(f"Error stopping periodic backup task: {e}")

    async def _periodic_backup_loop(self, registry: ProjectRegistry) -> None:
        """
        Background task loop for periodic backups.

        Args:
            registry: ProjectRegistry instance
        """
        interval_seconds = self.backup_interval_hours * 3600

        logger.info(f"Periodic backup loop started (interval={interval_seconds}s)")

        while not self._shutdown_event.is_set():
            try:
                # Wait for shutdown signal or interval
                await asyncio.wait_for(
                    self._shutdown_event.wait(),
                    timeout=interval_seconds
                )

                # If shutdown was signaled, exit loop
                if self._shutdown_event.is_set():
                    logger.info("Periodic backup loop received shutdown signal")
                    break

                # Check if backup is needed
                if self.backup_manager.should_create_backup(registry):
                    logger.info("Periodic backup: creating backup")
                    await self._create_backup_async(registry)
                else:
                    logger.debug("Periodic backup: no backup needed")

            except asyncio.TimeoutError:
                # Timeout is expected - means interval passed without shutdown
                continue
            except Exception as e:
                logger.error(f"Error in periodic backup loop: {e}")
                # Wait a bit before retrying to avoid tight error loop
                await asyncio.sleep(60)

        logger.info("Periodic backup loop exited")

    async def _create_backup_async(self, registry: ProjectRegistry) -> None:
        """
        Create a backup asynchronously.

        Args:
            registry: ProjectRegistry instance

        Raises:
            Exception: If backup creation fails
        """
        try:
            logger.info("Creating backup asynchronously...")
            backup_metadata = await self.backup_manager.create_backup_async(
                registry=registry
            )
            logger.info(
                f"Backup created successfully: {backup_metadata.backup_path.name} "
                f"({backup_metadata.project_count} projects, "
                f"{backup_metadata.backup_size_bytes} bytes)"
            )
        except Exception as e:
            logger.error(f"Failed to create backup: {e}")
            raise


# ============================================================================
# Global backup scheduler instance
# =============================================================================

_global_backup_scheduler: Optional[BackupScheduler] = None


def get_backup_scheduler() -> BackupScheduler:
    """
    Get the global backup scheduler instance.

    Returns:
        BackupScheduler instance
    """
    global _global_backup_scheduler

    if _global_backup_scheduler is None:
        _global_backup_scheduler = BackupScheduler()

    return _global_backup_scheduler


def setup_signal_handlers(scheduler: BackupScheduler) -> None:
    """
    Setup signal handlers for graceful shutdown.

    Args:
        scheduler: BackupScheduler instance
    """
    def signal_handler(signum, frame):
        """Handle shutdown signals."""
        logger.info(f"Received signal {signum}, initiating graceful shutdown...")
        # Signal the async shutdown in the event loop
        loop = asyncio.get_event_loop()
        if loop.is_running():
            loop.call_soon_threadsafe(scheduler._shutdown_event.set)

    # Register signal handlers
    signal.signal(signal.SIGTERM, signal_handler)
    signal.signal(signal.SIGINT, signal_handler)

    logger.info("Signal handlers registered for graceful shutdown")
