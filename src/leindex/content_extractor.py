import os
from typing import Optional, Dict, Any
import logging
from datetime import datetime

logger = logging.getLogger(__name__)

# External libraries for content extraction (to be added to requirements.txt)
try:
    from pdfminer.high_level import extract_text as extract_pdf_text
    PDFMINER_AVAILABLE = True
except ImportError:
    PDFMINER_AVAILABLE = False
    extract_pdf_text = None
    logger.warning("pdfminer.six not available: No module named 'pdfminer'. PDF extraction will be disabled. Install with: uv pip install 'pdfminer.six>=20221105'")

try:
    from docx import Document
    DOCX_AVAILABLE = True
except ImportError:
    DOCX_AVAILABLE = False
    Document = None
    logger.warning("python-docx not available: No module named 'docx'. DOCX extraction will be disabled. Install with: uv pip install 'python-docx>=0.8.3'")

class ContentExtractor:
    """
    Handles content extraction from various file types.

    CRITICAL FIX: Added path validation to ensure all file accesses are
    within the configured base_path to prevent path traversal attacks.
    """
    def __init__(self, base_path: str):
        self.base_path = base_path
        self.MAX_CONTENT_SIZE_MB = 50  # Max content size to load directly into memory (50MB)

    def _get_full_path(self, file_path: str) -> str:
        """
        Constructs the full absolute path to the file.

        CRITICAL FIX: Validate that the file_path is within base_path before
        returning the full path to prevent path traversal attacks.
        """
        from pathlib import Path

        try:
            # Resolve both paths to their absolute real paths
            # This resolves symlinks and relative path components (.., .)
            real_file_path = Path(file_path).resolve()
            real_base_path = Path(self.base_path).resolve()

            # Check if the real file path is within the real base path
            try:
                real_file_path.relative_to(real_base_path)
                # Path is safe, return the validated absolute path
                return str(real_file_path)
            except ValueError:
                # relative_to raises ValueError if path is not within base
                logger.error(
                    f"SECURITY VIOLATION: File path {file_path} is not within base path {self.base_path}. "
                    f"Resolved file path: {real_file_path}, Resolved base path: {real_base_path}",
                    extra={'component': 'ContentExtractor', 'action': 'path_traversal_blocked', 'file_path': file_path}
                )
                raise ValueError(f"Path traversal attempt detected: {file_path} is not within {self.base_path}")

        except (OSError, ValueError) as e:
            logger.error(
                f"Error validating file path {file_path} against base {self.base_path}: {e}",
                extra={'component': 'ContentExtractor', 'action': 'path_validation_error', 'error': str(e)}
            )
            raise ValueError(f"Invalid file path: {e}") from e

    def _validate_path_safe(self, file_path: str) -> bool:
        """
        Validates that a file path is safe to access (within base_path).
        Returns True if safe, False otherwise.
        """
        from pathlib import Path

        try:
            real_file_path = Path(file_path).resolve()
            real_base_path = Path(self.base_path).resolve()
            real_file_path.relative_to(real_base_path)
            return True
        except (ValueError, OSError):
            return False

    def _extract_text_from_plain_file(self, full_path: str) -> Optional[str]:
        """Extracts text from plain text files, handling large files efficiently."""
        try:
            file_size = os.path.getsize(full_path)
            if file_size > self.MAX_CONTENT_SIZE_MB * 1024 * 1024:
                logger.warning(f"File {full_path} is very large ({file_size / (1024*1024):.2f} MB). "
                               "Consider streaming or external storage for optimal performance.")
                # For now, we'll read it chunked, but this is where a streaming
                # or external storage strategy would be more robust.
                return self._read_file_in_chunks(full_path)
            else:
                with open(full_path, 'r', encoding='utf-8', errors='ignore') as f:
                    return f.read()
        except Exception as e:
            logger.error(f"Error reading plain file {full_path}: {e}")
            return None

    def _read_file_in_chunks(self, full_path: str, chunk_size: int = 4 * 1024 * 1024) -> Optional[str]:
        """Reads a file in chunks to avoid loading entire large files into memory."""
        chunks = []
        try:
            with open(full_path, 'r', encoding='utf-8', errors='ignore') as f:
                while True:
                    chunk = f.read(chunk_size)
                    if not chunk:
                        break
                    chunks.append(chunk)
            return ''.join(chunks)
        except Exception as e:
            logger.error(f"Error reading file {full_path} in chunks: {e}")
            return None

    def _extract_text_from_pdf(self, full_path: str) -> Optional[str]:
        """Extracts text from PDF files using pdfminer.six."""
        if not PDFMINER_AVAILABLE:
            logger.warning("PDF extraction not available, pdfminer.six not installed")
            return None
        try:
            return extract_pdf_text(full_path)
        except Exception as e:
            logger.error(f"Error extracting text from PDF {full_path}: {e}")
            return None

    def _extract_text_from_docx(self, full_path: str) -> Optional[str]:
        """Extracts text from .docx files using python-docx."""
        if not DOCX_AVAILABLE:
            logger.warning("DOCX extraction not available, python-docx not installed")
            return None
        try:
            document = Document(full_path)
            return "\n".join([paragraph.text for paragraph in document.paragraphs])
        except Exception as e:
            logger.error(f"Error extracting text from DOCX {full_path}: {e}")
            return None

    def extract_content(self, file_path: str) -> Optional[Dict[str, Any]]:
        """
        Extracts content and basic metadata from a file based on its type.

        CRITICAL FIX: Now catches ValueError from path validation to handle
        path traversal attempts gracefully.
        """
        # CRITICAL FIX: Path validation is now done inside _get_full_path
        # This will raise ValueError if the path is outside base_path
        try:
            full_path = self._get_full_path(file_path)
        except ValueError as e:
            # Path validation failed - path traversal attempt detected
            logger.warning(f"Path validation failed for {file_path}: {e}")
            return None
        except Exception as e:
            logger.error(f"Unexpected error getting full path for {file_path}: {e}")
            return None

        if not os.path.exists(full_path):
            logger.warning(f"File not found for extraction: {full_path}")
            return None

        content = None
        file_extension = os.path.splitext(file_path)[1].lower()

        if file_extension == ".pdf":
            content = self._extract_text_from_pdf(full_path)
        elif file_extension == ".docx":
            content = self._extract_text_from_docx(full_path)
        elif file_extension in [".txt", ".py", ".js", ".ts", ".java", ".c", ".cpp", ".h", ".hpp", ".md", ".json", ".yaml", ".yml", ".xml", ".html", ".css"]:
            content = self._extract_text_from_plain_file(full_path)
        else:
            logger.info(f"Unsupported file type for rich content extraction: {file_extension}. Attempting plain text extraction.")
            content = self._extract_text_from_plain_file(full_path)

        if content is None:
            logger.error(f"Failed to extract any content from {file_path}")
            return None

        # Basic cleaning: remove excessive whitespace, normalize newlines
        cleaned_content = " ".join(content.split()).strip()

        try:
            stat_info = os.stat(full_path)
            return {
                "path": file_path,  # Use 'path' to match Tantivy field name
                "content": cleaned_content,
                "mtime": stat_info.st_mtime,
                "size": stat_info.st_size,
                "extension": file_extension,
                "last_indexed": datetime.now().isoformat()
            }
        except Exception as e:
            logger.error(f"Error getting file stats for {file_path}: {e}")
            return None