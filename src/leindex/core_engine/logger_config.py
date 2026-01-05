"""
Logger configuration for core_engine module.

This module provides a centralized logger instance for the core_engine
submodule, used by tool_routers and other core engine components.
"""

import logging

# Create logger for the core_engine module
logger = logging.getLogger("leindex.core_engine")

# Set default level - will be configured by the main server
logger.setLevel(logging.DEBUG)

# Avoid adding handlers if root already has them configured
if not logger.handlers:
    # Null handler that prevents warnings but lets propagation to root logger
    logger.addHandler(logging.NullHandler())
