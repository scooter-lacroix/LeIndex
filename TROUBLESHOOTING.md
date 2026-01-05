# LeIndex Troubleshooting Guide: Solve Issues Like a Pro 🔧

<div align="center">

**Don't Panic! We've Got You Covered**

*Common issues and solutions when using LeIndex*

</div>

---

## Table of Contents

- [Installation Issues](#installation-issues)
- [Indexing Issues](#indexing-issues)
- [Search Issues](#search-issues)
- [Performance Issues](#performance-issues)
- [MCP Server Issues](#mcp-server-issues)
- [Configuration Issues](#configuration-issues)
- [Getting Help](#getting-help)
- [Diagnostic Commands](#diagnostic-commands)

---

## Installation Issues

### "Command not found: leindex"

**Symptoms:**
```bash
$ leindex --version
bash: leindex: command not found
```

**Causes:**
1. `~/.local/bin` not in PATH
2. LeIndex not installed correctly
3. Using wrong Python environment

**Solutions:**

1. Check if LeIndex is installed:
   ```bash
   pip show leindex
   ```

2. If installed, add to PATH:
   ```bash
   # Add to ~/.bashrc or ~/.zshrc
   export PATH="$HOME/.local/bin:$PATH"
   # Reload shell
   source ~/.bashrc
   ```

3. Reinstall if needed:
   ```bash
   pip uninstall leindex
   pip install leindex
   ```

---

### "Module not found: leann"

**Symptoms:**
```python
ImportError: No module named 'leann'
```

**Solutions:**

1. Reinstall LeIndex:
   ```bash
   pip install --force-reinstall leindex
   ```

2. Install LEANN manually:
   ```bash
   pip install leann --upgrade
   ```

3. Check Python version (requires 3.10+):
   ```bash
   python --version
   ```

---

### "Permission denied" during installation

**Solutions:**

1. Use virtual environment (recommended):
   ```bash
   python -m venv ~/.venv/leindex
   source ~/.venv/leindex/bin/activate
   pip install leindex
   ```

2. Or use `--user` flag:
   ```bash
   pip install --user leindex
   ```

---

## Indexing Issues

### "Indexing stuck at 0%"

**Symptoms:**
- Progress bar shows 0% for a long time
- No files being indexed

**Solutions:**

1. Check memory usage and reduce workers in config.yaml:
   ```yaml
   performance:
     workers: 2  # Reduce from 4
   ```

2. Check for large files:
   ```bash
   find /path/to/project -type f -size +100M
   ```

3. Add file size limit:
   ```yaml
   indexing:
     max_file_size: 52428800  # 50MB
   ```

4. Enable verbose logging:
   ```bash
   leindex index /path/to/project --verbose
   ```

---

### "File not found" errors during indexing

**Symptoms:**
- Errors about files not existing
- Permission denied errors

**Solutions:**

1. Check file permissions:
   ```bash
   ls -la /path/to/file.py
   ```

2. Skip problematic files:
   ```yaml
   indexing:
     exclude_patterns:
       - "**/problematic/**"
   ```

3. Use `--force` to reindex:
   ```bash
   leindex index /path/to/project --force
   ```

---

### "Out of memory" during indexing

**Symptoms:**
- Process killed during indexing
- MemoryError exceptions

**Solutions:**

1. Reduce memory limit:
   ```yaml
   performance:
     memory_limit_mb: 2048
   ```

2. Reduce batch size and workers:
   ```yaml
   indexing:
     batch_size: 50
   performance:
     workers: 2
   ```

3. Close other applications or index in smaller chunks

---

### Embedding generation fails

**Symptoms:**
- Errors during embedding generation
- Model download failures

**Solutions:**

1. Check model download:
   ```bash
   ls ~/.cache/huggingface/hub/
   ```

2. Re-download model:
   ```bash
   rm -rf ~/.cache/huggingface/hub/models--nomic-ai--CodeRankEmbed
   leindex index /path/to/project
   ```

3. Use different model:
   ```yaml
   embeddings:
     model: sentence-transformers/all-MiniLM-L6-v2
   ```

---

## Search Issues

### "No results found" for valid queries

**Symptoms:**
- Searching for code you know exists returns no results
- Empty result sets

**Solutions:**

1. Check if files are indexed:
   ```bash
   leindex stats /path/to/project
   ```

2. Try different backend:
   ```bash
   leindex-search "query" --backend tantivy
   leindex-search "query" --backend regex
   ```

3. Lower semantic threshold:
   ```yaml
   search:
     semantic_threshold: 0.5
   ```

4. Use broader query

---

### Search is very slow

**Symptoms:**
- Searches take seconds to complete
- Long wait times for results

**Solutions:**

1. Enable caching:
   ```yaml
   performance:
     enable_caching: true
   ```

2. Use appropriate backend:
   ```bash
   leindex-search "exact_name" --backend tantivy  # Faster
   leindex-search "concept" --backend semantic  # Better quality
   ```

3. Limit results:
   ```bash
   leindex-search "query" --limit 20
   ```

---

### Poor search quality

**Symptoms:**
- Results don't match your query
- Irrelevant code appears in results

**Solutions:**

1. Try different query formulations:
   ```bash
   # Good: "authentication logic"
   leindex-search "authentication logic"

   # Too specific: "def auth"
   leindex-search "def auth" --backend tantivy
   ```

2. Adjust semantic threshold:
   ```yaml
   search:
     semantic_threshold: 0.8  # Higher quality
   ```

3. Use hybrid search:
   ```bash
   leindex-search "function authenticate" --backend tantivy
   ```

---

## Performance Issues

### High memory usage

**Symptoms:**
- LeIndex using lots of RAM
- System slows down

**Solutions:**

1. Reduce memory limit:
   ```yaml
   performance:
     memory_limit_mb: 2048
   ```

2. Reduce workers:
   ```yaml
   performance:
     workers: 2
   ```

3. Reduce batch size:
   ```yaml
   indexing:
     batch_size: 50
   ```

---

### Slow indexing speed

**Symptoms:**
- Indexing takes a long time
- Progress bar moves slowly

**Solutions:**

1. Increase workers:
   ```yaml
   performance:
     workers: 8
   ```

2. Increase batch size:
   ```yaml
   indexing:
     batch_size: 200
   ```

3. Exclude unnecessary directories:
   ```yaml
   indexing:
     exclude_patterns:
       - "**/node_modules/**"
       - "**/.git/**"
       - "**/venv/**"
   ```

---

### High disk usage

**Symptoms:**
- LeIndex data directory using lots of space
- Disk filling up

**Solutions:**

1. Remove old projects:
   ```bash
   leindex remove /path/to/old-project --purge
   ```

2. Clean data directory:
   ```bash
   rm -rf ~/.leindex/data/*.tmp
   ```

3. Reduce index size by excluding more files

---

## MCP Server Issues

### MCP server not starting

**Symptoms:**
- Server fails to start
- Connection errors

**Solutions:**

1. Check installation:
   ```bash
   leindex --version
   ```

2. Check configuration:
   ```bash
   cat ~/.leindex/config.yaml
   ```

3. Enable debug logging:
   ```bash
   export LEINDEX_LOG_LEVEL=DEBUG
   leindex mcp
   ```

---

### MCP tools not discovered

**Symptoms:**
- MCP client can't see LeIndex tools
- Tools not showing up

**Solutions:**

1. Verify MCP server is running:
   ```bash
   leindex mcp
   ```

2. Check MCP client config:
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

3. Restart MCP client

---

## Configuration Issues

### Invalid configuration

**Symptoms:**
- Configuration errors on startup
- YAML parsing errors

**Solutions:**

1. Validate config file:
   ```bash
   python -c "import yaml; yaml.safe_load(open('~/.leindex/config.yaml'))"
   ```

2. Reset to default:
   ```bash
   rm ~/.leindex/config.yaml
   leindex init /path/to/project
   ```

3. Check YAML syntax:
   - Use a YAML validator
   - Check for proper indentation
   - Ensure no trailing spaces

---

## Getting Help

### Debug Mode 🔍

Enable debug logging for detailed information:

```bash
export LEINDEX_LOG_LEVEL=DEBUG
leindex --verbose
```

**What you'll see:**
- Detailed logs of all operations
- Stack traces for errors
- Performance metrics

---

### Check Logs 📋

```bash
# Check if logs exist
cat ~/.leindex/logs/leindex.log

# Or follow logs in real-time
tail -f ~/.leindex/logs/leindex.log
```

---

### Search GitHub Issues 🔍

- https://github.com/scooter-lacroix/leindex/issues

**Before creating a new issue:**
1. Search for similar issues
2. Check if your problem is already solved
3. Gather relevant information

---

### Create a New Issue 📝

If none of these solutions work, create a new issue with:

1. **LeIndex version:**
   ```bash
   leindex --version
   ```

2. **Python version:**
   ```bash
   python --version
   ```

3. **Operating system:**
   ```bash
   uname -a  # Linux/macOS
   ver       # Windows
   ```

4. **Full error message:**
   - Copy the complete error traceback
   - Include all error details

5. **Steps to reproduce:**
   - What you were trying to do
   - What commands you ran
   - What you expected to happen
   - What actually happened

6. **Configuration:**
   - Your config.yaml (sanitized)
   - Any custom settings

---

## Diagnostic Commands

### System Information

```bash
# LeIndex version
leindex --version

# Python version
python --version

# OS information
uname -a

# Available memory
free -h  # Linux
sysctl hw.memsize  # macOS

# Disk space
df -h
```

---

### LeIndex Diagnostics

```bash
# Check installation
pip show leindex

# Check configuration
cat ~/.leindex/config.yaml

# Check data directory
ls -la ~/.leindex/

# Test basic functionality
mkdir /tmp/test-project
echo "def test(): pass" > /tmp/test-project/test.py
leindex init /tmp/test-project
leindex index /tmp/test-project
leindex-search "test"

# Get statistics
leindex stats
```

---

### Database Health

```bash
# Check SQLite database
sqlite3 ~/.leindex/data/metadata.db "SELECT COUNT(*) FROM files;"

# Check Tantivy index
ls -la ~/.leindex/data/ft_index/

# Check LEANN index
ls -la ~/.leindex/data/vector_index/

# Check DuckDB database
ls -la ~/.leindex/data/analytics.db
```

---

## Common Error Messages

| Error | Cause | Solution |
|-------|-------|----------|
| `ImportError: No module named 'leann'` | LEANN not installed | `pip install leann --upgrade` |
| `Permission denied` | File permissions | Run with appropriate permissions |
| `MemoryError` | Out of memory | Reduce memory_limit_mb in config |
| `FileNotFoundError` | File not found | Check file path and permissions |
| `ConfigurationError` | Invalid config | Validate YAML syntax |
| `IndexNotFoundError` | Index doesn't exist | Run `leindex init` first |
| `SearchTimeout` | Search taking too long | Reduce result limit |
| `ConnectionError` | MCP connection failed | Check MCP server is running |

---

## Performance Tuning

### For Large Codebases (100K+ files)

```yaml
# Increase workers
performance:
  workers: 8
  memory_limit_mb: 8192

# Larger batches
indexing:
  batch_size: 200

# More aggressive caching
performance:
  enable_caching: true
  cache_size: 10000
```

### For Low-Resource Systems (4GB RAM)

```yaml
# Reduce workers
performance:
  workers: 2
  memory_limit_mb: 2048

# Smaller batches
indexing:
  batch_size: 50

# Disable some features
search:
  semantic_threshold: 0.8  # Higher threshold = fewer results = faster
```

### For SSD Storage

```yaml
# Enable more aggressive parallelism
performance:
  workers: 16

# Larger batches
indexing:
  batch_size: 500
```

---

**Still having issues?** Please open a GitHub issue with all relevant details. We're here to help! 🚀

**Want to avoid issues in the first place?** Check out the [Installation Guide](INSTALLATION.md) for proper setup.
