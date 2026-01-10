# Edge Cases Fixes - Quick Start Guide

This guide shows how to use the new edge case and thread safety features in your code.

## Table of Contents
1. [Symlink Protection](#1-symlink-protection)
2. [TOCTOU-Safe Caching](#2-toctou-safe-caching)
3. [Input Validation](#3-input-validation)
4. [Parallel Hashing](#4-parallel-hashing)
5. [Monitoring](#5-monitoring)
6. [Testing](#6-testing)

---

## 1. Symlink Protection

### Basic Usage

```python
from leindex.parallel_scanner import ParallelScanner

# Enable symlink protection (default: enabled)
scanner = ParallelScanner(
    max_workers=4,
    enable_symlink_protection=True,
    max_symlink_depth=8
)

# Scan directory - cycles are automatically detected and prevented
results = await scanner.scan('/path/to/project')

# Check statistics
stats = scanner.get_stats()
print(f"Symlink protection: {stats['symlink_protection_enabled']}")
print(f"Visited directories: {stats['symlink_detector']['visited_count']}")
```

### Configuration Options

```python
# High security (shallow symlinks only)
scanner = ParallelScanner(
    max_symlink_depth=5,  # Limit depth
    enable_symlink_protection=True
)

# Performance (disable if no symlinks)
scanner = ParallelScanner(
    enable_symlink_protection=False  # Skip detection
)
```

---

## 2. TOCTOU-Safe Caching

### Basic Usage

```python
from leindex.file_stat_cache import FileStatCache

# Create cache with TOCTOU protection (default: enabled)
cache = FileStatCache(
    toctou_max_retries=3,      # Retry 3 times on transient errors
    toctou_retry_delay=0.001   # 1ms delay with exponential backoff
)

# Use cache - retry logic is automatic
stat_info = cache.get_stat('/path/to/file.py')

# Get cache with hash
file_hash = cache.get_hash('/path/to/file.py')
```

### Configuration Options

```python
# High resilience (more retries)
cache = FileStatCache(
    toctou_max_retries=5,
    toctou_retry_delay=0.001
)

# Fast retries (aggressive)
cache = FileStatCache(
    toctou_max_retries=2,
    toctou_retry_delay=0.0001  # 0.1ms
)
```

### Monitoring TOCTOU Events

```python
# Check cache statistics
stats = cache.get_stats()
print(f"Cache hit rate: {stats['hit_rate']}")
print(f"Cache misses: {stats['misses']}")

# Logs will show TOCTOU retry events
# Look for: "TOCTOU: File disappeared after N retries"
```

---

## 3. Input Validation

### Decorator-Based Validation

```python
from leindex.validation import validate_file_path

@validate_file_path(
    policy=ValidationPolicy.STRICT,
    check_existence=True,
    expect_file=True
)
def process_file(file_path: str):
    # file_path is guaranteed to be:
    # - Validated and sanitized
    # - Absolute path
    # - Existing file
    with open(file_path, 'r') as f:
        return f.read()

# Use the function - validation is automatic
content = process_file('/path/to/file.py')
```

### Validation Policies

```python
from leindex.validation import ValidationPolicy, validate_file_path

# STRICT: Full validation + existence check
@validate_file_path(policy=ValidationPolicy.STRICT, check_existence=True)
def strict_function(file_path: str):
    pass

# STANDARD: Full validation without existence check
@validate_file_path(policy=ValidationPolicy.STANDARD)
def standard_function(file_path: str):
    pass

# PERMISSIVE: Basic format validation only
@validate_file_path(policy=ValidationPolicy.PERMISSIVE)
def permissive_function(file_path: str):
    pass
```

### Multiple Path Validation

```python
from leindex.validation import validate_multiple_paths

@validate_multiple_paths(
    ['source', 'destination'],
    check_existence=True,
    expect_file=True
)
def copy_files(source: str, destination: str):
    # Both paths are validated
    shutil.copy(source, destination)
```

### Quick Validation Functions

```python
from leindex.validation import validate_absolute_path, validate_existing_path

# Quick absolute path validation (no existence check)
validated = validate_absolute_path(user_input)

# Quick existing path validation
validated = validate_existing_path(user_input, expect_file=True)
```

---

## 4. Parallel Hashing

### Batch Hash Computation

```python
from leindex.parallel_hash import ParallelHashComputer

# Create parallel hash computer
computer = ParallelHashComputer(
    max_workers=4,  # Use 4 worker threads
    chunk_size=4 * 1024 * 1024  # 4MB chunks
)

# Compute hashes for multiple files in parallel
file_list = [
    '/path/to/file1.py',
    '/path/to/file2.py',
    '/path/to/file3.py'
]

results = computer.compute_hashes_batch(file_list)

# Process results
for result in results:
    if result.hash:
        print(f"{result.file_path}: {result.hash[:16]}...")
        print(f"  Size: {result.size}, Time: {result.computation_time:.3f}s")
    else:
        print(f"{result.file_path}: ERROR - {result.error}")
```

### Incremental Indexing

```python
from leindex.parallel_hash import IncrementalIndexer

# Create incremental indexer
indexer = IncrementalIndexer(
    max_workers=4,
    cache=existing_cache  # Optional: reuse existing cache
)

# Scan for changes
file_paths = list_all_files('/path/to/project')
new, changed, unchanged = indexer.scan_for_changes(file_paths)

print(f"New files: {len(new)}")
print(f"Changed files: {len(changed)}")
print(f"Unchanged files: {len(unchanged)}")

# Update index (only hashes new/changed files)
results = indexer.update_index(file_paths)

# Get metadata
metadata = indexer.get_all_metadata()
```

### Progress Tracking

```python
from leindex.parallel_hash import ParallelHashComputer

def progress_callback(completed: int, total: int):
    percent = (completed / total) * 100
    print(f"Progress: {completed}/{total} ({percent:.1f}%)")

computer = ParallelHashComputer(
    max_workers=4,
    progress_callback=progress_callback
)

results = computer.compute_hashes_batch(large_file_list)
```

---

## 5. Monitoring

### Metrics Collection

```python
from leindex.monitoring import get_metrics_registry

# Get global metrics registry
metrics = get_metrics_registry()

# Create metrics
files_indexed = metrics.counter('files_indexed', 'Files processed')
index_latency = metrics.histogram('index_latency_seconds', 'Indexing time')
queue_size = metrics.gauge('queue_size', 'Queue size')

# Use metrics
files_indexed.inc()
queue_size.set(10)

# Time operations
with index_latency.time():
    # Do indexing work
    process_file(file_path)

# Get all metrics
all_metrics = metrics.get_all_metrics()
for name, metric in all_metrics.items():
    print(f"{name}: {metric}")
```

### Health Checks

```python
from leindex.monitoring import get_health_checker

# Get global health checker
health = get_health_checker()

# Define health check function
def check_disk_space():
    import shutil
    usage = shutil.disk_usage('/')
    free_percent = (usage.free / usage.total) * 100

    if free_percent < 10:
        return {
            'healthy': False,
            'message': f'Low disk space: {free_percent:.1f}% free'
        }

    return {
        'healthy': True,
        'message': f'Disk space OK: {free_percent:.1f}% free'
    }

# Register health check
health.register_check('disk_space', check_disk_space)

# Run health checks
result = health.run_checks(force=True)
if result['healthy']:
    print("All systems healthy")
else:
    print("Health issues detected:")
    for check_name, check_result in result['checks'].items():
        if not check_result['healthy']:
            print(f"  {check_name}: {check_result['message']}")

# Quick health check
if health.is_healthy():
    print("System is healthy")
```

### Performance Monitoring

```python
from leindex.monitoring import get_performance_monitor

# Get global performance monitor
perf = get_performance_monitor()

# Record indexing operation
perf.record_index(
    latency_seconds=0.123,
    success=True
)

# Update queue size
perf.queue_size.set(5)

# Update memory usage
import psutil
perf.memory_usage_mb.set(psutil.virtual_memory().used / (1024 * 1024))

# Get performance summary
summary = perf.get_summary()
print(f"Uptime: {summary['uptime_seconds']:.0f}s")
print(f"Files indexed: {summary['files_indexed']}")
print(f"Throughput: {summary['throughput_files_per_sec']} files/sec")
print(f"Cache hit rate: {summary['cache_hit_rate']}")
print(f"Error rate: {summary['error_rate']}")
```

---

## 6. Testing

### Running Concurrent Tests

```bash
# Run all concurrent indexing tests
pytest src/leindex/core_engine/test_concurrency.py -v

# Run specific test
pytest src/leindex/core_engine/test_concurrency.py::TestConcurrentIndexing::test_concurrent_cache_access -v

# Run with coverage
pytest src/leindex/core_engine/test_concurrency.py --cov=src/leindex --cov-report=html

# Run specific test class
pytest src/leindex/core_engine/test_concurrency.py::TestSymlinkCycleDetection -v
```

### Writing Your Own Tests

```python
import pytest
from leindex.file_stat_cache import FileStatCache
from leindex.validation import validate_file_path

def test_my_concurrent_operation():
    """Test my concurrent operation."""
    cache = FileStatCache()

    # Test concurrent access
    def worker(thread_id):
        for i in range(100):
            stat_info = cache.get_stat(f'/path/to/file_{i}.txt')
            assert stat_info is not None

    import threading
    threads = [threading.Thread(target=worker, args=(i,)) for i in range(10)]

    for t in threads:
        t.start()
    for t in threads:
        t.join()

    # Verify cache state
    stats = cache.get_stats()
    assert stats['total_lookups'] == 1000
```

---

## Complete Example

Here's a complete example showing all features:

```python
import asyncio
from leindex.parallel_scanner import ParallelScanner
from leindex.file_stat_cache import FileStatCache
from leindex.parallel_hash import ParallelHashComputer, IncrementalIndexer
from leindex.validation import validate_file_path, ValidationPolicy
from leindex.monitoring import (
    get_metrics_registry,
    get_health_checker,
    get_performance_monitor
)

async def main():
    # Initialize components
    scanner = ParallelScanner(
        max_workers=4,
        enable_symlink_protection=True,
        max_symlink_depth=8
    )

    cache = FileStatCache(
        toctou_max_retries=3,
        toctou_retry_delay=0.001
    )

    hash_computer = ParallelHashComputer(max_workers=4)
    indexer = IncrementalIndexer(max_workers=4, cache=cache)

    metrics = get_metrics_registry()
    health = get_health_checker()
    perf = get_performance_monitor()

    @validate_file_path(
        policy=ValidationPolicy.STRICT,
        check_existence=True,
        expect_dir=True
    )
    async def index_directory(directory: str):
        """Index a directory with all safety features."""

        # Scan directory (with symlink protection)
        results = await scanner.scan(directory)
        all_files = []
        for root, dirs, files in results:
            for filename in files:
                all_files.append(f"{root}/{filename}")

        # Scan for changes
        new, changed, unchanged = indexer.scan_for_changes(all_files)
        perf.queue_size.set(len(new) + len(changed))

        # Compute hashes for changed files (in parallel)
        if new or changed:
            results = indexer.update_index(all_files)

            # Record metrics
            for result in results:
                if result.hash:
                    perf.record_index(
                        latency_seconds=result.computation_time,
                        success=True
                    )
                    metrics.counter('hashes_computed').inc()
                else:
                    perf.record_index(0, success=False)
                    metrics.counter('hash_errors').inc()

        # Get performance summary
        summary = perf.get_summary()
        print(f"Indexing complete:")
        print(f"  Files processed: {summary['files_indexed']}")
        print(f"  Throughput: {summary['throughput_files_per_sec']} files/sec")
        print(f"  Cache hit rate: {summary['cache_hit_rate']}")

        return len(new) + len(changed)

    # Run indexing
    changed_count = await index_directory('/path/to/project')

    # Health check
    if health.is_healthy():
        print("System healthy")

    print(f"Total files changed: {changed_count}")

if __name__ == '__main__':
    asyncio.run(main())
```

---

## Troubleshooting

### Common Issues

**Issue: Symlink depth limit reached**
```
Solution: Increase max_symlink_depth
scanner = ParallelScanner(max_symlink_depth=16)
```

**Issue: High TOCTOU retry rate**
```
Solution: Increase retry count or check for file system issues
cache = FileStatCache(toctou_max_retries=5)
```

**Issue: Validation errors**
```
Solution: Check validation policy and file existence
@validate_file_path(policy=ValidationPolicy.PERMISSIVE)
```

**Issue: Slow hashing**
```
Solution: Increase workers or decrease chunk size
computer = ParallelHashComputer(max_workers=8, chunk_size=2*1024*1024)
```

### Getting Help

- Check logs: Look for "TOCTOU", "symlink", "validation" keywords
- Run tests: `pytest src/leindex/core_engine/test_concurrency.py -v`
- Check metrics: Use monitoring to identify bottlenecks
- Review documentation: See `EDGE_CASE_FIXES_SUMMARY.md` for details

---

**Need more help?**
- Review the complete summary: `EDGE_CASE_FIXES_SUMMARY.md`
- Check the test suite: `src/leindex/core_engine/test_concurrency.py`
- Enable debug logging for detailed diagnostics
