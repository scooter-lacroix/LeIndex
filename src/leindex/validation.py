"""
Validation Framework Module

This module provides a centralized validation framework for all file path
and directory operations in the LeIndex system.

VALIDATION FRAMEWORK:
- Decorator-based validation for consistent application
- Type checking and sanitization
- Security checks against path traversal
- Comprehensive error reporting
- Configurable validation policies

USAGE EXAMPLE:
    @validate_file_path(allow_relative=False, check_existence=True)
    def process_file(file_path: str):
        # This function is guaranteed to receive a validated, absolute path
        pass
"""

import os
import functools
from typing import Callable, Optional, Union, List, Any
from pathlib import Path
from enum import Enum

from .logger_config import logger


class ValidationPolicy(Enum):
    """
    Validation policy levels for different security requirements.

    Levels:
        - STRICT: Full validation including existence checks (slowest, safest)
        - STANDARD: Full validation without existence checks (balanced)
        - PERMISSIVE: Basic format validation only (fastest, least safe)
    """
    STRICT = "strict"
    STANDARD = "standard"
    PERMISSIVE = "permissive"


class ValidationError(Exception):
    """Raised when validation fails."""

    def __init__(self, message: str, path: str, reason: str):
        self.message = message
        self.path = path
        self.reason = reason
        super().__init__(self.message)

    def __str__(self):
        return f"ValidationError: {self.message} (path={self.path}, reason={self.reason})"


class PathValidator:
    """
    Centralized path validation utility.

    VALIDATION CHECKS:
    1. Type checking (must be string)
    2. Non-empty after stripping
    3. No null bytes
    4. No obvious path traversal attempts
    5. Optional: Absolute path requirement
    6. Optional: File/directory existence check
    7. Optional: File/directory type check
    """

    @staticmethod
    def validate(
        path: str,
        policy: ValidationPolicy = ValidationPolicy.STANDARD,
        allow_relative: bool = False,
        check_existence: bool = False,
        expect_file: bool = False,
        expect_dir: bool = False,
        allowed_extensions: Optional[List[str]] = None
    ) -> str:
        """
        Validate a file path according to the specified policy.

        Args:
            path: Path to validate
            policy: Validation policy level (default: STANDARD)
            allow_relative: Allow relative paths (default: False)
            check_existence: Check if path exists (default: False)
            expect_file: Expect a file (not directory)
            expect_dir: Expect a directory (not file)
            allowed_extensions: List of allowed file extensions (e.g., [".py", ".txt"])

        Returns:
            Normalized absolute path

        Raises:
            ValidationError: If validation fails
        """
        # Type checking
        if not isinstance(path, str):
            raise ValidationError(
                f"Path must be a string, got {type(path).__name__}",
                path=str(path),
                reason="invalid_type"
            )

        # Basic format validation
        if not path or not path.strip():
            raise ValidationError(
                "Path cannot be empty or whitespace-only",
                path=path,
                reason="empty_path"
            )

        # Null byte check (security issue)
        if '\0' in path:
            raise ValidationError(
                "Path contains null bytes (security risk)",
                path=path,
                reason="null_bytes"
            )

        # Path traversal check
        if '../' in path or '..\\' in path:
            raise ValidationError(
                "Path contains obvious traversal attempts",
                path=path,
                reason="path_traversal"
            )

        # Absolute path requirement
        if not allow_relative:
            if not os.path.isabs(path):
                raise ValidationError(
                    "Path must be absolute",
                    path=path,
                    reason="not_absolute"
                )

        # Normalize path
        normalized = os.path.abspath(path) if not allow_relative else os.path.normpath(path)

        # Existence check (for STRICT policy or explicit request)
        if check_existence or policy == ValidationPolicy.STRICT:
            if not os.path.exists(normalized):
                raise ValidationError(
                    "Path does not exist",
                    path=normalized,
                    reason="not_found"
                )

            # Type checks
            if expect_file and not os.path.isfile(normalized):
                raise ValidationError(
                    "Path is not a file",
                    path=normalized,
                    reason="not_file"
                )

            if expect_dir and not os.path.isdir(normalized):
                raise ValidationError(
                    "Path is not a directory",
                    path=normalized,
                    reason="not_directory"
                )

        # Extension check
        if allowed_extensions:
            ext = os.path.splitext(normalized)[1].lower()
            if ext not in [e.lower() for e in allowed_extensions]:
                raise ValidationError(
                    f"File extension '{ext}' not in allowed list: {allowed_extensions}",
                    path=normalized,
                    reason="invalid_extension"
                )

        return normalized


def validate_file_path(
    policy: ValidationPolicy = ValidationPolicy.STANDARD,
    allow_relative: bool = False,
    check_existence: bool = False,
    expect_file: bool = False,
    expect_dir: bool = False,
    allowed_extensions: Optional[List[str]] = None,
    arg_name: str = "file_path"
):
    """
    Decorator to validate file path arguments.

    This decorator validates all file path arguments before the function
    is called, ensuring consistent validation across the codebase.

    Args:
        policy: Validation policy level (default: STANDARD)
        allow_relative: Allow relative paths (default: False)
        check_existence: Check if path exists (default: False)
        expect_file: Expect a file (not directory)
        expect_dir: Expect a directory (not file)
        allowed_extensions: List of allowed file extensions
        arg_name: Name of the argument to validate (default: "file_path")

    Example:
        @validate_file_path(check_existence=True, expect_file=True)
        def read_file_content(file_path: str) -> str:
            with open(file_path, 'r') as f:
                return f.read()

        @validate_file_path(expect_dir=True)
        def scan_directory(directory: str) -> List[str]:
            return os.listdir(directory)
    """
    def decorator(func: Callable) -> Callable:
        @functools.wraps(func)
        def wrapper(*args, **kwargs):
            # Validate the specified argument
            # Try both args and kwargs to find the argument
            validated_path = None

            # Check kwargs first
            if arg_name in kwargs:
                path = kwargs[arg_name]
                try:
                    validated_path = PathValidator.validate(
                        path,
                        policy=policy,
                        allow_relative=allow_relative,
                        check_existence=check_existence,
                        expect_file=expect_file,
                        expect_dir=expect_dir,
                        allowed_extensions=allowed_extensions
                    )
                    kwargs[arg_name] = validated_path
                except ValidationError as e:
                    logger.error(
                        f"Validation failed for {func.__name__}: {e}",
                        extra={'component': 'validate_file_path', 'function': func.__name__, 'path': path}
                    )
                    raise

            # Check args by position (inspect signature)
            else:
                import inspect
                sig = inspect.signature(func)
                param_names = list(sig.parameters.keys())

                if arg_name in param_names:
                    arg_index = param_names.index(arg_name)
                    if arg_index < len(args):
                        path = args[arg_index]
                        try:
                            validated_path = PathValidator.validate(
                                path,
                                policy=policy,
                                allow_relative=allow_relative,
                                check_existence=check_existence,
                                expect_file=expect_file,
                                expect_dir=expect_dir,
                                allowed_extensions=allowed_extensions
                            )
                            # Convert args to list for modification
                            args_list = list(args)
                            args_list[arg_index] = validated_path
                            args = tuple(args_list)
                        except ValidationError as e:
                            logger.error(
                                f"Validation failed for {func.__name__}: {e}",
                                extra={'component': 'validate_file_path', 'function': func.__name__, 'path': path}
                            )
                            raise

            return func(*args, **kwargs)

        return wrapper
    return decorator


def validate_multiple_paths(
    arg_names: List[str],
    policy: ValidationPolicy = ValidationPolicy.STANDARD,
    **validation_kwargs
):
    """
    Decorator to validate multiple file path arguments.

    Args:
        arg_names: List of argument names to validate
        policy: Validation policy level (default: STANDARD)
        **validation_kwargs: Additional validation arguments

    Example:
        @validate_multiple_paths(['source', 'dest'], check_existence=True)
        def copy_file(source: str, dest: str) -> None:
            shutil.copy(source, dest)
    """
    def decorator(func: Callable) -> Callable:
        # Apply validation for each argument
        for arg_name in arg_names:
            func = validate_file_path(
                policy=policy,
                arg_name=arg_name,
                **validation_kwargs
            )(func)

        return func
    return decorator


# Convenience functions for common validation patterns

def validate_absolute_path(path: str) -> str:
    """
    Quick validation for absolute paths (no existence check).

    Args:
        path: Path to validate

    Returns:
        Normalized absolute path

    Raises:
        ValidationError: If validation fails
    """
    return PathValidator.validate(
        path,
        policy=ValidationPolicy.PERMISSIVE,
        allow_relative=False,
        check_existence=False
    )


def validate_existing_path(path: str, expect_file: bool = False) -> str:
    """
    Quick validation for existing paths.

    Args:
        path: Path to validate
        expect_file: True if expecting a file, False for directory or file

    Returns:
        Normalized absolute path

    Raises:
        ValidationError: If validation fails
    """
    return PathValidator.validate(
        path,
        policy=ValidationPolicy.STRICT,
        allow_relative=False,
        check_existence=True,
        expect_file=expect_file
    )
