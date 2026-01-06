"""
LeIndex MCP Server

This MCP server allows LLMs to index, search, and analyze code from a project directory.
It provides tools for file discovery, content retrieval, and code analysis.
"""

from contextlib import asynccontextmanager
from dataclasses import dataclass
from typing import AsyncIterator, Dict, List, Optional, Tuple, Any, Union, Literal
import os
import pathlib
import json
import fnmatch
import sys
import tempfile
import time
import asyncio
import threading
import re
from datetime import datetime  # Import datetime
from .lazy_loader import LazyContentManager
from mcp.server.fastmcp import FastMCP, Context
from mcp import types

# Import the ProjectSettings class and constants - using relative import
from .optimized_project_settings import OptimizedProjectSettings
from .constants import SETTINGS_DIR
from .ignore_patterns import IgnorePatternMatcher
from .config_manager import ConfigManager
from .global_config_manager import GlobalConfigManager  # Import GlobalConfigManager
from .incremental_indexer import IncrementalIndexer
from .parallel_processor import ParallelIndexer, IndexingTask
from .memory_profiler import (
    MemoryProfiler,
    MemoryLimits,
    MemoryAwareLazyContentManager,
    create_memory_config_from_yaml,
)
from .performance_monitor import (
    PerformanceMonitor,
    create_performance_monitor_from_config,
)
from .progress_tracker import (
    progress_manager,
    ProgressContext,
    ProgressTracker,
    LoggingProgressHandler,
)
from .file_change_tracker import FileChangeTracker
from .storage.dal_factory import get_dal_instance
from .storage.storage_interface import (
    DALInterface,
)  # Import DALInterface and SearchInterface
from .logger_config import logger  # Import the centralized logger
from .file_reader import (
    SmartFileReader,
    ReadingStrategy,
    FileSizeCategory,
)  # Import SmartFileReader and enums
from .system_utils import detect_system, async_ensure_infrastructure
from .search_utils import (
    SearchBackendSelector,
    SearchErrorHandler,
    SearchPatternTranslator,
    SearchResultProcessor,
    search_monitor,
    degradation_manager,
)  # Import search utilities
from .core_engine import CoreEngine, SearchOptions  # Import CoreEngine

# ============================================================================
# PHASE 7 INTEGRATIONS: Ranking, API Key Manager, Stats Dashboard
# ============================================================================
from .search.ranking import ResultRanker, RankingConfig
from .api_key_manager import APIKeyManager, create_manager_from_env
from .stats_dashboard import IndexStatisticsCollector

# ============================================================================
# META-REGISTRY: Startup Migration
# ============================================================================
from .registry.startup_migration import check_and_migrate_on_startup
from .registry.backup_scheduler import get_backup_scheduler, setup_signal_handlers

# NOTE: FastMCP instance is created below after indexer_lifespan is defined (line ~528)
# This ensures the lifespan manager is properly attached during initialization.

# In-memory references (will be loaded from persistent storage)
file_index = {}
lazy_content_manager = LazyContentManager(max_loaded_files=100)

# Global DAL instance
dal_instance: Optional[DALInterface] = None
core_engine: Optional[CoreEngine] = None  # Global CoreEngine instance

# Global variable to store the current project path persistently
_current_project_path: str = ""

# Global instance of GlobalConfigManager
global_config_manager = GlobalConfigManager()

# Global memory profiler - will be initialized when project is set
memory_profiler = None
memory_aware_manager = None

# Global performance monitor - will be initialized when project is set
performance_monitor = None

# CRITICAL FIX: Global search session tracker for deduplication across queries
# This prevents the same files from appearing in multiple related searches
_search_session_files: Dict[str, int] = {}  # Maps file_path -> last_shown_timestamp
_search_session_start: float = time.time()  # When the current session started

# ============================================================================
# PHASE 7 GLOBAL INSTANCES
# ============================================================================
# Global result ranker for search ranking
result_ranker: Optional[ResultRanker] = None

# Global API key manager for multi-key rotation and quota management
api_key_manager: Optional[APIKeyManager] = None

# Global stats collector for dashboard
stats_collector: Optional[IndexStatisticsCollector] = None


def ensure_result_ranker() -> ResultRanker:
    """Ensure result ranker is initialized, creating a default one if needed."""
    global result_ranker
    if result_ranker is None:
        try:
            # Load ranking config from environment if available
            config = RankingConfig()
            result_ranker = ResultRanker(config)
            logger.info("Initialized default result ranker")
        except Exception as e:
            logger.warning(f"Could not initialize result ranker: {e}")
            # Create a minimal ranker
            result_ranker = ResultRanker(RankingConfig())
    return result_ranker


def ensure_api_key_manager() -> Optional[APIKeyManager]:
    """Ensure API key manager is initialized from environment variables."""
    global api_key_manager
    if api_key_manager is None:
        try:
            # Try to create from environment
            storage_path = os.path.join(SETTINGS_DIR, "api_key_stats.json")
            api_key_manager = create_manager_from_env(
                prefix="CORE_ENGINE_API_KEY_", storage_path=storage_path
            )
            if api_key_manager and api_key_manager._keys:
                logger.info(
                    f"Initialized API key manager with {len(api_key_manager._keys)} keys"
                )
            else:
                logger.info(
                    "No API keys configured in environment, using single-key mode"
                )
                api_key_manager = None
        except Exception as e:
            logger.warning(f"Could not initialize API key manager: {e}")
            api_key_manager = None
    return api_key_manager


def ensure_stats_collector() -> IndexStatisticsCollector:
    """Ensure stats collector is initialized."""
    global stats_collector
    if stats_collector is None:
        try:
            pg_dsn = os.getenv("DATABASE_URL")
            es_url = os.getenv("ELASTICSEARCH_URL", "http://localhost:9200")
            stats_collector = IndexStatisticsCollector(pg_dsn=pg_dsn, es_url=es_url)
            logger.info("Initialized index statistics collector")
        except Exception as e:
            logger.warning(f"Could not initialize stats collector: {e}")
            # Create a minimal collector
            stats_collector = IndexStatisticsCollector()
    return stats_collector


def ensure_performance_monitor():
    """Ensure performance monitor is initialized, creating a default one if needed."""
    global performance_monitor
    if performance_monitor is None:
        try:
            performance_monitor = PerformanceMonitor()
            logger.info("Initialized default performance monitor")
        except Exception as e:
            logger.warning(f"Could not initialize performance monitor: {e}")
    return performance_monitor


# CRITICAL FIX: Search session deduplication helpers
def _reset_search_session():
    """Reset the search session tracker (e.g., when project changes)."""
    global _search_session_files, _search_session_start
    _search_session_files.clear()
    _search_session_start = time.time()
    logger.debug("Search session reset")


def _mark_files_as_shown(file_paths: List[str]):
    """Mark files as shown in the current search session."""
    global _search_session_files
    now = int(time.time())
    for path in file_paths:
        _search_session_files[path] = now


def _filter_recently_shown_files(file_paths: List[str], min_interval_seconds: int = 300) -> List[str]:
    """
    Filter out files that were shown recently in the search session.

    Args:
        file_paths: List of file paths to filter
        min_interval_seconds: Minimum time before showing same file again (default: 5 minutes)

    Returns:
        Filtered list of file paths, excluding recently shown ones
    """
    global _search_session_files
    now = int(time.time())
    filtered = []

    for path in file_paths:
        last_shown = _search_session_files.get(path, 0)
        if now - last_shown > min_interval_seconds:
            filtered.append(path)
        else:
            logger.debug(f"Filtering out recently shown file: {path}")

    return filtered


supported_extensions = [
    ".py",
    ".js",
    ".ts",
    ".jsx",
    ".tsx",
    ".java",
    ".c",
    ".cpp",
    ".h",
    ".hpp",
    ".cs",
    ".go",
    ".rb",
    ".php",
    ".swift",
    ".kt",
    ".rs",
    ".scala",
    ".sh",
    ".bash",
    ".html",
    ".css",
    ".scss",
    ".md",
    ".json",
    ".xml",
    ".yml",
    ".yaml",
    ".zig",
    # Frontend frameworks
    ".vue",
    ".svelte",
    ".mjs",
    ".cjs",
    # Style languages
    ".less",
    ".sass",
    ".stylus",
    ".styl",
    # Template engines
    ".hbs",
    ".handlebars",
    ".ejs",
    ".pug",
    # Modern frontend
    ".astro",
    ".mdx",
    # Database and SQL
    ".sql",
    ".ddl",
    ".dml",
    ".mysql",
    ".postgresql",
    ".psql",
    ".sqlite",
    ".mssql",
    ".oracle",
    ".ora",
    ".db2",
    # Database objects
    ".proc",
    ".procedure",
    ".func",
    ".function",
    ".view",
    ".trigger",
    ".index",
    # Database frameworks and tools
    ".migration",
    ".seed",
    ".fixture",
    ".schema",
    # NoSQL and modern databases
    ".cql",
    ".cypher",
    ".sparql",
    ".gql",
    # Database migration tools
    ".liquibase",
    ".flyway",
]


@dataclass
class LeIndexContext:
    """Context for the LeIndex MCP server."""

    base_path: str
    settings: OptimizedProjectSettings
    file_count: int = 0
    file_change_tracker: Optional[FileChangeTracker] = None
    dal: Optional[DALInterface] = None  # Add DAL instance to context
    core_engine: Optional[CoreEngine] = None  # Add CoreEngine to context
    # Phase 7 additions
    result_ranker: Optional[ResultRanker] = None
    api_key_manager: Optional[APIKeyManager] = None
    stats_collector: Optional[IndexStatisticsCollector] = None


@asynccontextmanager
async def indexer_lifespan(server: FastMCP) -> AsyncIterator[LeIndexContext]:
    """Manage the lifecycle of the LeIndex MCP server."""
    global dal_instance
    global result_ranker, api_key_manager, stats_collector

    # Load base_path from settings if available, otherwise default to empty string
    logger.info("Initializing LeIndex MCP server...")

    # Auto-start required infrastructure services (db only)
    # This ensures the MCP server works out-of-the-box without manual service startup
    logger.info("Checking infrastructure services (db)...")
    try:
        infra_ready = await async_ensure_infrastructure(
            required_services=["db"],
            timeout=120,  # 2 minutes max wait for services to start
            auto_start=True
        )
        if infra_ready:
            logger.info("All infrastructure services are running and healthy")
        else:
            logger.warning(
                "Some infrastructure services may not be available. "
                "The server will continue with limited functionality."
            )
    except Exception as e:
        logger.warning(f"Error ensuring infrastructure services: {e}. Continuing with startup...")

    # Initialize a temporary settings manager to load the base_path from the default config location
    # This ensures we can retrieve the last saved project path even if the server restarts
    default_settings = OptimizedProjectSettings(
        "", skip_load=True, storage_backend="sqlite", use_trie_index=True
    )
    base_path_from_config = default_settings.load_config().get("base_path", "")

    global _current_project_path
    _current_project_path = base_path_from_config  # Initialize global variable

    # Initialize the actual settings manager with the loaded base_path
    # This settings object will be used throughout the server's lifespan
    settings = OptimizedProjectSettings(
        base_path_from_config,
        skip_load=not bool(base_path_from_config),
        storage_backend="sqlite",
        use_trie_index=True,
    )

    # Update the base_path in the settings object itself, as it might have been initialized with an empty string
    settings.base_path = base_path_from_config

    # Initialize DAL instance (use configured backend type)
    dal_instance = get_dal_instance()

    # ============================================================================
    # META-REGISTRY: Startup Migration (Phase 3)
    # ============================================================================
    # Check for and migrate legacy pickle indexes to MessagePack format
    # This runs automatically on server startup to ensure all indexes are
    # in the current format before the server starts serving requests
    logger.info("Checking for legacy pickle indexes that need migration...")
    try:
        migration_state = check_and_migrate_on_startup(
            project_path=base_path_from_config if base_path_from_config else None,
            auto_migrate=True,
            project_registry=None  # Could be added later if registry tracking is needed
        )
        if migration_state.migration_performed:
            logger.info(
                f"Startup migration completed: {migration_state.summary()}"
            )
        else:
            logger.debug("No startup migration required")
    except Exception as e:
        # Log error but don't fail startup - migration can be retried later
        logger.error(f"Error during startup migration: {e}. Continuing with startup...")

    # ============================================================================
    # PHASE 6: Automatic Backup System
    # ============================================================================
    # Initialize backup scheduler and perform startup backup check
    # This ensures the registry is backed up if >24h since last backup
    logger.info("Initializing automatic backup system...")
    try:
        from .registry.project_registry import ProjectRegistry
        from .registry.directories import get_registry_db_path

        # Initialize project registry for backup operations
        registry_db_path = get_registry_db_path()
        project_registry = ProjectRegistry(db_path=registry_db_path)

        # Get or create backup scheduler
        backup_scheduler = get_backup_scheduler()

        # Perform startup backup check (non-blocking async)
        backup_created, backup_msg = await backup_scheduler.startup_backup_check(
            project_registry
        )
        if backup_created:
            logger.info(f"Startup backup created: {backup_msg}")
        else:
            logger.debug(f"Startup backup check: {backup_msg}")

        # Setup signal handlers for graceful shutdown
        setup_signal_handlers(backup_scheduler)

        logger.info("Automatic backup system initialized")

    except Exception as e:
        # Log error but don't fail startup - backup system can retry later
        logger.error(f"Error initializing backup system: {e}. Continuing with startup...")
        backup_scheduler = None
        project_registry = None

    # ============================================================================
    # PHASE 7: Initialize API Key Manager and Result Ranker
    # ============================================================================
    # The API key manager must be initialized before CoreEngine so that
    # VectorBackend can use it for multi-key rotation and quota management
    api_key_manager = ensure_api_key_manager()
    if api_key_manager:
        logger.info(
            f"API Key Manager initialized with {len(api_key_manager._keys)} keys"
        )
    else:
        logger.info("API Key Manager not configured - using single-key mode")

    # Initialize Result Ranker for search ranking
    result_ranker = ensure_result_ranker()
    if result_ranker:
        logger.info("Result Ranker initialized for enhanced search ranking")

    # Initialize Stats Collector for dashboard
    stats_collector = ensure_stats_collector()
    if stats_collector:
        logger.info("Statistics Collector initialized for dashboard")

    # Initialize Vector Backend based on configuration
    # Read vector_store.backend_type from config to choose between LEANN and FAISS
    vector_store_settings = global_config_manager.get_vector_store_settings()
    backend_type = vector_store_settings.get('backend_type', 'leann').lower()

    logger.info(f"Initializing vector backend with type: {backend_type}")

    if backend_type == 'leann':
        # Initialize LEANNVectorBackend (LEANN-based, storage-efficient)
        from .core_engine.leann_backend import LEANNVectorBackend

        # Get LEANN-specific settings
        leann_backend = vector_store_settings.get('leann_backend', 'hnsw')
        index_path = vector_store_settings.get('index_path', './leann_index')
        embedding_model = vector_store_settings.get('embedding_model', 'nomic-ai/CodeRankEmbed')
        embedding_dim = vector_store_settings.get('embedding_dim', 768)

        vector_backend = LEANNVectorBackend(
            backend_name=leann_backend,
            index_path=index_path,
            dimension=embedding_dim,
            model_name=embedding_model
        )
        backend_name = "LEANNVectorBackend"
    elif backend_type == 'faiss':
        # Initialize LocalVectorBackend (FAISS-based, no cloud dependency)
        from .core_engine.local_vector_backend import LocalVectorBackend

        vector_backend = LocalVectorBackend()
        backend_name = "LocalVectorBackend"
    else:
        logger.warning(
            f"Unknown vector store backend type: '{backend_type}'. "
            f"Supported types: 'leann', 'faiss'. Defaulting to 'leann'."
        )
        from .core_engine.leann_backend import LEANNVectorBackend
        vector_backend = LEANNVectorBackend()
        backend_name = "LEANNVectorBackend (defaulted)"

    # CRITICAL FIX: Initialize the vector backend to load the embedding model and create index
    # This is an async operation that downloads the model if needed and sets up the index
    try:
        await vector_backend.initialize()
        logger.info(f"{backend_name} initialized successfully")
    except Exception as e:
        logger.error(f"Failed to initialize {backend_name}: {e}. Semantic search will be unavailable.")
        # Continue without semantic search - server will still work with Elasticsearch/Zoekt

    # Initialize Core Engine with configured backends
    core_engine = CoreEngine(vector_backend=vector_backend, legacy_backend=dal_instance)
    logger.info("Core Engine initialized with API key manager support")

    # Initialize IncrementalIndexer and FileChangeTracker
    incremental_indexer = IncrementalIndexer(settings)
    file_change_tracker = FileChangeTracker(dal_instance.metadata, incremental_indexer)
    logger.info("FileChangeTracker initialized with DAL metadata backend")

    # Initialize context with Phase 7 modules
    context = LeIndexContext(
        base_path=base_path_from_config,
        settings=settings,
        file_change_tracker=file_change_tracker,
        dal=dal_instance,  # Store DAL instance in context
        core_engine=core_engine,
        # Phase 7 additions
        result_ranker=result_ranker,
        api_key_manager=api_key_manager,
        stats_collector=stats_collector,
    )

    try:
        # Start periodic backup task
        if backup_scheduler and project_registry:
            backup_scheduler.start_periodic_backup(project_registry)

        logger.info("Server ready. Waiting for user to set project path...")
        yield context
    finally:
        # ========================================================================
        # PHASE 6: Stop periodic backup task
        # ========================================================================
        if backup_scheduler:
            logger.info("Stopping periodic backup task...")
            try:
                await backup_scheduler.stop_periodic_backup()
                logger.info("Periodic backup task stopped")
            except Exception as e:
                logger.error(f"Error stopping periodic backup task: {e}")

        # CRITICAL: Explicit flush before shutdown to ensure all data is persisted
        logger.info("Flushing storage backends before shutdown...")

        # Flush DAL instance if it has a flush method
        if dal_instance:
            logger.info("Flushing DAL instance...")
            if hasattr(dal_instance, 'flush'):
                dal_instance.flush()
                logger.info("DAL instance flushed.")

            # Close DAL instance
            logger.info("Closing DAL instance...")
            dal_instance.close()
            logger.info("DAL instance closed.")

        # Flush settings storage backends (SQLiteStorage, SQLiteFileMetadata)
        if settings:
            logger.info("Flushing settings storage backends...")
            if hasattr(settings, 'cache_storage') and hasattr(settings.cache_storage, 'flush'):
                try:
                    settings.cache_storage.flush()
                    logger.info("Cache storage flushed.")
                except Exception as e:
                    logger.error(f"Error flushing cache storage: {e}")

            if hasattr(settings, 'metadata_storage') and hasattr(settings.metadata_storage, 'flush'):
                try:
                    settings.metadata_storage.flush()
                    logger.info("Metadata storage flushed.")
                except Exception as e:
                    logger.error(f"Error flushing metadata storage: {e}")

            if hasattr(settings, 'file_index') and hasattr(settings.file_index, 'flush'):
                try:
                    settings.file_index.flush()
                    logger.info("File index flushed.")
                except Exception as e:
                    logger.error(f"Error flushing file index: {e}")

        # Stop RealtimeIndexer worker thread
        if realtime_indexer:
            logger.info("Stopping RealtimeIndexer...")
            realtime_indexer.stop()
            logger.info("RealtimeIndexer stopped.")

        # Only save index if project path has been set
        if context.base_path and file_index:
            logger.info(f"Saving index for project: {context.base_path}")
            settings.save_index(file_index)

        # Export memory profile on shutdown if configured
        global memory_profiler
        if memory_profiler:
            try:
                config_manager = ConfigManager()
                config_data = config_manager.load_config()

                if config_data.get("memory", {}).get(
                    "export_profile_on_shutdown", True
                ):
                    import tempfile

                    timestamp = int(time.time())
                    profile_path = os.path.join(
                        tempfile.gettempdir(),
                        f"memory_profile_shutdown_{timestamp}.json",
                    )
                    memory_profiler.export_profile(profile_path)
                    logger.info(f"Memory profile exported to: {profile_path}")

                # Stop monitoring
                memory_profiler.stop_monitoring()
                logger.info("Memory monitoring stopped")
            except Exception as e:
                logger.error(f"Error during memory profiler shutdown: {e}")

        # Save memory stats for loaded files
        memory_stats = lazy_content_manager.get_memory_stats()
        logger.info(f"Memory Stats: {memory_stats}")


# Initialize the server with our lifespan manager
mcp = FastMCP("LeIndex", lifespan=indexer_lifespan)

# ----- RESOURCES -----


@mcp.resource("storage://info")
def get_storage_info() -> str:
    """Get storage information for the current configuration."""
    ctx = mcp.get_context()
    settings = ctx.request_context.lifespan_context.settings
    storage_info = settings.get_storage_info()
    return json.dumps(storage_info, indent=2)


@mcp.resource("config://leindex")
def get_config() -> str:
    """Get the current configuration of the LeIndex."""
    ctx = mcp.get_context()

    # Get the base path from context
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return json.dumps(
            {
                "status": "not_configured",
                "message": "Project path not set. Please use set_project_path to set a project directory first.",
                "supported_extensions": supported_extensions,
            },
            indent=2,
        )

    # Get file count
    file_count = ctx.request_context.lifespan_context.file_count

    # Get settings stats
    settings = ctx.request_context.lifespan_context.settings
    settings_stats = settings.get_stats()

    config = {
        "base_path": base_path,
        "supported_extensions": supported_extensions,
        "file_count": file_count,
        "settings_directory": settings.settings_path,
        "settings_stats": settings_stats,
    }

    return json.dumps(config, indent=2)


@mcp.resource("files://{file_path}")
def get_file_content(file_path: str) -> str:
    """Get the content of a specific file using enhanced SmartFileReader."""
    # Handle both MCP context and direct calls
    try:
        ctx = mcp.get_context()
        # Get the base path from context
        base_path = ctx.request_context.lifespan_context.base_path

        # Check if base_path is set
        if not base_path:
            return "Error: Project path not set. Please use set_project_path to set a project directory first."
    except Exception:
        # Fallback for non-MCP calls - use global project path
        base_path = _current_project_path
        if not base_path:
            return "Error: Project path not set. Please use set_project_path to set a project directory first."

    # Handle absolute paths (especially Windows paths starting with drive letters)
    if os.path.isabs(file_path) or (len(file_path) > 1 and file_path[1] == ":"):
        # Absolute paths are not allowed via this endpoint
        return f"Error: Absolute file paths like '{file_path}' are not allowed. Please use paths relative to the project root."

    # Normalize the file path
    norm_path = os.path.normpath(file_path)

    # Check for path traversal attempts
    if "..\\" in norm_path or "../" in norm_path or norm_path.startswith(".."):
        return (
            f"Error: Invalid file path: {file_path} (directory traversal not allowed)"
        )

    # Construct the full path and verify it's within the project bounds
    full_path = os.path.join(base_path, norm_path)
    real_full_path = os.path.realpath(full_path)
    real_base_path = os.path.realpath(base_path)

    if not real_full_path.startswith(real_base_path):
        return "Error: Access denied. File path must be within project directory."

    try:
        # Use SmartFileReader for enhanced content loading with better error handling
        smart_reader = SmartFileReader(base_path)
        content = smart_reader.read_content(full_path)

        if content is None:
            return "Error reading file: Unable to decode or access"

        return content
    except Exception as e:
        logger.error(f"Error reading file {full_path}: {e}", exc_info=True)
        return f"Error reading file: {e}"


@mcp.resource("structure://project")
async def get_project_structure() -> str:
    """Get the structure of the project as a JSON tree."""
    ctx = mcp.get_context()

    # Get the base path from context
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return json.dumps(
            {
                "status": "not_configured",
                "message": "Project path not set. Please use set_project_path to set a project directory first.",
            },
            indent=2,
        )

    # Check if we need to refresh the index
    if not file_index:
        await _index_project(
            base_path, ctx.request_context.lifespan_context.core_engine
        )
        # Update file count in context
        ctx.request_context.lifespan_context.file_count = _count_files(file_index)
        # Save updated index
        ctx.request_context.lifespan_context.settings.save_index(file_index)

    return json.dumps(file_index, indent=2)


@mcp.resource("settings://stats")
def get_settings_stats() -> str:
    """Get statistics about the settings directory and files."""
    ctx = mcp.get_context()

    # Get settings manager from context
    settings = ctx.request_context.lifespan_context.settings

    # Get settings stats
    stats = settings.get_stats()

    return json.dumps(stats, indent=2)


# ----- TOOLS -----

# =============================================================================
# MEGA-TOOLS: Consolidated 9 mega-tools that replace 50+ individual tools
# Each mega-tool uses action/type/operation/mode-based routing
# =============================================================================


# -----------------------------------------------------------------------------
# MEGA-TOOL 1: manage_project
# Consolidates: set_project_path, refresh_index, force_reindex, clear_settings, reset_server_state
# -----------------------------------------------------------------------------
@mcp.tool()
async def manage_project(
    ctx: Context,
    action: Literal["set_path", "refresh", "reindex", "clear", "reset"],
    path: Optional[str] = None,
    clear_cache: bool = True,
) -> Union[str, Dict[str, Any]]:
    """
    Manage project lifecycle operations including setting path, refreshing, and reindexing.

    This mega-tool consolidates all project-level operations into a single interface
    with action-based routing.

    Actions:
        - "set_path": Set the base project path for indexing (requires: path)
        - "refresh": Refresh the project index using incremental indexing
        - "reindex": Force a complete re-index of the project (params: clear_cache)
        - "clear": Clear all settings and cached data
        - "reset": Completely reset the server state including global variables

    Examples:
        await manage_project(ctx, "set_path", path="/path/to/project")
        await manage_project(ctx, "refresh")
        await manage_project(ctx, "reindex", clear_cache=True)
        await manage_project(ctx, "clear")
        await manage_project(ctx, "reset")
    """
    match action:
        case "set_path":
            if path is None:
                return {
                    "success": False,
                    "error": "path parameter is required for set_path action",
                }
            return await set_project_path(path, ctx)
        case "refresh":
            return await refresh_index(ctx)
        case "reindex":
            return await force_reindex(ctx, clear_cache)
        case "clear":
            return clear_settings(ctx)
        case "reset":
            return reset_server_state(ctx)
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 2: search_content
# Consolidates: search_code_advanced, find_files, rank_search_results
# -----------------------------------------------------------------------------
@mcp.tool()
async def search_content(
    ctx: Context,
    action: Literal["search", "find", "rank"],
    pattern: Optional[str] = None,
    # Parameters for "search" action
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    fuzziness_level: Optional[str] = None,
    content_boost: float = 1.0,
    filepath_boost: float = 1.0,
    highlight_pre_tag: str = "<em>",
    highlight_post_tag: str = "</em>",
    page: int = 1,
    page_size: int = 20,
    # Parameters for "rank" action
    results: Optional[List[Dict[str, Any]]] = None,
    query: Optional[str] = None,
) -> Union[Dict[str, Any], List[str], List[Dict[str, Any]]]:
    """
    Search and discover content across the project using multiple strategies.

    This mega-tool provides unified access to all content search and discovery operations.

    Actions:
        - "search": Advanced code search with multiple backend support (requires: pattern)
        - "find": Find files matching a glob pattern (requires: pattern)
        - "rank": Re-rank search results based on query relevance (requires: results, query)

    Examples:
        await search_content(ctx, "search", pattern="function foo()", fuzzy=True)
        await search_content(ctx, "find", pattern="*.py")
        await search_content(ctx, "rank", results=search_results, query="auth logic")
    """
    match action:
        case "search":
            if pattern is None:
                return {
                    "success": False,
                    "error": "pattern parameter is required for search action",
                }
            return await search_code_advanced(
                pattern=pattern,
                ctx=ctx,
                case_sensitive=case_sensitive,
                context_lines=context_lines,
                file_pattern=file_pattern,
                fuzzy=fuzzy,
                fuzziness_level=fuzziness_level,
                content_boost=content_boost,
                filepath_boost=filepath_boost,
                highlight_pre_tag=highlight_pre_tag,
                highlight_post_tag=highlight_post_tag,
                page=page,
                page_size=page_size,
            )
        case "find":
            if pattern is None:
                return {
                    "success": False,
                    "error": "pattern parameter is required for find action",
                }
            return find_files(pattern, ctx)
        case "rank":
            if results is None:
                return {
                    "success": False,
                    "error": "results parameter is required for rank action",
                }
            if query is None:
                return {
                    "success": False,
                    "error": "query parameter is required for rank action",
                }
            return await rank_search_results(results, query, ctx)
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 3: manage_file
# Consolidates: write_to_file, apply_diff, insert_content, search_and_replace
# Note: Named 'manage_file' (singular) to distinguish from 'manage_files' (plural)
# -----------------------------------------------------------------------------
@mcp.tool()
async def manage_file(
    ctx: Context,
    operation: Literal["write", "diff", "insert", "replace"],
    path: str,
    content: Optional[str] = None,
    line_count: Optional[int] = None,
    search: Optional[str] = None,
    replace: Optional[str] = None,
    start_line: Optional[int] = None,
    end_line: Optional[int] = None,
    use_regex: bool = False,
    ignore_case: bool = False,
    line: Optional[int] = None,
) -> Dict[str, Any]:
    """
    Modify file content using various strategies with version tracking.

    This mega-tool consolidates all file content modification operations.

    Operations:
        - "write": Write complete content to a file (requires: content, line_count)
        - "diff": Apply targeted modifications using search/replace (requires: search, replace)
        - "insert": Insert new content at specified line (requires: content, line)
        - "replace": Search and replace in file (requires: search, replace)

    Examples:
        await manage_file(ctx, "write", path="src/main.py", content="code", line_count=1)
        await manage_file(ctx, "diff", path="config.json", search="old", replace="new")
        await manage_file(ctx, "insert", path="README.md", line=10, content="new section")
        await manage_file(ctx, "replace", path="api.md", search="TODO", replace="DONE")
    """
    match operation:
        case "write":
            if content is None or line_count is None:
                return {
                    "success": False,
                    "error": "content and line_count required for write operation",
                }
            return await write_to_file(path, content, line_count, ctx)
        case "diff":
            if search is None or replace is None:
                return {
                    "success": False,
                    "error": "search and replace required for diff operation",
                }
            return await apply_diff(
                path, search, replace, ctx, start_line, end_line, use_regex, ignore_case
            )
        case "insert":
            if content is None or line is None:
                return {
                    "success": False,
                    "error": "content and line required for insert operation",
                }
            return await insert_content(path, line, content, ctx)
        case "replace":
            if search is None or replace is None:
                return {
                    "success": False,
                    "error": "search and replace required for replace operation",
                }
            return await search_and_replace(
                path, search, replace, ctx, start_line, end_line, use_regex, ignore_case
            )
        case _:
            return {"success": False, "error": f"Unknown operation: {operation}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 4: manage_files
# Consolidates: delete_file, rename_file, revert_file_to_version, get_file_history
# -----------------------------------------------------------------------------
@mcp.tool()
async def manage_files(
    ctx: Context,
    action: Literal["delete", "rename", "revert", "history"],
    file_path: Optional[str] = None,
    new_file_path: Optional[str] = None,
    version_id: Optional[str] = None,
    timestamp: Optional[str] = None,
) -> Dict[str, Any]:
    """
    Manage file system operations including delete, rename, revert, and history.

    This mega-tool provides comprehensive file management with version tracking.

    Actions:
        - "delete": Delete a file from the filesystem (requires: file_path)
        - "rename": Rename or move a file (requires: file_path, new_file_path)
        - "revert": Revert a file to a previous version (requires: file_path, version_id or timestamp)
        - "history": Get the change history for a file (requires: file_path)

    Examples:
        await manage_files(ctx, "delete", file_path="old_file.py")
        await manage_files(ctx, "rename", file_path="src/old.py", new_file_path="src/new.py")
        await manage_files(ctx, "revert", file_path="config.json", version_id="v1.2.3")
        await manage_files(ctx, "history", file_path="src/main.py")
    """
    match action:
        case "delete":
            if file_path is None:
                return {
                    "success": False,
                    "error": "file_path parameter is required for delete action",
                }
            return await delete_file(file_path, ctx)
        case "rename":
            if file_path is None or new_file_path is None:
                return {
                    "success": False,
                    "error": "file_path and new_file_path required for rename action",
                }
            return await rename_file(file_path, new_file_path, ctx)
        case "revert":
            if file_path is None:
                return {
                    "success": False,
                    "error": "file_path parameter is required for revert action",
                }
            return await revert_file_to_version(file_path, ctx, version_id, timestamp)
        case "history":
            if file_path is None:
                return {
                    "success": False,
                    "error": "file_path parameter is required for history action",
                }
            return get_file_history(file_path, ctx)
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 5: get_diagnostics
# Consolidates: get_memory_profile, get_index_statistics, get_backend_health,
#               get_performance_metrics, get_active_operations, get_settings_info,
#               get_ignore_patterns, get_filtering_config, get_ranking_configuration
# -----------------------------------------------------------------------------
@mcp.tool()
async def get_diagnostics(
    ctx: Context,
    type: Literal[
        "memory",
        "index",
        "backend",
        "performance",
        "operations",
        "settings",
        "ignore",
        "filtering",
        "ranking",
    ],
    force_refresh: bool = False,
) -> Dict[str, Any]:
    """
    Get comprehensive diagnostics and metrics for all system components.

    This mega-tool provides unified access to all diagnostic information.

    Types:
        - "memory": Get comprehensive memory profiling statistics
        - "index": Get comprehensive index statistics (params: force_refresh)
        - "backend": Get health status of all backends
        - "performance": Get performance monitoring metrics
        - "operations": Get status of all active operations
        - "settings": Get information about project settings
        - "ignore": Get information about loaded ignore patterns
        - "filtering": Get current filtering configuration
        - "ranking": Get search ranking configuration

    Examples:
        await get_diagnostics(ctx, "memory")
        await get_diagnostics(ctx, "index", force_refresh=True)
        await get_diagnostics(ctx, "backend")
        await get_diagnostics(ctx, "performance")
    """
    match type:
        case "memory":
            return get_memory_profile()
        case "index":
            return await get_index_statistics(ctx, force_refresh)
        case "backend":
            return await get_backend_health(ctx)
        case "performance":
            return get_performance_metrics()
        case "operations":
            return get_active_operations()
        case "settings":
            return get_settings_info(ctx)
        case "ignore":
            return get_ignore_patterns(ctx)
        case "filtering":
            return get_filtering_config()
        case "ranking":
            return get_ranking_configuration(ctx)
        case _:
            return {"success": False, "error": f"Unknown type: {type}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 6: manage_memory
# Consolidates: trigger_memory_cleanup, configure_memory_limits, export_memory_profile
# -----------------------------------------------------------------------------
@mcp.tool()
def manage_memory(
    ctx: Context,
    action: Literal["cleanup", "configure", "export"],
    soft_limit_mb: Optional[float] = None,
    hard_limit_mb: Optional[float] = None,
    max_loaded_files: Optional[int] = None,
    max_cached_queries: Optional[int] = None,
    file_path: Optional[str] = None,
) -> Dict[str, Any]:
    """
    Manage memory usage including cleanup, configuration, and profiling.

    This mega-tool provides comprehensive memory management capabilities.

    Actions:
        - "cleanup": Manually trigger memory cleanup and garbage collection
        - "configure": Update memory limits configuration (params: soft_limit_mb, hard_limit_mb, etc.)
        - "export": Export detailed memory profile to a file (params: file_path)

    Examples:
        manage_memory(ctx, "cleanup")
        manage_memory(ctx, "configure", soft_limit_mb=1024)
        manage_memory(ctx, "export", file_path="/tmp/profile.json")
    """
    match action:
        case "cleanup":
            return trigger_memory_cleanup()
        case "configure":
            return configure_memory_limits(
                soft_limit_mb, hard_limit_mb, max_loaded_files, max_cached_queries
            )
        case "export":
            return export_memory_profile(file_path)
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 7: manage_operations
# Consolidates: get_active_operations, cancel_operation, cancel_all_operations,
#                cleanup_completed_operations
# -----------------------------------------------------------------------------
@mcp.tool()
async def manage_operations(
    ctx: Context,
    action: Literal["list", "cancel", "cleanup"],
    operation_id: Optional[str] = None,
    reason: str = "Operation cancelled by user",
    max_age_hours: float = 1.0,
    cancel_all: bool = False,
) -> Dict[str, Any]:
    """
    Manage tracked operations including listing, cancelling, and cleanup.

    This mega-tool provides comprehensive operation lifecycle management.

    Actions:
        - "list": Get status of all active operations
        - "cancel": Cancel a specific operation or all operations (params: operation_id or cancel_all)
        - "cleanup": Clean up completed operations older than specified hours (params: max_age_hours)

    Examples:
        await manage_operations(ctx, "list")
        await manage_operations(ctx, "cancel", operation_id="op-123")
        await manage_operations(ctx, "cancel", cancel_all=True, reason="shutdown")
        await manage_operations(ctx, "cleanup", max_age_hours=2.0)
    """
    match action:
        case "list":
            return get_active_operations()
        case "cancel":
            if cancel_all:
                return await cancel_all_operations(reason)
            if operation_id is None:
                return {
                    "success": False,
                    "error": "operation_id parameter is required for cancel action",
                }
            return await cancel_operation(operation_id, reason)
        case "cleanup":
            return cleanup_completed_operations(max_age_hours)
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 8: read_file
# Consolidates: analyze_file_with_smart_reader, read_file_chunks,
#               detect_file_errors, get_file_metadata
# -----------------------------------------------------------------------------
@mcp.tool()
def read_file(
    ctx: Context,
    mode: Literal["smart", "chunks", "detect_errors", "metadata"],
    file_path: str,
    include_content: bool = True,
    include_metadata: bool = True,
    include_errors: bool = True,
    include_chunks: bool = False,
    chunk_size: int = 4 * 1024 * 1024,
    max_chunks: Optional[int] = None,
) -> Dict[str, Any]:
    """
    Read files using various strategies optimized for different use cases.

    This mega-tool provides flexible file reading capabilities with automatic
    strategy selection based on file characteristics.

    Modes:
        - "smart": Comprehensive file analysis using SmartFileReader
        - "chunks": Read large file in chunks for memory efficiency
        - "detect_errors": Detect and analyze errors in a file
        - "metadata": Get comprehensive file metadata

    Examples:
        read_file(ctx, "smart", file_path="src/main.py", include_errors=True)
        read_file(ctx, "chunks", file_path="large.json", chunk_size=8*1024*1024)
        read_file(ctx, "detect_errors", file_path="config.py")
        read_file(ctx, "metadata", file_path="README.md")
    """
    match mode:
        case "smart":
            return analyze_file_with_smart_reader(
                file_path=file_path,
                ctx=ctx,
                include_content=include_content,
                include_metadata=include_metadata,
                include_errors=include_errors,
                include_chunks=include_chunks,
                chunk_size=chunk_size,
            )
        case "chunks":
            return read_file_chunks(
                file_path=file_path,
                ctx=ctx,
                chunk_size=chunk_size,
                max_chunks=max_chunks,
            )
        case "detect_errors":
            return detect_file_errors(file_path, ctx)
        case "metadata":
            return get_file_metadata(file_path, ctx)
        case _:
            return {"success": False, "error": f"Unknown mode: {mode}"}


# -----------------------------------------------------------------------------
# MEGA-TOOL 9: manage_temp
# Consolidates: create_temp_directory, check_temp_directory
# -----------------------------------------------------------------------------
@mcp.tool()
def manage_temp(
    ctx: Context,
    action: Literal["create", "check"],
) -> Dict[str, Any]:
    """
    Manage the temporary directory used for storing index data.

    This mega-tool provides simple operations for creating and checking
    the temporary directory where the indexer stores cached data.

    Actions:
        - "create": Create the temporary directory if it doesn't exist
        - "check": Check the temporary directory status and contents

    Examples:
        manage_temp(ctx, "create")
        manage_temp(ctx, "check")
    """
    match action:
        case "create":
            return create_temp_directory()
        case "check":
            return check_temp_directory()
        case _:
            return {"success": False, "error": f"Unknown action: {action}"}


# =============================================================================
# META-REGISTRY MCP TOOLS (Phase 5)
# Management tools for the project registry system
# =============================================================================

# -----------------------------------------------------------------------------
# Tool 1: get_registry_status
# Returns statistics and status information about the project registry
# -----------------------------------------------------------------------------
@mcp.tool()
async def get_registry_status(
    ctx: Context,
) -> Dict[str, Any]:
    """
    Get status and statistics about the project registry.

    Returns comprehensive information about the registry including:
    - Total project count
    - Last indexed project
    - Oldest and newest indexed projects
    - Storage location
    - Database health
    - Format statistics (pickle vs MessagePack)

    Returns:
        Dictionary with registry status information

    Example:
        {
            "success": true,
            "project_count": 5,
            "last_indexed": "2025-01-01T12:00:00",
            "registry_path": "/home/user/.leindex_data/registry.db",
            "oldest_project": "/path/to/old",
            "newest_project": "/path/to/new",
            "formats": {"msgpack": 4, "pickle": 1}
        }
    """
    from .registry import ProjectRegistry

    try:
        registry = ProjectRegistry()

        # Get basic statistics
        projects = registry.list_all()
        project_count = len(projects)

        # Get last indexed
        last_indexed = None
        if projects:
            # Sort by indexed_at, newest first
            sorted_projects = sorted(
                projects,
                key=lambda p: p.indexed_at,
                reverse=True
            )
            last_indexed = sorted_projects[0].indexed_at.isoformat()

        # Get oldest and newest projects
        oldest_project = None
        newest_project = None
        if project_count > 0:
            oldest_project = min(projects, key=lambda p: p.indexed_at).path
            newest_project = max(projects, key=lambda p: p.indexed_at).path

        # Check format statistics
        format_stats = {"msgpack": 0, "pickle": 0, "unknown": 0}
        from .registry.msgpack_serializer import MessagePackSerializer
        serializer = MessagePackSerializer()

        for project in projects:
            index_dir = pathlib.Path(project.index_location)
            if index_dir.exists():
                # Check for MessagePack files
                msgpack_files = list(index_dir.rglob("*.msgpack"))
                pickle_files = list(index_dir.rglob("*.pickle"))

                if msgpack_files:
                    format_stats["msgpack"] += 1
                elif pickle_files:
                    format_stats["pickle"] += 1
                else:
                    format_stats["unknown"] += 1

        return {
            "success": True,
            "project_count": project_count,
            "last_indexed": last_indexed,
            "registry_path": str(registry.db_path),
            "oldest_project": oldest_project,
            "newest_project": newest_project,
            "formats": format_stats,
            "registry_exists": registry.db_path.exists(),
        }

    except Exception as e:
        logger.error(f"Error getting registry status: {e}")
        return {
            "success": False,
            "error": str(e),
        }


# -----------------------------------------------------------------------------
# Tool 2: registry_health_check
# Performs health checks on all registered projects
# -----------------------------------------------------------------------------
@mcp.tool()
async def registry_health_check(
    ctx: Context,
) -> Dict[str, Any]:
    """
    Perform health checks on all registered projects.

    Checks:
    - Project paths exist
    - Index files exist
    - MessagePack integrity

    Returns per-project health details and overall status.

    Returns:
        Dictionary with health check results for each project and overall status

    Example:
        {
            "success": true,
            "overall_status": "healthy",
            "projects": {
                "/path/to/project": {
                    "path_exists": true,
                    "index_exists": true,
                    "index_valid": true,
                    "status": "healthy"
                }
            },
            "summary": {"healthy": 4, "warning": 1, "critical": 0}
        }
    """
    from .registry import ProjectRegistry
    from .registry.msgpack_serializer import MessagePackSerializer

    try:
        registry = ProjectRegistry()
        projects = registry.list_all()

        if not projects:
            return {
                "success": True,
                "overall_status": "healthy",
                "message": "No projects in registry",
                "projects": {},
                "summary": {"healthy": 0, "warning": 0, "critical": 0}
            }

        serializer = MessagePackSerializer()
        project_health = {}
        summary = {"healthy": 0, "warning": 0, "critical": 0}

        for project in projects:
            health = {
                "path_exists": False,
                "index_exists": False,
                "index_valid": False,
                "status": "unknown",
                "issues": []
            }

            # Check if project path exists
            project_path = pathlib.Path(project.path)
            health["path_exists"] = project_path.exists()

            if not health["path_exists"]:
                health["status"] = "critical"
                health["issues"].append("Project path does not exist")
                summary["critical"] += 1
            else:
                # Check if index exists
                index_dir = pathlib.Path(project.index_location)
                health["index_exists"] = index_dir.exists()

                if not health["index_exists"]:
                    health["status"] = "warning"
                    health["issues"].append("Index directory does not exist")
                    summary["warning"] += 1
                else:
                    # Check index integrity
                    try:
                        # Try to find and validate an index file
                        index_files = list(index_dir.rglob("*.msgpack")) + \
                                     list(index_dir.rglob("*.pickle"))

                        if index_files:
                            health["index_valid"] = all(
                                serializer.validate_index_file(f)[0]
                                for f in index_files
                            )

                            if not health["index_valid"]:
                                health["status"] = "warning"
                                health["issues"].append("Index file validation failed")
                                summary["warning"] += 1
                            else:
                                health["status"] = "healthy"
                                summary["healthy"] += 1
                        else:
                            health["status"] = "warning"
                            health["issues"].append("No index files found")
                            summary["warning"] += 1

                    except Exception as e:
                        health["status"] = "critical"
                        health["issues"].append(f"Error checking index: {e}")
                        summary["critical"] += 1

            project_health[project.path] = health

        # Determine overall status
        if summary["critical"] > 0:
            overall_status = "critical"
        elif summary["warning"] > 0:
            overall_status = "warning"
        else:
            overall_status = "healthy"

        return {
            "success": True,
            "overall_status": overall_status,
            "projects": project_health,
            "summary": summary,
        }

    except Exception as e:
        logger.error(f"Error during registry health check: {e}")
        return {
            "success": False,
            "error": str(e),
        }


# -----------------------------------------------------------------------------
# Tool 3: registry_cleanup
# Removes invalid projects from the registry
# -----------------------------------------------------------------------------
@mcp.tool()
async def registry_cleanup(
    ctx: Context,
    force: bool = False,
) -> Dict[str, Any]:
    """
    Clean up the registry by removing invalid projects.

    Removes projects where:
    - Project path does not exist
    - Index is corrupted

    Args:
        force: If True, skip confirmation. If False, requires confirmation.

    Returns:
        Dictionary with cleanup results

    Example:
        {
            "success": true,
            "removed_count": 2,
            "removed_projects": ["/path/to/old1", "/path/to/old2"],
            "backup_path": "/path/to/backup.db"
        }
    """
    from .registry import ProjectRegistry, RegistryBackupManager

    try:
        # Get health status first
        health_result = await registry_health_check(ctx)

        if not health_result.get("success"):
            return health_result

        # Identify projects to remove (critical or warning with missing path)
        projects_to_remove = []
        for project_path, health in health_result["projects"].items():
            if health["status"] == "critical" or not health["path_exists"]:
                projects_to_remove.append(project_path)

        if not projects_to_remove:
            return {
                "success": True,
                "removed_count": 0,
                "removed_projects": [],
                "message": "No invalid projects found to remove"
            }

        # Create backup before cleanup (non-blocking)
        backup_manager = RegistryBackupManager()
        registry = ProjectRegistry()
        backup_metadata = await backup_manager.create_backup_async(registry=registry)

        # Remove invalid projects
        removed_projects = []
        for project_path in projects_to_remove:
            try:
                if registry.delete(project_path):
                    removed_projects.append(project_path)
                    logger.info(f"Removed invalid project: {project_path}")
            except Exception as e:
                logger.warning(f"Failed to remove project {project_path}: {e}")

        return {
            "success": True,
            "removed_count": len(removed_projects),
            "removed_projects": removed_projects,
            "backup_path": str(backup_metadata.backup_path),
            "message": f"Removed {len(removed_projects)} invalid projects"
        }

    except Exception as e:
        logger.error(f"Error during registry cleanup: {e}")
        return {
            "success": False,
            "error": str(e),
        }


# -----------------------------------------------------------------------------
# Tool 4: reindex_all_projects
# Re-index all registered projects with change detection
# -----------------------------------------------------------------------------
@mcp.tool()
async def reindex_all_projects(
    ctx: Context,
    force: bool = False,
    dry_run: bool = False,
) -> Dict[str, Any]:
    """
    Re-index all registered projects with change detection.

    Iterates through all registered projects and re-indexes them if:
    - Files have been modified (mtime change)
    - Files have been added or removed
    - force flag is set

    Args:
        force: Re-index all projects regardless of changes
        dry_run: Only show what would be re-indexed without actually doing it

    Returns:
        Dictionary with re-indexing results

    Example:
        {
            "success": true,
            "total_projects": 5,
            "reindexed_count": 2,
            "skipped_count": 3,
            "results": [...]
        }
    """
    from .registry import ProjectRegistry
    from datetime import datetime

    try:
        registry = ProjectRegistry()
        projects = registry.list_all()

        if not projects:
            return {
                "success": True,
                "total_projects": 0,
                "reindexed_count": 0,
                "skipped_count": 0,
                "results": [],
                "message": "No projects in registry"
            }

        results = []
        reindexed_count = 0
        skipped_count = 0
        failed_count = 0

        for project in projects:
            result = {
                "project_path": project.path,
                "indexed": False,
                "skipped": False,
                "error": None,
                "reason": None
            }

            try:
                # Check if project path exists
                project_path = pathlib.Path(project.path)
                if not project_path.exists():
                    result["error"] = "Project path does not exist"
                    result["reason"] = "missing_path"
                    failed_count += 1
                    results.append(result)
                    continue

                # Check for changes if not forcing
                if not force:
                    # Check if any files have been modified since last index
                    last_indexed = project.indexed_at
                    has_changes = False

                    # Recursively check file mtimes
                    for file_path in project_path.rglob("*"):
                        if file_path.is_file():
                            file_mtime = datetime.fromtimestamp(file_path.stat().st_mtime)
                            if file_mtime > last_indexed:
                                has_changes = True
                                break

                    if not has_changes:
                        result["skipped"] = True
                        result["reason"] = "no_changes"
                        skipped_count += 1
                        results.append(result)
                        continue

                # Re-index the project
                if dry_run:
                    result["indexed"] = True
                    result["reason"] = "dry_run"
                    reindexed_count += 1
                else:
                    # Call the index_project operation
                    index_result = await manage_project(
                        ctx,
                        action="refresh",
                    )

                    if isinstance(index_result, dict) and index_result.get("success"):
                        result["indexed"] = True
                        result["reason"] = "reindexed"

                        # Update registry timestamp
                        registry.update(
                            project.path,
                            indexed_at=datetime.now()
                        )

                        reindexed_count += 1
                    else:
                        result["error"] = str(index_result)
                        failed_count += 1

            except Exception as e:
                result["error"] = str(e)
                result["reason"] = "error"
                failed_count += 1

            results.append(result)

        return {
            "success": True,
            "total_projects": len(projects),
            "reindexed_count": reindexed_count,
            "skipped_count": skipped_count,
            "failed_count": failed_count,
            "dry_run": dry_run,
            "results": results,
            "message": (
                f"Re-indexed {reindexed_count}, skipped {skipped_count}, "
                f"failed {failed_count} projects"
            )
        }

    except Exception as e:
        logger.error(f"Error during re-index all projects: {e}")
        return {
            "success": False,
            "error": str(e),
        }


# -----------------------------------------------------------------------------
# Tool 5: migrate_legacy_indexes
# Migrate pickle indexes to MessagePack format
# -----------------------------------------------------------------------------
@mcp.tool()
async def migrate_legacy_indexes(
    ctx: Context,
    project_path: Optional[str] = None,
    scan_global: bool = True,
) -> Dict[str, Any]:
    """
    Migrate legacy pickle indexes to MessagePack format.

    Exposes the IndexMigrator functionality as an MCP tool.
    Can migrate a specific project or all detected legacy indexes.

    Args:
        project_path: Optional project path to migrate. If None, migrates all.
        scan_global: Whether to scan the global data directory

    Returns:
        Dictionary with migration results

    Example:
        {
            "success": true,
            "migrated_count": 3,
            "results": [...]
        }
    """
    from .registry import IndexMigrator

    try:
        migrator = IndexMigrator(
            create_backups=True,
            verify_after_migration=True
        )

        # Detect legacy indexes
        pickle_files = migrator.detect_legacy_indexes(
            project_path=project_path,
            scan_global=scan_global
        )

        if not pickle_files:
            return {
                "success": True,
                "migrated_count": 0,
                "results": [],
                "message": "No legacy pickle indexes found"
            }

        # Migrate each file
        results = []
        migrated_count = 0
        failed_count = 0

        for pickle_file in pickle_files:
            result = migrator.migrate_index(pickle_file)
            results.append({
                "source": str(result.source_path),
                "target": str(result.target_path),
                "success": result.success,
                "backup": str(result.backup_path) if result.backup_path else None,
                "error": result.error_message,
                "duration_seconds": result.duration_seconds
            })

            if result.success:
                migrated_count += 1
            else:
                failed_count += 1

        return {
            "success": True,
            "migrated_count": migrated_count,
            "failed_count": failed_count,
            "results": results,
            "message": (
                f"Migrated {migrated_count} indexes successfully, "
                f"{failed_count} failed"
            )
        }

    except Exception as e:
        logger.error(f"Error during legacy index migration: {e}")
        return {
            "success": False,
            "error": str(e),
        }


# -----------------------------------------------------------------------------
# Tool 6: detect_orphaned_indexes
# Detect orphaned indexes that are not in the registry
# -----------------------------------------------------------------------------
@mcp.tool()
async def detect_orphaned_indexes(
    ctx: Context,
    max_depth: int = 3,
) -> Dict[str, Any]:
    """
    Detect orphaned indexes that exist on disk but are not registered.

    Offers recovery options (register or cleanup) for each orphan.

    Args:
        max_depth: Maximum directory depth to search (default: 3)

    Returns:
        Dictionary with orphan list and recommendations

    Example:
        {
            "success": true,
            "orphan_count": 2,
            "orphans": [...],
            "suggestions": {
                "register": ["/path/to/orphan1"],
                "cleanup": ["/path/to/orphan2"]
            }
        }
    """
    from .registry import OrphanDetector

    try:
        detector = OrphanDetector()

        # Scan for orphans
        orphans = detector.scan_for_orphans(max_depth=max_depth)

        if not orphans:
            return {
                "success": True,
                "orphan_count": 0,
                "orphans": [],
                "suggestions": {"register": [], "cleanup": []},
                "message": "No orphaned indexes found"
            }

        # Get suggestions
        suggestions = detector.suggest_actions(orphans)

        # Format orphan data
        orphan_data = []
        for orphan in orphans:
            orphan_data.append({
                "path": orphan.path,
                "index_location": orphan.index_location,
                "index_exists": orphan.index_exists,
                "index_size": orphan.index_size,
                "last_modified": (
                    orphan.last_modified.isoformat() if orphan.last_modified else None
                ),
                "reason": orphan.reason
            })

        return {
            "success": True,
            "orphan_count": len(orphans),
            "orphans": orphan_data,
            "suggestions": {
                "register": suggestions["register"],
                "cleanup": suggestions["cleanup"]
            },
            "message": f"Found {len(orphans)} orphaned indexes"
        }

    except Exception as e:
        logger.error(f"Error during orphan detection: {e}")
        return {
            "success": False,
            "error": str(e),
        }


# -----------------------------------------------------------------------------
# Tool 7: backup_registry
# Create an immediate backup of the registry
# -----------------------------------------------------------------------------
@mcp.tool()
async def backup_registry(
    ctx: Context,
) -> Dict[str, Any]:
    """
    Create an immediate backup of the project registry.

    Returns the backup file path and metadata.

    Returns:
        Dictionary with backup information

    Example:
        {
            "success": true,
            "backup_path": "/path/to/backup.db",
            "project_count": 5,
            "timestamp": "2025-01-01T12:00:00"
        }
    """
    from .registry import ProjectRegistry, RegistryBackupManager

    try:
        registry = ProjectRegistry()
        backup_manager = RegistryBackupManager()

        # Create backup
        backup_metadata = backup_manager.create_backup(registry=registry)

        return {
            "success": True,
            "backup_path": str(backup_metadata.backup_path),
            "project_count": backup_metadata.project_count,
            "timestamp": backup_metadata.timestamp.isoformat(),
            "backup_size": backup_metadata.backup_size_bytes,
            "checksum": backup_metadata.checksum,
            "message": f"Backup created at {backup_metadata.backup_path}"
        }

    except Exception as e:
        logger.error(f"Error creating registry backup: {e}")
        return {
            "success": False,
            "error": str(e),
        }


# =============================================================================
# END OF META-REGISTRY MCP TOOLS
# =============================================================================

# =============================================================================
# END OF MEGA-TOOLS
# Below are the original individual tool functions (preserved for backward compatibility)
# =============================================================================


async def set_project_path(path: str, ctx: Context) -> Union[str, Dict[str, Any]]:
    """
    Set the base project path for indexing.

    CRITICAL PERFORMANCE FIX: Early return when path is unchanged and index is recent.
    This prevents hanging for 8+ minutes when setting the same path that's already indexed.
    """
    # Validate and normalize path
    try:
        norm_path = os.path.normpath(path)
        abs_path = os.path.abspath(norm_path)

        if not os.path.exists(abs_path):
            return f"Error: Path does not exist: {abs_path}"

        if not os.path.isdir(abs_path):
            return f"Error: Path is not a directory: {abs_path}"

        # CRITICAL FIX: Early return if path is unchanged and index is recent
        current_base_path = ctx.request_context.lifespan_context.base_path
        if current_base_path:
            # Normalize for comparison
            current_normalized = os.path.normpath(os.path.abspath(current_base_path))
            if current_normalized == abs_path:
                # Path is unchanged - check if reindexing is needed
                logger.info(f"Path unchanged: {abs_path}. Checking if reindexing is needed...")

                # Get current config to check last_indexed
                config = ctx.request_context.lifespan_context.settings.load_config()
                last_indexed_str = config.get("last_indexed")

                if last_indexed_str:
                    try:
                        from datetime import datetime
                        last_indexed = datetime.fromisoformat(last_indexed_str)
                        now = datetime.now()
                        time_since_index = (now - last_indexed).total_seconds()

                        # Threshold: 48 hours
                        REINDEX_THRESHOLD_HOURS = 48
                        threshold_seconds = REINDEX_THRESHOLD_HOURS * 3600

                        if time_since_index < threshold_seconds:
                            hours = time_since_index / 3600
                            logger.info(f"Index is recent ({hours:.1f} hours old). Skipping reindex.")
                            return {
                                "status": "already_indexed",
                                "path": abs_path,
                                "message": f"Path already indexed {hours:.1f} hours ago. Index is up to date.",
                                "last_indexed": last_indexed_str,
                                "time_since_index_hours": round(hours, 1)
                            }
                        else:
                            # Re-indexing is needed due to staleness
                            hours = time_since_index / 3600
                            logger.info(f"Index is stale ({hours:.1f} hours old). Will reindex.")
                    except Exception as e:
                        logger.warning(f"Could not parse last_indexed timestamp: {e}")
                        # Continue with reindexing

        # Path changed or reindexing needed - continue with full setup

        # CRITICAL FIX: Properly dispose and recreate the LazyContentManager when projects change
        global \
            file_index, \
            lazy_content_manager, \
            memory_profiler, \
            memory_aware_manager, \
            performance_monitor, \
            es_client, \
            realtime_indexer, \
            dal_instance
        file_index = {}  # Always reset to dictionary - will be loaded as TrieFileIndex if available

        # CRITICAL FIX: Properly dispose of the old LazyContentManager before creating a new one
        # This prevents memory leaks from cached content of the previous project
        try:
            if lazy_content_manager is not None:
                # Unload all cached content
                lazy_content_manager.unload_all()
                # Clear any internal caches
                if hasattr(lazy_content_manager, "clear"):
                    lazy_content_manager.clear()
                logger.info("Old LazyContentManager properly disposed")
        except Exception as e:
            logger.error(f"Error disposing LazyContentManager: {e}")

        # CRITICAL FIX: Create a fresh LazyContentManager instance for the new project
        # This ensures no cached content from the old project remains in memory
        lazy_content_manager = LazyContentManager(max_loaded_files=100)
        logger.info("New LazyContentManager created for project switch")

        # CRITICAL FIX: Reset search session when project changes
        _reset_search_session()

        # Update the base path in context and global variable
        ctx.request_context.lifespan_context.base_path = abs_path
        global _current_project_path
        _current_project_path = abs_path

        # Save project path to config for persistence
        config = ctx.request_context.lifespan_context.settings.load_config()
        ctx.request_context.lifespan_context.settings.save_config(
            {**config, "base_path": abs_path}
        )

        # Create a new settings manager for the new path (don't skip loading files)
        new_settings = OptimizedProjectSettings(
            abs_path, skip_load=False, storage_backend="sqlite", use_trie_index=True
        )
        ctx.request_context.lifespan_context.settings = new_settings

        # Re-initialize IncrementalIndexer and FileChangeTracker with the new settings
        new_incremental_indexer = IncrementalIndexer(new_settings)

        # Initialize FileChangeTracker with DAL metadata backend after DAL is re-initialized
        ctx.request_context.lifespan_context.file_change_tracker = FileChangeTracker(
            dal_instance.metadata, new_incremental_indexer
        )

        # Re-initialize DAL instance based on settings
        try:
            # Close existing DAL instance if it exists
            if dal_instance:
                dal_instance.close()

            # Use environment variables for DAL configuration instead of hardcoded values
            # This allows the DAL factory to use the proper backend configuration
            dal_instance = get_dal_instance()
            ctx.request_context.lifespan_context.dal = dal_instance
            logger.info("DAL instance re-initialized using environment configuration")
        except Exception as e:
            logger.error(
                f"Error re-initializing DAL instance: {e}. Falling back to SQLite DAL."
            )
            dal_instance = get_dal_instance({"backend_type": "sqlite"})
            ctx.request_context.lifespan_context.dal = dal_instance

        # Re-initialize Elasticsearch client, RabbitMQ producer/consumer, and RealtimeIndexer if ES is available
        if es_client:
            try:
                # Stop existing RealtimeIndexer if running
                if realtime_indexer:
                    realtime_indexer.stop()

                # Re-initialize RabbitMQ producer and consumer with the new base_path
                rabbitmq_producer = RabbitMQProducer(
                    host=RABBITMQ_HOST,
                    port=RABBITMQ_PORT,
                    exchange=RABBITMQ_EXCHANGE_NAME,
                    routing_key=RABBITMQ_ROUTING_KEY,
                )
                rabbitmq_consumer = RabbitMQConsumer(
                    es_client=es_client,
                    base_path=abs_path,  # Update base_path for consumer
                    host=RABBITMQ_HOST,
                    port=RABBITMQ_PORT,
                    queue_name=RABBITMQ_QUEUE_NAME,
                    exchange=RABBITMQ_EXCHANGE_NAME,
                    routing_key=RABBITMQ_ROUTING_KEY,
                )
                realtime_indexer = RealtimeIndexer(
                    es_client, abs_path, rabbitmq_producer, rabbitmq_consumer
                )
                realtime_indexer.start()
                ctx.request_context.lifespan_context.es_client = es_client
                ctx.request_context.lifespan_context.rabbitmq_producer = (
                    rabbitmq_producer
                )
                ctx.request_context.lifespan_context.rabbitmq_consumer = (
                    rabbitmq_consumer
                )
                ctx.request_context.lifespan_context.realtime_indexer = realtime_indexer
                logger.info(
                    "RealtimeIndexer (RabbitMQ) re-initialized and started for new project path."
                )
            except Exception as e:
                logger.error(
                    f"Error re-initializing RealtimeIndexer (RabbitMQ) for new project path: {e}. Real-time indexing will be disabled."
                )
                ctx.request_context.lifespan_context.es_client = None
                ctx.request_context.lifespan_context.rabbitmq_producer = None
                ctx.request_context.lifespan_context.rabbitmq_consumer = None
                ctx.request_context.lifespan_context.realtime_indexer = None
                es_client = None  # Ensure global is also None
                rabbitmq_producer = None
                rabbitmq_consumer = None
                realtime_indexer = None
        else:
            logger.info(
                "Elasticsearch client not initialized, skipping RealtimeIndexer (RabbitMQ) setup."
            )
            ctx.request_context.lifespan_context.es_client = None
            ctx.request_context.lifespan_context.rabbitmq_producer = None
            ctx.request_context.lifespan_context.rabbitmq_consumer = None
            ctx.request_context.lifespan_context.realtime_indexer = None

        # Initialize memory profiler with comprehensive error handling and recovery
        try:
            logger.info("Initializing memory profiler with configuration...")

            # Load configuration with fallback
            config_manager = ConfigManager()
            try:
                config_data = config_manager.load_config()
                logger.debug("Configuration loaded successfully")
            except Exception as config_error:
                logger.warning(
                    f"Could not load configuration, using defaults: {config_error}"
                )
                config_data = {}

            # Create memory limits with validation
            try:
                memory_limits = create_memory_config_from_yaml(config_data)
                logger.debug(f"Memory limits created: {memory_limits}")
            except Exception as limits_error:
                logger.warning(
                    f"Could not create memory limits from config, using defaults: {limits_error}"
                )
                memory_limits = MemoryLimits()  # Use default limits

            # Stop existing profiler if running
            if memory_profiler:
                try:
                    memory_profiler.stop_monitoring()
                    logger.debug("Existing memory profiler stopped")
                except Exception as stop_error:
                    logger.warning(
                        f"Could not stop existing profiler cleanly: {stop_error}"
                    )

            # Create new memory profiler with error recovery
            try:
                memory_profiler = MemoryProfiler(memory_limits)
                logger.info("Memory profiler created successfully")

                # Validate profiler functionality
                test_snapshot = memory_profiler.take_snapshot()
                if test_snapshot and hasattr(test_snapshot, "process_memory_mb"):
                    logger.debug("Memory profiler validation successful")
                else:
                    raise ValueError("Profiler created but snapshot test failed")

            except Exception as profiler_error:
                logger.error(f"Failed to create memory profiler: {profiler_error}")
                # Try with default limits as fallback
                try:
                    memory_profiler = MemoryProfiler(MemoryLimits())
                    logger.warning(
                        "Memory profiler created with default limits as fallback"
                    )
                except Exception as fallback_error:
                    logger.error(
                        f"Failed to create memory profiler even with defaults: {fallback_error}"
                    )
                    memory_profiler = None
                    raise fallback_error

            # Create memory-aware manager if profiler is available
            if memory_profiler:
                try:
                    memory_aware_manager = MemoryAwareLazyContentManager(
                        memory_profiler, lazy_content_manager
                    )
                    logger.info("Memory-aware manager created successfully")
                except Exception as manager_error:
                    logger.warning(
                        f"Could not create memory-aware manager: {manager_error}"
                    )
                    memory_aware_manager = None

                # Start monitoring if enabled and profiler is healthy
                monitoring_enabled = config_data.get("memory", {}).get(
                    "enable_monitoring", True
                )
                if monitoring_enabled:
                    try:
                        interval = config_data.get("memory", {}).get(
                            "monitoring_interval", 30.0
                        )
                        memory_profiler.start_monitoring(interval)
                        logger.info(
                            f"Memory monitoring started with {interval}s interval"
                        )
                    except Exception as monitoring_error:
                        logger.warning(
                            f"Could not start memory monitoring: {monitoring_error}"
                        )
                        logger.info(
                            "Memory profiler will work without continuous monitoring"
                        )
            else:
                memory_aware_manager = None
                logger.warning(
                    "Memory profiler not available - memory-aware manager not created"
                )

            logger.info(f"Memory profiler initialization completed: {memory_limits}")

        except Exception as e:
            logger.error(f"Memory profiler initialization failed: {e}")
            # Ensure globals are in a safe state
            memory_profiler = None
            memory_aware_manager = None
            logger.warning(
                "Memory profiling will be unavailable - server will continue with reduced functionality"
            )

        # Initialize performance monitor with configuration from settings
        try:
            config_manager = ConfigManager()
            config_data = config_manager.load_config()

            # Create performance monitor from configuration
            performance_monitor = create_performance_monitor_from_config(config_data)

            logger.info("Performance monitor initialized")
        except Exception as e:
            logger.warning(f"Could not initialize performance monitor: {e}")
            # Fallback to default performance monitor
            performance_monitor = PerformanceMonitor()

        # Print the settings path for debugging
        settings_path = ctx.request_context.lifespan_context.settings.settings_path
        logger.info(f"Project settings path: {settings_path}")

        # Try to load existing index and cache
        logger.info("Attempting to load existing index and cache...")

        # Try to load index
        loaded_index = ctx.request_context.lifespan_context.settings.load_index()
        if loaded_index:
            logger.info("Existing index found and loaded successfully")
            # Convert TrieFileIndex to dictionary format for compatibility
            if hasattr(loaded_index, "get_all_files"):
                # This is a TrieFileIndex - convert to dict format
                file_index = {}
                for file_path, file_info in loaded_index.get_all_files():
                    # Navigate to correct directory in index
                    current_dir = file_index
                    rel_path = os.path.dirname(file_path)

                    if rel_path and rel_path != ".":
                        path_parts = rel_path.replace("\\", "/").split("/")
                        for part in path_parts:
                            if part not in current_dir:
                                current_dir[part] = {}
                            current_dir = current_dir[part]

                    # Add file to index
                    filename = os.path.basename(file_path)
                    current_dir[filename] = {
                        "type": "file",
                        "path": file_path,
                        "ext": file_info.get("extension", ""),
                    }
                logger.info("Converted TrieFileIndex to dictionary format")
            else:
                file_index = loaded_index

            file_count = _count_files(file_index)
            ctx.request_context.lifespan_context.file_count = file_count

            # Note: File content will be loaded lazily when accessed

            # Get search capabilities info
            search_tool = ctx.request_context.lifespan_context.settings.get_preferred_search_tool()

            if search_tool is None:
                search_info = " Basic search available."
            else:
                search_info = f" Advanced search enabled ({search_tool.name})."

            return f"Project path set to: {abs_path}. Loaded existing index with {file_count} files.{search_info}"
        else:
            logger.info("No existing index found, creating new index...")

        # If no existing index, create a new one
        file_count = await _index_project(
            abs_path, ctx.request_context.lifespan_context.core_engine
        )
        ctx.request_context.lifespan_context.file_count = file_count

        # Save the new index
        ctx.request_context.lifespan_context.settings.save_index(file_index)

        # Save project config
        config = {
            "base_path": abs_path,
            "supported_extensions": supported_extensions,
            "last_indexed": ctx.request_context.lifespan_context.settings.load_config().get(
                "last_indexed", None
            ),
        }
        ctx.request_context.lifespan_context.settings.save_config(config)

        # Get search capabilities info (this will trigger lazy detection)
        search_tool = (
            ctx.request_context.lifespan_context.settings.get_preferred_search_tool()
        )

        if search_tool is None:
            search_info = " Basic search available."
        else:
            search_info = f" Advanced search enabled ({search_tool.name})."

        return (
            f"Project path set to: {abs_path}. Indexed {file_count} files.{search_info}"
        )
    except Exception as e:
        logger.error(f"Error setting project path: {e}")
        return f"Error setting project path: {e}"


async def search_code_advanced(
    pattern: str,
    ctx: Context,
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    fuzziness_level: Optional[str] = None,  # New parameter for Elasticsearch fuzziness
    content_boost: float = 1.0,  # New parameter for content field boosting
    filepath_boost: float = 1.0,  # New parameter for file_path field boosting
    highlight_pre_tag: str = "<em>",  # New parameter for highlight pre-tag
    highlight_post_tag: str = "</em>",  # New parameter for highlight post-tag
    page: int = 1,
    page_size: int = 5,  # CRITICAL FIX: Reduced from 20 to 5 to prevent token flooding
) -> Dict[str, Any]:
    """
    Search for a code pattern in the project using an advanced, fast tool with improved backend selection and error handling.

    This tool automatically selects the best available search backend (Elasticsearch, SQLite FTS, or command-line tools)
    with robust fallback mechanisms and comprehensive error handling.

    Args:
        pattern: The search pattern (can be a regex if fuzzy=True).
        case_sensitive: Whether the search should be case-sensitive.
        context_lines: Number of lines to show before and after the match.
        file_pattern: A glob pattern to filter files to search in (e.g., "*.py").
        fuzzy: If True, treats the pattern as a regular expression.
                If False, performs a literal/fixed-string search.
        fuzziness_level: Elasticsearch fuzziness level (e.g., "AUTO", "0", "1", "2").
                          Only applicable when using Elasticsearch backend.
        content_boost: Boosting factor for content field. Only applicable when using Elasticsearch backend.
        filepath_boost: Boosting factor for file_path field. Only applicable when using Elasticsearch backend.
        highlight_pre_tag: HTML tag to prepend to highlighted terms. Only applicable when using Elasticsearch backend.
        highlight_post_tag: HTML tag to append to highlighted terms. Only applicable when using Elasticsearch backend.
        page: Page number for paginated results.
        page_size: Number of results per page.

    Returns:
        A dictionary containing the search results or an error message.
    """
    base_path = ctx.request_context.lifespan_context.base_path
    if not base_path:
        return {"error": "Project path not set. Please use set_project_path first."}

    settings = ctx.request_context.lifespan_context.settings
    dal = ctx.request_context.lifespan_context.dal
    core_engine = ctx.request_context.lifespan_context.core_engine

    # Ensure performance monitor is initialized
    ensure_performance_monitor()

    # Use global lazy_content_manager for now
    global lazy_content_manager

    # Create query key for caching
    query_key = "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}".format(
        pattern,
        case_sensitive,
        context_lines,
        file_pattern,
        fuzzy,
        fuzziness_level,
        content_boost,
        filepath_boost,
        highlight_pre_tag,
        highlight_post_tag,
        page,
    )

    # Check cache first
    cached_result = lazy_content_manager.get_cached_search_result(query_key)
    if cached_result:
        logger.info(f"Returning cached result for query: {query_key}")
        if performance_monitor:
            performance_monitor.increment_counter("search_cache_hits_total")
            performance_monitor.log_structured(
                "info", "Search cache hit", pattern=pattern, query_key=query_key
            )
        return cached_result

    # Log cache miss with diagnostics
    if performance_monitor:
        performance_monitor.increment_counter("search_cache_misses_total")
        performance_monitor.log_structured(
            "info",
            "Search cache miss",
            pattern=pattern,
            query_key_hash=query_key[:32] + "...",  # Log truncated key for debugging
            query_params=f"fuzzy={fuzzy}, cs={case_sensitive}, page={page}, page_size={page_size}"
        )
    logger.debug(f"Cache miss for query_key: {query_key[:64]}...")  # Debug log

    # Use Core Engine if available (Unified Search)
    if core_engine:
        try:
            # CRITICAL FIX: Determine search strategy based on query type
            # - fuzzy=True (regex): Use Zoekt for precise pattern matching
            # - case_sensitive=False: Use Zoekt for case-insensitive keyword search
            # - Default (fuzzy=False, case_sensitive=True): Use semantic/vector search
            # This ensures natural language queries use semantic search, while
            # pattern/regex queries use the faster Zoekt backend
            use_zoekt_for_search = fuzzy or (not case_sensitive)

            # CRITICAL FIX: Fetch more results to account for deduplication
            # We fetch 3x page_size so we have enough after filtering duplicates
            fetch_k = page_size * page * 3

            search_options = SearchOptions(
                rerank=True,  # Default to True as per mgrep default
                top_k=fetch_k,  # Fetch extra for deduplication
                use_zoekt=use_zoekt_for_search,  # Use Zoekt only for pattern/regex search
            )
            # Add file_pattern to query if needed, or handle in CoreEngine (TODO: Add filter support in CoreEngine)
            # For now, we rely on CoreEngine's internal handling or backend capabilities.
            # LocalVectorBackend handles metadata but search filtering depends on implementation.
            # Zoekt supports file pattern filtering natively.
            # CoreEngine.search currently doesn't accept filters in SearchOptions, but we can extend it or pass in query.
            # We'll assume for now CoreEngine handles it or we filter post-search (less efficient).
            # For this iteration, I'll proceed with basic query.

            store_ids = [base_path]  # Use base_path as store_id
            search_response = await core_engine.search(
                store_ids, pattern, search_options
            )

            # CRITICAL FIX: Collect file paths for deduplication
            all_file_paths = []
            for chunk in search_response.data:
                f_path = chunk.metadata.path if chunk.metadata else "unknown"
                if f_path != "unknown":
                    all_file_paths.append(f_path)

            # CRITICAL FIX: Filter out recently shown files (session deduplication)
            # Only apply on page 1 to avoid filtering out legitimate paginated results
            if page == 1:
                filtered_paths = _filter_recently_shown_files(all_file_paths, min_interval_seconds=300)
                filtered_set = set(filtered_paths)

                if len(filtered_set) < len(all_file_paths):
                    logger.info(f"Deduplication: Filtered {len(all_file_paths) - len(filtered_set)} recently shown files")

                # Rebuild results with only filtered files
            else:
                filtered_set = set(all_file_paths)

            # Map to Legacy Format
            results_dict = {}
            for chunk in search_response.data:
                f_path = chunk.metadata.path if chunk.metadata else "unknown"
                # Skip if filtered out by deduplication (page 1 only)
                if page == 1 and f_path not in filtered_set:
                    continue
                # Filter by file_pattern if provided (client-side filtering for now)
                if file_pattern and not fnmatch.fnmatch(f_path, file_pattern):
                    continue

                if f_path not in results_dict:
                    results_dict[f_path] = []

                results_dict[f_path].append(
                    {
                        "line": chunk.generated_metadata.get("line_number", 0)
                        if chunk.generated_metadata
                        else 0,
                        "text": chunk.text,
                        "score": chunk.score,
                    }
                )

            # CRITICAL FIX: Mark shown files for future deduplication (page 1 only)
            if page == 1:
                shown_files = list(results_dict.keys())
                _mark_files_as_shown(shown_files)

            # Paginate
            paginated_results = lazy_content_manager.paginate_results(
                results_dict, page, page_size
            )
            lazy_content_manager.cache_search_result(query_key, paginated_results)
            return paginated_results

        except Exception as e:
            logger.error(f"Core Engine search failed: {e}")
            # Fallback to legacy logic below
            pass

    # Normalize the search pattern
    normalized_pattern, is_regex = SearchPatternTranslator.normalize_pattern(
        pattern, fuzzy
    )
    logger.info(
        f"SEARCH_DEBUG: Pattern normalization - original: '{pattern}', normalized: '{normalized_pattern}', is_regex: {is_regex}, fuzzy: {fuzzy}"
    )
    logger.debug(f"Normalized pattern: '{normalized_pattern}', is_regex: {is_regex}")

    # Get search backend with validation
    search_backend = SearchBackendSelector.get_search_backend(dal)
    logger.info(
        f"SEARCH_DEBUG: Search backend selection - backend: {search_backend}, dal: {dal}"
    )

    if search_backend:
        # Get backend capabilities and health status
        backend_capabilities = SearchBackendSelector.get_backend_capabilities(
            search_backend
        )
        backend_health = degradation_manager.get_backend_status(search_backend)
        backend_type = backend_health.get(
            "backend_type", backend_capabilities.get("backend_type", "Unknown")
        )
        logger.info(
            f"SEARCH_DEBUG: Backend analysis - type: {backend_type}, capabilities: {backend_capabilities}, health: {backend_health}"
        )

        # Check if backend supports the requested features
        if fuzzy and not backend_capabilities.get("supports_regex", False):
            logger.warning(
                f"SEARCH_DEBUG: {backend_type} backend doesn't support regex patterns, will use literal search"
            )
            # Adjust pattern for backend limitations
            normalized_pattern, is_regex = SearchPatternTranslator.normalize_pattern(
                pattern, False
            )
            logger.info(
                f"SEARCH_DEBUG: Pattern adjusted for backend limitations - new pattern: '{normalized_pattern}', is_regex: {is_regex}"
            )

        if not backend_health.get("healthy", False):
            logger.warning(
                f"SEARCH_DEBUG: {backend_type} backend is unhealthy: {backend_health.get('reason', 'Unknown reason')}"
            )
            degradation_message = degradation_manager.get_degradation_message(
                backend_type, backend_health.get("reason", "Unknown reason")
            )
            logger.info(f"SEARCH_DEBUG: Graceful degradation: {degradation_message}")
        else:
            logger.info(
                f"SEARCH_DEBUG: Using healthy {backend_type} backend for search with pattern: '{normalized_pattern}'"
            )

        # Implement enhanced retry logic with backend-specific handling
        max_retries = 3
        retry_delay = 0.5  # seconds
        search_successful = False

        for attempt in range(max_retries + 1):
            operation_id = search_monitor.log_search_start(
                normalized_pattern,
                backend_type,
                attempt=attempt + 1,
                max_retries=max_retries,
            )

            try:
                if performance_monitor:
                    with performance_monitor.time_operation(
                        "search",
                        pattern=normalized_pattern,
                        strategy=f"{backend_type}_Content",
                        file_pattern=file_pattern,
                        case_sensitive=case_sensitive,
                        fuzzy=fuzzy,
                        fuzziness_level=fuzziness_level,
                        content_boost=content_boost,
                        filepath_boost=filepath_boost,
                    ) as operation:
                        # Perform the search based on backend type with enhanced parameter handling
                        if SearchBackendSelector.is_elasticsearch_backend(
                            search_backend
                        ):
                            # Elasticsearch-specific parameters
                            search_params = {
                                "query": normalized_pattern,
                                "is_sqlite_pattern": is_regex,  # Elasticsearch expects is_sqlite_pattern
                            }

                            # Add optional Elasticsearch-specific parameters
                            if fuzziness_level and backend_capabilities.get(
                                "supports_fuzzy", False
                            ):
                                search_params["fuzziness"] = fuzziness_level
                            if backend_capabilities.get("supports_highlighting", False):
                                search_params.update(
                                    {
                                        "content_boost": content_boost,
                                        "file_path_boost": filepath_boost,
                                        "highlight_pre_tags": [highlight_pre_tag],
                                        "highlight_post_tags": [highlight_post_tag],
                                    }
                                )

                            results_list = search_backend.search_content(
                                **search_params
                            )

                        elif SearchBackendSelector.is_sqlite_backend(search_backend):
                            # SQLite-specific parameters - SQLite expects is_regex
                            results_list = search_backend.search_content(
                                query=normalized_pattern, is_regex=is_regex
                            )

                        else:
                            # Generic backend handling - try is_regex first, fall back to is_sqlite_pattern
                            try:
                                results_list = search_backend.search_content(
                                    query=normalized_pattern, is_regex=is_regex
                                )
                            except TypeError as e:
                                if "is_regex" in str(e):
                                    # Fallback to is_sqlite_pattern for backends that expect it
                                    logger.debug(
                                        f"Backend doesn't support is_regex, trying is_sqlite_pattern: {e}"
                                    )
                                    results_list = search_backend.search_content(
                                        query=normalized_pattern,
                                        is_sqlite_pattern=is_regex,
                                    )
                                else:
                                    raise

                        # Process and standardize results with enhanced error handling
                        logger.info(
                            f"SEARCH_DEBUG: Raw results from {backend_type}: {len(results_list) if results_list else 0} items"
                        )
                        standardized_results = (
                            SearchResultProcessor.standardize_results(
                                results_list, backend_type
                            )
                        )
                        logger.info(
                            f"SEARCH_DEBUG: Standardized results from {backend_type}: {len(standardized_results)} items"
                        )

                        if not standardized_results:
                            logger.info(
                                f"SEARCH_DEBUG: No results found with {backend_type} backend"
                            )
                            # Don't treat empty results as an error, just log and continue

                        # Convert to the expected format for pagination
                        results_dict = {}
                        for result in standardized_results:
                            file_path = result.get("file_path", "")
                            if not file_path:
                                continue

                            if file_path not in results_dict:
                                results_dict[file_path] = []

                            results_dict[file_path].append(
                                {
                                    "line": result.get("line", 0),
                                    "text": result.get("content", ""),
                                    "start": result.get("start", 0),
                                    "end": result.get("end", 0),
                                    "score": result.get("score", 0.0),
                                }
                            )

                        total_matches = len(standardized_results)
                        operation.metadata.update(
                            {
                                "files_searched": len(results_dict),
                                "total_matches": total_matches,
                                "backend_type": backend_type,
                                "attempt": attempt + 1,
                                "backend_capabilities": backend_capabilities,
                            }
                        )

                        # Always cache results, even if empty
                        paginated_results = lazy_content_manager.paginate_results(
                            results_dict, page, page_size
                        )
                        lazy_content_manager.cache_search_result(
                            query_key, paginated_results
                        )

                        # Log successful search
                        search_monitor.log_search_success(
                            operation_id,
                            total_matches,
                            backend_type=backend_type,
                            files_searched=len(results_dict),
                            attempt=attempt + 1,
                        )

                        logger.info(
                            f"Search successful with {backend_type} (attempt {attempt + 1}). Found {total_matches} matches. Cached result for query: {query_key}"
                        )

                        if performance_monitor:
                            performance_monitor.log_structured(
                                "info",
                                "Search completed successfully",
                                pattern=normalized_pattern,
                                strategy=f"{backend_type}_Content",
                                files_searched=len(results_dict),
                                total_matches=total_matches,
                                duration_ms=operation.duration_ms,
                                backend_type=backend_type,
                                attempt=attempt + 1,
                            )
                        search_successful = True
                        return paginated_results

            except Exception as e:
                # CRITICAL FIX: Check for missing aiohttp dependency - this is not recoverable
                # and requires user action to reinstall dependencies
                if isinstance(e, RuntimeError) and "aiohttp" in str(e) and "not installed" in str(e):
                    error_msg = (
                        f"CRITICAL: Elasticsearch backend is unavailable due to missing dependency.\n\n"
                        f"Error: {str(e)}\n\n"
                        f"The search cannot proceed without reinstalling dependencies. "
                        f"Please run: pip install -e ."
                    )
                    logger.error(error_msg)
                    search_monitor.log_search_failure(
                        operation_id, e, backend_type=backend_type, attempt=attempt + 1
                    )
                    # Return error immediately - do not fall back to command-line tools
                    # because the user explicitly needs to fix the dependency issue
                    return {"error": error_msg, "error_type": "MISSING_DEPENDENCY", "dependency": "aiohttp"}

                # Log the failure with more context
                search_monitor.log_search_failure(
                    operation_id, e, backend_type=backend_type, attempt=attempt + 1
                )

                error_details = SearchErrorHandler.handle_search_error(
                    e, backend_type, f"content search (attempt {attempt + 1})"
                )

                # Check if this is a recoverable error
                if SearchResultProcessor._is_recoverable_error(e, backend_type):
                    # If this isn't the last attempt, wait and retry
                    if attempt < max_retries:
                        logger.warning(
                            f"{backend_type} search failed (attempt {attempt + 1}/{max_retries + 1}), retrying in {retry_delay}s: {e}"
                        )
                        import asyncio

                        await asyncio.sleep(retry_delay)
                        retry_delay *= 2  # Exponential backoff
                        continue
                    else:
                        logger.warning(
                            f"{backend_type} search failed after {max_retries + 1} attempts, attempting fallback to command-line tools"
                        )
                else:
                    # Non-recoverable error, skip retries
                    logger.error(
                        f"Non-recoverable error with {backend_type}, skipping retries: {e}"
                    )
                    break

                if performance_monitor:
                    performance_monitor.increment_counter(
                        "search_backend_failures_total"
                    )
                    performance_monitor.log_structured(
                        "warning",
                        f"{backend_type} search failed, trying fallback",
                        pattern=normalized_pattern,
                        error=str(e),
                        backend_type=backend_type,
                        attempt=attempt + 1,
                    )
                break  # Exit retry loop and fall back to command-line tools

        # If we get here, all database backend attempts failed
        if not search_successful:
            logger.warning(
                f"All {backend_type} backend attempts failed, falling back to command-line tools"
            )
    else:
        logger.info(
            "No database search backend available, falling back to command-line tools"
        )

    # Fallback to command-line search tools
    logger.info(
        f"SEARCH_DEBUG: Using command-line search tools for pattern: '{normalized_pattern}'"
    )

    # Get all available strategies in priority order for fallback
    all_strategies = settings.available_strategies
    logger.info(
        f"SEARCH_DEBUG: Total available strategies: {len(all_strategies) if all_strategies else 0}"
    )
    if not all_strategies:
        logger.error(
            "SEARCH_DEBUG: No search strategies available - this indicates a configuration issue"
        )
        return {"error": "No search strategies available. This is unexpected."}

    # Filter out database strategies since we already tried them
    # Only use command-line based strategies (zoekt, ugrep, ripgrep, ag, grep, basic)
    command_line_strategies = [
        strategy
        for strategy in all_strategies
        if strategy.name.lower() in ["zoekt", "ugrep", "ripgrep", "ag", "grep", "basic"]
    ]
    logger.info(
        f"SEARCH_DEBUG: Command-line strategies: {[s.name for s in command_line_strategies]}"
    )

    if not command_line_strategies:
        logger.warning(
            "SEARCH_DEBUG: No command-line search strategies available, falling back to basic search"
        )
        command_line_strategies = [
            strategy for strategy in all_strategies if strategy.name.lower() == "basic"
        ]

    if not command_line_strategies:
        logger.error("SEARCH_DEBUG: No suitable search strategies found")
        return {"error": "No suitable search strategies available."}

    # Prioritize zoekt for Python files since it works well with them
    if file_pattern and ("*.py" in file_pattern or file_pattern.endswith(".py")):
        # Try to find zoekt first for Python files
        zoekt_strategy = next(
            (s for s in command_line_strategies if s.name.lower() == "zoekt"), None
        )
        if zoekt_strategy:
            strategy = zoekt_strategy
            logger.info(
                f"SEARCH_DEBUG: Prioritizing zoekt for Python file search: {file_pattern}"
            )
        else:
            strategy = command_line_strategies[0]
    else:
        strategy = command_line_strategies[
            0
        ]  # Start with the highest priority command-line strategy

    logger.info(
        f"SEARCH_DEBUG: Using search strategy: {strategy.name} (first of {len(command_line_strategies)} available)"
    )
    logger.debug(
        f"SEARCH_DEBUG: Available command-line strategies: {[s.name for s in command_line_strategies]}"
    )

    # Try each command-line strategy in order until one succeeds
    last_error = None

    for strategy_index, strategy in enumerate(command_line_strategies):
        logger.info(
            f"Trying search strategy {strategy_index + 1}/{len(all_strategies)}: {strategy.name}"
        )

        # Use performance monitoring context manager for timing
        if performance_monitor:
            with performance_monitor.time_operation(
                "search",
                pattern=normalized_pattern,
                strategy=strategy.name,
                file_pattern=file_pattern,
                case_sensitive=case_sensitive,
                fuzzy=fuzzy,
                attempt=strategy_index + 1,
            ) as operation:
                try:
                    # Use async search with progress callback
                    def progress_callback(progress: float):
                        logger.debug(
                            f"Search progress ({strategy.name}): {progress:.1%}"
                        )

                    results = await strategy.search_async(
                        pattern=normalized_pattern,
                        base_path=base_path,
                        case_sensitive=case_sensitive,
                        context_lines=context_lines,
                        file_pattern=file_pattern,
                        fuzzy=is_regex,  # Use normalized regex flag
                        progress_callback=progress_callback,
                    )

                    # Count results for metrics
                    total_matches = sum(len(matches) for matches in results.values())
                    logger.info(
                        f"SEARCH_DEBUG: Command-line search results - strategy: {strategy.name}, files_searched: {len(results)}, total_matches: {total_matches}"
                    )
                    operation.metadata.update(
                        {
                            "files_searched": len(results),
                            "total_matches": total_matches,
                            "strategy": strategy.name,
                        }
                    )

                    paginated_results = lazy_content_manager.paginate_results(
                        results, page, page_size
                    )
                    lazy_content_manager.cache_search_result(
                        query_key, paginated_results
                    )
                    logger.info(
                        f"SEARCH_DEBUG: Search successful with {strategy.name}. Cached result for query: {query_key}"
                    )

                    # Log successful search
                    if performance_monitor:
                        performance_monitor.log_structured(
                            "info",
                            "Search completed successfully",
                            pattern=normalized_pattern,
                            strategy=strategy.name,
                            files_searched=len(results),
                            total_matches=total_matches,
                            duration_ms=operation.duration_ms,
                            attempt=strategy_index + 1,
                        )
                    return paginated_results

                except Exception as e:
                    last_error = e
                    # Log search error but continue to next strategy
                    if performance_monitor:
                        performance_monitor.log_structured(
                            "warning",
                            "Search strategy failed, trying next",
                            pattern=normalized_pattern,
                            strategy=strategy.name,
                            error=str(e),
                            attempt=strategy_index + 1,
                        )
                        performance_monitor.increment_counter(
                            "search_strategy_failures_total"
                        )

                    # If this isn't the last strategy, continue to the next one
                    if strategy_index < len(all_strategies) - 1:
                        logger.warning(
                            f"Search failed with {strategy.name}: {e}. Trying next strategy..."
                        )
                        continue
                    else:
                        # This was the last strategy, return comprehensive error
                        if performance_monitor:
                            performance_monitor.log_structured(
                                "error",
                                "All search strategies failed",
                                pattern=normalized_pattern,
                                error=str(e),
                                total_attempts=len(all_strategies),
                            )
                            performance_monitor.increment_counter("search_errors_total")

                        return {
                            "error": f"All search strategies failed. Last error from '{strategy.name}': {e}",
                            "attempted_strategies": [s.name for s in all_strategies],
                            "total_attempts": len(all_strategies),
                            "backend_type": "command_line_fallback",
                        }
        else:
            # Fallback without monitoring - same logic but without performance tracking
            try:
                # Use async search with progress callback
                def progress_callback(progress: float):
                    logger.debug(f"Search progress ({strategy.name}): {progress:.1%}")

                results = await strategy.search_async(
                    pattern=normalized_pattern,
                    base_path=base_path,
                    case_sensitive=case_sensitive,
                    context_lines=context_lines,
                    file_pattern=file_pattern,
                    fuzzy=is_regex,  # Use normalized regex flag
                    progress_callback=progress_callback,
                )

                paginated_results = lazy_content_manager.paginate_results(
                    results, page, page_size
                )
                lazy_content_manager.cache_search_result(query_key, paginated_results)
                logger.info(
                    f"Search successful with {strategy.name}. Cached result for query: {query_key}"
                )
                return paginated_results

            except Exception as e:
                last_error = e
                # If this isn't the last strategy, continue to the next one
                if strategy_index < len(all_strategies) - 1:
                    logger.warning(
                        f"Search failed with {strategy.name}: {e}. Trying next strategy..."
                    )
                    continue
                else:
                    # This was the last strategy, return error
                    return {
                        "error": f"All search strategies failed. Last error from '{strategy.name}': {e}",
                        "attempted_strategies": [s.name for s in all_strategies],
                        "total_attempts": len(all_strategies),
                        "backend_type": "command_line_fallback",
                    }

    # This should never be reached, but just in case
    return {
        "error": f"Unexpected error: no strategies were attempted. Last error: {last_error}",
        "backend_type": "unknown",
    }


def find_files(pattern: str, ctx: Context) -> List[str]:
    """Find files in the project matching a specific glob pattern."""
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return [
            "Error: Project path not set. Please use set_project_path to set a project directory first."
        ]

    # Check if we need to index the project
    if not file_index:
        _index_project(base_path)
        ctx.request_context.lifespan_context.file_count = _count_files(file_index)
        ctx.request_context.lifespan_context.settings.save_index(file_index)

    matching_files = []
    for file_path, _info in _get_all_files(file_index):
        if fnmatch.fnmatch(file_path, pattern):
            matching_files.append(file_path)

    return matching_files


def get_file_summary(file_path: str, ctx: Context) -> Dict[str, Any]:
    """
    Get a comprehensive summary of a specific file using SmartFileReader, including:
    - Line count and basic file information
    - Function/class definitions (for supported languages)
    - Import statements
    - Error detection and file health analysis
    - File metadata and comprehensive file information
    - Reading strategy information
    - Basic complexity metrics
    """
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Normalize the file path and ensure it's relative to base_path
    norm_path = os.path.normpath(file_path)
    if norm_path.startswith(".."):
        return {"error": f"Invalid file path: {file_path}"}

    # Ensure the path is relative to base_path
    if os.path.isabs(norm_path):
        # If absolute path is provided, make it relative to base_path
        try:
            norm_path = os.path.relpath(norm_path, base_path)
        except ValueError:
            return {"error": f"File path is not within project directory: {file_path}"}

    full_path = os.path.join(base_path, norm_path)

    try:
        # Initialize SmartFileReader for enhanced file analysis
        smart_reader = SmartFileReader(base_path)

        # Get comprehensive file information
        file_info = smart_reader.get_file_info(full_path)

        # Get file metadata
        metadata = smart_reader.read_metadata(full_path)

        # Detect errors in the file
        errors_result = smart_reader.detect_errors(full_path)

        # Get file content using SmartFileReader (enhanced lazy loading)
        content = smart_reader.read_content(full_path)

        if content is None:
            return {"error": "Unable to read file content"}

        # Basic file info
        lines = content.splitlines()
        line_count = len(lines)

        # File extension for language-specific analysis
        _, ext = os.path.splitext(norm_path)

        # Start building enhanced summary
        summary = {
            "file_path": norm_path,
            "line_count": line_count,
            "size_bytes": os.path.getsize(full_path),
            "extension": ext,
            # Add comprehensive file information
            "file_info": {
                # Handle strategy_used field with proper type checking
                "strategy_used": (
                    str(file_info)
                    if isinstance(file_info, (ReadingStrategy, FileSizeCategory))
                    else (
                        str(file_info.get("strategy_used", "unknown"))
                        if isinstance(file_info, dict)
                        else (
                            getattr(file_info, "strategy_used", "unknown").value
                            if hasattr(
                                getattr(file_info, "strategy_used", "unknown"), "value"
                            )
                            else str(getattr(file_info, "strategy_used", "unknown"))
                        )
                    )
                ),
                # Handle file_size_category field with proper type checking
                "file_size_category": (
                    str(file_info)
                    if isinstance(file_info, (ReadingStrategy, FileSizeCategory))
                    else (
                        str(file_info.get("file_size_category", "unknown"))
                        if isinstance(file_info, dict)
                        else (
                            getattr(file_info, "file_size_category", "unknown").value
                            if hasattr(
                                getattr(file_info, "file_size_category", "unknown"),
                                "value",
                            )
                            else str(
                                getattr(file_info, "file_size_category", "unknown")
                            )
                        )
                    )
                ),
                # Handle other fields with proper type checking
                "is_binary": (
                    file_info.get("is_binary", False)
                    if isinstance(file_info, dict)
                    else getattr(file_info, "is_binary", False)
                ),
                "encoding": (
                    file_info.get("encoding", "unknown")
                    if isinstance(file_info, dict)
                    else getattr(file_info, "encoding", "unknown")
                ),
                "estimated_read_time_ms": (
                    file_info.get("estimated_read_time_ms", 0)
                    if isinstance(file_info, dict)
                    else getattr(file_info, "estimated_read_time_ms", 0)
                ),
                "memory_efficiency_score": (
                    file_info.get("memory_efficiency_score", 0.0)
                    if isinstance(file_info, dict)
                    else getattr(file_info, "memory_efficiency_score", 0.0)
                ),
            },
            # Add file metadata - handle dict returns properly
            "metadata": {
                "last_modified": metadata.get("last_modified", None)
                if isinstance(metadata, dict)
                else (
                    metadata.last_modified.isoformat()
                    if metadata
                    and hasattr(metadata, "last_modified")
                    and metadata.last_modified
                    else None
                ),
                "created": metadata.get("created", None)
                if isinstance(metadata, dict)
                else (
                    metadata.created.isoformat()
                    if metadata and hasattr(metadata, "created") and metadata.created
                    else None
                ),
                "accessed": metadata.get("accessed", None)
                if isinstance(metadata, dict)
                else (
                    metadata.accessed.isoformat()
                    if metadata and hasattr(metadata, "accessed") and metadata.accessed
                    else None
                ),
                "is_symlink": metadata.get("is_symlink", False)
                if isinstance(metadata, dict)
                else getattr(metadata, "is_symlink", False),
                "is_hidden": metadata.get("is_hidden", False)
                if isinstance(metadata, dict)
                else getattr(metadata, "is_hidden", False),
                "owner": metadata.get("owner", None)
                if isinstance(metadata, dict)
                else getattr(metadata, "owner", None),
                "group": metadata.get("group", None)
                if isinstance(metadata, dict)
                else getattr(metadata, "group", None),
                "permissions": metadata.get("permissions", None)
                if isinstance(metadata, dict)
                else getattr(metadata, "permissions", None),
                "inode": metadata.get("inode", None)
                if isinstance(metadata, dict)
                else getattr(metadata, "inode", None),
            }
            if metadata
            else None,
            # Add error detection results - handle dict returns properly
            "errors": {
                "has_errors": errors_result.get("has_errors", False)
                if isinstance(errors_result, dict)
                else getattr(errors_result, "has_errors", False),
                "error_count": len(errors_result.get("errors", []))
                if isinstance(errors_result, dict)
                else (
                    len(errors_result.errors)
                    if hasattr(errors_result, "errors") and errors_result.errors
                    else 0
                ),
                "error_types": list(
                    set(
                        error.get("error_type", "unknown")
                        for error in errors_result.get("errors", [])
                    )
                )
                if isinstance(errors_result, dict)
                else (
                    list(set(error.error_type for error in errors_result.errors))
                    if hasattr(errors_result, "errors") and errors_result.errors
                    else []
                ),
                "errors": [
                    {
                        "error_type": error.get("error_type", "unknown")
                        if isinstance(error, dict)
                        else getattr(error, "error_type", "unknown"),
                        "severity": error.get("severity", "error")
                        if isinstance(error, dict)
                        else (
                            error.severity.value
                            if hasattr(error, "severity")
                            and hasattr(error.severity, "value")
                            else str(getattr(error, "severity", "error"))
                        ),
                        "message": error.get("message", "")
                        if isinstance(error, dict)
                        else getattr(error, "message", ""),
                        "line_number": error.get("line_number", None)
                        if isinstance(error, dict)
                        else getattr(error, "line_number", None),
                        "column_number": error.get("column_number", None)
                        if isinstance(error, dict)
                        else getattr(error, "column_number", None),
                    }
                    for error in (
                        errors_result.get("errors", [])
                        if isinstance(errors_result, dict)
                        else (
                            errors_result.errors
                            if hasattr(errors_result, "errors")
                            else []
                        )
                    )
                ],
            }
            if errors_result
            else None,
        }

        # Language-specific analysis (enhanced with SmartFileReader insights)
        if ext == ".py":
            # Python analysis
            imports = []
            classes = []
            functions = []

            for i, line in enumerate(lines):
                line = line.strip()

                # Check for imports
                if line.startswith("import ") or line.startswith("from "):
                    imports.append(line)

                # Check for class definitions
                if line.startswith("class "):
                    classes.append(
                        {
                            "line": i + 1,
                            "name": line.replace("class ", "")
                            .split("(")[0]
                            .split(":")[0]
                            .strip(),
                        }
                    )

                # Check for function definitions
                if line.startswith("def "):
                    functions.append(
                        {
                            "line": i + 1,
                            "name": line.replace("def ", "").split("(")[0].strip(),
                        }
                    )

            summary.update(
                {
                    "imports": imports,
                    "classes": classes,
                    "functions": functions,
                    "import_count": len(imports),
                    "class_count": len(classes),
                    "function_count": len(functions),
                    # Add Python-specific complexity metrics
                    "complexity_metrics": {
                        "cyclomatic_complexity_estimate": len(functions)
                        + len(classes),  # Simple estimate
                        "nesting_level_max": _estimate_max_nesting_level(lines),
                        "has_docstrings": _has_docstrings(lines),
                    },
                }
            )

        elif ext in [".js", ".jsx", ".ts", ".tsx"]:
            # JavaScript/TypeScript analysis
            imports = []
            classes = []
            functions = []

            for i, line in enumerate(lines):
                line = line.strip()

                # Check for imports
                if line.startswith("import ") or line.startswith("require("):
                    imports.append(line)

                # Check for class definitions
                if line.startswith("class ") or "class " in line:
                    class_name = ""
                    if "class " in line:
                        parts = line.split("class ")[1]
                        class_name = (
                            parts.split(" ")[0]
                            .split("{")[0]
                            .split("extends")[0]
                            .strip()
                        )
                    classes.append({"line": i + 1, "name": class_name})

                # Check for function definitions
                if "function " in line or "=>" in line:
                    functions.append({"line": i + 1, "content": line})

            summary.update(
                {
                    "imports": imports,
                    "classes": classes,
                    "functions": functions,
                    "import_count": len(imports),
                    "class_count": len(classes),
                    "function_count": len(functions),
                    # Add JavaScript/TypeScript-specific complexity metrics
                    "complexity_metrics": {
                        "arrow_functions": len(
                            [f for f in functions if "=>" in f.get("content", "")]
                        ),
                        "async_functions": len(
                            [f for f in functions if "async " in f.get("content", "")]
                        ),
                        "has_es6_imports": len(imports) > 0
                        and any("import" in imp for imp in imports),
                    },
                }
            )

        # Add general file analysis
        summary["general_analysis"] = {
            "non_empty_lines": len([line for line in lines if line.strip()]),
            "comment_lines": len(
                [
                    line
                    for line in lines
                    if line.strip().startswith("#")
                    or line.strip().startswith("//")
                    or line.strip().startswith("/*")
                ]
            ),
            "blank_lines": len([line for line in lines if not line.strip()]),
            "average_line_length": sum(len(line) for line in lines)
            / max(len(lines), 1),
        }

        return summary
    except Exception as e:
        logger.error(f"Error analyzing file {full_path}: {e}", exc_info=True)
        return {"error": f"Error analyzing file: {e}"}


def _estimate_max_nesting_level(lines: List[str]) -> int:
    """Estimate maximum nesting level in code."""
    max_level = 0
    current_level = 0

    for line in lines:
        stripped = line.strip()
        if not stripped or stripped.startswith(("#", "//", "/*")):
            continue

        # Count opening brackets
        current_level += stripped.count("{") + stripped.count("(") + stripped.count("[")
        # Count closing brackets
        current_level -= stripped.count("}") + stripped.count(")") + stripped.count("]")

        max_level = max(max_level, current_level)

    return max_level


def _has_docstrings(lines: List[str]) -> bool:
    """Check if file contains docstrings."""
    in_docstring = False
    for line in lines:
        stripped = line.strip()
        if stripped.startswith('"""') or stripped.startswith("'''"):
            if not in_docstring:
                in_docstring = True
            else:
                return True
    return False


async def refresh_index(ctx: Context) -> Dict[str, Any]:
    """Refresh the project index using incremental indexing with progress tracking.

    Phase 2 RabbitMQ Integration:
    - Queues files to RabbitMQ for asynchronous Elasticsearch indexing
    - Returns 'indexing_started' status with operation_id
    - Fails gracefully if RabbitMQ is unavailable
    """
    import asyncio  # Ensure asyncio is available in this function scope

    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first.",
            "success": False,
        }

    # PHASE 2: Check RabbitMQ availability
    realtime_indexer = ctx.request_context.lifespan_context.realtime_indexer

    if not realtime_indexer or not realtime_indexer.producer:
        return {
            "error": "RabbitMQ is required for async indexing. Start it with: python run.py start-dev-dbs",
            "success": False,
            "rabbitmq_required": True,
        }

    # Check if RabbitMQ producer has active connection
    if (
        not realtime_indexer.producer.channel
        or not realtime_indexer.producer.connection
        or realtime_indexer.producer.connection.is_closed
    ):
        return {
            "error": "RabbitMQ is required for async indexing. Start it with: python run.py start-dev-dbs",
            "success": False,
            "rabbitmq_required": True,
        }

    try:
        # Create progress tracker for indexing
        tracker = progress_manager.create_tracker(
            operation_name="Index Refresh",
            total_items=1000,  # Initial estimate, will be updated
            stages=["Scanning", "Indexing", "Saving"],
        )

        # Add console logging handler
        console_handler = LoggingProgressHandler()
        tracker.add_event_handler(console_handler)

        async with ProgressContext(
            operation_name="Index Refresh",
            total_items=1000,  # Will be updated with actual file count
            stages=["Scanning", "Queuing", "Saving"],
        ) as progress_tracker:
            # Add cleanup task to save partial state on cancellation
            def cleanup_partial_state():
                try:
                    if file_index:
                        ctx.request_context.lifespan_context.settings.save_index(
                            file_index
                        )
                        logger.info("Saved partial index state during cancellation")
                except Exception as e:
                    logger.error(f"Error saving partial state: {e}")

            progress_tracker.add_cleanup_task(cleanup_partial_state)

            # Stage 1: Scanning
            await progress_tracker.update_progress(
                stage_index=0, message="Starting directory scan..."
            )

            # PHASE 2: Collect all files for RabbitMQ publishing
            files_to_queue = []
            for root, dirs, files in os.walk(base_path):
                for file in files:
                    file_path = os.path.join(root, file)
                    files_to_queue.append(file_path)
                # Check for cancellation periodically
                if len(files_to_queue) % 100 == 0:
                    progress_tracker.cancellation_token.check_cancelled()

            total_files = len(files_to_queue)

            # Update total items with actual count
            progress_tracker.total_items = max(total_files, 1)

            await progress_tracker.update_progress(
                message=f"Found {total_files} files to process"
            )

            # Stage 2: Queue files to RabbitMQ for Elasticsearch indexing
            await progress_tracker.update_progress(
                stage_index=1, message="Queuing files to RabbitMQ for indexing..."
            )

            # PHASE 2: Publish each file to RabbitMQ
            files_queued = 0
            producer = realtime_indexer.producer

            for file_path in files_to_queue:
                # Check for cancellation
                if files_queued % 100 == 0:
                    progress_tracker.cancellation_token.check_cancelled()

                # Create indexing operation message
                operation = {
                    "type": "index",
                    "file_path": file_path,
                    "timestamp": datetime.now().isoformat(),
                }

                # Publish to RabbitMQ
                producer.publish(operation)
                files_queued += 1

            logger.info(f"Queued {files_queued} files to RabbitMQ for indexing")

            # Stage 3: Saving
            await progress_tracker.update_progress(
                stage_index=2, message="Updating metadata..."
            )

            # Update the last indexed timestamp in config
            config = ctx.request_context.lifespan_context.settings.load_config()
            ctx.request_context.lifespan_context.settings.save_config(
                {
                    **config,
                    "last_indexed": ctx.request_context.lifespan_context.settings._get_timestamp(),
                }
            )

            # Update file count
            ctx.request_context.lifespan_context.file_count = total_files

            await progress_tracker.update_progress(
                message=f"Indexing started: {files_queued} files queued to RabbitMQ"
            )

        # PHASE 2: Return indexing_started status instead of completion
        return {
            "status": "indexing_started",
            "success": True,
            "message": f"Queued {files_queued} files to RabbitMQ for Elasticsearch indexing.",
            "operation_id": progress_tracker.operation_id,
            "files_queued": files_queued,
            "elapsed_time": progress_tracker.elapsed_time,
        }
    except asyncio.CancelledError:
        return {
            "error": "Indexing operation was cancelled",
            "success": False,
            "cancelled": True,
        }
    except Exception as e:
        return {"error": f"Error during incremental re-indexing: {e}", "success": False}


async def force_reindex(ctx: Context, clear_cache: bool = True) -> Dict[str, Any]:
    """Force a complete re-index of the project, ignoring incremental metadata.

    Phase 2 RabbitMQ Integration:
    - Queues files to RabbitMQ for asynchronous Elasticsearch indexing
    - Returns 'indexing_started' status with operation_id
    - Fails gracefully if RabbitMQ is unavailable

    Args:
        clear_cache: Whether to clear all cached data before re-indexing (default: True)
    """
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first.",
            "success": False,
        }

    # PHASE 2: Check RabbitMQ availability
    realtime_indexer = ctx.request_context.lifespan_context.realtime_indexer

    if not realtime_indexer or not realtime_indexer.producer:
        return {
            "error": "RabbitMQ is required for async indexing. Start it with: python run.py start-dev-dbs",
            "success": False,
            "rabbitmq_required": True,
        }

    # Check if RabbitMQ producer has active connection
    if (
        not realtime_indexer.producer.channel
        or not realtime_indexer.producer.connection
        or realtime_indexer.producer.connection.is_closed
    ):
        return {
            "error": "RabbitMQ is required for async indexing. Start it with: python run.py start-dev-dbs",
            "success": False,
            "rabbitmq_required": True,
        }

    try:
        global performance_monitor

        # Start timing the force reindex operation
        if performance_monitor:
            performance_monitor.log_structured(
                "info",
                "Starting force re-index operation",
                base_path=base_path,
                clear_cache=clear_cache,
            )

        # Clear caches if requested
        if clear_cache:
            logger.info("Clearing all caches and metadata...")

            # Clear settings cache
            ctx.request_context.lifespan_context.settings.clear()

            # Clear lazy content manager cache
            global lazy_content_manager
            lazy_content_manager.unload_all()

            # Clear file index
            _safe_clear_file_index()

            # Clear incremental indexer metadata
            settings = ctx.request_context.lifespan_context.settings
            indexer = IncrementalIndexer(settings)
            indexer.clear_metadata()

            # Force garbage collection
            import gc

            gc.collect()

            logger.info("Cache clearing completed.")

        # Create progress tracker for force indexing
        async with ProgressContext(
            operation_name="Force Re-Index",
            total_items=1000,  # Will be updated with actual file count
            stages=["Clearing", "Scanning", "Full Indexing", "Saving"],
        ) as progress_tracker:
            # Stage 1: Clearing (if cache clearing)
            if clear_cache:
                await progress_tracker.update_progress(
                    stage_index=0, message="Cleared all caches and metadata"
                )

            # Stage 2: Scanning
            await progress_tracker.update_progress(
                stage_index=1, message="Starting complete directory scan..."
            )

            # Count files for progress tracking
            total_files = 0
            logger.info(f"Scanning directory: {base_path}")

            for root, dirs, files in os.walk(base_path):
                total_files += len(files)
                # Check for cancellation and provide progress updates
                if total_files % 1000 == 0:
                    progress_tracker.cancellation_token.check_cancelled()
                    await progress_tracker.update_progress(
                        message=f"Scanned {total_files} files so far..."
                    )

            # Update total items with actual count
            progress_tracker.total_items = max(total_files, 1)

            await progress_tracker.update_progress(
                message=f"Complete scan finished: {total_files} files found"
            )

            logger.info(f"Force re-indexing {total_files} files...")

            # Stage 3: Queue files to RabbitMQ for Elasticsearch indexing
            await progress_tracker.update_progress(
                stage_index=2,
                message=f"Queuing {total_files} files to RabbitMQ for indexing...",
            )

            # PHASE 2: Collect all files and queue to RabbitMQ
            files_to_queue = []
            for root, dirs, files in os.walk(base_path):
                for file in files:
                    file_path = os.path.join(root, file)
                    files_to_queue.append(file_path)
                # Check for cancellation periodically
                if len(files_to_queue) % 100 == 0:
                    progress_tracker.cancellation_token.check_cancelled()

            # PHASE 2: Publish each file to RabbitMQ
            files_queued = 0
            producer = realtime_indexer.producer

            for file_path in files_to_queue:
                # Check for cancellation
                if files_queued % 100 == 0:
                    progress_tracker.cancellation_token.check_cancelled()

                # Create indexing operation message
                operation = {
                    "type": "index",
                    "file_path": file_path,
                    "timestamp": datetime.now().isoformat(),
                    "metadata": {"source": "force_reindex"},
                }

                # Publish to RabbitMQ
                producer.publish(operation)
                files_queued += 1

                # Update progress periodically
                if files_queued % 100 == 0:
                    await progress_tracker.update_progress(
                        message=f"Queued {files_queued}/{total_files} files to RabbitMQ..."
                    )

            logger.info(
                f"Force re-index: Queued {files_queued} files to RabbitMQ for indexing"
            )

            # Stage 4: Saving
            await progress_tracker.update_progress(
                stage_index=3, message="Updating metadata..."
            )

            # Save the new index
            ctx.request_context.lifespan_context.settings.save_index(file_index, is_reindex=True)

            # Update config with new timestamp
            config = ctx.request_context.lifespan_context.settings.load_config()
            ctx.request_context.lifespan_context.settings.save_config(
                {
                    **config,
                    "last_indexed": ctx.request_context.lifespan_context.settings._get_timestamp(),
                    "force_reindex_count": config.get("force_reindex_count", 0) + 1,
                }
            )

            await progress_tracker.update_progress(
                message=f"Force re-index started: {files_queued} files queued to RabbitMQ"
            )

        # PHASE 2: Log completion
        if performance_monitor:
            performance_monitor.log_structured(
                "info",
                "Force re-index started (async via RabbitMQ)",
                base_path=base_path,
                files_queued=files_queued,
                elapsed_time=progress_tracker.elapsed_time,
            )
            performance_monitor.increment_counter("force_reindex_operations_total")

        # PHASE 2: Return indexing_started status instead of completion
        return {
            "status": "indexing_started",
            "success": True,
            "message": f"Queued {files_queued} files to RabbitMQ for Elasticsearch indexing.",
            "operation_id": progress_tracker.operation_id,
            "files_queued": files_queued,
            "cache_cleared": clear_cache,
            "elapsed_time": progress_tracker.elapsed_time,
        }

    except asyncio.CancelledError:
        return {
            "error": "Force re-index operation was cancelled",
            "success": False,
            "cancelled": True,
        }
    except Exception as e:
        if performance_monitor:
            performance_monitor.log_structured(
                "error", "Force re-index failed", error=str(e), base_path=base_path
            )
        return {"error": f"Error during force re-indexing: {e}", "success": False}


async def write_to_file(
    path: str, content: str, line_count: int, ctx: Context
) -> Dict[str, Any]:
    """
    Write content to a file. If the file exists, it will be overwritten. If it doesn't exist, it will be created.
    This tool will automatically create any directories needed to write the file.
    """
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker
    dal_instance = ctx.request_context.lifespan_context.dal

    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    full_path = os.path.join(base_path, path)

    # Ensure file is added to metadata store before version tracking
    file_extension = os.path.splitext(path)[1]
    file_type = "file"
    dal_instance.metadata.add_file(path, file_type, file_extension)

    # Capture pre-edit state (use relative path for consistency)
    old_content = file_change_tracker._capture_pre_edit_state(path)

    try:
        # Ensure directory exists
        os.makedirs(os.path.dirname(full_path), exist_ok=True)

        with open(full_path, "w", encoding="utf-8") as f:
            f.write(content)

        # Record post-edit state (use relative path for consistency)
        file_change_tracker._record_post_edit_state(path, old_content, content)
        file_change_tracker.flush()

        # Update Core Engine
        core_engine = ctx.request_context.lifespan_context.core_engine
        if core_engine:
            await core_engine.index_file(base_path, path, content)

        return {"success": True, "message": f"File '{path}' written successfully."}
    except Exception as e:
        return {"success": False, "error": f"Error writing to file '{path}': {e}"}


async def apply_diff(
    path: str,
    search: str,
    replace: str,
    ctx: Context,
    start_line: Optional[int] = None,
    end_line: Optional[int] = None,
    use_regex: bool = False,
    ignore_case: bool = False,
) -> Dict[str, Any]:
    """
    Apply targeted modifications to a file by searching for specific text and replacing it.

    This tool provides a simple and intuitive API for file modifications with support for:
    - Literal text search and replace
    - Regular expression patterns
    - Case-insensitive matching
    - Line range restrictions
    - File change tracking and versioning
    - Real-time indexing integration
    - Comprehensive error handling and rollback

    Args:
        path: Path to the file to modify (relative to project root)
        search: The text or pattern to search for
        replace: The text to replace matches with
        start_line: Optional starting line number for restricted replacement (1-based)
        end_line: Optional ending line number for restricted replacement (1-based)
        use_regex: Whether to treat search as a regular expression pattern
        ignore_case: Whether to perform case-insensitive matching

    Returns:
        A dictionary containing the operation result with success status and details

    Examples:
        # Simple text replacement
        apply_diff("src/main.py", "old_function()", "new_function()")

        # Regex replacement with line range
        apply_diff("config.json", r'"version": "\d+\.\d+\.\d+"', '"version": "2.0.0"',
                   use_regex=True, start_line=1, end_line=10)

        # Case-insensitive replacement
        apply_diff("README.md", "todo", "TODO", ignore_case=True)
    """
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker
    settings = ctx.request_context.lifespan_context.settings
    dal_instance = ctx.request_context.lifespan_context.dal

    # Validate project path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Validate required parameters
    if not path:
        return {"error": "File path is required."}

    if search is None:
        return {"error": "Search text/pattern is required."}

    # Validate line range parameters
    if start_line is not None and start_line < 1:
        return {"error": "start_line must be a positive integer (1-based)."}

    if end_line is not None and end_line < 1:
        return {"error": "end_line must be a positive integer (1-based)."}

    if start_line is not None and end_line is not None and start_line > end_line:
        return {"error": "start_line cannot be greater than end_line."}

    # Construct full path and validate
    full_path = os.path.join(base_path, path)

    if not os.path.exists(full_path):
        return {"success": False, "error": f"File not found: {path}"}

    if not os.path.isfile(full_path):
        return {"success": False, "error": f"Path is not a file: {path}"}

    # Create backup for rollback
    backup_path = None
    try:
        # Create a temporary backup file
        import tempfile

        with tempfile.NamedTemporaryFile(
            mode="w", delete=False, suffix=".backup"
        ) as backup_file:
            with open(full_path, "r", encoding="utf-8") as original_file:
                backup_file.write(original_file.read())
            backup_path = backup_file.name

        # Ensure file is added to metadata store before version tracking
        file_extension = os.path.splitext(path)[1]
        file_type = "file"
        dal_instance.metadata.add_file(path, file_type, file_extension)

        # Read file content
        with open(full_path, "r", encoding="utf-8") as f:
            lines = f.readlines()

        # Capture pre-edit state (use relative path for consistency)
        old_content = file_change_tracker._capture_pre_edit_state(path)

        # Initialize replacement tracking
        modified_lines = []
        replacements_made = 0
        lines_modified = []

        # Set up regex flags
        flags = 0
        if ignore_case:
            flags |= re.IGNORECASE

        # Process each line
        for i, line_content in enumerate(lines):
            line_num = i + 1

            # Check if line is within specified range
            if (start_line is None or line_num >= start_line) and (
                end_line is None or line_num <= end_line
            ):
                original_line = line_content
                if use_regex:
                    # Use regex substitution
                    new_line_content, count = re.subn(
                        search, replace, line_content, flags=flags
                    )
                else:
                    # Use simple string replacement
                    new_line_content = line_content.replace(search, replace)
                    count = (len(line_content) - len(new_line_content)) // max(
                        1, len(search)
                    )

                if count > 0:
                    lines_modified.append(line_num)

                replacements_made += count
                modified_lines.append(new_line_content)
            else:
                modified_lines.append(line_content)

        # Check if any replacements were made
        if replacements_made == 0:
            # Clean up backup file
            if backup_path and os.path.exists(backup_path):
                os.unlink(backup_path)
            return {
                "success": False,
                "error": f"No occurrences of '{search}' found in the specified range.",
                "replacements_made": 0,
                "search_pattern": search,
                "use_regex": use_regex,
                "ignore_case": ignore_case,
            }

        # Write modified content back to file
        modified_content = "".join(modified_lines)
        with open(full_path, "w", encoding="utf-8") as f:
            f.write(modified_content)

        # Record post-edit state (use relative path for consistency)
        file_change_tracker._record_post_edit_state(
            path, old_content, modified_content, operation_type="apply_diff"
        )
        file_change_tracker.flush()

        # Update incremental indexer
        indexer = IncrementalIndexer(settings)
        indexer.update_file_metadata(path, full_path)
        indexer.save_metadata()

        # Update Core Engine
        core_engine = ctx.request_context.lifespan_context.core_engine
        if core_engine:
            await core_engine.index_file(base_path, path, modified_content)

        # Enqueue for real-time indexing if available
        realtime_indexer = ctx.request_context.lifespan_context.realtime_indexer
        if realtime_indexer:
            realtime_indexer.enqueue_change(path, "update")
            message = f"Successfully replaced {replacements_made} occurrence(s) in '{path}' and enqueued for update."
        else:
            message = f"Successfully replaced {replacements_made} occurrence(s) in '{path}' (real-time indexing not active)."

        # Clean up backup file on success
        if backup_path and os.path.exists(backup_path):
            os.unlink(backup_path)

        return {
            "success": True,
            "message": message,
            "replacements_made": replacements_made,
            "lines_modified": lines_modified,
            "file_path": path,
            "search_pattern": search,
            "use_regex": use_regex,
            "ignore_case": ignore_case,
            "line_range": {"start": start_line, "end": end_line}
            if start_line or end_line
            else None,
        }

    except re.error as e:
        # Rollback on regex error
        _rollback_file(full_path, backup_path)
        return {"success": False, "error": f"Invalid regular expression: {e}"}
    except UnicodeDecodeError as e:
        # Rollback on encoding error
        _rollback_file(full_path, backup_path)
        return {"success": False, "error": f"File encoding error: {e}"}
    except PermissionError as e:
        # Rollback on permission error
        _rollback_file(full_path, backup_path)
        return {"success": False, "error": f"Permission denied: {e}"}
    except Exception as e:
        # Rollback on any other error
        _rollback_file(full_path, backup_path)
        logger.error(f"Error applying diff to file '{path}': {e}", exc_info=True)
        return {"success": False, "error": f"Error applying diff to file '{path}': {e}"}


def _rollback_file(original_path: str, backup_path: str) -> bool:
    """
    Rollback a file to its backup state.

    Args:
        original_path: Path to the original file
        backup_path: Path to the backup file

    Returns:
        True if rollback was successful, False otherwise
    """
    if not backup_path or not os.path.exists(backup_path):
        return False

    try:
        with open(backup_path, "r", encoding="utf-8") as backup_file:
            backup_content = backup_file.read()

        with open(original_path, "w", encoding="utf-8") as original_file:
            original_file.write(backup_content)

        # Clean up backup file
        os.unlink(backup_path)
        logger.info(f"Successfully rolled back file: {original_path}")
        return True
    except Exception as e:
        logger.error(f"Failed to rollback file {original_path}: {e}")
        return False


async def insert_content(
    path: str, line: int, content: str, ctx: Context
) -> Dict[str, Any]:
    """
    Insert new lines of content into a file without modifying existing content.
    Specify the line number to insert before, or use line 0 to append to the end.
    """
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker

    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    full_path = os.path.join(base_path, path)

    if not os.path.exists(full_path):
        return {"success": False, "error": f"File not found: {path}"}

    try:
        with open(full_path, "r", encoding="utf-8") as f:
            lines = f.readlines()

        # Capture pre-edit state
        old_content = file_change_tracker._capture_pre_edit_state(full_path)

        # Adjust line number for 0-indexed list
        insert_idx = line - 1 if line > 0 else len(lines)

        # Insert content
        new_lines = content.splitlines(keepends=True)
        modified_lines = lines[:insert_idx] + new_lines + lines[insert_idx:]

        modified_content = "".join(modified_lines)

        with open(full_path, "w", encoding="utf-8") as f:
            f.write(modified_content)

        # Record post-edit state
        file_change_tracker._record_post_edit_state(
            full_path, old_content, modified_content
        )
        file_change_tracker.flush()

        return {
            "success": True,
            "message": f"Content inserted into '{path}' at line {line}.",
        }
    except Exception as e:
        return {
            "success": False,
            "error": f"Error inserting content into file '{path}': {e}",
        }


async def search_and_replace(
    path: str,
    search: str,
    replace: str,
    ctx: Context,
    start_line: Optional[int] = None,
    end_line: Optional[int] = None,
    use_regex: bool = False,
    ignore_case: bool = False,
) -> Dict[str, Any]:
    """
    Find and replace specific text strings or patterns (using regex) within a file.
    """
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker
    settings = ctx.request_context.lifespan_context.settings
    dal_instance = ctx.request_context.lifespan_context.dal

    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    full_path = os.path.join(base_path, path)

    if not os.path.exists(full_path):
        return {"success": False, "error": f"File not found: {path}"}

    try:
        # Ensure file is added to metadata store before version tracking
        file_extension = os.path.splitext(path)[1]
        file_type = "file"
        dal_instance.metadata.add_file(path, file_type, file_extension)

        with open(full_path, "r", encoding="utf-8") as f:
            lines = f.readlines()

        # Capture pre-edit state (use relative path for consistency)
        old_content = file_change_tracker._capture_pre_edit_state(path)

        modified_lines = []
        replacements_made = 0

        import re

        flags = 0
        if ignore_case:
            flags |= re.IGNORECASE

        for i, line_content in enumerate(lines):
            line_num = i + 1
            if (start_line is None or line_num >= start_line) and (
                end_line is None or line_num <= end_line
            ):
                if use_regex:
                    new_line_content, count = re.subn(
                        search, replace, line_content, flags=flags
                    )
                else:
                    new_line_content = line_content.replace(search, replace)
                    count = (len(line_content) - len(new_line_content)) // max(
                        1, len(search)
                    )  # Simple count for non-regex

                replacements_made += count
                modified_lines.append(new_line_content)
            else:
                modified_lines.append(line_content)

        modified_content = "".join(modified_lines)

        with open(full_path, "w", encoding="utf-8") as f:
            f.write(modified_content)

        # Record post-edit state (use relative path for consistency)
        file_change_tracker._record_post_edit_state(
            path, old_content, modified_content, operation_type="search_replace"
        )
        file_change_tracker.flush()

        # Update incremental indexer
        indexer = IncrementalIndexer(settings)
        indexer.update_file_metadata(path, full_path)
        indexer.save_metadata()

        return {
            "success": True,
            "message": f"Replaced {replacements_made} occurrences in '{path}'.",
        }
    except Exception as e:
        return {
            "success": False,
            "error": f"Error performing search and replace in '{path}': {e}",
        }


async def delete_file(file_path: str, ctx: Context) -> Dict[str, Any]:
    """
    A tool to delete a specified file.
    """
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker
    settings = ctx.request_context.lifespan_context.settings

    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    full_path = os.path.join(base_path, file_path)

    if not os.path.exists(full_path):
        return {"success": False, "error": f"File not found: {file_path}"}

    try:
        # Capture pre-edit state
        old_content = file_change_tracker._capture_pre_edit_state(full_path)

        # Perform file deletion
        os.remove(full_path)

        # Record post-edit state (new_content is empty for deletion)
        file_change_tracker._record_post_edit_state(
            full_path, old_content, "", operation_type="delete"
        )
        file_change_tracker.flush()

        # Update the incremental indexer
        indexer = IncrementalIndexer(settings)
        indexer.remove_file_metadata(file_path)
        indexer.save_metadata()

        # Remove from in-memory file_index
        global file_index
        _remove_file_from_index(file_index, file_path)
        ctx.request_context.lifespan_context.file_count = _count_files(file_index)
        ctx.request_context.lifespan_context.settings.save_index(file_index)

        # Update Core Engine
        core_engine = ctx.request_context.lifespan_context.core_engine
        if core_engine:
            await core_engine.delete_file(base_path, file_path)

        return {"success": True, "message": f"File '{file_path}' deleted successfully."}
    except Exception as e:
        return {"success": False, "error": f"Error deleting file '{file_path}': {e}"}


async def rename_file(
    old_file_path: str, new_file_path: str, ctx: Context
) -> Dict[str, Any]:
    """
    A tool to rename/move a file.
    """
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker
    settings = ctx.request_context.lifespan_context.settings

    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    full_old_path = os.path.join(base_path, old_file_path)
    full_new_path = os.path.join(base_path, new_file_path)

    if not os.path.exists(full_old_path):
        return {"success": False, "error": f"Old file not found: {old_file_path}"}

    if os.path.exists(full_new_path):
        return {"success": False, "error": f"New file already exists: {new_file_path}"}

    try:
        # Capture pre-edit state of the old file
        old_content = file_change_tracker._capture_pre_edit_state(full_old_path)

        # Ensure new directory exists
        os.makedirs(os.path.dirname(full_new_path), exist_ok=True)

        # Perform the rename/move
        os.rename(full_old_path, full_new_path)

        # Record post-edit state for the rename operation
        # We pass old_file_path as the primary identifier for tracking,
        # and new_file_path as an additional detail.
        # The content is the same, but the path changes.
        file_change_tracker._record_post_edit_state(
            old_file_path,
            old_content,
            old_content,  # Content remains the same for rename
            operation_type="rename",
            new_file_path=new_file_path,
        )
        file_change_tracker.flush()

        # Update the incremental indexer
        indexer = IncrementalIndexer(settings)
        indexer.rename_file_metadata(old_file_path, new_file_path, full_new_path)
        indexer.save_metadata()

        # Update in-memory file_index
        global file_index
        _remove_file_from_index(file_index, old_file_path)
        _add_file_to_index(file_index, new_file_path)
        ctx.request_context.lifespan_context.file_count = _count_files(file_index)
        ctx.request_context.lifespan_context.settings.save_index(file_index)

        # Update Core Engine
        core_engine = ctx.request_context.lifespan_context.core_engine
        if core_engine:
            await core_engine.delete_file(base_path, old_file_path)
            # Re-index at new path (using old_content which is same)
            if old_content is not None:
                await core_engine.index_file(base_path, new_file_path, old_content)

        return {
            "success": True,
            "message": f"File '{old_file_path}' renamed to '{new_file_path}' successfully.",
        }
    except Exception as e:
        return {
            "success": False,
            "error": f"Error renaming file '{old_file_path}' to '{new_file_path}': {e}",
        }


async def revert_file_to_version(
    file_path: str,
    ctx: Context,
    version_id: Optional[str] = None,
    timestamp: Optional[str] = None,
) -> Dict[str, Any]:
    """
    A tool to revert a file to a previous version.
    """
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker
    settings = ctx.request_context.lifespan_context.settings

    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    full_path = os.path.join(base_path, file_path)

    if not os.path.exists(full_path):
        return {"success": False, "error": f"File not found: {file_path}"}

    if not version_id and not timestamp:
        return {
            "success": False,
            "error": "Either 'version_id' or 'timestamp' must be provided to revert.",
        }

    try:
        # Capture the current state of the file before reverting
        current_content = file_change_tracker._capture_pre_edit_state(full_path)

        # Reconstruct the desired historical version
        reconstructed_content = file_change_tracker.reconstruct_file_version(
            file_path, version_id=version_id, timestamp=timestamp
        )

        if reconstructed_content is None:
            return {
                "success": False,
                "error": f"Could not reconstruct version for '{file_path}' with version_id '{version_id}' or timestamp '{timestamp}'. Version might not exist or reconstruction failed.",
            }

        # Overwrite the current file content with the reconstructed content
        with open(full_path, "w", encoding="utf-8") as f:
            f.write(reconstructed_content)

        # Record the post-edit state for the revert operation
        file_change_tracker._record_post_edit_state(
            full_path, current_content, reconstructed_content, operation_type="revert"
        )
        file_change_tracker.flush()

        # Update the incremental indexer
        indexer = IncrementalIndexer(settings)
        indexer.update_file_metadata(file_path, full_path)
        indexer.save_metadata()

        return {
            "success": True,
            "message": f"File '{file_path}' reverted to specified version successfully.",
        }
    except Exception as e:
        return {"success": False, "error": f"Error reverting file '{file_path}': {e}"}


def get_file_history(file_path: str, ctx: Context) -> Dict[str, Any]:
    """Retrieves the history of changes for a given file path."""
    base_path = ctx.request_context.lifespan_context.base_path
    file_change_tracker = ctx.request_context.lifespan_context.file_change_tracker
    dal_instance = ctx.request_context.lifespan_context.dal

    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Normalize the file path
    norm_path = os.path.normpath(file_path)

    # Check for path traversal attempts
    if "..\\" in norm_path or "../" in norm_path or norm_path.startswith(".."):
        return {
            "error": f"Invalid file path: {file_path} (directory traversal not allowed)"
        }

    # Ensure the path is relative to base_path
    if os.path.isabs(norm_path):
        # If absolute path is provided, make it relative to base_path
        try:
            norm_path = os.path.relpath(norm_path, base_path)
        except ValueError:
            return {"error": f"File path is not within project directory: {file_path}"}

    full_path = os.path.join(base_path, norm_path)

    if not os.path.exists(full_path):
        return {"success": False, "error": f"File not found: {file_path}"}

    try:
        logger.info(f"Processing get_file_history for file: {norm_path}")

        # Ensure file is registered in metadata store before querying history
        file_extension = os.path.splitext(norm_path)[1]
        file_type = "file"
        logger.debug(
            f"Attempting to register file {norm_path} with extension {file_extension}"
        )
        registration_success = dal_instance.metadata.add_file(
            norm_path, file_type, file_extension
        )

        if not registration_success:
            logger.warning(f"Failed to register file {norm_path} in metadata store")
        else:
            logger.debug(f"Successfully registered file {norm_path}")

        # Create initial version if this is the first time accessing history for this file
        # This ensures we have at least one version to show in the history
        file_info = dal_instance.metadata.get_file_info(norm_path)
        logger.debug(f"File info for {norm_path}: {file_info}")

        if file_info:
            # Check if file has any versions
            existing_versions = dal_instance.metadata.get_file_versions_for_path(
                norm_path
            )
            logger.debug(
                f"Existing versions for {norm_path}: {len(existing_versions)} found"
            )

            if not existing_versions:
                # Create initial version
                logger.info(f"Creating initial version for file {norm_path}")
                old_content = file_change_tracker._capture_pre_edit_state(norm_path)
                if old_content is not None:
                    # Read current content
                    with open(full_path, "r", encoding="utf-8", errors="ignore") as f:
                        current_content = f.read()

                    logger.debug(
                        f"Captured content for initial version, length: {len(current_content)}"
                    )
                    # Record initial version
                    file_change_tracker._record_post_edit_state(
                        norm_path,
                        None,
                        current_content,
                        operation_type="initial_version",
                    )
                    file_change_tracker.flush()
                    logger.info(f"Created initial version for {norm_path}")
                else:
                    logger.warning(f"Failed to capture pre-edit state for {norm_path}")
            else:
                logger.debug(
                    f"File {norm_path} already has {len(existing_versions)} versions"
                )
        else:
            logger.warning(f"File {norm_path} is not registered in metadata store")

        # Use normalized relative path for consistency with storage
        logger.debug(f"Retrieving history for {norm_path}")
        history = file_change_tracker.get_file_history(norm_path)
        logger.info(f"Retrieved {len(history)} history items for {norm_path}")

        return {"success": True, "file_path": norm_path, "history": history}
    except Exception as e:
        logger.error(
            f"Error retrieving file history for '{norm_path}': {e}", exc_info=True
        )
        return {
            "success": False,
            "error": f"Error retrieving file history for '{norm_path}': {e}",
        }


def get_settings_info(ctx: Context) -> Dict[str, Any]:
    """Get information about the project settings."""
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        # Even if base_path is not set, we can still show the temp directory
        temp_dir = os.path.join(tempfile.gettempdir(), SETTINGS_DIR)
        return {
            "status": "not_configured",
            "message": "Project path not set. Please use set_project_path to set a project directory first.",
            "temp_directory": temp_dir,
            "temp_directory_exists": os.path.exists(temp_dir),
        }

    settings = ctx.request_context.lifespan_context.settings

    # Get config
    config = settings.load_config()

    # Get stats
    stats = settings.get_stats()

    # Get temp directory
    temp_dir = os.path.join(tempfile.gettempdir(), SETTINGS_DIR)

    return {
        "settings_directory": settings.settings_path,
        "temp_directory": temp_dir,
        "temp_directory_exists": os.path.exists(temp_dir),
        "config": config,
        "stats": stats,
        "exists": os.path.exists(settings.settings_path),
    }


def create_temp_directory() -> Dict[str, Any]:
    """Create the temporary directory used for storing index data."""
    temp_dir = os.path.join(tempfile.gettempdir(), SETTINGS_DIR)

    result = {
        "temp_directory": temp_dir,
        "existed_before": os.path.exists(temp_dir),
    }

    try:
        # Use OptimizedProjectSettings to handle directory creation consistently
        temp_settings = OptimizedProjectSettings("", skip_load=True)

        result["created"] = not result["existed_before"]
        result["exists_now"] = os.path.exists(temp_dir)
        result["is_directory"] = os.path.isdir(temp_dir)
    except Exception as e:
        result["error"] = str(e)

    return result


def check_temp_directory() -> Dict[str, Any]:
    """Check the temporary directory used for storing index data."""
    temp_dir = os.path.join(tempfile.gettempdir(), SETTINGS_DIR)

    result = {
        "temp_directory": temp_dir,
        "exists": os.path.exists(temp_dir),
        "is_directory": os.path.isdir(temp_dir) if os.path.exists(temp_dir) else False,
        "temp_root": tempfile.gettempdir(),
    }

    # If the directory exists, list its contents
    if result["exists"] and result["is_directory"]:
        try:
            contents = os.listdir(temp_dir)
            result["contents"] = contents
            result["subdirectories"] = []

            # Check each subdirectory
            for item in contents:
                item_path = os.path.join(temp_dir, item)
                if os.path.isdir(item_path):
                    subdir_info = {
                        "name": item,
                        "path": item_path,
                        "contents": os.listdir(item_path)
                        if os.path.exists(item_path)
                        else [],
                    }
                    result["subdirectories"].append(subdir_info)
        except Exception as e:
            result["error"] = str(e)

    return result


def clear_settings(ctx: Context) -> str:
    """Clear all settings and cached data."""
    settings = ctx.request_context.lifespan_context.settings
    settings.clear()
    return "Project settings, index, and cache have been cleared."


def reset_server_state(ctx: Context) -> str:
    """Completely reset the server state including global variables."""
    global \
        file_index, \
        lazy_content_manager, \
        memory_profiler, \
        memory_aware_manager, \
        performance_monitor

    try:
        # Reset global file_index to empty dict
        file_index = {}

        # Clear lazy content manager
        lazy_content_manager.unload_all()

        # Reset context to empty state
        ctx.request_context.lifespan_context.base_path = ""
        ctx.request_context.lifespan_context.file_count = 0

        # Create fresh settings with skip_load=True
        ctx.request_context.lifespan_context.settings = OptimizedProjectSettings(
            "", skip_load=True, storage_backend="sqlite", use_trie_index=True
        )

        # Stop memory profiler if running
        if memory_profiler:
            try:
                memory_profiler.stop_monitoring()
            except:
                pass
        memory_profiler = None
        memory_aware_manager = None
        performance_monitor = None

        return (
            "Server state completely reset. All global variables and context cleared."
        )
    except Exception as e:
        return f"Error resetting server state: {e}"


def refresh_search_tools(ctx: Context) -> str:
    """
    Manually re-detect the available command-line search tools on the system.
    This is useful if you have installed a new tool (like ripgrep) after starting the server.
    """
    settings = ctx.request_context.lifespan_context.settings
    settings.refresh_available_strategies()

    config = settings.get_search_tools_config()

    return f"Search tools refreshed. Available: {config['available_tools']}. Preferred: {config['preferred_tool']}."


def get_ignore_patterns(ctx: Context) -> Dict[str, Any]:
    """Get information about the loaded ignore patterns."""
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Initialize ignore pattern matcher
    ignore_matcher = IgnorePatternMatcher(base_path)

    # Get pattern information
    pattern_info = ignore_matcher.get_pattern_sources()
    all_patterns = ignore_matcher.get_patterns()

    return {
        "base_path": base_path,
        "pattern_sources": pattern_info,
        "all_patterns": all_patterns,
        "gitignore_path": str(ignore_matcher.base_path / ".gitignore"),
        "ignore_path": str(ignore_matcher.base_path / ".ignore"),
        "default_excludes": list(ignore_matcher.DEFAULT_EXCLUDES),
    }


def get_filtering_config() -> Dict[str, Any]:
    """Get information about the current filtering configuration."""
    config_manager = ConfigManager()

    # Get filtering stats
    filtering_stats = config_manager.get_filtering_stats()

    # Add some examples of current limits
    examples = {
        "file_size_examples": {
            "python_file_limit": config_manager.get_max_file_size("example.py"),
            "javascript_file_limit": config_manager.get_max_file_size("example.js"),
            "json_file_limit": config_manager.get_max_file_size("example.json"),
            "markdown_file_limit": config_manager.get_max_file_size("example.md"),
            "default_file_limit": config_manager.get_max_file_size("example.unknown"),
        },
        "directory_limits": {
            "max_files_per_directory": config_manager.get_max_files_per_directory(),
            "max_subdirectories_per_directory": config_manager.get_max_subdirectories_per_directory(),
        },
    }

    return {
        "filtering_configuration": filtering_stats,
        "examples": examples,
        "performance_settings": {
            "logging_enabled": config_manager.should_log_filtering_decisions(),
            "parallel_processing": config_manager.is_parallel_processing_enabled(),
            "max_workers": config_manager.get_max_workers(),
            "directory_caching": config_manager.is_directory_scan_caching_enabled(),
        },
    }


def get_lazy_loading_stats() -> Dict[str, Any]:
    """Get statistics about the lazy loading memory management."""
    global lazy_content_manager

    memory_stats = lazy_content_manager.get_memory_stats()

    return {
        "lazy_loading_enabled": True,
        "memory_stats": memory_stats,
        "description": "File contents are loaded on-demand to optimize memory usage",
    }


def get_incremental_indexing_stats(ctx: Context) -> Dict[str, Any]:
    """Get statistics about incremental indexing metadata."""
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    try:
        # Initialize incremental indexer
        settings = ctx.request_context.lifespan_context.settings
        indexer = IncrementalIndexer(settings)

        # Get indexer statistics
        stats = indexer.get_stats()

        return {
            "base_path": base_path,
            "incremental_indexing_enabled": True,
            "metadata_stats": stats,
            "metadata_file_path": settings.get_metadata_path(),
        }
    except Exception as e:
        return {
            "error": f"Error getting incremental indexing stats: {e}",
            "base_path": base_path,
        }


def get_memory_profile() -> Dict[str, Any]:
    """
    Get comprehensive memory profiling statistics with robust error handling and defensive programming.

    This function provides detailed memory usage information including:
    - Current memory snapshot with process and heap statistics
    - Memory limits and violations with actionable recommendations
    - Content manager statistics with timeout protection
    - Performance metrics and monitoring status
    - Comprehensive error handling, diagnostics, and graceful degradation
    - Initialization validation and recovery mechanisms

    Returns:
        Dictionary containing memory profile data or error information with recovery suggestions
    """
    import time
    import psutil
    import signal
    from contextlib import contextmanager
    from typing import Optional, Dict, Any

    global \
        memory_profiler, \
        lazy_content_manager, \
        memory_aware_manager, \
        _current_project_path

    # Initialize result structure with comprehensive metadata
    result = {
        "timestamp": time.time(),
        "request_id": f"memory_profile_{int(time.time() * 1000)}",
        "diagnostics": {},
        "warnings": [],
        "errors": [],
        "initialization_status": {},
        "recovery_actions": [],
    }

    def add_diagnostic(key: str, value: Any, level: str = "info"):
        """Add diagnostic information to the result."""
        result["diagnostics"][key] = {
            "value": value,
            "level": level,
            "timestamp": time.time(),
        }

    def add_warning(message: str, details: Optional[Dict] = None):
        """Add a warning to the result."""
        warning = {"message": message, "timestamp": time.time(), "severity": "warning"}
        if details:
            warning["details"] = details
        result["warnings"].append(warning)
        logger.warning(f"Memory profile warning: {message}")

    def add_error(
        message: str,
        exception: Optional[Exception] = None,
        details: Optional[Dict] = None,
    ):
        """Add an error to the result."""
        error = {"message": message, "timestamp": time.time(), "severity": "error"}
        if exception:
            error["exception_type"] = type(exception).__name__
            error["exception_message"] = str(exception)
        if details:
            error["details"] = details
        result["errors"].append(error)
        logger.error(f"Memory profile error: {message}", exc_info=exception)

    def add_recovery_action(
        action: str, priority: str = "medium", details: Optional[Dict] = None
    ):
        """Add a recovery action to the result."""
        recovery = {"action": action, "priority": priority, "timestamp": time.time()}
        if details:
            recovery["details"] = details
        result["recovery_actions"].append(recovery)

    @contextmanager
    def timeout_context(seconds: float):
        """Context manager for timeout handling."""

        def timeout_handler(signum, frame):
            raise TimeoutError(f"Operation timed out after {seconds} seconds")

        signal.signal(signal.SIGALRM, timeout_handler)
        signal.alarm(int(seconds))
        try:
            yield
        finally:
            signal.alarm(0)

    def safe_get_memory_stats(manager, timeout_seconds: float = 5.0):
        """Safely get memory stats with timeout and error handling."""
        if manager is None:
            return {
                "loaded_files": 0,
                "query_cache_size": 0,
                "total_managed_files": 0,
                "manager_unavailable": True,
            }

        try:
            with timeout_context(timeout_seconds):
                stats = manager.get_memory_stats()
                if stats is None:
                    raise ValueError("Memory stats returned None")
                return stats
        except TimeoutError:
            add_warning(
                "Content stats collection timed out",
                {
                    "timeout_seconds": timeout_seconds,
                    "impact": "Using fallback statistics",
                    "recovery": "Consider increasing timeout or checking manager health",
                },
            )
            return {
                "loaded_files": 0,
                "query_cache_size": 0,
                "total_managed_files": 0,
                "collection_timeout": True,
            }
        except Exception as e:
            add_error(
                "Failed to collect content statistics",
                e,
                {
                    "impact": "Content statistics unavailable",
                    "fallback": "Using default values",
                },
            )
            return {
                "loaded_files": 0,
                "query_cache_size": 0,
                "total_managed_files": 0,
                "collection_error": str(e),
            }

    def validate_initialization():
        """Comprehensive initialization validation."""
        init_status = {}

        # Check project path
        if not _current_project_path:
            init_status["project_path"] = False
            add_error(
                "Project path not set",
                details={
                    "global_project_path": _current_project_path,
                    "suggestion": "Use set_project_path to configure a project directory first",
                },
            )
            add_recovery_action(
                "Call set_project_path with a valid project directory", "high"
            )
        else:
            init_status["project_path"] = True
            add_diagnostic("project_path", _current_project_path)

        # Check lazy content manager
        if lazy_content_manager is None:
            init_status["lazy_content_manager"] = False
            add_warning(
                "Lazy content manager not initialized",
                {
                    "impact": "Content statistics will be unavailable",
                    "recovery": "Manager should be initialized during server startup",
                },
            )
            add_recovery_action(
                "Ensure LazyContentManager is properly initialized", "medium"
            )
        else:
            init_status["lazy_content_manager"] = True
            add_diagnostic("lazy_content_manager", "available")

        # Check memory profiler
        if memory_profiler is None:
            init_status["memory_profiler"] = False
            add_error(
                "Memory profiler not initialized",
                details={
                    "profiler_status": "None",
                    "suggestion": "Memory profiler should be initialized during set_project_path",
                },
            )
            add_recovery_action(
                "Re-initialize memory profiler in set_project_path", "high"
            )
        else:
            init_status["memory_profiler"] = True
            add_diagnostic("memory_profiler", "available")

        # Check memory-aware manager
        if memory_aware_manager is None:
            init_status["memory_aware_manager"] = False
            add_warning(
                "Memory-aware manager not initialized",
                {
                    "impact": "Automatic memory management may not work properly",
                    "recovery": "Manager should be created during profiler initialization",
                },
            )
            add_recovery_action(
                "Create MemoryAwareLazyContentManager during initialization", "low"
            )
        else:
            init_status["memory_aware_manager"] = True
            add_diagnostic("memory_aware_manager", "available")

        result["initialization_status"] = init_status
        return all(init_status.values())

    def collect_system_info():
        """Collect system-level information safely."""
        add_diagnostic("system_info_collection", "starting")

        try:
            system_memory = psutil.virtual_memory()
            system_stats = {
                "total_mb": system_memory.total / 1024 / 1024,
                "available_mb": system_memory.available / 1024 / 1024,
                "used_mb": system_memory.used / 1024 / 1024,
                "percentage": system_memory.percent,
            }
            add_diagnostic("system_memory", system_stats, "success")

            # Get CPU info
            cpu_percent = psutil.cpu_percent(interval=0.1)
            add_diagnostic("cpu_usage", f"{cpu_percent}%", "info")

            return system_stats
        except Exception as e:
            add_warning(
                "Could not collect system information",
                {"error": str(e), "impact": "System metrics unavailable"},
            )
            return None

    try:
        logger.info("Starting comprehensive memory profile collection")

        # Step 1: Validate initialization comprehensively
        add_diagnostic("initialization_validation", "starting")
        initialization_ok = validate_initialization()

        if not initialization_ok:
            result["status"] = "initialization_failed"
            result["collection_duration_ms"] = (
                time.time() - result["timestamp"]
            ) * 1000
            return result

        # Step 2: Collect system information
        system_info = collect_system_info()

        # Step 3: Collect content manager statistics safely
        add_diagnostic("content_stats_collection", "starting")
        content_stats = safe_get_memory_stats(lazy_content_manager)

        # Step 4: Take memory snapshot with comprehensive error handling
        add_diagnostic("snapshot_collection", "starting")

        try:
            snapshot = memory_profiler.take_snapshot(
                loaded_files=content_stats.get("loaded_files", 0),
                cached_queries=content_stats.get("query_cache_size", 0),
            )
            add_diagnostic("snapshot", "successful", "success")

        except psutil.NoSuchProcess as e:
            add_error(
                "Process monitoring failed - process may have ended",
                e,
                {
                    "impact": "Memory statistics unavailable",
                    "suggestion": "Restart the server",
                    "recovery": "Check if server process is still running",
                },
            )
            add_recovery_action("Restart the MCP server", "high")
            result["status"] = "process_error"
            result["collection_duration_ms"] = (
                time.time() - result["timestamp"]
            ) * 1000
            return result

        except psutil.AccessDenied as e:
            add_error(
                "Access denied for memory monitoring",
                e,
                {
                    "impact": "Limited memory statistics available",
                    "suggestion": "Check system permissions",
                    "recovery": "Run server with appropriate permissions or check psutil access",
                },
            )
            add_recovery_action(
                "Check system permissions for process monitoring", "high"
            )

            # Create minimal snapshot for partial functionality
            snapshot = type(
                "MinimalSnapshot",
                (),
                {
                    "timestamp": time.time(),
                    "process_memory_mb": 0.0,
                    "heap_size_mb": 0.0,
                    "peak_memory_mb": 0.0,
                    "gc_objects": 0,
                    "gc_collections": (0, 0, 0),
                    "active_threads": threading.active_count(),
                    "loaded_files": content_stats.get("loaded_files", 0),
                    "cached_queries": content_stats.get("query_cache_size", 0),
                },
            )()

        except Exception as e:
            add_error(
                "Failed to take memory snapshot",
                e,
                {
                    "impact": "Memory snapshot unavailable",
                    "recovery": "Check memory profiler health and psutil installation",
                },
            )
            add_recovery_action(
                "Verify memory profiler and psutil are working correctly", "high"
            )
            result["status"] = "snapshot_error"
            result["collection_duration_ms"] = (
                time.time() - result["timestamp"]
            ) * 1000
            return result

        # Step 5: Get comprehensive profiler statistics
        add_diagnostic("profiler_stats_collection", "starting")

        try:
            profiler_stats = memory_profiler.get_stats()
            add_diagnostic("profiler_stats", "collected", "success")

        except Exception as e:
            add_error(
                "Failed to collect profiler statistics",
                e,
                {
                    "impact": "Detailed profiler stats unavailable",
                    "recovery": "Check profiler internal state",
                },
            )
            add_recovery_action("Investigate memory profiler internal state", "medium")
            profiler_stats = {"error": "Collection failed", "partial_data": True}

        # Step 6: Check for memory limit violations with recommendations
        add_diagnostic("limit_violations_check", "starting")

        try:
            violations = memory_profiler.check_limits(snapshot)
            violation_count = sum(1 for v in violations.values() if v)

            if violation_count > 0:
                violation_details = []
                recommendations = []

                if violations.get("soft_limit", False):
                    violation_details.append("Soft memory limit exceeded")
                    recommendations.append("Consider triggering garbage collection")
                    add_recovery_action("Trigger manual garbage collection", "medium")

                if violations.get("hard_limit", False):
                    violation_details.append("Hard memory limit exceeded")
                    recommendations.append(
                        "Immediate action required - consider restarting"
                    )
                    add_recovery_action(
                        "Restart server or increase memory limits", "high"
                    )

                if violations.get("max_loaded_files", False):
                    violation_details.append("Maximum loaded files exceeded")
                    recommendations.append("Unload unused files from memory")
                    add_recovery_action("Unload least recently used files", "medium")

                if violations.get("max_cached_queries", False):
                    violation_details.append("Maximum cached queries exceeded")
                    recommendations.append("Clear query cache")
                    add_recovery_action("Clear query cache to free memory", "low")

                add_warning(
                    f"Memory limit violations detected: {violation_count}",
                    {
                        "violations": violations,
                        "violation_details": violation_details,
                        "recommendations": recommendations,
                    },
                )

            add_diagnostic("limit_violations", violations)

        except Exception as e:
            add_warning(
                "Could not check memory limit violations",
                {"error": str(e), "impact": "Violation status unknown"},
            )

        # Step 7: Build final result with comprehensive data
        result.update(
            {
                "status": "success",
                "current_snapshot": {
                    "timestamp": snapshot.timestamp,
                    "process_memory_mb": snapshot.process_memory_mb,
                    "heap_size_mb": snapshot.heap_size_mb,
                    "peak_memory_mb": snapshot.peak_memory_mb,
                    "gc_objects": snapshot.gc_objects,
                    "gc_collections": snapshot.gc_collections,
                    "active_threads": snapshot.active_threads,
                    "loaded_files": snapshot.loaded_files,
                    "cached_queries": snapshot.cached_queries,
                },
                "profiler_stats": profiler_stats,
                "content_manager_stats": content_stats,
                "system_info": system_info,
                "project_path": _current_project_path,
                "collection_duration_ms": (time.time() - result["timestamp"]) * 1000,
            }
        )

        # Step 8: Generate comprehensive summary and recommendations
        warning_count = len(result["warnings"])
        error_count = len(result["errors"])

        result["summary"] = {
            "total_warnings": warning_count,
            "total_errors": error_count,
            "data_completeness": "full"
            if error_count == 0
            else "partial"
            if warning_count == 0
            else "degraded",
            "recommendations": [],
            "health_score": max(0, 100 - (error_count * 20) - (warning_count * 5)),
        }

        # Add intelligent recommendations based on findings
        if error_count > 0:
            result["summary"]["recommendations"].append(
                "Review error details and address underlying issues"
            )
        if warning_count > 0:
            result["summary"]["recommendations"].append(
                "Review warnings for potential optimizations"
            )

        # Memory-specific recommendations
        if snapshot.process_memory_mb > (memory_profiler.limits.hard_limit_mb * 0.9):
            result["summary"]["recommendations"].append(
                "Critical: Memory usage near hard limit - immediate action required"
            )
            add_recovery_action(
                "Reduce memory usage or increase limits immediately", "critical"
            )
        elif snapshot.process_memory_mb > (memory_profiler.limits.soft_limit_mb * 0.9):
            result["summary"]["recommendations"].append(
                "High memory usage - consider cleanup"
            )
            add_recovery_action(
                "Trigger memory cleanup to prevent hard limit violation", "medium"
            )

        # Performance recommendations
        if result["collection_duration_ms"] > 1000:  # Over 1 second
            result["summary"]["recommendations"].append(
                "Slow collection detected - investigate performance bottlenecks"
            )

        logger.info(
            f"Memory profile collection completed successfully in {result['collection_duration_ms']:.2f}ms"
        )
        return result

    except Exception as e:
        # Catch-all exception handler for unexpected errors
        add_error(
            "Unexpected error during memory profile collection",
            e,
            {
                "impact": "Memory profile collection failed completely",
                "suggestion": "Check server logs for detailed error information",
            },
        )
        add_recovery_action("Check server logs and restart if necessary", "high")

        result["status"] = "unexpected_error"
        result["collection_duration_ms"] = (time.time() - result["timestamp"]) * 1000

        logger.error("Unexpected error in get_memory_profile", exc_info=True)
        return result


def trigger_memory_cleanup() -> Dict[str, Any]:
    """Manually trigger memory cleanup and garbage collection."""
    global memory_profiler, memory_aware_manager, lazy_content_manager

    if memory_profiler is None:
        return {
            "error": "Memory profiler not initialized. Please set a project path first.",
            "success": False,
        }

    try:
        # Get stats before cleanup
        stats_before = lazy_content_manager.get_memory_stats()

        # Trigger cleanup through memory aware manager if available
        if memory_aware_manager:
            memory_aware_manager.cleanup()
        else:
            # Fallback to direct cleanup
            lazy_content_manager.unload_all()

        # Force garbage collection
        import gc

        collected = gc.collect()

        # Get stats after cleanup
        stats_after = lazy_content_manager.get_memory_stats()

        # Take new memory snapshot
        snapshot = memory_profiler.take_snapshot(
            loaded_files=stats_after["loaded_files"],
            cached_queries=stats_after["query_cache_size"],
        )

        return {
            "success": True,
            "cleanup_results": {
                "gc_objects_collected": collected,
                "before_cleanup": stats_before,
                "after_cleanup": stats_after,
                "memory_freed_mb": max(
                    0,
                    stats_before.get("total_managed_files", 0)
                    - stats_after.get("total_managed_files", 0),
                ),
            },
            "current_memory_mb": snapshot.process_memory_mb,
            "peak_memory_mb": snapshot.peak_memory_mb,
        }
    except Exception as e:
        return {"error": f"Error during memory cleanup: {e}", "success": False}


def configure_memory_limits(
    soft_limit_mb: Optional[float] = None,
    hard_limit_mb: Optional[float] = None,
    max_loaded_files: Optional[int] = None,
    max_cached_queries: Optional[int] = None,
) -> Dict[str, Any]:
    """Update memory limits configuration."""
    global memory_profiler

    if memory_profiler is None:
        return {
            "error": "Memory profiler not initialized. Please set a project path first.",
            "success": False,
        }

    try:
        # Update limits if provided
        limits = memory_profiler.limits
        old_limits = {
            "soft_limit_mb": limits.soft_limit_mb,
            "hard_limit_mb": limits.hard_limit_mb,
            "max_loaded_files": limits.max_loaded_files,
            "max_cached_queries": limits.max_cached_queries,
        }

        if soft_limit_mb is not None:
            limits.soft_limit_mb = soft_limit_mb
        if hard_limit_mb is not None:
            limits.hard_limit_mb = hard_limit_mb
        if max_loaded_files is not None:
            limits.max_loaded_files = max_loaded_files
        if max_cached_queries is not None:
            limits.max_cached_queries = max_cached_queries

        new_limits = {
            "soft_limit_mb": limits.soft_limit_mb,
            "hard_limit_mb": limits.hard_limit_mb,
            "max_loaded_files": limits.max_loaded_files,
            "max_cached_queries": limits.max_cached_queries,
        }

        return {
            "success": True,
            "old_limits": old_limits,
            "new_limits": new_limits,
            "message": "Memory limits updated successfully",
        }
    except Exception as e:
        return {"error": f"Error updating memory limits: {e}", "success": False}


def export_memory_profile(file_path: Optional[str] = None) -> Dict[str, Any]:
    """Export detailed memory profile to a file."""
    global memory_profiler

    if memory_profiler is None:
        return {
            "error": "Memory profiler not initialized. Please set a project path first.",
            "success": False,
        }

    try:
        import tempfile
        import os

        # Use provided path or generate a default one
        if file_path is None:
            timestamp = int(time.time())
            file_path = os.path.join(
                tempfile.gettempdir(), f"memory_profile_{timestamp}.json"
            )

        # Export profile
        memory_profiler.export_profile(file_path)

        return {
            "success": True,
            "file_path": file_path,
            "message": f"Memory profile exported to {file_path}",
        }
    except Exception as e:
        return {"error": f"Error exporting memory profile: {e}", "success": False}


def get_performance_metrics() -> Dict[str, Any]:
    """Get comprehensive performance monitoring metrics and statistics."""
    global performance_monitor

    if performance_monitor is None:
        return {
            "error": "Performance monitor not initialized. Please set a project path first.",
            "initialized": False,
        }

    try:
        # Get all performance metrics
        metrics = performance_monitor.get_metrics_summary()

        # Get operation statistics
        operation_stats = performance_monitor.get_operation_stats()

        # Get structured logs (last 100 entries) - note: this method doesn't exist, will use empty list
        logs = []  # TODO: implement log retrieval if needed

        return {
            "initialized": True,
            "metrics": metrics,
            "operation_stats": operation_stats,
            "recent_logs": logs,
            "monitoring_enabled": True,
        }
    except Exception as e:
        return {"error": f"Error getting performance metrics: {e}", "initialized": True}


def export_performance_metrics(file_path: Optional[str] = None) -> Dict[str, Any]:
    """Export performance metrics to a JSON file."""
    global performance_monitor

    if performance_monitor is None:
        return {
            "error": "Performance monitor not initialized. Please set a project path first.",
            "success": False,
        }

    try:
        import tempfile
        import os

        # Use provided path or generate a default one
        if file_path is None:
            timestamp = int(time.time())
            file_path = os.path.join(
                tempfile.gettempdir(), f"performance_metrics_{timestamp}.json"
            )

        # Export metrics
        performance_monitor.export_metrics_json(file_path)

        return {
            "success": True,
            "file_path": file_path,
            "message": f"Performance metrics exported to {file_path}",
        }
    except Exception as e:
        return {"error": f"Error exporting performance metrics: {e}", "success": False}


# ----- PROGRESS TRACKING TOOLS -----


def get_active_operations() -> Dict[str, Any]:
    """Get status of all active operations with progress tracking."""
    try:
        active_ops = progress_manager.get_active_operations()
        all_ops = progress_manager.get_all_operations_status()

        return {
            "success": True,
            "active_operations": active_ops,
            "total_operations": len(all_ops),
            "active_count": len(active_ops),
        }
    except Exception as e:
        return {"error": f"Error getting active operations: {e}", "success": False}


def get_operation_status(operation_id: str) -> Dict[str, Any]:
    """Get detailed status of a specific operation."""
    try:
        tracker = progress_manager.get_tracker(operation_id)
        if not tracker:
            return {"error": f"Operation {operation_id} not found", "success": False}

        status = tracker.get_status()
        return {"success": True, "operation_status": status}
    except Exception as e:
        return {"error": f"Error getting operation status: {e}", "success": False}


async def cancel_operation(
    operation_id: str, reason: str = "Operation cancelled by user"
) -> Dict[str, Any]:
    """Cancel a specific operation."""
    try:
        success = await progress_manager.cancel_operation(operation_id, reason)
        if success:
            return {
                "success": True,
                "message": f"Operation {operation_id} cancelled successfully",
                "operation_id": operation_id,
                "reason": reason,
            }
        else:
            return {
                "error": f"Operation {operation_id} not found or already completed",
                "success": False,
            }
    except Exception as e:
        return {"error": f"Error cancelling operation: {e}", "success": False}


async def cancel_all_operations(
    reason: str = "All operations cancelled by user",
) -> Dict[str, Any]:
    """Cancel all active operations."""
    try:
        active_ops_before = progress_manager.get_active_operations()
        await progress_manager.cancel_all_operations(reason)

        return {
            "success": True,
            "message": "All operations cancelled successfully",
            "operations_cancelled": len(active_ops_before),
            "reason": reason,
        }
    except Exception as e:
        return {"error": f"Error cancelling all operations: {e}", "success": False}


def cleanup_completed_operations(max_age_hours: float = 1.0) -> Dict[str, Any]:
    """Clean up completed operations older than specified hours."""
    try:
        max_age_seconds = max_age_hours * 3600
        ops_before = len(progress_manager.get_all_operations_status())

        progress_manager.cleanup_completed_operations(max_age_seconds)

        ops_after = len(progress_manager.get_all_operations_status())
        cleaned_up = ops_before - ops_after

        return {
            "success": True,
            "message": f"Cleaned up {cleaned_up} completed operations",
            "operations_before": ops_before,
            "operations_after": ops_after,
            "operations_cleaned": cleaned_up,
            "max_age_hours": max_age_hours,
        }
    except Exception as e:
        return {"error": f"Error cleaning up operations: {e}", "success": False}


def analyze_file_with_smart_reader(
    file_path: str,
    ctx: Context,
    include_content: bool = True,
    include_metadata: bool = True,
    include_errors: bool = True,
    include_chunks: bool = False,
    chunk_size: int = 4 * 1024 * 1024,
) -> Dict[str, Any]:
    """
    Analyze a file using the SmartFileReader with comprehensive capabilities.

    This tool provides direct access to the SmartFileReader's advanced features:
    - Intelligent reading strategy selection based on file characteristics
    - Error detection and reporting
    - Memory-efficient chunked reading for large files
    - Comprehensive file metadata extraction
    - File health analysis and corruption detection

    Args:
        file_path: Path to the file to analyze (relative to project root)
        include_content: Whether to include file content in the response
        include_metadata: Whether to include file metadata
        include_errors: Whether to include error detection results
        include_chunks: Whether to read file in chunks (for very large files)
        chunk_size: Size of chunks when reading in chunks (default: 4MB)

    Returns:
        Comprehensive file analysis including content, metadata, errors, and reading strategy information
    """
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Normalize the file path and ensure it's relative to base_path
    norm_path = os.path.normpath(file_path)
    if norm_path.startswith(".."):
        return {"error": f"Invalid file path: {file_path}"}

    # Ensure the path is relative to base_path
    if os.path.isabs(norm_path):
        # If absolute path is provided, make it relative to base_path
        try:
            norm_path = os.path.relpath(norm_path, base_path)
        except ValueError:
            return {"error": f"File path is not within project directory: {file_path}"}

    full_path = os.path.join(base_path, norm_path)

    # Check if file exists
    if not os.path.exists(full_path):
        return {"error": f"File not found: {file_path}"}

    try:
        # Initialize SmartFileReader
        smart_reader = SmartFileReader(base_path)

        # Build response with requested components
        result = {
            "file_path": norm_path,
            "full_path": full_path,
            "exists": True,
            "file_size": os.path.getsize(full_path),
            "last_modified": os.path.getmtime(full_path),
        }

        # Get file information (strategy selection, etc.)
        file_info = smart_reader.get_file_info(full_path)
        if file_info:
            result["file_info"] = {
                "strategy_used": str(file_info.get("strategy_used", "unknown")),
                "file_size_category": str(
                    file_info.get("file_size_category", "unknown")
                ),
                "is_binary": file_info.get("is_binary", False),
                "encoding": file_info.get("encoding", "unknown"),
                "estimated_read_time_ms": file_info.get("estimated_read_time_ms", 0),
                "memory_efficiency_score": file_info.get(
                    "memory_efficiency_score", 0.0
                ),
            }

        # Include content if requested
        if include_content:
            if include_chunks:
                # Read in chunks for memory efficiency
                chunks = []
                for chunk in smart_reader.read_in_chunks(full_path, chunk_size):
                    chunks.append(chunk)
                result["content_chunks"] = chunks
                result["total_chunks"] = len(chunks)
            else:
                # Read entire content
                content = smart_reader.read_content(full_path)
                if content is not None:
                    result["content"] = content
                    result["content_length"] = len(content)
                else:
                    result["content_error"] = "Unable to read file content"

        # Include metadata if requested
        if include_metadata:
            metadata = smart_reader.read_metadata(full_path)
            if metadata:
                result["metadata"] = metadata

        # Include error detection if requested
        if include_errors:
            errors = smart_reader.detect_errors(full_path)
            if errors:
                result["errors"] = errors

        # Add file extension for context
        _, ext = os.path.splitext(norm_path)
        result["extension"] = ext

        return result

    except Exception as e:
        logger.error(
            f"Error analyzing file {full_path} with SmartFileReader: {e}", exc_info=True
        )
        return {"error": f"Error analyzing file: {e}"}


def read_file_chunks(
    file_path: str,
    ctx: Context,
    chunk_size: int = 4 * 1024 * 1024,
    max_chunks: Optional[int] = None,
) -> Dict[str, Any]:
    """
    Read a large file in chunks using SmartFileReader for memory efficiency.

    This tool is specifically designed for handling very large files that cannot
    be loaded entirely into memory. It uses the SmartFileReader's chunked reading
    capabilities with automatic strategy selection.

    Args:
        file_path: Path to the file to read (relative to project root)
        chunk_size: Size of each chunk in bytes (default: 4MB)
        max_chunks: Maximum number of chunks to return (None for all chunks)

    Returns:
        File chunks with metadata about the reading process
    """
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Normalize the file path
    norm_path = os.path.normpath(file_path)
    if norm_path.startswith(".."):
        return {"error": f"Invalid file path: {file_path}"}

    if os.path.isabs(norm_path):
        try:
            norm_path = os.path.relpath(norm_path, base_path)
        except ValueError:
            return {"error": f"File path is not within project directory: {file_path}"}

    full_path = os.path.join(base_path, norm_path)

    # Check if file exists
    if not os.path.exists(full_path):
        return {"error": f"File not found: {file_path}"}

    try:
        # Initialize SmartFileReader
        smart_reader = SmartFileReader(base_path)

        # Get file information first
        file_info = smart_reader.get_file_info(full_path)

        # Read chunks
        chunks = []
        chunk_count = 0

        for chunk in smart_reader.read_in_chunks(full_path, chunk_size):
            chunks.append(chunk)
            chunk_count += 1

            # Stop if we've reached the maximum number of chunks
            if max_chunks and chunk_count >= max_chunks:
                break

        # Calculate total size from chunks
        total_size = sum(len(chunk) for chunk in chunks)

        result = {
            "file_path": norm_path,
            "full_path": full_path,
            "file_size": os.path.getsize(full_path),
            "chunk_size": chunk_size,
            "chunks_read": chunk_count,
            "total_size_read": total_size,
            "is_complete": chunk_count * chunk_size >= os.path.getsize(full_path),
        }

        # Include file info if available
        if file_info:
            result["file_info"] = {
                "strategy_used": str(file_info.get("strategy_used", "unknown")),
                "file_size_category": str(
                    file_info.get("file_size_category", "unknown")
                ),
                "is_binary": file_info.get("is_binary", False),
                "encoding": file_info.get("encoding", "unknown"),
            }

        # Include chunks in response
        result["chunks"] = chunks

        return result

    except Exception as e:
        logger.error(f"Error reading file chunks for {full_path}: {e}", exc_info=True)
        return {"error": f"Error reading file chunks: {e}"}


def detect_file_errors(file_path: str, ctx: Context) -> Dict[str, Any]:
    """
    Detect and analyze errors in a file using SmartFileReader's error detection capabilities.

    This tool provides detailed error analysis including:
    - Syntax errors in code files
    - Malformed content detection
    - Encoding issues
    - File corruption detection
    - Line-by-line error reporting

    Args:
        file_path: Path to the file to analyze (relative to project root)

    Returns:
        Comprehensive error analysis with detailed error information
    """
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Normalize the file path
    norm_path = os.path.normpath(file_path)
    if norm_path.startswith(".."):
        return {"error": f"Invalid file path: {file_path}"}

    if os.path.isabs(norm_path):
        try:
            norm_path = os.path.relpath(norm_path, base_path)
        except ValueError:
            return {"error": f"File path is not within project directory: {file_path}"}

    full_path = os.path.join(base_path, norm_path)

    # Check if file exists
    if not os.path.exists(full_path):
        return {"error": f"File not found: {file_path}"}

    try:
        # Initialize SmartFileReader
        smart_reader = SmartFileReader(base_path)

        # Detect errors
        errors = smart_reader.detect_errors(full_path)

        result = {
            "file_path": norm_path,
            "full_path": full_path,
            "file_exists": True,
            "file_size": os.path.getsize(full_path),
        }

        # Add file extension for context
        _, ext = os.path.splitext(norm_path)
        result["extension"] = ext

        # Include error analysis
        if errors:
            result["error_analysis"] = errors
            result["has_errors"] = errors.get("has_errors", False)
            result["error_count"] = len(errors.get("errors", []))
            result["error_types"] = list(
                set(
                    error.get("error_type", "unknown")
                    for error in errors.get("errors", [])
                )
            )

            # Provide summary
            if result["has_errors"]:
                result["summary"] = (
                    f"Found {result['error_count']} errors of types: {', '.join(result['error_types'])}"
                )
            else:
                result["summary"] = "No errors detected in file"
        else:
            result["error_analysis"] = None
            result["has_errors"] = False
            result["error_count"] = 0
            result["error_types"] = []
            result["summary"] = "No error analysis available"

        return result

    except Exception as e:
        logger.error(f"Error detecting errors in file {full_path}: {e}", exc_info=True)
        return {"error": f"Error detecting file errors: {e}"}


def get_file_metadata(file_path: str, ctx: Context) -> Dict[str, Any]:
    """
    Get comprehensive metadata for a file using SmartFileReader.

    This tool provides detailed file metadata including:
    - File system metadata (timestamps, permissions, ownership)
    - File characteristics (size, encoding, type)
    - Reading strategy information
    - Memory efficiency metrics

    Args:
        file_path: Path to the file to analyze (relative to project root)

    Returns:
        Comprehensive file metadata
    """
    base_path = ctx.request_context.lifespan_context.base_path

    # Check if base_path is set
    if not base_path:
        return {
            "error": "Project path not set. Please use set_project_path to set a project directory first."
        }

    # Normalize the file path
    norm_path = os.path.normpath(file_path)
    if norm_path.startswith(".."):
        return {"error": f"Invalid file path: {file_path}"}

    if os.path.isabs(norm_path):
        try:
            norm_path = os.path.relpath(norm_path, base_path)
        except ValueError:
            return {"error": f"File path is not within project directory: {file_path}"}

    full_path = os.path.join(base_path, norm_path)

    # Check if file exists
    if not os.path.exists(full_path):
        return {"error": f"File not found: {file_path}"}

    try:
        # Initialize SmartFileReader
        smart_reader = SmartFileReader(base_path)

        # Get metadata
        metadata = smart_reader.read_metadata(full_path)

        # Get file information
        file_info = smart_reader.get_file_info(full_path)

        result = {
            "file_path": norm_path,
            "full_path": full_path,
            "exists": True,
        }

        # Add file extension
        _, ext = os.path.splitext(norm_path)
        result["extension"] = ext

        # Include metadata if available
        if metadata:
            result["metadata"] = metadata
        else:
            result["metadata"] = None

        # Include file information if available
        if file_info:
            result["file_info"] = {
                "strategy_used": str(file_info.get("strategy_used", "unknown")),
                "file_size_category": str(
                    file_info.get("file_size_category", "unknown")
                ),
                "is_binary": file_info.get("is_binary", False),
                "encoding": file_info.get("encoding", "unknown"),
                "estimated_read_time_ms": file_info.get("estimated_read_time_ms", 0),
                "memory_efficiency_score": file_info.get(
                    "memory_efficiency_score", 0.0
                ),
            }

        # Add basic file stats
        stat_info = os.stat(full_path)
        result["basic_stats"] = {
            "size_bytes": stat_info.st_size,
            "modified_time": stat_info.st_mtime,
            "created_time": stat_info.st_ctime,
            "accessed_time": stat_info.st_atime,
            "is_regular_file": os.path.isfile(full_path),
            "is_directory": os.path.isdir(full_path),
            "is_symlink": os.path.islink(full_path),
        }

        return result

    except Exception as e:
        logger.error(f"Error getting metadata for file {full_path}: {e}", exc_info=True)
        return {"error": f"Error getting file metadata: {e}"}

        # ----- PROGRESS TRACKING TOOLS -----

        ops_after = len(progress_manager.get_all_operations_status())
        cleaned_up = ops_before - ops_after

        return {
            "success": True,
            "message": f"Cleaned up {cleaned_up} completed operations",
            "operations_before": ops_before,
            "operations_after": ops_after,
            "operations_cleaned": cleaned_up,
            "max_age_hours": max_age_hours,
        }
    except Exception as e:
        return {"error": f"Error cleaning up operations: {e}", "success": False}


# ----- PROMPTS -----


@mcp.prompt()
def analyze_code(file_path: str = "", query: str = "") -> list[types.PromptMessage]:
    """Prompt for analyzing code in the project."""
    messages = [
        types.PromptMessage(
            role="user",
            content=types.TextContent(
                type="text",
                text=f"""I need you to analyze some code from my project.

{f"Please analyze the file: {file_path}" if file_path else ""}
{f"I want to understand: {query}" if query else ""}

First, let me give you some context about the project structure. Then, I'll provide the code to analyze.
""",
            ),
        ),
        types.PromptMessage(
            role="assistant",
            content=types.TextContent(
                type="text",
                text="I'll help you analyze the code. Let me first examine the project structure to get a better understanding of the codebase.",
            ),
        ),
    ]
    return messages


@mcp.prompt()
def code_search(query: str = "") -> types.TextContent:
    """Prompt for searching code in the project."""
    search_text = '"query"' if not query else f'"{query}"'
    return types.TextContent(
        type="text",
        text=f"""I need to search through my codebase for {search_text}.

Please help me find all occurrences of this query and explain what each match means in its context.
Focus on the most relevant files and provide a brief explanation of how each match is used in the code.

If there are too many results, prioritize the most important ones and summarize the patterns you see.""",
    )


@mcp.prompt()
def set_project() -> list[types.PromptMessage]:
    """Prompt for setting the project path."""
    messages = [
        types.PromptMessage(
            role="user",
            content=types.TextContent(
                type="text",
                text="""
        I need to analyze code from a project, but I haven't set the project path yet. Please help me set up the project path and index the code.

        First, I need to specify which project directory to analyze.
        """,
            ),
        ),
        types.PromptMessage(
            role="assistant",
            content=types.TextContent(
                type="text",
                text="""
        Before I can help you analyze any code, we need to set up the project path. This is a required first step.

        Please provide the full path to your project folder. For example:
        - Windows: "C:/Users/username/projects/my-project"
        - macOS/Linux: "/home/username/projects/my-project"

        Once you provide the path, I'll use the `set_project_path` tool to configure the code analyzer to work with your project.
        """,
            ),
        ),
    ]
    return messages


# ----- HELPER FUNCTIONS -----


def _safe_clear_file_index():
    """Safely clear the file_index regardless of its type."""
    global file_index

    # Always reset to empty dictionary to ensure compatibility
    file_index = {}


async def _index_project_with_progress(
    base_path: str,
    progress_tracker: ProgressTracker,
    core_engine: Optional[CoreEngine] = None,
) -> int:
    """
    Create an index of the project files with progress tracking and cancellation support.
    Returns the number of files indexed.
    """
    global performance_monitor

    # Start timing the indexing operation
    indexing_context = None
    if performance_monitor:
        indexing_context = performance_monitor.time_operation(
            "indexing",
            base_path=base_path,
            operation_type="incremental_index_with_progress",
        )
        indexing_context.__enter__()
        performance_monitor.log_structured(
            "info",
            "Starting project indexing with progress tracking",
            base_path=base_path,
        )

    file_count = 0
    filtered_files = 0
    filtered_dirs = 0
    _safe_clear_file_index()

    try:
        # Initialize configuration manager for filtering
        config_manager = ConfigManager()

        # Initialize ignore pattern matcher
        ignore_matcher = IgnorePatternMatcher(base_path)

        # Initialize incremental indexer
        settings = OptimizedProjectSettings(base_path)
        indexer = IncrementalIndexer(settings)

        # Update progress
        await progress_tracker.update_progress(
            message="Initialized indexing components"
        )
        progress_tracker.cancellation_token.check_cancelled()

        # Get pattern information for debugging
        pattern_info = ignore_matcher.get_pattern_sources()
        logger.info(f"Ignore patterns loaded: {pattern_info}")

        # Get filtering configuration
        filtering_stats = config_manager.get_filtering_stats()
        logger.info(f"Filtering configuration: {filtering_stats}")

        should_log = config_manager.should_log_filtering_decisions()

        # Gather current file list with progress updates
        current_file_list = []
        scanned_files = 0

        await progress_tracker.update_progress(message="Scanning project directory...")

        for root, dirs, files in os.walk(base_path):
            # Check for cancellation periodically
            if scanned_files % 50 == 0:
                progress_tracker.cancellation_token.check_cancelled()
                await progress_tracker.update_progress(
                    message=f"Scanned {scanned_files} files so far..."
                )

            # Create relative path from base_path
            rel_path = os.path.relpath(root, base_path)

            # Skip the current directory if it should be ignored by pattern matcher
            if rel_path != "." and ignore_matcher.should_ignore_directory(rel_path):
                logger.debug(f"Skipping directory '{rel_path}' due to ignore pattern.")
                dirs[:] = []  # Don't recurse into subdirectories
                filtered_dirs += 1
                continue

            # Check if directory should be skipped due to size/count filtering
            if rel_path != "." and config_manager.should_skip_directory_by_pattern(
                rel_path
            ):
                if should_log:
                    logger.debug(f"Skipping directory by pattern: {rel_path}")
                dirs[:] = []  # Don't recurse into subdirectories
                filtered_dirs += 1
                continue

            # Count files and subdirectories for directory filtering
            visible_files = []
            for file in files:
                scanned_files += 1

                # Skip hidden files and files with unsupported extensions
                _, ext = os.path.splitext(file)
                if file.startswith("."):
                    continue
                if ext not in supported_extensions:
                    logger.debug(
                        f"Skipping file with unsupported extension: '{os.path.join(rel_path, file)}' (extension: '{ext}')"
                    )
                    continue

                file_path = os.path.join(rel_path, file).replace("\\", "/")
                if rel_path == ".":
                    file_path = file

                # Check if file should be ignored by pattern matcher
                if ignore_matcher.should_ignore(file_path):
                    logger.debug(f"Skipping file '{file_path}' due to ignore pattern.")
                    continue

                # Check file size
                full_file_path = os.path.join(root, file)
                try:
                    file_size = os.path.getsize(full_file_path)
                    if config_manager.should_skip_file_by_size(file_path, file_size):
                        if should_log:
                            logger.debug(
                                f"Skipping large file: {file_path} ({file_size} bytes)"
                            )
                        filtered_files += 1
                        continue
                except (OSError, IOError) as e:
                    logger.exception(f"Error getting file size for {file_path}: {e}")
                    filtered_files += 1  # Count as filtered due to error
                    continue

                visible_files.append((file, file_path, ext))

            visible_dirs = [
                d
                for d in dirs
                if not ignore_matcher.should_ignore_directory(
                    os.path.join(rel_path, d) if rel_path != "." else d
                )
            ]

            # Apply directory count filtering
            if config_manager.should_skip_directory_by_count(
                rel_path, len(visible_files), len(visible_dirs)
            ):
                if should_log:
                    logger.debug(
                        f"Skipping directory by count: {rel_path} ({len(visible_files)} files, {len(visible_dirs)} subdirs)"
                    )
                dirs[:] = []  # Don't recurse into subdirectories
                filtered_dirs += 1
                continue

            # Filter directories using the ignore pattern matcher
            dirs[:] = visible_dirs

            # Add files to current file list for incremental indexing
            for file, file_path, ext in visible_files:
                current_file_list.append(file_path)

        # Update progress tracker with actual file count
        progress_tracker.total_items = max(len(current_file_list), 1)

        await progress_tracker.update_progress(
            message=f"Identified {len(current_file_list)} files to process"
        )
        progress_tracker.cancellation_token.check_cancelled()

        # Identify changed files using incremental indexer
        added_files, modified_files, deleted_files = indexer.get_changed_files(
            base_path, current_file_list
        )

        # Clean up deleted files metadata
        indexer.clean_deleted_files(deleted_files)

        logger.info(
            f"Incremental indexing: Added: {len(added_files)}, Modified: {len(modified_files)}, Deleted: {len(deleted_files)}"
        )

        await progress_tracker.update_progress(
            message=f"Incremental analysis: {len(added_files)} added, {len(modified_files)} modified, {len(deleted_files)} deleted"
        )
        progress_tracker.cancellation_token.check_cancelled()

        # Only process changed files (added + modified) for efficiency
        changed_files = added_files + modified_files
        if not changed_files and not deleted_files:
            logger.info("No changes detected, using existing index")
            # Count existing files in the metadata
            file_count = len(indexer.file_metadata)
            await progress_tracker.update_progress(
                message=f"No changes detected, index is up to date with {file_count} files"
            )
            return file_count

        # Use parallel processing for chunked indexing of changed files
        if changed_files:
            logger.info(
                f"Processing {len(changed_files)} changed files using parallel indexing..."
            )

            await progress_tracker.update_progress(
                message=f"Processing {len(changed_files)} changed files..."
            )

            # Create indexing tasks for changed files
            indexing_tasks = []
            for file_path in changed_files:
                progress_tracker.cancellation_token.check_cancelled()

                full_file_path = os.path.join(base_path, file_path)

                # Skip if file doesn't exist (might have been deleted)
                if not os.path.exists(full_file_path):
                    logger.debug(f"Skipping indexing of non-existent file: {file_path}")
                    continue

                # Get file info
                _, ext = os.path.splitext(file_path)
                task = IndexingTask(
                    directory_path=base_path,
                    files=[file_path],
                    task_id=file_path,
                    metadata={"extension": ext},
                )
                indexing_tasks.append(task)

            # Process tasks using parallel indexer
            if indexing_tasks:
                parallel_indexer = ParallelIndexer()

                # Process files in parallel chunks with progress updates
                try:
                    # Run the parallel processing with progress callback
                    async def progress_callback(completed: int, total: int):
                        progress_tracker.cancellation_token.check_cancelled()
                        progress_percent = (completed / total) * 100 if total > 0 else 0
                        await progress_tracker.update_progress(
                            items_processed=completed
                            - progress_tracker.items_processed,
                            message=f"Processed {completed}/{total} files ({progress_percent:.1f}%)",
                        )

                    # Run the parallel processing
                    results = await parallel_indexer.process_files(indexing_tasks)

                    progress_tracker.cancellation_token.check_cancelled()

                    # Merge results into file_index
                    for result in results:
                        progress_tracker.cancellation_token.check_cancelled()

                        if result.success:
                            # Process each indexed file in the result
                            for file_info in result.indexed_files:
                                file_path = file_info["path"]

                                # Navigate to the correct directory in the index
                                current_dir = file_index
                                rel_path = os.path.dirname(file_path)

                                # Skip the '.' directory (base_path itself)
                                if rel_path and rel_path != ".":
                                    # Split the path and navigate/create the tree
                                    path_parts = rel_path.replace("\\", "/").split("/")
                                    for part in path_parts:
                                        if part not in current_dir:
                                            current_dir[part] = {}
                                        current_dir = current_dir[part]

                                # Add file to index
                                filename = os.path.basename(file_path)
                                current_dir[filename] = {
                                    "type": "file",
                                    "path": file_path,
                                    "ext": file_info.get("extension", ""),
                                }
                                file_count += 1

                                # Update file metadata
                                full_file_path = os.path.join(base_path, file_path)
                                indexer.update_file_metadata(file_path, full_file_path)

                                # Index content into Elasticsearch
                                if dal_instance and dal_instance.search:
                                    try:
                                        # Use SmartFileReader for enhanced content loading with better error handling
                                        smart_reader = SmartFileReader(base_path)
                                        content = smart_reader.read_content(
                                            full_file_path
                                        )

                                        if content:
                                            logger.debug(
                                                f"Received content for {full_file_path}, length: {len(content)} bytes."
                                            )
                                            doc_id = (
                                                file_path  # Use file_path as doc_id
                                            )
                                            document = {
                                                "file_id": doc_id,
                                                "path": file_path,
                                                "content": content,
                                                "language": file_info.get(
                                                    "extension", ""
                                                ).lstrip("."),
                                                "last_modified": datetime.fromtimestamp(
                                                    os.path.getmtime(full_file_path)
                                                ).isoformat(),
                                                "size": os.path.getsize(full_file_path),
                                                "checksum": indexer.get_file_hash(
                                                    full_file_path
                                                ),
                                            }
                                            logger.debug(
                                                f"Calling dal_instance.search.index_document for {file_path}"
                                            )
                                            response = (
                                                dal_instance.search.index_document(
                                                    doc_id, document
                                                )
                                            )
                                            if response:
                                                logger.debug(
                                                    f"Indexed {file_path} into Elasticsearch. Response: {response}"
                                                )
                                            else:
                                                logger.error(
                                                    f"Failed to index {file_path} into Elasticsearch. Indexing method returned False."
                                                )
                                        else:
                                            logger.warning(
                                                f"SmartFileReader returned None content for {full_file_path}, skipping Elasticsearch indexing."
                                            )
                                    except Exception as es_e:
                                        logger.exception(
                                            f"Error indexing {file_path} into Elasticsearch: {es_e}"
                                        )

                                # Index into Core Engine
                                if core_engine:
                                    try:
                                        # Ensure content is read if not already
                                        if "content" not in locals() or content is None:
                                            smart_reader = SmartFileReader(base_path)
                                            content = smart_reader.read_content(
                                                full_file_path
                                            )

                                        if content:
                                            await core_engine.index_file(
                                                base_path, file_path, content
                                            )
                                    except Exception as core_e:
                                        logger.error(
                                            f"Error indexing {file_path} into Core Engine: {core_e}"
                                        )

                    logger.info(
                        f"Parallel indexing completed: {file_count} files processed"
                    )
                except Exception as e:
                    logger.exception(f"Error in parallel processing: {e}")
                    # Fall back to sequential processing
                    await progress_tracker.update_progress(
                        message="Parallel processing failed, falling back to sequential..."
                    )

                    # Sequential fallback (processing only changed files)
                    processed_files = 0
                    for file_path in changed_files:
                        progress_tracker.cancellation_token.check_cancelled()

                        full_file_path = os.path.join(base_path, file_path)

                        # Skip if file doesn't exist
                        if not os.path.exists(full_file_path):
                            logger.debug(
                                f"Skipping sequential indexing of non-existent file: {file_path}"
                            )
                            continue

                        # Navigate to the correct directory in the index
                        current_dir = file_index
                        rel_path = os.path.dirname(file_path)

                        # Skip the '.' directory (base_path itself)
                        if rel_path and rel_path != ".":
                            # Split the path and navigate/create the tree
                            path_parts = rel_path.replace("\\", "/").split("/")
                            for part in path_parts:
                                if part not in current_dir:
                                    current_dir[part] = {}
                                current_dir = current_dir[part]

                        # Add file to index
                        filename = os.path.basename(file_path)
                        _, ext = os.path.splitext(file_path)
                        current_dir[filename] = {
                            "type": "file",
                            "path": file_path,
                            "ext": ext,
                        }
                        file_count += 1
                        processed_files += 1

                        # Update file metadata
                        indexer.update_file_metadata(file_path, full_file_path)

                        # Index content into Elasticsearch (sequential fallback)
                        if dal_instance and dal_instance.search:
                            try:
                                # Use SmartFileReader for enhanced content loading with better error handling
                                smart_reader = SmartFileReader(base_path)
                                content = smart_reader.read_content(full_file_path)
                                if content is not None:
                                    logger.debug(
                                        f"Received content for {full_file_path} (sequential), length: {len(content)} bytes."
                                    )
                                    doc_id = file_path  # Use file_path as doc_id
                                    document = {
                                        "file_id": doc_id,
                                        "path": file_path,
                                        "content": content,
                                        "language": ext.lstrip("."),
                                        "last_modified": datetime.fromtimestamp(
                                            os.path.getmtime(full_file_path)
                                        ).isoformat(),
                                        "size": os.path.getsize(full_file_path),
                                        "checksum": indexer.get_file_hash(
                                            full_file_path
                                        ),
                                    }
                                    logger.debug(
                                        f"Calling dal_instance.search.index_document for {file_path} (sequential)"
                                    )
                                    response = dal_instance.search.index_document(
                                        doc_id, document
                                    )
                                    if response:
                                        logger.debug(
                                            f"Indexed {file_path} into Elasticsearch (sequential). Response: {response}"
                                        )
                                    else:
                                        logger.error(
                                            f"Failed to index {file_path} into Elasticsearch (sequential). Indexing method returned False."
                                        )
                                else:
                                    logger.warning(
                                        f"SmartFileReader returned None content for {full_file_path} (sequential), skipping Elasticsearch indexing."
                                    )
                            except Exception as es_e:
                                logger.exception(
                                    f"Error indexing {file_path} into Elasticsearch (sequential): {es_e}"
                                )

                        # Index into Core Engine (Sequential)
                        if core_engine:
                            try:
                                # Ensure content is read if not already
                                if "content" not in locals() or content is None:
                                    smart_reader = SmartFileReader(base_path)
                                    content = smart_reader.read_content(full_file_path)

                                if content:
                                    await core_engine.index_file(
                                        base_path, file_path, content
                                    )
                            except Exception as core_e:
                                logger.error(
                                    f"Error indexing {file_path} into Core Engine (sequential): {core_e}"
                                )

                        # Update progress periodically
                        if processed_files % 10 == 0:
                            progress_percent = (
                                processed_files / len(changed_files)
                            ) * 100
                            await progress_tracker.update_progress(
                                items_processed=1,
                                message=f"Sequential processing: {processed_files}/{len(changed_files)} files ({progress_percent:.1f}%)",
                            )
            else:  # This else is for 'if indexing_tasks:'
                logger.info("No files to process in parallel, using existing index")
                await progress_tracker.update_progress(message="No files to process")

        # Save updated metadata
        await progress_tracker.update_progress(message="Saving metadata...")
        indexer.save_metadata()

        # Complete performance monitoring
        if performance_monitor and indexing_context:
            try:
                # Update operation metadata with results
                indexing_context.metadata.update(
                    {
                        "files_indexed": file_count,
                        "files_filtered": filtered_files,
                        "directories_filtered": filtered_dirs,
                        "added_files": len(added_files)
                        if "added_files" in locals()
                        else 0,
                        "modified_files": len(modified_files)
                        if "modified_files" in locals()
                        else 0,
                        "deleted_files": len(deleted_files)
                        if "deleted_files" in locals()
                        else 0,
                    }
                )

                # Exit the timing context
                indexing_context.__exit__(None, None, None)

                # Log completion
                performance_monitor.log_structured(
                    "info",
                    "Project indexing with progress completed successfully",
                    base_path=base_path,
                    files_indexed=file_count,
                    files_filtered=filtered_files,
                    directories_filtered=filtered_dirs,
                    duration_ms=getattr(indexing_context, "duration_ms", 0),
                )

                # Increment success counter
                performance_monitor.increment_counter("indexing_operations_total")

            except Exception as e:
                # Log indexing error
                performance_monitor.log_structured(
                    "error",
                    "Error during indexing performance monitoring",
                    error=str(e),
                )
                # Still exit the context to avoid resource leaks
                if indexing_context:
                    try:
                        indexing_context.__exit__(Exception, type(e), None)
                    except:
                        pass

        await progress_tracker.update_progress(
            message=f"Indexing completed: {file_count} files indexed, {filtered_files} files filtered, {filtered_dirs} directories filtered"
        )
        logger.info(
            f"Indexing completed: {file_count} files indexed, {filtered_files} files filtered, {filtered_dirs} directories filtered"
        )
        return file_count

    except asyncio.CancelledError:
        logger.warning("Indexing operation was cancelled")
        if performance_monitor and indexing_context:
            try:
                indexing_context.metadata.update({"cancelled": True})
                indexing_context.__exit__(None, None, None)
                performance_monitor.log_structured(
                    "warning", "Indexing operation cancelled", base_path=base_path
                )
            except:
                pass
        raise
    except Exception as e:
        logger.exception(f"Error during indexing: {e}")
        if performance_monitor and indexing_context:
            try:
                indexing_context.__exit__(Exception, type(e), None)
                performance_monitor.log_structured(
                    "error",
                    "Indexing operation failed",
                    error=str(e),
                    base_path=base_path,
                )
            except:
                pass
        raise


async def _index_project(
    base_path: str, core_engine: Optional[CoreEngine] = None
) -> int:
    """
    Create an index of the project files with size and directory count filtering.
    Returns the number of files indexed.
    """
    global performance_monitor

    # Start timing the indexing operation
    indexing_context = None
    if performance_monitor:
        indexing_context = performance_monitor.time_operation(
            "indexing", base_path=base_path, operation_type="full_index"
        )
        indexing_context.__enter__()
        performance_monitor.log_structured(
            "info", "Starting project indexing", base_path=base_path
        )

    file_count = 0
    filtered_files = 0
    filtered_dirs = 0
    _safe_clear_file_index()

    # Initialize configuration manager for filtering
    config_manager = ConfigManager()

    # Initialize ignore pattern matcher
    ignore_matcher = IgnorePatternMatcher(base_path)

    # Initialize incremental indexer
    settings = OptimizedProjectSettings(base_path)
    indexer = IncrementalIndexer(settings)

    # Get pattern information for debugging
    pattern_info = ignore_matcher.get_pattern_sources()
    logger.info(f"Ignore patterns loaded: {pattern_info}")

    # Get filtering configuration
    filtering_stats = config_manager.get_filtering_stats()
    logger.info(f"Filtering configuration: {filtering_stats}")

    should_log = config_manager.should_log_filtering_decisions()

    # Gather current file list
    current_file_list = []

    for root, dirs, files in os.walk(base_path):
        # Create relative path from base_path
        rel_path = os.path.relpath(root, base_path)

        # Skip the current directory if it should be ignored by pattern matcher
        if rel_path != "." and ignore_matcher.should_ignore_directory(rel_path):
            logger.debug(f"Skipping directory '{rel_path}' due to ignore pattern.")
            filtered_dirs += 1
            continue

        # Check if directory should be skipped due to size/count filtering
        if rel_path != "." and config_manager.should_skip_directory_by_pattern(
            rel_path
        ):
            if should_log:
                logger.debug(f"Skipping directory by pattern: {rel_path}")
            dirs[:] = []  # Don't recurse into subdirectories
            filtered_dirs += 1
            continue

        # Count files and subdirectories for directory filtering
        visible_files = []
        for file in files:
            _, ext = os.path.splitext(file)
            file_path = os.path.join(rel_path, file).replace("\\", "/")
            if rel_path == ".":
                file_path = file

            if file.startswith("."):
                logger.debug(f"Skipping hidden file: '{file_path}'")
                filtered_files += 1
                continue
            if ext not in supported_extensions:
                logger.debug(
                    f"Skipping file with unsupported extension: '{file_path}' (extension: '{ext}')"
                )
                filtered_files += 1
                continue

            # Check if file should be ignored by pattern matcher
            if ignore_matcher.should_ignore(file_path):
                logger.debug(f"Skipping file '{file_path}' due to ignore pattern.")
                filtered_files += 1
                continue

            # Check file size
            full_file_path = os.path.join(root, file)
            try:
                file_size = os.path.getsize(full_file_path)
                if config_manager.should_skip_file_by_size(file_path, file_size):
                    if should_log:
                        logger.debug(
                            f"Skipping large file: {file_path} ({file_size} bytes)"
                        )
                    filtered_files += 1
                    continue
            except (OSError, IOError) as e:
                logger.exception(f"Error getting file size for {file_path}: {e}")

            visible_files.append((file, file_path, ext))

        visible_dirs = [
            d
            for d in dirs
            if not ignore_matcher.should_ignore_directory(
                os.path.join(rel_path, d) if rel_path != "." else d
            )
        ]

        # Apply directory count filtering
        if config_manager.should_skip_directory_by_count(
            rel_path, len(visible_files), len(visible_dirs)
        ):
            if should_log:
                logger.debug(
                    f"Skipping directory by count: {rel_path} ({len(visible_files)} files, {len(visible_dirs)} subdirs)"
                )
            dirs[:] = []  # Don't recurse into subdirectories
            filtered_dirs += 1
            continue

        # Filter directories using the ignore pattern matcher
        dirs[:] = visible_dirs

        # Add files to current file list for incremental indexing
        for file, file_path, ext in visible_files:
            current_file_list.append(file_path)

    # Identify changed files using incremental indexer
    added_files, modified_files, deleted_files = indexer.get_changed_files(
        base_path, current_file_list
    )

    # Clean up deleted files metadata
    indexer.clean_deleted_files(deleted_files)

    logger.info(
        f"Incremental indexing: Added: {len(added_files)}, Modified: {len(modified_files)}, Deleted: {len(deleted_files)}"
    )

    # Only process changed files (added + modified) for efficiency
    changed_files = added_files + modified_files
    if not changed_files and not deleted_files:
        logger.info("No changes detected, using existing index")
        # Count existing files in the metadata
        file_count = len(indexer.file_metadata)
        return file_count

    # Use parallel processing for chunked indexing of changed files
    if changed_files:
        logger.info(
            f"Processing {len(changed_files)} changed files using parallel indexing..."
        )

        # Create indexing tasks for changed files
        indexing_tasks = []
        for file_path in changed_files:
            full_file_path = os.path.join(base_path, file_path)

            # Skip if file doesn't exist (might have been deleted)
            if not os.path.exists(full_file_path):
                logger.debug(f"Skipping indexing of non-existent file: {file_path}")
                continue

            # Get file info
            _, ext = os.path.splitext(file_path)
            task = IndexingTask(
                directory_path=base_path,
                files=[file_path],
                task_id=file_path,
                metadata={"extension": ext},
            )
            indexing_tasks.append(task)

        # Process tasks using parallel indexer
        if indexing_tasks:
            parallel_indexer = ParallelIndexer()

            # Process files in parallel chunks
            try:
                # Run the parallel processing
                results = await parallel_indexer.process_files(indexing_tasks)

                # Merge results into file_index
                for result in results:
                    if result.success:
                        # Process each indexed file in the result
                        for file_info in result.indexed_files:
                            file_path = file_info["path"]

                            # Navigate to the correct directory in the index
                            current_dir = file_index
                            rel_path = os.path.dirname(file_path)

                            # Skip the '.' directory (base_path itself)
                            if rel_path and rel_path != ".":
                                # Split the path and navigate/create the tree
                                path_parts = rel_path.replace("\\", "/").split("/")
                                for part in path_parts:
                                    if part not in current_dir:
                                        current_dir[part] = {}
                                    current_dir = current_dir[part]

                            # Add file to index
                            filename = os.path.basename(file_path)
                            current_dir[filename] = {
                                "type": "file",
                                "path": file_path,
                                "ext": file_info.get("extension", ""),
                            }
                            file_count += 1

                            # Update file metadata
                            full_file_path = os.path.join(base_path, file_path)
                            indexer.update_file_metadata(file_path, full_file_path)
                            # Index content into Elasticsearch
                            if dal_instance and dal_instance.search:
                                try:
                                    # Use SmartFileReader for enhanced content loading with better error handling
                                    smart_reader = SmartFileReader(base_path)
                                    content = smart_reader.read_content(full_file_path)

                                    if content:
                                        logger.debug(
                                            f"Received content for {full_file_path}, length: {len(content)} bytes."
                                        )
                                        doc_id = file_path  # Use file_path as doc_id
                                        document = {
                                            "file_id": doc_id,
                                            "path": file_path,
                                            "content": content,
                                            "language": file_info.get(
                                                "extension", ""
                                            ).lstrip("."),
                                            "last_modified": datetime.fromtimestamp(
                                                os.path.getmtime(full_file_path)
                                            ).isoformat(),
                                            "size": os.path.getsize(full_file_path),
                                            "checksum": indexer.get_file_hash(
                                                full_file_path
                                            ),
                                        }
                                        logger.debug(
                                            f"Calling dal_instance.search.index_document for {file_path}"
                                        )
                                        response = dal_instance.search.index_document(
                                            doc_id, document
                                        )
                                        if response:
                                            logger.debug(
                                                f"Indexed {file_path} into Elasticsearch. Response: {response}"
                                            )
                                        else:
                                            logger.error(
                                                f"Failed to index {file_path} into Elasticsearch. Indexing method returned False."
                                            )
                                    else:
                                        logger.warning(
                                            f"SmartFileReader returned None content for {full_file_path}, skipping Elasticsearch indexing."
                                        )
                                except Exception as es_e:
                                    logger.exception(
                                        f"Error indexing {file_path} into Elasticsearch: {es_e}"
                                    )

                            # Index into Core Engine
                            if core_engine:
                                try:
                                    # Ensure content is read if not already
                                    if "content" not in locals() or content is None:
                                        smart_reader = SmartFileReader(base_path)
                                        content = smart_reader.read_content(
                                            full_file_path
                                        )

                                    if content:
                                        await core_engine.index_file(
                                            base_path, file_path, content
                                        )
                                except Exception as core_e:
                                    logger.error(
                                        f"Error indexing {file_path} into Core Engine: {core_e}"
                                    )

                    else:
                        logger.error(
                            f"Failed to index task {result.task_id}: {result.errors}"
                        )

                logger.info(
                    f"Parallel indexing completed: {file_count} files processed"
                )
            except Exception as e:
                logger.exception(f"Error in parallel processing: {e}")
                # Fall back to sequential processing
                logger.info("Falling back to sequential processing...")

                # Sequential fallback (existing logic)
                for root, dirs, files in os.walk(base_path):
                    # Create relative path from base_path
                    rel_path = os.path.relpath(root, base_path)

                    # Skip the current directory if it should be ignored by pattern matcher
                    if rel_path != "." and ignore_matcher.should_ignore_directory(
                        rel_path
                    ):
                        logger.debug(
                            f"Skipping directory '{rel_path}' due to ignore pattern (sequential fallback)."
                        )
                        dirs[:] = []  # Don't recurse into subdirectories
                        continue

                    # Check if directory should be skipped due to size/count filtering
                    if (
                        rel_path != "."
                        and config_manager.should_skip_directory_by_pattern(rel_path)
                    ):
                        logger.debug(
                            f"Skipping directory by pattern: {rel_path} (sequential fallback)"
                        )
                        dirs[:] = []  # Don't recurse into subdirectories
                        continue
                # Count files and subdirectories for directory filtering
                visible_files = []
                for file in files:
                    _, ext = os.path.splitext(file)
                    file_path = os.path.join(rel_path, file).replace("\\", "/")
                    if rel_path == ".":
                        file_path = file

                    if file.startswith("."):
                        logger.debug(
                            f"Skipping hidden file: '{file_path}' (sequential fallback)"
                        )
                        continue
                    if ext not in supported_extensions:
                        logger.debug(
                            f"Skipping file with unsupported extension: '{file_path}' (extension: '{ext}') (sequential fallback)"
                        )
                        continue

                    # Check if file should be ignored by pattern matcher
                    if ignore_matcher.should_ignore(file_path):
                        logger.debug(
                            f"Skipping file '{file_path}' due to ignore pattern (sequential fallback)."
                        )
                        continue

                    full_file_path = os.path.join(root, file)
                    try:
                        file_size = os.path.getsize(full_file_path)
                        if config_manager.should_skip_file_by_size(
                            file_path, file_size
                        ):
                            logger.debug(
                                f"Skipping large file: {file_path} ({file_size} bytes) (sequential fallback)"
                            )
                            continue
                    except (OSError, IOError) as e:
                        logger.exception(
                            f"Error getting file size for {file_path}: {e} (sequential fallback)"
                        )
                        continue

                    visible_files.append((file, file_path, ext))

                    visible_dirs = [
                        d
                        for d in dirs
                        if not ignore_matcher.should_ignore_directory(
                            os.path.join(rel_path, d) if rel_path != "." else d
                        )
                    ]
                    # Apply directory count filtering
                    if config_manager.should_skip_directory_by_count(
                        rel_path, len(visible_files), len(visible_dirs)
                    ):
                        logger.debug(
                            f"Skipping directory by count: {rel_path} ({len(visible_files)} files, {len(visible_dirs)} subdirs) (sequential fallback)"
                        )
                        dirs[:] = []  # Don't recurse into subdirectories
                        continue

                    # Filter directories using the ignore pattern matcher
                    dirs[:] = visible_dirs

                    current_dir = file_index

                    # Skip the '.' directory (base_path itself)
                    if rel_path != ".":
                        # Split the path and navigate/create the tree
                        path_parts = rel_path.replace("\\", "/").split("/")
                        for part in path_parts:
                            if part not in current_dir:
                                current_dir[part] = {}
                            current_dir = current_dir[part]

                    # Add files to current directory and update metadata
                    for file, file_path, ext in visible_files:
                        # Only add to index if it's a changed file or if we're doing a full rebuild
                        if not changed_files or file_path in changed_files:
                            current_dir[file] = {
                                "type": "file",
                                "path": file_path,
                                "ext": ext,
                            }
                            file_count += 1

                            # Update file metadata for changed files
                            if file_path in changed_files:
                                full_file_path = os.path.join(base_path, file_path)
                                indexer.update_file_metadata(file_path, full_file_path)
                                # Index content into Elasticsearch (sequential fallback)
                                if dal_instance and dal_instance.search:
                                    try:
                                        # Use SmartFileReader for enhanced content loading with better error handling
                                        smart_reader = SmartFileReader(base_path)
                                        content = smart_reader.read_content(
                                            full_file_path
                                        )

                                        if content is not None:
                                            logger.debug(
                                                f"Received content for {full_file_path} (sequential), length: {len(content)} bytes."
                                            )
                                            doc_id = (
                                                file_path  # Use file_path as doc_id
                                            )
                                            document = {
                                                "file_id": doc_id,
                                                "language": ext.lstrip("."),
                                                "last_modified": datetime.fromtimestamp(
                                                    os.path.getmtime(full_file_path)
                                                ).isoformat(),
                                                "size": os.path.getsize(full_file_path),
                                                "checksum": indexer.get_file_hash(
                                                    full_file_path
                                                ),
                                            }
                                            logger.debug(
                                                f"Calling dal_instance.search.index_document for {file_path} (sequential)"
                                            )
                                            response = (
                                                dal_instance.search.index_document(
                                                    doc_id, document
                                                )
                                            )
                                            if response:
                                                logger.debug(
                                                    f"Indexed {file_path} into Elasticsearch (sequential). Response: {response}"
                                                )
                                            else:
                                                logger.error(
                                                    f"Failed to index {file_path} into Elasticsearch (sequential). Indexing method returned False."
                                                )
                                        else:
                                            logger.warning(
                                                f"SmartFileReader returned None content for {full_file_path} (sequential), skipping Elasticsearch indexing."
                                            )
                                    except Exception as es_e:
                                        logger.exception(
                                            f"Error indexing {file_path} into Elasticsearch (sequential): {es_e}"
                                        )

                                # Index into Core Engine (Sequential)
                                if core_engine:
                                    try:
                                        # Ensure content is read if not already
                                        if "content" not in locals() or content is None:
                                            smart_reader = SmartFileReader(base_path)
                                            content = smart_reader.read_content(
                                                full_file_path
                                            )

                                        if content:
                                            await core_engine.index_file(
                                                base_path, file_path, content
                                            )
                                    except Exception as core_e:
                                        logger.error(
                                            f"Error indexing {file_path} into Core Engine (sequential): {core_e}"
                                        )

        # Save updated metadata
        indexer.save_metadata()

        # Complete performance monitoring
        if performance_monitor and indexing_context:
            try:
                # Update operation metadata with results
                indexing_context.metadata.update(
                    {
                        "files_indexed": file_count,
                        "files_filtered": filtered_files,
                        "directories_filtered": filtered_dirs,
                        "added_files": len(added_files)
                        if "added_files" in locals()
                        else 0,
                        "modified_files": len(modified_files)
                        if "modified_files" in locals()
                        else 0,
                        "deleted_files": len(deleted_files)
                        if "deleted_files" in locals()
                        else 0,
                    }
                )

                # Exit the timing context
                indexing_context.__exit__(None, None, None)

                # Log completion
                performance_monitor.log_structured(
                    "info",
                    "Project indexing completed successfully",
                    base_path=base_path,
                    files_indexed=file_count,
                    files_filtered=filtered_files,
                    directories_filtered=filtered_dirs,
                    duration_ms=getattr(indexing_context, "duration_ms", 0),
                )

                # Increment success counter
                performance_monitor.increment_counter("indexing_operations_total")

            except Exception as e:
                # Log indexing error
                performance_monitor.log_structured(
                    "error",
                    "Error during indexing performance monitoring",
                    error=str(e),
                )
                # Still exit the context to avoid resource leaks
                if indexing_context:
                    try:
                        indexing_context.__exit__(Exception, type(e), None)
                    except:
                        pass

        logger.info(
            f"Indexing completed: {file_count} files indexed, {filtered_files} files filtered, {filtered_dirs} directories filtered"
        )
        return file_count


def _count_files(directory) -> int:
    """
    Count the number of files in the index.
    Supports both dict and TrieFileIndex structures.
    """
    # Check if it's a TrieFileIndex with get_all_files method
    if hasattr(directory, "get_all_files"):
        return len(directory.get_all_files())

    # Check if it's a TrieFileIndex with __len__ method
    if hasattr(directory, "__len__") and hasattr(directory, "root"):
        return len(directory)

    # Check if it's a TrieFileIndex but can't call items() on it
    if hasattr(directory, "root") and not hasattr(directory, "items"):
        # This is a TrieFileIndex, but it doesn't have get_all_files
        # Try to get its length or return 0
        return getattr(directory, "_count", 0)

    # Handle regular dictionary structure
    if not isinstance(directory, dict):
        return 0

    count = 0
    for name, value in directory.items():
        if isinstance(value, dict):
            if "type" in value and value["type"] == "file":
                count += 1
            else:
                count += _count_files(value)
    return count


def _get_all_files(directory, prefix: str = "") -> List[Tuple[str, Dict]]:
    """Recursively get all files from the index.
    Supports both dict and TrieFileIndex structures.
    """
    # Check if it's a TrieFileIndex
    if hasattr(directory, "get_all_files"):
        return directory.get_all_files()

    # Handle regular dictionary structure
    if not isinstance(directory, dict):
        return []

    all_files = []
    for name, item in directory.items():
        current_path = os.path.join(prefix, name)
        if isinstance(item, dict) and item.get("type") == "file":
            all_files.append((current_path, item))
        elif isinstance(item, dict) and item.get("type") == "directory":
            all_files.extend(_get_all_files(item.get("children", {}), current_path))
        elif isinstance(item, dict) and "type" not in item:
            # Handle nested directory structure without explicit type
            all_files.extend(_get_all_files(item, current_path))
    return all_files


def _remove_file_from_index(directory: Dict, file_path: str):
    """Recursively remove a file from the in-memory file_index."""
    parts = file_path.replace("\\", "/").split("/")
    current_dir = directory
    for i, part in enumerate(parts):
        if i == len(parts) - 1:  # Last part is the file name
            if part in current_dir and current_dir[part].get("type") == "file":
                del current_dir[part]
                return True
            return False
        else:  # Directory part
            if part in current_dir and isinstance(current_dir[part], dict):
                current_dir = current_dir[part]
            else:
                return False  # Path not found
    return False


def _add_file_to_index(directory: Dict, file_path: str):
    """Recursively add a file to the in-memory file_index."""
    parts = file_path.replace("\\", "/").split("/")
    current_dir = directory
    for i, part in enumerate(parts):
        if i == len(parts) - 1:  # Last part is the file name
            _, ext = os.path.splitext(file_path)
            current_dir[part] = {"type": "file", "path": file_path, "ext": ext}
            return True
        else:  # Directory part
            if part not in current_dir:
                current_dir[part] = {}
            current_dir = current_dir[part]
    return False


# ============================================================================
# PHASE 7 MCP TOOLS: Ranking, API Key Manager, Stats Dashboard
# ============================================================================


async def get_index_statistics(
    ctx: Context, force_refresh: bool = False
) -> Dict[str, Any]:
    """
    Get comprehensive index statistics from the dashboard.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Index Statistics Dashboard: CLI command showing index health metrics"

    Provides statistics about:
    - Document count and size
    - Backend health (SQLite, DuckDB, Tantivy, LEANN)
    - Index status and health
    - Overall system status

    Args:
        force_refresh: Force refresh even if cache is valid

    Returns:
        Dictionary with index statistics including backend health,
        document counts, sizes, and overall status
    """
    stats_collector = ensure_stats_collector()

    try:
        stats = await stats_collector.collect_statistics(force_refresh=force_refresh)
        return stats.to_dict()
    except Exception as e:
        logger.error(f"Error collecting statistics: {e}")
        return {
            "error": str(e),
            "overall_status": "error",
            "indices": {},
            "backends": {},
        }


async def get_backend_health(ctx: Context) -> Dict[str, Any]:
    """
    Get health status of all backends.

    Returns the health status of PostgreSQL, Elasticsearch,
    and any other connected backends.

    Returns:
        Dictionary with backend names as keys and health status as values
    """
    stats_collector = ensure_stats_collector()

    try:
        stats = await stats_collector.collect_statistics(force_refresh=True)
        return {name: health.to_dict() for name, health in stats.backends.items()}
    except Exception as e:
        logger.error(f"Error checking backend health: {e}")
        return {"error": str(e)}


def get_ranking_configuration(ctx: Context) -> Dict[str, Any]:
    """
    Get the current search ranking configuration.

    Returns the weights and settings used for search result ranking,
    including semantic, recency, frequency, path importance, and
    file size weights.

    Returns:
        Dictionary with ranking configuration
    """
    ranker = ensure_result_ranker()

    config = ranker.config

    return {
        "weights": {
            "semantic": config.semantic_weight,
            "recency": config.recency_weight,
            "frequency": config.frequency_weight,
            "path_importance": config.path_importance_weight,
            "file_size": config.file_size_weight,
        },
        "recency_settings": {
            "half_life_days": config.recency_half_life_days,
            "max_bonus": config.max_recency_bonus,
        },
        "frequency_settings": {
            "decay_factor": config.frequency_decay_factor,
            "min_access_count": config.min_access_count,
        },
        "path_importance_scores": {
            category.value: score
            for category, score in config.path_importance_scores.items()
        },
        "file_size_settings": {
            "optimal_min": config.optimal_size_min,
            "optimal_max": config.optimal_size_max,
        },
        "user_tracking_enabled": config.enable_user_tracking,
    }


async def rank_search_results(
    ctx: Context, results: List[Dict[str, Any]], query: str = ""
) -> List[Dict[str, Any]]:
    """
    Apply intelligent ranking to search results.

    Enhances search results by applying multi-factor ranking:
    - Semantic similarity (base score)
    - File recency (recently modified files)
    - User behavior frequency (frequently accessed files)
    - Path importance (source > config > tests > docs)
    - File size (prefer moderate sizes)

    Args:
        results: List of search result dictionaries with 'file_path', 'score', etc.
        query: Optional search query for behavior tracking

    Returns:
        List of ranked search results with additional ranking metadata
    """
    ranker = ensure_result_ranker()

    try:
        ranked = ranker.rank_results(results, query=query)

        # Convert SearchResult objects back to dictionaries
        output = []
        for result in ranked:
            output.append(
                {
                    "file_path": result.file_path,
                    "original_score": result.original_score,
                    "ranked_score": result.ranked_score,
                    "content_preview": result.content_preview,
                    "metadata": result.metadata,
                    "ranking_components": {
                        "semantic": result.semantic_component,
                        "recency": result.recency_component,
                        "frequency": result.frequency_component,
                        "path": result.path_component,
                        "size": result.size_component,
                    },
                }
            )

        return output
    except Exception as e:
        logger.error(f"Error ranking search results: {e}")
        # Return original results on error
        return results


def main():
    """Main function to run the MCP server."""
    # Run the server. Tools are discovered automatically via decorators.
    # Elasticsearch checking is handled in the lifespan function.
    mcp.run()


if __name__ == "__main__":
    # Set path to project root
    sys.path.append(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    main()
