#!/usr/bin/env python
"""
Development convenience script to run the Code Index MCP server.
"""
import sys
import os
import traceback

# Add src directory to path
src_path = os.path.join(os.path.dirname(os.path.abspath(__file__)), 'src')
sys.path.insert(0, src_path)

try:
    from leindex.server import main
    from leindex.config_manager import ConfigManager
    from leindex.monitoring import DatabaseMonitor, ElasticsearchMonitor
    from leindex.logger_config import logger
    import subprocess
    import time
    import threading
    from typing import List
    from leindex.constants import ES_HOST, ES_PORT, RABBITMQ_HOST, RABBITMQ_PORT # Import ES and RabbitMQ constants

    def run_docker_compose_command(command: List[str]):
        """Helper to run docker compose commands."""
        try:
            print(f"Executing: docker compose {' '.join(command)}", file=sys.stderr)
            subprocess.run(["docker", "compose"] + command, check=True, cwd=os.path.dirname(os.path.abspath(__file__)))
            print(f"Docker compose {' '.join(command)} completed successfully.", file=sys.stderr)
        except subprocess.CalledProcessError as e:
            print(f"Error running docker compose {' '.join(command)}: {e}", file=sys.stderr)
            sys.exit(1)
        except FileNotFoundError:
            print("Error: 'docker compose' command not found. Please ensure Docker Desktop or Docker Engine is installed and running.", file=sys.stderr)
            sys.exit(1)

    if __name__ == "__main__":
        if len(sys.argv) > 1:
            command = sys.argv[1]
            if command == "start-dev-dbs":
                run_docker_compose_command(["up", "-d", "db", "elasticsearch"])
                sys.exit(0)
            elif command == "stop-dev-dbs":
                run_docker_compose_command(["down", "--volumes", "db", "elasticsearch"])
                sys.exit(0)
            elif command == "restart-dev-dbs":
                run_docker_compose_command(["down", "--volumes", "db", "elasticsearch"])
                run_docker_compose_command(["up", "-d", "db", "elasticsearch"])
                sys.exit(0)
            elif command == "server":
                print("Starting Code Index MCP server...", file=sys.stderr)
                print(f"Added path: {src_path}", file=sys.stderr)

                # Set auto-install environment variable for development
                os.environ['LEINDEX_AUTO_INSTALL_ES'] = '1'

                # Initialize ConfigManager
                config_manager = ConfigManager()
                dal_settings = config_manager.get_dal_settings()

                # Setup and start monitoring in a separate thread
                def start_monitoring():
                    db_config = {
                        'host': 'localhost',
                        'database': dal_settings.get('postgresql_database', 'code_index_db'),
                        'user': dal_settings.get('postgresql_user', 'user'),
                        'password': dal_settings.get('postgresql_password', 'password'),
                        'port': int(os.getenv('POSTGRES_PORT', dal_settings.get('postgresql_port', '5432')))
                    }
                    es_config = {
                        'hosts': [{"host": ES_HOST, "port": ES_PORT, "scheme": "http"}]
                    }

                    db_monitor = DatabaseMonitor(db_config)
                    es_monitor = ElasticsearchMonitor(es_config)

                    while True:
                        logger.info("Collecting database and Elasticsearch metrics...")
                        db_metrics = db_monitor.collect_postgresql_metrics()
                        es_metrics = es_monitor.collect_elasticsearch_metrics()
                        
                        # Log metrics with appropriate context
                        logger.info("PostgreSQL Metrics Collected", extra={'metrics': db_metrics, 'source': 'postgresql_monitor'})
                        logger.info("Elasticsearch Metrics Collected", extra={'metrics': es_metrics, 'source': 'elasticsearch_monitor'})
                        
                        time.sleep(60) # Collect metrics every 60 seconds

                monitoring_thread = threading.Thread(target=start_monitoring, daemon=True)
                monitoring_thread.start()

                main()
            else:
                print(f"Unknown command: {command}", file=sys.stderr)
                print("Usage: python run.py [server|start-dev-dbs|stop-dev-dbs|restart-dev-dbs]", file=sys.stderr)
                sys.exit(1)
        else:
            print("Starting Code Index MCP server (default command)...", file=sys.stderr)
            print(f"Added path: {src_path}", file=sys.stderr)

            # Set auto-install environment variable for development
            os.environ['LEINDEX_AUTO_INSTALL_ES'] = '1'

            # Initialize ConfigManager
            config_manager = ConfigManager()
            dal_settings = config_manager.get_dal_settings()

            # Setup and start monitoring in a separate thread
            def start_monitoring():
                db_config = {
                    'host': 'localhost',
                    'database': dal_settings.get('postgresql_database', 'code_index_db'),
                    'user': dal_settings.get('postgresql_user', 'user'),
                    'password': dal_settings.get('postgresql_password', 'password'),
                    'port': int(os.getenv('POSTGRES_PORT', dal_settings.get('postgresql_port', '5432')))
                }
                es_config = {
                    'hosts': [{"host": ES_HOST, "port": ES_PORT, "scheme": "http"}]
                }

                db_monitor = DatabaseMonitor(db_config)
                es_monitor = ElasticsearchMonitor(es_config)

                while True:
                    logger.info("Collecting database and Elasticsearch metrics...")
                    db_metrics = db_monitor.collect_postgresql_metrics()
                    es_metrics = es_monitor.collect_elasticsearch_metrics()
                    
                    # Log metrics with appropriate context
                    logger.info("PostgreSQL Metrics Collected", extra={'metrics': db_metrics, 'source': 'postgresql_monitor'})
                    logger.info("Elasticsearch Metrics Collected", extra={'metrics': es_metrics, 'source': 'elasticsearch_monitor'})
                    
                    time.sleep(60) # Collect metrics every 60 seconds

            monitoring_thread = threading.Thread(target=start_monitoring, daemon=True)
            monitoring_thread.start()

            main()
except ImportError as e:
    print(f"Import Error: {e}", file=sys.stderr)
    print(f"Current sys.path: {sys.path}", file=sys.stderr)
    print("Traceback:", file=sys.stderr)
    traceback.print_exc(file=sys.stderr)
except Exception as e:
    print(f"Error starting server: {e}", file=sys.stderr)
    print("Traceback:", file=sys.stderr)
    traceback.print_exc(file=sys.stderr)
