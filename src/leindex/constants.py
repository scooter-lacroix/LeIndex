"""
Shared constants for the LeIndex MCP server.

This module contains all magic numbers, configuration defaults, and
constant values used throughout the codebase. Each constant includes
documentation explaining its purpose and usage.
"""

# ============================================================================
# Directory and File Names
# ============================================================================

"""Global configuration file path for user-level settings."""
GLOBAL_CONFIG_FILE = "~/.leindex_mcp_global_config.json"

"""Settings directory name within project root."""
SETTINGS_DIR = "leindex"

"""Persistent data directory for project-specific data."""
PERSISTENT_SETTINGS_DIR = ".leindex_data"

"""Configuration file name."""
CONFIG_FILE = "config.json"

"""File index pickle file name."""
INDEX_FILE = "file_index.pickle"

"""Content cache pickle file name."""
CACHE_FILE = "content_cache.pickle"

"""File metadata pickle file name."""
METADATA_FILE = "file_metadata.pickle"

# ============================================================================
# Tantivy Configuration (Primary Full-Text Search Backend)
# ============================================================================

"""Default Tantivy index path."""
TANTIVY_INDEX_PATH = ".tantivy_index"

"""Default Tantivy cache enabled setting."""
TANTIVY_CACHE_ENABLED = True

"""Default Tantivy cache max size."""
TANTIVY_CACHE_MAX_SIZE = 128

"""Default Tantivy cache TTL in seconds."""
TANTIVY_CACHE_TTL_SECONDS = 300

"""Default BM25 k1 parameter (term frequency saturation)."""
TANTIVY_BM25_K1 = 1.2

"""Default BM25 b parameter (length normalization)."""
TANTIVY_BM25_B = 0.75

# ============================================================================
# File Size Limits (in bytes)
# ============================================================================

"""Default maximum file size for indexing (5MB)."""
DEFAULT_MAX_FILE_SIZE = 5242880

"""Maximum size for Python/JavaScript files (1MB)."""
TYPE_SPECIFIC_MAX_SIZE_DEFAULT = 1048576

"""Maximum size for JSON/YAML/XML files (512KB)."""
TYPE_SPECIFIC_MAX_SIZE_SMALL = 524288

"""No file size limit (infinity)."""
NO_FILE_SIZE_LIMIT = float('inf')

"""Maximum file size for large files (1GB)."""
LARGE_FILE_MAX_SIZE = 1073741824

# ============================================================================
# Directory Limits
# ============================================================================

"""Default maximum files per directory (1000)."""
DEFAULT_MAX_FILES_PER_DIRECTORY = 1000

"""Maximum subdirectories per directory (100)."""
DEFAULT_MAX_SUBDIRECTORIES_PER_DIRECTORY = 100

"""Large directory threshold - maximum files (10000)."""
LARGE_MAX_FILES_PER_DIRECTORY = 10000

"""Large directory threshold - maximum subdirectories (1000)."""
LARGE_MAX_SUBDIRECTORIES_PER_DIRECTORY = 1000

# ============================================================================
# Search Result Limits
# ============================================================================

"""Default maximum search results (1000)."""
DEFAULT_MAX_SEARCH_RESULTS = 1000

"""Default file versions to retrieve (100)."""
DEFAULT_FILE_VERSIONS_LIMIT = 100

"""Recent operations limit for monitoring (10)."""
RECENT_OPERATIONS_LIMIT = 10

# ============================================================================
# Cache and Timeout Settings (in seconds)
# ============================================================================

"""Default cache TTL for search results (300 seconds = 5 minutes)."""
DEFAULT_CACHE_TTL = 300

"""Health check cache timeout (300 seconds = 5 minutes)."""
HEALTH_CHECK_CACHE_TIMEOUT = 300

"""Cleanup interval for background tasks (300 seconds = 5 minutes)."""
DEFAULT_CLEANUP_INTERVAL = 300

"""Short connection timeout (3 seconds)."""
SHORT_CONNECTION_TIMEOUT = 3

"""Default connection timeout (5 seconds)."""
DEFAULT_CONNECTION_TIMEOUT = 5

"""Medium connection timeout (10 seconds)."""
MEDIUM_CONNECTION_TIMEOUT = 10

"""Long connection timeout (30 seconds)."""
LONG_CONNECTION_TIMEOUT = 30

"""Zoekt indexing timeout (300 seconds = 5 minutes)."""
ZOEKT_INDEXING_TIMEOUT = 300

"""Zoekt search timeout (30 seconds)."""
ZOEKT_SEARCH_TIMEOUT = 30

"""Thread join timeout (5 seconds)."""
THREAD_JOIN_TIMEOUT = 5

# ============================================================================
# Cache Sizes
# ============================================================================

"""Default search cache max size (128 entries)."""
DEFAULT_SEARCH_CACHE_MAX_SIZE = 128

"""Maximum loaded files in memory (100)."""
MAX_LOADED_FILES_IN_MEMORY = 100

# ============================================================================
# Pattern and Query Limits
# ============================================================================

"""Maximum pattern length to prevent DoS (1000 characters)."""
MAX_PATTERN_LENGTH = 1000

"""Maximum wildcard count in pattern (50)."""
MAX_WILDCARD_COUNT = 50

"""Maximum regex alternations (20)."""
MAX_REGEX_ALTERNATIONS = 20

"""Maximum regex nesting depth to prevent ReDoS (10)."""
MAX_REGEX_NESTING_DEPTH = 10

"""Maximum query length for search (1000 characters)."""
MAX_QUERY_LENGTH = 1000

"""Minimum search limit value (1)."""
MIN_SEARCH_LIMIT = 1

"""Maximum search limit value (1000)."""
MAX_SEARCH_LIMIT = 1000

# ============================================================================
# Memory Limits (in MB)
# ============================================================================

"""Soft memory limit for profiler (512MB)."""
MEMORY_PROFILER_SOFT_LIMIT_MB = 512.0

"""Hard memory limit for profiler (1024MB = 1GB)."""
MEMORY_PROFILER_HARD_LIMIT_MB = 1024.0

"""Soft memory limit from config (8192MB = 8GB)."""
CONFIG_SOFT_LIMIT_MB = 8192

"""Hard memory limit from config (16384MB = 16GB)."""
CONFIG_HARD_LIMIT_MB = 16384

# ============================================================================
# Worker/Thread Settings
# ============================================================================

"""Default maximum workers for parallel processing (4)."""
DEFAULT_MAX_WORKERS = 4

# ============================================================================
# Retry Settings
# ============================================================================

"""Default maximum retries for connection attempts (3)."""
DEFAULT_MAX_RETRIES = 3

"""Default wait time for completion (2 seconds)."""
DEFAULT_WAIT_FOR_COMPLETION = 2

# ============================================================================
# Monitoring and Cleanup
# ============================================================================

"""Max age for search operations before cleanup (24 hours)."""
SEARCH_OPERATIONS_MAX_AGE_HOURS = 24.0

"""Max age hours in seconds (24 * 3600)."""
MAX_AGE_SECONDS = 24.0 * 3600

# ============================================================================
# Async Indexer Configuration
# ============================================================================

"""Default batch size for async indexing operations."""
ASYNC_INDEXER_DEFAULT_BATCH_SIZE = 50

"""Maximum batch size for async indexing operations."""
ASYNC_INDEXER_MAX_BATCH_SIZE = 500

"""Batch timeout in seconds before forcing a flush."""
ASYNC_INDEXER_BATCH_TIMEOUT = 5.0

"""Default number of worker tasks for async processing."""
ASYNC_INDEXER_DEFAULT_WORKER_COUNT = 4

"""Maximum number of retries for failed indexing tasks."""
ASYNC_INDEXER_MAX_RETRIES = 3

"""Backpressure delay in seconds when under load."""
ASYNC_INDEXER_BACKPRESSURE_DELAY = 0.1

"""Shutdown timeout in seconds for batch flush."""
ASYNC_INDEXER_SHUTDOWN_FLUSH_TIMEOUT = 10.0

"""Maximum number of processing times to track for statistics."""
ASYNC_INDEXER_MAX_PROCESSING_TIMES = 1000

"""Default maximum number of retries for content extraction."""
ASYNC_INDEXER_MAX_EXTRACTION_RETRIES = 3

"""Retry delay in seconds for content extraction with exponential backoff."""
ASYNC_INDEXER_EXTRACTION_RETRY_DELAY = 1.0

# ============================================================================
# Queue Configuration
# ============================================================================

"""Maximum size of the async task queue."""
QUEUE_MAX_SIZE = 10000

"""Maximum memory bytes for the queue (100MB)."""
QUEUE_MAX_MEMORY_BYTES = 100 * 1024 * 1024

"""Maximum priority queue memory bytes (10MB)."""
QUEUE_MAX_PRIORITY_MEMORY_BYTES = 10 * 1024 * 1024

"""Default timeout in seconds for queue pop operations."""
QUEUE_POP_TIMEOUT = 1.0

"""Queue depth threshold for backpressure activation."""
QUEUE_BACKPRESSURE_THRESHOLD = 1000

"""Latency threshold in milliseconds for backpressure activation."""
QUEUE_BACKPRESSURE_LATENCY_THRESHOLD_MS = 5000

"""Recovery factor for backpressure (0.0-1.0)."""
QUEUE_BACKPRESSURE_RECOVERY_FACTOR = 0.8

# ============================================================================
# Search Configuration
# ============================================================================

"""Default limit for search results."""
DEFAULT_SEARCH_LIMIT = 100

"""Default offset for search pagination."""
DEFAULT_SEARCH_OFFSET = 0
