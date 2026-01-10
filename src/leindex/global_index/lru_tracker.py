"""
LRU Tracker for Global Index Tier 2 Cache.

Implements size-based LRU eviction to respect memory budget.
"""

import logging
from collections import OrderedDict
from dataclasses import dataclass, field
from threading import Lock
from typing import Any, Dict, Optional

logger = logging.getLogger(__name__)


@dataclass
class CacheEntry:
    """A single cache entry with size tracking."""
    key: str
    value: Any
    size_mb: float
    last_access: float = field(default_factory=lambda: __import__('time').time())

    def touch(self):
        """Update last access time."""
        self.last_access = __import__('time').time()


class LRUTracker:
    """
    Track cache entries with LRU eviction based on memory budget.

    Uses OrderedDict for O(1) access and eviction.
    """

    def __init__(self, max_size_mb: float = 500.0):
        """
        Initialize LRU tracker.

        Args:
            max_size_mb: Maximum cache size in megabytes
        """
        self.max_size_mb = max_size_mb
        self.cache: OrderedDict[str, CacheEntry] = OrderedDict()
        self.current_size_mb = 0.0
        self.lock = Lock()

        # Statistics
        self.hits = 0
        self.misses = 0
        self.evictions = 0

    def get(self, key: str) -> Optional[Any]:
        """
        Get value from cache, updating LRU order.

        Args:
            key: Cache key

        Returns:
            Cached value or None if not found
        """
        with self.lock:
            if key in self.cache:
                entry = self.cache[key]
                # Move to end (most recently used)
                self.cache.move_to_end(key)
                entry.touch()
                self.hits += 1
                return entry.value

            self.misses += 1
            return None

    def set(self, key: str, value: Any, size_mb: float) -> bool:
        """
        Add or update cache entry, evicting if necessary.

        Args:
            key: Cache key
            value: Value to cache
            size_mb: Size of value in megabytes

        Returns:
            True if entry was added/updated, False if evicted due to size
        """
        with self.lock:
            # If key exists, update it
            if key in self.cache:
                old_entry = self.cache[key]
                self.current_size_mb -= old_entry.size_mb

            # Check if single entry exceeds budget
            if size_mb > self.max_size_mb:
                logger.warning(
                    f"Cache entry {key[:16]}... ({size_mb:.2f}MB) "
                    f"exceeds total budget ({self.max_size_mb:.2f}MB)"
                )
                return False

            # Evict entries until we have space
            while (self.current_size_mb + size_mb) > self.max_size_mb and self.cache:
                self._evict_lru()

            # Add new entry
            entry = CacheEntry(key=key, value=value, size_mb=size_mb)
            self.cache[key] = entry
            self.cache.move_to_end(key)
            self.current_size_mb += size_mb

            return True

    def delete(self, key: str) -> bool:
        """
        Remove entry from cache.

        Args:
            key: Cache key

        Returns:
            True if entry was removed, False if not found
        """
        with self.lock:
            if key in self.cache:
                entry = self.cache.pop(key)
                self.current_size_mb -= entry.size_mb
                return True
            return False

    def _evict_lru(self):
        """Evict least recently used entry."""
        if not self.cache:
            return

        key, entry = self.cache.popitem(last=False)  # Remove first (LRU)
        self.current_size_mb -= entry.size_mb
        self.evictions += 1

        # Emit eviction metric
        try:
            from .monitoring import get_global_index_monitor
            monitor = get_global_index_monitor()
            monitor.record_cache_eviction()
        except Exception:
            pass  # Ignore monitoring errors

        logger.debug(
            f"Evicted cache entry {key[:16]}... "
            f"({entry.size_mb:.2f}MB, age={__import__('time').time() - entry.last_access:.1f}s)"
        )

    def clear(self):
        """Clear all cache entries."""
        with self.lock:
            self.cache.clear()
            self.current_size_mb = 0.0
            self.evictions = 0

    def get_stats(self) -> Dict[str, Any]:
        """
        Get cache statistics.

        Returns:
            Dictionary with cache stats
        """
        with self.lock:
            total_requests = self.hits + self.misses
            hit_rate = self.hits / total_requests if total_requests > 0 else 0.0

            return {
                'entries': len(self.cache),
                'size_mb': self.current_size_mb,
                'max_size_mb': self.max_size_mb,
                'usage_percent': (self.current_size_mb / self.max_size_mb * 100)
                if self.max_size_mb > 0 else 0,
                'hits': self.hits,
                'misses': self.misses,
                'evictions': self.evictions,
                'hit_rate': hit_rate,
            }

    def get_entries(self) -> Dict[str, Dict[str, Any]]:
        """
        Get all cache entries with metadata.

        Returns:
            Dictionary mapping keys to entry metadata
        """
        with self.lock:
            return {
                key: {
                    'size_mb': entry.size_mb,
                    'last_access': entry.last_access,
                    'age_seconds': __import__('time').time() - entry.last_access,
                }
                for key, entry in self.cache.items()
            }
