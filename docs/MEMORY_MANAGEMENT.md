# Memory Management Guide

## Overview

LeIndex v2.0 includes advanced memory management with automatic cleanup, priority-based eviction, and zero-downtime configuration reload. This guide covers the memory management architecture, configuration, and best practices.

### Key Features

- **RSS Memory Tracking**: Actual memory usage (not just allocations)
- **Hierarchical Configuration**: Global defaults + per-project overrides
- **Memory Threshold Actions**: Automatic cleanup at 80%, 93%, 98%
- **Priority-Based Eviction**: Intelligent freeing of cached data
- **Zero-Downtime Reload**: Update config without restarting
- **Graceful Shutdown**: Persist cache state for fast recovery
- **Continuous Monitoring**: Background tracking with alerts

## Architecture

### Memory Threshold System

```
┌─────────────────────────────────────────────────────────────┐
│                    Memory Budget: 3072 MB                    │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│  98% ████████████████████████████████████████ EMERGENCY      │
│  │                                                           │
│  │ Action: Emergency eviction of low-priority data           │
│  │ Target: Free 15%+ memory immediately                     │
│  │ Priority: CRITICAL                                        │
│  └─> Eviction: All non-essential caches                     │
│                                                               │
│  93% ████████████████████████████████████ HARD LIMIT         │
│  │                                                           │
│  │ Action: Spill cached data to disk                        │
│  │ Target: Free 10% memory                                   │
│  │ Priority: HIGH                                            │
│  └─> Spill: Query cache, loaded files                       │
│                                                               │
│  80% ████████████████████████████ SOFT LIMIT                 │
│  │                                                           │
│  │ Action: Trigger cleanup & garbage collection             │
│  │ Target: Free 5% memory                                    │
│  │ Priority: MEDIUM                                          │
│  └─> Cleanup: Old cache entries, GC triggers                │
│                                                               │
│  Current Usage: ████████████████████ 65% (1995 MB)          │
│                                                               │
└─────────────────────────────────────────────────────────────┘
```

### Memory Breakdown

```
Total Memory: 3072 MB (RSS)
│
├─ Heap Memory: 1200 MB (39%)
│  ├─ Loaded Content: 480 MB (16%)
│  ├─ Query Cache: 240 MB (8%)
│  ├─ Index Structures: 360 MB (12%)
│  └─ Other: 120 MB (4%)
│
├─ Stack Memory: 50 MB (2%)
│
├─ Code Segment: 100 MB (3%)
│
└─ Other RSS: 1722 MB (56%)
   ├─ LEANN Index: 800 MB
   ├─ Tantivy Index: 600 MB
   └─ Other: 322 MB
```

### Component Overview

#### Memory Tracker

**Module**: `leindex.memory.tracker`

Continuous memory monitoring with RSS tracking:

```python
from leindex.memory import MemoryTracker, get_current_usage_mb

# Get global tracker
tracker = MemoryTracker.get_global_tracker()

# Get current memory usage
current_mb = get_current_usage_mb()
print(f"Current memory: {current_mb:.1f} MB")

# Check memory budget
within_budget = tracker.check_memory_budget(max_mb=3072)
if not within_budget:
    print("Memory budget exceeded!")

# Get memory statistics
stats = tracker.get_stats()
print(f"Peak memory: {stats['peak_mb']:.1f} MB")
print(f"Average memory: {stats['average_mb']:.1f} MB")
```

**Features**:
- Real-time RSS tracking
- Memory budget enforcement
- Peak and average statistics
- Per-project tracking

#### Threshold Checker

**Module**: `leindex.memory.thresholds`

Multi-level threshold checking with automatic actions:

```python
from leindex.memory import check_thresholds, ThresholdLevel

# Check all thresholds
warnings = check_thresholds(current_memory_mb=2500, budget_mb=3072)

for warning in warnings:
    print(f"Level: {warning.level}")
    print(f"Message: {warning.message}")
    print(f"Action: {warning.suggested_action}")

    # Execute automatic action
    if warning.level == ThresholdLevel.SOFT:
        trigger_cleanup()
    elif warning.level == ThresholdLevel.HARD:
        spill_to_disk()
    elif warning.level == ThresholdLevel.EMERGENCY:
        emergency_eviction()
```

**Threshold Levels**:
- **SOFT (80%)**: Trigger cleanup and GC
- **HARD (93%)**: Spill cached data to disk
- **EMERGENCY (98%)**: Emergency eviction

#### Action Queue

**Module**: `leindex.memory.actions`

Queued execution of memory management actions:

```python
from leindex.memory import enqueue_action, execute_all_actions, ActionType

# Enqueue cleanup action
cleanup_action = enqueue_action(
    action_type=ActionType.CLEANUP,
    description="Clean up old cache entries",
    priority=5
)

# Enqueue spill action
spill_action = enqueue_action(
    action_type=ActionType.SPILL_TO_DISK,
    description="Spill query cache to disk",
    priority=7
)

# Execute all queued actions
results = execute_all_actions()

for result in results:
    print(f"{result.action_type}: {result.status}")
    if result.error:
        print(f"  Error: {result.error}")
```

#### Eviction Manager

**Module**: `leindex.memory.ejection`

Priority-based eviction of cached data:

```python
from leindex.memory import emergency_eviction, ProjectPriority, EvictionResult

# Perform emergency eviction
result: EvictionResult = emergency_eviction(
    target_mb=500,  # Free 500 MB
    priority_threshold=ProjectPriority.MEDIUM
)

print(f"Evicted: {result.evicted_count_mb} MB")
print(f"Projects affected: {len(result.evicted_projects)}")

for project_id in result.evicted_projects:
    print(f"  - {project_id}")
```

**Eviction Priority**:
1. **LOW**: Inactive projects, old cache entries
2. **MEDIUM**: Active projects, recent cache entries
3. **HIGH**: Current project, frequently accessed data
4. **CRITICAL**: Essential data, never evict

## Configuration

### Global Memory Configuration

```yaml
# ~/.leindex/config.yaml
memory:
  # Total memory budget (in MB)
  total_budget_mb: 3072  # 3 GB

  # Threshold percentages (of total budget)
  soft_limit_percent: 0.80    # 80% = 2457 MB
  hard_limit_percent: 0.93    # 93% = 2857 MB
  emergency_percent: 0.98     # 98% = 3010 MB

  # Maximum loaded files (across all projects)
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
    interval_seconds: 30  # Check every 30 seconds
    alert_on_soft_limit: true
    alert_on_hard_limit: true
    alert_on_emergency: true

  # Project defaults
  project_defaults:
    max_loaded_files: 100
    max_cached_queries: 50
    priority: "MEDIUM"
```

### Per-Project Overrides

```yaml
# ~/.leindex/projects/my-large-project.yaml
memory:
  # Override defaults for large project
  max_loaded_files: 500  # Allow more files
  max_cached_queries: 200
  priority: "HIGH"  # Higher eviction priority
```

```yaml
# ~/.leindex/projects/temp-analysis.yaml
memory:
  # Lower priority for temporary project
  max_loaded_files: 50
  max_cached_queries: 25
  priority: "LOW"  # Evict first
```

### Environment Variables

Override configuration with environment variables:

```bash
# Set memory budget
export LEINDEX_MEMORY_TOTAL_BUDGET_MB=4096

# Set thresholds
export LEINDEX_MEMORY_SOFT_LIMIT_PERCENT=0.75
export LEINDEX_MEMORY_HARD_LIMIT_PERCENT=0.90

# Set monitoring interval
export LEINDEX_MEMORY_MONITORING_INTERVAL_SECONDS=60
```

**Priority**: Environment variables > Project config > Global config

## Usage

### Python API

#### Basic Memory Monitoring

```python
from leindex.memory import MemoryManager, MemoryStatus

# Create memory manager
manager = MemoryManager()

# Get current memory status
status: MemoryStatus = manager.get_status()
print(f"Current: {status.current_mb:.1f} MB")
print(f"Peak: {status.peak_mb:.1f} MB")
print(f"Heap: {status.heap_size_mb:.1f} MB")
print(f"Loaded Files: {status.loaded_files}")
print(f"Cached Queries: {status.cached_queries}")
print(f"Soft Limit Exceeded: {status.soft_limit_exceeded}")
print(f"Hard Limit Exceeded: {status.hard_limit_exceeded}")
```

#### Memory Breakdown

```python
from leindex.memory import MemoryManager, MemoryBreakdown

manager = MemoryManager()

# Get detailed memory breakdown
breakdown: MemoryBreakdown = manager.get_breakdown()
print(f"Total: {breakdown.total_mb:.1f} MB")
print(f"Process RSS: {breakdown.process_rss_mb:.1f} MB")
print(f"Heap: {breakdown.heap_mb:.1f} MB")
print(f"Loaded Content: {breakdown.loaded_content_mb:.1f} MB")
print(f"Query Cache: {breakdown.query_cache_mb:.1f} MB")
print(f"Indexes: {breakdown.indexes_mb:.1f} MB")
print(f"Other: {breakdown.other_mb:.1f} MB")
```

#### Threshold Checking

```python
from leindex.memory import check_thresholds, ThresholdLevel

# Check thresholds with current memory
warnings = check_thresholds(
    current_memory_mb=2800,
    budget_mb=3072
)

for warning in warnings:
    if warning.level == ThresholdLevel.SOFT:
        print(f"SOFT: {warning.message}")
        print(f"  Action: {warning.suggested_action}")
        # Trigger cleanup
        manager.cleanup()
    elif warning.level == ThresholdLevel.HARD:
        print(f"HARD: {warning.message}")
        print(f"  Action: {warning.suggested_action}")
        # Spill to disk
        manager.spill_to_disk("query_cache", cached_queries)
    elif warning.level == ThresholdLevel.EMERGENCY:
        print(f"EMERGENCY: {warning.message}")
        print(f"  Action: {warning.suggested_action}")
        # Emergency eviction
        from leindex.memory import emergency_eviction
        emergency_eviction(target_mb=500)
```

#### Continuous Monitoring

```python
from leindex.memory import MemoryManager

manager = MemoryManager()

# Start continuous monitoring
manager.start_monitoring(interval_seconds=30)

# Register callbacks
def on_soft_limit():
    print("Soft limit exceeded, triggering cleanup")
    manager.cleanup()

def on_hard_limit():
    print("Hard limit exceeded, spilling to disk")
    manager.spill_to_disk("query_cache", get_cached_queries())

def on_emergency():
    print("Emergency level, evacuating memory")
    from leindex.memory import emergency_eviction
    emergency_eviction(target_mb=1000)

manager.register_limit_exceeded_callback(on_soft_limit)
manager.register_limit_exceeded_callback(on_hard_limit)
manager.register_limit_exceeded_callback(on_emergency)

# ... application runs ...

# Stop monitoring
manager.stop_monitoring()
```

#### Manual Cleanup

```python
from leindex.memory import MemoryManager

manager = MemoryManager()

# Trigger manual cleanup
success = manager.cleanup()
if success:
    print("Cleanup completed successfully")

# Spill specific data to disk
data = get_large_cached_object()
success = manager.spill_to_disk("large_object", data)
if success:
    print("Data spilled to disk")

# Load spilled data back
loaded_data = manager.load_from_disk("large_object")
if loaded_data:
    print("Data loaded from disk")
```

### MCP Tools

#### get_diagnostics (Type: "memory")

Get comprehensive memory diagnostics:

```json
{
  "name": "get_diagnostics",
  "arguments": {
    "type": "memory"
  }
}
```

**Response**:
```json
{
  "timestamp": 1234567890.0,
  "process_memory_mb": 2500.5,
  "peak_memory_mb": 2800.2,
  "heap_size_mb": 1200.3,
  "gc_objects": 45000,
  "active_threads": 8,
  "loaded_files": 850,
  "cached_queries": 320,
  "soft_limit_exceeded": false,
  "hard_limit_exceeded": false,
  "memory_breakdown": {
    "total_mb": 2500.5,
    "process_rss_mb": 2500.5,
    "heap_mb": 1200.3,
    "loaded_content_mb": 480.2,
    "query_cache_mb": 240.1,
    "indexes_mb": 360.4,
    "other_mb": 119.6
  }
}
```

#### manage_memory (Action: "cleanup")

Trigger manual memory cleanup:

```json
{
  "name": "manage_memory",
  "arguments": {
    "action": "cleanup"
  }
}
```

**Response**:
```json
{
  "success": true,
  "freed_mb": 125.3,
  "duration_seconds": 0.5,
  "cleanup_details": {
    "gc_objects_collected": 5000,
    "cache_entries_cleared": 150,
    "files_unloaded": 50
  }
}
```

#### manage_memory (Action: "configure")

Update memory configuration:

```json
{
  "name": "manage_memory",
  "arguments": {
    "action": "configure",
    "soft_limit_mb": 2457,
    "hard_limit_mb": 2857
  }
}
```

**Response**:
```json
{
  "success": true,
  "previous_limits": {
    "soft_limit_mb": 2048,
    "hard_limit_mb": 2816
  },
  "new_limits": {
    "soft_limit_mb": 2457,
    "hard_limit_mb": 2857
  }
}
```

## Zero-Downtime Configuration Reload

### Signal-Based Reload

Reload configuration without restarting using Unix signals:

```bash
# Send SIGHUP to reload configuration
kill -HUP $(cat ~/.leindex/leindex.pid)
```

### Programmatic Reload

```python
from leindex.config import reload_config, ReloadResult

# Reload configuration
result: ReloadResult = reload_config()

if result.success:
    print("Configuration reloaded successfully")
    print(f"Reloaded at: {result.reloaded_at}")
else:
    print(f"Reload failed: {result.error}")
```

### Configuration Observers

Register observers to be notified of config changes:

```python
from leindex.config import ConfigObserver, get_reload_manager

class MemoryConfigObserver(ConfigObserver):
    def on_config_reloaded(self, event):
        print(f"Config reloaded at {event.timestamp}")
        print(f"Memory budget: {event.new_config.memory.total_budget_mb} MB")

        # Update memory limits
        update_memory_limits(event.new_config.memory)

# Register observer
manager = get_reload_manager()
manager.register_observer(MemoryConfigObserver())
```

## Graceful Shutdown

### Persist Cache State

Save cache state for fast recovery:

```python
from leindex.memory import MemoryManager

manager = MemoryManager()

# Trigger graceful shutdown
manager.graceful_shutdown()

# This will:
# 1. Stop all monitoring
# 2. Persist cache state to disk
# 3. Close all file handles
# 4. Cleanup resources
```

### Cache Persistence

Cache state is persisted to `~/.leindex/cache_state.json`:

```json
{
  "version": "1.0",
  "timestamp": 1234567890.0,
  "projects": {
    "/path/to/project": {
      "loaded_files": ["file1.py", "file2.py"],
      "cached_queries": ["query1", "query2"],
      "last_accessed": 1234567880.0
    }
  }
}
```

### Fast Recovery

On restart, cache state is automatically restored:

```python
from leindex.memory import MemoryManager

manager = MemoryManager()

# Restore cache state from disk
manager.restore_cache_state()

# Previously cached data is available immediately
```

## Best Practices

### 1. Set Appropriate Memory Budgets

```yaml
# For development machines (16GB RAM)
memory:
  total_budget_mb: 3072  # 3 GB (30% of total RAM)

# For production servers (64GB RAM)
memory:
  total_budget_mb: 16384  # 16 GB (25% of total RAM)

# For resource-constrained environments (4GB RAM)
memory:
  total_budget_mb: 1024  # 1 GB (25% of total RAM)
```

### 2. Use Per-Project Priorities

```yaml
# High-priority project (active development)
projects:
  core-api:
    memory:
      priority: "HIGH"
      max_loaded_files: 500

# Low-priority project (reference only)
projects:
  legacy-code:
    memory:
      priority: "LOW"
      max_loaded_files: 50
```

### 3. Enable Spill-to-Disk for Large Projects

```yaml
memory:
  spill:
    enabled: true
    directory: "~/.leindex/spill"
    max_spill_size_mb: 2000  # Allow up to 2GB spill
```

### 4. Monitor Memory Usage Regularly

```python
from leindex.memory import MemoryManager

manager = MemoryManager()
manager.start_monitoring(interval_seconds=30)

# Log memory status periodically
import logging
logging.basicConfig(level=logging.INFO)

def log_memory_status():
    status = manager.get_status()
    logging.info(f"Memory: {status.current_mb:.1f} MB "
                f"(Peak: {status.peak_mb:.1f} MB)")

# Schedule periodic logging
import schedule
schedule.every(5).minutes.do(log_memory_status)
```

### 5. Test Memory Limits Before Deployment

```python
from leindex.memory import MemoryManager, MemoryLimits

# Test with different memory budgets
test_budgets = [1024, 2048, 4096, 8192]

for budget in test_budgets:
    manager = MemoryManager(MemoryLimits(
        soft_limit_mb=int(budget * 0.8),
        hard_limit_mb=int(budget * 0.93)
    ))

    # Simulate workload
    simulate_workload(manager)

    # Check if limits are respected
    status = manager.get_status()
    assert status.current_mb <= budget, f"Budget {budget} MB exceeded"
```

## Troubleshooting

### Memory Leaks

**Problem**: Memory usage continuously increases

**Diagnosis**:
```python
from leindex.memory import MemoryManager

manager = MemoryManager()

# Take snapshots over time
snapshots = []
for i in range(10):
    snapshot = manager.take_snapshot()
    snapshots.append(snapshot)
    time.sleep(60)  # Wait 1 minute

# Analyze growth
for i in range(1, len(snapshots)):
    growth = snapshots[i].process_memory_mb - snapshots[i-1].process_memory_mb
    print(f"Interval {i}: {growth:.1f} MB growth")
```

**Solution**:
1. Check for unclosed file handles
2. Verify cache eviction is working
3. Reduce cache sizes
4. Enable more aggressive GC

### High RSS vs Heap

**Problem**: RSS much higher than heap size

**Diagnosis**:
```python
from leindex.memory import MemoryManager, MemoryBreakdown

manager = MemoryManager()
breakdown = manager.get_breakdown()

rss_heap_ratio = breakdown.process_rss_mb / breakdown.heap_mb
print(f"RSS/Heap ratio: {rss_heap_ratio:.2f}")

if rss_heap_ratio > 2.0:
    print("Warning: RSS much higher than heap")
    print("This may indicate:")
    print("- Large index structures (LEANN, Tantivy)")
    print("- Memory fragmentation")
    print("- External library allocations")
```

**Solution**:
1. Index structures are typically in RSS but not heap
2. This is normal for search-heavy workloads
3. Consider reducing index size if problematic

### Frequent Evictions

**Problem**: Data being evicted too frequently

**Diagnosis**:
```python
from leindex.memory import EvictionManager

eviction_manager = EvictionManager()
history = eviction_manager.get_history()

for eviction in history[-10:]:
    print(f"Timestamp: {eviction['timestamp']}")
    print(f"Evicted: {eviction['evicted_count']} items")
    print(f"Candidates: {eviction['total_candidates']} items")
```

**Solution**:
1. Increase memory budget
2. Reduce cache sizes
3. Lower project priorities
4. Enable spill-to-disk

### Slow Spill-to-Disk

**Problem**: Spilling to disk takes too long

**Diagnosis**:
```python
import time
from leindex.memory import MemoryManager

manager = MemoryManager()

data = get_large_cached_object()

start = time.time()
success = manager.spill_to_disk("test_data", data)
duration = time.time() - start

print(f"Spill duration: {duration:.2f} seconds")

if duration > 5.0:
    print("Warning: Spill is slow")
    print("Possible causes:")
    print("- Slow disk (HDD vs SSD)")
    print("- Network filesystem (NFS)")
    print("- Large object size")
```

**Solution**:
1. Use fast storage (SSD)
2. Avoid network filesystems
3. Split large objects into chunks
4. Compress data before spilling

## Performance Characteristics

### Memory Overhead

| Component | Overhead | Notes |
|-----------|----------|-------|
| Base Indexer | ~200 MB | LEANN + Tantivy indexes |
| Per Loaded File | ~0.5 MB | Average file content |
| Per Cached Query | ~0.1 MB | Query results + metadata |
| Memory Tracker | ~5 MB | Monitoring data structures |
| Total Overhead | ~10% | Of total memory budget |

### Cleanup Performance

| Operation | Time | Memory Freed |
|-----------|------|--------------|
| Garbage Collection | 0.1-0.5s | 50-200 MB |
| Cache Eviction | 0.5-2s | 200-500 MB |
| Spill to Disk | 1-5s | 500-1000 MB |
| Emergency Eviction | 2-10s | 1000-2000 MB |

### Monitoring Overhead

| Interval | CPU Overhead | Memory Overhead |
|----------|--------------|-----------------|
| 10s | ~2% | ~5 MB |
| 30s | ~1% | ~5 MB |
| 60s | ~0.5% | ~5 MB |

## API Reference

### MemoryManager

```python
class MemoryManager:
    """Main memory management interface."""

    def __init__(self, limits: Optional[MemoryLimits] = None):
        """Initialize with optional memory limits."""

    def take_snapshot(self, loaded_files: int = 0,
                     cached_queries: int = 0) -> MemorySnapshot:
        """Take a memory snapshot."""

    def get_status(self) -> MemoryStatus:
        """Get current memory status."""

    def get_breakdown(self) -> MemoryBreakdown:
        """Get detailed memory breakdown."""

    def check_limits(self, snapshot: Optional[MemorySnapshot] = None) -> Dict[str, bool]:
        """Check if memory limits are exceeded."""

    def enforce_limits(self, snapshot: Optional[MemorySnapshot] = None) -> Dict[str, Any]:
        """Enforce memory limits and trigger appropriate actions."""

    def cleanup(self) -> bool:
        """Trigger cleanup to reduce memory usage."""

    def spill_to_disk(self, key: str, data: Any) -> bool:
        """Spill data to disk."""

    def load_from_disk(self, key: str) -> Optional[Any]:
        """Load spilled data from disk."""

    def start_monitoring(self, interval: float = 30.0):
        """Start continuous memory monitoring."""

    def stop_monitoring(self):
        """Stop continuous memory monitoring."""

    def register_cleanup_callback(self, callback: Callable):
        """Register a cleanup callback."""

    def register_spill_callback(self, callback: Callable):
        """Register a spill callback."""

    def register_limit_exceeded_callback(self, callback: Callable):
        """Register a limit exceeded callback."""
```

### Threshold Functions

```python
def check_thresholds(
    current_memory_mb: float,
    budget_mb: float,
    loaded_files: int = 0,
    cached_queries: int = 0
) -> List[MemoryWarning]:
    """Check all thresholds and return warnings."""
```

### Eviction Functions

```python
def emergency_eviction(
    target_mb: float,
    priority_threshold: ProjectPriority = ProjectPriority.LOW,
    excluded_projects: Optional[List[str]] = None
) -> EvictionResult:
    """Perform emergency eviction to free memory."""
```

## See Also

- [docs/GLOBAL_INDEX.md](GLOBAL_INDEX.md) - Global index memory usage
- [docs/CONFIGURATION.md](CONFIGURATION.md) - Configuration reference
- [examples/memory_configuration.py](../examples/memory_configuration.py) - Configuration examples
