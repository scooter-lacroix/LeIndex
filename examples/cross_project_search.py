#!/usr/bin/env python3
"""
Cross-Project Search Examples for LeIndex v2.0

This example demonstrates how to use the Global Index for searching
across multiple projects simultaneously.

Usage:
    python cross_project_search.py
"""

import sys
from pathlib import Path
from typing import List, Optional

# Add src to path for imports
sys.path.insert(0, str(Path(__file__).parent.parent / "src"))

from leindex.global_index import (
    cross_project_search,
    CrossProjectSearchResult,
    get_global_stats,
    list_projects,
    get_dashboard_data,
    get_project_comparison,
    get_language_distribution,
    ProjectNotFoundError,
    AllProjectsFailedError,
    InvalidPatternError,
)


def example_basic_search():
    """Example 1: Basic cross-project search."""
    print("=" * 60)
    print("Example 1: Basic Cross-Project Search")
    print("=" * 60)

    try:
        # Search across all projects
        results: CrossProjectSearchResult = cross_project_search(
            pattern="authentication",
            fuzzy=True,
            case_sensitive=False
        )

        print(f"\nTotal Results: {results.total_results}")
        print(f"Successful Projects: {results.successful_projects}")
        print(f"Failed Projects: {results.failed_projects}")

        # Display results from each project
        for project_result in results.project_results:
            print(f"\n{project_result.project_id}:")
            print(f"  Status: {project_result.status}")
            print(f"  Matches: {project_result.matches}")

            # Show first 3 results
            for i, match in enumerate(project_result.results[:3]):
                print(f"  [{i+1}] {match.file_path}:{match.line_number}")
                print(f"      Score: {match.score:.3f}")
                if match.context:
                    print(f"      Context: {match.context[0][:60]}...")

    except Exception as e:
        print(f"Error: {e}")


def example_filtered_search():
    """Example 2: Search with project filtering."""
    print("\n" + "=" * 60)
    print("Example 2: Search with Project Filtering")
    print("=" * 60)

    try:
        # List available projects first
        projects = list_projects(format="simple")
        print(f"\nAvailable Projects: {projects['count']}")
        for project in projects['projects'][:5]:
            print(f"  - {project['name']} ({project['id']})")

        # Search only specific projects
        project_ids = [
            "/path/to/project1",
            "/path/to/project2"
        ]

        results = cross_project_search(
            pattern="database connection",
            project_ids=project_ids,
            fuzzy=True,
            case_sensitive=False,
            context_lines=2,
            max_results_per_project=50
        )

        print(f"\nFiltered Search Results:")
        for project_result in results.project_results:
            print(f"\n{project_result.project_id}:")
            print(f"  Matches: {project_result.matches}")
            for match in project_result.results[:2]:
                print(f"  - {match.file_path}:{match.line_number}")
                if match.context:
                    for line in match.context:
                        print(f"    {line}")

    except ProjectNotFoundError as e:
        print(f"Project not found: {e.project_id}")
    except Exception as e:
        print(f"Error: {e}")


def example_pattern_matching():
    """Example 3: Advanced pattern matching."""
    print("\n" + "=" * 60)
    print("Example 3: Advanced Pattern Matching")
    print("=" * 60)

    try:
        # Regex pattern search
        results = cross_project_search(
            pattern=r"class\s+\w*User\w*",  # Match User-related classes
            fuzzy=False,
            case_sensitive=True,
            file_pattern="*.py"  # Only Python files
        )

        print(f"\nPattern: r'class\\s+\\w*User\\w*'")
        print(f"File Filter: *.py")
        print(f"Total Matches: {results.total_results}")

        for project_result in results.project_results:
            if project_result.matches > 0:
                print(f"\n{project_result.project_id}:")
                for match in project_result.results[:3]:
                    print(f"  - {match.file_path}:{match.line_number}")

    except InvalidPatternError as e:
        print(f"Invalid pattern: {e.pattern}")
    except Exception as e:
        print(f"Error: {e}")


def example_fuzzy_search():
    """Example 4: Fuzzy search with different levels."""
    print("\n" + "=" * 60)
    print("Example 4: Fuzzy Search with Different Levels")
    print("=" * 60)

    search_term = "autentication"  # Typo: missing 'h'

    print(f"Search term: '{search_term}' (typo intended)")

    # Exact match (no results expected)
    print("\n1. Exact Match (fuzzy=False):")
    try:
        results = cross_project_search(
            pattern=search_term,
            fuzzy=False
        )
        print(f"   Results: {results.total_results}")
    except Exception as e:
        print(f"   Error: {e}")

    # Fuzzy match (results expected)
    print("\n2. Fuzzy Match (fuzzy=True):")
    try:
        results = cross_project_search(
            pattern=search_term,
            fuzzy=True,
            case_sensitive=False
        )
        print(f"   Results: {results.total_results}")
        print("   Found 'authentication' despite typo!")
    except Exception as e:
        print(f"   Error: {e}")


def example_global_statistics():
    """Example 5: Get global statistics."""
    print("\n" + "=" * 60)
    print("Example 5: Global Statistics")
    print("=" * 60)

    try:
        stats = get_global_stats()

        print(f"\nGlobal Index Statistics:")
        print(f"  Total Projects: {stats.total_projects}")
        print(f"  Total Symbols: {stats.total_symbols:,}")
        print(f"  Total Files: {stats.total_files:,}")
        print(f"  Average Health Score: {stats.average_health_score:.2f}")
        print(f"  Total Size: {stats.total_size_mb:.1f} MB")

        print(f"\nLanguage Distribution:")
        for language, count in sorted(stats.languages.items(), key=lambda x: -x[1]):
            percentage = (count / stats.total_files) * 100
            print(f"  {language}: {count:,} files ({percentage:.1f}%)")

    except Exception as e:
        print(f"Error: {e}")


def example_project_dashboard():
    """Example 6: Project comparison dashboard."""
    print("\n" + "=" * 60)
    print("Example 6: Project Comparison Dashboard")
    print("=" * 60)

    try:
        # Get dashboard with filters
        dashboard = get_dashboard_data(
            language="Python",
            min_health_score=0.7,
            sort_by="health_score",
            sort_order="descending"
        )

        print(f"\nPython Projects (health_score >= 0.7):")
        print(f"Total Projects: {dashboard.total_projects}")
        print(f"Total Symbols: {dashboard.total_symbols:,}")
        print(f"Average Health: {dashboard.average_health_score:.2f}")

        print(f"\nTop 5 Projects by Health Score:")
        for i, project in enumerate(dashboard.projects[:5], 1):
            print(f"  {i}. {project.name}")
            print(f"     Health: {project.health_score:.2f}")
            print(f"     Files: {project.file_count:,}")
            print(f"     Symbols: {project.symbol_count:,}")

    except Exception as e:
        print(f"Error: {e}")


def example_project_comparison():
    """Example 7: Compare specific projects."""
    print("\n" + "=" * 60)
    print("Example 7: Project Comparison")
    print("=" * 60)

    try:
        # Get all projects first
        projects = list_projects(format="simple")
        if len(projects['projects']) < 2:
            print("Need at least 2 projects for comparison")
            return

        # Compare first 2 projects
        project_ids = [p['id'] for p in projects['projects'][:2]]
        print(f"Comparing projects:")
        for pid in project_ids:
            print(f"  - {pid}")

        comparison = get_project_comparison(
            project_ids=project_ids,
            metrics=["file_count", "symbol_count", "health_score"]
        )

        print(f"\nComparison Results:")
        for project_id, metrics in comparison.items():
            print(f"\n{project_id}:")
            for metric, value in metrics.items():
                print(f"  {metric}: {value}")

    except Exception as e:
        print(f"Error: {e}")


def example_language_distribution():
    """Example 8: Language distribution analysis."""
    print("\n" + "=" * 60)
    print("Example 8: Language Distribution")
    print("=" * 60)

    try:
        distribution = get_language_distribution()

        print(f"\nLanguage Distribution Across All Projects:")
        print(f"{'Language':<20} {'Files':>12} {'Percentage':>12}")
        print("-" * 46)

        total_files = sum(count for _, count in distribution)
        for language, count in sorted(distribution, key=lambda x: -x[1]):
            percentage = (count / total_files) * 100
            print(f"{language:<20} {count:>12,} {percentage:>11.1f}%")

    except Exception as e:
        print(f"Error: {e}")


def example_error_handling():
    """Example 9: Error handling."""
    print("\n" + "=" * 60)
    print("Example 9: Error Handling")
    print("=" * 60)

    # Invalid project ID
    print("\n1. Invalid Project ID:")
    try:
        results = cross_project_search(
            pattern="test",
            project_ids=["/nonexistent/project"]
        )
    except ProjectNotFoundError as e:
        print(f"   Caught ProjectNotFoundError: {e.project_id}")

    # Invalid pattern
    print("\n2. Invalid Pattern:")
    try:
        results = cross_project_search(
            pattern="[invalid(",  # Invalid regex
            fuzzy=False
        )
    except InvalidPatternError as e:
        print(f"   Caught InvalidPatternError: {e.pattern}")

    # All projects fail
    print("\n3. All Projects Fail:")
    try:
        results = cross_project_search(
            pattern="test",
            project_ids=["/nonexistent1", "/nonexistent2"]
        )
    except AllProjectsFailedError as e:
        print(f"   Caught AllProjectsFailedError")
        print(f"   Attempted: {len(e.attempted_projects)} projects")


def example_performance_tips():
    """Example 10: Performance optimization tips."""
    print("\n" + "=" * 60)
    print("Example 10: Performance Tips")
    print("=" * 60)

    print("""
To optimize cross-project search performance:

1. Use project_ids to filter relevant projects:
   results = cross_project_search(
       "authentication",
       project_ids=["project1", "project2"]  # Faster than searching all
   )

2. Enable fuzzy search only when needed:
   fuzzy=False  # Faster (exact match)
   fuzzy=True   # Slower (tolerates typos)

3. Use appropriate max_results_per_project:
   max_results_per_project=50   # Faster (less data)
   max_results_per_project=500  # Slower (more data)

4. Filter by file pattern when possible:
   file_pattern="*.py"  # Faster (only searches Python files)

5. Use Tier 2 cache for repeated queries:
   # First query (slow, creates cache)
   results1 = cross_project_search("authentication")

   # Second query (fast, uses cache)
   results2 = cross_project_search("authentication")

6. Monitor health scores and avoid unhealthy projects:
   projects = list_projects(min_health_score=0.8)
   healthy_ids = [p['id'] for p in projects['projects']]
   results = cross_project_search("test", project_ids=healthy_ids)
    """)


def main():
    """Run all examples."""
    print("\n" + "=" * 60)
    print("LeIndex v2.0 - Cross-Project Search Examples")
    print("=" * 60)

    examples = [
        example_basic_search,
        example_filtered_search,
        example_pattern_matching,
        example_fuzzy_search,
        example_global_statistics,
        example_project_dashboard,
        example_project_comparison,
        example_language_distribution,
        example_error_handling,
        example_performance_tips,
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
