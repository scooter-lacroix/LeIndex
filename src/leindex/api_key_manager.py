"""
API Key Management with rotation and quota tracking.

PRODUCT.MD ALIGNMENT:
---------------------
"Multi-key rotation and quota management for API keys"

This module provides comprehensive API key management:
- Multiple key rotation strategies
- Per-key quota tracking and limits
- Automatic key switching on quota exhaustion
- Persistent storage of key usage statistics
- Health monitoring and alerting
"""

import os
import json
import time
import hashlib
import threading
from dataclasses import dataclass, field, asdict
from datetime import datetime
from enum import Enum
from typing import Dict, List, Optional, Any, Set, Callable
from pathlib import Path
import logging

logger = logging.getLogger(__name__)


class KeyRotationStrategy(Enum):
    """
    Strategy for rotating between multiple API keys.
    """
    ROUND_ROBIN = "round_robin"           # Rotate keys in order
    LEAST_RECENTLY_USED = "lru"           # Use least recently used key
    LEAST_QUOTA_REMAINING = "lqr"         # Prefer keys with more quota
    RANDOM = "random"                     # Random selection


class QuotaPeriod(Enum):
    """
    Time periods for quota enforcement.
    """
    MINUTE = "minute"
    HOUR = "hour"
    DAY = "day"
    MONTH = "month"
    FOREVER = "forever"                   # No reset


@dataclass
class KeyUsageStats:
    """
    Usage statistics for a single API key.
    """
    key_id: str
    key_name: str
    request_count: int = 0
    last_used: Optional[str] = None
    last_error: Optional[str] = None
    error_count: int = 0
    quota_used: int = 0
    quota_limit: Optional[int] = None
    quota_reset_at: Optional[str] = None
    is_active: bool = True
    created_at: str = field(default_factory=lambda: datetime.now().isoformat())

    def to_dict(self) -> Dict[str, Any]:
        """Convert to dictionary for serialization."""
        return asdict(self)

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'KeyUsageStats':
        """Create from dictionary."""
        return cls(**data)

    def get_quota_remaining(self) -> Optional[int]:
        """Get remaining quota for this key."""
        if self.quota_limit is None:
            return None
        return max(0, self.quota_limit - self.quota_used)

    def is_quota_exceeded(self) -> bool:
        """Check if quota has been exceeded."""
        if self.quota_limit is None:
            return False
        return self.quota_used >= self.quota_limit

    def should_reset_quota(self) -> bool:
        """Check if quota should be reset based on time."""
        if self.quota_reset_at is None:
            return False

        reset_time = datetime.fromisoformat(self.quota_reset_at)
        return datetime.now() >= reset_time

    def reset_quota(self):
        """Reset quota counter."""
        self.quota_used = 0


@dataclass
class APIKey:
    """
    Represents a single API key configuration.
    """
    key_id: str                          # Unique identifier
    key_value: str                        # The actual API key
    key_name: str                         # Human-readable name
    quota_limit: Optional[int] = None     # Requests per period (None = unlimited)
    quota_period: QuotaPeriod = QuotaPeriod.FOREVER
    priority: int = 0                     # Higher priority keys preferred
    tags: Set[str] = field(default_factory=set)
    is_active: bool = True
    metadata: Dict[str, Any] = field(default_factory=dict)

    def __post_init__(self):
        """Convert tags to set if it's a list."""
        if isinstance(self.tags, list):
            self.tags = set(self.tags)

    def to_dict(self, redact: bool = True) -> Dict[str, Any]:
        """Convert to dictionary, optionally redacting the key value."""
        data = asdict(self)
        data['tags'] = list(self.tags)  # Convert set to list
        if redact and self.key_value:
            # Show only first 4 and last 4 characters
            if len(self.key_value) > 8:
                data['key_value'] = f"{self.key_value[:4]}...{self.key_value[-4:]}"
            else:
                data['key_value'] = "***"
        return data

    @classmethod
    def from_dict(cls, data: Dict[str, Any]) -> 'APIKey':
        """Create from dictionary."""
        if 'tags' in data and isinstance(data['tags'], list):
            data['tags'] = set(data['tags'])
        return cls(**data)

    def get_hash(self) -> str:
        """Get a hash of the key value for identification."""
        return hashlib.sha256(self.key_value.encode()).hexdigest()[:16]


class APIKeyManager:
    """
    Manages multiple API keys with rotation and quota tracking.

    PRODUCT.MD ALIGNMENT:
    ---------------------
    "Multi-key rotation and quota management"

    Features:
    - Multiple key rotation strategies
    - Per-key quota tracking
    - Automatic key switching
    - Persistent usage statistics
    - Health monitoring
    """

    def __init__(
        self,
        storage_path: Optional[str] = None,
        rotation_strategy: KeyRotationStrategy = KeyRotationStrategy.LEAST_QUOTA_REMAINING,
        auto_save: bool = True,
        save_interval: int = 60
    ):
        """
        Initialize the API key manager.

        Args:
            storage_path: Path to persist usage statistics (JSON file)
            rotation_strategy: Strategy for rotating between keys
            auto_save: Whether to automatically save statistics
            save_interval: Seconds between auto-saves
        """
        self.storage_path = storage_path
        self.rotation_strategy = rotation_strategy
        self.auto_save = auto_save
        self.save_interval = save_interval

        self._keys: Dict[str, APIKey] = {}
        self._usage_stats: Dict[str, KeyUsageStats] = {}
        self._current_key_index = 0
        self._lock = threading.RLock()
        self._last_save_time = 0
        self._callbacks: Dict[str, List[Callable]] = {
            'on_key_selected': [],
            'on_quota_exceeded': [],
            'on_key_error': [],
            'on_key_rotated': [],
        }

        # Load existing data if storage path provided
        if self.storage_path:
            self._load_from_storage()

        # Start auto-save thread if enabled
        if self.auto_save:
            self._start_auto_save()

    def add_key(
        self,
        key_id: str,
        key_value: str,
        key_name: str,
        quota_limit: Optional[int] = None,
        quota_period: QuotaPeriod = QuotaPeriod.FOREVER,
        priority: int = 0,
        tags: Optional[Set[str]] = None,
        metadata: Optional[Dict[str, Any]] = None
    ) -> bool:
        """
        Add a new API key.

        Args:
            key_id: Unique identifier for the key
            key_value: The actual API key
            key_name: Human-readable name
            quota_limit: Requests per period (None = unlimited)
            quota_period: Time period for quota
            priority: Higher priority keys preferred
            tags: Tags for grouping keys
            metadata: Additional metadata

        Returns:
            True if key was added, False if key_id already exists
        """
        with self._lock:
            if key_id in self._keys:
                logger.warning(f"Key {key_id} already exists, skipping")
                return False

            api_key = APIKey(
                key_id=key_id,
                key_value=key_value,
                key_name=key_name,
                quota_limit=quota_limit,
                quota_period=quota_period,
                priority=priority,
                tags=tags or set(),
                metadata=metadata or {}
            )

            self._keys[key_id] = api_key
            self._usage_stats[key_id] = KeyUsageStats(
                key_id=key_id,
                key_name=key_name,
                quota_limit=quota_limit
            )

            logger.info(f"Added API key: {key_name} ({key_id})")
            self._save_if_needed()

            return True

    def remove_key(self, key_id: str) -> bool:
        """
        Remove an API key.

        Args:
            key_id: Key identifier to remove

        Returns:
            True if key was removed, False if not found
        """
        with self._lock:
            if key_id not in self._keys:
                return False

            key_name = self._keys[key_id].key_name
            del self._keys[key_id]
            del self._usage_stats[key_id]

            logger.info(f"Removed API key: {key_name} ({key_id})")
            self._save_if_needed()

            return True

    def get_key(self, key_id: str) -> Optional[APIKey]:
        """Get an API key by ID (without the value)."""
        with self._lock:
            return self._keys.get(key_id)

    def list_keys(self, include_inactive: bool = False) -> List[Dict[str, Any]]:
        """
        List all API keys.

        Args:
            include_inactive: Whether to include inactive keys

        Returns:
            List of key dictionaries (values redacted)
        """
        with self._lock:
            result = []
            for key in self._keys.values():
                if not include_inactive and not key.is_active:
                    continue
                result.append(key.to_dict(redact=True))
            return result

    def select_key(self, tags: Optional[Set[str]] = None) -> Optional[str]:
        """
        Select the best available API key based on rotation strategy.

        Args:
            tags: Optional tags to filter keys

        Returns:
            Selected key ID or None if no keys available
        """
        with self._lock:
            # Filter available keys
            available_keys = []
            for key_id, key in self._keys.items():
                if not key.is_active:
                    continue

                # Check tags if specified
                if tags and not tags.intersection(key.tags):
                    continue

                # Check quota
                stats = self._usage_stats.get(key_id)
                if stats and stats.is_quota_exceeded():
                    # Check if quota should be reset
                    if stats.should_reset_quota():
                        stats.reset_quota()
                    else:
                        continue

                available_keys.append((key_id, key, stats))

            if not available_keys:
                logger.warning("No available API keys")
                return None

            # Select based on strategy
            selected_key_id = None

            if self.rotation_strategy == KeyRotationStrategy.ROUND_ROBIN:
                selected_key_id = self._round_robin_select(available_keys)

            elif self.rotation_strategy == KeyRotationStrategy.LEAST_RECENTLY_USED:
                selected_key_id = self._lru_select(available_keys)

            elif self.rotation_strategy == KeyRotationStrategy.LEAST_QUOTA_REMAINING:
                selected_key_id = self._lqr_select(available_keys)

            elif self.rotation_strategy == KeyRotationStrategy.RANDOM:
                import random
                selected_key_id, _, _ = random.choice(available_keys)

            # Fire callback
            if selected_key_id:
                self._fire_callback('on_key_selected', selected_key_id)

            return selected_key_id

    def get_key_value(self, key_id: str) -> Optional[str]:
        """
        Get the actual value of an API key.

        Args:
            key_id: Key identifier

        Returns:
            Key value or None if not found
        """
        with self._lock:
            key = self._keys.get(key_id)
            if key and key.is_active:
                return key.key_value
            return None

    def record_usage(
        self,
        key_id: str,
        success: bool = True,
        error_message: Optional[str] = None
    ):
        """
        Record API usage for a key.

        Args:
            key_id: Key that was used
            success: Whether the request was successful
            error_message: Error message if unsuccessful
        """
        with self._lock:
            stats = self._usage_stats.get(key_id)
            if not stats:
                logger.warning(f"No stats found for key {key_id}")
                return

            now = datetime.now().isoformat()
            stats.request_count += 1
            stats.last_used = now

            if success:
                stats.quota_used += 1
                if stats.is_quota_exceeded():
                    logger.warning(f"Quota exceeded for key {key_id}")
                    self._fire_callback('on_quota_exceeded', key_id)
            else:
                stats.error_count += 1
                stats.last_error = error_message
                self._fire_callback('on_key_error', key_id, error_message)

            self._save_if_needed()

    def get_usage_stats(self, key_id: Optional[str] = None) -> Dict[str, Any]:
        """
        Get usage statistics.

        Args:
            key_id: Specific key to get stats for, or None for all keys

        Returns:
            Dictionary of usage statistics
        """
        with self._lock:
            if key_id:
                stats = self._usage_stats.get(key_id)
                return stats.to_dict() if stats else {}

            return {
                kid: stats.to_dict()
                for kid, stats in self._usage_stats.items()
            }

    def reset_quota(self, key_id: str):
        """Reset quota for a specific key."""
        with self._lock:
            stats = self._usage_stats.get(key_id)
            if stats:
                stats.reset_quota()
                logger.info(f"Reset quota for key {key_id}")
                self._save_if_needed()

    def set_rotation_strategy(self, strategy: KeyRotationStrategy):
        """Change the rotation strategy."""
        with self._lock:
            self.rotation_strategy = strategy
            logger.info(f"Changed rotation strategy to {strategy.value}")

    def register_callback(self, event: str, callback: Callable):
        """
        Register a callback for an event.

        Events:
        - on_key_selected: Called when a key is selected (args: key_id)
        - on_quota_exceeded: Called when quota is exceeded (args: key_id)
        - on_key_error: Called on error (args: key_id, error_message)
        - on_key_rotated: Called when key is rotated (args: old_key_id, new_key_id)
        """
        if event in self._callbacks:
            self._callbacks[event].append(callback)

    def _round_robin_select(self, available: List[tuple]) -> str:
        """Select key using round-robin strategy."""
        # Sort by priority first
        available.sort(key=lambda x: x[1].priority, reverse=True)

        # Get highest priority group
        if available:
            max_priority = available[0][1].priority
            high_priority = [k for k in available if k[1].priority == max_priority]

            # Select from high priority group using index
            key_id = high_priority[self._current_key_index % len(high_priority)][0]
            self._current_key_index += 1
            return key_id

        return available[0][0]

    def _lru_select(self, available: List[tuple]) -> str:
        """Select key using least recently used strategy."""
        # Sort by last_used (oldest first)
        available.sort(key=lambda x: x[2].last_used or '0')
        return available[0][0]

    def _lqr_select(self, available: List[tuple]) -> str:
        """Select key with most quota remaining."""
        def quota_remaining(item):
            stats = item[2]
            if stats.quota_limit is None:
                return float('inf')
            return stats.quota_limit - stats.quota_used

        available.sort(key=quota_remaining, reverse=True)
        return available[0][0]

    def _fire_callback(self, event: str, *args):
        """Fire all callbacks for an event."""
        for callback in self._callbacks.get(event, []):
            try:
                callback(*args)
            except Exception as e:
                logger.error(f"Error in callback for {event}: {e}")

    def _save_if_needed(self):
        """Save statistics if auto-save is enabled."""
        if not self.auto_save or not self.storage_path:
            return

        now = time.time()
        if now - self._last_save_time >= self.save_interval:
            self._save_to_storage()
            self._last_save_time = now

    def _save_to_storage(self):
        """Save statistics to storage file."""
        if not self.storage_path:
            return

        try:
            data = {
                'usage_stats': {
                    kid: stats.to_dict()
                    for kid, stats in self._usage_stats.items()
                },
                'rotation_strategy': self.rotation_strategy.value,
                'saved_at': datetime.now().isoformat()
            }

            path = Path(self.storage_path)
            path.parent.mkdir(parents=True, exist_ok=True)

            with open(path, 'w') as f:
                json.dump(data, f, indent=2)

            logger.debug(f"Saved usage statistics to {self.storage_path}")

        except Exception as e:
            logger.error(f"Error saving to storage: {e}")

    def _load_from_storage(self):
        """Load statistics from storage file."""
        if not self.storage_path:
            return

        path = Path(self.storage_path)
        if not path.exists():
            return

        try:
            with open(path, 'r') as f:
                data = json.load(f)

            # Load usage stats
            for kid, stats_data in data.get('usage_stats', {}).items():
                self._usage_stats[kid] = KeyUsageStats.from_dict(stats_data)

            # Load rotation strategy
            strategy_str = data.get('rotation_strategy')
            if strategy_str:
                try:
                    self.rotation_strategy = KeyRotationStrategy(strategy_str)
                except ValueError:
                    pass

            logger.debug(f"Loaded usage statistics from {self.storage_path}")

        except Exception as e:
            logger.error(f"Error loading from storage: {e}")

    def _start_auto_save(self):
        """Start background thread for auto-saving."""
        def auto_save_worker():
            while True:
                time.sleep(self.save_interval)
                with self._lock:
                    self._save_to_storage()

        thread = threading.Thread(target=auto_save_worker, daemon=True)
        thread.start()
        logger.debug("Started auto-save thread")

    def shutdown(self):
        """Shutdown and save final state."""
        with self._lock:
            self._save_to_storage()
            logger.info("API key manager shutdown complete")


def create_manager_from_env(
    prefix: str = "API_KEY_",
    default_quota: Optional[int] = None,
    storage_path: Optional[str] = None
) -> APIKeyManager:
    """
    Create an API key manager from environment variables.

    Environment variables should be named like:
    - API_KEY_1=value
    - API_KEY_1_NAME=My Key
    - API_KEY_1_QUOTA=1000
    - API_KEY_1_PRIORITY=10

    Args:
        prefix: Environment variable prefix
        default_quota: Default quota limit if not specified
        storage_path: Path for persistent storage

    Returns:
        Configured APIKeyManager
    """
    manager = APIKeyManager(storage_path=storage_path)

    # Find all API_KEY_* variables
    api_keys = set()
    for key in os.environ:
        if key.startswith(prefix) and key.endswith('_1') or \
           any(key.startswith(f"{prefix}{i}_") for i in range(2, 100)):
            # Extract the index
            parts = key[len(prefix):].split('_')
            if parts and parts[0].isdigit():
                api_keys.add(parts[0])

    # Load each key
    for idx in sorted(api_keys):
        base = f"{prefix}{idx}"

        value = os.environ.get(base)
        if not value:
            continue

        key_id = os.environ.get(f"{base}_ID", f"key_{idx}")
        name = os.environ.get(f"{base}_NAME", f"API Key {idx}")
        quota = int(os.environ.get(f"{base}_QUOTA", default_quota or 0)) or None
        priority = int(os.environ.get(f"{base}_PRIORITY", 0))

        manager.add_key(
            key_id=key_id,
            key_value=value,
            key_name=name,
            quota_limit=quota,
            priority=priority
        )

    logger.info(f"Loaded {len(api_keys)} API keys from environment")
    return manager
