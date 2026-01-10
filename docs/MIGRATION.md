# Migration Guide: v1 to v2

## Overview

This guide helps you migrate from LeIndex v1.x to v2.0, which introduces significant new features including Global Index, advanced memory management, and hierarchical configuration.

### What's New in v2.0

| Feature | v1.x | v2.0 | Benefit |
|---------|------|------|---------|
| **Global Index** | ❌ | ✅ | Cross-project search |
| **Memory Management** | Manual | Automatic | 70% memory reduction |
| **Configuration** | Single file | Hierarchical | Per-project overrides |
| **Config Reload** | Restart required | Zero-downtime | Instant updates |
| **Graceful Degradation** | All-or-nothing | Fallback chain | Resilient search |
| **Project Dashboard** | ❌ | ✅ | Comparison analytics |

### Breaking Changes

⚠️ **Configuration Format**: v2.0 uses a new configuration format with hierarchical structure.

⚠️ **Memory Configuration**: Memory limits are now specified as percentages of total budget.

⚠️ **API Changes**: Some API functions have been renamed or moved to new modules.

## Pre-Migration Checklist

Before migrating, ensure you have:

- [ ] Backed up your current configuration
- [ ] Documented your current memory settings
- [ ] Noted any custom configuration overrides
- [ ] Identified all indexed projects
- [ ] Scheduled downtime for the migration (expected: 5-10 minutes)

## Step-by-Step Migration

### Step 1: Backup Current Configuration

```bash
# Create backup directory
mkdir -p ~/.leindex/backups

# Backup v1 configuration
cp ~/.leindex/config.yaml ~/.leindex/backups/config.v1.yaml

# Backup project data
cp -r ~/.leindex/data ~/.leindex/backups/data.v1

# Backup indexes
cp -r ~/.leindex/leann_index ~/.leindex/backups/leann_index.v1
```

### Step 2: Export Current Settings

```python
# export_v1_settings.py
import yaml
import json

# Load v1 config
with open("~/.leindex/config.yaml") as f:
    v1_config = yaml.safe_load(f)

# Extract relevant settings
settings = {
    "memory": {
        "budget_mb": v1_config.get("memory", {}).get("budget_mb", 3072),
        "soft_limit_mb": v1_config.get("memory", {}).get("soft_limit_mb", 2457),
        "hard_limit_mb": v1_config.get("memory", {}).get("hard_limit_mb", 2857),
    },
    "performance": {
        "parallel_workers": v1_config.get("performance", {}).get("parallel_workers", 4),
        "batch_size": v1_config.get("performance", {}).get("batch_size", 32),
    },
    "projects": v1_config.get("projects", {})
}

# Save to JSON for reference
with open("~/.leindex/backups/v1_settings.json", "w") as f:
    json.dump(settings, f, indent=2)

print("v1 settings exported to ~/.leindex/backups/v1_settings.json")
```

### Step 3: Upgrade LeIndex

```bash
# Uninstall v1.x
pip uninstall leindex -y

# Install v2.0
pip install leindex==2.0.0

# Verify installation
leindex --version
# Output: LeIndex 2.0.0 - Ready to search! 🚀
```

### Step 4: Run First-Time Setup

```python
# migrate_setup.py
from leindex.config import first_time_setup, SetupResult
import json

# Load v1 settings
with open("~/.leindex/backups/v1_settings.json") as f:
    v1_settings = json.load(f)

# Run first-time setup
result: SetupResult = first_time_setup()

if not result.success:
    print(f"Setup failed: {result.error}")
    exit(1)

print("Setup complete!")
print(f"Config created at: {result.config_path}")
print(f"Detected hardware: {result.detected_hardware}")
```

### Step 5: Migrate Configuration

```python
# migrate_config.py
from leindex.config import GlobalConfigManager, GlobalConfig, MemoryConfig, PerformanceConfig
import yaml
import json

# Load v1 settings
with open("~/.leindex/backups/v1_settings.json") as f:
    v1_settings = json.load(f)

# Calculate percentages from v1 absolute values
budget_mb = v1_settings["memory"]["budget_mb"]
soft_percent = v1_settings["memory"]["soft_limit_mb"] / budget_mb
hard_percent = v1_settings["memory"]["hard_limit_mb"] / budget_mb
emergency_percent = 0.98  # Default for v2

# Create v2 configuration
config = GlobalConfig(
    version="2.0",
    memory=MemoryConfig(
        total_budget_mb=budget_mb,
        soft_limit_percent=soft_percent,
        hard_limit_percent=hard_percent,
        emergency_percent=emergency_percent,
        max_loaded_files=1000,  # v2 default
        max_cached_queries=500,  # v2 default
        project_defaults={
            "max_loaded_files": 100,
            "max_cached_queries": 50,
            "priority": "MEDIUM"
        }
    ),
    performance=PerformanceConfig(
        parallel_scanner_max_workers=v1_settings["performance"]["parallel_workers"],
        parallel_processor_max_workers=v1_settings["performance"]["parallel_workers"],
        embeddings_batch_size=v1_settings["performance"]["batch_size"],
        embeddings_enable_gpu=True,
        embeddings_device="auto"
    )
)

# Save v2 configuration
manager = GlobalConfigManager()
manager.save_config(config, "~/.leindex/config.yaml")

print("Configuration migrated successfully!")
print(f"Memory budget: {budget_mb} MB")
print(f"Soft limit: {soft_percent*100:.1f}% ({soft_percent*budget_mb:.0f} MB)")
print(f"Hard limit: {hard_percent*100:.1f}% ({hard_percent*budget_mb:.0f} MB)")
```

### Step 6: Migrate Project Overrides

```python
# migrate_projects.py
from leindex.config import GlobalConfigManager
import json

# Load v1 settings
with open("~/.leindex/backups/v1_settings.json") as f:
    v1_settings = json.load(f)

manager = GlobalConfigManager()

# Migrate each project override
for project_id, project_config in v1_settings.get("projects", {}).items():
    project_name = project_id.split("/")[-1]
    project_path = f"~/.leindex/projects/{project_name}.yaml"

    # Create project override config
    override_config = {
        "project_id": project_id,
        "memory": {
            "max_loaded_files": project_config.get("max_loaded_files", 100),
            "max_cached_queries": project_config.get("max_cached_queries", 50),
            "priority": project_config.get("priority", "MEDIUM")
        }
    }

    # Save project override
    with open(project_path, "w") as f:
        yaml.dump(override_config, f, default_flow_style=False)

    print(f"Migrated project override: {project_name}")

print("All project overrides migrated successfully!")
```

### Step 7: Verify Migration

```python
# verify_migration.py
from leindex.config import GlobalConfigManager, ConfigValidator

manager = GlobalConfigManager()
validator = ConfigValidator()

# Load migrated configuration
config = manager.get_config()

# Validate configuration
try:
    validator.validate_model(config)
    print("✓ Configuration is valid")
except Exception as e:
    print(f"✗ Configuration validation failed: {e}")
    exit(1)

# Display key settings
print("\nMigration Summary:")
print(f"  Memory Budget: {config.memory.total_budget_mb} MB")
print(f"  Soft Limit: {config.memory.soft_limit_percent*100:.1f}%")
print(f"  Hard Limit: {config.memory.hard_limit_percent*100:.1f}%")
print(f"  Emergency Limit: {config.memory.emergency_percent*100:.1f}%")
print(f"  Max Loaded Files: {config.memory.max_loaded_files}")
print(f"  Max Cached Queries: {config.memory.max_cached_queries}")
print(f"  Parallel Workers: {config.performance.parallel_scanner_max_workers}")
print(f"  Batch Size: {config.performance.embeddings_batch_size}")

print("\n✓ Migration verification complete!")
```

### Step 8: Test v2.0 Features

```python
# test_v2_features.py
from leindex.global_index import get_global_stats, list_projects
from leindex.memory import MemoryManager, get_current_usage_mb

# Test global index
print("Testing Global Index...")
stats = get_global_stats()
print(f"  Total Projects: {stats.total_projects}")
print(f"  Total Symbols: {stats.total_symbols}")

# Test memory management
print("\nTesting Memory Management...")
manager = MemoryManager()
status = manager.get_status()
print(f"  Current Memory: {status.current_mb:.1f} MB")
print(f"  Peak Memory: {status.peak_mb:.1f} MB")

# Test project listing
print("\nTesting Project Listing...")
projects = list_projects(format="simple")
print(f"  Listed {projects['count']} projects")

print("\n✓ All v2.0 features working correctly!")
```

## Rollback Procedure

If you need to rollback to v1.x:

### Step 1: Stop v2.0

```bash
# Stop LeIndex server
pkill -f "leindex mcp"

# Or if running as service
systemctl stop leindex
```

### Step 2: Uninstall v2.0

```bash
pip uninstall leindex -y
```

### Step 3: Restore v1.x

```bash
# Restore v1 configuration
cp ~/.leindex/backups/config.v1.yaml ~/.leindex/config.yaml

# Restore v1 data
rm -rf ~/.leindex/data
cp -r ~/.leindex/backups/data.v1 ~/.leindex/data

# Restore v1 indexes
rm -rf ~/.leindex/leann_index
cp -r ~/.leindex/backups/leann_index.v1 ~/.leindex/leann_index
```

### Step 4: Reinstall v1.x

```bash
pip install leindex==1.1.0

# Verify installation
leindex --version
# Output: LeIndex 1.1.0
```

## Post-Migration Tasks

### 1. Update Scripts

Update any scripts that use v1.x APIs:

```python
# v1.x API
from leindex import MemoryProfiler
profiler = MemoryProfiler()
profiler.take_snapshot()

# v2.0 API
from leindex.memory import MemoryManager
manager = MemoryManager()
manager.take_snapshot()
```

### 2. Update Environment Variables

Rename environment variables:

```bash
# v1.x
export CODE_INDEX_MEMORY_BUDGET_MB=3072

# v2.0
export LEINDEX_MEMORY_TOTAL_BUDGET_MB=3072
```

### 3. Update MCP Configuration

Update MCP client configuration if needed:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {
        "LEINDEX_MEMORY_TOTAL_BUDGET_MB": "3072"
      }
    }
  }
}
```

### 4. Reindex Projects (Optional)

If you want to take advantage of v2.0 performance improvements:

```bash
# Reindex all projects
leindex reindex --all

# Or reindex specific project
leindex reindex /path/to/project
```

## Configuration Mapping

### Memory Settings

| v1.x Field | v2.0 Field | Conversion |
|------------|------------|------------|
| `memory.budget_mb` | `memory.total_budget_mb` | Direct mapping |
| `memory.soft_limit_mb` | `memory.soft_limit_percent` | Divide by budget |
| `memory.hard_limit_mb` | `memory.hard_limit_percent` | Divide by budget |
| N/A | `memory.emergency_percent` | New field (default: 0.98) |
| `memory.max_loaded_files` | `memory.max_loaded_files` | Direct mapping |
| `memory.max_cached_queries` | `memory.max_cached_queries` | Direct mapping |

**Example**:
```yaml
# v1.x
memory:
  budget_mb: 3072
  soft_limit_mb: 2457
  hard_limit_mb: 2857

# v2.0
memory:
  total_budget_mb: 3072
  soft_limit_percent: 0.80  # 2457 / 3072
  hard_limit_percent: 0.93  # 2857 / 3072
  emergency_percent: 0.98
```

### Performance Settings

| v1.x Field | v2.0 Field | Conversion |
|------------|------------|------------|
| `performance.parallel_workers` | `performance.parallel_scanner.max_workers` | Moved to nested config |
| `performance.parallel_workers` | `performance.parallel_processor.max_workers` | Duplicate value |
| `performance.batch_size` | `performance.embeddings.batch_size` | Moved to embeddings |
| `performance.enable_gpu` | `performance.embeddings.enable_gpu` | Moved to embeddings |
| N/A | `performance.file_stat_cache.enabled` | New field (default: true) |
| N/A | `performance.pattern_trie.enabled` | New field (default: true) |

**Example**:
```yaml
# v1.x
performance:
  parallel_workers: 4
  batch_size: 32
  enable_gpu: true

# v2.0
performance:
  parallel_scanner:
    max_workers: 4
  parallel_processor:
    max_workers: 4
  embeddings:
    batch_size: 32
    enable_gpu: true
```

## API Changes

### Memory Management

```python
# v1.x API
from leindex.memory_profiler import MemoryProfiler, MemorySnapshot, MemoryLimits

profiler = MemoryProfiler(limits=MemoryLimits(
    soft_limit_mb=2457,
    hard_limit_mb=2857
))
snapshot = profiler.take_snapshot()

# v2.0 API
from leindex.memory import MemoryManager, MemoryStatus

manager = MemoryManager()
status = manager.get_status()
```

### Global Index (New)

```python
# v1.x (not available)
# No equivalent

# v2.0 API
from leindex.global_index import get_global_stats, cross_project_search

stats = get_global_stats()
results = cross_project_search("authentication")
```

### Configuration

```python
# v1.x API
from leindex.config_manager import ConfigManager

manager = ConfigManager()
config = manager.load_config()

# v2.0 API
from leindex.config import GlobalConfigManager

manager = GlobalConfigManager()
config = manager.get_config()
```

## Troubleshooting

### Migration Fails

**Problem**: Migration script fails with error

**Solution**:
1. Check v1 config syntax: `python -c "import yaml; yaml.safe_load(open('~/.leindex/config.yaml'))"`
2. Verify v1 settings export: Check `~/.leindex/backups/v1_settings.json`
3. Run migration with debug output: `python migrate_config.py --debug`

### Configuration Validation Errors

**Problem**: Migrated configuration fails validation

**Solution**:
1. Check threshold percentages are 0.0-1.0
2. Verify soft < hard < emergency ordering
3. Ensure all numeric values are positive
4. Review validation error message for specific field

### Memory Issues After Migration

**Problem**: Memory usage higher than expected

**Solution**:
1. Check total_budget_mb matches v1 budget_mb
2. Verify threshold percentages are correct
3. Reduce max_loaded_files and max_cached_queries
4. Enable spill-to-disk: `memory.spill.enabled: true`

### Projects Not Found

**Problem**: Previously indexed projects not found

**Solution**:
1. Check project paths in `~/.leindex/projects/*.yaml`
2. Verify project paths still exist
3. Reindex projects: `leindex reindex /path/to/project`
4. Check global index: `list_projects(format="detailed")`

## Best Practices

### 1. Test Migration in Staging

Before migrating production:

```bash
# Create test environment
mkdir -p ~/leindex-test
cp -r ~/.leindex ~/leindex-test

# Run migration in test environment
cd ~/leindex-test
python migrate_config.py
```

### 2. Gradual Rollout

Roll out v2.0 gradually:

1. **Week 1**: Test with 1-2 small projects
2. **Week 2**: Add medium projects
3. **Week 3**: Migrate large projects
4. **Week 4**: Full rollout

### 3. Monitor After Migration

Monitor system metrics after migration:

```python
# monitor_migration.py
from leindex.memory import MemoryManager
import time

manager = MemoryManager()

# Monitor for 24 hours
for i in range(24):
    status = manager.get_status()
    print(f"{i}h: {status.current_mb:.1f} MB")
    time.sleep(3600)  # Wait 1 hour
```

### 4. Document Custom Changes

Document any custom configuration changes:

```markdown
# Migration Notes

## Custom Configuration Changes

1. Increased memory budget to 4096 MB (from 3072 MB)
2. Reduced soft limit to 75% (from 80%)
3. Enabled GPU for embeddings
4. Added project override for large-project

## Issues Encountered

1. Validation error on threshold percentages - Fixed by adjusting values
2. Project path mismatch - Fixed by updating project_id in override

## Rollback Information

- v1.x version: 1.1.0
- v2.0 version: 2.0.0
- Migration date: 2025-01-08
- Rollback steps: See section "Rollback Procedure"
```

## Additional Resources

- **[docs/GLOBAL_INDEX.md](GLOBAL_INDEX.md)** - Global index documentation
- **[docs/MEMORY_MANAGEMENT.md](MEMORY_MANAGEMENT.md)** - Memory management guide
- **[docs/CONFIGURATION.md](CONFIGURATION.md)** - Configuration reference
- **[examples/config_migration.py](../examples/config_migration.py)** - Migration examples
- **GitHub Issues**: [https://github.com/scooter-lacroix/leindex/issues](https://github.com/scooter-lacroix/leindex/issues)

## Support

If you encounter issues during migration:

1. Check this guide's troubleshooting section
2. Review error messages carefully
3. Search GitHub Issues for similar problems
4. Create a new issue with:
   - LeIndex versions (v1.x and v2.0)
   - Error messages
   - Configuration files (sanitized)
   - System information (OS, Python version, RAM)

## Summary

Migrating from v1.x to v2.0 involves:

1. ✅ Backup current configuration and data
2. ✅ Export v1 settings for reference
3. ✅ Upgrade to v2.0
4. ✅ Run first-time setup
5. ✅ Migrate configuration to v2 format
6. ✅ Migrate project overrides
7. ✅ Verify migration
8. ✅ Test v2.0 features
9. ✅ Update scripts and environment variables
10. ✅ Monitor system after migration

**Expected Migration Time**: 5-10 minutes

**Downtime**: Minimal (configuration reload is zero-downtime)

**Benefits**: Cross-project search, automatic memory management, per-project configuration, graceful degradation
