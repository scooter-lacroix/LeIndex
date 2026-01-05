"""
Search Strategy for The Silver Searcher (ag)
"""
import shutil
import subprocess
from typing import Dict, List, Optional, Tuple

from .base import SearchStrategy, parse_search_output, create_safe_fuzzy_pattern

class AgStrategy(SearchStrategy):
    """Search strategy using 'The Silver Searcher' (ag) command-line tool."""

    @property
    def name(self) -> str:
        """The name of the search tool."""
        return 'ag'

    def is_available(self) -> bool:
        """Check if 'ag' command is available on the system."""
        return shutil.which('ag') is not None

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
        Execute a search using The Silver Searcher (ag).

        Note: ag does not support native fuzzy searching. When fuzzy=True, a
        safe fuzzy pattern with word boundaries is used for regex search.
        When fuzzy=False, a literal string search is performed.
        """
        # ag prints line numbers and groups by file by default, which is good.
        # --noheading is used to be consistent with other tools' output format.
        cmd = ['ag', '--noheading']

        if not case_sensitive:
            cmd.append('--ignore-case')

        # Prepare search pattern
        search_pattern = pattern
        if fuzzy:
            # Use safe fuzzy pattern for regex search
            search_pattern = create_safe_fuzzy_pattern(pattern)
        else:
            cmd.append('--literal') # or -Q

        if context_lines > 0:
            cmd.extend(['--before', str(context_lines)])
            cmd.extend(['--after', str(context_lines)])
            
        if file_pattern:
            # Use -G to filter files by regex pattern
            cmd.extend(['-G', file_pattern])

        # Add -- to treat pattern as a literal argument, preventing injection
        cmd.append('--')
        cmd.append(search_pattern)
        cmd.append(base_path)
        
        try:
            # ag exits with 1 if no matches are found, which is not an error.
            # It exits with 0 on success (match found). Other codes are errors.
            process = subprocess.run(
                cmd, 
                capture_output=True, 
                text=True, 
                encoding='utf-8',
                errors='replace',
                check=False  # Do not raise CalledProcessError on non-zero exit
            )
            # We don't check returncode > 1 because ag's exit code behavior
            # is less standardized than rg/ug. 0 for match, 1 for no match.
            # Any actual error will likely raise an exception or be in stderr.
            if process.returncode > 1:
                 raise RuntimeError(f"ag failed with exit code {process.returncode}: {process.stderr}")

            return parse_search_output(process.stdout, base_path)
        
        except FileNotFoundError:
            raise RuntimeError("'ag' (The Silver Searcher) not found. Please install it and ensure it's in your PATH.")
        except Exception as e:
            # Re-raise other potential exceptions like permission errors
            raise RuntimeError(f"An error occurred while running ag: {e}") 