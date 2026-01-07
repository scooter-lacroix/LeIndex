#!/usr/bin/env python
"""
Development convenience script to run the LeIndex MCP server.

This script provides a simple way to start the LeIndex server during development.
The server uses:
- LEANN for vector search (no external dependencies)
- Tantivy for full-text search (embedded Rust engine)
- SQLite + DuckDB for storage (local files)
- asyncio for task processing (native Python)
"""
import sys
import os

# Add src directory to path
src_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'src')
sys.path.insert(0, src_path)

try:
    from leindex.server import main
    from leindex.logger_config import logger

    if __name__ == "__main__":
        print("Starting LeIndex MCP server...", file=sys.stderr)
        print(f"Using src path: {src_path}", file=sys.stderr)
        print("", file=sys.stderr)
        print("Architecture:", file=sys.stderr)
        print("  - Vector Search: LEANN (HNSW)", file=sys.stderr)
        print("  - Full-Text Search: Tantivy (Lucene)", file=sys.stderr)
        print("  - Metadata Storage: SQLite", file=sys.stderr)
        print("  - Analytics: DuckDB", file=sys.stderr)
        print("  - Task Processing: asyncio", file=sys.stderr)
        print("", file=sys.stderr)

        # Run the server
        main()

except ImportError as e:
    print(f"Import Error: {e}", file=sys.stderr)
    print(f"Current sys.path: {sys.path}", file=sys.stderr)
    print("Please ensure all dependencies are installed:", file=sys.stderr)
    print("  pip install -e .", file=sys.stderr)
    sys.exit(1)
except Exception as e:
    print(f"Error starting server: {e}", file=sys.stderr)
    import traceback
    traceback.print_exc(file=sys.stderr)
    sys.exit(1)
