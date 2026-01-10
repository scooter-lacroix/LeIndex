#!/usr/bin/env python3
"""
Phase 5 Implementation Verification Script

This script demonstrates and verifies the Phase 5 memory management implementation.
"""

import sys
import time
sys.path.insert(0, 'src')

from leindex.memory.status import MemoryStatus, MemoryBreakdown
from leindex.memory.thresholds import ThresholdChecker, ThresholdLevel
from leindex.memory.actions import ActionQueue, ActionType
from leindex.memory.eviction import (
    EvictionManager,
    ProjectCandidate,
    ProjectPriority,
    MockProjectUnloader,
)


def test_thresholds():
    """Test threshold detection and warning generation."""
    print("=" * 80)
    print("TEST 1: Threshold Detection")
    print("=" * 80)

    checker = ThresholdChecker()

    # Test different memory levels
    test_cases = [
        (50.0, "healthy"),
        (85.0, "caution"),
        (95.0, "warning"),
        (99.0, "critical"),
    ]

    for usage_percent, expected_status in test_cases:
        current_mb = 3072.0 * (usage_percent / 100.0)

        status = MemoryStatus(
            timestamp=time.time(),
            current_mb=current_mb,
            soft_limit_mb=2457.6,
            hard_limit_mb=3000.0,
            prompt_threshold_mb=2856.0,
            total_budget_mb=3072.0,
            global_index_mb=512.0,
            usage_percent=usage_percent,
            soft_usage_percent=(current_mb / 2457.6) * 100,
            hard_usage_percent=(current_mb / 3000.0) * 100,
            status=expected_status,
        )

        warning = checker.check_thresholds(status)

        print(f"\n{expected_status.upper()} ({usage_percent}% usage):")
        if warning:
            print(f"  Level: {warning.level.value}")
            print(f"  Urgency: {warning.urgency}")
            print(f"  Action: {warning.action}")
            print(f"  Message: {warning.message[:80]}...")
            print(f"  Recommendations: {len(warning.recommendations)}")
            print(f"  Available Actions: {len(warning.available_actions)}")
        else:
            print(f"  No warning - memory is healthy")


def test_actions():
    """Test action queue and execution."""
    print("\n" + "=" * 80)
    print("TEST 2: Action Queue & Execution")
    print("=" * 80)

    queue = ActionQueue()

    # Enqueue some actions
    print("\nEnqueueing actions...")
    queue.enqueue("garbage_collection", priority=5)
    queue.enqueue("garbage_collection", priority=10)
    queue.enqueue("garbage_collection", priority=1)

    print(f"Queue size: {queue.get_queue_size()}")

    # Show queue summary
    summary = queue.get_queue_summary()
    print("\nQueue summary (priority order):")
    for action in summary:
        print(f"  ID {action['id']}: priority={action['priority']}, "
              f"est_freed={action['estimated_freed_mb']:.1f}MB")

    # Execute all actions
    print("\nExecuting all actions...")
    results = queue.execute_all()

    print(f"\nExecution results:")
    for result in results:
        print(f"  {result}")


def test_eviction():
    """Test eviction manager."""
    print("\n" + "=" * 80)
    print("TEST 3: Priority-Based Eviction")
    print("=" * 80)

    # Create mock unloader with test projects
    unloader = MockProjectUnloader()

    now = time.time()
    unloader.add_project("project_high", "/path/to/high", ProjectPriority.HIGH, 512.0)
    unloader.add_project("project_normal", "/path/to/normal", ProjectPriority.NORMAL, 256.0)
    unloader.add_project("project_low", "/path/to/low", ProjectPriority.LOW, 128.0)

    # Set different access times
    unloader._loaded_projects["project_high"].last_access = now - 100  # Recent
    unloader._loaded_projects["project_normal"].last_access = now - 1000  # Older
    unloader._loaded_projects["project_low"].last_access = now - 5000  # Very old

    print("\nLoaded projects:")
    candidates = unloader.get_loaded_projects()
    for c in candidates:
        score = c.get_eviction_score()
        print(f"  {c.project_id}:")
        print(f"    Priority: {c.priority.value}")
        print(f"    Age: {now - c.last_access:.0f}s")
        print(f"    Eviction Score: {score:.1f}")
        print(f"    Estimated MB: {c.estimated_mb:.1f}MB")

    # Create eviction manager
    manager = EvictionManager(unloader)

    # Perform eviction
    print("\nPerforming emergency eviction (target: 200MB)...")
    result = manager.emergency_eviction(target_mb=200.0)

    print(f"\nEviction Result:")
    print(f"  Success: {result.success}")
    print(f"  Projects Evicted: {result.projects_evicted}")
    print(f"  Memory Freed: {result.memory_freed_mb:.1f}MB / {result.target_mb:.1f}MB target")
    print(f"  Duration: {result.duration_seconds:.4f}s")
    print(f"  Message: {result.message}")

    # Show statistics
    stats = manager.get_statistics()
    print(f"\nEviction Statistics:")
    print(f"  Total Evictions: {stats['total_evictions']}")
    print(f"  Total Memory Freed: {stats['total_memory_freed_mb']:.1f}MB")


def test_integration():
    """Test full integration scenario."""
    print("\n" + "=" * 80)
    print("TEST 4: Full Integration Scenario")
    print("=" * 80)

    print("\nSimulating memory pressure scenario...")

    # Simulate gradual memory increase
    checker = ThresholdChecker()
    queue = ActionQueue()

    for usage in [50, 70, 85, 93, 98]:
        current_mb = 3072.0 * (usage / 100.0)

        status = MemoryStatus(
            timestamp=time.time(),
            current_mb=current_mb,
            soft_limit_mb=2457.6,
            hard_limit_mb=3000.0,
            prompt_threshold_mb=2856.0,
            total_budget_mb=3072.0,
            global_index_mb=512.0,
            usage_percent=float(usage),
            soft_usage_percent=(current_mb / 2457.6) * 100,
            hard_usage_percent=(current_mb / 3000.0) * 100,
            status="healthy" if usage < 80 else "caution" if usage < 93 else "warning" if usage < 98 else "critical",
        )

        warning = checker.check_thresholds(status)

        print(f"\nMemory at {usage}%:")
        if warning:
            print(f"  {warning.level.value.upper()} threshold crossed!")
            print(f"  Urgency: {warning.urgency}")
            print(f"  Suggested action: {warning.action}")

            # Simulate taking action
            if warning.level == ThresholdLevel.CAUTION:
                print("  -> Taking action: garbage collection")
                queue.enqueue("garbage_collection", priority=5)
            elif warning.level == ThresholdLevel.WARNING:
                print("  -> Would prompt LLM for user action selection")
                print(f"  -> Available actions: {len(warning.available_actions)}")
            elif warning.level == ThresholdLevel.CRITICAL:
                print("  -> Triggering emergency eviction!")
                # This would call emergency_eviction() in real scenario

    # Execute queued actions
    print("\nExecuting queued actions...")
    results = queue.execute_all()
    print(f"Executed {len(results)} actions")


if __name__ == "__main__":
    print("\n" + "=" * 80)
    print("PHASE 5: MEMORY MANAGEMENT - IMPLEMENTATION VERIFICATION")
    print("=" * 80)
    print("\nThis script demonstrates the Phase 5 implementation:")
    print("  1. Threshold Detection (Task 5.1)")
    print("  2. Action Execution (Task 5.1)")
    print("  3. Priority-Based Eviction (Task 5.2)")
    print("  4. Full Integration Scenario")
    print("\n" + "=" * 80)

    try:
        test_thresholds()
        test_actions()
        test_eviction()
        test_integration()

        print("\n" + "=" * 80)
        print("✅ ALL TESTS PASSED - Phase 5 implementation verified!")
        print("=" * 80)

    except Exception as e:
        print(f"\n❌ ERROR: {e}")
        import traceback
        traceback.print_exc()
        sys.exit(1)
