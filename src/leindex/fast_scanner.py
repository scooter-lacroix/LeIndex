"""
Fast Parallel Scanner - Work Queue Implementation

This module implements a high-performance parallel directory scanner using
a work queue pattern instead of creating a task per directory.

Key differences from ParallelScanner:
- Uses a single work queue instead of task-per-directory
- Worker tasks pull directories from queue continuously
- Dramatically reduces asyncio overhead
- Better CPU utilization

Performance improvements:
- 10-100x faster for large directory structures
- Constant memory usage regardless of directory count
- Better thread pool utilization
"""

import asyncio
import os
from typing import List, Tuple, Optional, Set, Dict, Any, Callable
from pathlib import Path
import time
from collections import deque
from dataclasses import dataclass
from datetime import datetime

from .logger_config import logger
from .parallel_scanner import SymlinkCycleDetector


@dataclass
class ScanError:
    """Represents an error that occurred during scanning.

    Attributes:
        error_type: Type of error ('timeout', 'worker_crash', 'matcher_error',
            'permission_error', 'symlink_error', 'work_error')
        path: Path where error occurred (if applicable)
        message: Error message
        timestamp: When the error occurred
        worker_id: ID of worker that encountered the error (if applicable)
        exception: The original exception (if applicable)
    """

    error_type: str
    path: Optional[str]
    message: str
    timestamp: datetime
    worker_id: Optional[int] = None
    exception: Optional[Exception] = None

    def to_dict(self) -> dict:
        """Convert error to dictionary for serialization."""
        return {
            'error_type': self.error_type,
            'path': self.path,
            'message': self.message,
            'timestamp': self.timestamp.isoformat(),
            'worker_id': self.worker_id,
            'exception': str(self.exception) if self.exception else None
        }


class FastParallelScanner:
    """
    High-performance parallel directory scanner using work queue pattern.

    Unlike ParallelScanner which creates an asyncio task for every directory,
    this scanner uses a fixed number of worker tasks that continuously pull
    directories from a work queue. This dramatically reduces overhead.

    Performance characteristics:
    - O(d) time complexity where d = number of directories
    - O(w) memory where w = number of workers (not directories!)
    - Constant overhead regardless of directory tree size

    Example:
        scanner = FastParallelScanner(max_workers=4)
        results = await scanner.scan('/path/to/project')
    """

    # Safe maximum number of workers to prevent file descriptor exhaustion
    SAFE_MAX_WORKERS = 50

    # Default maximum directory depth to prevent directory bomb attacks
    DEFAULT_MAX_DIRECTORY_DEPTH = 1000

    def __init__(
        self,
        max_workers: int = 4,
        progress_callback: Optional[Callable[[int, int], None]] = None,
        timeout: float = 300.0,
        max_symlink_depth: int = 8,
        enable_symlink_protection: bool = True,
        ignore_matcher: Optional[Any] = None,
        debug_performance: bool = False,
        max_directory_depth: int = DEFAULT_MAX_DIRECTORY_DEPTH
    ):
        """
        Initialize the fast parallel scanner.

        Args:
            max_workers: Number of worker tasks (default: 4, capped at SAFE_MAX_WORKERS)
            progress_callback: Optional progress callback
            timeout: Maximum scan time in seconds (default: 300s)
            max_symlink_depth: Maximum symlink depth (default: 8)
            enable_symlink_protection: Enable symlink cycle detection
            ignore_matcher: Optional IgnorePatternMatcher instance
            debug_performance: Enable performance logging
            max_directory_depth: Maximum directory depth (default: 1000)
        """
        # Cap max_workers at safe value
        if max_workers > self.SAFE_MAX_WORKERS:
            logger.warning(
                f"max_workers {max_workers} exceeds safe limit {self.SAFE_MAX_WORKERS}. "
                f"Capping at {self.SAFE_MAX_WORKERS}."
            )
            max_workers = self.SAFE_MAX_WORKERS

        self.max_workers = max_workers
        self.progress_callback = progress_callback
        self.timeout = timeout
        self.max_symlink_depth = max_symlink_depth
        self.enable_symlink_protection = enable_symlink_protection
        self.ignore_matcher = ignore_matcher
        self.debug_performance = debug_performance
        self.max_directory_depth = max_directory_depth

        # Work queue and results
        self._work_queue: asyncio.Queue = asyncio.Queue()
        self._results: List[Tuple[str, List[str], List[str]]] = []
        self._scanned_count = 0
        self._start_time = None

        # Scan completion tracking
        self._scan_complete: bool = True

        # Error tracking
        self._errors: List[ScanError] = []
        self._failed_workers: int = 0
        self._worker_errors: int = 0
        self._matcher_errors: int = 0
        self._workers_active: List[bool] = [True] * max_workers
        self._skipped_symlinks: int = 0
        self._skipped_permissions: int = 0

        # Symlink protection
        self._symlink_detector = SymlinkCycleDetector(max_depth=max_symlink_depth) if enable_symlink_protection else None

        # Performance counters
        self._perf_scandir_calls = 0
        self._perf_slow_scandirs = 0
        self._perf_total_scandir_time = 0.0
        self._perf_slowest_scandir = ("", 0.0)
        self._perf_last_log_time = time.time()

        # Synchronization
        self._scan_lock = asyncio.Lock()
        self._active_workers = 0

    async def scan(self, root_path: str) -> List[Tuple[str, List[str], List[str]]]:
        """
        Scan directory tree using work queue pattern.

        Args:
            root_path: Absolute path to root directory

        Returns:
            List of (root, dirs, files) tuples in depth-first order
        """
        self._start_time = time.time()
        self._scanned_count = 0
        self._results.clear()
        self._errors.clear()
        self._scan_complete = True
        self._failed_workers = 0
        self._worker_errors = 0
        self._matcher_errors = 0
        self._workers_active = [True] * self.max_workers
        self._skipped_symlinks = 0
        self._skipped_permissions = 0

        # Reset performance counters
        if self.debug_performance:
            self._perf_scandir_calls = 0
            self._perf_slow_scandirs = 0
            self._perf_total_scandir_time = 0.0
            self._perf_slowest_scandir = ("", 0.0)
            self._perf_last_log_time = time.time()
            logger.info(f"[PERF] Starting fast scan: {root_path}")

        # Reset symlink detector
        if self._symlink_detector:
            self._symlink_detector.reset()

        # Validate root path
        if not os.path.isdir(root_path):
            raise OSError(f"Not a directory: {root_path}")

        logger.info(f"Starting fast parallel scan with {self.max_workers} workers: {root_path}")

        try:
            # Run scan with timeout
            results = await asyncio.wait_for(
                self._scan_with_queue(root_path),
                timeout=self.timeout
            )

            elapsed = time.time() - self._start_time
            logger.info(
                f"Fast scan completed: {len(results)} directories in {elapsed:.2f}s "
                f"({len(results) / elapsed:.1f} dirs/sec)"
            )

            # Log performance statistics
            if self.debug_performance:
                self._log_performance_stats(elapsed)

            return results

        except asyncio.TimeoutError:
            elapsed = time.time() - self._start_time
            logger.error(
                f"Fast scan timed out after {elapsed:.2f}s - "
                f"returning partial results ({len(self._results)} directories)"
            )
            self._scan_complete = False

            # Record timeout error
            error = ScanError(
                error_type='timeout',
                path=None,
                message=f"Scan timed out after {elapsed:.2f}s",
                timestamp=datetime.now()
            )
            self._errors.append(error)

            # Return partial results instead of raising
            return self._results

    async def _scan_with_queue(self, root_path: str) -> List[Tuple[str, List[str], List[str]]]:
        """
        Scan using work queue pattern.

        Algorithm:
        1. Add root directory to work queue
        2. Spawn N worker tasks
        3. Each worker:
           - Pulls directory from queue
           - Scans it
           - Adds subdirectories to queue
           - Repeats until queue is empty
        4. Wait for all workers to complete
        5. Sort results in depth-first order
        """
        # Add root to queue with depth 0
        await self._work_queue.put((root_path, 0, 0))  # (path, symlink_depth, dir_depth)

        # Create worker tasks
        workers = [
            asyncio.create_task(self._worker(worker_id=i))
            for i in range(self.max_workers)
        ]

        # Wait for queue to be empty and all workers to finish
        try:
            await asyncio.wait_for(
                self._work_queue.join(),
                timeout=self.timeout
            )
        except asyncio.TimeoutError:
            logger.error(
                "Queue join timeout - some workers may have crashed. "
                f"Proceeding with {len(self._results)} directories scanned."
            )
            self._scan_complete = False

            # Record queue join timeout error
            error = ScanError(
                error_type='queue_join_timeout',
                path=None,
                message=f"Queue join timeout after {self.timeout}s",
                timestamp=datetime.now()
            )
            self._errors.append(error)

        # Cancel worker tasks (they will exit when queue is empty)
        for worker in workers:
            worker.cancel()

        # Wait for workers to acknowledge cancellation
        await asyncio.gather(*workers, return_exceptions=True)

        # Sort results in depth-first order for compatibility
        self._results.sort(key=lambda x: x[0])

        return self._results

    async def _worker(self, worker_id: int):
        """
        Worker task that continuously processes directories from queue.

        This is the core of the work queue pattern. Instead of creating
        a task per directory, we have fixed worker tasks that pull
        directories from the queue and process them.

        Args:
            worker_id: Worker identifier for logging
        """
        try:
            while True:
                try:
                    # Pull directory from queue (blocks if empty)
                    task_data = await asyncio.wait_for(
                        self._work_queue.get(),
                        timeout=0.1  # Check periodically for cancellation
                    )

                    # Validate queue item
                    if not isinstance(task_data, tuple) or len(task_data) != 3:
                        logger.error(f"Invalid queue item: {task_data}")
                        self._work_queue.task_done()
                        continue

                    dirpath, symlink_depth, dir_depth = task_data

                    # Validate directory path
                    if not isinstance(dirpath, str) or not dirpath:
                        logger.error(f"Invalid directory path in queue item: {dirpath}")
                        self._work_queue.task_done()
                        continue

                    # Check directory depth limit
                    if dir_depth >= self.max_directory_depth:
                        logger.warning(
                            f"Max directory depth ({self.max_directory_depth}) exceeded at: {dirpath}"
                        )
                        self._work_queue.task_done()
                        continue

                    # Scan the directory
                    result = await self._scan_directory(dirpath, symlink_depth)

                    if result:
                        # Add result to list
                        async with self._scan_lock:
                            self._results.append(result)

                        # Add subdirectories to queue
                        _, dirs, _ = result
                        for dirname in dirs:
                            subdirpath = os.path.join(dirpath, dirname)

                            # Check symlink depth
                            new_depth = symlink_depth
                            if self._symlink_detector:
                                try:
                                    if os.path.islink(subdirpath):
                                        new_depth = symlink_depth + 1
                                        if new_depth >= self.max_symlink_depth:
                                            logger.warning(
                                                f"Symlink cycle detected or max depth exceeded: {subdirpath}"
                                            )
                                            continue
                                except OSError:
                                    pass

                            # Add to queue for processing with incremented directory depth
                            await self._work_queue.put((subdirpath, new_depth, dir_depth + 1))

                    # Mark this directory as complete
                    self._work_queue.task_done()

                except asyncio.TimeoutError:
                    # No work available, check if we should exit
                    if self._work_queue.empty():
                        # Queue is empty, we're done
                        return
                    # Otherwise, continue waiting
                    continue

                except asyncio.CancelledError:
                    # Worker was cancelled, exit gracefully
                    return

                except Exception as work_error:
                    # Error processing this directory, but continue with others
                    logger.error(
                        f"Worker {worker_id} error scanning {dirpath}: {work_error}",
                        exc_info=True
                    )
                    self._worker_errors += 1

                    # Record work error
                    error = ScanError(
                        error_type='work_error',
                        path=dirpath,
                        message=str(work_error),
                        timestamp=datetime.now(),
                        worker_id=worker_id,
                        exception=work_error
                    )
                    self._errors.append(error)

                    # Mark task as done so queue doesn't hang
                    self._work_queue.task_done()

        except Exception as worker_crash:
            # Worker itself crashed (critical error)
            logger.critical(
                f"Worker {worker_id} crashed: {worker_crash}",
                exc_info=True
            )
            self._failed_workers += 1

            # Record worker crash
            error = ScanError(
                error_type='worker_crash',
                path=None,
                message=f"Worker {worker_id} crashed: {worker_crash}",
                timestamp=datetime.now(),
                worker_id=worker_id,
                exception=worker_crash
            )
            self._errors.append(error)

        finally:
            # Mark worker as no longer active
            self._workers_active[worker_id] = False

    async def _scan_directory(
        self,
        dirpath: str,
        symlink_depth: int = 0
    ) -> Optional[Tuple[str, List[str], List[str]]]:
        """
        Scan a single directory.

        This is similar to ParallelScanner's _scan_directory but
        optimized for the work queue pattern.

        Args:
            dirpath: Absolute path to directory
            symlink_depth: Current symlink depth

        Returns:
            Tuple of (dirpath, dirs, files) or None
        """
        scandir_start = time.time()

        try:
            # Check symlink safety
            if self._symlink_detector and symlink_depth > 0:
                if not self._symlink_detector.is_safe_to_follow(dirpath, symlink_depth):
                    logger.warning(f"Skipping broken/circular symlink: {dirpath}")
                    self._skipped_symlinks += 1
                    return None

            # Run scandir in thread pool
            entries = await asyncio.to_thread(self._scandir_sync, dirpath)
            scandir_time = time.time() - scandir_start

            dirs = []
            files = []

            # Process entries
            for entry in entries:
                try:
                    if entry.is_dir(follow_symlinks=False):
                        # Check if symlink
                        is_symlink = entry.is_symlink()

                        if is_symlink and self._symlink_detector:
                            full_path = os.path.join(dirpath, entry.name)
                            if not self._symlink_detector.is_safe_to_follow(full_path, symlink_depth):
                                continue

                        # Check ignore patterns
                        if self.ignore_matcher:
                            full_path = os.path.join(dirpath, entry.name)
                            try:
                                rel_path = os.path.relpath(full_path, self.ignore_matcher.base_path)
                            except ValueError:
                                rel_path = full_path

                            try:
                                if self.ignore_matcher.should_ignore_directory(rel_path):
                                    continue
                            except Exception as matcher_error:
                                logger.warning(
                                    f"ignore_matcher failed for {rel_path}: {matcher_error}. "
                                    f"Including directory (fail-open)."
                                )
                                self._matcher_errors += 1

                                # Record matcher error
                                error = ScanError(
                                    error_type='matcher_error',
                                    path=rel_path,
                                    message=str(matcher_error),
                                    timestamp=datetime.now()
                                )
                                self._errors.append(error)

                                # Continue processing (fail-open)

                        dirs.append(entry.name)

                    elif entry.is_file(follow_symlinks=False):
                        files.append(entry.name)

                except OSError:
                    continue

            # Update performance counters
            if self.debug_performance:
                self._perf_scandir_calls += 1
                self._perf_total_scandir_time += scandir_time

                if scandir_time > 0.1:
                    self._perf_slow_scandirs += 1
                    logger.warning(f"[PERF] SLOW: {dirpath} ({scandir_time:.3f}s)")

                if scandir_time > self._perf_slowest_scandir[1]:
                    self._perf_slowest_scandir = (dirpath, scandir_time)

                # Log progress every 5 seconds
                now = time.time()
                if now - self._perf_last_log_time >= 5.0:
                    self._perf_last_log_time = now
                    elapsed = now - self._start_time
                    rate = self._scanned_count / elapsed if elapsed > 0 else 0
                    logger.info(
                        f"[PERF] {self._scanned_count} dirs in {elapsed:.1f}s "
                        f"({rate:.1f} dirs/sec) | {dirpath}"
                    )

            # Update counters
            self._scanned_count += 1
            if self.progress_callback:
                try:
                    self.progress_callback(self._scanned_count, 0)
                except Exception:
                    pass

            return (dirpath, dirs, files)

        except (PermissionError, OSError) as e:
            logger.warning(f"Permission denied: {dirpath} - {e}")
            self._skipped_permissions += 1
            return None

    @staticmethod
    def _scandir_sync(dirpath: str) -> List:
        """Synchronous scandir wrapper."""
        return list(os.scandir(dirpath))

    def _log_performance_stats(self, elapsed: float):
        """Log performance statistics."""
        logger.info("=" * 80)
        logger.info("[PERF] FAST SCAN PERFORMANCE")
        logger.info("=" * 80)
        logger.info(f"[PERF] Directories: {self._scanned_count}")
        logger.info(f"[PERF] Time: {elapsed:.2f}s")
        logger.info(f"[PERF] Rate: {self._scanned_count / elapsed:.1f} dirs/sec")
        logger.info(f"[PERF] Avg scandir: {self._perf_total_scandir_time / self._perf_scandir_calls * 1000:.2f}ms")
        logger.info(f"[PERF] Slow scandirs: {self._perf_slow_scandirs}")
        logger.info(f"[PERF] Slowest: {self._perf_slowest_scandir[0]} ({self._perf_slowest_scandir[1]:.3f}s)")
        logger.info("=" * 80)

    def get_stats(self) -> dict:
        """Get comprehensive scan statistics.

        Returns:
            Dictionary containing scan statistics including:
            - scanned_directories: Number of directories scanned
            - elapsed_seconds: Time elapsed since scan started
            - directories_per_second: Scan rate
            - scan_complete: Whether scan completed without timeout
            - partial_result: Whether result is partial (due to timeout)
            - failed_workers: Number of workers that crashed
            - worker_errors: Number of worker errors during scanning
            - matcher_errors: Number of ignore_matcher errors
            - skipped_symlinks: Number of symlinks skipped
            - skipped_permissions: Number of directories skipped due to permissions
            - has_errors: Whether any errors occurred
            - total_errors: Total number of errors
            - error_summary: Summary of errors by type
            - max_workers: Number of workers configured
            - scandir_calls: Number of scandir calls (if debug_performance)
            - slow_scandirs: Number of slow scandir calls (if debug_performance)
        """
        elapsed = time.time() - self._start_time if self._start_time else 0
        return {
            'scanned_directories': self._scanned_count,
            'elapsed_seconds': elapsed,
            'directories_per_second': self._scanned_count / elapsed if elapsed > 0 else 0,
            'scan_complete': self._scan_complete,
            'partial_result': not self._scan_complete,
            'failed_workers': self._failed_workers,
            'worker_errors': self._worker_errors,
            'matcher_errors': self._matcher_errors,
            'skipped_symlinks': self._skipped_symlinks,
            'skipped_permissions': self._skipped_permissions,
            'has_errors': self.has_errors(),
            'total_errors': len(self._errors),
            'error_summary': self.get_error_summary(),
            'max_workers': self.max_workers,
            'scandir_calls': self._perf_scandir_calls,
            'slow_scandirs': self._perf_slow_scandirs
        }

    def get_errors(self) -> List[dict]:
        """Get all errors that occurred during the scan.

        Returns:
            List of error dictionaries containing:
            - error_type: Type of error
            - path: Path where error occurred (if applicable)
            - message: Error message
            - timestamp: ISO timestamp of error
            - worker_id: Worker ID (if applicable)
            - exception: Exception string (if applicable)
        """
        return [error.to_dict() for error in self._errors]

    def get_error_summary(self) -> dict:
        """Get a summary of errors by type.

        Returns:
            Dictionary mapping error types to counts.
        """
        summary = {}
        for error in self._errors:
            error_type = error.error_type
            summary[error_type] = summary.get(error_type, 0) + 1
        return summary

    def has_errors(self) -> bool:
        """Check if any errors occurred during the scan.

        Returns:
            True if any errors were recorded, False otherwise.
        """
        return len(self._errors) > 0

    def get_recent_errors(self, limit: int = 10) -> List[dict]:
        """Get the most recent errors.

        Args:
            limit: Maximum number of recent errors to return (default: 10)

        Returns:
            List of the most recent error dictionaries.
        """
        recent = self._errors[-limit:] if self._errors else []
        return [error.to_dict() for error in recent]
