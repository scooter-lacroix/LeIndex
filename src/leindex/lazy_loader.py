"""
Lazy Loading Module

This module implements lazy loading of file contents to defer reading
until required by analysis/search operations.
"""

import os
import hashlib
import mmap
import logging
from typing import Dict, Optional, Any, List
from threading import Lock, Thread, Event
import time
import atexit
from concurrent.futures import ThreadPoolExecutor
from weakref import WeakValueDictionary

logger = logging.getLogger(__name__)


class LazyFileContent:
    """Represents a file whose content is loaded on demand."""
    
    def __init__(self, file_path: str, metadata: Optional[Dict[str, Any]] = None, manager_ref=None):
        self.file_path = file_path
        self.metadata = metadata or {}
        self._content: Optional[str] = None
        self._content_loaded = False
        self._lock = Lock()
        self._manager_ref = manager_ref
    
    @property
    def content(self) -> Optional[str]:
        """Load and return file content on first access."""
        if not self._content_loaded:
            with self._lock:
                if not self._content_loaded:  # Double-check locking
                    self._load_content()
                    self._content_loaded = True
                    # Notify manager that content was loaded
                    if hasattr(self, '_manager_ref') and self._manager_ref:
                        self._manager_ref._on_content_loaded(self.file_path)
        return self._content
    
    def _load_content(self):
        """Load the actual file content with memory-efficient chunked reading for large files."""
        try:
            file_size = os.path.getsize(self.file_path)
            # For files larger than 10MB, use chunked reading
            if file_size > 10 * 1024 * 1024:  # 10MB threshold
                self._content = self._load_content_chunked()
            else:
                # For smaller files, use standard reading
                with open(self.file_path, 'r', encoding='utf-8', errors='ignore') as f:
                    self._content = f.read()
        except (UnicodeDecodeError, IOError) as e:
            print(f"Error loading content for {self.file_path}: {e}")
            self._content = None
    
    def _load_content_chunked(self) -> Optional[str]:
        """Load file content in chunks to handle large files efficiently."""
        try:
            chunks = []
            chunk_size = 4 * 1024 * 1024  # 4MB chunks
            
            with open(self.file_path, 'r', encoding='utf-8', errors='ignore') as f:
                while True:
                    chunk = f.read(chunk_size)
                    if not chunk:
                        break
                    chunks.append(chunk)
            
            return ''.join(chunks)
        except (UnicodeDecodeError, IOError) as e:
            print(f"Error loading chunked content for {self.file_path}: {e}")
            return None
    
    def get_line_count(self) -> int:
        """Get the number of lines in the file without loading full content."""
        try:
            with open(self.file_path, 'rb') as f:
                return sum(1 for _ in f)
        except IOError:
            return 0
    
    def get_size(self) -> int:
        """Get file size in bytes."""
        try:
            return os.path.getsize(self.file_path)
        except OSError:
            return 0
    
    def is_content_loaded(self) -> bool:
        """Check if content has been loaded."""
        return self._content_loaded
    
    def unload_content(self):
        """Unload content to free memory."""
        with self._lock:
            self._content = None
            self._content_loaded = False


class ChunkedFileReader:
    """Reads large files in chunks for memory efficiency."""
    
    def __init__(self, file_path: str, chunk_size: int = 4 * 1024 * 1024):  # 4MB default
        self.file_path = file_path
        self.chunk_size = chunk_size
    
    def read_chunks(self):
        """Generator that yields file content in chunks."""
        try:
            with open(self.file_path, 'rb') as f:
                while True:
                    chunk = f.read(self.chunk_size)
                    if not chunk:
                        break
                    yield chunk
        except IOError as e:
            print(f"Error reading chunks from {self.file_path}: {e}")
    
    def compute_hash(self) -> Optional[str]:
        """Compute SHA-256 hash by reading file in chunks."""
        try:
            hasher = hashlib.sha256()
            for chunk in self.read_chunks():
                hasher.update(chunk)
            return hasher.hexdigest()
        except Exception as e:
            print(f"Error computing hash for {self.file_path}: {e}")
            return None
    
    def search_in_chunks(self, pattern: str, case_sensitive: bool = True) -> List[Dict[str, Any]]:
        """Search for pattern in file by reading chunks."""
        matches = []
        line_number = 1
        buffer = b""
        
        if not case_sensitive:
            pattern = pattern.lower()
        
        try:
            for chunk in self.read_chunks():
                buffer += chunk
                lines = buffer.split(b'\n')
                
                # Keep the last incomplete line in buffer
                buffer = lines[-1]
                
                # Process complete lines
                for line_data in lines[:-1]:
                    try:
                        line = line_data.decode('utf-8', errors='ignore')
                        search_line = line if case_sensitive else line.lower()
                        
                        if pattern in search_line:
                            matches.append({
                                'line_number': line_number,
                                'line_content': line.strip(),
                                'file_path': self.file_path
                            })
                    except UnicodeDecodeError:
                        pass  # Skip lines that can't be decoded
                    
                    line_number += 1
            
            # Process the last line if there's remaining data
            if buffer:
                try:
                    line = buffer.decode('utf-8', errors='ignore')
                    search_line = line if case_sensitive else line.lower()
                    
                    if pattern in search_line:
                        matches.append({
                            'line_number': line_number,
                            'line_content': line.strip(),
                            'file_path': self.file_path
                        })
                except UnicodeDecodeError:
                    pass
        
        except Exception as e:
            print(f"Error searching in {self.file_path}: {e}")
        
        return matches

import json
from collections import OrderedDict
from .lru_cache import PersistentLRUCache

class LazyContentManager:
    """Manages lazy loading of file contents with memory management and LRU cache for search results."""

    class QueryCache:
        """LRU cache for storing search query results."""
        def __init__(self, capacity: int = 50):
            self.cache = OrderedDict()
            self.capacity = capacity

        def get(self, query: str) -> Optional[Dict[str, Any]]:
            if query in self.cache:
                self.cache.move_to_end(query)
                return self.cache[query]
            return None

        def put(self, query: str, result: Dict[str, Any]):
            self.cache[query] = result
            self.cache.move_to_end(query)
            if len(self.cache) > self.capacity:
                self.cache.popitem(last=False)

        def save_to_disk(self, file_path: str):
            """Save cached queries to disk."""
            try:
                with open(file_path, 'w') as f:
                    json.dump(list(self.cache.items()), f, indent=2)
            except Exception as e:
                print(f"Error saving query cache to {file_path}: {e}")
        
        def evict_stale_entries(self, max_age_seconds: int = 3600):
            """Evict stale entries older than max_age_seconds."""
            current_time = time.time()
            stale_keys = []
            
            for key, result in self.cache.items():
                if isinstance(result, dict) and 'timestamp' in result:
                    if current_time - result['timestamp'] > max_age_seconds:
                        stale_keys.append(key)
            
            for key in stale_keys:
                del self.cache[key]
            
            return len(stale_keys)
        
        def compact_cache(self):
            """Compact the cache by removing duplicate entries."""
            # Create a new OrderedDict to compact the cache
            compacted = OrderedDict()
            for key, value in self.cache.items():
                compacted[key] = value
            self.cache = compacted

        def load_from_disk(self, file_path: str):
            """Load cached queries from disk."""
            try:
                if os.path.exists(file_path):
                    with open(file_path, 'r') as f:
                        cached_items = json.load(f)
                        self.cache = OrderedDict(cached_items)
            except Exception as e:
                print(f"Error loading query cache from {file_path}: {e}")
                self.cache = OrderedDict()

    def __init__(self, max_loaded_files: int = 100, query_cache_capacity: int = 50, 
                 cleanup_interval: int = 300, max_age_seconds: int = 3600):
        self.max_loaded_files = max_loaded_files
        self._loaded_files: WeakValueDictionary = WeakValueDictionary()
        self._access_order: List[str] = []  # LRU tracking
        self._lock = Lock()
        self.query_cache = self.QueryCache(capacity=query_cache_capacity)
        self.file_content_cache = PersistentLRUCache(
            capacity=max_loaded_files, ttl_seconds=3600,
            cleanup_interval=300, enable_cleanup=True,
            persistence_file='file_content_cache.json')
        
        # Background cleanup configuration
        self.cleanup_interval = cleanup_interval  # seconds
        self.max_age_seconds = max_age_seconds
        self._cleanup_thread = None
        self._cleanup_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="cleanup")
        self._shutdown_event = Event()
        
        # Start background cleanup
        self._start_background_cleanup()
        
        # Register cleanup on exit
        atexit.register(self._shutdown_cleanup)
    
    def get_file_content(self, file_path: str, metadata: Optional[Dict[str, Any]] = None) -> LazyFileContent:
        """Get or create a LazyFileContent instance."""
        with self._lock:
            # Check if already loaded
            if file_path in self._loaded_files:
                # Update access order
                if file_path in self._access_order:
                    self._access_order.remove(file_path)
                self._access_order.append(file_path)
                return self._loaded_files[file_path]
            
            # Create new lazy file content
            lazy_content = LazyFileContent(file_path, metadata, manager_ref=self)
            self._loaded_files[file_path] = lazy_content
            self._access_order.append(file_path)
            
            # Check if we need to evict old entries
            self._enforce_memory_limits()
            
            # Cache file content with LRU
            self.file_content_cache.put(file_path, lazy_content)
            
            return lazy_content
    
    def _enforce_memory_limits(self):
        """Enforce memory limits by unloading least recently used files."""
        # Only enforce limits if we have loaded content, not just managed files
        loaded_files = [lc for lc in self._loaded_files.values() if lc.is_content_loaded()]
        
        while len(loaded_files) > self.max_loaded_files:
            # Find the oldest loaded file and unload it
            oldest_path = None
            for path in self._access_order:
                if path in self._loaded_files and self._loaded_files[path].is_content_loaded():
                    oldest_path = path
                    break
            
            if oldest_path:
                self._loaded_files[oldest_path].unload_content()
                self._access_order.remove(oldest_path)
                loaded_files = [lc for lc in self._loaded_files.values() if lc.is_content_loaded()]
            else:
                break  # No more loaded files to unload
    
    def _on_content_loaded(self, file_path: str):
        """Called when content is loaded for a file."""
        with self._lock:
            self._enforce_memory_limits()
    
    def unload_all(self):
        """Unload all file contents to free memory."""
        with self._lock:
            for lazy_content in self._loaded_files.values():
                lazy_content.unload_content()
            self._access_order.clear()
    
    def get_memory_stats(self) -> Dict[str, Any]:
        """Get statistics about memory usage."""
        loaded_count = sum(1 for lc in self._loaded_files.values() if lc.is_content_loaded())
        total_managed = len(self._loaded_files)
        
        return {
            'total_managed_files': total_managed,
            'loaded_files': loaded_count,
            'max_loaded_files': self.max_loaded_files,
            'memory_pressure': loaded_count / self.max_loaded_files if self.max_loaded_files > 0 else 0,
            'query_cache_size': len(self.query_cache.cache),
            'query_cache_capacity': self.query_cache.capacity
        }
    
    def get_cached_search_result(self, query_key: str) -> Optional[Dict[str, Any]]:
        """Get cached search result for a query."""
        return self.query_cache.get(query_key)
    
    def cache_search_result(self, query_key: str, result: Dict[str, Any]):
        """Cache a search result."""
        # Add timestamp to result for staleness tracking
        result_with_timestamp = result.copy()
        result_with_timestamp['timestamp'] = time.time()
        self.query_cache.put(query_key, result_with_timestamp)
    
    def save_query_cache(self, cache_file_path: str):
        """Save query cache to disk."""
        self.query_cache.save_to_disk(cache_file_path)
    
    def load_query_cache(self, cache_file_path: str):
        """Load query cache from disk."""
        self.query_cache.load_from_disk(cache_file_path)
    
    def _start_background_cleanup(self):
        """Start background cleanup thread."""
        if self._cleanup_thread is None or not self._cleanup_thread.is_alive():
            self._cleanup_thread = Thread(target=self._background_cleanup_task, daemon=True)
            self._cleanup_thread.start()
    
    def _background_cleanup_task(self):
        """Background task that periodically cleans up caches."""
        while not self._shutdown_event.is_set():
            try:
                # Wait for cleanup interval or shutdown event
                if self._shutdown_event.wait(timeout=self.cleanup_interval):
                    break
                
                # Perform cleanup
                self._perform_cleanup()
                
            except Exception as e:
                print(f"Error in background cleanup task: {e}")
    
    def _perform_cleanup(self):
        """Perform cleanup operations."""
        try:
            # Evict stale search results
            evicted_count = self.query_cache.evict_stale_entries(self.max_age_seconds)
            if evicted_count > 0:
                print(f"Evicted {evicted_count} stale search results")
            
            # Compact query cache
            self.query_cache.compact_cache()
            
            # Clean up file content cache
            self._cleanup_file_content_cache()
            
            # Force memory cleanup if needed
            with self._lock:
                self._enforce_memory_limits()
                
        except Exception as e:
            print(f"Error during cleanup: {e}")
    
    def _cleanup_file_content_cache(self):
        """Clean up file content cache by removing least recently used entries."""
        # The LRU cache handles this automatically through its capacity limit
        pass
    
    def _shutdown_cleanup(self):
        """Shutdown cleanup threads and resources."""
        try:
            self._shutdown_event.set()
            
            if self._cleanup_thread and self._cleanup_thread.is_alive():
                self._cleanup_thread.join(timeout=5)
            
            if self._cleanup_executor:
                self._cleanup_executor.shutdown(wait=True)
                
        except Exception as e:
            print(f"Error during cleanup shutdown: {e}")
    
    def trigger_manual_cleanup(self):
        """Manually trigger cleanup operations."""
        future = self._cleanup_executor.submit(self._perform_cleanup)
        return future
    
    @staticmethod
    def paginate_results(results: Dict[str, List], page: int = 1, page_size: int = 20) -> Dict[str, Any]:
        """
        Paginate search results with token-efficient compact format.

        TOKEN EFFICIENCY: Uses compact response format to reduce tokens:
        - Flattened results array instead of nested dict structure
        - Compact pagination object (4 fields instead of 6)
        - File paths stored once per file (not repeated per match)
        """
        if page < 1:
            page = 1
        if page_size < 1:
            page_size = 20

        # Convert results to a flat list of matches
        all_matches = []
        for file_path, matches in results.items():
            for match in matches:
                # Debug: Log the match structure
                logger.debug(f"Processing match: type={type(match)}, value={match}")

                # Handle both tuple format (line_number, line_content) and dict format
                if isinstance(match, tuple) and len(match) == 2:
                    line_number, line_content = match
                elif isinstance(match, dict):
                    line_number = match.get('line', 0)
                    line_content = match.get('text', '')
                    # Extract score if available
                    score = match.get('score', None)
                else:
                    # Fallback for unexpected formats
                    logger.warning(f"Unexpected match format: {type(match)} - {match}")
                    line_number = 0
                    line_content = str(match)
                    score = None

                match_dict = {
                    'file': file_path,  # Compact key name
                    'line': line_number,
                    'text': line_content
                }
                if score is not None:
                    match_dict['score'] = score
                all_matches.append(match_dict)

        # Calculate pagination
        total_matches = len(all_matches)
        total_pages = (total_matches + page_size - 1) // page_size if total_matches > 0 else 1
        start_idx = (page - 1) * page_size
        end_idx = min(start_idx + page_size, total_matches)

        # Get current page results
        current_page_matches = all_matches[start_idx:end_idx]

        # TOKEN EFFICIENCY: Return flattened array instead of nested dict
        return {
            'results': current_page_matches,  # Flattened format
            'page': page,
            'page_size': page_size,
            'total': total_matches,
            'has_more': page < total_pages  # Compact boolean name
        }


class MemoryMappedFileReader:
    """Memory-mapped file reader for very large files."""
    
    def __init__(self, file_path: str):
        self.file_path = file_path
        self._file = None
        self._mmap = None
    
    def __enter__(self):
        try:
            self._file = open(self.file_path, 'rb')
            self._mmap = mmap.mmap(self._file.fileno(), 0, access=mmap.ACCESS_READ)
            return self
        except (IOError, OSError) as e:
            print(f"Error memory-mapping file {self.file_path}: {e}")
            self.close()
            raise
    
    def __exit__(self, exc_type, exc_val, exc_tb):
        self.close()
    
    def close(self):
        """Close memory-mapped file."""
        if self._mmap:
            self._mmap.close()
            self._mmap = None
        if self._file:
            self._file.close()
            self._file = None
    
    def search_pattern(self, pattern: bytes, max_results: int = 1000) -> List[int]:
        """Search for pattern in memory-mapped file and return byte positions."""
        if not self._mmap:
            return []
        
        positions = []
        start = 0
        
        while len(positions) < max_results:
            pos = self._mmap.find(pattern, start)
            if pos == -1:
                break
            positions.append(pos)
            start = pos + 1
        
        return positions
    
    def get_line_at_position(self, byte_pos: int) -> tuple[int, str]:
        """Get line number and content at given byte position."""
        if not self._mmap:
            return 0, ""
        
        # Find start of line
        line_start = byte_pos
        while line_start > 0 and self._mmap[line_start - 1:line_start] != b'\n':
            line_start -= 1
        
        # Find end of line
        line_end = byte_pos
        while line_end < len(self._mmap) and self._mmap[line_end:line_end + 1] != b'\n':
            line_end += 1
        
        # Calculate line number
        line_number = self._mmap[:line_start].count(b'\n') + 1
        
        # Extract line content
        try:
            line_content = self._mmap[line_start:line_end].decode('utf-8', errors='ignore')
            return line_number, line_content
        except UnicodeDecodeError:
            return line_number, ""
