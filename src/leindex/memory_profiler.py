"""
Memory Profiler and Management System

This module provides comprehensive memory tracking, profiling, and enforcement
of memory limits with cleanup and spill-to-disk capabilities.
"""

import gc
import sys
import time
import psutil
import tempfile
import threading
import pickle
import json
from typing import Dict, List, Optional, Callable, Any, Tuple
from dataclasses import dataclass, asdict
from pathlib import Path
from collections import deque
from threading import Lock, Event
import logging

# Configure logging
logging.basicConfig(level=logging.INFO)
logger = logging.getLogger(__name__)


@dataclass
class MemorySnapshot:
    """Represents a memory usage snapshot at a point in time."""
    timestamp: float
    process_memory_mb: float
    heap_size_mb: float
    peak_memory_mb: float
    gc_objects: int
    gc_collections: Tuple[int, int, int]  # gen0, gen1, gen2 collections
    active_threads: int
    loaded_files: int
    cached_queries: int


@dataclass
class MemoryLimits:
    """Memory limits configuration."""
    soft_limit_mb: float = 512.0  # 512MB soft limit
    hard_limit_mb: float = 1024.0  # 1GB hard limit
    max_loaded_files: int = 100
    max_cached_queries: int = 50
    gc_threshold_mb: float = 256.0  # Trigger GC at 256MB
    spill_threshold_mb: float = 768.0  # Spill to disk at 768MB


class MemoryProfiler:
    """Memory profiler that tracks peak usage and enforces limits."""
    
    def __init__(self, limits: Optional[MemoryLimits] = None):
        """Initialize memory profiler with comprehensive validation and error handling."""
        try:
            logger.info("Initializing MemoryProfiler...")

            # Validate and set limits
            self.limits = limits or MemoryLimits()
            if not isinstance(self.limits, MemoryLimits):
                logger.warning(f"Invalid limits type, using defaults: {type(limits)}")
                self.limits = MemoryLimits()

            # Initialize data structures
            self.snapshots: deque = deque(maxlen=100)  # Keep last 100 snapshots
            self.peak_memory_mb = 0.0
            self.start_time = time.time()

            # Initialize process monitoring
            try:
                self.process = psutil.Process()
                logger.debug("Process monitoring initialized")
            except Exception as e:
                logger.error(f"Failed to initialize process monitoring: {e}")
                self.process = None

            # Initialize threading primitives
            self._lock = Lock()
            self._monitoring = False
            self._monitor_thread = None
            self._shutdown_event = Event()

            # Callbacks for memory events
            self.cleanup_callbacks: List[Callable] = []
            self.spill_callbacks: List[Callable] = []
            self.limit_exceeded_callbacks: List[Callable] = []

            # Initialize spill storage
            try:
                self.spill_dir = Path(tempfile.gettempdir()) / "leindex_spill"
                self.spill_dir.mkdir(exist_ok=True)
                self.spilled_data: Dict[str, str] = {}  # key -> file_path mapping
                logger.debug(f"Spill directory initialized: {self.spill_dir}")
            except Exception as e:
                logger.warning(f"Could not initialize spill directory: {e}")
                self.spill_dir = None
                self.spilled_data = {}

            # Initialize baseline memory usage
            self._baseline_memory = self._get_memory_usage()
            logger.debug(f"Baseline memory set: {self._baseline_memory:.2f}MB")

            # Validate initialization
            self._validate_initialization()

            logger.info("MemoryProfiler initialization completed successfully")

        except Exception as e:
            logger.error(f"MemoryProfiler initialization failed: {e}")
            # Ensure object is in a safe state even if initialization fails
            self._emergency_fallback_initialization()
            raise

    def _validate_initialization(self) -> bool:
        """Validate that the profiler was initialized correctly."""
        issues = []

        if self.process is None:
            issues.append("Process monitoring not available")

        if self.spill_dir is None or not self.spill_dir.exists():
            issues.append("Spill directory not available")

        if self._baseline_memory < 0:
            issues.append(f"Invalid baseline memory: {self._baseline_memory}MB")

        if self.start_time > time.time():
            issues.append("Invalid start time (future timestamp)")

        if issues:
            logger.warning(f"MemoryProfiler initialization issues detected: {issues}")
            return False

        logger.debug("MemoryProfiler initialization validation passed")
        return True

    def _emergency_fallback_initialization(self):
        """Emergency fallback initialization when main init fails."""
        logger.warning("Performing emergency fallback initialization")

        # Set safe defaults
        self.limits = MemoryLimits()
        self.snapshots = deque(maxlen=100)
        self.peak_memory_mb = 0.0
        self.start_time = time.time()
        self.process = None
        self._lock = Lock()
        self._monitoring = False
        self._monitor_thread = None
        self._shutdown_event = Event()
        self.cleanup_callbacks = []
        self.spill_callbacks = []
        self.limit_exceeded_callbacks = []
        self.spill_dir = None
        self.spilled_data = {}
        self._baseline_memory = 0.0

        logger.warning("Emergency fallback initialization completed - profiler will have limited functionality")
        
    def _get_memory_usage(self) -> float:
        """Get current memory usage in MB with comprehensive error handling."""
        try:
            if self.process is None:
                logger.warning("Process object is None - cannot get memory usage")
                return 0.0

            memory_info = self.process.memory_info()
            if memory_info is None:
                logger.warning("Memory info returned None")
                return 0.0

            memory_mb = memory_info.rss / 1024 / 1024  # Convert to MB

            # Validate memory value is reasonable
            if memory_mb < 0:
                logger.warning(f"Negative memory usage detected: {memory_mb}MB")
                return 0.0
            elif memory_mb > 1000000:  # More than 1TB seems unreasonable
                logger.warning(f"Excessive memory usage detected: {memory_mb}MB - possible error")
                return 0.0

            return memory_mb

        except psutil.NoSuchProcess:
            logger.warning("Process no longer exists - cannot get memory usage")
            return 0.0
        except psutil.AccessDenied:
            logger.warning("Access denied for memory monitoring - check permissions")
            return 0.0
        except AttributeError as e:
            logger.warning(f"Memory info attribute error: {e}")
            return 0.0
        except Exception as e:
            logger.warning(f"Unexpected error getting memory usage: {e}")
            return 0.0
    
    def _get_heap_size(self) -> float:
        """Estimate heap size in MB."""
        try:
            # Get object count and estimate size
            objects = gc.get_objects()
            total_size = sum(sys.getsizeof(obj) for obj in objects[:1000])  # Sample first 1000
            estimated_total = (total_size / 1000) * len(objects)
            return estimated_total / 1024 / 1024  # Convert to MB
        except Exception:
            return 0.0
    
    def _get_gc_stats(self) -> Tuple[int, int, int]:
        """Get garbage collection statistics."""
        try:
            stats = gc.get_stats()
            return (
                stats[0]['collections'] if len(stats) > 0 else 0,
                stats[1]['collections'] if len(stats) > 1 else 0,
                stats[2]['collections'] if len(stats) > 2 else 0
            )
        except Exception:
            return (0, 0, 0)
    
    def take_snapshot(self, loaded_files: int = 0, cached_queries: int = 0) -> MemorySnapshot:
        """Take a memory snapshot with comprehensive error handling and validation."""
        try:
            logger.debug("Taking memory snapshot...")

            # Get memory metrics with error handling
            current_memory = self._get_memory_usage()
            heap_size = self._get_heap_size()

            # Validate memory values
            if current_memory < 0:
                logger.warning(f"Invalid current memory value: {current_memory}MB")
                current_memory = 0.0

            if heap_size < 0:
                logger.warning(f"Invalid heap size value: {heap_size}MB")
                heap_size = 0.0

            # Update peak memory with validation
            if current_memory > 0 and current_memory > self.peak_memory_mb:
                self.peak_memory_mb = current_memory

            # Get GC statistics safely
            try:
                gc_objects = len(gc.get_objects())
                if gc_objects < 0:
                    logger.warning(f"Invalid GC objects count: {gc_objects}")
                    gc_objects = 0
            except Exception as e:
                logger.warning(f"Could not get GC objects count: {e}")
                gc_objects = 0

            gc_collections = self._get_gc_stats()

            # Get thread count safely
            try:
                active_threads = threading.active_count()
                if active_threads < 0:
                    logger.warning(f"Invalid thread count: {active_threads}")
                    active_threads = 0
            except Exception as e:
                logger.warning(f"Could not get thread count: {e}")
                active_threads = 0

            # Validate input parameters
            if loaded_files < 0:
                logger.warning(f"Invalid loaded_files count: {loaded_files}")
                loaded_files = 0

            if cached_queries < 0:
                logger.warning(f"Invalid cached_queries count: {cached_queries}")
                cached_queries = 0

            # Create snapshot
            snapshot = MemorySnapshot(
                timestamp=time.time(),
                process_memory_mb=current_memory,
                heap_size_mb=heap_size,
                peak_memory_mb=self.peak_memory_mb,
                gc_objects=gc_objects,
                gc_collections=gc_collections,
                active_threads=active_threads,
                loaded_files=loaded_files,
                cached_queries=cached_queries
            )

            # Store snapshot safely
            try:
                with self._lock:
                    self.snapshots.append(snapshot)
                logger.debug(f"Memory snapshot taken: {current_memory:.2f}MB process, {heap_size:.2f}MB heap")
            except Exception as e:
                logger.warning(f"Could not store snapshot: {e}")
                # Continue without storing - snapshot is still valid

            return snapshot

        except Exception as e:
            logger.error(f"Failed to take memory snapshot: {e}")
            # Return a minimal snapshot to prevent complete failure
            return MemorySnapshot(
                timestamp=time.time(),
                process_memory_mb=0.0,
                heap_size_mb=0.0,
                peak_memory_mb=self.peak_memory_mb,
                gc_objects=0,
                gc_collections=(0, 0, 0),
                active_threads=0,
                loaded_files=loaded_files,
                cached_queries=cached_queries
            )
    
    def check_limits(self, snapshot: MemorySnapshot) -> Dict[str, bool]:
        """Check if memory limits are exceeded."""
        violations = {
            'soft_limit': snapshot.process_memory_mb > self.limits.soft_limit_mb,
            'hard_limit': snapshot.process_memory_mb > self.limits.hard_limit_mb,
            'gc_threshold': snapshot.process_memory_mb > self.limits.gc_threshold_mb,
            'spill_threshold': snapshot.process_memory_mb > self.limits.spill_threshold_mb,
            'max_loaded_files': snapshot.loaded_files > self.limits.max_loaded_files,
            'max_cached_queries': snapshot.cached_queries > self.limits.max_cached_queries
        }
        
        return violations
    
    def enforce_limits(self, snapshot: MemorySnapshot) -> Dict[str, Any]:
        """Enforce memory limits and trigger appropriate actions."""
        violations = self.check_limits(snapshot)
        actions_taken = {
            'garbage_collection': False,
            'cleanup_triggered': False,
            'spill_triggered': False,
            'limit_exceeded': False
        }
        
        # Trigger garbage collection
        if violations['gc_threshold']:
            logger.info(f"Triggering garbage collection at {snapshot.process_memory_mb:.2f}MB")
            collected = gc.collect()
            actions_taken['garbage_collection'] = True
            logger.info(f"Garbage collection freed {collected} objects")
        
        # Trigger cleanup
        if violations['soft_limit'] or violations['max_loaded_files'] or violations['max_cached_queries']:
            logger.info(f"Triggering cleanup at {snapshot.process_memory_mb:.2f}MB")
            self._trigger_cleanup()
            actions_taken['cleanup_triggered'] = True
        
        # Trigger spill to disk
        if violations['spill_threshold']:
            logger.info(f"Triggering spill to disk at {snapshot.process_memory_mb:.2f}MB")
            self._trigger_spill()
            actions_taken['spill_triggered'] = True
        
        # Hard limit exceeded
        if violations['hard_limit']:
            logger.warning(f"Hard memory limit exceeded: {snapshot.process_memory_mb:.2f}MB")
            self._trigger_limit_exceeded()
            actions_taken['limit_exceeded'] = True
        
        return actions_taken
    
    def _trigger_cleanup(self):
        """Trigger cleanup callbacks."""
        for callback in self.cleanup_callbacks:
            try:
                callback()
            except Exception as e:
                logger.error(f"Error in cleanup callback: {e}")
    
    def _trigger_spill(self):
        """Trigger spill to disk callbacks."""
        for callback in self.spill_callbacks:
            try:
                callback()
            except Exception as e:
                logger.error(f"Error in spill callback: {e}")
    
    def _trigger_limit_exceeded(self):
        """Trigger limit exceeded callbacks."""
        for callback in self.limit_exceeded_callbacks:
            try:
                callback()
            except Exception as e:
                logger.error(f"Error in limit exceeded callback: {e}")
    
    def spill_to_disk(self, key: str, data: Any) -> bool:
        """Spill data to disk and return success status."""
        try:
            spill_file = self.spill_dir / f"{key}.pkl"
            with open(spill_file, 'wb') as f:
                pickle.dump(data, f)
            self.spilled_data[key] = str(spill_file)
            logger.info(f"Spilled data for key '{key}' to {spill_file}")
            return True
        except Exception as e:
            logger.error(f"Failed to spill data for key '{key}': {e}")
            return False
    
    def load_from_disk(self, key: str) -> Optional[Any]:
        """Load spilled data from disk."""
        if key not in self.spilled_data:
            return None
        
        try:
            spill_file = Path(self.spilled_data[key])
            if not spill_file.exists():
                del self.spilled_data[key]
                return None
            
            with open(spill_file, 'rb') as f:
                data = pickle.load(f)
            logger.info(f"Loaded spilled data for key '{key}' from {spill_file}")
            return data
        except Exception as e:
            logger.error(f"Failed to load spilled data for key '{key}': {e}")
            return None
    
    def cleanup_spill_files(self):
        """Clean up spilled files."""
        for key, file_path in list(self.spilled_data.items()):
            try:
                Path(file_path).unlink(missing_ok=True)
                del self.spilled_data[key]
            except Exception as e:
                logger.error(f"Failed to cleanup spill file {file_path}: {e}")
    
    def start_monitoring(self, interval: float = 30.0):
        """Start continuous memory monitoring."""
        if self._monitoring:
            return
        
        self._monitoring = True
        self._shutdown_event.clear()
        
        def monitor_loop():
            while not self._shutdown_event.wait(interval):
                try:
                    snapshot = self.take_snapshot()
                    self.enforce_limits(snapshot)
                except Exception as e:
                    logger.error(f"Error in memory monitoring: {e}")
        
        self._monitor_thread = threading.Thread(target=monitor_loop, daemon=True)
        self._monitor_thread.start()
        logger.info(f"Started memory monitoring with {interval}s interval")
    
    def stop_monitoring(self):
        """Stop continuous memory monitoring."""
        if not self._monitoring:
            return
        
        self._monitoring = False
        self._shutdown_event.set()
        if self._monitor_thread:
            self._monitor_thread.join(timeout=5.0)
        logger.info("Stopped memory monitoring")
    
    def get_stats(self) -> Dict[str, Any]:
        """Get comprehensive memory statistics with error handling and validation."""
        try:
            logger.debug("Collecting memory profiler statistics...")

            # Get current memory metrics safely
            current_memory = self._get_memory_usage()
            heap_size = self._get_heap_size()

            # Calculate memory growth safely
            try:
                memory_growth = current_memory - self._baseline_memory
                if abs(memory_growth) > 1000000:  # More than 1TB change seems unreasonable
                    logger.warning(f"Unreasonable memory growth detected: {memory_growth}MB")
                    memory_growth = 0.0
            except Exception as e:
                logger.warning(f"Could not calculate memory growth: {e}")
                memory_growth = 0.0

            # Get recent snapshots safely
            try:
                with self._lock:
                    recent_snapshots = list(self.snapshots)[-10:]  # Last 10 snapshots
            except Exception as e:
                logger.warning(f"Could not access snapshots: {e}")
                recent_snapshots = []

            # Get limits safely
            try:
                limits_dict = asdict(self.limits) if self.limits else {}
            except Exception as e:
                logger.warning(f"Could not serialize limits: {e}")
                limits_dict = {}

            # Check limits safely
            try:
                # Create a temporary snapshot for limit checking
                temp_snapshot = MemorySnapshot(
                    timestamp=time.time(),
                    process_memory_mb=current_memory,
                    heap_size_mb=heap_size,
                    peak_memory_mb=self.peak_memory_mb,
                    gc_objects=0,  # We'll get this safely below
                    gc_collections=(0, 0, 0),
                    active_threads=0,
                    loaded_files=0,
                    cached_queries=0
                )
                violations = self.check_limits(temp_snapshot)
            except Exception as e:
                logger.warning(f"Could not check limits: {e}")
                violations = {}

            # Get GC stats safely
            gc_stats = self._get_gc_stats()

            # Calculate uptime safely
            try:
                uptime_seconds = time.time() - self.start_time
                if uptime_seconds < 0:
                    logger.warning(f"Negative uptime detected: {uptime_seconds}s")
                    uptime_seconds = 0.0
            except Exception as e:
                logger.warning(f"Could not calculate uptime: {e}")
                uptime_seconds = 0.0

            # Get spill data safely
            try:
                spilled_items = len(self.spilled_data) if self.spilled_data else 0
                spill_directory = str(self.spill_dir) if self.spill_dir else ""
            except Exception as e:
                logger.warning(f"Could not get spill data: {e}")
                spilled_items = 0
                spill_directory = ""

            # Build comprehensive stats dictionary
            stats = {
                'current_memory_mb': current_memory,
                'peak_memory_mb': self.peak_memory_mb,
                'heap_size_mb': heap_size,
                'baseline_memory_mb': self._baseline_memory,
                'memory_growth_mb': memory_growth,
                'limits': limits_dict,
                'violations': violations,
                'monitoring_active': self._monitoring,
                'snapshots_count': len(recent_snapshots),
                'recent_snapshots': [asdict(s) for s in recent_snapshots if s],
                'spilled_items': spilled_items,
                'spill_directory': spill_directory,
                'gc_stats': gc_stats,
                'uptime_seconds': uptime_seconds,
                'health_status': 'healthy' if not violations else 'warning',
                'collection_timestamp': time.time()
            }

            logger.debug(f"Memory stats collected successfully: {current_memory:.2f}MB current, {heap_size:.2f}MB heap")
            return stats

        except Exception as e:
            logger.error(f"Failed to collect memory statistics: {e}")
            # Return minimal stats to prevent complete failure
            return {
                'error': f"Stats collection failed: {str(e)}",
                'current_memory_mb': 0.0,
                'peak_memory_mb': self.peak_memory_mb,
                'heap_size_mb': 0.0,
                'baseline_memory_mb': self._baseline_memory,
                'memory_growth_mb': 0.0,
                'limits': {},
                'violations': {},
                'monitoring_active': self._monitoring,
                'snapshots_count': 0,
                'recent_snapshots': [],
                'spilled_items': 0,
                'spill_directory': "",
                'gc_stats': (0, 0, 0),
                'uptime_seconds': 0.0,
                'health_status': 'error',
                'collection_timestamp': time.time()
            }
    
    def register_cleanup_callback(self, callback: Callable):
        """Register a callback to be called when cleanup is needed."""
        self.cleanup_callbacks.append(callback)
    
    def register_spill_callback(self, callback: Callable):
        """Register a callback to be called when spill is needed."""
        self.spill_callbacks.append(callback)
    
    def register_limit_exceeded_callback(self, callback: Callable):
        """Register a callback to be called when hard limits are exceeded."""
        self.limit_exceeded_callbacks.append(callback)
    
    def export_profile(self, file_path: str):
        """Export memory profile to a file."""
        try:
            profile_data = {
                'stats': self.get_stats(),
                'all_snapshots': [asdict(s) for s in self.snapshots]
            }
            
            with open(file_path, 'w') as f:
                json.dump(profile_data, f, indent=2)
            
            logger.info(f"Memory profile exported to {file_path}")
        except Exception as e:
            logger.error(f"Failed to export memory profile: {e}")
    
    def __del__(self):
        """Cleanup on destruction."""
        self.stop_monitoring()
        self.cleanup_spill_files()


class MemoryAwareManager:
    """Base class for memory-aware managers that integrate with the profiler."""
    
    def __init__(self, profiler: MemoryProfiler):
        self.profiler = profiler
        self.profiler.register_cleanup_callback(self.cleanup)
        self.profiler.register_spill_callback(self.spill_to_disk)
        self.profiler.register_limit_exceeded_callback(self.handle_limit_exceeded)
    
    def cleanup(self):
        """Override in subclasses to implement cleanup logic."""
        pass
    
    def spill_to_disk(self):
        """Override in subclasses to implement spill logic."""
        pass
    
    def handle_limit_exceeded(self):
        """Override in subclasses to handle hard limit exceeded."""
        pass


class MemoryAwareLazyContentManager(MemoryAwareManager):
    """Memory-aware version of LazyContentManager."""
    
    def __init__(self, profiler: MemoryProfiler, lazy_content_manager):
        super().__init__(profiler)
        self.lazy_content_manager = lazy_content_manager
        self._spill_lock = Lock()
    
    def cleanup(self):
        """Clean up loaded content to reduce memory usage."""
        logger.info("Cleaning up loaded file content")
        
        # Get memory stats before cleanup
        stats_before = self.lazy_content_manager.get_memory_stats()
        
        # Unload content from least recently used files
        with self.lazy_content_manager._lock:
            loaded_files = [
                (path, lc) for path, lc in self.lazy_content_manager._loaded_files.items()
                if lc.is_content_loaded()
            ]
            
            # Unload half of the loaded files
            files_to_unload = len(loaded_files) // 2
            for i in range(files_to_unload):
                if i < len(self.lazy_content_manager._access_order):
                    path = self.lazy_content_manager._access_order[i]
                    if path in self.lazy_content_manager._loaded_files:
                        self.lazy_content_manager._loaded_files[path].unload_content()
        
        # Clear query cache
        self.lazy_content_manager.query_cache.cache.clear()
        
        # Force garbage collection
        gc.collect()
        
        # Get memory stats after cleanup
        stats_after = self.lazy_content_manager.get_memory_stats()
        
        logger.info(f"Cleanup completed: {stats_before['loaded_files']} -> {stats_after['loaded_files']} loaded files")
    
    def spill_to_disk(self):
        """Spill cached query results to disk."""
        with self._spill_lock:
            logger.info("Spilling query cache to disk")
            
            # Spill query cache to disk
            cache_items = list(self.lazy_content_manager.query_cache.cache.items())
            if cache_items:
                spill_key = f"query_cache_{int(time.time())}"
                if self.profiler.spill_to_disk(spill_key, cache_items):
                    self.lazy_content_manager.query_cache.cache.clear()
                    logger.info(f"Spilled {len(cache_items)} query cache items to disk")
    
    def handle_limit_exceeded(self):
        """Handle hard memory limit exceeded."""
        logger.warning("Hard memory limit exceeded - performing aggressive cleanup")
        
        # Aggressive cleanup: unload all content
        self.lazy_content_manager.unload_all()
        
        # Clear all caches
        self.lazy_content_manager.query_cache.cache.clear()
        
        # Force garbage collection
        gc.collect()
        
        logger.warning("Aggressive cleanup completed")


def create_memory_config_from_yaml(config_data: Dict[str, Any]) -> MemoryLimits:
    """Create memory limits configuration from YAML data."""
    memory_config = config_data.get('memory', {})
    
    return MemoryLimits(
        soft_limit_mb=memory_config.get('soft_limit_mb', 512.0),
        hard_limit_mb=memory_config.get('hard_limit_mb', 1024.0),
        max_loaded_files=memory_config.get('max_loaded_files', 100),
        max_cached_queries=memory_config.get('max_cached_queries', 50),
        gc_threshold_mb=memory_config.get('gc_threshold_mb', 256.0),
        spill_threshold_mb=memory_config.get('spill_threshold_mb', 768.0)
    )
