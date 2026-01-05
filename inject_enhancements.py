#!/usr/bin/env python3
"""
Script to inject enhancement code into leann_backend.py
"""

import sys
import os

# Helper classes to insert
HELPER_CLASSES = '''
# ============================================================================
# HELPER CLASSES FOR ENHANCEMENTS
# ============================================================================

class TTLCache:
    """
    Thread-safe LRU cache with TTL (Time-To-Live) support.

    Features:
    - LRU eviction when cache is full
    - TTL-based expiration of entries
    - Cache statistics (hits, misses, eviction rate)
    - Thread-safe operations

    Used for caching search results to improve performance.
    """

    def __init__(self, maxsize: int = 1000, ttl: int = 300):
        """
        Initialize the TTL cache.

        Args:
            maxsize: Maximum number of items in cache
            ttl: Time-to-live in seconds for each cache entry
        """
        self.maxsize = maxsize
        self.ttl = ttl
        self._cache: OrderedDict[str, Tuple[Any, float]] = OrderedDict()
        self._lock = Lock()

        # Statistics
        self._hits = 0
        self._misses = 0
        self._evictions = 0
        self._expirations = 0

    def get(self, key: str) -> Optional[Any]:
        """
        Get a value from the cache.

        Args:
            key: Cache key

        Returns:
            Cached value or None if not found/expired
        """
        with self._lock:
            if key not in self._cache:
                self._misses += 1
                return None

            value, timestamp = self._cache[key]
            current_time = time.time()

            # Check if expired
            if current_time - timestamp > self.ttl:
                # Expired - remove and return None
                del self._cache[key]
                self._misses += 1
                self._expirations += 1
                return None

            # Not expired - move to end (most recently used)
            self._cache.move_to_end(key)
            self._hits += 1
            return value

    def put(self, key: str, value: Any) -> None:
        """
        Put a value in the cache.

        Args:
            key: Cache key
            value: Value to cache
        """
        with self._lock:
            # Update existing key or add new one
            if key in self._cache:
                # Update existing - move to end
                self._cache.move_to_end(key)
            else:
                # Check if we need to evict
                if len(self._cache) >= self.maxsize:
                    # Evict least recently used (first item)
                    self._cache.popitem(last=False)
                    self._evictions += 1

            # Add/update the entry with current timestamp
            self._cache[key] = (value, time.time())

    def invalidate(self, key: Optional[str] = None) -> None:
        """
        Invalidate cache entries.

        Args:
            key: Specific key to invalidate, or None to clear all
        """
        with self._lock:
            if key is None:
                # Clear all cache
                self._cache.clear()
            else:
                # Remove specific key
                if key in self._cache:
                    del self._cache[key]

    def get_stats(self) -> Dict[str, Any]:
        """
        Get cache statistics.

        Returns:
            Dictionary with cache statistics
        """
        with self._lock:
            total_requests = self._hits + self._misses
            hit_rate = self._hits / total_requests if total_requests > 0 else 0

            return {
                "size": len(self._cache),
                "maxsize": self.maxsize,
                "ttl": self.ttl,
                "hits": self._hits,
                "misses": self._misses,
                "evictions": self._evictions,
                "expirations": self._expirations,
                "hit_rate": hit_rate,
            }

    def reset_stats(self) -> None:
        """Reset cache statistics."""
        with self._lock:
            self._hits = 0
            self._misses = 0
            self._evictions = 0
            self._expirations = 0


class VectorDeduplicator:
    """
    Vector deduplication registry with content-based hashing.

    Features:
    - Content-based hash generation for chunks
    - Hash -> vector_id mapping
    - Reference counting for duplicate chunks
    - Automatic cleanup of unused entries

    Used to prevent storing duplicate vectors for identical content chunks.
    """

    def __init__(self):
        """Initialize the vector deduplicator."""
        self._hash_to_vector_id: Dict[str, str] = {}
        self._reference_counts: Dict[str, int] = {}
        self._lock = Lock()

    @staticmethod
    def hash_content(content: str) -> str:
        """
        Generate a content-based hash for a chunk.

        Args:
            content: Chunk content

        Returns:
            SHA-256 hash of the content
        """
        return hashlib.sha256(content.encode('utf-8')).hexdigest()

    def get_vector_id(self, content_hash: str) -> Optional[str]:
        """
        Get vector ID for a content hash.

        Args:
            content_hash: Hash of the chunk content

        Returns:
            Vector ID if exists, None otherwise
        """
        with self._lock:
            return self._hash_to_vector_id.get(content_hash)

    def register_vector(self, content_hash: str, vector_id: str) -> str:
        """
        Register a new vector in the deduplication registry.

        Args:
            content_hash: Hash of the chunk content
            vector_id: Vector ID to register

        Returns:
            The registered vector ID
        """
        with self._lock:
            self._hash_to_vector_id[content_hash] = vector_id
            self._reference_counts[vector_id] = 1
            return vector_id

    def add_reference(self, content_hash: str) -> int:
        """
        Increment reference count for a vector.

        Args:
            content_hash: Hash of the chunk content

        Returns:
            New reference count
        """
        with self._lock:
            vector_id = self._hash_to_vector_id.get(content_hash)
            if vector_id:
                self._reference_counts[vector_id] = self._reference_counts.get(vector_id, 0) + 1
                return self._reference_counts[vector_id]
            return 0

    def remove_reference(self, vector_id: str) -> int:
        """
        Decrement reference count for a vector.

        Args:
            vector_id: Vector ID to decrement

        Returns:
            New reference count (0 if vector can be deleted)
        """
        with self._lock:
            current_count = self._reference_counts.get(vector_id, 0)
            if current_count > 0:
                new_count = current_count - 1
                self._reference_counts[vector_id] = new_count
                return new_count
            return 0

    def cleanup_entry(self, content_hash: str, vector_id: str) -> None:
        """
        Remove an entry from the registry when reference count reaches 0.

        Args:
            content_hash: Hash of the chunk content
            vector_id: Vector ID to remove
        """
        with self._lock:
            if self._hash_to_vector_id.get(content_hash) == vector_id:
                del self._hash_to_vector_id[content_hash]
            if vector_id in self._reference_counts:
                del self._reference_counts[vector_id]

    def get_stats(self) -> Dict[str, Any]:
        """
        Get deduplication statistics.

        Returns:
            Dictionary with deduplication stats
        """
        with self._lock:
            total_refs = sum(self._reference_counts.values())
            return {
                "unique_vectors": len(self._hash_to_vector_id),
                "total_references": total_refs,
                "reference_counts": dict(self._reference_counts),
            }


def generate_cache_key(query: str, store_ids: List[str], top_k: int) -> str:
    """
    Generate a cache key for search results.

    Args:
        query: Search query
        store_ids: List of store IDs
        top_k: Number of results

    Returns:
        Cache key string (MD5 hash)
    """
    # Normalize query
    normalized_query = query.lower().strip()

    # Sort store IDs for consistency
    sorted_store_ids = sorted(store_ids)

    # Create key components
    key_parts = [
        normalized_query,
        ",".join(sorted_store_ids),
        str(top_k)
    ]

    # Hash the key for efficiency
    key_string = ":".join(key_parts)
    return hashlib.md5(key_string.encode()).hexdigest()


'''

def main():
    """Main function to inject enhancements."""
    # Read the original file
    input_file = 'src/leindex/core_engine/leann_backend.py'
    backup_file = 'src/leindex/core_engine/leann_backend.py.bak'

    with open(input_file, 'r') as f:
        lines = f.readlines()

    # Create backup
    with open(backup_file, 'w') as f:
        f.writelines(lines)

    print(f"Created backup: {backup_file}")

    # Find insertion point
    insert_line = None
    for i, line in enumerate(lines):
        if 'class LEANNVectorBackend:' in line:
            insert_line = i
            break

    if insert_line is None:
        print("ERROR: Could not find LEANNVectorBackend class", file=sys.stderr)
        sys.exit(1)

    # Insert helper classes
    lines.insert(insert_line, HELPER_CLASSES)

    # Write back
    with open(input_file, 'w') as f:
        f.writelines(lines)

    print(f"Successfully inserted helper classes into {input_file}")
    print(f"Inserted at line {insert_line + 1}")

if __name__ == '__main__':
    main()
