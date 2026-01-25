#!/bin/bash
# LeIndex MCP Connection Verification Script
# This script verifies that the MCP server is properly configured and accessible

set -e

echo "=================================="
echo "LeIndex MCP Connection Verification"
echo "=================================="
echo ""

# Color codes
GREEN='\033[0;32m'
RED='\033[0;31m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

# Check 1: Python version
echo -n "Checking Python version in .venv... "
if source .venv/bin/activate && python --version >/dev/null 2>&1; then
    PY_VERSION=$(source .venv/bin/activate && python --version 2>&1 | awk '{print $2}')
    echo -e "${GREEN}✓${NC} Python $PY_VERSION"
else
    echo -e "${RED}✗${NC} Failed to check Python version"
fi
echo ""

# Check 2: pyproject.toml constraint
echo -n "Checking pyproject.toml Python constraint... "
CONSTRAINT=$(grep "requires-python" pyproject.toml | head -1)
if echo "$CONSTRAINT" | grep -q "<3.15"; then
    echo -e "${GREEN}✓${NC} Supports Python 3.14"
    echo "  $CONSTRAINT"
else
    echo -e "${YELLOW}⚠${NC} Constraint may not support Python 3.14"
    echo "  $CONSTRAINT"
fi
echo ""

# Check 3: LeIndex in anaconda3
echo -n "Checking anaconda3 leindex installation... "
if [ -f "/home/stan/anaconda3/bin/leindex" ]; then
    echo -e "${GREEN}✓${NC} Found at /home/stan/anaconda3/bin/leindex"
    ANACONDA_PY=$(/home/stan/anaconda3/bin/python --version 2>&1 | awk '{print $2}')
    echo "  Using Python $ANACONDA_PY"
else
    echo -e "${RED}✗${NC} Not found in anaconda3"
fi
echo ""

# Check 4: MCP configuration
echo -n "Checking MCP configuration... "
if [ -f "$HOME/.config/claude-code/mcp.json" ]; then
    echo -e "${GREEN}✓${NC} Found at ~/.config/claude-code/mcp.json"
    if grep -q '"leindex"' "$HOME/.config/claude-code/mcp.json"; then
        echo -e "  ${GREEN}✓${NC} LeIndex is configured"
        grep -A 3 '"leindex"' "$HOME/.config/claude-code/mcp.json" | head -4
    else
        echo -e "  ${RED}✗${NC} LeIndex not found in MCP config"
    fi
else
    echo -e "${RED}✗${NC} MCP config not found"
fi
echo ""

# Check 5: Test leindex command
echo -n "Testing leindex command... "
if /home/stan/anaconda3/bin/leindex mcp --help >/dev/null 2>&1; then
    echo -e "${GREEN}✓${NC} LeIndex MCP server starts successfully"
else
    echo -e "${RED}✗${NC} LeIndex MCP server failed to start"
fi
echo ""

# Check 6: Installation in .venv (optional)
echo -n "Checking if leindex is installed in .venv... "
if source .venv/bin/activate && python -c "import leindex" 2>/dev/null; then
    echo -e "${GREEN}✓${NC} LeIndex is installed in .venv"
else
    echo -e "${YELLOW}⚠${NC} LeIndex not installed in .venv (use 'pip install -e .' to install)"
fi
echo ""

echo "=================================="
echo "Verification Complete"
echo "=================================="
echo ""
echo "Next Steps:"
echo "1. If LeIndex is not installed in .venv, run:"
echo "   source .venv/bin/activate"
echo "   pip install -e ."
echo ""
echo "2. Test MCP tools in Claude Code:"
echo "   mcp__leindex__get_global_stats"
echo "   mcp__leindex__list_projects"
echo ""
echo "3. If MCP tools don't work, restart Claude Code to reload the MCP configuration"
echo ""
echo "For detailed analysis, see: MCP_FIX_REPORT.md"
