"""
Trie-based file index.

This module implements a file index using a Trie data structure for efficient
storage and retrieval of file paths.
"""

from typing import Any, Dict, Optional, List, Tuple
from collections import defaultdict
from .storage_interface import FileMetadataInterface


class TrieNode:
    def __init__(self):
        self.children = defaultdict(TrieNode)
        self.is_end_of_word = False
        self.file_info: Optional[Dict[str, Any]] = None


class TrieFileIndex(FileMetadataInterface):
    """File index using Trie data structure."""
    
    def __init__(self):
        self.root = TrieNode()

    def add_file(self, file_path: str, file_type: str, extension: str, 
                 metadata: Optional[Dict[str, Any]] = None) -> bool:
        current = self.root
        parts = file_path.split('/')
        for part in parts:
            current = current.children[part]
        current.is_end_of_word = True
        current.file_info = {
            "type": file_type,
            "extension": extension,
            **(metadata or {})
        }
        return True

    def remove_file(self, file_path: str) -> bool:
        def _remove(node: TrieNode, parts: List[str], depth: int) -> bool:
            if depth == len(parts):
                if not node.is_end_of_word:
                    return False  # File not found
                node.is_end_of_word = False
                return not node.children  # If no children, node can be deleted
            part = parts[depth]
            if part not in node.children:
                return False  # File not found
            can_delete = _remove(node.children[part], parts, depth + 1)
            if can_delete:
                del node.children[part]
                return not node.children and not node.is_end_of_word
            return False
        return _remove(self.root, file_path.split('/'), 0)

    def get_file_info(self, file_path: str) -> Optional[Dict[str, Any]]:
        current = self.root
        parts = file_path.split('/')
        for part in parts:
            if part not in current.children:
                return None
            current = current.children[part]
        return current.file_info if current.is_end_of_word else None

    def find_files_by_pattern(self, pattern: str) -> List[str]:
        raise NotImplementedError("Pattern search not implemented in TrieFileIndex")

    def find_files_by_extension(self, extension: str) -> List[str]:
        result = []
        def _search(node: TrieNode, path: str):
            if node.is_end_of_word and node.file_info and node.file_info['extension'] == extension:
                result.append(path)
            for part, child_node in node.children.items():
                _search(child_node, f"{path}/{part}" if path else part)
        _search(self.root, "")
        return result

    def get_directory_structure(self, directory_path: str = "") -> Dict[str, Any]:
        raise NotImplementedError("Directory structure retrieval not implemented in TrieFileIndex")

    def get_all_files(self) -> List[Tuple[str, Dict[str, Any]]]:
        files = []
        def _gather_files(node: TrieNode, path: str):
            if node.is_end_of_word:
                files.append((path, node.file_info))
            for part, child_node in node.children.items():
                _gather_files(child_node, f"{path}/{part}" if path else part)
        _gather_files(self.root, "")
        return files
    
    def clear(self) -> None:
        """Clear all files from the index."""
        self.root = TrieNode()

    def insert_file_version(self, version_id: str, file_path: str, content: str, hash: str, timestamp: str, size: int) -> bool:
        """Inserts a new file version."""
        # For now, just return True as this is a simple in-memory implementation
        return True

    def get_file_version(self, version_id: str) -> Optional[Dict]:
        """Retrieves a file version by its ID."""
        # For now, return None as this is a simple in-memory implementation
        return None

    def get_file_versions_for_path(self, file_path: str) -> List[Dict]:
        """Retrieves all versions for a given file path."""
        # For now, return empty list as this is a simple in-memory implementation
        return []

    def insert_file_diff(self, diff_id: str, file_path: str, previous_version_id: Optional[str], current_version_id: str, diff_content: str, diff_type: str, operation_type: str, operation_details: Optional[str], timestamp: str) -> bool:
        """Inserts a new file diff."""
        # For now, just return True as this is a simple in-memory implementation
        return True

    def get_file_diffs_for_path(self, file_path: str) -> List[Dict]:
        """Retrieves all diffs for a given file path."""
        # For now, return empty list as this is a simple in-memory implementation
        return []

    # CRITICAL FIX: Added missing abstract methods from FileMetadataInterface
    def save_file_metadata(self, file_path: str, metadata: Dict[str, Any]) -> None:
        """Save file metadata to storage."""
        current = self.root
        parts = file_path.split('/')
        for part in parts:
            current = current.children[part]
        if current.is_end_of_word and current.file_info:
            current.file_info.update(metadata)
        else:
            raise IOError(f"File {file_path} not found, cannot save metadata")

    def get_file_metadata(self, file_path: str) -> Optional[Dict[str, Any]]:
        """Retrieve file metadata from storage."""
        file_info = self.get_file_info(file_path)
        return file_info

    def delete_file_metadata(self, file_path: str) -> None:
        """Delete file metadata from storage."""
        self.remove_file(file_path)

    def get_all_file_paths(self) -> List[str]:
        """Get all file paths in the storage."""
        files = []
        def _gather_paths(node: TrieNode, path: str):
            if node.is_end_of_word:
                files.append(path)
            for part, child_node in node.children.items():
                _gather_paths(child_node, f"{path}/{part}" if path else part)
        _gather_paths(self.root, "")
        return files

    def close(self) -> None:
        """Close the storage backend."""
        # No-op for in-memory storage
        pass

    def size(self) -> int:
        """Get the number of files in the storage."""
        return len(self.get_all_file_paths())

    def flush(self) -> bool:
        """Flush any pending operations."""
        # No-op for in-memory storage
        return True

