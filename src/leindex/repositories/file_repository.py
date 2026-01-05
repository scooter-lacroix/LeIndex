"""
File Repository - Business logic for file operations.

ARCHITECTURAL FIX (Issue #3 - Missing Repository Pattern):
-------------------------------------------------------
This repository encapsulates all business logic related to file operations,
separating it from the data access layer (DAL).

Business Rules Implemented:
1. File path validation and normalization
2. Duplicate file detection
3. File metadata enrichment
4. Audit trail for file operations
5. Transaction management for file operations
"""

import logging
import hashlib
from typing import Optional, Dict, List, Any
from datetime import datetime
import os

from .base import Repository, RepositoryError, NotFoundError, ValidationError, DuplicateError, OperationError
from ..storage.storage_interface import DALInterface

logger = logging.getLogger(__name__)


class FileRepository(Repository):
    """
    Repository for file-related operations with business logic.

    ARCHITECTURAL FIX (Issue #3):
    ----------------------------
    Encapsulates business rules for file operations that were previously
    scattered across DAL implementations.

    Responsibilities:
    - File validation and normalization
    - Duplicate detection and handling
    - Metadata enrichment
    - Audit logging
    - Business rule enforcement
    """

    def __init__(self, dal: DALInterface, enable_audit: bool = True):
        """
        Initialize the file repository.

        Args:
            dal: Data Access Layer instance
            enable_audit: Whether to enable audit logging
        """
        super().__init__(dal)
        self._enable_audit = enable_audit

        # Require metadata backend
        if not self._dal.metadata:
            raise ValueError("FileRepository requires a DAL with metadata support")

    # ========================================================================
    # CREATE operations with business logic
    # ========================================================================

    def add_file_with_validation(
        self,
        file_path: str,
        file_type: str,
        extension: str,
        content: Optional[str] = None,
        metadata: Optional[Dict[str, Any]] = None,
        skip_duplicate_check: bool = False
    ) -> Dict[str, Any]:
        """
        Add a file with validation and business logic.

        Business Rules:
        1. Validate and normalize file path
        2. Check for duplicates (optional)
        3. Enrich metadata with hash, size, timestamp
        4. Store content and metadata atomically

        Args:
            file_path: Path of the file
            file_type: Type of file (file, directory, etc.)
            extension: File extension
            content: Optional file content
            metadata: Optional additional metadata
            skip_duplicate_check: Skip duplicate detection

        Returns:
            Created file information

        Raises:
            ValidationError: If validation fails
            DuplicateError: If file already exists
            OperationError: If creation fails
        """
        # Validate inputs
        normalized_path = self.validate_file_path(file_path)

        if not file_type:
            raise ValidationError("File", "file_type", file_type, "File type cannot be empty")

        # Check for duplicates
        if not skip_duplicate_check:
            existing = self._dal.metadata.get_file_info(normalized_path)
            if existing:
                raise DuplicateError("File", normalized_path)

        # Enrich metadata
        enriched_metadata = self._enrich_metadata(normalized_path, content, metadata)

        # Store file content if provided
        if content is not None and self._dal.storage:
            try:
                self._dal.storage.save_file_content(normalized_path, content)
            except Exception as e:
                self._handle_error("save_file_content", e)

        # Add file to metadata store
        try:
            success = self._dal.metadata.add_file(
                file_path=normalized_path,
                file_type=file_type,
                extension=extension,
                metadata=enriched_metadata
            )

            if not success:
                raise OperationError("add_file", "File", "DAL operation returned False")

        except Exception as e:
            # Rollback content storage if metadata fails
            try:
                if self._dal.storage:
                    self._dal.storage.delete_file_content(normalized_path)
            except:
                pass  # Best effort rollback
            self._handle_error("add_file_metadata", e)

        # Audit log
        if self._enable_audit:
            self._audit_log("file_created", normalized_path, {
                "file_type": file_type,
                "extension": extension,
                "size": len(content) if content else 0
            })

        return self._dal.metadata.get_file_info(normalized_path)

    def _enrich_metadata(
        self,
        file_path: str,
        content: Optional[str],
        metadata: Optional[Dict[str, Any]]
    ) -> Dict[str, Any]:
        """
        Enrich file metadata with computed fields.

        Args:
            file_path: File path
            content: File content
            metadata: Existing metadata

        Returns:
            Enriched metadata dictionary
        """
        enriched = metadata.copy() if metadata else {}

        # Add hash if content provided
        if content is not None:
            enriched["hash"] = self._compute_hash(content)
            enriched["size"] = len(content)
            enriched["line_count"] = content.count("\n") + 1

        # Add timestamps
        now = datetime.utcnow().isoformat()
        enriched["indexed_at"] = now
        if "created_at" not in enriched:
            enriched["created_at"] = now

        # Add file extension extraction if not provided
        if "extension" not in enriched:
            _, ext = os.path.splitext(file_path)
            enriched["extension"] = ext.lstrip(".")

        return enriched

    def _compute_hash(self, content: str) -> str:
        """Compute SHA-256 hash of content."""
        return hashlib.sha256(content.encode('utf-8')).hexdigest()

    # ========================================================================
    # READ operations
    # ========================================================================

    def get_file_info(self, file_path: str) -> Dict[str, Any]:
        """
        Get file information with business logic.

        Args:
            file_path: Path of the file

        Returns:
            File information dictionary

        Raises:
            NotFoundError: If file not found
            ValidationError: If path is invalid
        """
        normalized_path = self.validate_file_path(file_path)

        result = self._dal.metadata.get_file_info(normalized_path)
        if not result:
            raise NotFoundError("File", normalized_path)

        return result

    def get_file_content(self, file_path: str) -> str:
        """
        Get file content with business logic.

        Args:
            file_path: Path of the file

        Returns:
            File content

        Raises:
            NotFoundError: If file not found
            ValidationError: If path is invalid
        """
        normalized_path = self.validate_file_path(file_path)

        if not self._dal.storage:
            raise RepositoryError("Storage backend not available", "File")

        content = self._dal.storage.get_file_content(normalized_path)
        if content is None:
            raise NotFoundError("FileContent", normalized_path)

        return content

    def list_files_by_extension(self, extension: str) -> List[Dict[str, Any]]:
        """
        List all files with a specific extension.

        Args:
            extension: File extension to filter by (e.g., 'py')

        Returns:
            List of file information dictionaries
        """
        all_files = self._dal.metadata.get_all_files()
        normalized_ext = extension.lstrip(".")

        return [
            {**info, "path": path}
            for path, info in all_files
            if info.get("extension", "").lower() == normalized_ext.lower()
        ]

    def get_directory_structure(self, directory_path: str = "") -> Dict[str, Any]:
        """
        Get directory structure with business logic.

        Args:
            directory_path: Optional directory path

        Returns:
            Directory structure dictionary
        """
        normalized_path = directory_path
        if directory_path:
            normalized_path = self.validate_file_path(directory_path)

        return self._dal.metadata.get_directory_structure(normalized_path)

    # ========================================================================
    # UPDATE operations
    # ========================================================================

    def update_file_metadata(self, file_path: str, metadata: Dict[str, Any]) -> Dict[str, Any]:
        """
        Update file metadata with validation.

        Args:
            file_path: Path of the file
            metadata: New metadata

        Returns:
            Updated file information

        Raises:
            NotFoundError: If file not found
            ValidationError: If validation fails
        """
        normalized_path = self.validate_file_path(file_path)

        # Check file exists
        existing = self._dal.metadata.get_file_info(normalized_path)
        if not existing:
            raise NotFoundError("File", normalized_path)

        # Update metadata
        try:
            self._dal.metadata.save_file_metadata(normalized_path, metadata)
        except Exception as e:
            self._handle_error("update_metadata", e)

        # Audit log
        if self._enable_audit:
            self._audit_log("file_metadata_updated", normalized_path, {
                "updated_fields": list(metadata.keys())
            })

        return self._dal.metadata.get_file_info(normalized_path)

    # ========================================================================
    # DELETE operations
    # ========================================================================

    def delete_file(self, file_path: str, delete_content: bool = True) -> bool:
        """
        Delete a file with cascading business logic.

        Args:
            file_path: Path of the file
            delete_content: Whether to delete content as well

        Returns:
            True if deleted

        Raises:
            NotFoundError: If file not found
            OperationError: If deletion fails
        """
        normalized_path = self.validate_file_path(file_path)

        # Check file exists
        existing = self._dal.metadata.get_file_info(normalized_path)
        if not existing:
            raise NotFoundError("File", normalized_path)

        # Delete metadata
        try:
            success = self._dal.metadata.remove_file(normalized_path)
            if not success:
                raise OperationError("remove_file", "File", "DAL operation returned False")
        except Exception as e:
            self._handle_error("delete_metadata", e)

        # Delete content if requested
        if delete_content and self._dal.storage:
            try:
                self._dal.storage.delete_file_content(normalized_path)
            except Exception as e:
                self._handle_error("delete_content", e)

        # Delete from search index
        if self._dal.search:
            try:
                self._dal.search.delete_indexed_file(normalized_path)
            except Exception as e:
                # Non-critical, log and continue
                self._logger.warning(f"Failed to delete from search index: {e}")

        # Audit log
        if self._enable_audit:
            self._audit_log("file_deleted", normalized_path, {
                "delete_content": delete_content
            })

        return True

    # ========================================================================
    # Abstract method implementations
    # ========================================================================

    def get_by_id(self, identifier: str) -> Optional[Dict[str, Any]]:
        """Get file by path (identifier)."""
        return self._dal.metadata.get_file_info(identifier)

    def list_all(self, limit: Optional[int] = None, offset: int = 0) -> List[Dict[str, Any]]:
        """List all files."""
        all_files = self._dal.metadata.get_all_files()

        if offset:
            all_files = all_files[offset:]

        if limit:
            all_files = all_files[:limit]

        return [{**info, "path": path} for path, info in all_files]

    def create(self, data: Dict[str, Any]) -> Dict[str, Any]:
        """Create a file from data dictionary."""
        return self.add_file_with_validation(
            file_path=data.get("file_path", ""),
            file_type=data.get("file_type", "file"),
            extension=data.get("extension", ""),
            content=data.get("content"),
            metadata=data.get("metadata")
        )

    def update(self, identifier: str, data: Dict[str, Any]) -> Dict[str, Any]:
        """Update a file."""
        return self.update_file_metadata(identifier, data)

    def delete(self, identifier: str) -> bool:
        """Delete a file."""
        return self.delete_file(identifier)

    def exists(self, identifier: str) -> bool:
        """Check if file exists."""
        return self._dal.metadata.get_file_info(identifier) is not None

    def count(self) -> int:
        """Count all files."""
        return self._dal.metadata.size()

    # ========================================================================
    # Audit logging
    # ========================================================================

    def _audit_log(self, action: str, file_path: str, details: Dict[str, Any]) -> None:
        """Log an audit event."""
        audit_data = {
            "timestamp": datetime.utcnow().isoformat(),
            "action": action,
            "file_path": file_path,
            "details": details
        }
        self._logger.info(f"AUDIT: {audit_data}")
