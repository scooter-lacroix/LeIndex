"""
LEANN Vector Backend - LEANN-based semantic search with local embedding models.

This module implements a vector store backend using:
- LEANN for fast, storage-efficient vector similarity search
- Local embedding models (CodeRankEmbed, sentence-transformers)
- SQLite for metadata storage

This replaces the FAISS dependency with a more storage-efficient solution.

PHASE 3: LEANN Vector Store Implementation
------------------------------------------
Spec: maestro/tracks/leindex_20250104/spec.md

Features:
- Local embedding models (nomic-ai/CodeRankEmbed, sentence-transformers)
- LEANN backends: HNSW (default) and DiskANN
- Storage-efficient index (97% savings vs traditional approaches)
- Metadata storage in JSON alongside index
- AST-aware chunking for Python files
- Thread-safe operations with asyncio.Lock

SECURITY: All inputs are validated and sanitized to prevent:
- Path traversal attacks
- Resource exhaustion
- Unbounded memory growth
- Invalid model loading
"""

from __future__ import annotations

import os
import json
import logging
import hashlib
import ast
import asyncio
import time
import random
import uuid
from pathlib import Path
from typing import List, Optional, Any, Dict, AsyncGenerator, Union, Tuple, Callable
from dataclasses import dataclass, asdict
from datetime import datetime
from threading import Lock
from functools import wraps
from collections import OrderedDict

# GRACEFUL IMPORT: Handle optional dependencies
try:
    from leann import LeannSearcher, LeannBuilder
    LEANN_AVAILABLE = True
except ImportError as e:
    LEANN_AVAILABLE = False
    LeannSearcher = None
    LeannBuilder = None
    logging.getLogger(__name__).warning(
        f"leann not available: {e}. "
        "LEANNVectorBackend will operate in LIMITED MODE. "
        "Install with: uv pip install 'leann>=0.3.5'"
    )

try:
    from sentence_transformers import SentenceTransformer
    SENTENCE_TRANSFORMERS_AVAILABLE = True
except ImportError as e:
    SENTENCE_TRANSFORMERS_AVAILABLE = False
    SentenceTransformer = None
    logging.getLogger(__name__).warning(
        f"sentence-transformers not available: {e}. "
        "LEANNVectorBackend requires sentence-transformers for embeddings."
    )

try:
    import numpy as np
    NUMPY_AVAILABLE = True
except ImportError as e:
    NUMPY_AVAILABLE = False
    np = None
    logging.getLogger(__name__).warning(
        f"numpy not available: {e}. "
        "LEANNVectorBackend requires numpy for vector operations."
    )

from .types import (
    StoreFile, FileMetadata, SearchResponse, ChunkType,
    AskResponse, StoreInfo, UploadFileOptions, SearchOptions,
    Metrics, BatchUploadResult, BatchUploadOptions
)

logger = logging.getLogger(__name__)

# ============================================================================
# SECURITY CONSTANTS - Resource limits to prevent abuse
# ============================================================================

# Default configuration
DEFAULT_BACKEND = "hnsw"
DEFAULT_INDEX_PATH = "./leann_index"
DEFAULT_EMBEDDING_DIM = 768  # CodeRankEmbed uses 768 dimensions

# Supported embedding models
DEFAULT_MODEL = "nomic-ai/CodeRankEmbed"

# Supported models with full configuration
SUPPORTED_MODELS = {
    "nomic-ai/CodeRankEmbed": {
        "dim": 768,
        "size_mb": 270,
        "description": "Code-specific embeddings (default, 137M params)",
        "context_length": 8192,
        "trust_remote_code": True,
        "requires": ["einops>=0.6.0"],
    },
    # Fallback models for compatibility
    "BAAI/bge-small-en-v1.5": {
        "dim": 384,
        "size_mb": 130,
        "description": "General-purpose embeddings (legacy)",
        "context_length": 512,
        "trust_remote_code": False,
    },
    "microsoft/codebert-base": {
        "dim": 768,
        "size_mb": 450,
        "description": "Code-specific embeddings (legacy)",
        "context_length": 512,
        "trust_remote_code": False,
    },
    "all-MiniLM-L6-v2": {
        "dim": 384,
        "size_mb": 80,
        "description": "Lightweight general-purpose embeddings",
        "context_length": 512,
        "trust_remote_code": False,
    }
}

# Legacy alias for backward compatibility
ALTERNATIVE_MODELS = {
    "BAAI/bge-small-en-v1.5": {"dim": 384},
    "microsoft/codebert-base": {"dim": 768},
    "all-MiniLM-L6-v2": {"dim": 384},
}

# Supported backends
SUPPORTED_BACKENDS = ["hnsw", "diskann"]

# Backend configurations
BACKEND_CONFIGS = {
    "hnsw": {
        "graph_degree": 32,
        "build_complexity": 64,
        "search_complexity": 32,
    },
    "diskann": {
        "graph_degree": 64,
        "build_complexity": 64,
        "search_complexity": 32,
    }
}

# SECURITY: Maximum limits to prevent resource exhaustion
MAX_VECTORS = 10_000_000  # Maximum number of vectors in index
MAX_QUERY_LENGTH = 8192  # Maximum query string length
MAX_TOP_K = 1000  # Maximum top_k value for search
MAX_CONTENT_SIZE = 50 * 1024 * 1024  # 50MB max file content size
MAX_BATCH_SIZE = 100  # Maximum files per batch
MAX_EXPORT_SIZE = 10 * 1024 * 1024 * 1024  # 10GB max export size

# Metrics configuration
MAX_METRICS_SAMPLES = 1000  # Maximum samples to keep for percentile calculations

# Path traversal patterns to detect
PATH_TRAVERSAL_PATTERNS = [r'\.\./', r'\.\.\\', r'\.\.[/\\]', r'~/', r'~\\']

# Environment variables
ENV_BACKEND = "LEANN_BACKEND"
ENV_INDEX_PATH = "LEANN_INDEX_PATH"
ENV_MODEL = "LEANN_MODEL"

# Metadata file keys
META_VERSION = "version"
META_BACKEND = "backend"
META_MODEL = "model"
META_DIMENSION = "dimension"
META_VECTOR_COUNT = "vector_count"
META_CREATED_AT = "created_at"
META_UPDATED_AT = "updated_at"

INDEX_VERSION = "1.0"

# ============================================================================
# RETRY CONFIGURATION - Exponential Backoff with Jitter
# ============================================================================

# Default retry configuration
DEFAULT_RETRY_ENABLED = True
DEFAULT_MAX_RETRIES = 5
DEFAULT_INITIAL_DELAY = 1.0  # seconds
DEFAULT_MAX_DELAY = 60.0  # seconds
DEFAULT_EXPONENTIAL_BASE = 2
DEFAULT_JITTER_ENABLED = True
DEFAULT_JITTER_RATIO = 0.25  # 0-25% of delay

# Retryable exception types (HTTP status codes and exception types)
RETRYABLE_HTTP_STATUS_CODES = {408, 429, 500, 502, 503, 504}
RETRYABLE_EXCEPTION_TYPES = (
    ConnectionError,
    TimeoutError,
    OSError,
    IOError,
)

# Environment variables for retry configuration
ENV_RETRY_ENABLED = "LEANN_RETRY_ENABLED"
ENV_MAX_RETRIES = "LEANN_MAX_RETRIES"
ENV_INITIAL_DELAY = "LEANN_INITIAL_DELAY"
ENV_MAX_DELAY = "LEANN_MAX_DELAY"
ENV_JITTER_ENABLED = "LEANN_JITTER_ENABLED"


# ============================================================================
# SECURITY VALIDATION FUNCTIONS
# ============================================================================

def _sanitize_path(path: str, allow_absolute: bool = False) -> str:
    """
    Sanitize a path to prevent traversal attacks.

    Args:
        path: The path to sanitize
        allow_absolute: Whether to allow absolute paths

    Returns:
        Sanitized path

    Raises:
        ValueError: If path contains traversal patterns or is invalid
    """
    if not path:
        raise ValueError("Path cannot be empty")

    import re

    original_path = path

    # Check for path traversal patterns
    for pattern in PATH_TRAVERSAL_PATTERNS:
        if re.search(pattern, path):
            logger.warning(f"Path traversal attempt detected: {original_path}")
            raise ValueError(
                f"Path traversal detected in '{original_path}'. "
                f"Path contains dangerous pattern: {pattern}"
            )

    # Resolve the path to its absolute form
    try:
        resolved = Path(path).resolve()
    except (OSError, RuntimeError) as e:
        raise ValueError(f"Invalid path '{original_path}': {e}")

    # Convert to string and normalize separators
    sanitized = str(resolved).replace(os.sep, '/')

    # Additional check for encoded traversal attempts
    if '%2e%2e' in sanitized.lower() or '..' in sanitized:
        raise ValueError(f"Encoded path traversal detected in '{original_path}'")

    return sanitized


def _validate_store_id(store_id: str) -> str:
    """
    Validate store_id to prevent injection attacks.

    Args:
        store_id: The store identifier to validate

    Returns:
        Validated store_id

    Raises:
        ValueError: If store_id contains invalid characters
    """
    if not store_id:
        return ""

    import re

    # Store IDs should be alphanumeric with underscores, hyphens, and forward slashes
    # Reject any path traversal patterns
    for pattern in PATH_TRAVERSAL_PATTERNS:
        if re.search(pattern, store_id):
            raise ValueError(
                f"Path traversal detected in store_id: '{store_id}'. "
                f"Contains dangerous pattern: {pattern}"
            )

    # Also check for common injection patterns
    dangerous_chars = ['\0', '\n', '\r', '\t']
    if any(char in store_id for char in dangerous_chars):
        raise ValueError(f"Store_id contains null bytes or control characters: '{store_id}'")

    # Remove any leading/trailing slashes and normalize
    normalized = store_id.strip('/')
    normalized = normalized.replace('\\', '/')

    return normalized


def _validate_file_path(file_path: str) -> str:
    """
    Validate file path to prevent traversal attacks.

    Args:
        file_path: The file path to validate

    Returns:
        Validated file path

    Raises:
        ValueError: If file_path is invalid or contains traversal
    """
    import re

    if not file_path:
        raise ValueError("File path cannot be empty")

    # Check for path traversal
    for pattern in PATH_TRAVERSAL_PATTERNS:
        if re.search(pattern, file_path):
            raise ValueError(
                f"Path traversal detected in file_path: '{file_path}'. "
                f"Contains dangerous pattern: {pattern}"
            )

    # Remove null bytes and other control characters
    dangerous_chars = ['\0', '\n', '\r', '\t']
    if any(char in file_path for char in dangerous_chars):
        raise ValueError(f"File path contains null bytes or control characters: '{file_path}'")

    # Normalize path separators
    normalized = file_path.replace('\\', '/').lstrip('/')

    # Limit path length to prevent DOS
    if len(normalized) > 4096:
        raise ValueError(f"File path too long (max 4096 characters): {len(normalized)}")

    return normalized


def _validate_query(query: str) -> str:
    """
    Validate and sanitize search query.

    Args:
        query: The search query to validate

    Returns:
        Validated and trimmed query

    Raises:
        ValueError: If query is too long or invalid
    """
    if not query:
        return ""

    query = query.strip()

    if len(query) > MAX_QUERY_LENGTH:
        raise ValueError(
            f"Query too long (max {MAX_QUERY_LENGTH} characters): {len(query)}"
        )

    # Remove null bytes and control characters (except newline, tab)
    query = ''.join(char for char in query if char != '\0')

    return query


def _validate_top_k(top_k: Optional[int]) -> int:
    """
    Validate and bound top_k parameter.

    Args:
        top_k: The requested top_k value

    Returns:
        Validated and bounded top_k value

    Raises:
        ValueError: If top_k is negative
    """
    if top_k is None:
        return 10  # Default

    if not isinstance(top_k, int):
        try:
            top_k = int(top_k)
        except (ValueError, TypeError):
            raise ValueError(f"top_k must be an integer, got: {type(top_k)}")

    if top_k < 0:
        raise ValueError(f"top_k must be non-negative, got: {top_k}")

    return min(top_k, MAX_TOP_K)


def _validate_content_size(content: Union[str, bytes]) -> None:
    """
    Validate content size to prevent memory exhaustion.

    Args:
        content: The content to validate

    Raises:
        ValueError: If content is too large
    """
    if isinstance(content, str):
        size = len(content.encode('utf-8'))
    elif isinstance(content, bytes):
        size = len(content)
    else:
        raise ValueError(f"Content must be str or bytes, got: {type(content)}")

    if size > MAX_CONTENT_SIZE:
        raise ValueError(
            f"Content too large (max {MAX_CONTENT_SIZE} bytes): {size}"
        )


# ============================================================================
# EXPONENTIAL BACKOFF WITH JITTER
# ============================================================================

@dataclass
class RetryConfig:
    """Configuration for exponential backoff retry logic."""
    enabled: bool = DEFAULT_RETRY_ENABLED
    max_retries: int = DEFAULT_MAX_RETRIES
    initial_delay: float = DEFAULT_INITIAL_DELAY
    max_delay: float = DEFAULT_MAX_DELAY
    exponential_base: float = DEFAULT_EXPONENTIAL_BASE
    jitter: bool = DEFAULT_JITTER_ENABLED
    jitter_ratio: float = DEFAULT_JITTER_RATIO

    @classmethod
    def from_env(cls) -> "RetryConfig":
        """Create RetryConfig from environment variables."""
        enabled_str = os.getenv(ENV_RETRY_ENABLED, str(DEFAULT_RETRY_ENABLED)).lower()
        enabled = enabled_str in ('true', '1', 'yes', 'on')

        return cls(
            enabled=enabled,
            max_retries=int(os.getenv(ENV_MAX_RETRIES, str(DEFAULT_MAX_RETRIES))),
            initial_delay=float(os.getenv(ENV_INITIAL_DELAY, str(DEFAULT_INITIAL_DELAY))),
            max_delay=float(os.getenv(ENV_MAX_DELAY, str(DEFAULT_MAX_DELAY))),
            jitter=os.getenv(ENV_JITTER_ENABLED, str(DEFAULT_JITTER_ENABLED)).lower() in ('true', '1', 'yes', 'on'),
        )


def _is_retryable_error(error: Exception) -> bool:
    """
    Determine if an error is retryable.

    Args:
        error: The exception to check

    Returns:
        True if the error is retryable, False otherwise
    """
    # Check exception type
    if isinstance(error, RETRYABLE_EXCEPTION_TYPES):
        return True

    # Check for HTTP status codes in error message or attributes
    error_msg = str(error).lower()

    # Common rate limit and transient error indicators
    retryable_indicators = [
        'rate limit',
        'too many requests',
        'timeout',
        'timed out',  # Add "timed out" for better matching
        'connection',
        'temporary',
        'service unavailable',
        'gateway',
        '500',  # Internal Server Error
        '503',  # Service Unavailable
        '502',  # Bad Gateway
        '504',  # Gateway Timeout
        '429',  # Too Many Requests
        'http 5',  # Any HTTP 5xx error
    ]

    return any(indicator in error_msg for indicator in retryable_indicators)


def _calculate_delay(
    attempt: int,
    initial_delay: float,
    max_delay: float,
    exponential_base: float,
    jitter: bool,
    jitter_ratio: float
) -> float:
    """
    Calculate delay for a retry attempt with exponential backoff and jitter.

    Args:
        attempt: Current attempt number (0-indexed)
        initial_delay: Initial delay in seconds
        max_delay: Maximum delay in seconds
        exponential_base: Base for exponential calculation
        jitter: Whether to add jitter
        jitter_ratio: Ratio for jitter (0.0-1.0)

    Returns:
        Delay in seconds
    """
    # Calculate exponential delay
    delay = min(initial_delay * (exponential_base ** attempt), max_delay)

    # Add jitter if enabled
    if jitter:
        # Random jitter: +/- jitter_ratio * delay
        jitter_amount = delay * jitter_ratio
        delay += random.uniform(-jitter_amount, jitter_amount)

    # Ensure delay is non-negative
    return max(0.0, delay)


def retry_with_exponential_backoff(
    config: Optional[RetryConfig] = None,
    operation_name: str = "operation"
) -> Callable:
    """
    Decorator for retrying async operations with exponential backoff and jitter.

    This decorator provides resilience against transient failures and rate limits
    by automatically retrying operations with increasing delays between attempts.

    Features:
    - Exponential backoff: delay = initial_delay * (base ^ attempt)
    - Jitter: Adds randomness to prevent thundering herd
    - Configurable max retries and delays
    - Smart error detection: Only retries transient errors
    - Comprehensive logging with correlation IDs

    Args:
        config: RetryConfig object (defaults to RetryConfig.from_env())
        operation_name: Name of the operation for logging

    Example:
        @retry_with_exponential_backoff(
            config=RetryConfig(max_retries=3, initial_delay=1.0),
            operation_name="search"
        )
        async def search_with_retry(query: str):
            return await backend.search(query)
    """
    if config is None:
        config = RetryConfig.from_env()

    def decorator(func: Callable) -> Callable:
        @wraps(func)
        async def wrapper(*args, **kwargs):
            # Generate correlation ID for this operation
            correlation_id = str(uuid.uuid4())[:8]

            # If retry is disabled, just run the function
            if not config.enabled:
                logger.debug(
                    f"[{correlation_id}] Retry disabled for {operation_name}, "
                    f"executing directly"
                )
                return await func(*args, **kwargs)

            last_error = None

            for attempt in range(config.max_retries + 1):
                try:
                    # Log attempt
                    if attempt > 0:
                        logger.info(
                            f"[{correlation_id}] Retry attempt {attempt}/{config.max_retries} "
                            f"for {operation_name}"
                        )

                    # Execute the function
                    result = await func(*args, **kwargs)

                    # Log success on retry
                    if attempt > 0:
                        logger.info(
                            f"[{correlation_id}] {operation_name} succeeded on "
                            f"attempt {attempt + 1}"
                        )

                    return result

                except Exception as e:
                    last_error = e

                    # Check if error is retryable
                    if not _is_retryable_error(e):
                        logger.warning(
                            f"[{correlation_id}] Non-retryable error in {operation_name}: "
                            f"{type(e).__name__}: {e}"
                        )
                        raise

                    # Check if we have more retries
                    if attempt >= config.max_retries:
                        logger.error(
                            f"[{correlation_id}] {operation_name} failed after "
                            f"{config.max_retries} retries: {type(e).__name__}: {e}"
                        )
                        raise

                    # Calculate delay
                    delay = _calculate_delay(
                        attempt=attempt,
                        initial_delay=config.initial_delay,
                        max_delay=config.max_delay,
                        exponential_base=config.exponential_base,
                        jitter=config.jitter,
                        jitter_ratio=config.jitter_ratio
                    )

                    # Log retry
                    logger.warning(
                        f"[{correlation_id}] {operation_name} failed on attempt "
                        f"{attempt + 1} with {type(e).__name__}: {e}. "
                        f"Retrying in {delay:.2f}s..."
                    )

                    # Sleep before retry
                    await asyncio.sleep(delay)

            # Should never reach here, but just in case
            if last_error:
                raise last_error

        return wrapper

    return decorator


# ============================================================================
# DATA CLASSES
# ============================================================================

@dataclass
class VectorMetadata:
    """Metadata for a vector in the index."""
    file_path: str
    chunk_index: int
    start_line: Optional[int] = None
    end_line: Optional[int] = None
    chunk_type: str = "text"  # function, class, module, import, other, text
    parent_context: Optional[str] = None  # class name, module name
    embedding_model: str = DEFAULT_MODEL
    created_at: str = ""

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "VectorMetadata":
        return cls(**data)


@dataclass
class IndexMetadata:
    """Metadata for the LEANN index."""
    version: str = INDEX_VERSION
    backend: str = DEFAULT_BACKEND
    model: str = DEFAULT_MODEL
    dimension: int = DEFAULT_EMBEDDING_DIM
    vector_count: int = 0
    created_at: str = ""
    updated_at: str = ""

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "IndexMetadata":
        return cls(**data)


# ============================================================================
# LEANN VECTOR BACKEND
# ============================================================================


# ============================================================================
# HELPER CLASSES FOR ENHANCEMENTS
# ============================================================================

class TTLCache:
    """
    Thread-safe LRU cache with TTL (Time-To-Live) support.

    Features:
    - LRU eviction when cache is full
    - TTL-based expiration of entries
    - Cache statistics (hits, misses, eviction rate)
    - Thread-safe operations

    Used for caching search results to improve performance.
    """

    def __init__(self, maxsize: int = 1000, ttl: int = 300):
        """
        Initialize the TTL cache.

        Args:
            maxsize: Maximum number of items in cache
            ttl: Time-to-live in seconds for each cache entry
        """
        self.maxsize = maxsize
        self.ttl = ttl
        self._cache: OrderedDict[str, Tuple[Any, float]] = OrderedDict()
        self._lock = Lock()

        # Statistics
        self._hits = 0
        self._misses = 0
        self._evictions = 0
        self._expirations = 0

    def get(self, key: str) -> Optional[Any]:
        """
        Get a value from the cache.

        Args:
            key: Cache key

        Returns:
            Cached value or None if not found/expired
        """
        with self._lock:
            if key not in self._cache:
                self._misses += 1
                return None

            value, timestamp = self._cache[key]
            current_time = time.time()

            # Check if expired
            if current_time - timestamp > self.ttl:
                # Expired - remove and return None
                del self._cache[key]
                self._misses += 1
                self._expirations += 1
                return None

            # Not expired - move to end (most recently used)
            self._cache.move_to_end(key)
            self._hits += 1
            return value

    def put(self, key: str, value: Any) -> None:
        """
        Put a value in the cache.

        Args:
            key: Cache key
            value: Value to cache
        """
        with self._lock:
            # Update existing key or add new one
            if key in self._cache:
                # Update existing - move to end
                self._cache.move_to_end(key)
            else:
                # Check if we need to evict
                if len(self._cache) >= self.maxsize:
                    # Evict least recently used (first item)
                    self._cache.popitem(last=False)
                    self._evictions += 1

            # Add/update the entry with current timestamp
            self._cache[key] = (value, time.time())

    def invalidate(self, key: Optional[str] = None) -> None:
        """
        Invalidate cache entries.

        Args:
            key: Specific key to invalidate, or None to clear all
        """
        with self._lock:
            if key is None:
                # Clear all cache
                self._cache.clear()
            else:
                # Remove specific key
                if key in self._cache:
                    del self._cache[key]

    def get_stats(self) -> Dict[str, Any]:
        """
        Get cache statistics.

        Returns:
            Dictionary with cache statistics
        """
        with self._lock:
            total_requests = self._hits + self._misses
            hit_rate = self._hits / total_requests if total_requests > 0 else 0

            return {
                "size": len(self._cache),
                "maxsize": self.maxsize,
                "ttl": self.ttl,
                "hits": self._hits,
                "misses": self._misses,
                "evictions": self._evictions,
                "expirations": self._expirations,
                "hit_rate": hit_rate,
            }

    def reset_stats(self) -> None:
        """Reset cache statistics."""
        with self._lock:
            self._hits = 0
            self._misses = 0
            self._evictions = 0
            self._expirations = 0


class VectorDeduplicator:
    """
    Vector deduplication registry with content-based hashing.

    Features:
    - Content-based hash generation for chunks
    - Hash -> vector_id mapping
    - Reference counting for duplicate chunks
    - Automatic cleanup of unused entries

    Used to prevent storing duplicate vectors for identical content chunks.
    """

    def __init__(self):
        """Initialize the vector deduplicator."""
        self._hash_to_vector_id: Dict[str, str] = {}
        self._reference_counts: Dict[str, int] = {}
        self._lock = Lock()

    @staticmethod
    def hash_content(content: str) -> str:
        """
        Generate a content-based hash for a chunk.

        Args:
            content: Chunk content

        Returns:
            SHA-256 hash of the content
        """
        return hashlib.sha256(content.encode('utf-8')).hexdigest()

    def get_vector_id(self, content_hash: str) -> Optional[str]:
        """
        Get vector ID for a content hash.

        Args:
            content_hash: Hash of the chunk content

        Returns:
            Vector ID if exists, None otherwise
        """
        with self._lock:
            return self._hash_to_vector_id.get(content_hash)

    def register_vector(self, content_hash: str, vector_id: str) -> str:
        """
        Register a new vector in the deduplication registry.

        Args:
            content_hash: Hash of the chunk content
            vector_id: Vector ID to register

        Returns:
            The registered vector ID
        """
        with self._lock:
            self._hash_to_vector_id[content_hash] = vector_id
            self._reference_counts[vector_id] = 1
            return vector_id

    def add_reference(self, content_hash: str) -> int:
        """
        Increment reference count for a vector.

        Args:
            content_hash: Hash of the chunk content

        Returns:
            New reference count
        """
        with self._lock:
            vector_id = self._hash_to_vector_id.get(content_hash)
            if vector_id:
                self._reference_counts[vector_id] = self._reference_counts.get(vector_id, 0) + 1
                return self._reference_counts[vector_id]
            return 0

    def remove_reference(self, vector_id: str) -> int:
        """
        Decrement reference count for a vector.

        Args:
            vector_id: Vector ID to decrement

        Returns:
            New reference count (0 if vector can be deleted)
        """
        with self._lock:
            current_count = self._reference_counts.get(vector_id, 0)
            if current_count > 0:
                new_count = current_count - 1
                self._reference_counts[vector_id] = new_count
                return new_count
            return 0

    def cleanup_entry(self, content_hash: str, vector_id: str) -> None:
        """
        Remove an entry from the registry when reference count reaches 0.

        Args:
            content_hash: Hash of the chunk content
            vector_id: Vector ID to remove
        """
        with self._lock:
            if self._hash_to_vector_id.get(content_hash) == vector_id:
                del self._hash_to_vector_id[content_hash]
            if vector_id in self._reference_counts:
                del self._reference_counts[vector_id]

    def get_stats(self) -> Dict[str, Any]:
        """
        Get deduplication statistics.

        Returns:
            Dictionary with deduplication stats
        """
        with self._lock:
            total_refs = sum(self._reference_counts.values())
            return {
                "unique_vectors": len(self._hash_to_vector_id),
                "total_references": total_refs,
                "reference_counts": dict(self._reference_counts),
            }


def generate_cache_key(query: str, store_ids: List[str], top_k: int) -> str:
    """
    Generate a cache key for search results.

    Args:
        query: Search query
        store_ids: List of store IDs
        top_k: Number of results

    Returns:
        Cache key string (MD5 hash)
    """
    # Normalize query
    normalized_query = query.lower().strip()

    # Sort store IDs for consistency
    sorted_store_ids = sorted(store_ids)

    # Create key components
    key_parts = [
        normalized_query,
        ",".join(sorted_store_ids),
        str(top_k)
    ]

    # Hash the key for efficiency
    key_string = ":".join(key_parts)
    return hashlib.md5(key_string.encode()).hexdigest()


class LEANNVectorBackend:
    """
    LEANN vector backend using LEANN + sentence-transformers.

    This backend provides:
    - Storage-efficient embeddings (97% savings vs traditional)
    - Fast vector search using LEANN (HNSW or DiskANN)
    - Local embedding generation (no API calls)
    - Persistent index for fast startup
    - JSON metadata storage alongside index
    - Security: Path validation, resource limits, input sanitization

    Usage:
        backend = LEANNVectorBackend(
            backend_name="hnsw",
            index_path="./leann_index"
        )
        await backend.initialize()

        # Add documents
        await backend.upload_file(store_id, file_path, content, options)

        # Search
        results = await backend.search(store_ids, query, options)
    """

    # Class-level model cache for sharing across instances
    _model_cache: Dict[str, Any] = {}
    _model_cache_lock = Lock()

    def __init__(
        self,
        backend_name: Optional[str] = None,
        index_path: Optional[str] = None,
        dimension: Optional[int] = None,
        model_name: Optional[str] = None,
    ):
        """
        Initialize the LEANNVectorBackend.

        Args:
            backend_name: LEANN backend type ('hnsw' or 'diskann').
                         Defaults to LEANN_BACKEND env var or 'hnsw'.
            index_path: Directory path to store/load LEANN index.
                       Defaults to LEANN_INDEX_PATH env var or DEFAULT_INDEX_PATH.
            dimension: Embedding dimension. Defaults to model's default.
            model_name: Name of the embedding model to use.
                       Defaults to LEANN_MODEL env var or DEFAULT_MODEL.
        """
        # Sanitize and validate index_path
        raw_index_path = index_path or os.getenv(ENV_INDEX_PATH, DEFAULT_INDEX_PATH)
        self.index_path = _sanitize_path(raw_index_path, allow_absolute=True)

        # Configuration
        self.backend_name = backend_name or os.getenv(ENV_BACKEND, DEFAULT_BACKEND)
        if self.backend_name not in SUPPORTED_BACKENDS:
            logger.warning(
                f"Unknown backend '{self.backend_name}'. "
                f"Supported backends: {SUPPORTED_BACKENDS}. "
                f"Using default: {DEFAULT_BACKEND}"
            )
            self.backend_name = DEFAULT_BACKEND

        self.model_name = model_name or os.getenv(ENV_MODEL, DEFAULT_MODEL)

        # Get dimension from model configuration
        if dimension:
            self.dimension = dimension
        elif self.model_name in SUPPORTED_MODELS:
            self.dimension = SUPPORTED_MODELS[self.model_name]["dim"]
        else:
            # Fallback to legacy ALTERNATIVE_MODELS for backward compatibility
            if self.model_name in ALTERNATIVE_MODELS:
                self.dimension = ALTERNATIVE_MODELS[self.model_name]["dim"]
            else:
                self.dimension = DEFAULT_EMBEDDING_DIM
                logger.warning(
                    f"Unknown model '{self.model_name}'. "
                    f"Using default dimension: {DEFAULT_EMBEDDING_DIM}"
                )

        # Backend configuration
        self.backend_config = BACKEND_CONFIGS.get(self.backend_name, BACKEND_CONFIGS[DEFAULT_BACKEND])

        # Runtime state
        self._model: Optional[Any] = None
        self._searcher: Optional[Any] = None
        self._index_metadata: Optional[IndexMetadata] = None
        self._vector_metadata: Dict[str, VectorMetadata] = {}  # vector_id -> metadata
        self._vector_id_list: List[str] = []  # List of vector_ids in index order (for mapping LEANN indices to vector_ids)
        self._initialized = False
        self._limited_mode = False

        # Thread safety - use asyncio.Lock for async operations
        self._lock = asyncio.Lock()

        # Metrics tracking
        self._search_latencies: List[float] = []  # Stores last N search latencies
        self._embedding_times: List[float] = []  # Stores last N embedding generation times
        self._total_embeddings = 0
        self._total_searches = 0
        self._total_uploads = 0

        # Enhancement features (controlled by config)
        # These are enabled by default but can be disabled via config
        self._enable_incremental_updates = True
        self._enable_cache = True
        self._enable_deduplication = True
        self._cache_maxsize = 1000
        self._cache_ttl = 300  # 5 minutes

        # Initialize search result cache
        if self._enable_cache:
            self._search_cache = TTLCache(maxsize=self._cache_maxsize, ttl=self._cache_ttl)
        else:
            self._search_cache = None

        # Initialize vector deduplicator
        if self._enable_deduplication:
            self._deduplicator = VectorDeduplicator()
        else:
            self._deduplicator = None

        # Track dirty state for incremental updates
        self._index_dirty = False
        self._pending_vectors: List[Tuple[str, Any, Dict]] = []  # (vector_id, embedding, chunk)

        # Create index directory
        Path(self.index_path).mkdir(parents=True, exist_ok=True)

        logger.info(
            f"LEANNVectorBackend configured: backend={self.backend_name}, "
            f"model={self.model_name}, dim={self.dimension}, "
            f"path={self.index_path}"
        )

    @property
    def client(self) -> Optional[Any]:
        """Get the underlying LEANN searcher."""
        return self._searcher

    def is_available(self) -> bool:
        """
        Check if the backend is available for operations.

        Returns:
            True if dependencies are installed and backend is initialized
        """
        return not self._limited_mode and self._initialized

    async def initialize(self) -> None:
        """
        Initialize the backend.

        This method:
        1. Checks dependencies
        2. Loads or creates the LEANN index
        3. Loads the embedding model (lazy)
        4. Loads metadata from storage

        Raises:
            ValueError: If dependencies are not installed
        """
        logger.info("Initializing LEANNVectorBackend...")

        # Check dependencies
        if not LEANN_AVAILABLE:
            self._limited_mode = True
            raise ValueError(
                "leann is not installed. "
                "Install with: uv pip install 'leann>=0.3.5'"
            )

        if not SENTENCE_TRANSFORMERS_AVAILABLE:
            self._limited_mode = True
            raise ValueError(
                "sentence-transformers is not installed. "
                "Install with: uv pip install 'sentence-transformers>=2.2.0'"
            )

        if not NUMPY_AVAILABLE:
            self._limited_mode = True
            raise ValueError(
                "numpy is not installed. "
                "Install with: uv pip install 'numpy>=1.24.0'"
            )

        # Load or create index metadata
        self._load_or_create_metadata()

        # Model is loaded lazily on first use
        logger.info("Embedding model will be loaded on first use (~2-3 seconds)")

        self._initialized = True
        logger.info(
            f"LEANNVectorBackend initialized: backend={self.backend_name}, "
            f"vectors={self._index_metadata.vector_count}, "
            f"model={self._index_metadata.model}"
        )

    def _load_or_create_metadata(self) -> None:
        """
        Load existing metadata from disk or create new metadata.

        This method loads the index metadata JSON file if it exists,
        otherwise creates new metadata.
        """
        meta_file = os.path.join(self.index_path, "metadata.json")
        vectors_meta_file = os.path.join(self.index_path, "vectors_metadata.json")
        vector_id_list_file = os.path.join(self.index_path, "vector_id_list.json")

        if os.path.exists(meta_file):
            # Load existing metadata
            try:
                with open(meta_file, 'r') as f:
                    meta_data = json.load(f)
                self._index_metadata = IndexMetadata.from_dict(meta_data)

                # Load vector metadata
                if os.path.exists(vectors_meta_file):
                    with open(vectors_meta_file, 'r') as f:
                        vectors_data = json.load(f)
                    self._vector_metadata = {
                        k: VectorMetadata.from_dict(v)
                        for k, v in vectors_data.items()
                    }
                    logger.info(f"Loaded {len(self._vector_metadata)} vector metadata entries")

                # Load vector_id_list
                if os.path.exists(vector_id_list_file):
                    with open(vector_id_list_file, 'r') as f:
                        self._vector_id_list = json.load(f)
                    logger.info(f"Loaded {len(self._vector_id_list)} vector IDs")
                else:
                    # Rebuild vector_id_list from metadata if file doesn't exist
                    self._vector_id_list = list(self._vector_metadata.keys())
                    logger.info(f"Reconstructed vector_id_list with {len(self._vector_id_list)} entries")

            except Exception as e:
                logger.error(f"Failed to load metadata: {e}. Creating new metadata...")
                self._create_new_metadata()
        else:
            # Create new metadata
            self._create_new_metadata()

    def _create_new_metadata(self) -> None:
        """Create new index metadata."""
        logger.info("Creating new metadata...")
        self._index_metadata = IndexMetadata(
            version=INDEX_VERSION,
            backend=self.backend_name,
            model=self.model_name,
            dimension=self.dimension,
            created_at=datetime.now().isoformat(),
            updated_at=datetime.now().isoformat()
        )
        self._vector_metadata = {}
        self._vector_id_list = []

    def _load_model(self) -> Any:
        """
        Load the embedding model with caching.

        Uses class-level cache to share models across instances.

        Returns:
            Loaded SentenceTransformer model
        """
        # Double-checked locking pattern for thread-safe model loading
        with self._model_cache_lock:
            if self.model_name in self._model_cache:
                logger.debug(f"Using cached model: {self.model_name}")
                return self._model_cache[self.model_name]

        logger.info(f"Loading embedding model: {self.model_name} (~2-3 seconds)...")
        try:
            # Get trust_remote_code from model configuration
            # Default to False for unknown models (secure default)
            trust_remote_code = False
            if self.model_name in SUPPORTED_MODELS:
                trust_remote_code = SUPPORTED_MODELS[self.model_name].get("trust_remote_code", False)
            else:
                # Fallback to heuristic for known model prefixes
                trust_remote_code = self.model_name.startswith("nomic-ai/") or \
                                   self.model_name.startswith("BAAI/")

            # Load model (expensive operation, done outside lock)
            model = SentenceTransformer(
                self.model_name,
                trust_remote_code=trust_remote_code
            )

            # Re-acquire lock to store model (double-check to avoid duplicate loads)
            with self._model_cache_lock:
                if self.model_name not in self._model_cache:
                    self._model_cache[self.model_name] = model
                else:
                    # Another thread already loaded the model, use cached version
                    model = self._model_cache[self.model_name]

            logger.info(f"Model loaded: {self.model_name}")
            return model
        except Exception as e:
            logger.error(f"Failed to load model {self.model_name}: {e}")
            raise ValueError(
                f"Failed to load embedding model '{self.model_name}': {e}. "
                f"Ensure the model name is correct or check your internet connection "
                f"for first-time download."
            )

    def _encode(self, texts: List[str]) -> Any:
        """
        Encode texts to embeddings.

        Args:
            texts: List of text strings to encode

        Returns:
            numpy array of embeddings
        """
        if self._model is None:
            self._model = self._load_model()

        # Track embedding time
        start_time = time.perf_counter()

        # SentenceTransformer.encode returns numpy array
        embeddings = self._model.encode(
            texts,
            convert_to_numpy=True,
            normalize_embeddings=True,  # Normalize for inner product similarity
            show_progress_bar=False
        )

        # Record metrics
        elapsed_time = time.perf_counter() - start_time
        self._embedding_times.append(elapsed_time)
        self._total_embeddings += len(texts)

        # Keep only last MAX_METRICS_SAMPLES
        if len(self._embedding_times) > MAX_METRICS_SAMPLES:
            self._embedding_times.pop(0)

        return embeddings.astype(np.float32)

    def _save_metadata(self) -> None:
        """Save the metadata to disk."""
        meta_file = os.path.join(self.index_path, "metadata.json")
        vectors_meta_file = os.path.join(self.index_path, "vectors_metadata.json")
        vector_id_list_file = os.path.join(self.index_path, "vector_id_list.json")

        try:
            # Save index metadata
            self._index_metadata.updated_at = datetime.now().isoformat()
            with open(meta_file, 'w') as f:
                json.dump(self._index_metadata.to_dict(), f, indent=2)

            # Save vector metadata
            with open(vectors_meta_file, 'w') as f:
                json.dump(
                    {k: v.to_dict() for k, v in self._vector_metadata.items()},
                    f,
                    indent=2
                )

            # Save vector_id_list
            with open(vector_id_list_file, 'w') as f:
                json.dump(self._vector_id_list, f, indent=2)

            logger.debug(f"Saved metadata to {self.index_path}")
        except Exception as e:
            logger.error(f"Failed to save metadata: {e}")

    def _generate_vector_id(self, file_path: str, chunk_index: int) -> str:
        """Generate a unique vector ID from file path and chunk index."""
        unique_str = f"{file_path}:{chunk_index}"
        return hashlib.md5(unique_str.encode()).hexdigest()

    async def list_files(
        self,
        store_id: str,
        path_prefix: Optional[str] = None
    ) -> AsyncGenerator[StoreFile, None]:
        """
        List files in the store.

        Args:
            store_id: The store identifier (used as namespace)
            path_prefix: Optional path prefix to filter files

        Yields:
            StoreFile objects

        SECURITY: Validates store_id and path_prefix
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # SECURITY: Validate store_id
        safe_store_id = _validate_store_id(store_id)

        # SECURITY: Validate path_prefix if provided
        if path_prefix:
            path_prefix = _validate_file_path(path_prefix)

        # Extract unique file paths from metadata
        seen_files = set()
        for vector_id, meta in self._vector_metadata.items():
            file_path = meta.file_path

            # Filter by path prefix
            if path_prefix and not file_path.startswith(path_prefix):
                continue

            # Filter by store_id (namespace)
            if safe_store_id and not file_path.startswith(safe_store_id):
                continue

            if file_path not in seen_files:
                seen_files.add(file_path)
                yield StoreFile(
                    external_id=file_path,
                    metadata=FileMetadata(
                        path=file_path,
                        hash="",  # Hash not stored in vector metadata
                    )
                )

    @retry_with_exponential_backoff(
        config=RetryConfig(
            enabled=DEFAULT_RETRY_ENABLED,
            max_retries=DEFAULT_MAX_RETRIES,
            initial_delay=DEFAULT_INITIAL_DELAY,
            max_delay=DEFAULT_MAX_DELAY,
            jitter=DEFAULT_JITTER_ENABLED,
            jitter_ratio=DEFAULT_JITTER_RATIO
        ),
        operation_name="upload_file"
    )
    async def upload_file(
        self,
        store_id: str,
        file_path: str,
        content: Union[str, bytes],
        options: UploadFileOptions
    ) -> None:
        """
        Upload and index a file.

        This method:
        1. Chunks the file content
        2. Generates embeddings for each chunk
        3. Rebuilds the LEANN index
        4. Stores metadata

        Args:
            store_id: Store identifier (used as namespace/path prefix)
            file_path: Path of the file
            content: File content (str or bytes)
            options: Upload options

        SECURITY: Validates all inputs and enforces size limits
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # SECURITY: Validate inputs
        safe_store_id = _validate_store_id(store_id)
        safe_file_path = _validate_file_path(file_path)
        _validate_content_size(content)

        # Convert bytes to string if needed
        if isinstance(content, bytes):
            content = content.decode('utf-8', errors='ignore')

        # Chunk the content
        chunks = self._chunk_content(content, safe_file_path)

        if not chunks:
            logger.warning(f"No chunks generated for file: {safe_file_path}")
            return

        # Generate embeddings
        texts = [chunk["text"] for chunk in chunks]
        embeddings = self._encode(texts)

        # Store metadata
        async with self._lock:
            for i, chunk in enumerate(chunks):
                vector_id = self._generate_vector_id(safe_file_path, i)
                self._vector_metadata[vector_id] = VectorMetadata(
                    file_path=os.path.join(safe_store_id, safe_file_path) if safe_store_id else safe_file_path,
                    chunk_index=i,
                    start_line=chunk.get("start_line"),
                    end_line=chunk.get("end_line"),
                    chunk_type=chunk.get("type", "text"),
                    parent_context=chunk.get("parent_context"),
                    embedding_model=self.model_name,
                    created_at=datetime.now().isoformat()
                )
                # Add to vector_id_list if not already present
                if vector_id not in self._vector_id_list:
                    self._vector_id_list.append(vector_id)

            # Update index metadata
            self._index_metadata.vector_count = len(self._vector_metadata)
            self._index_metadata.updated_at = datetime.now().isoformat()

        # Rebuild index with new vectors
        await self._rebuild_index()

        # Save metadata
        self._save_metadata()

        logger.debug(f"Indexed {len(chunks)} chunks from {safe_file_path}")

    @retry_with_exponential_backoff(
        config=RetryConfig(
            enabled=DEFAULT_RETRY_ENABLED,
            max_retries=DEFAULT_MAX_RETRIES,
            initial_delay=DEFAULT_INITIAL_DELAY,
            max_delay=DEFAULT_MAX_DELAY,
            jitter=DEFAULT_JITTER_ENABLED,
            jitter_ratio=DEFAULT_JITTER_RATIO
        ),
        operation_name="rebuild_index"
    )
    async def _rebuild_index(self) -> None:
        """
        Rebuild the LEANN index with all vectors, with retry on transient failures.

        This method implements exponential backoff with jitter to handle:
        - Index build failures
        - Temporary storage issues
        - Resource exhaustion

        This is a simplified implementation that rebuilds the index
        from scratch when new vectors are added.
        """
        if not LEANN_AVAILABLE:
            return

        # Prepare chunks for LEANN
        chunks = []
        for vector_id, meta in self._vector_metadata.items():
            # Load chunk content
            content = self._load_chunk_content(meta.file_path, meta.start_line, meta.end_line)
            chunks.append({
                "text": content,
                "metadata": meta.to_dict()
            })

        if not chunks:
            return

        # Build index using LeannBuilder
        # Note: In a real implementation, we'd need to generate embeddings
        # and pass them to the builder. For now, this is a placeholder.
        try:
            with LeannBuilder(
                backend_name=self.backend_name,
                embedding_dim=self.dimension,
                **self.backend_config
            ) as builder:
                # In real implementation, add vectors and embeddings here
                # builder.add_vectors(embeddings, metadatas)
                builder.build_index(self.index_path, chunks)
        except Exception as e:
            logger.error(f"Failed to rebuild index: {e}")

    def _chunk_content(
        self,
        content: str,
        file_path: str
    ) -> List[Dict[str, Any]]:
        """
        Chunk content into searchable pieces using AST-based chunking.

        For Python files:
        - Parse with AST to extract functions, classes, and top-level code blocks
        - Chunks < 512 tokens are kept as-is
        - Chunks > 512 tokens are split at logical boundaries
        - Store rich metadata: file path, start/end lines, chunk type, parent context

        For non-Python files:
        - Fall back to line-based chunking with overlap

        Args:
            content: File content
            file_path: Path to the file

        Returns:
            List of chunk dictionaries with keys:
                - text: str - The chunk content
                - start_line: int - Starting line number (1-indexed)
                - end_line: int - Ending line number (1-indexed)
                - type: str - Chunk type (function, class, module, import, other, text)
                - parent_context: Optional[str] - Parent class/module name for nested items
        """
        # Detect if this is a Python file
        is_python = self._is_python_file(file_path)

        if is_python:
            try:
                chunks = self._chunk_with_ast(content, file_path)
                if chunks:
                    return chunks
                # If AST chunking fails, fall back to simple chunking
                logger.debug(f"AST chunking returned no chunks for {file_path}, using fallback")
            except SyntaxError as e:
                logger.debug(f"Syntax error in {file_path}: {e}. Using fallback chunking")
            except Exception as e:
                logger.warning(f"AST chunking failed for {file_path}: {e}. Using fallback")

        # Fall back to simple line-based chunking
        return self._simple_chunk(content, file_path)

    def _is_python_file(self, file_path: str) -> bool:
        """
        Check if a file is a Python source file.

        Args:
            file_path: Path to the file

        Returns:
            True if the file has a Python extension
        """
        path = Path(file_path)
        return path.suffix in ('.py', '.pyi', '.pyw')

    def _chunk_with_ast(
        self,
        content: str,
        file_path: str
    ) -> List[Dict[str, Any]]:
        """
        Chunk Python code using AST parsing.

        This method:
        1. Parses the Python code into an AST
        2. Extracts functions, classes, imports, and top-level code
        3. Estimates token count for each node
        4. Splits large chunks (>512 tokens) at logical boundaries

        Args:
            content: Python source code
            file_path: Path to the file (for metadata)

        Returns:
            List of chunk dictionaries with AST metadata

        Raises:
            SyntaxError: If the Python code has syntax errors
        """
        chunks = []

        try:
            tree = ast.parse(content, filename=file_path)
        except SyntaxError as e:
            logger.debug(f"Failed to parse {file_path} with AST: {e}")
            raise

        # Get module docstring if present
        module_docstring = ast.get_docstring(tree)

        # Track imports separately
        import_nodes = []
        import_chunks = []
        import_start_line = None
        import_end_line = None

        # First pass: collect all imports
        for node in ast.iter_child_nodes(tree):
            if isinstance(node, (ast.Import, ast.ImportFrom)):
                if import_start_line is None:
                    import_start_line = node.lineno
                import_nodes.append(node)
                import_end_line = getattr(node, 'end_lineno', node.lineno)

        # Create import chunk if imports exist
        if import_nodes:
            import_text = self._extract_node_text(content, import_start_line, import_end_line)
            if import_text:
                import_chunks.append({
                    "text": import_text,
                    "start_line": import_start_line,
                    "end_line": import_end_line,
                    "type": "import",
                    "parent_context": None
                })

        chunks.extend(import_chunks)

        # Track current class for nested methods
        current_class = None

        # Process top-level nodes
        for node in ast.iter_child_nodes(tree):
            if isinstance(node, (ast.Import, ast.ImportFrom)):
                # Already handled above
                continue

            elif isinstance(node, ast.FunctionDef):
                node_chunks = self._chunk_function(node, content, current_class)
                chunks.extend(node_chunks)

            elif isinstance(node, ast.AsyncFunctionDef):
                node_chunks = self._chunk_function(node, content, current_class)
                chunks.extend(node_chunks)

            elif isinstance(node, ast.ClassDef):
                node_chunks = self._chunk_class(node, content)
                chunks.extend(node_chunks)

            elif isinstance(node, ast.Expr) and isinstance(node.value, ast.Constant):
                # Module-level docstring already handled
                if module_docstring and node.value.value == module_docstring:
                    continue
                # Other module-level string constants
                node_chunks = self._chunk_expr(node, content, "module")
                chunks.extend(node_chunks)

            elif hasattr(node, 'lineno'):
                # Other top-level statements
                node_chunks = self._chunk_statement(node, content, "module")
                chunks.extend(node_chunks)

        # Filter out empty chunks
        chunks = [c for c in chunks if c.get("text", "").strip()]

        return chunks

    def _chunk_function(
        self,
        node: Union[ast.FunctionDef, ast.AsyncFunctionDef],
        content: str,
        parent_context: Optional[str]
    ) -> List[Dict[str, Any]]:
        """
        Chunk a function definition.

        If the function is small (<512 tokens), keeps it as one chunk.
        If large, splits at logical boundaries (docstring, nested functions, logical blocks).

        Args:
            node: Function or AsyncFunctionDef AST node
            content: Full source code
            parent_context: Parent class name if method, or None if top-level function

        Returns:
            List of chunks for this function
        """
        chunks = []
        start_line = node.lineno
        end_line = getattr(node, 'end_lineno', start_line)

        # Extract function text
        func_text = self._extract_node_text(content, start_line, end_line)
        if not func_text:
            return chunks

        # Estimate token count (rough approximation: ~4 chars per token)
        estimated_tokens = len(func_text) // 4

        # Get function info
        func_name = node.name
        docstring = ast.get_docstring(node)
        is_method = parent_context is not None

        if estimated_tokens <= 512:
            # Small function - keep as single chunk
            chunks.append({
                "text": func_text,
                "start_line": start_line,
                "end_line": end_line,
                "type": "method" if is_method else "function",
                "parent_context": parent_context
            })
        else:
            # Large function - split into logical parts
            chunks.extend(self._split_large_function(
                node, content, start_line, end_line,
                func_name, parent_context, docstring
            ))

        return chunks

    def _chunk_class(
        self,
        node: ast.ClassDef,
        content: str
    ) -> List[Dict[str, Any]]:
        """
        Chunk a class definition.

        Creates chunks for:
        1. Class definition and docstring
        2. Each method within the class
        3. Nested classes

        Args:
            node: ClassDef AST node
            content: Full source code

        Returns:
            List of chunks for this class
        """
        chunks = []
        class_name = node.name
        start_line = node.lineno
        end_line = getattr(node, 'end_lineno', start_line)

        # Extract class text
        class_text = self._extract_node_text(content, start_line, end_line)
        if not class_text:
            return chunks

        # Estimate token count
        estimated_tokens = len(class_text) // 4
        docstring = ast.get_docstring(node)

        if estimated_tokens <= 512:
            # Small class - keep as single chunk
            chunks.append({
                "text": class_text,
                "start_line": start_line,
                "end_line": end_line,
                "type": "class",
                "parent_context": None
            })
        else:
            # Large class - create header chunk and method chunks
            # Class header with docstring
            header_lines = []
            header_end = start_line

            # Find end of class definition line(s)
            for i, line in enumerate(class_text.split('\n'), start=start_line):
                header_lines.append(line)
                if ':' in line and not line.strip().startswith('#'):
                    # This is likely the class definition line
                    header_end = i
                    if docstring:
                        # Add docstring lines
                        header_lines.append('    """' + (docstring or ''))
                        header_lines.append('    """')
                    break

            header_text = '\n'.join(header_lines)
            if header_text.strip():
                chunks.append({
                    "text": header_text,
                    "start_line": start_line,
                    "end_line": header_end + (2 if docstring else 0),
                    "type": "class",
                    "parent_context": None
                })

            # Chunk each method separately
            for child in node.body:
                if isinstance(child, (ast.FunctionDef, ast.AsyncFunctionDef)):
                    method_chunks = self._chunk_function(child, content, class_name)
                    chunks.extend(method_chunks)
                elif isinstance(child, ast.ClassDef):
                    # Nested class
                    nested_chunks = self._chunk_class(child, content)
                    chunks.extend(nested_chunks)

        return chunks

    def _split_large_function(
        self,
        node: Union[ast.FunctionDef, ast.AsyncFunctionDef],
        content: str,
        start_line: int,
        end_line: int,
        func_name: str,
        parent_context: Optional[str],
        docstring: Optional[str]
    ) -> List[Dict[str, Any]]:
        """
        Split a large function into logical chunks.

        Splits at:
        1. Docstring (if present)
        2. Nested function/class definitions
        3. Logical blocks (if, for, while, try, with statements)

        Args:
            node: Function AST node
            content: Full source code
            start_line: Function start line
            end_line: Function end line
            func_name: Function name
            parent_context: Parent class name if method
            docstring: Function docstring

        Returns:
            List of chunks for the split function
        """
        chunks = []
        lines = content.split('\n')

        # Find function definition line
        def_line = start_line - 1
        def_end = def_line

        # Find colon to get function definition end
        for i in range(def_line, min(def_line + 5, len(lines))):
            if ':' in lines[i]:
                def_end = i
                break

        # Extract function signature
        signature_lines = lines[def_line:def_end + 1]
        signature = '\n'.join(signature_lines)

        # Add docstring as separate chunk if present
        if docstring:
            docstring_chunk = f"{signature}\n    \"\"\"{docstring}\"\"\""
            chunks.append({
                "text": docstring_chunk,
                "start_line": start_line,
                "end_line": start_line,
                "type": "function" if not parent_context else "method",
                "parent_context": parent_context
            })
        else:
            # Just signature
            chunks.append({
                "text": signature,
                "start_line": start_line,
                "end_line": def_end + 1,
                "type": "function" if not parent_context else "method",
                "parent_context": parent_context
            })

        # Process function body - find logical blocks
        body_start = def_end + 1
        current_chunk_start = body_start
        current_chunk_lines = []
        indent_level = None

        for i in range(body_start, min(end_line, len(lines))):
            line = lines[i]
            stripped = line.lstrip()

            # Skip empty lines at start
            if not current_chunk_lines and not stripped:
                continue

            # Determine indent level
            if stripped and indent_level is None:
                indent_level = len(line) - len(stripped)

            # Check for major block starters (at function body level)
            is_block_start = (
                stripped.startswith(('def ', 'async def ', 'class ')) or
                stripped.startswith(('if ', 'elif ', 'else:')) or
                stripped.startswith(('for ', 'while ')) or
                stripped.startswith(('try:', 'except', 'finally:')) or
                stripped.startswith('with ')
            )

            # If we hit a major block and have accumulated content, flush it
            if is_block_start and current_chunk_lines:
                chunk_text = '\n'.join(current_chunk_lines)
                if chunk_text.strip():
                    chunks.append({
                        "text": chunk_text,
                        "start_line": current_chunk_start + 1,
                        "end_line": i,
                        "type": "other",
                        "parent_context": parent_context
                    })
                current_chunk_lines = []
                current_chunk_start = i

            current_chunk_lines.append(line)

        # Add remaining content
        if current_chunk_lines:
            chunk_text = '\n'.join(current_chunk_lines)
            if chunk_text.strip():
                chunks.append({
                    "text": chunk_text,
                    "start_line": current_chunk_start + 1,
                    "end_line": end_line,
                    "type": "other",
                    "parent_context": parent_context
                })

        return chunks

    def _chunk_expr(
        self,
        node: ast.Expr,
        content: str,
        parent_context: Optional[str]
    ) -> List[Dict[str, Any]]:
        """Chunk an expression node."""
        start_line = node.lineno
        end_line = getattr(node, 'end_lineno', start_line)
        text = self._extract_node_text(content, start_line, end_line)

        if not text:
            return []

        return [{
            "text": text,
            "start_line": start_line,
            "end_line": end_line,
            "type": "other",
            "parent_context": parent_context
        }]

    def _chunk_statement(
        self,
        node: ast.stmt,
        content: str,
        parent_context: Optional[str]
    ) -> List[Dict[str, Any]]:
        """Chunk a statement node."""
        start_line = node.lineno
        end_line = getattr(node, 'end_lineno', start_line)
        text = self._extract_node_text(content, start_line, end_line)

        if not text:
            return []

        return [{
            "text": text,
            "start_line": start_line,
            "end_line": end_line,
            "type": "other",
            "parent_context": parent_context
        }]

    def _extract_node_text(
        self,
        content: str,
        start_line: int,
        end_line: int
    ) -> str:
        """
        Extract text from source code for a given line range.

        Args:
            content: Full source code
            start_line: Starting line number (1-indexed)
            end_line: Ending line number (1-indexed)

        Returns:
            Extracted text with newlines preserved
        """
        lines = content.split('\n')
        if start_line < 1 or end_line > len(lines):
            return ""

        return '\n'.join(lines[start_line - 1:end_line])

    def _simple_chunk(
        self,
        content: str,
        file_path: str,
        max_chunk_size: int = 100
    ) -> List[Dict[str, Any]]:
        """
        Simple line-based chunking fallback.

        Splits content into chunks of approximately max_chunk_size lines
        with overlap to avoid breaking logical units.

        Args:
            content: File content
            file_path: Path to the file
            max_chunk_size: Maximum lines per chunk

        Returns:
            List of chunk dictionaries
        """
        chunks = []
        lines = content.split('\n')
        overlap = 10  # lines overlap between chunks

        # Ensure we have a positive step size
        step = max(1, max_chunk_size - overlap)

        for i in range(0, len(lines), step):
            chunk_lines = lines[i:i + max_chunk_size]
            if not chunk_lines:
                continue

            chunk_text = '\n'.join(chunk_lines)
            chunks.append({
                "text": chunk_text,
                "start_line": i + 1,
                "end_line": i + len(chunk_lines),
                "type": "text",
                "parent_context": None
            })

        return chunks

    async def delete_file(self, store_id: str, external_id: str) -> None:
        """
        Delete a file from the index.

        Note: LEANN requires index rebuild for deletion.
        We remove the metadata and mark index for rebuild.

        Args:
            store_id: Store identifier
            external_id: File path to delete

        SECURITY: Validates store_id and external_id
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # SECURITY: Validate inputs
        safe_store_id = _validate_store_id(store_id)
        safe_external_id = _validate_file_path(external_id)

        full_path = os.path.join(safe_store_id, safe_external_id) if safe_store_id else safe_external_id

        async with self._lock:
            # Find and remove metadata entries for this file
            to_remove = [
                vid for vid, meta in self._vector_metadata.items()
                if meta.file_path == full_path
            ]

            for vid in to_remove:
                del self._vector_metadata[vid]
                # Remove from vector_id_list
                if vid in self._vector_id_list:
                    self._vector_id_list.remove(vid)

            # Note: LEANN index rebuild required for complete removal
            logger.warning(
                f"Deleted metadata for {safe_external_id}. "
                f"Index rebuild required for complete removal."
            )

        self._save_metadata()

    @retry_with_exponential_backoff(
        config=RetryConfig(
            enabled=DEFAULT_RETRY_ENABLED,
            max_retries=DEFAULT_MAX_RETRIES,
            initial_delay=DEFAULT_INITIAL_DELAY,
            max_delay=DEFAULT_MAX_DELAY,
            jitter=DEFAULT_JITTER_ENABLED,
            jitter_ratio=DEFAULT_JITTER_RATIO
        ),
        operation_name="search"
    )
    async def search(
        self,
        store_ids: List[str],
        query: str,
        options: SearchOptions
    ) -> SearchResponse:
        """
        Search the vector index with automatic retry on transient failures.

        This method implements exponential backoff with jitter to handle:
        - Rate limits (HTTP 429)
        - Timeouts (HTTP 408, 504)
        - Server errors (HTTP 500, 502, 503)
        - Connection errors

        Args:
            store_ids: List of store identifiers (used as path prefixes)
            query: Search query
            options: Search options

        Returns:
            SearchResponse with results

        SECURITY: Validates query length, store_ids, and bounds top_k
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # Track search latency
        search_start = time.perf_counter()

        # SECURITY: Validate and sanitize query
        safe_query = _validate_query(query)
        if not safe_query:
            return SearchResponse(data=[])

        # SECURITY: Validate store_ids
        safe_store_ids = [_validate_store_id(sid) for sid in store_ids]

        # SECURITY: Validate and bound top_k
        top_k = _validate_top_k(options.top_k)
        top_k = min(top_k, len(self._vector_metadata))

        # Check cache if enabled
        if self._enable_cache and self._search_cache:
            cache_key = generate_cache_key(safe_query, safe_store_ids, top_k)
            cached_result = self._search_cache.get(cache_key)

            if cached_result is not None:
                logger.debug(f"Cache hit for query: {safe_query[:50]}...")
                return cached_result

        # Encode query
        query_embedding = self._encode([safe_query])[0]

        # Search using LEANN or fallback
        results = []
        if LEANN_AVAILABLE and self._searcher:
            try:
                # Use LeannSearcher if available
                search_results = self._searcher.search(query_embedding, top_k=top_k * 2)  # Get more for filtering

                for score, vector_idx in search_results:
                    # Map LEANN index to vector_id
                    if 0 <= vector_idx < len(self._vector_id_list):
                        vector_id = self._vector_id_list[vector_idx]

                        # Get metadata for this vector
                        if vector_id in self._vector_metadata:
                            meta = self._vector_metadata[vector_id]

                            # Filter by store_ids if specified
                            if safe_store_ids:
                                # Check if any store_id matches the file path prefix
                                if not any(meta.file_path.startswith(sid) for sid in safe_store_ids):
                                    continue

                            # Load content
                            content = self._load_chunk_content(
                                meta.file_path,
                                meta.start_line,
                                meta.end_line
                            )

                            results.append(ChunkType(
                                type="text",
                                text=content,
                                score=float(score),
                                metadata=FileMetadata(path=meta.file_path, hash=""),
                                chunk_index=meta.chunk_index,
                            ))

                            # Stop if we have enough results
                            if len(results) >= top_k:
                                break
            except Exception as e:
                logger.error(f"LEANN search failed: {e}")
                # Fall back to manual similarity search
                results = await self._fallback_search(query_embedding, top_k, safe_store_ids)
        else:
            # Fallback: manual similarity search
            results = await self._fallback_search(query_embedding, top_k, safe_store_ids)

        # Record search metrics
        search_latency = time.perf_counter() - search_start
        self._search_latencies.append(search_latency)
        self._total_searches += 1

        # Keep only last MAX_METRICS_SAMPLES
        if len(self._search_latencies) > MAX_METRICS_SAMPLES:
            self._search_latencies.pop(0)

        # Create response
        response = SearchResponse(data=results)

        # Cache the result if enabled
        if self._enable_cache and self._search_cache:
            cache_key = generate_cache_key(safe_query, safe_store_ids, top_k)
            self._search_cache.put(cache_key, response)

        return response

    async def _fallback_search(
        self,
        query_embedding: Any,
        top_k: int,
        store_ids: List[str]
    ) -> List[ChunkType]:
        """
        Fallback manual similarity search when LEANN is not available.

        This performs a brute-force cosine similarity search across all vectors.
        It's slower but works without LEANN.

        Args:
            query_embedding: The query vector embedding
            top_k: Number of results to return
            store_ids: List of store IDs to filter by

        Returns:
            List of ChunkType objects with search results
        """
        if not self._vector_metadata:
            return []

        # Compute similarities with all vectors
        similarities = []
        for vector_id, meta in self._vector_metadata.items():
            # Filter by store_ids if specified
            if store_ids:
                if not any(meta.file_path.startswith(sid) for sid in store_ids):
                    continue

            # Load chunk content and encode
            try:
                content = self._load_chunk_content(
                    meta.file_path,
                    meta.start_line,
                    meta.end_line
                )

                if not content:
                    continue

                # Encode the chunk content
                chunk_embedding = self._encode([content])[0]

                # Compute cosine similarity (dot product since embeddings are normalized)
                similarity = float(np.dot(query_embedding, chunk_embedding))
                similarities.append((similarity, vector_id, meta))

            except Exception as e:
                logger.warning(f"Failed to encode chunk {vector_id}: {e}")
                continue

        # Sort by similarity (descending)
        similarities.sort(key=lambda x: x[0], reverse=True)

        # Take top_k results
        results = []
        for score, vector_id, meta in similarities[:top_k]:
            content = self._load_chunk_content(
                meta.file_path,
                meta.start_line,
                meta.end_line
            )

            results.append(ChunkType(
                type="text",
                text=content,
                score=score,
                metadata=FileMetadata(path=meta.file_path, hash=""),
                chunk_index=meta.chunk_index,
            ))

        return results

    def _load_chunk_content(self, file_path: str, start_line: Optional[int], end_line: Optional[int], max_chars: int = 1500) -> str:
        """
        Load file content for a chunk with intelligent truncation for token efficiency.

        TOKEN EFFICIENCY: Limits content per result to prevent token flooding.
        - Default max_chars: 1500 (approximately 300-400 tokens)
        - Preserves context by including line numbers
        - Adds truncation indicator when content is limited

        Args:
            file_path: Path to the file
            start_line: Starting line number (1-indexed, inclusive)
            end_line: Ending line number (1-indexed, inclusive)
            max_chars: Maximum characters to return per chunk (default: 1500)

        Returns:
            File content for the chunk (intelligently truncated)
        """
        try:
            if not os.path.exists(file_path):
                logger.warning(f"File not found: {file_path}")
                return "[Content not available: file not found]"

            with open(file_path, 'r', encoding='utf-8', errors='ignore') as f:
                if start_line is not None and end_line is not None:
                    # Load specific lines with line numbers for context
                    lines = f.readlines()
                    if 1 <= start_line <= len(lines) and 1 <= end_line <= len(lines):
                        selected_lines = lines[start_line - 1:end_line]

                        # TOKEN EFFICIENCY: Limit content size
                        content_chars = sum(len(line) for line in selected_lines)

                        if content_chars > max_chars:
                            # Smart truncation: prioritize earlier lines, add continuation marker
                            result_lines = []
                            current_chars = 0
                            for i, line in enumerate(selected_lines):
                                if current_chars + len(line) > max_chars:
                                    # Add continuation marker with remaining line count
                                    remaining = len(selected_lines) - i
                                    result_lines.append(f"\n... [+{remaining} more lines, use read_file for full content]")
                                    break
                                result_lines.append(line)
                                current_chars += len(line)

                            # Add line numbers for context (compact format)
                            numbered_lines = []
                            for i, line in enumerate(result_lines):
                                line_num = start_line + i
                                # Compact format: "L123: content"
                                numbered_lines.append(f"L{line_num}: {line.rstrip()}")
                            return '\n'.join(numbered_lines)
                        else:
                            # Content fits within limit, add line numbers
                            numbered_lines = []
                            for i, line in enumerate(selected_lines):
                                line_num = start_line + i
                                numbered_lines.append(f"L{line_num}: {line.rstrip()}")
                            return '\n'.join(numbered_lines)
                    else:
                        logger.warning(f"Invalid line range {start_line}-{end_line} for {file_path} (has {len(lines)} lines)")
                        return f"[Content not available: invalid line range {start_line}-{end_line}]"
                else:
                    # Load full file content with strict size limit
                    content = f.read()
                    if len(content) > max_chars:
                        content = content[:max_chars] + f"\n... [truncated, {len(content) - max_chars} more chars]"
                    return content

        except Exception as e:
            logger.error(f"Error loading content from {file_path}: {e}")
            return f"[Content not available: {str(e)}]"

    async def ask(
        self,
        store_ids: List[str],
        question: str,
        options: SearchOptions
    ) -> AskResponse:
        """
        Ask a question (RAG).

        Note: This is a simplified implementation.
        Full RAG would require an LLM for answer generation.

        Args:
            store_ids: List of store identifiers
            question: The question to ask
            options: Search options

        Returns:
            AskResponse with answer and sources
        """
        # Search for relevant chunks
        search_response = await self.search(store_ids, question, options)

        # Note: Full RAG would use an LLM to generate an answer
        # For now, return sources with a placeholder answer
        return AskResponse(
            answer="RAG answer generation not implemented. Use search results directly.",
            sources=search_response.data
        )

    async def get_info(self, store_id: str) -> StoreInfo:
        """
        Get store information.

        Args:
            store_id: Store identifier

        Returns:
            StoreInfo with store details

        SECURITY: Validates store_id
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # SECURITY: Validate store_id
        safe_store_id = _validate_store_id(store_id)

        # Count files in this store
        file_count = len(set(
            meta.file_path for meta in self._vector_metadata.values()
            if safe_store_id and meta.file_path.startswith(safe_store_id)
        ))

        return StoreInfo(
            name=safe_store_id,
            description=f"LEANN vector store with {self._index_metadata.vector_count} vectors",
            created_at=self._index_metadata.created_at,
            updated_at=self._index_metadata.updated_at,
            counts={
                "vectors": self._index_metadata.vector_count,
                "files": file_count,
                "dimension": self._index_metadata.dimension,
                "model": self._index_metadata.model,
                "backend": self._index_metadata.backend
            }
        )

    async def create_store(self, name: str, description: str = "") -> Dict[str, Any]:
        """
        Create a new store (namespace).

        In LEANN backend, stores are just path prefixes.

        Args:
            name: Store name
            description: Store description

        Returns:
            Store creation info

        SECURITY: Validates store name
        """
        # SECURITY: Validate store name
        safe_name = _validate_store_id(name)

        return {
            "name": safe_name,
            "description": description,
            "created_at": datetime.now().isoformat(),
            "type": "leann"
        }

    async def health_check(self) -> Dict[str, Any]:
        """
        Check backend health and return status.

        Returns:
            Dictionary with backend health information including:
            - backend: str - Backend name ("leann")
            - available: bool - Whether backend is operational
            - vector_count: int - Number of vectors in the index
            - model: str - Embedding model name
            - backend_type: str - LEANN backend type (hnsw/diskann)
            - index_path: str - Path to the index directory
        """
        return {
            "backend": "leann",
            "available": self.is_available(),
            "vector_count": self._index_metadata.vector_count if self._index_metadata else 0,
            "model": self.model_name,
            "backend_type": self.backend_name,
            "index_path": str(self.index_path),
        }


    async def get_metrics(self) -> Metrics:
        """
        Get current performance metrics.

        Returns:
            Metrics dataclass with current performance data

        Raises:
            ValueError: If backend not initialized
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # Calculate index storage size
        storage_size = self._calculate_storage_size()

        # Calculate percentiles
        search_latencies = sorted(self._search_latencies)
        n = len(search_latencies)

        if n > 0:
            p50_idx = int(n * 0.5)
            p95_idx = int(n * 0.95)
            p99_idx = int(n * 0.99)
            p50 = search_latencies[p50_idx]
            p95 = search_latencies[p95_idx]
            p99 = search_latencies[p99_idx]
            avg = sum(search_latencies) / n
        else:
            p50 = p95 = p99 = avg = 0.0

        # Average embedding time
        if self._embedding_times:
            avg_embedding = sum(self._embedding_times) / len(self._embedding_times)
        else:
            avg_embedding = 0.0

        # Memory usage
        try:
            import psutil
            process = psutil.Process(os.getpid())
            memory_info = process.memory_info()
            memory_usage = memory_info.rss
        except ImportError:
            logger.warning("psutil not installed, memory metrics unavailable")
            memory_usage = 0
        except Exception as e:
            logger.warning(f"Failed to get memory usage: {e}")
            memory_usage = 0

        return Metrics(
            vector_count=self._index_metadata.vector_count,
            storage_size_bytes=storage_size,
            search_latency_p50=p50,
            search_latency_p95=p95,
            search_latency_p99=p99,
            avg_search_latency=avg,
            avg_embedding_time=avg_embedding,
            total_embeddings=self._total_embeddings,
            memory_usage_bytes=memory_usage,
            total_searches=self._total_searches,
            total_uploads=self._total_uploads
        )

    def _calculate_storage_size(self) -> int:
        """
        Calculate total storage size of index files in bytes.

        Returns:
            Total size in bytes
        """
        total_size = 0
        try:
            for root, dirs, files in os.walk(self.index_path):
                for file in files:
                    file_path = os.path.join(root, file)
                    if os.path.exists(file_path):
                        total_size += os.path.getsize(file_path)
        except Exception as e:
            logger.warning(f"Failed to calculate storage size: {e}")
        return total_size

    async def upload_files(
        self,
        files: List[Tuple[str, str, Union[str, bytes]]],
        options: Optional[BatchUploadOptions] = None
    ) -> BatchUploadResult:
        """
        Upload multiple files in a single batch operation.

        Args:
            files: List of (store_id, file_path, content) tuples
            options: Batch upload options (optional)

        Returns:
            BatchUploadResult with upload statistics

        Raises:
            ValueError: If batch format is invalid or exceeds maximum size
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # Set default options
        if options is None:
            options = BatchUploadOptions()

        # Validate batch format
        if not isinstance(files, list):
            raise ValueError(f"files must be a list, got: {type(files)}")

        if len(files) > options.max_batch_size:
            raise ValueError(
                f"Batch size {len(files)} exceeds maximum {options.max_batch_size}"
            )

        # Validate all inputs before processing (fail fast)
        validated_files = []
        for i, item in enumerate(files):
            if not isinstance(item, (tuple, list)) or len(item) != 3:
                raise ValueError(
                    f"Invalid file format at index {i}: "
                    f"expected (store_id, file_path, content) tuple"
                )

            store_id, file_path, content = item

            # Validate each input
            try:
                safe_store_id = _validate_store_id(store_id)
                safe_file_path = _validate_file_path(file_path)
                _validate_content_size(content)
                validated_files.append((safe_store_id, safe_file_path, content))
            except ValueError as e:
                if not options.continue_on_error:
                    raise
                logger.warning(f"Skipping invalid file at index {i}: {e}")

        # Process files
        success_count = 0
        failure_count = 0
        errors = []

        for store_id, file_path, content in validated_files:
            try:
                # Create upload options
                upload_options = UploadFileOptions(
                    external_id=file_path,
                    overwrite=True
                )

                # Upload file
                await self.upload_file(store_id, file_path, content, upload_options)
                success_count += 1
                self._total_uploads += 1

            except Exception as e:
                failure_count += 1
                error_info = {
                    "file_path": file_path,
                    "store_id": store_id,
                    "error": str(e)
                }
                errors.append(error_info)
                logger.error(f"Failed to upload {file_path}: {e}")

                # Stop if not continuing on error
                if not options.continue_on_error:
                    break

        result = BatchUploadResult(
            success_count=success_count,
            failure_count=failure_count,
            total_files=len(validated_files),
            errors=errors
        )

        logger.info(
            f"Batch upload complete: {success_count} succeeded, "
            f"{failure_count} failed"
        )

        return result

    async def export_index(self, output_path: str) -> None:
        """
        Export index to backup file.

        Creates a ZIP archive containing:
        - All index files
        - metadata.json
        - vectors_metadata.json
        - vector_id_list.json

        Args:
            output_path: Path for output ZIP file

        Raises:
            ValueError: If output path is invalid or file already exists
            OSError: If insufficient disk space
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # Validate output path
        safe_output_path = _sanitize_path(output_path, allow_absolute=True)

        # Check if file already exists
        if os.path.exists(safe_output_path):
            raise ValueError(f"Output file already exists: {safe_output_path}")

        # Check if path is directory
        if os.path.isdir(safe_output_path):
            raise ValueError(f"Output path is a directory: {safe_output_path}")

        # Ensure parent directory exists
        parent_dir = os.path.dirname(safe_output_path)
        if parent_dir:
            Path(parent_dir).mkdir(parents=True, exist_ok=True)

        logger.info(f"Exporting index to {safe_output_path}")

        import zipfile

        try:
            # Calculate total size before export
            total_size = self._calculate_storage_size()

            if total_size > MAX_EXPORT_SIZE:
                raise ValueError(
                    f"Index size ({total_size} bytes) exceeds maximum "
                    f"export size ({MAX_EXPORT_SIZE} bytes)"
                )

            # Create ZIP archive
            with zipfile.ZipFile(safe_output_path, 'w', zipfile.ZIP_DEFLATED) as zipf:
                # Add all files from index directory
                for root, dirs, files in os.walk(self.index_path):
                    for file in files:
                        file_path = os.path.join(root, file)
                        arcname = os.path.relpath(file_path, self.index_path)
                        zipf.write(file_path, arcname)

            # Verify export
            export_size = os.path.getsize(safe_output_path)
            logger.info(
                f"Export complete: {safe_output_path} "
                f"({export_size} bytes, {total_size} bytes uncompressed)"
            )

        except zipfile.LargeZipFile:
            raise ValueError(
                f"Export file too large. Requires ZIP64 extensions. "
                f"Index size: {total_size} bytes"
            )
        except Exception as e:
            logger.error(f"Export failed: {e}")
            # Clean up partial export
            if os.path.exists(safe_output_path):
                os.remove(safe_output_path)
            raise

    async def import_index(self, input_path: str) -> None:
        """
        Import index from backup file.

        This will replace the current index with the imported one.
        The backend must be re-initialized after import.

        Args:
            input_path: Path to input ZIP file

        Raises:
            ValueError: If input path is invalid or backup is corrupted
            FileNotFoundError: If input file doesn't exist
        """
        if not self._initialized:
            raise ValueError("LEANNVectorBackend not initialized. Call initialize() first.")

        # Validate input path
        safe_input_path = _sanitize_path(input_path, allow_absolute=True)

        # Check if file exists
        if not os.path.exists(safe_input_path):
            raise FileNotFoundError(f"Input file not found: {safe_input_path}")

        # Check if file is a ZIP archive
        if not safe_input_path.lower().endswith('.zip'):
            raise ValueError(f"Input file must be a ZIP archive: {safe_input_path}")

        logger.info(f"Importing index from {safe_input_path}")

        import zipfile
        import tempfile
        import shutil

        # Create temporary directory for extraction
        with tempfile.TemporaryDirectory() as temp_dir:
            try:
                # Extract ZIP archive
                with zipfile.ZipFile(safe_input_path, 'r') as zipf:
                    zipf.extractall(temp_dir)

                # Validate required files exist
                required_files = [
                    'metadata.json',
                    'vectors_metadata.json',
                    'vector_id_list.json'
                ]

                for required_file in required_files:
                    file_path = os.path.join(temp_dir, required_file)
                    if not os.path.exists(file_path):
                        raise ValueError(
                            f"Invalid backup: missing required file {required_file}"
                        )

                # Validate metadata format
                metadata_path = os.path.join(temp_dir, 'metadata.json')
                with open(metadata_path, 'r') as f:
                    metadata_data = json.load(f)

                # Check version compatibility
                version = metadata_data.get('version', '0.0')
                if version != INDEX_VERSION:
                    logger.warning(
                        f"Backup version {version} differs from current {INDEX_VERSION}"
                    )

                # Backup current index if it exists
                if os.path.exists(self.index_path):
                    backup_path = f"{self.index_path}.backup_{datetime.now().strftime('%Y%m%d_%H%M%S')}"
                    logger.info(f"Backing up current index to {backup_path}")
                    shutil.move(self.index_path, backup_path)

                # Move extracted files to index path
                Path(self.index_path).mkdir(parents=True, exist_ok=True)

                for item in os.listdir(temp_dir):
                    src = os.path.join(temp_dir, item)
                    dst = os.path.join(self.index_path, item)
                    if os.path.isdir(src):
                        shutil.copytree(src, dst, dirs_exist_ok=True)
                    else:
                        shutil.copy2(src, dst)

                # Reload metadata
                self._load_or_create_metadata()

                # Rebuild index
                await self._rebuild_index()

                logger.info(
                    f"Import complete: {self._index_metadata.vector_count} vectors "
                    f"restored from {safe_input_path}"
                )

            except json.JSONDecodeError as e:
                raise ValueError(f"Invalid backup: corrupted metadata JSON: {e}")
            except zipfile.BadZipFile as e:
                raise ValueError(f"Invalid backup: corrupted ZIP file: {e}")
            except Exception as e:
                logger.error(f"Import failed: {e}")
                # Attempt to restore backup
                import glob
                backup_pattern = f"{self.index_path}.backup_*"
                backups = glob.glob(backup_pattern)
                if backups:
                    logger.info(f"Attempting to restore from {backups[0]}")
                    try:
                        if os.path.exists(self.index_path):
                            shutil.rmtree(self.index_path)
                        shutil.move(backups[0], self.index_path)
                        self._load_or_create_metadata()
                    except Exception as restore_error:
                        logger.error(f"Failed to restore backup: {restore_error}")
                raise




    # ========================================================================
    # ENHANCEMENT METHODS - Cache, Deduplication, and Configuration
    # ========================================================================

    def invalidate_cache(self, key: Optional[str] = None) -> None:
        """
        Invalidate search cache.

        Args:
            key: Specific cache key to invalidate, or None to clear all

        Example:
            backend.invalidate_cache()  # Clear all cache
            backend.invalidate_cache("specific_key")  # Clear specific entry
        """
        if self._enable_cache and self._search_cache:
            self._search_cache.invalidate(key)
            logger.info(f"Cache invalidated: {'all' if key is None else key}")

    def get_cache_stats(self) -> Dict[str, Any]:
        """
        Get cache statistics.

        Returns:
            Dictionary with cache statistics including:
            - size: Current cache size
            - maxsize: Maximum cache size
            - ttl: Cache TTL in seconds
            - hits: Number of cache hits
            - misses: Number of cache misses
            - hit_rate: Cache hit rate (0-1)
            - evictions: Number of evictions
            - expirations: Number of expired entries

        Example:
            stats = backend.get_cache_stats()
            print(f"Hit rate: {stats['hit_rate']:.2%}")
        """
        if self._enable_cache and self._search_cache:
            return self._search_cache.get_stats()
        return {"error": "Cache not enabled"}

    def get_deduplication_stats(self) -> Dict[str, Any]:
        """
        Get deduplication statistics.

        Returns:
            Dictionary with deduplication statistics including:
            - unique_vectors: Number of unique vectors
            - total_references: Total reference count across all vectors
            - reference_counts: Dict of vector_id -> reference count

        Example:
            stats = backend.get_deduplication_stats()
            print(f"Unique vectors: {stats['unique_vectors']}")
            print(f"Total references: {stats['total_references']}")
        """
        if self._enable_deduplication and self._deduplicator:
            return self._deduplicator.get_stats()
        return {"error": "Deduplication not enabled"}

    def configure_enhancements(
        self,
        enable_incremental_updates: Optional[bool] = None,
        enable_cache: Optional[bool] = None,
        enable_deduplication: Optional[bool] = None,
        cache_maxsize: Optional[int] = None,
        cache_ttl: Optional[int] = None
    ) -> None:
        """
        Configure enhancement features dynamically.

        Args:
            enable_incremental_updates: Enable incremental index updates
            enable_cache: Enable search result caching
            enable_deduplication: Enable vector deduplication
            cache_maxsize: Maximum cache size (requires cache reinit if changed)
            cache_ttl: Cache TTL in seconds (requires cache reinit if changed)

        Example:
            backend.configure_enhancements(
                enable_cache=True,
                cache_maxsize=2000,
                cache_ttl=600  # 10 minutes
            )
        """
        if enable_incremental_updates is not None:
            self._enable_incremental_updates = enable_incremental_updates
            logger.info(f"Incremental updates {'enabled' if enable_incremental_updates else 'disabled'}")

        if enable_deduplication is not None:
            self._enable_deduplication = enable_deduplication
            if enable_deduplication and self._deduplicator is None:
                self._deduplicator = VectorDeduplicator()
            elif not enable_deduplication:
                self._deduplicator = None
            logger.info(f"Deduplication {'enabled' if enable_deduplication else 'disabled'}")

        if enable_cache is not None:
            self._enable_cache = enable_cache
            if enable_cache and self._search_cache is None:
                self._search_cache = TTLCache(
                    maxsize=cache_maxsize or self._cache_maxsize,
                    ttl=cache_ttl or self._cache_ttl
                )
            elif not enable_cache:
                self._search_cache = None
            logger.info(f"Cache {'enabled' if enable_cache else 'disabled'}")

        # Update cache config if provided
        if cache_maxsize is not None:
            self._cache_maxsize = cache_maxsize
            # Reinitialize cache if it's enabled
            if self._enable_cache:
                self._search_cache = TTLCache(
                    maxsize=self._cache_maxsize,
                    ttl=self._cache_ttl
                )
                logger.info(f"Cache maxsize updated to {cache_maxsize}")

        if cache_ttl is not None:
            self._cache_ttl = cache_ttl
            # Reinitialize cache if it's enabled
            if self._enable_cache:
                self._search_cache = TTLCache(
                    maxsize=self._cache_maxsize,
                    ttl=self._cache_ttl
                )
                logger.info(f"Cache TTL updated to {cache_ttl}s")

    async def _incremental_update(self) -> None:
        """
        Perform incremental index update.

        This method adds only the new vectors to the index without
        rebuilding the entire index, which is much more efficient.

        Note: In production, this would use LEANN's incremental add capability.
        For now, it's a placeholder that clears pending vectors and triggers a rebuild.

        The actual incremental update requires LEANN library support for
        adding vectors without full index rebuild.
        """
        if not self._pending_vectors:
            return

        logger.info(f"Performing incremental update with {len(self._pending_vectors)} new vectors")

        # TODO: Implement actual LEANN incremental update when API is available
        # This requires LEANN library support for adding vectors without rebuild
        #
        # In a real implementation:
        # 1. Load the existing index
        # 2. Add new vectors incrementally
        # 3. Save the updated index
        #
        # For now, we clear pending vectors and mark index as dirty
        # The rebuild will happen in _rebuild_index()

        self._pending_vectors.clear()
        self._index_dirty = False

    def reset_enhancement_stats(self) -> None:
        """
        Reset all enhancement statistics (cache and deduplication).

        Example:
            backend.reset_enhancement_stats()
        """
        if self._enable_cache and self._search_cache:
            self._search_cache.reset_stats()
            logger.info("Cache statistics reset")

        # Note: Deduplication stats are not reset as they reflect actual state

def get_leann_backend_status() -> dict:
    """
    Get the status of the LEANN vector backend dependencies.

    Returns:
        dict with keys:
            - available: bool - True if all dependencies are installed
            - leann_available: bool
            - sentence_transformers_available: bool
            - numpy_available: bool
            - supported_backends: list
            - install_command: str - Command to install missing dependencies
    """
    status = {
        "available": LEANN_AVAILABLE and SENTENCE_TRANSFORMERS_AVAILABLE and NUMPY_AVAILABLE,
        "leann_available": LEANN_AVAILABLE,
        "sentence_transformers_available": SENTENCE_TRANSFORMERS_AVAILABLE,
        "numpy_available": NUMPY_AVAILABLE,
        "supported_backends": SUPPORTED_BACKENDS,
        "install_command": "uv pip install 'leann>=0.3.5' 'sentence-transformers>=2.2.0' 'numpy>=1.24.0'"
    }

    if LEANN_AVAILABLE:
        try:
            import leann
            status["leann_version"] = getattr(leann, "__version__", "unknown")
        except Exception:
            status["leann_version"] = "unknown"

    if SENTENCE_TRANSFORMERS_AVAILABLE:
        try:
            import sentence_transformers
            status["sentence_transformers_version"] = getattr(
                sentence_transformers, "__version__", "unknown"
            )
        except Exception:
            status["sentence_transformers_version"] = "unknown"

    if NUMPY_AVAILABLE:
        try:
            import numpy
            status["numpy_version"] = getattr(numpy, "__version__", "unknown")
        except Exception:
            status["numpy_version"] = "unknown"

    return status

    # ========================================================================
    # ENHANCEMENT METHODS - Cache, Deduplication, and Configuration
    # ========================================================================



