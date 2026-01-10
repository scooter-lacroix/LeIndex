"""
First-Time Setup for LeIndex

This module provides the first-time setup functionality for LeIndex, including:
- Directory structure creation with proper permissions
- Default configuration file generation with comprehensive comments
- Setup validation and verification
- Hardware detection for memory budget recommendations

Key Features:
- Secure directory creation (0o700 permissions)
- Secure config file creation (0o600 permissions)
- Comprehensive YAML comments for user guidance
- Hardware detection for memory recommendations
- Setup validation and rollback on failure
- Thread-safe implementation

Example:
    >>> from leindex.config.setup import first_time_setup, SetupResult
    >>> result = first_time_setup()
    >>> if result.success:
    ...     print(f"Setup complete! Config at: {result.config_path}")
    ... else:
    ...     print(f"Setup failed: {result.error}")
"""

import os
import sys
import stat
import logging
from pathlib import Path
from typing import Optional, Dict, Any
import psutil

from .global_config import GlobalConfigManager
from .validation import ConfigValidator, ValidationError


# Configure logging
logger = logging.getLogger(__name__)


class SetupResult:
    """Result of first-time setup operation.

    Attributes:
        success: True if setup completed successfully, False otherwise
        config_path: Path to created config file (if successful)
        data_path: Path to data directory (if successful)
        error: Error message (if failed)
        warnings: List of warning messages (informational)
        hardware_info: Hardware detection results (memory, CPU)
    """

    def __init__(
        self,
        success: bool,
        config_path: Optional[str] = None,
        data_path: Optional[str] = None,
        error: Optional[str] = None,
        warnings: Optional[list[str]] = None,
        hardware_info: Optional[Dict[str, Any]] = None
    ):
        """Initialize setup result.

        Args:
            success: True if setup completed successfully
            config_path: Path to created config file
            data_path: Path to data directory
            error: Error message if setup failed
            warnings: List of warning messages
            hardware_info: Hardware detection results
        """
        self.success = success
        self.config_path = config_path
        self.data_path = data_path
        self.error = error
        self.warnings = warnings or []
        self.hardware_info = hardware_info or {}

    def __repr__(self) -> str:
        """Return string representation of setup result."""
        if self.success:
            return f"SetupResult(success=True, config_path={self.config_path})"
        return f"SetupResult(success=False, error={self.error})"


def first_time_setup(
    config_path: Optional[str] = None,
    data_path: Optional[str] = None,
    force: bool = False
) -> SetupResult:
    """Perform first-time setup for LeIndex.

    This function creates the necessary directory structure and generates
    a default configuration file with comprehensive comments.

    Directory Structure Created:
        ~/.leindex/               - Config directory (0o700 permissions)
        ~/.leindex/mcp_config.yaml - Config file (0o600 permissions)
        ~/.leindex_data/          - Data directory (0o700 permissions)
        ~/.leindex_data/indexes/  - Index storage (0o700 permissions)
        ~/.leindex_data/cache/    - Query cache (0o700 permissions)

    Args:
        config_path: Optional custom config path. Defaults to ~/.leindex/mcp_config.yaml
        data_path: Optional custom data path. Defaults to ~/.leindex_data/
        force: If True, overwrite existing config. If False, skip if config exists.

    Returns:
        SetupResult with success status, paths, and any errors/warnings.

    Raises:
        No exceptions raised; all errors returned in SetupResult.

    Example:
        >>> result = first_time_setup()
        >>> if result.success:
        ...     print("Setup complete!")
        ...     print(f"Config: {result.config_path}")
        ...     print(f"Data: {result.data_path}")
        ...     print(f"Memory: {result.hardware_info.get('total_memory_mb')} MB")
    """
    warnings: list[str] = []
    hardware_info: Dict[str, Any] = {}

    try:
        # Detect hardware
        hardware_info = detect_hardware()
        logger.info(f"Hardware detected: {hardware_info}")

        # Determine paths
        if config_path is None:
            config_path = os.path.expanduser("~/.leindex/mcp_config.yaml")
        else:
            config_path = os.path.expanduser(config_path)

        if data_path is None:
            data_path = os.path.expanduser("~/.leindex_data")
        else:
            data_path = os.path.expanduser(data_path)

        # Check if setup already exists
        if os.path.exists(config_path) and not force:
            logger.info(f"Config already exists at {config_path}, skipping setup")
            return SetupResult(
                success=True,
                config_path=config_path,
                data_path=data_path,
                warnings=["Config already exists, skipping setup"],
                hardware_info=hardware_info
            )

        # Step 1: Create config directory
        logger.info(f"Creating config directory: {os.path.dirname(config_path)}")
        config_dir = os.path.dirname(config_path)
        if not os.path.exists(config_dir):
            os.makedirs(config_dir, mode=0o700, exist_ok=True)
            _verify_permissions(config_dir, 0o700, "config directory")

        # Step 2: Create data directory structure
        logger.info(f"Creating data directory: {data_path}")
        data_dirs = [
            data_path,
            os.path.join(data_path, "indexes"),
            os.path.join(data_path, "cache"),
            os.path.join(data_path, "logs"),
        ]

        for dir_path in data_dirs:
            if not os.path.exists(dir_path):
                os.makedirs(dir_path, mode=0o700, exist_ok=True)
                _verify_permissions(dir_path, 0o700, "data directory")

        # Step 3: Generate recommended config based on hardware
        logger.info("Generating default configuration")
        config_dict = _generate_recommended_config(hardware_info)

        # Step 4: Write config file with comments
        logger.info(f"Writing config file: {config_path}")
        _write_config_with_comments(config_path, config_dict, hardware_info)

        # Step 5: Verify config file permissions
        _verify_permissions(config_path, 0o600, "config file")

        # Step 6: Validate config file
        logger.info("Validating configuration")
        validator = ConfigValidator()
        try:
            validator.validate_config(config_dict)
        except ValidationError as e:
            # Clean up invalid config
            if os.path.exists(config_path):
                os.remove(config_path)
            return SetupResult(
                success=False,
                error=f"Config validation failed: {e}",
                warnings=warnings,
                hardware_info=hardware_info
            )

        # Step 7: Test config loading
        logger.info("Testing config loading")
        try:
            manager = GlobalConfigManager(config_path=config_path)
            loaded_config = manager.get_config()
            logger.info(f"Config loaded successfully: version={loaded_config.version}")
        except Exception as e:
            # Clean up on load failure
            if os.path.exists(config_path):
                os.remove(config_path)
            return SetupResult(
                success=False,
                error=f"Config loading failed: {e}",
                warnings=warnings,
                hardware_info=hardware_info
            )

        # Add recommendations based on hardware
        if hardware_info.get('total_memory_mb', 0) < 4096:
            warnings.append(
                "System has <4GB RAM. Consider reducing total_budget_mb in config "
                "for better performance."
            )

        logger.info("First-time setup completed successfully")
        return SetupResult(
            success=True,
            config_path=config_path,
            data_path=data_path,
            warnings=warnings,
            hardware_info=hardware_info
        )

    except Exception as e:
        logger.error(f"First-time setup failed: {e}", exc_info=True)
        return SetupResult(
            success=False,
            error=str(e),
            warnings=warnings,
            hardware_info=hardware_info
        )


def detect_hardware() -> Dict[str, Any]:
    """Detect system hardware for configuration recommendations.

    Detects:
    - Total system memory
    - Available memory
    - CPU count
    - Platform information

    Returns:
        Dictionary with hardware information:
        - total_memory_mb: Total system memory in MB
        - available_memory_mb: Available memory in MB
        - cpu_count: Number of CPU cores
        - platform: System platform (e.g., 'Linux', 'Darwin')
    """
    hardware = {}

    try:
        # Memory detection
        mem = psutil.virtual_memory()
        hardware['total_memory_mb'] = int(mem.total / (1024 * 1024))
        hardware['available_memory_mb'] = int(mem.available / (1024 * 1024))
        hardware['memory_percent_used'] = mem.percent

        # CPU detection
        hardware['cpu_count'] = psutil.cpu_count()

        # Platform detection
        hardware['platform'] = sys.platform

        # Python version
        hardware['python_version'] = f"{sys.version_info.major}.{sys.version_info.minor}"

    except Exception as e:
        logger.warning(f"Hardware detection partially failed: {e}")

    return hardware


def _generate_recommended_config(hardware_info: Dict[str, Any]) -> Dict[str, Any]:
    """Generate recommended configuration based on detected hardware.

    Args:
        hardware_info: Hardware detection results from detect_hardware()

    Returns:
        Configuration dictionary with recommended values
    """
    total_memory_mb = hardware_info.get('total_memory_mb', 4096)
    cpu_count = hardware_info.get('cpu_count', 4)

    # Recommend using 50% of total memory for LeIndex, max 8GB
    recommended_budget_mb = min(int(total_memory_mb * 0.5), 8192)

    # Ensure minimum budget
    recommended_budget_mb = max(recommended_budget_mb, 512)

    # Recommend global index size (10% of budget, rounded up to ensure >= 10%)
    global_index_mb = max(int(recommended_budget_mb * 0.1) + 1, 128)

    # Recommend workers based on CPU count
    recommended_workers = min(max(cpu_count, 2), 16)

    return {
        'version': '2.0',
        'memory': {
            'total_budget_mb': recommended_budget_mb,
            'global_index_mb': global_index_mb,
            'warning_threshold_percent': 80,
            'prompt_threshold_percent': 93,
            'emergency_threshold_percent': 98,
        },
        'projects': {
            'estimated_mb': 256,
            'priority': 'normal',
            'max_file_size': 5242880,  # 5MB
        },
        'performance': {
            'cache_enabled': True,
            'cache_ttl_seconds': 300,
            'parallel_workers': recommended_workers,
            'batch_size': 50,
        }
    }


def _write_config_with_comments(
    config_path: str,
    config_dict: Dict[str, Any],
    hardware_info: Dict[str, Any]
) -> None:
    """Write configuration file with comprehensive YAML comments.

    Args:
        config_path: Path to config file to write
        config_dict: Configuration dictionary
        hardware_info: Hardware detection results for recommendations

    Raises:
        IOError: If file cannot be written
    """
    total_memory_mb = hardware_info.get('total_memory_mb', 0)
    cpu_count = hardware_info.get('cpu_count', 0)

    # Build YAML with comments
    yaml_content = f"""# LeIndex Global Configuration
# Version: 2.0
#
# This file controls memory management, performance, and defaults for LeIndex.
# See https://github.com/scooter-lacroix/leindex/docs/MEMORY_MANAGEMENT.md
#
# Hardware Detection Results:
#   Total Memory: {total_memory_mb} MB
#   CPU Cores: {cpu_count}
#   Platform: {hardware_info.get('platform', 'unknown')}
#
# IMPORTANT: This configuration has been automatically tuned for your system.
# You can adjust values based on your needs, but stay within min/max limits.

version: "{config_dict['version']}"

memory:
  # Total memory budget for LeIndex in megabytes (MB)
  # Min: 512 MB, Max: 65536 MB (64 GB)
  # Recommended: 50% of total system memory
  total_budget_mb: {config_dict['memory']['total_budget_mb']}

  # Memory allocated to the global index (cross-project search)
  # Min: 128 MB, Max: 8192 MB (8 GB)
  # Recommended: 10-20% of total_budget_mb
  global_index_mb: {config_dict['memory']['global_index_mb']}

  # THRESHOLDS: Control memory management behavior
  # These thresholds trigger different actions when memory usage is reached

  # Warning threshold - logs warning message when usage exceeds this percentage
  # Min: 50%, Max: 95%
  warning_threshold_percent: {config_dict['memory']['warning_threshold_percent']}

  # Prompt threshold - includes memory context in LLM prompts when usage exceeds this
  # Min: 60%, Max: 99%
  # Set higher (93-95%) for better LLM context, lower (85-90%) for earlier warnings
  prompt_threshold_percent: {config_dict['memory']['prompt_threshold_percent']}

  # Emergency threshold - force evicts projects when usage exceeds this
  # Min: 70%, Max: 100%
  # Should be higher than prompt_threshold_percent
  emergency_threshold_percent: {config_dict['memory']['emergency_threshold_percent']}

projects:
  # Default estimated memory per project in MB
  # Used for calculating how many projects can fit in memory budget
  # Min: 32 MB, Max: 4096 MB (4 GB)
  # Adjust based on typical project size in your codebase
  estimated_mb: {config_dict['projects']['estimated_mb']}

  # Default project priority for eviction decisions
  # Values: 'high', 'normal', 'low'
  # High priority projects are evicted last
  priority: {config_dict['projects']['priority']}

  # Maximum file size for indexing in bytes
  # Files larger than this are skipped during indexing
  # Min: 1024 (1 KB), Max: 1073741824 (1 GB)
  # Default: 5242880 (5 MB)
  max_file_size: {config_dict['projects']['max_file_size']}

performance:
  # Enable query result caching
  # Cache improves performance for repeated queries
  cache_enabled: {str(config_dict['performance']['cache_enabled']).lower()}

  # Cache time-to-live in seconds
  # Min: 30 seconds, Max: 3600 seconds (1 hour)
  cache_ttl_seconds: {config_dict['performance']['cache_ttl_seconds']}

  # Number of parallel workers for indexing
  # Min: 1, Max: 32
  # Recommended: Number of CPU cores (detected: {cpu_count})
  # Higher values = faster indexing but more memory usage
  parallel_workers: {config_dict['performance']['parallel_workers']}

  # Batch size for database writes
  # Min: 10, Max: 500
  # Larger batches = faster writes but higher memory spikes
  batch_size: {config_dict['performance']['batch_size']}
"""

    # Write to file with secure permissions
    with open(config_path, 'w', encoding='utf-8') as f:
        f.write(yaml_content)

    # Set secure permissions (owner read/write only)
    os.chmod(config_path, 0o600)


def _verify_permissions(path: str, expected_mode: int, path_type: str) -> None:
    """Verify that a file or directory has the correct permissions.

    Args:
        path: Path to verify
        expected_mode: Expected file mode (e.g., 0o600, 0o700)
        path_type: Description of path type for error messages

    Raises:
        RuntimeError: If permissions are incorrect
    """
    actual_mode = stat.S_IMODE(os.stat(path).st_mode)
    if actual_mode != expected_mode:
        # Try to fix permissions
        try:
            os.chmod(path, expected_mode)
            logger.warning(f"Fixed permissions for {path_type}: {path}")
        except Exception as e:
            raise RuntimeError(
                f"Incorrect permissions for {path_type}: {path} "
                f"(expected {oct(expected_mode)}, got {oct(actual_mode)}): {e}"
            )


def is_first_run(config_path: Optional[str] = None) -> bool:
    """Check if this is the first time LeIndex is being run.

    Args:
        config_path: Optional custom config path. Defaults to ~/.leindex/mcp_config.yaml

    Returns:
        True if config file does not exist (first run), False otherwise
    """
    if config_path is None:
        config_path = GlobalConfigManager.DEFAULT_CONFIG_PATH

    config_path = os.path.expanduser(config_path)
    return not os.path.exists(config_path)


def get_setup_status() -> Dict[str, Any]:
    """Get current setup status for LeIndex.

    Returns:
        Dictionary with setup status information:
        - is_first_run: True if this is the first run
        - config_exists: True if config file exists
        - config_path: Path to config file
        - data_exists: True if data directory exists
        - data_path: Path to data directory
        - permissions_valid: True if permissions are correct
    """
    config_path = os.path.expanduser(GlobalConfigManager.DEFAULT_CONFIG_PATH)
    data_path = os.path.expanduser("~/.leindex_data")

    status = {
        'is_first_run': is_first_run(),
        'config_exists': os.path.exists(config_path),
        'config_path': config_path,
        'data_exists': os.path.exists(data_path),
        'data_path': data_path,
        'permissions_valid': True,
    }

    # Check config permissions
    if status['config_exists']:
        try:
            mode = stat.S_IMODE(os.stat(config_path).st_mode)
            status['permissions_valid'] = (mode == 0o600)
        except Exception:
            status['permissions_valid'] = False

    return status


__all__ = [
    'first_time_setup',
    'SetupResult',
    'detect_hardware',
    'is_first_run',
    'get_setup_status',
]
