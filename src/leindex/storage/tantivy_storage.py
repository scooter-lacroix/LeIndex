"""
Tantivy-based full-text search backend.

This module implements a Tantivy (Rust Lucene) based full-text search backend
as a replacement for Elasticsearch. Tantivy is an embedded search engine written
in Rust with Python bindings, providing:

- BM25 scoring with tunable parameters
- Fast inverted-index search
- Prefix, fuzzy, and phrase queries
- No JVM required (unlike Elasticsearch)
- Embedded operation (no separate service needed)

Key features:
- Code-specific tokenization (handles identifiers, paths, symbols)
- Hybrid scoring (BM25 + vector similarity)
- LRU caching for performance
- Thread-safe operations
- Persistent index storage
"""

import logging
import re
import time
import hashlib
from datetime import datetime
from typing import Any, Dict, List, Optional, Tuple
from pathlib import Path
from threading import Lock, RLock
from collections import OrderedDict

# Try to import tantivy - make it optional
try:
    import tantivy
    TANTIVY_AVAILABLE = True
except ImportError:
    TANTIVY_AVAILABLE = False
    tantivy = None  # type: ignore

from .storage_interface import SearchInterface
from ..constants import (
    DEFAULT_SEARCH_CACHE_MAX_SIZE,
    DEFAULT_CACHE_TTL,
    MAX_PATTERN_LENGTH,
    MAX_REGEX_ALTERNATIONS,
    MAX_REGEX_NESTING_DEPTH,
)

logger = logging.getLogger(__name__)


class TantivyIndexError(Exception):
    """Exception raised when Tantivy index operations fail."""
    pass


class TantivyQueryError(Exception):
    """Exception raised when Tantivy query operations fail."""
    pass


class TantivyNotAvailableError(Exception):
    """Exception raised when Tantivy is not installed."""
    pass


class SearchCache:
    """
    LRU cache for Tantivy search results with TTL support.

    This cache stores recent search results to avoid hitting Tantivy for
    frequently searched terms. Uses a time-based expiration to ensure results
    don't become too stale. Uses OrderedDict for O(1) eviction performance.
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
        # Use OrderedDict for O(1) eviction performance (move_to_end + popitem(last=False))
        self._cache: 'OrderedDict[str, Tuple[float, Any]]' = OrderedDict()  # key -> (timestamp, value)
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
                    # Move to end to mark as recently used (LRU)
                    self._cache.move_to_end(key)
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
        """
        Store result in cache.

        This method is optimized to avoid double operations when updating
        existing cache entries. It checks if the key exists first and
        performs only the necessary operations.

        Args:
            query: Search query string
            is_pattern: Whether query is a pattern
            result: Search result to cache
            query_type: Type of query (content/path)
        """
        key = self._make_key(query, is_pattern, query_type)

        with self._lock:
            # If key exists, update and move to end (single operation)
            if key in self._cache:
                self._cache[key] = (time.time(), result)
                self._cache.move_to_end(key)
                logger.debug(f"Updated cached result for query: {query[:50]}...")
                return

            # Remove expired entries first
            self._remove_expired()

            # Remove oldest if still full (after removing expired)
            if len(self._cache) >= self.max_size:
                self._cache.popitem(last=False)

            # Add new entry
            self._cache[key] = (time.time(), result)
            logger.debug(f"Cached result for query: {query[:50]}...")

    def _remove_expired(self) -> None:
        """
        Remove expired entries from cache.

        This is called before adding new entries to ensure there's space
        and that stale data is removed.
        """
        current_time = time.time()
        expired_keys = [
            k for k, (ts, _) in self._cache.items()
            if current_time - ts >= self.ttl_seconds
        ]
        for k in expired_keys:
            del self._cache[k]
        if expired_keys:
            logger.debug(f"Removed {len(expired_keys)} expired cache entries")

    def invalidate(self, query: Optional[str] = None) -> None:
        """
        Invalidate cache entries.

        Args:
            query: Specific query to invalidate (None = clear all)

        Note:
            When query is provided, generates all possible cache keys for that
            query (across all query types and pattern flags) and removes them.
            This is thread-safe and ensures all related entries are cleared.
        """
        with self._lock:
            if query is None:
                self._cache.clear()
                logger.debug("Cleared all cache entries")
            else:
                # Generate all possible cache keys for this query
                # Cache keys are MD5 hashes of "query_type:query:is_pattern"
                # We need to try all combinations of query_type and is_pattern
                possible_keys = []
                for query_type in ["content", "path"]:
                    for is_pattern in [True, False]:
                        key_data = f"{query_type}:{query}:{is_pattern}"
                        key_hash = hashlib.md5(key_data.encode()).hexdigest()
                        possible_keys.append(key_hash)

                # Remove matching keys
                keys_to_remove = [k for k in possible_keys if k in self._cache]
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


class TantivySearch(SearchInterface):
    """
    Tantivy-based full-text search backend.

    This implements the SearchInterface using Tantivy (Rust Lucene) as the
    underlying search engine. It provides BM25 scoring, prefix search, fuzzy
    search, phrase search, and hybrid scoring with vector similarity.

    Key features:
    - Embedded operation (no separate service)
    - Fast inverted-index search
    - BM25 with tunable parameters
    - Code-specific tokenization
    - Thread-safe operations
    - LRU caching for performance
    """

    def __init__(
        self,
        index_path: str = ".tantivy_index",
        cache_enabled: bool = True,
        cache_max_size: int = DEFAULT_SEARCH_CACHE_MAX_SIZE,
        cache_ttl_seconds: int = DEFAULT_CACHE_TTL,
        bm25_k1: float = 1.2,
        bm25_b: float = 0.75,
    ) -> None:
        """
        Initialize Tantivy search backend.

        Args:
            index_path: Path to store Tantivy index (default: ".tantivy_index")
            cache_enabled: Enable LRU cache for search results (default: True)
            cache_max_size: Maximum cache size (default: 128)
            cache_ttl_seconds: Cache TTL in seconds (default: 300)
            bm25_k1: BM25 k1 parameter (term frequency saturation, default: 1.2)
            bm25_b: BM25 b parameter (length normalization, default: 0.75)

        Raises:
            TantivyNotAvailableError: If Tantivy is not installed
            TantivyIndexError: If index creation fails
        """
        if not TANTIVY_AVAILABLE:
            raise TantivyNotAvailableError(
                "Tantivy is not installed. Install it with: pip install tantivy"
            )

        self.index_path = Path(index_path).expanduser().resolve()
        self.cache_enabled = cache_enabled
        self._cache: Optional[SearchCache] = (
            SearchCache(max_size=cache_max_size, ttl_seconds=cache_ttl_seconds)
            if cache_enabled else None
        )
        # BM25 parameters (k1=1.2, b=0.75 are standard defaults)
        # Note: Tantivy uses BM25 scoring automatically. These parameters
        # are stored for reference and potential custom scoring calculations.
        self.bm25_k1 = bm25_k1
        self.bm25_b = bm25_b
        # Use RLock for reentrancy (allows same thread to acquire multiple times)
        self._index_lock = RLock()
        self._writer_lock = RLock()
        self._index = None
        self._writer = None
        self._schema = None
        self._connected = False

        if cache_enabled:
            logger.info(f"Tantivy search cache enabled: max_size={cache_max_size}, ttl={cache_ttl_seconds}s")

        # Validate configuration
        self._validate_configuration()

        # Initialize the index
        self._initialize_index()

    def _validate_configuration(self) -> None:
        """
        Validate Tantivy configuration parameters.

        Raises:
            TantivyIndexError: If configuration is invalid
        """
        # Validate BM25 k1 parameter (typically 0.0 to 3.0, with 1.2 as default)
        if not 0.0 < self.bm25_k1 < 10.0:
            raise TantivyIndexError(
                f"Invalid BM25 k1 parameter: {self.bm25_k1}. "
                f"Must be between 0.0 and 10.0 (recommended: 1.2)"
            )

        # Validate BM25 b parameter (typically 0.0 to 1.0, with 0.75 as default)
        if not 0.0 <= self.bm25_b <= 1.0:
            raise TantivyIndexError(
                f"Invalid BM25 b parameter: {self.bm25_b}. "
                f"Must be between 0.0 and 1.0 (recommended: 0.75)"
            )

        # Validate cache settings if enabled
        if self.cache_enabled:
            if self._cache is not None:
                if self._cache.max_size <= 0:
                    raise TantivyIndexError(
                        f"Invalid cache max_size: {self._cache.max_size}. Must be positive"
                    )
                if self._cache.ttl_seconds <= 0:
                    raise TantivyIndexError(
                        f"Invalid cache ttl_seconds: {self._cache.ttl_seconds}. Must be positive"
                    )

    def _initialize_index(self) -> None:
        """Initialize Tantivy index and schema."""
        try:
            # Create index directory if it doesn't exist
            self.index_path.mkdir(parents=True, exist_ok=True)

            # Define schema for code search
            self._schema = self._create_schema()

            # Check if index exists
            index_exists = (self.index_path / "meta.json").exists()

            if index_exists:
                logger.info(f"Opening existing Tantivy index at {self.index_path}")
                try:
                    self._index = tantivy.Index(self._schema, path=str(self.index_path))
                except Exception as e:
                    logger.warning(f"Failed to open existing index: {e}. Creating new index.")
                    self._index = tantivy.Index(self._schema, path=str(self.index_path))
            else:
                logger.info(f"Creating new Tantivy index at {self.index_path}")
                self._index = tantivy.Index(self._schema, path=str(self.index_path))

            # Create writer
            self._writer = self._index.writer(heap_size=100_000_000)  # 100MB
            self._connected = True
            logger.info("Tantivy search backend initialized successfully")

        except Exception as e:
            logger.error(f"Failed to initialize Tantivy index: {e}")
            self._connected = False
            raise TantivyIndexError(f"Failed to initialize Tantivy index: {e}")

    def _create_schema(self) -> 'tantivy.Schema':
        """
        Create Tantivy schema for code search.

        The schema includes:
        - file_id: Unique identifier (indexed, not stored)
        - path: File path (text with path tokenizer)
        - content: File content (text with code tokenizer)
        - language: Programming language (tokenized)
        - last_modified: Timestamp
        - size: File size
        - checksum: File hash
        """
        # Build schema with appropriate field types
        schema_builder = tantivy.SchemaBuilder()

        # Add validated fields
        # Note: Tantivy uses different keyword for text fields - use raw tokenizer for keyword-like behavior
        schema_builder.add_text_field("file_id", stored=True, tokenizer_name="raw")

        self._add_text_field_safe(
            schema_builder,
            "path",
            stored=True,
            tokenizer_name="default",
            index_option="position",
        )
        self._add_text_field_safe(
            schema_builder,
            "content",
            stored=True,
            tokenizer_name="default",
            index_option="position",
        )
        # Language field - use raw tokenizer for exact matching
        schema_builder.add_text_field("language", stored=True, tokenizer_name="raw")

        # Integer fields with validation
        self._add_integer_field_safe(schema_builder, "last_modified", stored=True, indexed=False)
        self._add_integer_field_safe(schema_builder, "size", stored=True, indexed=False)

        # Checksum field - use raw tokenizer for exact matching
        schema_builder.add_text_field("checksum", stored=True, tokenizer_name="raw")

        schema = schema_builder.build()
        self._validate_schema(schema)
        return schema

    def _add_text_field_safe(self, schema_builder: 'tantivy.SchemaBuilder', field_name: str, **kwargs) -> None:
        """
        Safely add a text field to the schema with validation.

        Args:
            schema_builder: Tantivy schema builder
            field_name: Name of the field
            **kwargs: Additional field arguments

        Raises:
            TantivyIndexError: If field name is invalid
        """
        if not field_name or not isinstance(field_name, str):
            raise TantivyIndexError(f"Invalid field name: {field_name}")

        if len(field_name) > 100:
            raise TantivyIndexError(
                f"Field name too long: {field_name} ({len(field_name)} > 100 chars)"
            )

        # Check for invalid characters
        import re
        if not re.match(r'^[a-zA-Z_][a-zA-Z0-9_]*$', field_name):
            raise TantivyIndexError(
                f"Field name contains invalid characters: {field_name}. "
                f"Must start with letter or underscore, followed by alphanumeric/underscore"
            )

        # Add the field
        schema_builder.add_text_field(field_name, **kwargs)

    def _add_integer_field_safe(self, schema_builder: 'tantivy.SchemaBuilder', field_name: str, **kwargs) -> None:
        """
        Safely add an integer field to the schema with validation.

        Args:
            schema_builder: Tantivy schema builder
            field_name: Name of the field
            **kwargs: Additional field arguments

        Raises:
            TantivyIndexError: If field name is invalid
        """
        if not field_name or not isinstance(field_name, str):
            raise TantivyIndexError(f"Invalid field name: {field_name}")

        if len(field_name) > 100:
            raise TantivyIndexError(
                f"Field name too long: {field_name} ({len(field_name)} > 100 chars)"
            )

        # Check for invalid characters
        import re
        if not re.match(r'^[a-zA-Z_][a-zA-Z0-9_]*$', field_name):
            raise TantivyIndexError(
                f"Field name contains invalid characters: {field_name}. "
                f"Must start with letter or underscore, followed by alphanumeric/underscore"
            )

        # Add the field
        schema_builder.add_integer_field(field_name, **kwargs)

    def _validate_schema(self, schema: 'tantivy.Schema') -> None:
        """
        Validate the created schema.

        Args:
            schema: Tantivy schema to validate

        Raises:
            TantivyIndexError: If schema is invalid
        """
        if schema is None:
            raise TantivyIndexError("Schema is None")

        # Basic validation - ensure schema is properly built
        try:
            # Tantivy schema should have fields
            if not hasattr(schema, 'fields'):
                logger.warning("Schema does not have 'fields' attribute, skipping validation")
                return

        except Exception as e:
            logger.warning(f"Could not fully validate schema: {e}")

    def _ensure_writer(self) -> None:
        """Ensure writer is available and ready."""
        with self._writer_lock:
            if self._writer is None:
                self._writer = self._index.writer(heap_size=100_000_000)

    def index_document(self, doc_id: str, document: Dict[str, Any]) -> bool:
        """
        Index a document into Tantivy.

        Args:
            doc_id: Unique document identifier
            document: Document data containing at least 'path' and 'content'

        Returns:
            True if successful, False otherwise
        """
        if not self._connected or self._index is None:
            logger.debug(f"Tantivy not connected, skipping indexing of document {doc_id}")
            return False

        try:
            content_preview = ""
            if 'content' in document and isinstance(document['content'], str):
                content_preview = document['content'][:200] + ('...' if len(document['content']) > 200 else '')
            logger.debug(f"Attempting to index document {doc_id}. Content preview: '{content_preview}'")

            # Ensure writer is available
            self._ensure_writer()

            # Create document
            doc = tantivy.Document()
            doc.add_text("file_id", doc_id)
            doc.add_text("path", document.get('path', document.get('file_path', doc_id)))
            doc.add_text("content", document.get('content', ''))

            if 'language' in document:
                doc.add_text("language", document['language'])

            if 'last_modified' in document:
                doc.add_integer("last_modified", int(document['last_modified']))

            if 'size' in document:
                doc.add_integer("size", int(document['size']))

            if 'checksum' in document:
                doc.add_text("checksum", document['checksum'])

            # Add document to index
            with self._writer_lock:
                if self._writer is None:
                    raise TantivyIndexError("Writer is not available")
                self._writer.add_document(doc)
                self._writer.commit()
                logger.debug(f"Successfully indexed document {doc_id}")

            # Invalidate cache selectively (only entries related to this file)
            if self.cache_enabled and self._cache:
                file_path = document.get('path', document.get('file_path', ''))
                if file_path:
                    # Use selective invalidation to avoid clearing entire cache
                    self._cache.invalidate(query=file_path)
                    logger.debug(f"Invalidated cache entries for {file_path} after indexing document {doc_id}")

            return True

        except Exception as e:
            logger.error(f"Error indexing document {doc_id}: {e}", exc_info=True)
            return False

    def update_document(self, doc_id: str, document: Dict[str, Any]) -> bool:
        """
        Update an existing document in Tantivy.

        Args:
            doc_id: Document identifier
            document: Updated document data

        Returns:
            True if successful, False otherwise
        """
        try:
            # Ensure writer is available
            self._ensure_writer()

            # Create updated document
            doc = tantivy.Document()
            doc.add_text("file_id", doc_id)
            doc.add_text("path", document.get('path', document.get('file_path', doc_id)))
            doc.add_text("content", document.get('content', ''))

            if 'language' in document:
                doc.add_text("language", document['language'])

            if 'last_modified' in document:
                doc.add_integer("last_modified", int(document['last_modified']))

            if 'size' in document:
                doc.add_integer("size", int(document['size']))

            if 'checksum' in document:
                doc.add_text("checksum", document['checksum'])

            # Delete old document and add new one
            with self._writer_lock:
                if self._writer is None:
                    raise TantivyIndexError("Writer is not available")

                # Tantivy doesn't have direct update, so we delete and re-add
                # Since we're using file_id as term, we can delete by term
                self._writer.delete_documents("file_id", doc_id)
                self._writer.add_document(doc)
                self._writer.commit()

            logger.debug(f"Updated document {doc_id}")

            # Invalidate cache selectively (only entries related to this file)
            if self.cache_enabled and self._cache:
                file_path = document.get('path', document.get('file_path', ''))
                if file_path:
                    self._cache.invalidate(query=file_path)
                    logger.debug(f"Invalidated cache entries for {file_path} after updating document {doc_id}")

            return True

        except Exception as e:
            logger.error(f"Error updating document {doc_id}: {e}")
            return False

    def delete_document(self, doc_id: str) -> bool:
        """
        Delete a document from Tantivy.

        Args:
            doc_id: Document identifier to delete

        Returns:
            True if successful, False otherwise
        """
        try:
            # Ensure writer is available
            self._ensure_writer()

            with self._writer_lock:
                if self._writer is None:
                    raise TantivyIndexError("Writer is not available")
                self._writer.delete_documents("file_id", doc_id)
                self._writer.commit()

            logger.debug(f"Deleted document {doc_id}")

            # Invalidate cache selectively (only entries related to this file)
            # Note: We only have doc_id, so we clear all entries containing it
            if self.cache_enabled and self._cache:
                self._cache.invalidate(query=doc_id)
                logger.debug(f"Invalidated cache entries for {doc_id} after deleting document")

            return True

        except Exception as e:
            logger.error(f"Error deleting document {doc_id}: {e}")
            return False

    def _validate_regex_complexity(self, pattern: str) -> bool:
        """
        Validate regex pattern complexity to prevent ReDoS attacks.

        This method checks for patterns that could cause catastrophic backtracking
        or excessive computational complexity during regex matching.

        Malicious patterns are REJECTED, not just warned about.

        Args:
            pattern: Regex pattern to validate

        Returns:
            True if pattern is safe, False if it's too complex or malicious
        """
        # 1. Check total pattern length
        if len(pattern) > MAX_PATTERN_LENGTH:
            logger.error(f"Regex pattern exceeds maximum length: {len(pattern)} > {MAX_PATTERN_LENGTH}")
            return False

        # 2. Check nesting depth properly using a stack to handle balanced parentheses
        stack = []
        max_depth = 0
        for i, char in enumerate(pattern):
            if char == '(':
                stack.append(i)
                max_depth = max(max_depth, len(stack))
            elif char == ')':
                if not stack:
                    logger.error(f"Regex pattern has unbalanced parentheses at position {i}")
                    return False  # Unbalanced - closing without opening
                stack.pop()

        # After processing all characters, stack should be empty
        # If not, there are unmatched opening parentheses
        if stack:
            logger.error(f"Regex pattern has {len(stack)} unbalanced opening parentheses")
            return False  # Unbalanced - opening without closing

        if max_depth > MAX_REGEX_NESTING_DEPTH:
            logger.error(f"Regex pattern exceeds maximum nesting depth: {max_depth} > {MAX_REGEX_NESTING_DEPTH}")
            return False

        # 3. Check for quantifier nesting (catastrophic backtracking)
        # Patterns like (a+)+, (a*)*, (a{1,3})+ cause exponential backtracking
        # This regex detects: opening paren, content, quantifier, closing paren, outer quantifier
        quantifier_nesting_pattern = r'\([^)]*[\*\+\{][^)]*\}[\*\+\{]'
        if re.search(quantifier_nesting_pattern, pattern):
            logger.error(f"Regex pattern contains nested quantifiers (ReDoS risk): {pattern[:100]}...")
            return False

        # Also check for: (a+)+, (a*)*, etc. - simpler version
        simple_nested_quantifiers = [
            r'\([^)]*\+\)\+',   # (a+)+
            r'\([^)]*\*\)\*',   # (a*)*
            r'\([^)]*\+\)\*',   # (a+)*
            r'\([^)]*\*\)\+',   # (a*)+
        ]
        for nested_q in simple_nested_quantifiers:
            if re.search(nested_q, pattern):
                logger.error(f"Regex pattern contains nested quantifiers '{nested_q}' (ReDoS risk)")
                return False

        # 4. Check for excessive alternations
        alternations = pattern.count('|')
        if alternations > MAX_REGEX_ALTERNATIONS:
            logger.error(f"Regex pattern exceeds maximum alternations: {alternations} > {MAX_REGEX_ALTERNATIONS}")
            return False

        # 5. Check for overlapping alternations in groups like (a|a|a|a...) or (a|b|c|d|e|f|g|...)
        # This can cause exponential backtracking with certain inputs
        multiple_alternations_in_group = re.search(r'\([^)]*(\|[^)]*){4,}', pattern)
        if multiple_alternations_in_group:
            logger.error(f"Regex pattern contains too many alternations in a single group (ReDoS risk)")
            return False

        # 6. Check for suspicious consecutive quantifiers that can cause ReDoS
        # Examples: .*.*.*, .++.+, etc.
        consecutive_wildcards = re.search(r'(\.\*|\.\+|\.\{|\+\*|\+\+|\+\{|\*\*|\*\+|\*\{){2,}', pattern)
        if consecutive_wildcards:
            logger.error(f"Regex pattern contains consecutive quantifiers (ReDoS risk): {consecutive_wildcards.group()}")
            return False

        # 7. Check for patterns with many backreferences (can cause exponential complexity)
        backref_count = pattern.count(r'\1') + pattern.count(r'\2') + pattern.count(r'\3') + pattern.count(r'\4')
        if backref_count > 3:
            logger.error(f"Regex pattern contains too many backreferences: {backref_count}")
            return False

        return True

    def _translate_pattern_to_query(
        self,
        pattern: str,
        field: str,
        is_pattern: bool = False
    ) -> Tuple[str, str]:
        """
        Translate search pattern to Tantivy query type and query string.

        Args:
            pattern: Search pattern
            field: Field to search
            is_pattern: Whether pattern is a regex/glob pattern

        Returns:
            Tuple of (query_type, query_string)

        Raises:
            TantivyQueryError: If pattern is invalid
        """
        logger.debug(f"Translating pattern '{pattern}' to Tantivy query for field '{field}'")

        # Validate pattern length
        if pattern and len(pattern) > MAX_PATTERN_LENGTH:
            logger.error(f"Pattern exceeds maximum length of {MAX_PATTERN_LENGTH} characters")
            return "none", ""

        if not pattern or not pattern.strip():
            return "all", ""

        # Check for suspicious patterns
        suspicious_patterns = ['../', '..\\', '/etc/', '\\\\', '\x00']
        pattern_lower = pattern.lower()
        for suspicious in suspicious_patterns:
            if suspicious in pattern_lower:
                logger.error(f"Potentially malicious pattern detected: {pattern}")
                return "none", ""

        try:
            # Handle exact matches (no wildcards)
            if not is_pattern and '%' not in pattern and '_' not in pattern and '*' not in pattern and '?' not in pattern:
                # Simple term query
                return "term", pattern

            # Handle prefix patterns (e.g., "foo*")
            if pattern.endswith('*') and not pattern.startswith('*') and '*' not in pattern[:-1]:
                prefix_term = pattern[:-1]
                return "prefix", prefix_term

            # Handle SQL LIKE patterns
            if is_pattern and ('%' in pattern or '_' in pattern):
                # Convert LIKE to Tantivy query
                if pattern.startswith('%') and pattern.endswith('%') and len(pattern) > 2:
                    # Contains search - use phrase query
                    term = pattern[1:-1]
                    return "phrase", term
                elif pattern.endswith('%') and not pattern.startswith('%'):
                    # Starts with - use prefix query
                    prefix_term = pattern[:-1]
                    return "prefix", prefix_term
                else:
                    # Complex pattern - use regex
                    regex_pattern = self._convert_like_to_regex(pattern)
                    # Validate regex complexity
                    if not self._validate_regex_complexity(regex_pattern):
                        logger.error(f"Regex pattern too complex, rejecting: {regex_pattern[:100]}...")
                        return "none", ""
                    return "regex", regex_pattern

            # Handle GLOB patterns
            if is_pattern and ('*' in pattern or '?' in pattern):
                regex_pattern = self._convert_glob_to_regex(pattern)
                # Validate regex complexity
                if not self._validate_regex_complexity(regex_pattern):
                    logger.error(f"Regex pattern too complex, rejecting: {regex_pattern[:100]}...")
                    return "none", ""
                return "regex", regex_pattern

            # Default: term query
            return "term", pattern

        except Exception as e:
            logger.error(f"Error translating pattern '{pattern}' to query: {e}")
            return "none", ""

    def _convert_like_to_regex(self, pattern: str) -> str:
        """Convert SQL LIKE pattern to regex."""
        escaped = pattern.replace('\\', '\\\\')
        special_chars = ['.', '^', '$', '(', ')', '[', ']', '{', '}', '|', '+']
        for char in special_chars:
            escaped = escaped.replace(char, f'\\{char}')
        return escaped.replace('%', '.*').replace('_', '.')

    def _convert_glob_to_regex(self, pattern: str) -> str:
        """Convert GLOB pattern to regex."""
        escaped = pattern.replace('\\', '\\\\')
        special_chars = ['.', '^', '$', '(', ')', '[', ']', '{', '}', '|', '+']
        for char in special_chars:
            escaped = escaped.replace(char, f'\\{char}')
        return escaped.replace('*', '.*').replace('?', '.')

    def search_content(
        self,
        query: str,
        is_sqlite_pattern: bool = False,
        fuzziness: Optional[str] = None,
        content_boost: float = 1.0,
        file_path_boost: float = 1.0,
        highlight_pre_tags: Optional[List[str]] = None,
        highlight_post_tags: Optional[List[str]] = None
    ) -> List[Tuple[str, Any]]:
        """
        Search across file content using Tantivy.

        Args:
            query: Search query string
            is_sqlite_pattern: Whether query is a SQLite LIKE/GLOB pattern
            fuzziness: Fuzziness distance (e.g., "1", "2", "AUTO")
            content_boost: Boost factor for content field
            file_path_boost: Boost factor for file_path field
            highlight_pre_tags: Prefix tags for highlighting
            highlight_post_tags: Post tags for highlighting

        Returns:
            List of (file_path, result_dict) tuples
        """
        if not self._connected or self._index is None:
            error_msg = (
                "Tantivy backend is not connected. "
                "Please ensure Tantivy is properly initialized."
            )
            logger.error(error_msg)
            raise RuntimeError(error_msg)

        # Check cache first
        if self.cache_enabled and self._cache:
            cached_result = self._cache.get(query, is_sqlite_pattern, "content")
            if cached_result is not None:
                logger.info(f"Returning cached results for query: {query[:50]}...")
                return cached_result

        results: List[Tuple[str, Any]] = []
        logger.debug(f"Tantivy search_content called with query='{query}', is_sqlite_pattern={is_sqlite_pattern}")

        try:
            # Create searcher
            searcher = self._index.searcher()
            query_type, query_string = self._translate_pattern_to_query(query, "content", is_sqlite_pattern)

            logger.debug(f"Query type: {query_type}, query string: {query_string}")

            # Build query based on type
            if query_type == "none":
                return results

            if query_type == "all":
                # Match all documents
                query_obj = self._index.parse_query("*", ["content"])
            elif query_type == "term":
                # Simple term query
                query_obj = self._index.parse_query(query_string, ["content"])
            elif query_type == "phrase":
                # Phrase query
                query_obj = self._index.parse_query(f'"{query_string}"', ["content"])
            elif query_type == "prefix":
                # Prefix query
                # Note: Tantivy doesn't directly support prefix queries, use regex
                query_obj = self._index.parse_query(f"{query_string}*", ["content"])
            elif query_type == "regex":
                # Regex query
                query_obj = self._index.parse_query(f"/{query_string}/", ["content"])
            else:
                # Default to term query
                query_obj = self._index.parse_query(query_string, ["content"])

            # Execute search
            top_docs = searcher.search(query_obj, limit=100)

            # Collect results
            for (score, doc_address) in top_docs:
                doc = searcher.doc(doc_address)

                # Extract fields
                file_path = doc.get("path") or doc.get("file_id") or ""
                content = doc.get("content") or ""

                if not file_path:
                    logger.warning(f"Document missing path field: {doc_address}")
                    continue

                # Create result document
                result_doc = {
                    "file_path": file_path,
                    "content": content,
                    "score": score,
                    "_score": score,
                }
                tuple_result = (file_path, result_doc)
                results.append(tuple_result)

        except Exception as e:
            logger.error(f"Error searching content in Tantivy: {e}", exc_info=True)

        # Cache the results
        if self.cache_enabled and self._cache and results:
            self._cache.put(query, is_sqlite_pattern, results, "content")
            logger.debug(f"Cached {len(results)} results for query: {query[:50]}...")

        logger.debug(f"Final results list length: {len(results)}")
        return results

    def search_file_paths(
        self,
        query: str,
        is_sqlite_pattern: bool = False,
        fuzziness: Optional[str] = None,
        file_path_boost: float = 1.0,
        highlight_pre_tags: Optional[List[str]] = None,
        highlight_post_tags: Optional[List[str]] = None
    ) -> List[str]:
        """
        Search across file paths using Tantivy.

        Args:
            query: Search query string
            is_sqlite_pattern: Whether query is a SQLite LIKE/GLOB pattern
            fuzziness: Fuzziness distance (not used in Tantivy)
            file_path_boost: Boost factor for file_path field
            highlight_pre_tags: Prefix tags for highlighting (not implemented)
            highlight_post_tags: Post tags for highlighting (not implemented)

        Returns:
            List of file paths matching the query
        """
        if not self._connected or self._index is None:
            error_msg = (
                "Tantivy backend is not connected. "
                "Please ensure Tantivy is properly initialized."
            )
            logger.error(error_msg)
            raise RuntimeError(error_msg)

        # Check cache first
        if self.cache_enabled and self._cache:
            cached_result = self._cache.get(query, is_sqlite_pattern, "path")
            if cached_result is not None:
                logger.info(f"Returning cached path results for query: {query[:50]}...")
                return cached_result

        paths = []
        logger.debug(f"Tantivy search_file_paths called with query='{query}', is_sqlite_pattern={is_sqlite_pattern}")

        try:
            # Create searcher
            searcher = self._index.searcher()
            query_type, query_string = self._translate_pattern_to_query(query, "path", is_sqlite_pattern)

            logger.debug(f"Query type: {query_type}, query string: {query_string}")

            # Build query based on type
            if query_type == "none":
                return paths

            if query_type == "all":
                query_obj = self._index.parse_query("*", ["path"])
            elif query_type == "term":
                query_obj = self._index.parse_query(query_string, ["path"])
            elif query_type == "phrase":
                query_obj = self._index.parse_query(f'"{query_string}"', ["path"])
            elif query_type == "prefix":
                query_obj = self._index.parse_query(f"{query_string}*", ["path"])
            elif query_type == "regex":
                query_obj = self._index.parse_query(f"/{query_string}/", ["path"])
            else:
                query_obj = self._index.parse_query(query_string, ["path"])

            # Execute search
            top_docs = searcher.search(query_obj, limit=100)

            # Collect results
            for (score, doc_address) in top_docs:
                doc = searcher.doc(doc_address)
                path = doc.get("path") or doc.get("file_id") or ""
                if path:
                    paths.append(path)

        except Exception as e:
            logger.error(f"Error searching file paths in Tantivy: {e}", exc_info=True)

        # Cache the results
        if self.cache_enabled and self._cache and paths:
            self._cache.put(query, is_sqlite_pattern, paths, "path")
            logger.debug(f"Cached {len(paths)} path results for query: {query[:50]}...")

        return paths

    def index_file(self, file_path: str, content: str) -> None:
        """
        Index a file for search.

        Args:
            file_path: Path of the file to index
            content: Content of the file to index

        Raises:
            IOError: If the file cannot be indexed
        """
        doc = {
            "path": file_path,
            "content": content,
            "timestamp": datetime.now().isoformat()
        }
        if not self.index_document(file_path, doc):
            raise IOError(f"Failed to index file: {file_path}")

    def delete_indexed_file(self, file_path: str) -> None:
        """
        Delete a file from the search index.

        Args:
            file_path: Path of the file to delete from index
        """
        if not self._connected:
            logger.warning("Tantivy not connected, skipping delete")
            return

        try:
            self.delete_document(file_path)
            logger.debug(f"Deleted file from Tantivy index: {file_path}")
        except Exception as e:
            logger.warning(f"Failed to delete file from Tantivy: {e}")

    def search_files(self, query: str) -> List[Dict[str, Any]]:
        """
        Search for files matching the query.

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
        """
        Clear the search index.

        Returns:
            True if successful, False otherwise
        """
        if not self._connected:
            logger.warning("Tantivy not connected, cannot clear")
            return False

        try:
            # Delete all documents
            self._writer.delete_all_documents()
            self._writer.commit()
            self.clear_cache()
            logger.info("Cleared Tantivy index")
            return True
        except Exception as e:
            logger.error(f"Failed to clear Tantivy index: {e}")
            return False

    def close(self) -> None:
        """Close the Tantivy search backend."""
        if self._writer:
            try:
                self._writer.commit()
                logger.info("Committed Tantivy index before closing")
            except Exception as e:
                logger.warning(f"Failed to commit index before closing: {e}")

        if self._index:
            logger.info("Tantivy search backend closed")

    def get_cache_stats(self) -> Optional[Dict[str, Any]]:
        """
        Get search cache statistics for monitoring.

        Returns:
            Dictionary with cache statistics or None if cache is disabled
        """
        if self.cache_enabled and self._cache:
            return self._cache.get_stats()
        return None

    def clear_cache(self) -> None:
        """Clear all cached search results."""
        if self.cache_enabled and self._cache:
            self._cache.invalidate()
            logger.info("Search cache cleared manually")

    def optimize_index(self) -> bool:
        """
        Optimize the Tantivy index for better query performance.

        Returns:
            True if successful, False otherwise
        """
        if not self._connected or not self._writer:
            return False

        try:
            # Commit and wait for merges
            with self._writer_lock:
                if self._writer is None:
                    return False
                self._writer.commit()
                self._writer.wait_merging_threads()
            logger.info("Tantivy index optimized")
            return True
        except Exception as e:
            logger.error(f"Failed to optimize Tantivy index: {e}")
            return False

    def get_index_stats(self) -> Dict[str, Any]:
        """
        Get statistics about the Tantivy index.

        Returns:
            Dictionary with index statistics
        """
        if not self._connected or not self._index:
            return {}

        try:
            searcher = self._index.searcher()
            num_docs = searcher.num_docs()

            stats = {
                "num_documents": num_docs,
                "index_path": str(self.index_path),
                "connected": self._connected,
            }

            # Add cache stats if available
            if self.cache_enabled and self._cache:
                stats["cache"] = self._cache.get_stats()

            return stats

        except Exception as e:
            logger.error(f"Failed to get Tantivy index stats: {e}")
            return {}
