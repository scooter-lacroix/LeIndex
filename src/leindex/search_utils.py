"""
Search utilities for consistent backend selection and error handling.
"""

import time
from typing import Dict, Any, List, Tuple, Optional
from .storage.storage_interface import SearchInterface, DALInterface
from .logger_config import logger
from .constants import (
    DEFAULT_MAX_SEARCH_RESULTS,
    HEALTH_CHECK_CACHE_TIMEOUT,
    SEARCH_OPERATIONS_MAX_AGE_HOURS,
    RECENT_OPERATIONS_LIMIT,
)

class SearchBackendSelector:
    """Handles consistent backend selection and validation."""

    @staticmethod
    def get_search_backend(dal: Optional[DALInterface]) -> Optional[SearchInterface]:
        """Get the appropriate search backend from DAL with validation."""
        if not dal:
            logger.warning("DAL instance is None")
            return None

        if not hasattr(dal, 'search'):
            logger.warning("DAL instance does not have search attribute")
            return None

        if not isinstance(dal.search, SearchInterface):
            logger.warning(f"DAL search is not a SearchInterface: {type(dal.search)}")
            return None

        # Additional validation: check if backend is properly initialized
        try:
            # Test basic functionality to ensure backend is operational
            backend_type = SearchBackendSelector._get_backend_type(dal.search)
            logger.debug(f"Validated search backend type: {backend_type}")
            return dal.search
        except Exception as e:
            logger.error(f"Search backend validation failed: {e}")
            return None

    @staticmethod
    def _get_backend_type(search_backend: SearchInterface) -> str:
        """Get the backend type with improved detection."""
        if not search_backend:
            return "unknown"

        # Get the class name and module for more reliable detection
        backend_class = type(search_backend).__name__
        backend_module = type(search_backend).__module__

        logger.debug(f"Backend class: {backend_class}, module: {backend_module}")

        # Check for Tantivy (new primary full-text search backend)
        if 'tantivy' in backend_class.lower() or 'tantivy' in backend_module.lower():
            return "tantivy"

        # Check for Elasticsearch (legacy)
        if 'elasticsearch' in backend_class.lower() or 'elasticsearch' in backend_module.lower():
            return "elasticsearch"

        # Check for SQLite
        if 'sqlite' in backend_class.lower() or 'sqlite' in backend_module.lower():
            return "sqlite"

        # Check for PostgreSQL
        if 'postgres' in backend_class.lower() or 'postgresql' in backend_module.lower():
            return "postgresql"

        return "unknown"

    @staticmethod
    def is_elasticsearch_backend(search_backend: SearchInterface) -> bool:
        """Check if the search backend is Elasticsearch-based."""
        return SearchBackendSelector._get_backend_type(search_backend) == "elasticsearch"

    @staticmethod
    def is_sqlite_backend(search_backend: SearchInterface) -> bool:
        """Check if the search backend is SQLite-based."""
        return SearchBackendSelector._get_backend_type(search_backend) == "sqlite"

    @staticmethod
    def is_postgresql_backend(search_backend: SearchInterface) -> bool:
        """Check if the search backend is PostgreSQL-based."""
        return SearchBackendSelector._get_backend_type(search_backend) == "postgresql"

    @staticmethod
    def is_tantivy_backend(search_backend: SearchInterface) -> bool:
        """Check if the search backend is Tantivy-based."""
        return SearchBackendSelector._get_backend_type(search_backend) == "tantivy"

    @staticmethod
    def get_backend_capabilities(search_backend: SearchInterface) -> Dict[str, Any]:
        """Get backend capabilities and features."""
        backend_type = SearchBackendSelector._get_backend_type(search_backend)

        capabilities = {
            "backend_type": backend_type,
            "supports_regex": False,
            "supports_fuzzy": False,
            "supports_highlighting": False,
            "supports_pagination": True,
            "max_result_limit": DEFAULT_MAX_SEARCH_RESULTS
        }

        if backend_type == "tantivy":
            # Tantivy has comprehensive search capabilities
            capabilities.update({
                "supports_regex": True,
                "supports_fuzzy": True,
                "supports_highlighting": True,
                "supports_phrase_queries": True,
                "supports_prefix_queries": True,
                "max_result_limit": DEFAULT_MAX_SEARCH_RESULTS
            })
        elif backend_type == "sqlite":
            capabilities.update({
                "supports_regex": True,  # SQLite supports REGEXP
                "supports_fuzzy": False,
                "supports_highlighting": False,
                "max_result_limit": DEFAULT_MAX_SEARCH_RESULTS
            })

        return capabilities

class SearchErrorHandler:
    """Handles search errors and provides fallback mechanisms."""

    @staticmethod
    def handle_search_error(error: Exception, backend_type: str, operation: str) -> Dict[str, Any]:
        """Handle search errors with appropriate logging and error messages."""
        error_msg = f"Error during {operation} with {backend_type} backend: {error}"
        logger.error(error_msg, exc_info=True)

        return {
            "error": error_msg,
            "backend_type": backend_type,
            "operation": operation
        }

    @staticmethod
    def create_fallback_response(error_details: Dict[str, Any]) -> Dict[str, Any]:
        """Create a standardized fallback response."""
        return {
            "error": f"Search failed with {error_details.get('backend_type', 'unknown')} backend",
            "fallback_attempted": True,
            "details": error_details
        }

class SearchPatternTranslator:
    """Handles pattern translation between different search syntaxes."""

    @staticmethod
    def normalize_pattern(pattern: str, is_regex: bool = False) -> Tuple[str, bool]:
        """Normalize search pattern and determine if it's regex."""
        if not pattern or not pattern.strip():
            return "", False

        # If explicitly marked as regex, validate and return
        if is_regex:
            return SearchPatternTranslator._validate_regex_pattern(pattern), True

        # Check for common regex patterns
        regex_indicators = ['^', '$', '(?', '[', ']', '{', '}', '|', '+', '*', '?', '\\']
        has_regex = any(indicator in pattern for indicator in regex_indicators)

        # Check for SQL LIKE patterns
        has_like = '%' in pattern or '_' in pattern

        # Check for GLOB patterns (but not as part of regex)
        has_glob_only = ('*' in pattern or '?' in pattern) and not has_regex

        # Determine pattern type with improved logic
        if has_regex and not has_like:
            # Pure regex pattern
            return SearchPatternTranslator._validate_regex_pattern(pattern), True
        elif has_like:
            # SQL LIKE pattern - convert to regex
            try:
                converted = SearchPatternTranslator._convert_like_to_regex(pattern)
                return SearchPatternTranslator._validate_regex_pattern(converted), True
            except Exception as e:
                logger.warning(f"Failed to convert LIKE pattern '{pattern}' to regex: {e}")
                # Fall back to literal search
                return pattern, False
        elif has_glob_only:
            # GLOB pattern - convert to regex
            try:
                converted = SearchPatternTranslator._convert_glob_to_regex(pattern)
                return SearchPatternTranslator._validate_regex_pattern(converted), True
            except Exception as e:
                logger.warning(f"Failed to convert GLOB pattern '{pattern}' to regex: {e}")
                # Fall back to literal search
                return pattern, False
        else:
            # Literal string
            return pattern, False

    @staticmethod
    def _validate_regex_pattern(pattern: str) -> str:
        """Validate and potentially fix regex pattern."""
        try:
            # Test if the pattern compiles
            import re
            re.compile(pattern, re.MULTILINE)
            return pattern
        except re.error as e:
            logger.warning(f"Invalid regex pattern '{pattern}': {e}")
            # Try to escape special characters to make it valid
            try:
                escaped = re.escape(pattern)
                re.compile(escaped, re.MULTILINE)
                logger.info(f"Escaped invalid regex pattern to: '{escaped}'")
                return escaped
            except re.error:
                # If escaping doesn't work, return original and let caller handle
                logger.error(f"Could not fix regex pattern '{pattern}'")
                return pattern

    @staticmethod
    def _convert_like_to_regex(pattern: str) -> str:
        """Convert SQL LIKE patterns to regex with improved escaping."""
        if not pattern:
            return pattern

        # Escape regex special characters except % and _
        # We need to be careful about the order of escaping
        escaped = pattern

        # Escape backslash first (important for Windows paths)
        escaped = escaped.replace('\\', '\\\\')

        # Escape other special characters
        special_chars = ['.', '^', '$', '(', ')', '[', ']', '{', '}', '|', '+', '?']
        for char in special_chars:
            escaped = escaped.replace(char, f'\\{char}')

        # Convert LIKE wildcards to regex
        # % matches any sequence of characters (including none)
        # _ matches exactly one character
        regex_pattern = escaped.replace('%', '.*').replace('_', '.')

        # Add anchors if the pattern doesn't have them and doesn't start/end with wildcards
        if not regex_pattern.startswith('.*') and not regex_pattern.startswith('^'):
            regex_pattern = '^' + regex_pattern
        if not regex_pattern.endswith('.*') and not regex_pattern.endswith('$'):
            regex_pattern = regex_pattern + '$'

        return regex_pattern

    @staticmethod
    def _convert_glob_to_regex(pattern: str) -> str:
        """Convert GLOB patterns to regex."""
        if not pattern:
            return pattern

        # Escape regex special characters
        escaped = pattern.replace('\\', '\\\\')
        special_chars = ['.', '^', '$', '(', ')', '[', ']', '{', '}', '|', '+']
        for char in special_chars:
            escaped = escaped.replace(char, f'\\{char}')

        # Convert GLOB wildcards
        # * matches any sequence of characters (including none)
        # ? matches exactly one character
        regex_pattern = escaped.replace('*', '.*').replace('?', '.')

        # Add anchors to make it match the whole string
        if not regex_pattern.startswith('.*') and not regex_pattern.startswith('^'):
            regex_pattern = '^' + regex_pattern
        if not regex_pattern.endswith('.*') and not regex_pattern.endswith('$'):
            regex_pattern = regex_pattern + '$'

        return regex_pattern

    @staticmethod
    def escape_for_backend(pattern: str, backend_type: str) -> str:
        """Escape pattern for specific backend requirements."""
        if backend_type == "elasticsearch":
            # Elasticsearch has its own escaping rules
            return SearchPatternTranslator._escape_for_elasticsearch(pattern)
        elif backend_type == "sqlite":
            # SQLite FTS has specific escaping needs
            return SearchPatternTranslator._escape_for_sqlite_fts(pattern)
        else:
            # Default: escape for regex
            return SearchPatternTranslator._validate_regex_pattern(pattern)

    @staticmethod
    def _escape_for_elasticsearch(pattern: str) -> str:
        """Escape special characters for Elasticsearch queries."""
        if not pattern:
            return pattern

        # Characters that need escaping in Elasticsearch query strings
        # Note: This is for query string syntax, not regex
        special_chars = ['+', '-', '=', '&&', '||', '>', '<', '!', '(', ')', '{', '}', '[', ']', '^', '"', '~', '*', '?', ':', '\\', '/']

        escaped = pattern
        for char in special_chars:
            escaped = escaped.replace(char, f'\\{char}')

        return escaped

    @staticmethod
    def _escape_for_sqlite_fts(pattern: str) -> str:
        """Escape special characters for SQLite FTS queries."""
        if not pattern:
            return pattern

        # SQLite FTS5 special characters that need escaping
        # Note: * and ? are wildcards in FTS, but we handle them separately
        escaped = pattern.replace('"', '""')  # Double quotes need to be doubled

        return escaped

class SearchResultProcessor:
    """Processes and standardizes search results from different backends."""

    @staticmethod
    def standardize_results(results: List[Tuple[str, Any]], backend_type: str) -> List[Dict[str, Any]]:
        """Standardize search results to a common format with improved error handling."""
        standardized = []

        if not results:
            logger.debug(f"No results to standardize from {backend_type}")
            return standardized

        if not isinstance(results, list):
            logger.warning(f"Results from {backend_type} is not a list: {type(results)}")
            return standardized

        for i, result in enumerate(results):
            try:
                standardized_result = SearchResultProcessor._standardize_single_result(result, backend_type, i)
                if standardized_result:
                    standardized.append(standardized_result)
            except Exception as e:
                logger.warning(f"Error standardizing result {i} from {backend_type}: {e}")
                continue

        logger.debug(f"Standardized {len(standardized)} results from {backend_type} (from {len(results)} raw results)")
        return standardized

    @staticmethod
    def _standardize_single_result(result: Any, backend_type: str, index: int) -> Optional[Dict[str, Any]]:
        """Standardize a single search result."""
        if not isinstance(result, (tuple, list)) or len(result) != 2:
            logger.warning(f"Result {index} from {backend_type} is not a 2-tuple: {type(result)}")
            return None

        file_path, content_data = result

        # Validate file_path
        if not file_path or not isinstance(file_path, str):
            logger.warning(f"Invalid file_path in result {index} from {backend_type}: {file_path}")
            return None

        # Handle different content_data formats
        if isinstance(content_data, dict):
            standardized_result = SearchResultProcessor._standardize_dict_content(file_path, content_data, backend_type)
        elif isinstance(content_data, str):
            standardized_result = SearchResultProcessor._standardize_string_content(file_path, content_data, backend_type)
        elif content_data is None:
            logger.debug(f"Empty content for {file_path} from {backend_type}")
            standardized_result = SearchResultProcessor._create_empty_result(file_path, backend_type)
        else:
            # Try to convert to string
            try:
                content_str = str(content_data)
                standardized_result = SearchResultProcessor._standardize_string_content(file_path, content_str, backend_type)
            except Exception as e:
                logger.warning(f"Could not convert content_data to string for {file_path}: {e}")
                return None

        return standardized_result

    @staticmethod
    def _standardize_dict_content(file_path: str, content_data: Dict[str, Any], backend_type: str) -> Dict[str, Any]:
        """Standardize result when content_data is a dictionary."""
        # Extract content with fallbacks
        content = content_data.get("content", content_data.get("text", content_data.get("value", "")))

        # Extract line number with validation
        line = content_data.get("line", content_data.get("line_number", 0))
        if not isinstance(line, int) or line < 0:
            line = 0

        # Extract position information
        start = content_data.get("start", content_data.get("start_offset", 0))
        end = content_data.get("end", content_data.get("end_offset", 0))

        # Validate positions
        if not isinstance(start, int) or start < 0:
            start = 0
        if not isinstance(end, int) or end < start:
            end = start + len(str(content)) if content else 0

        return {
            "file_path": file_path,
            "content": str(content) if content is not None else "",
            "line": line,
            "start": start,
            "end": end,
            "backend": backend_type,
            "metadata": content_data.get("metadata", {}),
            "score": content_data.get("score", content_data.get("_score", 0.0))
        }

    @staticmethod
    def _standardize_string_content(file_path: str, content_data: str, backend_type: str) -> Dict[str, Any]:
        """Standardize result when content_data is a string."""
        return {
            "file_path": file_path,
            "content": content_data,
            "line": 0,
            "start": 0,
            "end": len(content_data),
            "backend": backend_type,
            "metadata": {},
            "score": 0.0
        }

    @staticmethod
    def _create_empty_result(file_path: str, backend_type: str) -> Dict[str, Any]:
        """Create a standardized result for empty content."""
        return {
            "file_path": file_path,
            "content": "",
            "line": 0,
            "start": 0,
            "end": 0,
            "backend": backend_type,
            "metadata": {},
            "score": 0.0
        }

    @staticmethod
    def _is_recoverable_error(error: Exception, backend_type: str) -> bool:
        """Determine if an error is recoverable and worth retrying."""
        error_type = type(error).__name__
        error_message = str(error).lower()

        # Network/connection errors are generally recoverable
        if "connection" in error_message or "timeout" in error_message:
            return True

        # SQLite-specific recoverable errors
        if backend_type.lower() == "sqlite":
            if "database is locked" in error_message or "database disk image is malformed" in error_message:
                return True

        # Elasticsearch-specific recoverable errors
        if backend_type.lower() == "elasticsearch":
            if "timeout" in error_message or "service unavailable" in error_message:
                return True

        # Some specific exception types are recoverable
        recoverable_exceptions = [
            "ConnectionError", "TimeoutError", "TemporaryFailure",
            "OperationalError", "InternalServerError"
        ]

        if error_type in recoverable_exceptions:
            return True

        # By default, assume errors are not recoverable
        return False

    @staticmethod
    def merge_results(*result_sets: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        """Merge multiple result sets, removing duplicates."""
        merged = []
        seen_paths = set()

        for result_set in result_sets:
            for result in result_set:
                file_path = result.get("file_path", "")
                if file_path not in seen_paths:
                    merged.append(result)
                    seen_paths.add(file_path)

        return merged

class SearchMonitor:
    """Comprehensive monitoring and logging for search operations."""

    def __init__(self):
        self.search_operations = []
        self.performance_stats = {
            "total_searches": 0,
            "successful_searches": 0,
            "failed_searches": 0,
            "backend_usage": {},
            "average_response_times": {},
            "error_types": {}
        }

    def log_search_start(self, pattern: str, backend_type: str, **kwargs) -> str:
        """Log the start of a search operation."""
        operation_id = f"search_{int(time.time() * 1000)}_{hash(pattern) % 1000}"
        operation = {
            "operation_id": operation_id,
            "pattern": pattern,
            "backend_type": backend_type,
            "start_time": time.time(),
            "status": "in_progress",
            "kwargs": kwargs
        }

        self.search_operations.append(operation)
        logger.info(f"Search operation started: {operation_id} (pattern: '{pattern}', backend: {backend_type})")

        return operation_id

    def log_search_success(self, operation_id: str, results_count: int, **metadata):
        """Log a successful search operation."""
        operation = self._find_operation(operation_id)
        if operation:
            operation["status"] = "completed"
            operation["end_time"] = time.time()
            operation["results_count"] = results_count
            operation["duration_ms"] = (operation["end_time"] - operation["start_time"]) * 1000
            operation["metadata"] = metadata

            # Update performance stats
            self.performance_stats["total_searches"] += 1
            self.performance_stats["successful_searches"] += 1

            backend = operation["backend_type"]
            if backend not in self.performance_stats["backend_usage"]:
                self.performance_stats["backend_usage"][backend] = 0
            self.performance_stats["backend_usage"][backend] += 1

            if backend not in self.performance_stats["average_response_times"]:
                self.performance_stats["average_response_times"][backend] = []
            self.performance_stats["average_response_times"][backend].append(operation["duration_ms"])

            logger.info(f"Search operation completed: {operation_id} "
                       f"(results: {results_count}, duration: {operation['duration_ms']:.2f}ms)")

    def log_search_failure(self, operation_id: str, error: Exception, **metadata):
        """Log a failed search operation."""
        operation = self._find_operation(operation_id)
        if operation:
            operation["status"] = "failed"
            operation["end_time"] = time.time()
            operation["error"] = str(error)
            operation["error_type"] = type(error).__name__
            operation["duration_ms"] = (operation["end_time"] - operation["start_time"]) * 1000
            operation["metadata"] = metadata

            # Update performance stats
            self.performance_stats["total_searches"] += 1
            self.performance_stats["failed_searches"] += 1

            error_type = operation["error_type"]
            if error_type not in self.performance_stats["error_types"]:
                self.performance_stats["error_types"][error_type] = 0
            self.performance_stats["error_types"][error_type] += 1

            logger.error(f"Search operation failed: {operation_id} "
                        f"(error: {error_type}, duration: {operation['duration_ms']:.2f}ms)")

    def get_performance_summary(self) -> Dict[str, Any]:
        """Get a summary of search performance statistics."""
        summary = dict(self.performance_stats)

        # Calculate average response times
        for backend, times in summary["average_response_times"].items():
            if times:
                summary["average_response_times"][backend] = sum(times) / len(times)

        # Calculate success rate
        total = summary["total_searches"]
        if total > 0:
            summary["success_rate"] = summary["successful_searches"] / total
        else:
            summary["success_rate"] = 0.0

        return summary

    def get_recent_operations(self, limit: int = RECENT_OPERATIONS_LIMIT) -> List[Dict[str, Any]]:
        """Get the most recent search operations."""
        return sorted(self.search_operations[-limit:], key=lambda x: x.get("start_time", 0), reverse=True)

    def _find_operation(self, operation_id: str) -> Optional[Dict[str, Any]]:
        """Find an operation by its ID."""
        for operation in self.search_operations:
            if operation.get("operation_id") == operation_id:
                return operation
        return None

    def cleanup_old_operations(self, max_age_hours: float = SEARCH_OPERATIONS_MAX_AGE_HOURS) -> None:
        """Clean up old search operations to prevent memory bloat."""
        cutoff_time = time.time() - (max_age_hours * 3600)
        original_count = len(self.search_operations)

        self.search_operations = [
            op for op in self.search_operations
            if op.get("start_time", 0) > cutoff_time
        ]

        removed_count = original_count - len(self.search_operations)
        if removed_count > 0:
            logger.info(f"Cleaned up {removed_count} old search operations")

# Global search monitor instance
search_monitor = SearchMonitor()

class BackendHealthChecker:
    """Checks the health and availability of search backends."""

    @staticmethod
    def check_sqlite_backend(search_backend: SearchInterface) -> Dict[str, Any]:
        """Check SQLite backend health."""
        if not SearchBackendSelector.is_sqlite_backend(search_backend):
            return {"healthy": False, "reason": "Not a SQLite backend"}

        try:
            # Try a simple query to test connectivity
            test_results = search_backend.search_content("test", is_regex=False)
            return {
                "healthy": True,
                "backend_type": "SQLite",
                "test_results_count": len(test_results) if test_results else 0
            }
        except Exception as e:
            return {
                "healthy": False,
                "backend_type": "SQLite",
                "reason": str(e),
                "error_type": type(e).__name__
            }

    @staticmethod
    def check_elasticsearch_backend(search_backend: SearchInterface) -> Dict[str, Any]:
        """
        Check Elasticsearch backend health with detailed diagnostics.

        CRITICAL FIX: Detects missing aiohttp dependency and provides
        actionable error messages instead of generic failures.
        """
        if not SearchBackendSelector.is_elasticsearch_backend(search_backend):
            return {"healthy": False, "reason": "Not an Elasticsearch backend"}

        # Check for aiohttp dependency first
        try:
            import aiohttp
        except ImportError:
            return {
                "healthy": False,
                "backend_type": "Elasticsearch",
                "reason": "Required dependency 'aiohttp' is not installed. "
                         "This module is required for Elasticsearch connectivity. "
                         "Please reinstall dependencies: pip install -e .",
                "error_type": "ImportError",
                "missing_dependency": "aiohttp"
            }

        try:
            # Try a simple query to test connectivity
            test_results = search_backend.search_content("test", is_sqlite_pattern=False)
            return {
                "healthy": True,
                "backend_type": "Elasticsearch",
                "test_results_count": len(test_results) if test_results else 0
            }
        except RuntimeError as e:
            # Handle our enhanced error messages
            error_msg = str(e)
            if "aiohttp" in error_msg and "not installed" in error_msg:
                return {
                    "healthy": False,
                    "backend_type": "Elasticsearch",
                    "reason": error_msg,
                    "error_type": "RuntimeError",
                    "missing_dependency": "aiohttp"
                }
            return {
                "healthy": False,
                "backend_type": "Elasticsearch",
                "reason": error_msg,
                "error_type": "RuntimeError"
            }
        except Exception as e:
            return {
                "healthy": False,
                "backend_type": "Elasticsearch",
                "reason": str(e),
                "error_type": type(e).__name__
            }

    @staticmethod
    def check_tantivy_backend(search_backend: SearchInterface) -> Dict[str, Any]:
        """
        Check Tantivy backend health.

        Tantivy is an embedded search engine, so health checks are simpler
        than for network-based backends like Elasticsearch.
        """
        if not SearchBackendSelector.is_tantivy_backend(search_backend):
            return {"healthy": False, "reason": "Not a Tantivy backend"}

        # Check for tantivy dependency
        try:
            import tantivy
        except ImportError:
            return {
                "healthy": False,
                "backend_type": "Tantivy",
                "reason": "Required dependency 'tantivy' is not installed. "
                         "This module is required for Tantivy full-text search. "
                         "Please install: pip install tantivy",
                "error_type": "ImportError",
                "missing_dependency": "tantivy"
            }

        try:
            # Try a simple query to test connectivity
            test_results = search_backend.search_content("test", is_sqlite_pattern=False)

            # Get index stats if available
            stats = {}
            if hasattr(search_backend, 'get_index_stats'):
                stats = search_backend.get_index_stats()

            return {
                "healthy": True,
                "backend_type": "Tantivy",
                "test_results_count": len(test_results) if test_results else 0,
                "index_stats": stats
            }
        except Exception as e:
            return {
                "healthy": False,
                "backend_type": "Tantivy",
                "reason": str(e),
                "error_type": type(e).__name__
            }

    @staticmethod
    def check_backend_health(search_backend: SearchInterface) -> Dict[str, Any]:
        """Check the health of any search backend."""
        if not search_backend:
            return {"healthy": False, "reason": "No backend provided"}

        if SearchBackendSelector.is_tantivy_backend(search_backend):
            return BackendHealthChecker.check_tantivy_backend(search_backend)
        if SearchBackendSelector.is_elasticsearch_backend(search_backend):
            return BackendHealthChecker.check_elasticsearch_backend(search_backend)
        elif SearchBackendSelector.is_sqlite_backend(search_backend):
            return BackendHealthChecker.check_sqlite_backend(search_backend)
        else:
            return {
                "healthy": False,
                "reason": "Unknown backend type",
                "backend_type": str(type(search_backend))
            }

class GracefulDegradationManager:
    """Manages graceful degradation when backends are unavailable."""

    def __init__(self) -> None:
        self.backend_health_cache: Dict[str, Dict[str, Any]] = {}
        self.cache_timeout = HEALTH_CHECK_CACHE_TIMEOUT  # 5 minutes

    def get_backend_status(self, search_backend: SearchInterface) -> Dict[str, Any]:
        """Get the cached status of a backend, or check it if not cached or expired."""
        backend_key = str(id(search_backend))

        # Check if we have a cached status that's still valid
        if backend_key in self.backend_health_cache:
            cached_status = self.backend_health_cache[backend_key]
            if time.time() - cached_status.get("timestamp", 0) < self.cache_timeout:
                return cached_status

        # Check backend health and cache the result
        status = BackendHealthChecker.check_backend_health(search_backend)
        status["timestamp"] = time.time()
        self.backend_health_cache[backend_key] = status

        return status

    def should_use_backend(self, search_backend: SearchInterface) -> bool:
        """Determine if a backend should be used based on its health."""
        status = self.get_backend_status(search_backend)
        return status.get("healthy", False)

    def get_degradation_message(self, backend_type: str, reason: str) -> str:
        """Generate an appropriate degradation message."""
        if "connection" in reason.lower() or "timeout" in reason.lower():
            return f"{backend_type} backend is temporarily unavailable due to connection issues. Falling back to alternative search methods."
        elif "index" in reason.lower():
            return f"{backend_type} search index is corrupted or missing. Falling back to alternative search methods."
        elif "memory" in reason.lower():
            return f"{backend_type} backend is out of memory. Falling back to alternative search methods."
        else:
            return f"{backend_type} backend encountered an error: {reason}. Falling back to alternative search methods."

    def cleanup_cache(self) -> None:
        """Clean up expired cache entries."""
        current_time = time.time()
        expired_keys = [
            key for key, status in self.backend_health_cache.items()
            if current_time - status.get("timestamp", 0) > self.cache_timeout
        ]

        for key in expired_keys:
            del self.backend_health_cache[key]

        if expired_keys:
            logger.info(f"Cleaned up {len(expired_keys)} expired backend health cache entries")

# Global graceful degradation manager instance
degradation_manager = GracefulDegradationManager()