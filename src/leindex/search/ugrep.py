"""
Search Strategy for ugrep
"""
import os
import shutil
import subprocess
from typing import Dict, List, Optional, Tuple

from .base import SearchStrategy, parse_search_output

class UgrepStrategy(SearchStrategy):
    """Search strategy using the 'ugrep' (ug) command-line tool."""

    @property
    def name(self) -> str:
        """The name of the search tool."""
        return 'ugrep'

    def is_available(self) -> bool:
        """Check if 'ug' command is available on the system."""
        return shutil.which('ug') is not None

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
        Execute a search using the 'ug' command-line tool.
        """
        if not self.is_available():
            return {"error": "ugrep (ug) command not found."}

        cmd = ['ug', '--line-number', '--no-heading', '--threads', str(os.cpu_count() or 1)]

        if fuzzy:
            cmd.append('--fuzzy') # Enable fuzzy search (long form for clarity)
        else:
            cmd.append('--fixed-strings') # Use fixed-strings for non-fuzzy search

        if not case_sensitive:
            cmd.append('--ignore-case')
        
        if context_lines > 0:
            cmd.extend(['-A', str(context_lines), '-B', str(context_lines)])
            
        if file_pattern:
            cmd.extend(['--include', file_pattern])  # Correct parameter for file patterns

        # Add '--' to treat pattern as a literal argument, preventing injection
        cmd.append('--')
        cmd.append(pattern)
        cmd.append(base_path)

        try:
            process = subprocess.run(
                cmd,
                capture_output=True,
                text=True,
                encoding='utf-8',
                errors='ignore', # Ignore decoding errors for binary-like content
                check=False  # Do not raise exception on non-zero exit codes
            )
            
            # ugrep exits with 1 if no matches are found, which is not an error for us.
            # It exits with 2 for actual errors.
            if process.returncode > 1:
                error_output = process.stderr.strip()
                return {"error": f"ugrep execution failed with code {process.returncode}", "details": error_output}

            return parse_search_output(process.stdout, base_path)

        except FileNotFoundError:
            return {"error": "ugrep (ug) command not found. Please ensure it's installed and in your PATH."}
        except Exception as e:
            return {"error": f"An unexpected error occurred during search: {str(e)}"}
