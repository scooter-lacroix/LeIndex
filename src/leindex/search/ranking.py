"""
Search Result Ranking Algorithm.

PRODUCT.MD ALIGNMENT:
---------------------
"Enhanced Search Capabilities: Implement sophisticated result ranking combining:
- Code relevance (semantic similarity)
- File recency/frequency
- Path importance (tests vs. source)
- User behavior analytics"

This module implements multi-factor scoring for search results to provide
the most relevant results to users.
"""

import os
import re
import logging
from dataclasses import dataclass, field
from typing import List, Dict, Any, Optional, Tuple, Set
from datetime import datetime
from enum import Enum
from collections import defaultdict

logger = logging.getLogger(__name__)


class PathImportance(Enum):
    """
    Categories for path-based importance scoring.
    """
    CORE_SOURCE = "core_source"      # Main application code (src/, lib/, app/)
    CONFIG = "config"                 # Configuration files
    TEST = "test"                     # Test files
    DOCS = "docs"                     # Documentation
    BUILD = "build"                   # Build scripts, tooling
    DEPS = "deps"                     # Dependencies, vendor
    ASSETS = "assets"                 # Static assets
    UNKNOWN = "unknown"               # Uncategorized


@dataclass
class RankingConfig:
    """
    Configuration for ranking algorithm weights.
    """
    # Weights for different factors (sum should ideally be 1.0)
    semantic_weight: float = 0.50      # Semantic similarity score
    recency_weight: float = 0.15       # How recently the file was modified
    frequency_weight: float = 0.15     # How often the file is searched
    path_importance_weight: float = 0.15  # Path-based importance
    file_size_weight: float = 0.05     # Prefer moderately sized files

    # Recency scoring parameters
    recency_half_life_days: int = 30   # Days for score to halve
    max_recency_bonus: float = 1.0     # Maximum bonus for recent files

    # Frequency scoring parameters
    frequency_decay_factor: float = 0.95  # How fast frequency scores decay
    min_access_count: int = 2          # Minimum accesses before frequency matters

    # Path importance scores
    path_importance_scores: Dict[PathImportance, float] = field(default_factory=lambda: {
        PathImportance.CORE_SOURCE: 1.0,
        PathImportance.CONFIG: 0.7,
        PathImportance.TEST: 0.5,
        PathImportance.DOCS: 0.4,
        PathImportance.BUILD: 0.3,
        PathImportance.DEPS: 0.1,
        PathImportance.ASSETS: 0.1,
        PathImportance.UNKNOWN: 0.5,
    })

    # File size parameters (prefer moderate sizes)
    optimal_size_min: int = 1000       # 1KB
    optimal_size_max: int = 100000     # 100KB

    # User behavior tracking
    enable_user_tracking: bool = True
    tracking_window_size: int = 100    # Number of recent searches to track


@dataclass
class SearchResult:
    """
    Represents a search result with ranking metadata.
    """
    file_path: str
    original_score: float              # Original semantic/vector similarity score
    ranked_score: float = 0.0          # Final ranked score
    content_preview: str = ""
    metadata: Dict[str, Any] = field(default_factory=dict)

    # Component scores for debugging
    semantic_component: float = 0.0
    recency_component: float = 0.0
    frequency_component: float = 0.0
    path_component: float = 0.0
    size_component: float = 0.0


class UserBehaviorTracker:
    """
    Tracks user search and access patterns for personalized ranking.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "User behavior analytics" - tracks which files users frequently access
    and search for to improve ranking over time.
    """

    def __init__(self, window_size: int = 100):
        """
        Initialize the behavior tracker.

        Args:
            window_size: Number of recent searches to keep in memory
        """
        self.window_size = window_size
        self._search_history: List[Tuple[str, str, float]] = []  # (query, file_path, score)
        self._access_counts: Dict[str, int] = defaultdict(int)
        self._last_access: Dict[str, datetime] = {}
        self._lock = __import__('threading').Lock()

    def record_search(self, query: str, results: List[SearchResult]):
        """
        Record a search and its results.

        Args:
            query: The search query
            results: List of search results
        """
        with self._lock:
            timestamp = datetime.now()
            for result in results:
                file_path = result.file_path
                self._access_counts[file_path] += 1
                self._last_access[file_path] = timestamp
                self._search_history.append((query, file_path, result.original_score))

            # Trim history to window size
            if len(self._search_history) > self.window_size:
                excess = len(self._search_history) - self.window_size
                self._search_history = self._search_history[excess:]

    def get_access_frequency(self, file_path: str) -> int:
        """Get the number of times a file has been accessed."""
        return self._access_counts.get(file_path, 0)

    def get_last_access(self, file_path: str) -> Optional[datetime]:
        """Get the last access time for a file."""
        return self._last_access.get(file_path)

    def get_frequent_files(self, threshold: int = 5) -> Set[str]:
        """Get files accessed more than threshold times."""
        return {path for path, count in self._access_counts.items() if count >= threshold}

    def get_recent_queries(self, limit: int = 10) -> List[str]:
        """Get recent unique queries."""
        recent = self._search_history[-limit:] if self._search_history else []
        return list(set(query for query, _, _ in recent))

    def clear_history(self):
        """Clear all tracking history."""
        with self._lock:
            self._search_history.clear()
            self._access_counts.clear()
            self._last_access.clear()


class PathImportanceClassifier:
    """
    Classifies file paths into importance categories.
    """

    # Patterns for detecting path types
    SOURCE_PATTERNS = [
        re.compile(r'^(src/|lib/|app/|main/|core/|server/|client/)'),
        re.compile(r'^((src|lib|app|main|core|server|client)/.*\.(py|js|ts|java|go|rs|c|cpp|h|cs)$)'),
    ]

    TEST_PATTERNS = [
        re.compile(r'^(test/|tests/|__tests__/|spec/|testing/)'),
        re.compile(r'(_test\.|_spec\.|test_\.|spec_\.|\.test\.|\.spec\.)(py|js|ts|java|go|rs)$'),
    ]

    CONFIG_PATTERNS = [
        re.compile(r'^(\.?config|settings|conf|cfg)/'),
        re.compile(r'\.(json|yaml|yml|toml|ini|conf|cfg|env|config)$'),
        re.compile(r'^(package\.json|tsconfig\.json|pyproject\.toml|setup\.py|go\.mod|cargo\.toml)$'),
    ]

    BUILD_PATTERNS = [
        re.compile(r'^(build/|scripts/|tools/|\.github/|\.gitlab/|ci/|cd/|docker/|k8s/|infrastructure/)$'),
        re.compile(r'\.(sh|bash|zsh|fish|makefile|dockerfile|dockerignore|gitignore|gitattributes)$', re.IGNORECASE),
    ]

    DOCS_PATTERNS = [
        re.compile(r'^(docs/|doc/|documentation/|guide/|README|CHANGELOG|LICENSE|CONTRIBUTING|\.md$|\.rst$|\.txt$)'),
    ]

    DEPS_PATTERNS = [
        re.compile(r'^(node_modules/|vendor/|venv/|env/|\.env/|third_party/|deps/|dependencies/|\.bundle/)'),
        re.compile(r'^\.(git|svn|hg)/'),
    ]

    ASSETS_PATTERNS = [
        re.compile(r'^(assets/|static/|public/|resources/|media/|images/|fonts/|styles/|css/)'),
        re.compile(r'\.(png|jpg|jpeg|gif|svg|ico|bmp|webp|woff|woff2|ttf|eot|css|scss|less|sass)$'),
    ]

    @classmethod
    def classify(cls, file_path: str) -> PathImportance:
        """
        Classify a file path into an importance category.

        Args:
            file_path: The file path to classify

        Returns:
            PathImportance category
        """
        normalized_path = file_path.replace('\\', '/')

        # Check in order of specificity (most specific first)

        # Dependencies (should be lowest priority)
        if any(pattern.search(normalized_path) for pattern in cls.DEPS_PATTERNS):
            return PathImportance.DEPS

        # Assets
        if any(pattern.search(normalized_path) for pattern in cls.ASSETS_PATTERNS):
            return PathImportance.ASSETS

        # Build/tooling
        if any(pattern.search(normalized_path) for pattern in cls.BUILD_PATTERNS):
            return PathImportance.BUILD

        # Documentation
        if any(pattern.search(normalized_path) for pattern in cls.DOCS_PATTERNS):
            return PathImportance.DOCS

        # Tests
        if any(pattern.search(normalized_path) for pattern in cls.TEST_PATTERNS):
            return PathImportance.TEST

        # Configuration
        if any(pattern.search(normalized_path) for pattern in cls.CONFIG_PATTERNS):
            return PathImportance.CONFIG

        # Core source
        if any(pattern.search(normalized_path) for pattern in cls.SOURCE_PATTERNS):
            return PathImportance.CORE_SOURCE

        return PathImportance.UNKNOWN


class ResultRanker:
    """
    Main ranking engine that combines multiple signals to rank search results.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Implement sophisticated result ranking combining:
    - Code relevance (semantic similarity)
    - File recency/frequency
    - Path importance (tests vs. source)
    - User behavior analytics"
    """

    def __init__(
        self,
        config: Optional[RankingConfig] = None,
        behavior_tracker: Optional[UserBehaviorTracker] = None
    ):
        """
        Initialize the result ranker.

        Args:
            config: Ranking configuration (uses defaults if None)
            behavior_tracker: User behavior tracker (creates new if None)
        """
        self.config = config or RankingConfig()
        self.behavior_tracker = behavior_tracker or (
            UserBehaviorTracker(self.config.tracking_window_size)
            if self.config.enable_user_tracking
            else None
        )
        self.path_classifier = PathImportanceClassifier()

    def calculate_semantic_score(
        self,
        original_score: float
    ) -> float:
        """
        Normalize and apply semantic similarity score.

        Args:
            original_score: Original semantic/vector similarity score

        Returns:
            Normalized semantic component score (0-1)
        """
        # Most similarity scores are already 0-1, but clamp to be safe
        return max(0.0, min(1.0, original_score))

    def calculate_recency_score(
        self,
        file_path: str,
        last_modified: Optional[Any] = None
    ) -> float:
        """
        Calculate recency score based on file modification time.

        More recently modified files get higher scores.

        Args:
            file_path: Path to the file
            last_modified: Last modified timestamp (datetime or isoformat string)

        Returns:
            Recency component score (0-1)
        """
        try:
            # Parse last modified time
            if isinstance(last_modified, str):
                mod_time = datetime.fromisoformat(last_modified.replace('Z', '+00:00'))
            elif isinstance(last_modified, datetime):
                mod_time = last_modified
            else:
                # Try to get from filesystem
                full_path = os.path.abspath(file_path)
                if os.path.exists(full_path):
                    mod_time = datetime.fromtimestamp(os.path.getmtime(full_path))
                else:
                    return 0.5  # Default for files we can't check
        except (OSError, ValueError, TypeError):
            return 0.5  # Default on error

        # Calculate days since modification
        now = datetime.now(mod_time.tzinfo) if mod_time.tzinfo else datetime.now()
        days_since = (now - mod_time).total_seconds() / 86400

        # Exponential decay based on half-life
        decay = 0.5 ** (days_since / self.config.recency_half_life_days)

        return self.config.max_recency_bonus * decay

    def calculate_frequency_score(
        self,
        file_path: str
    ) -> float:
        """
        Calculate frequency score based on user access patterns.

        Frequently accessed files get higher scores.

        Args:
            file_path: Path to the file

        Returns:
            Frequency component score (0-1)
        """
        if not self.behavior_tracker:
            return 0.0

        access_count = self.behavior_tracker.get_access_frequency(file_path)

        if access_count < self.config.min_access_count:
            return 0.0

        # Logarithmic scaling to avoid giving too much advantage to very popular files
        import math
        normalized = min(1.0, math.log(access_count - self.config.min_access_count + 2) / 5)

        return normalized

    def calculate_path_importance_score(
        self,
        file_path: str
    ) -> float:
        """
        Calculate path importance score.

        Files in important directories (core source) get higher scores.

        Args:
            file_path: Path to the file

        Returns:
            Path importance component score (0-1)
        """
        category = self.path_classifier.classify(file_path)
        base_score = self.config.path_importance_scores.get(category, 0.5)

        # Additional heuristics for fine-tuning

        # Penalize files very deep in directory structure
        depth = file_path.count('/') - file_path.replace('\\', '/').count('/')
        depth_penalty = max(0.7, 1.0 - (depth * 0.02))

        # Bonus for files near the "root" of src/
        if any(src in file_path for src in ['src/', 'lib/', 'app/']):
            # Extract relative path after src/
            for src in ['src/', 'lib/', 'app/']:
                if src in file_path:
                    rel_path = file_path.split(src, 1)[1] if src in file_path else ""
                    if rel_path and '/' not in rel_path[:50]:  # Top-level file
                        depth_penalty *= 1.1
                    break

        return base_score * depth_penalty

    def calculate_file_size_score(
        self,
        file_path: str,
        file_size: Optional[int] = None
    ) -> float:
        """
        Calculate file size score.

        Prefer moderately sized files - too small might be trivial,
        too large might be overwhelming.

        Args:
            file_path: Path to the file
            file_size: File size in bytes (will fetch from FS if None)

        Returns:
            File size component score (0-1)
        """
        try:
            if file_size is None:
                full_path = os.path.abspath(file_path)
                if os.path.exists(full_path):
                    file_size = os.path.getsize(full_path)
                else:
                    return 0.5  # Default
        except OSError:
            return 0.5

        # Prefer files in optimal size range
        if self.config.optimal_size_min <= file_size <= self.config.optimal_size_max:
            return 1.0
        elif file_size < self.config.optimal_size_min:
            # Small files: linear penalty
            ratio = file_size / self.config.optimal_size_min
            return 0.3 + 0.7 * ratio
        else:
            # Large files: logarithmic penalty
            import math
            excess = file_size - self.config.optimal_size_max
            return max(0.3, 1.0 - math.log(excess + 1) / 15)

    def rank_results(
        self,
        results: List[Dict[str, Any]],
        query: str = ""
    ) -> List[SearchResult]:
        """
        Rank search results using multi-factor scoring.

        Args:
            results: List of search result dictionaries
            query: Optional search query for behavior tracking

        Returns:
            List of ranked SearchResult objects, sorted by ranked_score
        """
        ranked_results: List[SearchResult] = []

        for result in results:
            # Extract common fields
            file_path = result.get('file_path') or result.get('path') or result.get('id', '')
            original_score = float(result.get('score', result.get('similarity', 0.5)))
            metadata = result.get('metadata', {})

            search_result = SearchResult(
                file_path=file_path,
                original_score=original_score,
                content_preview=result.get('content', result.get('text', ''))[:500],
                metadata=metadata
            )

            # Calculate component scores
            search_result.semantic_component = (
                self.calculate_semantic_score(original_score) * self.config.semantic_weight
            )

            search_result.recency_component = (
                self.calculate_recency_score(
                    file_path,
                    metadata.get('last_modified') or metadata.get('modified')
                ) * self.config.recency_weight
            )

            search_result.frequency_component = (
                self.calculate_frequency_score(file_path) * self.config.frequency_weight
            )

            search_result.path_component = (
                self.calculate_path_importance_score(file_path) * self.config.path_importance_weight
            )

            search_result.size_component = (
                self.calculate_file_size_score(
                    file_path,
                    metadata.get('size')
                ) * self.config.file_size_weight
            )

            # Calculate final ranked score
            search_result.ranked_score = (
                search_result.semantic_component +
                search_result.recency_component +
                search_result.frequency_component +
                search_result.path_component +
                search_result.size_component
            )

            ranked_results.append(search_result)

        # Sort by ranked score (descending)
        ranked_results.sort(key=lambda r: r.ranked_score, reverse=True)

        # Track user behavior if enabled
        if self.behavior_tracker and query:
            self.behavior_tracker.record_search(query, ranked_results)

        return ranked_results

    def get_ranking_explanation(self, result: SearchResult) -> str:
        """
        Generate a human-readable explanation of why a result was ranked.

        Args:
            result: A ranked search result

        Returns:
            Explanation string
        """
        parts = []

        parts.append(f"Base similarity: {result.semantic_component / self.config.semantic_weight:.2f}")

        if result.recency_component > 0:
            parts.append(f"Recency bonus: +{result.recency_component:.2f}")

        if result.frequency_component > 0:
            parts.append(f"Frequency bonus: +{result.frequency_component:.2f}")

        if result.path_component > 0:
            category = self.path_classifier.classify(result.file_path)
            parts.append(f"Path ({category.value}): +{result.path_component:.2f}")

        if result.size_component > 0:
            parts.append(f"Size factor: +{result.size_component:.2f}")

        explanation = f"Score: {result.ranked_score:.2f} | " + " | ".join(parts)
        return explanation


def create_default_ranker() -> ResultRanker:
    """
    Create a ResultRanker with default configuration.

    Returns:
        Configured ResultRanker instance
    """
    config = RankingConfig()
    behavior_tracker = UserBehaviorTracker()
    return ResultRanker(config, behavior_tracker)
