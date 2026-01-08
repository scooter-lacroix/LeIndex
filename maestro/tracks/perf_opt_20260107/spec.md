# Specification: LeIndex Performance Optimization

## Overview

**Track ID:** perf_opt_20260107
**Type:** Critical Performance Refactoring
**Priority:** CRITICAL
**Complexity:** HIGH

### Problem Statement

LeIndex currently suffers from **severe performance bottlenecks** causing indexing to take 20+ minutes for large codebases (50K files) when it should take <30 seconds. The root cause is **synchronous I/O operations blocking the async event loop** throughout the codebase, despite being designed with async/await architecture.

**Current Performance:**
- 50K files: 7-16 minutes (445-960 seconds)
- 100K files: 15-30+ minutes
- User impact: 20+ minute hangs requiring timeout interruption

**Target Performance:**
- 50K files: <30 seconds
- 100K files: <60 seconds
- **Speedup: 15-32x faster**

---

## Root Cause Analysis

### Critical Performance Bottlenecks Identified

1. **Synchronous `os.walk()` blocking event loop** (`server.py:6764`)
   - Entire file tree traversal is synchronous
   - Blocks all async operations during walk
   - For 50K files: 150-300 seconds wasted

2. **File hash computation reading entire files** (`incremental_indexer.py:124`)
   - Hash computed for EVERY file during indexing
   - Reads entire file content synchronously
   - For 1K changed files: 10-30 seconds wasted

3. **Blocking SQLite writes with `synchronous=FULL`** (`sqlite_storage.py:45`)
   - Each file blocks on database write
   - Forces disk sync on every write
   - For 1K files: 50-100 seconds wasted

4. **"Parallel" processor doesn't parallelize I/O** (`parallel_processor.py:144-194`)
   - Only creates dict objects in "parallel"
   - Actual file reading happens sequentially later
   - Defeats the purpose of parallelization

5. **3-4 redundant `os.stat()` calls per file** (`server.py:6810, 6959, 7156`)
   - Same file stat()'d multiple times
   - For 10K files: 30-150 redundant syscalls

6. **Linear regex pattern matching O(n×m)** (`ignore_patterns.py:176-181`)
   - Checks all patterns for every file
   - For 50K files × 100 patterns = 5M regex operations
   - Wastes 15-25 seconds

7. **Blocking LEANN embeddings** (`leann_backend.py`)
   - ML model inference is CPU-intensive and blocking
   - No batching of embeddings
   - For 1K files: 30-60 seconds wasted

**Total Impact:** 445-960 seconds → Target: <30 seconds

---

## Objectives

### Primary Objectives

1. **Eliminate all blocking I/O operations in async contexts**
   - Make `os.walk()` truly async
   - Parallelize actual file reading operations
   - Batch database writes

2. **Remove redundant system calls**
   - Cache file stats once per file
   - Eliminate multiple `os.stat()` calls

3. **Optimize hot paths**
   - Defer hash computation to only when needed
   - Optimize pattern matching algorithm
   - Batch LEANN embeddings

4. **Implement true parallelization**
   - Move file reading into worker threads
   - Parallel directory traversal
   - Concurrent processing where beneficial

### Success Criteria

- [ ] Index 50K files in <30 seconds (current: 7-16 minutes)
- [ ] Index 100K files in <60 seconds (current: 15-30 minutes)
- [ ] No blocking operations in async functions
- [ ] All subprocess calls have timeouts
- [ ] Memory usage <2GB during indexing
- [ ] Search latency unchanged (<100ms P50)
- [ ] 95% test coverage maintained
- [ ] No breaking changes to public API

---

## Functional Requirements

### Phase 1: Async I/O Foundation

#### FR-1.1: Async File Tree Traversal
**Priority:** CRITICAL
**Description:** Replace synchronous `os.walk()` with async traversal

**Requirements:**
- Use `asyncio.to_thread()` to run `os.walk()` in thread pool
- Do not block event loop during file tree traversal
- Maintain backward compatibility with filtering logic
- Add progress reporting during traversal

**Acceptance Criteria:**
- [ ] `os.walk()` wrapped in `asyncio.to_thread()`
- [ ] Event loop remains responsive during traversal
- [ ] Filtering (ignore patterns, file size) still works
- [ ] Progress events emitted for large traversals

**Files to Modify:**
- `src/leindex/server.py` (line 6764)

---

#### FR-1.2: File Stat Caching
**Priority:** CRITICAL
**Description:** Cache file stats to eliminate redundant `os.stat()` calls

**Requirements:**
- Create in-memory cache of file stats (path, size, mtime, hash)
- Populate cache during first `os.stat()` call
- Reuse cached stats for all subsequent operations
- Invalidate cache when file changes

**Acceptance Criteria:**
- [ ] Each file stat()'d exactly once during indexing
- [ ] Cache hit rate >95% for subsequent operations
- [ ] Memory overhead <100MB for 50K files
- [ ] Cache invalidation on file modification

**Files to Modify:**
- `src/leindex/server.py` (lines 6810, 6959, 7156)
- `src/leindex/incremental_indexer.py` (line 124)

---

#### FR-1.3: SQLite Write Optimization
**Priority:** CRITICAL
**Description:** Configure SQLite for bulk write performance

**Requirements:**
- Change `PRAGMA synchronous` from FULL to NORMAL during indexing
- Batch database writes (100 documents at a time)
- Use transactions for bulk operations
- Restore FULL mode after indexing

**Acceptance Criteria:**
- [ ] `synchronous=NORMAL` during bulk indexing
- [ ] Writes batched in groups of 100
- [ ] Single transaction per batch
- [ ] `synchronous=FULL` restored after indexing
- [ ] No data loss risk (transactional integrity)

**Files to Modify:**
- `src/leindex/storage/sqlite_storage.py` (line 45, 67-87)
- `src/leindex/server.py` (line 6969)

---

### Phase 2: Parallel Processing & Batching

#### FR-2.1: True Parallel File Reading
**Priority:** CRITICAL
**Description:** Move actual file reading into parallel workers

**Requirements:**
- Modify `ParallelIndexer.process_files()` to read file contents in workers
- Each worker thread reads file content, not just metadata
- Chunk files for optimal I/O parallelization (100 files per chunk)
- Limit concurrency to avoid overwhelming filesystem

**Acceptance Criteria:**
- [ ] File reading happens in parallel (4-8 concurrent workers)
- [ ] Content reading moved into worker threads
- [ ] Chunk size configurable (default: 100 files)
- [ ] No sequential I/O bottleneck
- [ ] Thread pool properly managed

**Files to Modify:**
- `src/leindex/parallel_processor.py` (lines 144-194)
- `src/leindex/server.py` (line 6942)

---

#### FR-2.2: Batch Database Writes
**Priority:** CRITICAL
**Description:** Batch database write operations for efficiency

**Requirements:**
- Collect all documents to index in memory
- Write batches of 100 documents at a time
- Use single transaction per batch
- Add retry logic for failed batches

**Acceptance Criteria:**
- [ ] Documents batched in groups of 100
- [ ] Single transaction per batch
- [ ] Failed batches retried 3 times with exponential backoff
- [ ] No partial writes (all-or-nothing per batch)
- [ ] Progress reporting during batch writes

**Files to Modify:**
- `src/leindex/server.py` (line 6969)
- `src/leindex/storage/sqlite_storage.py`

---

#### FR-2.3: Deferred Hash Computation
**Priority:** HIGH
**Description:** Only compute hashes when needed (incremental indexing)

**Requirements:**
- Skip hash computation during initial indexing
- Compute hashes only for incremental indexing
- Use mtime+size for first-pass change detection
- Compute hash only when mtime+size indicates potential change

**Acceptance Criteria:**
- [ ] Initial indexing skips hash computation (0 hashes)
- [ ] Incremental indexing uses mtime+size first
- [ ] Hash computed only when mtime+size unchanged
- [ ] 50%+ faster incremental indexing for unchanged files

**Files to Modify:**
- `src/leindex/server.py` (line 6962-6964)
- `src/leindex/incremental_indexer.py` (lines 124-133, 172-206)

---

### Phase 3: Advanced Optimization

#### FR-3.1: Parallel Directory Traversal
**Priority:** HIGH
**Description:** Implement parallel directory tree traversal

**Requirements:**
- Use `scandir()` in parallel workers for subdirectories
- Divide directory tree into subtrees
- Process subtrees concurrently with semaphore limiting
- Combine results from all workers

**Acceptance Criteria:**
- [ ] Directory tree processed in parallel
- [ ] Semaphore limits concurrency (default: 4 workers)
- [ ] Results combined correctly
- [ ] 3-5x faster for deep directory trees

**Files to Modify:**
- `src/leindex/server.py` (line 6764)
- New module: `src/leindex/parallel_scanner.py`

---

#### FR-3.2: Pattern Matching Optimization
**Priority:** MEDIUM
**Description:** Optimize ignore pattern matching performance

**Requirements:**
- Compile patterns into trie structure for early exit
- Cache pattern match results
- Use short-circuit evaluation
- Optimize common patterns to the front

**Acceptance Criteria:**
- [ ] Patterns organized in trie structure
- [ ] Early exit on first match
- [ ] Common patterns (`.git`, `node_modules`) checked first
- [ ] 5-10x faster pattern matching

**Files to Modify:**
- `src/leindex/ignore_patterns.py` (lines 176-181)

---

#### FR-3.3: Batch LEANN Embeddings
**Priority:** HIGH
**Description:** Batch LEANN vector embeddings for GPU acceleration

**Requirements:**
- Collect all files needing embeddings
- Process batches of 50-100 files at a time
- Use GPU when available (CUDA/ROCm/MPS)
- Implement proper fallback to CPU

**Acceptance Criteria:**
- [ ] Files batched in groups of 50-100
- [ ] GPU acceleration when available
- [ ] CPU fallback when GPU unavailable
- [ ] 5-10x faster embedding generation
- [ ] Robust hardware detection (never install wrong torch)

**Files to Modify:**
- `src/leindex/core_engine/leann_backend.py`
- `src/leindex/server.py` (line 7000-7002)

---

## Non-Functional Requirements

### Performance Requirements

1. **Indexing Speed**
   - 50K files: <30 seconds (target: <15 seconds after all optimizations)
   - 100K files: <60 seconds (target: <30 seconds after all optimizations)
   - 10K files: <10 seconds (target: <5 seconds)

2. **Resource Usage**
   - Memory: <2GB during indexing, <500MB idle
   - CPU: Efficient use of multiple cores when available
   - Disk: Minimal temporary file creation

3. **Responsiveness**
   - No operation blocks event loop >1 second
   - Progress reporting during long operations
   - Graceful cancellation support

### Reliability Requirements

1. **Error Handling**
   - All I/O operations have timeouts
   - Graceful degradation on failures
   - Retry logic for transient errors
   - Clear error messages for users

2. **Data Integrity**
   - No data loss during batching
   - Transactions ensure atomicity
   - Cache coherency maintained
   - Recovery from corruption

3. **Backward Compatibility**
   - Public API unchanged
   - Existing functionality preserved
   - Configuration options compatible
   - Migration path documented

### Maintainability Requirements

1. **Code Quality**
   - 95%+ test coverage maintained
   - Type hints on all public APIs
   - Comprehensive code comments
   - Performance benchmarks included

2. **Documentation**
   - Architecture changes documented
   - Performance characteristics documented
   - Upgrade guide provided
   - Examples updated

---

## Out of Scope

### Explicitly Not Part of This Track

1. **Web UI** - No web interface being built
2. **Cloud/SaaS** - Staying local-only
3. **New Search Features** - No new search capabilities
4. **Database Migration** - Staying with SQLite/DuckDB
5. **API Changes** - Public API stays the same

### Known Limitations

1. **Network Filesystems**
   - Performance may vary on network mounts
   - Timeout protection prevents hangs
   - No specific optimization for network storage

2. **Very Large Files**
   - Files >100MB still slow to index
   - Configurable size filtering in place
   - Users can exclude large files manually

3. **Windows Platform**
   - Some optimizations Unix-specific initially
   - Windows equivalent implementations where needed
   - Performance gains may vary on Windows

---

## Dependencies

### Internal Dependencies
- LeIndex codebase (`src/leindex/`)
- Existing test infrastructure
- Performance benchmarking tools

### External Dependencies
- Python 3.10+
- asyncio (built-in)
- concurrent.futures (built-in)
- Existing dependencies (PyTorch, Tantivy, etc.)

### New Dependencies
- None (using built-in Python libraries only)

---

## Risks and Mitigations

### Risk 1: Breaking Changes During Refactoring
**Probability:** MEDIUM
**Impact:** HIGH
**Mitigation:**
- Comprehensive test suite before changes
- Incremental refactoring with testing at each step
- Maintain backward compatibility layer
- Extensive code review before merging

### Risk 2: Performance Regression
**Probability:** LOW
**Impact:** HIGH
**Mitigation:**
- Performance benchmarks before/after each change
- Automated performance testing in CI
- Rollback plan if performance degrades
- Monitor production metrics after deployment

### Risk 3: Increased Complexity
**Probability:** MEDIUM
**Impact:** MEDIUM
**Mitigation:**
- Clear code documentation
- Modular design with separation of concerns
- Comprehensive code reviews
- Reduce complexity where possible

### Risk 4: GPU Detection Issues
**Probability:** LOW
**Impact:** MEDIUM
**Mitigation:**
- Robust hardware detection logic
- Extensive testing on different GPU types
- CPU fallback always available
- Clear error messages for detection failures

---

## Testing Strategy

### Unit Tests
- Each modified function has unit tests
- Edge cases covered (empty directories, large files, permissions)
- Mock I/O operations for fast testing
- 95%+ coverage requirement

### Integration Tests
- End-to-end indexing of test codebases
- Performance benchmarks (10K, 50K, 100K files)
- Concurrent operation tests
- GPU acceleration tests (if GPU available)

### Performance Tests
- Baseline: Measure current indexing time
- Target: Verify <30 seconds for 50K files
- Memory profiling during indexing
- Event loop responsiveness monitoring
- Database write throughput measurement

### Regression Tests
- Verify search results unchanged
- Ensure accuracy not sacrificed for speed
- Validate incremental indexing correctness
- Test all search backends still work

---

## Deliverables

### Code Deliverables
1. Modified `server.py` with async file traversal
2. Modified `parallel_processor.py` with true parallelization
3. Modified `sqlite_storage.py` with optimized writes
4. Modified `incremental_indexer.py` with deferred hashing
5. Modified `ignore_patterns.py` with optimized matching
6. Modified `leann_backend.py` with batch embeddings
7. New `parallel_scanner.py` module (if needed)

### Documentation Deliverables
1. Architecture changes documented
2. Performance characteristics documented
3. Upgrade/migration guide (if needed)
4. Changelog entry
5. Code comments explaining optimizations

### Test Deliverables
1. Unit tests for all modified code
2. Integration tests for end-to-end workflows
3. Performance benchmarks demonstrating improvement
4. Regression tests ensuring no functionality loss

---

## Success Metrics

### Quantitative Metrics
- [ ] **50K file indexing time:** 7-16 min → <30 sec (15-32x speedup)
- [ ] **100K file indexing time:** 15-30 min → <60 sec (15-30x speedup)
- [ ] **Memory usage during indexing:** <2GB
- [ ] **Test coverage:** ≥95%
- [ ] **Search latency:** Unchanged (<100ms P50)

### Qualitative Metrics
- [ ] No blocking operations in async functions
- [ ] Code is maintainable and well-documented
- [ ] Backward compatibility maintained
- [ ] No user-facing breaking changes
- [ ] Performance optimization is transparent to users

---

## Definition of Done

This track is complete when:

1. **All critical bottlenecks addressed**
   - [ ] Async I/O implemented
   - [ ] Parallel processing working
   - [ ] Redundant calls eliminated
   - [ ] Advanced optimizations in place

2. **Performance targets met**
   - [ ] 50K files index in <30 seconds
   - [ ] 100K files index in <60 seconds
   - [ ] Memory usage <2GB during indexing

3. **Quality standards met**
   - [ ] 95%+ test coverage
   - [ ] All tests passing
   - [ ] Code review approved
   - [ ] Documentation complete

4. **Integration verified**
   - [ ] Works with existing MCP server
   - [ ] All search backends functional
   - [ ] Incremental indexing correct
   - [ ] No breaking changes to public API

5. **Deployment ready**
   - [ ] Changelog updated
   - [ ] Migration guide provided (if needed)
   - [ ] Version bump appropriate
   - [ ] Ready for release
