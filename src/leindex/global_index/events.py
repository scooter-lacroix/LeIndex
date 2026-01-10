"""
Global Index Events Module

This module defines the events used for communication between the project registry
and the global index. Events are emitted when projects are indexed, updated, or deleted.

Event Flow:
    Project Index Complete
        ↓
    ProjectRegistry.on_index_complete()
        ↓
    Emit ProjectIndexedEvent(project_id, stats, ...)
        ↓
    GlobalIndex.on_project_indexed(event)
        ├→ Tier 1: Update metadata synchronously (<5ms)
        └→ Tier 2: Mark stale (don't delete, don't rebuild)
"""

from dataclasses import dataclass, field
from typing import Dict, Any, Optional
from datetime import datetime
import time


@dataclass
class ProjectIndexedEvent:
    """
    Emitted when a project indexing operation completes.

    This event triggers updates to both Tier 1 (synchronous metadata update)
    and Tier 2 (marking affected queries as stale).

    Attributes:
        project_id: Unique identifier for the project
        project_path: File system path to the project
        timestamp: Unix timestamp when indexing completed
        stats: Dictionary containing indexing statistics
        status: Indexing status ("completed", "error", "partial")
        error_message: Optional error message if status is "error"
        metadata: Additional metadata about the indexed project
    """
    project_id: str
    project_path: str
    timestamp: float = field(default_factory=time.time)
    stats: Dict[str, Any] = field(default_factory=dict)
    status: str = "completed"
    error_message: Optional[str] = None
    metadata: Dict[str, Any] = field(default_factory=dict)

    def __post_init__(self):
        """Validate event data after initialization."""
        if self.status not in ["completed", "error", "partial", "building"]:
            raise ValueError(f"Invalid status: {self.status}")

        if self.status == "error" and not self.error_message:
            raise ValueError("error_message required when status is 'error'")


@dataclass
class ProjectUpdatedEvent:
    """
    Emitted when a project is updated (files changed, modified, deleted).

    This event is less frequent than ProjectIndexedEvent and indicates
    significant changes to the project structure.

    Attributes:
        project_id: Unique identifier for the project
        timestamp: Unix timestamp when update was detected
        change_type: Type of change ("files_added", "files_deleted", "files_modified")
        affected_files: List of file paths that changed
        metadata: Additional metadata about the update
    """
    project_id: str
    timestamp: float = field(default_factory=time.time)
    change_type: str = "files_modified"
    affected_files: list = field(default_factory=list)
    metadata: Dict[str, Any] = field(default_factory=dict)


@dataclass
class ProjectDeletedEvent:
    """
    Emitted when a project is deleted from the registry.

    This event triggers cleanup of metadata from Tier 1 and invalidation
    of all related queries in Tier 2.

    Attributes:
        project_id: Unique identifier for the deleted project
        timestamp: Unix timestamp when deletion occurred
        metadata: Additional metadata about the deletion
    """
    project_id: str
    timestamp: float = field(default_factory=time.time)
    metadata: Dict[str, Any] = field(default_factory=dict)


# Event type aliases for better readability
IndexedEvent = ProjectIndexedEvent
UpdatedEvent = ProjectUpdatedEvent
DeletedEvent = ProjectDeletedEvent
