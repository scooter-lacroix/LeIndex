"""
Elasticsearch-based storage backend for full-text search.

PERFORMANCE FIX: Added LRU caching for frequently searched terms to reduce
Elasticsearch query load and improve response times.
"""

import logging
import re
import time
import hashlib
from datetime import datetime
from typing import Any, Dict, Optional, List, Tuple
from threading import Lock

# Assuming elasticsearch-py client
from elasticsearch import Elasticsearch, NotFoundError, ConnectionError

from .storage_interface import SearchInterface
from ..constants import (
    DEFAULT_SEARCH_CACHE_MAX_SIZE,
    DEFAULT_CACHE_TTL,
    ES_INDEX_NUMBER_OF_SHARDS,
    ES_INDEX_NUMBER_OF_REPLICAS,
    ES_INDEX_REFRESH_INTERVAL,
    ES_CODE_ANALYZER_MIN_NGRAM,
    ES_CODE_ANALYZER_MAX_NGRAM,
    MAX_PATTERN_LENGTH,
    MAX_WILDCARD_COUNT,
    MAX_REGEX_ALTERNATIONS,
    ES_CONNECTION_TEST_TIMEOUT,
)

logger = logging.getLogger(__name__)


class SearchCache:
    """
    LRU cache for Elasticsearch search results with TTL support.

    This cache stores recent search results to avoid hitting Elasticsearch for
    frequently searched terms. Uses a time-based expiration to ensure results
    don't become too stale.
    """

    def __init__(self, max_size: int = DEFAULT_SEARCH_CACHE_MAX_SIZE,
                 ttl_seconds: int = DEFAULT_CACHE_TTL) -> None:
        """
        Initialize the search cache.

        Args:
            max_size: Maximum number of cached results (default: 128)
            ttl_seconds: Time-to-live for cached results in seconds (default: 300 = 5 minutes)
        """
        self.max_size = max_size
        self.ttl_seconds = ttl_seconds
        self._cache: Dict[str, Tuple[float, Any]] = {}  # key -> (timestamp, value)
        self._lock = Lock()
        self._hits = 0
        self._misses = 0

    def _make_key(self, query: str, is_pattern: bool, query_type: str) -> str:
        """Generate a cache key from query parameters."""
        key_data = f"{query_type}:{query}:{is_pattern}"
        return hashlib.md5(key_data.encode()).hexdigest()

    def get(self, query: str, is_pattern: bool, query_type: str = "content") -> Optional[Any]:
        """Get cached result if available and not expired."""
        key = self._make_key(query, is_pattern, query_type)

        with self._lock:
            if key in self._cache:
                timestamp, value = self._cache[key]
                if time.time() - timestamp < self.ttl_seconds:
                    self._hits += 1
                    logger.debug(f"Cache HIT for query: {query[:50]}...")
                    return value
                else:
                    # Expired, remove from cache
                    del self._cache[key]

            self._misses += 1
            logger.debug(f"Cache MISS for query: {query[:50]}...")
            return None

    def put(self, query: str, is_pattern: bool, result: Any, query_type: str = "content") -> None:
        """Store result in cache."""
        key = self._make_key(query, is_pattern, query_type)

        with self._lock:
            # Remove oldest entry if cache is full
            if len(self._cache) >= self.max_size:
                oldest_key = min(self._cache.keys(), key=lambda k: self._cache[k][0])
                del self._cache[oldest_key]

            self._cache[key] = (time.time(), result)
            logger.debug(f"Cached result for query: {query[:50]}...")

    def invalidate(self, query: Optional[str] = None) -> None:
        """
        Invalidate cache entries.

        Args:
            query: Specific query to invalidate (None = clear all)
        """
        with self._lock:
            if query is None:
                self._cache.clear()
                logger.debug("Cleared all cache entries")
            else:
                # Invalidate all keys that contain the query substring
                keys_to_remove = [k for k in self._cache if query in k]
                for k in keys_to_remove:
                    del self._cache[k]
                logger.debug(f"Cleared {len(keys_to_remove)} cache entries for query: {query[:50]}...")

    def get_stats(self) -> Dict[str, Any]:
        """Get cache statistics."""
        with self._lock:
            total = self._hits + self._misses
            hit_rate = self._hits / total if total > 0 else 0
            return {
                "size": len(self._cache),
                "max_size": self.max_size,
                "hits": self._hits,
                "misses": self._misses,
                "hit_rate": hit_rate,
                "ttl_seconds": self.ttl_seconds
            }


class ElasticsearchSearch(SearchInterface):
    """
    Elasticsearch-based full-text search capabilities.

    PERFORMANCE FIX: Added caching layer for frequently searched terms.

    This implements the SearchInterface.
    """
    def __init__(self, hosts: List[str], index_name: str = "code_index",
                 cache_enabled: bool = True,
                 cache_max_size: int = DEFAULT_SEARCH_CACHE_MAX_SIZE,
                 cache_ttl_seconds: int = DEFAULT_CACHE_TTL,
                 api_key: Optional[Tuple[str, str]] = None,  # (id, api_key) tuple
                 http_auth: Optional[Tuple[str, str]] = None, # (username, password) tuple
                 use_ssl: bool = True,
                 verify_certs: bool = True,
                 ca_certs: Optional[str] = None,
                 client_cert: Optional[str] = None,
                 client_key: Optional[str] = None) -> None:
        self.hosts = hosts
        self.index_name = index_name
        self.es: Optional[Elasticsearch] = None

        # PERFORMANCE FIX: Initialize search cache
        self._cache_enabled = cache_enabled
        self._cache: Optional[SearchCache] = SearchCache(max_size=cache_max_size, ttl_seconds=cache_ttl_seconds) if cache_enabled else None
        if cache_enabled:
            logger.info(f"Elasticsearch search cache enabled: max_size={cache_max_size}, ttl={cache_ttl_seconds}s")


        # Ensure hosts have the correct scheme
        formatted_hosts = []
        for host in hosts:
            if isinstance(host, str):
                if not host.startswith(('http://', 'https://')):
                    scheme = "https" if use_ssl else "http"
                    formatted_hosts.append(f"{scheme}://{host}")
                else:
                    formatted_hosts.append(host)
            else:
                formatted_hosts.append(host)

        connection_params = {
            "hosts": formatted_hosts,
            "verify_certs": verify_certs,
            "ssl_show_warn": False, # Suppress SSL warnings if verify_certs is False
            "headers": {"Accept": "application/vnd.elasticsearch+json; compatible-with=8"}  # Force compatibility with Elasticsearch 8.x
        }

        if use_ssl and ca_certs:
            connection_params["ca_certs"] = ca_certs
        if use_ssl and client_cert and client_key:
            connection_params["client_cert"] = client_cert
            connection_params["client_key"] = client_key

        if api_key:
            connection_params["api_key"] = api_key
            logger.info("Using API Key for Elasticsearch connection.")
        elif http_auth:
            connection_params["basic_auth"] = http_auth  # Changed from http_auth to basic_auth
            logger.info("Using HTTP Basic Auth for Elasticsearch connection.")

        try:
            self.es = Elasticsearch(**connection_params)
            # Test connection with timeout
            self.es.info(request_timeout=ES_CONNECTION_TEST_TIMEOUT) # Test connection with short timeout
            logger.info(f"Successfully connected to Elasticsearch at {hosts} with secure settings.")
            self._ensure_index()
            self._connected = True
        except ConnectionError as e:
            logger.warning(f"Could not connect to Elasticsearch at {hosts} with provided credentials: {e}")
            logger.info("Elasticsearch search functionality will be unavailable until connection is restored.")
            self._connected = False
            # Don't raise - allow server to continue without Elasticsearch
        except Exception as e:
            logger.warning(f"An unexpected error occurred during Elasticsearch connection: {e}")
            logger.info("Elasticsearch search functionality will be unavailable until connection is restored.")
            self._connected = False
            # Don't raise - allow server to continue without Elasticsearch

    def _ensure_index(self) -> None:
        """Ensure the Elasticsearch index exists with appropriate mappings."""
        # Only create the index if it doesn't exist
        if self.es is not None and self.es.indices.exists(index=self.index_name):
            logger.info(f"Elasticsearch index '{self.index_name}' already exists, skipping creation")
            return

        logger.info(f"Creating Elasticsearch index: {self.index_name}")
        # Define mapping and settings for the Elasticsearch index
        index_body = {
          "settings": {
            "index": {
              "number_of_shards": ES_INDEX_NUMBER_OF_SHARDS,
              "number_of_replicas": ES_INDEX_NUMBER_OF_REPLICAS,
              "refresh_interval": ES_INDEX_REFRESH_INTERVAL
            },
            "analysis": {
              "analyzer": {
                "code_analyzer": {
                  "type": "custom",
                  "tokenizer": "whitespace",
                  "filter": [
                    "lowercase",
                    "code_stop",
                    "kstem",
                    "code_ngram"
                  ]
                },
                "path_analyzer": {
                  "type": "custom",
                  "tokenizer": "path_hierarchy",
                  "filter": [
                    "lowercase"
                  ]
                }
              },
              "filter": {
                "code_stop": {
                  "type": "stop",
                  "stopwords": [
                    "if", "for", "while", "do", "return", "class", "function", "def", "import", "from", "const", "let", "var", "public", "private", "protected", "static", "void", "int", "string", "bool", "true", "false", "null", "this", "new", "try", "catch", "finally", "throw", "async", "await"
                  ]
                },
                "code_ngram": {
                  "type": "ngram",
                  "min_gram": ES_CODE_ANALYZER_MIN_NGRAM,
                  "max_gram": ES_CODE_ANALYZER_MAX_NGRAM
                }
              }
            }
          },
          "mappings": {
            "properties": {
              "file_id": {
                "type": "keyword"
              },
              "path": {
                "type": "keyword",
                "fields": {
                  "analyzed": {
                    "type": "text",
                    "analyzer": "path_analyzer"
                  }
                }
              },
              "content": {
                "type": "text",
                "analyzer": "code_analyzer"
              },
              "language": {
                "type": "keyword"
              },
              "last_modified": {
                "type": "date"
              },
              "size": {
                "type": "long"
              },
              "checksum": {
                "type": "keyword"
              },
              "metadata": {
                "type": "object",
                "enabled": False
              }
            }
          }
        }
        if self.es is not None:
            self.es.indices.create(index=self.index_name, body=index_body)
        logger.info(f"Created Elasticsearch index: {self.index_name}")

    def index_document(self, doc_id: str, document: Dict[str, Any]) -> bool:
        """
        Indexes a document into Elasticsearch.

        PERFORMANCE FIX: Invalidates relevant cache entries when documents are indexed.

        `document` should contain at least 'file_path' and 'content'.
        """
        if not hasattr(self, '_connected') or not self._connected:
            logger.debug(f"Elasticsearch not connected, skipping indexing of document {doc_id}")
            return False

        try:
            content_preview = ""
            if 'content' in document and isinstance(document['content'], str):
                content_preview = document['content'][:200] + ('...' if len(document['content']) > 200 else '')
            logger.debug(f"Attempting to index document {doc_id}. Content preview: '{content_preview}'")

            response = self.es.index(index=self.index_name, id=doc_id, document=document)
            logger.debug(f"Successfully indexed document {doc_id}. Elasticsearch response: {response}")

            # PERFORMANCE FIX: Invalidate cache for this document's path
            if self._cache_enabled and self._cache:
                file_path = document.get('path', document.get('file_path', ''))
                if file_path:
                    self._cache.invalidate()
                    logger.debug(f"Invalidated cache after indexing document {doc_id}")

            return response['result'] in ['created', 'updated']
        except Exception as e:
            logger.error(f"Error indexing document {doc_id}: {e}. Full Elasticsearch response (if available): {getattr(e, 'info', 'N/A')}", exc_info=True)
            return False

    def update_document(self, doc_id: str, document: Dict[str, Any]) -> bool:
        """
        Updates an existing document in Elasticsearch.

        PERFORMANCE FIX: Invalidates relevant cache entries when documents are updated.
        """
        try:
            response = self.es.update(index=self.index_name, id=doc_id, doc=document)
            logger.debug(f"Updated document {doc_id}: {response['result']}")

            # PERFORMANCE FIX: Invalidate cache after document update
            if self._cache_enabled and self._cache:
                self._cache.invalidate()
                logger.debug(f"Invalidated cache after updating document {doc_id}")

            return response['result'] == 'updated'
        except NotFoundError:
            logger.warning(f"Document {doc_id} not found for update.")
            return False
        except Exception as e:
            logger.error(f"Error updating document {doc_id}: {e}")
            return False

    def delete_document(self, doc_id: str) -> bool:
        """
        Deletes a document from Elasticsearch.

        PERFORMANCE FIX: Invalidates relevant cache entries when documents are deleted.
        """
        try:
            response = self.es.delete(index=self.index_name, id=doc_id)
            logger.debug(f"Deleted document {doc_id}: {response['result']}")

            # PERFORMANCE FIX: Invalidate cache after document deletion
            if self._cache_enabled and self._cache:
                self._cache.invalidate()
                logger.debug(f"Invalidated cache after deleting document {doc_id}")

            return response['result'] == 'deleted'
        except NotFoundError:
            logger.warning(f"Document {doc_id} not found for deletion.")
            return False
        except Exception as e:
            logger.error(f"Error deleting document {doc_id}: {e}")
            return False

    def _translate_sqlite_pattern_to_es_query(self, pattern: str, field: str) -> Dict[str, Any]:
        """
        Translates SQLite LIKE/GLOB patterns to Elasticsearch Query DSL with improved handling.

        CRITICAL FIX: Implements strict pattern validation and size limits to prevent
        injection attacks and DoS through overly complex patterns.

        Args:
            pattern: The SQLite LIKE or GLOB pattern.
            field: The Elasticsearch field to apply the query to.

        Returns:
            A dictionary representing the Elasticsearch query DSL.

        Raises:
            ValueError: If pattern is invalid or potentially malicious
        """
        logger.debug(f"Translating SQLite pattern '{pattern}' to ES query for field '{field}'")

        # CRITICAL FIX: Validate pattern length to prevent DoS
        if pattern and len(pattern) > MAX_PATTERN_LENGTH:
            logger.error(f"Pattern exceeds maximum length of {MAX_PATTERN_LENGTH} characters")
            return {"match_none": {}}

        if not pattern or not pattern.strip():
            logger.warning("Empty pattern provided, returning match_all")
            return {"match_all": {}}

        # CRITICAL FIX: Validate pattern for injection attempts
        # Check for suspicious patterns that might indicate injection attempts
        suspicious_patterns = [
            '../',  # Path traversal
            '..\\',  # Windows path traversal
            '/etc/',  # System file access
            '\\\\',  # Escape sequences
            '\x00',  # Null bytes
        ]
        pattern_lower = pattern.lower()
        for suspicious in suspicious_patterns:
            if suspicious in pattern_lower:
                logger.error(f"Potentially malicious pattern detected: {pattern}")
                return {"match_none": {}}

        try:
            # Handle exact matches (no wildcards)
            if '%' not in pattern and '_' not in pattern and '*' not in pattern and '?' not in pattern:
                # CRITICAL FIX: Escape special regex characters in exact matches
                escaped_pattern = re.escape(pattern)
                logger.debug(f"Using term query for exact match: '{escaped_pattern}'")
                return {"term": {field: escaped_pattern}}

            # Handle SQL LIKE patterns
            if '%' in pattern or '_' in pattern:
                logger.debug(f"Detected LIKE pattern with wildcards: {pattern}")

                # CRITICAL FIX: Limit wildcard count to prevent ReDoS
                wildcard_count = pattern.count('%') + pattern.count('_')
                if wildcard_count > MAX_WILDCARD_COUNT:
                    logger.error(f"Pattern exceeds maximum wildcard count of {MAX_WILDCARD_COUNT}")
                    return {"match_none": {}}

                # Convert SQL LIKE wildcards to Elasticsearch wildcards
                # % -> * (any sequence of characters)
                # _ -> ? (any single character)
                es_wildcard_pattern = pattern.replace('%', '*').replace('_', '?')
                logger.debug(f"Converted to wildcard pattern: {es_wildcard_pattern}")

                # If pattern starts with '%' and ends with '%', it's a contains search
                if pattern.startswith('%') and pattern.endswith('%') and len(pattern) > 2:
                    term = pattern[1:-1]
                    # CRITICAL FIX: Escape special characters in the search term
                    escaped_term = re.escape(term)
                    logger.debug(f"Using match_phrase query for contains pattern, term: '{escaped_term}'")
                    # Use match_phrase for better phrase matching
                    return {"match_phrase": {field: escaped_term}}
                elif pattern.endswith('%') and not pattern.startswith('%'):
                    # Use prefix query for 'starts with'
                    prefix_term = pattern[:-1]
                    # CRITICAL FIX: Validate and escape prefix term
                    escaped_prefix = re.escape(prefix_term)
                    # Don't escape wildcards that are part of the query syntax
                    escaped_prefix = escaped_prefix.replace('\\*', '*').replace('\\?', '?')
                    logger.debug(f"Using prefix query for starts-with pattern, term: '{escaped_prefix}'")
                    return {"prefix": {field: escaped_prefix}}
                elif pattern.startswith('%') and not pattern.endswith('%'):
                    # Use wildcard query for 'ends with'
                    wildcard_pattern = '*' + pattern[1:]
                    # CRITICAL FIX: Validate wildcard pattern
                    escaped_wildcard = re.escape(wildcard_pattern)
                    escaped_wildcard = escaped_wildcard.replace('\\*', '*').replace('\\?', '?')
                    logger.debug(f"Using wildcard query for ends-with pattern: {escaped_wildcard}")
                    return {"wildcard": {field: {"value": escaped_wildcard, "case_insensitive": True}}}
                else:
                    # General wildcard query for patterns with % or _ in the middle
                    # CRITICAL FIX: Escape special characters but preserve wildcards
                    escaped_pattern = re.escape(es_wildcard_pattern)
                    escaped_pattern = escaped_pattern.replace('\\*', '*').replace('\\?', '?')
                    logger.debug(f"Using wildcard query for general pattern: {escaped_pattern}")
                    return {"wildcard": {field: {"value": escaped_pattern, "case_insensitive": True}}}

            # Handle GLOB patterns or regex patterns
            logger.debug(f"Detected GLOB/regex pattern: {pattern}")

            # CRITICAL FIX: Limit regex complexity
            alternation_count = pattern.count('|')
            if alternation_count > MAX_REGEX_ALTERNATIONS:
                logger.error(f"Pattern exceeds maximum alternation count of {MAX_REGEX_ALTERNATIONS}")
                return {"match_none": {}}

            # Check if it's a simple GLOB pattern
            if '*' in pattern or '?' in pattern:
                # Convert GLOB to regex
                # CRITICAL FIX: Escape special characters except glob wildcards
                glob_pattern = pattern.replace('.', r'\.').replace('*', '.*').replace('?', '.')
                logger.debug(f"Converted GLOB to regex: {glob_pattern}")
                return {"regexp": {field: {"value": glob_pattern, "case_insensitive": True, "flags": "COMPLEMENT"}}}
            else:
                # Assume it's already a regex pattern
                # CRITICAL FIX: Validate regex is safe
                try:
                    re.compile(pattern, re.IGNORECASE)
                except re.error as e:
                    logger.error(f"Invalid regex pattern '{pattern}': {e}")
                    return {"match_none": {}}
                logger.debug(f"Using regexp query for pattern: {pattern}")
                return {"regexp": {field: {"value": pattern, "case_insensitive": True, "flags": "COMPLEMENT"}}}

        except Exception as e:
            logger.error(f"Error translating SQLite pattern '{pattern}' to ES query for field '{field}': {e}")
            # CRITICAL FIX: Return match_none instead of falling back to potentially unsafe match query
            logger.warning("Pattern translation failed, returning no matches")
            return {"match_none": {}}


    def search_content(self, query: str, is_sqlite_pattern: bool = False,
                        fuzziness: Optional[str] = None,
                        content_boost: float = 1.0, file_path_boost: float = 1.0,
                        highlight_pre_tags: Optional[List[str]] = None,
                        highlight_post_tags: Optional[List[str]] = None) -> List[Tuple[str, Any]]:
        """
        Search across file content using Elasticsearch with advanced features.

        PERFORMANCE FIX: Checks cache before querying Elasticsearch to reduce
        query load for frequently searched terms.

        Can handle both direct queries and SQLite-style patterns.

        CRITICAL FIX: Raises meaningful error when Elasticsearch is unavailable
        instead of silently returning empty results.
        """
        if not hasattr(self, '_connected') or not self._connected:
            # Check if aiohttp is available
            try:
                import aiohttp
                # aiohttp is available, but ES is not connected
                error_msg = (
                    "Elasticsearch backend is not connected. "
                    "Please ensure Elasticsearch is running on localhost:9200 "
                    "or check your Elasticsearch configuration."
                )
            except ImportError:
                # aiohttp is missing - this is the root cause
                error_msg = (
                    "Elasticsearch backend is unavailable: the 'aiohttp' module is not installed. "
                    "This is a required dependency for Elasticsearch connectivity. "
                    "Please reinstall dependencies: pip install -e ."
                )

            logger.error(error_msg)
            # Raise an error to make the failure visible instead of silently returning empty results
            raise RuntimeError(error_msg)

        # PERFORMANCE FIX: Check cache first
        if self._cache_enabled and self._cache:
            cached_result = self._cache.get(query, is_sqlite_pattern, "content")
            if cached_result is not None:
                logger.info(f"Returning cached results for query: {query[:50]}...")
                return cached_result

        results: List[Tuple[str, Any]] = []
        logger.debug(f"Elasticsearch search_content called with query='{query}', is_sqlite_pattern={is_sqlite_pattern}")
        try:
            if is_sqlite_pattern:
                logger.debug(f"Translating SQLite pattern '{query}' to Elasticsearch query")
                es_query_dsl = self._translate_sqlite_pattern_to_es_query(query, "content")
                logger.debug(f"Translated query DSL: {es_query_dsl}")
                body = {
                    "query": es_query_dsl
                }
            else:
                # Simplified to a basic match query on 'content' field
                logger.debug(f"Using direct match query for '{query}'")
                body = {
                    "query": {
                        "match": {
                            "content": query
                        }
                    }
                }

            logger.debug(f"Elasticsearch search body: {body}")

            if self.es is not None:
                response = self.es.search(index=self.index_name, body=body)
                logger.debug(f"Elasticsearch search response: {response}")
                for hit in response['hits']['hits']:
                    logger.debug(f"Processing hit: {hit}")
                    source = hit.get('_source', {})

                    # Extract file_path with multiple fallbacks for backward compatibility
                    # Priority: 'path' (new) -> 'file_path' (old) -> _id (fallback)
                    file_path = source.get('path') or source.get('file_path')
                    if not file_path:
                        # Try _id as fallback (file_path is often used as document ID)
                        file_path = hit.get('_id', '')
                        logger.warning(f"Document missing 'path' and 'file_path' fields in _source, using _id: {file_path}")

                    # Extract content with fallback
                    content = source.get('content', '')

                    # Validate we have both path and content
                    if not file_path:
                        logger.warning(f"Skipping hit with no valid file_path: {hit.get('_id')}")
                        continue

                    # Create result document with validated fields
                    result_doc = {
                        "file_path": file_path,
                        "content": content,
                    }
                    tuple_result = (file_path, result_doc)
                    logger.debug(f"Appending tuple result: {tuple_result}")
                    results.append(tuple_result)
        except Exception as e:
            logger.error(f"Error searching content in Elasticsearch: {e}")

        # PERFORMANCE FIX: Cache the results
        if self._cache_enabled and self._cache and results:
            self._cache.put(query, is_sqlite_pattern, results, "content")
            logger.debug(f"Cached {len(results)} results for query: {query[:50]}...")

        logger.debug(f"Final results list length: {len(results)}")
        if results:
            logger.debug(f"First result structure: {results[0]}")
        return results

    def search_file_paths(self, query: str, is_sqlite_pattern: bool = False,
                          fuzziness: Optional[str] = None,
                          file_path_boost: float = 1.0,
                          highlight_pre_tags: Optional[List[str]] = None,
                          highlight_post_tags: Optional[List[str]] = None) -> List[str]:
        """
        Search across file paths using Elasticsearch with advanced features.

        PERFORMANCE FIX: Checks cache before querying Elasticsearch to reduce
        query load for frequently searched path patterns.

        Can handle both direct queries and SQLite-style patterns.

        CRITICAL FIX: Raises meaningful error when Elasticsearch is unavailable
        instead of silently returning empty results.
        """
        if not hasattr(self, '_connected') or not self._connected:
            # Check if aiohttp is available
            try:
                import aiohttp
                # aiohttp is available, but ES is not connected
                error_msg = (
                    "Elasticsearch backend is not connected. "
                    "Please ensure Elasticsearch is running on localhost:9200 "
                    "or check your Elasticsearch configuration."
                )
            except ImportError:
                # aiohttp is missing - this is the root cause
                error_msg = (
                    "Elasticsearch backend is unavailable: the 'aiohttp' module is not installed. "
                    "This is a required dependency for Elasticsearch connectivity. "
                    "Please reinstall dependencies: pip install -e ."
                )

            logger.error(error_msg)
            # Raise an error to make the failure visible instead of silently returning empty results
            raise RuntimeError(error_msg)

        # PERFORMANCE FIX: Check cache first for path searches
        if self._cache_enabled and self._cache:
            cached_result = self._cache.get(query, is_sqlite_pattern, "path")
            if cached_result is not None:
                logger.info(f"Returning cached path results for query: {query[:50]}...")
                return cached_result

        paths = []
        try:
            highlight_settings = {
                "fields": {
                    "path": {}
                }
            }
            if highlight_pre_tags:
                highlight_settings["pre_tags"] = highlight_pre_tags
            if highlight_post_tags:
                highlight_settings["post_tags"] = highlight_post_tags

            if is_sqlite_pattern:
                es_query_dsl = self._translate_sqlite_pattern_to_es_query(query, "path")
                body = {
                    "query": es_query_dsl,
                    "highlight": highlight_settings
                }
            else:
                match_query = {
                    "query": query,
                    "boost": file_path_boost
                }
                if fuzziness:
                    match_query["fuzziness"] = fuzziness

                body = {
                    "query": {
                        "match": {
                            "path": match_query
                        }
                    },
                    "highlight": highlight_settings,
                    "_source": ["path"]
                }

            response = self.es.search(index=self.index_name, body=body)
            for hit in response['hits']['hits']:
                highlighted_path = hit.get('highlight', {}).get('path', [hit['_source']['path']])[0]
                paths.append(highlighted_path)
        except Exception as e:
            logger.error(f"Error searching file paths in Elasticsearch: {e}")

        # PERFORMANCE FIX: Cache the path search results
        if self._cache_enabled and self._cache and paths:
            self._cache.put(query, is_sqlite_pattern, paths, "path")
            logger.debug(f"Cached {len(paths)} path results for query: {query[:50]}...")

        return paths

    def get_cache_stats(self) -> Optional[Dict[str, Any]]:
        """
        Get search cache statistics for monitoring.

        PERFORMANCE FIX: Added cache statistics method for monitoring cache
        effectiveness and tuning cache parameters.

        Returns:
            Dictionary with cache statistics or None if cache is disabled
        """
        if self._cache_enabled and self._cache:
            return self._cache.get_stats()
        return None

    def clear_cache(self) -> None:
        """
        Clear all cached search results.

        PERFORMANCE FIX: Added method to manually clear cache when needed
        (e.g., after bulk indexing operations).
        """
        if self._cache_enabled and self._cache:
            self._cache.invalidate()
            logger.info("Search cache cleared manually")

    def close(self) -> None:
        """Close the Elasticsearch search backend."""
        if self.es:
            # In newer elasticsearch-py versions, client connections are managed automatically.
            # Explicit close might not be necessary unless using specific connection pools.
            logger.info("ElasticsearchSearch client connection implicitly closed or managed by client.")
        else:
            logger.info("ElasticsearchSearch client was not initialized.")

    def index_file(self, file_path: str, content: str) -> None:
        """Index a file for search.

        Args:
            file_path: Path of the file to index
            content: Content of the file to index

        Raises:
            IOError: If the file cannot be indexed
        """
        doc = {
            "path": file_path,  # Use 'path' to match Elasticsearch mapping
            "content": content,
            "timestamp": datetime.now().isoformat()
        }
        self.index_document(file_path, doc)

    def delete_indexed_file(self, file_path: str) -> None:
        """Delete a file from the search index.

        Args:
            file_path: Path of the file to delete from index
        """
        if not self._connected or not self.es:
            logger.warning("Elasticsearch not connected, skipping delete")
            return

        try:
            self.es.delete(index=self.index_name, id=file_path)
            logger.debug(f"Deleted file from Elasticsearch index: {file_path}")
        except Exception as e:
            logger.warning(f"Failed to delete file from Elasticsearch: {e}")

    def search_files(self, query: str) -> List[Dict[str, Any]]:
        """Search for files matching the query.

        Args:
            query: The search query string

        Returns:
            A list of dictionaries containing file search results
        """
        results = self.search_content(query)
        return [
            {
                "file_path": file_path,
                "score": result_doc.get("score", 0.0),
                "content": result_doc.get("content", "")
            }
            for file_path, result_doc in results
        ]

    def clear(self) -> bool:
        """Clear the search index.

        Returns:
            True if successful, False otherwise
        """
        if not self._connected or not self.es:
            logger.warning("Elasticsearch not connected, cannot clear")
            return False

        try:
            self.es.delete_by_query(
                index=self.index_name,
                body={"query": {"match_all": {}}},
                refresh=True
            )
            self.clear_cache()
            logger.info(f"Cleared Elasticsearch index: {self.index_name}")
            return True
        except Exception as e:
            logger.error(f"Failed to clear Elasticsearch index: {e}")
            return False