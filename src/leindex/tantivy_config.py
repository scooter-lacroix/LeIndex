"""
Tantivy configuration and connection management.

This module handles Tantivy configuration, index path management,
and initialization. It provides a simple API for creating and
configuring Tantivy search backends.

Tantivy is a Rust-based search engine (Lucene equivalent) with Python bindings.
It's embedded (no separate service), fast, and provides BM25 scoring.
"""

import os
from typing import Dict, Any, Optional
from pathlib import Path

from .config_manager import ConfigManager
from .logger_config import logger

# Try to import tantivy - make it optional
try:
    import tantivy
    TANTIVY_AVAILABLE = True
except ImportError:
    TANTIVY_AVAILABLE = False
    tantivy = None  # type: ignore


class TantivyConfig:
    """Manages Tantivy configuration and index path."""

    def __init__(self):
        self.config_manager = ConfigManager()
        self._index_path: Optional[str] = None
        self._cache_enabled: Optional[bool] = None
        self._cache_max_size: Optional[int] = None
        self._cache_ttl_seconds: Optional[int] = None
        self._bm25_k1: Optional[float] = None
        self._bm25_b: Optional[float] = None

    def get_tantivy_index_path(self) -> str:
        """Get Tantivy index path from configuration."""
        if self._index_path is None:
            # Priority: Environment variables > Config file > Default
            env_path = os.getenv("TANTIVY_INDEX_PATH")
            if env_path:
                self._index_path = env_path
            else:
                dal_settings = self.config_manager.get_dal_settings()
                self._index_path = dal_settings.get(
                    "tantivy_index_path",
                    os.path.join(os.getcwd(), ".tantivy_index")
                )

        return self._index_path

    def is_cache_enabled(self) -> bool:
        """Check if Tantivy search cache is enabled."""
        if self._cache_enabled is None:
            env_cache = os.getenv("TANTIVY_CACHE_ENABLED")
            if env_cache is not None:
                self._cache_enabled = self._parse_bool(env_cache)
            else:
                dal_settings = self.config_manager.get_dal_settings()
                self._cache_enabled = self._parse_bool(
                    dal_settings.get("tantivy_cache_enabled", "true")
                )

        return self._cache_enabled

    def get_cache_max_size(self) -> int:
        """Get maximum cache size."""
        if self._cache_max_size is None:
            env_size = os.getenv("TANTIVY_CACHE_MAX_SIZE")
            if env_size:
                self._cache_max_size = int(env_size)
            else:
                dal_settings = self.config_manager.get_dal_settings()
                self._cache_max_size = int(
                    dal_settings.get("tantivy_cache_max_size", 128)
                )

        return self._cache_max_size

    def get_cache_ttl_seconds(self) -> int:
        """Get cache TTL in seconds."""
        if self._cache_ttl_seconds is None:
            env_ttl = os.getenv("TANTIVY_CACHE_TTL_SECONDS")
            if env_ttl:
                self._cache_ttl_seconds = int(env_ttl)
            else:
                dal_settings = self.config_manager.get_dal_settings()
                self._cache_ttl_seconds = int(
                    dal_settings.get("tantivy_cache_ttl_seconds", 300)
                )

        return self._cache_ttl_seconds

    def get_bm25_k1(self) -> float:
        """Get BM25 k1 parameter (term frequency saturation)."""
        if self._bm25_k1 is None:
            env_k1 = os.getenv("TANTIVY_BM25_K1")
            if env_k1:
                self._bm25_k1 = float(env_k1)
            else:
                dal_settings = self.config_manager.get_dal_settings()
                self._bm25_k1 = float(
                    dal_settings.get("tantivy_bm25_k1", 1.2)
                )

        return self._bm25_k1

    def get_bm25_b(self) -> float:
        """Get BM25 b parameter (length normalization)."""
        if self._bm25_b is None:
            env_b = os.getenv("TANTIVY_BM25_B")
            if env_b:
                self._bm25_b = float(env_b)
            else:
                dal_settings = self.config_manager.get_dal_settings()
                self._bm25_b = float(
                    dal_settings.get("tantivy_bm25_b", 0.75)
                )

        return self._bm25_b

    def _parse_bool(self, value) -> bool:
        """Parse boolean value from string or boolean."""
        if isinstance(value, bool):
            return value
        if isinstance(value, str):
            return value.lower() in ('true', '1', 'yes', 'on')
        return bool(value)

    def create_tantivy_search_backend(self) -> 'tantivy.Search':
        """
        Create and configure a Tantivy search backend.

        Returns:
            TantivySearch instance

        Raises:
            ImportError: If Tantivy is not installed
            Exception: If backend creation fails
        """
        if not TANTIVY_AVAILABLE:
            raise ImportError(
                "Tantivy is not installed. Install it with: pip install tantivy"
            )

        # Import here to avoid import errors if tantivy is not available
        from .storage.tantivy_storage import TantivySearch, TantivyNotAvailableError

        # Get configuration
        index_path = self.get_tantivy_index_path()
        cache_enabled = self.is_cache_enabled()
        cache_max_size = self.get_cache_max_size()
        cache_ttl_seconds = self.get_cache_ttl_seconds()
        bm25_k1 = self.get_bm25_k1()
        bm25_b = self.get_bm25_b()

        logger.info(f"Creating Tantivy search backend with index_path: {index_path}")
        logger.debug(
            f"Tantivy config: cache_enabled={cache_enabled}, "
            f"cache_max_size={cache_max_size}, cache_ttl_seconds={cache_ttl_seconds}, "
            f"bm25_k1={bm25_k1}, bm25_b={bm25_b}"
        )

        try:
            search_backend = TantivySearch(
                index_path=index_path,
                cache_enabled=cache_enabled,
                cache_max_size=cache_max_size,
                cache_ttl_seconds=cache_ttl_seconds,
                bm25_k1=bm25_k1,
                bm25_b=bm25_b,
            )
            logger.info("Tantivy search backend created successfully")
            return search_backend

        except TantivyNotAvailableError as e:
            logger.error(f"Tantivy not available: {e}")
            raise
        except Exception as e:
            logger.error(f"Failed to create Tantivy search backend: {e}")
            raise

    def test_connection(self) -> bool:
        """
        Test Tantivy availability and configuration.

        Returns:
            True if Tantivy is available and configured correctly
        """
        if not TANTIVY_AVAILABLE:
            logger.error("Tantivy is not installed")
            return False

        try:
            # Check if we can create a simple index
            index_path = self.get_tantivy_index_path()
            logger.info(f"Testing Tantivy with index path: {index_path}")

            # Create a temporary test index
            test_path = Path(index_path).parent / ".tantivy_test"
            test_path.mkdir(parents=True, exist_ok=True)

            # Try to create a simple schema and index
            schema_builder = tantivy.SchemaBuilder()
            schema_builder.add_text_field("test", stored=True)
            schema = schema_builder.build()

            index = tantivy.Index(schema, path=str(test_path))
            writer = index.writer()
            writer.add_document(tantivy.Document(test="hello world"))
            writer.commit()

            # Clean up test index
            import shutil
            shutil.rmtree(test_path, ignore_errors=True)

            logger.info("Tantivy connection test successful")
            return True

        except Exception as e:
            logger.error(f"Tantivy connection test failed: {e}")
            return False

    def ensure_index_directory(self) -> bool:
        """
        Ensure the Tantivy index directory exists.

        Returns:
            True if directory exists or was created successfully
        """
        try:
            index_path = Path(self.get_tantivy_index_path())
            index_path.mkdir(parents=True, exist_ok=True)
            logger.info(f"Tantivy index directory ready: {index_path}")
            return True
        except Exception as e:
            logger.error(f"Failed to create Tantivy index directory: {e}")
            return False

    def get_connection_info(self) -> Dict[str, Any]:
        """Get current Tantivy connection information."""
        return {
            "index_path": self.get_tantivy_index_path(),
            "cache_enabled": self.is_cache_enabled(),
            "cache_max_size": self.get_cache_max_size(),
            "cache_ttl_seconds": self.get_cache_ttl_seconds(),
            "bm25_k1": self.get_bm25_k1(),
            "bm25_b": self.get_bm25_b(),
            "tantivy_available": TANTIVY_AVAILABLE,
        }


# Global instance
tantivy_config = TantivyConfig()
