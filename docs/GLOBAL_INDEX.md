# Global Index Architecture and Usage

## Overview

The Global Index is a powerful multi-project search and comparison system that enables unified search across all indexed projects. It provides cross-project semantic search, project comparison dashboards, and intelligent query routing with graceful degradation.

### Key Features

- **Cross-Project Search**: Search across multiple projects simultaneously
- **Two-Tier Architecture**: Fast metadata (Tier 1) + query cache (Tier 2)
- **Project Comparison Dashboard**: Compare projects by size, language, and health
- **Event-Driven Updates**: Real-time synchronization across projects
- **Graceful Degradation**: Automatic fallback to alternative search methods
- **Global Statistics**: Aggregate metrics across all projects

## Architecture

### Two-Tier Design

```
┌─────────────────────────────────────────────────────────────┐
│                     Global Index System                      │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  Tier 1: Materialized Metadata (Always Fresh)        │    │
│  │  - Project metadata (path, language, size)           │    │
│  │  - Health scores and statistics                      │    │
│  │  - Last indexed timestamps                          │    │
│  │  - <1ms response time                               │    │
│  └─────────────────────────────────────────────────────┘    │
│                          │                                  │
│                          ▼                                  │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  Tier 2: Stale-Allowed Query Cache                  │    │
│  │  - Cached search results                            │    │
│  │  - Serves stale immediately, rebuilds async         │    │
│  │  - LRU eviction with configurable TTL               │    │
│  │  - Improves response time for repeated queries      │    │
│  └─────────────────────────────────────────────────────┘    │
│                          │                                  │
│                          ▼                                  │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  Query Router & Result Merger                       │    │
│  │  - Routes queries to appropriate projects           │    │
│  │  - Merges and ranks results from multiple projects  │    │
│  │  - Handles failures with graceful degradation       │    │
│  └─────────────────────────────────────────────────────┘    │
│                          │                                  │
│                          ▼                                  │
│  ┌─────────────────────────────────────────────────────┐    │
│  │  Individual Project Indexes                         │    │
│  │  - LEANN (vector search)                            │    │
│  │  - Tantivy (full-text search)                       │    │
│  │  - Ripgrep/Grep (fallback)                          │    │
│  └─────────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────────┘
```

### Component Overview

#### Tier 1: Materialized Metadata

**Module**: `leindex.global_index.tier1_metadata`

Provides instant access to project metadata that is always fresh:

```python
from leindex.global_index import GlobalIndexTier1, ProjectMetadata

# Get Tier 1 index
tier1 = GlobalIndexTier1()

# Get project metadata
metadata = tier1.get_project_metadata("/path/to/project")
print(f"Language: {metadata.primary_language}")
print(f"Health Score: {metadata.health_score}")
print(f"Total Files: {metadata.file_count}")
```

**Features**:
- Always-fresh metadata (no staleness)
- Sub-millisecond response times
- Project health scores
- Language distribution
- File and symbol counts
- Last indexed timestamps

#### Tier 2: Query Cache

**Module**: `leindex.global_index.tier2_cache`

Caches search results with stale-allowed semantics:

```python
from leindex.global_index import GlobalIndexTier2, CachedQuery

# Get Tier 2 cache
tier2 = GlobalIndexTier2(max_size=1000, ttl_seconds=300)

# Check cache before searching
cached = tier2.get(query_hash)
if cached:
    # Serve stale results immediately
    results = cached.results
    # Trigger async rebuild in background
    tier2.refresh_async(query_hash)
else:
    # Perform actual search
    results = perform_search(query)
    # Cache results
    tier2.put(query_hash, results)
```

**Features**:
- Serves stale results immediately
- Rebuilds cache asynchronously
- Configurable TTL (default: 5 minutes)
- LRU eviction when full
- Improves response time for repeated queries

#### Query Router

**Module**: `leindex.global_index.query_router`

Intelligently routes queries to appropriate projects:

```python
from leindex.global_index import QueryRouter, QueryResult

# Create query router
router = QueryRouter(tier1_index)

# Route query to relevant projects
result: QueryResult = router.route(
    query="authentication flow",
    project_ids=["project1", "project2", "project3"],
    fuzzy=True
)

# Access merged results
for match in result.matches:
    print(f"{match.project_id}: {match.file_path}:{match.line_number}")
```

**Features**:
- Intelligent query routing
- Result merging and ranking
- Fuzzy search support
- Context lines extraction
- Project-specific result aggregation

#### Graceful Degradation

**Module**: `leindex.global_index.graceful_degradation`

Automatic fallback to alternative search methods:

```python
from leindex.global_index import execute_with_degradation

# Execute search with automatic fallback
result = execute_with_degradation(
    search_func=lambda: search_with_leann(query),
    fallback_chain=[
        fallback_from_leann,  # Try Tantivy
        fallback_from_tantivy,  # Try ripgrep
        fallback_to_grep  # Try grep
    ]
)

# Check degradation level
if result.degraded:
    print(f"Search degraded: {result.fallback_reason}")
```

**Fallback Chain**:
1. **LEANN** (vector search) - Best semantic understanding
2. **Tantivy** (full-text) - Fast lexical search
3. **Ripgrep** - Fast regex search
4. **Grep** - Basic text search

## Usage

### Python API

#### Cross-Project Search

```python
from leindex.global_index import cross_project_search, CrossProjectSearchResult

# Search across multiple projects
results: CrossProjectSearchResult = cross_project_search(
    pattern="authentication",
    project_ids=["project-a", "project-b", "project-c"],
    fuzzy=True,
    case_sensitive=False,
    context_lines=2,
    max_results_per_project=100
)

# Iterate through results
for project_result in results.project_results:
    print(f"\n{project_result.project_id}:")
    print(f"  Matches: {project_result.matches}")
    print(f"  Status: {project_result.status}")

    for match in project_result.results:
        print(f"  - {match.file_path}:{match.line_number}")
        print(f"    Score: {match.score:.3f}")
        if match.context:
            print(f"    Context: {match.context[0]}")
```

#### Project Comparison Dashboard

```python
from leindex.global_index import get_dashboard_data, get_project_comparison

# Get dashboard data with filters
dashboard = get_dashboard_data(
    language="Python",
    min_health_score=0.8,
    sort_by="last_indexed",
    sort_order="descending"
)

# Access statistics
print(f"Total Projects: {dashboard.total_projects}")
print(f"Total Symbols: {dashboard.total_symbols}")
print(f"Average Health: {dashboard.average_health_score:.2f}")

# Compare specific projects
comparison = get_project_comparison(
    project_ids=["project-a", "project-b"],
    metrics=["file_count", "symbol_count", "health_score"]
)

for project_id, metrics in comparison.items():
    print(f"\n{project_id}:")
    for metric, value in metrics.items():
        print(f"  {metric}: {value}")
```

#### Global Statistics

```python
from leindex.global_index import GlobalIndexTier1

# Get global statistics
tier1 = GlobalIndexTier1()
stats = tier1.get_global_stats()

print(f"Total Projects: {stats.total_projects}")
print(f"Total Symbols: {stats.total_symbols}")
print(f"Total Files: {stats.total_files}")
print(f"Languages: {stats.languages}")
print(f"Average Health Score: {stats.average_health_score:.2f}")
print(f"Total Size: {stats.total_size_mb:.1f} MB")
```

### MCP Tools

#### get_global_stats

Get aggregate statistics across all indexed projects:

```json
{
  "name": "get_global_stats",
  "arguments": {}
}
```

**Response**:
```json
{
  "total_projects": 5,
  "total_symbols": 50000,
  "total_files": 250,
  "languages": {
    "Python": 150,
    "JavaScript": 100
  },
  "average_health_score": 0.85,
  "total_size_mb": 125.5,
  "last_updated": 1234567890.0
}
```

#### list_projects

List all projects with optional filtering:

```json
{
  "name": "list_projects",
  "arguments": {
    "status": "completed",
    "language": "Python",
    "min_health_score": 0.8,
    "format": "detailed"
  }
}
```

**Response**:
```json
{
  "projects": [
    {
      "id": "/path/to/project",
      "name": "project-a",
      "status": "completed",
      "primary_language": "Python",
      "file_count": 150,
      "symbol_count": 5000,
      "health_score": 0.92,
      "last_indexed": "2025-01-08T12:00:00",
      "size_mb": 25.5
    }
  ]
}
```

#### cross_project_search_tool

Search across multiple projects:

```json
{
  "name": "cross_project_search_tool",
  "arguments": {
    "pattern": "authentication",
    "project_ids": ["project-a", "project-b"],
    "fuzzy": true,
    "case_sensitive": false,
    "context_lines": 2,
    "max_results_per_project": 100
  }
}
```

**Response**:
```json
{
  "total_results": 45,
  "successful_projects": 2,
  "failed_projects": 0,
  "project_results": [
    {
      "project_id": "project-a",
      "matches": 30,
      "status": "success",
      "results": [...]
    }
  ]
}
```

#### get_dashboard

Get project comparison dashboard:

```json
{
  "name": "get_dashboard",
  "arguments": {
    "language": "Python",
    "min_health_score": 0.8,
    "sort_by": "last_indexed",
    "sort_order": "descending"
  }
}
```

**Response**:
```json
{
  "total_projects": 3,
  "total_symbols": 15000,
  "total_files": 75,
  "languages": {
    "Python": 75
  },
  "average_health_score": 0.88,
  "total_size_mb": 45.2,
  "projects": [...]
}
```

## Configuration

### Global Index Settings

```yaml
# config.yaml
global_index:
  # Tier 1: Metadata settings
  tier1:
    enabled: true
    auto_refresh: true
    refresh_interval_seconds: 60

  # Tier 2: Query cache settings
  tier2:
    enabled: true
    max_size: 1000  # Maximum cached queries
    ttl_seconds: 300  # Cache TTL (5 minutes)
    stale_allowed: true  # Serve stale results

  # Query routing
  query_router:
    max_concurrent_queries: 10
    query_timeout_seconds: 30
    merge_strategy: "weighted"  # weighted, ranked, simple

  # Graceful degradation
  graceful_degradation:
    enabled: true
    fallback_chain:
      - "leann"
      - "tantivy"
      - "ripgrep"
      - "grep"
    max_fallback_depth: 4
```

## Event System

### Event Types

The Global Index uses an event-driven architecture to stay synchronized:

```python
from leindex.global_index.events import (
    ProjectIndexedEvent,
    ProjectUpdatedEvent,
    ProjectDeletedEvent
)

# Subscribe to events
def on_project_indexed(event: ProjectIndexedEvent):
    print(f"Project indexed: {event.project_id}")
    # Update Tier 1 metadata
    tier1.update_project_metadata(event.project_id)

def on_project_updated(event: ProjectUpdatedEvent):
    print(f"Project updated: {event.project_id}")
    # Invalidate stale cache entries
    tier2.invalidate_project(event.project_id)

def on_project_deleted(event: ProjectDeletedEvent):
    print(f"Project deleted: {event.project_id}")
    # Remove from all tiers
    tier1.remove_project(event.project_id)
    tier2.remove_project(event.project_id)
```

### Publishing Events

```python
from leindex.global_index.event_bus import EventBus

# Get event bus
event_bus = EventBus.get_instance()

# Publish events
event_bus.publish(ProjectIndexedEvent(
    project_id="/path/to/project",
    timestamp=time.time(),
    metadata={
        "file_count": 100,
        "symbol_count": 500,
        "primary_language": "Python"
    }
))
```

## Performance Characteristics

### Response Times

| Operation | Tier 1 Only | Tier 2 Cache | Full Search |
|-----------|-------------|--------------|-------------|
| Project Metadata | <1ms | N/A | N/A |
| Cached Query | N/A | <5ms | N/A |
| Cross-Project Search (3 projects) | N/A | <10ms | 50-200ms |
| Dashboard Query | <10ms | N/A | N/A |

### Scalability

| Metric | Small (<10 projects) | Medium (10-50) | Large (50+) |
|--------|---------------------|----------------|-------------|
| Memory Overhead | ~50MB | ~200MB | ~500MB |
| Query Latency | <50ms | <100ms | <200ms |
| Index Sync Time | <1s | <5s | <10s |

### Cache Hit Rates

Typical cache hit rates for Tier 2:

- Development workflows: 60-80%
- Code review workflows: 40-60%
- Exploration workflows: 20-40%

## Security

### Path Validation

The Global Index validates all project paths to prevent directory traversal:

```python
from leindex.global_index.security import validate_project_path

# Validate project path
is_valid, error = validate_project_path(user_input)
if not is_valid:
    raise ValueError(f"Invalid project path: {error}")
```

### Access Control

Project-level access control ensures users can only search indexed projects:

```python
from leindex.global_index.security import check_project_access

# Check access before searching
if not check_project_access(user_id, project_id):
    raise PermissionError(f"No access to project: {project_id}")
```

## Troubleshooting

### High Memory Usage

**Problem**: Global Index consuming too much memory

**Solution**:
1. Reduce Tier 2 cache size:
   ```yaml
   global_index:
     tier2:
       max_size: 500  # Reduce from 1000
   ```

2. Reduce TTL:
   ```yaml
   global_index:
     tier2:
       ttl_seconds: 60  # Reduce from 300
   ```

### Slow Cross-Project Search

**Problem**: Cross-project search taking too long

**Solution**:
1. Increase query timeout:
   ```yaml
   global_index:
     query_router:
       query_timeout_seconds: 60  # Increase from 30
   ```

2. Enable more aggressive caching:
   ```yaml
   global_index:
     tier2:
       max_size: 2000  # Increase cache size
   ```

### Stale Metadata

**Problem**: Tier 1 metadata not updating

**Solution**:
1. Check auto-refresh is enabled:
   ```yaml
   global_index:
     tier1:
       auto_refresh: true
       refresh_interval_seconds: 60
   ```

2. Manually trigger refresh:
   ```python
   tier1.refresh_project_metadata(project_id)
   ```

## Best Practices

### 1. Use Tier 2 for Repeated Queries

```python
# Good: Uses cache
for i in range(100):
    results = cross_project_search("authentication")

# Better: Explicit caching
tier2 = GlobalIndexTier2()
cached = tier2.get(query_hash)
if not cached:
    results = cross_project_search("authentication")
    tier2.put(query_hash, results)
```

### 2. Filter Projects When Possible

```python
# Good: Searches all projects
results = cross_project_search("authentication")

# Better: Filters relevant projects
results = cross_project_search(
    "authentication",
    project_ids=["auth-service", "user-api"]
)
```

### 3. Use Appropriate Fuzzy Levels

```python
# For exact matches
results = cross_project_search("func_name", fuzzy=False)

# For typo tolerance
results = cross_project_search("func_name", fuzzy=True, fuzziness_level="AUTO")

# For approximate matching
results = cross_project_search("func_name", fuzzy=True, fuzziness_level="2")
```

### 4. Monitor Health Scores

```python
# Check project health before searching
projects = list_projects(min_health_score=0.8)
healthy_ids = [p["id"] for p in projects["projects"]]

results = cross_project_search(
    "authentication",
    project_ids=healthy_ids
)
```

## API Reference

### Cross-Project Search

```python
def cross_project_search(
    pattern: str,
    project_ids: Optional[List[str]] = None,
    fuzzy: bool = False,
    case_sensitive: bool = True,
    context_lines: int = 0,
    max_results_per_project: int = 100,
    file_pattern: Optional[str] = None,
    use_tier2_cache: bool = True
) -> CrossProjectSearchResult
```

**Parameters**:
- `pattern`: Search pattern (regex-compatible)
- `project_ids`: List of project IDs to search (None = all)
- `fuzzy`: Enable fuzzy matching
- `case_sensitive`: Case-sensitive search
- `context_lines`: Number of context lines to extract
- `max_results_per_project`: Maximum results per project
- `file_pattern`: Filter results by file pattern
- `use_tier2_cache`: Use Tier 2 cache if available

**Returns**: `CrossProjectSearchResult` with aggregated results

### Dashboard Functions

```python
def get_dashboard_data(
    status: Optional[str] = None,
    language: Optional[str] = None,
    min_health_score: Optional[float] = None,
    max_health_score: Optional[float] = None,
    sort_by: str = "last_indexed",
    sort_order: str = "descending"
) -> DashboardData
```

**Parameters**:
- `status`: Filter by index status ("completed", "building", "error")
- `language`: Filter by primary language
- `min_health_score`: Minimum health score (0.0 - 1.0)
- `max_health_score`: Maximum health score (0.0 - 1.0)
- `sort_by`: Sort field ("name", "size", "health_score", "last_indexed")
- `sort_order`: Sort order ("ascending", "descending")

**Returns**: `DashboardData` with filtered projects

## See Also

- [docs/MEMORY_MANAGEMENT.md](MEMORY_MANAGEMENT.md) - Memory management for global index
- [docs/CONFIGURATION.md](CONFIGURATION.md) - Configuration reference
- [examples/cross_project_search.py](../examples/cross_project_search.py) - Usage examples
- [examples/dashboard_usage.py](../examples/dashboard_usage.py) - Dashboard examples
