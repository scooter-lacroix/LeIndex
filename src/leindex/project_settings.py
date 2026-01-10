"""
Project Settings Management

This module provides functionality for managing project settings and persistent data
for the LeIndex MCP server.
"""
import os
import json
import shutil
import pickle
import tempfile
import hashlib
from datetime import datetime

from .constants import (
    SETTINGS_DIR, CONFIG_FILE, INDEX_FILE, CACHE_FILE, METADATA_FILE
)
from .config_manager import ConfigManager
from .search.base import SearchStrategy
from .search.ugrep import UgrepStrategy
from .search.ripgrep import RipgrepStrategy
from .search.ag import AgStrategy
from .search.grep import GrepStrategy
from .search.basic import BasicSearchStrategy


# Prioritized list of search strategies
SEARCH_STRATEGY_CLASSES = [
    UgrepStrategy,
    RipgrepStrategy,
    AgStrategy,
    GrepStrategy,
    BasicSearchStrategy,
]


def _get_available_strategies() -> list[SearchStrategy]:
    """
    Detect and return a list of available search strategy instances,
    ordered by preference.
    """
    available = []
    for strategy_class in SEARCH_STRATEGY_CLASSES:
        try:
            strategy = strategy_class()
            if strategy.is_available():
                available.append(strategy)
        except Exception as e:
            print(f"Error initializing strategy {strategy_class.__name__}: {e}")
    return available


class ProjectSettings:
    """Class for managing project settings and index data"""

    def __init__(self, base_path, skip_load=False):
        """Initialize project settings

        Args:
            base_path (str): Base path of the project
            skip_load (bool): Whether to skip loading files
        """
        self.base_path = base_path
        self.skip_load = skip_load
        self.available_strategies: list[SearchStrategy] = []
        self.refresh_available_strategies()

        # Ensure the base path of the temporary directory exists
        try:
            # Get system temporary directory
            system_temp = tempfile.gettempdir()
            print(f"System temporary directory: {system_temp}")

            # Check if the system temporary directory exists and is writable
            if not os.path.exists(system_temp):
                print(f"Warning: System temporary directory does not exist: {system_temp}")
                # Try using current directory as fallback
                system_temp = os.getcwd()
                print(f"Using current directory as fallback: {system_temp}")

            if not os.access(system_temp, os.W_OK):
                print(f"Warning: No write access to system temporary directory: {system_temp}")
                # Try using current directory as fallback
                system_temp = os.getcwd()
                print(f"Using current directory as fallback: {system_temp}")

            # Create leindex directory
            temp_base_dir = os.path.join(system_temp, SETTINGS_DIR)
            print(f"Code indexer directory path: {temp_base_dir}")

            if not os.path.exists(temp_base_dir):
                print(f"Creating code indexer directory: {temp_base_dir}")
                os.makedirs(temp_base_dir, exist_ok=True)
                print(f"Code indexer directory created: {temp_base_dir}")
            else:
                print(f"Code indexer directory already exists: {temp_base_dir}")
        except Exception as e:
            print(f"Error setting up temporary directory: {e}")
            # If unable to create temporary directory, use .leindex in current directory
            temp_base_dir = os.path.join(os.getcwd(), ".leindex")
            print(f"Using fallback directory: {temp_base_dir}")
            if not os.path.exists(temp_base_dir):
                os.makedirs(temp_base_dir, exist_ok=True)

        # Use system temporary directory to store index data
        try:
            if base_path:
                # Use hash of project path as unique identifier
                path_hash = hashlib.md5(base_path.encode()).hexdigest()
                self.settings_path = os.path.join(temp_base_dir, path_hash)
                print(f"Using project-specific directory: {self.settings_path}")
            else:
                # If no base path provided, use a default directory
                self.settings_path = os.path.join(temp_base_dir, "default")
                print(f"Using default directory: {self.settings_path}")

            self.ensure_settings_dir()
        except Exception as e:
            print(f"Error setting up project settings: {e}")
            # If error occurs, use .leindex in current directory as fallback
            fallback_dir = os.path.join(os.getcwd(), ".leindex",
                                      "default" if not base_path else hashlib.md5(base_path.encode()).hexdigest())
            print(f"Using fallback directory: {fallback_dir}")
            self.settings_path = fallback_dir
            if not os.path.exists(fallback_dir):
                os.makedirs(fallback_dir, exist_ok=True)

    def ensure_settings_dir(self):
        """Ensure settings directory exists"""
        print(f"Checking project settings directory: {self.settings_path}")

        try:
            if not os.path.exists(self.settings_path):
                print(f"Creating project settings directory: {self.settings_path}")
                # Create directory structure
                os.makedirs(self.settings_path, exist_ok=True)
                print(f"Project settings directory created: {self.settings_path}")
            else:
                print(f"Project settings directory already exists: {self.settings_path}")

            # Check if directory is writable
            if not os.access(self.settings_path, os.W_OK):
                print(f"Warning: No write access to project settings directory: {self.settings_path}")
                # If directory is not writable, use .leindex in current directory as fallback
                fallback_dir = os.path.join(os.getcwd(), ".leindex",
                                          os.path.basename(self.settings_path))
                print(f"Using fallback directory: {fallback_dir}")
                self.settings_path = fallback_dir
                if not os.path.exists(fallback_dir):
                    os.makedirs(fallback_dir, exist_ok=True)
        except Exception as e:
            print(f"Error ensuring settings directory: {e}")
            # If unable to create settings directory, use .leindex in current directory
            fallback_dir = os.path.join(os.getcwd(), ".leindex",
                                      "default" if not self.base_path else hashlib.md5(self.base_path.encode()).hexdigest())
            print(f"Using fallback directory: {fallback_dir}")
            self.settings_path = fallback_dir
            if not os.path.exists(fallback_dir):
                os.makedirs(fallback_dir, exist_ok=True)

    def get_config_path(self):
        """Get the path to the configuration file"""
        try:
            path = os.path.join(self.settings_path, CONFIG_FILE)
            # Ensure directory exists
            os.makedirs(os.path.dirname(path), exist_ok=True)
            return path
        except Exception as e:
            print(f"Error getting config path: {e}")
            # If error occurs, use file in current directory as fallback
            return os.path.join(os.getcwd(), CONFIG_FILE)

    def get_index_path(self):
        """Get the path to the index file"""
        try:
            path = os.path.join(self.settings_path, INDEX_FILE)
            # Ensure directory exists
            os.makedirs(os.path.dirname(path), exist_ok=True)
            return path
        except Exception as e:
            print(f"Error getting index path: {e}")
            # If error occurs, use file in current directory as fallback
            return os.path.join(os.getcwd(), INDEX_FILE)

    def get_cache_path(self):
        """Get the path to the cache file"""
        try:
            path = os.path.join(self.settings_path, CACHE_FILE)
            # Ensure directory exists
            os.makedirs(os.path.dirname(path), exist_ok=True)
            return path
        except Exception as e:
            print(f"Error getting cache path: {e}")
            # If error occurs, use file in current directory as fallback
            return os.path.join(os.getcwd(), CACHE_FILE)

    def get_metadata_path(self):
        """Get the path to the metadata file"""
        try:
            path = os.path.join(self.settings_path, METADATA_FILE)
            # Ensure directory exists
            os.makedirs(os.path.dirname(path), exist_ok=True)
            return path
        except Exception as e:
            print(f"Error getting metadata path: {e}")
            # If error occurs, use file in current directory as fallback
            return os.path.join(os.getcwd(), METADATA_FILE)

    def _get_timestamp(self):
        """Get current timestamp"""
        return datetime.now().isoformat()

    def save_config(self, config):
        """Save configuration data

        Args:
            config (dict): Configuration data
        """
        try:
            config_path = self.get_config_path()
            # Add timestamp
            config['last_updated'] = self._get_timestamp()

            # Ensure directory exists
            os.makedirs(os.path.dirname(config_path), exist_ok=True)

            with open(config_path, 'w', encoding='utf-8') as f:
                json.dump(config, f, indent=2, ensure_ascii=False)

            print(f"Config saved to: {config_path}")
            return config
        except Exception as e:
            print(f"Error saving config: {e}")
            return config

    def load_config(self):
        """Load configuration data

        Returns:
            dict: Configuration data, or empty dict if file doesn't exist
        """
        # If skip_load is set, return empty dict directly
        if self.skip_load:
            return {}

        try:
            config_path = self.get_config_path()
            if os.path.exists(config_path):
                try:
                    with open(config_path, 'r', encoding='utf-8') as f:
                        config = json.load(f)
                    print(f"Config loaded from: {config_path}")
                    return config
                except (json.JSONDecodeError, UnicodeDecodeError) as e:
                    print(f"Error parsing config file: {e}")
                    # If file is corrupted, return empty dict
                    return {}
            else:
                print(f"Config file does not exist: {config_path}")
            return {}
        except Exception as e:
            print(f"Error loading config: {e}")
            return {}

    def save_index(self, file_index):
        """Save file index

        Args:
            file_index (dict): File index data
        """
        try:
            index_path = self.get_index_path()
            print(f"Saving index to: {index_path}")

            # Ensure directory exists
            dir_path = os.path.dirname(index_path)
            if not os.path.exists(dir_path):
                print(f"Creating directory: {dir_path}")
                os.makedirs(dir_path, exist_ok=True)

            # Check if directory is writable
            if not os.access(dir_path, os.W_OK):
                print(f"Warning: Directory is not writable: {dir_path}")
                # Use current directory as fallback
                index_path = os.path.join(os.getcwd(), INDEX_FILE)
                print(f"Using fallback path: {index_path}")

            with open(index_path, 'wb') as f:
                pickle.dump(file_index, f)

            print(f"Index saved successfully to: {index_path}")
        except Exception as e:
            print(f"Error saving index: {e}")
            # Try saving to current directory
            try:
                fallback_path = os.path.join(os.getcwd(), INDEX_FILE)
                print(f"Trying fallback path: {fallback_path}")
                with open(fallback_path, 'wb') as f:
                    pickle.dump(file_index, f)
                print(f"Index saved to fallback path: {fallback_path}")
            except Exception as e2:
                print(f"Error saving index to fallback path: {e2}")

    def load_index(self):
        """Load file index

        Returns:
            dict: File index data, or empty dict if file doesn't exist
        """
        # If skip_load is set, return empty dict directly
        if self.skip_load:
            return {}

        try:
            index_path = self.get_index_path()

            if os.path.exists(index_path):
                try:
                    with open(index_path, 'rb') as f:
                        index = pickle.load(f)
                    print(f"Index loaded successfully from: {index_path}")
                    return index
                except (pickle.PickleError, EOFError) as e:
                    print(f"Error parsing index file: {e}")
                    # If file is corrupted, return empty dict
                    return {}
                except Exception as e:
                    print(f"Unexpected error loading index: {e}")
                    return {}
            else:
                # Try loading from current directory
                fallback_path = os.path.join(os.getcwd(), INDEX_FILE)
                if os.path.exists(fallback_path):
                    print(f"Trying fallback path: {fallback_path}")
                    try:
                        with open(fallback_path, 'rb') as f:
                            index = pickle.load(f)
                        print(f"Index loaded from fallback path: {fallback_path}")
                        return index
                    except Exception as e:
                        print(f"Error loading index from fallback path: {e}")

            return {}
        except Exception as e:
            print(f"Error in load_index: {e}")
            return {}

    def save_cache(self, content_cache):
        """Save content cache

        Args:
            content_cache (dict): Content cache data
        """
        try:
            cache_path = self.get_cache_path()
            print(f"Saving cache to: {cache_path}")

            # Ensure directory exists
            dir_path = os.path.dirname(cache_path)
            if not os.path.exists(dir_path):
                print(f"Creating directory: {dir_path}")
                os.makedirs(dir_path, exist_ok=True)

            # Check if directory is writable
            if not os.access(dir_path, os.W_OK):
                print(f"Warning: Directory is not writable: {dir_path}")
                # Use current directory as fallback
                cache_path = os.path.join(os.getcwd(), CACHE_FILE)
                print(f"Using fallback path: {cache_path}")

            with open(cache_path, 'wb') as f:
                pickle.dump(content_cache, f)

            print(f"Cache saved successfully to: {cache_path}")
        except Exception as e:
            print(f"Error saving cache: {e}")
            # Try saving to current directory
            try:
                fallback_path = os.path.join(os.getcwd(), CACHE_FILE)
                print(f"Trying fallback path: {fallback_path}")
                with open(fallback_path, 'wb') as f:
                    pickle.dump(content_cache, f)
                print(f"Cache saved to fallback path: {fallback_path}")
            except Exception as e2:
                print(f"Error saving cache to fallback path: {e2}")

    def load_cache(self):
        """Load content cache

        Returns:
            dict: Content cache data, or empty dict if file doesn't exist
        """
        # If skip_load is set, return empty dict directly
        if self.skip_load:
            return {}

        try:
            cache_path = self.get_cache_path()

            if os.path.exists(cache_path):
                try:
                    with open(cache_path, 'rb') as f:
                        cache = pickle.load(f)
                    print(f"Cache loaded successfully from: {cache_path}")
                    return cache
                except (pickle.PickleError, EOFError) as e:
                    print(f"Error parsing cache file: {e}")
                    # If file is corrupted, return empty dict
                    return {}
                except Exception as e:
                    print(f"Unexpected error loading cache: {e}")
                    return {}
            else:
                # Try loading from current directory
                fallback_path = os.path.join(os.getcwd(), CACHE_FILE)
                if os.path.exists(fallback_path):
                    print(f"Trying fallback path: {fallback_path}")
                    try:
                        with open(fallback_path, 'rb') as f:
                            cache = pickle.load(f)
                        print(f"Cache loaded from fallback path: {fallback_path}")
                        return cache
                    except Exception as e:
                        print(f"Error loading cache from fallback path: {e}")

            return {}
        except Exception as e:
            print(f"Error in load_cache: {e}")
            return {}

    def save_metadata(self, metadata):
        """Save file metadata

        Args:
            metadata (dict): File metadata data containing timestamps and hashes
        """
        try:
            metadata_path = self.get_metadata_path()
            print(f"Saving metadata to: {metadata_path}")

            # Ensure directory exists
            dir_path = os.path.dirname(metadata_path)
            if not os.path.exists(dir_path):
                print(f"Creating directory: {dir_path}")
                os.makedirs(dir_path, exist_ok=True)

            # Check if directory is writable
            if not os.access(dir_path, os.W_OK):
                print(f"Warning: Directory is not writable: {dir_path}")
                # Use current directory as fallback
                metadata_path = os.path.join(os.getcwd(), METADATA_FILE)
                print(f"Using fallback path: {metadata_path}")

            with open(metadata_path, 'wb') as f:
                pickle.dump(metadata, f)

            print(f"Metadata saved successfully to: {metadata_path}")
        except Exception as e:
            print(f"Error saving metadata: {e}")
            # Try saving to current directory
            try:
                fallback_path = os.path.join(os.getcwd(), METADATA_FILE)
                print(f"Trying fallback path: {fallback_path}")
                with open(fallback_path, 'wb') as f:
                    pickle.dump(metadata, f)
                print(f"Metadata saved to fallback path: {fallback_path}")
            except Exception as e2:
                print(f"Error saving metadata to fallback path: {e2}")

    def load_metadata(self):
        """Load file metadata

        Returns:
            dict: File metadata data, or empty dict if file doesn't exist
        """
        # If skip_load is set, return empty dict directly
        if self.skip_load:
            return {}

        try:
            metadata_path = self.get_metadata_path()

            if os.path.exists(metadata_path):
                try:
                    with open(metadata_path, 'rb') as f:
                        metadata = pickle.load(f)
                    print(f"Metadata loaded successfully from: {metadata_path}")
                    return metadata
                except (pickle.PickleError, EOFError) as e:
                    print(f"Error parsing metadata file: {e}")
                    # If file is corrupted, return empty dict
                    return {}
                except Exception as e:
                    print(f"Unexpected error loading metadata: {e}")
                    return {}
            else:
                # Try loading from current directory
                fallback_path = os.path.join(os.getcwd(), METADATA_FILE)
                if os.path.exists(fallback_path):
                    print(f"Trying fallback path: {fallback_path}")
                    try:
                        with open(fallback_path, 'rb') as f:
                            metadata = pickle.load(f)
                        print(f"Metadata loaded from fallback path: {fallback_path}")
                        return metadata
                    except Exception as e:
                        print(f"Error loading metadata from fallback path: {e}")

            return {}
        except Exception as e:
            print(f"Error in load_metadata: {e}")
            return {}

    def clear(self):
        """Clear all settings and cache files"""
        try:
            print(f"Clearing settings directory: {self.settings_path}")

            if os.path.exists(self.settings_path):
                # Check if directory is writable
                if not os.access(self.settings_path, os.W_OK):
                    print(f"Warning: Directory is not writable: {self.settings_path}")
                    return

                # Delete all files in the directory
                try:
                    for filename in os.listdir(self.settings_path):
                        file_path = os.path.join(self.settings_path, filename)
                        try:
                            if os.path.isfile(file_path):
                                os.unlink(file_path)
                                print(f"Deleted file: {file_path}")
                            elif os.path.isdir(file_path):
                                shutil.rmtree(file_path)
                                print(f"Deleted directory: {file_path}")
                        except Exception as e:
                            print(f"Error deleting {file_path}: {e}")
                except Exception as e:
                    print(f"Error listing directory: {e}")

                print("Settings directory cleared successfully")
            else:
                print(f"Settings directory does not exist: {self.settings_path}")
        except Exception as e:
            print(f"Error clearing settings: {e}")

    def get_stats(self):
        """Get statistics for the settings directory

        Returns:
            dict: Dictionary containing file sizes and update times
        """
        try:
            print(f"Getting stats for settings directory: {self.settings_path}")

            stats = {
                'settings_path': self.settings_path,
                'exists': os.path.exists(self.settings_path),
                'is_directory': os.path.isdir(self.settings_path) if os.path.exists(self.settings_path) else False,
                'writable': os.access(self.settings_path, os.W_OK) if os.path.exists(self.settings_path) else False,
                'files': {},
                'temp_dir': tempfile.gettempdir(),
                'current_dir': os.getcwd()
            }

            if stats['exists'] and stats['is_directory']:
                try:
                    # Get all files in the directory
                    all_files = os.listdir(self.settings_path)
                    stats['all_files'] = all_files

                    # Get details for specific files
                    for filename in [CONFIG_FILE, INDEX_FILE, CACHE_FILE, METADATA_FILE]:
                        file_path = os.path.join(self.settings_path, filename)
                        if os.path.exists(file_path):
                            try:
                                file_stats = os.stat(file_path)
                                stats['files'][filename] = {
                                    'path': file_path,
                                    'size_bytes': file_stats.st_size,
                                    'last_modified': datetime.fromtimestamp(file_stats.st_mtime).isoformat(),
                                    'readable': os.access(file_path, os.R_OK),
                                    'writable': os.access(file_path, os.W_OK)
                                }
                            except Exception as e:
                                stats['files'][filename] = {
                                    'path': file_path,
                                    'error': str(e)
                                }
                except Exception as e:
                    stats['list_error'] = str(e)

            # Check fallback path
            fallback_dir = os.path.join(os.getcwd(), ".leindex")
            stats['fallback_path'] = fallback_dir
            stats['fallback_exists'] = os.path.exists(fallback_dir)
            stats['fallback_is_directory'] = os.path.isdir(fallback_dir) if os.path.exists(fallback_dir) else False

            return stats
        except Exception as e:
            print(f"Error getting stats: {e}")
            return {
                'error': str(e),
                'settings_path': self.settings_path,
                'temp_dir': tempfile.gettempdir(),
                'current_dir': os.getcwd()
            }

    def get_search_tools_config(self):
        """Get the configuration of available search tools.

        Returns:
            dict: A dictionary containing the list of available tool names.
        """
        return {
            "available_tools": [s.name for s in self.available_strategies],
            "preferred_tool": self.get_preferred_search_tool().name if self.available_strategies else None
        }

    def get_preferred_search_tool(self) -> SearchStrategy | None:
        """Get the preferred search tool based on availability and priority.

        Returns:
            SearchStrategy: An instance of the preferred search strategy, or None.
        """
        if not self.available_strategies:
            self.refresh_available_strategies()
        
        return self.available_strategies[0] if self.available_strategies else None

    def refresh_available_strategies(self):
        """
        Force a refresh of the available search tools list.
        """
        print("Refreshing available search strategies...")
        self.available_strategies = _get_available_strategies()
        print(f"Available strategies found: {[s.name for s in self.available_strategies]}")
    
    def get_config_manager(self) -> ConfigManager:
        """Get ConfigManager instance with project-specific overrides.

        Returns:
            ConfigManager: Configured instance with project path for overrides
        """
        return ConfigManager(project_path=self.base_path)

    def get_effective_config(self) -> dict:
        """Get the effective configuration with project overrides applied.

        Returns:
            dict: Merged configuration with project overrides
        """
        config_manager = self.get_config_manager()
        return config_manager.get_config()

    def get_memory_config(self) -> dict:
        """Get effective memory configuration with project overrides.

        Integrates with the new project_config module to provide memory
        allocation hints and eviction priorities.

        Returns:
            dict: Effective memory configuration with keys:
                - estimated_mb: Memory estimate (project override or global default)
                - priority: Eviction priority (high/normal/low)
                - priority_score: Numeric priority score (2.0/1.0/0.5)
                - is_overridden: True if project overrode defaults
                - max_override_mb: Maximum allowed override (512MB)

        Example:
            >>> settings = ProjectSettings("/path/to/project")
            >>> mem_config = settings.get_memory_config()
            >>> print(f"Memory: {mem_config['estimated_mb']}MB")
            >>> print(f"Priority: {mem_config['priority']}")
        """
        try:
            # Import here to avoid circular dependency
            from .project_config import get_effective_memory_config

            return get_effective_memory_config(self.base_path)
        except Exception as e:
            print(f"Error loading project memory config: {e}")
            # Return defaults if project config fails
            return {
                "estimated_mb": 256,  # Global default
                "priority": "normal",
                "priority_score": 1.0,
                "is_overridden": False,
                "max_override_mb": 512,
            }
