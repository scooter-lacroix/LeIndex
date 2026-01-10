"""
Global Index Tier 2: Stale-Allowed Query Cache.

Implements LRU cache for expensive cross-project queries with:
- Stale-allowed reads (serve stale data while rebuilding)
- Async rebuild in background thread
- Race condition prevention (separate rebuilding_keys tracking)
- Memory budget enforcement
"""

import logging
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, Optional, Set, Tuple

from leindex.global_index.lru_tracker import LRUTracker
from leindex.global_index.monitoring import (
    get_global_index_monitor,
    log_global_index_operation,
    CacheError
)

logger = logging.getLogger(__name__)


@dataclass
class CachedQuery:
    """A cached query result with metadata."""
    data: Any
    computed_at: float
    involved_projects: Set[str]
    is_stale: bool = False
    size_mb: float = 0.0


@dataclass
class QueryMetadata:
    """Metadata about a query result."""
    is_stale: bool
    staleness_age_seconds: float
    rebuild_in_progress: bool
    last_updated: float
    source: str  # 'tier1', 'tier2_fresh', 'tier2_stale', 'federation'
    cache_key: Optional[str] = None


class GlobalIndexTier2:
    """
    Tier 2 cache for expensive cross-project queries.

    Key features:
    - Stale-allowed reads: Serve stale data immediately, rebuild async
    - Race condition prevention: Separate rebuilding_keys set with lock
    - Duplicate rebuild prevention: Check rebuilding_keys before triggering
    - LRU eviction: Respect memory budget (510MB default)
    - Project-based invalidation: Mark queries stale when projects update
    """

    def __init__(self, max_size_mb: float = 500.0, max_workers: int = 2):
        """
        Initialize Tier 2 cache.

        Args:
            max_size_mb: Maximum cache size in megabytes
            max_workers: Maximum number of rebuild worker threads
        """
        self.max_size_mb = max_size_mb

        # LRU cache with size tracking
        self.lru_tracker = LRUTracker(max_size_mb=max_size_mb)

        # Internal cache storage (key -> CachedQuery)
        self.cache: Dict[str, CachedQuery] = {}

        # ✅ Race condition fix: separate rebuilding_keys tracking
        self.rebuilding_keys: Set[str] = set()
        self.rebuild_lock = threading.Lock()

        # Background rebuild executor
        self.rebuild_executor = ThreadPoolExecutor(
            max_workers=max_workers,
            thread_name_prefix="tier2_rebuild"
        )

        # Lock for cache access
        self.cache_lock = threading.RLock()

        # Statistics
        self.stats = {
            'queries': 0,
            'cache_hits': 0,
            'cache_misses': 0,
            'stale_serves': 0,
            'rebuilds_triggered': 0,
            'rebuilds_completed': 0,
            'rebuilds_failed': 0,
        }
        self.stats_lock = threading.Lock()

        # Monitoring
        self.monitor = get_global_index_monitor()

    def query(
        self,
        cache_key: str,
        query_func: Callable[[], Any],
        involved_projects: Set[str]
    ) -> Tuple[Any, QueryMetadata]:
        """
        Query cache with stale-allowed reads.

        Args:
            cache_key: Unique cache key for this query
            query_func: Function to compute result (called on cache miss)
            involved_projects: Set of project IDs involved in this query

        Returns:
            Tuple of (result_data, query_metadata)
        """
        with self.stats_lock:
            self.stats['queries'] += 1

        # Check cache
        cached = self.lru_tracker.get(cache_key)

        if cached is not None:
            # Cache hit
            with self.cache_lock:
                if cache_key not in self.cache:
                    # Entry was evicted between get and cache check
                    return self._compute_and_store(
                        cache_key, query_func, involved_projects
                    )

                cached_query = self.cache[cache_key]

            with self.stats_lock:
                self.stats['cache_hits'] += 1

            # Record cache hit metric
            self.monitor.record_cache_hit()

            # ✅ Check if rebuild is in progress
            with self.rebuild_lock:
                is_rebuilding = cache_key in self.rebuilding_keys

            staleness_age = time.time() - cached_query.computed_at

            # Stale entry: serve stale + trigger async rebuild
            if cached_query.is_stale and not is_rebuilding:
                self._rebuild_async(cache_key, query_func, involved_projects)

                with self.stats_lock:
                    self.stats['stale_serves'] += 1

                log_global_index_operation(
                    operation='cache_query_stale',
                    component='tier2_cache',
                    status='success',
                    duration_ms=0,
                    cache_key=cache_key,
                    staleness_age_seconds=staleness_age
                )

                return cached_query.data, QueryMetadata(
                    is_stale=True,
                    staleness_age_seconds=staleness_age,
                    rebuild_in_progress=True,
                    last_updated=cached_query.computed_at,
                    source='tier2_stale',
                    cache_key=cache_key,
                )

            # Fresh entry: return immediately
            log_global_index_operation(
                operation='cache_query_hit',
                component='tier2_cache',
                status='success',
                duration_ms=0,
                cache_key=cache_key
            )

            return cached_query.data, QueryMetadata(
                is_stale=cached_query.is_stale,
                staleness_age_seconds=staleness_age if cached_query.is_stale else 0.0,
                rebuild_in_progress=is_rebuilding,
                last_updated=cached_query.computed_at,
                source='tier2_fresh' if not cached_query.is_stale else 'tier2_stale',
                cache_key=cache_key,
            )

        # Cache miss: compute synchronously
        self.monitor.record_cache_miss()
        return self._compute_and_store(cache_key, query_func, involved_projects)

    def _compute_and_store(
        self,
        cache_key: str,
        query_func: Callable[[], Any],
        involved_projects: Set[str]
    ) -> Tuple[Any, QueryMetadata]:
        """
        Compute query result and store in cache.

        Args:
            cache_key: Unique cache key
            query_func: Function to compute result
            involved_projects: Set of project IDs involved

        Returns:
            Tuple of (result_data, query_metadata)
        """
        with self.stats_lock:
            self.stats['cache_misses'] += 1

        # Compute result
        start_time = time.time()
        data = query_func()
        compute_time = time.time() - start_time

        # Estimate size (rough estimate based on data structure)
        size_mb = self._estimate_size(data)

        # Store in cache
        cached_query = CachedQuery(
            data=data,
            computed_at=time.time(),
            involved_projects=involved_projects,
            is_stale=False,
            size_mb=size_mb,
        )

        with self.cache_lock:
            self.cache[cache_key] = cached_query

        # Store in LRU tracker
        added = self.lru_tracker.set(cache_key, cached_query, size_mb)

        if not added:
            # Entry too large for cache, remove from cache dict
            with self.cache_lock:
                self.cache.pop(cache_key, None)
            logger.warning(
                f"Cache entry {cache_key[:16]}... ({size_mb:.2f}MB) "
                f"exceeds budget, not cached"
            )

        logger.debug(
            f"Cache miss {cache_key[:16]}... computed in {compute_time*1000:.1f}ms, "
            f"size={size_mb:.2f}MB"
        )

        return data, QueryMetadata(
            is_stale=False,
            staleness_age_seconds=0.0,
            rebuild_in_progress=False,
            last_updated=cached_query.computed_at,
            source='federation',
            cache_key=cache_key,
        )

    def _rebuild_async(
        self,
        cache_key: str,
        query_func: Callable[[], Any],
        involved_projects: Set[str]
    ) -> None:
        """
        Rebuild cache entry in background without blocking.

        ✅ Race condition fix: Check rebuilding_keys before triggering.

        Args:
            cache_key: Unique cache key to rebuild
            query_func: Function to compute fresh result
            involved_projects: Set of project IDs involved
        """
        # ✅ Duplicate rebuild prevention
        with self.rebuild_lock:
            if cache_key in self.rebuilding_keys:
                # Already rebuilding, don't trigger another
                return
            self.rebuilding_keys.add(cache_key)

        with self.stats_lock:
            self.stats['rebuilds_triggered'] += 1

        def rebuild() -> None:
            try:
                # Compute fresh data
                start_time = time.time()
                fresh_data = query_func()
                compute_time = time.time() - start_time

                # Check if entry still exists (not evicted)
                with self.cache_lock:
                    if cache_key not in self.cache:
                        # Entry was evicted, don't store
                        logger.debug(
                            f"Rebuild of {cache_key[:16]}... completed but entry evicted, "
                            f"not storing"
                        )
                        return

                    # Update existing entry
                    old_entry = self.cache[cache_key]
                    old_entry.data = fresh_data
                    old_entry.computed_at = time.time()
                    old_entry.is_stale = False
                    old_entry.involved_projects = involved_projects
                    old_entry.size_mb = self._estimate_size(fresh_data)

                # Update LRU tracker
                self.lru_tracker.set(cache_key, old_entry, old_entry.size_mb)

                with self.stats_lock:
                    self.stats['rebuilds_completed'] += 1

                logger.debug(
                    f"Rebuild of {cache_key[:16]}... completed in {compute_time*1000:.1f}ms"
                )

            except Exception as e:
                with self.stats_lock:
                    self.stats['rebuilds_failed'] += 1

                logger.error(f"Async rebuild failed for {cache_key[:16]}...: {e}")

            finally:
                # ✅ Remove from rebuilding set
                with self.rebuild_lock:
                    self.rebuilding_keys.discard(cache_key)

        # Submit to background executor
        self.rebuild_executor.submit(rebuild)

    def mark_project_stale(self, project_id: str) -> None:
        """
        Mark all queries involving a project as stale.

        Does not delete cache entries - just marks them stale for async rebuild.

        Args:
            project_id: Project ID that was updated
        """
        stale_count = 0

        with self.cache_lock:
            for cache_key, cached_query in self.cache.items():
                if project_id in cached_query.involved_projects:
                    cached_query.is_stale = True
                    stale_count += 1

        if stale_count > 0:
            logger.info(
                f"Marked {stale_count} cache entries as stale due to "
                f"project update: {project_id}"
            )

    def _estimate_size(self, data: Any) -> float:
        """
        Estimate memory size of cached data.

        Args:
            data: Data to estimate size for

        Returns:
            Estimated size in megabytes
        """
        import sys

        try:
            # Rough estimate using sizeof
            size_bytes = sys.getsizeof(data)

            # For collections, add overhead
            if isinstance(data, (list, tuple, set)):
                size_bytes += sum(sys.getsizeof(item) for item in data)
            elif isinstance(data, dict):
                size_bytes += sum(
                    sys.getsizeof(k) + sys.getsizeof(v)
                    for k, v in data.items()
                )

            return size_bytes / (1024 * 1024)  # Convert to MB

        except Exception:
            # Fallback: assume 1MB
            return 1.0

    def invalidate(self, cache_key: str) -> None:
        """
        Remove a specific cache entry.

        Args:
            cache_key: Cache key to invalidate
        """
        with self.cache_lock:
            self.cache.pop(cache_key, None)

        self.lru_tracker.delete(cache_key)

    def clear(self) -> None:
        """Clear all cache entries."""
        with self.cache_lock:
            self.cache.clear()

        self.lru_tracker.clear()

    def shutdown(self) -> None:
        """Shutdown rebuild executor and clean up."""
        self.rebuild_executor.shutdown(wait=True)

    def get_stats(self) -> Dict[str, Any]:
        """
        Get cache statistics.

        Returns:
            Dictionary with cache stats
        """
        lru_stats = self.lru_tracker.get_stats()

        with self.stats_lock:
            cache_stats = self.stats.copy()

        return {
            **lru_stats,
            'queries': cache_stats['queries'],
            'cache_hits': cache_stats['cache_hits'],
            'cache_misses': cache_stats['cache_misses'],
            'stale_serves': cache_stats['stale_serves'],
            'rebuilds_triggered': cache_stats['rebuilds_triggered'],
            'rebuilds_completed': cache_stats['rebuilds_completed'],
            'rebuilds_failed': cache_stats['rebuilds_failed'],
            'rebuilding_now': len(self.rebuilding_keys),
        }
