"""
Search Strategy for standard grep
"""
import shutil
import subprocess
from typing import Dict, List, Optional, Tuple

from .base import SearchStrategy, parse_search_output, create_safe_fuzzy_pattern

class GrepStrategy(SearchStrategy):
    """
    Search strategy using the standard 'grep' command-line tool.
    
    This is intended as a fallback for when more advanced tools like
    ugrep, ripgrep, or ag are not available.
    """

    @property
    def name(self) -> str:
        """The name of the search tool."""
        return 'grep'

    def is_available(self) -> bool:
        """Check if 'grep' command is available on the system."""
        return shutil.which('grep') is not None

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
        Execute a search using standard grep.

        Note: grep does not support native fuzzy searching. When fuzzy=True, an
        Extended Regular Expression (ERE) search is performed with safe fuzzy pattern.
        When fuzzy=False, a literal string search is performed (-F).
        """
        # -r: recursive, -n: line number
        cmd = ['grep', '-r', '-n']

        # Prepare search pattern
        search_pattern = pattern
        if not fuzzy:
            cmd.append('-F')  # Fixed strings, literal search
        else:
            cmd.append('-E')  # Extended Regular Expressions
            search_pattern = create_safe_fuzzy_pattern(pattern)

        if not case_sensitive:
            cmd.append('-i')

        if context_lines > 0:
            cmd.extend(['-A', str(context_lines)])
            cmd.extend(['-B', str(context_lines)])
            
        if file_pattern:
            # Note: grep's --include uses glob patterns, not regex
            cmd.append(f'--include={file_pattern}')

        # Add -- to treat pattern as a literal argument, preventing injection
        cmd.append('--')
        cmd.append(search_pattern)
        cmd.append(base_path)
        
        try:
            # grep exits with 1 if no matches are found, which is not an error.
            # It exits with 0 on success (match found). >1 for errors.
            process = subprocess.run(
                cmd, 
                capture_output=True, 
                text=True, 
                encoding='utf-8',
                errors='replace',
                check=False
            )
            
            if process.returncode > 1:
                 raise RuntimeError(f"grep failed with exit code {process.returncode}: {process.stderr}")

            return parse_search_output(process.stdout, base_path)
        
        except FileNotFoundError:
            raise RuntimeError("'grep' not found. Please install it and ensure it's in your PATH.")
        except Exception as e:
            raise RuntimeError(f"An error occurred while running grep: {e}") 