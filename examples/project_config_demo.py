#!/usr/bin/env python3
"""
Demo script showing project configuration overrides usage.

This script demonstrates how to use per-project configuration overrides
to customize memory allocation and eviction priorities.
"""

import sys
import os
from pathlib import Path

# Add src to path
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from leindex.project_config import (
    ProjectConfigManager,
    ProjectConfig,
    ProjectMemoryConfig,
    load_project_config,
    get_effective_memory_config,
)


def demo_create_config():
    """Demonstrate creating and saving a project configuration."""
    print("=" * 60)
    print("Demo 1: Creating Project Configuration")
    print("=" * 60)

    # Use current directory as example project
    project_path = Path.cwd()

    # Create configuration for a large ML project
    config = ProjectConfig(
        memory=ProjectMemoryConfig(
            estimated_mb=512,  # Double the default
            priority="high"     # Keep in memory
        )
    )

    # Save configuration
    manager = ProjectConfigManager(str(project_path))
    manager.save_config(config)

    print(f"\n✓ Created config for: {project_path}")
    print(f"  - Memory estimate: {config.memory.estimated_mb}MB")
    print(f"  - Priority: {config.memory.priority}")
    print(f"  - Priority score: {config.memory.get_priority_score()}")

    # Show config file location
    print(f"\n✓ Config saved to: {manager.config_path}")
    print(f"  - File exists: {manager.config_exists()}")

    return manager


def demo_load_config(manager):
    """Demonstrate loading project configuration."""
    print("\n" + "=" * 60)
    print("Demo 2: Loading Project Configuration")
    print("=" * 60)

    # Load configuration
    config = manager.get_config()

    print(f"\n✓ Loaded config from: {config._source_path}")
    print(f"  - Memory estimate: {config.memory.estimated_mb}MB")
    print(f"  - Priority: {config.memory.priority}")
    print(f"  - Max override: {config.memory.max_override_mb}MB")


def demo_effective_config(manager):
    """Demonstrate getting effective configuration."""
    print("\n" + "=" * 60)
    print("Demo 3: Effective Configuration (Merged with Defaults)")
    print("=" * 60)

    # Get effective configuration
    effective = manager.get_effective_memory_config()

    print(f"\n✓ Effective memory configuration:")
    print(f"  - Estimated memory: {effective['estimated_mb']}MB")
    print(f"  - Priority: {effective['priority']}")
    print(f"  - Priority score: {effective['priority_score']}")
    print(f"  - Is overridden: {effective['is_overridden']}")
    print(f"  - Max override: {effective['max_override_mb']}MB")


def demo_priority_scores():
    """Demonstrate priority scores for eviction."""
    print("\n" + "=" * 60)
    print("Demo 4: Priority Scores for Eviction")
    print("=" * 60)

    priorities = ["high", "normal", "low"]

    print("\n✓ Priority scores (higher = less likely to be evicted):")
    for priority in priorities:
        config = ProjectMemoryConfig(priority=priority)
        score = config.get_priority_score()
        print(f"  - {priority:8s}: score {score:.1f}")


def demo_validation():
    """Demonstrate configuration validation."""
    print("\n" + "=" * 60)
    print("Demo 5: Configuration Validation")
    print("=" * 60)

    print("\n✓ Valid configurations:")
    valid_configs = [
        (512, "high", "Large ML project"),
        (256, "normal", "Typical project"),
        (64, "low", "Small utility"),
        (0, "normal", "Minimal memory"),
    ]

    for mb, priority, desc in valid_configs:
        try:
            config = ProjectMemoryConfig(estimated_mb=mb, priority=priority)
            print(f"  - {desc:25s}: {mb:3d}MB, {priority:8s} ✓")
        except ValueError as e:
            print(f"  - {desc:25s}: ERROR - {e}")

    print("\n✗ Invalid configurations (should fail):")
    invalid_configs = [
        (1024, "normal", "Exceeds max (512MB)"),
        (-100, "normal", "Negative memory"),
        (256, "urgent", "Invalid priority"),
    ]

    for mb, priority, desc in invalid_configs:
        try:
            config = ProjectMemoryConfig(estimated_mb=mb, priority=priority)
            print(f"  - {desc:25s}: UNEXPECTED SUCCESS")
        except ValueError as e:
            print(f"  - {desc:25s}: ✓ Correctly rejected")


def demo_convenience_functions():
    """Demonstrate convenience functions."""
    print("\n" + "=" * 60)
    print("Demo 6: Convenience Functions")
    print("=" * 60)

    project_path = Path.cwd()

    # Quick load
    config = load_project_config(str(project_path))
    print(f"\n✓ load_project_config():")
    print(f"  - Priority: {config.memory.priority}")

    # Quick effective config
    effective = get_effective_memory_config(str(project_path))
    print(f"\n✓ get_effective_memory_config():")
    print(f"  - Estimated: {effective['estimated_mb']}MB")
    print(f"  - Score: {effective['priority_score']}")


def demo_config_scenarios():
    """Demonstrate different configuration scenarios."""
    print("\n" + "=" * 60)
    print("Demo 7: Real-World Configuration Scenarios")
    print("=" * 60)

    scenarios = [
        {
            "name": "Large ML Project",
            "config": ProjectMemoryConfig(estimated_mb=512, priority="high"),
            "use_case": "Deep learning codebase with large models"
        },
        {
            "name": "Active Development",
            "config": ProjectMemoryConfig(estimated_mb=384, priority="high"),
            "use_case": "Frequently modified microservices"
        },
        {
            "name": "Typical Project",
            "config": ProjectMemoryConfig(estimated_mb=256, priority="normal"),
            "use_case": "Standard web application"
        },
        {
            "name": "Reference Code",
            "config": ProjectMemoryConfig(estimated_mb=128, priority="low"),
            "use_case": "Occasionally accessed legacy code"
        },
        {
            "name": "Small Utility",
            "config": ProjectMemoryConfig(estimated_mb=64, priority="low"),
            "use_case": "Rarely used helper scripts"
        },
    ]

    print("\n✓ Example configurations for different use cases:\n")
    for scenario in scenarios:
        cfg = scenario["config"]
        print(f"  {scenario['name']}:")
        print(f"    Use case: {scenario['use_case']}")
        print(f"    Config: {cfg.estimated_mb}MB, priority={cfg.priority}, "
              f"score={cfg.get_priority_score():.1f}")
        print()


def demo_delete_config(manager):
    """Demonstrate deleting configuration."""
    print("=" * 60)
    print("Demo 8: Deleting Configuration")
    print("=" * 60)

    print(f"\n✓ Deleting config at: {manager.config_path}")
    manager.delete_config()

    print(f"  - Config exists: {manager.config_exists()}")
    print(f"\n  Project will now use global defaults.")


def main():
    """Run all demos."""
    print("\n" + "=" * 60)
    print("PROJECT CONFIGURATION OVERRIDES DEMO")
    print("=" * 60)
    print(f"\nCurrent directory: {Path.cwd()}")
    print(f"Config location: {Path.cwd() / '.leindex_data' / 'config.yaml'}")

    try:
        # Run demos
        manager = demo_create_config()
        demo_load_config(manager)
        demo_effective_config(manager)
        demo_priority_scores()
        demo_validation()
        demo_convenience_functions()
        demo_config_scenarios()

        # Cleanup
        demo_delete_config(manager)

        print("\n" + "=" * 60)
        print("Demo Complete!")
        print("=" * 60)
        print("\n✓ All demonstrations completed successfully.")
        print("\nNext steps:")
        print("  1. Create .leindex_data/config.yaml in your project")
        print("  2. Adjust estimated_mb and priority as needed")
        print("  3. Monitor actual memory usage to tune values")
        print("  4. See docs/PROJECT_CONFIG_OVERRIDES.md for details")

    except Exception as e:
        print(f"\n✗ Error: {e}")
        import traceback
        traceback.print_exc()
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
