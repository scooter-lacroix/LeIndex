#!/usr/bin/env python3
"""
Graceful Degradation Demo

This script demonstrates the graceful degradation functionality for global
index operations in LeIndex.

Usage:
    python examples/graceful_degradation_demo.py
"""

import sys
import os

# Add parent directory to path for imports
sys.path.insert(0, os.path.join(os.path.dirname(__file__), '..'))

from src.leindex.global_index.graceful_degradation import (
    DegradedStatus,
    FallbackResult,
    is_leann_available,
    is_tantivy_available,
    is_ripgrep_available,
    is_grep_available,
    fallback_from_leann,
    fallback_from_tantivy,
    fallback_to_ripgrep,
    fallback_to_grep,
    is_project_healthy,
    filter_healthy_projects,
    execute_with_degradation,
    get_backend_status,
    get_current_degradation_level
)


def print_section(title: str) -> None:
    """Print a section header."""
    print("\n" + "=" * 70)
    print(f"  {title}")
    print("=" * 70)


def print_backend_status() -> None:
    """Display backend availability status."""
    print_section("Backend Availability Status")

    status = get_backend_status()

    for backend, available in status.items():
        icon = "✅" if available else "❌"
        print(f"{icon} {backend:15} {'Available' if available else 'Unavailable'}")

    level = get_current_degradation_level()
    print(f"\n📊 Current degradation level: {level.value}")


def demo_backend_detection() -> None:
    """Demonstrate backend availability detection."""
    print_section("1. Backend Detection")

    backends = {
        "LEANN": is_leann_available(),
        "Tantivy": is_tantivy_available(),
        "ripgrep": is_ripgrep_available(),
        "grep": is_grep_available()
    }

    for name, available in backends.items():
        status = "✅ Available" if available else "❌ Unavailable"
        print(f"  {name:15} {status}")


def demo_degradation_levels() -> None:
    """Demonstrate degradation levels."""
    print_section("2. Degradation Levels")

    levels = [
        (DegradedStatus.FULL, "All backends operational"),
        (DegradedStatus.DEGRADED_LEANN_UNAVAILABLE, "LEANN unavailable, using Tantivy"),
        (DegradedStatus.DEGRADED_TANTIVY_UNAVAILABLE, "Tantivy unavailable, using ripgrep"),
        (DegradedStatus.DEGRADED_SEARCH_FALLBACK, "Only grep/ripgrep available"),
        (DegradedStatus.DEGRADED_NO_BACKEND, "No backends available")
    ]

    for status, description in levels:
        print(f"  {status.value:35} - {description}")


def demo_project_health() -> None:
    """Demonstrate project health checking."""
    print_section("3. Project Health Checking")

    # Check current directory as a project
    print("  Checking health of current directory...")
    healthy = is_project_healthy(
        project_id="demo_project",
        project_path=os.getcwd()
    )

    status = "✅ Healthy" if healthy else "❌ Unhealthy"
    print(f"  Status: {status}")

    # Check non-existent project
    print("\n  Checking health of non-existent project...")
    healthy = is_project_healthy(
        project_id="nonexistent",
        project_path="/nonexistent/path"
    )

    status = "✅ Healthy" if healthy else "❌ Unhealthy"
    print(f"  Status: {status}")


def demo_project_filtering() -> None:
    """Demonstrate filtering healthy projects."""
    print_section("4. Filter Healthy Projects")

    # Create test project list
    projects = [f"project_{i}" for i in range(1, 6)]
    paths = {p: os.getcwd() for p in projects}

    # Add a non-existent project
    projects.append("nonexistent_project")
    paths["nonexistent_project"] = "/nonexistent/path"

    print(f"  Checking {len(projects)} projects...")

    healthy, unhealthy = filter_healthy_projects(
        project_ids=projects,
        project_paths=paths
    )

    print(f"\n  ✅ Healthy projects ({len(healthy)}):")
    for p in healthy:
        print(f"    - {p}")

    print(f"\n  ❌ Unhealthy projects ({len(unhealthy)}):")
    for p in unhealthy:
        print(f"    - {p}")


def demo_execute_with_degradation() -> None:
    """Demonstrate executing queries with automatic degradation."""
    print_section("5. Execute with Degradation")

    print("  Executing search query with automatic fallback...")
    print("  Query: 'def test'")
    print("  Base path: .")

    result = execute_with_degradation(
        operation="demo_search",
        query_pattern="def test",
        base_path=os.getcwd(),
        case_sensitive=False
    )

    print(f"\n  📊 Results:")
    print(f"    Backend used:  {result['backend_used']}")
    print(f"    Status:        {result['degraded_status']}")
    print(f"    Duration:      {result.get('duration_ms', 0):.2f}ms")
    print(f"    Result count:  {len(result.get('results', {}))}")

    if result.get('fallback_reason'):
        print(f"    Fallback:      {result['fallback_reason']}")


def demo_fallback_chain() -> None:
    """Demonstrate the fallback chain."""
    print_section("6. Fallback Chain Demo")

    print("  Simulating fallback chain: LEANN → Tantivy → ripgrep → grep")

    # Start with LEANN (which will likely fall back)
    result = fallback_from_leann(
        operation="demo_search",
        query_pattern="import",
        base_path=os.getcwd()
    )

    print(f"\n  📊 Fallback Result:")
    print(f"    Status:        {result.status.value}")
    print(f"    Original:      {result.original_backend}")
    print(f"    Actual:        {result.actual_backend}")
    print(f"    Reason:        {result.fallback_reason or 'No fallback needed'}")


def main() -> None:
    """Run all demo sections."""
    print("\n" + "╔" + "═" * 68 + "╗")
    print("║" + " " * 15 + "GRACEFUL DEGRADATION DEMO" + " " * 24 + "║")
    print("╚" + "═" * 68 + "╝")

    try:
        print_backend_status()
        demo_backend_detection()
        demo_degradation_levels()
        demo_project_health()
        demo_project_filtering()
        demo_execute_with_degradation()
        demo_fallback_chain()

        print_section("Demo Complete")
        print("  All graceful degradation features demonstrated successfully!")
        print("  Check the code in examples/graceful_degradation_demo.py")

    except Exception as e:
        print(f"\n❌ Error during demo: {e}")
        import traceback
        traceback.print_exc()
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
