import sqlite3
import json
import os
from typing import Dict, Any, List, Optional

# Optional PostgreSQL dependencies
try:
    import psycopg2
    PSYCOPG2_AVAILABLE = True
except ImportError:
    PSYCOPG2_AVAILABLE = False
    psycopg2 = None  # type: ignore

from elasticsearch import Elasticsearch, helpers
from leindex.content_extractor import ContentExtractor # Import ContentExtractor
from leindex.logger_config import logger # Import the centralized logger

# Security: Allowlist of valid table names to prevent SQL injection
ALLOWED_TABLES = {'kv_store', 'file_versions', 'file_diffs'}

# Placeholder for base_path, will be initialized in __init__
BASE_PATH = ""

class ETLScript:
    def __init__(self, sqlite_db_path: str, pg_conn_string: Optional[str] = None, es_hosts: Optional[List[str]] = None, es_index_name: Optional[str] = None, base_path: str = ""):
        """
        Initialize ETL script with optional PostgreSQL and Elasticsearch targets.

        Args:
            sqlite_db_path: Path to SQLite database
            pg_conn_string: Optional PostgreSQL connection string (requires psycopg2)
            es_hosts: Optional Elasticsearch hosts
            es_index_name: Optional Elasticsearch index name
            base_path: Base path for content extraction
        """
        self.sqlite_db_path = sqlite_db_path
        self.pg_conn_string = pg_conn_string
        self.es_hosts = es_hosts
        self.es_index_name = es_index_name
        self.base_path = base_path # Store base_path
        self.sqlite_conn = None
        self.pg_conn = None
        self.es_client = None
        self.content_extractor = ContentExtractor(self.base_path) # Initialize ContentExtractor

    def connect_sqlite(self):
        """Establishes a connection to the SQLite database."""
        try:
            self.sqlite_conn = sqlite3.connect(self.sqlite_db_path)
            self.sqlite_conn.row_factory = sqlite3.Row # Allows accessing columns by name
            logger.info(f"Connected to SQLite database: {self.sqlite_db_path}")
        except sqlite3.Error as e:
            logger.error(f"Error connecting to SQLite database: {e}")
            raise

    def connect_postgresql(self):
        """Establishes a connection to the PostgreSQL database."""
        if not PSYCOPG2_AVAILABLE:
            raise ImportError(
                "psycopg2 is required for PostgreSQL connection. "
                "Install with: pip install leindex[postgresql]"
            )
        if not self.pg_conn_string:
            raise ValueError("PostgreSQL connection string not provided")
        try:
            self.pg_conn = psycopg2.connect(self.pg_conn_string)
            self.pg_conn.autocommit = False # Manage transactions manually
            logger.info("Connected to PostgreSQL database.")
        except psycopg2.Error as e:
            logger.error(f"Error connecting to PostgreSQL database: {e}")
            raise

    def connect_elasticsearch(self):
        """Establishes a connection to Elasticsearch."""
        if not self.es_hosts:
            raise ValueError("Elasticsearch hosts not provided")
        try:
            self.es_client = Elasticsearch(self.es_hosts)
            if not self.es_client.ping():
                raise ValueError("Connection to Elasticsearch failed!")
            logger.info(f"Connected to Elasticsearch at {self.es_hosts}")
        except Exception as e:
            logger.error(f"Error connecting to Elasticsearch: {e}")
            raise

    def create_es_index(self):
        """Creates the Elasticsearch index with a predefined mapping if it doesn't exist."""
        if not self.es_client:
            self.connect_elasticsearch()

        # Define the index mapping for file content
        # This mapping should align with your indexing strategy
        mapping = {
            "mappings": {
                "properties": {
                    "file_id": {"type": "keyword"},
                    "file_path": {"type": "text"},
                    "content": {"type": "text"},
                    "hash": {"type": "keyword"},
                    "timestamp": {"type": "date"},
                    "size": {"type": "long"},
                    "last_modified": {"type": "date"},
                    "deleted_at": {"type": "date"},
                    # Add other fields as per your indexing strategy, e.g., extracted_text, metadata
                }
            }
        }

        try:
            if not self.es_client.indices.exists(index=self.es_index_name):
                self.es_client.indices.create(index=self.es_index_name, body=mapping)
                logger.info(f"Elasticsearch index '{self.es_index_name}' created with mapping.")
            else:
                logger.info(f"Elasticsearch index '{self.es_index_name}' already exists.")
        except Exception as e:
            logger.error(f"Error creating Elasticsearch index '{self.es_index_name}': {e}")
            raise

    def close_connections(self):
        """Closes both SQLite and PostgreSQL connections."""
        if self.sqlite_conn:
            self.sqlite_conn.close()
            logger.info("SQLite connection closed.")
        if self.pg_conn:
            self.pg_conn.close()
            logger.info("PostgreSQL connection closed.")
        if self.es_client:
            # No explicit close method for Elasticsearch client, but good to log
            logger.info("Elasticsearch client connection implicitly closed.")

    def extract_data(self, table_name: str) -> List[Dict[str, Any]]:
        """Extracts all data from a given SQLite table."""
        if not self.sqlite_conn:
            self.connect_sqlite()

        # Security: Validate table name against allowlist to prevent SQL injection
        if table_name not in ALLOWED_TABLES:
            raise ValueError(f"Invalid table name: {table_name}. Allowed tables: {ALLOWED_TABLES}")

        try:
            cursor = self.sqlite_conn.cursor()
            cursor.execute(f"SELECT * FROM {table_name}")
            rows = cursor.fetchall()
            logger.info(f"Extracted {len(rows)} rows from SQLite table: {table_name}")
            return [dict(row) for row in rows]
        except sqlite3.Error as e:
            logger.error(f"Error extracting data from {table_name}: {e}")
            return []

    def get_last_migration_timestamp(self, table_name: str) -> Optional[str]:
        """Retrieves the last migration timestamp for a given table from PostgreSQL."""
        if not self.pg_conn:
            self.connect_postgresql()
        try:
            cursor = self.pg_conn.cursor()
            # Assuming a metadata table or a way to store last migration timestamps
            # For simplicity, we'll assume a table `migration_metadata` with `table_name` and `last_migrated_at`
            cursor.execute("SELECT last_migrated_at FROM migration_metadata WHERE table_name = %s", (table_name,))
            result = cursor.fetchone()
            if result:
                return result[0]
            return None
        except psycopg2.Error as e:
            logger.error(f"Error retrieving last migration timestamp for {table_name}: {e}")
            return None

    def update_last_migration_timestamp(self, table_name: str, timestamp: str):
        """Updates the last migration timestamp for a given table in PostgreSQL."""
        if not self.pg_conn:
            self.connect_postgresql()
        try:
            cursor = self.pg_conn.cursor()
            insert_sql = """
                INSERT INTO migration_metadata (table_name, last_migrated_at)
                VALUES (%s, %s)
                ON CONFLICT (table_name) DO UPDATE SET
                    last_migrated_at = EXCLUDED.last_migrated_at;
            """
            cursor.execute(insert_sql, (table_name, timestamp))
            self.pg_conn.commit()
            logger.info(f"Updated last migration timestamp for {table_name} to {timestamp}")
        except psycopg2.Error as e:
            self.pg_conn.rollback()
            logger.error(f"Error updating last migration timestamp for {table_name}: {e}")
            raise

    def extract_incremental_data(self, table_name: str, last_migrated_at: Optional[str]) -> List[Dict[str, Any]]:
        """Extracts incremental data from a given SQLite table based on last_modified timestamp."""
        if not self.sqlite_conn:
            self.connect_sqlite()

        # Security: Validate table name against allowlist to prevent SQL injection
        if table_name not in ALLOWED_TABLES:
            raise ValueError(f"Invalid table name: {table_name}. Allowed tables: {ALLOWED_TABLES}")

        try:
            cursor = self.sqlite_conn.cursor()
            if last_migrated_at:
                # Assuming 'last_modified' column exists in SQLite tables
                cursor.execute(f"SELECT * FROM {table_name} WHERE last_modified > ?", (last_migrated_at,))
                logger.info(f"Extracting incremental data from {table_name} where last_modified > {last_migrated_at}")
            else:
                cursor.execute(f"SELECT * FROM {table_name}")
                logger.info(f"Extracting all data from {table_name} (initial load or no last_migrated_at)")

            rows = cursor.fetchall()
            logger.info(f"Extracted {len(rows)} incremental rows from SQLite table: {table_name}")
            return [dict(row) for row in rows]
        except sqlite3.Error as e:
            logger.error(f"Error extracting incremental data from {table_name}: {e}")
            return []

    def transform_kv_store(self, data: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        """Transforms kv_store data for PostgreSQL."""
        transformed_data = []
        for row in data:
            try:
                value_blob = row['value']
                value_type = row['value_type']
                
                # Decode BLOB content based on value_type
                if value_type == 'text':
                    value = value_blob.decode('utf-8')
                elif value_type == 'json':
                    value = json.loads(value_blob.decode('utf-8'))
                else:
                    value = None # Or handle other types as needed
                    logger.warning(f"Unknown value_type '{value_type}' for key '{row['key']}'. Value set to None.")

                transformed_data.append({
                    'key': row['key'],
                    'value': value, # Store as text/json in PG, or bytea if original BLOB is needed
                    'value_type': row['value_type'],
                    'created_at': row['created_at'],
                    'updated_at': row['updated_at'],
                    'last_modified': row['updated_at'], # Using updated_at as last_modified for kv_store
                    'deleted_at': row.get('deleted_at') # Add deleted_at for soft deletes
                })
            except Exception as e:
                logger.error(f"Error transforming kv_store row (key: {row.get('key')}): {e}")
                continue
        return transformed_data

    def transform_for_elasticsearch(self, data: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        """Transforms file_versions data into a format suitable for Elasticsearch bulk indexing."""
        es_documents = []
        for row in data:
            try:
                # Assuming 'file_id' can be derived from 'file_path' or 'version_id'
                # For idempotency, using a unique ID like version_id is good.
                # If file_id needs to be consistent across versions of the same file,
                # you might need a separate lookup or a different strategy.
                file_id = row['version_id'] # Using version_id as _id for now

                # Handle BLOB content: if large, store reference; otherwise, index directly.
                # For simplicity, assuming content is always text and indexing directly.
                # In a real scenario, you'd check file size or content type.
                content_blob = row['content']
                content = content_blob.decode('utf-8') # Assuming content is always text

                document = {
                    "_index": self.es_index_name,
                    "_id": file_id,
                    "_source": {
                        "file_id": file_id,
                        "file_path": row['file_path'],
                        "content": content, # Indexing content directly
                        "hash": row['hash'],
                        "timestamp": row['timestamp'],
                        "size": row['size'],
                        "last_modified": row['timestamp'],
                        "deleted_at": row.get('deleted_at'),
                        # Add other fields as per your indexing strategy
                    }
                }
                es_documents.append(document)
            except Exception as e:
                logger.error(f"Error transforming file_versions row for Elasticsearch (version_id: {row.get('version_id')}): {e}")
                continue
        return es_documents

    def transform_file_versions(self, data: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        """Transforms file_versions data for PostgreSQL."""
        transformed_data = []
        for row in data:
            try:
                content_blob = row['content']
                content = content_blob.decode('utf-8') # Assuming content is always text
                
                transformed_data.append({
                    'version_id': row['version_id'],
                    'file_path': row['file_path'],
                    'content': content,
                    'hash': row['hash'],
                    'timestamp': row['timestamp'],
                    'size': row['size'],
                    'last_modified': row['timestamp'], # Using timestamp as last_modified for file_versions
                    'deleted_at': row.get('deleted_at') # Add deleted_at for soft deletes
                })
            except Exception as e:
                logger.error(f"Error transforming file_versions row (version_id: {row.get('version_id')}): {e}")
                continue
        return transformed_data

    def transform_file_diffs(self, data: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        """Transforms file_diffs data for PostgreSQL."""
        transformed_data = []
        for row in data:
            try:
                diff_content_blob = row['diff_content']
                diff_content = diff_content_blob.decode('utf-8') # Assuming diff_content is always text

                transformed_data.append({
                    'diff_id': row['diff_id'],
                    'file_path': row['file_path'],
                    'previous_version_id': row['previous_version_id'],
                    'current_version_id': row['current_version_id'],
                    'diff_content': diff_content,
                    'diff_type': row['diff_type'],
                    'operation_type': row['operation_type'],
                    'operation_details': row['operation_details'],
                    'timestamp': row['timestamp'],
                    'last_modified': row['timestamp'], # Using timestamp as last_modified for file_diffs
                    'deleted_at': row.get('deleted_at') # Add deleted_at for soft deletes
                })
            except Exception as e:
                logger.error(f"Error transforming file_diffs row (diff_id: {row.get('diff_id')}): {e}")
                continue
        return transformed_data

    def load_kv_store(self, data: List[Dict[str, Any]]):
        """Loads transformed kv_store data into PostgreSQL."""
        if not self.pg_conn:
            self.connect_postgresql()
        
        try:
            cursor = self.pg_conn.cursor()
            # Assuming PostgreSQL table 'kv_store' has columns: key, value, value_type, created_at, updated_at
            # Adjust column types in PG to match: key (TEXT/VARCHAR), value (TEXT/JSONB/BYTEA), value_type (TEXT/VARCHAR), created_at (TIMESTAMP), updated_at (TIMESTAMP), last_modified (TIMESTAMP), deleted_at (TIMESTAMP)
            insert_sql = """
                INSERT INTO kv_store (key, value, value_type, created_at, updated_at, last_modified, deleted_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s)
                ON CONFLICT (key) DO UPDATE SET
                    value = EXCLUDED.value,
                    value_type = EXCLUDED.value_type,
                    updated_at = EXCLUDED.updated_at,
                    last_modified = EXCLUDED.last_modified,
                    deleted_at = EXCLUDED.deleted_at;
            """
            for row in data:
                # Convert Python objects to JSON string for PostgreSQL JSONB/TEXT column if value_type is 'json'
                # Otherwise, keep as string for 'text' type or handle as bytea if original BLOB is needed
                value_to_insert = row['value']
                if row['value_type'] == 'json':
                    value_to_insert = json.dumps(value_to_insert)

                cursor.execute(insert_sql, (
                    row['key'],
                    value_to_insert,
                    row['value_type'],
                    row['created_at'],
                    row['updated_at'],
                    row['last_modified'],
                    row['deleted_at']
                ))
            self.pg_conn.commit()
            logger.info(f"Loaded {len(data)} rows into PostgreSQL table: kv_store")
        except psycopg2.Error as e:
            self.pg_conn.rollback()
            logger.error(f"Error loading data into kv_store: {e}")
            raise

    def load_file_versions(self, data: List[Dict[str, Any]]):
        """Loads transformed file_versions data into PostgreSQL."""
        if not self.pg_conn:
            self.connect_postgresql()
        
        try:
            cursor = self.pg_conn.cursor()
            # Assuming PostgreSQL table 'file_versions' has columns: version_id, file_path, content, hash, timestamp, size, last_modified
            # Adjust column types in PG to match: version_id (TEXT/VARCHAR), file_path (TEXT/VARCHAR), content (TEXT/BYTEA), hash (TEXT/VARCHAR), timestamp (TIMESTAMP), size (BIGINT/INTEGER), last_modified (TIMESTAMP), deleted_at (TIMESTAMP)
            insert_sql = """
                INSERT INTO file_versions (version_id, file_path, content, hash, timestamp, size, last_modified, deleted_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s)
                ON CONFLICT (version_id) DO UPDATE SET
                    file_path = EXCLUDED.file_path,
                    content = EXCLUDED.content,
                    hash = EXCLUDED.hash,
                    timestamp = EXCLUDED.timestamp,
                    size = EXCLUDED.size,
                    last_modified = EXCLUDED.last_modified,
                    deleted_at = EXCLUDED.deleted_at;
            """
            for row in data:
                cursor.execute(insert_sql, (
                    row['version_id'],
                    row['file_path'],
                    row['content'],
                    row['hash'],
                    row['timestamp'],
                    row['size'],
                    row['last_modified'],
                    row['deleted_at']
                ))
            self.pg_conn.commit()
            logger.info(f"Loaded {len(data)} rows into PostgreSQL table: file_versions")
        except psycopg2.Error as e:
            self.pg_conn.rollback()
            logger.error(f"Error loading data into file_versions: {e}")
            raise

    def load_file_diffs(self, data: List[Dict[str, Any]]):
        """Loads transformed file_diffs data into PostgreSQL."""
        if not self.pg_conn:
            self.connect_postgresql()
        
        try:
            cursor = self.pg_conn.cursor()
            # Assuming PostgreSQL table 'file_diffs' has columns: diff_id, file_path, previous_version_id, current_version_id, diff_content, diff_type, operation_type, operation_details, timestamp, last_modified
            # Adjust column types in PG to match: diff_id (TEXT/VARCHAR), file_path (TEXT/VARCHAR), previous_version_id (TEXT/VARCHAR), current_version_id (TEXT/VARCHAR), diff_content (TEXT/BYTEA), diff_type (TEXT/VARCHAR), operation_type (TEXT/VARCHAR), operation_details (TEXT/VARCHAR), timestamp (TIMESTAMP), last_modified (TIMESTAMP), deleted_at (TIMESTAMP)
            insert_sql = """
                INSERT INTO file_diffs (diff_id, file_path, previous_version_id, current_version_id, diff_content, diff_type, operation_type, operation_details, timestamp, last_modified, deleted_at)
                VALUES (%s, %s, %s, %s, %s, %s, %s, %s, %s, %s, %s)
                ON CONFLICT (diff_id) DO UPDATE SET
                    file_path = EXCLUDED.file_path,
                    previous_version_id = EXCLUDED.previous_version_id,
                    current_version_id = EXCLUDED.current_version_id,
                    diff_content = EXCLUDED.diff_content,
                    diff_type = EXCLUDED.diff_type,
                    operation_type = EXCLUDED.operation_type,
                    operation_details = EXCLUDED.operation_details,
                    timestamp = EXCLUDED.timestamp,
                    last_modified = EXCLUDED.last_modified,
                    deleted_at = EXCLUDED.deleted_at;
            """
            for row in data:
                cursor.execute(insert_sql, (
                    row['diff_id'],
                    row['file_path'],
                    row['previous_version_id'],
                    row['current_version_id'],
                    row['diff_content'],
                    row['diff_type'],
                    row['operation_type'],
                    row['operation_details'],
                    row['timestamp'],
                    row['last_modified'],
                    row['deleted_at']
                ))
            self.pg_conn.commit()
            logger.info(f"Loaded {len(data)} rows into PostgreSQL table: file_diffs")
        except psycopg2.Error as e:
            self.pg_conn.rollback()
            logger.error(f"Error loading data into file_diffs: {e}")
            raise

    def load_to_elasticsearch(self, documents: List[Dict[str, Any]]):
        """Loads transformed documents into Elasticsearch using bulk API."""
        if not self.es_client:
            self.connect_elasticsearch()

        if not documents:
            logger.info("No documents to load into Elasticsearch.")
            return

        try:
            # The 'helpers.bulk' function handles batching and retries
            success, failed = helpers.bulk(self.es_client, documents, index=self.es_index_name, chunk_size=500, raise_on_error=False)
            logger.info(f"Successfully indexed {success} documents into Elasticsearch.")
            if failed:
                logger.warning(f"Failed to index {len(failed)} documents into Elasticsearch. First 5 errors: {failed[:5]}")
        except Exception as e:
            logger.error(f"Error during Elasticsearch bulk indexing: {e}")
            raise

    def run_etl(self):
        """Runs the full ETL process."""
        try:
            # Set BASE_PATH globally for ContentExtractor if needed, or pass it directly
            # For this script, it's passed via __init__
            self.connect_sqlite()
            # Conditionally connect to PostgreSQL and Elasticsearch only if configured
            if self.pg_conn_string:
                self.connect_postgresql()
            if self.es_hosts:
                self.connect_elasticsearch()
            self.create_es_index() # Ensure index exists before loading

            # Initial full load (if no last_migrated_at) or incremental load
            tables = ['kv_store', 'file_versions', 'file_diffs']
            for table_name in tables:
                logger.info(f"Starting ETL for {table_name} table...")
                last_migrated_at = self.get_last_migration_timestamp(table_name)
                
                data = self.extract_incremental_data(table_name, last_migrated_at)
                
                if table_name == 'kv_store':
                    transformed_data = self.transform_kv_store(data)
                    self.load_kv_store(transformed_data)
                elif table_name == 'file_versions':
                    transformed_data = self.transform_file_versions(data)
                    self.load_file_versions(transformed_data)
                    es_documents = self.transform_for_elasticsearch(data)
                    self.load_to_elasticsearch(es_documents)
                elif table_name == 'file_diffs':
                    transformed_data = self.transform_file_diffs(data)
                    self.load_file_diffs(transformed_data)
                
                if data: # Only update timestamp if there was data to process
                    # Find the maximum last_modified timestamp from the processed data
                    # Note: This assumes 'last_modified' is present in all extracted data.
                    # For Elasticsearch, we might track a separate timestamp for ES sync.
                    max_timestamp = max(row['last_modified'] for row in data)
                    self.update_last_migration_timestamp(table_name, max_timestamp)
                
                logger.info(f"Completed ETL for {table_name} table.")

            logger.info("ETL process completed successfully.")

        except psycopg2.Error as e:
            logger.critical(f"PostgreSQL error during ETL process: {e}")
            self.pg_conn.rollback() # Ensure rollback on critical PG errors
        except sqlite3.Error as e:
            logger.critical(f"SQLite error during ETL process: {e}")
        except Exception as e:
            logger.critical(f"An unexpected error occurred during ETL process: {e}")
        finally:
            self.close_connections()

if __name__ == "__main__":
    # Example usage:
    # These should be configured based on your environment
    # For SQLite, use a relative or absolute path to your .sqlite file
    # For PostgreSQL, use a connection string like "dbname=your_db user=your_user password=your_password host=your_host port=your_port"
    
    # Placeholder for SQLite DB path (adjust as needed)
    # You might want to get this from a config file or environment variable
    sqlite_db_path = os.getenv("SQLITE_DB_PATH", "data/code_index.sqlite")
    
    # Placeholder for PostgreSQL connection string (adjust as needed)
    # You might want to get this from a config file or environment variable
    pg_conn_string = os.getenv("PG_CONN_STRING", "dbname=code_index user=postgres password=password host=localhost port=5432")

    # Placeholder for Elasticsearch hosts (adjust as needed)
    # Can be a list of hostnames or IPs, e.g., ["localhost:9200"]
    es_hosts = os.getenv("ES_HOSTS", "http://localhost:9200").split(',')
    es_index_name = os.getenv("ES_INDEX_NAME", "code_index")
    
    # Determine the base path for file content extraction
    # This should typically be the root of the project being indexed
    base_path = os.getenv("CODE_BASE_PATH", os.getcwd()) # Default to current working directory

    # Ensure the directory for the SQLite DB exists if it's a new path
    db_dir = os.path.dirname(sqlite_db_path)
    if db_dir and not os.path.exists(db_dir):
        os.makedirs(db_dir, exist_ok=True)

    etl = ETLScript(sqlite_db_path, pg_conn_string, es_hosts, es_index_name, base_path)
    etl.run_etl()