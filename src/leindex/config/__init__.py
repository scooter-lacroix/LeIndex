"""
Configuration Management Package for LeIndex

This package provides hierarchical configuration management with YAML persistence,
validation, migration support, and secure file permissions.

Modules:
    global_config: Core configuration manager with dataclass structures
    migration: Configuration version migration support
    validation: Configuration validation rules and error handling
    setup: First-time setup and hardware detection
    reload: Zero-downtime configuration reload with signal handling

Example Usage:
    >>> from leindex.config import GlobalConfigManager
    >>> manager = GlobalConfigManager()
    >>> config = manager.get_config()
    >>> print(config.memory.total_budget_mb)
    3072

    >>> from leindex.config import first_time_setup
    >>> result = first_time_setup()
    >>> if result.success:
    ...     print("Setup complete!")
"""

from .global_config import (
    GlobalConfig,
    GlobalConfigManager,
    MemoryConfig,
    ProjectDefaultsConfig,
    PerformanceConfig,
)
from .validation import ConfigValidator, ValidationError
from .migration import ConfigMigration, ConfigMigrationError
from .setup import (
    first_time_setup,
    SetupResult,
    detect_hardware,
    is_first_run,
    get_setup_status,
)
from .reload import (
    ConfigReloadManager,
    ReloadResult,
    ReloadEvent,
    ConfigObserver,
    get_reload_manager,
    initialize_reload_manager,
    reload_config,
)

__all__ = [
    # Global Config
    'GlobalConfig',
    'GlobalConfigManager',
    'MemoryConfig',
    'ProjectDefaultsConfig',
    'PerformanceConfig',
    # Validation
    'ConfigValidator',
    'ValidationError',
    # Migration
    'ConfigMigration',
    'ConfigMigrationError',
    # Setup
    'first_time_setup',
    'SetupResult',
    'detect_hardware',
    'is_first_run',
    'get_setup_status',
    # Reload
    'ConfigReloadManager',
    'ReloadResult',
    'ReloadEvent',
    'ConfigObserver',
    'get_reload_manager',
    'initialize_reload_manager',
    'reload_config',
]
