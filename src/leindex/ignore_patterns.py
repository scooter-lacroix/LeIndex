"""
Ignore Patterns Module

This module provides functionality for loading and processing ignore patterns
from .gitignore and .ignore files, combined with default exclude patterns.
"""
import os
import re
from typing import List
from pathlib import Path


class IgnorePatternMatcher:
    """A class for matching file paths against ignore patterns."""
    
    # Default exclude patterns that should always be ignored
    DEFAULT_EXCLUDES = {
        # Version control
        '.git', '.svn', '.hg', '.bzr',
        # Virtual environments
        'venv', 'env', 'ENV', '.venv', '.env',
        # Python cache
        '__pycache__', '*.pyc', '*.pyo', '*.pyd', '.Python',
        # Build directories
        'build', 'dist', 'target', 'out', 'bin',
        # IDE and editor files
        '.vscode', '.idea', '.vs', '*.swp', '*.swo', '*~',
        # OS specific
        '.DS_Store', 'Thumbs.db', 'desktop.ini',
        # Documentation builds (but not docs/ itself)
        'docs/_build', 'docs/build', '_build',
        # Logs and temporary files
        '*.log', '*.tmp', 'tmp', 'temp',
        # Coverage reports
        'htmlcov', '.coverage', '.pytest_cache',
        # Package files
        '*.egg-info', '.eggs',
    }
    
    def __init__(self, base_path: str):
        """Initialize the ignore pattern matcher.
        
        Args:
            base_path: The base path of the project
        """
        self.base_path = Path(base_path).resolve()
        self.patterns: List[str] = []
        self.compiled_patterns: List[re.Pattern] = []
        
        # Load patterns from various sources
        self._load_default_patterns()
        self._load_gitignore_patterns()
        self._load_ignore_patterns()
        
        # Compile patterns for better performance
        self._compile_patterns()
    
    def _load_default_patterns(self):
        """Load default exclude patterns."""
        self.patterns.extend(self.DEFAULT_EXCLUDES)
    
    def _load_gitignore_patterns(self):
        """Load patterns from .gitignore file."""
        gitignore_path = self.base_path / '.gitignore'
        if gitignore_path.exists():
            try:
                with open(gitignore_path, 'r', encoding='utf-8') as f:
                    for line in f:
                        line = line.strip()
                        if line and not line.startswith('#'):
                            self.patterns.append(line)
            except Exception as e:
                print(f"Warning: Could not read .gitignore file: {e}")
    
    def _load_ignore_patterns(self):
        """Load patterns from .ignore file."""
        ignore_path = self.base_path / '.ignore'
        if ignore_path.exists():
            try:
                with open(ignore_path, 'r', encoding='utf-8') as f:
                    for line in f:
                        line = line.strip()
                        if line and not line.startswith('#'):
                            self.patterns.append(line)
            except Exception as e:
                print(f"Warning: Could not read .ignore file: {e}")
    
    def _compile_patterns(self):
        """Compile gitignore patterns to regex patterns for better performance."""
        self.compiled_patterns = []
        
        for pattern in self.patterns:
            # Skip empty patterns
            if not pattern:
                continue
                
            # Handle negation patterns (starting with !)
            negated = pattern.startswith('!')
            if negated:
                pattern = pattern[1:]
            
            # Convert gitignore pattern to regex
            regex_pattern = self._gitignore_to_regex(pattern)
            
            try:
                compiled = re.compile(regex_pattern, re.IGNORECASE)
                self.compiled_patterns.append({
                    'pattern': compiled,
                    'negated': negated,
                    'original': pattern
                })
            except re.error as e:
                print(f"Warning: Invalid regex pattern '{pattern}': {e}")
    
    def _gitignore_to_regex(self, pattern: str) -> str:
        """Convert a gitignore pattern to a regex pattern.
        
        Args:
            pattern: The gitignore pattern
            
        Returns:
            A regex pattern string
        """
        # Handle directory patterns (ending with /)
        is_dir_pattern = pattern.endswith('/')
        if is_dir_pattern:
            pattern = pattern[:-1]
        
        # Handle patterns starting with /
        if pattern.startswith('/'):
            pattern = pattern[1:]
            anchor_start = True
        else:
            anchor_start = False
        
        # Escape special regex characters except for * and ?
        pattern = re.escape(pattern)
        
        # Convert gitignore wildcards to regex
        pattern = pattern.replace(r'\*\*', '.*')  # ** matches any number of directories
        pattern = pattern.replace(r'\*', '[^/]*')  # * matches any characters except /
        pattern = pattern.replace(r'\?', '[^/]')   # ? matches any single character except /
        
        # Build the final regex
        if anchor_start:
            # Pattern is anchored to project root
            regex = f'^{pattern}'
        else:
            # Pattern can match anywhere
            regex = f'(^|/){pattern}'
        
        if is_dir_pattern:
            regex += '(/|$)'
        else:
            regex += '(/.*)?$'
        
        return regex
    
    def should_ignore(self, path: str) -> bool:
        """Check if a path should be ignored based on the loaded patterns.
        
        Args:
            path: The path to check (relative to base_path)
            
        Returns:
            True if the path should be ignored, False otherwise
        """
        # Normalize the path
        path = path.replace('\\', '/')
        if path.startswith('./'):
            path = path[2:]
        
        # Check against compiled patterns
        should_ignore = False
        
        for pattern_info in self.compiled_patterns:
            pattern = pattern_info['pattern']
            negated = pattern_info['negated']
            
            if pattern.search(path):
                should_ignore = not negated
        
        return should_ignore
    
    def should_ignore_directory(self, dir_path: str) -> bool:
        """Check if a directory should be ignored.
        
        This is a specialized check for directories that can help optimize
        directory traversal by skipping entire directory trees.
        
        Args:
            dir_path: The directory path to check (relative to base_path)
            
        Returns:
            True if the directory should be ignored, False otherwise
        """
        # Check if the directory itself should be ignored
        if self.should_ignore(dir_path):
            return True
        
        # Check if it's a common directory that should be ignored
        dir_name = os.path.basename(dir_path)
        
        # Common directories to ignore
        ignore_dirs = {
            '.git', '.svn', '.hg', '.bzr',
            '__pycache__', '.pytest_cache',
            'venv', 'env', 'ENV', '.venv', '.env',
            'build', 'dist', 'target', 'out',
            '.vscode', '.idea', '.vs',
            'htmlcov', '.coverage', '.eggs',
            'docs/_build', 'docs/build', '_build'
        }
        
        if dir_name in ignore_dirs:
            return True
        
        # Check if directory starts with a dot (hidden directories)
        if dir_name.startswith('.') and dir_name not in {'.', '..'}:
            # Allow some common dotfiles/directories that might contain code
            allowed_dotdirs = {'.github', '.vscode', '.config'}
            if dir_name not in allowed_dotdirs:
                return True
        
        return False
    
    def get_patterns(self) -> List[str]:
        """Get all loaded patterns.
        
        Returns:
            List of all patterns that were loaded
        """
        return self.patterns.copy()
    
    def get_pattern_sources(self) -> dict:
        """Get information about pattern sources.
        
        Returns:
            Dictionary with information about which files were loaded
        """
        sources = {
            'default_patterns': len(self.DEFAULT_EXCLUDES),
            'gitignore_exists': (self.base_path / '.gitignore').exists(),
            'ignore_exists': (self.base_path / '.ignore').exists(),
            'total_patterns': len(self.patterns),
            'compiled_patterns': len(self.compiled_patterns)
        }
        return sources
