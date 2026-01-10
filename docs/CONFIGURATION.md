# Configuration Reference

## Overview

LeIndex v2.0 uses a hierarchical YAML configuration system with validation, migration support, and zero-downtime reload. Configuration is organized into global defaults and per-project overrides.

### Configuration Hierarchy

```
Global Config (~/.leindex/config.yaml)
│
├─> Project Configs (~/.leindex/projects/*.yaml)
│   │
│   └─> Environment Variables
│       │
│       └─> Command-Line Arguments
```

**Priority** (highest to lowest):
1. Command-line arguments
2. Environment variables
3. Project-specific configuration
4. Global configuration
5. Default values

## Configuration Structure

### Global Configuration

Location: `~/.leindex/config.yaml`

```yaml
# LeIndex v2.0 Configuration
version: "2.0"

# ==============================================================================
# MEMORY MANAGEMENT
# ==============================================================================
memory:
  # Total memory budget (in MB)
  total_budget_mb: 3072  # 3 GB

  # Threshold percentages (of total budget)
  soft_limit_percent: 0.80    # 80% = cleanup triggered
  hard_limit_percent: 0.93    # 93% = spill to disk
  emergency_percent: 0.98     # 98% = emergency eviction

  # Maximum resources (across all projects)
  max_loaded_files: 1000
  max_cached_queries: 500

  # Spill-to-disk configuration
  spill:
    enabled: true
    directory: "~/.leindex/spill"
    max_spill_size_mb: 1000

  # Monitoring configuration
  monitoring:
    enabled: true
    interval_seconds: 30
    alert_on_soft_limit: true
    alert_on_hard_limit: true
    alert_on_emergency: true

  # Project defaults
  project_defaults:
    max_loaded_files: 100
    max_cached_queries: 50
    priority: "MEDIUM"  # LOW, MEDIUM, HIGH, CRITICAL

# ==============================================================================
# PERFORMANCE SETTINGS
# ==============================================================================
performance:
  # Parallel scanner (directory traversal)
  parallel_scanner:
    enabled: true
    max_workers: 4  # Number of concurrent directory scans
    timeout_seconds: 300  # Scan timeout

  # Parallel processor (content extraction)
  parallel_processor:
    enabled: true
    max_workers: 4  # Number of content extraction workers
    batch_size: 100  # Files per batch

  # Embedding optimization
  embeddings:
    batch_size: 32  # Files per embedding batch
    enable_gpu: true  # Use GPU if available
    device: "auto"  # auto, cuda, mps, rocm, cpu
    fp16: true  # Use half-precision on GPU

  # Pattern matching optimization
  pattern_trie:
    enabled: true
    cache_size: 1000  # Pattern cache size

  # File stat caching
  file_stat_cache:
    enabled: true
    max_size: 10000  # Maximum cache entries
    ttl_seconds: 300  # Cache TTL (5 minutes)

# ==============================================================================
# GLOBAL INDEX
# ==============================================================================
global_index:
  # Tier 1: Metadata settings
  tier1:
    enabled: true
    auto_refresh: true
    refresh_interval_seconds: 60

  # Tier 2: Query cache settings
  tier2:
    enabled: true
    max_size: 1000  # Maximum cached queries
    ttl_seconds: 300  # Cache TTL (5 minutes)
    stale_allowed: true  # Serve stale results

  # Query routing
  query_router:
    max_concurrent_queries: 10
    query_timeout_seconds: 30
    merge_strategy: "weighted"  # weighted, ranked, simple

  # Graceful degradation
  graceful_degradation:
    enabled: true
    fallback_chain:
      - "leann"
      - "tantivy"
      - "ripgrep"
      - "grep"
    max_fallback_depth: 4

# ==============================================================================
# DATA ACCESS LAYER
# ==============================================================================
dal_settings:
  backend_type: "sqlite_duckdb"
  db_path: "~/.leindex/data/leindex.db"
  duckdb_db_path: "~/.leindex/data/leindex.db.duckdb"

# ==============================================================================
# VECTOR STORE
# ==============================================================================
vector_store:
  backend_type: "leann"
  index_path: "~/.leindex/leann_index"
  embedding_model: "nomic-ai/CodeRankEmbed"
  embedding_dim: 768

# ==============================================================================
# ASYNC PROCESSING
# ==============================================================================
async_processing:
  enabled: true
  worker_count: 4
  max_queue_size: 10000

# ==============================================================================
# FILE FILTERING
# ==============================================================================
file_filtering:
  max_file_size: 1073741824  # 1GB per file
  type_specific_limits:
    ".py": 1073741824
    ".json": 104857600
    ".md": 10485760

# ==============================================================================
# DIRECTORY FILTERING
# ==============================================================================
directory_filtering:
  skip_large_directories:
    - "**/node_modules/**"
    - "**/.git/**"
    - "**/venv/**"
    - "**/__pycache__/**"
    - "**/dist/**"
    - "**/build/**"
    - "**/.venv/**"

# ==============================================================================
# LOGGING
# ==============================================================================
logging:
  level: "INFO"  # DEBUG, INFO, WARNING, ERROR, CRITICAL
  format: "json"  # json, text
  file: "~/.leindex/logs/leindex.log"
  max_size_mb: 100
  backup_count: 5

# ==============================================================================
# SECURITY
# ==============================================================================
security:
  # Path validation
  validate_paths: true
  allowed_path_prefixes:
    - "/home"
    - "/Users"
    - "C:\\Users"

  # Access control
  enable_access_control: false
  allowed_users: []

  # Encryption
  encrypt_spill_data: false
  encryption_key: null
```

### Project-Specific Configuration

Location: `~/.leindex/projects/{project-name}.yaml`

```yaml
# Project-specific configuration overrides
project_id: "/path/to/project"

# Memory overrides
memory:
  max_loaded_files: 500  # Override global default (100)
  max_cached_queries: 200  # Override global default (50)
  priority: "HIGH"  # Override global default (MEDIUM)

# Performance overrides
performance:
  parallel_scanner:
    max_workers: 8  # Override global default (4)

  embeddings:
    batch_size: 64  # Override global default (32)

# File filtering overrides
file_filtering:
  max_file_size: 536870912  # 512MB (override global 1GB)
  type_specific_limits:
    ".py": 536870912

# Directory filtering overrides
directory_filtering:
  skip_large_directories:
    - "**/node_modules/**"
    - "**/.git/**"
    - "**/generated/**"  # Project-specific addition
```

## Environment Variables

Override configuration with environment variables:

### Memory Settings

```bash
# Total memory budget
export LEINDEX_MEMORY_TOTAL_BUDGET_MB=4096

# Threshold percentages
export LEINDEX_MEMORY_SOFT_LIMIT_PERCENT=0.75
export LEINDEX_MEMORY_HARD_LIMIT_PERCENT=0.90
export LEINDEX_MEMORY_EMERGENCY_PERCENT=0.95

# Maximum resources
export LEINDEX_MEMORY_MAX_LOADED_FILES=2000
export LEINDEX_MEMORY_MAX_CACHED_QUERIES=1000

# Spill configuration
export LEINDEX_MEMORY_SPILL_ENABLED=true
export LEINDEX_MEMORY_SPILL_DIRECTORY="/tmp/leindex/spill"
export LEINDEX_MEMORY_SPILL_MAX_SIZE_MB=2000

# Monitoring
export LEINDEX_MEMORY_MONITORING_ENABLED=true
export LEINDEX_MEMORY_MONITORING_INTERVAL_SECONDS=60
```

### Performance Settings

```bash
# Parallel scanner
export LEINDEX_PERFORMANCE_PARALLEL_SCANNER_ENABLED=true
export LEINDEX_PERFORMANCE_PARALLEL_SCANNER_MAX_WORKERS=8
export LEINDEX_PERFORMANCE_PARALLEL_SCANNER_TIMEOUT_SECONDS=600

# Parallel processor
export LEINDEX_PERFORMANCE_PARALLEL_PROCESSOR_ENABLED=true
export LEINDEX_PERFORMANCE_PARALLEL_PROCESSOR_MAX_WORKERS=8
export LEINDEX_PERFORMANCE_PARALLEL_PROCESSOR_BATCH_SIZE=200

# Embeddings
export LEINDEX_PERFORMANCE_EMBEDDINGS_BATCH_SIZE=64
export LEINDEX_PERFORMANCE_EMBEDDINGS_ENABLE_GPU=true
export LEINDEX_PERFORMANCE_EMBEDDINGS_DEVICE="cuda"
export LEINDEX_PERFORMANCE_EMBEDDINGS_FP16=true

# Pattern matching
export LEINDEX_PERFORMANCE_PATTERN_TRIE_ENABLED=true
export LEINDEX_PERFORMANCE_PATTERN_TRIE_CACHE_SIZE=2000

# File stat cache
export LEINDEX_PERFORMANCE_FILE_STAT_CACHE_ENABLED=true
export LEINDEX_PERFORMANCE_FILE_STAT_CACHE_MAX_SIZE=20000
export LEINDEX_PERFORMANCE_FILE_STAT_CACHE_TTL_SECONDS=600
```

### Global Index Settings

```bash
# Tier 1
export LEINDEX_GLOBAL_INDEX_TIER1_ENABLED=true
export LEINDEX_GLOBAL_INDEX_TIER1_AUTO_REFRESH=true
export LEINDEX_GLOBAL_INDEX_TIER1_REFRESH_INTERVAL_SECONDS=120

# Tier 2
export LEINDEX_GLOBAL_INDEX_TIER2_ENABLED=true
export LEINDEX_GLOBAL_INDEX_TIER2_MAX_SIZE=2000
export LEINDEX_GLOBAL_INDEX_TIER2_TTL_SECONDS=600
export LEINDEX_GLOBAL_INDEX_TIER2_STALE_ALLOWED=true

# Query router
export LEINDEX_GLOBAL_INDEX_QUERY_ROUTER_MAX_CONCURRENT_QUERIES=20
export LEINDEX_GLOBAL_INDEX_QUERY_ROUTER_QUERY_TIMEOUT_SECONDS=60
export LEINDEX_GLOBAL_INDEX_QUERY_ROUTER_MERGE_STRATEGY="ranked"

# Graceful degradation
export LEINDEX_GLOBAL_INDEX_GRACEFUL_DEGRADATION_ENABLED=true
export LEINDEX_GLOBAL_INDEX_GRACEFUL_DEGRADATION_MAX_FALLBACK_DEPTH=4
```

### Data Access Settings

```bash
export LEINDEX_DAL_BACKEND_TYPE="sqlite_duckdb"
export LEINDEX_DAL_DB_PATH="/data/leindex.db"
export LEINDEX_DAL_DUCKDB_DB_PATH="/data/leindex.db.duckdb"
```

### Vector Store Settings

```bash
export LEINDEX_VECTOR_STORE_BACKEND_TYPE="leann"
export LEINDEX_VECTOR_STORE_INDEX_PATH="/data/leann_index"
export LEINDEX_VECTOR_STORE_EMBEDDING_MODEL="nomic-ai/CodeRankEmbed"
export LEINDEX_VECTOR_STORE_EMBEDDING_DIM=768
```

### Logging Settings

```bash
export LEINDEX_LOGGING_LEVEL="DEBUG"
export LEINDEX_LOGGING_FORMAT="json"
export LEINDEX_LOGGING_FILE="/var/log/leindex/leindex.log"
export LEINDEX_LOGGING_MAX_SIZE_MB=200
export LEINDEX_LOGGING_BACKUP_COUNT=10
```

## Configuration Validation

### Validation Rules

The configuration system validates all settings:

```python
from leindex.config import ConfigValidator, ValidationError

validator = ConfigValidator()

# Validate configuration
try:
    validator.validate(config_dict)
    print("Configuration is valid")
except ValidationError as e:
    print(f"Validation error: {e.message}")
    print(f"Field: {e.field}")
    print(f"Value: {e.value}")
```

### Common Validation Errors

#### Invalid Memory Budget

```yaml
# INVALID: Negative memory budget
memory:
  total_budget_mb: -1000
```

**Error**: `memory.total_budget_mb must be positive (got: -1000)`

#### Invalid Threshold Percentages

```yaml
# INVALID: Threshold > 100%
memory:
  soft_limit_percent: 1.50  # 150%
```

**Error**: `memory.soft_limit_percent must be <= 1.0 (got: 1.50)`

#### Invalid Threshold Ordering

```yaml
# INVALID: Hard limit < Soft limit
memory:
  soft_limit_percent: 0.90
  hard_limit_percent: 0.80
```

**Error**: `memory.hard_limit_percent must be > memory.soft_limit_percent`

#### Invalid Worker Count

```yaml
# INVALID: Negative worker count
performance:
  parallel_scanner:
    max_workers: -4
```

**Error**: `performance.parallel_scanner.max_workers must be >= 0 (got: -4)`

#### Invalid File Size

```yaml
# INVALID: Negative file size
file_filtering:
  max_file_size: -1000000
```

**Error**: `file_filtering.max_file_size must be positive (got: -1000000)`

## Configuration Migration

### Automatic Migration

Configuration is automatically migrated from v1 to v2:

```python
from leindex.config import ConfigMigration

migration = ConfigMigration()

# Migrate v1 config to v2
result = migration.migrate_v1_to_v2(
    v1_config_path="~/.leindex/config.yaml",
    v2_config_path="~/.leindex/config.v2.yaml"
)

if result.success:
    print(f"Migration successful: {result.migrated_fields}")
else:
    print(f"Migration failed: {result.errors}")
```

### Migration Rules

| v1 Field | v2 Field | Notes |
|----------|----------|-------|
| `memory.soft_limit_mb` | `memory.soft_limit_percent` | Converted to percentage |
| `memory.hard_limit_mb` | `memory.hard_limit_percent` | Converted to percentage |
| `performance.parallel_workers` | `performance.parallel_scanner.max_workers` | Renamed |
| `performance.batch_size` | `performance.embeddings.batch_size` | Moved to embeddings |
| `cache.enabled` | `performance.file_stat_cache.enabled` | Renamed |

### Manual Migration

For complex migrations, use manual configuration:

```python
from leindex.config import GlobalConfigManager

manager = GlobalConfigManager()

# Load v1 config
v1_config = manager.load_config("~/.leindex/config.v1.yaml")

# Create v2 config
v2_config = {
    "version": "2.0",
    "memory": {
        "total_budget_mb": v1_config["memory"]["budget_mb"],
        "soft_limit_percent": v1_config["memory"]["soft_limit_mb"] / v1_config["memory"]["budget_mb"],
        "hard_limit_percent": v1_config["memory"]["hard_limit_mb"] / v1_config["memory"]["budget_mb"],
    },
    # ... other fields
}

# Save v2 config
manager.save_config(v2_config, "~/.leindex/config.yaml")
```

## Zero-Downtime Reload

### Signal-Based Reload

Reload configuration without restarting:

```bash
# Send SIGHUP to reload configuration
kill -HUP $(cat ~/.leindex/leindex.pid)
```

### Programmatic Reload

```python
from leindex.config import reload_config, ReloadResult

# Reload configuration
result = reload_config()

if result.success:
    print(f"Configuration reloaded at {result.reloaded_at}")
    print(f"Changed fields: {result.changed_fields}")
else:
    print(f"Reload failed: {result.error}")
```

### Configuration Observers

Register observers to be notified of changes:

```python
from leindex.config import ConfigObserver, get_reload_manager

class MyConfigObserver(ConfigObserver):
    def on_config_reloaded(self, event: ReloadEvent):
        print(f"Config reloaded at {event.timestamp}")
        print(f"Previous config: {event.old_config}")
        print(f"New config: {event.new_config}")

        # Handle specific changes
        if event.old_config.memory.total_budget_mb != event.new_config.memory.total_budget_mb:
            print("Memory budget changed!")
            update_memory_budget(event.new_config.memory.total_budget_mb)

# Register observer
manager = get_reload_manager()
manager.register_observer(MyConfigObserver())
```

## First-Time Setup

### Automatic Setup

Run first-time setup with hardware detection:

```python
from leindex.config import first_time_setup, SetupResult

result: SetupResult = first_time_setup()

if result.success:
    print("Setup complete!")
    print(f"Config created at: {result.config_path}")
    print(f"Detected hardware: {result.detected_hardware}")
else:
    print(f"Setup failed: {result.error}")
```

### Hardware Detection

Automatic hardware detection optimizes configuration:

```python
from leindex.config import detect_hardware

hardware = detect_hardware()

print(f"CPU cores: {hardware.cpu_count}")
print(f"Total RAM: {hardware.total_ram_mb} MB")
print(f"Available RAM: {hardware.available_ram_mb} MB")
print(f"GPU available: {hardware.gpu_available}")
if hardware.gpu_available:
    print(f"GPU type: {hardware.gpu_type}")
    print(f"GPU memory: {hardware.gpu_memory_mb} MB")
```

### Manual Setup

Create configuration manually:

```python
from leindex.config import GlobalConfigManager, GlobalConfig

manager = GlobalConfigManager()

# Create configuration
config = GlobalConfig(
    version="2.0",
    memory=MemoryConfig(
        total_budget_mb=3072,
        soft_limit_percent=0.80,
        hard_limit_percent=0.93,
        emergency_percent=0.98,
    ),
    performance=PerformanceConfig(
        parallel_scanner_max_workers=4,
        parallel_processor_max_workers=4,
        embeddings_batch_size=32,
    ),
    # ... other sections
)

# Save configuration
manager.save_config(config, "~/.leindex/config.yaml")
```

## Configuration Examples

### Development Machine (8GB RAM)

```yaml
memory:
  total_budget_mb: 2048  # 2 GB (25% of total RAM)
  soft_limit_percent: 0.75
  hard_limit_percent: 0.90

performance:
  parallel_scanner:
    max_workers: 4  # 4-core CPU
  parallel_processor:
    max_workers: 4
  embeddings:
    enable_gpu: false  # No GPU
    batch_size: 16  # Smaller batches for CPU

global_index:
  tier2:
    max_size: 500  # Smaller cache
    ttl_seconds: 300
```

### Production Server (64GB RAM, GPU)

```yaml
memory:
  total_budget_mb: 16384  # 16 GB (25% of total RAM)
  soft_limit_percent: 0.80
  hard_limit_percent: 0.93

performance:
  parallel_scanner:
    max_workers: 16  # 16-core CPU
  parallel_processor:
    max_workers: 16
  embeddings:
    enable_gpu: true  # Use GPU
    device: "cuda"
    batch_size: 128  # Larger batches for GPU

global_index:
  tier2:
    max_size: 5000  # Larger cache
    ttl_seconds: 600  # Longer TTL
```

### Resource-Constrained Environment (4GB RAM)

```yaml
memory:
  total_budget_mb: 1024  # 1 GB (25% of total RAM)
  soft_limit_percent: 0.70
  hard_limit_percent: 0.85

performance:
  parallel_scanner:
    max_workers: 2  # Fewer workers
  parallel_processor:
    max_workers: 2
  embeddings:
    enable_gpu: false
    batch_size: 8  # Smaller batches

global_index:
  tier2:
    enabled: false  # Disable cache
```

### Large Monorepo (100K+ files)

```yaml
memory:
  total_budget_mb: 8192  # 8 GB
  max_loaded_files: 5000  # More files
  max_cached_queries: 2000  # More queries

performance:
  parallel_scanner:
    max_workers: 16  # More parallelism
    timeout_seconds: 600  # Longer timeout
  parallel_processor:
    max_workers: 16
    batch_size: 200  # Larger batches

global_index:
  tier1:
    refresh_interval_seconds: 300  # Less frequent refresh
  tier2:
    max_size: 10000  # Much larger cache
```

## Best Practices

### 1. Use Hierarchical Configuration

```yaml
# Global defaults (config.yaml)
memory:
  max_loaded_files: 100

# Override for large project (projects/large-project.yaml)
memory:
  max_loaded_files: 1000
```

### 2. Validate Configuration Before Use

```python
from leindex.config import ConfigValidator

validator = ConfigValidator()
config = load_config("config.yaml")

try:
    validator.validate(config)
except ValidationError as e:
    print(f"Invalid config: {e}")
    sys.exit(1)
```

### 3. Use Environment Variables for Secrets

```yaml
# config.yaml
security:
  encryption_key: "${LEINDEX_ENCRYPTION_KEY}"
```

```bash
export LEINDEX_ENCRYPTION_KEY="your-secret-key"
```

### 4. Backup Configuration Before Changes

```python
from leindex.config import GlobalConfigManager

manager = GlobalConfigManager()

# Backup current config
manager.backup_config("~/.leindex/config.yaml")

# Make changes
config = manager.get_config()
config.memory.total_budget_mb = 4096

# Save new config
manager.save_config(config, "~/.leindex/config.yaml")
```

### 5. Test Configuration Changes

```python
from leindex.config import GlobalConfigManager

manager = GlobalConfigManager()

# Load test configuration
config = manager.load_config("config.test.yaml")

# Validate
validator = ConfigValidator()
validator.validate(config)

# Test with actual workload
test_workload(config)

# If successful, replace production config
if test_passed:
    manager.backup_config("~/.leindex/config.yaml")
    manager.save_config(config, "~/.leindex/config.yaml")
```

## Troubleshooting

### Configuration Not Loading

**Problem**: Changes to config.yaml not taking effect

**Solution**:
1. Check file path: `~/.leindex/config.yaml`
2. Validate YAML syntax: `python -c "import yaml; yaml.safe_load(open('~/.leindex/config.yaml'))"`
3. Check file permissions: `ls -la ~/.leindex/config.yaml`
4. Reload configuration: `kill -HUP $(cat ~/.leindex/leindex.pid)`

### Validation Errors

**Problem**: Configuration validation fails

**Solution**:
1. Check error message for specific field
2. Verify data types (int vs string)
3. Check value ranges (percentages: 0.0-1.0)
4. Ensure threshold ordering: soft < hard < emergency

### Migration Errors

**Problem**: v1 to v2 migration fails

**Solution**:
1. Backup v1 config: `cp ~/.leindex/config.yaml ~/.leindex/config.v1.backup.yaml`
2. Run migration manually: See "Manual Migration" section
3. Create fresh v2 config: Run `first_time_setup()`
4. Manually migrate settings: Copy values from v1 to v2

## API Reference

### GlobalConfigManager

```python
class GlobalConfigManager:
    """Manages global configuration with validation and persistence."""

    def __init__(self, config_path: str = "~/.leindex/config.yaml"):
        """Initialize with config path."""

    def get_config(self) -> GlobalConfig:
        """Get current configuration."""

    def load_config(self, path: str) -> GlobalConfig:
        """Load configuration from file."""

    def save_config(self, config: GlobalConfig, path: str):
        """Save configuration to file."""

    def backup_config(self, path: str) -> str:
        """Backup configuration file."""

    def reload_config(self) -> ReloadResult:
        """Reload configuration from file."""
```

### ConfigValidator

```python
class ConfigValidator:
    """Validates configuration against schema."""

    def validate(self, config: Dict[str, Any]) -> None:
        """Validate configuration, raises ValidationError if invalid."""

    def validate_field(self, field: str, value: Any) -> None:
        """Validate a single field."""
```

### ConfigMigration

```python
class ConfigMigration:
    """Migrates configuration between versions."""

    def migrate_v1_to_v2(self, v1_config_path: str, v2_config_path: str) -> MigrationResult:
        """Migrate v1 configuration to v2 format."""

    def get_migration_diff(self, v1_config: Dict, v2_config: Dict) -> List[str]:
        """Get list of changes between versions."""
```

## See Also

- [docs/GLOBAL_INDEX.md](GLOBAL_INDEX.md) - Global index configuration
- [docs/MEMORY_MANAGEMENT.md](MEMORY_MANAGEMENT.md) - Memory configuration
- [docs/MIGRATION.md](MIGRATION.md) - v1 to v2 migration guide
- [examples/memory_configuration.py](../examples/memory_configuration.py) - Configuration examples
- [examples/config_migration.py](../examples/config_migration.py) - Migration examples
