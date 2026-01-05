"""
Search Strategies for LeIndex

This module defines the abstract base class for search strategies and will contain
concrete implementations for different search tools like ugrep, ripgrep, etc.
"""
import os
import re
import sys
import asyncio
from abc import ABC, abstractmethod
from typing import Dict, List, Optional, Tuple, Callable

def parse_search_output(output: str, base_path: str) -> Dict[str, List[Tuple[int, str]]]:
    """
    Parse the output of command-line search tools (grep, ag, rg).

    Args:
        output: The raw output from the command-line tool.
        base_path: The base path of the project to make file paths relative.

    Returns:
        A dictionary where keys are file paths and values are lists of (line_number, line_content) tuples.
    """
    results = {}
    # Normalize base_path to ensure consistent path separation
    normalized_base_path = os.path.normpath(base_path)

    for line in output.strip().split('\n'):
        if not line.strip():
            continue
        try:
            # Handle Windows paths which might have a drive letter, e.g., C:
            parts = line.split(':', 2)
            if sys.platform == "win32" and len(parts[0]) == 1 and parts[1].startswith('\\'):
                # Re-join drive letter with the rest of the path
                file_path_abs = f"{parts[0]}:{parts[1]}"
                line_number_str = parts[2].split(':', 1)[0]
                content = parts[2].split(':', 1)[1]
            else:
                file_path_abs = parts[0]
                line_number_str = parts[1]
                content = parts[2]
            
            line_number = int(line_number_str)

            # Make the file path relative to the base_path
            relative_path = os.path.relpath(file_path_abs, normalized_base_path)
            
            # Normalize path separators for consistency
            relative_path = relative_path.replace('\\', '/')

            if relative_path not in results:
                results[relative_path] = []
            results[relative_path].append((line_number, content))
        except (ValueError, IndexError):
            # Silently ignore lines that don't match the expected format
            # This can happen with summary lines or other tool-specific output
            pass

    return results


def create_safe_fuzzy_pattern(pattern: str) -> str:
    """
    Create safe fuzzy search patterns that are more permissive than exact match
    but still safe from regex injection attacks.
    
    Args:
        pattern: Original search pattern
        
    Returns:
        Safe fuzzy pattern for extended regex
    """
    # Escape any regex special characters to make them literal
    escaped = re.escape(pattern)
    
    # Create fuzzy pattern that matches:
    # 1. Word at start of word boundary (e.g., "test" in "testing")
    # 2. Word at end of word boundary (e.g., "test" in "mytest") 
    # 3. Whole word (e.g., "test" as standalone word)
    if len(pattern) >= 3:  # Only for patterns of reasonable length
        # This pattern allows partial matches at word boundaries
        fuzzy_pattern = f"\\b{escaped}|{escaped}\\b"
    else:
        # For short patterns, require full word boundaries to avoid too many matches
        fuzzy_pattern = f"\\b{escaped}\\b"
    
    return fuzzy_pattern


class SearchStrategy(ABC):
    """
    Abstract base class for a search strategy.
    
    Each strategy is responsible for searching code using a specific tool or method.
    """

    @property
    @abstractmethod
    def name(self) -> str:
        """The name of the search tool (e.g., 'ugrep', 'ripgrep')."""
        pass

    @abstractmethod
    def is_available(self) -> bool:
        """
        Check if the search tool for this strategy is available on the system.
        
        Returns:
            True if the tool is available, False otherwise.
        """
        pass

    @abstractmethod
    def search(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False
    ) -> Dict[str, List[Tuple[int, str]]]:
        """
        Execute a search using the specific strategy.

        Args:
            pattern: The search pattern (string or regex).
            base_path: The root directory to search in.
            case_sensitive: Whether the search is case-sensitive.
            context_lines: Number of context lines to show around each match.
            file_pattern: Glob pattern to filter files (e.g., "*.py").
            fuzzy: Whether to enable fuzzy search.

        Returns:
            A dictionary mapping filenames to lists of (line_number, line_content) tuples.
        """
        pass

    def search_multiple(
        self,
        patterns: List[str],
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
        scope_path: Optional[str] = None
    ) -> Dict[str, Dict[str, List[Tuple[int, str]]]]:
        """
        Execute concurrent searches for multiple patterns.

        Args:
            patterns: List of search patterns.
            base_path: The root directory to search in.
            case_sensitive: Whether the search is case-sensitive.
            context_lines: Number of context lines to show around each match.
            file_pattern: Glob pattern to filter files (e.g., "*.py").
            fuzzy: Whether to enable fuzzy search.
            scope_path: Optional subdirectory to limit search scope.

        Returns:
            A dictionary mapping pattern to search results.
        """
        import concurrent.futures
        
        # Determine search path
        search_path = os.path.join(base_path, scope_path) if scope_path else base_path
        
        # Use ThreadPoolExecutor for concurrent searches
        results = {}
        with concurrent.futures.ThreadPoolExecutor(max_workers=min(len(patterns), os.cpu_count() or 1)) as executor:
            # Submit all search tasks
            future_to_pattern = {
                executor.submit(
                    self.search,
                    pattern,
                    search_path,
                    case_sensitive,
                    context_lines,
                    file_pattern,
                    fuzzy
                ): pattern
                for pattern in patterns
            }
            
            # Collect results as they complete
            for future in concurrent.futures.as_completed(future_to_pattern):
                pattern = future_to_pattern[future]
                try:
                    result = future.result()
                    results[pattern] = result
                except Exception as e:
                    results[pattern] = {"error": f"Search failed for pattern '{pattern}': {str(e)}"}
        
        return results
    
    # Async methods for non-blocking search operations
    
    async def search_async(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
        progress_callback: Optional[Callable[[float], None]] = None
    ) -> Dict[str, List[Tuple[int, str]]]:
        """
        Execute an async search using the specific strategy.

        CRITICAL FIX: Use asyncio.to_thread() for Python 3.9+ instead of deprecated
        asyncio.get_event_loop().run_in_executor().

        Args:
            pattern: The search pattern (string or regex).
            base_path: The root directory to search in.
            case_sensitive: Whether the search is case-sensitive.
            context_lines: Number of context lines to show around each match.
            file_pattern: Glob pattern to filter files (e.g., "*.py").
            fuzzy: Whether to enable fuzzy search.
            progress_callback: Optional callback for progress updates.

        Returns:
            A dictionary mapping filenames to lists of (line_number, line_content) tuples.
        """
        # CRITICAL FIX: Use asyncio.to_thread() for Python 3.9+ instead of deprecated
        # asyncio.get_event_loop().run_in_executor(). This is the recommended way
        # to run synchronous I/O-bound functions in async code starting with Python 3.9.
        def run_search():
            if progress_callback:
                progress_callback(0.0)

            result = self.search(
                pattern, base_path, case_sensitive, context_lines, file_pattern, fuzzy
            )

            if progress_callback:
                progress_callback(1.0)

            return result

        # Use asyncio.to_thread() for cleaner async execution
        # This is preferred over loop.run_in_executor() for Python 3.9+
        return await asyncio.to_thread(run_search)
    
    async def search_multiple_async(
        self,
        patterns: List[str],
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
        scope_path: Optional[str] = None,
        progress_callback: Optional[Callable[[str, float], None]] = None
    ) -> Dict[str, Dict[str, List[Tuple[int, str]]]]:
        """
        Execute concurrent async searches for multiple patterns.
        
        Args:
            patterns: List of search patterns.
            base_path: The root directory to search in.
            case_sensitive: Whether the search is case-sensitive.
            context_lines: Number of context lines to show around each match.
            file_pattern: Glob pattern to filter files (e.g., "*.py").
            fuzzy: Whether to enable fuzzy search.
            scope_path: Optional subdirectory to limit search scope.
            progress_callback: Optional callback for progress updates per pattern.
            
        Returns:
            A dictionary mapping pattern to search results.
        """
        # Determine search path
        search_path = os.path.join(base_path, scope_path) if scope_path else base_path
        
        # Create tasks for all patterns
        async def search_single_pattern(pattern: str) -> Tuple[str, Dict[str, List[Tuple[int, str]]]]:
            def single_progress_callback(progress: float):
                if progress_callback:
                    progress_callback(pattern, progress)
            
            try:
                result = await self.search_async(
                    pattern, search_path, case_sensitive, context_lines,
                    file_pattern, fuzzy, single_progress_callback
                )
                return pattern, result
            except Exception as e:
                return pattern, {"error": f"Search failed for pattern '{pattern}': {str(e)}"}
        
        # Execute all searches concurrently
        tasks = [search_single_pattern(pattern) for pattern in patterns]
        results = {}
        
        for task in asyncio.as_completed(tasks):
            pattern, result = await task
            results[pattern] = result
        
        return results

