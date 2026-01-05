"""
Index Repository - Business logic for indexing operations.

ARCHITECTURAL FIX (Issue #3 - Missing Repository Pattern):
-------------------------------------------------------
This repository encapsulates all business logic related to indexing operations
including version tracking and diff management.
"""

import logging
from typing import Optional, Dict, List, Any
from datetime import datetime
import uuid
import hashlib

from .base import Repository, RepositoryError, NotFoundError, ValidationError, OperationError
from ..storage.storage_interface import DALInterface

logger = logging.getLogger(__name__)


class IndexRepository(Repository):
    """
    Repository for indexing operations with business logic.

    ARCHITECTURAL FIX (Issue #3):
    ----------------------------
    Encapsulates business rules for indexing operations including
    version tracking and diff management.

    Responsibilities:
    - Version tracking with business rules
    - Diff generation and storage
    - Index consistency checks
    """

    def __init__(self, dal: DALInterface):
        """
        Initialize the index repository.

        Args:
            dal: Data Access Layer instance
        """
        super().__init__(dal)

        # Require metadata backend
        if not self._dal.metadata:
            raise ValueError("IndexRepository requires a DAL with metadata support")

    # ========================================================================
    # VERSION TRACKING operations
    # ========================================================================

    def create_file_version(
        self,
        file_path: str,
        content: str,
        previous_version_id: Optional[str] = None,
        operation_type: str = "modify"
    ) -> Dict[str, Any]:
        """
        Create a new file version with business logic.

        Business Rules:
        1. Generate unique version ID
        2. Compute content hash
        3. Validate content size
        4. Store version atomically
        5. Create diff if previous version exists

        Args:
            file_path: Path of the file
            content: File content
            previous_version_id: Optional previous version ID
            operation_type: Type of operation (create, modify, delete, rename)

        Returns:
            Created version information

        Raises:
            ValidationError: If validation fails
            OperationError: If creation fails
        """
        # Validate inputs
        normalized_path = self.validate_file_path(file_path)

        if content is None:
            raise ValidationError("Version", "content", content, "Content cannot be None")

        # Check content size
        content_size = len(content.encode('utf-8'))
        MAX_SIZE = 10 * 1024 * 1024  # 10MB
        if content_size > MAX_SIZE:
            raise ValidationError(
                "Version",
                "content",
                f"<{content_size} bytes>",
                f"Content exceeds maximum size of {MAX_SIZE} bytes"
            )

        # Generate version ID
        version_id = self._generate_version_id(normalized_path)
        content_hash = self._compute_hash(content)
        timestamp = datetime.utcnow().isoformat()

        # Insert version
        try:
            success = self._dal.metadata.insert_file_version(
                version_id=version_id,
                file_path=normalized_path,
                content=content,
                hash=content_hash,
                timestamp=timestamp,
                size=content_size
            )

            if not success:
                raise OperationError("insert_file_version", "FileVersion", "DAL operation returned False")

        except Exception as e:
            self._handle_error("create_version", e)

        # Create diff if previous version exists
        if previous_version_id:
            self._create_and_store_diff(
                file_path=normalized_path,
                previous_version_id=previous_version_id,
                current_version_id=version_id,
                operation_type=operation_type
            )

        return {
            "version_id": version_id,
            "file_path": normalized_path,
            "hash": content_hash,
            "timestamp": timestamp,
            "size": content_size
        }

    def get_file_version(self, version_id: str) -> Dict[str, Any]:
        """
        Get a file version by ID.

        Args:
            version_id: Version identifier

        Returns:
            Version information

        Raises:
            NotFoundError: If version not found
        """
        version = self._dal.metadata.get_file_version(version_id)

        if not version:
            raise NotFoundError("FileVersion", version_id)

        return version

    def get_file_versions(self, file_path: str, limit: int = 100) -> List[Dict[str, Any]]:
        """
        Get all versions for a file.

        Args:
            file_path: Path of the file
            limit: Maximum number of versions to return

        Returns:
            List of version information

        Raises:
            ValidationError: If path is invalid
        """
        normalized_path = self.validate_file_path(file_path)

        versions = self._dal.metadata.get_file_versions_for_path(normalized_path)

        return versions[-limit:] if limit else versions

    def get_latest_version(self, file_path: str) -> Optional[Dict[str, Any]]:
        """
        Get the latest version of a file.

        Args:
            file_path: Path of the file

        Returns:
            Latest version info or None if no versions exist
        """
        versions = self.get_file_versions(file_path, limit=1)
        return versions[0] if versions else None

    def _generate_version_id(self, file_path: str) -> str:
        """Generate a unique version ID."""
        unique_string = f"{file_path}:{datetime.utcnow().isoformat()}:{uuid.uuid4()}"
        return hashlib.sha256(unique_string.encode()).hexdigest()[:32]

    def _compute_hash(self, content: str) -> str:
        """Compute SHA-256 hash of content."""
        return hashlib.sha256(content.encode('utf-8')).hexdigest()

    # ========================================================================
    # DIFF operations
    # ========================================================================

    def _create_and_store_diff(
        self,
        file_path: str,
        previous_version_id: str,
        current_version_id: str,
        operation_type: str
    ) -> Dict[str, Any]:
        """
        Create and store a diff between two versions.

        Args:
            file_path: Path of the file
            previous_version_id: Previous version ID
            current_version_id: Current version ID
            operation_type: Type of operation

        Returns:
            Diff information

        Raises:
            NotFoundError: If versions not found
            OperationError: If diff creation fails
        """
        # Get versions
        previous = self._dal.metadata.get_file_version(previous_version_id)
        current = self._dal.metadata.get_file_version(current_version_id)

        if not previous:
            raise NotFoundError("FileVersion", previous_version_id)
        if not current:
            raise NotFoundError("FileVersion", current_version_id)

        # Generate diff
        diff_content = self._generate_diff(
            previous.get("content", ""),
            current.get("content", "")
        )

        # Store diff
        diff_id = self._generate_diff_id(file_path, previous_version_id, current_version_id)
        timestamp = datetime.utcnow().isoformat()

        try:
            success = self._dal.metadata.insert_file_diff(
                diff_id=diff_id,
                file_path=file_path,
                previous_version_id=previous_version_id,
                current_version_id=current_version_id,
                diff_content=diff_content,
                diff_type="unified",
                operation_type=operation_type,
                operation_details=None,
                timestamp=timestamp
            )

            if not success:
                raise OperationError("insert_file_diff", "FileDiff", "DAL operation returned False")

        except Exception as e:
            self._handle_error("store_diff", e)

        return {
            "diff_id": diff_id,
            "file_path": file_path,
            "operation_type": operation_type,
            "timestamp": timestamp
        }

    def _generate_diff(self, old_content: str, new_content: str) -> str:
        """Generate a unified diff between two contents."""
        import difflib

        old_lines = old_content.splitlines(keepends=True)
        new_lines = new_content.splitlines(keepends=True)

        diff = difflib.unified_diff(
            old_lines,
            new_lines,
            fromfile="old",
            tofile="new",
            lineterm=""
        )

        return "".join(diff)

    def _generate_diff_id(self, file_path: str, previous_id: str, current_id: str) -> str:
        """Generate a unique diff ID."""
        unique_string = f"{file_path}:{previous_id}:{current_id}"
        return hashlib.sha256(unique_string.encode()).hexdigest()[:32]

    def get_file_diffs(self, file_path: str) -> List[Dict[str, Any]]:
        """
        Get all diffs for a file.

        Args:
            file_path: Path of the file

        Returns:
            List of diff information

        Raises:
            ValidationError: If path is invalid
        """
        normalized_path = self.validate_file_path(file_path)
        return self._dal.metadata.get_file_diffs_for_path(normalized_path)

    # ========================================================================
    # Abstract method implementations
    # ========================================================================

    def get_by_id(self, identifier: str) -> Optional[Dict[str, Any]]:
        """Get a version by ID."""
        return self.get_file_version(identifier)

    def list_all(self, limit: Optional[int] = None, offset: int = 0) -> List[Dict[str, Any]]:
        """List all versions across all files."""
        # This is expensive - should be avoided
        raise NotImplementedError("Listing all versions is not supported")

    def create(self, data: Dict[str, Any]) -> Dict[str, Any]:
        """Create a version from data dictionary."""
        return self.create_file_version(
            file_path=data.get("file_path", ""),
            content=data.get("content", ""),
            previous_version_id=data.get("previous_version_id"),
            operation_type=data.get("operation_type", "modify")
        )

    def update(self, identifier: str, data: Dict[str, Any]) -> Dict[str, Any]:
        """Versions are immutable - raise error."""
        raise RepositoryError("File versions are immutable and cannot be updated", "FileVersion")

    def delete(self, identifier: str) -> bool:
        """Delete a version by ID."""
        # Version deletion is not typically supported
        # Could be implemented as soft delete
        raise NotImplementedError("Version deletion is not supported")

    def exists(self, identifier: str) -> bool:
        """Check if a version exists."""
        return self._dal.metadata.get_file_version(identifier) is not None

    def count(self) -> int:
        """Count all versions."""
        raise NotImplementedError("Counting all versions is not supported")
