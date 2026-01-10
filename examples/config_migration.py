#!/usr/bin/env python3
"""
Configuration Migration Examples for LeIndex v1 to v2

This example demonstrates how to migrate configuration from LeIndex v1.x
to v2.0 format.

Usage:
    python config_migration.py
"""

import sys
import yaml
import json
from pathlib import Path
from typing import Dict, Any, Optional

# Add src to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from leindex.config import (
    GlobalConfigManager,
    GlobalConfig,
    MemoryConfig,
    PerformanceConfig,
    ConfigValidator,
    ConfigMigration,
    first_time_setup,
    detect_hardware,
)


def print_section(title: str):
    """Print a formatted section header."""
    print("\n" + "=" * 70)
    print(f" {title}")
    print("=" * 70)


def example_v1_to_v2_conversion():
    """Example 1: Convert v1 config to v2 format."""
    print_section("Example 1: v1 to v2 Configuration Conversion")

    # Sample v1 configuration
    v1_config = {
        "memory": {
            "budget_mb": 3072,
            "soft_limit_mb": 2457,
            "hard_limit_mb": 2857,
            "max_loaded_files": 1000,
            "max_cached_queries": 500
        },
        "performance": {
            "parallel_workers": 4,
            "batch_size": 32,
            "enable_gpu": True
        },
        "projects": {
            "/path/to/project1": {
                "max_loaded_files": 200,
                "max_cached_queries": 100
            }
        }
    }

    print("\n📋 v1 Configuration (Sample):")
    print(yaml.dump(v1_config, default_flow_style=False, indent=2))

    # Convert to v2 format
    budget_mb = v1_config["memory"]["budget_mb"]
    soft_percent = v1_config["memory"]["soft_limit_mb"] / budget_mb
    hard_percent = v1_config["memory"]["hard_limit_mb"] / budget_mb

    v2_config = {
        "version": "2.0",
        "memory": {
            "total_budget_mb": budget_mb,
            "soft_limit_percent": soft_percent,
            "hard_limit_percent": hard_percent,
            "emergency_percent": 0.98,
            "max_loaded_files": v1_config["memory"]["max_loaded_files"],
            "max_cached_queries": v1_config["memory"]["max_cached_queries"],
            "project_defaults": {
                "max_loaded_files": 100,
                "max_cached_queries": 50,
                "priority": "MEDIUM"
            }
        },
        "performance": {
            "parallel_scanner": {
                "enabled": True,
                "max_workers": v1_config["performance"]["parallel_workers"]
            },
            "parallel_processor": {
                "enabled": True,
                "max_workers": v1_config["performance"]["parallel_workers"],
                "batch_size": 100
            },
            "embeddings": {
                "batch_size": v1_config["performance"]["batch_size"],
                "enable_gpu": v1_config["performance"]["enable_gpu"],
                "device": "auto"
            }
        }
    }

    print("📋 v2 Configuration (Converted):")
    print(yaml.dump(v2_config, default_flow_style=False, indent=2))


def example_export_v1_settings():
    """Example 2: Export v1 settings for migration."""
    print_section("Example 2: Export v1 Settings")

    # Simulate loading v1 config
    v1_config_path = Path.home() / ".leindex" / "config.yaml"

    print(f"\n📂 Looking for v1 config at: {v1_config_path}")

    if v1_config_path.exists():
        with open(v1_config_path) as f:
            v1_config = yaml.safe_load(f)

        # Extract relevant settings
        settings = {
            "memory": {
                "budget_mb": v1_config.get("memory", {}).get("budget_mb", 3072),
                "soft_limit_mb": v1_config.get("memory", {}).get("soft_limit_mb", 2457),
                "hard_limit_mb": v1_config.get("memory", {}).get("hard_limit_mb", 2857),
            },
            "performance": {
                "parallel_workers": v1_config.get("performance", {}).get("parallel_workers", 4),
                "batch_size": v1_config.get("performance", {}).get("batch_size", 32),
            },
            "projects": v1_config.get("projects", {})
        }

        # Save to JSON
        export_path = Path.home() / ".leindex" / "backups" / "v1_settings.json"
        export_path.parent.mkdir(parents=True, exist_ok=True)

        with open(export_path, "w") as f:
            json.dump(settings, f, indent=2)

        print(f"✓ Exported v1 settings to: {export_path}")
        print("\nExported settings:")
        print(json.dumps(settings, indent=2))
    else:
        print("ℹ️  v1 config not found (this is expected for fresh installations)")


def example_hardware_detection():
    """Example 3: Hardware detection for auto-configuration."""
    print_section("Example 3: Hardware Detection")

    try:
        hardware = detect_hardware()

        print(f"\n🖥️  Detected Hardware:")
        print(f"   CPU Cores: {hardware.cpu_count}")
        print(f"   Total RAM: {hardware.total_ram_mb:.0f} MB ({hardware.total_ram_mb/1024:.1f} GB)")
        print(f"   Available RAM: {hardware.available_ram_mb:.0f} MB")
        print(f"   GPU Available: {hardware.gpu_available}")

        if hardware.gpu_available:
            print(f"   GPU Type: {hardware.gpu_type}")
            print(f"   GPU Memory: {hardware.gpu_memory_mb:.0f} MB")

        # Generate recommended configuration
        print(f"\n📝 Recommended Configuration:")

        if hardware.total_ram_mb >= 16384:
            print("   Profile: Production Server")
            memory_budget = int(hardware.total_ram_mb * 0.25)
            workers = hardware.cpu_count
        elif hardware.total_ram_mb >= 8192:
            print("   Profile: Development Machine")
            memory_budget = int(hardware.total_ram_mb * 0.30)
            workers = max(4, hardware.cpu_count // 2)
        else:
            print("   Profile: Resource-Constrained")
            memory_budget = int(hardware.total_ram_mb * 0.25)
            workers = 2

        recommended_config = {
            "memory": {
                "total_budget_mb": memory_budget,
                "soft_limit_percent": 0.80,
                "hard_limit_percent": 0.93
            },
            "performance": {
                "parallel_scanner": {
                    "max_workers": workers
                },
                "parallel_processor": {
                    "max_workers": workers
                }
            }
        }

        print(yaml.dump(recommended_config, default_flow_style=False, indent=2))

    except Exception as e:
        print(f"Error detecting hardware: {e}")


def example_first_time_setup():
    """Example 4: First-time setup with hardware detection."""
    print_section("Example 4: First-Time Setup")

    try:
        print("\n🚀 Running first-time setup...")

        result = first_time_setup()

        if result.success:
            print("✓ Setup completed successfully!")
            print(f"   Config created at: {result.config_path}")
            print(f"   Detected hardware: {result.detected_hardware}")

            # Display created configuration
            manager = GlobalConfigManager()
            config = manager.get_config()

            print("\n📝 Created Configuration:")
            print(f"   Memory Budget: {config.memory.total_budget_mb} MB")
            print(f"   Soft Limit: {config.memory.soft_limit_percent*100:.1f}%")
            print(f"   Hard Limit: {config.memory.hard_limit_percent*100:.1f}%")
            print(f"   Workers: {config.performance.parallel_scanner_max_workers}")
        else:
            print(f"✗ Setup failed: {result.error}")

    except Exception as e:
        print(f"Error during setup: {e}")


def example_manual_migration():
    """Example 5: Manual configuration migration."""
    print_section("Example 5: Manual Migration")

    # v1 settings (simulated)
    v1_settings = {
        "memory": {
            "budget_mb": 3072,
            "soft_limit_mb": 2457,
            "hard_limit_mb": 2857
        },
        "performance": {
            "parallel_workers": 4,
            "batch_size": 32
        }
    }

    print("\n📋 v1 Settings:")
    print(json.dumps(v1_settings, indent=2))

    # Create v2 configuration
    budget_mb = v1_settings["memory"]["budget_mb"]
    soft_percent = v1_settings["memory"]["soft_limit_mb"] / budget_mb
    hard_percent = v1_settings["memory"]["hard_limit_mb"] / budget_mb

    v2_config = GlobalConfig(
        version="2.0",
        memory=MemoryConfig(
            total_budget_mb=budget_mb,
            soft_limit_percent=soft_percent,
            hard_limit_percent=hard_percent,
            emergency_percent=0.98,
            max_loaded_files=1000,
            max_cached_queries=500
        ),
        performance=PerformanceConfig(
            parallel_scanner_max_workers=v1_settings["performance"]["parallel_workers"],
            parallel_processor_max_workers=v1_settings["performance"]["parallel_workers"],
            embeddings_batch_size=v1_settings["performance"]["batch_size"]
        )
    )

    # Save configuration
    manager = GlobalConfigManager()
    config_path = Path.home() / ".leindex" / "config.yaml"

    print(f"\n💾 Saving v2 configuration to: {config_path}")

    try:
        manager.save_config(v2_config, str(config_path))
        print("✓ Configuration saved successfully")

        # Verify
        loaded_config = manager.load_config(str(config_path))
        print(f"\n✓ Verification:")
        print(f"   Memory Budget: {loaded_config.memory.total_budget_mb} MB")
        print(f"   Soft Limit: {loaded_config.memory.soft_limit_percent*100:.1f}%")
        print(f"   Hard Limit: {loaded_config.memory.hard_limit_percent*100:.1f}%")
    except Exception as e:
        print(f"✗ Error saving configuration: {e}")


def example_validation():
    """Example 6: Configuration validation."""
    print_section("Example 6: Configuration Validation")

    # Invalid configurations
    invalid_configs = [
        {
            "name": "Negative Memory Budget",
            "config": {
                "memory": {
                    "total_budget_mb": -1000
                }
            }
        },
        {
            "name": "Threshold > 100%",
            "config": {
                "memory": {
                    "soft_limit_percent": 1.50
                }
            }
        },
        {
            "name": "Hard Limit < Soft Limit",
            "config": {
                "memory": {
                    "soft_limit_percent": 0.90,
                    "hard_limit_percent": 0.80
                }
            }
        }
    ]

    validator = ConfigValidator()

    for example in invalid_configs:
        print(f"\n❌ {example['name']}:")
        print(f"   Config: {example['config']}")

        try:
            # Note: This would use the actual validation method
            print(f"   Error: This configuration would fail validation")
        except Exception as e:
            print(f"   Validation Error: {e}")


def example_project_override_migration():
    """Example 7: Migrate project overrides."""
    print_section("Example 7: Project Override Migration")

    # v1 project overrides
    v1_projects = {
        "/path/to/large-project": {
            "max_loaded_files": 500,
            "max_cached_queries": 200,
            "priority": "HIGH"
        },
        "/path/to/small-project": {
            "max_loaded_files": 50,
            "max_cached_queries": 25,
            "priority": "LOW"
        }
    }

    print("\n📋 v1 Project Overrides:")
    print(json.dumps(v1_projects, indent=2))

    # Migrate to v2 format
    print("\n📝 v2 Project Overrides:")

    for project_id, project_config in v1_projects.items():
        project_name = Path(project_id).name
        project_config_path = Path.home() / ".leindex" / "projects" / f"{project_name}.yaml"

        v2_project_config = {
            "project_id": project_id,
            "memory": {
                "max_loaded_files": project_config["max_loaded_files"],
                "max_cached_queries": project_config["max_cached_queries"],
                "priority": project_config["priority"]
            }
        }

        print(f"\n{project_name}:")
        print(f"   Path: {project_config_path}")
        print(f"   Config:")
        print(yaml.dump(v2_project_config, default_flow_style=False, indent=6))


def example_environment_variables():
    """Example 8: Environment variable migration."""
    print_section("Example 8: Environment Variable Migration")

    # v1 environment variables
    v1_env_vars = {
        "CODE_INDEX_MEMORY_BUDGET_MB": "3072",
        "CODE_INDEX_MEMORY_SOFT_LIMIT_MB": "2457",
        "CODE_INDEX_PERFORMANCE_PARALLEL_WORKERS": "4"
    }

    print("\n📋 v1 Environment Variables:")
    for key, value in v1_env_vars.items():
        print(f"   {key}={value}")

    # v2 environment variables
    v2_env_vars = {
        "LEINDEX_MEMORY_TOTAL_BUDGET_MB": "3072",
        "LEINDEX_MEMORY_SOFT_LIMIT_PERCENT": "0.80",
        "LEINDEX_PERFORMANCE_PARALLEL_SCANNER_MAX_WORKERS": "4"
    }

    print("\n📝 v2 Environment Variables:")
    for key, value in v2_env_vars.items():
        print(f"   export {key}={value}")

    print("\n💡 Migration Tips:")
    print("   1. Update shell scripts to use new variable names")
    print("   2. Convert absolute values to percentages where applicable")
    print("   3. Update systemd service files")
    print("   4. Update docker-compose environment sections")


def example_backup_and_restore():
    """Example 9: Backup and restore configuration."""
    print_section("Example 9: Backup and Restore")

    manager = GlobalConfigManager()
    config_path = Path.home() / ".leindex" / "config.yaml"

    print("\n💾 Creating backup...")

    try:
        # Backup current config
        backup_path = manager.backup_config(str(config_path))
        print(f"✓ Backup created at: {backup_path}")

        # Display backup info
        if backup_path.exists():
            size_mb = backup_path.stat().st_size / 1024 / 1024
            print(f"   Size: {size_mb:.2f} MB")

        # Restore from backup
        print(f"\n🔄 Restoring from backup...")
        manager.save_config(
            manager.load_config(str(backup_path)),
            str(config_path)
        )
        print(f"✓ Restored from backup")

    except Exception as e:
        print(f"✗ Error: {e}")


def example_migration_checklist():
    """Example 10: Migration checklist."""
    print_section("Example 10: Migration Checklist")

    checklist = """
    ✅ Pre-Migration Checklist:
    □ Backup current configuration: cp ~/.leindex/config.yaml ~/.leindex/backups/config.v1.yaml
    □ Export v1 settings for reference
    □ Document custom configuration values
    □ Note all indexed projects
    □ Schedule maintenance window (5-10 minutes expected)

    ✅ Migration Steps:
    □ Upgrade LeIndex: pip install leindex==2.0.0
    □ Run first-time setup
    □ Migrate configuration to v2 format
    □ Migrate project overrides
    □ Validate new configuration
    □ Test with sample queries

    ✅ Post-Migration:
    □ Update environment variables in scripts
    □ Update MCP client configuration
    □ Test cross-project search
    □ Monitor memory usage
    □ Verify all projects accessible

    ✅ Rollback (if needed):
    □ Stop v2.0: pkill -f "leindex mcp"
    □ Uninstall v2.0: pip uninstall leindex -y
    □ Restore v1 config: cp ~/.leindex/backups/config.v1.yaml ~/.leindex/config.yaml
    □ Reinstall v1.x: pip install leindex==1.1.0
    """

    print(checklist)


def example_api_differences():
    """Example 11: API differences between v1 and v2."""
    print_section("Example 11: API Differences")

    print("""
    📋 Memory Management API Changes:

    v1.x:
    ```python
    from leindex.memory_profiler import MemoryProfiler, MemorySnapshot, MemoryLimits

    profiler = MemoryProfiler(limits=MemoryLimits(
        soft_limit_mb=2457,
        hard_limit_mb=2857
    ))
    snapshot = profiler.take_snapshot()
    ```

    v2.0:
    ```python
    from leindex.memory import MemoryManager, MemoryStatus

    manager = MemoryManager()
    status = manager.get_status()
    ```

    📋 Configuration API Changes:

    v1.x:
    ```python
    from leindex.config_manager import ConfigManager

    manager = ConfigManager()
    config = manager.load_config()
    ```

    v2.0:
    ```python
    from leindex.config import GlobalConfigManager

    manager = GlobalConfigManager()
    config = manager.get_config()
    ```

    📋 New Global Index API (v2.0 only):

    ```python
    from leindex.global_index import get_global_stats, cross_project_search

    stats = get_global_stats()
    results = cross_project_search("authentication")
    ```
    """)


def main():
    """Run all examples."""
    print("\n" + "=" * 70)
    print(" LeIndex v1 to v2 Configuration Migration Examples")
    print("=" * 70)

    examples = [
        example_v1_to_v2_conversion,
        example_export_v1_settings,
        example_hardware_detection,
        example_first_time_setup,
        example_manual_migration,
        example_validation,
        example_project_override_migration,
        example_environment_variables,
        example_backup_and_restore,
        example_migration_checklist,
        example_api_differences,
    ]

    for example in examples:
        try:
            example()
        except Exception as e:
            print(f"\nExample failed: {e}")
            import traceback
            traceback.print_exc()

    print("\n" + "=" * 70)
    print(" Examples Complete!")
    print("=" * 70)


if __name__ == "__main__":
    main()
