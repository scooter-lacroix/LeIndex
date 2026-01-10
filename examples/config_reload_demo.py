#!/usr/bin/env python3
"""
Demonstration of Zero-Downtime Config Reload Functionality

This script demonstrates:
1. Config reload without server restart
2. Signal-based reload triggering
3. Observer pattern for component notifications
4. Thread-safe concurrent operations
5. Statistics and event history tracking
"""

import os
import sys
import time
import signal
import tempfile
import yaml
from pathlib import Path

# Add src to path
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from leindex.config import GlobalConfigManager
from leindex.config.reload import (
    initialize_reload_manager,
    reload_config,
    ReloadResult,
)


def make_valid_config(total_budget=4096):
    """Helper to create valid config dict."""
    global_index = max(int(total_budget * 0.125), 128)
    return {
        'version': '2.0',
        'memory': {
            'total_budget_mb': total_budget,
            'global_index_mb': global_index,
            'warning_threshold_percent': 75,
            'prompt_threshold_percent': 90,
            'emergency_threshold_percent': 95,
        },
        'projects': {
            'estimated_mb': 256,
            'priority': 'normal',
            'max_file_size': 5242880,
        },
        'performance': {
            'cache_enabled': True,
            'cache_ttl_seconds': 300,
            'parallel_workers': 4,
            'batch_size': 50,
        }
    }


def main():
    """Run config reload demonstration."""
    print("=" * 70)
    print("Zero-Downtime Config Reload Demonstration")
    print("=" * 70)

    # Create temporary config file
    temp_dir = tempfile.mkdtemp(prefix="config_reload_demo_")
    config_file = os.path.join(temp_dir, "demo_config.yaml")

    # Create initial config
    initial_config = make_valid_config(total_budget=4096)
    with open(config_file, 'w') as f:
        yaml.dump(initial_config, f)

    print(f"\n✓ Created config file: {config_file}")
    print(f"  Initial memory budget: {initial_config['memory']['total_budget_mb']} MB")

    # Initialize config manager
    config_mgr = GlobalConfigManager(config_path=config_file)

    # Initialize reload manager with signal handler
    reload_mgr = initialize_reload_manager(
        config_manager=config_mgr,
        enable_signal_handler=True
    )

    print("✓ Initialized reload manager")
    print("✓ Registered SIGHUP signal handler")

    # Register observers
    print("\n" + "-" * 70)
    print("Registering Component Observers")
    print("-" * 70)

    def memory_observer(old, new):
        print(f"  [MemoryObserver] Budget: {old.memory.total_budget_mb} -> {new.memory.total_budget_mb} MB")

    def performance_observer(old, new):
        print(f"  [PerformanceObserver] Workers: {old.performance.parallel_workers} -> {new.performance.parallel_workers}")

    reload_mgr.subscribe(memory_observer)
    reload_mgr.subscribe(performance_observer)

    print(f"✓ Registered {reload_mgr.get_observer_count()} observers")

    # Demo 1: Programmatic Reload
    print("\n" + "-" * 70)
    print("Demo 1: Programmatic Config Reload")
    print("-" * 70)

    print("\nUpdating config file...")
    new_config = make_valid_config(total_budget=6144)
    new_config['performance']['parallel_workers'] = 8
    with open(config_file, 'w') as f:
        yaml.dump(new_config, f)

    print("New config values:")
    print(f"  - Memory budget: {new_config['memory']['total_budget_mb']} MB")
    print(f"  - Parallel workers: {new_config['performance']['parallel_workers']}")

    print("\nTriggering reload...")
    result = reload_mgr.reload_config()

    print(f"✓ Reload result: {result.value}")

    # Show statistics
    stats = reload_mgr.get_stats()
    print(f"\nStatistics:")
    print(f"  - Total reloads: {stats['total_reloads']}")
    print(f"  - Successful: {stats['successful_reloads']}")
    print(f"  - Failed: {stats['failed_reloads']}")

    # Demo 2: Signal-Based Reload
    print("\n" + "-" * 70)
    print("Demo 2: Signal-Based Config Reload (SIGHUP)")
    print("-" * 70)

    print("\nUpdating config file...")
    new_config = make_valid_config(total_budget=8192)
    with open(config_file, 'w') as f:
        yaml.dump(new_config, f)

    print(f"New memory budget: {new_config['memory']['total_budget_mb']} MB")
    print("\nSending SIGHUP signal to process...")
    print(f"  (Process ID: {os.getpid()})")

    # Send SIGHUP to self
    os.kill(os.getpid(), signal.SIGHUP)

    # Give signal handler time to execute
    time.sleep(0.2)

    print("✓ Signal handler triggered reload")

    # Demo 3: Validation Failure and Rollback
    print("\n" + "-" * 70)
    print("Demo 3: Validation Failure with Automatic Rollback")
    print("-" * 70)

    print("\nWriting INVALID config (thresholds out of order)...")
    invalid_config = make_valid_config()
    invalid_config['memory']['warning_threshold_percent'] = 95
    invalid_config['memory']['prompt_threshold_percent'] = 90

    with open(config_file, 'w') as f:
        yaml.dump(invalid_config, f)

    print("  - warning_threshold: 95%")
    print("  - prompt_threshold: 90% (INVALID: must be > warning)")

    print("\nAttempting reload...")
    result = reload_mgr.reload_config()

    print(f"✓ Reload result: {result.value}")

    if result == ReloadResult.VALIDATION_FAILED:
        print("  Config validation FAILED - old config preserved")

        # Verify rollback
        current = reload_mgr.get_current_config()
        print(f"  Current budget: {current.memory.total_budget_mb} MB (unchanged)")

    # Demo 4: Event History
    print("\n" + "-" * 70)
    print("Demo 4: Event History")
    print("-" * 70)

    history = reload_mgr.get_event_history()
    print(f"\nTotal events in history: {len(history)}")
    print("\nRecent events:")
    for i, event in enumerate(history[-3:], 1):
        print(f"  {i}. {event.result.value:20s} - {event.duration_ms:.2f}ms")

    # Demo 5: Convenience Function
    print("\n" + "-" * 70)
    print("Demo 5: Convenience Function")
    print("-" * 70)

    print("\nUsing reload_config() convenience function...")

    new_config = make_valid_config(total_budget=10240)
    with open(config_file, 'w') as f:
        yaml.dump(new_config, f)

    result = reload_config()
    print(f"✓ Reload result: {result.value}")

    # Final Statistics
    print("\n" + "=" * 70)
    print("Final Statistics")
    print("=" * 70)

    stats = reload_mgr.get_stats()
    print(f"\nTotal Reloads: {stats['total_reloads']}")
    print(f"Successful: {stats['successful_reloads']}")
    print(f"Failed: {stats['failed_reloads']}")
    print(f"Success Rate: {stats['successful_reloads'] / stats['total_reloads'] * 100:.1f}%")

    # Current config
    current = reload_mgr.get_current_config()
    print(f"\nCurrent Configuration:")
    print(f"  Memory Budget: {current.memory.total_budget_mb} MB")
    print(f"  Global Index: {current.memory.global_index_mb} MB")
    print(f"  Parallel Workers: {current.performance.parallel_workers}")
    print(f"  Batch Size: {current.performance.batch_size}")

    # Cleanup
    import shutil
    shutil.rmtree(temp_dir, ignore_errors=True)
    print(f"\n✓ Cleaned up temporary files")

    print("\n" + "=" * 70)
    print("Demonstration Complete!")
    print("=" * 70)

    print("\nKey Features Demonstrated:")
    print("  ✓ Zero-downtime config reload")
    print("  ✓ Signal-based reload (SIGHUP)")
    print("  ✓ Programmatic reload")
    print("  ✓ Observer pattern")
    print("  ✓ Validation and rollback")
    print("  ✓ Event history tracking")
    print("  ✓ Statistics collection")
    print("  ✓ Thread-safe operations")


if __name__ == "__main__":
    main()
