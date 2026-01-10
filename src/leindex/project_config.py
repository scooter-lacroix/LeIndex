"""
Project Configuration Overrides for LeIndex

This module provides per-project configuration overrides with a focus on memory
management settings. It allows projects to customize their memory allocation
hints and eviction priorities while maintaining validation against global limits.

Key Features:
- Per-project memory estimation overrides
- Priority-based eviction control
- YAML configuration in .leindex_data/config.yaml
- Deep merge with global defaults
- Validation against limits
- Warning when exceeding defaults

Example:
    # In .leindex_data/config.yaml:
    # memory:
    #   estimated_mb: 512
    #   priority: high
"""

import os
import logging
from dataclasses import dataclass, field
from typing import Dict, Any, Optional
from pathlib import Path
import yaml

from .config.global_config import GlobalConfigManager


logger = logging.getLogger(__name__)


@dataclass
class ProjectMemoryConfig:
    """Memory configuration override for a specific project.

    This configuration provides hints to the memory manager about how much
    memory a project needs and its priority for eviction decisions. These are
    hints, not reservations - the actual memory manager may allocate more or
    less based on global constraints.

    Attributes:
        estimated_mb: Estimated memory allocation in MB. Overrides the global
                     default (256MB). None means use global default.
                     Max allowed: 512MB to prevent one project from monopolizing memory.
        priority: Priority for eviction decisions. Higher priority projects are
                 less likely to be evicted. Options: "high", "normal", "low".
        max_override_mb: Maximum allowed override in MB. Validates estimated_mb
                        doesn't exceed this limit.

    Example:
        >>> config = ProjectMemoryConfig(estimated_mb=512, priority="high")
        >>> print(config.estimated_mb)
        512
    """

    estimated_mb: Optional[int] = None
    priority: str = "normal"
    max_override_mb: int = 512

    def __post_init__(self):
        """Validate configuration after initialization."""
        # Validate priority
        valid_priorities = ["high", "normal", "low"]
        if self.priority not in valid_priorities:
            raise ValueError(
                f"Invalid priority '{self.priority}'. "
                f"Must be one of: {', '.join(valid_priorities)}"
            )

        # Validate estimated_mb if provided
        if self.estimated_mb is not None:
            if self.estimated_mb < 0:
                raise ValueError(
                    f"estimated_mb must be non-negative, got {self.estimated_mb}"
                )
            if self.estimated_mb > self.max_override_mb:
                raise ValueError(
                    f"estimated_mb ({self.estimated_mb}) exceeds "
                    f"max_override_mb ({self.max_override_mb})"
                )

    def get_priority_score(self) -> float:
        """Get numeric priority score for eviction calculations.

        Higher scores mean higher priority (less likely to be evicted).

        Returns:
            float: Priority score (high=2.0, normal=1.0, low=0.5)
        """
        priority_scores = {
            "high": 2.0,
            "normal": 1.0,
            "low": 0.5,
        }
        return priority_scores.get(self.priority, 1.0)


@dataclass
class ProjectConfig:
    """Complete project configuration with all overrides.

    Attributes:
        memory: Memory configuration overrides
        _source_path: Path to the config file this was loaded from (for debugging)
    """

    memory: ProjectMemoryConfig = field(default_factory=ProjectMemoryConfig)
    _source_path: Optional[str] = None

    def to_dict(self) -> Dict[str, Any]:
        """Convert configuration to dictionary for serialization."""
        return {
            "memory": {
                "estimated_mb": self.memory.estimated_mb,
                "priority": self.memory.priority,
            }
        }


class ProjectConfigManager:
    """Manages per-project configuration overrides.

    This class handles loading, validating, and merging project-specific
    configuration with global defaults. Project configs are stored in
    .leindex_data/config.yaml in the project root.

    Example:
        >>> manager = ProjectConfigManager("/path/to/project")
        >>> config = manager.get_config()
        >>> print(config.memory.estimated_mb)
        512
    """

    CONFIG_FILENAME = "config.yaml"
    CONFIG_DIRNAME = ".leindex_data"

    def __init__(self, project_path: str):
        """Initialize the project config manager.

        Args:
            project_path: Absolute path to the project root directory
        """
        self.project_path = Path(project_path).resolve()
        self.config_path = self.project_path / self.CONFIG_DIRNAME / self.CONFIG_FILENAME
        self.global_config_manager = GlobalConfigManager()

        # Cache for loaded config
        self._config_cache: Optional[ProjectConfig] = None

    def get_config(self, force_reload: bool = False) -> ProjectConfig:
        """Get the project configuration, loading from file if needed.

        Args:
            force_reload: Force reload from file even if cached

        Returns:
            ProjectConfig: Complete project configuration with defaults applied
        """
        if self._config_cache is None or force_reload:
            self._config_cache = self._load_config()

        return self._config_cache

    def _load_config(self) -> ProjectConfig:
        """Load configuration from project config file.

        Returns:
            ProjectConfig: Loaded configuration with defaults applied
        """
        if not self.config_path.exists():
            # No project config, use defaults
            logger.debug(f"No project config at {self.config_path}, using defaults")
            return ProjectConfig(_source_path=None)

        try:
            with open(self.config_path, 'r', encoding='utf-8') as f:
                config_dict = yaml.safe_load(f)

            if config_dict is None:
                return ProjectConfig(_source_path=str(self.config_path))

            # Parse memory config
            memory_dict = config_dict.get('memory', {})
            memory_config = ProjectMemoryConfig(
                estimated_mb=memory_dict.get('estimated_mb'),
                priority=memory_dict.get('priority', 'normal'),
            )

            config = ProjectConfig(
                memory=memory_config,
                _source_path=str(self.config_path)
            )

            # Log warnings for exceeding defaults
            self._validate_and_warn(config)

            return config

        except (yaml.YAMLError, ValueError) as e:
            logger.error(f"Error loading project config from {self.config_path}: {e}")
            # Return defaults on error
            return ProjectConfig(_source_path=str(self.config_path))

    def _validate_and_warn(self, config: ProjectConfig) -> None:
        """Validate config and log warnings for concerning values.

        Args:
            config: Configuration to validate
        """
        # Get global defaults
        global_config = self.global_config_manager.get_config()
        default_mb = global_config.projects.estimated_mb

        # Warn if exceeding default
        if config.memory.estimated_mb is not None:
            if config.memory.estimated_mb > default_mb:
                ratio = config.memory.estimated_mb / default_mb
                logger.warning(
                    f"Project {self.project_path} has estimated_mb={config.memory.estimated_mb}MB, "
                    f"which is {ratio:.1f}x the global default ({default_mb}MB). "
                    f"This is a hint, not a reservation. Actual allocation may vary."
                )

            # Warn if approaching max
            if config.memory.estimated_mb > config.memory.max_override_mb * 0.9:
                logger.warning(
                    f"Project {self.project_path} estimated_mb ({config.memory.estimated_mb}MB) "
                    f"is approaching max_override_mb ({config.memory.max_override_mb}MB)"
                )

    def get_effective_memory_config(self) -> Dict[str, Any]:
        """Get effective memory configuration (project overrides + global defaults).

        This returns the complete memory configuration that should be used
        for this project, merging project-specific overrides with global defaults.

        Returns:
            dict: Effective memory configuration with keys:
                - estimated_mb: Memory estimate (project override or global default)
                - priority: Eviction priority (project override or global default)
                - priority_score: Numeric priority score for calculations
                - is_overridden: True if project overrode any defaults
        """
        project_config = self.get_config()
        global_config = self.global_config_manager.get_config()

        # Determine effective values
        effective_mb = project_config.memory.estimated_mb
        if effective_mb is None:
            effective_mb = global_config.projects.estimated_mb

        effective_priority = project_config.memory.priority

        return {
            "estimated_mb": effective_mb,
            "priority": effective_priority,
            "priority_score": project_config.memory.get_priority_score(),
            "is_overridden": project_config.memory.estimated_mb is not None,
            "max_override_mb": project_config.memory.max_override_mb,
        }

    def save_config(self, config: ProjectConfig) -> None:
        """Save project configuration to file.

        Args:
            config: Configuration to save

        Raises:
            IOError: If file cannot be written
            ValueError: If configuration is invalid
        """
        # Validate config
        try:
            # This will raise ValueError if invalid
            ProjectMemoryConfig(
                estimated_mb=config.memory.estimated_mb,
                priority=config.memory.priority,
            )
        except ValueError as e:
            raise ValueError(f"Invalid configuration: {e}")

        # Ensure config directory exists
        self.config_path.parent.mkdir(parents=True, exist_ok=True)

        # Save to file
        try:
            with open(self.config_path, 'w', encoding='utf-8') as f:
                f.write("# LeIndex Project Configuration\n")
                f.write("# This file contains per-project overrides for memory management\n")
                f.write("# Memory values are hints, not reservations\n")
                f.write("\n")
                yaml.dump(
                    config.to_dict(),
                    f,
                    default_flow_style=False,
                    sort_keys=False
                )

            # Update cache
            self._config_cache = config

            logger.info(f"Saved project config to {self.config_path}")

        except IOError as e:
            raise IOError(f"Failed to write config to {self.config_path}: {e}")

    def config_exists(self) -> bool:
        """Check if project config file exists.

        Returns:
            bool: True if config file exists, False otherwise
        """
        return self.config_path.exists()

    def delete_config(self) -> None:
        """Delete project configuration file.

        After deletion, the project will use global defaults.
        """
        if self.config_path.exists():
            self.config_path.unlink()
            self._config_cache = None
            logger.info(f"Deleted project config at {self.config_path}")


def load_project_config(project_path: str) -> ProjectConfig:
    """Convenience function to load project configuration.

    Args:
        project_path: Path to the project root

    Returns:
        ProjectConfig: Loaded project configuration

    Example:
        >>> config = load_project_config("/path/to/project")
        >>> print(config.memory.priority)
        'normal'
    """
    manager = ProjectConfigManager(project_path)
    return manager.get_config()


def get_effective_memory_config(project_path: str) -> Dict[str, Any]:
    """Convenience function to get effective memory configuration for a project.

    Args:
        project_path: Path to the project root

    Returns:
        dict: Effective memory configuration (see ProjectConfigManager.get_effective_memory_config)

    Example:
        >>> config = get_effective_memory_config("/path/to/project")
        >>> print(config['estimated_mb'])
        256
    """
    manager = ProjectConfigManager(project_path)
    return manager.get_effective_memory_config()
