#!/usr/bin/env python3
"""
Dashboard Usage Examples for LeIndex v2.0

This example demonstrates how to use the project comparison dashboard
and analytics features of LeIndex v2.0.

Usage:
    python dashboard_usage.py
"""

import sys
from pathlib import Path
from typing import List, Dict, Any

# Add src to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from leindex.global_index import (
    get_dashboard_data,
    get_project_comparison,
    get_language_distribution,
    list_projects,
    get_global_stats,
    DashboardData,
    ProjectMetadata,
)


def print_section(title: str):
    """Print a formatted section header."""
    print("\n" + "=" * 70)
    print(f" {title}")
    print("=" * 70)


def example_basic_dashboard():
    """Example 1: Basic dashboard with all projects."""
    print_section("Example 1: Basic Dashboard")

    try:
        dashboard = get_dashboard_data()

        print(f"\n📊 Global Dashboard Overview")
        print(f"   Total Projects: {dashboard.total_projects}")
        print(f"   Total Symbols: {dashboard.total_symbols:,}")
        print(f"   Total Files: {dashboard.total_files:,}")
        print(f"   Average Health Score: {dashboard.average_health_score:.2f}")
        print(f"   Total Size: {dashboard.total_size_mb:.1f} MB")
        print(f"   Last Updated: {dashboard.last_updated}")

        print(f"\n   Language Distribution:")
        for language, count in sorted(dashboard.languages.items(), key=lambda x: -x[1])[:5]:
            percentage = (count / dashboard.total_files) * 100
            print(f"     {language}: {count:,} files ({percentage:.1f}%)")

    except Exception as e:
        print(f"Error: {e}")


def example_filtered_dashboard():
    """Example 2: Filtered dashboard by language and health."""
    print_section("Example 2: Filtered Dashboard (Python, Health >= 0.8)")

    try:
        dashboard = get_dashboard_data(
            language="Python",
            min_health_score=0.8,
            sort_by="health_score",
            sort_order="descending"
        )

        print(f"\n📊 Python Projects (Health >= 0.8)")
        print(f"   Total Projects: {dashboard.total_projects}")
        print(f"   Average Health: {dashboard.average_health_score:.2f}")

        print(f"\n   Top 5 Projects by Health Score:")
        for i, project in enumerate(dashboard.projects[:5], 1):
            print(f"\n   {i}. {project.name}")
            print(f"      Path: {project.path}")
            print(f"      Health: {project.health_score:.2f} ⭐")
            print(f"      Files: {project.file_count:,}")
            print(f"      Symbols: {project.symbol_count:,}")
            print(f"      Language: {project.primary_language}")
            print(f"      Last Indexed: {project.last_indexed}")

    except Exception as e:
        print(f"Error: {e}")


def example_size_comparison():
    """Example 3: Compare projects by size."""
    print_section("Example 3: Projects Sorted by Size")

    try:
        dashboard = get_dashboard_data(
            sort_by="size_mb",
            sort_order="descending"
        )

        print(f"\n📊 Top 5 Largest Projects")
        print(f"{'Project':<30} {'Files':>10} {'Symbols':>12} {'Size (MB)':>10}")
        print("-" * 70)

        for project in dashboard.projects[:5]:
            print(f"{project.name:<30} {project.file_count:>10,} "
                  f"{project.symbol_count:>12,} {project.size_mb:>10.1f}")

    except Exception as e:
        print(f"Error: {e}")


def example_recently_indexed():
    """Example 4: Recently indexed projects."""
    print_section("Example 4: Recently Indexed Projects")

    try:
        dashboard = get_dashboard_data(
            sort_by="last_indexed",
            sort_order="descending"
        )

        print(f"\n📊 5 Most Recently Indexed Projects")
        print(f"{'Project':<30} {'Last Indexed':>25} {'Health':>8}")
        print("-" * 70)

        for project in dashboard.projects[:5]:
            # Format timestamp nicely
            from datetime import datetime
            dt = datetime.fromtimestamp(project.last_indexed)
            time_str = dt.strftime("%Y-%m-%d %H:%M")

            health_emoji = "⭐" if project.health_score >= 0.8 else "✓" if project.health_score >= 0.6 else "⚠️"

            print(f"{project.name:<30} {time_str:>25} {health_emoji:>8}")

    except Exception as e:
        print(f"Error: {e}")


def example_health_analysis():
    """Example 5: Health score analysis."""
    print_section("Example 5: Health Score Analysis")

    try:
        dashboard = get_dashboard_data()

        # Analyze health distribution
        health_ranges = {
            "Excellent (0.9-1.0)": 0,
            "Good (0.8-0.9)": 0,
            "Fair (0.7-0.8)": 0,
            "Poor (0.6-0.7)": 0,
            "Critical (<0.6)": 0
        }

        for project in dashboard.projects:
            score = project.health_score
            if score >= 0.9:
                health_ranges["Excellent (0.9-1.0)"] += 1
            elif score >= 0.8:
                health_ranges["Good (0.8-0.9)"] += 1
            elif score >= 0.7:
                health_ranges["Fair (0.7-0.8)"] += 1
            elif score >= 0.6:
                health_ranges["Poor (0.6-0.7)"] += 1
            else:
                health_ranges["Critical (<0.6)"] += 1

        print(f"\n📊 Health Score Distribution")
        for range_name, count in health_ranges.items():
            percentage = (count / dashboard.total_projects) * 100
            bar = "█" * int(percentage / 2)
            print(f"   {range_name:<20} {bar:<50} {count:>3} ({percentage:>5.1f}%)")

    except Exception as e:
        print(f"Error: {e}")


def example_project_comparison():
    """Example 6: Compare specific projects."""
    print_section("Example 6: Project Comparison")

    try:
        # Get all projects first
        projects = list_projects(format="simple")
        if len(projects['projects']) < 2:
            print("Need at least 2 projects for comparison")
            return

        # Compare first 3 projects
        project_ids = [p['id'] for p in projects['projects'][:3]]
        project_names = [p['name'] for p in projects['projects'][:3]]

        comparison = get_project_comparison(
            project_ids=project_ids,
            metrics=["file_count", "symbol_count", "health_score", "size_mb"]
        )

        print(f"\n📊 Comparing {len(project_ids)} Projects")

        # Print table header
        print(f"\n{'Metric':<20} " + " ".join(f"{name:>15}" for name in project_names))
        print("-" * (20 + 15 * len(project_names)))

        # Print each metric
        metrics = ["file_count", "symbol_count", "health_score", "size_mb"]
        metric_names = ["File Count", "Symbol Count", "Health Score", "Size (MB)"]

        for metric, name in zip(metrics, metric_names):
            row = [f"{name:<20}"]
            for project_id in project_ids:
                value = comparison[project_id].get(metric, 0)
                if metric == "health_score":
                    row.append(f"{value:>15.2f}")
                elif metric in ["file_count", "symbol_count"]:
                    row.append(f"{value:>15,}")
                else:
                    row.append(f"{value:>15.1f}")
            print(" ".join(row))

    except Exception as e:
        print(f"Error: {e}")


def example_language_distribution():
    """Example 7: Language distribution analysis."""
    print_section("Example 7: Language Distribution")

    try:
        distribution = get_language_distribution()

        print(f"\n📊 Language Distribution Across All Projects")
        print(f"{'Language':<20} {'Files':>12} {'Percentage':>12} {'Bar':<30}")
        print("-" * 80)

        total_files = sum(count for _, count in distribution)

        for language, count in sorted(distribution, key=lambda x: -x[1]):
            percentage = (count / total_files) * 100
            bar_length = int(percentage / 2)
            bar = "█" * bar_length
            print(f"{language:<20} {count:>12,} {percentage:>11.1f}% {bar:<30}")

    except Exception as e:
        print(f"Error: {e}")


def example_status_filtering():
    """Example 8: Filter by index status."""
    print_section("Example 8: Projects by Index Status")

    statuses = ["completed", "building", "error", "partial"]

    for status in statuses:
        try:
            dashboard = get_dashboard_data(status=status)

            if dashboard.total_projects > 0:
                print(f"\n📊 Status: {status.upper()}")
                print(f"   Count: {dashboard.total_projects}")

                for project in dashboard.projects[:3]:
                    print(f"     - {project.name} ({project.path})")

        except Exception as e:
            print(f"Error filtering by {status}: {e}")


def example_combined_filters():
    """Example 9: Combined filters."""
    print_section("Example 9: Combined Filters (Python + Large + Healthy)")

    try:
        dashboard = get_dashboard_data(
            language="Python",
            min_health_score=0.8,
            sort_by="size_mb",
            sort_order="descending"
        )

        print(f"\n📊 Large, Healthy Python Projects")
        print(f"   Criteria: Python, Health >= 0.8")
        print(f"   Found: {dashboard.total_projects} projects")

        if dashboard.total_projects > 0:
            print(f"\n   {'Project':<30} {'Size (MB)':>12} {'Health':>8} {'Files':>10}")
            print("-" * 70)

            for project in dashboard.projects[:5]:
                health_emoji = "⭐" if project.health_score >= 0.9 else "✓"
                print(f"{project.name:<30} {project.size_mb:>12.1f} "
                      f"{health_emoji:>8} {project.file_count:>10,}")

    except Exception as e:
        print(f"Error: {e}")


def example_analytics_insights():
    """Example 10: Analytics insights."""
    print_section("Example 10: Analytics Insights")

    try:
        stats = get_global_stats()
        dashboard = get_dashboard_data()

        print(f"\n📊 Key Insights")

        # 1. Average project size
        if dashboard.total_projects > 0:
            avg_files = dashboard.total_files / dashboard.total_projects
            avg_symbols = dashboard.total_symbols / dashboard.total_projects
            avg_size = dashboard.total_size_mb / dashboard.total_projects

            print(f"\n   Average Project Size:")
            print(f"     Files: {avg_files:.0f}")
            print(f"     Symbols: {avg_symbols:.0f}")
            print(f"     Size: {avg_size:.1f} MB")

        # 2. Health score insights
        healthy_count = sum(1 for p in dashboard.projects if p.health_score >= 0.8)
        healthy_percentage = (healthy_count / dashboard.total_projects) * 100

        print(f"\n   Health Score:")
        print(f"     Healthy Projects (>= 0.8): {healthy_count}/{dashboard.total_projects} ({healthy_percentage:.1f}%)")
        print(f"     Average Health: {dashboard.average_health_score:.2f}")

        # 3. Language diversity
        language_count = len(dashboard.languages)
        dominant_language = max(dashboard.languages.items(), key=lambda x: x[1])
        dominant_percentage = (dominant_language[1] / dashboard.total_files) * 100

        print(f"\n   Language Diversity:")
        print(f"     Languages Used: {language_count}")
        print(f"     Dominant: {dominant_language[0]} ({dominant_percentage:.1f}%)")

        # 4. Size distribution
        sizes = [p.size_mb for p in dashboard.projects]
        if sizes:
            print(f"\n   Size Distribution:")
            print(f"     Smallest: {min(sizes):.1f} MB")
            print(f"     Largest: {max(sizes):.1f} MB")
            print(f"     Median: {sorted(sizes)[len(sizes)//2]:.1f} MB")

    except Exception as e:
        print(f"Error: {e}")


def example_export_dashboard():
    """Example 11: Export dashboard data."""
    print_section("Example 11: Export Dashboard Data")

    try:
        dashboard = get_dashboard_data()

        # Export to JSON-like structure
        export_data = {
            "timestamp": dashboard.last_updated,
            "summary": {
                "total_projects": dashboard.total_projects,
                "total_files": dashboard.total_files,
                "total_symbols": dashboard.total_symbols,
                "average_health": dashboard.average_health_score,
                "total_size_mb": dashboard.total_size_mb
            },
            "languages": dashboard.languages,
            "projects": []
        }

        for project in dashboard.projects[:5]:  # First 5 projects
            export_data["projects"].append({
                "name": project.name,
                "path": project.path,
                "primary_language": project.primary_language,
                "file_count": project.file_count,
                "symbol_count": project.symbol_count,
                "health_score": project.health_score,
                "size_mb": project.size_mb,
                "last_indexed": project.last_indexed
            })

        print(f"\n📊 Export Data Preview (First 5 Projects):")
        print(f"\n   Timestamp: {export_data['timestamp']}")
        print(f"   Summary:")
        for key, value in export_data['summary'].items():
            print(f"     {key}: {value}")

        print(f"\n   Projects:")
        for i, project in enumerate(export_data['projects'], 1):
            print(f"     {i}. {project['name']} ({project['primary_language']})")
            print(f"        Health: {project['health_score']:.2f}, Files: {project['file_count']:,}")

        print(f"\n   (Full export would include all {dashboard.total_projects} projects)")

    except Exception as e:
        print(f"Error: {e}")


def example_mcp_tool_usage():
    """Example 12: MCP tool usage examples."""
    print_section("Example 12: MCP Tool Usage")

    print("""
The following MCP tools are available for dashboard functionality:

1. get_dashboard
   {
     "name": "get_dashboard",
     "arguments": {
       "language": "Python",           // Optional: filter by language
       "min_health_score": 0.8,        // Optional: minimum health score
       "max_health_score": 1.0,        // Optional: maximum health score
       "sort_by": "health_score",      // Optional: sort field
       "sort_order": "descending"      // Optional: sort order
     }
   }

2. get_project_comparison
   {
     "name": "get_project_comparison",
     "arguments": {
       "project_ids": [                // Required: list of project IDs
         "/path/to/project1",
         "/path/to/project2"
       ],
       "metrics": [                    // Optional: metrics to compare
         "file_count",
         "symbol_count",
         "health_score"
       ]
     }
   }

3. get_language_distribution
   {
     "name": "get_language_distribution",
     "arguments": {}                   // No arguments required
   }

4. list_projects
   {
     "name": "list_projects",
     "arguments": {
       "status": "completed",          // Optional: filter by status
       "language": "Python",           // Optional: filter by language
       "min_health_score": 0.8,        // Optional: minimum health score
       "format": "detailed"            // Optional: "simple" or "detailed"
     }
   }

5. get_global_stats
   {
     "name": "get_global_stats",
     "arguments": {}                   // No arguments required
   }

Example MCP client usage (in Claude Desktop config.json):

{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {}
    }
  }
}

Then in Claude:
> "Show me all Python projects sorted by health score"
> "Compare project-a and project-b"
> "What's the language distribution across all projects?"
    """)


def main():
    """Run all examples."""
    print("\n" + "=" * 70)
    print(" LeIndex v2.0 - Dashboard Usage Examples")
    print("=" * 70)

    examples = [
        example_basic_dashboard,
        example_filtered_dashboard,
        example_size_comparison,
        example_recently_indexed,
        example_health_analysis,
        example_project_comparison,
        example_language_distribution,
        example_status_filtering,
        example_combined_filters,
        example_analytics_insights,
        example_export_dashboard,
        example_mcp_tool_usage,
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
