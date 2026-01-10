"""
Optimized Project Settings Management

This module provides enhanced project settings management with configurable
storage backends for better performance and memory efficiency.
"""

import hashlib
import json
import os
from datetime import datetime
from pathlib import Path
from typing import Any, Dict, List, Optional, Union

from .constants import (
    CACHE_FILE,
    CONFIG_FILE,
    INDEX_FILE,
    METADATA_FILE,
    PERSISTENT_SETTINGS_DIR,
)
from .logger_config import logger
from .registry.msgpack_serializer import FormatType, MessagePackSerializer
from .registry.registration_integrator import register_after_index_save
from .search.ag import AgStrategy
from .search.base import SearchStrategy
from .search.basic import BasicSearchStrategy
from .search.grep import GrepStrategy
from .search.ripgrep import RipgrepStrategy
from .search.ugrep import UgrepStrategy
from .search.zoekt import ZoektStrategy
from .storage import SQLiteFileMetadata, SQLiteStorage, TrieFileIndex

# Supported file extensions for indexing
SUPPORTED_EXTENSIONS = [
    ".py", ".js", ".ts", ".jsx", ".tsx", ".java", ".c", ".cpp", ".h", ".hpp",
    ".cs", ".go", ".rb", ".php", ".swift", ".kt", ".rs", ".scala", ".sh",
    ".bash", ".html", ".css", ".scss", ".md", ".json", ".xml", ".yml", ".yaml",
    ".zig", ".vue", ".svelte", ".mjs", ".cjs", ".less", ".sass", ".stylus",
    ".styl", ".hbs", ".handlebars", ".ejs", ".pug", ".astro", ".mdx",
]

# Prioritized list of search strategies (highest priority first)
SEARCH_STRATEGY_CLASSES = [
    ZoektStrategy,
    UgrepStrategy,
    RipgrepStrategy,
    AgStrategy,
    GrepStrategy,
    BasicSearchStrategy,
]


def _get_available_strategies() -> List[SearchStrategy]:
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
            logger.warning(
                f"Error initializing strategy {strategy_class.__name__}: {e}"
            )
    return available


class OptimizedProjectSettings:
    """Enhanced project settings with configurable storage backends."""

    def __init__(
        self,
        base_path: str,
        skip_load: bool = False,
        storage_backend: str = "sqlite",
        use_trie_index: bool = False,
    ):
        """Initialize optimized project settings.

        Args:
            base_path: Base path of the project
            skip_load: Whether to skip loading files
            storage_backend: Storage backend to use ('sqlite' or 'memory')
            use_trie_index: Whether to use Trie-based file index
        """
        self.base_path = base_path
        self.skip_load = skip_load
        self.storage_backend = storage_backend
        self.use_trie_index = use_trie_index
        self.available_strategies: List[SearchStrategy] = []

        # Initialize MessagePack serializer for index persistence
        self.msgpack_serializer = MessagePackSerializer(use_bin_type=True)

        # Initialize storage backend
        self._init_storage_backend()

        # Initialize search strategies
        self.refresh_available_strategies()

    def _init_storage_backend(self):
        """Initialize the storage backend."""
        try:
            # Use a persistent directory within the project for settings
            # This ensures settings persist across restarts and are tied to the project
            if self.base_path:
                # Use a hash of the base_path to create a unique subdirectory
                # within the persistent settings directory.
                path_hash = hashlib.md5(self.base_path.encode()).hexdigest()
                self.settings_path = os.path.join(
                    self.base_path, PERSISTENT_SETTINGS_DIR, path_hash
                )
            else:
                # Fallback to a default persistent directory if base_path is not set
                self.settings_path = os.path.join(
                    os.getcwd(), PERSISTENT_SETTINGS_DIR, "default"
                )

            logger.debug(
                f"OptimizedProjectSettings will store data at: {self.settings_path}"
            )

            # Ensure settings directory exists
            os.makedirs(self.settings_path, exist_ok=True)

            # Initialize storage backends
            if self.storage_backend == "sqlite":
                # SQLite storage for cache and config
                cache_db_path = os.path.join(self.settings_path, "cache.db")
                self.cache_storage = SQLiteStorage(cache_db_path)

                # File index storage
                if self.use_trie_index:
                    self.file_index = TrieFileIndex()
                else:
                    index_db_path = os.path.join(self.settings_path, "index.db")
                    self.file_index = SQLiteFileMetadata(index_db_path)

                # Metadata storage
                metadata_db_path = os.path.join(self.settings_path, "metadata.db")
                self.metadata_storage = SQLiteStorage(metadata_db_path)

                logger.info(
                    f"Initialized SQLite storage backend at: {self.settings_path}"
                )
            else:
                # Fallback to memory-based storage (for backward compatibility)
                # For metadata, always use a persistent SQLite DB for file change tracking
                # even if main storage_backend is not sqlite
                fallback_metadata_db_path = os.path.join(
                    self.settings_path, "fallback_metadata.db"
                )
                self.metadata_storage = SQLiteStorage(fallback_metadata_db_path)

                self.cache_storage = {}
                self.file_index = {}
                logger.warning(
                    "Using memory-based storage backend with persistent metadata DB at %s",
                    fallback_metadata_db_path,
                )

        except Exception as e:
            logger.exception(f"Error initializing storage backend: {e}")
            # Fallback to memory-based storage for cache and index, but try to keep metadata persistent
            fallback_metadata_db_path = os.path.join(
                self.settings_path, "fallback_metadata.db"
            )
            try:
                self.metadata_storage = SQLiteStorage(fallback_metadata_db_path)
                logger.warning(
                    "Initialized fallback persistent metadata DB at %s",
                    fallback_metadata_db_path,
                )
            except Exception as metadata_e:
                logger.exception(
                    f"Critical Error: Could not initialize fallback metadata DB: {metadata_e}"
                )
                self.metadata_storage = {}  # Fallback to in-memory if persistent fails

            self.cache_storage = {}
            self.file_index = {}
            logger.warning(f"Using memory-based storage backend due to error: {e}")

    def get_config_path(self) -> str:
        """Get the path to the configuration file."""
        return os.path.join(self.settings_path, CONFIG_FILE)

    def get_index_path(self) -> str:
        """Get the path to the index file."""
        return os.path.join(self.settings_path, INDEX_FILE)

    def get_cache_path(self) -> str:
        """Get the path to the cache file."""
        return os.path.join(self.settings_path, CACHE_FILE)

    def get_metadata_path(self) -> str:
        """Get the path to the metadata file."""
        return os.path.join(self.settings_path, METADATA_FILE)

    def _get_timestamp(self) -> str:
        """Get current timestamp."""
        return datetime.now().isoformat()

    def save_config(self, config: Dict[str, Any]) -> Dict[str, Any]:
        """Save configuration data."""
        try:
            config_path = self.get_config_path()
            config["last_updated"] = self._get_timestamp()

            os.makedirs(os.path.dirname(config_path), exist_ok=True)

            with open(config_path, "w", encoding="utf-8") as f:
                json.dump(config, f, indent=2, ensure_ascii=False)

            logger.debug(f"Config saved to: {config_path}")
            return config
        except Exception as e:
            logger.exception(f"Error saving config: {e}")
            return config

    def load_config(self) -> Dict[str, Any]:
        """Load configuration data."""
        if self.skip_load:
            return {}

        try:
            config_path = self.get_config_path()
            if os.path.exists(config_path):
                with open(config_path, "r", encoding="utf-8") as f:
                    config = json.load(f)
                logger.debug(f"Config loaded from: {config_path}")
                return config
            return {}
        except Exception as e:
            logger.exception(f"Error loading config: {e}")
            return {}

    def save_index(
        self,
        file_index: Union[Dict[str, Any], TrieFileIndex, SQLiteFileMetadata],
        is_reindex: bool = False,
    ):
        """Save file index using the configured storage backend.

        Phase 4: Auto-Registration Integration
        - After MessagePack save, automatically register project in registry
        - Sequential write pattern: index first, registry second
        - Graceful failure handling: log warning, continue on registration errors
        - Update registry on reindex

        Args:
            file_index: The file index to save
            is_reindex: Whether this is a reindex operation (updates registry instead of inserting)
        """
        try:
            if self.storage_backend == "sqlite":
                if self.use_trie_index:
                    # For Trie index, serialize with MessagePack
                    index_path = self.get_index_path()
                    # Use .msgpack extension for new files
                    msgpack_path = str(Path(index_path).with_suffix(".msgpack"))
                    # Convert to dict for serialization if needed
                    if isinstance(file_index, dict):
                        # Already a dict, save directly
                        index_data = file_index
                    elif hasattr(file_index, "to_dict"):
                        index_data = file_index.to_dict()
                    else:
                        # Fallback: serialize the trie structure
                        index_data = {"trie_data": file_index.__dict__}
                    self.msgpack_serializer.write(msgpack_path, index_data)
                    logger.debug(f"Trie index saved to: {msgpack_path}")

                    # Phase 4: Auto-register after index save
                    self._auto_register_after_save(index_data, is_reindex)
                elif isinstance(self.file_index, SQLiteFileMetadata):
                    # SQLite file index is already persisted
                    logger.debug("SQLite file index is automatically persisted")

                    # Phase 4: Auto-register for SQLite indexes too
                    file_count = (
                        self.file_index.count_files()
                        if hasattr(self.file_index, "count_files")
                        else 0
                    )
                    self._auto_register_after_save(
                        {"file_count": file_count}, is_reindex
                    )
                else:
                    # Dict-based index - save with MessagePack
                    self._save_index_msgpack(file_index)

                    # Phase 4: Auto-register after index save
                    if isinstance(file_index, dict):
                        self._auto_register_after_save(file_index, is_reindex)
            else:
                # Memory-based storage
                self.file_index = file_index
                logger.debug("Index saved to memory")
        except Exception as e:
            logger.exception(f"Error saving index: {e}")

    def _auto_register_after_save(
        self, index_data: Dict[str, Any], is_reindex: bool = False
    ):
        """
        Automatically register project after index save.

        This implements Phase 4 auto-registration:
        - Sequential write pattern (index first, registry second)
        - Graceful failure handling (log warning, continue)
        - Update registry on reindex

        Args:
            index_data: The index data that was just saved
            is_reindex: Whether this is a reindex operation
        """
        if not self.base_path:
            # No base path set, skip registration
            return

        try:
            # Calculate file count
            if isinstance(index_data, dict):
                file_count = index_data.get("file_count", len(index_data))
            else:
                file_count = 0

            # Call auto-registration (graceful failure handling built-in)
            register_after_index_save(
                project_path=self.base_path,
                index_data=index_data,
                file_count=file_count,
                is_reindex=is_reindex,
            )
        except Exception as e:
            # Graceful failure - log warning but don't fail the save operation
            logger.warning(f"Auto-registration failed (continuing anyway): {e}")

    def _save_index_msgpack(self, file_index: Dict[str, Any]):
        """Save dictionary-based index with MessagePack."""
        try:
            index_path = self.get_index_path()
            # Use .msgpack extension
            msgpack_path = str(Path(index_path).with_suffix(".msgpack"))
            self.msgpack_serializer.write(msgpack_path, file_index)
            logger.debug(f"Index saved to: {msgpack_path}")
        except Exception as e:
            logger.exception(f"Error saving index with MessagePack: {e}")

    def load_index(
        self,
    ) -> Union[Dict[str, Any], TrieFileIndex, SQLiteFileMetadata, None]:
        """Load file index using the configured storage backend with format detection."""
        if self.skip_load:
            return {} if self.storage_backend != "sqlite" else None

        try:
            if self.storage_backend == "sqlite":
                if self.use_trie_index:
                    # Load Trie index from file with format detection
                    index_path = self.get_index_path()
                    msgpack_path = str(Path(index_path).with_suffix(".msgpack"))

                    # Try MessagePack first
                    if os.path.exists(msgpack_path):
	                        try:
	                            index_data = self.msgpack_serializer.read(msgpack_path)
	                            logger.debug(
	                                f"Trie index loaded from MessagePack: {msgpack_path}"
	                            )
	                            # Return the raw data as-is (dict or whatever was saved)
	                            # This allows tests to save a dict and get that same dict back
	                            return index_data
	                        except Exception as e:
	                            logger.exception(f"Error loading MessagePack index: {e}")

                    # Fallback to legacy pickle file for migration
                    if os.path.exists(index_path):
	                        format_type = self.msgpack_serializer.detect_format(index_path)
	                        if format_type == FormatType.PICKLE:
	                            logger.info(f"Migrating legacy pickle index: {index_path}")
	                            try:
	                                # Read with pickle support and migrate to MessagePack
	                                legacy_data = self.msgpack_serializer.read(index_path)
	                                # Save as MessagePack
	                                self.msgpack_serializer.write(msgpack_path, legacy_data)
	                                logger.info(
	                                    f"Migrated index to MessagePack: {msgpack_path}"
	                                )
	                                # Optionally remove old pickle file after successful migration
	                                # os.unlink(index_path)
	                                return legacy_data
	                            except Exception as e:
	                                logger.exception(f"Error migrating pickle index: {e}")

                    # Return empty dict if no file found
                    logger.debug("No existing Trie index found, returning empty dict")
                    return {}
                else:
                    # SQLite file index is already loaded
                    logger.debug("SQLite file index is ready")
                    return self.file_index
            else:
                # Memory-based storage - try to load with format detection
                return self._load_index_with_format_detection()
        except Exception as e:
            logger.exception(f"Error loading index: {e}")
            return {} if self.storage_backend != "sqlite" else None
    def _load_index_with_format_detection(self) -> Dict[str, Any]:
        """Load index with automatic format detection and migration."""
        try:
            index_path = self.get_index_path()
            msgpack_path = str(Path(index_path).with_suffix(".msgpack"))

            # Try MessagePack first
            if os.path.exists(msgpack_path):
                try:
                    index = self.msgpack_serializer.read(msgpack_path)
                    logger.debug(f"Index loaded from MessagePack: {msgpack_path}")
                    return index
                except Exception as e:
                    logger.exception(f"Error loading MessagePack index: {e}")

            # Fallback to legacy pickle file
            if os.path.exists(index_path):
                format_type = self.msgpack_serializer.detect_format(index_path)
                if format_type == FormatType.PICKLE:
                    logger.info(f"Migrating legacy pickle index: {index_path}")
                    try:
                        # Read with pickle support
                        legacy_index = self.msgpack_serializer.read(index_path)
                        # Migrate to MessagePack
                        self.msgpack_serializer.write(msgpack_path, legacy_index)
                        logger.info(
                            f"Migrated index to MessagePack: {msgpack_path}"
                        )
                        return legacy_index
                    except Exception as e:
                        logger.exception(f"Error migrating pickle index: {e}")
                else:
                    # Unknown format, try reading as MessagePack
                    try:
                        index = self.msgpack_serializer.read(index_path)
                        return index
                    except Exception as e:
                        logger.exception(f"Error reading index: {e}")

            return {}
        except Exception as e:
            logger.exception(f"Error loading index with format detection: {e}")
            return {}


    def save_cache(self, content_cache: Dict[str, Any]):
        """Save content cache using the configured storage backend."""
        try:
            if self.storage_backend == "sqlite":
                # Save to SQLite storage
                for key, value in content_cache.items():
                    self.cache_storage.put(key, value)
                    self.cache_storage.flush()
                    logger.debug(
                        f"Cache saved to SQLite storage ({len(content_cache)} items)"
                    )
            else:
                # Memory-based storage
                self.cache_storage.update(content_cache)
                logger.debug(f"Cache saved to memory ({len(content_cache)} items)")
        except Exception as e:
            logger.exception(f"Error saving cache: {e}")

    def load_cache(self) -> Dict[str, Any]:
        """Load content cache using the configured storage backend."""
        if self.skip_load:
            return {}

        try:
            if self.storage_backend == "sqlite":
                # Load from SQLite storage
                cache = {}
                for key, value in self.cache_storage.items():
                    cache[key] = value
                logger.debug(f"Cache loaded from SQLite storage ({len(cache)} items)")
                return cache
            else:
                # Memory-based storage
                logger.debug(f"Cache loaded from memory ({len(self.cache_storage)} items)")
                return dict(self.cache_storage)
        except Exception as e:
            logger.exception(f"Error loading cache: {e}")
            return {}

    def save_metadata(self, metadata: Dict[str, Any]):
        """Save file metadata using the configured storage backend."""
        try:
            if self.storage_backend == "sqlite":
                # Save to SQLite storage
                for key, value in metadata.items():
                    self.metadata_storage.put(key, value)
                self.metadata_storage.flush()
                logger.debug(f"Metadata saved to SQLite storage ({len(metadata)} items)")
            else:
                # Memory-based storage
                self.metadata_storage.update(metadata)
                logger.debug(f"Metadata saved to memory ({len(metadata)} items)")
        except Exception as e:
            logger.exception(f"Error saving metadata: {e}")

    def load_metadata(self) -> Dict[str, Any]:
        """Load file metadata using the configured storage backend."""
        if self.skip_load:
            return {}

        try:
            if self.storage_backend == "sqlite":
                # Load from SQLite storage
                metadata = {}
                for key, value in self.metadata_storage.items():
                    metadata[key] = value
                logger.debug(f"Metadata loaded from SQLite storage ({len(metadata)} items)")
                return metadata
            else:
                # Memory-based storage
                logger.debug(
                    f"Metadata loaded from memory ({len(self.metadata_storage)} items)"
                )
                return dict(self.metadata_storage)
        except Exception as e:
            logger.exception(f"Error loading metadata: {e}")
            return {}

    def clear(self):
        """Clear all settings and cache files."""
        try:
            if self.storage_backend == "sqlite":
                # For SQLite, it's safer to delete the database files and recreate storage objects
                logger.info("Clearing SQLite storage...")

                # Close existing storage objects
                if hasattr(self.cache_storage, "close"):
                    self.cache_storage.close()
                if hasattr(self.metadata_storage, "close"):
                    self.metadata_storage.close()
                if hasattr(self.file_index, "close"):
                    self.file_index.close()

                # Delete database files
                if os.path.exists(self.settings_path):
                    for filename in os.listdir(self.settings_path):
                        file_path = os.path.join(self.settings_path, filename)
                        if os.path.isfile(file_path) and filename.endswith(".db"):
                            os.unlink(file_path)
                            logger.debug(f"Deleted database file: {file_path}")

                # Recreate storage objects with fresh databases
                self._init_storage_backend()
                logger.info("SQLite storage cleared and reinitialized")
            else:
                # Clear memory-based storage
                self.cache_storage.clear()
                self.metadata_storage.clear()
                if hasattr(self.file_index, "clear"):
                    self.file_index.clear()
                else:
                    self.file_index = {}
                logger.info("Memory storage cleared")

            # Also clear any remaining legacy files
            if os.path.exists(self.settings_path):
                for filename in os.listdir(self.settings_path):
                    file_path = os.path.join(self.settings_path, filename)
                    if os.path.isfile(file_path) and not filename.endswith(".db"):
                        os.unlink(file_path)
                        logger.debug(f"Deleted legacy file: {file_path}")
        except Exception as e:
            logger.exception(f"Error clearing settings: {e}")

    def get_stats(self) -> Dict[str, Any]:
        """Get statistics for the settings."""
        try:
            stats = {
                "settings_path": self.settings_path,
                "storage_backend": self.storage_backend,
                "use_trie_index": self.use_trie_index,
                "exists": os.path.exists(self.settings_path),
                "is_directory": os.path.isdir(self.settings_path)
                if os.path.exists(self.settings_path)
                else False,
                "writable": os.access(self.settings_path, os.W_OK)
                if os.path.exists(self.settings_path)
                else False,
                "files": {},
                "storage_stats": {},
            }

            if self.storage_backend == "sqlite":
                # Get SQLite storage stats
                stats["storage_stats"] = {
                    "cache_size": self.cache_storage.size(),
                    "metadata_size": self.metadata_storage.size(),
                    "file_index_type": type(self.file_index).__name__,
                }

                if hasattr(self.file_index, "size"):
                    stats["storage_stats"]["file_index_size"] = self.file_index.size()
            else:
                # Memory-based storage stats
                stats["storage_stats"] = {
                    "cache_size": len(self.cache_storage),
                    "metadata_size": len(self.metadata_storage),
                    "file_index_size": len(self.file_index),
                }

            return stats
        except Exception as e:
            logger.exception(f"Error getting stats: {e}")
            return {"error": str(e)}

    def get_search_tools_config(self) -> Dict[str, Any]:
        """Get the configuration of available search tools."""
        return {
            "available_tools": [s.name for s in self.available_strategies],
            "preferred_tool": self.get_preferred_search_tool().name
            if self.available_strategies
            else None,
        }

    def get_preferred_search_tool(self) -> Optional[SearchStrategy]:
        """Get the preferred search tool based on availability and priority."""
        if not self.available_strategies:
            self.refresh_available_strategies()

        return self.available_strategies[0] if self.available_strategies else None

    def refresh_available_strategies(self):
        """Force a refresh of the available search tools list."""
        logger.debug("Refreshing available search strategies...")
        self.available_strategies = _get_available_strategies()
        logger.debug(
            f"Available strategies found: {[s.name for s in self.available_strategies]}"
        )

    def close(self):
        """Close storage backends and release resources."""
        try:
            if self.storage_backend == "sqlite":
                if hasattr(self.cache_storage, "close"):
                    self.cache_storage.close()
                if hasattr(self.metadata_storage, "close"):
                    self.metadata_storage.close()
                if hasattr(self.file_index, "close"):
                    self.file_index.close()
                logger.info("SQLite storage backends closed")
        except Exception as e:
            logger.exception(f"Error closing storage backends: {e}")

    def get_storage_info(self) -> Dict[str, Any]:
        """Get detailed information about the storage backend."""
        return {
            "backend_type": self.storage_backend,
            "use_trie_index": self.use_trie_index,
            "settings_path": self.settings_path,
            "cache_storage_type": type(self.cache_storage).__name__,
            "file_index_type": type(self.file_index).__name__,
            "metadata_storage_type": type(self.metadata_storage).__name__,
            "benefits": self._get_storage_benefits(),
        }

    def _get_storage_benefits(self) -> Dict[str, str]:
        """Get benefits of the current storage configuration."""
        benefits = {}

        if self.storage_backend == "sqlite":
            benefits["persistence"] = "Data is persisted to disk automatically"
            benefits["memory_efficiency"] = (
                "Lower memory usage compared to in-memory storage"
            )
            benefits["search_capability"] = "Full-text search enabled for content"
            benefits["scalability"] = "Can handle larger datasets efficiently"
            benefits["concurrent_access"] = "Thread-safe operations"
        else:
            benefits["speed"] = "Faster access for small datasets"
            benefits["simplicity"] = "Simple in-memory operations"

        if self.use_trie_index:
            benefits["prefix_search"] = "Efficient prefix-based file path lookups"
            benefits["memory_structure"] = "Trie data structure for path hierarchies"

        return benefits

    def get_supported_extensions(self) -> List[str]:
        """Get the list of supported file extensions for indexing."""
        return SUPPORTED_EXTENSIONS
