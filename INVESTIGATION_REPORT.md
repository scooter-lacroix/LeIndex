# LeIndex MCP Server - Investigation Report: set_path Hanging Issue

## Executive Summary

The LeIndex MCP server's `manage_project` tool with `set_path` action hung indefinitely for over 20 minutes during testing. This investigation identifies the root cause as **missing timeout parameters** on blocking subprocess calls in several search backend modules, combined with the potential for large file tree traversal operations during project indexing.

**Critical Finding**: Multiple search backends (`grep.py`, `ag.py`, `ripgrep.py`, `ugrep.py`) execute subprocess calls without timeout parameters, which can cause indefinite hangs if the underlying processes encounter issues like network mounts, permission problems, or infinite loops.

## Investigation Details

### Test Environment
- **Test Command**: `manage_project` with action `set_path`
- **Test Path**: `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer`
- **Observed Behavior**: Command hung for 20+ minutes until interrupted

### Server Status Before Hang

```
Registry Status:
- 6 projects registered (all temporary directories that no longer exist)
- Registry path: /home/stan/.leindex/projects.db
- All projects in critical state (paths don't exist)

Memory Diagnostics:
- Project path: NOT SET
- LazyContentManager: available
- Memory profiler: NOT initialized
- Memory-aware manager: NOT initialized

Error: "Project path not set"
```

## Root Causes Identified

### 1. CRITICAL: Missing Timeouts on Subprocess Calls

The most likely cause of the indefinite hang is the absence of timeout parameters on `subprocess.run()` calls in multiple search backend modules:

#### Files Without Timeout Protection:

| File | Line | Issue |
|------|------|-------|
| `src/leindex/search/grep.py` | 73 | `subprocess.run()` has NO timeout |
| `src/leindex/search/ag.py` | 69 | `subprocess.run()` has NO timeout |
| `src/leindex/search/ripgrep.py` | 66 | `subprocess.run()` has NO timeout |
| `src/leindex/search/ugrep.py` | 60 | `subprocess.run()` has NO timeout |

#### Comparison with Properly Implemented Backend:
`src/leindex/search/zoekt.py` correctly implements timeouts on all subprocess calls:
- Line 144: `timeout=10`
- Line 150: `timeout=5`
- Line 305: `timeout=5`
- Line 343: `timeout=5`
- Line 515: `timeout=300` (5 minutes for indexing)
- Line 610: `timeout=30` (30 seconds for searches)

#### Potential Hang Scenarios:
1. **Network Mounts**: If the project path contains or accesses network-mounted directories, `grep`/`ag`/`rg`/`ug` may hang indefinitely waiting for responses
2. **Permission Issues**: Subprocess may hang waiting for authentication prompts
3. **Broken Symlinks**: May cause infinite loops or hangs
4. **FIFO/Pipe Files**: May block indefinitely when attempting to read
5. **Tainted Filesystems**: Corrupted filesystems can cause hangs

### 2. File Tree Traversal Without Progress Feedback

The `_index_project()` function in `server.py:6716` performs recursive directory traversal:

```python
for root, dirs, files in os.walk(base_path):
    # ... filtering logic ...
```

**Issues**:
- No timeout or cancellation mechanism
- No progress reporting during traversal
- Large directory trees (like `node_modules/`) can cause excessive traversal
- Synchronous operation blocks the event loop

### 3. DAL Initialization Without Timeout

The `get_dal_instance()` call at `server.py:2136` creates database connections:

**SQLiteDuckDBDAL initialization** includes:
- Directory creation (`os.makedirs`)
- Disk space checking (`os.statvfs`)
- Multiple SQLite database connections
- DuckDB analytics backend initialization

**Risk**: These operations may hang on:
- Network filesystems with high latency
- I/O errors or disk issues
- Lock contention on database files

### 4. Memory Profiler Initialization Complexity

The memory profiler initialization at `server.py:2146-2256` includes:
- Configuration loading from YAML
- Memory limit creation with validation
- Profiler creation and validation
- Test snapshot taking
- Monitoring startup

**Risk**: If any of these operations block, the entire `set_path` operation hangs.

## Code Flow During set_path

```
set_project_path()
├── Path validation (fast)
├── Early return check (fast - if path unchanged and index recent)
├── Global resource cleanup (fast)
├── Context update (fast)
├── Config save (fast)
├── OptimizedProjectSettings creation (POTENTIALLY SLOW - includes file loading)
├── IncrementalIndexer creation (fast)
├── FileChangeTracker creation (fast)
├── DAL re-initialization (POTENTIALLY SLOW - database operations)
├── Memory profiler initialization (POTENTIALLY SLOW - multiple I/O operations)
├── Performance monitor initialization (fast)
├── Index loading attempt (fast if exists, SLOW if needs indexing)
└── _index_project() if needed (VERY SLOW - file tree traversal + indexing)
    ├── os.walk() over entire directory tree (NO TIMEOUT)
    ├── File filtering (fast)
    ├── Changed file detection (fast)
    └── ParallelIndexer.process_files()
        └── Search backend indexing (POTENTIALLY HANGING - no timeout)
```

## Additional Findings

### Existing Performance Optimizations

The code includes several good practices:
1. **Early Return Optimization** (`server.py:2028-2068`): Skips reindexing if path unchanged and index is recent (48-hour threshold)
2. **Memory Cleanup** (`server.py:2072-2098`): Properly disposes old LazyContentManager
3. **Parallel Processing**: Uses `ParallelIndexer` with ThreadPoolExecutor
4. **Incremental Indexing**: Only processes changed files

### Other Potential Blocking Operations

While less likely to cause 20+ minute hangs, these operations could contribute:
- `os.walk()` on network filesystems
- `os.path.getsize()` on locked files
- SQLite database operations with locks
- File reading with `SmartFileReader`

## Recommendations

### Critical Fixes (Required)

1. **Add timeouts to ALL subprocess.run() calls**:
   ```python
   # grep.py, ag.py, ripgrep.py, ugrep.py
   process = subprocess.run(
       cmd,
       capture_output=True,
       text=True,
       timeout=30,  # ADD THIS LINE
       # ... other parameters
   )
   ```

2. **Add timeout/cancellation to os.walk() operations**:
   - Implement a timeout wrapper around `os.walk()`
   - Add cancellation token support
   - Consider using `scandir()` with explicit timeout

3. **Add progress reporting for long operations**:
   - Emit progress events during `os.walk()`
   - Report indexing progress
   - Allow client to query operation status

### Important Improvements

4. **Make DAL initialization async with timeout**:
   - Add `asyncio.wait_for()` around DAL initialization
   - Set reasonable timeout (e.g., 30 seconds)

5. **Add operation timeouts to set_project_path**:
   - Total operation timeout
   - Per-phase timeouts

6. **Implement graceful degradation**:
   - Skip problematic directories
   - Continue with partial index
   - Log warnings for skipped paths

### Nice-to-Have Enhancements

7. **Add operation cancellation support**:
   - Allow client to cancel long-running operations
   - Clean up resources on cancellation

8. **Add configuration for timeouts**:
   - Allow users to customize timeout values
   - Different timeouts for different operations

9. **Add pre-flight checks**:
   - Detect network mounts
   - Warn about large directory trees
   - Offer to skip certain directories

## Conclusion

The most likely cause of the 20+ minute hang is the combination of:
1. **Missing timeouts on subprocess calls** in search backends (CRITICAL)
2. **Large file tree traversal** during indexing
3. **No operation timeout or cancellation mechanism**

The immediate fix is to add timeout parameters to all `subprocess.run()` calls in the search backends. This will prevent indefinite hangs and ensure the server remains responsive even when encountering problematic files or directories.

## Files Referenced

- `src/leindex/server.py` - Main server implementation with `set_project_path()` function
- `src/leindex/search/grep.py` - Grep search backend (MISSING TIMEOUT)
- `src/leindex/search/ag.py` - The Silver Searcher backend (MISSING TIMEOUT)
- `src/leindex/search/ripgrep.py` - Ripgrep backend (MISSING TIMEOUT)
- `src/leindex/search/ugrep.py` - Ugrep backend (MISSING TIMEOUT)
- `src/leindex/search/zoekt.py` - Zoekt backend (HAS TIMEOUTS - reference implementation)
- `src/leindex/parallel_processor.py` - Parallel indexing implementation
- `src/leindex/storage/dal_factory.py` - DAL factory and initialization

---

**Report Generated**: 2026-01-07
**Investigation Method**: Code analysis, subprocess call audit, code flow tracing
**Severity**: CRITICAL - Multiple indefinite hang vectors identified
