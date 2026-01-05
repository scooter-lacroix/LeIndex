"""
Async Search Module

This module provides asynchronous search capabilities with progress tracking
and cancellation support for the MCP server.
"""

import asyncio
import shutil
import subprocess
import time
from abc import ABC, abstractmethod
from typing import Dict, List, Optional, Tuple, Callable
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass
from enum import Enum

from .base import parse_search_output, create_safe_fuzzy_pattern


class OperationStatus(Enum):
    """Status of async operations."""
    PENDING = "pending"
    RUNNING = "running"
    COMPLETED = "completed"
    CANCELLED = "cancelled"
    FAILED = "failed"


@dataclass
class SearchProgress:
    """Progress information for search operations."""
    operation_id: str
    status: OperationStatus
    current_file: Optional[str] = None
    files_searched: int = 0
    total_files: int = 0
    matches_found: int = 0
    elapsed_time: float = 0.0
    estimated_remaining: float = 0.0
    error: Optional[str] = None

    @property
    def progress_percent(self) -> float:
        """Calculate progress percentage."""
        if self.total_files <= 0:
            return 0.0
        return (self.files_searched / self.total_files) * 100.0


class AsyncSearchStrategy(ABC):
    """Abstract base class for async search strategies."""
    
    def __init__(self, max_workers: int = 4):
        self.max_workers = max_workers
        self._executor = ThreadPoolExecutor(max_workers=max_workers)
        self._active_operations: Dict[str, asyncio.Task] = {}
        self._operation_counter = 0
        self._lock = asyncio.Lock()
    
    @property
    @abstractmethod
    def name(self) -> str:
        """Name of the search strategy."""
        pass
    
    @abstractmethod
    def is_available(self) -> bool:
        """Check if search tool is available."""
        pass
    
    async def search_async(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
        progress_callback: Optional[Callable[[SearchProgress], None]] = None,
        cancellation_token: Optional[asyncio.Event] = None
    ) -> Dict[str, List[Tuple[int, str]]]:
        """Perform async search with progress tracking."""
        
        async with self._lock:
            operation_id = f"search_{self._operation_counter}"
            self._operation_counter += 1
        
        start_time = time.time()
        progress = SearchProgress(
            operation_id=operation_id,
            status=OperationStatus.PENDING
        )
        
        if progress_callback:
            progress_callback(progress)
        
        try:
            # Update status to running
            progress.status = OperationStatus.RUNNING
            if progress_callback:
                progress_callback(progress)
            
            # Perform the actual search
            result = await self._execute_search(
                pattern, base_path, case_sensitive, context_lines,
                file_pattern, fuzzy, progress, progress_callback,
                cancellation_token
            )
            
            # Update final status
            progress.status = OperationStatus.COMPLETED
            progress.elapsed_time = time.time() - start_time
            progress.matches_found = sum(len(matches) for matches in result.values())
            
            if progress_callback:
                progress_callback(progress)
            
            return result
        
        except asyncio.CancelledError:
            progress.status = OperationStatus.CANCELLED
            progress.elapsed_time = time.time() - start_time
            if progress_callback:
                progress_callback(progress)
            raise
        
        except Exception as e:
            progress.status = OperationStatus.FAILED
            progress.error = str(e)
            progress.elapsed_time = time.time() - start_time
            if progress_callback:
                progress_callback(progress)
            raise
    
    @abstractmethod
    async def _execute_search(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool,
        context_lines: int,
        file_pattern: Optional[str],
        fuzzy: bool,
        progress: SearchProgress,
        progress_callback: Optional[Callable[[SearchProgress], None]],
        cancellation_token: Optional[asyncio.Event]
    ) -> Dict[str, List[Tuple[int, str]]]:
        """Execute the actual search operation."""
        pass
    
    async def search_multiple_async(
        self,
        patterns: List[str],
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
        progress_callback: Optional[Callable[[str, SearchProgress], None]] = None,
        cancellation_token: Optional[asyncio.Event] = None
    ) -> Dict[str, Dict[str, List[Tuple[int, str]]]]:
        """Execute multiple searches concurrently."""
        
        async def search_single_pattern(pattern: str) -> Tuple[str, Dict[str, List[Tuple[int, str]]]]:
            """Search for a single pattern."""
            def single_progress_callback(progress: SearchProgress):
                if progress_callback:
                    progress_callback(pattern, progress)
            
            try:
                result = await self.search_async(
                    pattern, base_path, case_sensitive, context_lines,
                    file_pattern, fuzzy, single_progress_callback, cancellation_token
                )
                return pattern, result
            except Exception as e:
                return pattern, {"error": str(e)}
        
        # Create tasks for all patterns
        tasks = [search_single_pattern(pattern) for pattern in patterns]
        
        # Execute concurrently
        results = {}
        for task in asyncio.as_completed(tasks):
            if cancellation_token and cancellation_token.is_set():
                # Cancel remaining tasks
                for remaining_task in tasks:
                    if not remaining_task.done():
                        remaining_task.cancel()
                raise asyncio.CancelledError("Search operation was cancelled")
            
            pattern, result = await task
            results[pattern] = result
        
        return results
    
    def shutdown(self):
        """Shutdown the executor."""
        self._executor.shutdown(wait=True)
    
    async def cancel_operation(self, operation_id: str) -> bool:
        """Cancel a specific operation."""
        async with self._lock:
            if operation_id in self._active_operations:
                task = self._active_operations[operation_id]
                task.cancel()
                del self._active_operations[operation_id]
                return True
        return False
    
    async def cancel_all_operations(self):
        """Cancel all active operations."""
        async with self._lock:
            for task in self._active_operations.values():
                task.cancel()
            self._active_operations.clear()


class AsyncRipgrepStrategy(AsyncSearchStrategy):
    """Async version of ripgrep search strategy."""
    
    @property
    def name(self) -> str:
        return 'ripgrep-async'
    
    def is_available(self) -> bool:
        return shutil.which('rg') is not None
    
    async def _execute_search(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool,
        context_lines: int,
        file_pattern: Optional[str],
        fuzzy: bool,
        progress: SearchProgress,
        progress_callback: Optional[Callable[[SearchProgress], None]],
        cancellation_token: Optional[asyncio.Event]
    ) -> Dict[str, List[Tuple[int, str]]]:
        """Execute ripgrep search asynchronously."""
        
        # Build command
        cmd = ['rg', '--line-number', '--no-heading', '--color=never', '--threads', str(self.max_workers)]
        
        if not case_sensitive:
            cmd.append('--ignore-case')
        
        # Prepare search pattern
        search_pattern = pattern
        if fuzzy:
            search_pattern = create_safe_fuzzy_pattern(pattern)
        else:
            cmd.append('--fixed-strings')
        
        if context_lines > 0:
            cmd.extend(['--context', str(context_lines)])
        
        if file_pattern:
            cmd.extend(['--glob', file_pattern])
        
        cmd.append('--')
        cmd.append(search_pattern)
        cmd.append(base_path)
        
        # Execute command in thread pool
        loop = asyncio.get_event_loop()
        
        def run_ripgrep():
            if cancellation_token and cancellation_token.is_set():
                raise asyncio.CancelledError("Operation was cancelled")
            
            try:
                process = subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    encoding='utf-8',
                    errors='replace',
                    check=False
                )
                
                if process.returncode > 1:
                    raise RuntimeError(f"ripgrep failed with exit code {process.returncode}: {process.stderr}")
                
                return process.stdout
            except FileNotFoundError:
                raise RuntimeError("ripgrep (rg) not found. Please install it and ensure it's in your PATH.")
        
        # Run in executor
        output = await loop.run_in_executor(self._executor, run_ripgrep)
        
        # Parse results
        results = parse_search_output(output, base_path)
        
        # Update progress
        progress.files_searched = len(results)
        progress.total_files = len(results)  # We don't know total files ahead of time for ripgrep
        
        if progress_callback:
            progress_callback(progress)
        
        return results


class AsyncSearchManager:
    """Manager for async search operations."""
    
    def __init__(self):
        self._strategies: Dict[str, AsyncSearchStrategy] = {}
        self._default_strategy: Optional[AsyncSearchStrategy] = None
        self._initialize_strategies()
    
    def _initialize_strategies(self):
        """Initialize available search strategies."""
        # Add ripgrep strategy
        ripgrep_strategy = AsyncRipgrepStrategy()
        if ripgrep_strategy.is_available():
            self._strategies['ripgrep'] = ripgrep_strategy
            if self._default_strategy is None:
                self._default_strategy = ripgrep_strategy
        
        # Add more strategies as needed
        # TODO: Add AsyncUgrepStrategy, AsyncAgStrategy, etc.
    
    def get_strategy(self, name: Optional[str] = None) -> Optional[AsyncSearchStrategy]:
        """Get a search strategy by name or default."""
        if name and name in self._strategies:
            return self._strategies[name]
        return self._default_strategy
    
    def get_available_strategies(self) -> List[str]:
        """Get list of available strategy names."""
        return list(self._strategies.keys())
    
    async def search_async(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
        strategy_name: Optional[str] = None,
        progress_callback: Optional[Callable[[SearchProgress], None]] = None,
        cancellation_token: Optional[asyncio.Event] = None
    ) -> Dict[str, List[Tuple[int, str]]]:
        """Perform async search using specified or default strategy."""
        
        strategy = self.get_strategy(strategy_name)
        if not strategy:
            raise RuntimeError(f"No search strategy available{f' named {strategy_name}' if strategy_name else ''}")
        
        return await strategy.search_async(
            pattern, base_path, case_sensitive, context_lines,
            file_pattern, fuzzy, progress_callback, cancellation_token
        )
    
    async def search_multiple_async(
        self,
        patterns: List[str],
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
        strategy_name: Optional[str] = None,
        progress_callback: Optional[Callable[[str, SearchProgress], None]] = None,
        cancellation_token: Optional[asyncio.Event] = None
    ) -> Dict[str, Dict[str, List[Tuple[int, str]]]]:
        """Perform multiple async searches concurrently."""
        
        strategy = self.get_strategy(strategy_name)
        if not strategy:
            raise RuntimeError(f"No search strategy available{f' named {strategy_name}' if strategy_name else ''}")
        
        return await strategy.search_multiple_async(
            patterns, base_path, case_sensitive, context_lines,
            file_pattern, fuzzy, progress_callback, cancellation_token
        )
    
    def shutdown(self):
        """Shutdown all strategies."""
        for strategy in self._strategies.values():
            strategy.shutdown()


# Global async search manager
async_search_manager = AsyncSearchManager()
