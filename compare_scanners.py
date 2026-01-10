#!/usr/bin/env python3
"""
Scanner Performance Comparison Test

Compares the old ParallelScanner against the new FastParallelScanner
to demonstrate performance improvements.

Usage:
    python3 compare_scanners.py /path/to/project
"""

import asyncio
import sys
import time
from pathlib import Path

# Add src to path
sys.path.insert(0, str(Path(__file__).parent / 'src'))

from leindex.parallel_scanner import ParallelScanner
from leindex.fast_scanner import FastParallelScanner
from leindex.ignore_patterns import IgnorePatternMatcher
from leindex.logger_config import setup_logger

# Setup logging
setup_logger(verbose=True)


async def test_scanner(scanner_class, scanner_name, project_path, ignore_matcher):
    """Test a single scanner and return results."""
    print(f"\n{'='*80}")
    print(f"Testing: {scanner_name}")
    print(f"{'='*80}\n")

    scanner = scanner_class(
        max_workers=4,
        timeout=120.0,
        ignore_matcher=ignore_matcher,
        debug_performance=True
    )

    start = time.time()
    try:
        results = await scanner.scan(project_path)
        elapsed = time.time() - start

        print(f"\n✓ SUCCESS")
        print(f"  Directories: {len(results)}")
        print(f"  Time: {elapsed:.2f}s")
        print(f"  Rate: {len(results) / elapsed:.1f} dirs/sec")

        stats = scanner.get_stats()
        if 'performance' in stats:
            perf = stats['performance']
            print(f"  Avg scandir: {perf['total_scandir_time'] / perf['scandir_calls'] * 1000:.2f}ms")
            print(f"  Slow scans: {perf['slow_scandirs']}")

        return len(results), elapsed

    except TimeoutError:
        elapsed = time.time() - start
        print(f"\n✗ TIMEOUT after {elapsed:.2f}s")
        return None, elapsed


async def main():
    """Main comparison test."""
    if len(sys.argv) < 2:
        print("Usage: python3 compare_scanners.py <project_path>")
        sys.exit(1)

    project_path = sys.argv[1]

    if not Path(project_path).exists():
        print(f"Error: Path does not exist: {project_path}")
        sys.exit(1)

    print(f"\n{'='*80}")
    print(f"SCANNER PERFORMANCE COMPARISON")
    print(f"{'='*80}")
    print(f"Project: {project_path}")
    print(f"{'='*80}")

    # Initialize ignore matcher (shared)
    print("\nInitializing ignore pattern matcher...")
    ignore_matcher = IgnorePatternMatcher(project_path)
    print(f"Loaded {len(ignore_matcher.get_patterns())} patterns")

    # Test old scanner
    old_count, old_time = await test_scanner(
        ParallelScanner,
        "Old ParallelScanner (task-per-directory)",
        project_path,
        ignore_matcher
    )

    # Test new scanner
    new_count, new_time = await test_scanner(
        FastParallelScanner,
        "New FastParallelScanner (work-queue)",
        project_path,
        ignore_matcher
    )

    # Print comparison
    print(f"\n{'='*80}")
    print(f"COMPARISON RESULTS")
    print(f"{'='*80}")

    if old_count and new_count:
        speedup = old_time / new_time
        print(f"Old scanner: {old_count} dirs in {old_time:.2f}s ({old_count/old_time:.1f} dirs/sec)")
        print(f"New scanner: {new_count} dirs in {new_time:.2f}s ({new_count/new_time:.1f} dirs/sec)")
        print(f"\n🚀 Speedup: {speedup:.1f}x faster")
        print(f"⏱️  Time saved: {old_time - new_time:.2f}s")
    elif new_count:
        print(f"Old scanner: TIMEOUT")
        print(f"New scanner: {new_count} dirs in {new_time:.2f}s ({new_count/new_time:.1f} dirs/sec)")
        print(f"\n🚀 New scanner succeeded where old timed out!")
    else:
        print("Both scanners timed out")

    print(f"{'='*80}\n")


if __name__ == '__main__':
    asyncio.run(main())
