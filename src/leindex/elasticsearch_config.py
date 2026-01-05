"""
Elasticsearch configuration and connection management.
"""
import os
from typing import List, Dict, Any, Optional
from elasticsearch import Elasticsearch

from .config_manager import ConfigManager
from .constants import ES_DEFAULT_URL, ES_INDEX_NAME
from .logger_config import logger


class ElasticsearchConfig:
    """Manages Elasticsearch configuration and connection."""

    def __init__(self):
        self.config_manager = ConfigManager()
        self._hosts: Optional[List[str]] = None
        self._index_name: Optional[str] = None
        self._auth: Optional[Dict[str, Any]] = None
        self._ssl_config: Optional[Dict[str, Any]] = None

    def get_elasticsearch_hosts(self) -> List[str]:
        """Get Elasticsearch hosts from configuration."""
        if self._hosts is None:
            # Priority: Environment variables > Config file > Default
            env_hosts = os.getenv("ELASTICSEARCH_HOSTS")
            if env_hosts:
                self._hosts = [host.strip() for host in env_hosts.split(",")]
            else:
                # Get from config manager
                dal_settings = self.config_manager.get_dal_settings()
                config_hosts = dal_settings.get("elasticsearch_hosts", [ES_DEFAULT_URL])
                if isinstance(config_hosts, str):
                    self._hosts = [config_hosts]
                elif isinstance(config_hosts, list):
                    self._hosts = config_hosts
                else:
                    self._hosts = [ES_DEFAULT_URL]

        return self._hosts

    def get_elasticsearch_index_name(self) -> str:
        """Get Elasticsearch index name from configuration."""
        if self._index_name is None:
            # Priority: Environment variables > Config file > Default
            env_index = os.getenv("ELASTICSEARCH_INDEX_NAME")
            if env_index:
                self._index_name = env_index
            else:
                dal_settings = self.config_manager.get_dal_settings()
                self._index_name = dal_settings.get("elasticsearch_index_name", ES_INDEX_NAME)

        return self._index_name

    def get_elasticsearch_auth(self) -> Optional[Dict[str, Any]]:
        """Get Elasticsearch authentication configuration."""
        if self._auth is None:
            # Check for API key authentication
            api_key_id = os.getenv("ELASTICSEARCH_API_KEY_ID") or self.config_manager.get_dal_settings().get("elasticsearch_api_key_id")
            api_key = os.getenv("ELASTICSEARCH_API_KEY") or self.config_manager.get_dal_settings().get("elasticsearch_api_key")

            if api_key_id and api_key:
                self._auth = {"api_key": (api_key_id, api_key)}
                logger.debug("Using Elasticsearch API key authentication")
                return self._auth

            # Check for HTTP authentication
            username = os.getenv("ELASTICSEARCH_USERNAME") or self.config_manager.get_dal_settings().get("elasticsearch_username")
            password = os.getenv("ELASTICSEARCH_PASSWORD") or self.config_manager.get_dal_settings().get("elasticsearch_password")

            if username and password:
                self._auth = {"http_auth": (username, password)}
                logger.debug("Using Elasticsearch HTTP authentication")
                return self._auth

            self._auth = {}

        return self._auth if self._auth else None

    def get_elasticsearch_ssl_config(self) -> Dict[str, Any]:
        """Get Elasticsearch SSL/TLS configuration."""
        if self._ssl_config is None:
            dal_settings = self.config_manager.get_dal_settings()

            # SSL configuration
            use_ssl = self._parse_bool(os.getenv("ELASTICSEARCH_USE_SSL") or dal_settings.get("elasticsearch_use_ssl", "false"))
            verify_certs = self._parse_bool(os.getenv("ELASTICSEARCH_VERIFY_CERTS") or dal_settings.get("elasticsearch_verify_certs", "true"))
            ca_certs = os.getenv("ELASTICSEARCH_CA_CERTS") or dal_settings.get("elasticsearch_ca_certs")
            client_cert = os.getenv("ELASTICSEARCH_CLIENT_CERT") or dal_settings.get("elasticsearch_client_cert")
            client_key = os.getenv("ELASTICSEARCH_CLIENT_KEY") or dal_settings.get("elasticsearch_client_key")

            self._ssl_config = {
                "use_ssl": use_ssl,
                "verify_certs": verify_certs,
            }

            if ca_certs:
                self._ssl_config["ca_certs"] = ca_certs
            if client_cert:
                self._ssl_config["client_cert"] = client_cert
            if client_key:
                self._ssl_config["client_key"] = client_key

        return self._ssl_config

    def _parse_bool(self, value) -> bool:
        """Parse boolean value from string or boolean."""
        if isinstance(value, bool):
            return value
        if isinstance(value, str):
            return value.lower() in ('true', '1', 'yes', 'on')
        return bool(value)

    def create_elasticsearch_client(self, timeout: int = 30, max_retries: int = 3) -> Elasticsearch:
        """Create and configure Elasticsearch client."""
        hosts = self.get_elasticsearch_hosts()
        auth = self.get_elasticsearch_auth()
        ssl_config = self.get_elasticsearch_ssl_config()

        # Prepare client configuration
        client_config = {
            "hosts": hosts,
            "timeout": timeout,
            "max_retries": max_retries,
            "retry_on_timeout": True,
        }

        # Add authentication
        if auth:
            client_config.update(auth)

        # Add SSL configuration
        if ssl_config["use_ssl"]:
            # For Elasticsearch 8.x, SSL is handled through the host URL scheme
            # If hosts don't already have https scheme, update them
            https_hosts = []
            for host in hosts:
                if host.startswith("http://"):
                    https_hosts.append(host.replace("http://", "https://"))
                elif not host.startswith("https://"):
                    https_hosts.append(f"https://{host}")
                else:
                    https_hosts.append(host)
            client_config["hosts"] = https_hosts

            # SSL verification settings
            if not ssl_config["verify_certs"]:
                client_config["verify_certs"] = False
                client_config["ssl_show_warn"] = False

            # Additional SSL certificates
            if ssl_config.get("ca_certs"):
                client_config["ca_certs"] = ssl_config["ca_certs"]
            if ssl_config.get("client_cert"):
                client_config["client_cert"] = ssl_config["client_cert"]
            if ssl_config.get("client_key"):
                client_config["client_key"] = ssl_config["client_key"]

        logger.info(f"Creating Elasticsearch client with hosts: {hosts}")
        logger.debug(f"Elasticsearch client config: {client_config}")

        return Elasticsearch(**client_config)

    def test_connection(self, client: Elasticsearch) -> bool:
        """Test Elasticsearch connection."""
        try:
            # Ping the cluster
            if client.ping():
                logger.info("Successfully connected to Elasticsearch cluster")

                # Get cluster info for additional validation
                info = client.info()
                cluster_name = info.get("cluster_name", "unknown")
                version = info.get("version", {}).get("number", "unknown")
                logger.info(f"Elasticsearch cluster: {cluster_name}, version: {version}")

                return True
            else:
                logger.error("Elasticsearch ping failed")
                return False
        except Exception as e:
            logger.error(f"Failed to connect to Elasticsearch: {e}")
            return False

    def ensure_index_exists(self, client: Elasticsearch) -> bool:
        """Ensure the Elasticsearch index exists with proper mapping."""
        try:
            index_name = self.get_elasticsearch_index_name()

            if client.indices.exists(index=index_name):
                logger.debug(f"Elasticsearch index '{index_name}' already exists")
                return True

            # Create index with mapping
            mapping = {
                "mappings": {
                    "properties": {
                        "file_path": {
                            "type": "text",
                            "analyzer": "standard",
                            "fields": {
                                "keyword": {
                                    "type": "keyword",
                                    "ignore_above": 256
                                }
                            }
                        },
                        "content": {
                            "type": "text",
                            "analyzer": "standard"
                        },
                        "file_type": {
                            "type": "keyword"
                        },
                        "extension": {
                            "type": "keyword"
                        },
                        "size": {
                            "type": "long"
                        },
                        "last_modified": {
                            "type": "date"
                        },
                        "language": {
                            "type": "keyword"
                        },
                        "project_path": {
                            "type": "keyword"
                        },
                        "checksum": {
                            "type": "keyword"
                        }
                    }
                },
                "settings": {
                    "number_of_shards": 1,
                    "number_of_replicas": 0,
                    "analysis": {
                        "analyzer": {
                            "default": {
                                "type": "standard"
                            }
                        }
                    }
                }
            }

            response = client.indices.create(index=index_name, body=mapping)
            if response.get("acknowledged", False):
                logger.info(f"Created Elasticsearch index '{index_name}'")
                return True
            else:
                logger.error(f"Failed to create Elasticsearch index '{index_name}': {response}")
                return False

        except Exception as e:
            logger.error(f"Error ensuring Elasticsearch index exists: {e}")
            return False

    def get_connection_info(self) -> Dict[str, Any]:
        """Get current Elasticsearch connection information."""
        return {
            "hosts": self.get_elasticsearch_hosts(),
            "index_name": self.get_elasticsearch_index_name(),
            "auth_type": "api_key" if self.get_elasticsearch_auth() and "api_key" in self.get_elasticsearch_auth() else
                         "http_auth" if self.get_elasticsearch_auth() and "http_auth" in self.get_elasticsearch_auth() else
                         "none",
            "ssl_enabled": self.get_elasticsearch_ssl_config()["use_ssl"],
            "ssl_verify_certs": self.get_elasticsearch_ssl_config()["verify_certs"],
        }


# Global instance
elasticsearch_config = ElasticsearchConfig()