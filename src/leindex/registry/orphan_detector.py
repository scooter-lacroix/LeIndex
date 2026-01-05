"""
Orphan Detection and Recovery for the meta-registry system.

This module provides functionality to detect orphaned indexes that exist
on the filesystem but are not registered in the project registry, and
offers recovery options to register or clean them up.
"""

import os
from pathlib import Path
from typing import List, Dict, Any, Optional, Tuple
import logging
from datetime import datetime

from .project_registry import ProjectRegistry, ProjectInfo
from .directories import (
    get_project_registry_dir,
    get_project_index_dir,
)

logger = logging.getLogger(__name__)


# ============================================================================
# Data Classes
# ============================================================================


class OrphanedProject:
    """
    Information about an orphaned project.

    An orphaned project is a project that has a .leindex directory
    with index data but is not registered in the global registry.

    Attributes:
        path: Absolute path to the orphaned project
        index_location: Path to the index data
        index_exists: Whether the index file exists
        index_size: Size of the index file in bytes (0 if doesn't exist)
        last_modified: Last modification time of the index directory
        reason: Reason why this project is considered orphaned
    """

    def __init__(
        self,
        path: str,
        index_location: str,
        index_exists: bool,
        index_size: int,
        last_modified: Optional[datetime],
        reason: str,
    ):
        self.path = path
        self.index_location = index_location
        self.index_exists = index_exists
        self.index_size = index_size
        self.last_modified = last_modified
        self.reason = reason

    def to_dict(self) -> Dict[str, Any]:
        """
        Convert to dictionary representation.

        Returns:
            Dictionary with all fields
        """
        return {
            "path": self.path,
            "index_location": self.index_location,
            "index_exists": self.index_exists,
            "index_size": self.index_size,
            "last_modified": self.last_modified.isoformat() if self.last_modified else None,
            "reason": self.reason,
        }

    def __repr__(self) -> str:
        return (
            f"OrphanedProject(path={self.path}, "
            f"index_exists={self.index_exists}, "
            f"index_size={self.index_size})"
        )


# ============================================================================
# Orphan Detector
# ============================================================================


class OrphanDetector:
    """
    Detector for orphaned project indexes.

    This class scans the filesystem for projects that have .leindex
    directories with index data but are not registered in the global
    project registry.

    The detector performs:
    1. Filesystem scan for .leindex directories
    2. Comparison against registry entries
    3. Classification of orphaned projects
    4. Recovery options (register or cleanup)

    Attributes:
        registry: ProjectRegistry instance for checking registered projects
        search_paths: List of root paths to search for orphaned projects
    """

    def __init__(
        self,
        registry: Optional[ProjectRegistry] = None,
        search_paths: Optional[List[str]] = None,
    ):
        """
        Initialize the orphan detector.

        Args:
            registry: ProjectRegistry instance. If None, creates a new instance.
            search_paths: List of root paths to search. If None, uses common paths.
        """
        self.registry = registry if registry is not None else ProjectRegistry()
        self.search_paths = search_paths if search_paths is not None else self._default_search_paths()
        logger.info(
            f"OrphanDetector initialized with {len(self.search_paths)} search paths"
        )

    @staticmethod
    def _default_search_paths() -> List[str]:
        """
        Get default search paths for orphaned projects.

        Returns:
            List of common project root directories
        """
        paths = []

        # Add user's home directory
        home = str(Path.home())
        paths.append(home)

        # Add common project directories
        common_dirs = ["projects", "dev", "development", "workspace", "work"]
        for dir_name in common_dirs:
            project_dir = Path(home) / dir_name
            if project_dir.exists() and project_dir.is_dir():
                paths.append(str(project_dir))

        # Add current directory if it's a project directory
        cwd = os.getcwd()
        if cwd != home:
            paths.append(cwd)

        logger.debug(f"Default search paths: {paths}")
        return paths

    # ------------------------------------------------------------------------
    # Detection Methods
    # ------------------------------------------------------------------------

    def scan_for_orphans(
        self,
        max_depth: int = 3,
        follow_symlinks: bool = False,
    ) -> List[OrphanedProject]:
        """
        Scan filesystem for orphaned projects.

        This method performs a depth-first search of the configured search
        paths, looking for .leindex directories that are not registered
        in the project registry.

        Args:
            max_depth: Maximum directory depth to search (default: 3)
            follow_symlinks: Whether to follow symbolic links (default: False)

        Returns:
            List of OrphanedProject instances
        """
        logger.info(f"Scanning for orphaned projects (max_depth={max_depth})...")

        orphans = []
        registered_paths = self._get_registered_paths()

        for search_path in self.search_paths:
            try:
                found_orphans = self._scan_directory(
                    Path(search_path),
                    registered_paths,
                    max_depth=max_depth,
                    follow_symlinks=follow_symlinks,
                )
                orphans.extend(found_orphans)
            except (PermissionError, OSError) as e:
                logger.warning(f"Cannot access search path {search_path}: {e}")
                continue

        logger.info(f"Found {len(orphans)} orphaned projects")
        return orphans

    def _get_registered_paths(self) -> set:
        """
        Get set of all registered project paths.

        Returns:
            Set of registered absolute paths
        """
        try:
            registered = self.registry.list_all()
            paths = {project.path for project in registered}
            logger.debug(f"Found {len(paths)} registered projects")
            return paths
        except Exception as e:
            logger.error(f"Error getting registered paths: {e}")
            return set()

    def _scan_directory(
        self,
        root_path: Path,
        registered_paths: set,
        max_depth: int = 3,
        current_depth: int = 0,
        follow_symlinks: bool = False,
    ) -> List[OrphanedProject]:
        """
        Recursively scan a directory for orphaned projects.

        Args:
            root_path: Root directory to scan
            registered_paths: Set of registered project paths
            max_depth: Maximum depth to search
            current_depth: Current depth in recursion
            follow_symlinks: Whether to follow symlinks

        Returns:
            List of OrphanedProject instances found in this directory
        """
        orphans = []

        # Check if we've exceeded max depth
        if current_depth > max_depth:
            return orphans

        # Check if this is an orphaned project
        orphan = self._check_orphaned_project(root_path, registered_paths)
        if orphan:
            orphans.append(orphan)
            # Don't recurse into orphaned project directories
            return orphans

        # Recurse into subdirectories
        try:
            for entry in root_path.iterdir():
                if entry.is_dir():
                    # Skip hidden directories and common non-project directories
                    if entry.name.startswith("."):
                        continue

                    skip_dirs = {
                        "node_modules",
                        "venv",
                        "virtualenv",
                        "env",
                        ".git",
                        ".svn",
                        ".hg",
                        "__pycache__",
                        "target",
                        "build",
                        "dist",
                    }
                    if entry.name in skip_dirs:
                        continue

                    # Check symlinks
                    if entry.is_symlink() and not follow_symlinks:
                        continue

                    # Recurse
                    sub_orphans = self._scan_directory(
                        entry,
                        registered_paths,
                        max_depth=max_depth,
                        current_depth=current_depth + 1,
                        follow_symlinks=follow_symlinks,
                    )
                    orphans.extend(sub_orphans)
        except (PermissionError, OSError) as e:
            logger.debug(f"Cannot scan directory {root_path}: {e}")

        return orphans

    def _check_orphaned_project(
        self,
        project_path: Path,
        registered_paths: set,
    ) -> Optional[OrphanedProject]:
        """
        Check if a directory is an orphaned project.

        An orphaned project has:
        1. A .leindex directory
        2. Index data present
        3. Not registered in the registry

        Args:
            project_path: Path to the project directory
            registered_paths: Set of registered project paths

        Returns:
            OrphanedProject if orphaned, None otherwise
        """
        try:
            # Normalize path for comparison
            abs_path = str(project_path.resolve())
            if abs_path in registered_paths:
                return None

            # Check for .leindex directory
            project_dir = get_project_registry_dir(project_path)
            if not project_dir.exists():
                return None

            # Check for index data
            index_dir = get_project_index_dir(project_path)
            index_exists = index_dir.exists()

            # Check for index files
            index_size = 0
            if index_exists:
                for index_file in index_dir.rglob("*"):
                    if index_file.is_file():
                        index_size += index_file.stat().st_size

            # Only consider orphaned if there's actual index data
            if index_size == 0:
                return None

            # Get last modified time
            last_modified = None
            if project_dir.exists():
                last_modified = datetime.fromtimestamp(project_dir.stat().st_mtime)

            # Determine reason
            if not index_exists:
                reason = "Registry directory exists but no index data"
            else:
                reason = "Has index data but not registered in registry"

            return OrphanedProject(
                path=abs_path,
                index_location=str(index_dir),
                index_exists=index_exists,
                index_size=index_size,
                last_modified=last_modified,
                reason=reason,
            )

        except (PermissionError, OSError) as e:
            logger.debug(f"Error checking {project_path}: {e}")
            return None

    # ------------------------------------------------------------------------
    # Recovery Methods
    # ------------------------------------------------------------------------

    def register_orphan(
        self,
        orphan: OrphanedProject,
        config: Optional[Dict[str, Any]] = None,
        stats: Optional[Dict[str, Any]] = None,
    ) -> ProjectInfo:
        """
        Register an orphaned project in the registry.

        Args:
            orphan: OrphanedProject to register
            config: Optional configuration to use
            stats: Optional statistics to use

        Returns:
            Registered ProjectInfo

        Raises:
            RegistryError: If registration fails
        """
        logger.info(f"Registering orphaned project: {orphan.path}")

        # Use provided config/stats or create defaults
        if config is None:
            config = {
                "registered_from": "orphan_detector",
                "auto_registered": True,
            }

        if stats is None:
            stats = {
                "index_size_bytes": orphan.index_size,
                "recovered_at": datetime.now().isoformat(),
            }

        try:
            # Determine indexed_at from last_modified
            indexed_at = orphan.last_modified if orphan.last_modified else datetime.now()

            # Count files (estimate based on index size)
            file_count = max(1, orphan.index_size // 1000)  # Rough estimate

            # Register the project
            project_info = self.registry.insert(
                path=orphan.path,
                indexed_at=indexed_at,
                file_count=file_count,
                config=config,
                stats=stats,
                index_location=orphan.index_location,
            )

            logger.info(f"Successfully registered orphaned project: {orphan.path}")
            return project_info

        except Exception as e:
            logger.error(f"Failed to register orphaned project {orphan.path}: {e}")
            raise

    def cleanup_orphan(self, orphan: OrphanedProject) -> bool:
        """
        Clean up an orphaned project by removing its index data.

        This removes the .leindex directory and all its contents.

        Args:
            orphan: OrphanedProject to cleanup

        Returns:
            True if cleanup succeeded, False otherwise

        Raises:
            OSError: If cleanup fails
        """
        logger.info(f"Cleaning up orphaned project: {orphan.path}")

        try:
            project_dir = get_project_registry_dir(orphan.path)

            if not project_dir.exists():
                logger.warning(f"Project directory does not exist: {project_dir}")
                return False

            # Remove the entire directory
            import shutil

            shutil.rmtree(project_dir)

            logger.info(f"Successfully cleaned up orphaned project: {orphan.path}")
            return True

        except Exception as e:
            logger.error(f"Failed to cleanup orphaned project {orphan.path}: {e}")
            raise

    def suggest_actions(self, orphans: List[OrphanedProject]) -> Dict[str, List[str]]:
        """
        Suggest actions for a list of orphaned projects.

        Args:
            orphans: List of OrphanedProject instances

        Returns:
            Dictionary with 'register' and 'cleanup' keys containing project paths
        """
        suggestions = {"register": [], "cleanup": []}

        for orphan in orphans:
            # Suggest registration if index exists and has data
            if orphan.index_exists and orphan.index_size > 0:
                suggestions["register"].append(orphan.path)

            # Suggest cleanup if index doesn't exist or is very small
            if not orphan.index_exists or orphan.index_size < 1000:
                suggestions["cleanup"].append(orphan.path)

        logger.info(
            f"Suggested {len(suggestions['register'])} registrations, "
            f"{len(suggestions['cleanup'])} cleanups"
        )
        return suggestions

    # ------------------------------------------------------------------------
    # Batch Operations
    # ------------------------------------------------------------------------

    def register_all_orphans(
        self,
        orphans: List[OrphanedProject],
        config: Optional[Dict[str, Any]] = None,
        stats: Optional[Dict[str, Any]] = None,
    ) -> Tuple[int, int, List[str]]:
        """
        Register all orphaned projects in a list.

        Args:
            orphans: List of OrphanedProject instances
            config: Optional configuration to use for all
            stats: Optional statistics to use for all

        Returns:
            Tuple of (success_count, failure_count, failed_paths)
        """
        success_count = 0
        failure_count = 0
        failed_paths = []

        for orphan in orphans:
            try:
                self.register_orphan(orphan, config=config, stats=stats)
                success_count += 1
            except Exception as e:
                logger.error(f"Failed to register {orphan.path}: {e}")
                failure_count += 1
                failed_paths.append(orphan.path)

        logger.info(
            f"Batch registration complete: "
            f"{success_count} succeeded, {failure_count} failed"
        )
        return success_count, failure_count, failed_paths

    def cleanup_all_orphans(
        self,
        orphans: List[OrphanedProject],
    ) -> Tuple[int, int, List[str]]:
        """
        Clean up all orphaned projects in a list.

        Args:
            orphans: List of OrphanedProject instances

        Returns:
            Tuple of (success_count, failure_count, failed_paths)
        """
        success_count = 0
        failure_count = 0
        failed_paths = []

        for orphan in orphans:
            try:
                if self.cleanup_orphan(orphan):
                    success_count += 1
                else:
                    failure_count += 1
                    failed_paths.append(orphan.path)
            except Exception as e:
                logger.error(f"Failed to cleanup {orphan.path}: {e}")
                failure_count += 1
                failed_paths.append(orphan.path)

        logger.info(
            f"Batch cleanup complete: "
            f"{success_count} succeeded, {failure_count} failed"
        )
        return success_count, failure_count, failed_paths
