"""
Parallel Processing Module

This module implements chunked and parallel indexing capabilities
to avoid blocking the event loop and improve performance.
"""

import os
import asyncio
import concurrent.futures
from typing import List, Dict, Any, Callable, Optional
from threading import Lock
from dataclasses import dataclass
import time

# Import progress tracking
from .file_reader import SmartFileReader
from .logger_config import logger


@dataclass
class IndexingTask:
    """Represents a single indexing task."""
    directory_path: str
    files: List[str]
    task_id: str
    metadata: Optional[Dict[str, Any]] = None


@dataclass
class IndexingResult:
    """Result of an indexing task."""
    task_id: str
    indexed_files: List[Dict[str, Any]]
    errors: List[str]
    processing_time: float
    success: bool


class ParallelIndexer:
    """Handles parallel indexing of directory chunks."""
    
    def __init__(self, max_workers: int = 4, chunk_size: int = 100):
        self.max_workers = max_workers
        self.chunk_size = chunk_size
        self.executor = concurrent.futures.ThreadPoolExecutor(max_workers=max_workers)
        self._active_tasks: Dict[str, asyncio.Task] = {}
        self._task_counter = 0
        self._lock = Lock()
    
    def create_chunks(self, file_list: List[str], base_path: str) -> List[IndexingTask]:
        """Divide file list into chunks for parallel processing."""
        chunks = []
        
        for i in range(0, len(file_list), self.chunk_size):
            chunk_files = file_list[i:i + self.chunk_size]
            
            with self._lock:
                task_id = f"task_{self._task_counter}"
                self._task_counter += 1
            
            task = IndexingTask(
                directory_path=base_path,
                files=chunk_files,
                task_id=task_id
            )
            chunks.append(task)
        
        return chunks
    
    async def process_chunk_async(self, task: IndexingTask, 
                                  processor_func: Callable[[IndexingTask], IndexingResult]) -> IndexingResult:
        """Process a single chunk asynchronously."""
        loop = asyncio.get_event_loop()
        
        try:
            # Run the blocking operation in the thread pool
            result = await loop.run_in_executor(self.executor, processor_func, task)
            return result
        except Exception as e:
            return IndexingResult(
                task_id=task.task_id,
                indexed_files=[],
                errors=[str(e)],
                processing_time=0.0,
                success=False
            )
    
    async def process_chunks_parallel(self, tasks: List[IndexingTask], 
                                      processor_func: Callable[[IndexingTask], IndexingResult],
                                      progress_callback: Optional[Callable[[float], None]] = None) -> List[IndexingResult]:
        """Process multiple chunks in parallel."""
        
        # Create async tasks for all chunks
        async_tasks = []
        for task in tasks:
            async_task = asyncio.create_task(
                self.process_chunk_async(task, processor_func)
            )
            async_tasks.append(async_task)
            self._active_tasks[task.task_id] = async_task
        
        results = []
        completed = 0
        total = len(async_tasks)
        
        try:
            # Process tasks as they complete
            for coro in asyncio.as_completed(async_tasks):
                result = await coro
                results.append(result)
                completed += 1
                
                # Update progress
                if progress_callback:
                    progress = completed / total
                    progress_callback(progress)
                
                # Clean up completed task
                if result.task_id in self._active_tasks:
                    del self._active_tasks[result.task_id]
        
        except asyncio.CancelledError:
            # Cancel all remaining tasks
            for task in async_tasks:
                if not task.done():
                    task.cancel()
            raise
        
        return results
    
    def cancel_all_tasks(self):
        """Cancel all active tasks."""
        for task in self._active_tasks.values():
            task.cancel()
        self._active_tasks.clear()
    
    def get_active_task_count(self) -> int:
        """Get the number of currently active tasks."""
        return len(self._active_tasks)
    
    def shutdown(self):
        """Shutdown the thread pool executor."""
        self.executor.shutdown(wait=True)
    
    async def process_files(self, tasks: List[IndexingTask]) -> List[IndexingResult]:
        """Process files using the parallel indexer."""

        def process_task(task: IndexingTask) -> IndexingResult:
            """Process a single indexing task.

            This function runs in a worker thread and performs TRUE parallel I/O
            by reading file content directly in the worker, not sequentially later.
            """
            start_time = time.time()
            indexed_files = []
            errors = []

            # Create SmartFileReader instance for this worker thread
            # Each worker gets its own instance for thread safety
            smart_reader = SmartFileReader(task.directory_path)

            try:
                # Process each file in the task
                for file_path in task.files:
                    try:
                        # Get file extension
                        _, ext = os.path.splitext(file_path)

                        # Get full file path for content reading
                        full_path = os.path.join(task.directory_path, file_path)

                        # ✅ Read content HERE in worker thread (TRUE PARALLEL I/O)
                        # This is the key change: content is read in parallel by workers,
                        # not sequentially later in the main thread
                        content = None
                        content_error = None

                        try:
                            content = smart_reader.read_content(full_path)
                            if content is None:
                                content_error = "SmartFileReader returned None"
                        except Exception as read_err:
                            content_error = str(read_err)
                            logger.warning(f"Failed to read content for {file_path}: {read_err}")

                        # Create file info with pre-read content
                        file_info = {
                            'path': file_path,
                            'type': 'file',
                            'extension': ext,
                            'content': content,  # ✅ Include pre-read content
                            'content_error': content_error,  # ✅ Track read errors
                            'metadata': task.metadata or {}
                        }

                        indexed_files.append(file_info)

                    except Exception as e:
                        error_msg = f"Error processing {file_path}: {str(e)}"
                        errors.append(error_msg)
                        logger.warning(error_msg)

                processing_time = time.time() - start_time

                # Consider task successful if we processed at least some files
                # even if some individual files had errors
                success = len(indexed_files) > 0 or len(errors) == 0

                return IndexingResult(
                    task_id=task.task_id,
                    indexed_files=indexed_files,
                    errors=errors,
                    processing_time=processing_time,
                    success=success
                )

            except Exception as e:
                processing_time = time.time() - start_time
                error_msg = f"Task {task.task_id} failed with exception: {str(e)}"
                logger.error(error_msg)
                return IndexingResult(
                    task_id=task.task_id,
                    indexed_files=[],
                    errors=[error_msg],
                    processing_time=processing_time,
                    success=False
                )

        # Process tasks in parallel
        return await self.process_chunks_parallel(tasks, process_task)


class AsyncFileProcessor:
    """Processes files asynchronously with progress tracking."""
    
    def __init__(self, max_concurrent: int = 10):
        self.max_concurrent = max_concurrent
        self._semaphore = asyncio.Semaphore(max_concurrent)
        self._processed_count = 0
        self._total_count = 0
        self._lock = asyncio.Lock()
    
    async def process_files_async(self, file_paths: List[str], 
                                  processor_func: Callable[[str], Dict[str, Any]],
                                  progress_callback: Optional[Callable[[int, int], None]] = None) -> List[Dict[str, Any]]:
        """Process files asynchronously with concurrency control."""
        
        self._total_count = len(file_paths)
        self._processed_count = 0
        
        async def process_single_file(file_path: str) -> Dict[str, Any]:
            async with self._semaphore:
                loop = asyncio.get_event_loop()
                
                try:
                    # Run blocking operation in thread pool
                    result = await loop.run_in_executor(None, processor_func, file_path)
                    
                    async with self._lock:
                        self._processed_count += 1
                        if progress_callback:
                            progress_callback(self._processed_count, self._total_count)
                    
                    return result
                
                except Exception as e:
                    async with self._lock:
                        self._processed_count += 1
                        if progress_callback:
                            progress_callback(self._processed_count, self._total_count)
                    
                    return {
                        'file_path': file_path,
                        'error': str(e),
                        'success': False
                    }
        
        # Create tasks for all files
        tasks = [process_single_file(fp) for fp in file_paths]
        
        # Execute all tasks concurrently
        results = await asyncio.gather(*tasks, return_exceptions=True)
        
        # Handle any exceptions that occurred
        processed_results = []
        for i, result in enumerate(results):
            if isinstance(result, Exception):
                processed_results.append({
                    'file_path': file_paths[i],
                    'error': str(result),
                    'success': False
                })
            else:
                processed_results.append(result)
        
        return processed_results


# Legacy classes remain for backward compatibility
# Use new progress tracking system from progress_tracker module instead
