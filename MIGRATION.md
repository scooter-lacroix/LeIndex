# LeIndex Migration Guide

Migrating from Python v2.0.2 to Rust v0.1.0

---

## Overview

LeIndex v0.1.0 is a **complete rewrite** from Python to Rust. This document helps you migrate from the old Python-based LeIndex v2.0.2 to the new Rust implementation.

### Key Changes

| Aspect | Python v2.0.2 | Rust v0.1.0 |
|--------|---------------|-------------|
| **Language** | Python 3.10+ | Rust 1.75+ |
| **Installation** | `pip install leindex` | `cargo build --release` |
| **Vector Search** | LEANN (file-based) | HNSW (in-memory, temporary) |
| **Full-Text Search** | Tantivy-py | Not yet implemented |
| **Metadata Storage** | SQLite | Turso/libsql (planned) |
| **Analytics** | DuckDB | Not yet implemented |
| **Binary Name** | `leindex` | `leindex` (same!) |
| **Config Format** | YAML | TOML |

---

## Breaking Changes

### 1. Installation Method

**Before (Python):**
```bash
pip install leindex
```

**After (Rust):**
```bash
cargo build --release --bins
# Or use the installer
./install.sh
```

### 2. Configuration Format

**Before (Python - YAML):**
```yaml
dal_settings:
  backend_type: "sqlite_duckdb"
  db_path: "./data/leindex.db"
  duckdb_db_path: "./data/leindex.db.duckdb"

vector_store:
  backend_type: "leann"
  index_path: "./leann_index"
  embedding_model: "nomic-ai/CodeRankEmbed"
  embedding_dim: 768

async_processing:
  enabled: true
  worker_count: 4
  max_queue_size: 10000
```

**After (Rust - TOML):**
```toml
[memory]
total_budget_mb = 3072
soft_limit_percent = 0.80
hard_limit_percent = 0.93
emergency_percent = 0.98

[file_filtering]
max_file_size = 1073741824
exclude_patterns = [
    "**/node_modules/**",
    "**/.git/**",
    "**/target/**"
]

[parsing]
batch_size = 100
parallel_parsers = 4
```

### 3. Python API Removed

The Python API is no longer available. Use the CLI or MCP server instead.

**Before (Python):**
```python
from leindex import LeIndex

indexer = LeIndex("~/my-project")
indexer.index()
results = indexer.search("authentication")
```

**After (Rust - CLI):**
```bash
leindex index ~/my-project
leindex search "authentication"
```

**After (Rust - MCP):**
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

---

## Feature Parity Status

### ✅ Implemented (100% Parity)

- **CLI Commands** - All core commands available
- **MCP Server** - Full MCP protocol support
- **Tree-sitter Parsing** - 11+ languages supported
- **Memory Management** - Advanced cache spilling
- **Project Configuration** - TOML-based settings

### ⚠️ Partial Implementation (Temporary)

- **Vector Search** - HNSW in-memory (temporary, will be Turso/libsql)
- **Persistence** - Not yet implemented (planned for v0.2.0)

### ❌ Not Yet Implemented

- **Full-text Search** - Tantivy integration planned
- **Analytics** - DuckDB queries not available
- **Cross-Project Search** - Planned for v0.3.0
- **Global Index Dashboard** - Planned for v0.3.0

---

## Data Migration

### Python Index Data

The Python version stored data in:
- `~/.leindex/data/` - SQLite database
- `~/.leindex/leann_index/` - LEANN vector index
- `./leann_index/` - Project-local LEANN index

**Action Required:**

The Rust implementation uses a different storage format. **You must re-index your projects:**

```bash
# Re-index each project
leindex index /path/to/project

# Or index multiple projects
for project in project1 project2 project3; do
    leindex index ~/code/$project
done
```

### Configuration Migration

If you have custom `config.yaml` files, you'll need to convert them to TOML format.

**Example Migration:**

```yaml
# config.yaml (Python)
file_filtering:
  max_file_size: 104857600
  exclude_patterns:
    - "**/node_modules/**"
    - "**/dist/**"
```

```toml
# leindex.toml (Rust)
[file_filtering]
max_file_size = 104857600
exclude_patterns = [
    "**/node_modules/**",
    "**/dist/**"
]
```

---

## MCP Configuration

### No Changes Required

Good news! The MCP configuration **doesn't need to change** because the binary name is still `leindex`:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"],
      "env": {}
    }
  }
}
```

However, the available MCP tools have changed:

| Python v2.0.2 | Rust v0.1.0 |
|---------------|-------------|
| `manage_project` | `leindex_index` |
| `search_content` | `leindex_search` |
| `get_diagnostics` | `leindex_diagnostics` |
| N/A | `leindex_deep_analyze` (NEW) |
| N/A | `leindex_context` (NEW) |

---

## Rollback Instructions

If you need to revert to Python v2.0.2:

### 1. Uninstall Rust Version

```bash
# Remove Rust binary
rm -f ~/.leindex/bin/leindex

# Remove data (optional - keep if you want to preserve data)
rm -rf ~/.leindex
```

### 2. Reinstall Python Version

```bash
# Install from PyPI
pip install leindex==2.0.2

# Or from source
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
git checkout python-v2.0.2
pip install -e .
```

### 3. Restore MCP Configuration

The MCP configuration doesn't need to change - it uses the same binary name.

---

## Known Limitations (v0.1.0)

### Temporary Workarounds

1. **In-Memory Vectors Only**
   - Vectors are stored in HNSW memory structure
   - **Workaround:** Re-index after restart
   - **Fix:** v0.2.0 will add Turso/libsql persistence

2. **No Full-Text Search**
   - Tantivy integration not yet complete
   - **Workaround:** Use semantic search and file patterns
   - **Fix:** Planned for future release

3. **Swift/Kotlin/Dart Parsers Disabled**
   - Tree-sitter version conflicts
   - **Workaround:** Use other supported languages
   - **Fix:** Planned for v0.3.0

---

## Performance Comparison

| Metric | Python v2.0.2 | Rust v0.1.0 | Status |
|--------|---------------|-------------|--------|
| **Indexing Speed** | ~10K files/min | ~10K files/min (target) | 🎯 Target |
| **Search Latency (p50)** | ~50ms | ~50ms (target) | 🎯 Target |
| **Memory Usage** | <3GB | <3GB (target) | 🎯 Target |
| **Startup Time** | ~2s | <1s (expected) | ⚡ Faster |
| **Binary Size** | N/A (Python) | ~10MB | ✅ Self-contained |

---

## Getting Help

### Migration Issues

If you encounter problems during migration:

1. **Check Diagnostics**
   ```bash
   leindex diagnostics
   ```

2. **Review Logs**
   ```bash
   cat ~/.leindex/logs/install-*.log
   cat ~/.leindex/logs/leindex.log
   ```

3. **Report Issues**
   - GitHub: [https://github.com/scooter-lacroix/leindex/issues](https://github.com/scooter-lacroix/leindex/issues)
   - Include: LeIndex version, OS, and error messages

### Documentation

- [Installation Guide](INSTALLATION_RUST.md) - Setup instructions
- [Architecture](ARCHITECTURE.md) - System design
- [README](README.md) - Project overview

---

## Summary

**Quick Migration Checklist:**

- [ ] Uninstall Python version: `pip uninstall leindex`
- [ ] Install Rust: `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- [ ] Build LeIndex: `cargo build --release`
- [ ] Add binary to PATH
- [ ] Re-index projects: `leindex index /path/to/project`
- [ ] Update MCP configuration (optional - same binary name)
- [ ] Verify: `leindex --version`

**Welcome to LeIndex Rust!** 🚀

---

*Last Updated: 2025-01-26*
*LeIndex v0.1.0*
