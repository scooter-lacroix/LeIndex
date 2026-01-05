"""
Incremental Indexing Module

This module provides functionality for incremental indexing by tracking file
modification timestamps and content hashes to determine which files have changed
since the last indexing operation.

PERFORMANCE FIX: Now uses aiofiles for truly asynchronous file I/O operations
instead of blocking the thread pool with synchronous operations.
"""
import os
import hashlib
import asyncio
from datetime import datetime
from typing import Dict, List, Tuple, Optional, Any, Callable
from .project_settings import ProjectSettings
from .lazy_loader import ChunkedFileReader

# PERFORMANCE FIX: Import aiofiles for truly async file operations
try:
    import aiofiles
    import aiofiles.os as aios
    AIOFILES_AVAILABLE = True
except ImportError:
    AIOFILES_AVAILABLE = False


class IncrementalIndexer:
    """
    Manages incremental indexing based on file modification timestamps and content hashes.
    
    This class tracks file metadata to determine which files have been added, modified,
    or deleted since the last indexing operation, enabling efficient re-indexing of
    only the changed files.
    """
    
    def __init__(self, settings: ProjectSettings):
        """
        Initialize the incremental indexer.
        
        Args:
            settings: ProjectSettings instance for persisting metadata
        """
        self.settings = settings
        self.file_metadata: Dict[str, Dict[str, Any]] = {}
        self.load_metadata()
    
    def load_metadata(self):
        """Load existing file metadata from persistent storage."""
        try:
            self.file_metadata = self.settings.load_metadata()
            print(f"Loaded metadata for {len(self.file_metadata)} files")
        except Exception as e:
            print(f"Error loading metadata: {e}")
            self.file_metadata = {}
    
    def save_metadata(self):
        """Save current file metadata to persistent storage."""
        try:
            self.settings.save_metadata(self.file_metadata)
            print(f"Saved metadata for {len(self.file_metadata)} files")
        except Exception as e:
            print(f"Error saving metadata: {e}")
    
    def get_file_hash(self, file_path: str) -> Optional[str]:
        """
        Calculate SHA-256 hash of a file's content using 4MB chunks for memory efficiency.
        
        Args:
            file_path: Path to the file
            
        Returns:
            SHA-256 hash as hex string, or None if file cannot be read
        """
        try:
            # Use ChunkedFileReader for consistent chunked reading
            reader = ChunkedFileReader(file_path)
            return reader.compute_hash()
        except Exception as e:
            print(f"Error calculating hash for {file_path}: {e}")
            return None
    
    def get_file_metadata(self, file_path: str, compute_hash: bool = True) -> Dict[str, Any]:
        """
        Get current metadata for a file.

        PERFORMANCE FIX: Added optional hash computation to avoid expensive hash
        calculation when not needed (e.g., during initial change detection).

        Args:
            file_path: Path to the file
            compute_hash: Whether to compute file hash (default: True)

        Returns:
            Dictionary containing file metadata (timestamp, hash, size)
        """
        try:
            stat_info = os.stat(file_path)
            metadata = {
                'mtime': stat_info.st_mtime,
                'size': stat_info.st_size,
                'last_checked': datetime.now().isoformat()
            }

            # Only compute hash if requested (lazy hash computation)
            if compute_hash:
                metadata['hash'] = self.get_file_hash(file_path)
            else:
                metadata['hash'] = None

            return metadata
        except Exception as e:
            print(f"Error getting metadata for {file_path}: {e}")
            return {}
    
    def has_file_changed(self, file_path: str, verify_hash: bool = False) -> bool:
        """
        Check if a file has changed since last indexing.

        PERFORMANCE FIX: Optimized change detection to avoid expensive hash calculations
        unless explicitly requested. Uses timestamp and size as fast pre-checks.

        Args:
            file_path: Relative path to the file from project root
            verify_hash: If True, verify hash even when mtime/size match (for extra certainty)

        Returns:
            True if file has changed or is new, False otherwise
        """
        if file_path not in self.file_metadata:
            return True  # New file

        try:
            current_stat = os.stat(file_path)
            stored_metadata = self.file_metadata[file_path]

            # Fast check: modification time changed
            if current_stat.st_mtime != stored_metadata.get('mtime', 0):
                return True

            # Fast check: file size changed
            if current_stat.st_size != stored_metadata.get('size', 0):
                return True

            # If timestamp and size are the same, file likely hasn't changed
            # Only verify hash if explicitly requested (e.g., for security-critical files)
            if verify_hash and 'hash' in stored_metadata and stored_metadata['hash']:
                current_hash = self.get_file_hash(file_path)
                return current_hash != stored_metadata['hash']

            return False

        except Exception as e:
            print(f"Error checking if file changed {file_path}: {e}")
            return True  # Assume changed if we can't check
    
    def update_file_metadata(self, file_path: str, full_path: str):
        """
        Update metadata for a file after indexing.
        
        Args:
            file_path: Relative path to the file from project root
            full_path: Full absolute path to the file
        """
        try:
            metadata = self.get_file_metadata(full_path)
            if metadata:
                self.file_metadata[file_path] = metadata
        except Exception as e:
            print(f"Error updating metadata for {file_path}: {e}")
    
    def get_changed_files(self, base_path: str, current_files: List[str]) -> Tuple[List[str], List[str], List[str]]:
        """
        Determine which files have been added, modified, or deleted.
        
        Args:
            base_path: Base directory path
            current_files: List of current file paths (relative to base_path)
            
        Returns:
            Tuple of (added_files, modified_files, deleted_files)
        """
        added_files = []
        modified_files = []
        deleted_files = []
        
        # Convert current files to set for efficient lookup
        current_files_set = set(current_files)
        
        # Files that exist in metadata but not in current scan are deleted
        for file_path in self.file_metadata:
            if file_path not in current_files_set:
                deleted_files.append(file_path)
        
        # Check each current file
        for file_path in current_files:
            full_path = os.path.join(base_path, file_path)
            
            if file_path not in self.file_metadata:
                # New file
                added_files.append(file_path)
            elif self.has_file_changed(full_path):
                # Modified file
                modified_files.append(file_path)
        
        return added_files, modified_files, deleted_files
    
    def clean_deleted_files(self, deleted_files: List[str]):
        """
        Remove metadata for deleted files.
        
        Args:
            deleted_files: List of file paths that have been deleted
        """
        for file_path in deleted_files:
            self.remove_file_metadata(file_path)
    
    def remove_file_metadata(self, file_path: str):
        """
        Remove metadata for a specific file.
        
        Args:
            file_path: Relative path to the file from project root
        """
        if file_path in self.file_metadata:
            del self.file_metadata[file_path]
            print(f"Removed metadata for file: {file_path}")

    def rename_file_metadata(self, old_file_path: str, new_file_path: str, full_new_path: str):
        """
        Rename file metadata when a file is renamed/moved.
        
        Args:
            old_file_path: The old relative path of the file
            new_file_path: The new relative path of the file
            full_new_path: The full absolute path of the new file
        """
        if old_file_path in self.file_metadata:
            metadata = self.file_metadata.pop(old_file_path)
            # Update mtime, size, and hash for the new path
            try:
                stat_info = os.stat(full_new_path)
                metadata['mtime'] = stat_info.st_mtime
                metadata['size'] = stat_info.st_size
                metadata['hash'] = self.get_file_hash(full_new_path) # Recalculate hash for new path
                metadata['last_checked'] = datetime.now().isoformat()
            except Exception as e:
                print(f"Warning: Could not update metadata for renamed file {new_file_path}: {e}")
            self.file_metadata[new_file_path] = metadata
            print(f"Renamed metadata from {old_file_path} to {new_file_path}")
        else:
            print(f"Warning: Old file metadata not found for rename: {old_file_path}")
        
    def clear_metadata(self):
        """
        Clear all file metadata.
        
        This is useful for force reindexing operations where we want to treat
        all files as new and rebuild the metadata from scratch.
        """
        self.file_metadata.clear()
        print("Cleared all file metadata")
    
    def get_stats(self) -> Dict[str, Any]:
        """
        Get statistics about the current metadata.
        
        Returns:
            Dictionary containing metadata statistics
        """
        if not self.file_metadata:
            return {
                'total_files': 0,
                'oldest_timestamp': None,
                'newest_timestamp': None,
                'files_with_hashes': 0
            }
        
        timestamps = []
        files_with_hashes = 0
        
        for metadata in self.file_metadata.values():
            if 'last_checked' in metadata:
                timestamps.append(metadata['last_checked'])
            if 'hash' in metadata and metadata['hash']:
                files_with_hashes += 1
        
        return {
            'total_files': len(self.file_metadata),
            'oldest_timestamp': min(timestamps) if timestamps else None,
            'newest_timestamp': max(timestamps) if timestamps else None,
            'files_with_hashes': files_with_hashes
        }
    
    def force_rehash_file(self, file_path: str, full_path: str):
        """
        Force recalculation of hash for a specific file.
        
        Args:
            file_path: Relative path to the file from project root
            full_path: Full absolute path to the file
        """
        try:
            if os.path.exists(full_path):
                new_hash = self.get_file_hash(full_path)
                if file_path in self.file_metadata:
                    self.file_metadata[file_path]['hash'] = new_hash
                    self.file_metadata[file_path]['last_checked'] = datetime.now().isoformat()
                    print(f"Updated hash for {file_path}")
                else:
                    print(f"File {file_path} not in metadata, adding...")
                    self.update_file_metadata(file_path, full_path)
        except Exception as e:
            print(f"Error force rehashing {file_path}: {e}")
    
    def verify_file_integrity(self, file_path: str, full_path: str) -> bool:
        """
        Verify file integrity by comparing stored hash with current hash.
        
        Args:
            file_path: Relative path to the file from project root
            full_path: Full absolute path to the file
            
        Returns:
            True if file integrity is verified, False otherwise
        """
        if file_path not in self.file_metadata:
            return False
        
        stored_hash = self.file_metadata[file_path].get('hash')
        if not stored_hash:
            return False
        
        current_hash = self.get_file_hash(full_path)
        return stored_hash == current_hash
    
    # Async methods for improved performance

    async def get_file_hash_async(self, file_path: str) -> Optional[str]:
        """
        Asynchronously calculate SHA-256 hash of a file's content.

        PERFORMANCE FIX: Uses aiofiles for truly asynchronous file I/O instead of
        blocking the thread pool with run_in_executor.

        Args:
            file_path: Path to the file

        Returns:
            SHA-256 hash as hex string, or None if file cannot be read
        """
        if not AIOFILES_AVAILABLE:
            # Fallback to thread pool if aiofiles is not available
            loop = asyncio.get_event_loop()
            try:
                return await loop.run_in_executor(None, self.get_file_hash, file_path)
            except Exception as e:
                print(f"Error calculating hash async for {file_path}: {e}")
                return None

        try:
            sha256_hash = hashlib.sha256()
            # Use aiofiles for non-blocking async file reading
            async with aiofiles.open(file_path, 'rb') as f:
                # Read in 4MB chunks for memory efficiency
                while chunk := await f.read(4 * 1024 * 1024):
                    sha256_hash.update(chunk)
            return sha256_hash.hexdigest()
        except Exception as e:
            print(f"Error calculating hash async for {file_path}: {e}")
            return None
    
    async def get_file_metadata_async(self, file_path: str, compute_hash: bool = True) -> Dict[str, Any]:
        """
        Asynchronously get current metadata for a file.

        PERFORMANCE FIX: Uses aiofiles.os for non-blocking async file stat operations
        instead of blocking synchronous os.stat calls.

        Args:
            file_path: Path to the file
            compute_hash: Whether to compute file hash (default: True)

        Returns:
            Dictionary containing file metadata (timestamp, hash, size)
        """
        if AIOFILES_AVAILABLE:
            try:
                # Use aiofiles.os for async stat operation
                stat_result = await aios.stat(file_path)

                metadata = {
                    'mtime': stat_result.st_mtime,
                    'size': stat_result.st_size,
                    'last_checked': datetime.now().isoformat()
                }

                # Only compute hash if requested
                if compute_hash:
                    metadata['hash'] = await self.get_file_hash_async(file_path)
                else:
                    metadata['hash'] = None

                return metadata
            except Exception as e:
                print(f"Error getting metadata async for {file_path}: {e}")
                return {}

        # Fallback to synchronous stat with async hash
        loop = asyncio.get_event_loop()
        try:
            stat_info = await loop.run_in_executor(None, os.stat, file_path)

            metadata = {
                'mtime': stat_info.st_mtime,
                'size': stat_info.st_size,
                'last_checked': datetime.now().isoformat()
            }

            if compute_hash:
                metadata['hash'] = await self.get_file_hash_async(file_path)
            else:
                metadata['hash'] = None

            return metadata
        except Exception as e:
            print(f"Error getting metadata async for {file_path}: {e}")
            return {}
    
    async def update_file_metadata_async(self, file_path: str, full_path: str):
        """
        Asynchronously update metadata for a file after indexing.
        
        Args:
            file_path: Relative path to the file from project root
            full_path: Full absolute path to the file
        """
        try:
            metadata = await self.get_file_metadata_async(full_path)
            if metadata:
                self.file_metadata[file_path] = metadata
        except Exception as e:
            print(f"Error updating metadata async for {file_path}: {e}")
    
    async def get_changed_files_async(
        self, 
        base_path: str, 
        current_files: List[str],
        progress_callback: Optional[Callable[[float], None]] = None
    ) -> Tuple[List[str], List[str], List[str]]:
        """
        Asynchronously determine which files have been added, modified, or deleted.
        
        Args:
            base_path: Base directory path
            current_files: List of current file paths (relative to base_path)
            progress_callback: Optional callback for progress updates
            
        Returns:
            Tuple of (added_files, modified_files, deleted_files)
        """
        added_files = []
        modified_files = []
        deleted_files = []
        
        # Convert current files to set for efficient lookup
        current_files_set = set(current_files)
        
        # Files that exist in metadata but not in current scan are deleted
        for file_path in self.file_metadata:
            if file_path not in current_files_set:
                deleted_files.append(file_path)
        
        # Check each current file with progress tracking
        total_files = len(current_files)
        
        # Use semaphore to limit concurrent file checks
        semaphore = asyncio.Semaphore(10)  # Limit to 10 concurrent operations
        
        async def check_file(idx: int, file_path: str):
            async with semaphore:
                full_path = os.path.join(base_path, file_path)
                
                if file_path not in self.file_metadata:
                    # New file
                    added_files.append(file_path)
                else:
                    # Check if file has changed (run in thread pool)
                    loop = asyncio.get_event_loop()
                    changed = await loop.run_in_executor(None, self.has_file_changed, full_path)
                    if changed:
                        modified_files.append(file_path)
                
                # Update progress
                if progress_callback:
                    progress = (idx + 1) / total_files
                    progress_callback(progress)
        
        # Process files concurrently
        tasks = [check_file(idx, file_path) for idx, file_path in enumerate(current_files)]
        await asyncio.gather(*tasks)
        
        return added_files, modified_files, deleted_files
    
    async def update_multiple_files_async(
        self, 
        file_paths: List[Tuple[str, str]], 
        progress_callback: Optional[Callable[[float], None]] = None
    ):
        """
        Asynchronously update metadata for multiple files.
        
        Args:
            file_paths: List of (relative_path, full_path) tuples
            progress_callback: Optional callback for progress updates
        """
        total_files = len(file_paths)
        
        # Use semaphore to limit concurrent operations
        semaphore = asyncio.Semaphore(5)  # Limit to 5 concurrent hash calculations
        
        async def update_single_file(idx: int, file_path: str, full_path: str):
            async with semaphore:
                await self.update_file_metadata_async(file_path, full_path)
                
                # Update progress
                if progress_callback:
                    progress = (idx + 1) / total_files
                    progress_callback(progress)
        
        # Process files concurrently
        tasks = [
            update_single_file(idx, file_path, full_path) 
            for idx, (file_path, full_path) in enumerate(file_paths)
        ]
        await asyncio.gather(*tasks)
    
    async def verify_integrity_async(
        self, 
        file_paths: List[Tuple[str, str]], 
        progress_callback: Optional[Callable[[float], None]] = None
    ) -> Dict[str, bool]:
        """
        Asynchronously verify file integrity for multiple files.
        
        Args:
            file_paths: List of (relative_path, full_path) tuples
            progress_callback: Optional callback for progress updates
            
        Returns:
            Dictionary mapping file paths to integrity status
        """
        results = {}
        total_files = len(file_paths)
        
        # Use semaphore to limit concurrent operations
        semaphore = asyncio.Semaphore(5)  # Limit to 5 concurrent hash calculations
        
        async def verify_single_file(idx: int, file_path: str, full_path: str):
            async with semaphore:
                loop = asyncio.get_event_loop()
                integrity = await loop.run_in_executor(
                    None, self.verify_file_integrity, file_path, full_path
                )
                results[file_path] = integrity
                
                # Update progress
                if progress_callback:
                    progress = (idx + 1) / total_files
                    progress_callback(progress)
        
        # Process files concurrently
        tasks = [
            verify_single_file(idx, file_path, full_path) 
            for idx, (file_path, full_path) in enumerate(file_paths)
        ]
        await asyncio.gather(*tasks)
        
        return results
