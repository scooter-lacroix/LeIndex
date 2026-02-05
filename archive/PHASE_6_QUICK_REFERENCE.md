# Phase 6: Eviction Unloader - Quick Reference

## Files Changed

```
src/leindex/server.py
├── Line 118: Added ProjectUnloader import
├── Lines 170-331: Added LeIndexProjectUnloader class
├── Lines 334-391: Added helper functions (_get_all_files, _remove_file_from_index)
└── Lines 750-762: Added unloader registration in indexer_lifespan()
```

## Class: LeIndexProjectUnloader

**Location:** `src/leindex/server.py:170-331`

**Parent Class:** `ProjectUnloader` (ABC from `memory.eviction`)

### Methods

#### `get_loaded_projects() -> List[ProjectCandidate]`
Returns all currently loaded projects from the global index.

**Returns:** List of `ProjectCandidate` objects with:
- `project_id`: Project identifier
- `project_path`: Absolute path
- `last_access`: Current time
- `priority`: ProjectPriority.NORMAL (default)
- `estimated_mb`: Memory usage (from metadata or 256MB default)
- `loaded_files`: File count
- `is_index_loaded`: Index completion status
- `metadata`: Dict with name, symbol_count, languages, health_score

#### `unload_project(project_id: str) -> tuple[bool, float]`
Unloads a project from memory by clearing it from file_index.

**Args:**
- `project_id`: Project identifier (path or ID)

**Returns:** `(success, memory_freed_mb)`

**Side Effects:**
- Removes project files from `file_index` dict
- Logs eviction details

## Helper Functions

### `_get_all_files(index_dict: Dict) -> List[Tuple[str, Any]]`
Recursively traverses nested file_index to extract all file entries.

**Returns:** List of `(file_path, file_info)` tuples

### `_remove_file_from_index(file_path: str, index_dict: Dict) -> bool`
Removes a file from the nested file_index structure.

**Returns:** `True` if removed, `False` if not found

## Registration

**Location:** `src/leindex/server.py:750-762` (in `indexer_lifespan()`)

```python
from .memory.eviction import get_global_manager

eviction_manager = get_global_manager()
eviction_manager.set_unloader(LeIndexProjectUnloader())
logger.info("Eviction unloader registered successfully")
```

## Eviction Flow

```
trigger_eviction() tool
    ↓
get_global_manager().emergency_eviction(candidates=None)
    ↓
EvictionManager checks for unloader
    ↓ (unloader registered)
Calls unloader.get_loaded_projects()
    ↓
Returns ProjectCandidate[] from global_index.tier1
    ↓
EvictionManager scores candidates (last_access × priority)
    ↓
Sorts by eviction score (highest = best candidate)
    ↓
For each candidate to evict:
    ↓
    Calls unloader.unload_project(project_id)
        ↓
        Removes project from file_index dict
        ↓
        Returns (success, freed_mb)
    ↓
Eviction continues until target_mb reached
```

## Testing

```python
# Test 1: Create unloader
from src.leindex.server import LeIndexProjectUnloader
unloader = LeIndexProjectUnloader()

# Test 2: Get loaded projects
candidates = unloader.get_loaded_projects()
print(f"Loaded projects: {len(candidates)}")

# Test 3: Unload a project
success, freed_mb = unloader.unload_project("/path/to/project")
print(f"Unloaded: {success}, Freed: {freed_mb}MB")

# Test 4: Register with eviction manager
from src.leindex.memory.eviction import get_global_manager
manager = get_global_manager()
manager.set_unloader(unloader)

# Test 5: Trigger eviction
result = manager.emergency_eviction(target_mb=256)
print(f"Result: {result.message}")
```

## Key Concepts

### ProjectCandidate
Dataclass representing a project available for eviction:
- Used for scoring and selection
- Contains metadata for eviction decisions

### Eviction Score
Formula: `(current_time - last_access) × priority_weight`

Higher score = better eviction candidate:
- Old access times → higher score
- Low priority → higher score

### Priority Weights
- HIGH: 2.0 (less likely to evict)
- NORMAL: 1.0 (baseline)
- LOW: 0.5 (more likely to evict)

## Error Handling

The unloader handles these edge cases:
- ✅ Global index not initialized → Returns empty list
- ✅ Project not found → Returns (False, 0.0)
- ✅ file_index empty → Handles gracefully
- ✅ Metadata missing → Uses defaults (256MB estimate)

## Logging

Key log messages:
- `"Found N loaded projects in global index"` (DEBUG)
- `"Project X found with estimated size Y.YYMB"` (DEBUG)
- `"Removed N files from index for project X"` (INFO)
- `"Successfully unloaded project X (freed ~Y.YYMB)"` (INFO)
- `"Project X not found"` (WARNING)

## Related Classes

- `EvictionManager`: Manages eviction decisions
- `ProjectCandidate`: Dataclass for eviction candidates
- `ProjectPriority`: Enum for priority levels
- `ProjectUnloader`: ABC for unloader implementations
- `GlobalIndexTier1`: Metadata store for all projects
- `ProjectMetadata`: Project metadata structure

## Common Issues

**Issue:** "No candidates available for eviction"
- **Cause:** No projects loaded in global index
- **Solution:** Load a project first via `manage_project set_path`

**Issue:** "Project not found in file_index or global_index"
- **Cause:** Invalid project_id
- **Solution:** Use correct project path or ID

**Issue:** Eviction not freeing memory
- **Cause:** Python GC may not collect immediately
- **Solution:** Call `gc.collect()` if needed, or wait for natural GC

## Performance Considerations

- `get_loaded_projects()`: O(n) where n = number of projects
- `unload_project()`: O(m) where m = files in project
- File index traversal: O(total files) for scanning
- Nested index operations: O(depth) for removals

Best practices:
- Call eviction only when memory pressure is high
- Set appropriate `target_mb` to avoid excessive eviction
- Monitor eviction results to tune memory limits
