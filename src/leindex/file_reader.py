"""
Sophisticated File Reader Module

This module provides intelligent file reading capabilities with strategy pattern,
error detection, and memory-efficient reading for various file formats and sizes.
"""

import os
import mmap
import hashlib
import logging
from abc import ABC, abstractmethod
from typing import Optional, Dict, Any, Iterator, List
from enum import Enum
from dataclasses import dataclass
import threading
import json

from .lazy_loader import LazyContentManager
from .content_extractor import ContentExtractor
from .logger_config import logger

class FileSizeCategory(Enum):
    """File size categories for strategy selection."""
    SMALL = "small"      # < 1MB
    MEDIUM = "medium"    # 1MB - 10MB
    LARGE = "large"      # 10MB - 100MB
    VERY_LARGE = "very_large"  # > 100MB

class ReadingStrategy(Enum):
    """Reading strategies for different file types and sizes."""
    LAZY_LOADING = "lazy_loading"
    CHUNKED_READING = "chunked_reading"
    MEMORY_MAPPING = "memory_mapping"
    CONTENT_EXTRACTION = "content_extraction"

@dataclass
class FileError:
    """Represents an error detected in a file."""
    error_type: str
    message: str
    line_number: Optional[int] = None
    column_number: Optional[int] = None
    severity: str = "error"  # error, warning, info
    context: Optional[str] = None

@dataclass
class FileMetadata:
    """Comprehensive file metadata."""
    file_path: str
    size: int
    last_modified: float
    extension: str
    encoding: Optional[str] = None
    checksum: Optional[str] = None
    line_count: Optional[int] = None
    category: Optional[FileSizeCategory] = None
    strategy: Optional[ReadingStrategy] = None
    errors: List[FileError] = None
    warnings: List[FileError] = None

class FileReaderInterface(ABC):
    """Abstract interface for file reading operations."""

    @abstractmethod
    def read_content(self, file_path: str, **kwargs) -> Optional[str]:
        """Read file content with specified options.
        
        Args:
            file_path: Path to the file to read
            **kwargs: Additional options for reading (encoding, chunk_size, etc.)
            
        Returns:
            File content as string or None if reading failed
        """
        pass

    @abstractmethod
    def read_metadata(self, file_path: str) -> Optional[Dict[str, Any]]:
        """Read file metadata without loading content.
        
        Args:
            file_path: Path to the file to analyze
            
        Returns:
            Dictionary containing file metadata or None if failed
        """
        pass

    @abstractmethod
    def read_in_chunks(self, file_path: str, chunk_size: int = 4*1024*1024) -> Iterator[str]:
        """Read file in chunks for memory efficiency.
        
        Args:
            file_path: Path to the file to read
            chunk_size: Size of each chunk in bytes (default: 4MB)
            
        Yields:
            String chunks of the file content
        """
        pass

    @abstractmethod
    def detect_errors(self, file_path: str) -> List[FileError]:
        """Detect and report issues within files.
        
        Args:
            file_path: Path to the file to analyze
            
        Returns:
            List of detected errors and warnings
        """
        pass

class LazyLoadingStrategy:
    """Strategy for lazy loading small to medium files."""
    
    def __init__(self, lazy_content_manager: LazyContentManager):
        self.lazy_content_manager = lazy_content_manager
    
    def read_content(self, file_path: str, **kwargs) -> Optional[str]:
        """Read content using lazy loading."""
        try:
            lazy_content = self.lazy_content_manager.get_file_content(file_path)
            return lazy_content.content
        except Exception as e:
            logger.error(f"Lazy loading failed for {file_path}: {e}")
            return None
    
    def read_in_chunks(self, file_path: str, chunk_size: int = 4*1024*1024) -> Iterator[str]:
        """Read in chunks using lazy loading."""
        content = self.read_content(file_path)
        if content:
            # Split content into chunks
            for i in range(0, len(content), chunk_size):
                yield content[i:i + chunk_size]

class ChunkedReadingStrategy:
    """Strategy for reading large files in chunks."""
    
    def __init__(self, chunk_size: int = 4*1024*1024):
        self.chunk_size = chunk_size
    
    def read_content(self, file_path: str, **kwargs) -> Optional[str]:
        """Read content using chunked reading."""
        chunks = []
        try:
            with open(file_path, 'r', encoding=kwargs.get('encoding', 'utf-8'), errors='ignore') as f:
                while True:
                    chunk = f.read(self.chunk_size)
                    if not chunk:
                        break
                    chunks.append(chunk)
            return ''.join(chunks)
        except Exception as e:
            logger.error(f"Chunked reading failed for {file_path}: {e}")
            return None
    
    def read_in_chunks(self, file_path: str, chunk_size: int = 4*1024*1024, **kwargs) -> Iterator[str]:
        """Read file in chunks directly."""
        try:
            with open(file_path, 'r', encoding=kwargs.get('encoding', 'utf-8'), errors='ignore') as f:
                while True:
                    chunk = f.read(chunk_size)
                    if not chunk:
                        break
                    yield chunk
        except Exception as e:
            logger.error(f"Chunked reading failed for {file_path}: {e}")
            raise

class MemoryMappingStrategy:
    """Strategy for memory-mapping very large files."""
    
    def __init__(self):
        pass
    
    def read_content(self, file_path: str, **kwargs) -> Optional[str]:
        """Read content using memory mapping."""
        try:
            with open(file_path, 'rb') as f:
                with mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as mm:
                    return mm.decode(kwargs.get('encoding', 'utf-8'), errors='ignore')
        except Exception as e:
            logger.error(f"Memory mapping failed for {file_path}: {e}")
            return None
    
    def read_in_chunks(self, file_path: str, chunk_size: int = 4*1024*1024, **kwargs) -> Iterator[str]:
        """Read memory-mapped file in chunks."""
        try:
            with open(file_path, 'rb') as f:
                with mmap.mmap(f.fileno(), 0, access=mmap.ACCESS_READ) as mm:
                    for i in range(0, len(mm), chunk_size):
                        chunk = mm[i:i + chunk_size]
                        yield chunk.decode(kwargs.get('encoding', 'utf-8'), errors='ignore')
        except Exception as e:
            logger.error(f"Memory mapping chunked reading failed for {file_path}: {e}")
            raise

class ContentExtractionStrategy:
    """Strategy for extracting content from various file formats."""
    
    def __init__(self, base_path: str):
        self.content_extractor = ContentExtractor(base_path)
    
    def read_content(self, file_path: str, **kwargs) -> Optional[str]:
        """Read content using content extraction."""
        try:
            result = self.content_extractor.extract_content(file_path)
            return result['content'] if result else None
        except Exception as e:
            logger.error(f"Content extraction failed for {file_path}: {e}")
            return None
    
    def read_in_chunks(self, file_path: str, chunk_size: int = 4*1024*1024) -> Iterator[str]:
        """Read extracted content in chunks."""
        content = self.read_content(file_path)
        if content:
            for i in range(0, len(content), chunk_size):
                yield content[i:i + chunk_size]

class ErrorDetector:
    """Detects various types of errors in files."""
    
    def __init__(self):
        self.code_patterns = {
            'python': {
                'syntax_errors': [r'SyntaxError', r'IndentationError', r'IndentationError'],
                'common_errors': [r'NameError', r'TypeError', r'ValueError', r'KeyError']
            },
            'javascript': {
                'syntax_errors': [r'SyntaxError', r'Unexpected token', r'Unterminated string literal'],
                'common_errors': [r'ReferenceError', r'TypeError', r'Cannot read property']
            },
            'json': {
                'syntax_errors': [r'JSON parse error', r'Unexpected end of JSON input']
            }
        }
    
    def detect_errors(self, file_path: str, content: Optional[str] = None, file_extension: str = '') -> List[FileError]:
        """Detect errors in a file."""
        errors = []
        
        if not os.path.exists(file_path):
            errors.append(FileError(
                error_type="file_not_found",
                message=f"File not found: {file_path}",
                severity="error"
            ))
            return errors
        
        # Get file content if not provided
        if content is None:
            try:
                with open(file_path, 'r', encoding='utf-8', errors='ignore') as f:
                    content = f.read()
            except Exception as e:
                errors.append(FileError(
                    error_type="file_read_error",
                    message=f"Failed to read file: {e}",
                    severity="error"
                ))
                return errors
        
        # Detect encoding issues
        if self._has_encoding_issues(content):
            errors.append(FileError(
                error_type="encoding_issue",
                message="File contains encoding issues (mojibake)",
                severity="warning"
            ))
        
        # Detect syntax errors based on file type
        file_type = file_extension.lstrip('.').lower()

        # Map file extensions to language types
        extension_to_language = {
            'py': 'python',
            'js': 'javascript',
            'jsx': 'javascript',
            'ts': 'javascript',  # TypeScript is similar to JavaScript for syntax checking
            'tsx': 'javascript',
            'json': 'json',
            'yml': 'yaml',
            'yaml': 'yaml'
        }

        language_type = extension_to_language.get(file_type, file_type)

        if language_type in self.code_patterns:
            errors.extend(self._detect_syntax_errors(content, language_type))
        
        # Detect malformed content
        errors.extend(self._detect_malformed_content(content, file_type))
        
        # Detect very large files that might cause issues
        file_size = os.path.getsize(file_path)
        if file_size > 100 * 1024 * 1024:  # 100MB
            errors.append(FileError(
                error_type="large_file",
                message=f"File is very large ({file_size / (1024*1024):.1f}MB), may impact performance",
                severity="warning"
            ))
        
        return errors
    
    def _has_encoding_issues(self, content: str) -> bool:
        """Check for encoding issues in content."""
        # Look for common mojibake patterns
        mojibake_patterns = [
            r'\x85', r'\x91', r'\x92', r'\x93', r'\x94', r'\x96', r'\x97',
            r'\uFFFD',  # Replacement character
        ]
        
        import re
        for pattern in mojibake_patterns:
            if re.search(pattern, content):
                return True
        return False
    
    def _detect_syntax_errors(self, content: str, file_type: str) -> List[FileError]:
        """Detect syntax errors in code files."""
        errors = []
        patterns = self.code_patterns.get(file_type, {})

        import re
        lines = content.split('\n')

        # For Python files, use AST parsing to detect real syntax errors
        if file_type == 'python':
            errors.extend(self._detect_python_syntax_errors(content))

        # Also check for common error patterns in content
        for line_num, line in enumerate(lines, 1):
            for error_type, error_patterns in patterns.items():
                for pattern in error_patterns:
                    if re.search(pattern, line, re.IGNORECASE):
                        errors.append(FileError(
                            error_type=error_type,
                            message=f"Potential {error_type.replace('_', ' ')} detected",
                            line_number=line_num,
                            context=line.strip(),
                            severity="error" if error_type == "syntax_errors" else "warning"
                        ))

        return errors

    def _detect_python_syntax_errors(self, content: str) -> List[FileError]:
        """Detect Python syntax errors using AST parsing."""
        errors = []

        try:
            import ast
            ast.parse(content)
        except SyntaxError as e:
            errors.append(FileError(
                error_type="syntax_error",
                message=f"SyntaxError: {e.msg}",
                line_number=e.lineno,
                column_number=e.offset,
                context=e.text.strip() if e.text else None,
                severity="error"
            ))
        except Exception as e:
            errors.append(FileError(
                error_type="parse_error",
                message=f"Failed to parse Python code: {e}",
                severity="error"
            ))

        return errors
    
    def _detect_malformed_content(self, content: str, file_type: str) -> List[FileError]:
        """Detect malformed content based on file type."""
        errors = []
        
        if file_type == 'json':
            try:
                json.loads(content)
            except json.JSONDecodeError as e:
                errors.append(FileError(
                    error_type="json_malformed",
                    message=f"Invalid JSON: {e}",
                    line_number=getattr(e, 'lineno', None),
                    severity="error"
                ))
        
        elif file_type in ['yaml', 'yml']:
            # Basic YAML structure checks
            if content.count(':') == 0 and not content.strip().startswith('#'):
                errors.append(FileError(
                    error_type="yaml_malformed",
                    message="YAML file appears to have no key-value pairs",
                    severity="warning"
                ))
        
        return errors

class SmartFileReader(FileReaderInterface):
    """Intelligent file reader that selects the best reading strategy based on file characteristics."""
    
    def __init__(self, base_path: str, lazy_content_manager: Optional[LazyContentManager] = None):
        self.base_path = base_path
        self.lazy_content_manager = lazy_content_manager or LazyContentManager()
        
        # Initialize strategies
        self.strategies = {
            ReadingStrategy.LAZY_LOADING: LazyLoadingStrategy(self.lazy_content_manager),
            ReadingStrategy.CHUNKED_READING: ChunkedReadingStrategy(),
            ReadingStrategy.MEMORY_MAPPING: MemoryMappingStrategy(),
            ReadingStrategy.CONTENT_EXTRACTION: ContentExtractionStrategy(base_path)
        }
        
        # Initialize error detector
        self.error_detector = ErrorDetector()
        
        # Cache for file metadata
        self._metadata_cache = {}
        self._cache_lock = threading.Lock()
    
    def _determine_file_category(self, file_path: str) -> FileSizeCategory:
        """Determine file size category."""
        try:
            file_size = os.path.getsize(file_path)
            
            if file_size < 1024 * 1024:  # < 1MB
                return FileSizeCategory.SMALL
            elif file_size < 10 * 1024 * 1024:  # < 10MB
                return FileSizeCategory.MEDIUM
            elif file_size < 100 * 1024 * 1024:  # < 100MB
                return FileSizeCategory.LARGE
            else:
                return FileSizeCategory.VERY_LARGE
        except OSError:
            return FileSizeCategory.MEDIUM  # Default to medium if can't determine size
    
    def _determine_best_strategy(self, file_path: str, file_extension: str = '') -> ReadingStrategy:
        """Auto-determine the best reading strategy based on file characteristics."""
        category = self._determine_file_category(file_path)
        file_extension = file_extension.lower()
        
        # Content extraction for specific file types
        extraction_extensions = {'.pdf', '.docx', '.doc', '.xls', '.xlsx', '.ppt', '.pptx'}
        if file_extension in extraction_extensions:
            return ReadingStrategy.CONTENT_EXTRACTION
        
        # Strategy selection based on file size and type
        if category == FileSizeCategory.SMALL:
            return ReadingStrategy.LAZY_LOADING
        elif category == FileSizeCategory.MEDIUM:
            return ReadingStrategy.LAZY_LOADING
        elif category == FileSizeCategory.LARGE:
            return ReadingStrategy.CHUNKED_READING
        else:  # VERY_LARGE
            return ReadingStrategy.MEMORY_MAPPING
    
    def read_content(self, file_path: str, **kwargs) -> Optional[str]:
        """Read file content with specified options."""
        # Normalize file path
        if not os.path.isabs(file_path):
            file_path = os.path.join(self.base_path, file_path)
        
        try:
            # Get file extension
            file_extension = os.path.splitext(file_path)[1]
            
            # Determine best strategy
            strategy = self._determine_best_strategy(file_path, file_extension)
            
            # Read content using selected strategy
            content = self.strategies[strategy].read_content(file_path, **kwargs)
            
            # Log the strategy used
            logger.debug(f"Used {strategy.value} strategy for {file_path}")
            
            return content
            
        except Exception as e:
            logger.error(f"Failed to read content from {file_path}: {e}")
            return None
    
    def read_metadata(self, file_path: str) -> Optional[Dict[str, Any]]:
        """Read file metadata without loading content."""
        # Normalize file path
        if not os.path.isabs(file_path):
            file_path = os.path.join(self.base_path, file_path)
        
        # Check cache first
        cache_key = file_path
        with self._cache_lock:
            if cache_key in self._metadata_cache:
                # Return a copy of the cached metadata as a dictionary
                metadata = self._metadata_cache[cache_key]
                return {
                    'file_path': metadata.file_path,
                    'size': metadata.size,
                    'last_modified': metadata.last_modified,
                    'extension': metadata.extension,
                    'checksum': metadata.checksum,
                    'line_count': metadata.line_count,
                    'category': metadata.category.value if metadata.category else None,
                    'strategy': metadata.strategy.value if metadata.strategy else None,
                    'human_readable_size': self._format_file_size(metadata.size)
                }
        
        try:
            # Get file stats
            stat_info = os.stat(file_path)
            
            # Get file extension
            file_extension = os.path.splitext(file_path)[1]
            
            # Determine file category and strategy
            category = self._determine_file_category(file_path)
            strategy = self._determine_best_strategy(file_path, file_extension)
            
            # Calculate checksum
            checksum = self._calculate_checksum(file_path)
            
            # Get line count
            line_count = self._count_lines(file_path)
            
            # Create metadata object
            metadata = FileMetadata(
                file_path=file_path,
                size=stat_info.st_size,
                last_modified=stat_info.st_mtime,
                extension=file_extension,
                checksum=checksum,
                line_count=line_count,
                category=category,
                strategy=strategy
            )
            
            # Convert to dictionary
            metadata_dict = {
                'file_path': metadata.file_path,
                'size': metadata.size,
                'last_modified': metadata.last_modified,
                'extension': metadata.extension,
                'checksum': metadata.checksum,
                'line_count': metadata.line_count,
                'category': metadata.category.value if metadata.category else None,
                'strategy': metadata.strategy.value if metadata.strategy else None,
                'human_readable_size': self._format_file_size(metadata.size)
            }
            
            # Cache the metadata
            with self._cache_lock:
                self._metadata_cache[cache_key] = metadata
            
            return metadata_dict
            
        except OSError as e:
            logger.error(f"Failed to read metadata for {file_path}: {e}")
            return None
    
    def read_in_chunks(self, file_path: str, chunk_size: int = 4*1024*1024) -> Iterator[str]:
        """Read file in chunks for memory efficiency."""
        # Normalize file path
        if not os.path.isabs(file_path):
            file_path = os.path.join(self.base_path, file_path)
        
        try:
            # Get file extension
            file_extension = os.path.splitext(file_path)[1]
            
            # Determine best strategy
            strategy = self._determine_best_strategy(file_path, file_extension)
            
            # Read in chunks using selected strategy
            yield from self.strategies[strategy].read_in_chunks(file_path, chunk_size)
            
        except Exception as e:
            logger.error(f"Failed to read chunks from {file_path}: {e}")
            raise
    
    def detect_errors(self, file_path: str) -> Dict[str, Any]:
        """Detect and report issues within files."""
        # Normalize file path
        if not os.path.isabs(file_path):
            file_path = os.path.join(self.base_path, file_path)
        
        try:
            # Get file extension
            file_extension = os.path.splitext(file_path)[1]
            
            # Get content for error detection
            content = self.read_content(file_path)
            
            # Detect errors
            errors = self.error_detector.detect_errors(file_path, content, file_extension)
            
            # Log detected errors
            for error in errors:
                log_level = logging.ERROR if error.severity == "error" else logging.WARNING
                logger.log(log_level, f"Error in {file_path}: {error.message} (line {error.line_number})")
            
            # Return structured error information
            return {
                'has_errors': any(error.severity == 'error' for error in errors),
                'has_warnings': any(error.severity == 'warning' for error in errors),
                'errors': [error for error in errors if error.severity == 'error'],
                'warnings': [error for error in errors if error.severity in ['warning', 'info']],
                'total_errors': len(errors),
                'error_summary': f"Found {len([e for e in errors if e.severity == 'error'])} errors and {len([e for e in errors if e.severity == 'warning'])} warnings"
            }
            
        except Exception as e:
            logger.error(f"Failed to detect errors in {file_path}: {e}")
            return {
                'has_errors': True,
                'has_warnings': False,
                'errors': [FileError(
                    error_type="detection_error",
                    message=f"Failed to detect errors: {e}",
                    severity="error"
                )],
                'warnings': [],
                'total_errors': 1,
                'error_summary': f"Error detection failed: {e}"
            }
    
    def _calculate_checksum(self, file_path: str) -> Optional[str]:
        """Calculate SHA-256 checksum of file."""
        try:
            hasher = hashlib.sha256()
            with open(file_path, 'rb') as f:
                for chunk in iter(lambda: f.read(4096), b""):
                    hasher.update(chunk)
            return hasher.hexdigest()
        except Exception as e:
            logger.warning(f"Failed to calculate checksum for {file_path}: {e}")
            return None
    
    def _count_lines(self, file_path: str) -> Optional[int]:
        """Count lines in a file."""
        try:
            with open(file_path, 'rb') as f:
                return sum(1 for _ in f)
        except Exception as e:
            logger.warning(f"Failed to count lines for {file_path}: {e}")
            return None
    
    def _format_file_size(self, size_bytes: int) -> str:
        """Format file size in human readable format."""
        for unit in ['B', 'KB', 'MB', 'GB']:
            if size_bytes < 1024.0:
                return f"{size_bytes:.1f}{unit}"
            size_bytes /= 1024.0
        return f"{size_bytes:.1f}TB"
    
    def get_file_info(self, file_path: str) -> Dict[str, Any]:
        """Get comprehensive file information including content, metadata, and errors."""
        # Normalize file path
        if not os.path.isabs(file_path):
            file_path = os.path.join(self.base_path, file_path)
        
        # Get file extension
        file_extension = os.path.splitext(file_path)[1]
        
        # Determine file category and strategy
        category = self._determine_file_category(file_path)
        strategy = self._determine_best_strategy(file_path, file_extension)
        
        # Check if file is binary
        is_binary = self._is_binary_file(file_path)
        
        # Calculate estimated read time
        estimated_read_time_ms = self._estimate_read_time_ms(file_path)
        
        # Calculate memory efficiency score
        memory_efficiency_score = self._calculate_memory_efficiency_score(file_path, strategy)
        
        info = {
            'file_path': file_path,
            'strategy_used': strategy,
            'file_size_category': category,
            'is_binary': is_binary,
            'encoding': 'utf-8' if not is_binary else None,
            'estimated_read_time_ms': estimated_read_time_ms,
            'memory_efficiency_score': memory_efficiency_score,
            'metadata': self.read_metadata(file_path),
            'errors': [],
            'warnings': []
        }
        
        # Detect errors
        errors_result = self.detect_errors(file_path)
        if isinstance(errors_result, dict):
            info['errors'] = errors_result.get('errors', [])
            info['warnings'] = errors_result.get('warnings', [])
        else:
            # Handle legacy format (list of FileError objects)
            info['errors'] = [error for error in errors_result if error.severity == 'error']
            info['warnings'] = [error for error in errors_result if error.severity in ['warning', 'info']]
        
        return info
    
    def _is_binary_file(self, file_path: str) -> bool:
        """Check if file is binary."""
        try:
            with open(file_path, 'rb') as f:
                chunk = f.read(1024)
                return b'\x00' in chunk
        except Exception:
            return False
    
    def _estimate_read_time_ms(self, file_path: str) -> int:
        """Estimate read time in milliseconds."""
        try:
            file_size = os.path.getsize(file_path)
            # Assume 10MB/s read speed
            return int((file_size / (10 * 1024 * 1024)) * 1000)
        except Exception:
            return 0
    
    def _calculate_memory_efficiency_score(self, file_path: str, strategy: ReadingStrategy) -> float:
        """Calculate memory efficiency score (0.0 to 1.0)."""
        try:
            file_size = os.path.getsize(file_path)
            
            # Strategy-based scoring
            strategy_scores = {
                ReadingStrategy.LAZY_LOADING: 0.9,
                ReadingStrategy.CHUNKED_READING: 0.7,
                ReadingStrategy.MEMORY_MAPPING: 0.8,
                ReadingStrategy.CONTENT_EXTRACTION: 0.6
            }
            
            base_score = strategy_scores.get(strategy, 0.5)
            
            # Adjust based on file size (smaller files = more efficient)
            if file_size < 1024 * 1024:  # < 1MB
                size_multiplier = 1.0
            elif file_size < 10 * 1024 * 1024:  # < 10MB
                size_multiplier = 0.9
            elif file_size < 100 * 1024 * 1024:  # < 100MB
                size_multiplier = 0.7
            else:
                size_multiplier = 0.5
            
            return min(1.0, base_score * size_multiplier)
        except Exception:
            return 0.5
    
    def clear_cache(self):
        """Clear the metadata cache."""
        with self._cache_lock:
            self._metadata_cache.clear()
    
    def get_cache_stats(self) -> Dict[str, Any]:
        """Get cache statistics."""
        with self._cache_lock:
            return {
                'cache_size': len(self._metadata_cache),
                'cached_files': list(self._metadata_cache.keys())
            }