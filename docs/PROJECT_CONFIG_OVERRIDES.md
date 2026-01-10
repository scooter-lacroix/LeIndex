# Project Configuration Overrides

## Overview

LeIndex supports per-project configuration overrides that allow individual projects to customize their memory allocation hints and eviction priorities. This feature is particularly useful for projects with unusual memory requirements or different performance characteristics.

## Key Concepts

### Hints, Not Reservations

**IMPORTANT:** Project configuration values are *hints*, not *reservations*. The memory manager uses these values as guidance but may allocate more or less memory based on:

- Global memory budget constraints
- Overall system load
- Other projects' needs
- Available system resources

### Configuration Location

Project-specific configuration is stored in:
```
<project_root>/.leindex_data/config.yaml
```

This file is created automatically when you first configure a project.

## Configuration Options

### Memory Configuration

The `memory` section controls how much memory a project needs and its eviction priority.

```yaml
memory:
  # Estimated memory allocation in MB (optional)
  # Default: 256MB (from global config)
  # Maximum: 512MB (to prevent monopolization)
  # If not specified, uses global default
  estimated_mb: 512

  # Priority for eviction decisions (optional)
  # Default: "normal"
  # Options: "high", "normal", "low"
  # Higher priority projects are less likely to be evicted
  priority: high
```

#### estimated_mb

- **Purpose:** Tells the memory manager how much memory this project typically needs
- **Default:** 256MB (from global config)
- **Maximum:** 512MB (enforced to prevent one project from monopolizing memory)
- **Behavior:**
  - Values above the global default trigger a warning
  - Values approaching 512MB trigger a warning
  - The memory manager uses this as a suggestion, not a guarantee
  - Actual allocation may vary based on system conditions

#### priority

Controls how likely a project is to be evicted during memory pressure:

- **`high`**: Priority score 2.0 - Evicted last (for active/critical projects)
- **`normal`**: Priority score 1.0 - Default behavior
- **`low`**: Priority score 0.5 - Evicted first (for inactive/reference projects)

Priority affects eviction order when the memory manager needs to free up space.

## Example Configurations

### Large ML Project

A machine learning project with large code and data files:

```yaml
# .leindex_data/config.yaml
memory:
  estimated_mb: 512  # Double the default
  priority: high      # Keep in memory during active development
```

### Small Utility Project

A small utility script that's rarely used:

```yaml
# .leindex_data/config.yaml
memory:
  estimated_mb: 64   # Much smaller than default
  priority: low       # OK to evict when memory is needed
```

### Default Configuration

If no `.leindex_data/config.yaml` exists, the project uses global defaults:

```yaml
# From ~/.leindex/mcp_config.yaml
memory:
  estimated_mb: 256  # Default for all projects
  priority: normal
```

## Usage

### Programmatic API

#### Loading Configuration

```python
from leindex.project_config import ProjectConfigManager

# Create manager for a project
manager = ProjectConfigManager("/path/to/project")

# Get project configuration
config = manager.get_config()
print(f"Memory estimate: {config.memory.estimated_mb}MB")
print(f"Priority: {config.memory.priority}")
```

#### Getting Effective Configuration

```python
from leindex.project_config import get_effective_memory_config

# Get effective memory config (merges project overrides with global defaults)
effective = get_effective_memory_config("/path/to/project")

print(f"Estimated memory: {effective['estimated_mb']}MB")
print(f"Priority: {effective['priority']}")
print(f"Priority score: {effective['priority_score']}")
print(f"Is overridden: {effective['is_overridden']}")
```

#### Saving Configuration

```python
from leindex.project_config import ProjectConfigManager, ProjectConfig, ProjectMemoryConfig

manager = ProjectConfigManager("/path/to/project")

# Create custom config
config = ProjectConfig(
    memory=ProjectMemoryConfig(
        estimated_mb=512,
        priority="high"
    )
)

# Save to .leindex_data/config.yaml
manager.save_config(config)
```

#### Deleting Configuration

```python
from leindex.project_config import ProjectConfigManager

manager = ProjectConfigManager("/path/to/project")

# Remove project config (reverts to global defaults)
manager.delete_config()
```

### Convenience Functions

```python
from leindex.project_config import load_project_config, get_effective_memory_config

# Quick load
config = load_project_config("/path/to/project")

# Quick effective config
effective = get_effective_memory_config("/path/to/project")
```

## Validation and Warnings

### Validation Rules

1. **Priority Values:** Must be "high", "normal", or "low" (case-sensitive)
2. **Non-negative Memory:** `estimated_mb` must be ≥ 0
3. **Maximum Override:** `estimated_mb` cannot exceed 512MB

### Warnings

The system logs warnings in these cases:

1. **Exceeding Global Default:** When `estimated_mb` > global default (256MB)
   ```
   WARNING: Project /path/to/project has estimated_mb=512MB,
   which is 2.0x the global default (256MB). This is a hint,
   not a reservation. Actual allocation may vary.
   ```

2. **Approaching Maximum:** When `estimated_mb` > 460MB (90% of max)
   ```
   WARNING: Project /path/to/project estimated_mb (512MB)
   is approaching max_override_mb (512MB)
   ```

## Integration with Memory Manager

The project configuration integrates with the hierarchical memory management system:

### Priority-Based Eviction

When memory pressure occurs, the memory manager considers project priorities:

```python
# Pseudocode for eviction decision
def should_evict(project_a, project_b):
    """Return True if project_a should be evicted before project_b."""
    score_a = get_effective_memory_config(project_a)['priority_score']
    score_b = get_effective_memory_config(project_b)['priority_score']
    return score_a < score_b
```

Example eviction order (low to high priority):
1. Low priority projects (score 0.5)
2. Normal priority projects (score 1.0)
3. High priority projects (score 2.0)

### Memory Allocation Hints

The memory manager uses `estimated_mb` as a hint for:

- **Initial allocation:** How much memory to reserve when loading a project
- **Cache sizing:** How large to make project-specific caches
- **Budget planning:** How to distribute the global memory budget

However, actual allocation is adjusted based on:
- Available system memory
- Current memory pressure
- Other projects' needs
- Runtime performance metrics

## Best Practices

### When to Set estimated_mb

**Set higher than default (256MB) if:**
- Project has very large source files (e.g., generated code, ML models)
- Project uses complex language features requiring more analysis
- Project is frequently accessed and performance is critical

**Set lower than default if:**
- Project is small (few files, simple code)
- Project is rarely accessed
- System memory is constrained

**Use default if:**
- Project is typical size (hundreds to thousands of files)
- You're unsure of actual memory usage

### When to Set Priority

**Use `high` priority for:**
- Active development projects
- Critical production code
- Projects where load time is expensive

**Use `normal` priority for:**
- Most projects (default)
- Projects with moderate usage

**Use `low` priority for:**
- Reference projects (rarely accessed)
- Archived code (kept for occasional reference)
- Test/example projects

### Monitoring Configuration

Monitor memory usage to adjust configuration:

```python
# Check actual memory usage
from leindex.memory import MemoryProfiler

profiler = MemoryProfiler()
stats = profiler.get_project_stats("/path/to/project")

print(f"Actual memory: {stats['memory_mb']}MB")
print(f"Configured estimate: {effective['estimated_mb']}MB")

# Adjust if significantly different
if stats['memory_mb'] > effective['estimated_mb'] * 1.5:
    print("Consider increasing estimated_mb")
```

## Troubleshooting

### Configuration Not Applied

**Problem:** Changes to `.leindex_data/config.yaml` don't seem to work.

**Solutions:**
1. Check the file is in the correct location: `<project_root>/.leindex_data/config.yaml`
2. Verify YAML syntax is valid
3. Check logs for validation errors
4. Use `force_reload=True` when testing:
   ```python
   config = manager.get_config(force_reload=True)
   ```

### Unexpected Memory Allocation

**Problem:** Project uses more/less memory than configured.

**Explanation:** Configuration is a hint, not a reservation. The memory manager adjusts based on:
- System memory pressure
- Other projects' needs
- Actual memory usage patterns

**Solutions:**
1. Monitor actual memory usage
2. Adjust `estimated_mb` if consistently off
3. Consider priority for important projects
4. Review global memory budget settings

### Validation Errors

**Problem:** Configuration fails to load with validation error.

**Common issues:**
1. Invalid priority value (must be "high", "normal", or "low")
2. `estimated_mb` exceeds 512MB
3. Negative `estimated_mb`
4. Malformed YAML

**Solution:** Check logs for specific validation error, fix the issue, and reload.

## Files

- **Implementation:** `src/leindex/project_config.py`
- **Tests:** `tests/unit/test_project_config.py` (50 tests, 100% passing)
- **Global Config:** `src/leindex/config/global_config.py`
- **Memory Manager:** `src/leindex/memory/` (integrated with project config)

## See Also

- [Memory Management](./MEMORY_MANAGEMENT.md) - How the memory manager uses project config
- [Global Configuration](./GLOBAL_CONFIG.md) - Global defaults and limits
- [Performance Tuning](./PERFORMANCE_TUNING.md) - Optimizing memory allocation
