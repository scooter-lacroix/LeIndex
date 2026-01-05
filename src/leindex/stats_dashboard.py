"""
Index Statistics Dashboard.

PRODUCT.MD ALIGNMENT:
---------------------
"Index Statistics Dashboard: CLI command showing index health metrics"

This module provides comprehensive statistics about:
- Document count and size
- Index health status
- Last update times
- Backend health (PostgreSQL, Elasticsearch)
- Usage analytics
"""

import os
import json
import time
import asyncio
from dataclasses import dataclass, field
from datetime import datetime
from typing import Dict, Any, Optional
import logging

logger = logging.getLogger(__name__)


@dataclass
class IndexStats:
    """Statistics for a single index."""
    name: str
    document_count: int = 0
    size_bytes: int = 0
    last_update: Optional[str] = None
    health_status: str = "unknown"
    error_count: int = 0
    avg_index_time_ms: float = 0.0
    total_indexed: int = 0

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            "name": self.name,
            "document_count": self.document_count,
            "size_mb": round(self.size_bytes / (1024 * 1024), 2),
            "last_update": self.last_update,
            "health_status": self.health_status,
            "error_count": self.error_count,
            "avg_index_time_ms": round(self.avg_index_time_ms, 2),
            "total_indexed": self.total_indexed,
        }


@dataclass
class BackendHealth:
    """Health status of a backend."""
    name: str
    is_healthy: bool = False
    response_time_ms: float = 0.0
    last_checked: Optional[str] = None
    error_message: Optional[str] = None
    details: Dict[str, Any] = field(default_factory=dict)

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            "name": self.name,
            "status": "healthy" if self.is_healthy else "unhealthy",
            "response_time_ms": round(self.response_time_ms, 2),
            "last_checked": self.last_checked,
            "error": self.error_message,
            "details": self.details,
        }


@dataclass
class DashboardStats:
    """Complete dashboard statistics."""
    indices: Dict[str, IndexStats] = field(default_factory=dict)
    backends: Dict[str, BackendHealth] = field(default_factory=dict)
    overall_status: str = "unknown"
    last_updated: Optional[str] = None
    uptime_seconds: float = 0.0

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary."""
        return {
            "overall_status": self.overall_status,
            "last_updated": self.last_updated,
            "uptime_seconds": round(self.uptime_seconds, 2),
            "indices": {k: v.to_dict() for k, v in self.indices.items()},
            "backends": {k: v.to_dict() for k, v in self.backends.items()},
        }


class IndexStatisticsCollector:
    """
    Collects statistics from all index components.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Index Statistics Dashboard: CLI command showing index health metrics"

    Collects statistics from:
    - PostgreSQL storage backend
    - Elasticsearch search backend
    - Vector store backend
    - Real-time indexer queue
    """

    def __init__(
        self,
        pg_dsn: Optional[str] = None,
        es_url: Optional[str] = None,
        storage_path: Optional[str] = None
    ):
        """
        Initialize the statistics collector.

        Args:
            pg_dsn: PostgreSQL connection string
            es_url: Elasticsearch URL
            storage_path: Path for local statistics storage
        """
        self.pg_dsn = pg_dsn
        self.es_url = es_url
        self.storage_path = storage_path
        self._start_time = time.time()
        self._cached_stats: Optional[DashboardStats] = None
        self._cache_timestamp = 0
        self._cache_ttl = 30  # seconds

    async def collect_statistics(self, force_refresh: bool = False) -> DashboardStats:
        """
        Collect statistics from all backends.

        Args:
            force_refresh: Force refresh even if cache is valid

        Returns:
            Complete dashboard statistics
        """
        now = time.time()

        # Return cached stats if still valid
        if not force_refresh and self._cached_stats:
            if now - self._cache_timestamp < self._cache_ttl:
                return self._cached_stats

        stats = DashboardStats()
        stats.last_updated = datetime.now().isoformat()
        stats.uptime_seconds = now - self._start_time

        # Collect Elasticsearch statistics
        if self.es_url:
            es_health = await self._check_elasticsearch_health()
            stats.backends["elasticsearch"] = es_health
            if es_health.is_healthy:
                es_stats = await self._collect_elasticsearch_stats()
                stats.indices.update(es_stats)

        # Collect PostgreSQL statistics
        if self.pg_dsn:
            pg_health = await self._check_postgresql_health()
            stats.backends["postgresql"] = pg_health
            if pg_health.is_healthy:
                pg_stats = await self._collect_postgresql_stats()
                stats.indices.update(pg_stats)

        # Determine overall status
        stats.overall_status = self._determine_overall_status(stats)

        # Cache results
        self._cached_stats = stats
        self._cache_timestamp = now

        return stats

    async def _check_elasticsearch_health(self) -> BackendHealth:
        """Check Elasticsearch health."""
        health = BackendHealth(name="elasticsearch")
        health.last_checked = datetime.now().isoformat()

        try:
            from elasticsearch import AsyncElasticsearch

            start = time.time()
            client = AsyncElasticsearch(self.es_url)

            # Basic health check
            info = await client.info()
            health.is_healthy = True
            health.response_time_ms = (time.time() - start) * 1000
            health.details = {
                "version": info.get("version", {}).get("number", "unknown"),
                "cluster_name": info.get("cluster_name", "unknown"),
            }

            await client.close()

        except ImportError:
            health.error_message = "Elasticsearch client not installed"
        except Exception as e:
            health.error_message = str(e)

        return health

    async def _collect_elasticsearch_stats(self) -> Dict[str, IndexStats]:
        """Collect Elasticsearch index statistics."""
        indices: Dict[str, IndexStats] = {}

        try:
            from elasticsearch import AsyncElasticsearch

            client = AsyncElasticsearch(self.es_url)

            # Get all indices
            stats_response = await client.indices.stats(index="*")
            index_names = list(stats_response["indices"].keys())

            for index_name in index_names:
                index_data = stats_response["indices"][index_name]
                stats = IndexStats(name=index_name)
                stats.document_count = index_data.get("primaries", {}).get("docs", {}).get("count", 0)
                stats.size_bytes = index_data.get("primaries", {}).get("store", {}).get("size_in_bytes", 0)
                stats.health_status = "healthy"
                stats.last_update = datetime.now().isoformat()

                indices[index_name] = stats

            await client.close()

        except Exception as e:
            logger.error(f"Error collecting Elasticsearch stats: {e}")

        return indices

    async def _check_postgresql_health(self) -> BackendHealth:
        """Check PostgreSQL health."""
        health = BackendHealth(name="postgresql")
        health.last_checked = datetime.now().isoformat()

        try:
            import asyncpg

            start = time.time()
            conn = await asyncpg.connect(self.pg_dsn)

            # Simple query to check connection
            await conn.fetchval("SELECT 1")

            health.is_healthy = True
            health.response_time_ms = (time.time() - start) * 1000

            await conn.close()

        except ImportError:
            health.error_message = "asyncpg not installed"
        except Exception as e:
            health.error_message = str(e)

        return health

    async def _collect_postgresql_stats(self) -> Dict[str, IndexStats]:
        """Collect PostgreSQL statistics."""
        indices: Dict[str, IndexStats] = {}

        try:
            import asyncpg

            conn = await asyncpg.connect(self.pg_dsn)

            # Get file count
            file_count = await conn.fetchval("SELECT COUNT(*) FROM files WHERE deleted_at IS NULL")

            # Get index size
            size_query = """
                SELECT pg_total_relation_size('files') +
                       pg_total_relation_size('file_versions') +
                       pg_total_relation_size('file_diffs') as total_size
            """
            size_bytes = await conn.fetchval(size_query)

            stats = IndexStats(name="postgresql_files")
            stats.document_count = file_count or 0
            stats.size_bytes = size_bytes or 0
            stats.health_status = "healthy"
            stats.last_update = datetime.now().isoformat()

            indices["postgresql_files"] = stats

            await conn.close()

        except Exception as e:
            logger.error(f"Error collecting PostgreSQL stats: {e}")

        return indices

    def _determine_overall_status(self, stats: DashboardStats) -> str:
        """Determine overall system status."""
        if not stats.backends:
            return "unknown"

        # Check if any critical backend is unhealthy
        critical_backends = ["elasticsearch", "postgresql"]
        for backend_name in critical_backends:
            if backend_name in stats.backends:
                backend = stats.backends[backend_name]
                if not backend.is_healthy:
                    return "unhealthy"

        return "healthy"

    async def get_realtime_queue_stats(self) -> Optional[Dict[str, Any]]:
        """
        Get real-time indexing queue statistics.

        Returns:
            Queue statistics or None if queue is not available
        """
        try:
            # Try to import the realtime_indexer
            from .realtime_indexer import PrioritizedIndexingQueue

            # This is a placeholder - in a real implementation,
            # you would need access to the actual queue instance
            return {
                "queue_size": 0,
                "priority_counts": {
                    "critical": 0,
                    "high": 0,
                    "normal": 0,
                    "low": 0,
                },
                "status": "not_configured",
            }
        except ImportError:
            return None


class DashboardCLI:
    """
    CLI interface for the index statistics dashboard.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Index Statistics Dashboard: CLI command showing index health metrics"

    Usage:
        python -m leindex.stats_dashboard

    Commands:
        stats       Show all statistics
        health      Show backend health
        indices     Show index statistics
        watch       Continuously update statistics
    """

    def __init__(self, collector: IndexStatisticsCollector):
        """
        Initialize the CLI.

        Args:
            collector: Statistics collector instance
        """
        self.collector = collector

    def format_size(self, size_bytes: int) -> str:
        """Format size in human-readable format."""
        for unit in ["B", "KB", "MB", "GB"]:
            if size_bytes < 1024:
                return f"{size_bytes:.1f} {unit}"
            size_bytes /= 1024
        return f"{size_bytes:.1f} TB"

    def format_duration(self, seconds: float) -> str:
        """Format duration in human-readable format."""
        if seconds < 60:
            return f"{seconds:.0f}s"
        elif seconds < 3600:
            return f"{seconds / 60:.1f}m"
        elif seconds < 86400:
            return f"{seconds / 3600:.1f}h"
        else:
            return f"{seconds / 86400:.1f}d"

    def print_header(self, title: str):
        """Print a formatted header."""
        print("\n" + "=" * 60)
        print(f"  {title}")
        print("=" * 60)

    def print_health(self, stats: DashboardStats):
        """Print backend health status."""
        self.print_header("Backend Health")

        for name, health in stats.backends.items():
            status_icon = "OK" if health.is_healthy else "FAIL"
            status_color = "\033[92m" if health.is_healthy else "\033[91m"
            reset = "\033[0m"

            print(f"\n[{status_color}{status_icon}{reset}] {name.upper()}")
            print(f"  Status:        {'Healthy' if health.is_healthy else 'Unhealthy'}")
            print(f"  Response Time: {health.response_time_ms:.0f}ms")
            print(f"  Last Checked:  {health.last_checked}")

            if health.error_message:
                print(f"  Error:         {health.error_message}")

            if health.details:
                print("  Details:")
                for key, value in health.details.items():
                    print(f"    {key}: {value}")

    def print_indices(self, stats: DashboardStats):
        """Print index statistics."""
        self.print_header("Index Statistics")

        if not stats.indices:
            print("\nNo indices found.")
            return

        for name, index in stats.indices.items():
            print(f"\n{name}:")
            print(f"  Documents:     {index.document_count:,}")
            print(f"  Size:          {self.format_size(index.size_bytes)}")
            print(f"  Health:        {index.health_status}")
            print(f"  Last Update:   {index.last_update}")

            if index.total_indexed > 0:
                print(f"  Total Indexed: {index.total_indexed:,}")
                print(f"  Avg Time:      {index.avg_index_time_ms:.0f}ms")

            if index.error_count > 0:
                print(f"  Errors:        {index.error_count}")

    def print_summary(self, stats: DashboardStats):
        """Print overall summary."""
        self.print_header("Summary")

        status_icon = "OK" if stats.overall_status == "healthy" else "WARN"
        status_color = "\033[92m" if stats.overall_status == "healthy" else "\033[93m"
        reset = "\033[0m"

        total_docs = sum(idx.document_count for idx in stats.indices.values())
        total_size = sum(idx.size_bytes for idx in stats.indices.values())

        print(f"\nOverall Status:  [{status_color}{status_icon}{reset}] {stats.overall_status.upper()}")
        print(f"Uptime:          {self.format_duration(stats.uptime_seconds)}")
        print(f"Total Documents: {total_docs:,}")
        print(f"Total Size:      {self.format_size(total_size)}")
        print(f"Last Updated:    {stats.last_updated}")

    async def show_stats(self, watch: bool = False, interval: int = 5):
        """
        Show statistics dashboard.

        Args:
            watch: Continuously update
            interval: Update interval in seconds
        """
        try:
            while True:
                stats = await self.collector.collect_statistics(force_refresh=True)

                # Clear screen for watch mode
                if watch:
                    os.system("clear" if os.name != "nt" else "cls")

                self.print_summary(stats)
                self.print_health(stats)
                self.print_indices(stats)

                if not watch:
                    break

                print(f"\nRefreshing every {interval}s... (Ctrl+C to exit)")
                await asyncio.sleep(interval)

        except KeyboardInterrupt:
            print("\n\nExiting dashboard.")

    async def show_json(self):
        """Show statistics in JSON format."""
        stats = await self.collector.collect_statistics(force_refresh=True)
        print(json.dumps(stats.to_dict(), indent=2))


async def main():
    """Main CLI entry point."""
    import argparse

    parser = argparse.ArgumentParser(description="Index Statistics Dashboard")
    parser.add_argument("--es-url", default=os.getenv("ELASTICSEARCH_URL", "http://localhost:9200"),
                        help="Elasticsearch URL")
    parser.add_argument("--pg-dsn", default=os.getenv("DATABASE_URL"),
                        help="PostgreSQL connection string")
    parser.add_argument("--json", action="store_true", help="Output in JSON format")
    parser.add_argument("--watch", action="store_true", help="Continuously update")
    parser.add_argument("--interval", type=int, default=5, help="Update interval for watch mode")

    args = parser.parse_args()

    # Create collector
    collector = IndexStatisticsCollector(
        pg_dsn=args.pg_dsn,
        es_url=args.es_url,
    )

    # Create CLI
    cli = DashboardCLI(collector)

    if args.json:
        await cli.show_json()
    else:
        await cli.show_stats(watch=args.watch, interval=args.interval)


if __name__ == "__main__":
    asyncio.run(main())
