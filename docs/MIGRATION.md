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
      "command": "npx",
      "args": ["-y", "@leindex/mcp"],
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
- **GitHub Issues**: [https://github.com/scooter-lacroix/LeIndex/issues](https://github.com/scooter-lacroix/LeIndex/issues)

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

---

# Migration Guide: v2.0 to v1.9.0 (MCP Index Start/Poll)

This section covers the behavior change in the LeIndex MCP `index` tool introduced in v1.9.0. The `leindex.index` MCP tool changed from a **request-blocking (synchronous)** model to a **start/poll (asynchronous)** model powered by registry-owned single-flight index jobs.

> **Scope:** This migration only affects MCP clients that call `leindex.index` over stdio/HTTP transports. Direct CLI invocations (`leindex index <path>`) remain synchronous and are unaffected. The change is backward compatible via the `wait` parameter, so existing clients that pass `wait: true` continue to receive the full blocking response.

## Overview

| Concern | v2.0 (and earlier) | v1.9.0 |
|---------|--------------------|--------|
| **Index call semantics** | Blocking: request held open until the full index (PDG, TF-IDF, optional neural) completes | Non-blocking: returns a `job_id` and phase snapshot immediately |
| **Transport disconnect** | Could cancel in-flight parse/transaction/generation work | Owned job survives disconnect and runs to a terminal state |
| **Completion polling** | Not needed (single call returned final results) | Repeat `leindex.index` calls until `status` is `complete` or `failed` |
| **Backward compatibility** | N/A | `wait: true` parameter restores the v2.0 blocking response |
| **Concurrency** | Caller-driven; duplicate concurrent requests could run redundant builds | Registry coalesces concurrent requests for the same project into one job |
| **Freshness signal** | Free-text `_warning` string on stale responses | Structured `_meta.freshness` badge on every tool response |

### Why the change

In v2.0 and earlier, the MCP `index` tool held the JSON-RPC request open for the entire indexing duration. Long-running neural enrichment tied up the request, and if an MCP client (editor, agent) dropped the connection mid-index, the in-flight parse, transaction, or generation swap could be interrupted. v1.9.0 makes index jobs **owned by the registry**, detached from the caller's request lifetime, and queryable through a stable `job_id`. Core PDG and TF-IDF layers publish first and are immediately queryable, after which the configured neural worker is actively evaluated for hybrid rows.

## 1. Before/After: Blocking vs Start/Poll

### Before (v2.0 and earlier) - request-blocking

A single `leindex.index` call blocked until the full index was built, then returned the final stats inline. The MCP client had to keep the request open for the whole duration.

```jsonc
// Request (v2.0)
{
  "method": "tools/call",
  "params": {
    "name": "leindex.index",
    "arguments": {
      "project_path": "/code/my-project"
    }
  }
}

// Response (v2.0) - returned only after the FULL index finished.
// The JSON-RPC request was held open the entire time.
{
  "project_path": "/code/my-project",
  "files_indexed": 842,
  "symbols": 5123,
  "generation": 4,
  "status": "complete"
}
```

### After (v1.9.0) - start/poll with `job_id`

The first call **starts** the job and returns immediately with a `job_id` and the current phase snapshot. Subsequent calls with the same `project_path` **poll** the same job (no new job is started while one is running).

```jsonc
// Request - START the index job (returns immediately)
{
  "method": "tools/call",
  "params": {
    "name": "leindex.index",
    "arguments": {
      "project_path": "/code/my-project"
    }
  }
}

// Response (v1.9.0) - returns at once with a pollable job snapshot.
// status is "running" while the job is in flight.
{
  "job_id": "a1b2c3d4e5f67890-1",
  "status": "running",
  "phase": "scan",
  "generation": 0,
  "completed_units": 0,
  "total_units": 0,
  "published": {
    "pdg": false,
    "lexical": false,
    "neural": false
  },
  "last_error": null
}
```

Once the core PDG and TF-IDF layers are durable, the snapshot reports `published.pdg: true` and `published.lexical: true` even while the optional neural phase is still running - those core results are already queryable through the current generation. The job reaches a terminal state of `complete` (all configured layers published) or `failed` (with `last_error` populated).

## 2. How to Poll for Completion

To poll, send the **same** `leindex.index` call again with the same `project_path`. Concurrent calls for the same project are coalesced into the running job and return the same `job_id`, so polling does not start a second build.

```jsonc
// Poll #1 - same arguments, returns the current snapshot for the running job
{
  "method": "tools/call",
  "params": {
    "name": "leindex.index",
    "arguments": { "project_path": "/code/my-project" }
  }
}

// Response while still running (core layers already published)
{
  "job_id": "a1b2c3d4e5f67890-1",
  "status": "running",
  "phase": "neural",
  "generation": 5,
  "completed_units": 842,
  "total_units": 842,
  "published": { "pdg": true, "lexical": true, "neural": false },
  "last_error": null
}
```

```jsonc
// Poll #2 - terminal state: job is complete.
// Stop polling once status is "complete" (or "failed").
{
  "job_id": "a1b2c3d4e5f67890-1",
  "status": "complete",
  "phase": "complete",
  "generation": 5,
  "completed_units": 842,
  "total_units": 842,
  "published": { "pdg": true, "lexical": true, "neural": true },
  "last_error": null
}
```

**Polling guidance:**

- The `job_id` is stable for the lifetime of the job; reuse it to correlate start and poll responses.
- Polling is cheap: a poll for a running job returns the in-memory snapshot without re-spawning any indexing work.
- Stop polling when `status` is `"complete"` or `"failed"`. On `"failed"`, inspect `last_error`.
- You do not need to pass `job_id` in the poll request - the registry keys jobs by canonical `project_path`, so passing `project_path` alone retrieves the active job.

## 3. `wait=true` Backward Compatibility

Callers that want the v2.0 synchronous behavior (block until the job reaches a terminal state, then return the final snapshot) can pass `wait: true`. This is intended for **interactive** callers (a human running a one-shot CLI-style command through MCP); polling is the default and recommended model for automated clients.

```jsonc
// Request - wait for completion (v2.0-compatible blocking behavior)
{
  "method": "tools/call",
  "params": {
    "name": "leindex.index",
    "arguments": {
      "project_path": "/code/my-project",
      "wait": true
    }
  }
}

// Response - returns once the job reaches a terminal state.
// Identical shape to a poll response, but the call did not return
// until status became "complete" or "failed".
{
  "job_id": "a1b2c3d4e5f67890-1",
  "status": "complete",
  "phase": "complete",
  "generation": 5,
  "completed_units": 842,
  "total_units": 842,
  "published": { "pdg": true, "lexical": true, "neural": true },
  "last_error": null
}
```

Notes:

- `wait` defaults to `false`. Omitting it returns immediately (start/poll model).
- `wait=true` is still owned by the registry: if the MCP transport disconnects while waiting, the **job** continues to run to completion - only the caller's wait is dropped. A later `leindex.index` poll for the same project will return the (now terminal) snapshot.
- Existing v2.0 clients that already pass `wait: true` (or were upgraded to do so) require **no code changes** to keep receiving a single terminal response.

## 4. `force_reindex=true` Behavior

By default, if the project is already indexed and `is_stale_fast()` reports the index as fresh, `leindex.index` returns the existing current generation without re-indexing. Pass `force_reindex: true` to bypass the freshness cache and rebuild a new generation unconditionally.

```jsonc
// Request - force a full rebuild even when the index is fresh
{
  "method": "tools/call",
  "params": {
    "name": "leindex.index",
    "arguments": {
      "project_path": "/code/my-project",
      "force_reindex": true
    }
  }
}
```

Behavior:

- `force_reindex` is accepted as a boolean (`true`/`false`) and also as the compatibility strings `"true"`/`"false"`, `"1"`/`"0"`, or `"yes"`/`"no"`. It defaults to `false`.
- A forced rebuild allocates a new generation (`previous_generation + 1`) and publishes it atomically once complete; the prior generation is retained until the new one is durable, so queries remain served throughout the rebuild.
- `force_reindex` combines with `wait`: `force_reindex: true, wait: true` performs a synchronous full rebuild.
- If a job is already running for the project, `force_reindex` does not start a second concurrent build - the running job is returned (request coalescing). To force a fresh rebuild, wait for the current job to reach a terminal state first.

## 5. Structured `_meta.freshness` Badge

In v2.0 and earlier, staleness was surfaced as a free-text `_warning` string (e.g. `"Index may be stale; run leindex.index with force_reindex=true to refresh."`) attached to tool responses. Clients had to substring-match prose to detect staleness.

In v1.9.0, every tool response carries a structured `_meta.freshness` object with machine-readable fields. The free-text `_warning` is replaced by this badge, and a separate per-session `advisory` string (shown once per session and generation) carries the human-readable hint.

```jsonc
// Tool response (v1.9.0) - structured freshness badge on _meta
{
  "project_path": "/code/my-project",
  "results": [ /* ...tool-specific payload... */ ],
  "_meta": {
    "freshness": {
      "generation": 5,
      "status": "Fresh",            // ComponentStatus: Fresh | Stale | Partial | Failed
      "phase": "Complete",          // IndexPhase
      "head_oid": "abc1234",
      "tree_oid": "def5678",
      "indexed_file_count": 842,
      "dirty_file_count": 3,        // live git dirty count, floors at health value
      "changed_unindexed_count": 3,
      "age_ms": 1864000,            // ms since the generation was indexed
      "last_failure_phase": null,   // set when the last index failed at a phase
      "last_failure": null,
      "warning": "Index may be stale; run leindex.index with force_reindex=true to refresh.",
      "advisory": null              // human-readable hint, shown once per session+generation
    }
  }
}
```

Field semantics:

- **`generation`** - the currently loaded immutable generation number. Compare across calls to detect that a reindex published a new generation.
- **`status`** - `ComponentStatus` enum serialized as a string: `Fresh`, `Stale`, `Partial`, or `Failed`. `Fresh` means the loaded generation matches the current git tree and no tracked source files are dirty.
- **`phase`** - the `IndexPhase` of the loaded generation (typically `Complete` for queryable results).
- **`dirty_file_count`** / **`changed_unindexed_count`** - live `git status --porcelain=v2` counts, floored at the persisted health value. Non-zero with `status != Fresh` indicates the worktree has drifted from the indexed generation.
- **`age_ms`** - milliseconds since the generation was indexed. Useful for "how stale?" heuristics.
- **`warning`** - present (non-null) **only when** the index is genuinely stale (a tracked source file differs from what was indexed). This is the compact, always-present freshness warning. When you see it, call `leindex.index` (optionally with `force_reindex=true`) to refresh.
- **`advisory`** - a per-session, per-generation hint string. The MCP transport emits the advisory at most once per session for each generation; subsequent responses for the same generation set `advisory: null` to avoid repeating the hint in long conversations. A new `generation` resets the advisory for that session. Direct CLI calls are unaffected.

**Migration steps for clients:**

1. Stop substring-matching the old free-text `_warning`. Instead, read `_meta.freshness.status` and `_meta.freshness.warning`.
2. Treat `status == "Fresh"` as "results are current"; any other value (or a non-null `warning`) means a reindex is recommended.
3. Use `generation` to correlate a `leindex.index` poll/start response with the freshness badge on subsequent tool calls (the badge generation updates once a new generation publishes).
4. The `advisory` field replaces the "show this warning once" UX. MCP clients can display `advisory` when non-null and rely on the badge afterward.

## Migration Quick Reference

| Old (v2.0) behavior | New (v1.9.0) behavior | What to do |
|---------------------|-----------------------|------------|
| `leindex.index` blocks until complete | `leindex.index` returns a `job_id` immediately | Poll with the same call until `status` is terminal, or pass `wait: true` |
| Disconnect cancels index | Job survives disconnect, runs to terminal state | No action needed; poll later to retrieve the terminal snapshot |
| Duplicate concurrent requests run redundant builds | Concurrent requests coalesce into one job (same `job_id`) | No action needed; coalescing is automatic |
| Free-text `_warning` string | Structured `_meta.freshness` badge + per-session `advisory` | Read `_meta.freshness.status` / `.warning` instead of substring-matching |
| No freshness struct | `generation`, `status`, `phase`, `age_ms`, `dirty_file_count`, ... | Use these fields for staleness heuristics and reindex triggers |
| Always rebuild on stale | Rebuild only when stale; `force_reindex=true` forces it | Pass `force_reindex=true` only when you must bypass the freshness cache |

## Rollback / Coexistence

- To get the exact v2.0 synchronous behavior back, pass `wait: true` on every `leindex.index` call.
- The structured freshness badge is additive (it sits under `_meta.freshness`); clients that ignore `_meta` are unaffected.
- `force_reindex` semantics are unchanged from v2.0; only the default non-blocking return shape is new.
