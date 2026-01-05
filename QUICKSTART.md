# LeIndex Quick Start Guide

Get up and running with LeIndex in 5 minutes! This guide will walk you through the essentials of indexing and searching your codebase.

---

## Table of Contents

- [Installation](#installation)
- [Basic Usage](#basic-usage)
- [Common Workflows](#common-workflows)
- [MCP Integration](#mcp-integration)
- [Next Steps](#next-steps)

---

## Installation

### Step 1: Install LeIndex

```bash
pip install leindex
```

That's it! No Docker, no databases to configure.

### Step 2: Verify Installation

```bash
leindex --version
leindex-search --help
```

---

## Basic Usage

### Index Your Codebase

```bash
# Navigate to your project directory
cd /path/to/your/project

# Initialize the index
leindex init .

# Index all files
leindex index .
```

### Search Your Code

```bash
# Semantic search (find code by meaning)
leindex-search "how does authentication work?"

# Full-text search (find exact text)
leindex-search "def authenticate" --backend tantivy

# Symbol search (find definitions)
leindex-search "class User" --backend symbol

# Regex search (find patterns)
leindex-search "TODO.*fix" --backend regex
```

---

## Common Workflows

### Workflow 1: First-Time Setup

```bash
# 1. Install LeIndex
pip install leindex

# 2. Navigate to your project
cd ~/my-project

# 3. Initialize the index
leindex init .

# 4. Index your code
leindex index .

# 5. Search!
leindex-search "authentication logic"
```

### Workflow 2: Search with Filters

```bash
# Search only Python files
leindex-search "database" --file-pattern "*.py"

# Exclude test files
leindex-search "API" --exclude "*test*.py"

# Search specific directory
leindex-search "config" --path ./src/config
```

### Workflow 3: MCP Integration with Claude

1. **Configure MCP server** in your Claude settings:

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

2. **Restart Claude** to load the MCP server

3. **Use LeIndex tools** directly in Claude:
   - `search_code_advanced` - Search your codebase
   - `index_repository` - Index your code
   - `refresh_index` - Update the index

4. **Example Claude conversation**:
   ```
   You: Can you find where authentication is implemented?
   Claude: [Uses LeIndex to search for authentication code]
   Found authentication implementation in src/auth/login.py...
   ```

### Workflow 4: Continuous Indexing

```bash
# Start watch mode (auto-update on file changes)
leindex update . --watch

# In another terminal, search as usual
leindex-search "new feature"
```

### Workflow 5: Multiple Projects

```bash
# Index multiple projects
leindex init ~/project1
leindex index ~/project1

leindex init ~/project2
leindex index ~/project2

# Search across all projects
leindex-search "shared utility"

# Or search specific project
leindex-search "config" --path ~/project1
```

---

## MCP Integration

### What is MCP?

The Model Context Protocol (MCP) lets AI assistants like Claude interact directly with your codebase through LeIndex.

### Setting Up MCP

#### Claude Desktop

1. Open Claude Desktop settings
2. Navigate to "MCP Servers"
3. Add LeIndex server:

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

4. Restart Claude Desktop

#### Cursor IDE

1. Open Cursor settings (Cmd/Ctrl + ,)
2. Search for "MCP"
3. Add LeIndex server configuration
4. Restart Cursor

#### Other MCP Clients

Check your client's documentation for MCP server configuration.

### Using MCP with Claude

Once configured, Claude can:

1. **Search your code** - Ask Claude to find code by meaning
2. **Explain code** - Get explanations with full context
3. **Refactor code** - Find similar patterns to refactor
4. **Answer questions** - Get accurate answers about your codebase

**Example queries:**
- "Where is the authentication logic?"
- "How do I handle errors in this codebase?"
- "Find all functions that call the database"
- "Show me examples of API endpoints"

---

## Configuration

### Minimal Configuration

LeIndex works out of the box, but you can customize it:

Create `~/.leindex/config.yaml`:

```yaml
# Data storage
dal_settings:
  backend_type: "sqlite_duckdb"
  db_path: "./data/leindex.db"
  duckdb_db_path: "./data/leindex.db.duckdb"

# Vector search
vector_store:
  backend_type: "leann"
  index_path: "./leann_index"
  embedding_model: "nomic-ai/CodeRankEmbed"

# File filtering
file_filtering:
  max_file_size: 1073741824  # 1GB
  type_specific_limits:
    ".py": 1073741824
    ".js": 1073741824

directory_filtering:
  skip_large_directories:
    - "**/node_modules/**"
    - "**/.git/**"
    - "**/venv/**"
```

### Common Configurations

#### For Small Projects (<10K files)

```yaml
async_processing:
  worker_count: 2

memory:
  soft_limit_mb: 2048
```

#### For Large Projects (>100K files)

```yaml
async_processing:
  worker_count: 8
  max_queue_size: 10000

memory:
  soft_limit_mb: 16384
  hard_limit_mb: 32768

directory_filtering:
  max_files_per_directory: 1000000
```

---

## Tips and Best Practices

### 1. First-Time Indexing

```bash
# Start with a smaller directory to test
leindex index ./src

# Then index the full project
leindex index .
```

### 2. Incremental Updates

```bash
# After making changes, update only modified files
leindex update .

# Or use watch mode for continuous updates
leindex update . --watch
```

### 3. Search Strategies

**Semantic Search** (default, best for understanding intent):
```bash
leindex-search "how to handle errors"
```

**Full-Text Search** (best for exact text):
```bash
leindex-search "def handle_error" --backend tantivy
```

**Symbol Search** (best for finding definitions):
```bash
leindex-search "class ErrorHandler" --backend symbol
```

### 4. Performance Tips

- **Use filters** to narrow search scope: `--file-pattern "*.py"`
- **Exclude noise**: `--exclude "*test*.py"`
- **Be specific**: "authentication login flow" vs "authentication"
- **Use appropriate backend**: `tantivy` for exact text, `semantic` for meaning

---

## Troubleshooting

### Issue: LeIndex not found

```bash
# Make sure you installed it
pip install leindex

# Check installation
which leindex
```

### Issue: Model download fails

```bash
# Set HuggingFace cache
export HF_HOME=~/.cache/huggingface

# Try again
leindex index .
```

### Issue: Out of memory

```bash
# Reduce workers in config.yaml
async_processing:
  worker_count: 2
```

### Issue: No search results

```bash
# Make sure you indexed first
leindex index .

# Try a broader search
leindex-search "function"  # instead of specific name
```

---

## Next Steps

### Learn More

- [Full Installation Guide](INSTALLATION.md) - Detailed installation instructions
- [Architecture Documentation](ARCHITECTURE.md) - System design and internals
- [API Reference](API.md) - Complete API documentation
- [MCP Configuration](MCP_CONFIGURATION.md) - Advanced MCP setup

### Advanced Features

- **Hybrid Search** - Combine semantic and full-text search
- **Analytics** - Use DuckDB for code metrics
- **Custom Embeddings** - Use your own embedding models
- **Multi-Language Support** - Index any programming language

### Contribute

- [GitHub Repository](https://github.com/scooter-lacroix/leindex)
- [Report Issues](https://github.com/scooter-lacroix/leindex/issues)
- [Contributing Guide](CONTRIBUTING.md)

---

**Happy Searching!** 🚀

If you find LeIndex useful, please star us on GitHub!
