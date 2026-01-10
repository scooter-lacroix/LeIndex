# Project Configuration Overrides - Quick Reference

## Configuration File Location

```
<project_root>/.leindex_data/config.yaml
```

## Quick Start

### 1. Create Configuration File

```bash
# In your project root
mkdir -p .leindex_data
cat > .leindex_data/config.yaml << EOF
memory:
  estimated_mb: 512
  priority: high
EOF
```

### 2. Use in Code

```python
from leindex.project_settings import ProjectSettings

settings = ProjectSettings("/path/to/project")
mem_config = settings.get_memory_config()

print(f"Memory: {mem_config['estimated_mb']}MB")
print(f"Priority: {mem_config['priority']}")
```

## Configuration Options

### Memory Section

| Field | Type | Default | Max | Description |
|-------|------|---------|-----|-------------|
| `estimated_mb` | int | 256 (global) | 512 | Memory hint in MB |
| `priority` | str | "normal" | - | Eviction priority |

### Priority Values

| Priority | Score | Eviction Order | Use Case |
|----------|-------|----------------|----------|
| `high` | 2.0 | Last (kept longest) | Active development |
| `normal` | 1.0 | Middle | Typical projects |
| `low` | 0.5 | First (evicted first) | Reference code |

## Example Configurations

### Large ML Project
```yaml
memory:
  estimated_mb: 512  # 2x default
  priority: high     # Keep in memory
```

### Typical Project
```yaml
memory:
  estimated_mb: 256  # Use default
  priority: normal   # Default priority
```

### Small Utility
```yaml
memory:
  estimated_mb: 64   # Minimal
  priority: low      # OK to evict
```

## API Reference

### ProjectSettings Integration

```python
from leindex.project_settings import ProjectSettings

settings = ProjectSettings("/path/to/project")

# Get memory configuration
mem_config = settings.get_memory_config()
# Returns: {
#   'estimated_mb': 512,
#   'priority': 'high',
#   'priority_score': 2.0,
#   'is_overridden': True,
#   'max_override_mb': 512
# }
```

### Direct API

```python
from leindex.project_config import (
    ProjectConfigManager,
    ProjectConfig,
    ProjectMemoryConfig,
    get_effective_memory_config,
)

# Quick effective config
effective = get_effective_memory_config("/path/to/project")

# Full manager API
manager = ProjectConfigManager("/path/to/project")
config = manager.get_config()
manager.save_config(config)
manager.delete_config()
```

## Validation Rules

✅ **Valid:**
- `estimated_mb`: 0 to 512
- `priority`: "high", "normal", "low"

❌ **Invalid:**
- `estimated_mb` < 0
- `estimated_mb` > 512
- `priority` not in ["high", "normal", "low"]

## Warnings

The system warns when:
1. `estimated_mb` > global default (256MB)
2. `estimated_mb` > 460MB (90% of max)

## Important Notes

⚠️ **Configuration values are hints, not reservations**
- Actual allocation may vary
- Memory manager adjusts based on system conditions
- Priorities are suggestions for eviction order

## Files

- Implementation: `src/leindex/project_config.py`
- Tests: `tests/unit/test_project_config.py` (50 tests)
- Documentation: `docs/PROJECT_CONFIG_OVERRIDES.md`
- Demo: `examples/project_config_demo.py`

## See Also

- Full documentation: `docs/PROJECT_CONFIG_OVERRIDES.md`
- Memory management: `docs/MEMORY_MANAGEMENT.md`
- Global configuration: `docs/GLOBAL_CONFIG.md`
