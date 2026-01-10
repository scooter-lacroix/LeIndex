"""
Global Configuration Management for LeIndex

This module provides hierarchical configuration management with YAML persistence,
validation, migration support, and secure file permissions.

Key Features:
- YAML-based configuration with comments
- Deep merge of user config with defaults
- Auto-creation of config file on first run
- Secure file permissions (0o600)
- Validation against min/max limits
- Version tracking for migrations
"""

import os
import copy
from dataclasses import dataclass, field
from typing import Dict, Any, Optional
import yaml

from .validation import ConfigValidator, ValidationError
from .migration import ConfigMigration


@dataclass
class MemoryConfig:
    """Memory management configuration.

    Attributes:
        total_budget_mb: Total memory budget in MB (default: 3072 = 3GB)
        global_index_mb: Global index memory allocation in MB (default: 512)
        warning_threshold_percent: Warning threshold at 80% (default: 80)
        prompt_threshold_percent: LLM prompt threshold at 93% (default: 93)
        emergency_threshold_percent: Emergency eviction threshold at 98% (default: 98)
    """

    total_budget_mb: int = 3072
    global_index_mb: int = 512
    warning_threshold_percent: int = 80
    prompt_threshold_percent: int = 93
    emergency_threshold_percent: int = 98


@dataclass
class ProjectDefaultsConfig:
    """Default configuration for projects.

    Attributes:
        estimated_mb: Estimated memory per project in MB (default: 256)
        priority: Default project priority (high/normal/low, default: normal)
        max_file_size: Maximum file size for indexing in bytes (default: 5MB)
    """

    estimated_mb: int = 256
    priority: str = "normal"
    max_file_size: int = 5242880  # 5MB


@dataclass
class PerformanceConfig:
    """Performance tuning configuration.

    Attributes:
        cache_enabled: Enable query result caching (default: True)
        cache_ttl_seconds: Cache TTL in seconds (default: 300)
        parallel_workers: Number of parallel workers (default: 4)
        batch_size: Batch size for operations (default: 50)
    """

    cache_enabled: bool = True
    cache_ttl_seconds: int = 300
    parallel_workers: int = 4
    batch_size: int = 50


@dataclass
class GlobalConfig:
    """Global configuration structure for LeIndex.

    This dataclass represents the complete configuration structure with defaults.

    Attributes:
        version: Configuration schema version (for migrations)
        memory: Memory management settings
        projects: Default project settings
        performance: Performance tuning settings
    """

    version: str = "2.0"
    memory: MemoryConfig = field(default_factory=MemoryConfig)
    projects: ProjectDefaultsConfig = field(default_factory=ProjectDefaultsConfig)
    performance: PerformanceConfig = field(default_factory=PerformanceConfig)


class GlobalConfigManager:
    """Manages global configuration with validation, persistence, and migration.

    This class handles:
    - Loading configuration from YAML file
    - Saving configuration with proper permissions
    - Validation of configuration values
    - Migration from older config versions
    - Deep merge with defaults

    Example:
        >>> manager = GlobalConfigManager()
        >>> config = manager.get_config()
        >>> print(config.memory.total_budget_mb)
        3072
    """

    DEFAULT_CONFIG_PATH = "~/.leindex/mcp_config.yaml"
    CURRENT_VERSION = "2.0"

    def __init__(self, config_path: Optional[str] = None):
        """Initialize the configuration manager.

        Args:
            config_path: Path to config file. Defaults to ~/.leindex/mcp_config.yaml
        """
        if config_path is None:
            config_path = self.DEFAULT_CONFIG_PATH

        self.config_path = os.path.expanduser(config_path)
        self.validator = ConfigValidator()
        self.migrator = ConfigMigration()
        self._config_cache: Optional[GlobalConfig] = None
        self._ensure_config_directory()

    def _ensure_config_directory(self) -> None:
        """Ensure the configuration directory exists with proper permissions."""
        config_dir = os.path.dirname(self.config_path)

        if config_dir and not os.path.exists(config_dir):
            os.makedirs(config_dir, mode=0o700, exist_ok=True)

    def get_config(self, force_reload: bool = False) -> GlobalConfig:
        """Get the current configuration, loading from file if needed.

        Args:
            force_reload: Force reload from file even if cached

        Returns:
            Current configuration as GlobalConfig dataclass
        """
        if self._config_cache is None or force_reload:
            self._config_cache = self.load_config()

        return self._config_cache

    def load_config(self) -> GlobalConfig:
        """Load configuration from YAML file with validation and migration.

        This method:
        1. Checks if config file exists
        2. Loads and parses YAML
        3. Detects version and migrates if needed
        4. Validates configuration
        5. Deep merges with defaults

        Returns:
            Validated configuration as GlobalConfig dataclass
        """
        if not os.path.exists(self.config_path):
            # First run - create default config
            return self._create_default_config()

        try:
            with open(self.config_path, 'r', encoding='utf-8') as f:
                config_dict = yaml.safe_load(f)

            if config_dict is None:
                return self._create_default_config()

            # Check version and migrate if needed
            config_version = config_dict.get('version', '1.0')
            if config_version != self.CURRENT_VERSION:
                config_dict = self.migrator.migrate_config(
                    config_dict,
                    config_version,
                    self.CURRENT_VERSION
                )

            # Validate configuration
            self.validator.validate_config(config_dict)

            # Deep merge with defaults and create dataclass
            default_dict = self.get_default_config_dict()
            merged_dict = self._deep_merge(default_dict, config_dict)

            return self._dict_to_dataclass(merged_dict)

        except yaml.YAMLError as e:
            raise ValidationError(f"Failed to parse YAML config: {e}")
        except Exception as e:
            raise ValidationError(f"Failed to load config: {e}")

    def save_config(self, config: GlobalConfig) -> None:
        """Save configuration to YAML file with secure permissions.

        Args:
            config: Configuration to save

        Raises:
            ValidationError: If configuration is invalid
            IOError: If file cannot be written
        """
        # Validate before saving
        config_dict = self._dataclass_to_dict(config)
        self.validator.validate_config(config_dict)

        # Ensure directory exists
        self._ensure_config_directory()

        # Save with comments
        try:
            with open(self.config_path, 'w', encoding='utf-8') as f:
                f.write("# LeIndex Global Configuration\n")
                f.write("# This file controls memory management, performance, and defaults\n")
                f.write("# See https://github.com/scooter-lacroix/leindex/docs/MEMORY_MANAGEMENT.md\n")
                f.write("\n")

                yaml.dump(
                    config_dict,
                    f,
                    default_flow_style=False,
                    sort_keys=False,
                    allow_unicode=True
                )

            # Set secure permissions (owner read/write only)
            os.chmod(self.config_path, 0o600)

            # Update cache
            self._config_cache = config

        except IOError as e:
            raise ValidationError(f"Failed to write config file: {e}")

    def get_default_config_dict(self) -> Dict[str, Any]:
        """Get default configuration as a dictionary.

        Returns:
            Default configuration as nested dict
        """
        default_config = GlobalConfig()
        return self._dataclass_to_dict(default_config)

    def _create_default_config(self) -> GlobalConfig:
        """Create default configuration and save to file.

        This is called on first run to create a new config file with comments.

        Returns:
            Default configuration
        """
        default_config = GlobalConfig()
        self.save_config(default_config)
        return default_config

    def _deep_merge(
        self,
        base: Dict[str, Any],
        override: Dict[str, Any]
    ) -> Dict[str, Any]:
        """Deep merge override dict into base dict.

        Args:
            base: Base dictionary (defaults)
            override: Override dictionary (user config)

        Returns:
            Merged dictionary
        """
        result = copy.deepcopy(base)

        for key, value in override.items():
            if key in result and isinstance(result[key], dict) and isinstance(value, dict):
                result[key] = self._deep_merge(result[key], value)
            else:
                result[key] = value

        return result

    def _dataclass_to_dict(self, config: GlobalConfig) -> Dict[str, Any]:
        """Convert GlobalConfig dataclass to dictionary.

        Args:
            config: Configuration dataclass

        Returns:
            Configuration as nested dict
        """
        return {
            'version': config.version,
            'memory': {
                'total_budget_mb': config.memory.total_budget_mb,
                'global_index_mb': config.memory.global_index_mb,
                'warning_threshold_percent': config.memory.warning_threshold_percent,
                'prompt_threshold_percent': config.memory.prompt_threshold_percent,
                'emergency_threshold_percent': config.memory.emergency_threshold_percent,
            },
            'projects': {
                'estimated_mb': config.projects.estimated_mb,
                'priority': config.projects.priority,
                'max_file_size': config.projects.max_file_size,
            },
            'performance': {
                'cache_enabled': config.performance.cache_enabled,
                'cache_ttl_seconds': config.performance.cache_ttl_seconds,
                'parallel_workers': config.performance.parallel_workers,
                'batch_size': config.performance.batch_size,
            }
        }

    def _dict_to_dataclass(self, config_dict: Dict[str, Any]) -> GlobalConfig:
        """Convert dictionary to GlobalConfig dataclass.

        Args:
            config_dict: Configuration as nested dict

        Returns:
            Configuration as GlobalConfig dataclass
        """
        memory_dict = config_dict.get('memory', {})
        projects_dict = config_dict.get('projects', {})
        performance_dict = config_dict.get('performance', {})

        return GlobalConfig(
            version=config_dict.get('version', self.CURRENT_VERSION),
            memory=MemoryConfig(
                total_budget_mb=memory_dict.get('total_budget_mb', 3072),
                global_index_mb=memory_dict.get('global_index_mb', 512),
                warning_threshold_percent=memory_dict.get('warning_threshold_percent', 80),
                prompt_threshold_percent=memory_dict.get('prompt_threshold_percent', 93),
                emergency_threshold_percent=memory_dict.get('emergency_threshold_percent', 98),
            ),
            projects=ProjectDefaultsConfig(
                estimated_mb=projects_dict.get('estimated_mb', 256),
                priority=projects_dict.get('priority', 'normal'),
                max_file_size=projects_dict.get('max_file_size', 5242880),
            ),
            performance=PerformanceConfig(
                cache_enabled=performance_dict.get('cache_enabled', True),
                cache_ttl_seconds=performance_dict.get('cache_ttl_seconds', 300),
                parallel_workers=performance_dict.get('parallel_workers', 4),
                batch_size=performance_dict.get('batch_size', 50),
            )
        )

    def reload(self) -> GlobalConfig:
        """Reload configuration from file.

        Returns:
            Reloaded configuration
        """
        return self.load_config()

    def config_exists(self) -> bool:
        """Check if configuration file exists.

        Returns:
            True if config file exists, False otherwise
        """
        return os.path.exists(self.config_path)

    def to_dict_persistent(self, config: GlobalConfig) -> Dict[str, Any]:
        """Convert GlobalConfig dataclass to dictionary for persistent storage.

        This is a public wrapper around the private _dataclass_to_dict method,
        providing controlled access for external components that need to serialize
        configuration data.

        Args:
            config: Configuration dataclass to convert

        Returns:
            Configuration as nested dict suitable for YAML serialization

        Example:
            >>> manager = GlobalConfigManager()
            >>> config = manager.get_config()
            >>> config_dict = manager.to_dict_persistent(config)
            >>> print(config_dict['memory']['total_budget_mb'])
            3072
        """
        return self._dataclass_to_dict(config)

    def update_config_cache(self, new_config: GlobalConfig) -> None:
        """Update the configuration cache atomically.

        This method provides a public interface for updating the cached configuration
        in a thread-safe manner. It's used during config reload operations to ensure
        atomic updates without exposing the internal _config_cache attribute directly.

        Args:
            new_config: The new configuration to cache

        Example:
            >>> manager = GlobalConfigManager()
            >>> new_config = GlobalConfig(memory.total_budget_mb=4096)
            >>> manager.update_config_cache(new_config)
        """
        self._config_cache = new_config
