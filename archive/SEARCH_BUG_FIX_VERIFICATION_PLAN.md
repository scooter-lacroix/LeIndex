# Search Bug Fix Verification Plan

**Date:** 2026-01-20  
**Status:** Verification Required  
**Reference:** LEINDEX_SEARCH_BUG_REPORT.md

---

## Summary

Both bugs from the bug report have **already been fixed** in the source code. This plan provides verification steps and test procedures.

---

## Bug Status

| Bug | Description | Status | Location |
|-----|-------------|--------|----------|
| #1 | Parameter mismatch in `search_content` | ✅ FIXED | server.py L3140-3143 |
| #2 | `.message` attribute on `InvalidPatternError` | ✅ FIXED | server.py L2738 |

---

## Verification Evidence

### Bug #1: Parameter Mismatch - FIXED

**Current `search_code_advanced` signature (server.py L3133-3145):**
```python
async def search_code_advanced(
    pattern: str,
    ctx: Context,
    case_sensitive: bool = True,
    context_lines: int = 0,
    file_pattern: Optional[str] = None,
    fuzzy: bool = False,
    content_boost: float = 1.0,           # ✅ Now supported
    filepath_boost: float = 1.0,          # ✅ Now supported
    highlight_pre_tag: str = "<em>",      # ✅ Now supported
    highlight_post_tag: str = "</em>",    # ✅ Now supported
    page: int = 1,
    page_size: int = 5,
) -> Dict[str, Any]:
```

The parameters are validated (L3182-3189) and passed to CoreEngine's SearchOptions (L3247-3255).

### Bug #2: InvalidPatternError Handler - FIXED

**Current exception handler (server.py L2734-2740):**
```python
except InvalidPatternError as e:
    logger.error(f"Invalid search pattern: {e}")
    return {
        "success": False,
        "error": f"Invalid search pattern: {str(e)}",  # ✅ Uses str(e)
        "error_type": "InvalidPatternError"
    }
```

---

## Verification Steps

### Step 1: Run Unit Tests
```bash
cd /mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer
python -m pytest tests/ -v -k "search" --tb=short
```

### Step 2: Manual Integration Test - search_content
```python
# Test file: test_search_verification.py

import asyncio
from unittest.mock import MagicMock, AsyncMock

async def test_search_content_parameters():
    """Verify search_content passes all parameters correctly."""
    from leindex.server import search_content, search_code_advanced
    
    # Mock context
    ctx = MagicMock()
    ctx.request_context.lifespan_context.base_path = "/test/path"
    ctx.request_context.lifespan_context.settings = MagicMock()
    ctx.request_context.lifespan_context.dal = MagicMock()
    ctx.request_context.lifespan_context.core_engine = None  # Force fallback
    
    # This should NOT raise "unexpected keyword argument"
    try:
        result = await search_content(
            ctx=ctx,
            action="search",
            pattern="def main",
            fuzzy=True,
            content_boost=2.0,
            filepath_boost=1.5,
            highlight_pre_tag="<mark>",
            highlight_post_tag="</mark>",
        )
        print("✅ search_content accepts all parameters")
    except TypeError as e:
        if "unexpected keyword argument" in str(e):
            print(f"❌ Bug #1 NOT FIXED: {e}")
            raise
        raise

if __name__ == "__main__":
    asyncio.run(test_search_content_parameters())
```

### Step 3: Manual Integration Test - InvalidPatternError
```python
# Test file: test_invalid_pattern_verification.py

import asyncio
from leindex.global_index.cross_project_search import InvalidPatternError

def test_invalid_pattern_error_str():
    """Verify InvalidPatternError works with str()."""
    try:
        raise InvalidPatternError("Test pattern [invalid(")
    except InvalidPatternError as e:
        # This should NOT raise "no attribute 'message'"
        error_msg = f"Invalid search pattern: {str(e)}"
        print(f"✅ str(e) works: {error_msg}")
        
        # Verify .message would fail (proving the fix was needed)
        try:
            _ = e.message
            print("⚠️ WARNING: .message exists now (class was modified)")
        except AttributeError:
            print("✅ Confirmed: .message does not exist (str(e) is correct fix)")

if __name__ == "__main__":
    test_invalid_pattern_error_str()
```

### Step 4: MCP Server Integration Test
```bash
# Start MCP server and test via client
cd /mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer
python -m leindex.server &
MCP_PID=$!

# Wait for server startup
sleep 3

# Test search_content tool (would fail before fix)
echo '{"method": "search_content", "params": {"action": "search", "pattern": "def", "fuzzy": true}}' | \
  python -c "import sys, json; print(json.loads(sys.stdin.read()))"

# Test cross_project_search with invalid pattern (would fail before fix)
echo '{"method": "cross_project_search_tool", "params": {"pattern": "[invalid("}}' | \
  python -c "import sys, json; print(json.loads(sys.stdin.read()))"

kill $MCP_PID 2>/dev/null
```

---

## Remaining Improvements (P1/P2)

These are optional enhancements from the bug report that could still be implemented:

### P1: Ensure boost params reach Tantivy backend
- **Status:** Partially implemented
- **Current:** Params passed to `SearchOptions` (L3251-3254)
- **Verify:** Check `CoreEngine.search()` forwards these to Tantivy

### P2: Token-efficient response mode
- **Status:** Not implemented
- **Description:** Add compact response format for LLM context efficiency
- **Effort:** 4 hours

---

## Test Commands Summary

```bash
# Quick verification
cd /mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer

# 1. Type check
python -m mypy src/leindex/server.py --ignore-missing-imports 2>&1 | head -20

# 2. Run existing tests
python -m pytest tests/test_mcp_server.py -v -k "search" --tb=short 2>&1 | tail -30

# 3. Verify no .message on exceptions
grep -rn "e\.message" src/leindex/ && echo "❌ Found e.message usage" || echo "✅ No e.message usage"

# 4. Verify search_code_advanced signature has all params
grep -A 15 "async def search_code_advanced" src/leindex/server.py | head -16
```

---

## Conclusion

Both critical bugs have been fixed. Run the verification steps above to confirm the fixes work in your environment.
