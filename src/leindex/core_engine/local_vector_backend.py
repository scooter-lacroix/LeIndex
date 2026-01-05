"""
Local Vector Backend - FAISS-based semantic search with local embedding models.

This module implements a local vector store backend using:
- FAISS for fast vector similarity search
- sentence-transformers for embedding generation
- PostgreSQL for metadata storage

This replaces the Mixedbread cloud API dependency with a fully local solution.

PHASE 2: Local Vector Store Implementation
------------------------------------------
Spec: conductor/tracks/mcp_consolidation_local_vector_20251230/spec.md

Features:
- Local embedding models (BAAI/bge-small-en-v1.5, microsoft/codebert-base, all-MiniLM-L6-v2)
- Adaptive FAISS index (IndexFlatIP -> IndexIVFFlat) based on size
- Model mismatch detection on startup
- Index persistence for fast startup
- Metadata storage in PostgreSQL

SECURITY: All inputs are validated and sanitized to prevent:
- Path traversal attacks
- Resource exhaustion
- Unbounded memory growth
- Model loading abuse
"""

from __future__ import annotations

import os
import json
import logging
import hashlib
import ast
from pathlib import Path
from typing import List, Optional, Any, Dict, AsyncGenerator, Union
from dataclasses import dataclass, asdict
from datetime import datetime
from threading import Lock, Semaphore
import re

# GRACEFUL IMPORT: Handle optional dependencies
try:
    import faiss
    FAISS_AVAILABLE = True
except ImportError as e:
    FAISS_AVAILABLE = False
    faiss = None
    logging.getLogger(__name__).warning(
        f"faiss-cpu not available: {e}. "
        "LocalVectorBackend will operate in LIMITED MODE. "
        "Install with: uv pip install 'faiss-cpu>=1.7.4'"
    )

try:
    from sentence_transformers import SentenceTransformer
    SENTENCE_TRANSFORMERS_AVAILABLE = True
except ImportError as e:
    SENTENCE_TRANSFORMERS_AVAILABLE = False
    SentenceTransformer = None
    logging.getLogger(__name__).warning(
        f"sentence-transformers not available: {e}. "
        "LocalVectorBackend will operate in LIMITED MODE. "
        "Install with: uv pip install 'sentence-transformers>=2.2.0'"
    )

try:
    import numpy as np
    NUMPY_AVAILABLE = True
except ImportError as e:
    NUMPY_AVAILABLE = False
    np = None
    logging.getLogger(__name__).warning(
        f"numpy not available: {e}. "
        "LocalVectorBackend requires numpy for vector operations."
    )

from .types import (
    StoreFile, FileMetadata, SearchResponse, ChunkType,
    AskResponse, StoreInfo, UploadFileOptions, SearchOptions
)

logger = logging.getLogger(__name__)

# ============================================================================
# SECURITY CONSTANTS - Resource limits to prevent abuse
# ============================================================================

# Default configuration
DEFAULT_MODEL = "BAAI/bge-small-en-v1.5"
DEFAULT_INDEX_THRESHOLD = 100000
DEFAULT_EMBEDDING_DIM = 384  # bge-small-en-v1.5 uses 384 dimensions

# SECURITY: Maximum limits to prevent resource exhaustion
MAX_VECTORS = 1_000_000  # Maximum number of vectors in index
MAX_QUERY_LENGTH = 8192  # Maximum query string length
MAX_TOP_K = 1000  # Maximum top_k value for search
MAX_METADATA_FILE_SIZE = 100 * 1024 * 1024  # 100MB max metadata file size
MAX_CONTENT_SIZE = 50 * 1024 * 1024  # 50MB max file content size
MAX_CONCURRENT_MODEL_LOADS = 2  # Max concurrent model loading operations

# Path traversal patterns to detect
PATH_TRAVERSAL_PATTERNS = [r'\.\./', r'\.\.\\', r'\.\.[/\\]', r'~/', r'~\\']

# Supported models and their configurations
SUPPORTED_MODELS = {
    "BAAI/bge-small-en-v1.5": {
        "dim": 384,
        "size_mb": 130,
        "description": "High-quality general-purpose embeddings (default)"
    },
    "microsoft/codebert-base": {
        "dim": 768,
        "size_mb": 450,
        "description": "Code-specific embeddings"
    },
    "all-MiniLM-L6-v2": {
        "dim": 384,
        "size_mb": 80,
        "description": "Lightweight general-purpose embeddings"
    }
}

# Environment variables
ENV_MODEL = "LOCAL_VECTOR_MODEL"
ENV_INDEX_THRESHOLD = "FAISS_INDEX_THRESHOLD"
ENV_INDEX_PATH = "FAISS_INDEX_PATH"
ENV_CACHE_DIR = "TRANSFORMERS_CACHE"

# Metadata file keys
META_VERSION = "version"
META_MODEL = "model"
META_DIMENSION = "dimension"
META_VECTOR_COUNT = "vector_count"
META_INDEX_TYPE = "index_type"
META_CREATED_AT = "created_at"
META_UPDATED_AT = "updated_at"

INDEX_VERSION = "1.0"


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


def _validate_metadata_file_size(file_path: str) -> None:
    """
    Validate metadata file size before loading.

    Args:
        file_path: Path to the metadata file

    Raises:
        ValueError: If file is too large
    """
    try:
        file_size = os.path.getsize(file_path)
        if file_size > MAX_METADATA_FILE_SIZE:
            raise ValueError(
                f"Metadata file too large (max {MAX_METADATA_FILE_SIZE} bytes): {file_size}"
            )
    except OSError as e:
        raise ValueError(f"Cannot access metadata file '{file_path}': {e}")


@dataclass
class VectorMetadata:
    """Metadata for a vector in the index."""
    file_path: str
    chunk_index: int
    start_line: Optional[int] = None
    end_line: Optional[int] = None
    chunk_type: str = "text"  # function, class, module, text
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
    """Metadata for the FAISS index."""
    version: str = INDEX_VERSION
    model: str = DEFAULT_MODEL
    dimension: int = DEFAULT_EMBEDDING_DIM
    vector_count: int = 0
    index_type: str = "IndexFlatIP"
    created_at: str = ""
    updated_at: str = ""

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> "IndexMetadata":
        return cls(**data)


class LocalVectorBackend:
    """
    Local vector backend using FAISS + sentence-transformers.

    This backend provides:
    - Local embedding generation (no API calls)
    - Fast vector search using FAISS
    - Adaptive index (FlatIP -> IVFFlat) based on size
    - Persistent index for fast startup
    - Metadata storage in PostgreSQL
    - Security: Path validation, resource limits, input sanitization

    Usage:
        backend = LocalVectorBackend(
            model_name="BAAI/bge-small-en-v1.5",
            index_path="./faiss_index"
        )
        await backend.initialize()

        # Add documents
        await backend.add_documents(store_id, chunks)

        # Search
        results = await backend.search(store_ids, query, options)
    """

    # Class-level model cache for sharing across instances
    _model_cache: Dict[str, Any] = {}
    _model_cache_lock = Lock()

    # SECURITY: Semaphore to limit concurrent model loading operations
    _model_load_semaphore = Semaphore(MAX_CONCURRENT_MODEL_LOADS)

    def __init__(
        self,
        model_name: Optional[str] = None,
        index_path: Optional[str] = None,
        index_threshold: Optional[int] = None,
        cache_dir: Optional[str] = None
    ):
        """
        Initialize the LocalVectorBackend.

        Args:
            model_name: Name of the sentence-transformers model to use.
                       Defaults to LOCAL_VECTOR_MODEL env var or DEFAULT_MODEL.
            index_path: Directory path to store/load FAISS index.
                       Defaults to FAISS_INDEX_PATH env var or "./faiss_index".
            index_threshold: Number of vectors before switching to IVFFlat.
                           Defaults to FAISS_INDEX_THRESHOLD env var or 100000.
            cache_dir: Directory for caching transformer models.
                      Defaults to TRANSFORMERS_CACHE env var or system default.
        """
        # Sanitize and validate index_path
        raw_index_path = index_path or os.getenv(ENV_INDEX_PATH, "./faiss_index")
        self.index_path = _sanitize_path(raw_index_path, allow_absolute=True)

        # Configuration
        self.model_name = model_name or os.getenv(ENV_MODEL, DEFAULT_MODEL)
        self.index_threshold = int(
            index_threshold or os.getenv(ENV_INDEX_THRESHOLD, str(DEFAULT_INDEX_THRESHOLD))
        )
        self.cache_dir = cache_dir or os.getenv(ENV_CACHE_DIR)

        # Validate model name
        if self.model_name not in SUPPORTED_MODELS:
            logger.warning(
                f"Unknown model '{self.model_name}'. "
                f"Supported models: {list(SUPPORTED_MODELS.keys())}. "
                f"Using default: {DEFAULT_MODEL}"
            )
            self.model_name = DEFAULT_MODEL

        self.model_config = SUPPORTED_MODELS[self.model_name]
        self.dimension = self.model_config["dim"]

        # Runtime state
        self._model: Optional[Any] = None
        self._index: Optional[Any] = None
        self._index_metadata: Optional[IndexMetadata] = None
        self._vector_metadata: Dict[str, VectorMetadata] = {}  # vector_id -> metadata
        self._initialized = False
        self._limited_mode = False

        # Thread safety
        self._lock = Lock()

        # Create index directory
        Path(self.index_path).mkdir(parents=True, exist_ok=True)

        logger.info(
            f"LocalVectorBackend configured: model={self.model_name}, "
            f"dim={self.dimension}, path={self.index_path}, "
            f"threshold={self.index_threshold}"
        )

    @property
    def client(self) -> Optional[Any]:
        """Get the underlying client (FAISS index)."""
        return self._index

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
        2. Loads or creates the FAISS index
        3. Loads the embedding model
        4. Checks for model mismatches
        5. Loads metadata from storage

        Raises:
            ValueError: If dependencies are not installed
        """
        logger.info("Initializing LocalVectorBackend...")

        # Check dependencies
        if not FAISS_AVAILABLE:
            self._limited_mode = True
            raise ValueError(
                "faiss-cpu is not installed. "
                "Install with: uv pip install 'faiss-cpu>=1.7.4'"
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

        # Load or create index
        self._load_or_create_index()

        # Load model (lazy loading - happens on first search)
        logger.info("Embedding model will be loaded on first search (~2-3 seconds)")

        # Load metadata from PostgreSQL if available
        await self._load_metadata()

        self._initialized = True
        logger.info(
            f"LocalVectorBackend initialized: {self._index_metadata.vector_count} vectors, "
            f"model={self._index_metadata.model}"
        )

    def _load_or_create_index(self) -> None:
        """
        Load existing index from disk or create a new one.

        This method:
        1. Checks for existing index files
        2. Loads index metadata
        3. Detects model mismatches
        4. Loads or creates the FAISS index

        SECURITY: Validates metadata file size before loading
        """
        index_file = os.path.join(self.index_path, "index.faiss")
        meta_file = os.path.join(self.index_path, "metadata.json")
        vectors_meta_file = os.path.join(self.index_path, "vectors_metadata.json")

        if os.path.exists(index_file) and os.path.exists(meta_file):
            # SECURITY: Validate metadata file size before loading
            _validate_metadata_file_size(meta_file)
            if os.path.exists(vectors_meta_file):
                _validate_metadata_file_size(vectors_meta_file)

            # Load existing index
            try:
                with open(meta_file, 'r') as f:
                    meta_data = json.load(f)
                self._index_metadata = IndexMetadata.from_dict(meta_data)

                # SECURITY: Check vector count limit
                if self._index_metadata.vector_count > MAX_VECTORS:
                    logger.warning(
                        f"Index has {self._index_metadata.vector_count} vectors, "
                        f"exceeds safety limit of {MAX_VECTORS}. "
                        f"Creating new index to prevent resource exhaustion."
                    )
                    self._create_new_index()
                    return

                # Check model mismatch
                if self._index_metadata.model != self.model_name:
                    logger.warning(
                        f"MODEL MISMATCH: Index was created with '{self._index_metadata.model}' "
                        f"but current configuration uses '{self.model_name}'. "
                        f"Search results may be degraded. Reindex recommended."
                    )

                # Check dimension mismatch (always check, regardless of model name)
                if self._index_metadata.dimension != self.dimension:
                    logger.warning(
                        f"DIMENSION MISMATCH: Index has {self._index_metadata.dimension}D "
                        f"but model expects {self.dimension}D. "
                        f"Reindex required."
                    )
                    # Force reindex by creating new index
                    self._create_new_index()
                    return

                # Load FAISS index
                self._index = faiss.read_index(index_file)
                logger.info(
                    f"Loaded existing index: {self._index_metadata.vector_count} vectors, "
                    f"type={self._index_metadata.index_type}"
                )

                # Load vector metadata
                if os.path.exists(vectors_meta_file):
                    with open(vectors_meta_file, 'r') as f:
                        vectors_data = json.load(f)
                    self._vector_metadata = {
                        k: VectorMetadata.from_dict(v)
                        for k, v in vectors_data.items()
                    }
                    logger.info(f"Loaded {len(self._vector_metadata)} vector metadata entries")

            except Exception as e:
                logger.error(f"Failed to load existing index: {e}. Creating new index...")
                self._create_new_index()
        else:
            # Create new index
            self._create_new_index()

    def _create_new_index(self) -> None:
        """Create a new FAISS index with default configuration."""
        logger.info("Creating new FAISS index...")
        self._index_metadata = IndexMetadata(
            version=INDEX_VERSION,
            model=self.model_name,
            dimension=self.dimension,
            index_type="IndexFlatIP",
            created_at=datetime.now().isoformat(),
            updated_at=datetime.now().isoformat()
        )
        # IndexFlatIP: Exact inner product search
        self._index = faiss.IndexFlatIP(self.dimension)
        self._vector_metadata = {}
        logger.info(f"Created new IndexFlatIP with dimension {self.dimension}")

    def _maybe_upgrade_index(self) -> None:
        """
        Upgrade to IVFFlat if threshold is exceeded.

        Transitions from IndexFlatIP (exact search) to IndexIVFFlat (approximate)
        when the number of vectors exceeds the configured threshold.

        SECURITY: Enforces MAX_VECTORS limit
        """
        if self._index_metadata.index_type == "IndexFlatIP":
            vector_count = self._index_metadata.vector_count
            if vector_count >= self.index_threshold:
                logger.info(
                    f"Vector count ({vector_count}) exceeds threshold "
                    f"({self.index_threshold}). Upgrading to IndexIVFFlat..."
                )
                self._upgrade_to_ivf_flat()

    def _upgrade_to_ivf_flat(self) -> None:
        """Upgrade the current index to IndexIVFFlat for approximate search."""
        if self._index_metadata.index_type != "IndexFlatIP":
            return

        # Get current vectors
        vector_count = self._index.ntotal
        vectors = np.zeros((vector_count, self.dimension), dtype=np.float32)
        self._index.reconstruct_n(0, vector_count, vectors)

        # Create IVFFlat index
        # nlist: number of cluster centroids (typically sqrt(N))
        nlist = min(int(vector_count ** 0.5), 4096)
        quantizer = faiss.IndexFlatIP(self.dimension)
        ivf_index = faiss.IndexIVFFlat(
            quantizer, self.dimension, nlist, faiss.METRIC_INNER_PRODUCT
        )

        # Train the index
        logger.info(f"Training IVFFlat index with {nlist} centroids...")
        ivf_index.train(vectors)
        ivf_index.add(vectors)

        # Update index
        self._index = ivf_index
        self._index_metadata.index_type = "IndexIVFFlat"
        self._index_metadata.updated_at = datetime.now().isoformat()

        logger.info(
            f"Upgraded to IndexIVFFlat: nlist={nlist}, vectors={vector_count}"
        )

    def _load_model(self) -> Any:
        """
        Load the embedding model with caching.

        SECURITY: Uses semaphore to limit concurrent model loads

        Uses class-level cache to share models across instances.

        Returns:
            Loaded SentenceTransformer model
        """
        with self._model_cache_lock:
            if self.model_name in self._model_cache:
                logger.debug(f"Using cached model: {self.model_name}")
                return self._model_cache[self.model_name]

        # SECURITY: Limit concurrent model loading to prevent resource exhaustion
        with self._model_load_semaphore:
            # Double-check after acquiring semaphore
            with self._model_cache_lock:
                if self.model_name in self._model_cache:
                    return self._model_cache[self.model_name]

            logger.info(f"Loading embedding model: {self.model_name} (~2-3 seconds)...")
            try:
                model = SentenceTransformer(
                    self.model_name,
                    cache_folder=self.cache_dir
                )
                with self._model_cache_lock:
                    self._model_cache[self.model_name] = model
                logger.info(f"Model loaded: {self.model_name}")
                return model
            except Exception as e:
                logger.error(f"Failed to load model {self.model_name}: {e}")
                raise ValueError(
                    f"Failed to load embedding model '{self.model_name}': {e}. "
                    f"Ensure the model name is correct or check your internet connection "
                    f"for first-time download."
                )

    async def _load_metadata(self) -> None:
        """
        Load metadata from PostgreSQL.

        This is a placeholder for future PostgreSQL integration.
        Currently, metadata is stored alongside the index.
        """
        # TODO: Implement PostgreSQL metadata loading
        # For now, metadata is loaded from JSON file in _load_or_create_index
        pass

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

        # SentenceTransformer.encode returns numpy array
        embeddings = self._model.encode(
            texts,
            convert_to_numpy=True,
            normalize_embeddings=True,  # Normalize for inner product similarity
            show_progress_bar=False
        )
        return embeddings.astype(np.float32)

    def _save_index(self) -> None:
        """Save the index and metadata to disk."""
        index_file = os.path.join(self.index_path, "index.faiss")
        meta_file = os.path.join(self.index_path, "metadata.json")
        vectors_meta_file = os.path.join(self.index_path, "vectors_metadata.json")

        try:
            # Save FAISS index
            faiss.write_index(self._index, index_file)

            # Save metadata
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

            logger.debug(f"Saved index to {self.index_path}")
        except Exception as e:
            logger.error(f"Failed to save index: {e}")

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
            raise ValueError("LocalVectorBackend not initialized. Call initialize() first.")

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
        3. Adds vectors to the FAISS index
        4. Stores metadata

        Args:
            store_id: Store identifier (used as namespace/path prefix)
            file_path: Path of the file
            content: File content (str or bytes)
            options: Upload options

        SECURITY: Validates all inputs and enforces size limits
        """
        if not self._initialized:
            raise ValueError("LocalVectorBackend not initialized. Call initialize() first.")

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

        # Add to index
        with self._lock:
            # SECURITY: Check vector count limit before adding
            current_count = self._index.ntotal
            new_count = current_count + len(chunks)
            if new_count > MAX_VECTORS:
                raise ValueError(
                    f"Cannot add {len(chunks)} vectors: would exceed limit of {MAX_VECTORS}. "
                    f"Current: {current_count}, Requested: {new_count}"
                )

            start_idx = self._index.ntotal
            self._index.add(embeddings)

            # Store metadata
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

            # Update index metadata
            self._index_metadata.vector_count = self._index.ntotal
            self._index_metadata.updated_at = datetime.now().isoformat()

        # Check for index upgrade
        self._maybe_upgrade_index()

        # Save to disk
        self._save_index()

        logger.debug(f"Indexed {len(chunks)} chunks from {safe_file_path}")

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

        Note: FAISS doesn't support efficient deletion.
        We remove the metadata and mark vectors for removal.
        The index should be rebuilt periodically.

        Args:
            store_id: Store identifier
            external_id: File path to delete

        SECURITY: Validates store_id and external_id
        """
        if not self._initialized:
            raise ValueError("LocalVectorBackend not initialized. Call initialize() first.")

        # SECURITY: Validate inputs
        safe_store_id = _validate_store_id(store_id)
        safe_external_id = _validate_file_path(external_id)

        full_path = os.path.join(safe_store_id, safe_external_id) if safe_store_id else safe_external_id

        with self._lock:
            # Find and remove metadata entries for this file
            to_remove = [
                vid for vid, meta in self._vector_metadata.items()
                if meta.file_path == full_path
            ]

            for vid in to_remove:
                del self._vector_metadata[vid]

            # Note: FAISS removal is expensive, requiring index rebuild
            # For now, we just remove metadata
            logger.warning(
                f"Deleted metadata for {safe_external_id}. "
                f"FAISS index rebuild required for complete removal."
            )

        self._save_index()

    async def search(
        self,
        store_ids: List[str],
        query: str,
        options: SearchOptions
    ) -> SearchResponse:
        """
        Search the vector index.

        Args:
            store_ids: List of store identifiers (used as path prefixes)
            query: Search query
            options: Search options

        Returns:
            SearchResponse with results

        SECURITY: Validates query length, store_ids, and bounds top_k
        """
        if not self._initialized:
            raise ValueError("LocalVectorBackend not initialized. Call initialize() first.")

        # SECURITY: Validate and sanitize query
        safe_query = _validate_query(query)
        if not safe_query:
            return SearchResponse(data=[])

        # SECURITY: Validate store_ids
        safe_store_ids = [_validate_store_id(sid) for sid in store_ids]

        # SECURITY: Validate and bound top_k
        top_k = _validate_top_k(options.top_k)
        top_k = min(top_k, self._index.ntotal)

        # Encode query
        query_embedding = self._encode([safe_query])[0]

        # Set nprobe for IVFFlat (number of clusters to search)
        if hasattr(self._index, 'nprobe'):
            # Search 10% of clusters, at least 1, at most 100
            nprobe = min(max(int(self._index.nlist * 0.1), 1), 100)
            self._index.nprobe = nprobe

        # Search
        with self._lock:
            scores, indices = self._index.search(
                query_embedding.reshape(1, -1),
                top_k
            )

        # Convert results
        results = []
        vector_ids = list(self._vector_metadata.keys())

        for score, idx in zip(scores[0], indices[0]):
            if idx < 0 or idx >= len(vector_ids):
                continue

            vector_id = vector_ids[idx]
            meta = self._vector_metadata.get(vector_id)

            if not meta:
                continue

            # Filter by store_id
            if safe_store_ids:
                matches_store = any(
                    meta.file_path.startswith(store_id)
                    for store_id in safe_store_ids
                )
                if not matches_store:
                    continue

            # Load actual file content for the chunk
            text_content = self._load_chunk_content(meta.file_path, meta.start_line, meta.end_line)

            results.append(ChunkType(
                type="text",
                text=text_content,
                score=float(score),
                metadata=FileMetadata(
                    path=meta.file_path,
                    hash=""
                ),
                chunk_index=meta.chunk_index,
                generated_metadata={
                    "start_line": meta.start_line,
                    "end_line": meta.end_line,
                    "chunk_type": meta.chunk_type,
                    "parent_context": meta.parent_context
                }
            ))

        return SearchResponse(data=results)

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
            AskResponse with sources (no generated answer)
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
            store_id: Store identifier (not used in local backend)

        Returns:
            StoreInfo with store details

        SECURITY: Validates store_id
        """
        if not self._initialized:
            raise ValueError("LocalVectorBackend not initialized. Call initialize() first.")

        # SECURITY: Validate store_id
        safe_store_id = _validate_store_id(store_id)

        # Count files in this store
        file_count = len(set(
            meta.file_path for meta in self._vector_metadata.values()
            if safe_store_id and meta.file_path.startswith(safe_store_id)
        ))

        return StoreInfo(
            name=safe_store_id,
            description=f"Local vector store with {self._index_metadata.vector_count} vectors",
            created_at=self._index_metadata.created_at,
            updated_at=self._index_metadata.updated_at,
            counts={
                "vectors": self._index_metadata.vector_count,
                "files": file_count,
                "dimension": self._index_metadata.dimension,
                "model": self._index_metadata.model
            }
        )

    async def create_store(self, name: str, description: str = "") -> Dict[str, Any]:
        """
        Create a new store (namespace).

        In local backend, stores are just path prefixes.

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
            "type": "local_vector"
        }

    def clear_index(self) -> None:
        """Clear all vectors from the index."""
        with self._lock:
            self._create_new_index()
            self._save_index()
        logger.info("Index cleared")

    def rebuild_index(self) -> None:
        """
        Rebuild the FAISS index from metadata.

        Useful for cleaning up deleted vectors.
        """
        if not self._vector_metadata:
            logger.warning("No metadata to rebuild from")
            return

        # TODO: Implement full index rebuild from stored content
        logger.warning("Index rebuild not yet implemented")


def get_local_vector_backend_status() -> dict:
    """
    Get the status of the local vector backend dependencies.

    Returns:
        dict with keys:
            - available: bool - True if all dependencies are installed
            - faiss_available: bool
            - sentence_transformers_available: bool
            - numpy_available: bool
            - install_command: str - Command to install missing dependencies
    """
    status = {
        "available": FAISS_AVAILABLE and SENTENCE_TRANSFORMERS_AVAILABLE and NUMPY_AVAILABLE,
        "faiss_available": FAISS_AVAILABLE,
        "sentence_transformers_available": SENTENCE_TRANSFORMERS_AVAILABLE,
        "numpy_available": NUMPY_AVAILABLE,
        "supported_models": SUPPORTED_MODELS,
        "install_command": "uv pip install 'faiss-cpu>=1.7.4' 'sentence-transformers>=2.2.0' 'numpy>=1.24.0'"
    }

    if FAISS_AVAILABLE:
        try:
            import faiss
            status["faiss_version"] = getattr(faiss, "__version__", "unknown")
        except Exception:
            status["faiss_version"] = "unknown"

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
