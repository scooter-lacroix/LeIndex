#!/usr/bin/env python3
"""
Memory Monitoring System - Usage Examples

This file demonstrates how to use the memory monitoring system
implemented in Task 5.6 of the Search Enhancement Track.

Examples include:
1. Basic monitoring with global functions
2. Advanced monitoring with custom configuration
3. Health checks and alerts
4. Custom error handling
5. Integration with existing code
"""

from leindex.memory.monitoring import (
    MemoryMonitor,
    StructuredLogger,
    start_monitoring,
    stop_monitoring,
    get_metrics_sync,
    health_check_sync,
    MemoryError,
    ThresholdError,
    EvictionError,
)
import time


# =============================================================================
# Example 1: Basic Monitoring
# =============================================================================

def example_1_basic_monitoring():
    """Basic monitoring using global convenience functions."""
    print("=" * 60)
    print("Example 1: Basic Monitoring")
    print("=" * 60)

    # Start monitoring (begins background profiling at 30s intervals)
    print("\n1. Starting monitoring...")
    start_monitoring()
    print("   ✓ Monitoring started (background profiling active)")

    # Wait for some snapshots to be collected
    print("\n2. Waiting for snapshots (3 seconds)...")
    time.sleep(3)

    # Get current metrics
    print("\n3. Current Metrics:")
    metrics = get_metrics_sync()

    mem = metrics["metrics"]
    print(f"   • Memory RSS: {mem['memory_rss_mb']:.1f} MB")
    print(f"   • Usage: {mem['memory_usage_percent']:.1f}%")
    print(f"   • Status: {mem['status']}")
    print(f"   • Evictions: {mem['eviction_count']}")
    print(f"   • Growth Rate: {mem['growth_rate_mb_per_sec']:+.2f} MB/s")

    # Get profiler statistics
    print("\n4. Profiler Statistics:")
    profiler = metrics["profiler"]
    print(f"   • Snapshots Taken: {profiler['total_snapshots_taken']}")
    print(f"   • Current Snapshots: {profiler['current_snapshot_count']}")
    print(f"   • Profiling Active: {profiler['profiling_active']}")

    # Perform health check
    print("\n5. Health Check:")
    health = health_check_sync()
    print(f"   • Overall Status: {health['status']}")
    print(f"   • Failed Checks: {health['failed_checks']}")
    print(f"   • Critical Issues: {health['critical_issues']}")

    # Stop monitoring
    print("\n6. Stopping monitoring...")
    stop_monitoring()
    print("   ✓ Monitoring stopped")


# =============================================================================
# Example 2: Advanced Monitoring with Custom Configuration
# =============================================================================

def example_2_advanced_monitoring():
    """Advanced monitoring with custom configuration."""
    print("\n" + "=" * 60)
    print("Example 2: Advanced Monitoring")
    print("=" * 60)

    # Create monitor with custom settings
    print("\n1. Creating monitor with custom configuration...")
    monitor = MemoryMonitor(
        profiling_interval_seconds=5,  # Snapshot every 5 seconds
        max_snapshots=120  # Keep 10 minutes of history
    )
    print("   ✓ Monitor created (5s interval, 120 snapshot max)")

    # Start monitoring
    print("\n2. Starting monitoring...")
    monitor.start_sync()
    print("   ✓ Monitoring started")

    # Simulate some work
    print("\n3. Simulating work (waiting 12 seconds)...")
    for i in range(3):
        time.sleep(4)  # Wait for snapshots
        print(f"   • Collected {len(monitor.get_snapshots())} snapshots")

    # Get snapshots for analysis
    print("\n4. Analyzing snapshots:")
    snapshots = monitor.get_snapshots()
    print(f"   • Total snapshots: {len(snapshots)}")

    if snapshots:
        print("\n   Latest 3 snapshots:")
        for snapshot in snapshots[-3:]:
            print(f"   - {snapshot.rss_mb:.1f} MB, {snapshot.usage_percent:.1f}%, "
                  f"status={snapshot.status}")

        # Calculate statistics
        rss_values = [s.rss_mb for s in snapshots]
        print(f"\n   RSS Statistics:")
        print(f"   - Min: {min(rss_values):.1f} MB")
        print(f"   - Max: {max(rss_values):.1f} MB")
        print(f"   - Avg: {sum(rss_values)/len(rss_values):.1f} MB")

    # Get latest snapshot
    print("\n5. Latest Snapshot:")
    latest = monitor.get_latest_snapshot()
    if latest:
        print(f"   • RSS: {latest.rss_mb:.1f} MB")
        print(f"   • Heap Objects: {latest.heap_objects:,}")
        print(f"   • Status: {latest.status}")
        print(f"   • Growth Rate: {latest.growth_rate_mb_per_sec:+.2f} MB/s")

    # Stop monitoring
    print("\n6. Stopping monitoring...")
    monitor.stop_sync()
    print("   ✓ Monitoring stopped")


# =============================================================================
# Example 3: Health Checks and Alerts
# =============================================================================

def example_3_health_checks():
    """Health checks and alert handling."""
    print("\n" + "=" * 60)
    print("Example 3: Health Checks and Alerts")
    print("=" * 60)

    # Start monitoring
    print("\n1. Starting monitoring...")
    monitor = MemoryMonitor()
    monitor.start_sync()
    print("   ✓ Monitoring started")

    # Perform health check
    print("\n2. Performing comprehensive health check...")
    health = monitor.health_check_sync()

    print(f"\n   Overall Status: {health['status'].upper()}")
    print(f"\n   Individual Checks:")

    for check_name, check_result in health['checks'].items():
        status_symbol = "✓" if check_result['healthy'] else "✗"
        severity = check_result.get('severity', '').upper()
        print(f"   {status_symbol} {check_name}:")
        print(f"      - Healthy: {check_result['healthy']}")
        print(f"      - Message: {check_result.get('message', 'N/A')}")
        if severity:
            print(f"      - Severity: {severity}")

    print(f"\n   Summary:")
    print(f"   - Failed Checks: {health['failed_checks']}")
    print(f"   - Critical Issues: {health['critical_issues']}")

    # Check specific conditions
    print("\n3. Checking specific conditions:")

    if health['status'] == 'healthy':
        print("   ✓ System is healthy - no action needed")
    elif health['status'] == 'warning':
        print("   ⚠ System has warnings - monitor closely")
    elif health['status'] == 'critical':
        print("   ✗ System is critical - immediate action required")

    # Stop monitoring
    print("\n4. Stopping monitoring...")
    monitor.stop_sync()
    print("   ✓ Monitoring stopped")


# =============================================================================
# Example 4: Custom Error Handling
# =============================================================================

def example_4_error_handling():
    """Custom error handling for memory operations."""
    print("\n" + "=" * 60)
    print("Example 4: Custom Error Handling")
    print("=" * 60)

    print("\n1. Demonstrating error categories:")

    # MemoryError (base class)
    print("\n   a) MemoryError (base exception):")
    try:
        raise MemoryError("Generic memory error occurred")
    except MemoryError as e:
        print(f"      Caught: {e}")

    # ThresholdError
    print("\n   b) ThresholdError (threshold exceeded):")
    try:
        raise ThresholdError(
            "Memory limit exceeded",
            threshold_type="warning",
            current_mb=700.0,
            threshold_mb=614.4
        )
    except ThresholdError as e:
        print(f"      Threshold Type: {e.threshold_type}")
        print(f"      Current: {e.current_mb} MB")
        print(f"      Limit: {e.threshold_mb} MB")
        print(f"      Message: {e}")

    # EvictionError
    print("\n   c) EvictionError (eviction failed):")
    try:
        raise EvictionError(
            "Failed to free required memory",
            target_mb=500.0,
            freed_mb=300.0,
            errors=["Failed to evict project1", "Failed to evict project2"]
        )
    except EvictionError as e:
        print(f"      Target: {e.target_mb} MB")
        print(f"      Freed: {e.freed_mb} MB")
        print(f"      Success Rate: {e.freed_mb / e.target_mb * 100:.1f}%")
        print(f"      Errors: {e.errors}")

    print("\n2. Example error handling pattern:")
    print("""
    try:
        monitor = MemoryMonitor()
        monitor.start_sync()

        # Memory operations here...
        metrics = monitor.get_metrics_sync()

        # Check for critical status
        if metrics['metrics']['status'] == 'critical':
            raise ThresholdError(
                "Critical memory threshold",
                threshold_type="emergency",
                current_mb=metrics['metrics']['memory_rss_mb'],
                threshold_mb=metrics['metrics']['hard_limit_mb']
            )

    except ThresholdError as e:
        # Handle threshold crossings
        print(f"Threshold {e.threshold_type} exceeded")
        print(f"Current: {e.current_mb}MB, Limit: {e.threshold_mb}MB")
        # Trigger eviction, cleanup, etc.

    except EvictionError as e:
        # Handle eviction failures
        print(f"Eviction incomplete: {e.freed_mb}MB of {e.target_mb}MB")
        print(f"Errors: {e.errors}")
        # Try alternative cleanup strategies

    except MemoryError as e:
        # Generic memory error handling
        print(f"Memory error: {e}")
        # Log, alert, or perform generic cleanup

    finally:
        # Always cleanup
        monitor.stop_sync()
    """)


# =============================================================================
# Example 5: Integration with Existing Code
# =============================================================================

def example_5_integration():
    """Integration with existing memory tracker and eviction manager."""
    print("\n" + "=" * 60)
    print("Example 5: Integration with Existing Code")
    print("=" * 60)

    print("\n1. The monitoring system integrates automatically with:")
    print("   • MemoryTracker (from leindex.memory.tracker)")
    print("   • EvictionManager (from leindex.memory.eviction)")
    print("   • MemoryStatus (from leindex.memory.status)")

    print("\n2. Create monitor with custom components:")
    print("""
    from leindex.memory.tracker import MemoryTracker, MemoryTrackerConfig
    from leindex.memory.eviction import EvictionManager

    # Custom tracker configuration
    tracker = MemoryTracker(
        tracker_config=MemoryTrackerConfig(
            monitoring_interval_seconds=60,
            history_retention_hours=48
        )
    )

    # Create monitor with custom tracker
    monitor = MemoryMonitor(tracker=tracker)

    # Monitor automatically uses tracker and eviction manager
    monitor.start_sync()

    # Metrics include data from both components
    metrics = monitor.get_metrics_sync()

    # Memory tracker data
    print(f"RSS: {metrics['metrics']['memory_rss_mb']} MB")
    print(f"Growth Rate: {metrics['metrics']['growth_rate_mb_per_sec']} MB/s")

    # Eviction manager data
    print(f"Evictions: {metrics['metrics']['eviction_count']}")
    print(f"Memory Freed: {metrics['metrics']['memory_freed_total_mb']} MB")
    """)

    print("\n3. Using structured logger:")
    print("""
    from leindex.memory.monitoring import StructuredLogger

    logger = StructuredLogger(component="my_component")

    # Log memory events
    logger.log_memory_event(
        "custom_operation",
        level="info",
        operation="data_processing",
        memory_used_mb=128.5,
        duration_seconds=2.3
    )

    # Log threshold crossings
    logger.log_threshold_crossing(
        "warning",
        current_mb=700.0,
        threshold_mb=614.4,
        usage_percent=85.0
    )

    # Log eviction events
    logger.log_eviction_event(
        projects_evicted=["project1", "project2"],
        memory_freed_mb=512.0,
        target_mb=500.0,
        duration_seconds=2.5
    )

    # Log errors
    logger.log_error(
        error_type="memory_error",
        error_message="Memory allocation failed",
        context={"operation": "load_index"}
    )
    """)


# =============================================================================
# Main Entry Point
# =============================================================================

def main():
    """Run all examples."""
    print("\n" + "=" * 60)
    print("MEMORY MONITORING SYSTEM - USAGE EXAMPLES")
    print("=" * 60)
    print("\nThis demo shows the monitoring system from Task 5.6")
    print("implementing comprehensive memory monitoring with:")
    print("• Structured JSON logging")
    print("• Real-time metrics collection")
    print("• Health checks")
    print("• Error categorization")
    print("• Periodic profiling snapshots")

    # Run examples
    try:
        example_1_basic_monitoring()
        example_2_advanced_monitoring()
        example_3_health_checks()
        example_4_error_handling()
        example_5_integration()

        print("\n" + "=" * 60)
        print("All Examples Complete!")
        print("=" * 60)
        print("\nFor more information, see:")
        print("• src/leindex/memory/monitoring.py")
        print("• tests/unit/test_memory_monitoring.py")
        print("• TASK_5.6_COMPLETION_SUMMARY.md")

    except KeyboardInterrupt:
        print("\n\nDemo interrupted by user")
    except Exception as e:
        print(f"\n\nDemo error: {e}")
        import traceback
        traceback.print_exc()


if __name__ == "__main__":
    main()
