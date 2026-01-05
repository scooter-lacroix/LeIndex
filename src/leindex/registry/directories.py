"""
Directory management utilities for the meta-registry system.

This module provides utilities for managing global and per-project directories
used by the code indexer's meta-registry feature.
"""

from pathlib import Path
from typing import Optional
import logging

logger = logging.getLogger(__name__)


# ============================================================================
# Directory Constants
# ============================================================================

"""Global directory name in user's home directory."""
GLOBAL_DIR_NAME = ".leindex"

"""Per-project directory name for registry data."""
PROJECT_DIR_NAME = ".leindex"


# ============================================================================
# Path Utilities
# ============================================================================

def get_global_registry_dir() -> Path:
    """
    Get the global registry directory path.

    The global directory is located at ~/.leindex/ and contains:
    - projects.db: SQLite registry database
    - Other global configuration and metadata

    Returns:
        Path object for the global registry directory

    Examples:
        >>> get_global_registry_dir()
        Path('/home/user/.leindex')
    """
    home_dir = Path.home()
    global_dir = home_dir / GLOBAL_DIR_NAME
    logger.debug(f"Global registry directory: {global_dir}")
    return global_dir


def get_project_registry_dir(project_path: str | Path) -> Path:
    """
    Get the registry directory for a specific project.

    The per-project directory is located at <project>/.leindex/
    and contains project-specific index data and metadata.

    Args:
        project_path: Absolute path to the project root

    Returns:
        Path object for the project registry directory

    Raises:
        ValueError: If project_path is empty or not an absolute path

    Examples:
        >>> get_project_registry_dir("/home/user/myproject")
        Path('/home/user/myproject/.leindex')
    """
    if not project_path:
        raise ValueError("project_path cannot be empty")

    project_path_obj = Path(project_path)

    if not project_path_obj.is_absolute():
        raise ValueError(
            f"project_path must be an absolute path, got: {project_path}"
        )

    project_dir = project_path_obj / PROJECT_DIR_NAME
    logger.debug(f"Project registry directory for {project_path}: {project_dir}")
    return project_dir


def get_project_index_dir(project_path: str | Path, index_name: Optional[str] = None) -> Path:
    """
    Get the index directory for a specific project.

    Args:
        project_path: Absolute path to the project root
        index_name: Optional name of the index (defaults to 'default')

    Returns:
        Path object for the project index directory

    Raises:
        ValueError: If project_path is empty or not an absolute path

    Examples:
        >>> get_project_index_dir("/home/user/myproject")
        Path('/home/user/myproject/.leindex/index')

        >>> get_project_index_dir("/home/user/myproject", "semantic")
        Path('/home/user/myproject/.leindex/index/semantic')
    """
    project_dir = get_project_registry_dir(project_path)
    index_dir = project_dir / "index"

    if index_name:
        index_dir = index_dir / index_name

    logger.debug(f"Project index directory: {index_dir}")
    return index_dir


# ============================================================================
# Directory Creation Utilities
# ============================================================================

def ensure_directories(
    project_path: Optional[str | Path] = None,
    create_global: bool = True,
    create_project: bool = False,
) -> dict[str, Path]:
    """
    Ensure that required directories exist, creating them if necessary.

    Args:
        project_path: Optional project path. Required if create_project is True
        create_global: Whether to ensure global directory exists
        create_project: Whether to ensure project directory exists

    Returns:
        Dictionary with keys 'global' and/or 'project' containing created Paths

    Raises:
        ValueError: If create_project is True but project_path is not provided
        OSError: If directory creation fails

    Examples:
        >>> ensure_directories()
        {'global': Path('/home/user/.leindex')}

        >>> ensure_directories(
        ...     project_path="/home/user/myproject",
        ...     create_global=True,
        ...     create_project=True
        ... )
        {
            'global': Path('/home/user/.leindex'),
            'project': Path('/home/user/myproject/.leindex')
        }
    """
    result = {}

    if create_global:
        global_dir = get_global_registry_dir()
        _ensure_directory(global_dir)
        result["global"] = global_dir
        logger.info(f"Ensured global directory exists: {global_dir}")

    if create_project:
        if not project_path:
            raise ValueError(
                "project_path must be provided when create_project is True"
            )

        project_dir = get_project_registry_dir(project_path)
        _ensure_directory(project_dir)
        result["project"] = project_dir
        logger.info(f"Ensured project directory exists: {project_dir}")

    return result


def _ensure_directory(directory: Path) -> None:
    """
    Ensure a directory exists, creating it if necessary.

    Args:
        directory: Path object for the directory

    Raises:
        OSError: If directory creation fails
    """
    try:
        directory.mkdir(parents=True, exist_ok=True)
        logger.debug(f"Directory ensured: {directory}")
    except OSError as e:
        logger.error(f"Failed to create directory {directory}: {e}")
        raise


def get_registry_db_path() -> Path:
    """
    Get the path to the registry database file.

    Returns:
        Path object for the registry database

    Examples:
        >>> get_registry_db_path()
        Path('/home/user/.leindex/projects.db')
    """
    global_dir = get_global_registry_dir()
    db_path = global_dir / "projects.db"
    logger.debug(f"Registry database path: {db_path}")
    return db_path


def is_indexed_project(project_path: str | Path) -> bool:
    """
    Check if a project has been indexed (has a .leindex directory).

    Args:
        project_path: Path to the project

    Returns:
        True if the project has been indexed, False otherwise
    """
    try:
        project_dir = get_project_registry_dir(project_path)
        exists = project_dir.exists() and project_dir.is_dir()
        logger.debug(f"Project {project_path} indexed: {exists}")
        return exists
    except (ValueError, OSError):
        logger.debug(f"Project {project_path} not indexed (error checking)")
        return False
