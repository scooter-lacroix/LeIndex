import logging
from datetime import datetime

# Optional PostgreSQL dependencies
try:
    import psycopg2
    PSYCOPG2_AVAILABLE = True
except ImportError:
    PSYCOPG2_AVAILABLE = False
    psycopg2 = None  # type: ignore

from elasticsearch import Elasticsearch, ConnectionError, NotFoundError

# Configure logging
logger = logging.getLogger(__name__)

class DatabaseMonitor:
    def __init__(self, db_config):
        self.db_config = db_config

    def _get_db_connection(self):
        if not PSYCOPG2_AVAILABLE:
            logger.error("psycopg2 is not installed. Install with: pip install leindex[postgresql]")
            return None
        try:
            conn = psycopg2.connect(**self.db_config)
            return conn
        except psycopg2.Error as e:
            logger.error(f"Database connection error: {e}")
            return None

    def collect_postgresql_metrics(self):
        if not PSYCOPG2_AVAILABLE:
            logger.warning("PostgreSQL metrics collection skipped: psycopg2 not installed")
            return {}

        metrics = {}
        conn = None
        try:
            conn = self._get_db_connection()
            if conn:
                with conn.cursor() as cur:
                    # CPU and Memory (indirectly via pg_stat_activity)
                    cur.execute("SELECT pid, usename, datname, application_name, client_addr, backend_start, state, query_start, query FROM pg_stat_activity WHERE datname = current_database();")
                    activity = cur.fetchall()
                    metrics['pg_stat_activity'] = [dict(zip([col[0] for col in cur.description], row)) for row in activity]

                    # Disk I/O (via pg_stat_io) - PostgreSQL 16+
                    try:
                        cur.execute("SELECT backend_type, object, reads, writes, op_bytes, backend_type, op_bytes FROM pg_stat_io;")
                        io_stats = cur.fetchall()
                        metrics['pg_stat_io'] = [dict(zip([col[0] for col in cur.description], row)) for row in io_stats]
                    except psycopg2.ProgrammingError:
                        logger.warning("pg_stat_io not available or PostgreSQL version < 16. Skipping disk I/O metrics.")

                    # Query Performance (via pg_stat_statements)
                    try:
                        cur.execute("SELECT query, calls, total_exec_time, mean_exec_time, rows FROM pg_stat_statements ORDER BY total_exec_time DESC LIMIT 10;")
                        query_stats = cur.fetchall()
                        metrics['pg_stat_statements'] = [dict(zip([col[0] for col in cur.description], row)) for row in query_stats]
                    except psycopg2.ProgrammingError:
                        logger.warning("pg_stat_statements not enabled or available. Skipping query performance metrics.")

                logger.info(f"PostgreSQL metrics collected at {datetime.now()}: {metrics}")
        except Exception as e:
            logger.error(f"Error collecting PostgreSQL metrics: {e}")
        finally:
            if conn:
                conn.close()
        return metrics

class ElasticsearchMonitor:
    def __init__(self, es_config):
        self.es_config = es_config
        # Add compatibility headers for Elasticsearch 8.x
        config_with_headers = es_config.copy()
        config_with_headers["headers"] = {"Accept": "application/vnd.elasticsearch+json; compatible-with=8"}
        self.es = Elasticsearch(**config_with_headers)

    def collect_elasticsearch_metrics(self):
        metrics = {}
        try:
            # Cluster Health
            cluster_health = self.es.cluster.health()
            metrics['cluster_health'] = cluster_health

            # Cluster Stats
            cluster_stats = self.es.cluster.stats()
            metrics['cluster_stats'] = cluster_stats

            # Node Stats
            node_stats = self.es.nodes.stats()
            metrics['node_stats'] = node_stats

            # Index Stats (example for all indices)
            index_stats = self.es.indices.stats()
            metrics['index_stats'] = index_stats

            logger.info(f"Elasticsearch metrics collected at {datetime.now()}: {metrics}")
        except ConnectionError as e:
            logger.error(f"Elasticsearch connection error: {e}")
        except NotFoundError as e:
            logger.error(f"Elasticsearch API not found error: {e}")
        except Exception as e:
            logger.error(f"Error collecting Elasticsearch metrics: {e}")
        return metrics

if __name__ == '__main__':
    # Example Usage (replace with actual configuration)
    logging.basicConfig(level=logging.INFO)

    db_config = {
        'host': 'localhost',
        'database': 'code_index_db',
        'user': 'user',
        'password': 'password',
        'port': '5432'
    }
    es_config = {
        'hosts': ['http://localhost:9200']
    }

    db_monitor = DatabaseMonitor(db_config)
    pg_metrics = db_monitor.collect_postgresql_metrics()
    print("\n--- PostgreSQL Metrics ---")
    print(pg_metrics)

    es_monitor = ElasticsearchMonitor(es_config)
    es_metrics = es_monitor.collect_elasticsearch_metrics()
    print("\n--- Elasticsearch Metrics ---")
    print(es_metrics)