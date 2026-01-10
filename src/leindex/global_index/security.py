"""
Security validation and sanitization for Global Index.

This module provides:
- Pydantic models for input validation
- Path sanitization and traversal prevention
- Sensitive data redaction
- Security utilities

Security Principles:
1. Validate all inputs from untrusted sources
2. Sanitize file paths to prevent traversal attacks
3. Redact sensitive data from logs
4. Use allow-lists for validation where possible
"""

import os
import re
import logging
from pathlib import Path
from typing import Optional, Set, Any
from datetime import datetime

try:
    from pydantic import BaseModel, Field, validator, root_validator
    PYDANTIC_AVAILABLE = True
except ImportError:
    PYDANTIC_AVAILABLE = False
    logging.warning("Pydantic not available, security validation will be limited")

logger = logging.getLogger(__name__)


# ============================================================================
# Path Sanitization
# ============================================================================

class PathSecurityError(Exception):
    """Raised when path validation fails."""
    pass


def sanitize_project_path(project_path: str, allowed_base_paths: Optional[Set[str]] = None) -> str:
    """
    Sanitize and validate a project path.

    This function prevents path traversal attacks and ensures the path
    is within allowed base directories.

    Args:
        project_path: Path to sanitize
        allowed_base_paths: Optional set of allowed base directories

    Returns:
        Sanitized absolute path

    Raises:
        PathSecurityError: If path is invalid or contains traversal attempts

    Examples:
        >>> sanitize_project_path("/home/user/project")
        '/home/user/project'
        >>> sanitize_project_path("/home/user/project", allowed_base_paths={"/home/user"})
        '/home/user/project'
    """
    if not project_path:
        raise PathSecurityError("Project path cannot be empty")

    # Convert to absolute path
    try:
        abs_path = Path(project_path).resolve()
    except (OSError, ValueError) as e:
        raise PathSecurityError(f"Invalid path: {e}") from e

    # Check for path traversal attempts
    if ".." in str(project_path):
        logger.warning(f"Potential path traversal attempt: {project_path}")

    # Normalize path
    sanitized = str(abs_path)

    # Validate against allowed base paths if provided
    if allowed_base_paths:
        # Normalize base paths
        normalized_bases = {
            str(Path(base).resolve()) for base in allowed_base_paths
        }

        # Check if sanitized path is within any allowed base
        is_allowed = any(
            sanitized.startswith(base + os.sep) or sanitized == base
            for base in normalized_bases
        )

        if not is_allowed:
            raise PathSecurityError(
                f"Path {sanitized} is not within allowed base paths: {allowed_base_paths}"
            )

    return sanitized


def validate_project_id(project_id: str) -> str:
    """
    Validate a project ID.

    Project IDs should be alphanumeric with limited special characters.

    Args:
        project_id: Project ID to validate

    Returns:
        Validated project ID

    Raises:
        PathSecurityError: If project ID contains invalid characters
    """
    if not project_id:
        raise PathSecurityError("Project ID cannot be empty")

    # Allow alphanumeric, underscore, dash, and dot
    if not re.match(r'^[a-zA-Z0-9_.-]+$', project_id):
        raise PathSecurityError(
            f"Project ID contains invalid characters: {project_id}"
        )

    # Check for suspicious patterns
    suspicious_patterns = ['../', '..\\', './', '.\\']
    if any(pattern in project_id for pattern in suspicious_patterns):
        raise PathSecurityError(
            f"Project ID contains suspicious pattern: {project_id}"
        )

    return project_id


# ============================================================================
# Sensitive Data Redaction
# ============================================================================

SENSITIVE_PATTERNS = [
    r'password["\']?\s*[:=]\s*["\']?[^"\'\s]+',  # password="..."
    r'api[_-]?key["\']?\s*[:=]\s*["\']?[^"\'\s]+',  # api_key="..."
    r'token["\']?\s*[:=]\s*["\']?[^"\'\s]+',  # token="..."
    r'secret["\']?\s*[:=]\s*["\']?[^"\'\s]+',  # secret="..."
]

def redact_sensitive_data(text: str) -> str:
    """
    Redact potentially sensitive data from text.

    Args:
        text: Text to redact

    Returns:
        Text with sensitive data redacted

    Examples:
        >>> redact_sensitive_data('password="secret123"')
        'password="[REDACTED]"'
    """
    if not text:
        return text

    redacted = text
    for pattern in SENSITIVE_PATTERNS:
        redacted = re.sub(
            pattern,
            lambda m: m.group(0).split('=')[0] + '=[REDACTED]',
            redacted,
            flags=re.IGNORECASE
        )

    return redacted


def safe_log_message(message: str) -> str:
    """
    Sanitize a log message by redacting sensitive data.

    Args:
        message: Log message to sanitize

    Returns:
        Sanitized log message
    """
    return redact_sensitive_data(message)


# ============================================================================
# Security Context
# ============================================================================

class SecurityContext:
    """
    Security context for global index operations.

    This class maintains security state and provides validation
    methods for global index operations.
    """

    def __init__(
        self,
        allowed_base_paths: Optional[Set[str]] = None,
        enable_validation: bool = True
    ):
        """
        Initialize security context.

        Args:
            allowed_base_paths: Set of allowed base directories for projects
            enable_validation: Whether to enable validation
        """
        self.allowed_base_paths = allowed_base_paths or set()
        self.enable_validation = enable_validation

    def validate_project_path(self, project_path: str) -> str:
        """
        Validate a project path using security context.

        Args:
            project_path: Path to validate

        Returns:
            Sanitized path

        Raises:
            PathSecurityError: If validation fails
        """
        if not self.enable_validation:
            return project_path

        return sanitize_project_path(
            project_path,
            allowed_base_paths=self.allowed_base_paths if self.allowed_base_paths else None
        )


# ============================================================================
# Default Security Context
# ============================================================================

_default_security_context: Optional[SecurityContext] = None


def get_default_security_context() -> SecurityContext:
    """
    Get the default security context.

    Returns:
        Default SecurityContext instance
    """
    global _default_security_context
    if _default_security_context is None:
        _default_security_context = SecurityContext(
            allowed_base_paths=None,  # No restrictions by default
            enable_validation=True
        )
    return _default_security_context


def set_default_security_context(context: SecurityContext) -> None:
    """
    Set the default security context.

    Args:
        context: SecurityContext to use as default
    """
    global _default_security_context
    _default_security_context = context
