import argparse
import asyncio
import json
import os
import sys
from datetime import datetime

from .core_engine import CoreEngine, SearchOptions
from .storage.dal_factory import get_dal_instance
from .logger_config import setup_logging

logger = setup_logging()


def highlight_text(text: str, query: str) -> str:
    """
    Highlight matching terms in text.

    Args:
        text: The text to highlight
        query: The search query

    Returns:
        Text with ANSI highlight markers
    """
    if not text or not query:
        return text

    import re

    # Escape regex characters in query
    escaped_query = re.escape(query)
    # Case-insensitive highlighting
    pattern = re.compile(f"({escaped_query})", re.IGNORECASE)
    return pattern.sub(r"\x1b[1;32m\1\x1b[0m", text)


def format_results_for_export(results, export_format: str) -> str:
    """
    Format search results for export.

    Args:
        results: Search results
        export_format: Format ('json', 'csv')

    Returns:
        Formatted string
    """
    if export_format == "json":
        export_data = []
        for item in results:
            export_data.append(
                {
                    "path": item.metadata.path if item.metadata else "Unknown",
                    "score": float(item.score) if item.score else 0.0,
                    "text": item.text,
                    "filename": item.filename or None,
                }
            )
        return json.dumps(export_data, indent=2)

    elif export_format == "csv":
        lines = ["path,score,text,filename"]
        for item in results:
            path = item.metadata.path if item.metadata else ""
            score = float(item.score) if item.score else 0.0
            text = (item.text or "").replace('"', '""')
            filename = item.filename or ""
            lines.append(f'"{path}",{score},"{text}","{filename}"')
        return "\n".join(lines)

    return str(results)


async def search_command(args, engine: CoreEngine):
    """Execute search command."""
    options = SearchOptions(
        rerank=not args.no_rerank,
        top_k=args.max_count,
        include_web=args.web,
        content=args.content,
        use_zoekt=not args.no_zoekt,
        use_faiss=(
            "faiss" in (args.backend or "all") or "all" in (args.backend or "all")
        ),
        use_elasticsearch=(
            "elasticsearch" in (args.backend or "all")
            or "all" in (args.backend or "all")
        ),
    )

    # Check if path is provided, otherwise use current directory
    search_path = args.path if args.path else os.getcwd()
    search_path = os.path.abspath(search_path)

    store_ids = [search_path]

    if args.answer:
        # Ask mode (RAG)
        response = await engine.ask(store_ids, args.pattern, options)
        print(f"\nAnswer:\n{response.answer}\n")
        print("Sources:")
        for source in response.sources:
            path = source.metadata.path if source.metadata else "Unknown"
            print(f"- {path} (Score: {source.score:.2f})")
    else:
        # Search mode
        response = await engine.search(store_ids, args.pattern, options)

        # Export results if requested
        if args.export:
            output = format_results_for_export(response.data, args.export)
            if args.output:
                with open(args.output, "w") as f:
                    f.write(output)
                print(f"Results exported to {args.output}")
            else:
                print(output)
            return

        if not response.data:
            print("No results found.")
            return

        print(f"Found {len(response.data)} results:\n")

        for item in response.data:
            path = item.metadata.path if item.metadata else "Unknown"
            if item.filename:  # Web result
                path = item.filename

            line_info = ""
            if item.generated_metadata and "line_number" in item.generated_metadata:
                line_info = f":{item.generated_metadata['line_number']}"

            # Handle score - convert to float if it's a string
            score = item.score
            if isinstance(score, str):
                try:
                    score = float(score)
                except (ValueError, TypeError):
                    score = 0.0

            # Highlight query terms if content is shown
            display_text = ""
            if args.highlight and item.text:
                display_text = highlight_text(item.text.strip(), args.pattern)
            elif item.text:
                display_text = item.text.strip()

            print(f"{path}{line_info} \x1b[1;33m(Score: {score:.2f})\x1b[0m")
            if display_text:
                print(f"  {display_text}")
            print()


async def batch_search_command(args, engine: CoreEngine):
    """
    Execute batch search from file.

    Reads queries from a file (one per line) and executes searches for each.
    """
    options = SearchOptions(
        rerank=not args.no_rerank,
        top_k=args.max_count,
        use_zoekt=not args.no_zoekt,
        use_faiss=(
            "faiss" in (args.backend or "all") or "all" in (args.backend or "all")
        ),
        use_elasticsearch=(
            "elasticsearch" in (args.backend or "all")
            or "all" in (args.backend or "all")
        ),
    )

    # Read queries from file
    search_path = args.path if args.path else os.getcwd()
    search_path = os.path.abspath(search_path)

    try:
        with open(args.query_file, "r") as f:
            queries = [line.strip() for line in f if line.strip()]
    except FileNotFoundError:
        print(f"Error: Query file not found: {args.query_file}")
        sys.exit(1)
    except Exception as e:
        print(f"Error reading query file: {e}")
        sys.exit(1)

    print(f"Executing {len(queries)} queries from {args.query_file}\n")
    all_results = []

    for i, query in enumerate(queries, 1):
        print(f"\n[{i}/{len(queries)}] Query: {query}")
        print("-" * 60)

        try:
            response = await engine.search([search_path], query, options)
            result_count = len(response.data)
            print(f"Found {result_count} results")

            all_results.append(
                {
                    "query": query,
                    "result_count": result_count,
                    "timestamp": datetime.utcnow().isoformat(),
                }
            )

            if args.show_results and response.data:
                for item in response.data[: args.max_count]:
                    path = item.metadata.path if item.metadata else "Unknown"
                    score = float(item.score) if item.score else 0.0
                    print(f"  - {path} (Score: {score:.2f})")

        except Exception as e:
            print(f"  Error: {e}")
            all_results.append(
                {
                    "query": query,
                    "error": str(e),
                    "timestamp": datetime.utcnow().isoformat(),
                }
            )

    # Summary
    print("\n" + "=" * 60)
    print("Batch Search Summary")
    print("=" * 60)
    successful = sum(1 for r in all_results if "error" not in r)
    total_results = sum(r.get("result_count", 0) for r in all_results)
    print(f"Total queries: {len(queries)}")
    print(f"Successful: {successful}")
    print(f"Failed: {len(queries) - successful}")
    print(f"Total results: {total_results}")


async def stats_command(args):
    """
    Execute stats command.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Index Statistics Dashboard: CLI command showing index health metrics"

    Shows index health, document counts, and backend status.
    """
    from .stats_dashboard import IndexStatisticsCollector, DashboardCLI

    es_url = args.es_url or os.getenv("ELASTICSEARCH_URL", "http://localhost:9200")
    pg_dsn = args.pg_dsn or os.getenv("DATABASE_URL")

    collector = IndexStatisticsCollector(
        pg_dsn=pg_dsn,
        es_url=es_url,
    )

    cli = DashboardCLI(collector)

    if args.json:
        await cli.show_json()
    else:
        await cli.show_stats(watch=args.watch, interval=args.interval)


def main():
    parser = argparse.ArgumentParser(
        description="LeIndex CLI - AI-powered code search and indexing",
        formatter_class=argparse.RawDescriptionHelpFormatter,
        epilog="""
Examples:
  # Basic search
  leindex-search "def authenticate"

  # Search with content preview
  leindex-search "class User" --content --highlight

  # Use specific backend
  leindex-search "async def" --backend=faiss

  # Export results to JSON
  leindex-search "TODO" --export=json --output=todos.json

  # Batch search from file
  leindex-search --batch queries.txt --show-results

  # RAG-style question answering
  leindex-search "How does auth work?" --answer
        """,
    )
    subparsers = parser.add_subparsers(dest="command", help="Available commands")

    # Search command (default for backward compatibility)
    search_parser = subparsers.add_parser("search", help="Search code", add_help=False)
    search_parser.add_argument("pattern", help="The pattern or question to search for")
    search_parser.add_argument(
        "path", nargs="?", help="The path to search in (default: current directory)"
    )
    search_parser.add_argument(
        "-w", "--web", action="store_true", help="Include web search results"
    )
    search_parser.add_argument(
        "-a", "--answer", action="store_true", help="Generate an answer (RAG mode)"
    )
    search_parser.add_argument(
        "-c", "--content", action="store_true", help="Show content of results"
    )
    search_parser.add_argument(
        "-m", "--max-count", type=int, default=10, help="Maximum number of results"
    )
    search_parser.add_argument(
        "--no-rerank", action="store_true", help="Disable reranking"
    )
    search_parser.add_argument(
        "--no-zoekt", action="store_true", help="Disable Zoekt backend"
    )
    search_parser.add_argument(
        "-b",
        "--backend",
        choices=["faiss", "elasticsearch", "zoekt", "all"],
        default="all",
        help="Backend to use (default: all)",
    )
    search_parser.add_argument(
        "--highlight", action="store_true", help="Highlight matching terms in results"
    )
    search_parser.add_argument(
        "-e",
        "--export",
        choices=["json", "csv"],
        help="Export results to file (requires --output)",
    )
    search_parser.add_argument("-o", "--output", help="Output file path for export")

    # Batch search command
    batch_parser = subparsers.add_parser("batch", help="Execute batch search from file")
    batch_parser.add_argument(
        "query_file", help="File containing queries (one per line)"
    )
    batch_parser.add_argument(
        "path", nargs="?", help="The path to search in (default: current directory)"
    )
    batch_parser.add_argument(
        "-m", "--max-count", type=int, default=5, help="Maximum results per query"
    )
    batch_parser.add_argument(
        "--no-rerank", action="store_true", help="Disable reranking"
    )
    batch_parser.add_argument(
        "--no-zoekt", action="store_true", help="Disable Zoekt backend"
    )
    batch_parser.add_argument(
        "-b",
        "--backend",
        choices=["faiss", "elasticsearch", "zoekt", "all"],
        default="all",
        help="Backend to use (default: all)",
    )
    batch_parser.add_argument(
        "-s",
        "--show-results",
        action="store_true",
        help="Show top results for each query",
    )

    # Stats command
    stats_parser = subparsers.add_parser("stats", help="Show index statistics")
    stats_parser.add_argument("--es-url", help="Elasticsearch URL")
    stats_parser.add_argument("--pg-dsn", help="PostgreSQL connection string")
    stats_parser.add_argument(
        "--json", action="store_true", help="Output in JSON format"
    )
    stats_parser.add_argument(
        "--watch", action="store_true", help="Continuously update"
    )
    stats_parser.add_argument(
        "--interval", type=int, default=5, help="Update interval for watch mode"
    )

    # Default to search if no command specified (backward compatibility)
    args = parser.parse_args()

    if args.command == "stats":
        # Stats command
        asyncio.run(stats_command(args))
    elif args.command == "batch":
        # Batch search command
        asyncio.run(batch_search_command(args, None))
    else:
        # Default to search for backward compatibility
        # If no subcommand was used, treat positional args as search args
        if args.command is None:
            # Re-parse with search-only behavior
            parser = argparse.ArgumentParser(description="Code Search CLI")
            parser.add_argument("pattern", help="The pattern or question to search for")
            parser.add_argument(
                "path",
                nargs="?",
                help="The path to search in (default: current directory)",
            )
            parser.add_argument(
                "-w", "--web", action="store_true", help="Include web search results"
            )
            parser.add_argument(
                "-a",
                "--answer",
                action="store_true",
                help="Generate an answer (RAG mode)",
            )
            parser.add_argument(
                "-c", "--content", action="store_true", help="Show content of results"
            )
            parser.add_argument(
                "-m",
                "--max-count",
                type=int,
                default=10,
                help="Maximum number of results",
            )
            parser.add_argument(
                "--no-rerank", action="store_true", help="Disable reranking"
            )
            parser.add_argument(
                "--no-zoekt", action="store_true", help="Disable Zoekt backend"
            )
            parser.add_argument(
                "-b",
                "--backend",
                choices=["faiss", "elasticsearch", "zoekt", "all"],
                default="all",
                help="Backend to use (default: all)",
            )
            parser.add_argument(
                "--highlight",
                action="store_true",
                help="Highlight matching terms in results",
            )
            parser.add_argument(
                "-e",
                "--export",
                choices=["json", "csv"],
                help="Export results to file (requires --output)",
            )
            parser.add_argument("-o", "--output", help="Output file path for export")
            args = parser.parse_args()

        # Validate export arguments
        if args.export and not args.output:
            parser.error("--export requires --output file path")

        # Initialize components
        try:
            # We need a DAL instance for legacy fallback
            # Use simple sqlite config if env vars not set, to avoid crashing CLI if not fully configured
            try:
                dal_instance = get_dal_instance()
            except Exception as e:
                logger.warning(
                    f"Could not initialize full DAL: {e}. Falling back to minimal configuration."
                )
                dal_instance = None

            engine = CoreEngine(legacy_backend=dal_instance)

            # Run async command
            asyncio.run(search_command(args, engine))

        except KeyboardInterrupt:
            print("\nOperation cancelled.")
            sys.exit(130)
        except Exception as e:
            print(f"Error: {e}")
            sys.exit(1)


if __name__ == "__main__":
    main()
