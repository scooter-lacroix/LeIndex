# LeIndex MCP Server Configuration Guide

<div align="center">

**Make Your AI Assistant Code-Aware in Minutes** 🚀

*The definitive guide to integrating LeIndex with Claude, Cursor, Windsurf, and more*

</div>

---

## ✨ What is the LeIndex MCP Server?

The LeIndex MCP Server is your bridge between powerful code search and AI assistants. It transforms your favorite AI coding partner (Claude, Cursor, Windsurf, etc.) into a code-understanding genius by giving it instant access to your entire codebase - semantically indexed and ready to query.

**Think of it as giving your AI assistant a photographic memory of your code.** 🧠⚡

### Why You'll Love It

- **🎯 Lightning Fast** - Search results in milliseconds, not seconds
- **🔍 Semantic Understanding** - Finds code by meaning, not just text matching
- **🪶 Featherweight** - Pure MCP architecture, no plugin bloat
- **💾 Token Efficient** - Saves ~200 tokens per session (no hook overhead!)
- **🔒 Privacy-First** - Everything runs locally, your code never leaves your machine
- **🔌 Universal** - Works with any MCP-compatible AI assistant

---

## 🎭 MCP Tools vs Skills: Choose Your Fighter

LeIndex gives you two ways to work your magic:

### 🛠️ MCP Tools (The Direct Approach)

Use MCP tools directly for most day-to-day operations:

**Perfect For:**
- ✅ Single-project operations
- ✅ Quick searches and lookups
- ✅ Direct API access
- ✅ CI/CD integration
- ✅ Automated scripts

**Your MCP Superpowers:**
```python
manage_project    # Set project paths & manage indexing
search_content    # Semantic + full-text code search
get_diagnostics   # Project stats & health checks
```

### 🎪 Skills (The Orchestrator)

Use the optional code-search skill for complex, multi-step workflows:

**Perfect For:**
- ✅ Multi-project batch operations
- ✅ Cross-project search with aggregation
- ✅ Complex orchestrated workflows
- ✅ Automated analysis pipelines

**Pro Tip:** Skills orchestrate MCP tools internally. They're convenient wrappers for complex operations, not replacements!

### 📊 Token Efficiency Showdown

| Architecture | Token Overhead | Winner |
|--------------|----------------|--------|
| **Hook-based** | ~200 tokens/session | ❌ |
| **Pure MCP + Skills** | ~0 tokens/session | ✅ **You win!** |

**What this means:**
- Cleaner conversation context
- Faster response times
- More efficient token usage
- Predictable, reliable behavior

---

## 🚀 Installation (Get Up and Running)

### Quick Install

```bash
# Install from PyPI
pip install leindex

# That's it. Seriously.
```

### From Source (For the Adventurous)

```bash
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
uv pip install -e .
```

### Direct UV Installation (Blazing Fast)

```bash
uvx git+https://github.com/scooter-lacroix/leindex.git
```

---

## ⚙️ Configuration Examples (Copy, Paste, Go!)

### 1️⃣ LM Studio 🎨

```json
{
  "mcpServers": {
    "leindex": {
      "command": "uvx",
      "args": ["git+https://github.com/scooter-lacroix/leindex.git"],
      "env": {},
      "start_on_launch": true
    }
  }
}
```

**Why it's cool:** Auto-starts with LM Studio, ready to search from day one! 🚀

---

### 2️⃣ VS Code / Cursor / Windsurf 💻

#### Using MCP Extension
```json
{
  "mcp.servers": {
    "leindex": {
      "command": "leindex",
      "args": [],
      "env": {},
      "transport": "stdio"
    }
  }
}
```

#### Using Continue Extension
```json
{
  "mcpServers": [
    {
      "name": "leindex",
      "command": "leindex",
      "args": [],
      "env": {}
    }
  ]
}
```

**Why it's cool:** Seamless integration with your favorite editor! Search without leaving your code. ⚡

---

### 3️⃣ Jan AI 🤖

```json
{
  "mcp_servers": {
    "leindex": {
      "command": "leindex",
      "args": [],
      "env": {}
    }
  }
}
```

**Why it's cool:** Works out of the box with Jan's modular architecture! 🔧

---

### 4️⃣ OpenHands 🖐️

```json
{
  "mcp": {
    "servers": {
      "leindex": {
        "command": "leindex",
        "args": [],
        "env": {}
      }
    }
  }
}
```

**Why it's cool:** Power up your AI coding agent with deep code understanding! 🎯

---

### 5️⃣ HTTP/HTTPS Server Mode 🌐

For web-based integrations or remote access:

```bash
# Start HTTP server
python -m leindex.server --port 8765

# Or using the convenience script
uvx leindex --http
```

**Configuration:**
```json
{
  "mcpServers": {
    "leindex": {
      "transport": "http",
      "url": "http://localhost:8765/mcp",
      "headers": {
        "Authorization": "Bearer your-token-here"
      }
    }
  }
}
```

**Why it's cool:** Access LeIndex from anywhere! Team-wide code search without the complexity. 🌍

---

### 6️⃣ Git Link Installation (The Modern Way) 📦

For environments that support git installation:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "uvx",
      "args": [
        "git+https://github.com/scooter-lacroix/leindex.git"
      ],
      "env": {}
    }
  }
}
```

**Why it's cool:** Always get the latest version automatically! Updates are a breeze. 🌬️

---

## 🎮 Usage Examples (See It in Action)

### 🔍 Using MCP Tools Directly

#### Search for Code (The Magic Show)

```json
{
  "tool": "search_content",
  "parameters": {
    "query": "authentication logic",
    "project_path": "/home/user/my_project"
  }
}
```

**What happens:**
- LeIndex searches semantically for authentication-related code
- Finds login handlers, session management, JWT verification, password hashing
- Even finds them if they're named completely differently!
- Returns ranked results with relevance scores

**It's like having a senior developer who's memorized every line of code!** 🧠✨

---

#### Manage Project Indexing (Set It Up)

```json
{
  "tool": "manage_project",
  "parameters": {
    "action": "index",
    "project_path": "/home/user/my_project"
  }
}
```

**What happens:**
- Scans your project for code files
- Indexes symbols, references, and code structure
- Builds semantic embeddings for understanding
- Ready to search in seconds!

---

#### Get Project Diagnostics (Health Check)

```json
{
  "tool": "get_diagnostics",
  "parameters": {
    "project_path": "/home/user/my_project"
  }
}
```

**What happens:**
- Returns project statistics
- Shows indexing status
- Reveals file counts and types
- Displays search metrics

---

### 🎪 Using Skills for Complex Workflows

For multi-project operations or complex orchestrated workflows, invoke the code-search skill which will internally orchestrate the appropriate MCP tools:

**User:**
> "Reindex all projects in my workspace and search for authentication patterns"

**The skill automatically:**
1. ✅ Calls `manage_project` for each project to trigger indexing
2. ✅ Calls `search_content` across all indexed projects
3. ✅ Calls `get_diagnostics` to validate results
4. ✅ Aggregates and returns comprehensive results

**It's like having a project manager for your code search!** 🎯📊

---

### 📋 Decision Tree: MCP Tools vs Skills

```
Need to search code?
    |
    ├─ Single project?
    │   └─ Use MCP Tools → search_content
    |
    ├─ Multiple projects?
    │   └─ Use Skills → Automatic orchestration
    |
    ├─ Simple query?
    │   └─ Use MCP Tools → Direct access
    |
    └─ Complex workflow?
        └─ Use Skills → Multi-step orchestration
```

---

## 🔧 CLI Tools (Power User Mode)

### Basic Search

```bash
# Search for a function
leindex-search "function_name"

# Search with path filter
leindex-search "function_name" --path "src/*"
```

### Indexing Operations

```bash
# Index a local repository
leindex init /home/user/my_project
leindex index /home/user/my_project

# Re-index with remote repository
leindex index --url "https://github.com/scooter-lacroix/LeIndex.git"
```

### HTTP Server Mode

```bash
# Start HTTP server for remote access
uvx leindex --http --port 8765
```

---

## 🌍 Environment Variables (Tweak the Knobs)

### Required
- `VIRTUAL_ENV` - Path to Python virtual environment (auto-detected, usually)

### Optional (Power User Settings)

| Variable | Purpose | Default |
|----------|---------|---------|
| `LEINDEX_LOG_LEVEL` | Logging verbosity (DEBUG, INFO, WARNING, ERROR) | INFO |
| `LEINDEX_MAX_MEMORY` | Maximum memory usage in MB | Auto-detected |
| `LEINDEX_PORT` | HTTP server port | 8765 |

**Example:**
```bash
export LEINDEX_LOG_LEVEL=DEBUG
export LEINDEX_PORT=8080
uvx leindex
```

---

## 🎯 Available Commands (Your Toolkit)

### Core Indexing
```bash
index_repository    # Index a repository for symbols and references
search_symbols      # Search symbols within the indexed data
clear_index         # Clear existing index data
```

### Integration
```bash
integrate_with_lm_studio    # Set up LM Studio integration
integrate_with_vscode       # Configure VS Code integration
configure_remote_access     # Set up remote HTTP access
```

### Monitoring
```bash
get_indexing_status     # Get current indexing progress
get_search_statistics   # Retrieve search performance metrics
```

---

## 🚨 Troubleshooting (When Things Go Wrong)

### Common Issues & Quick Fixes

#### 1. Connection Refused

**Problem:** Can't connect to HTTP server

**Solution:**
```bash
# Verify HTTP server is running
python -m leindex.server --port 8765

# Check if port is available
netstat -an | grep 8765
```

---

#### 2. Index Not Found

**Problem:** Search returns no results

**Solution:**
```bash
# Ensure repository is indexed
uvx leindex init /path/to/project
uvx leindex index /path/to/project

# Verify index was created
ls -la ~/.leindex/indices/
```

---

#### 3. Slow Performance

**Problem:** Searches are taking too long

**Solution:**
```bash
# Check system resources
export LEINDEX_LOG_LEVEL=DEBUG
uvx leindex

# Consider reducing indexing scope
# Edit config.yaml to exclude large directories
```

---

### Debug Mode (X-Ray Vision)

Enable debug logging to see what's happening under the hood:

```bash
export LEINDEX_LOG_LEVEL=DEBUG
uvx leindex
```

**You'll see:**
- File scanning progress
- Indexing operations
- Search query details
- Performance metrics

---

## 🎨 Advanced Configuration (For the Tinkerers)

### Custom Port

```json
{
  "leindex": {
    "env": {
      "LEINDEX_PORT": "8080"
    }
  }
}
```

### Memory Limit

```json
{
  "leindex": {
    "env": {
      "LEINDEX_MAX_MEMORY": "1024"
    }
  }
}
```

### Custom Index Location

```yaml
# In config.yaml
dal_settings:
  db_path: "/custom/path/leindex.db"
  duckdb_db_path: "/custom/path/leindex.db.duckdb"
```

---

## 📚 Next Steps (Keep Exploring!)

- [Main README](README.md) - Overview and quick start
- [Architecture](ARCHITECTURE.md) - Deep dive into system design
- [API Reference](API.md) - Complete API documentation
- [Installation Guide](INSTALLATION.md) - Detailed setup instructions

---

## 🤝 Contributing (Join the Fun!)

We'd love your help making LeIndex even better! Check out our [Contributing Guide](CONTRIBUTING.md).

---

## 📜 License

MIT License - see [LICENSE](LICENSE) for details. Use it, modify it, share it. Go wild! 🎉

---

<div align="center">

**Built with ❤️ for developers who demand the best**

*Questions? Issues? Ideas?* [Open an issue on GitHub](https://github.com/scooter-lacroix/leindex/issues)

**Ready to supercharge your AI assistant?** [Back to Quick Start](#-installation-get-up-and-running) 🚀

</div>
