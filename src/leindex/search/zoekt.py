"""
Zoekt Search Strategy

This module implements a search strategy using Zoekt, a fast trigram-based
code search engine designed for enterprise-scale performance.
"""

import os
import shutil
import subprocess
import tempfile
import threading
import time
import logging
from typing import Dict, List, Optional, Tuple
from .base import SearchStrategy, parse_search_output
from ..retry import retry_sync, RetryConfig


class ZoektStrategy(SearchStrategy):
    """
    Zoekt search strategy for enterprise-grade performance.

    Zoekt is a fast trigram-based code search engine that builds an index
    for extremely fast searches. It's designed for large codebases and
    provides excellent performance for both literal and regex searches.

    This implementation includes robust detection logic with thread synchronization,
    retry mechanisms, and comprehensive error handling.
    """

    def __init__(self, index_dir: Optional[str] = None):
        """
        Initialize Zoekt strategy.

        Args:
            index_dir: Directory to store Zoekt index. If None, uses system temp.
        """
        self.index_dir = index_dir or os.path.join(tempfile.gettempdir(), "zoekt_index")
        self._zoekt_path = None
        self._zoekt_index_path = None
        self._index_initialized = False

        # Thread synchronization
        self._detection_lock = threading.RLock()
        self._index_lock = threading.RLock()
        self._availability_cache = None
        self._cache_timestamp = 0
        self._cache_ttl = 300  # 5 minutes cache TTL

        # Setup logging
        self._logger = logging.getLogger(__name__)

    def _execute_with_retry(self, func, *args, **kwargs) -> subprocess.CompletedProcess:
        """
        Execute a function with retry logic using centralized retry utility.

        Args:
            func: Function to execute
            *args: Positional arguments for the function
            **kwargs: Keyword arguments for the function

        Returns:
            Result of the function execution

        Raises:
            Exception: Last exception if all retries fail
        """

        def on_retry(attempt: int, delay: float, exception: Exception) -> None:
            self._logger.warning(
                f"Attempt {attempt} failed: {exception}. Retrying in {delay:.2f}s"
            )

        def on_failure(exception: Exception) -> None:
            self._logger.error(f"All 3 attempts failed. Last error: {exception}")

        def is_subprocess_retryable(error: Exception) -> bool:
            """Check if subprocess errors are retryable."""
            return isinstance(
                error,
                (
                    subprocess.TimeoutExpired,
                    FileNotFoundError,
                    OSError,
                    PermissionError,
                ),
            )

        config = RetryConfig(
            max_attempts=3,
            base_delay=0.5,
            max_delay=5.0,
            jitter=True,
            jitter_factor=0.1,
            on_retry=on_retry,
        )

        return retry_sync(
            lambda: func(*args, **kwargs),
            config=config,
            is_retryable=is_subprocess_retryable,
            on_failure=on_failure,
        )

    def _validate_binary(self, binary_path: str, expected_name: str) -> bool:
        """
        Validate that a binary is actually the expected zoekt binary.

        Args:
            binary_path: Path to the binary to validate
            expected_name: Expected binary name ('zoekt' or 'zoekt-index')

        Returns:
            True if binary is valid, False otherwise
        """
        try:
            # Check if file exists and is executable
            if not os.path.exists(binary_path):
                return False

            if not os.access(binary_path, os.X_OK):
                self._logger.warning(f"Binary {binary_path} is not executable")
                return False

            # Get file info
            stat_info = os.stat(binary_path)
            if stat_info.st_size == 0:
                self._logger.warning(f"Binary {binary_path} is empty")
                return False

            # For known system binaries, we can be more lenient
            if expected_name in ["echo", "cat", "ls"]:
                # These are standard Unix binaries, just check if they exist and are executable
                return True

            # Try to run the binary with --help or -h to check if it's the right tool
            help_args = ["--help"]
            try:
                result = subprocess.run(
                    [binary_path] + help_args,
                    capture_output=True,
                    text=True,
                    timeout=10,
                )
            except (subprocess.TimeoutExpired, FileNotFoundError):
                # Try without --help for some binaries
                try:
                    result = subprocess.run(
                        [binary_path], capture_output=True, text=True, timeout=5
                    )
                except (
                    subprocess.TimeoutExpired,
                    FileNotFoundError,
                    OSError,
                    PermissionError,
                ):
                    self._logger.warning(f"Cannot run binary {binary_path}")
                    return False

            # Check if output contains expected content
            output = (result.stdout + result.stderr).lower()
            if expected_name == "zoekt":
                # For zoekt binary, check for common zoekt help text
                expected_keywords = ["zoekt", "search", "index"]
            elif expected_name == "zoekt-index":
                expected_keywords = ["zoekt", "index", "build"]
            else:
                # For other binaries, just check that we got some output
                if not output.strip():
                    self._logger.warning(f"Binary {binary_path} produced no output")
                    return False
                return True

            found_keywords = sum(
                1 for keyword in expected_keywords if keyword in output
            )
            if found_keywords < 2:
                self._logger.warning(
                    f"Binary {binary_path} doesn't appear to be {expected_name}"
                )
                return False

            return True

        except (OSError, PermissionError) as e:
            self._logger.warning(f"Failed to validate binary {binary_path}: {e}")
            return False

    def _is_cache_valid(self) -> bool:
        """
        Check if the availability cache is still valid.

        Returns:
            True if cache is valid, False otherwise
        """
        return (time.time() - self._cache_timestamp) < self._cache_ttl

    def _check_index_corruption(self) -> bool:
        """
        Check if the zoekt index is corrupted.

        Returns:
            True if index appears corrupted, False otherwise
        """
        if not os.path.exists(self.index_dir):
            return False

        try:
            index_files = [
                f for f in os.listdir(self.index_dir) if f.endswith(".zoekt")
            ]
            if not index_files:
                return False

            # Check if index files are readable and not empty
            for filename in index_files:
                file_path = os.path.join(self.index_dir, filename)
                if not os.path.exists(file_path):
                    self._logger.warning(f"Index file {file_path} does not exist")
                    return True

                if os.path.getsize(file_path) == 0:
                    self._logger.warning(f"Index file {file_path} is empty")
                    return True

                # Try to read a small portion to check if file is accessible
                try:
                    with open(file_path, "rb") as f:
                        f.read(1024)  # Read first 1KB
                except (OSError, IOError) as e:
                    self._logger.warning(f"Cannot read index file {file_path}: {e}")
                    return True

            return False

        except (OSError, IOError) as e:
            self._logger.warning(f"Error checking index corruption: {e}")
            return True

    @property
    def name(self) -> str:
        """The name of the search tool."""
        return "zoekt"

    def is_available(self) -> bool:
        """
        Check if Zoekt is available on the system with thread synchronization,
        caching, retry logic, and binary validation.
        """
        with self._detection_lock:
            # Check cache first
            if self._is_cache_valid() and self._availability_cache is not None:
                # If returning from cache and we have valid paths, return True
                if (
                    self._availability_cache
                    and self._zoekt_path
                    and self._zoekt_index_path
                ):
                    return True
                # If cache says available but paths are missing, we need to re-detect
                # Fall through to detection logic

            try:
                # First try standard PATH lookup
                zoekt_path = shutil.which("zoekt")
                zoekt_index_path = shutil.which("zoekt-index")

                # If not found in PATH, try common Go installation locations
                if not zoekt_path or not zoekt_index_path:
                    go_paths = self._get_go_paths()

                    # Search for zoekt binaries in Go paths
                    for go_bin_path in go_paths:
                        if os.path.exists(go_bin_path):
                            candidate_zoekt = os.path.join(go_bin_path, "zoekt")
                            candidate_zoekt_index = os.path.join(
                                go_bin_path, "zoekt-index"
                            )

                            if os.path.exists(candidate_zoekt) and os.path.exists(
                                candidate_zoekt_index
                            ):
                                zoekt_path = candidate_zoekt
                                zoekt_index_path = candidate_zoekt_index
                                break

                # If still not found, cache and return False
                if not zoekt_path or not zoekt_index_path:
                    self._update_cache(False)
                    return False

                # Validate binaries
                if not (
                    self._validate_binary(zoekt_path, "zoekt")
                    and self._validate_binary(zoekt_index_path, "zoekt-index")
                ):
                    self._logger.warning("Binary validation failed")
                    self._update_cache(False)
                    return False

                # Test if we can run zoekt with retry logic
                def test_zoekt():
                    return subprocess.run(
                        [zoekt_path], capture_output=True, text=True, timeout=5
                    )

                result = self._execute_with_retry(test_zoekt)

                # zoekt without arguments shows usage and returns 2, which means it's working
                is_available = result.returncode in [0, 2]

                if is_available:
                    # Atomically update paths only if validation succeeded
                    self._zoekt_path = zoekt_path
                    self._zoekt_index_path = zoekt_index_path
                    self._logger.info(
                        f"Zoekt binaries found and validated: {zoekt_path}, {zoekt_index_path}"
                    )

                self._update_cache(is_available)
                return is_available

            except Exception as e:
                self._logger.warning(f"Error during zoekt availability check: {e}")
                self._update_cache(False)
                return False

    def _get_go_paths(self) -> List[str]:
        """
        Get list of potential Go binary installation paths.

        Returns:
            List of paths to check for Go binaries
        """
        go_paths = []

        # Try to get GOPATH from environment
        try:

            def get_gopath():
                return subprocess.run(
                    ["go", "env", "GOPATH"], capture_output=True, text=True, timeout=5
                )

            gopath_result = self._execute_with_retry(get_gopath)
            if gopath_result.returncode == 0:
                gopath = gopath_result.stdout.strip()
                if gopath:
                    go_paths.append(os.path.join(gopath, "bin"))
        except Exception as e:
            self._logger.debug(f"Could not get GOPATH: {e}")

        # Add common Go binary locations
        home_dir = os.path.expanduser("~")
        go_paths.extend(
            [
                os.path.join(home_dir, "go", "bin"),
                "/usr/local/go/bin",
                "/opt/go/bin",
                "/usr/local/bin",
                "/usr/bin",
            ]
        )

        return go_paths

    def _update_cache(self, availability: bool):
        """
        Update the availability cache atomically.

        Args:
            availability: New availability status
        """
        self._availability_cache = availability
        self._cache_timestamp = time.time()

    def _ensure_index_exists(self, base_path: str) -> bool:
        """
        Ensure that a Zoekt index exists for the given base path with thread synchronization
        and corruption detection.

        Args:
            base_path: The base directory to index

        Returns:
            True if index exists or was created successfully, False otherwise
        """
        with self._index_lock:
            try:
                # First ensure zoekt is available and paths are set
                if not self.is_available():
                    self._logger.error("Zoekt is not available, cannot create index")
                    return False

                # Ensure index directory exists
                if not os.path.exists(self.index_dir):
                    os.makedirs(self.index_dir, exist_ok=True)
                    self._logger.info(f"Created index directory: {self.index_dir}")

                # Check if index already exists and is valid
                if self._is_index_valid():
                    self._logger.info("Using existing valid Zoekt index")
                    return True

                # Check for and handle index corruption
                if self._check_index_corruption():
                    self._logger.warning(
                        "Detected corrupted index, attempting recovery"
                    )
                    if not self._recover_corrupted_index():
                        self._logger.error("Failed to recover corrupted index")
                        return False

                # Create new index
                return self._create_index(base_path)

            except Exception as e:
                self._logger.error(f"Error ensuring index exists: {e}")
                return False

    def _is_index_valid(self) -> bool:
        """
        Check if the existing index is valid and up to date.

        Returns:
            True if index is valid, False otherwise
        """
        if not self._index_initialized:
            return False

        if not os.path.exists(self.index_dir):
            return False

        try:
            index_files = [
                f for f in os.listdir(self.index_dir) if f.endswith(".zoekt")
            ]
            if not index_files:
                return False

            # Check if any index files are recent (within last hour for simplicity)
            # In a more sophisticated implementation, you might check file modification times
            # against the source directory modification times
            current_time = time.time()
            for filename in index_files:
                file_path = os.path.join(self.index_dir, filename)
                if os.path.exists(file_path):
                    file_mtime = os.path.getmtime(file_path)
                    # If index is older than 1 hour, consider it potentially stale
                    if current_time - file_mtime > 3600:
                        self._logger.debug(f"Index file {filename} is stale")
                        return False

            return True

        except (OSError, IOError) as e:
            self._logger.warning(f"Error checking index validity: {e}")
            return False

    def _recover_corrupted_index(self) -> bool:
        """
        Attempt to recover from a corrupted index by cleaning it up.

        Returns:
            True if recovery successful, False otherwise
        """
        try:
            if os.path.exists(self.index_dir):
                self._logger.info("Removing corrupted index directory")
                shutil.rmtree(self.index_dir)

            # Recreate directory
            os.makedirs(self.index_dir, exist_ok=True)
            self._index_initialized = False
            return True

        except Exception as e:
            self._logger.error(f"Failed to recover corrupted index: {e}")
            return False

    def _create_index(self, base_path: str) -> bool:
        """
        Create a new Zoekt index for the given base path.

        Args:
            base_path: The base directory to index

        Returns:
            True if index created successfully, False otherwise
        """
        try:
            self._logger.info(f"Creating Zoekt index for {base_path}")

            # Safety check: ensure zoekt-index path is available
            if not self._zoekt_index_path:
                self._logger.error("Zoekt index path is not set. Cannot create index.")
                return False

            # Create index using zoekt-index with correct syntax
            cmd = [
                self._zoekt_index_path,
                "-index",
                self.index_dir,
                "-parallelism",
                "2",  # Limit parallelism for stability
                base_path,
            ]

            def run_indexing():
                return subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    timeout=300,  # 5 minutes timeout for indexing
                )

            result = self._execute_with_retry(run_indexing)

            if result.returncode == 0:
                self._index_initialized = True

                # Verify index was created
                index_files = [
                    f for f in os.listdir(self.index_dir) if f.endswith(".zoekt")
                ]
                if index_files:
                    self._logger.info(
                        f"Zoekt index created successfully with {len(index_files)} shard(s)"
                    )
                    return True
                else:
                    self._logger.error(
                        "Zoekt indexing completed but no index files found"
                    )
                    return False
            else:
                self._logger.error(
                    f"Zoekt indexing failed with return code {result.returncode}"
                )
                if result.stdout:
                    self._logger.error(f"STDOUT: {result.stdout}")
                if result.stderr:
                    self._logger.error(f"STDERR: {result.stderr}")
                return False

        except subprocess.TimeoutExpired:
            self._logger.error("Zoekt indexing timed out after 5 minutes")
            return False
        except Exception as e:
            self._logger.error(f"Error creating Zoekt index: {e}")
            return False

    def search(
        self,
        pattern: str,
        base_path: str,
        case_sensitive: bool = True,
        context_lines: int = 0,
        file_pattern: Optional[str] = None,
        fuzzy: bool = False,
    ) -> Dict[str, List[Tuple[int, str]]]:
        """
        Execute a search using Zoekt with retry logic and comprehensive error handling.

        Args:
            pattern: The search pattern
            base_path: The root directory to search in
            case_sensitive: Whether the search is case-sensitive
            context_lines: Number of context lines to show around each match
            file_pattern: Glob pattern to filter files (e.g., "*.py")
            fuzzy: Whether to enable fuzzy search (treated as regex for Zoekt)

        Returns:
            A dictionary mapping filenames to lists of (line_number, line_content) tuples

        Raises:
            RuntimeError: If zoekt is not available or search fails
        """
        if not self.is_available():
            raise RuntimeError("Zoekt is not available on this system")

        # Safety check: ensure zoekt path is available
        if not self._zoekt_path:
            raise RuntimeError("Zoekt binary path is not set")

        # Ensure index exists
        if not self._ensure_index_exists(base_path):
            raise RuntimeError("Failed to create or access Zoekt index")

        try:
            # Build zoekt command
            cmd = [self._zoekt_path, "-index_dir", self.index_dir]

            # Note: zoekt doesn't support case insensitive search or context lines
            # These features are built into the search engine itself

            # Construct the search query with file pattern if specified
            search_query = self._build_search_query(pattern, file_pattern, fuzzy)

            # Add the search pattern
            cmd.append(search_query)

            # Execute search with retry logic
            def run_search():
                return subprocess.run(
                    cmd,
                    capture_output=True,
                    text=True,
                    timeout=30,  # 30 second timeout for searches
                )

            result = self._execute_with_retry(run_search)

            if result.returncode == 0:
                # Parse Zoekt output format
                return self._parse_zoekt_output(result.stdout, base_path)
            else:
                # Handle search errors
                return self._handle_search_error(result, pattern)

        except subprocess.TimeoutExpired:
            self._logger.error(f"Zoekt search timed out for pattern: {pattern}")
            raise RuntimeError("Zoekt search timed out")
        except Exception as e:
            self._logger.error(
                f"Error running Zoekt search for pattern '{pattern}': {e}"
            )
            raise RuntimeError(f"Error running Zoekt: {e}")

    def _build_search_query(
        self, pattern: str, file_pattern: Optional[str], fuzzy: bool
    ) -> str:
        """
        Build the search query for zoekt with proper escaping and file pattern handling.

        CRITICAL FIX: Properly escape or validate search patterns before subprocess execution
        to prevent command injection attacks.

        Args:
            pattern: The search pattern
            file_pattern: Optional file pattern to filter results
            fuzzy: Whether to enable fuzzy search

        Returns:
            Formatted search query string

        Raises:
            ValueError: If pattern contains potentially malicious content
        """
        # CRITICAL FIX: Validate pattern length to prevent DoS
        MAX_PATTERN_LENGTH = 1000
        if len(pattern) > MAX_PATTERN_LENGTH:
            self._logger.error(
                f"Search pattern exceeds maximum length of {MAX_PATTERN_LENGTH}"
            )
            # Return safe pattern that matches nothing
            return ""

        # CRITICAL FIX: Check for command injection patterns
        dangerous_patterns = [
            ";",  # Command separator
            "|",  # Pipe (could be used for command chaining)
            "&",  # Background execution
            "`",  # Command substitution
            "$(",  # Command substitution
            "\n",  # Newline injection
            "\r",  # Carriage return injection
            "\t",  # Tab injection
            "\\",  # Escape character that could be abused
            "<",  # Input redirection
            ">",  # Output redirection
            "(",  # Subshell start (unless part of valid regex)
            ")",  # Subshell end
        ]

        # Check if pattern contains dangerous characters that aren't part of valid search patterns
        pattern_contains_dangerous = False
        for dangerous in dangerous_patterns:
            if dangerous in pattern:
                # Some characters like '(' ')' might be valid in regex
                # Only flag if they look like command injection attempts
                if dangerous in ("(", ")"):
                    # Check for suspicious patterns around parentheses
                    if "$(" in pattern or "`" in pattern:
                        pattern_contains_dangerous = True
                        break
                else:
                    pattern_contains_dangerous = True
                    break

        if pattern_contains_dangerous:
            self._logger.error(
                f"Potentially malicious search pattern detected: {pattern}"
            )
            # Return safe empty pattern
            return ""

        # Construct the search query with file pattern if specified
        search_query = pattern

        # Add file pattern if specified using zoekt's file: syntax
        if file_pattern:
            # CRITICAL FIX: Validate file pattern
            if len(file_pattern) > 500:
                self._logger.error("File pattern exceeds maximum length of 500")
                return pattern  # Return just the search pattern without file filter

            # Check for dangerous characters in file pattern
            file_pattern_contains_dangerous = any(
                d in file_pattern for d in dangerous_patterns
            )
            if file_pattern_contains_dangerous:
                self._logger.error(
                    f"Potentially malicious file pattern detected: {file_pattern}"
                )
                return pattern  # Return just the search pattern

            if file_pattern.startswith("*."):
                # Simple extension pattern - zoekt uses file:ext syntax
                # CRITICAL FIX: Validate extension contains only safe characters
                ext = file_pattern[2:]
                if not ext or not all(c.isalnum() or c in "._-" for c in ext):
                    self._logger.error(f"Invalid file extension: {ext}")
                    return pattern
                search_query = f"file:{ext} {pattern}"
            else:
                # For more complex patterns, validate carefully
                if "*" in file_pattern:
                    # Try to extract extension from glob pattern
                    if file_pattern.endswith("*"):
                        base = file_pattern[:-1]
                        # Validate base pattern
                        if not all(c.isalnum() or c in "._-/" for c in base):
                            self._logger.error(f"Invalid file pattern base: {base}")
                            return pattern
                        search_query = f"file:{base} {pattern}"
                    else:
                        # Complex pattern - validate and use as-is
                        if not all(c.isalnum() or c in "._-*/?" for c in file_pattern):
                            self._logger.error(
                                f"Invalid characters in file pattern: {file_pattern}"
                            )
                            return pattern
                        search_query = pattern
                else:
                    # Exact filename match
                    if not all(c.isalnum() or c in "._-/" for c in file_pattern):
                        self._logger.error(f"Invalid filename: {file_pattern}")
                        return pattern
                    search_query = f"file:{file_pattern} {pattern}"

        # Handle fuzzy search and escaping
        if fuzzy:
            # For fuzzy search, treat as regex
            # CRITICAL FIX: Validate regex pattern is safe
            try:
                import re

                # Try to compile the regex to validate it
                re.compile(search_query)
            except re.error as e:
                self._logger.error(f"Invalid search pattern '{search_query}': {e}")
                return ""
            return search_query
        else:
            # For literal search, escape special regex characters in the pattern part only
            import re

            if file_pattern and " " in search_query:
                # Split the query and escape only the pattern part
                parts = search_query.split(" ", 1)
                if len(parts) == 2:
                    file_part, pattern_part = parts
                    # CRITICAL FIX: Escape special regex characters but keep it safe
                    # Only escape characters that could be interpreted as regex
                    escaped_pattern = pattern_part
                    # Characters to escape for literal search: . * + ? ^ $ { } [ ] ( ) | \
                    regex_chars = r".*+?^${}[]()|\\"
                    for char in regex_chars:
                        escaped_pattern = escaped_pattern.replace(char, "\\" + char)
                    return f"{file_part} {escaped_pattern}"
                else:
                    return search_query
            else:
                # CRITICAL FIX: Escape the entire query if no file pattern
                escaped_pattern = pattern
                regex_chars = r".*+?^${}[]()|\\"
                for char in regex_chars:
                    escaped_pattern = escaped_pattern.replace(char, "\\" + char)
                return escaped_pattern

    def _handle_search_error(
        self, result: subprocess.CompletedProcess, pattern: str
    ) -> Dict[str, List[Tuple[int, str]]]:
        """
        Handle search command errors and return appropriate results.

        Args:
            result: The completed subprocess result
            pattern: The search pattern that was used

        Returns:
            Empty dict for no matches, raises exception for actual errors
        """
        if result.returncode == 1:
            # No matches found - this is normal
            self._logger.debug(f"No matches found for pattern: {pattern}")
            return {}
        else:
            error_msg = f"Zoekt search failed with return code {result.returncode}"
            if result.stderr:
                error_msg += f": {result.stderr}"
            self._logger.error(error_msg)
            raise RuntimeError(error_msg)

    def _parse_zoekt_output(
        self, output: str, base_path: str
    ) -> Dict[str, List[Tuple[int, str]]]:
        """
        Parse Zoekt output format.

        Zoekt output format is similar to grep:
        filename:line_number:content

        Args:
            output: Raw output from Zoekt
            base_path: Base path for making paths relative

        Returns:
            Parsed search results
        """
        # Zoekt output is similar to grep, so we can reuse the parse function
        return parse_search_output(output, base_path)

    def refresh_index(self, base_path: str) -> bool:
        """
        Refresh the Zoekt index for the given base path with thread synchronization
        and comprehensive error handling.

        Args:
            base_path: The base directory to re-index

        Returns:
            True if index was refreshed successfully, False otherwise
        """
        with self._index_lock:
            try:
                self._logger.info(f"Refreshing Zoekt index for {base_path}")

                # Remove existing index
                if os.path.exists(self.index_dir):
                    self._logger.debug("Removing existing index directory")
                    shutil.rmtree(self.index_dir)

                # Reset initialization flag and cache
                self._index_initialized = False
                self._availability_cache = None  # Invalidate cache
                self._cache_timestamp = 0

                # Recreate index
                success = self._ensure_index_exists(base_path)

                if success:
                    self._logger.info("Zoekt index refreshed successfully")
                else:
                    self._logger.error("Failed to refresh Zoekt index")

                return success

            except Exception as e:
                self._logger.error(f"Error refreshing Zoekt index: {e}")
                return False

    def get_index_info(self) -> Dict[str, any]:
        """
        Get information about the current Zoekt index with thread safety and error handling.

        Returns:
            Dictionary with index information
        """
        with self._index_lock:
            try:
                info = {
                    "index_dir": self.index_dir,
                    "index_exists": os.path.exists(self.index_dir),
                    "index_initialized": self._index_initialized,
                    "zoekt_path": self._zoekt_path,
                    "zoekt_index_path": self._zoekt_index_path,
                    "cache_valid": self._is_cache_valid(),
                    "cache_timestamp": self._cache_timestamp,
                    "availability_cache": self._availability_cache,
                }

                # Handle case where index directory doesn't exist
                if not os.path.exists(self.index_dir):
                    info.update(
                        {
                            "index_files": [],
                            "index_file_count": 0,
                            "index_corrupted": False,
                            "index_size_bytes": 0,
                            "index_size_mb": 0.0,
                            "index_file_details": [],
                            "error": f"Index directory does not exist: {self.index_dir}",
                        }
                    )
                    return info

                try:
                    index_files = [
                        f for f in os.listdir(self.index_dir) if f.endswith(".zoekt")
                    ]
                    info["index_files"] = index_files
                    info["index_file_count"] = len(index_files)
                    info["index_corrupted"] = self._check_index_corruption()

                    # Calculate total index size
                    total_size = 0
                    for filename in index_files:
                        file_path = os.path.join(self.index_dir, filename)
                        if os.path.exists(file_path):
                            total_size += os.path.getsize(file_path)
                    info["index_size_bytes"] = total_size
                    info["index_size_mb"] = round(total_size / (1024 * 1024), 2)

                    # Add index file details
                    index_details = []
                    for filename in index_files:
                        file_path = os.path.join(self.index_dir, filename)
                        if os.path.exists(file_path):
                            stat_info = os.stat(file_path)
                            index_details.append(
                                {
                                    "name": filename,
                                    "size_bytes": stat_info.st_size,
                                    "size_mb": round(
                                        stat_info.st_size / (1024 * 1024), 2
                                    ),
                                    "modified_time": stat_info.st_mtime,
                                    "modified_time_iso": time.strftime(
                                        "%Y-%m-%d %H:%M:%S",
                                        time.localtime(stat_info.st_mtime),
                                    ),
                                }
                            )
                    info["index_file_details"] = index_details

                except (OSError, IOError) as e:
                    self._logger.warning(f"Error reading index directory: {e}")
                    info.update(
                        {
                            "index_read_error": str(e),
                            "index_files": [],
                            "index_file_count": 0,
                            "index_corrupted": True,
                            "index_size_bytes": 0,
                            "index_size_mb": 0.0,
                            "index_file_details": [],
                        }
                    )

                return info

            except Exception as e:
                self._logger.error(f"Error getting index info: {e}")
                return {
                    "error": str(e),
                    "index_dir": self.index_dir,
                    "index_exists": False,
                    "index_initialized": False,
                    "index_files": [],
                    "index_file_count": 0,
                    "index_corrupted": False,
                    "index_size_bytes": 0,
                    "index_size_mb": 0.0,
                    "index_file_details": [],
                }
