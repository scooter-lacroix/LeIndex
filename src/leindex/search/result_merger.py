"""
Multi-Backend Search Result Merger

This module implements a unified search result merger that combines results from
multiple search backends (LEANN, Tantivy, Zoekt) into a single ranked list.

Features:
- Reciprocal Rank Fusion (RRF) for rank-based merging
- Weighted merging with configurable weights
- Score normalization across heterogeneous backends
- Deduplication based on file path + line range
- Backend-specific result conversion

Phase 3: Search Integration, Optimization, and Production Readiness
Spec: conductor/tracks/mcp_consolidation_local_vector_20251230/spec.md
"""

from __future__ import annotations

import statistics
import logging
from dataclasses import dataclass, field
from typing import List, Optional, Dict, Any, Tuple, Set
from enum import Enum
from collections import defaultdict

logger = logging.getLogger(__name__)


class SearchBackend(Enum):
    """Enum representing the available search backends."""

    LEANN = "leann"
    TANTIVY = "tantivy"
    ZOEKT = "zoekt"


@dataclass
class MergedSearchResult:
    """
    Unified search result from any backend.

    This dataclass represents a search result after merging from multiple
    backends, with normalized scoring and metadata from all sources.

    Attributes:
        file_path: Path to the file containing the match
        score: Final normalized score (0-1)
        backend: Primary backend that found this result
        start_line: Starting line number of the match (1-indexed)
        end_line: Ending line number of the match (1-indexed)
        content: Content snippet for the match
        chunk_index: Index of the chunk if from vector search
        chunk_type: Type of chunk (function, class, text) if from vector search
        parent_context: Parent class/module context if from vector search
        original_scores: Scores from each backend that found this result
        backends_found: Set of all backends that found this result
        rank: Final rank after merging (1-indexed)
    """

    file_path: str
    score: float = 0.0
    backend: SearchBackend = SearchBackend.LEANN
    start_line: Optional[int] = None
    end_line: Optional[int] = None
    content: Optional[str] = None
    chunk_index: Optional[int] = None
    chunk_type: Optional[str] = None
    parent_context: Optional[str] = None
    original_scores: Dict[SearchBackend, float] = field(default_factory=dict)
    backends_found: Set[SearchBackend] = field(default_factory=set)
    rank: int = 0

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for serialization."""
        return {
            "file_path": self.file_path,
            "score": self.score,
            "backend": self.backend.value,
            "start_line": self.start_line,
            "end_line": self.end_line,
            "content": self.content[:200]
            if self.content
            else None,  # Truncate for display
            "chunk_index": self.chunk_index,
            "chunk_type": self.chunk_type,
            "parent_context": self.parent_context,
            "original_scores": {b.value: s for b, s in self.original_scores.items()},
            "backends_found": [b.value for b in self.backends_found],
            "rank": self.rank,
        }


class ScoreNormalizer:
    """
    Normalize scores from different backends to a common scale.

    Different search backends return scores on different scales:
    - LEANN: Cosine similarity (0-1, higher is better)
    - Tantivy: BM25 score (unbounded, higher is better)
    - Zoekt: No explicit score, uses rank-based scoring

    This class provides methods to normalize these scores to a 0-1 range.
    """

    @staticmethod
    def min_max_normalize(scores: List[float]) -> List[float]:
        """
        Normalize scores to 0-1 range using min-max scaling.

        Args:
            scores: List of scores to normalize

        Returns:
            List of normalized scores in 0-1 range
        """
        if not scores:
            return []

        min_score = min(scores)
        max_score = max(scores)

        if max_score == min_score:
            return [0.5] * len(scores)

        return [(s - min_score) / (max_score - min_score) for s in scores]

    @staticmethod
    def percentile_normalize(scores: List[float]) -> List[float]:
        """
        Convert scores to percentile ranks (0-100).

        Args:
            scores: List of scores to normalize

        Returns:
            List of percentile ranks (0-100)
        """
        if not scores:
            return []

        sorted_scores = sorted(scores)
        return [(sorted_scores.index(s) + 1) / len(sorted_scores) * 100 for s in scores]

    @staticmethod
    def z_score_normalize(scores: List[float]) -> List[float]:
        """
        Normalize scores using z-score (standard score).

        Args:
            scores: List of scores to normalize

        Returns:
            List of z-scores (can be negative)
        """
        if not scores:
            return []

        mean = statistics.mean(scores)
        stdev = statistics.stdev(scores) if len(scores) > 1 else 1.0

        if stdev == 0:
            return [0.0] * len(scores)

        return [(s - mean) / stdev for s in scores]

    @staticmethod
    def clamp_score(score: float, min_val: float = 0.0, max_val: float = 1.0) -> float:
        """
        Clamp a score to a specified range.

        Args:
            score: Score to clamp
            min_val: Minimum allowed value
            max_val: Maximum allowed value

        Returns:
            Clamped score
        """
        return max(min_val, min(score, max_val))


class ResultConverter:
    """
    Convert search results from each backend to MergedSearchResult format.
    """

    @staticmethod
    def from_leann_chunk(
        chunk_data: Any,
        original_score: float,
        backend: SearchBackend = SearchBackend.LEANN,
    ) -> MergedSearchResult:
        """
        Convert LEANN ChunkType to MergedSearchResult.

        Args:
            chunk_data: ChunkType object from LEANN search
            original_score: Raw similarity score from LEANN
            backend: The backend identifier

        Returns:
            MergedSearchResult with populated fields
        """
        # Extract metadata from chunk
        metadata = getattr(chunk_data, "metadata", None)
        generated_meta = getattr(chunk_data, "generated_metadata", {})

        file_path = getattr(metadata, "path", "") if metadata else ""
        if not file_path and hasattr(chunk_data, "file_path"):
            file_path = chunk_data.file_path

        return MergedSearchResult(
            file_path=file_path,
            score=ScoreNormalizer.clamp_score(original_score),
            backend=backend,
            start_line=generated_meta.get("start_line"),
            end_line=generated_meta.get("end_line"),
            content=getattr(chunk_data, "text", None),
            chunk_index=getattr(chunk_data, "chunk_index", None),
            chunk_type=generated_meta.get("chunk_type"),
            parent_context=generated_meta.get("parent_context"),
            original_scores={backend: original_score},
            backends_found={backend},
        )

    @staticmethod
    def from_tantivy_result(
        file_path: str,
        result_doc: Dict[str, Any],
        score: float = 0.0,
        backend: SearchBackend = SearchBackend.TANTIVY,
    ) -> MergedSearchResult:
        """
        Convert Tantivy result to MergedSearchResult.

        Args:
            file_path: Path to the file
            result_doc: Document dict from Tantivy
            score: BM25 score from Tantivy
            backend: The backend identifier

        Returns:
            MergedSearchResult with populated fields
        """
        content = result_doc.get("content", "")

        # Try to extract line information if available
        start_line = result_doc.get("start_line")
        end_line = result_doc.get("end_line")

        return MergedSearchResult(
            file_path=file_path,
            score=ScoreNormalizer.clamp_score(score),
            backend=backend,
            start_line=start_line,
            end_line=end_line,
            content=content[:500] if content else None,  # Truncate long content
            original_scores={backend: score},
            backends_found={backend},
        )

    @staticmethod
    def from_zoekt_results(
        zoekt_results: Dict[str, List[Tuple[int, str]]],
        backend: SearchBackend = SearchBackend.ZOEKT,
    ) -> List[MergedSearchResult]:
        """
        Convert Zoekt results to list of MergedSearchResult.

        Args:
            zoekt_results: Dict mapping file_path to [(line_num, content), ...]
            backend: The backend identifier

        Returns:
            List of MergedSearchResult objects
        """
        results = []

        for file_path, matches in zoekt_results.items():
            for line_num, content in matches:
                result = MergedSearchResult(
                    file_path=file_path,
                    score=0.0,  # Zoekt doesn't provide explicit scores
                    backend=backend,
                    start_line=line_num,
                    end_line=line_num,
                    content=content,
                    original_scores={backend: 1.0 / (line_num + 1)},  # Rank-based score
                    backends_found={backend},
                )
                results.append(result)

        return results


def reciprocal_rank_fusion(
    ranked_lists: List[List[MergedSearchResult]], k: float = 60.0
) -> Dict[str, float]:
    """
    Combine multiple ranked lists using Reciprocal Rank Fusion (RRF) algorithm.

    RRF is particularly effective for combining results from heterogeneous
    backends where scores are not directly comparable (e.g., BM25 vs cosine similarity).

    The algorithm:
    1. For each result, calculate RRF score = sum(1 / (k + rank))
    2. Results appearing consistently at high ranks across lists get highest scores

    Args:
        ranked_lists: List of ranked result lists, one per backend
        k: Smoothing constant (default 60, from RRF paper)

    Returns:
        Dict mapping result key to RRF score
    """
    rrf_scores: Dict[str, float] = defaultdict(float)

    for ranked_list in ranked_lists:
        for rank, result in enumerate(ranked_list, start=1):
            # Use file_path + start_line as unique key
            key = _get_result_key(result)
            rrf_scores[key] += 1.0 / (k + rank)

    return dict(rrf_scores)


def _get_result_key(result: MergedSearchResult) -> str:
    """Generate a unique key for a search result."""
    start = result.start_line or 0
    end = result.end_line or start
    return f"{result.file_path}:{start}-{end}"


def _results_overlap(r1: MergedSearchResult, r2: MergedSearchResult) -> bool:
    """Check if two results have overlapping line ranges."""
    # If either has no line info, they might overlap - be conservative
    if r1.start_line is None or r1.end_line is None:
        return r1.file_path == r2.file_path
    if r2.start_line is None or r2.end_line is None:
        return r1.file_path == r2.file_path

    # Check for actual overlap
    return r1.start_line <= r2.end_line and r1.end_line >= r2.start_line


def deduplicate_results(
    results: List[MergedSearchResult], merge_keys: bool = True
) -> List[MergedSearchResult]:
    """
    Remove duplicate results from multiple backends.

    Results are considered duplicates if they match on:
    - file_path AND start_line AND end_line (exact match)
    - OR file_path AND overlapping line ranges
    - OR file_path when one has None line info

    When duplicates exist, the result with the highest score is kept,
    and backends_found is merged.

    Args:
        results: List of results to deduplicate
        merge_keys: Whether to merge results with overlapping line ranges

    Returns:
        Deduplicated list of results
    """
    seen: Dict[str, MergedSearchResult] = {}

    for result in results:
        # Try to find an existing result that overlaps
        matched_key = None
        for existing_key, existing_result in seen.items():
            if result.file_path == existing_result.file_path:
                if _results_overlap(result, existing_result):
                    matched_key = existing_key
                    break

        if matched_key is None:
            # No overlap found, use current result
            key = _get_result_key(result)
            seen[key] = result
        else:
            # Merge with existing result
            existing = seen[matched_key]

            # If new result has higher score, update the existing result's fields
            if result.score > existing.score:
                existing.file_path = result.file_path
                existing.score = result.score
                existing.backend = result.backend
                # Preserve wider line range to maintain overlap detection
                new_start = (
                    result.start_line
                    if result.start_line is not None
                    else existing.start_line
                )
                new_end = (
                    result.end_line
                    if result.end_line is not None
                    else existing.end_line
                )
                existing.start_line = (
                    min(existing.start_line or new_start, new_start)
                    if existing.start_line is not None
                    else new_start
                )
                existing.end_line = (
                    max(existing.end_line or new_end, new_end)
                    if existing.end_line is not None
                    else new_end
                )
                existing.content = result.content
                existing.chunk_index = result.chunk_index
                existing.chunk_type = result.chunk_type
                existing.parent_context = result.parent_context

            # Merge backends_found
            seen[matched_key].backends_found.update(result.backends_found)

            # Merge original scores
            for backend, score in result.original_scores.items():
                if (
                    backend not in seen[matched_key].original_scores
                    or score > seen[matched_key].original_scores[backend]
                ):
                    seen[matched_key].original_scores[backend] = score

    return list(seen.values())


def weighted_merge_scores(
    results_by_backend: Dict[SearchBackend, List[MergedSearchResult]],
    weights: Dict[SearchBackend, float],
) -> Dict[str, float]:
    """
    Merge scores using weighted average of normalized scores.

    Args:
        results_by_backend: Results grouped by backend
        weights: Weight for each backend (should sum to ~1.0)

    Returns:
        Dict mapping result key to weighted score
    """
    merged_scores: Dict[str, float] = defaultdict(float)
    result_presence: Dict[str, int] = defaultdict(int)

    for backend, results in results_by_backend.items():
        if not results:
            continue

        weight = weights.get(backend, 1.0)

        # Get scores from this backend's results
        raw_scores = [r.original_scores.get(backend, r.score) for r in results]

        # Normalize scores to 0-1 range
        normalized = ScoreNormalizer.min_max_normalize(raw_scores)

        # Apply weight and accumulate
        for result, norm_score in zip(results, normalized):
            key = _get_result_key(result)
            merged_scores[key] += norm_score * weight
            result_presence[key] += 1

    # Adjust for backends that didn't find the result
    total_backends = len(results_by_backend)
    for key in merged_scores:
        presence = result_presence[key]
        if presence < total_backends:
            # Penalize results not found in all backends
            merged_scores[key] *= presence / total_backends

    return dict(merged_scores)


class SearchResultMerger:
    """
    Merge search results from multiple backends into a unified ranked list.

    This class provides a unified interface for combining results from:
    - LEANN (vector semantic search)
    - Tantivy (full-text search)
    - Zoekt (regex/symbolic search)

    Supported merge strategies:
    - "rrf": Reciprocal Rank Fusion (default, recommended)
    - "weighted": Weighted average of normalized scores
    - "round_robin": Take results from each backend in turn

    Example:
        merger = SearchResultMerger(
            merge_strategy="rrf",
            weights={"leann": 0.5, "tantivy": 0.3, "zoekt": 0.2}
        )

        results = merger.merge(
            leann_results=leann_chunks,
            tantivy_results=tantivy_docs,
            zoekt_results=zoekt_matches
        )
    """

    DEFAULT_WEIGHTS = {
        SearchBackend.LEANN: 0.5,
        SearchBackend.TANTIVY: 0.3,
        SearchBackend.ZOEKT: 0.2,
    }

    def __init__(
        self,
        merge_strategy: str = "rrf",
        weights: Optional[Dict[SearchBackend, float]] = None,
        enable_deduplication: bool = True,
        k: float = 60.0,  # RRF parameter
        max_results: int = 100,
    ):
        """
        Initialize the SearchResultMerger.

        Args:
            merge_strategy: Strategy to use for merging ("rrf", "weighted")
            weights: Backend weights for weighted merging
            enable_deduplication: Whether to remove duplicate results
            k: RRF smoothing constant (default 60)
            max_results: Maximum number of results to return
        """
        self.merge_strategy = merge_strategy
        self.weights = weights or self.DEFAULT_WEIGHTS.copy()
        self.enable_deduplication = enable_deduplication
        self.k = k
        self.max_results = max_results

        # Validate merge strategy
        if merge_strategy not in ("rrf", "weighted"):
            logger.warning(f"Unknown merge strategy '{merge_strategy}', using 'rrf'")
            self.merge_strategy = "rrf"

        logger.info(
            f"SearchResultMerger initialized: strategy={merge_strategy}, "
            f"deduplication={enable_deduplication}, max_results={max_results}"
        )

    def merge(
        self,
        leann_results: Optional[List[Any]] = None,
        tantivy_results: Optional[List[Tuple[str, Dict[str, Any]]]] = None,
        zoekt_results: Optional[Dict[str, List[Tuple[int, str]]]] = None,
    ) -> List[MergedSearchResult]:
        """
        Merge results from multiple backends.

        Args:
            leann_results: List of ChunkType results from LEANN
            tantivy_results: List of (path, doc) tuples from Tantivy
            zoekt_results: Dict of file_path -> [(line, content)] from Zoekt

        Returns:
            List of MergedSearchResult objects, sorted by final score
        """
        # Convert results to unified format and track original backends
        converted_results: List[MergedSearchResult] = []

        # Convert LEANN results
        if leann_results:
            for chunk in leann_results:
                # Handle both raw ChunkType and pre-converted MergedSearchResult
                if isinstance(chunk, MergedSearchResult):
                    converted_results.append(chunk)
                else:
                    # Extract score from chunk
                    score = getattr(chunk, "score", 0.0)
                    converted = ResultConverter.from_leann_chunk(
                        chunk, score, SearchBackend.LEANN
                    )
                    converted_results.append(converted)

        # Convert Tantivy results
        if tantivy_results:
            for file_path, doc in tantivy_results:
                score = doc.get("score", 0.0)
                converted = ResultConverter.from_tantivy_result(
                    file_path, doc, score, SearchBackend.TANTIVY
                )
                converted_results.append(converted)

        # Convert Zoekt results
        if zoekt_results:
            zoekt_converted = ResultConverter.from_zoekt_results(
                zoekt_results, SearchBackend.ZOEKT
            )
            converted_results.extend(zoekt_converted)

        # Deduplicate and merge backends_found
        if self.enable_deduplication:
            converted_results = deduplicate_results(converted_results)

        if not converted_results:
            return []

        # Group results by backend for merging
        results_by_backend: Dict[SearchBackend, List[MergedSearchResult]] = {
            SearchBackend.LEANN: [],
            SearchBackend.TANTIVY: [],
            SearchBackend.ZOEKT: [],
        }

        for result in converted_results:
            for backend in result.backends_found:
                if backend in results_by_backend:
                    results_by_backend[backend].append(result)

        # Apply merge strategy
        if self.merge_strategy == "rrf":
            merged_scores = self._merge_rrf(results_by_backend)
        else:  # weighted
            merged_scores = self._merge_weighted(results_by_backend)

        # Apply merged scores to results
        for result in converted_results:
            key = _get_result_key(result)
            final_score = merged_scores.get(key, result.score)
            result.score = ScoreNormalizer.clamp_score(final_score)

        # Sort by final score and assign ranks
        converted_results.sort(key=lambda r: r.score, reverse=True)

        for rank, result in enumerate(converted_results, start=1):
            result.rank = rank

        # Limit results
        return converted_results[: self.max_results]

    def _merge_rrf(
        self, results_by_backend: Dict[SearchBackend, List[MergedSearchResult]]
    ) -> Dict[str, float]:
        """Merge using Reciprocal Rank Fusion."""
        # Create ranked lists from each backend
        ranked_lists: List[List[MergedSearchResult]] = []

        for backend in [
            SearchBackend.LEANN,
            SearchBackend.TANTIVY,
            SearchBackend.ZOEKT,
        ]:
            results = results_by_backend.get(backend, [])
            if results:
                # Sort by score for this backend
                sorted_results = sorted(results, key=lambda r: r.score, reverse=True)
                ranked_lists.append(sorted_results)

        if not ranked_lists:
            return {}

        return reciprocal_rank_fusion(ranked_lists, k=self.k)

    def _merge_weighted(
        self, results_by_backend: Dict[SearchBackend, List[MergedSearchResult]]
    ) -> Dict[str, float]:
        """Merge using weighted average of normalized scores."""
        return weighted_merge_scores(results_by_backend, self.weights)

    def get_backend_stats(self) -> Dict[str, Any]:
        """Get statistics about the merger configuration."""
        return {
            "merge_strategy": self.merge_strategy,
            "weights": {b.value: w for b, w in self.weights.items()},
            "enable_deduplication": self.enable_deduplication,
            "k": self.k,
            "max_results": self.max_results,
        }


def merge_search_results(
    leann_results: Optional[List[Any]] = None,
    tantivy_results: Optional[List[Tuple[str, Dict[str, Any]]]] = None,
    zoekt_results: Optional[Dict[str, List[Tuple[int, str]]]] = None,
    strategy: str = "rrf",
    weights: Optional[Dict[str, float]] = None,
) -> List[MergedSearchResult]:
    """
    Convenience function to merge search results from multiple backends.

    Args:
        leann_results: Results from LEANN vector search
        tantivy_results: Results from Tantivy
        zoekt_results: Results from Zoekt
        strategy: Merge strategy ("rrf" or "weighted")
        weights: Backend weights (if strategy="weighted")

    Returns:
        Merged and ranked results
    """
    # Convert string weights to enum keys if needed
    enum_weights = None
    if weights:
        enum_weights = {}
        for key, value in weights.items():
            try:
                enum_weights[SearchBackend(key)] = value
            except ValueError:
                logger.warning(f"Unknown backend in weights: {key}")

    merger = SearchResultMerger(merge_strategy=strategy, weights=enum_weights)

    return merger.merge(
        leann_results=leann_results,
        tantivy_results=tantivy_results,
        zoekt_results=zoekt_results,
    )
