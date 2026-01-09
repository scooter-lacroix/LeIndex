# LeIndex MCP Quick Start

## ✅ Good News: Your MCP Server is Already Working!

The LeIndex MCP server is installed and configured via your anaconda3 environment.

---

## 🚀 Start Using LeIndex MCP Tools Right Now

In Claude Code, try these commands:

### Get System Status
```
mcp__leindex__get_global_stats
mcp__leindex__get_memory_status
mcp__leindex__list_projects
```

### Manage Your Project
```
mcp__leindex__manage_project action="set_path" path="/path/to/your/project"
mcp__leindex__manage_project action="refresh"
mcp__leindex__manage_project action="reindex"
```

### Search Code
```
mcp__leindex__search_content action="search" pattern="your_search_term"
mcp__leindex__search_content action="find" pattern="*.py"
```

### Read Files
```
mcp__leindex__read_file mode="smart" file_path="/path/to/file.py"
```

---

## 🔧 If You Want to Use Python 3.14.0 Instead

```bash
cd /mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer
source .venv/bin/activate
pip install -e .
```

The Python version constraint has been fixed to support 3.14.0.

---

## ✨ Verification

Run this to verify everything is working:

```bash
./verify_mcp_connection.sh
```

---

## 📚 Documentation

- **Quick Start:** This file
- **Full Analysis:** `MCP_FIX_REPORT.md`
- **Summary:** `MCP_FIX_SUMMARY.md`

---

## 🎯 What Was Fixed

**Problem:** Python 3.14.0 was blocked by version constraint
**Solution:** Updated `pyproject.toml` to allow Python 3.14
**Result:** You can now install and use LeIndex with Python 3.14.0

**Status:** ✅ Complete and committed to git

---

**Need Help?** Check `MCP_FIX_REPORT.md` for detailed troubleshooting steps.
