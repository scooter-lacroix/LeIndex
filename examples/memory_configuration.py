#!/usr/bin/env python3
"""
Memory Configuration Examples for LeIndex v2.0

This example demonstrates how to configure and use the advanced memory
management system in LeIndex v2.0.

Usage:
    python memory_configuration.py
"""

import sys
import time
from pathlib import Path
from typing import Dict, Any

# Add src to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from leindex.memory import (
    MemoryManager,
    MemoryStatus,
    MemoryBreakdown,
    ThresholdManager,
    ThresholdLevel,
    MemoryActionManager,
    MemoryActionType,
    EvictionManager,
    ProjectPriority,
    check_thresholds,
    get_current_usage_mb,
)
from leindex.config import (
    GlobalConfigManager,
    GlobalConfig,
    MemoryConfig,
    first_time_setup,
    reload_config,
)


def example_basic_memory_monitoring():
    """Example 1: Basic memory monitoring."""
    print("=" * 60)
    print("Example 1: Basic Memory Monitoring")
    print("=" * 60)

    # Create memory manager with default limits
    manager = MemoryManager()

    # Get current memory status
    status: MemoryStatus = manager.get_status()

    print(f"\nCurrent Memory Status:")
    print(f"  Current: {status.current_mb:.1f} MB")
    print(f"  Peak: {status.peak_mb:.1f} MB")
    print(f"  Heap: {status.heap_size_mb:.1f} MB")
    print(f"  GC Objects: {status.gc_objects:,}")
    print(f"  Active Threads: {status.active_threads}")
    print(f"  Loaded Files: {status.loaded_files}")
    print(f"  Cached Queries: {status.cached_queries}")
    print(f"  Soft Limit Exceeded: {status.soft_limit_exceeded}")
    print(f"  Hard Limit Exceeded: {status.hard_limit_exceeded}")


def example_memory_breakdown():
    """Example 2: Detailed memory breakdown."""
    print("\n" + "=" * 60)
    print("Example 2: Memory Breakdown")
    print("=" * 60)

    manager = MemoryManager()
    breakdown: MemoryBreakdown = manager.get_breakdown()

    print(f"\nMemory Breakdown:")
    print(f"  Total: {breakdown.total_mb:.1f} MB")
    print(f"  Process RSS: {breakdown.process_rss_mb:.1f} MB")
    print(f"  Heap: {breakdown.heap_mb:.1f} MB")

    print(f"\nHeap Allocation:")
    print(f"  Loaded Content: {breakdown.loaded_content_mb:.1f} MB ({breakdown.loaded_content_mb/breakdown.heap_mb*100:.1f}%)")
    print(f"  Query Cache: {breakdown.query_cache_mb:.1f} MB ({breakdown.query_cache_mb/breakdown.heap_mb*100:.1f}%)")
    print(f"  Indexes: {breakdown.indexes_mb:.1f} MB ({breakdown.indexes_mb/breakdown.heap_mb*100:.1f}%)")
    print(f"  Other: {breakdown.other_mb:.1f} MB ({breakdown.other_mb/breakdown.heap_mb*100:.1f}%)")


def example_threshold_checking():
    """Example 3: Memory threshold checking."""
    print("\n" + "=" * 60)
    print("Example 3: Memory Threshold Checking")
    print("=" * 60)

    # Get current memory usage
    current_mb = get_current_usage_mb()
    budget_mb = 3072  # 3 GB

    print(f"\nCurrent Usage: {current_mb:.1f} MB / {budget_mb} MB")
    print(f"Percentage: {current_mb/budget_mb*100:.1f}%")

    # Check thresholds
    warnings = check_thresholds(
        current_memory_mb=current_mb,
        budget_mb=budget_mb
    )

    if not warnings:
        print("\n✓ All thresholds within limits")
    else:
        print(f"\n⚠️ {len(warnings)} threshold(s) exceeded:")
        for warning in warnings:
            print(f"\n  Level: {warning.level.value}")
            print(f"  Message: {warning.message}")
            print(f"  Current: {warning.current_mb:.1f} MB")
            print(f"  Limit: {warning.limit_mb:.1f} MB")
            print(f"  Action: {warning.suggested_action}")


def example_custom_memory_limits():
    """Example 4: Custom memory limits."""
    print("\n" + "=" * 60)
    print("Example 4: Custom Memory Limits")
    print("=" * 60)

    from leindex.memory_profiler import MemoryLimits

    # Create custom limits
    limits = MemoryLimits(
        soft_limit_mb=2048,      # 2 GB
        hard_limit_mb=2857,      # 2.8 GB
        gc_threshold_mb=1536,    # 1.5 GB
        spill_threshold_mb=2560, # 2.5 GB
        max_loaded_files=500,
        max_cached_queries=250
    )

    manager = MemoryManager(limits=limits)
    status = manager.get_status()

    print(f"\nCustom Memory Limits:")
    print(f"  Budget: 3072 MB")
    print(f"  Soft Limit: {limits.soft_limit_mb} MB ({limits.soft_limit_mb/3072*100:.1f}%)")
    print(f"  Hard Limit: {limits.hard_limit_mb} MB ({limits.hard_limit_mb/3072*100:.1f}%)")
    print(f"  GC Threshold: {limits.gc_threshold_mb} MB")
    print(f"  Spill Threshold: {limits.spill_threshold_mb} MB")

    print(f"\nCurrent Status:")
    print(f"  Current: {status.current_mb:.1f} MB")
    print(f"  Soft Limit Exceeded: {status.soft_limit_exceeded}")
    print(f"  Hard Limit Exceeded: {status.hard_limit_exceeded}")


def example_manual_cleanup():
    """Example 5: Manual memory cleanup."""
    print("\n" + "=" * 60)
    print("Example 5: Manual Memory Cleanup")
    print("=" * 60)

    manager = MemoryManager()

    # Get memory before cleanup
    status_before = manager.get_status()
    print(f"\nMemory Before Cleanup:")
    print(f"  Current: {status_before.current_mb:.1f} MB")
    print(f"  GC Objects: {status_before.gc_objects:,}")

    # Trigger cleanup
    print("\nTriggering cleanup...")
    success = manager.cleanup()

    if success:
        # Get memory after cleanup
        status_after = manager.get_status()
        freed_mb = status_before.current_mb - status_after.current_mb

        print(f"\nMemory After Cleanup:")
        print(f"  Current: {status_after.current_mb:.1f} MB")
        print(f"  GC Objects: {status_after.gc_objects:,}")
        print(f"  Freed: {freed_mb:.1f} MB")
    else:
        print("Cleanup failed")


def example_spill_to_disk():
    """Example 6: Spill data to disk."""
    print("\n" + "=" * 60)
    print("Example 6: Spill to Disk")
    print("=" * 60)

    manager = MemoryManager()

    # Create some test data
    test_data = {
        "query_results": [{"file": "test.py", "line": 10}] * 100,
        "metadata": {"timestamp": time.time()}
    }

    print(f"\nTest data size: {sys.getsizeof(test_data) / 1024:.1f} KB")

    # Spill to disk
    print("Spilling to disk...")
    success = manager.spill_to_disk("test_data", test_data)

    if success:
        print("✓ Data spilled to disk successfully")

        # Load back from disk
        print("Loading from disk...")
        loaded_data = manager.load_from_disk("test_data")

        if loaded_data:
            print(f"✓ Data loaded from disk")
            print(f"  Original keys: {list(test_data.keys())}")
            print(f"  Loaded keys: {list(loaded_data.keys())}")
        else:
            print("✗ Failed to load data from disk")
    else:
        print("✗ Failed to spill data to disk")


def example_continuous_monitoring():
    """Example 7: Continuous memory monitoring."""
    print("\n" + "=" * 60)
    print("Example 7: Continuous Memory Monitoring (10 seconds)")
    print("=" * 60)

    manager = MemoryManager()

    # Start monitoring
    manager.start_monitoring(interval_seconds=2)
    print("Started memory monitoring (2 second interval)")

    # Register callbacks
    def on_soft_limit():
        print("⚠️ Soft limit exceeded!")

    def on_hard_limit():
        print("🚨 Hard limit exceeded!")

    manager.register_limit_exceeded_callback(on_soft_limit)
    manager.register_limit_exceeded_callback(on_hard_limit)

    # Monitor for 10 seconds
    print("\nMonitoring for 10 seconds...")
    for i in range(5):
        time.sleep(2)
        status = manager.get_status()
        print(f"[{i*2}s] Memory: {status.current_mb:.1f} MB")

    # Stop monitoring
    manager.stop_monitoring()
    print("\nStopped memory monitoring")


def example_threshold_manager():
    """Example 8: Threshold manager."""
    print("\n" + "=" * 60)
    print("Example 8: Threshold Manager")
    print("=" * 60)

    from leindex.memory_profiler import MemoryLimits

    limits = MemoryLimits(
        soft_limit_mb=2048,
        hard_limit_mb=2857
    )

    threshold_mgr = ThresholdManager(limits)

    # Simulate different memory levels
    test_levels = [
        (1500, "Normal"),
        (2200, "Soft Limit Exceeded"),
        (2900, "Hard Limit Exceeded")
    ]

    for memory_mb, description in test_levels:
        print(f"\n{description} ({memory_mb} MB):")

        # Create mock snapshot
        from leindex.memory_profiler import MemorySnapshot
        snapshot = MemorySnapshot(
            timestamp=time.time(),
            process_memory_mb=memory_mb,
            peak_memory_mb=memory_mb,
            heap_size_mb=memory_mb * 0.5,
            gc_objects=10000,
            active_threads=4,
            loaded_files=100,
            cached_queries=50
        )

        # Check thresholds
        violations = threshold_mgr.check_thresholds(snapshot)
        warnings = threshold_mgr.get_warnings()

        if violations:
            print(f"  Thresholds exceeded: {list(violations.keys())}")
        else:
            print(f"  ✓ All thresholds OK")

        if warnings:
            for warning in warnings:
                print(f"  ⚠️ {warning.level.value}: {warning.message}")


def example_action_queue():
    """Example 9: Action queue."""
    print("\n" + "=" * 60)
    print("Example 9: Action Queue")
    print("=" * 60)

    action_mgr = MemoryActionManager()

    # Queue multiple actions
    print("Queueing actions...")

    action1 = action_mgr.queue_action(
        action_type=MemoryActionType.CLEANUP,
        description="Clean up old cache entries"
    )
    print(f"  Queued: {action1.action_type.value} - {action1.description}")

    action2 = action_mgr.queue_action(
        action_type=MemoryActionType.SPILL_TO_DISK,
        description="Spill query cache to disk"
    )
    print(f"  Queued: {action2.action_type.value} - {action2.description}")

    action3 = action_mgr.queue_action(
        action_type=MemoryActionType.GC_TRIGGER,
        description="Trigger garbage collection"
    )
    print(f"  Queued: {action3.action_type.value} - {action3.description}")

    # Execute all actions
    print("\nExecuting actions...")
    results = action_mgr.execute_pending()

    for i, result in enumerate(results, 1):
        status = "✓" if result else "✗"
        print(f"  {status} Action {i}: {'Success' if result else 'Failed'}")

    # Show history
    print(f"\nAction History: {len(action_mgr.get_history())} actions")


def example_eviction_manager():
    """Example 10: Eviction manager."""
    print("\n" + "=" * 60)
    print("Example 10: Eviction Manager")
    print("=" * 60)

    eviction_mgr = EvictionManager()

    # Define project candidates
    from leindex.memory.ejection import ProjectCandidate

    candidates = [
        ProjectCandidate(
            project_id="/path/to/project1",
            priority=ProjectPriority.LOW,
            memory_mb=200,
            last_accessed=time.time() - 3600  # 1 hour ago
        ),
        ProjectCandidate(
            project_id="/path/to/project2",
            priority=ProjectPriority.MEDIUM,
            memory_mb=300,
            last_accessed=time.time() - 1800  # 30 minutes ago
        ),
        ProjectCandidate(
            project_id="/path/to/project3",
            priority=ProjectPriority.HIGH,
            memory_mb=400,
            last_accessed=time.time() - 60  # 1 minute ago
        ),
    ]

    print(f"\nProject Candidates:")
    for candidate in candidates:
        print(f"  {candidate.project_id}")
        print(f"    Priority: {candidate.priority.value}")
        print(f"    Memory: {candidate.memory_mb} MB")
        print(f"    Last Access: {time.time() - candidate.last_accessed:.0f}s ago")

    # Simulate eviction
    print(f"\nSimulating Eviction (target: 300 MB):")

    # Simple eviction policy: evict lowest priority first
    to_evict = [c for c in candidates if c.priority == ProjectPriority.LOW]
    evicted_mb = sum(c.memory_mb for c in to_evict)

    print(f"  Projects to evict: {len(to_evict)}")
    print(f"  Memory to free: {evicted_mb} MB")

    for candidate in to_evict:
        print(f"    - {candidate.project_id} ({candidate.memory_mb} MB)")


def example_configuration_yaml():
    """Example 11: Configuration via YAML."""
    print("\n" + "=" * 60)
    print("Example 11: Configuration via YAML")
    print("=" * 60)

    config_yaml = """
memory:
  total_budget_mb: 3072
  soft_limit_percent: 0.80
  hard_limit_percent: 0.93
  emergency_percent: 0.98
  max_loaded_files: 1000
  max_cached_queries: 500
  project_defaults:
    max_loaded_files: 100
    max_cached_queries: 50
    priority: "MEDIUM"

projects:
  my-large-project:
    memory:
      max_loaded_files: 500
      max_cached_queries: 200
      priority: "HIGH"
"""

    print("Example YAML Configuration:")
    print(config_yaml)


def example_hardware_detection():
    """Example 12: Hardware detection for auto-configuration."""
    print("\n" + "=" * 60)
    print("Example 12: Hardware Detection")
    print("=" * 60)

    from leindex.config import detect_hardware

    hardware = detect_hardware()

    print(f"\nDetected Hardware:")
    print(f"  CPU Cores: {hardware.cpu_count}")
    print(f"  Total RAM: {hardware.total_ram_mb:.0f} MB ({hardware.total_ram_mb/1024:.1f} GB)")
    print(f"  Available RAM: {hardware.available_ram_mb:.0f} MB")
    print(f"  GPU Available: {hardware.gpu_available}")

    if hardware.gpu_available:
        print(f"  GPU Type: {hardware.gpu_type}")
        print(f"  GPU Memory: {hardware.gpu_memory_mb:.0f} MB")

    # Recommended configuration
    print(f"\nRecommended Configuration:")

    if hardware.total_ram_mb >= 16384:  # 16 GB+
        print("  Profile: Production Server")
        print(f"  Memory Budget: {hardware.total_ram_mb * 0.25:.0f} MB")
        print(f"  Workers: {hardware.cpu_count}")
    elif hardware.total_ram_mb >= 8192:  # 8-16 GB
        print("  Profile: Development Machine")
        print(f"  Memory Budget: {hardware.total_ram_mb * 0.30:.0f} MB")
        print(f"  Workers: {max(4, hardware.cpu_count // 2)}")
    else:  # < 8 GB
        print("  Profile: Resource-Constrained")
        print(f"  Memory Budget: {hardware.total_ram_mb * 0.25:.0f} MB")
        print(f"  Workers: 2")


def example_config_reload():
    """Example 13: Zero-downtime configuration reload."""
    print("\n" + "=" * 60)
    print("Example 13: Configuration Reload")
    print("=" * 60)

    from leindex.config import get_reload_manager, ConfigObserver

    # Create a custom observer
    class MemoryConfigObserver(ConfigObserver):
        def __init__(self):
            self.reload_count = 0

        def on_config_reloaded(self, event):
            self.reload_count += 1
            print(f"\n🔄 Configuration reloaded (#{self.reload_count})")
            print(f"   Timestamp: {event.timestamp}")

            # Check if memory config changed
            old_mem = event.old_config.memory if event.old_config else None
            new_mem = event.new_config.memory

            if old_mem and new_mem:
                if old_mem.total_budget_mb != new_mem.total_budget_mb:
                    print(f"   ✓ Memory budget changed: {old_mem.total_budget_mb} → {new_mem.total_budget_mb} MB")
                if old_mem.soft_limit_percent != new_mem.soft_limit_percent:
                    print(f"   ✓ Soft limit changed: {old_mem.soft_limit_percent} → {new_mem.soft_limit_percent}")

    # Register observer
    manager = get_reload_manager()
    observer = MemoryConfigObserver()
    manager.register_observer(observer)

    print("Registered configuration observer")
    print("\nTo test reload:")
    print("  1. Modify ~/.leindex/config.yaml")
    print("  2. Run: kill -HUP $(cat ~/.leindex/leindex.pid)")
    print("  3. Observer will be notified automatically")


def main():
    """Run all examples."""
    print("\n" + "=" * 60)
    print("LeIndex v2.0 - Memory Configuration Examples")
    print("=" * 60)

    examples = [
        example_basic_memory_monitoring,
        example_memory_breakdown,
        example_threshold_checking,
        example_custom_memory_limits,
        example_manual_cleanup,
        example_spill_to_disk,
        example_continuous_monitoring,
        example_threshold_manager,
        example_action_queue,
        example_eviction_manager,
        example_configuration_yaml,
        example_hardware_detection,
        example_config_reload,
    ]

    for example in examples:
        try:
            example()
        except Exception as e:
            print(f"\nExample failed: {e}")
            import traceback
            traceback.print_exc()

    print("\n" + "=" * 60)
    print("Examples Complete!")
    print("=" * 60)


if __name__ == "__main__":
    main()
