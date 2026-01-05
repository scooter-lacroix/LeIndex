"""
LRU Cache Implementation with Background Cleanup

This module provides an LRU (Least Recently Used) cache implementation
with support for background cleanup tasks and persistence file compaction.
"""

import os
import json
import time
from collections import OrderedDict
from typing import Dict, Any, Optional, List, Tuple
from threading import Lock, Thread, Event
from concurrent.futures import ThreadPoolExecutor
import atexit


class LRUCache:
    """Thread-safe LRU cache with TTL support and background cleanup."""
    
    def __init__(self, capacity: int = 1000, ttl_seconds: int = 3600, 
                 cleanup_interval: int = 300, enable_cleanup: bool = True):
        """Initialize LRU cache.
        
        Args:
            capacity: Maximum number of items in cache
            ttl_seconds: Time-to-live for cache entries in seconds
            cleanup_interval: Interval between cleanup runs in seconds
            enable_cleanup: Whether to enable background cleanup
        """
        self.capacity = capacity
        self.ttl_seconds = ttl_seconds
        self.cleanup_interval = cleanup_interval
        self.enable_cleanup = enable_cleanup
        
        # Cache storage
        self._cache = OrderedDict()
        self._lock = Lock()
        
        # Background cleanup
        self._cleanup_thread = None
        self._cleanup_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="lru-cleanup")
        self._shutdown_event = Event()
        
        # Statistics
        self._stats = {
            'hits': 0,
            'misses': 0,
            'evictions': 0,
            'cleanups': 0,
            'expired_entries': 0
        }
        
        # Start background cleanup if enabled
        if self.enable_cleanup:
            self._start_background_cleanup()
            atexit.register(self._shutdown_cleanup)
    
    def get(self, key: str) -> Optional[Any]:
        """Get item from cache."""
        with self._lock:
            if key not in self._cache:
                self._stats['misses'] += 1
                return None
            
            entry = self._cache[key]
            
            # Check if entry has expired
            if self._is_expired(entry):
                del self._cache[key]
                self._stats['misses'] += 1
                self._stats['expired_entries'] += 1
                return None
            
            # Move to end (most recently used)
            self._cache.move_to_end(key)
            self._stats['hits'] += 1
            return entry['value']
    
    def put(self, key: str, value: Any, ttl: Optional[int] = None) -> bool:
        """Put item in cache."""
        with self._lock:
            current_time = time.time()
            expires_at = current_time + (ttl if ttl is not None else self.ttl_seconds)
            
            entry = {
                'value': value,
                'created_at': current_time,
                'expires_at': expires_at,
                'access_count': 1
            }
            
            # Update existing entry
            if key in self._cache:
                self._cache[key] = entry
                self._cache.move_to_end(key)
                return True
            
            # Add new entry
            self._cache[key] = entry
            
            # Check capacity and evict if necessary
            if len(self._cache) > self.capacity:
                self._evict_lru()
            
            return True
    
    def delete(self, key: str) -> bool:
        """Delete item from cache."""
        with self._lock:
            if key in self._cache:
                del self._cache[key]
                return True
            return False
    
    def clear(self) -> int:
        """Clear all cache entries."""
        with self._lock:
            count = len(self._cache)
            self._cache.clear()
            return count
    
    def size(self) -> int:
        """Get current cache size."""
        with self._lock:
            return len(self._cache)
    
    def get_stats(self) -> Dict[str, Any]:
        """Get cache statistics."""
        with self._lock:
            total_requests = self._stats['hits'] + self._stats['misses']
            hit_rate = self._stats['hits'] / total_requests if total_requests > 0 else 0
            
            return {
                'size': len(self._cache),
                'capacity': self.capacity,
                'hit_rate': hit_rate,
                'total_requests': total_requests,
                **self._stats
            }
    
    def keys(self) -> List[str]:
        """Get all cache keys."""
        with self._lock:
            return list(self._cache.keys())
    
    def items(self) -> List[Tuple[str, Any]]:
        """Get all cache items."""
        with self._lock:
            return [(key, entry['value']) for key, entry in self._cache.items()]
    
    def _is_expired(self, entry: Dict[str, Any]) -> bool:
        """Check if cache entry has expired."""
        return time.time() > entry['expires_at']
    
    def _evict_lru(self):
        """Evict least recently used item."""
        if self._cache:
            self._cache.popitem(last=False)
            self._stats['evictions'] += 1
    
    def _start_background_cleanup(self):
        """Start background cleanup thread."""
        if self._cleanup_thread is None or not self._cleanup_thread.is_alive():
            self._cleanup_thread = Thread(target=self._background_cleanup_task, daemon=True)
            self._cleanup_thread.start()
    
    def _background_cleanup_task(self):
        """Background task that periodically cleans up expired entries."""
        while not self._shutdown_event.is_set():
            try:
                # Wait for cleanup interval or shutdown event
                if self._shutdown_event.wait(timeout=self.cleanup_interval):
                    break
                
                # Perform cleanup
                self._perform_cleanup()
                
            except Exception as e:
                print(f"Error in LRU cache cleanup task: {e}")
    
    def _perform_cleanup(self):
        """Perform cleanup operations."""
        try:
            expired_count = 0
            current_time = time.time()
            
            with self._lock:
                # Remove expired entries
                expired_keys = []
                for key, entry in self._cache.items():
                    if current_time > entry['expires_at']:
                        expired_keys.append(key)
                
                for key in expired_keys:
                    del self._cache[key]
                    expired_count += 1
                
                self._stats['expired_entries'] += expired_count
                self._stats['cleanups'] += 1
            
            if expired_count > 0:
                print(f"LRU cache cleanup: removed {expired_count} expired entries")
                
        except Exception as e:
            print(f"Error during LRU cache cleanup: {e}")
    
    def _shutdown_cleanup(self):
        """Shutdown cleanup threads and resources."""
        try:
            self._shutdown_event.set()
            
            if self._cleanup_thread and self._cleanup_thread.is_alive():
                self._cleanup_thread.join(timeout=5)
            
            if self._cleanup_executor:
                self._cleanup_executor.shutdown(wait=True)
                
        except Exception as e:
            print(f"Error during LRU cache cleanup shutdown: {e}")
    
    def trigger_manual_cleanup(self):
        """Manually trigger cleanup operations."""
        future = self._cleanup_executor.submit(self._perform_cleanup)
        return future


class PersistentLRUCache(LRUCache):
    """LRU cache with disk persistence and compaction support."""
    
    def __init__(self, capacity: int = 1000, ttl_seconds: int = 3600,
                 cleanup_interval: int = 300, enable_cleanup: bool = True,
                 persistence_file: Optional[str] = None,
                 compact_threshold: int = 1000):
        """Initialize persistent LRU cache.
        
        Args:
            capacity: Maximum number of items in cache
            ttl_seconds: Time-to-live for cache entries in seconds
            cleanup_interval: Interval between cleanup runs in seconds
            enable_cleanup: Whether to enable background cleanup
            persistence_file: Path to persistence file
            compact_threshold: Threshold for triggering compaction
        """
        super().__init__(capacity, ttl_seconds, cleanup_interval, enable_cleanup)
        
        self.persistence_file = persistence_file
        self.compact_threshold = compact_threshold
        self._write_count = 0
        
        # Load from persistence file if exists
        if self.persistence_file and os.path.exists(self.persistence_file):
            self._load_from_disk()
    
    def put(self, key: str, value: Any, ttl: Optional[int] = None) -> bool:
        """Put item in cache and persist to disk."""
        result = super().put(key, value, ttl)
        
        if result and self.persistence_file:
            self._write_count += 1
            self._save_to_disk()
            
            # Check if compaction is needed
            if self._write_count >= self.compact_threshold:
                self._compact_persistence_file()
                self._write_count = 0
        
        return result
    
    def delete(self, key: str) -> bool:
        """Delete item from cache and persist to disk."""
        result = super().delete(key)
        
        if result and self.persistence_file:
            self._write_count += 1
            self._save_to_disk()
        
        return result
    
    def clear(self) -> int:
        """Clear all cache entries and persistence file."""
        count = super().clear()
        
        if self.persistence_file and os.path.exists(self.persistence_file):
            try:
                os.remove(self.persistence_file)
                self._write_count = 0
            except Exception as e:
                print(f"Error clearing persistence file: {e}")
        
        return count
    
    def _save_to_disk(self):
        """Save cache to disk."""
        if not self.persistence_file:
            return
        
        try:
            with self._lock:
                cache_data = {
                    'timestamp': time.time(),
                    'entries': dict(self._cache)
                }
            
            # Write to temporary file first, then rename for atomicity
            temp_file = self.persistence_file + '.tmp'
            with open(temp_file, 'w') as f:
                json.dump(cache_data, f, indent=2)
            
            os.rename(temp_file, self.persistence_file)
            
        except Exception as e:
            print(f"Error saving cache to disk: {e}")
    
    def _load_from_disk(self):
        """Load cache from disk."""
        if not self.persistence_file or not os.path.exists(self.persistence_file):
            return
        
        try:
            with open(self.persistence_file, 'r') as f:
                cache_data = json.load(f)
            
            if 'entries' in cache_data:
                current_time = time.time()
                loaded_count = 0
                
                with self._lock:
                    for key, entry in cache_data['entries'].items():
                        # Skip expired entries
                        if current_time <= entry['expires_at']:
                            self._cache[key] = entry
                            loaded_count += 1
                
                print(f"Loaded {loaded_count} cache entries from disk")
                
        except Exception as e:
            print(f"Error loading cache from disk: {e}")
    
    def _compact_persistence_file(self):
        """Compact persistence file by removing expired entries."""
        if not self.persistence_file:
            return
        
        try:
            # Create backup
            backup_file = self.persistence_file + '.backup'
            if os.path.exists(self.persistence_file):
                os.rename(self.persistence_file, backup_file)
            
            # Save current state (this removes expired entries)
            self._save_to_disk()
            
            # Remove backup if successful
            if os.path.exists(backup_file):
                os.remove(backup_file)
            
            print(f"Compacted persistence file: {self.persistence_file}")
            
        except Exception as e:
            print(f"Error compacting persistence file: {e}")
            
            # Restore backup if it exists
            backup_file = self.persistence_file + '.backup'
            if os.path.exists(backup_file):
                os.rename(backup_file, self.persistence_file)
    
    def _perform_cleanup(self):
        """Perform cleanup operations including persistence file compaction."""
        super()._perform_cleanup()
        
        # Compact persistence file periodically
        if self.persistence_file and self._write_count >= self.compact_threshold // 2:
            self._compact_persistence_file()
            self._write_count = 0
