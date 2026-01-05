import json
import os
# from pathlib import Path # Removed pathlib
from .constants import GLOBAL_CONFIG_FILE
from .logger_config import logger

class GlobalConfigManager:
    """Manages global configuration settings for the LeIndex MCP server."""

    def __init__(self):
        self.config_path = os.path.expanduser(GLOBAL_CONFIG_FILE) # Use os.path
        self._ensure_config_directory()

    def _ensure_config_directory(self):
        """Ensures the directory for the global config file exists."""
        config_dir = os.path.dirname(self.config_path)
        if not os.path.exists(config_dir):
            logger.debug(f"Creating global config directory: {config_dir}")
            os.makedirs(config_dir, exist_ok=True)
        else:
            logger.debug(f"Global config directory already exists: {config_dir}")

    def load_config(self) -> dict:
        """Loads the global configuration from the file."""
        if not os.path.exists(self.config_path):
            return {}
        try:
            with open(self.config_path, 'r', encoding='utf-8') as f:
                config = json.load(f)
            logger.debug(f"Loaded global config from {self.config_path}: {config}")
            return config
        except json.JSONDecodeError as e:
            logger.error(f"Error decoding global config file {self.config_path}: {e}")
            return {}
        except Exception as e:
            logger.error(f"Error loading global config from {self.config_path}: {e}")
            return {}

    def save_config(self, config: dict):
        """Saves the global configuration to the file."""
        self._ensure_config_directory()
        try:
            logger.debug(f"Attempting to save global config to {self.config_path} with content: {config}")
            f = open(self.config_path, 'w', encoding='utf-8')
            json.dump(config, f, indent=2)
            f.flush() # Explicitly flush the buffer
            f.close() # Explicitly close the file
            logger.debug(f"Saved global config to {self.config_path}")
        except Exception as e:
            logger.error(f"Error saving global config to {self.config_path}: {e}")
        finally:
            if 'f' in locals() and not f.closed:
                f.close() # Ensure file is closed even if an error occurs

    def get_base_path(self) -> str:
        """Retrieves the stored base path from the global config."""
        return self.load_config().get('base_path', "")

    def set_base_path(self, path: str):
        """Sets and saves the base path in the global config."""
        config = self.load_config()
        config['base_path'] = path
        self.save_config(config)
        logger.info(f"Global base path set to: {path}")
