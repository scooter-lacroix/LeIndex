"""
Storage module for optimized data structures.

This module provides efficient storage backends for the code index,
including Trie-based file indexing and SQLite-based content caching.
"""

from .trie_index import TrieFileIndex
from .sqlite_storage import SQLiteStorage, SQLiteFileMetadata
from .storage_interface import StorageInterface

__all__ = ['TrieFileIndex', 'SQLiteStorage', 'SQLiteFileMetadata', 'StorageInterface']
