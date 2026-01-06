import sqlite3
import json
import os
from typing import Dict, Any, List, Optional

from leindex.content_extractor import ContentExtractor # Import ContentExtractor
from leindex.logger_config import logger # Import the centralized logger

# Security: Allowlist of valid table names to prevent SQL injection
ALLOWED_TABLES = {'kv_store', 'file_versions', 'file_diffs'}

# Placeholder for base_path, will be initialized in __init__
BASE_PATH = ""

class ETLScript:
    def __init__(self, sqlite_db_path: str, base_path: str = ""):
        """
        Initialize ETL script for SQLite data extraction and transformation.

        Args:
            sqlite_db_path: Path to SQLite database
            base_path: Base path for content extraction
        """
        self.sqlite_db_path = sqlite_db_path
        self.base_path = base_path # Store base_path
        self.sqlite_conn = None
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

    def close_connections(self):
        """Closes SQLite connection."""
        if self.sqlite_conn:
            self.sqlite_conn.close()
            logger.info("SQLite connection closed.")

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
        """Transforms kv_store data for export."""
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
                    'value': value, # Store as text/json
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

    def transform_file_versions(self, data: List[Dict[str, Any]]) -> List[Dict[str, Any]]:
        """Transforms file_versions data for export."""
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
        """Transforms file_diffs data for export."""
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

    def run_etl(self):
        """Runs the full ETL process for data extraction and transformation."""
        try:
            # Set BASE_PATH globally for ContentExtractor if needed, or pass it directly
            # For this script, it's passed via __init__
            self.connect_sqlite()

            # Initial full load (if no last_migrated_at) or incremental load
            tables = ['kv_store', 'file_versions', 'file_diffs']
            for table_name in tables:
                logger.info(f"Starting ETL for {table_name} table...")

                data = self.extract_incremental_data(table_name, None)

                if table_name == 'kv_store':
                    transformed_data = self.transform_kv_store(data)
                    logger.info(f"Transformed {len(transformed_data)} rows from {table_name}")
                elif table_name == 'file_versions':
                    transformed_data = self.transform_file_versions(data)
                    logger.info(f"Transformed {len(transformed_data)} rows from {table_name}")
                elif table_name == 'file_diffs':
                    transformed_data = self.transform_file_diffs(data)
                    logger.info(f"Transformed {len(transformed_data)} rows from {table_name}")

                logger.info(f"Completed ETL for {table_name} table.")

            logger.info("ETL process completed successfully.")

        except sqlite3.Error as e:
            logger.critical(f"SQLite error during ETL process: {e}")
        except Exception as e:
            logger.critical(f"An unexpected error occurred during ETL process: {e}")
        finally:
            self.close_connections()

if __name__ == "__main__":
    # Example usage:
    # These should be configured based on your environment

    # Placeholder for SQLite DB path (adjust as needed)
    # You might want to get this from a config file or environment variable
    sqlite_db_path = os.getenv("SQLITE_DB_PATH", "data/code_index.sqlite")

    # Determine the base path for file content extraction
    # This should typically be the root of the project being indexed
    base_path = os.getenv("CODE_BASE_PATH", os.getcwd()) # Default to current working directory

    # Ensure the directory for the SQLite DB exists if it's a new path
    db_dir = os.path.dirname(sqlite_db_path)
    if db_dir and not os.path.exists(db_dir):
        os.makedirs(db_dir, exist_ok=True)

    etl = ETLScript(sqlite_db_path, base_path)
    etl.run_etl()
