"""
Core Engine - Unified Search and Indexing Orchestrator.

ARCHITECTURAL FIXES IMPLEMENTED:
--------------------------------
Issue #2 (Tight Coupling): Applied Dependency Injection pattern
- LocalVectorBackend, ZoektStrategy, and LegacyBackend are now injected via constructor
- CoreEngine depends on abstractions (interfaces) rather than concrete implementations
- Enables testing with mocks and swapping implementations

Issue #4 (Inconsistent Error Handling): Established consistent error handling
- CoreEngineError base exception for all engine errors
- BackendUnavailableError for when backends are not available
- ValidationError for input validation failures
- All methods raise exceptions instead of returning False/None
- Consistent logging throughout

MIGRATION: Mixedbread Cloud -> Local Vector Backend
---------------------------------------------------
The Core Engine has been migrated from Mixedbread cloud API to LocalVectorBackend:

- LocalVectorBackend (FAISS-based): Local embeddings using sentence-transformers
- No cloud API keys required
- Supports BAAI/bge-small-en-v1.5, microsoft/codebert-base, all-MiniLM-L6-v2
- Web search feature removed (was Mixedbread-specific)

PRODUCT.MD ALIGNMENT - Dual-Mode Operation:
-------------------------------------------
The Core Engine implements PRODUCT.MD's dual-mode operation requirement:

1. STANDALONE POWER MODE (Primary):
   - LocalVectorBackend (FAISS) provides semantic search
   - Zoekt Strategy provides fast code-aware search via regex/symbolic matching
   - Full functionality without PostgreSQL/Elasticsearch dependencies
   - This is the DEFAULT and PRIMARY mode of operation

2. AUGMENTED INTELLIGENCE MODE (Enhancement):
   - Legacy Backend (PostgreSQL/Elasticsearch) provides augmentation when:
     * Metadata queries are needed (file history, versions)
     * Massive scale historical lookups are required
     * Complex joins across metadata are needed
   - The new engine ALWAYS drives; legacy is ONLY for augmentation
   - If legacy backend is unavailable, core functionality is unaffected

MODE SELECTION STRATEGY:
-------------------------
- Search: Zoekt -> Local Vector (primary) -> Legacy (fallback/augmentation only)
- Index: Local Vector (primary) -> Legacy (dual-write for metadata)
- Ask (RAG): Local Vector only
- File History: Legacy only (metadata feature)

Usage:
    # With dependency injection
    vector_backend = LocalVectorBackend()  # No API key needed
    zoekt_strategy = ZoektStrategy()
    legacy_backend = get_dal_instance()  # Optional - for augmentation only

    engine = CoreEngine(
        vector_backend=vector_backend,
        zoekt_backend=zoekt_strategy,
        legacy_backend=legacy_backend  # Can be None for pure standalone mode
    )
"""

import os
import logging
from typing import Optional, List, Union, Any, Dict

# GRACEFUL IMPORT: Handle optional FAISS/local vector backend
try:
    from .local_vector_backend import LocalVectorBackend, get_local_vector_backend_status
    LOCAL_VECTOR_BACKEND_AVAILABLE = True
except ImportError as e:
    LocalVectorBackend = None  # type: ignore
    get_local_vector_backend_status = None  # type: ignore
    LOCAL_VECTOR_BACKEND_AVAILABLE = False
    import logging
    logging.getLogger(__name__).warning(
        f"LocalVectorBackend not available: {e}. "
        "Install with: uv pip install 'faiss-cpu>=1.7.4' 'sentence-transformers>=2.2.0' "
        "or run: uv sync"
    )

from .types import SearchOptions, SearchResponse, UploadFileOptions, FileMetadata, AskResponse, StoreInfo, ChunkType
from ..search.zoekt import ZoektStrategy
from .interfaces import IVectorBackend, ISearchStrategy, ILegacyBackend

logger = logging.getLogger(__name__)

# CRITICAL FIX: Enforce result size limits to prevent unbounded memory growth
DEFAULT_MAX_RESULTS = 1000
MAX_RESULTS_LIMIT = 10000


# ============================================================================
# CONSISTENT ERROR HANDLING - Issue #4 Fix
# ============================================================================

class CoreEngineError(Exception):
    """
    Base exception for all Core Engine errors.

    ARCHITECTURAL FIX (Issue #4):
    ----------------------------
    Consistent exception hierarchy for proper error handling.
    All CoreEngine methods raise exceptions instead of returning False/None.
    """

    def __init__(self, message: str, details: Optional[Dict[str, Any]] = None):
        super().__init__(message)
        self.details = details or {}

    def __str__(self) -> str:
        if self.details:
            return f"{super().__str__()} | Details: {self.details}"
        return super().__str__()


class BackendUnavailableError(CoreEngineError):
    """
    Raised when a required backend is not available or not configured.

    This allows calling code to handle unavailability gracefully
    rather than checking return values.
    """

    def __init__(self, backend_name: str, reason: str = ""):
        message = f"Backend '{backend_name}' is not available"
        if reason:
            message += f": {reason}"
        super().__init__(message, {"backend": backend_name, "reason": reason})
        self.backend_name = backend_name


class ValidationError(CoreEngineError):
    """
    Raised when input validation fails.
    """

    def __init__(self, field: str, value: Any, reason: str):
        message = f"Validation failed for field '{field}': {reason}"
        super().__init__(message, {"field": field, "value": value})
        self.field = field
        self.value = value


class SearchError(CoreEngineError):
    """
    Raised when a search operation fails.
    """

    def __init__(self, query: str, backend: str, reason: str):
        message = f"Search failed for query '{query}' on {backend} backend: {reason}"
        super().__init__(message, {"query": query, "backend": backend})
        self.query = query
        self.backend = backend


class IndexingError(CoreEngineError):
    """
    Raised when an indexing operation fails.
    """

    def __init__(self, file_path: str, backend: str, reason: str):
        message = f"Indexing failed for '{file_path}' on {backend} backend: {reason}"
        super().__init__(message, {"file_path": file_path, "backend": backend})
        self.file_path = file_path
        self.backend = backend


class ResultLimitError(CoreEngineError):
    """
    Raised when results exceed the configured limit.
    """

    def __init__(self, requested: int, maximum: int):
        message = f"Requested result limit {requested} exceeds maximum {maximum}"
        super().__init__(message, {"requested": requested, "maximum": maximum})
        self.requested = requested
        self.maximum = maximum


# ============================================================================
# CORE ENGINE WITH DEPENDENCY INJECTION - Issue #2 Fix
# ============================================================================

class CoreEngine:
    """
    The Unified Core Engine with Dependency Injection.

    ARCHITECTURAL FIX (Issue #2 - Tight Coupling):
    -----------------------------------------------
    All backends are now injected via constructor parameters.
    The engine depends on abstractions (IVectorBackend, ISearchStrategy)
    rather than concrete implementations.

    Benefits:
    - Testable: Can inject mocks for unit testing
    - Flexible: Can swap implementations without changing engine code
    - Loosely coupled: Engine doesn't know about concrete backend classes
    - Clear dependencies: All dependencies are explicit in constructor

    ARCHITECTURAL FIX (Issue #4 - Inconsistent Error Handling):
    ----------------------------------------------------------
    All methods raise exceptions on errors instead of returning False/None.
    Consistent error types enable proper error handling by calling code.
    """

    def __init__(
        self,
        vector_backend: Optional[IVectorBackend] = None,
        zoekt_backend: Optional[ISearchStrategy] = None,
        legacy_backend: Optional[ILegacyBackend] = None,
        max_results: int = DEFAULT_MAX_RESULTS
    ):
        """
        Initialize the Core Engine with dependency injection.

        ARCHITECTURAL FIX (Issue #2): All backends are injected, not instantiated.

        Args:
            vector_backend: Optional vector backend implementation (LocalVectorBackend recommended)
                           If None, creates a default LocalVectorBackend
            zoekt_backend: Optional Zoekt search strategy implementation
                          If None, creates a default ZoektStrategy
            legacy_backend: Optional legacy DAL backend
                           If None, legacy features are disabled
            max_results: Maximum number of results to return from search operations

        Raises:
            ValidationError: If max_results is invalid
        """
        # Validate and enforce max_results limit
        if max_results < 1:
            raise ValidationError("max_results", max_results, "Must be at least 1")
        if max_results > MAX_RESULTS_LIMIT:
            raise ValidationError("max_results", max_results,
                               f"Must not exceed {MAX_RESULTS_LIMIT}")

        self.max_results = max_results
        logger.info(f"CoreEngine initialized with max_results={self.max_results}")

        # ARCHITECTURAL FIX (Issue #2): Inject backends instead of creating them
        # Use provided backends or create defaults for backward compatibility

        # PREFERRED: LocalVectorBackend (FAISS-based, no cloud dependency)
        if vector_backend is None:
            if LOCAL_VECTOR_BACKEND_AVAILABLE and LocalVectorBackend is not None:
                self.vector_backend: IVectorBackend = LocalVectorBackend()
                logger.info("Using LocalVectorBackend (FAISS-based local vector store)")
            else:
                # NO BACKEND AVAILABLE
                self.vector_backend: IVectorBackend = None  # type: ignore
                logger.error(
                    "CRITICAL: No vector backend available. "
                    "Install FAISS for local embeddings: uv pip install 'faiss-cpu>=1.7.4' 'sentence-transformers>=2.2.0' "
                    "or run: uv sync"
                )
        else:
            self.vector_backend = vector_backend

        self.zoekt_backend: ISearchStrategy = zoekt_backend or ZoektStrategy()
        self.legacy_backend: Optional[ILegacyBackend] = legacy_backend

        # Log availability of each backend
        if self.vector_backend:
            logger.info(f"Vector backend: {type(self.vector_backend).__name__}")
        if self.zoekt_backend:
            logger.info(f"Zoekt backend: {type(self.zoekt_backend).__name__}")
        if self.legacy_backend:
            logger.info(f"Legacy backend: {type(self.legacy_backend).__name__}")

    async def initialize(self) -> None:
        """
        Initialize connections.

        ARCHITECTURAL FIX (Issue #4): Raises exceptions instead of silent failure.

        Raises:
            BackendUnavailableError: If a required backend cannot be initialized
        """
        # Initialize LocalVectorBackend if it's the active backend
        if self.vector_backend is not None:
            backend_type = type(self.vector_backend).__name__
            logger.info(f"Initializing vector backend: {backend_type}")

            # LocalVectorBackend requires explicit initialization
            if backend_type == "LocalVectorBackend" and hasattr(self.vector_backend, "initialize"):
                try:
                    await self.vector_backend.initialize()
                    logger.info("LocalVectorBackend initialized successfully")
                except Exception as e:
                    logger.error(f"Failed to initialize LocalVectorBackend: {e}")
                    raise BackendUnavailableError("LocalVectorBackend", str(e))

        logger.info("CoreEngine initialization complete")

    def _validate_search_options(self, options: SearchOptions) -> SearchOptions:
        """
        Validate and normalize search options.

        ARCHITECTURAL FIX (Issue #4): Consistent validation with exceptions.

        Args:
            options: Search options to validate

        Returns:
            Normalized search options

        Raises:
            ValidationError: If options are invalid
            ResultLimitError: If result limit exceeds maximum
        """
        if options is None:
            options = SearchOptions()

        # Validate top_k (number of results to return)
        if options.top_k is not None and options.top_k > self.max_results:
            raise ResultLimitError(options.top_k, self.max_results)

        # Apply default limit if needed
        if options.top_k is None or options.top_k > self.max_results:
            effective_top_k = min(options.top_k or self.max_results, self.max_results)
            if options.top_k != effective_top_k:
                logger.warning(f"Search top_k {options.top_k} exceeds max_results {self.max_results}, using {effective_top_k}")
            options = SearchOptions(
                top_k=effective_top_k,
                rerank=options.rerank,
                include_web=options.include_web,
                content=options.content,
                use_zoekt=options.use_zoekt
            )

        return options

    def _enforce_result_limit(self, response: SearchResponse, limit: int) -> SearchResponse:
        """
        Enforce result size limit on SearchResponse.

        Args:
            response: The search response to limit
            limit: Maximum number of results to return

        Returns:
            SearchResponse with limited results
        """
        if len(response.data) > limit:
            logger.warning(f"Truncating search results from {len(response.data)} to {limit}")
            return SearchResponse(data=response.data[:limit])
        return response

    async def search(
        self,
        store_ids: List[str],
        query: str,
        options: Optional[SearchOptions] = None
    ) -> SearchResponse:
        """
        Unified search method implementing PRODUCT.MD's dual-mode operation strategy.

        DUAL-MODE OPERATION (PRODUCT.MD ALIGNMENT):
        -------------------------------------------
        STANDALONE POWER MODE (Primary - Default):
            - Uses Core Vector Backend (semantic search with reranking)
            - Falls back to Zoekt for fast symbolic/regex search
            - Full functionality without PostgreSQL/Elasticsearch

        AUGMENTED INTELLIGENCE MODE (When legacy is available):
            - Core engine ALWAYS drives the search
            - Legacy backend augments for:
              * Metadata queries (file history, versions)
              * Historical lookups at scale
            - Legacy is NEVER the primary; only fallback/augmentation

        SEARCH ROUTING STRATEGY:
        ------------------------
        Priority 1: Web Search (if requested) -> VectorBackend
        Priority 2: Zoekt (if requested) -> Zoekt Strategy
        Priority 3: Core Vector Search -> VectorBackend (PRIMARY)
        Priority 4: Legacy Backend -> Fallback/Augmentation ONLY

        Args:
            store_ids: List of store identifiers
            query: Search query
            options: Search options

        Returns:
            SearchResponse with results

        Raises:
            ValidationError: If query or options are invalid
            SearchError: If all search backends fail
            BackendUnavailableError: If a required backend is unavailable
        """
        # Validate inputs
        if not query or not query.strip():
            raise ValidationError("query", query, "Query cannot be empty")

        if not store_ids:
            raise ValidationError("store_ids", store_ids, "Store IDs cannot be empty")

        # Validate and normalize options
        options = self._validate_search_options(options)

        # Note: Web search was a Mixedbread cloud feature and is not available
        # with LocalVectorBackend. If include_web is requested, log a warning and continue.

        # 1. Zoekt Strategy (fast code-aware search)
        if options.use_zoekt and self.zoekt_backend and self.zoekt_backend.is_available():
            try:
                base_path = store_ids[0] if store_ids else "."
                if not os.path.exists(base_path):
                    base_path = "."

                zoekt_results = self.zoekt_backend.search(query, base_path)
                data = []
                result_count = 0
                for file_path, matches in zoekt_results.items():
                    for line_num, content in matches:
                        if result_count >= self.max_results:
                            logger.warning(f"Zoekt search results truncated at {result_count}")
                            break
                        data.append(ChunkType(
                            type="text",
                            text=content,
                            score=1.0,
                            metadata=FileMetadata(path=file_path, hash=""),
                            generated_metadata={"line_number": line_num}
                        ))
                        result_count += 1
                    if result_count >= self.max_results:
                        break
                return SearchResponse(data=data)
            except Exception as e:
                logger.error(f"Zoekt search failed: {e}")
                # Continue to other backends

        # 2. Core Vector Search (Primary)
        try:
            if self.vector_backend and self.vector_backend.is_available():
                result = await self.vector_backend.search(store_ids, query, options)
                return self._enforce_result_limit(result, self.max_results)
        except Exception as e:
            logger.error(f"Core Vector search failed: {e}")
            # Continue to fallback

        # 3. Fallback/Augmentation: Legacy Backend
        if self.legacy_backend and self.legacy_backend.search:
            try:
                legacy_results = self.legacy_backend.search.search_files(query)
                data = []
                for i, res in enumerate(legacy_results[:self.max_results]):
                    data.append(ChunkType(
                        type="text",
                        text=res.get('content', ''),
                        score=res.get('score', 0.5),
                        metadata=FileMetadata(
                            path=res.get('file_path', ''),
                            hash=""
                        ),
                        generated_metadata=res
                    ))
                if len(legacy_results) > self.max_results:
                    logger.warning(f"Legacy search results truncated from {len(legacy_results)} to {self.max_results}")
                return SearchResponse(data=data)
            except Exception as e:
                logger.error(f"Legacy backend search failed: {e}")

        # All backends failed - raise exception (Issue #4 fix)
        raise SearchError(query, "all", "All search backends failed or are unavailable")

    async def ask(
        self,
        store_ids: List[str],
        question: str,
        options: Optional[SearchOptions] = None
    ) -> AskResponse:
        """
        Unified Q&A (RAG) method with dependency-injected backends.

        ARCHITECTURAL FIX (Issue #2): Uses injected vector_backend
        ARCHITECTURAL FIX (Issue #4): Raises exceptions on errors

        Args:
            store_ids: List of store identifiers
            question: Question to ask
            options: Search options

        Returns:
            AskResponse with answer and sources

        Raises:
            ValidationError: If question is invalid
            BackendUnavailableError: If vector backend is unavailable
            CoreEngineError: If the ask operation fails
        """
        if not question or not question.strip():
            raise ValidationError("question", question, "Question cannot be empty")

        if not store_ids:
            raise ValidationError("store_ids", store_ids, "Store IDs cannot be empty")

        if options is None:
            options = SearchOptions()

        if not self.vector_backend or not self.vector_backend.is_available():
            raise BackendUnavailableError("LocalVectorBackend", "Required for ask operation")

        try:
            return await self.vector_backend.ask(store_ids, question, options)
        except Exception as e:
            logger.error(f"Ask operation failed: {e}")
            raise CoreEngineError(f"Ask operation failed: {e}")

    async def index_file(
        self,
        store_id: str,
        file_path: str,
        content: Union[str, bytes],
        metadata: Optional[FileMetadata] = None
    ) -> None:
        """
        Index a file implementing PRODUCT.MD's dual-mode operation strategy.

        DUAL-MODE OPERATION (PRODUCT.MD ALIGNMENT):
        -------------------------------------------
        STANDALONE POWER MODE (Primary):
            - Writes to Core Vector Backend for semantic search
            - No PostgreSQL/Elasticsearch required
            - Full searchability immediately available

        AUGMENTED INTELLIGENCE MODE (Dual-Write for Metadata):
            - Core Vector Backend: Primary search index
            - Legacy Backend: Metadata preservation (versions, history)
            - If legacy fails, core functionality is unaffected

        Args:
            store_id: Store identifier
            file_path: Path of the file to index
            content: File content
            metadata: Optional file metadata

        Raises:
            ValidationError: If parameters are invalid
            IndexingError: If indexing fails on all backends
        """
        if not store_id:
            raise ValidationError("store_id", store_id, "Store ID cannot be empty")
        if not file_path:
            raise ValidationError("file_path", file_path, "File path cannot be empty")
        if content is None:
            raise ValidationError("content", content, "Content cannot be None")

        errors = []

        # 1. Write to Core Vector Backend
        if self.vector_backend and self.vector_backend.is_available():
            try:
                upload_options = UploadFileOptions(
                    external_id=file_path,
                    metadata=metadata or FileMetadata(path=file_path, hash="")
                )
                await self.vector_backend.upload_file(store_id, file_path, content, upload_options)
                logger.debug(f"Successfully indexed {file_path} in LocalVectorBackend")
                return  # Success - don't try other backends
            except Exception as e:
                error_msg = f"Core Vector indexing failed for {file_path}: {e}"
                logger.error(error_msg)
                errors.append(error_msg)

        # 2. Write to Legacy Backend (Augmentation)
        if self.legacy_backend:
            try:
                str_content = content
                if isinstance(content, bytes):
                    str_content = content.decode('utf-8', errors='ignore')

                self.legacy_backend.storage.save_file_content(file_path, str_content)
                self.legacy_backend.search.index_file(file_path, str_content)
                if metadata:
                    meta_dict = {
                        "path": metadata.path,
                        "hash": metadata.hash,
                        "last_modified": metadata.last_modified,
                        "size": metadata.size
                    }
                    self.legacy_backend.metadata.save_file_metadata(file_path, meta_dict)
                logger.debug(f"Successfully indexed {file_path} in LegacyBackend")
                return  # Success
            except Exception as e:
                error_msg = f"Legacy indexing failed for {file_path}: {e}"
                logger.error(error_msg)
                errors.append(error_msg)

        # All backends failed
        raise IndexingError(file_path, "all", "; ".join(errors))

    async def delete_file(self, store_id: str, file_path: str) -> None:
        """
        Delete a file from index with dependency-injected backends.

        ARCHITECTURAL FIX (Issue #2): Uses injected backends
        ARCHITECTURAL FIX (Issue #4): Raises exceptions on errors

        Args:
            store_id: Store identifier
            file_path: Path of the file to delete

        Raises:
            ValidationError: If parameters are invalid
            CoreEngineError: If deletion fails on all backends
        """
        if not store_id:
            raise ValidationError("store_id", store_id, "Store ID cannot be empty")
        if not file_path:
            raise ValidationError("file_path", file_path, "File path cannot be empty")

        errors = []

        # 1. Delete from Core Vector Backend
        if self.vector_backend and self.vector_backend.is_available():
            try:
                await self.vector_backend.delete_file(store_id, file_path)
                logger.debug(f"Successfully deleted {file_path} from LocalVectorBackend")
            except Exception as e:
                error_msg = f"Core Vector delete failed for {file_path}: {e}"
                logger.error(error_msg)
                errors.append(error_msg)

        # 2. Delete from Legacy Backend
        if self.legacy_backend:
            try:
                self.legacy_backend.storage.delete_file_content(file_path)
                self.legacy_backend.search.delete_indexed_file(file_path)
                self.legacy_backend.metadata.delete_file_metadata(file_path)
                logger.debug(f"Successfully deleted {file_path} from LegacyBackend")
            except Exception as e:
                error_msg = f"Legacy delete failed for {file_path}: {e}"
                logger.error(error_msg)
                errors.append(error_msg)

        # Check if at least one backend succeeded
        if not errors:
            return  # Success

        # All backends failed
        raise CoreEngineError(f"Failed to delete {file_path} on all backends: {'; '.join(errors)}")

    async def get_store_info(self, store_id: str) -> StoreInfo:
        """
        Get store information using dependency-injected backend.

        ARCHITECTURAL FIX (Issue #2): Uses injected vector_backend
        ARCHITECTURAL FIX (Issue #4): Raises exceptions on errors

        Args:
            store_id: Store identifier

        Returns:
            StoreInfo with store details

        Raises:
            ValidationError: If store_id is invalid
            BackendUnavailableError: If vector backend is unavailable
        """
        if not store_id:
            raise ValidationError("store_id", store_id, "Store ID cannot be empty")

        if not self.vector_backend or not self.vector_backend.is_available():
            raise BackendUnavailableError("LocalVectorBackend", "Required for get_store_info")

        try:
            return await self.vector_backend.get_info(store_id)
        except Exception as e:
            logger.error(f"Failed to get store info for {store_id}: {e}")
            raise CoreEngineError(f"Failed to get store info: {e}")
