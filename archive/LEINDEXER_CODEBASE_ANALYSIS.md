# LeIndexer Codebase Analysis Report

**Analysis Date:** 2026-01-08
**Analyzed By:** gemini-analyzer agent
**Repository Path:** `/mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer`

---

## Executive Summary

This comprehensive analysis covers five critical areas of the LeIndexer codebase:

1. **Search Tool Parameter Issue** - ✅ **CRITICAL BUG FOUND**
2. **Memory Management Implementation** - ✅ **FULLY IMPLEMENTED**
3. **LEANN Semantic Search** - ✅ **OPERATIONAL WITH DEPENDENCIES**
4. **Global/Index Tracking** - ✅ **IMPLEMENTED**
5. **Maestro Track Structure** - ✅ **WELL-DEFINED**

---

## 1. Search Tool Parameter Issue - CRITICAL BUG

### Issue Identified

**Location:** `src/leindex/core_engine/tool_routers.py:873-880`

**Problem:** The `search_content_router` function attempts to pass parameters to `search_code_advanced` that the function doesn't accept.

### Root Cause Analysis

**File:** `src/leindex/core_engine/consolidated_tools.py` (Lines 123-202)

The `search_content` mega-tool schema defines these parameters:
```python
fuzziness_level: Optional[str] = None,
content_boost: float = 1.0,
filepath_boost: float = 1.0,
highlight_pre_tag: str = "<em>",
highlight_post_tag: str = "</em>",
```

**File:** `src/leindex/core_engine/tool_routers.py` (Lines 559-573)

The router passes these parameters:
```python
return await search_code_advanced(
    pattern=pattern,
    ctx=ctx,
    case_sensitive=case_sensitive,
    context_lines=validated_context_lines,
    file_pattern=file_pattern,
    fuzzy=fuzzy,
    fuzziness_level=fuzziness_level,      # ❌ NOT ACCEPTED
    content_boost=validated_content_boost,     # ❌ NOT ACCEPTED
    filepath_boost=validated_filepath_boost,   # ❌ NOT ACCEPTED
    highlight_pre_tag=highlight_pre_tag,       # ❌ NOT ACCEPTED
    highlight_post_tag=highlight_post_tag,     # ❌ NOT ACCEPTED
    page=validated_page,
    page_size=validated_page_size,
)
```

**File:** `src/leindex/server.py` (Lines 2365-2374)

But `search_code_advanced` only accepts:
```python
async def search_code_advanced(
    pattern: str,
    ctx: Context,
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    page: int = 1,
    page_size: int = 5,
) -> Dict[str, Any]:
```

### Impact

- **Severity:** CRITICAL - Function call will fail with `TypeError: search_code_advanced() got an unexpected keyword argument`
- **Affected Operations:** All search operations using the mega-tool interface
- **Scope:** Any MCP client calling `search_content` with action="search"

### Fix Required

**Option 1:** Update `search_code_advanced` signature to accept the missing parameters

**Option 2:** Remove the parameters from the router call if they're not implemented

**Recommendation:** These parameters appear to be intended for Elasticsearch/LEANN backends based on the names (fuzziness_level, content_boost, etc.). The function signature should be updated to accept them even if they're not all used yet.

### Additional Findings

**Line 2415 in server.py:** The function references `fuzziness_level` in the query key generation but doesn't accept it as a parameter:
```python
query_key = "{}:{}:{}:{}:{}:{}:{}:{}:{}:{}:{}".format(
    pattern,
    case_sensitive,
    context_lines,
    file_pattern,
    fuzzy,
    fuzziness_level,  # ❌ USED BUT NOT IN SIGNATURE
    content_boost,    # ❌ USED BUT NOT IN SIGNATURE
    filepath_boost,   # ❌ USED BUT NOT IN SIGNATURE
    highlight_pre_tag,  # ❌ USED BUT NOT IN SIGNATURE
    highlight_post_tag, # ❌ USED BUT NOT IN SIGNATURE
    page,
)
```

This will cause a `NameError` at runtime.

---

## 2. Memory Management Implementation

### Overview

**File:** `src/leindex/memory_profiler.py`

**Status:** ✅ **FULLY IMPLEMENTED AND COMPREHENSIVE**

### Memory Limits Configuration

**Location:** Lines 43-51

```python
@dataclass
class MemoryLimits:
    """Memory limits configuration."""
    soft_limit_mb: float = 512.0        # 512MB soft limit
    hard_limit_mb: float = 1024.0       # 1GB hard limit
    max_loaded_files: int = 100
    max_cached_queries: int = 50
    gc_threshold_mb: float = 256.0      # Trigger GC at 256MB
    spill_threshold_mb: float = 768.0   # Spill to disk at 768MB
```

### Garbage Collection Triggers

**Location:** Lines 328-363

The system triggers GC at multiple thresholds:

1. **GC Threshold (256MB):**
   - Triggers `gc.collect()`
   - Logs objects collected

2. **Soft Limit (512MB):**
   - Triggers cleanup callbacks
   - Unloads loaded files
   - Clears cached queries

3. **Spill Threshold (768MB):**
   - Spills cached data to disk
   - Uses `/tmp/leindex_spill` directory
   - Pickles data for persistence

4. **Hard Limit (1GB):**
   - Triggers aggressive cleanup
   - Unloads ALL content
   - Clears ALL caches
   - Forces garbage collection

### Caching Implementation

**File Stat Cache:**
- **Location:** Referenced in `server.py:5269-5276`
- **Purpose:** Cache file stats to eliminate redundant `os.stat()` calls
- **Implementation:** Caches path, size, mtime, hash
- **Thread Safety:** Uses Lock for concurrent access
- **Memory Overhead:** <100MB for 50K files

**Query Cache:**
- **Location:** Integrated in LazyContentManager
- **Purpose:** Cache search results
- **Spill to Disk:** When memory threshold exceeded
- **Cleanup:** Half of loaded files unloaded when soft limit reached

### Memory Monitoring

**Location:** Lines 430-448

```python
def start_monitoring(self, interval: float = 30.0):
    """Start continuous memory monitoring."""
```

- Runs background thread
- Checks memory every 30 seconds
- Enforces limits automatically
- Stops on shutdown

### Configuration Functions

**Location:** `server.py` Lines 5369-5404

```python
async def configure_memory_limits(
    ctx: Context,
    soft_limit_mb: Optional[float] = None,
    hard_limit_mb: Optional[float] = None,
    max_loaded_files: Optional[int] = None,
    max_cached_queries: Optional[int] = None,
):
```

Allows runtime configuration of all limits.

---

## 3. LEANN Semantic Search

### Overview

**File:** `src/leindex/core_engine/leann_backend.py`

**Status:** ✅ **OPERATIONAL (WITH OPTIONAL DEPENDENCIES)**

### Architecture

**Lines 1-28:**

LEANN backend provides:
- Local embedding models (no API calls needed)
- Storage-efficient vector search (97% savings vs traditional)
- HNSW or DiskANN indexing algorithms
- Metadata storage in JSON alongside index
- AST-aware chunking for Python files

### Dependency Management

**Lines 49-96:** Graceful import handling

```python
# LEANN library
try:
    from leann import LeannSearcher, LeannBuilder
    LEANN_AVAILABLE = True
except ImportError:
    LEANN_AVAILABLE = False
    # Logged warning

# sentence-transformers for embeddings
try:
    from sentence_transformers import SentenceTransformer
    SENTENCE_TRANSFORMERS_AVAILABLE = True
except ImportError:
    SENTENCE_TRANSFORMERS_AVAILABLE = False
    # Logged warning

# PyTorch for GPU support
try:
    import torch
    TORCH_AVAILABLE = True
except ImportError:
    TORCH_AVAILABLE = False
    # GPU disabled
```

### Supported Embedding Models

**Lines 115-148:**

```python
SUPPORTED_MODELS = {
    "nomic-ai/code-embeddings-v1.5": {
        "dimensions": 768,
        "description": "Code-specific embeddings (default, 137M params)"
    },
    "sentence-transformers/all-MiniLM-L6-v2": {
        "dimensions": 384,
        "description": "General-purpose embeddings (legacy)"
    },
    # ... more models
}
```

### Search Implementation

**Class:** `LEANNVectorBackend` (Line 1007)

**Key Features:**
1. **Local Embedding Generation:**
   - Models run locally on CPU/GPU
   - No external API calls
   - Batching support for efficiency

2. **Vector Storage:**
   - LEANN HNSW index (default)
   - DiskANN alternative for larger datasets
   - 97% storage efficiency vs traditional approaches

3. **Query Processing:**
   - Convert query text to embedding
   - Search vector space for nearest neighbors
   - Return ranked results with scores

4. **Metadata Handling:**
   - JSON metadata stored alongside vectors
   - File path, line numbers, chunk info
   - Enables filtering and ranking

### Current Operational Status

**Dependencies Required:**
- `leann>=0.3.5`
- `sentence-transformers`
- `torch>=2.0.0` (optional, for GPU)
- `numpy`

**Installation:**
```bash
uv pip install 'leann>=0.3.5'
uv pip install sentence-transformers
uv pip install 'torch>=2.0.0'  # Optional for GPU
```

**Known Issues:**
- None - operates in limited mode if dependencies missing
- Graceful degradation to keyword search if unavailable

---

## 4. Global/Index Tracking

### Overview

**Files:**
- `src/leindex/registry/project_registry.py`
- `src/leindex/file_change_tracker.py`

**Status:** ✅ **COMPREHENSIVE TRACKING IMPLEMENTED**

### Project Registry

**Purpose:** SQLite-based registry for tracking indexed projects

**Features:**
1. **Multi-Project Tracking:**
   - Track multiple indexed projects
   - Store project metadata (path, index date, stats)
   - Support project-specific settings

2. **Index Versioning:**
   - Track index versions per project
   - Detect when reindexing needed
   - Support incremental updates

3. **Migration Support:**
   - Legacy pickle → MessagePack migration
   - Automatic schema upgrades
   - Backup before migration

### File Change Tracking

**File:** `src/leindex/file_change_tracker.py`

**Purpose:** Track file-level changes with granular categorization

**Change Categories (Lines 24-51):**

```python
class ChangeCategory(Enum):
    FUNCTION_ADD = "function_add"
    FUNCTION_REMOVE = "function_remove"
    FUNCTION_MODIFY = "function_modify"
    CLASS_ADD = "class_add"
    CLASS_REMOVE = "class_remove"
    CLASS_MODIFY = "class_modify"
    IMPORT_ADD = "import_add"
    IMPORT_REMOVE = "import_remove"
    COMMENT_CHANGE = "comment_change"
    WHITESPACE_CHANGE = "whitespace_change"
    LOGIC_CHANGE = "logic_change"
    STRUCTURAL_CHANGE = "structural_change"
    DOCSTRING_CHANGE = "docstring_change"
    UNKNOWN = "unknown"
```

**Line-Level Tracking (Lines 54-84):**

```python
class LineChange:
    """Represents a single line change with attribution."""
    line_number: int
    old_content: str
    new_content: str
    change_type: str  # 'added', 'removed', 'modified'
    category: ChangeCategory
```

### Change Analyzer

**Class:** `ChangeAnalyzer` (Line 86)

**Capabilities:**
1. **Change Categorization:**
   - Detect function/class/imports changes
   - Identify comment/whitespace-only changes
   - Categorize logic vs structural changes

2. **Impact Scoring:**
   - Calculate significance of changes
   - Weight by type and scope
   - Support prioritization

3. **Pattern Detection:**
   - Track frequently changed areas
   - Identify hotspots
   - Enable focused analysis

### History Tracking

**Features:**
1. **Version History:**
   - Track all file versions
   - Store diffs between versions
   - Enable rollback to any version

2. **Metadata Storage:**
   - File metadata (size, mtime, hash)
   - Change attribution
   - Timestamp tracking

3. **Incremental Indexing:**
   - Detect changed files via mtime/size/hash
   - Only reindex changed files
   - Maintain index consistency

---

## 5. Maestro Track Structure

### Overview

**Location:** `maestro/tracks/perf_opt_20260107/`

**Files:**
- `spec.md` - Detailed specification
- `plan.md` - Implementation plan with task breakdown
- `metadata.json` - Track metadata

**Status:** ✅ **WELL-DEFINED AND ACTIVE**

### Spec Structure (`spec.md`)

**Sections:**

1. **Overview (Lines 1-24):**
   - Track ID: `perf_opt_20260107`
   - Type: Critical Performance Refactoring
   - Priority: CRITICAL
   - Complexity: HIGH

2. **Problem Statement (Lines 10-24):**
   - Current performance: 20+ minutes for 50K files
   - Target: <30 seconds for 50K files
   - Speedup goal: 15-32x faster

3. **Root Cause Analysis (Lines 26-64):**
   - 7 critical bottlenecks identified
   - Each with file locations and impact
   - Total time waste quantified

4. **Objectives (Lines 68-101):**
   - Primary objectives
   - Success criteria (checkbox list)
   - Performance targets

5. **Functional Requirements (Lines 103-149+):**
   - Broken down by phase
   - Each requirement has:
     - Priority level
     - Description
     - Requirements (bullet list)
     - Acceptance criteria (checkbox list)
     - Files to modify

### Plan Structure (`plan.md`)

**Organization:**

1. **Phases:** 3 phases (Async I/O, Parallel Processing, Advanced Optimization)

2. **Tasks per Phase:** 3 tasks each (9 total tasks)

3. **Task Breakdown:**
   ```
   ### Task X.Y: [Task Name]
   - [ ] Task: Write unit tests for...
   - [ ] Task: Implement...
   - [ ] Task: Verify...
   - [ ] Task: Maestro - Phase Verification...
   ```

4. **Progress Tracking:**
   - `[ ]` = Not started
   - `[x]` = Completed
   - Current status: Most tasks marked complete

5. **Level of Detail:**
   - Each task broken into 4-5 subtasks
   - Specific implementation steps
   - Verification steps
   - Maestro checkpoint references

### Best Practices Observed

1. **Traceability:**
   - Each requirement links to specific files
   - Line numbers provided for exact locations
   - Clear mapping from problem → solution → implementation

2. **Measurable Criteria:**
   - Quantitative targets (30 seconds, 95% cache hit rate)
   - Qualitative checks (no breaking changes)
   - Performance baselines established

3. **Incremental Delivery:**
   - Phases build on each other
   - Each phase has checkpoint verification
   - Can stop after any phase if needed

4. **Documentation:**
   - Comprehensive inline comments
   - Rationale for each decision
   - Alternatives considered

---

## Summary of Findings

### Critical Issues

1. **CRITICAL BUG:** `search_content` router passes parameters that `search_code_advanced` doesn't accept
   - **Impact:** All search operations via mega-tool will fail
   - **Fix:** Update function signature or remove parameters
   - **Priority:** IMMEDIATE

### Strengths

1. **Memory Management:** Comprehensive and well-implemented
   - Multi-tier thresholds (GC, soft, spill, hard)
   - Automatic monitoring and cleanup
   - Configurable at runtime

2. **LEANN Backend:** Operational with graceful degradation
   - Local embeddings (no API costs)
   - Storage efficient
   - GPU support available

3. **Change Tracking:** Granular and detailed
   - 13 change categories
   - Line-level attribution
   - Impact scoring

4. **Maestro Structure:** Well-organized and executable
   - Clear problem definition
   - Traceable requirements
   - Incremental delivery

### Recommendations

1. **IMMEDIATE:** Fix the search parameter mismatch
2. **CONSIDER:** Add integration tests for mega-tool parameter passing
3. **IMPROVE:** Document which LEANN features work in limited mode
4. **ENHANCE:** Add metrics for change tracking utilization

---

## File Locations Reference

### Search Implementation
- Schema: `src/leindex/core_engine/consolidated_tools.py:123-202`
- Router: `src/leindex/core_engine/tool_routers.py:458-607`
- Implementation: `src/leindex/server.py:2365-2564`

### Memory Management
- Profiler: `src/leindex/memory_profiler.py`
- Configuration: `src/leindex/server.py:5369-5404`
- Cache: `src/leindex/file_stat_cache.py`

### LEANN Backend
- Main: `src/leindex/core_engine/leann_backend.py`
- Enhanced: `src/leindex/core_engine/leann_backend_enhanced.py`
- Core Engine: `src/leindex/core_engine/engine.py`

### Tracking & Registry
- File Tracker: `src/leindex/file_change_tracker.py`
- Project Registry: `src/leindex/registry/project_registry.py`
- Incremental Indexer: `src/leindex/incremental_indexer.py`

### Maestro Track
- Spec: `maestro/tracks/perf_opt_20260107/spec.md`
- Plan: `maestro/tracks/perf_opt_20260107/plan.md`
- Metadata: `maestro/tracks/perf_opt_20260107/metadata.json`

---

**End of Analysis Report**
