# LeIndex API Reference: Your Code Search Playground 🎮

<div align="center">

**Beautiful APIs for Beautiful Code Search**

*Everything you need to search, index, and understand your code*

</div>

---

## Table of Contents

- [CLI Tools](#cli-tools)
- [Python API](#python-api)
- [MCP Server](#mcp-server)
- [Configuration](#configuration)
- [Data Models](#data-models)
- [Error Handling](#error-handling)
- [Examples](#examples)

---

## CLI Tools

### leindex

The main CLI tool for indexing and management. Simple, powerful, and fast.

#### Commands

##### `leindex init <path>`

Initialize a new project for indexing.

```bash
leindex init /path/to/project
```

**Options:**
- `--config, -c <path>` - Custom config file path
- `--exclude <pattern>` - Additional exclude patterns
- `--force` - Reinitialize even if already initialized

**Example:**
```bash
leindex init ~/my-project --exclude "**/test_*.py"
```

**What it does:**
- Creates project configuration
- Sets up index directories
- Configures file filters
- Gets you ready to index!

##### `leindex index <path>`

Index files in the specified path. This is where the magic happens.

```bash
leindex index /path/to/project
```

**Options:**
- `--force` - Reindex all files (ignore cache)
- `--parallel <n>` - Number of parallel workers (default: 4)
- `--batch-size <n>` - Batch size for processing (default: 100)
- `--verbose, -v` - Verbose output
- `--progress` - Show progress bar

**Example:**
```bash
leindex index ~/my-project --parallel 8 --progress
```

**What happens:**
- Scans all files in your project
- Extracts symbols and code structure
- Generates semantic embeddings
- Builds full-text index
- Stores metadata
- **Speed:** ~10K files/minute

##### `leindex update <path>`

Incrementally update index for changed files. Smart and efficient.

```bash
leindex update /path/to/project
```

**Options:**
- `--watch` - Watch for changes and auto-update
- `--interval <seconds>` - Check interval for watch mode (default: 5)

**Example:**
```bash
leindex update ~/my-project --watch
```

**What happens:**
- Detects changed files
- Only reindexes what's needed
- Keeps your index fresh

##### `leindex remove <path>`

Remove a project from the index.

```bash
leindex remove /path/to/project
```

**Options:**
- `--purge` - Also remove indexed data
- `--confirm` - Skip confirmation prompt

**Example:**
```bash
leindex remove ~/my-project --purge
```

##### `leindex stats [path]`

Show statistics for indexed projects.

```bash
leindex stats
leindex stats /path/to/project
```

**Output:**
```
Project: /home/user/my-project
Files indexed: 1,234
Symbols extracted: 5,678
Index size: 123 MB
Last indexed: 2024-01-04 10:30:00
```

##### `leindex list`

List all indexed projects.

```bash
leindex list
```

**Output:**
```
Indexed Projects:
  /home/user/project1 (1,234 files)
  /home/user/project2 (567 files)
```

---

### leindex-search

The search CLI tool. Lightning-fast code search at your fingertips.

#### Commands

##### `leindex-search <query>`

Search for code.

```bash
leindex-search "authentication logic"
```

**Options:**
- `--backend <name>` - Search backend: `semantic`, `tantivy`, `regex`, `symbol` (default: semantic)
- `--limit <n>` - Maximum results (default: 100)
- `--offset <n>` - Skip first N results
- `--path <path>` - Search in specific project
- `--file-pattern <pattern>` - Filter by file pattern (e.g., `*.py`)
- `--exclude <pattern>` - Exclude file patterns
- `--context <n>` - Lines of context (default: 3)
- `--content` - Show file content
- `--highlight` - Highlight matches
- `--export <format>` - Export format: `json`, `csv`, `text`
- `--output <path>` - Output file

**Examples:**

Semantic search (finds by meaning):
```bash
leindex-search "how does authentication work?"
```

Full-text search (finds by keywords):
```bash
leindex-search "def authenticate" --backend tantivy
```

Symbol search (finds definitions):
```bash
leindex-search "class User" --backend symbol
```

Regex search (finds by pattern):
```bash
leindex-search "TODO.*fix" --backend regex
```

Filter by file type:
```bash
leindex-search "database" --ext py --exclude test_*
```

Export results:
```bash
leindex-search "database" --export json --output results.json
```

---

## Python API

### Core Classes

#### `LeIndex`

The main entry point for programmatic access. Beautiful, simple, powerful.

```python
from leindex import LeIndex

# Initialize
indexer = LeIndex(project_path="~/my-project")

# Index files
await indexer.index()

# Search
results = await indexer.search("authentication")

# Close
await indexer.close()
```

**Methods:**

##### `async index(force: bool = False) -> None`

Index all files in the project.

```python
await indexer.index(force=True)
```

**What happens:**
- Discovers all files
- Extracts symbols and code structure
- Generates semantic embeddings
- Builds full-text index
- Stores metadata
- **Returns:** Nothing (it's async!)

##### `async update() -> None`

Update index for changed files.

```python
await indexer.update()
```

**What happens:**
- Detects changed files
- Only reindexes what's needed
- **Speed:** 100x faster than full reindex

##### `async search(query: str, **kwargs) -> SearchResults`

Search for code. This is where the magic happens.

```python
results = await indexer.search(
    query="authentication",
    backend="semantic",
    limit=10
)

for result in results:
    print(f"{result.file}:{result.line}")
    print(result.content)
    print(f"Score: {result.score}")
```

**Parameters:**
- `query` - Search query string
- `backend` - Search backend (default: "semantic")
- `limit` - Maximum results (default: 100)
- `file_patterns` - Filter by file patterns
- `exclude_patterns` - Exclude file patterns

**Returns:** `SearchResults` object

##### `async get_stats() -> ProjectStats`

Get project statistics.

```python
stats = await indexer.get_stats()
print(f"Files: {stats.files_indexed}")
print(f"Symbols: {stats.symbols_count}")
```

#### `SearchResults`

Container for search results. Iterable and powerful.

```python
results = await indexer.search("query")

# Iterate
for result in results:
    print(result.file)

# Access as list
all_results = results.all()

# Filter
python_results = results.filter(language="python")

# Sort
sorted_results = results.sort(key=lambda r: r.score, reverse=True)
```

**Methods:**
- `all()` - Return all results as list
- `filter(**kwargs)` - Filter results
- `sort(key, reverse=False)` - Sort results
- `limit(n)` - Limit to N results

**Properties:**
- `total` - Total number of results
- `query` - Original query
- `backend` - Backend used

#### `SearchResult`

Individual search result. Rich and informative.

```python
result = results[0]

print(result.file)        # File path
print(result.line)        # Line number
print(result.content)     # Code content
print(result.score)       # Relevance score
print(result.language)    # Programming language
print(result.symbols)     # Symbols in result
```

**Properties:**
- `file` - File path (str)
- `line` - Line number (int)
- `content` - Code content (str)
- `score` - Relevance score (float)
- `language` - Programming language (str)
- `symbols` - List of symbols (List[Symbol])
- `context` - Context lines (Dict[int, str])

#### `CoreEngine`

Low-level search and indexing engine. For power users.

```python
from leindex.core_engine import CoreEngine

engine = CoreEngine()

# Search
results = await engine.search("query")

# Index file
await engine.index_file("/path/to/file.py")

# Get statistics
stats = await engine.get_stats()
```

---

## MCP Server

### Starting the Server

So simple, it's magical.

```bash
leindex mcp
```

Or via MCP client configuration:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"]
    }
  }
}
```

### Available Tools

#### `manage_project`

Set up and manage indexing for your projects.

**Parameters:**
```json
{
  "action": "string (required)",
  "path": "string (optional)",
  "options": "object (optional)"
}
```

**Actions:**
- `init` - Initialize a new project
- `index` - Index files
- `update` - Update index
- `remove` - Remove project

**Example:**
```json
{
  "action": "index",
  "path": "/path/to/project"
}
```

#### `search_content`

Search code with semantic + full-text powers.

**Parameters:**
```json
{
  "query": "string (required)",
  "backend": "string (optional, default: semantic)",
  "limit": "number (optional, default: 100)",
  "file_patterns": "array<string> (optional)",
  "exclude_patterns": "array<string> (optional)"
}
```

**Response:**
```json
{
  "results": [
    {
      "file": "string",
      "line": "number",
      "content": "string",
      "score": "number",
      "language": "string",
      "symbols": ["string"]
    }
  ],
  "total": "number"
}
```

**Example:**
```json
{
  "query": "authentication logic",
  "backend": "semantic",
  "limit": 10
}
```

#### `get_diagnostics`

Get project stats and health checks.

**Parameters:**
```json
{
  "path": "string (optional)"
}
```

**Response:**
```json
{
  "projects": [
    {
      "path": "string",
      "files_indexed": "number",
      "symbols_count": "number",
      "index_size_mb": "number",
      "last_indexed": "string"
    }
  ]
}
```

---

## Configuration

### Config File

Location: `~/.leindex/config.yaml`

```yaml
# Storage paths
storage:
  sqlite_path: ~/.leindex/data/metadata.db
  tantivy_path: ~/.leindex/data/ft_index
  leann_path: ~/.leindex/data/vector_index
  duckdb_path: ~/.leindex/data/analytics.db

# Indexing settings
indexing:
  max_file_size: 104857600  # 100MB
  batch_size: 100
  workers: 4
  exclude_patterns:
    - "**/node_modules/**"
    - "**/.git/**"
    - "**/venv/**"
    - "**/__pycache__/**"
    - "**/dist/**"
    - "**/build/**"

# Search settings
search:
  default_backend: semantic  # semantic, tantivy, regex
  semantic_threshold: 0.7
  tantivy_bm25: true
  max_results: 100
  snippet_length: 200

# Performance settings
performance:
  workers: 4
  memory_limit_mb: 4096
  enable_caching: true

# Embedding model settings
embeddings:
  model: nomic-ai/CodeRankEmbed
  device: cpu  # cpu or cuda
  batch_size: 32
```

### Environment Variables

```bash
# LeIndex home directory
export LEINDEX_HOME=~/.leindex

# Config file path
export LEINDEX_CONFIG=~/.leindex/config.yaml

# Log level
export LEINDEX_LOG_LEVEL=INFO

# Data directory
export LEINDEX_DATA_DIR=~/.leindex/data

# Disable auto model download
export LEINDEX_NO_AUTO_DOWNLOAD_MODELS=0
```

---

## Data Models

### FileMetadata

```python
@dataclass
class FileMetadata:
    path: str
    hash: str
    size: int
    mtime: float
    language: str
    indexed_at: datetime
```

### Symbol

```python
@dataclass
class Symbol:
    name: str
    kind: str  # function, class, variable
    file_path: str
    line_start: int
    line_end: int
    docstring: Optional[str]
    signature: Optional[str]
```

### Reference

```python
@dataclass
class Reference:
    from_file: str
    from_line: int
    to_symbol: int
    ref_kind: str  # call, import, usage
```

### SearchOptions

```python
@dataclass
class SearchOptions:
    backend: str = "semantic"
    limit: int = 100
    offset: int = 0
    file_patterns: List[str] = None
    exclude_patterns: List[str] = None
    semantic_threshold: float = 0.7
```

### ProjectStats

```python
@dataclass
class ProjectStats:
    project_path: str
    files_indexed: int
    symbols_count: int
    references_count: int
    index_size_bytes: int
    last_indexed: datetime
    indexing_duration_seconds: float
```

---

## Error Handling

### Exceptions

#### `LeIndexError`

Base exception for all LeIndex errors.

```python
try:
    await indexer.index()
except LeIndexError as e:
    print(f"Indexing failed: {e}")
```

#### `IndexingError`

Raised when indexing fails.

```python
try:
    await indexer.index()
except IndexingError as e:
    print(f"Failed to index: {e.file}")
```

#### `SearchError`

Raised when search fails.

```python
try:
    results = await indexer.search("query")
except SearchError as e:
    print(f"Search failed: {e}")
```

#### `ConfigurationError`

Raised when configuration is invalid.

```python
try:
    indexer = LeIndex(config="invalid.yaml")
except ConfigurationError as e:
    print(f"Invalid config: {e}")
```

---

## Examples

### Basic Usage

```python
from leindex import LeIndex

# Initialize
indexer = LeIndex("~/my-project")

# Index
await indexer.index()

# Search
results = await indexer.search("authentication")

# Display results
for result in results:
    print(f"{result.file}:{result.line}")
    print(result.content[:100])
```

### Advanced Search

```python
# Search with filters
results = await indexer.search(
    query="database",
    backend="semantic",
    file_patterns=["*.py"],
    exclude_patterns=["test_*.py"],
    limit=20
)

# Filter results
python_results = [r for r in results if r.language == "python"]

# Sort by score
sorted_results = sorted(results, key=lambda r: r.score, reverse=True)
```

### Watch Mode

```python
import asyncio

async def watch_and_update():
    indexer = LeIndex("~/my-project")

    while True:
        await indexer.update()
        await asyncio.sleep(5)  # Check every 5 seconds

asyncio.run(watch_and_update())
```

### Custom Configuration

```python
from leindex import LeIndex
from leindex.config import Config

# Load custom config
config = Config.from_file("~/.leindex/custom_config.yaml")

# Initialize with custom config
indexer = LeIndex("~/my-project", config=config)

await indexer.index()
```

### Batch Operations

```python
# Index multiple projects
projects = ["~/project1", "~/project2", "~/project3"]

for project in projects:
    indexer = LeIndex(project)
    await indexer.index()
    print(f"Indexed {project}")

# Search across all projects
results = []
for project in projects:
    indexer = LeIndex(project)
    project_results = await indexer.search("authentication")
    results.extend(project_results)

print(f"Total results: {len(results)}")
```

---

**Ready to search your code like a wizard?** [Install LeIndex now](INSTALLATION.md) 🚀

**Questions?** Check out the [Architecture Deep Dive](ARCHITECTURE.md) or [Troubleshooting Guide](TROUBLESHOOTING.md)
