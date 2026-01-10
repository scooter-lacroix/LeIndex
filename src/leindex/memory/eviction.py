"""
Priority-Based Eviction System for LeIndex

This module implements intelligent eviction of cached data based on priority
and recent access patterns. It uses a scoring system that combines recency
of access with priority weights to select the best candidates for eviction.

Key Features:
- Priority-based eviction scoring (recent_access × priority_weight)
- LRU (Least Recently Used) candidate selection
- Project unloading with memory tracking
- Thread-safe implementation
- Comprehensive logging of eviction decisions

Priority Weights:
- high: 2.0 (less likely to be evicted)
- normal: 1.0 (baseline)
- low: 0.5 (more likely to be evicted)

Eviction Scoring:
- Higher score = better candidate for eviction
- Score = (current_time - last_access_time) × priority_weight
- Older access times + lower priority = higher eviction score

Example:
    >>> from leindex.memory.evacuation import EvictionManager, ProjectCandidate
    >>> manager = EvictionManager()
    >>> # Simulate having some projects loaded
    >>> candidates = [
    ...     ProjectCandidate(project_id="p1", last_access=1000, priority="normal"),
    ...     ProjectCandidate(project_id="p2", last_access=500, priority="low"),
    ... ]
    >>> freed_mb = manager.emergency_eviction(candidates, target_mb=100)
    >>> print(f"Freed {freed_mb:.1f}MB")
"""

import logging
import time
from dataclasses import dataclass, field
from typing import Optional, List, Dict, Any, Callable
from enum import Enum
from threading import Lock
from abc import ABC, abstractmethod


logger = logging.getLogger(__name__)


# =============================================================================
# Project Priority Enum
# =============================================================================
class ProjectPriority(Enum):
    """Priority levels for projects."""
    HIGH = "high"
    NORMAL = "normal"
    LOW = "low"


# =============================================================================
# Project Eviction Candidate
# =============================================================================
@dataclass
class ProjectCandidate:
    """A project that is a candidate for eviction.

    Attributes:
        project_id: Unique identifier for the project
        project_path: Absolute path to the project
        last_access: Unix timestamp of last access
        priority: Priority level (high/normal/low)
        estimated_mb: Estimated memory usage in MB
        loaded_files: Number of files currently loaded
        is_index_loaded: Whether the index is currently loaded
        metadata: Additional metadata about the project
    """
    project_id: str
    project_path: str
    last_access: float
    priority: ProjectPriority = ProjectPriority.NORMAL
    estimated_mb: float = 256.0
    loaded_files: int = 0
    is_index_loaded: bool = True
    metadata: Dict[str, Any] = field(default_factory=dict)

    def get_eviction_score(self, current_time: Optional[float] = None) -> float:
        """Calculate eviction score for this project.

        Higher score = better candidate for eviction.
        Score = (current_time - last_access) × priority_weight

        Args:
            current_time: Current timestamp (uses time.time() if None)

        Returns:
            Eviction score (higher = more likely to be evicted)
        """
        if current_time is None:
            current_time = time.time()

        # Calculate time since last access (in seconds)
        time_since_access = current_time - self.last_access

        # Get priority weight
        priority_weight = _priority_weight(self.priority)

        # Calculate eviction score
        # Older access + lower priority = higher score
        score = time_since_access * priority_weight

        return score

    def to_dict(self) -> Dict[str, Any]:
        """Convert candidate to dictionary.

        Returns:
            Dictionary representation of the candidate
        """
        return {
            "project_id": self.project_id,
            "project_path": self.project_path,
            "last_access": self.last_access,
            "priority": self.priority.value,
            "estimated_mb": self.estimated_mb,
            "loaded_files": self.loaded_files,
            "is_index_loaded": self.is_index_loaded,
            "metadata": self.metadata,
            "eviction_score": self.get_eviction_score(),
        }


# =============================================================================
# Priority Weight Calculation
# =============================================================================
def _priority_weight(priority: ProjectPriority) -> float:
    """Get the priority weight for eviction scoring.

    Higher priority projects have higher weights, making them LESS likely
    to be evicted (since we want to KEEP high-priority projects).

    Args:
        priority: Project priority level

    Returns:
        Priority weight (high=2.0, normal=1.0, low=0.5)
    """
    weights = {
        ProjectPriority.HIGH: 2.0,
        ProjectPriority.NORMAL: 1.0,
        ProjectPriority.LOW: 0.5,
    }
    return weights.get(priority, 1.0)


# =============================================================================
# Eviction Result
# =============================================================================
@dataclass
class EvictionResult:
    """Result of an eviction operation.

    Attributes:
        success: Whether the eviction was successful
        projects_evicted: List of project IDs that were evicted
        memory_freed_mb: Actual memory freed in MB
        target_mb: Target memory to free in MB
        duration_seconds: Time taken for eviction
        message: Human-readable result message
        errors: List of errors that occurred during eviction
        timestamp: Unix timestamp when eviction completed
    """
    success: bool
    projects_evicted: List[str]
    memory_freed_mb: float
    target_mb: float
    duration_seconds: float
    message: str
    errors: List[str] = field(default_factory=list)
    timestamp: float = field(default_factory=time.time)

    def to_dict(self) -> Dict[str, Any]:
        """Convert result to dictionary.

        Returns:
            Dictionary representation of the result
        """
        return {
            "success": self.success,
            "projects_evicted": self.projects_evicted,
            "memory_freed_mb": self.memory_freed_mb,
            "target_mb": self.target_mb,
            "duration_seconds": self.duration_seconds,
            "message": self.message,
            "errors": self.errors,
            "timestamp": self.timestamp,
        }

    def __str__(self) -> str:
        """Get human-readable string representation."""
        status = "✓" if self.success else "✗"
        return (
            f"{status} Eviction: {'SUCCESS' if self.success else 'FAILED'} - "
            f"Freed {self.memory_freed_mb:.1f}MB / {self.target_mb:.1f}MB target "
            f"({len(self.projects_evicted)} projects) in {self.duration_seconds:.2f}s"
        )


# =============================================================================
# Project Unloader Interface
# =============================================================================
class ProjectUnloader(ABC):
    """Abstract interface for unloading projects from memory.

    Concrete implementations should know how to actually unload
    project data from the specific indexing system being used.
    """

    @abstractmethod
    def unload_project(self, project_id: str) -> tuple[bool, float]:
        """Unload a project from memory.

        Args:
            project_id: Unique identifier for the project

        Returns:
            Tuple of (success, memory_freed_mb)
        """
        pass

    @abstractmethod
    def get_loaded_projects(self) -> List[ProjectCandidate]:
        """Get list of currently loaded projects.

        Returns:
            List of project candidates for eviction
        """
        pass


# =============================================================================
# Eviction Manager
# =============================================================================
class EvictionManager:
    """Manages priority-based eviction of cached projects.

    This class implements intelligent eviction based on:
    1. Recent access (LRU - least recently used)
    2. Priority weights (high priority projects less likely to be evicted)
    3. Memory estimates (unload projects until target memory freed)

    Thread Safety:
        All public methods are thread-safe and can be called from multiple threads.

    Example:
        >>> manager = EvictionManager(unloader=my_unloader)
        >>> result = manager.emergency_eviction(target_mb=500)
        >>> if result.success:
        ...     print(f"Freed {result.memory_freed_mb:.1f}MB")
    """

    def __init__(self, unloader: Optional[ProjectUnloader] = None):
        """Initialize the eviction manager.

        Args:
            unloader: Project unloader implementation (optional)
        """
        self._unloader = unloader
        self._lock = Lock()

        # Statistics tracking
        self._total_evictions = 0
        self._total_memory_freed_mb = 0.0
        self._eviction_history: List[EvictionResult] = []

        # Callbacks
        self._before_eviction_callbacks: List[Callable[[ProjectCandidate], None]] = []
        self._after_eviction_callbacks: List[
            Callable[[ProjectCandidate, tuple[bool, float]], None]
        ] = []

    def set_unloader(self, unloader: ProjectUnloader) -> None:
        """Set the project unloader implementation.

        Args:
            unloader: Project unloader to use
        """
        with self._lock:
            self._unloader = unloader
            logger.info("Project unloader registered")

    def emergency_eviction(
        self,
        candidates: Optional[List[ProjectCandidate]] = None,
        target_mb: float = 512.0,
        max_projects: Optional[int] = None
    ) -> EvictionResult:
        """Perform emergency eviction to free memory.

        This is the main eviction method. It will unload projects until
        the target memory is freed or there are no more candidates.

        Args:
            candidates: List of project candidates (uses unloader if None)
            target_mb: Target memory to free in MB
            max_projects: Maximum number of projects to evict (optional)

        Returns:
            EvictionResult with details of what was evicted
        """
        start_time = time.time()

        try:
            logger.info(
                f"Starting emergency eviction: target={target_mb:.1f}MB, "
                f"max_projects={max_projects}"
            )

            # Get candidates if not provided
            if candidates is None:
                if self._unloader is None:
                    error = "No candidates provided and no unloader registered"
                    logger.error(error)

                    return EvictionResult(
                        success=False,
                        projects_evicted=[],
                        memory_freed_mb=0.0,
                        target_mb=target_mb,
                        duration_seconds=time.time() - start_time,
                        message=error,
                        errors=[error],
                    )

                candidates = self._unloader.get_loaded_projects()

            if not candidates:
                logger.warning("No candidates available for eviction")

                return EvictionResult(
                    success=False,
                    projects_evicted=[],
                    memory_freed_mb=0.0,
                    target_mb=target_mb,
                    duration_seconds=time.time() - start_time,
                    message="No candidates available for eviction",
                )

            # Sort candidates by eviction score (highest first)
            sorted_candidates = self._sort_candidates_by_score(candidates)

            # Evict projects until target reached or no more candidates
            evicted_projects = []
            memory_freed = 0.0
            errors = []

            for candidate in sorted_candidates:
                # Check if we've reached the target
                if memory_freed >= target_mb:
                    logger.info(
                        f"Target reached: freed {memory_freed:.1f}MB / {target_mb:.1f}MB"
                    )
                    break

                # Check if we've hit max projects limit
                if max_projects and len(evicted_projects) >= max_projects:
                    logger.info(f"Max projects reached: {len(evicted_projects)}")
                    break

                # Evict this project
                try:
                    success, freed_mb = self._evict_project(candidate)

                    if success:
                        evicted_projects.append(candidate.project_id)
                        memory_freed += freed_mb

                        logger.info(
                            f"Evicted {candidate.project_id}: "
                            f"freed {freed_mb:.1f}MB (score={candidate.get_eviction_score():.1f})"
                        )
                    else:
                        errors.append(f"Failed to evict {candidate.project_id}")

                except Exception as e:
                    error_msg = f"Error evicting {candidate.project_id}: {e}"
                    logger.error(error_msg)
                    errors.append(error_msg)

            duration = time.time() - start_time

            # Determine success
            success = memory_freed >= target_mb * 0.8  # 80% of target is acceptable
            message = (
                f"Evicted {len(evicted_projects)} projects, "
                f"freed {memory_freed:.1f}MB / {target_mb:.1f}MB target"
            )

            # Update statistics
            with self._lock:
                self._total_evictions += len(evicted_projects)
                self._total_memory_freed_mb += memory_freed

            # Create result
            result = EvictionResult(
                success=success,
                projects_evicted=evicted_projects,
                memory_freed_mb=memory_freed,
                target_mb=target_mb,
                duration_seconds=duration,
                message=message,
                errors=errors,
            )

            # Store in history
            self._eviction_history.append(result)

            # Log summary
            logger.info(f"Emergency eviction completed: {result}")

            return result

        except Exception as e:
            duration = time.time() - start_time
            error_msg = f"Emergency eviction failed: {e}"
            logger.error(error_msg)

            return EvictionResult(
                success=False,
                projects_evicted=[],
                memory_freed_mb=0.0,
                target_mb=target_mb,
                duration_seconds=duration,
                message=error_msg,
                errors=[error_msg],
            )

    def _sort_candidates_by_score(
        self,
        candidates: List[ProjectCandidate]
    ) -> List[ProjectCandidate]:
        """Sort candidates by eviction score (highest first).

        Args:
            candidates: List of project candidates

        Returns:
            Sorted list of candidates
        """
        current_time = time.time()

        # Calculate scores and sort
        sorted_candidates = sorted(
            candidates,
            key=lambda c: c.get_eviction_score(current_time),
            reverse=True  # Highest score first
        )

        # Log top candidates for debugging
        if sorted_candidates:
            logger.debug(
                f"Top eviction candidates: "
                f"{[c.project_id for c in sorted_candidates[:5]]}"
            )

        return sorted_candidates

    def _evict_project(self, candidate: ProjectCandidate) -> tuple[bool, float]:
        """Evict a single project from memory.

        Args:
            candidate: Project candidate to evict

        Returns:
            Tuple of (success, memory_freed_mb)
        """
        # Trigger before callbacks
        for callback in self._before_eviction_callbacks:
            try:
                callback(candidate)
            except Exception as e:
                logger.error(f"Error in before-eviction callback: {e}")

        # Evict the project
        if self._unloader:
            success, freed_mb = self._unloader.unload_project(candidate.project_id)
        else:
            # No unloader - simulate eviction
            logger.warning(f"No unloader registered, simulating eviction of {candidate.project_id}")
            success = True
            freed_mb = candidate.estimated_mb

        # Trigger after callbacks
        for callback in self._after_eviction_callbacks:
            try:
                callback(candidate, (success, freed_mb))
            except Exception as e:
                logger.error(f"Error in after-eviction callback: {e}")

        return success, freed_mb

    def get_statistics(self) -> Dict[str, Any]:
        """Get eviction statistics.

        Returns:
            Dictionary with eviction statistics
        """
        with self._lock:
            return {
                "total_evictions": self._total_evictions,
                "total_memory_freed_mb": self._total_memory_freed_mb,
                "eviction_history_count": len(self._eviction_history),
                "recent_evictions": [
                    {
                        "timestamp": r.timestamp,
                        "projects_evicted": len(r.projects_evicted),
                        "memory_freed_mb": r.memory_freed_mb,
                    }
                    for r in self._eviction_history[-10:]  # Last 10
                ],
            }

    def register_before_eviction_callback(
        self,
        callback: Callable[[ProjectCandidate], None]
    ) -> None:
        """Register a callback to be called before each project eviction.

        Args:
            callback: Function to call before evicting each project
        """
        with self._lock:
            self._before_eviction_callbacks.append(callback)

    def register_after_eviction_callback(
        self,
        callback: Callable[[ProjectCandidate, tuple[bool, float]], None]
    ) -> None:
        """Register a callback to be called after each project eviction.

        Args:
            callback: Function to call after evicting each project
        """
        with self._lock:
            self._after_eviction_callbacks.append(callback)


# =============================================================================
# Mock Project Unloader (for testing)
# =============================================================================
class MockProjectUnloader(ProjectUnloader):
    """Mock implementation of ProjectUnloader for testing.

    This simulates unloading projects without actually needing
    a real indexing system.
    """

    def __init__(self):
        """Initialize the mock unloader."""
        self._loaded_projects: Dict[str, ProjectCandidate] = {}
        self._unload_log: List[str] = []

    def add_project(
        self,
        project_id: str,
        project_path: str,
        priority: ProjectPriority = ProjectPriority.NORMAL,
        estimated_mb: float = 256.0
    ) -> None:
        """Add a project to the "loaded" list.

        Args:
            project_id: Unique project identifier
            project_path: Path to the project
            priority: Project priority
            estimated_mb: Estimated memory usage
        """
        self._loaded_projects[project_id] = ProjectCandidate(
            project_id=project_id,
            project_path=project_path,
            last_access=time.time(),
            priority=priority,
            estimated_mb=estimated_mb,
        )

    def unload_project(self, project_id: str) -> tuple[bool, float]:
        """Unload a project (simulated).

        Args:
            project_id: Project to unload

        Returns:
            Tuple of (success, memory_freed_mb)
        """
        if project_id not in self._loaded_projects:
            return False, 0.0

        candidate = self._loaded_projects[project_id]
        freed_mb = candidate.estimated_mb

        del self._loaded_projects[project_id]
        self._unload_log.append(project_id)

        logger.info(f"[MOCK] Unloaded project {project_id} (freed {freed_mb:.1f}MB)")

        return True, freed_mb

    def get_loaded_projects(self) -> List[ProjectCandidate]:
        """Get list of loaded projects.

        Returns:
            List of project candidates
        """
        # Update last access times to current time
        current_time = time.time()
        for candidate in self._loaded_projects.values():
            candidate.last_access = current_time

        return list(self._loaded_projects.values())


# =============================================================================
# Convenience Functions
# =============================================================================

# Global eviction manager instance
_global_manager: Optional[EvictionManager] = None
_global_manager_lock = Lock()


def get_global_manager() -> EvictionManager:
    """Get the global eviction manager instance.

    Returns:
        Global EvictionManager instance (creates if needed)
    """
    global _global_manager

    with _global_manager_lock:
        if _global_manager is None:
            _global_manager = EvictionManager()

        return _global_manager


def emergency_eviction(
    candidates: Optional[List[ProjectCandidate]] = None,
    target_mb: float = 512.0,
    max_projects: Optional[int] = None
) -> EvictionResult:
    """Perform emergency eviction using the global manager.

    Args:
        candidates: List of project candidates (uses unloader if None)
        target_mb: Target memory to free in MB
        max_projects: Maximum number of projects to evict

    Returns:
        EvictionResult with details of what was evicted
    """
    manager = get_global_manager()
    return manager.emergency_eviction(candidates, target_mb, max_projects)
