# LeIndex Installation Guide: Get Up and Running in 2 Minutes! 🚀

<div align="center">

**Zero Drama Installation**

*No Docker. No databases. No headaches. Just pip install and go.*

</div>

---

## Table of Contents

- [Requirements](#requirements)
- [Installation Methods](#installation-methods)
- [Configuration](#configuration)
- [Verification](#verification)
- [Platform-Specific Notes](#platform-specific-notes)
- [Troubleshooting](#troubleshooting)
- [Upgrading](#upgrading)

---

## Requirements

### System Requirements 🖥️

- **Operating System:** Linux, macOS, or Windows
- **Python:** 3.10 or higher (3.10-3.12 supported)
- **RAM:** 4GB minimum (8GB+ recommended for large codebases)
- **Disk Space:** 1GB minimum (more for larger indices)

### Python Dependencies 📦

LeIndex automatically installs its Python dependencies via pip:

- `leann` - Vector similarity search (storage-efficient, pure Python)
- `tantivy` - Full-text search (Rust Lucene, pure Python wrapper)
- `duckdb` - Analytical database (in-memory analytics beast)
- `sentence-transformers` - Code embeddings (understands your code)
- `torch` - Deep learning framework (CPU-only, no GPU needed)
- `mcp` - Model Context Protocol SDK (AI assistant integration)

**Note:** All dependencies are pure Python or include pre-built binaries. No compilation required. No C++ compiler needed. No Java runtime needed. Just pure Python magic.

---

## Installation Methods

### Method 1: pip install (Recommended) ⭐

The easiest way to install LeIndex is via pip. It's literally one command:

```bash
pip install leindex
```

This will install:
- The `leindex` CLI tool (for MCP server and indexing)
- The `leindex-search` CLI tool (for searching)
- The LeIndex Python package
- All required Python dependencies

**That's it!** No Docker, no databases to set up, no configuration files to edit (unless you want to). Just works.

### Method 2: Install from Source 🛠️

If you want to contribute or run the latest development version:

```bash
# Clone the repository
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex

# Install in editable mode
pip install -e .

# Or install with development dependencies (for testing)
pip install -e ".[dev]"
```

**Why editable mode?** Changes to the source code are immediately reflected without reinstalling.

### Method 3: Using conda 🐍

If you use conda for environment management:

```bash
# Create a new environment
conda create -n leindex python=3.10
conda activate leindex

# Install LeIndex
pip install leindex
```

**Why conda?** Isolated environment, easy Python version management, works great with data science tools.

### Method 4: Using pipx (Isolated Installation) 📦

For an isolated installation that doesn't affect your system Python:

```bash
# Install pipx if you don't have it
pip install pipx

# Install LeIndex with pipx
pipx install leindex
```

**Why pipx?** Completely isolated installation, no dependency conflicts, easy to uninstall.

---

## Configuration

### Default Configuration 🎯

LeIndex looks for configuration in the following locations:

1. `config.yaml` in the current directory
2. `~/.leindex/config.yaml`
3. `/etc/leindex/config.yaml`

**Pro tip:** LeIndex works great out of the box with default settings. You don't need to configure anything unless you want to!

### Minimal Configuration

If you want to customize LeIndex, create a `config.yaml` file:

```yaml
# Data Access Layer (DAL) Configuration
dal_settings:
  # Backend type: "sqlite_duckdb" is the default and recommended
  backend_type: "sqlite_duckdb"

  # SQLite database path for transactional metadata
  db_path: "./data/leindex.db"

  # DuckDB database path for analytical queries
  duckdb_db_path: "./data/leindex.db.duckdb"

# Vector Store Configuration for Semantic Search
vector_store:
  # Backend type: "leann" is the default
  backend_type: "leann"

  # LEANN index path
  index_path: "./leann_index"

  # Embedding model (CodeRankEmbed for code search)
  embedding_model: "nomic-ai/CodeRankEmbed"

# Async Processing Configuration
async_processing:
  enabled: true
  worker_count: 4

# File filtering
file_filtering:
  max_file_size: 1073741824  # 1GB

# Directory filtering
directory_filtering:
  max_files_per_directory: 1000000  # 1M files
  skip_large_directories:
    - "**/node_modules/**"
    - "**/.git/**"
    - "**/venv/**"
```

### Environment Variables 🔧

You can also configure LeIndex using environment variables:

```bash
# LeIndex home directory
export LEINDEX_HOME=~/.leindex

# Config file path
export LEINDEX_CONFIG=~/.leindex/config.yaml

# Log level
export LEINDEX_LOG_LEVEL=INFO

# Data directory
export LEINDEX_DATA_DIR=~/.leindex/data
```

**Why environment variables?** Perfect for Docker containers, CI/CD pipelines, and automated deployments.

---

## Verification

### Verify Installation ✅

Let's make sure everything is working:

```bash
# Check LeIndex version
leindex --version

# Check leindex-search CLI
leindex-search --help

# Test MCP server
leindex mcp --help
```

**Expected output:**
```
LeIndex 2.0.2 - Ready to search! 🚀
```

### Test Indexing 🧪

Let's index a small test project to make sure everything works:

```bash
# Create a test project
mkdir /tmp/test-project
echo "def hello(): print('Hello, World!')" > /tmp/test-project/test.py

# Initialize and index
leindex init /tmp/test-project
leindex index /tmp/test-project

# Search via CLI
leindex-search "hello function" --path /tmp/test-project

# Search via MCP (if MCP server is running)
# Use your MCP client to search for "hello"
```

**Expected result:** You should see search results matching the hello function.

**Success?** You're ready to index your real projects!

---

## Platform-Specific Notes

### Linux 🐧

Most Linux distributions work out of the box. On Ubuntu/Debian:

```bash
# Install Python 3.10+
sudo apt-get update
sudo apt-get install python3.11 python3-pip

# Install LeIndex
pip3 install leindex
```

**Fedora/RHEL:**
```bash
sudo dnf install python3.11 python3-pip
pip3 install leindex
```

**Arch Linux:**
```bash
sudo pacman -S python python-pip
pip install leindex
```

### macOS 🍎

```bash
# Install Python 3.10+ using Homebrew
brew install python@3.11

# Install LeIndex
pip3 install leindex
```

**Note:** macOS comes with Python, but it's often an older version. We recommend installing Python via Homebrew.

### Windows 🪟

```bash
# Install Python 3.10+ from python.org
# Download: https://www.python.org/downloads/

# Or use Chocolatey
choco install python

# Install LeIndex
pip install leindex
```

**Windows Subsystem for Linux (WSSL):**
```bash
# Install Ubuntu on WSL
wsl --install

# Inside WSL
sudo apt-get update
sudo apt-get install python3.11 python3-pip
pip3 install leindex
```

---

## Troubleshooting

### Common Issues 🔧

#### 1. Python version mismatch

**Error:** `Python 3.10 or higher required`

**Solution:**
```bash
# Check Python version
python --version

# Install correct Python version
# Ubuntu/Debian
sudo apt-get install python3.11

# macOS (using Homebrew)
brew install python@3.11

# Create virtual environment
python3.11 -m venv venv
source venv/bin/activate  # On Windows: venv\Scripts\activate
pip install leindex
```

#### 2. LEANN model download fails

**Error:** `Failed to download embedding model`

**Solution:**
```bash
# Set HuggingFace cache directory
export HF_HOME=~/.cache/huggingface

# Or download model manually
# The model will be downloaded automatically on first use
# Make sure you have internet connection
```

**What's happening?** LeIndex downloads the CodeRankEmbed model on first use. It's about 500MB.

#### 3. Permission denied writing to data directory

**Error:** `Permission denied when creating index files`

**Solution:**
```bash
# Create data directory with proper permissions
mkdir -p ~/.leindex/data
chmod 755 ~/.leindex/data

# Or specify a different data directory in config.yaml
```

#### 4. Out of memory during indexing

**Error:** `MemoryError` or process killed during indexing

**Solution:**
```bash
# Reduce worker count in config.yaml
async_processing:
  worker_count: 2  # Reduce from 4

# Or reduce batch size
indexing:
  batch_size: 25  # Reduce from default
```

**What's happening?** Large codebases with large files can use lots of memory. Reduce workers and batch size.

### Debug Mode 🔍

Enable debug logging for troubleshooting:

```bash
export LEINDEX_LOG_LEVEL=DEBUG
leindex mcp
```

**What you'll see:** Detailed logs of what LeIndex is doing, helpful for debugging.

---

## Upgrading

### Upgrade LeIndex ⬆️

```bash
# Upgrade to latest version
pip install --upgrade leindex

# Or from source
git pull
pip install -e .
```

### Migrate Data

When upgrading major versions, backup your data first:

```bash
# Backup your data directory
cp -r ~/.leindex ~/.leindex.backup.$(date +%Y%m%d)

# After upgrade, test with a small project first
```

**Why backup?** Major version upgrades might change the index format. Better safe than sorry!

---

## Uninstallation

### Remove LeIndex 🗑️

```bash
# Uninstall package
pip uninstall leindex

# Remove configuration and data
rm -rf ~/.leindex
rm /etc/leindex/config.yaml  # if exists
```

**That's it!** LeIndex is completely removed. No leftover services, no leftover databases.

---

## Next Steps

### After Installation 🎉

Now that you have LeIndex installed:

1. **Index your first project**
   ```bash
   leindex init ~/my-project
   leindex index ~/my-project
   ```

2. **Search your code**
   ```bash
   leindex-search "authentication logic"
   ```

3. **Set up MCP integration with your AI assistant**
   - Configure your MCP client (Claude, Cursor, Windsurf, etc.)
   - Add LeIndex to your MCP server list

4. **Explore the documentation**
   - [API Reference](API.md) - Complete API documentation
   - [Architecture Deep Dive](ARCHITECTURE.md) - System design and internals
   - [Troubleshooting Guide](TROUBLESHOOTING.md) - Common issues and solutions

### Quick Links 🔗

- [Configuration Guide](ARCHITECTURE.md#configuration) - Customize LeIndex to your needs
- [MCP Setup](MCP_CONFIGURATION.md) - MCP server setup and examples
- [API Reference](API.md) - Complete API documentation
- [Troubleshooting](TROUBLESHOOTING.md) - Common issues and solutions
- [Quick Start Tutorial](QUICKSTART.md) - Get started in 5 minutes

---

## Need Help? 🆘

If you encounter issues not covered here:

- **Check GitHub Issues:** https://github.com/scooter-lacroix/leindex/issues
- **Review Troubleshooting Guide:** [TROUBLESHOOTING.md](TROUBLESHOOTING.md)
- **Enable debug logging:** `export LEINDEX_LOG_LEVEL=DEBUG`
- **Check your config.yaml:** Validate YAML syntax

---

**Ready to search your code like a wizard?** [Start searching now](API.md) 🚀

**Installation complete?** Time to [index your first project](QUICKSTART.md) and experience the magic!
