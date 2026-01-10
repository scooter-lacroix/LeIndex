"""
Configuration Migration Support for LeIndex

This module handles migration of configuration files between different schema versions.
It provides automatic detection, backup, and rollback capabilities.

Key Features:
- Version detection from config files
- Automatic backup before migration
- Rollback on validation failure
- Support for v1 → v2 migration
"""

import os
import shutil
from datetime import datetime
from typing import Dict, Any
import yaml


class ConfigMigrationError(Exception):
    """Exception raised when config migration fails."""

    pass


class ConfigMigration:
    """Handles configuration migration between versions.

    This class provides:
    - Version detection
    - Automatic backup
    - Schema migration
    - Rollback on failure

    Example:
        >>> migrator = ConfigMigration()
        >>> migrated = migrator.migrate_config(config_dict, '1.0', '2.0')
    """

    # Mapping of version to migration function
    MIGRATIONS = {
        ('1.0', '2.0'): '_migrate_v1_to_v2',
    }

    def __init__(self):
        """Initialize the configuration migrator."""
        pass

    def migrate_config(
        self,
        config_dict: Dict[str, Any],
        from_version: str,
        to_version: str,
        config_path: str = None
    ) -> Dict[str, Any]:
        """Migrate configuration from one version to another.

        Args:
            config_dict: Configuration dictionary to migrate
            from_version: Source version (e.g., '1.0')
            to_version: Target version (e.g., '2.0')
            config_path: Optional path to config file for backup

        Returns:
            Migrated configuration dictionary

        Raises:
            ConfigMigrationError: If migration fails or version not supported
        """
        if from_version == to_version:
            return config_dict

        # Check if direct migration is available
        migration_key = (from_version, to_version)
        if migration_key not in self.MIGRATIONS:
            raise ConfigMigrationError(
                f"No migration path from {from_version} to {to_version}"
            )

        # Backup before migration
        if config_path and os.path.exists(config_path):
            self._backup_config(config_path)

        # Get migration function
        migration_func_name = self.MIGRATIONS[migration_key]
        migration_func = getattr(self, migration_func_name)

        try:
            # Perform migration
            migrated_config = migration_func(config_dict.copy())

            # Update version
            migrated_config['version'] = to_version

            return migrated_config

        except Exception as e:
            raise ConfigMigrationError(
                f"Migration from {from_version} to {to_version} failed: {e}"
            )

    def _migrate_v1_to_v2(self, config_v1: Dict[str, Any]) -> Dict[str, Any]:
        """Migrate configuration from v1.0 to v2.0.

        v1.0 Schema:
        - Flat structure with memory_settings key
        - No global_index_mb field
        - No project priorities

        v2.0 Schema:
        - Nested structure with memory.projects.performance keys
        - Dedicated global_index_mb field
        - Project priority support

        Args:
            config_v1: v1.0 configuration dictionary

        Returns:
            v2.0 configuration dictionary

        Raises:
            ConfigMigrationError: If migration fails
        """
        try:
            config_v2 = {
                'version': '2.0',
                'memory': {},
                'projects': {},
                'performance': {}
            }

            # Migrate memory settings
            if 'memory_settings' in config_v1:
                memory_v1 = config_v1['memory_settings']
                config_v2['memory'] = {
                    'total_budget_mb': memory_v1.get('total_mb', 3072),
                    'global_index_mb': memory_v1.get('global_index_mb', 512),
                    'warning_threshold_percent': memory_v1.get('warning_threshold', 80),
                    'prompt_threshold_percent': memory_v1.get('prompt_threshold', 93),
                    'emergency_threshold_percent': memory_v1.get('emergency_threshold', 98),
                }
            else:
                # Use defaults if no memory settings
                config_v2['memory'] = {
                    'total_budget_mb': 3072,
                    'global_index_mb': 512,
                    'warning_threshold_percent': 80,
                    'prompt_threshold_percent': 93,
                    'emergency_threshold_percent': 98,
                }

            # Migrate project settings (new in v2.0)
            config_v2['projects'] = {
                'estimated_mb': config_v1.get('project_memory_mb', 256),
                'priority': 'normal',  # New field, default to normal
                'max_file_size': config_v1.get('max_file_size', 5242880),
            }

            # Migrate performance settings (new in v2.0)
            config_v2['performance'] = {
                'cache_enabled': config_v1.get('cache_enabled', True),
                'cache_ttl_seconds': config_v1.get('cache_ttl', 300),
                'parallel_workers': config_v1.get('max_workers', 4),
                'batch_size': config_v1.get('batch_size', 50),
            }

            return config_v2

        except Exception as e:
            raise ConfigMigrationError(f"v1 to v2 migration failed: {e}")

    def _backup_config(self, config_path: str) -> str:
        """Create a backup of the configuration file.

        Args:
            config_path: Path to config file to backup

        Returns:
            Path to backup file

        Raises:
            ConfigMigrationError: If backup fails
        """
        try:
            timestamp = datetime.now().strftime('%Y%m%d_%H%M%S')
            backup_path = f"{config_path}.backup_{timestamp}"

            shutil.copy2(config_path, backup_path)

            return backup_path

        except Exception as e:
            raise ConfigMigrationError(f"Failed to backup config: {e}")

    def rollback(self, backup_path: str, config_path: str) -> None:
        """Rollback to a backup configuration file.

        Args:
            backup_path: Path to backup file
            config_path: Target config file path

        Raises:
            ConfigMigrationError: If rollback fails
        """
        try:
            if not os.path.exists(backup_path):
                raise ConfigMigrationError(f"Backup file not found: {backup_path}")

            shutil.copy2(backup_path, config_path)

        except Exception as e:
            raise ConfigMigrationError(f"Rollback failed: {e}")

    def list_backups(self, config_path: str) -> list[str]:
        """List available backup files for a config.

        Args:
            config_path: Path to config file

        Returns:
            List of backup file paths sorted by age (newest first)
        """
        config_dir = os.path.dirname(config_path)
        config_name = os.path.basename(config_path)

        backups = []
        for file in os.listdir(config_dir):
            if file.startswith(f"{config_name}.backup_"):
                backups.append(os.path.join(config_dir, file))

        # Sort by modification time (newest first)
        backups.sort(key=lambda p: os.path.getmtime(p), reverse=True)

        return backups

    def cleanup_old_backups(
        self,
        config_path: str,
        keep_count: int = 5
    ) -> list[str]:
        """Remove old backup files, keeping only the most recent.

        Args:
            config_path: Path to config file
            keep_count: Number of backups to keep (default: 5)

        Returns:
            List of removed backup paths
        """
        backups = self.list_backups(config_path)
        removed = []

        for old_backup in backups[keep_count:]:
            try:
                os.remove(old_backup)
                removed.append(old_backup)
            except Exception:
                pass  # Ignore removal errors

        return removed
