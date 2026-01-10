# Memory Management MCP Tools - Quick Reference

## Quick Start

All 4 tools are now available in the LeIndex MCP server. Use them to monitor, configure, and control memory usage.

---

## Tool 1: get_memory_status()

**Check current memory usage and health status**

```python
result = await get_memory_status(ctx)
```

**Key Fields:**
- `result['current_mb']` - Current RSS memory (MB)
- `result['total_budget_mb']` - Total budget (MB)
- `result['usage_percent']` - Usage percentage
- `result['status']` - "healthy" | "caution" | "warning" | "critical"
- `result['breakdown']` - Component memory breakdown

**Example Output:**
```json
{
  "success": true,
  "current_mb": 2048.5,
  "total_budget_mb": 3072.0,
  "usage_percent": 66.7,
  "status": "caution",
  "soft_limit_mb": 2457.6,
  "hard_limit_mb": 3010.6,
  "prompt_threshold_mb": 2857.0,
  "global_index_mb": 512.0,
  "growth_rate_mb_per_sec": 0.15
}
```

---

## Tool 2: configure_memory()

**Update memory limits and thresholds**

```python
result = await configure_memory(
    ctx,
    total_budget_mb=4096,        # Optional: Total budget (512-65536 MB)
    global_index_mb=1024,        # Optional: Global index allocation (128-8192 MB)
    soft_limit_percent=85,       # Optional: Warning threshold (50-95%)
    prompt_threshold_percent=95, # Optional: LLM threshold (50-99%)
    hard_limit_percent=99        # Optional: Emergency threshold (51-100%)
)
```

**Key Fields:**
- `result['success']` - Boolean status
- `result['old_values']` - Previous configuration
- `result['new_values']` - Updated configuration
- `result['updated_fields']` - List of changed fields

**Example:**
```python
# Increase total budget to 4GB
result = await configure_memory(ctx, total_budget_mb=4096)
print(f"Updated: {result['updated_fields']}")
# Output: Updated: ['total_budget_mb']

# Update thresholds
result = await configure_memory(
    ctx,
    soft_limit_percent=85,
    prompt_threshold_percent=95,
    hard_limit_percent=99
)
print(f"Updated: {result['updated_fields']}")
# Output: Updated: ['warning_threshold_percent', 'prompt_threshold_percent', 'emergency_threshold_percent']
```

---

## Tool 3: trigger_eviction()

**Free memory by evicting cached projects**

```python
result = await trigger_eviction(
    ctx,
    action="evict_projects",  # "evict_projects" or "unload_projects"
    target_mb=512             # Target memory to free (10-4096 MB, default: 256)
)
```

**Key Fields:**
- `result['success']` - Boolean status
- `result['projects_affected']` - List of evicted project IDs
- `result['memory_freed_mb']` - Actual memory freed
- `result['target_mb']` - Target memory to free
- `result['duration_seconds']` - Time taken

**Example:**
```python
# Free 512MB
result = await trigger_eviction(ctx, target_mb=512)
print(f"Freed {result['memory_freed_mb']:.1f}MB from {len(result['projects_affected'])} projects")
# Output: Freed 523.4MB from 3 projects

# Default (256MB)
result = await trigger_eviction(ctx)
print(f"Freed {result['memory_freed_mb']:.1f}MB")
# Output: Freed 267.8MB
```

**Eviction Logic:**
- Uses priority + recency scoring
- High priority projects evicted last
- Old projects evicted first
- Continues until target reached or no candidates

---

## Tool 4: unload_project()

**Unload a specific project from memory**

```python
result = await unload_project(
    ctx,
    project_id="my-project"  # Required: Project ID to unload
)
```

**Key Fields:**
- `result['success']` - Boolean status
- `result['project_id']` - Project ID unloaded
- `result['memory_freed_mb']` - Memory freed

**Example:**
```python
result = await unload_project(ctx, project_id="my-project")
if result['success']:
    print(f"Freed {result['memory_freed_mb']:.1f}MB from {result['project_id']}")
else:
    print(f"Error: {result['error']}")
```

---

## Common Workflows

### Workflow 1: Monitor and Alert
```python
# Check memory status
status = await get_memory_status(ctx)

# Alert if usage is high
if status['usage_percent'] > 90:
    print(f"WARNING: {status['status'].upper()} - {status['current_mb']:.1f}MB / {status['total_budget_mb']:.1f}MB")

# Show recommendations
if status['recommendations']:
    print("Recommendations:")
    for rec in status['recommendations']:
        print(f"  - {rec}")
```

### Workflow 2: Adjust for Large Project
```python
# Increase budget before loading large project
result = await configure_memory(
    ctx,
    total_budget_mb=6144,   # 6GB
    global_index_mb=1024    # 1GB for global index
)

if result['success']:
    print(f"Budget increased: {result['old_values']['total_budget_mb']}MB → {result['new_values']['total_budget_mb']}MB")
```

### Workflow 3: Emergency Memory Free
```python
# Check if memory is critical
status = await get_memory_status(ctx)

if status['status'] == 'critical':
    print("Critical memory usage - freeing 1GB...")
    result = await trigger_eviction(ctx, target_mb=1024)

    if result['success']:
        print(f"Freed {result['memory_freed_mb']:.1f}MB from {len(result['projects_affected'])} projects")
    else:
        print(f"Eviction failed: {result['message']}")
```

### Workflow 4: Unload Unused Projects
```python
# Get list of loaded projects (would need to implement this)
loaded_projects = ["project-a", "project-b", "project-c"]

# Unload specific project
result = await unload_project(ctx, project_id="project-a")

if result['success']:
    print(f"Unloaded {result['project_id']}: freed {result['memory_freed_mb']:.1f}MB")
```

---

## Parameter Reference

### configure_memory() Limits

| Parameter | Min | Max | Default |
|-----------|-----|-----|---------|
| `total_budget_mb` | 512 | 65536 | 3072 |
| `global_index_mb` | 128 | 8192 | 512 |
| `soft_limit_percent` | 50 | 95 | 80 |
| `prompt_threshold_percent` | 50 | 99 | 93 |
| `hard_limit_percent` | 51 | 100 | 98 |

**Constraints:**
- `global_index_mb` cannot exceed 50% of `total_budget_mb`
- `soft_limit_percent < prompt_threshold_percent < hard_limit_percent`

### trigger_eviction() Limits

| Parameter | Min | Max | Default |
|-----------|-----|-----|---------|
| `target_mb` | 10 | 4096 | 256 |
| `action` | "evict_projects" or "unload_projects" | - | "evict_projects" |

---

## Error Handling

All tools return:
```python
{
    "success": False,
    "error": "Error message",
    "error_type": "ErrorType"
}
```

**Common Error Types:**
- `ValueError` - Invalid parameter (out of range, wrong type)
- `RuntimeError` - Operation failed (unloader not registered)
- `ProjectNotFoundError` - Project not found or not loaded

**Example:**
```python
result = await configure_memory(ctx, total_budget_mb=100)  # Too small
if not result['success']:
    print(f"Error: {result['error']}")
    # Output: Error: total_budget_mb must be between 512 and 65536, got 100
```

---

## Status Levels

| Status | Usage % | Description |
|--------|---------|-------------|
| `healthy` | < 80% | Normal operation |
| `caution` | 80-93% | Elevated usage, monitor |
| `warning` | 93-98% | Approaching limit, consider eviction |
| `critical` | > 98% | Emergency, immediate action needed |

---

## Tips

1. **Monitor regularly** - Call `get_memory_status()` periodically to track usage
2. **Configure proactively** - Adjust limits before loading large projects
3. **Evict carefully** - Start with small `target_mb` and increase if needed
4. **Check errors** - Always check `success` field and handle errors
5. **Use recommendations** - `get_memory_status()` provides action suggestions

---

## File Locations

**Implementation:** `src/leindex/server.py` (lines 8126-8582)
**Dependencies:**
- `src/leindex/memory/tracker.py`
- `src/leindex/memory/status.py`
- `src/leindex/memory/eviction.py`
- `src/leindex/config/global_config.py`

---

## Summary

4 new MCP tools for complete memory management:
1. **Monitor** - `get_memory_status()`
2. **Configure** - `configure_memory()`
3. **Evict** - `trigger_eviction()`
4. **Unload** - `unload_project()`

All tools are production-ready with full validation and error handling.
