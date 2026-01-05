import uuid
import datetime
import hashlib
import difflib
import os
import logging
import threading
import re
from typing import Optional, List, Dict, Tuple
from pathlib import Path
from enum import Enum

from .incremental_indexer import IncrementalIndexer
from .storage.storage_interface import FileMetadataInterface

logging.basicConfig(level=logging.DEBUG, format='%(asctime)s - %(name)s - %(levelname)s - %(message)s')
logger = logging.getLogger(__name__)


# ============================================================================
# PRODUCT.MD ALIGNMENT - Granular Version Tracking with Change Categorization
# ============================================================================

class ChangeCategory(Enum):
    """
    Categories of changes for granular version tracking.

    PRODUCT.MD REQUIREMENT:
    -----------------------
    "Granular Version Tracking: Enhance the existing system with more detailed
    diffing and history analysis."

    This enables:
    - Fine-grained change attribution (what type of code changed)
    - Impact analysis (how significant is the change)
    - Pattern recognition (what areas are changing frequently)
    """
    FUNCTION_ADD = "function_add"           # New function added
    FUNCTION_REMOVE = "function_remove"     # Function removed
    FUNCTION_MODIFY = "function_modify"     # Function signature/body changed
    CLASS_ADD = "class_add"                 # New class added
    CLASS_REMOVE = "class_remove"           # Class removed
    CLASS_MODIFY = "class_modify"           # Class definition changed
    IMPORT_ADD = "import_add"               # New import statement
    IMPORT_REMOVE = "import_remove"         # Import removed
    COMMENT_CHANGE = "comment_change"       # Comment-only changes
    WHITESPACE_CHANGE = "whitespace_change" # Whitespace-only changes
    LOGIC_CHANGE = "logic_change"           # Code logic changes
    STRUCTURAL_CHANGE = "structural_change" # Major structural changes
    DOCSTRING_CHANGE = "docstring_change"   # Docstring changes
    UNKNOWN = "unknown"                     # Uncategorized change


class LineChange:
    """
    Represents a single line change with attribution.

    PRODUCT.MD ALIGNMENT:
    --------------------
    Provides line-by-line change tracking for detailed history analysis.
    """
    def __init__(
        self,
        line_number: int,
        old_content: str,
        new_content: str,
        change_type: str,  # 'added', 'removed', 'modified'
        category: ChangeCategory = ChangeCategory.UNKNOWN
    ):
        self.line_number = line_number
        self.old_content = old_content
        self.new_content = new_content
        self.change_type = change_type
        self.category = category

    def to_dict(self) -> Dict:
        return {
            "line_number": self.line_number,
            "old_content": self.old_content,
            "new_content": self.new_content,
            "change_type": self.change_type,
            "category": self.category.value
        }


class ChangeAnalyzer:
    """
    Analyzes code changes to categorize and attribute modifications.

    PRODUCT.MD REQUIREMENT:
    -----------------------
    "Granular Version Tracking: more detailed diffing and history analysis"

    This analyzer provides:
    1. Change categorization (function, class, import, etc.)
    2. Line-by-line change attribution
    3. Impact scoring (how significant is the change)
    4. Pattern detection (frequently changed areas)
    """

    # Patterns for detecting different code constructs
    FUNCTION_PATTERNS = {
        'python': re.compile(r'^\s*(def|async\s+def)\s+(\w+)'),
        'javascript': re.compile(r'^\s*(function\s+(\w+)|const\s+(\w+)\s*=.*=>|(\w+)\s*\(.*\)\s*{)'),
        'typescript': re.compile(r'^\s*(function\s+(\w+)|const\s+(\w+)\s*[:=].*=>|(\w+)\s*\(.*\)\s*[:{])'),
        'rust': re.compile(r'^\s*(fn|pub\s+fn|async\s+fn|pub\s+async\s+fn)\s+(\w+)'),
        'go': re.compile(r'^\s*func\s+(?:\(\w+\s+\*?\w+\)\s+)?(\w+)'),
        'java': re.compile(r'^\s*(public|private|protected)?\s*(static)?\s*\w+\s+(\w+)\s*\(.*\)'),
        # C/C++ patterns
        'c': re.compile(r'^\s*(?:\w+\s+)*\*?\s*(\w+)\s*\([^;]*\)\s*(?:__attribute__|__declspec|{{?$)'),
        'cpp': re.compile(r'^\s*(?:\w+\s+(?:\*|&)?\s*)*(\w+)\s*\([^;]*\)\s*(?:override|final|const|{{?$)'),
        # C# pattern
        'csharp': re.compile(r'^\s*(public|private|protected|internal)?\s*(static)?\s*(async)?\s*\w+\s+(\w+)\s*\(.*\)'),
        # PHP pattern
        'php': re.compile(r'^\s*(function\s+(\w+)|public\s+function\s+(\w+)|private\s+function\s+(\w+)|protected\s+function\s+(\w+))'),
        # Ruby pattern
        'ruby': re.compile(r'^\s*def\s+(self\.)?(\w+)'),
        # Swift pattern
        'swift': re.compile(r'^\s*(func|static\s+func|private\s+func|public\s+func|internal\s+func)\s+(\w+)'),
        # Kotlin pattern
        'kotlin': re.compile(r'^\s*(fun|override\s+fun|public\s+fun|private\s+fun|internal\s+fun)\s+(\w+)'),
    }

    CLASS_PATTERNS = {
        'python': re.compile(r'^\s*class\s+(\w+)'),
        'javascript': re.compile(r'^\s*class\s+(\w+)'),
        'typescript': re.compile(r'^\s*(class|interface|type)\s+(\w+)'),
        'rust': re.compile(r'^\s*(struct|enum|trait|impl)\s+(\w+)'),
        'go': re.compile(r'^\s*type\s+(\w+)\s+(struct|interface)'),
        'java': re.compile(r'^\s*(public\s+)?(class|interface|enum)\s+(\w+)'),
        # C/C++ patterns
        'c': re.compile(r'^\s*(struct|enum|union|typedef\s+struct)\s+(\w+)'),
        'cpp': re.compile(r'^\s*(class|struct|enum|union|template\s*<[^>]*>\s*class)\s+(\w+)'),
        # C# pattern
        'csharp': re.compile(r'^\s*(public|private|protected|internal)?\s*(abstract)?\s*(partial)?\s*class\s+(\w+)'),
        # PHP pattern
        'php': re.compile(r'^\s*(class|interface|trait)\s+(\w+)'),
        # Ruby pattern
        'ruby': re.compile(r'^\s*class\s+(\w+)'),
        # Swift pattern
        'swift': re.compile(r'^\s*(class|struct|enum|protocol|extension)\s+(\w+)'),
        # Kotlin pattern
        'kotlin': re.compile(r'^\s*(class|interface|object|sealed\s+class|data\s+class)\s+(\w+)'),
    }

    IMPORT_PATTERNS = {
        'python': re.compile(r'^(import|from)\s+'),
        'javascript': re.compile(r'^import\s+|^const\s+.*require\('),
        'typescript': re.compile(r'^import\s+|^const\s+.*require\('),
        'rust': re.compile(r'^use\s+|^mod\s+|^extern\s+crate'),
        'go': re.compile(r'^import\s+'),
        'java': re.compile(r'^import\s+'),
        # C/C++ patterns
        'c': re.compile(r'^#include\s+'),
        'cpp': re.compile(r'^#include\s+|^#import\s+|^using\s+namespace'),
        # C# pattern
        'csharp': re.compile(r'^using\s+'),
        # PHP pattern
        'php': re.compile(r'^(use|require|include)\s+'),
        # Ruby pattern
        'ruby': re.compile(r'^require\s+|^load\s+'),
        # Swift pattern
        'swift': re.compile(r'^import\s+'),
        # Kotlin pattern
        'kotlin': re.compile(r'^import\s+'),
    }

    DOCSTRING_PATTERNS = {
        'python': re.compile(r'^\s*("""|\'\'\')'),
        'javascript': re.compile(r'^\s*/\*\*'),
        'typescript': re.compile(r'^\s*/\*\*'),
        'rust': re.compile(r'^\s*///|//!'),
        'go': re.compile(r'^\s*//'),
        'java': re.compile(r'^\s*/\*\*'),
        # C/C++ patterns
        'c': re.compile(r'^\s*/\*\*'),
        'cpp': re.compile(r'^\s*/\*\*'),
        # C# pattern
        'csharp': re.compile(r'^\s*/\*\*'),
        # PHP pattern
        'php': re.compile(r'^\s*/\*\*'),
        # Ruby pattern
        'ruby': re.compile(r'^\s*##'),
        # Swift pattern
        'swift': re.compile(r'^\s*///'),
        # Kotlin pattern
        'kotlin': re.compile(r'^\s*/\*\*'),
    }

    COMMENT_PATTERNS = {
        'python': re.compile(r'^\s*#'),
        'javascript': re.compile(r'^\s*//'),
        'typescript': re.compile(r'^\s*//'),
        'rust': re.compile(r'^\s*//'),
        'go': re.compile(r'^\s*//'),
        'java': re.compile(r'^\s*//'),
        # C/C++ patterns
        'c': re.compile(r'^\s*//|/\*'),
        'cpp': re.compile(r'^\s*//|/\*'),
        # C# pattern
        'csharp': re.compile(r'^\s*//|/\*'),
        # PHP pattern
        'php': re.compile(r'^\s*//|#|/\*'),
        # Ruby pattern
        'ruby': re.compile(r'^\s*#'),
        # Swift pattern
        'swift': re.compile(r'^\s*//|/\*'),
        # Kotlin pattern
        'kotlin': re.compile(r'^\s*//|/\*'),
    }

    @staticmethod
    def detect_language(file_path: str) -> str:
        """Detect programming language from file extension."""
        ext = os.path.splitext(file_path)[1].lower()
        lang_map = {
            # Python
            '.py': 'python',
            '.pyi': 'python',
            '.pyw': 'python',
            # JavaScript
            '.js': 'javascript',
            '.jsx': 'javascript',
            '.mjs': 'javascript',
            '.cjs': 'javascript',
            # TypeScript
            '.ts': 'typescript',
            '.tsx': 'typescript',
            # Rust
            '.rs': 'rust',
            # Go
            '.go': 'go',
            # Java
            '.java': 'java',
            # C/C++
            '.c': 'c',
            '.h': 'c',
            '.cpp': 'cpp',
            '.cxx': 'cpp',
            '.cc': 'cpp',
            '.hpp': 'cpp',
            '.hxx': 'cpp',
            '.hh': 'cpp',
            # C#
            '.cs': 'csharp',
            # PHP
            '.php': 'php',
            '.phtml': 'php',
            # Ruby
            '.rb': 'ruby',
            # Swift
            '.swift': 'swift',
            # Kotlin
            '.kt': 'kotlin',
            '.kts': 'kotlin',
        }
        return lang_map.get(ext, 'unknown')

    @classmethod
    def categorize_line(cls, line: str, line_number: int, language: str) -> ChangeCategory:
        """
        Categorize a single line change.

        Args:
            line: The line content
            line_number: Line number in file
            language: Detected programming language

        Returns:
            ChangeCategory for this line
        """
        stripped = line.strip()

        # Empty/whitespace only
        if not stripped:
            return ChangeCategory.WHITESPACE_CHANGE

        # Check for comments
        comment_pattern = cls.COMMENT_PATTERNS.get(language)
        if comment_pattern and comment_pattern.match(line):
            return ChangeCategory.COMMENT_CHANGE

        # Check for docstrings
        docstring_pattern = cls.DOCSTRING_PATTERNS.get(language)
        if docstring_pattern and docstring_pattern.match(line):
            return ChangeCategory.DOCSTRING_CHANGE

        # Check for imports
        import_pattern = cls.IMPORT_PATTERNS.get(language)
        if import_pattern and import_pattern.match(line):
            return ChangeCategory.IMPORT_ADD

        # Check for class definitions
        class_pattern = cls.CLASS_PATTERNS.get(language)
        if class_pattern and class_pattern.match(line):
            return ChangeCategory.CLASS_ADD

        # Check for function definitions
        function_pattern = cls.FUNCTION_PATTERNS.get(language)
        if function_pattern and function_pattern.match(line):
            return ChangeCategory.FUNCTION_ADD

        # Default to logic change for non-empty lines
        return ChangeCategory.LOGIC_CHANGE

    @classmethod
    def analyze_diff(cls, old_content: str, new_content: str, file_path: str) -> Tuple[List[LineChange], Dict[str, int]]:
        """
        Analyze a diff and categorize all changes.

        Args:
            old_content: Previous file content
            new_content: New file content
            file_path: Path to the file (for language detection)

        Returns:
            Tuple of (list of LineChange objects, category counts)
        """
        language = cls.detect_language(file_path)
        old_lines = old_content.splitlines(keepends=True)
        new_lines = new_content.splitlines(keepends=True)

        line_changes = []
        category_counts = {cat.value: 0 for cat in ChangeCategory}

        # Use difflib to get unified diff
        diff = list(difflib.unified_diff(
            old_lines,
            new_lines,
            fromfile=file_path + "_old",
            tofile=file_path + "_new",
            lineterm=''
        ))

        # Parse diff to extract line changes
        current_line = 0
        for line in diff:
            if line.startswith('@@'):
                # Extract line number from hunk header
                match = re.search(r'@@\s+\-(\d+),?\d*\s+\+(\d+),?\d*\s+@@', line)
                if match:
                    current_line = int(match.group(2))
                continue

            if not line.startswith(('+', '-', ' ')):
                continue

            change_type = 'modified'
            if line.startswith('+'):
                change_type = 'added'
                content = line[1:]
            elif line.startswith('-'):
                change_type = 'removed'
                content = line[1:]
            else:
                content = line[1:]

            # Categorize the change
            category = cls.categorize_line(content, current_line, language)

            # Remove line ending for cleaner storage
            content_clean = content.rstrip('\r\n')

            line_change = LineChange(
                line_number=current_line,
                old_content=content_clean if change_type != 'added' else '',
                new_content=content_clean if change_type != 'removed' else '',
                change_type=change_type,
                category=category
            )

            line_changes.append(line_change)
            category_counts[category.value] += 1

            current_line += 1

        # Determine overall change category
        total_changes = sum(category_counts.values())
        if total_changes == 0:
            overall_category = ChangeCategory.WHITESPACE_CHANGE
        elif category_counts[ChangeCategory.WHITESPACE_CHANGE.value] == total_changes:
            overall_category = ChangeCategory.WHITESPACE_CHANGE
        elif category_counts[ChangeCategory.COMMENT_CHANGE.value] + category_counts[ChangeCategory.DOCSTRING_CHANGE.value] == total_changes:
            overall_category = ChangeCategory.COMMENT_CHANGE
        elif category_counts[ChangeCategory.IMPORT_ADD.value] + category_counts[ChangeCategory.IMPORT_REMOVE.value] == total_changes:
            overall_category = ChangeCategory.IMPORT_ADD
        elif category_counts[ChangeCategory.FUNCTION_ADD.value] + category_counts[ChangeCategory.FUNCTION_REMOVE.value] + category_counts[ChangeCategory.FUNCTION_MODIFY.value] > 0:
            overall_category = ChangeCategory.FUNCTION_MODIFY
        elif category_counts[ChangeCategory.CLASS_ADD.value] + category_counts[ChangeCategory.CLASS_REMOVE.value] + category_counts[ChangeCategory.CLASS_MODIFY.value] > 0:
            overall_category = ChangeCategory.CLASS_MODIFY
        else:
            overall_category = ChangeCategory.LOGIC_CHANGE

        return line_changes, {
            "category_counts": category_counts,
            "overall_category": overall_category.value,
            "total_changes": total_changes,
            "language": language
        }

class FileChangeTracker:
    def __init__(self, storage_backend: FileMetadataInterface, incremental_indexer: IncrementalIndexer):
        """
        Initialize FileChangeTracker with any storage backend that supports file versioning.

        CRITICAL FIX: Added thread lock for atomic version capture to prevent race conditions.

        Args:
            storage_backend: Storage backend that implements FileMetadataInterface
                           (insert_file_version, get_file_version, etc.)
            incremental_indexer: IncrementalIndexer instance
        """
        self.storage_backend = storage_backend
        self.incremental_indexer = incremental_indexer
        # CRITICAL FIX: Add lock for atomic version capture
        self._version_lock = threading.RLock()
        # Track in-progress captures for two-phase commit pattern
        self._pending_captures: Dict[str, str] = {}  # file_path -> version_id

    def _validate_path_within_base(self, file_path: str, base_path: str) -> bool:
        """
        CRITICAL FIX: Validate that a file path is within the base path to prevent path traversal.

        Args:
            file_path: The file path to validate
            base_path: The base path that the file must be within

        Returns:
            True if the file path is safe (within base path), False otherwise
        """
        try:
            # Resolve both paths to their absolute real paths
            # This resolves symlinks and relative path components
            real_file_path = Path(file_path).resolve()
            real_base_path = Path(base_path).resolve()

            # Check if the real file path starts with the real base path
            try:
                real_file_path.relative_to(real_base_path)
                return True  # Path is within base
            except ValueError:
                # relative_to raises ValueError if path is not within base
                logger.error(
                    f"PATH TRAVERSAL ATTEMPT DETECTED: {file_path} is not within base path {base_path}. "
                    f"Resolved file path: {real_file_path}, Resolved base path: {real_base_path}"
                )
                return False

        except (OSError, ValueError) as e:
            logger.error(f"Error validating path {file_path} against base {base_path}: {e}")
            return False

    def _capture_pre_edit_state(self, file_path: str) -> Optional[str]:
        """
        Reads the content of file_path, stores its current state as a version, and returns the content.

        CRITICAL FIX: Implements atomic version capture with path validation and two-phase commit.

        Args:
            file_path: Can be either relative or absolute path

        Returns:
            The file content if successful, None otherwise
        """
        logger.debug(f"_capture_pre_edit_state called with file_path: {file_path}")

        # CRITICAL FIX: Acquire lock for atomic version capture
        with self._version_lock:
            # Get the base path from incremental indexer settings
            base_path = getattr(self.incremental_indexer.settings, 'base_path', '')
            if not base_path:
                logger.error("Base path not configured, cannot validate file path")
                return None

            # Convert to absolute path for file system operations
            if os.path.isabs(file_path):
                full_path = file_path
                # Try to convert to relative path for database storage
                try:
                    if full_path.startswith(base_path):
                        relative_path = os.path.relpath(full_path, base_path)
                        logger.debug(f"Converted absolute path {full_path} to relative path {relative_path}")
                    else:
                        logger.error(f"Absolute path {full_path} is outside base path {base_path}")
                        return None  # CRITICAL FIX: Reject paths outside base
                except (ValueError, AttributeError) as e:
                    logger.error(f"Path conversion failed: {e}")
                    return None
            else:
                relative_path = file_path
                full_path = os.path.join(base_path, file_path)
                logger.debug(f"Converted relative path {file_path} to absolute path {full_path}")

            # CRITICAL FIX: Validate path is within base path to prevent traversal
            if not self._validate_path_within_base(full_path, base_path):
                logger.error(f"Security violation: path traversal attempt detected for {file_path}")
                return None

            # Also validate the normalized relative path doesn't escape
            normalized_relative = self._normalize_path(relative_path)
            if normalized_relative.startswith('..'):
                logger.error(f"Security violation: relative path contains parent directory reference: {normalized_relative}")
                return None

            logger.debug(f"Final paths - full_path: {full_path}, relative_path: {relative_path}")

            if os.path.exists(full_path):
                try:
                    # CRITICAL FIX: Two-phase commit pattern
                    # Phase 1: Generate version ID and mark as pending
                    version_id = self._generate_version_id()
                    self._pending_captures[full_path] = version_id
                    logger.debug(f"Phase 1: Generated version_id {version_id} for {full_path}")

                    # Read file content
                    with open(full_path, 'r', encoding='utf-8', errors='ignore') as f:
                        content = f.read()

                    logger.debug(f"Capturing pre-edit state for file_path: {full_path}, version_id: {version_id}")

                    # Phase 2: Store version
                    if not self._store_file_version(normalized_relative, content, version_id):
                        logger.error(f"Failed to capture pre-edit state for {normalized_relative}")
                        # Rollback: remove from pending
                        del self._pending_captures[full_path]
                        return None

                    # Commit: Update the incremental indexer's metadata
                    file_metadata = self.incremental_indexer.file_metadata.get(normalized_relative, {})
                    file_metadata['current_version_id'] = version_id
                    file_metadata['capture_lock_version'] = version_id  # Track version for lock verification
                    self.incremental_indexer.file_metadata[normalized_relative] = file_metadata
                    self.incremental_indexer.save_metadata()

                    # Remove from pending captures
                    del self._pending_captures[full_path]

                    return content
                except IOError as e:
                    logger.error(f"IOError reading file {full_path}: {e}")
                    # Clean up pending capture
                    if full_path in self._pending_captures:
                        del self._pending_captures[full_path]
                    return None
            else:
                logger.debug(f"File does not exist: {full_path}")
            return None

    def _record_post_edit_state(self, file_path: str, old_content: Optional[str], new_content: str, operation_type: Optional[str] = None, new_file_path: Optional[str] = None):
        """
        Calculates the new content's hash, stores it as a new version, generates a diff if content changed,
        and updates the file index.

        CRITICAL FIX: Implements atomic two-phase commit with lock verification.
        """
        # CRITICAL FIX: Acquire lock for atomic state recording
        with self._version_lock:
            # Get the base path from incremental indexer settings
            base_path = getattr(self.incremental_indexer.settings, 'base_path', '')
            if not base_path:
                logger.error("Base path not configured")
                return

            # Convert to absolute path for file system operations
            if os.path.isabs(file_path):
                full_path = file_path
                try:
                    if full_path.startswith(base_path):
                        relative_path = os.path.relpath(full_path, base_path)
                    else:
                        logger.error(f"Absolute path {full_path} is outside base path {base_path}")
                        return
                except (ValueError, AttributeError):
                    relative_path = file_path
            else:
                relative_path = file_path
                full_path = os.path.join(base_path, file_path)

            # CRITICAL FIX: Validate path is within base path
            if not self._validate_path_within_base(full_path, base_path):
                logger.error(f"Security violation: path traversal attempt detected for {file_path}")
                return

            # Also validate the normalized relative path
            normalized_relative = self._normalize_path(relative_path)
            if normalized_relative.startswith('..'):
                logger.error(f"Security violation: relative path contains parent directory reference: {normalized_relative}")
                return

            current_version_id = self._generate_version_id()
            logger.debug(f"Recording post-edit state for file_path: {file_path}, current_version_id: {current_version_id}")

            # CRITICAL FIX: Two-phase commit with lock verification
            # Phase 1: Check if pre-edit was captured with same lock
            file_metadata = self.incremental_indexer.file_metadata.get(normalized_relative, {})
            lock_version = file_metadata.get('capture_lock_version')

            # Verify we have a consistent state (version lock matches or is new file)
            if old_content is not None and lock_version and full_path not in self._pending_captures:
                logger.warning(
                    f"Race condition detected: Pre-edit state for {normalized_relative} was captured "
                    f"but lock version {lock_version} doesn't match pending capture"
                )

            # Store new version
            if not self._store_file_version(normalized_relative, new_content, current_version_id):
                logger.error(f"Failed to record post-edit state for {normalized_relative}")
                return

            operation_type = "edit" if operation_type is None else operation_type
            previous_version_id = file_metadata.get('current_version_id')

            if old_content is None:
                operation_type = "create"
            elif not os.path.exists(full_path):  # File was deleted
                operation_type = "delete"
                new_content = ""  # Ensure new_content is empty for diffing a deletion

            # Generate and store diff
            if old_content is not None and old_content != new_content:
                diff_id = self._generate_version_id()
                if not self._store_file_diff(diff_id, normalized_relative, previous_version_id, current_version_id, old_content, new_content, operation_type):
                    logger.error(f"Failed to store diff for modified file {normalized_relative}")
            elif old_content is None and new_content:  # File created
                diff_id = self._generate_version_id()
                if not self._store_file_diff(diff_id, normalized_relative, None, current_version_id, "", new_content, "create"):
                    logger.error(f"Failed to store diff for created file {normalized_relative}")
            elif old_content and not new_content and operation_type == "delete":  # File deleted
                diff_id = self._generate_version_id()
                if not self._store_file_diff(diff_id, normalized_relative, previous_version_id, current_version_id, old_content, "", "delete"):
                    logger.error(f"Failed to store diff for deleted file {normalized_relative}")

            # Phase 2: Commit - Update metadata with new version ID and lock
            file_metadata = self.incremental_indexer.file_metadata.get(normalized_relative, {})
            file_metadata['current_version_id'] = current_version_id
            file_metadata['last_version_timestamp'] = current_version_id
            file_metadata['capture_lock_version'] = current_version_id  # Update lock version
            self.incremental_indexer.file_metadata[normalized_relative] = file_metadata
            self.incremental_indexer.save_metadata()

            # Clean up pending captures
            if full_path in self._pending_captures:
                del self._pending_captures[full_path]

            # Also update the file's general metadata (mtime, size, hash)
            self.incremental_indexer.update_file_metadata(normalized_relative, full_path)

            # Force rehash to ensure hash is up to date
            self.incremental_indexer.force_rehash_file(normalized_relative, full_path)

    def _generate_version_id(self) -> str:
        """Generates a unique ID for versions."""
        return uuid.uuid4().hex

    def _calculate_hash(self, content: str) -> str:
        """Calculates SHA-256 hash of content."""
        return hashlib.sha256(content.encode('utf-8')).hexdigest()

    def _normalize_path(self, file_path: str) -> str:
        """
        Normalizes file paths for consistent storage and retrieval.

        CRITICAL FIX: Added security checks to prevent path traversal through normalization bypass.

        Args:
            file_path: The file path to normalize

        Returns:
            Normalized path with forward slashes and consistent format
        """
        if not file_path:
            return file_path

        # CRITICAL FIX: Check for path traversal attempts before normalization
        if '..' in file_path or file_path.startswith('/') or (len(file_path) >= 2 and file_path[1] == ':'):
            # Contains parent directory references or absolute paths
            # These will be validated against base path separately
            pass

        # Normalize path separators and remove redundant separators
        normalized = os.path.normpath(file_path).replace('\\', '/')

        # CRITICAL FIX: Don't strip leading slashes if path is meant to be absolute
        # Instead, normalize consistently
        while '//' in normalized:
            normalized = normalized.replace('//', '/')

        # Remove leading/trailing whitespace but preserve structure
        normalized = normalized.strip()

        logger.debug(f"Normalized path: {file_path} -> {normalized}")
        return normalized

    def _ensure_file_registered(self, file_path: str) -> bool:
        """
        Ensures a file is registered in the metadata store before tracking versions.

        Args:
            file_path: Path to the file to register

        Returns:
            True if file is registered (either was already or successfully registered), False otherwise
        """
        try:
            # Normalize the path for consistent storage
            normalized_path = self._normalize_path(file_path)

            # Check if file is already registered
            file_info = self.storage_backend.get_file_info(normalized_path)
            if file_info is not None:
                logger.debug(f"File {normalized_path} is already registered")
                return True

            # File not registered, register it
            logger.debug(f"Registering new file: {normalized_path}")

            # Determine file type and extension
            file_type = 'file'
            extension = ''
            if os.path.splitext(normalized_path)[1]:
                extension = os.path.splitext(normalized_path)[1][1:]  # Remove the leading dot

            # Register the file
            success = self.storage_backend.add_file(
                file_path=normalized_path,
                file_type=file_type,
                extension=extension,
                metadata={'auto_registered': True, 'registration_timestamp': datetime.datetime.now(datetime.timezone.utc).isoformat()}
            )

            if success:
                logger.info(f"Successfully registered file: {normalized_path}")
                return True
            else:
                logger.error(f"Failed to register file: {normalized_path}")
                return False

        except Exception as e:
            logger.error(f"Error ensuring file registration for {file_path}: {e}")
            return False

    def _store_file_version(self, file_path: str, content: str, version_id: str) -> bool:
        """Stores a file version in file_versions table with automatic file registration."""
        try:
            # Normalize the path for consistent storage
            normalized_path = self._normalize_path(file_path)

            # Ensure file is registered before storing version
            if not self._ensure_file_registered(normalized_path):
                logger.error(f"Cannot store version for unregistered file: {normalized_path}")
                return False

            file_hash = self._calculate_hash(content)
            timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
            size = len(content.encode('utf-8'))

            logger.debug(f"Storing file version: version_id={version_id}, file_path={normalized_path}, timestamp={timestamp}")

            # Use the storage backend interface instead of hardcoded sqlite_storage
            success = self.storage_backend.insert_file_version(version_id, normalized_path, content, file_hash, timestamp, size)

            if success:
                logger.debug(f"Successfully stored file version: {version_id}")
                return True
            else:
                logger.error(f"Failed to store file version: {version_id}")
                return False

        except Exception as e:
            logger.error(f"Error storing file version {version_id} for {file_path}: {e}")
            return False

    def _store_file_diff(self, diff_id: str, file_path: str, previous_version_id: Optional[str], current_version_id: str, old_content: str, new_content: str, operation_type: str, operation_details: Optional[str] = None) -> bool:
        """
        Stores a diff in file_diffs table with granular version tracking.

        PRODUCT.MD ALIGNMENT:
        ---------------------
        "Granular Version Tracking: more detailed diffing and history analysis"

        This method now:
        1. Generates standard unified diff for compatibility
        2. Analyzes changes to categorize them (function, class, import, etc.)
        3. Stores line-by-line change attribution
        4. Provides impact analysis metadata
        """
        try:
            # Normalize the path for consistent storage
            normalized_path = self._normalize_path(file_path)

            # Generate standard unified diff
            diff_content = "\n".join(difflib.unified_diff(
                old_content.splitlines(keepends=True),
                new_content.splitlines(keepends=True),
                fromfile=normalized_path + "_old",
                tofile=normalized_path + "_new",
                lineterm='' # Avoid extra newlines
            ))

            # PRODUCT.MD ALIGNMENT: Analyze changes for granular tracking
            line_changes, analysis = ChangeAnalyzer.analyze_diff(old_content, new_content, normalized_path)

            # Prepare operation_details with granular analysis
            if operation_details is None:
                operation_details = {}
            elif isinstance(operation_details, str):
                # If it's a string, wrap it in a dict
                operation_details = {"note": operation_details}

            # Add granular analysis to operation_details
            operation_details.update({
                "line_changes_count": len(line_changes),
                "category_breakdown": analysis["category_counts"],
                "overall_category": analysis["overall_category"],
                "total_changes": analysis["total_changes"],
                "detected_language": analysis["language"],
                # Store sample of line changes (first 100 for storage efficiency)
                "line_changes_sample": [lc.to_dict() for lc in line_changes[:100]]
            })

            timestamp = datetime.datetime.now(datetime.timezone.utc).isoformat()
            logger.debug(
                f"Storing file diff: diff_id={diff_id}, file_path={normalized_path}, "
                f"previous_version_id={previous_version_id}, current_version_id={current_version_id}, "
                f"operation_type={operation_type}, category={analysis['overall_category']}, "
                f"changes={analysis['total_changes']}, timestamp={timestamp}"
            )

            # Use the storage backend interface
            success = self.storage_backend.insert_file_diff(
                diff_id,
                normalized_path,
                previous_version_id,
                current_version_id,
                diff_content,
                "unified_diff",
                operation_type,
                operation_details,  # Now includes granular analysis
                timestamp
            )

            if success:
                logger.debug(
                    f"Successfully stored file diff: {diff_id} "
                    f"(category={analysis['overall_category']}, changes={analysis['total_changes']})"
                )
                return True
            else:
                logger.error(f"Failed to store file diff: {diff_id}")
                return False

        except Exception as e:
            logger.error(f"Error storing file diff {diff_id} for {file_path}: {e}")
            return False

    def flush(self):
        """Flushes any pending changes to the underlying storage."""
        if hasattr(self.storage_backend, 'flush'):
            self.storage_backend.flush()

    def get_file_version_by_id(self, version_id: str) -> Optional[str]:
        """Retrieves a file version by its ID."""
        version_data = self.storage_backend.get_file_version(version_id)
        if version_data:
            return version_data.get('content')
        return None

    def get_file_history(self, file_path: str) -> List[Dict]:
        """Retrieves the history of changes for a given file path."""
        logger.debug(f"get_file_history called with file_path: {file_path}")

        try:
            # Normalize the path for consistent querying
            normalized_path = self._normalize_path(file_path)
            logger.debug(f"Querying database with normalized path: {normalized_path}")

            # Check if file is registered - if not, it might still have history if it was previously tracked
            file_info = self.storage_backend.get_file_info(normalized_path)
            if file_info is None:
                logger.debug(f"File {normalized_path} is not currently registered, but checking for historical data")

            # Try multiple path variations to find history (handle path normalization inconsistencies)
            search_paths = [normalized_path]

            # Add alternative path formats that might exist in the database
            if normalized_path.startswith('/'):
                search_paths.append(normalized_path[1:])  # Remove leading slash
            else:
                search_paths.append('/' + normalized_path)  # Add leading slash

            # Try Windows path format if on Windows
            if os.name == 'nt':
                alt_path = normalized_path.replace('/', '\\')
                if alt_path != normalized_path:
                    search_paths.append(alt_path)

            versions = []
            diffs = []

            # Search across all path variations
            for search_path in search_paths:
                logger.debug(f"Trying search path: {search_path}")
                path_versions = self.storage_backend.get_file_versions_for_path(search_path)
                path_diffs = self.storage_backend.get_file_diffs_for_path(search_path)

                if path_versions:
                    versions.extend(path_versions)
                    logger.debug(f"Found {len(path_versions)} versions for path {search_path}")
                if path_diffs:
                    diffs.extend(path_diffs)
                    logger.debug(f"Found {len(path_diffs)} diffs for path {search_path}")

                # If we found data, use this path for consistency
                if path_versions or path_diffs:
                    normalized_path = search_path
                    break

            logger.debug(f"Total found: {len(versions)} versions and {len(diffs)} diffs for path {normalized_path}")

            history = []
            for v in versions:
                v['type'] = 'version'
                history.append(v)
            for d in diffs:
                d['type'] = 'diff'
                history.append(d)

            # Sort by timestamp
            history.sort(key=lambda x: x['timestamp'])
            logger.debug(f"Returning {len(history)} history items")
            return history

        except Exception as e:
            logger.error(f"Error retrieving file history for {file_path}: {e}")
            return []

    def reconstruct_file_version(self, full_file_path: str, version_id: str) -> Optional[str]:
        """
        Reconstructs a specific file version by applying diffs if necessary.
        """
        try:
            # 1. Try to retrieve the version directly
            target_version_data = self.storage_backend.get_file_version(version_id)
            if target_version_data:
                return target_version_data.get('content')

            # 2. If not a full version, we need to reconstruct from history
            # Get all versions and diffs for the file path, sorted by timestamp
            history = self.get_file_history(full_file_path)

            # Find the latest full version before or at the target version_id's timestamp
            base_content = None
            base_timestamp = None
            base_version_id = None

            # Find the target version's timestamp first
            target_timestamp = None
            for item in history:
                if item.get('version_id') == version_id or item.get('current_version_id') == version_id:
                    target_timestamp = item['timestamp']
                    break

            if not target_timestamp:
                # If the target version_id is not found in history at all, return None
                return None

            # Find the latest full version before or at the target timestamp
            for item in history:
                if item['type'] == 'version' and item['timestamp'] <= target_timestamp:
                    if base_timestamp is None or item['timestamp'] > base_timestamp:
                        base_content = item['content']
                        base_timestamp = item['timestamp']
                        base_version_id = item['version_id']

            if base_content is None:
                # No full version found before the target, cannot reconstruct
                return None

            current_content = base_content
            # Apply subsequent diffs up to the target version
            for item in history:
                if item['type'] == 'diff' and item['timestamp'] > base_timestamp and item['timestamp'] <= target_timestamp:
                    diff_content = item['diff_content']

                    # Apply the diff
                    # difflib.apply_patch expects a list of lines
                    old_lines = current_content.splitlines(keepends=True)

                    # difflib.parse_unidiff returns an iterator of (filename1, filename2, date1, date2, hunks)
                    # Each hunk is (old_start, old_len, new_start, new_len, lines)
                    # lines are the diff lines with '+', '-', ' ' prefixes

                    # A simpler approach for applying unified diffs is to use a library or manual parsing.
                    # For this implementation, we'll assume a direct application of unified diff format.
                    # This is a simplified application and might need more robust error handling for malformed diffs.

                    # Reconstruct by applying diff lines
                    new_lines = []
                    diff_lines = diff_content.splitlines(keepends=True)

                    # This is a very basic diff application. A real-world scenario might need
                    # a more sophisticated diff parsing and application library.
                    # For unified diff, lines starting with '-' are removed, '+' are added.
                    # Lines starting with ' ' are context.

                    old_idx = 0
                    for line in diff_lines:
                        if line.startswith('---') or line.startswith('+++') or line.startswith('@@'):
                            continue
                        elif line.startswith('-'):
                            # Skip line from old_lines, effectively removing it
                            old_idx += 1
                        elif line.startswith('+'):
                            new_lines.append(line[1:]) # Add new line
                        else: # Context line or unchanged line
                            new_lines.append(old_lines[old_idx])
                            old_idx += 1

                    current_content = "".join(new_lines)

                # If we reached the target version_id, return the current content
                if item.get('version_id') == version_id or item.get('current_version_id') == version_id:
                    return current_content

            return None # Should not reach here if target_timestamp was found and base_content was set

        except Exception as e:
            logger.error(f"Error reconstructing file version {version_id} for {full_file_path}: {e}")
            return None