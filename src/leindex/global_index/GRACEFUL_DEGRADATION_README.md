# Graceful Degradation for Global Index Operations

## Overview

The graceful degradation module provides fallback mechanisms for global index operations when primary backends are unavailable. This ensures the system continues to function with reduced capabilities rather than failing completely.

## Architecture

### Backend Fallback Chain

```
LEANN (AI-powered semantic search)
    ↓ (unavailable)
Tantivy (fast full-text search)
    ↓ (unavailable)
ripgrep (rg) (fast regex search)
    ↓ (unavailable)
grep (basic text search)
    ↓ (unavailable)
No backend available (error state)
```

### Degradation Status Indicators

The system provides clear status indicators to show the current degradation level:

- **`full`**: All backends operational (LEANN available)
- **`degraded_leann_unavailable`**: LEANN unavailable, using Tantivy
- **`degraded_tantivy_unavailable`**: Tantivy unavailable, using ripgrep
- **`degraded_search_fallback`**: Only basic grep/ripgrep available
- **`degraded_no_backend`**: No search backends available

## Usage

### Basic Usage

```python
from leindex.global_index.graceful_degradation import (
    execute_with_degradation,
    DegradedStatus
)

# Execute search with automatic fallback
result = execute_with_degradation(
    operation="cross_project_search",
    query_pattern="async def fetch",
    project_ids=["proj1", "proj2"],
    case_sensitive=False
)

print(f"Results: {result['results']}")
print(f"Backend used: {result['backend_used']}")
print(f"Status: {result['degraded_status']}")
print(f"Projects skipped: {result['projects_skipped']}")
```

### Backend Availability Detection

```python
from leindex.global_index.graceful_degradation import (
    get_backend_status,
    get_current_degradation_level
)

# Check all backends
status = get_backend_status()
print(status)
# {'leann': False, 'tantivy': True, 'ripgrep': True, 'grep': True}

# Get current degradation level
level = get_current_degradation_level()
print(f"Current level: {level.value}")
```

### Project Health Checking

```python
from leindex.global_index.graceful_degradation import (
    is_project_healthy,
    filter_healthy_projects
)

# Check single project
if is_project_healthy(project_id="myproject"):
    results = query_project("myproject")
else:
    logger.warning("Project index corrupted, skipping")

# Filter multiple projects
healthy, unhealthy = filter_healthy_projects(
    project_ids=["proj1", "proj2", "proj3"]
)
print(f"Healthy: {healthy}")
print(f"Unhealthy: {unhealthy}")
```

### Manual Fallback Control

```python
from leindex.global_index.graceful_degradation import (
    fallback_from_leann,
    fallback_from_tantivy,
    fallback_to_ripgrep
)

# Start with LEANN (or fallback)
result = fallback_from_leann(
    operation="cross_project_search",
    query_pattern="function foo",
    project_ids=["proj1", "proj2"]
)

print(f"Status: {result.status}")
print(f"Backend: {result.actual_backend}")
print(f"Reason: {result.fallback_reason}")
```

## API Reference

### Functions

#### `execute_with_degradation()`

Main entry point for executing operations with automatic graceful degradation.

**Parameters:**
- `operation` (str): The operation to perform
- `query_pattern` (str): The search query pattern
- `project_ids` (Optional[List[str]]): List of project IDs to search
- `base_path` (Optional[str]): Base path for search (if not using project_ids)
- `**kwargs`: Additional operation-specific parameters

**Returns:**
- `Dict[str, Any]`: Dictionary containing:
  - `results`: Query results
  - `degraded_status`: Current degradation status
  - `backend_used`: Which backend was actually used
  - `projects_skipped`: List of unhealthy projects (if any)
  - `fallback_reason`: Reason for fallback (if applicable)
  - `duration_ms`: Operation duration in milliseconds

#### `is_project_healthy()`

Check if a project index is healthy and can be queried.

**Parameters:**
- `project_id` (str): The project identifier
- `project_path` (Optional[str]): Path to the project directory

**Returns:**
- `bool`: True if project index is healthy, False otherwise

#### `filter_healthy_projects()`

Filter out unhealthy projects from a list.

**Parameters:**
- `project_ids` (List[str]): List of project IDs to check
- `project_paths` (Optional[Dict[str, str]]): Mapping of project_id → project_path

**Returns:**
- `Tuple[List[str], List[str]]`: (healthy_projects, unhealthy_projects)

#### `get_backend_status()`

Get the availability status of all search backends.

**Returns:**
- `Dict[str, bool]`: Mapping of backend names to availability status

#### `get_current_degradation_level()`

Determine the current degradation level based on backend availability.

**Returns:**
- `DegradedStatus`: Current system degradation status

### Classes

#### `DegradedStatus`

Enum indicating current degradation level.

**Values:**
- `FULL`: All backends operational
- `DEGRADED_LEANN_UNAVAILABLE`: LEANN unavailable
- `DEGRADED_TANTIVY_UNAVAILABLE`: Tantivy unavailable
- `DEGRADED_SEARCH_FALLBACK`: Only grep/ripgrep available
- `DEGRADED_NO_BACKEND`: No backends available

#### `FallbackResult`

Result from a fallback operation with metadata.

**Attributes:**
- `results` (Any): Query results from the fallback backend
- `status` (DegradedStatus): Current degradation status
- `fallback_reason` (str): Reason for fallback
- `original_backend` (str): Backend that was attempted first
- `actual_backend` (str): Backend that was actually used

## Logging

All fallback operations are logged with structured logging using `log_global_index_operation()`:

```python
log_global_index_operation(
    operation="cross_project_search",
    component='graceful_degradation',
    status='warning',
    duration_ms=45.2,
    backend='ripgrep',
    fallback_from='leann',
    result_count=42
)
```

## Testing

Run the test suite:

```bash
pytest tests/global_index/test_graceful_degradation.py -v
```

Test coverage:
- Backend availability detection (4 tests)
- Degraded status indicators (1 test)
- LEANN → Tantivy fallback (2 tests)
- Tantivy → ripgrep fallback (2 tests)
- ripgrep → grep fallback (2 tests)
- Project health checking (3 tests)
- Healthy project filtering (2 tests)
- Execute with degradation (2 tests)
- Backend status retrieval (3 tests)
- Degradation level detection (3 tests)
- Integration tests (1 test)

**Total: 27 tests**

## Error Handling

The module handles errors gracefully at each level:

1. **LEANN errors**: Falls back to Tantivy with error logging
2. **Tantivy errors**: Falls back to ripgrep with error logging
3. **ripgrep errors**: Falls back to grep with error logging
4. **grep errors**: Returns `DEGRADED_NO_BACKEND` status
5. **Project errors**: Skips unhealthy projects and continues with healthy ones

All errors are logged with structured logging for debugging and monitoring.

## Performance Considerations

- **LEANN**: Slowest but most accurate (semantic search)
- **Tantivy**: Fast full-text search (recommended for most use cases)
- **ripgrep**: Very fast regex search (3-5x faster than grep)
- **grep**: Slowest but universally available

The system automatically uses the best available backend, ensuring optimal performance while maintaining functionality.

## Integration with Global Index

The graceful degradation module integrates with:

- **Tier 1 Metadata**: Project health checking
- **Tier 2 Cache**: Degraded status in cached queries
- **Query Router**: Automatic fallback selection
- **Monitoring**: Structured logging of fallback events
- **Cross-Project Search**: Multi-project query with filtering

## Best Practices

1. **Always check degraded status**: Always check the `degraded_status` field in responses
2. **Handle degraded mode**: Provide appropriate UI/UX feedback when system is degraded
3. **Monitor fallbacks**: Log and monitor fallback events to detect systemic issues
4. **Test fallbacks**: Regularly test fallback mechanisms to ensure they work
5. **Plan for no backend**: Handle the case when all backends are unavailable

## Example: API Response with Degradation Status

```json
{
  "results": {
    "file1.py": [
      [10, "async def fetch_data():"],
      [25, "async def fetch_user():"]
    ]
  },
  "degraded_status": "degraded_search_fallback",
  "backend_used": "ripgrep",
  "projects_skipped": ["corrupted_project"],
  "fallback_reason": "LEANN and Tantivy unavailable",
  "duration_ms": 23.4
}
```

## Future Enhancements

- Add more backend options (e.g., Elasticsearch, Solr)
- Implement caching of fallback results
- Add performance metrics for each backend
- Implement automatic backend recovery detection
- Add circuit breaker pattern for failing backends
- Provide degradation alerts and notifications
