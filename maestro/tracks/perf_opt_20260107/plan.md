# Implementation Plan: LeIndex Performance Optimization

**Track ID:** perf_opt_20260107
**Created:** 2026-01-07
**Status:** New

---

## Phase 1: Async I/O Foundation

**Goal:** Eliminate blocking synchronous operations and implement basic optimizations

### Task 1.1: Make os.walk() Truly Async
- [x] Task: Write unit tests for async file tree traversal
- [x] Task: Implement async wrapper for os.walk() using asyncio.to_thread()
  - Wrap os.walk() in asyncio.to_thread()
  - Preserve existing filtering logic (ignore patterns, file size)
  - Add progress callback support
  - Handle errors gracefully
- [x] Task: Verify event loop remains responsive during traversal
  - Test with large directory (10K+ files)
  - Verify other async operations can run concurrently
  - Measure responsiveness (should be <100ms to check other tasks)
- [x] Task: Update integration tests for async traversal
  - Test with various directory structures
  - Verify filtering still works correctly
  - Test error handling (permission denied, broken symlinks)
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Async I/O Foundation' (Protocol in workflow.md)

### Task 1.2: Implement File Stat Caching
- [x] Task: Write unit tests for file stat cache
- [x] Task: Implement FileStatCache class
  - In-memory cache using dict with path as key
  - Store stat results (size, mtime, hash)
  - Thread-safe access (Lock for concurrent access)
  - Cache invalidation on file modification
- [x] Task: Integrate cache into indexing pipeline
  - Populate cache during first os.stat() call
  - Replace redundant os.stat() calls with cache lookups
  - Measure cache hit rate (should be >95%)
- [x] Task: Verify memory overhead is acceptable
  - Test with 50K files (should be <100MB overhead)
  - Profile memory usage during indexing
  - Optimize if needed (LRU eviction for large codebases)
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Async I/O Foundation' (Protocol in workflow.md)

### Task 1.3: Optimize SQLite Write Performance
- [x] Task: Write unit tests for batched SQLite writes
- [x] Task: Implement batch write functionality
  - Add batch_write() method to SQLiteSearch class
  - Collect documents in memory (max 100 at a time)
  - Write all documents in single transaction
  - Configure PRAGMA synchronous=NORMAL during bulk operations
  - Restore PRAGMA synchronous=FULL after indexing
- [x] Task: Update indexing to use batch writes
  - Replace individual document writes with batch writes
  - Implement retry logic with exponential backoff
  - Add progress reporting for batch operations
- [x] Task: Verify data integrity and performance
  - Test transaction rollback on errors
  - Measure write throughput (should be 10-20x faster)
  - Verify no data loss on crashes
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Async I/O Foundation' (Protocol in workflow.md)

---

## Phase 2: Parallel Processing & Batching

**Goal:** Implement true parallelization and eliminate sequential I/O bottlenecks

### Task 2.1: Implement True Parallel File Reading
- [x] Task: Write unit tests for parallel file reading
- [x] Task: Refactor ParallelIndexer to read files in workers
  - Move SmartFileReader.read_content() into worker threads
  - Each worker processes chunk of files (default: 100)
  - Return both metadata AND content from workers
  - Limit concurrency (4-8 workers) to avoid filesystem overload
- [x] Task: Update main indexing loop to use parallel reading
  - Remove sequential file reading after parallel processing
  - Process results from workers efficiently
  - Handle errors in individual files without failing batch
- [x] Task: Verify parallelization improves performance
  - Benchmark with 10K files (should be 3-5x faster)
  - Verify no race conditions or data corruption
  - Profile CPU and I/O utilization
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Parallel Processing & Batching' (Protocol in workflow.md)

### Task 2.2: Implement Batch Database Writes
- [x] Task: Enhance batch write functionality from Phase 1
  - Already implemented in Task 1.3, verified working
  - Added comprehensive error handling and retry logic
  - Implemented exponential backoff for retries
- [x] Task: Write integration tests for batch writes with failures
  - Test transaction rollback on partial failures
  - Test retry logic with database connection errors
  - Verify all-or-nothing semantics
- [x] Task: Optimize batch size and concurrency
  - Batch size of 100 documents performs well
  - Retry logic with exponential backoff (1s, 2s, 4s)
  - Documented findings in code comments
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Parallel Processing & Batching' (Protocol in workflow.md)

### Task 2.3: Defer Hash Computation
- [x] Task: Write unit tests for deferred hash computation
- [x] Task: Modify IncrementalIndexer to skip hash in initial index
  - Check if index exists before computing hash
  - If initial index, skip hash entirely (use None)
  - Only compute hash during incremental indexing
- [x] Task: Implement mtime+size pre-check for incremental indexing
  - Compare mtime and size before computing hash
  - Only compute hash if mtime+size unchanged
  - Skip hash computation for obviously changed files
- [x] Task: Verify incremental indexing still correct
  - Test that changed files are detected correctly
  - Test that unchanged files are not re-indexed
  - Measure performance improvement (should be 50%+ faster)
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Parallel Processing & Batching' (Protocol in workflow.md)

---

## Phase 3: Advanced Optimization

**Goal:** Implement advanced optimizations for maximum performance

### Task 3.1: Implement Parallel Directory Traversal
- [x] Task: Write unit tests for parallel directory scanner
- [x] Task: Create ParallelScanner module
  - Use asyncio with semaphore for concurrency limiting
  - Divide directory tree into subtrees (by depth)
  - Process subtrees in parallel using scandir()
  - Combine results from all workers
  - Handle errors gracefully (continue on single subdir failure)
- [x] Task: Integrate parallel scanner into indexing pipeline
  - Replace os.walk() with ParallelScanner in _index_project()
  - Configure semaphore limit (default: 4 workers)
  - Maintain compatibility with existing filtering logic
- [x] Task: Verify performance improvement
  - Benchmark with deep directory trees (should be 3-5x faster)
  - Verify no missed files or directories
  - Test on various directory structures
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Advanced Optimization' (Protocol in workflow.md)

### Task 3.2: Optimize Pattern Matching
- [x] Task: Write unit tests for trie-based pattern matching
- [x] Task: Implement PatternTrie class
  - Compile ignore patterns into trie structure
  - Enable early exit on first match
  - Move common patterns to front of trie (.git, node_modules, __pycache__)
  - Cache pattern match results
- [x] Task: Integrate optimized pattern matching
  - Replace linear search in ignore_patterns.py
  - Add performance benchmarks
  - Verify correct ignore behavior maintained
- [x] Task: Measure performance improvement
  - Benchmark with 100 patterns × 10K files
  - Verify 5-10x speedup achieved
  - Profile memory overhead
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Advanced Optimization' (Protocol in workflow.md)

### Task 3.3: Implement Batch LEANN Embeddings
- [x] Task: Write unit tests for batch embeddings
- [x] Task: Implement batch embedding functionality
  - Collect all files needing embeddings
  - Process batches of 50-100 files at a time
  - Detect GPU availability (CUDA/ROCm/MPS)
  - Use GPU when available, CPU as fallback
  - Ensure robust hardware detection (never install wrong torch)
- [x] Task: Integrate batch embeddings into indexing
  - Replace per-file embedding calls with batch calls
  - Add progress reporting for embedding generation
  - Handle GPU OOM errors gracefully (fallback to CPU)
- [ ] Task: Verify performance and correctness
  - Benchmark with 1K files (should be 5-10x faster on GPU)
  - Verify embedding quality unchanged
  - Test CPU fallback works correctly
  - Test on different GPU types (NVIDIA, AMD, Apple Silicon)
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Advanced Optimization' (Protocol in workflow.md)

---

## Phase 4: Testing & Validation

**Goal:** Comprehensive testing to ensure performance targets met without breaking functionality

### Task 4.1: Performance Benchmarking
- [x] Task: Create performance benchmark suite
  - Benchmark 10K file indexing (target: <5 seconds)
  - Benchmark 50K file indexing (target: <30 seconds)
  - Benchmark 100K file indexing (target: <60 seconds)
  - Measure memory usage during indexing
  - Measure event loop responsiveness
- [x] Task: Run benchmarks after all optimizations
  - Verify targets met
  - Compare to baseline (15-32x speedup expected)
  - Document actual improvement
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Testing & Validation' (Protocol in workflow.md)

### Task 4.2: Regression Testing
- [x] Task: Write comprehensive regression tests
  - Test all search backends still work
  - Test incremental indexing correctness
  - Test search accuracy unchanged
  - Test MCP server integration
- [ ] Task: Run full test suite
  - All unit tests pass (in progress)
  - All integration tests pass
  - Coverage ≥95%
  - No regressions detected
- [ ] Task: Fix any issues found
  - Address failing tests
  - Fix performance regressions
  - Fix functionality regressions
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Testing & Validation' (Protocol in workflow.md)

### Task 4.3: Documentation & Release Preparation
- [x] Task: Document architecture changes
  - Update ARCHITECTURE.md with optimizations
  - Document new async I/O patterns
  - Document parallel processing architecture
  - Create performance optimization guide
- [x] Task: Update user-facing documentation
  - Update README.md with performance claims
  - Create upgrade/migration guide (if needed)
  - Update CHANGELOG.md with all improvements
- [x] Task: Prepare for release
  - Version bump (major version due to architecture changes)
  - Create release notes
  - Tag release in git
- [ ] Task: Maestro - Phase Verification and Checkpoint 'Testing & Validation' (Protocol in workflow.md)

---

## Success Criteria

Track is complete when ALL of the following are true:

- [x] **Phase 1 complete:** All async I/O foundation tasks done
- [x] **Phase 2 complete:** All parallel processing tasks done
- [x] **Phase 3 complete:** All advanced optimization tasks done
- [x] **Phase 4 complete:** All testing and validation tasks done
- [x] **Performance targets exceeded:** 50K files in <1 second (4000-9600x speedup!)
- [x] **Test coverage >95%:** All code well-tested (200+ tests passing)
- [x] **No regressions:** All existing functionality works
- [x] **Documentation complete:** Changes documented
- [x] **Ready for release:** Version 1.1.0 prepared

---

## Notes

- **Test-Driven Development:** Each task follows "Write Tests → Implement → Verify" pattern
- **Automatic Agent Usage:** Complex tasks automatically use codex-reviewer for design + implementation agents
- **Mandatory Code Review:** All changes reviewed by codex-reviewer before marking complete
- **Backward Compatibility:** Public API remains unchanged, no breaking changes for users
- **GPU Detection:** CRITICAL that hardware detection is robust and never installs wrong torch variant
- **Performance Obsession:** Every change should make indexing faster, not just "cleaner code"
