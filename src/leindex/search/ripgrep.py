"""
Search Strategy for ripgrep
"""
import os
import shutil
import subprocess
from typing import Dict, List, Optional, Tuple

from .base import SearchStrategy, parse_search_output, create_safe_fuzzy_pattern

class RipgrepStrategy(SearchStrategy):
    """Search strategy using the 'ripgrep' (rg) command-line tool."""

    @property
    def name(self) -> str:
        """The name of the search tool."""
        return 'ripgrep'

    def is_available(self) -> bool:
        """Check if 'rg' command is available on the system."""
        return shutil.which('rg') is not None

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
        Execute a search using ripgrep.
        
        Note: ripgrep does not support native fuzzy searching. When fuzzy=True, a
        safe fuzzy pattern with word boundaries is used for regex search.
        When fuzzy=False, a literal string search is performed with --fixed-strings.
        """
        cmd = ['rg', '--line-number', '--no-heading', '--color=never', '--threads', str(os.cpu_count())]

        if not case_sensitive:
            cmd.append('--ignore-case')

        # Prepare search pattern
        search_pattern = pattern
        if fuzzy:
            # Use safe fuzzy pattern for regex search
            search_pattern = create_safe_fuzzy_pattern(pattern)
        else:
            cmd.append('--fixed-strings')

        if context_lines > 0:
            cmd.extend(['--context', str(context_lines)])
            
        if file_pattern:
            cmd.extend(['--glob', file_pattern])

        # Add -- to treat pattern as a literal argument, preventing injection
        cmd.append('--')
        cmd.append(search_pattern)
        cmd.append(base_path)
        
        try:
            # ripgrep exits with 1 if no matches are found, which is not an error.
            # It exits with 2 for actual errors.
            process = subprocess.run(
                cmd, 
                capture_output=True, 
                text=True, 
                encoding='utf-8',
                errors='replace',
                check=False  # Do not raise CalledProcessError on non-zero exit
            )
            if process.returncode > 1:
                raise RuntimeError(f"ripgrep failed with exit code {process.returncode}: {process.stderr}")

            return parse_search_output(process.stdout, base_path)
        
        except FileNotFoundError:
            raise RuntimeError("ripgrep (rg) not found. Please install it and ensure it's in your PATH.")
        except Exception as e:
            # Re-raise other potential exceptions like permission errors
            raise RuntimeError(f"An error occurred while running ripgrep: {e}") 