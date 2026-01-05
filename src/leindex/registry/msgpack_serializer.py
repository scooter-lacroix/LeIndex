"""
MessagePack-based serializer for code index data.

This module provides serialization support for index data with format detection
and migration capabilities from the legacy pickle format to MessagePack.
"""

import os
import pickle
import hashlib
from pathlib import Path
from typing import Any, Optional
from enum import Enum
import logging

try:
    import msgpack
except ImportError:
    raise ImportError(
        "msgpack is required for serialization. "
        "Install it with: pip install msgpack"
    )

logger = logging.getLogger(__name__)


# ============================================================================
# Format Detection Constants
# ============================================================================

"""MessagePack magic bytes (first byte of most MessagePack data)."""
MSGPACK_MAGIC_PREFIX = bytes([0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
                               0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f,
                               0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97,
                               0x98, 0x99, 0x9a, 0x9b, 0x9c, 0x9d, 0x9e, 0x9f])

"""Pickle magic bytes (protocol 3+ starts with 0x80)."""
PICKLE_MAGIC = bytes([0x80])

"""MessagePack file extension."""
MSGPACK_EXT = ".msgpack"

"""Pickle file extension."""
PICKLE_EXT = ".pickle"


# ============================================================================
# Format Type Enumeration
# ============================================================================

class FormatType(Enum):
    """
    Enumeration of supported serialization formats.

    Attributes:
        MSGPACK: MessagePack binary format (current/writable format)
        PICKLE: Python pickle format (read-only, for migration)
        UNKNOWN: Unknown or unsupported format
    """
    MSGPACK = "msgpack"
    PICKLE = "pickle"
    UNKNOWN = "unknown"


# ============================================================================
# MessagePack Serializer
# ============================================================================

class MessagePackSerializer:
    """
    Serializer for code index data using MessagePack format.

    This class provides:
    - Format detection based on file extension and content inspection
    - Reading from MessagePack format
    - Reading from pickle format (for migration purposes)
    - Writing to MessagePack format
    - Atomic write operations (temp file + rename)

    The serializer prioritizes MessagePack for all new writes while maintaining
    backward compatibility with existing pickle files through read-only support.

    Attributes:
        use_bin_type: Whether to use MessagePack binary type for bytes
    """

    def __init__(self, use_bin_type: bool = True):
        """
        Initialize the MessagePack serializer.

        Args:
            use_bin_type: Whether to use MessagePack binary type (recommended)
        """
        self.use_bin_type = use_bin_type
        logger.debug(
            f"MessagePackSerializer initialized (use_bin_type={use_bin_type})"
        )

    # ------------------------------------------------------------------------
    # Format Detection
    # ------------------------------------------------------------------------

    def detect_format(self, file_path: str | Path) -> FormatType:
        """
        Detect the serialization format of a file.

        Detection strategy:
        1. Check file extension
        2. If extension is unknown, inspect file content

        Args:
            file_path: Path to the file to inspect

        Returns:
            Detected format type

        Examples:
            >>> serializer = MessagePackSerializer()
            >>> serializer.detect_format("index.msgpack")
            FormatType.MSGPACK

            >>> serializer.detect_format("index.pickle")
            FormatType.PICKLE
        """
        file_path = Path(file_path)

        # Check file extension first
        if file_path.suffix == MSGPACK_EXT:
            logger.debug(f"Detected MessagePack format by extension: {file_path}")
            return FormatType.MSGPACK
        elif file_path.suffix == PICKLE_EXT:
            logger.debug(f"Detected pickle format by extension: {file_path}")
            return FormatType.PICKLE

        # If no extension or unknown extension, inspect content
        if not file_path.exists():
            # Check if it has a recognized extension even if it doesn't exist
            if file_path.suffix == MSGPACK_EXT:
                return FormatType.MSGPACK
            elif file_path.suffix == PICKLE_EXT:
                return FormatType.PICKLE
            logger.debug(f"File does not exist, assuming unknown format: {file_path}")
            return FormatType.UNKNOWN

        try:
            with open(file_path, "rb") as f:
                first_byte = f.read(1)

                if not first_byte:
                    logger.debug(f"Empty file, assuming unknown format: {file_path}")
                    return FormatType.UNKNOWN

                # Check for pickle (protocol 3+ starts with 0x80 followed by protocol version)
                if first_byte == PICKLE_MAGIC:
                    # Read next byte to confirm pickle
                    second_byte = f.read(1)
                    if second_byte and 0x01 <= second_byte[0] <= 0x05:
                        logger.debug(f"Detected pickle format by content: {file_path}")
                        return FormatType.PICKLE

                # Check for MessagePack (most common markers)
                if first_byte[0] in MSGPACK_MAGIC_PREFIX:
                    logger.debug(f"Detected MessagePack format by content: {file_path}")
                    return FormatType.MSGPACK

                logger.debug(f"Could not detect format for: {file_path}")
                return FormatType.UNKNOWN

        except (IOError, OSError) as e:
            logger.error(f"Error detecting format for {file_path}: {e}")
            return FormatType.UNKNOWN

    # ------------------------------------------------------------------------
    # Reading Data
    # ------------------------------------------------------------------------

    def read(self, file_path: str | Path) -> Any:
        """
        Read and deserialize data from a file.

        This method automatically detects the format and deserializes accordingly:
        - MessagePack files are read using msgpack.unpackb
        - Pickle files are read using pickle.load (read-only, for migration)

        Args:
            file_path: Path to the file to read

        Returns:
            Deserialized data

        Raises:
            FileNotFoundError: If the file doesn't exist
            ValueError: If the format is unknown or unsupported
            IOError: If there's an error reading the file

        Examples:
            >>> serializer = MessagePackSerializer()
            >>> data = serializer.read("index.msgpack")
            >>> isinstance(data, dict)
            True
        """
        file_path = Path(file_path)

        if not file_path.exists():
            raise FileNotFoundError(f"File not found: {file_path}")

        format_type = self.detect_format(file_path)

        if format_type == FormatType.MSGPACK:
            return self._read_msgpack(file_path)
        elif format_type == FormatType.PICKLE:
            logger.info(f"Reading legacy pickle format for migration: {file_path}")
            return self._read_pickle(file_path)
        else:
            raise ValueError(
                f"Unknown or unsupported format for file: {file_path}"
            )

    def _read_msgpack(self, file_path: Path) -> Any:
        """
        Read data from a MessagePack file.

        Args:
            file_path: Path to the MessagePack file

        Returns:
            Deserialized data

        Raises:
            IOError: If there's an error reading or parsing the file
        """
        try:
            with open(file_path, "rb") as f:
                data = msgpack.unpackb(
                    f.read(),
                    raw=False
                )
            logger.debug(f"Successfully read MessagePack file: {file_path}")
            return data
        except (msgpack.exceptions.ExtraData,
                msgpack.exceptions.UnpackException) as e:
            logger.error(f"Error unpacking MessagePack from {file_path}: {e}")
            raise IOError(f"Failed to read MessagePack file: {e}") from e

    def _read_pickle(self, file_path: Path) -> Any:
        """
        Read data from a pickle file (read-only, for migration).

        Args:
            file_path: Path to the pickle file

        Returns:
            Deserialized data

        Raises:
            IOError: If there's an error reading or parsing the file
        """
        try:
            with open(file_path, "rb") as f:
                data = pickle.load(f)
            logger.debug(f"Successfully read pickle file: {file_path}")
            return data
        except (pickle.PickleError, EOFError) as e:
            logger.error(f"Error unpickling {file_path}: {e}")
            raise IOError(f"Failed to read pickle file: {e}") from e

    # ------------------------------------------------------------------------
    # Writing Data
    # ------------------------------------------------------------------------

    def write(self, file_path: str | Path, data: Any) -> None:
        """
        Serialize and write data to a file in MessagePack format.

        This method uses an atomic write pattern:
        1. Write to a temporary file
        2. Flush and sync the temporary file
        3. Rename the temporary file to the target path

        This ensures that the target file is never in a partially written state.

        Args:
            file_path: Path to the target file
            data: Data to serialize and write

        Raises:
            IOError: If there's an error writing the file

        Examples:
            >>> serializer = MessagePackSerializer()
            >>> serializer.write("index.msgpack", {"files": [...]})
        """
        file_path = Path(file_path)

        # Ensure parent directory exists
        file_path.parent.mkdir(parents=True, exist_ok=True)

        # Create temporary file path
        temp_file_path = file_path.with_suffix(file_path.suffix + ".tmp")

        try:
            # Serialize and write to temporary file
            packed_data = msgpack.packb(data, use_bin_type=self.use_bin_type)

            with open(temp_file_path, "wb") as f:
                f.write(packed_data)
                f.flush()
                os.fsync(f.fileno())  # Ensure data is written to disk

            # Atomic rename
            temp_file_path.replace(file_path)

            logger.debug(f"Successfully wrote MessagePack file: {file_path}")

        except (msgpack.exceptions.PackException, IOError, OSError) as e:
            logger.error(f"Error writing MessagePack to {file_path}: {e}")
            # Clean up temporary file if it exists
            if temp_file_path.exists():
                try:
                    temp_file_path.unlink()
                except OSError:
                    pass
            raise IOError(f"Failed to write MessagePack file: {e}") from e

    # ------------------------------------------------------------------------
    # Utility Methods
    # ------------------------------------------------------------------------

    def migrate(self, source_path: str | Path, target_path: str | Path) -> None:
        """
        Migrate data from pickle format to MessagePack format.

        This method:
        1. Reads data from the source file (pickle or MessagePack)
        2. Writes the data to the target file in MessagePack format
        3. Does NOT remove the source file (caller should verify success first)

        Args:
            source_path: Path to the source file
            target_path: Path to the target file (will be .msgpack format)

        Raises:
            FileNotFoundError: If the source file doesn't exist
            ValueError: If the source format is unknown
            IOError: If there's an error reading or writing

        Examples:
            >>> serializer = MessagePackSerializer()
            >>> serializer.migrate("index.pickle", "index.msgpack")
        """
        source_path = Path(source_path)
        target_path = Path(target_path)

        if not source_path.exists():
            raise FileNotFoundError(f"Source file not found: {source_path}")

        # Read from source (auto-detect format)
        data = self.read(source_path)

        # Write to target (always MessagePack)
        self.write(target_path, data)

        logger.info(f"Migrated data from {source_path} to {target_path}")

    def compute_hash(self, file_path: str | Path) -> str:
        """
        Compute SHA-256 hash of a file for integrity verification.

        Args:
            file_path: Path to the file

        Returns:
            Hexadecimal SHA-256 hash

        Raises:
            FileNotFoundError: If the file doesn't exist
            IOError: If there's an error reading the file
        """
        file_path = Path(file_path)

        if not file_path.exists():
            raise FileNotFoundError(f"File not found: {file_path}")

        sha256 = hashlib.sha256()

        try:
            with open(file_path, "rb") as f:
                for chunk in iter(lambda: f.read(8192), b""):
                    sha256.update(chunk)

            hash_hex = sha256.hexdigest()
            logger.debug(f"Computed SHA-256 hash for {file_path}: {hash_hex}")
            return hash_hex

        except IOError as e:
            logger.error(f"Error computing hash for {file_path}: {e}")
            raise IOError(f"Failed to compute file hash: {e}") from e

    def validate_index_file(self, file_path: str | Path) -> tuple[bool, Optional[str]]:
        """
        Validate an index file for integrity.

        Checks:
        - File exists and is readable
        - File has valid format (MessagePack or pickle)
        - Data can be deserialized

        Args:
            file_path: Path to the index file

        Returns:
            Tuple of (is_valid, error_message)

        Examples:
            >>> serializer = MessagePackSerializer()
            >>> is_valid, error = serializer.validate_index_file("index.msgpack")
            >>> is_valid
            True
        """
        file_path = Path(file_path)

        # Check file exists
        if not file_path.exists():
            return False, f"File does not exist: {file_path}"

        # Check file is readable
        if not os.access(file_path, os.R_OK):
            return False, f"File is not readable: {file_path}"

        # Check file is not empty
        if file_path.stat().st_size == 0:
            return False, f"File is empty: {file_path}"

        # Detect format
        format_type = self.detect_format(file_path)

        if format_type == FormatType.UNKNOWN:
            return False, f"Unknown file format: {file_path}"

        # Try to read and deserialize
        try:
            data = self.read(file_path)

            # Check data is dict-like (expected structure)
            if not isinstance(data, dict):
                return False, f"Data is not dict-like: {type(data)}"

            logger.debug(f"Validated index file: {file_path}")
            return True, None

        except Exception as e:
            error_msg = f"Failed to read index file: {e}"
            logger.error(f"Validation failed for {file_path}: {error_msg}")
            return False, error_msg
