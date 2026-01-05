"""
Basic, pure-Python search strategy.
"""
import os
import re
from typing import Dict, List, Optional, Tuple

from .base import SearchStrategy, create_safe_fuzzy_pattern

class BasicSearchStrategy(SearchStrategy):
    """
    A basic, pure-Python search strategy.

    This strategy iterates through files and lines manually. It's a fallback
    for when no advanced command-line search tools are available.
    It does not support context lines.
    """

    @property
    def name(self) -> str:
        """The name of the search tool."""
        return 'basic'

    def is_available(self) -> bool:
        """This basic strategy is always available."""
        return True

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
        Execute a basic, line-by-line search.

        Note: This implementation does not support context_lines.
        Fuzzy searching uses the shared create_safe_fuzzy_pattern function.
        """
        results: Dict[str, List[Tuple[int, str]]] = {}
        
        flags = 0 if case_sensitive else re.IGNORECASE
        
        if fuzzy:
            # Use the shared safe fuzzy pattern function
            search_pattern = create_safe_fuzzy_pattern(pattern)
            search_regex = re.compile(search_pattern, flags)
        else:
            search_regex = re.compile(pattern, flags)

        for root, _, files in os.walk(base_path):
            for file in files:
                # Basic file pattern matching (not full glob support)
                if file_pattern and not file.endswith(file_pattern.replace('*', '')):
                    continue

                file_path = os.path.join(root, file)
                rel_path = os.path.relpath(file_path, base_path)
                
                try:
                    file_size = os.path.getsize(file_path)
                    # For large files (>10MB), use chunked reading for memory efficiency
                    if file_size > 10 * 1024 * 1024:  # 10MB threshold
                        matches = self._search_file_chunked(file_path, search_regex)
                        if matches:
                            results[rel_path] = matches
                    else:
                        # For smaller files, use standard line-by-line reading
                        with open(file_path, 'r', encoding='utf-8', errors='ignore') as f:
                            for line_num, line in enumerate(f, 1):
                                if search_regex.search(line):
                                    if rel_path not in results:
                                        results[rel_path] = []
                                    # Strip newline for consistent output
                                    results[rel_path].append((line_num, line.rstrip('\\n')))
                except Exception:
                    # Ignore files that can't be opened or read
                    continue
        
        return results
    
    def _search_file_chunked(self, file_path: str, search_regex) -> List[Tuple[int, str]]:
        """Search large files using chunked reading for memory efficiency."""
        matches = []
        line_number = 1
        buffer = ""
        chunk_size = 4 * 1024 * 1024  # 4MB chunks
        
        try:
            with open(file_path, 'r', encoding='utf-8', errors='ignore') as f:
                while True:
                    chunk = f.read(chunk_size)
                    if not chunk:
                        break
                    
                    # Add chunk to buffer
                    buffer += chunk
                    
                    # Process complete lines from buffer
                    lines = buffer.split('\n')
                    
                    # Keep the last incomplete line in buffer
                    buffer = lines[-1]
                    
                    # Process complete lines
                    for line in lines[:-1]:
                        if search_regex.search(line):
                            matches.append((line_number, line.rstrip()))
                        line_number += 1
                
                # Process the last line if there's remaining data
                if buffer:
                    if search_regex.search(buffer):
                        matches.append((line_number, buffer.rstrip()))
                        
        except Exception as e:
            print(f"Error in chunked search for {file_path}: {e}")
            return []
        
        return matches
