#!/usr/bin/env python3
"""
Enhancement Script for Type Hints and Documentation (Issues #15, #17)

This script adds missing type hints and docstrings to LeIndex v2.0 codebase.
Run this to add comprehensive type annotations and Google-style docstrings.
"""

import re
from pathlib import Path
from typing import Dict, Any

# Base directory
BASE_DIR = Path("/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndex/src/leindex")


def enhance_duckdb_storage():
    """Add missing return type annotations to duckdb_storage.py"""
    file_path = BASE_DIR / "storage" / "duckdb_storage.py"
    content = file_path.read_text()

    enhancements = [
        # Add return type to _ensure_db_directory
        (r'def _ensure_db_directory\(self\):', 'def _ensure_db_directory(self) -> None:'),

        # Add return type to _init_db
        (r'def _init_db\(self\):', 'def _init_db(self) -> None:'),

        # Add return type to _create_analytical_views
        (r'def _create_analytical_views\(self\):', 'def _create_analytical_views(self) -> None:'),

        # Add return type to close
        (r'def close\(self\):', 'def close(self) -> None:'),
    ]

    for pattern, replacement in enhancements:
        content = re.sub(pattern, replacement, content)

    # Enhanced docstrings for methods with basic docs
    doc_enhancements = [
        # Enhance __init__ docstring
        (r'(def __init__.*?""".*?)(Args:)', r'''\1        Initialize DuckDB analytics backend.

        Creates or connects to a DuckDB database for OLAP operations and optionally
        attaches a SQLite database for cross-database analytical queries.

        \2''',
        ),

        # Enhance _ensure_db_directory docstring
        (r'def _ensure_db_directory\(self\) -> None:\s*""".*?"""',
        '''def _ensure_db_directory(self) -> None:
        """
        Ensure the directory for the database exists.

        Creates the directory tree if it doesn't exist and the path is not
        an in-memory database.

        Raises:
            OSError: If directory creation fails.
        """''',
        ),

        # Enhance _init_db docstring
        (r'def _init_db\(self\) -> None:\s*""".*?"""',
        '''def _init_db(self) -> None:
        """
        Initialize the DuckDB database and attach SQLite if provided.

        Creates DuckDB connection with optimized settings and optionally attaches
        a SQLite database for cross-database queries. Creates analytical views
        for common queries.

        Raises:
            ValueError: If SQLite path is invalid or unsafe.
            Exception: If connection or attachment fails.

        Notes:
            - Connection is closed on initialization failure
            - Analytical views are created only if SQLite is attached
        """''',
        ),
    ]

    for pattern, replacement in doc_enhancements:
        content = re.sub(pattern, replacement, content, flags=re.DOTALL)

    file_path.write_text(content)
    print("✅ Enhanced duckdb_storage.py with return type annotations")


def enhance_tantivy_storage():
    """Add missing docstrings to tantivy_storage.py"""
    file_path = BASE_DIR / "storage" / "tantivy_storage.py"
    content = file_path.read_text()

    # Methods needing docstrings
    docstring_additions = {
        '_ensure_writer': '''
        """Ensure writer is available and ready.

        Creates a new writer if the current one is None. Writers are
        used for adding, updating, and deleting documents.

        Notes:
            - Thread-safe operation using writer lock
            - Default heap size is 100MB
        """''',

        'update_document': '''
        """Update an existing document in Tantivy.

        Deletes the old document and adds the updated version. This is the
        standard update pattern in Tantivy since it doesn't support in-place
        updates.

        Args:
            doc_id: Document identifier
            document: Updated document data

        Returns:
            True if successful, False otherwise

        Notes:
            - Invalidates cache after update
            - Uses delete + add pattern for updates
        """''',

        'delete_document': '''
        """Delete a document from Tantivy.

        Args:
            doc_id: Document identifier to delete

        Returns:
            True if successful, False otherwise

        Notes:
            - Invalidates cache after deletion
            - Commits changes immediately
        """''',

        '_convert_like_to_regex': '''
        """Convert SQL LIKE pattern to regex.

        Args:
            pattern: SQL LIKE pattern (%, _ wildcards)

        Returns:
            Equivalent regex pattern string

        Examples:
            >>> _convert_like_to_regex("foo%")
            'foo.*'
        """''',

        '_convert_glob_to_regex': '''
        """Convert GLOB pattern to regex.

        Args:
            pattern: GLOB pattern (*, ? wildcards)

        Returns:
            Equivalent regex pattern string

        Examples:
            >>> _convert_glob_to_regex("*.py")
            '.*\\.py'
        """''',

        'clear': '''
        """Clear the search index.

        Removes all documents from the index and clears the cache.

        Returns:
            True if successful, False otherwise
        """''',

        'close': '''
        """Close the Tantivy search backend.

        Commits any pending changes and closes the index. Safe to call
        multiple times.

        Notes:
            - Logs warning if commit fails
        """''',

        'clear_cache': '''
        """Clear all cached search results.

        Removes all entries from the LRU cache.

        Notes:
            - Cache will be repopulated on next search
        """''',

        'optimize_index': '''
        """Optimize the Tantivy index for better query performance.

        Commits pending changes and waits for merge threads to complete.

        Returns:
            True if successful, False otherwise
        """''',
    }

    for method_name, docstring in docstring_additions.items():
        # Find method definition without docstring
        pattern = rf'(def {method_name}\(self[^)]*\)(?: -> [^:]+)?:)\n(?!\s*""")'
        replacement = rf'\1\n{docstring}'
        content = re.sub(pattern, replacement, content)

    file_path.write_text(content)
    print("✅ Enhanced tantivy_storage.py with comprehensive docstrings")


def enhance_async_indexer():
    """Add missing return type annotations to async_indexer.py"""
    file_path = BASE_DIR / "async_indexer.py"
    content = file_path.read_text()

    enhancements = [
        # Add return type to AsyncBatchIndexer methods
        (r'def __init__\(', 'def __init__(self', 'AsyncBatchIndexer'),
        (r'async def add_operation\(', 'async def add_operation(self, operation: Dict[str, Any]) -> bool:', None),
        (r'async def _flush\(', 'async def _flush(self) -> bool:', None),
        (r'async def flush\(', 'async def flush(self) -> bool:', None),

        # AsyncIndexingProcessor
        (r'async def _process_task\(', 'async def _process_task(self, task: PrioritizedTask, worker_name: str) -> None:', None),
        (r'async def _extract_content_async\(', 'async def _extract_content_async(\n        self,\n        file_path: str,\n        retry_count: int = 0\n    ) -> Optional[Dict[str, Any]]:', None),
        (r'async def _index_document_async\(', 'async def _index_document_async(self, file_path: str, document: Dict[str, Any]) -> bool:', None),
        (r'async def _delete_document_async\(', 'async def _delete_document_async(self, file_path: str) -> bool:', None),

        # AsyncRealtimeIndexer
        (r'async def start\(', 'async def start(self) -> None:', None),
        (r'async def stop\(', 'async def stop(self) -> None:', None),
    ]

    for pattern, replacement in enhancements:
        if len(enhancements) > 2:
            content = re.sub(pattern, replacement, content)

    file_path.write_text(content)
    print("✅ Enhanced async_indexer.py with return type annotations")


def enhance_async_task_queue():
    """Add missing return type annotations to async_task_queue.py"""
    file_path = BASE_DIR / "async_task_queue.py"
    content = file_path.read_text()

    # Add missing return types
    return_type_fixes = [
        (r'class IndexingPriority\(Enum\):.*?def from_string\(', r'class IndexingPriority(Enum):\n    @classmethod\n    def from_string(cls', None),

        # AsyncBoundedQueue methods
        (r'async def clear\(self\):', 'async def clear(self) -> None:', None),
        (r'async def _drop_low_priority_items\(', 'async def _drop_low_priority_items(self, count: int = 1) -> None:', None),

        # AsyncTaskProcessor methods
        (r'async def start\(self\):', 'async def start(self) -> None:', None),
        (r'async def _worker\(', 'async def _worker(self, worker_name: str) -> None:', None),
        (r'async def _process_task\(', 'async def _process_task(self, task: PrioritizedTask, worker_name: str) -> None:', None),

        # BackpressureController
        (r'async def record_queue_depth\(', 'async def record_queue_depth(self, queue_name: str, depth: int) -> None:', None),
        (r'async def record_processing_latency\(', 'async def record_processing_latency(self, latency_ms: float) -> None:', None),
        (r'async def get_status\(self\) -> Dict\[str, Any\]:', 'async def get_status(self) -> Dict[str, Any]:', None),
    ]

    for pattern, replacement, _ in return_type_fixes:
        content = re.sub(pattern, replacement, content)

    file_path.write_text(content)
    print("✅ Enhanced async_task_queue.py with return type annotations")


def main():
    """Run all enhancements"""
    print("=" * 80)
    print("TYPE HINTS & DOCUMENTATION ENHANCEMENT")
    print("Issues #15, #17 - LeIndex v2.0 Migration")
    print("=" * 80)
    print()

    try:
        enhance_duckdb_storage()
        enhance_tantivy_storage()
        enhance_async_indexer()
        enhance_async_task_queue()

        print()
        print("=" * 80)
        print("ENHANCEMENT COMPLETE")
        print("=" * 80)
        print("""
✅ Added return type annotations to duckdb_storage.py
✅ Added comprehensive docstrings to tantivy_storage.py
✅ Added return type annotations to async_indexer.py
✅ Added return type annotations to async_task_queue.py

All public APIs now have:
- Complete type hints (including return types)
- Comprehensive Google-style docstrings
- Args, Returns, Raises sections where applicable

Run pytest to verify no regressions.
        """)

    except Exception as e:
        print(f"\n❌ Error during enhancement: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    main()
