"""
Auto-Registration Integration for the meta-registry system.

This module provides automatic registration integration with the indexing
pipeline, ensuring projects are registered after their index is saved.

Event Integration:
    This module emits ProjectIndexedEvent after successful registration,
    which triggers updates to the global index (Tier 1 and Tier 2).
"""

import os
import time
from typing import Optional, Dict, Any, List
from datetime import datetime
import logging

from .project_registry import ProjectRegistry, ProjectInfo, DuplicateProjectError
from .directories import get_project_index_dir

logger = logging.getLogger(__name__)


def _get_event_bus():
    """
    Lazy import and get the global event bus.

    This avoids circular imports and ensures the event bus is only
    imported when actually needed.
    """
    try:
        from leindex.global_index.event_bus import get_global_event_bus
        return get_global_event_bus()
    except ImportError:
        logger.warning("Global index event bus not available, events will not be emitted")
        return None


class RegistrationIntegrator:
    """
    Integrates automatic project registration with the indexing pipeline.

    This class handles:
    - Automatic registration after MessagePack index save
    - Sequential write pattern (index first, registry second)
    - Graceful failure handling (log warning, continue)
    - Registry updates on reindex (update timestamp, stats)
    - Event emission for global index integration

    The integrator is designed to be called after index save operations,
    ensuring that the registry is always kept in sync with the filesystem.

    Event Integration:
        After successful registration, this class emits ProjectIndexedEvent
        to trigger updates to the global index (Tier 1 metadata and Tier 2 cache).

    Attributes:
        registry: ProjectRegistry instance for registration
        enabled: Whether auto-registration is enabled
        emit_events: Whether to emit events for global index integration
    """

    def __init__(
        self,
        registry: Optional[ProjectRegistry] = None,
        enabled: bool = True,
        emit_events: bool = True,
    ):
        """
        Initialize the registration integrator.

        Args:
            registry: ProjectRegistry instance. If None, creates a new instance.
            enabled: Whether auto-registration is enabled (default: True)
            emit_events: Whether to emit events for global index (default: True)
        """
        self.registry = registry if registry is not None else ProjectRegistry()
        self.enabled = enabled
        self.emit_events = emit_events
        self._event_bus = None

        logger.info(
            f"RegistrationIntegrator initialized "
            f"(enabled={enabled}, emit_events={emit_events})"
        )

    # ------------------------------------------------------------------------
    # Event Emission
    # ------------------------------------------------------------------------

    def _emit_project_indexed_event(
        self,
        project_path: str,
        project_id: str,
        file_count: int,
        stats: Dict[str, Any],
        status: str = "completed"
    ) -> None:
        """
        Emit ProjectIndexedEvent for global index integration.

        This method is called after successful project registration to trigger
        updates to Tier 1 (metadata) and Tier 2 (cache invalidation) of the
        global index.

        Args:
            project_path: Absolute path to the project
            project_id: Unique project identifier (database ID)
            file_count: Number of files in the index
            stats: Index statistics
            status: Indexing status ("completed", "error", "partial")
        """
        if not self.emit_events:
            return

        try:
            # Lazy load event bus
            if self._event_bus is None:
                self._event_bus = _get_event_bus()

            if self._event_bus is None:
                return

            # Create event
            from leindex.global_index.event_bus import Event

            event = Event(
                event_type="project_indexed",
                timestamp=time.time(),
                data={
                    "project_id": project_id,
                    "project_path": project_path,
                    "file_count": file_count,
                    "stats": stats,
                    "status": status,
                }
            )

            # Emit event (synchronous, should complete in <1ms)
            self._event_bus.emit(event)

            logger.debug(
                f"Emitted project_indexed event for {project_path} "
                f"(id={project_id}, status={status})"
            )

        except Exception as e:
            # Don't fail registration if event emission fails
            logger.warning(
                f"Failed to emit project_indexed event: {e}. "
                f"Registration succeeded but global index may be out of sync."
            )

    # ------------------------------------------------------------------------
    # Registration Methods
    # ------------------------------------------------------------------------

    def register_after_save(
        self,
        project_path: str,
        index_data: Dict[str, Any],
        file_count: int,
        config: Optional[Dict[str, Any]] = None,
        is_reindex: bool = False,
    ) -> Optional[ProjectInfo]:
        """
        Register a project after its index has been saved.

        This implements the sequential write pattern:
        1. Index is saved first (already complete when this is called)
        2. Registry is updated second (this method)

        Registration failures are handled gracefully - a warning is logged
        but the operation continues.

        Args:
            project_path: Absolute path to the project
            index_data: Index data dictionary
            file_count: Number of files in the index
            config: Optional configuration dictionary
            is_reindex: Whether this is a reindex operation

        Returns:
            ProjectInfo if registration succeeded, None otherwise
        """
        if not self.enabled:
            logger.debug("Auto-registration is disabled, skipping")
            return None

        try:
            # Normalize path
            project_path = os.path.abspath(project_path)

            # Get index location
            index_location = str(get_project_index_dir(project_path))

            # Check if already registered
            exists = self.registry.exists(project_path)

            if exists:
                if is_reindex:
                    # Update existing entry on reindex
                    return self._update_registered_project(
                        project_path,
                        file_count,
                        config,
                    )
                else:
                    # Project already registered, skip
                    logger.debug(f"Project already registered: {project_path}")
                    return self.registry.get_by_path(project_path)
            else:
                # Register new project
                return self._register_new_project(
                    project_path,
                    file_count,
                    config,
                    index_location,
                )

        except Exception as e:
            # Graceful failure handling - log warning, continue
            logger.warning(
                f"Failed to register project {project_path}: {e}. "
                f"Continuing anyway (graceful degradation)."
            )
            return None

    def _register_new_project(
        self,
        project_path: str,
        file_count: int,
        config: Optional[Dict[str, Any]],
        index_location: str,
    ) -> Optional[ProjectInfo]:
        """
        Register a new project in the registry.

        Args:
            project_path: Absolute path to the project
            file_count: Number of files in the index
            config: Optional configuration dictionary
            index_location: Path to index data

        Returns:
            ProjectInfo if registration succeeded, None otherwise
        """
        try:
            # Prepare config
            if config is None:
                config = {
                    "auto_registered": True,
                    "registered_at": datetime.now().isoformat(),
                }
            else:
                config["auto_registered"] = True
                config["registered_at"] = datetime.now().isoformat()

            # Prepare stats
            stats = {
                "file_count": file_count,
                "indexed_at": datetime.now().isoformat(),
            }

            # Insert into registry
            project_info = self.registry.insert(
                path=project_path,
                indexed_at=datetime.now(),
                file_count=file_count,
                config=config,
                stats=stats,
                index_location=index_location,
            )

            logger.info(f"Auto-registered new project: {project_path}")

            # Emit event for global index integration
            self._emit_project_indexed_event(
                project_path=project_path,
                project_id=str(project_info.id),
                file_count=file_count,
                stats=stats,
                status="completed"
            )

            return project_info

        except DuplicateProjectError:
            # Race condition - project was just registered by another process
            logger.debug(f"Project already registered (race condition): {project_path}")
            project_info = self.registry.get_by_path(project_path)
            # Still emit event since the project is now registered
            if project_info:
                self._emit_project_indexed_event(
                    project_path=project_path,
                    project_id=str(project_info.id),
                    file_count=file_count,
                    stats={"file_count": file_count},
                    status="completed"
                )
            return project_info
        except Exception as e:
            logger.error(f"Failed to register new project {project_path}: {e}")
            return None

    def _update_registered_project(
        self,
        project_path: str,
        file_count: int,
        config: Optional[Dict[str, Any]],
    ) -> Optional[ProjectInfo]:
        """
        Update an existing project registration (e.g., on reindex).

        Args:
            project_path: Absolute path to the project
            file_count: New file count
            config: Optional configuration dictionary

        Returns:
            Updated ProjectInfo if update succeeded, None otherwise
        """
        try:
            # Prepare stats
            stats = {
                "file_count": file_count,
                "reindexed_at": datetime.now().isoformat(),
            }

            # Update in registry
            project_info = self.registry.update(
                path=project_path,
                indexed_at=datetime.now(),
                file_count=file_count,
                stats=stats,
            )

            logger.info(f"Auto-updated project on reindex: {project_path}")

            # Emit event for global index integration
            self._emit_project_indexed_event(
                project_path=project_path,
                project_id=str(project_info.id),
                file_count=file_count,
                stats=stats,
                status="completed"
            )

            return project_info

        except Exception as e:
            logger.error(f"Failed to update project {project_path}: {e}")
            return None

    # ------------------------------------------------------------------------
    # Batch Operations
    # ------------------------------------------------------------------------

    def register_batch(
        self,
        projects: List[Dict[str, Any]],
    ) -> Dict[str, Any]:
        """
        Register multiple projects in batch.

        Args:
            projects: List of project dictionaries with keys:
                - path: Project path
                - index_data: Index data
                - file_count: File count
                - config: Optional config
                - is_reindex: Whether this is a reindex

        Returns:
            Dictionary with success/failure statistics
        """
        results = {
            "success_count": 0,
            "failure_count": 0,
            "skipped_count": 0,
            "registered_projects": [],
            "failed_projects": [],
        }

        for project_spec in projects:
            project_path = project_spec.get("path")
            if not project_path:
                results["failure_count"] += 1
                continue

            result = self.register_after_save(
                project_path=project_path,
                index_data=project_spec.get("index_data", {}),
                file_count=project_spec.get("file_count", 0),
                config=project_spec.get("config"),
                is_reindex=project_spec.get("is_reindex", False),
            )

            if result:
                results["success_count"] += 1
                results["registered_projects"].append(project_path)
            else:
                results["failure_count"] += 1
                results["failed_projects"].append(project_path)

        logger.info(
            f"Batch registration complete: "
            f"{results['success_count']} succeeded, "
            f"{results['failure_count']} failed, "
            f"{results['skipped_count']} skipped"
        )

        return results

    # ------------------------------------------------------------------------
    # Orphan Detection
    # ------------------------------------------------------------------------

    def detect_and_register_orphans(
        self,
        search_paths: Optional[List[str]] = None,
        max_depth: int = 3,
        auto_register: bool = False,
    ) -> Dict[str, Any]:
        """
        Detect and optionally register orphaned projects.

        This method should be called on startup to detect any orphaned
        indexes from previous runs.

        Args:
            search_paths: Optional list of paths to search
            max_depth: Maximum search depth
            auto_register: Whether to automatically register orphans

        Returns:
            Dictionary with detection results
        """
        from .orphan_detector import OrphanDetector

        # Initialize orphan detector
        detector = OrphanDetector(
            registry=self.registry,
            search_paths=search_paths,
        )

        # Scan for orphans
        orphans = detector.scan_for_orphans(max_depth=max_depth)

        results = {
            "orphans_found": len(orphans),
            "orphans": [o.to_dict() for o in orphans],
            "registered_count": 0,
            "registered_projects": [],
        }

        if auto_register and orphans:
            # Register all orphans
            for orphan in orphans:
                try:
                    project_info = detector.register_orphan(orphan)
                    results["registered_count"] += 1
                    results["registered_projects"].append(orphan.path)
                except Exception as e:
                    logger.warning(f"Failed to register orphan {orphan.path}: {e}")

        logger.info(
            f"Orphan detection complete: "
            f"{results['orphans_found']} found, "
            f"{results['registered_count']} registered"
        )

        return results


# Global singleton instance for convenience
_global_integrator: Optional[RegistrationIntegrator] = None


def get_registration_integrator() -> RegistrationIntegrator:
    """
    Get the global registration integrator singleton.

    Returns:
        RegistrationIntegrator instance
    """
    global _global_integrator
    if _global_integrator is None:
        _global_integrator = RegistrationIntegrator()
    return _global_integrator


def register_after_index_save(
    project_path: str,
    index_data: Dict[str, Any],
    file_count: int,
    config: Optional[Dict[str, Any]] = None,
    is_reindex: bool = False,
) -> Optional[ProjectInfo]:
    """
    Convenience function to register a project after index save.

    This function uses the global RegistrationIntegrator singleton.

    Args:
        project_path: Absolute path to the project
        index_data: Index data dictionary
        file_count: Number of files in the index
        config: Optional configuration dictionary
        is_reindex: Whether this is a reindex operation

    Returns:
        ProjectInfo if registration succeeded, None otherwise
    """
    integrator = get_registration_integrator()
    return integrator.register_after_save(
        project_path=project_path,
        index_data=index_data,
        file_count=file_count,
        config=config,
        is_reindex=is_reindex,
    )
