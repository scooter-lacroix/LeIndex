from typing import List, Optional, Any, Dict, Literal
from dataclasses import dataclass

@dataclass
class FileMetadata:
    path: str
    hash: str
    last_modified: Optional[str] = None
    size: Optional[int] = None

@dataclass
class StoreFile:
    external_id: Optional[str]
    metadata: Optional[FileMetadata]

@dataclass
class ChunkType:
    type: Literal["text", "image_url", "audio_url", "video_url"]
    text: Optional[str] = None
    score: float = 0.0
    metadata: Optional[FileMetadata] = None
    chunk_index: Optional[int] = None
    generated_metadata: Optional[Dict[str, Any]] = None
    filename: Optional[str] = None # For web results

@dataclass
class SearchResponse:
    data: List[ChunkType]

@dataclass
class AskResponse:
    answer: str
    sources: List[ChunkType]

@dataclass
class StoreInfo:
    name: str
    description: str
    created_at: str
    updated_at: str
    counts: Dict[str, int]

@dataclass
class UploadFileOptions:
    external_id: str
    overwrite: bool = True
    metadata: Optional[FileMetadata] = None

@dataclass
class SearchOptions:
    rerank: bool = True
    top_k: int = 10
    include_web: bool = False
    content: bool = False # Show content in result
    use_zoekt: bool = False # Use Zoekt strategy if available
    content_boost: float = 1.0
    filepath_boost: float = 1.0
    highlight_pre_tag: str = "<em>"
    highlight_post_tag: str = "</em>"

@dataclass
class Metrics:
    """Performance metrics for LEANN backend."""
    # Index metrics
    vector_count: int
    storage_size_bytes: int

    # Search performance (in seconds)
    search_latency_p50: float
    search_latency_p95: float
    search_latency_p99: float
    avg_search_latency: float

    # Embedding performance (in seconds)
    avg_embedding_time: float
    total_embeddings: int

    # Memory usage (in bytes)
    memory_usage_bytes: int

    # Cache statistics
    cache_hits: int = 0
    cache_misses: int = 0
    cache_hit_rate: float = 0.0

    # Timing statistics
    total_searches: int = 0
    total_uploads: int = 0

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)


@dataclass
class BatchUploadResult:
    """Result of batch upload operation."""
    success_count: int
    failure_count: int
    total_files: int
    errors: List[Dict[str, Any]]

    def to_dict(self) -> Dict[str, Any]:
        return asdict(self)


@dataclass
class BatchUploadOptions:
    """Options for batch upload operations."""
    max_concurrent: int = 1  # Process sequentially for now
    continue_on_error: bool = True  # Continue processing if individual files fail
    max_batch_size: int = 100  # Maximum files per batch
