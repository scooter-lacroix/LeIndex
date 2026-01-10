# Performance Regression Fix - Quick Summary

## Problem

After expanding `DEFAULT_EXCLUDES` with glob patterns (`*.pyc`, `*.log`, etc.), directory scanning slowed from **0.046s to 60+ seconds**.

## Root Cause

The `should_ignore_directory` method was calling PatternTrie for **EVERY directory**, which performed expensive regex matching against 20+ glob patterns for **EVERY** directory check.

For 1396 directories:
- 1396 × 20 glob patterns = **27,000+ regex searches**
- Each regex compiled on-the-fly (no caching)
- Result: **1300x performance regression**

## The Fix

### 1. Fast-Path Directory Checks (O(1) lookups)

**Before:**
```python
def should_ignore_directory(self, dir_path: str) -> bool:
    if self.should_ignore(dir_path):  # EXPENSIVE PatternTrie call
        return True
    # ... more checks
```

**After:**
```python
def should_ignore_directory(self, dir_path: str) -> bool:
    dir_name = os.path.basename(dir_path)

    # FAST PATH 1: O(1) set lookup
    dir_only_excludes = {
        '.git', 'node_modules', '__pycache__', 'venv',
        'build', 'dist', 'target', 'vendor', ...
    }
    if dir_name in dir_only_excludes:
        return True  # NO PatternTrie call!

    # FAST PATH 2: Hidden directories
    if dir_name.startswith('.'):
        if dir_name not in {'.github', '.vscode', '.config'}:
            return True  # NO PatternTrie call!

    # SLOW PATH: Only for uncommon directories
    return self.should_ignore(dir_path)
```

### 2. Pre-Compiled Wildcard Patterns

**Before:**
```python
def __init__(self, patterns: List[str], cache_size: int = 10000):
    self.patterns = patterns
    # No pre-compilation
```

**After:**
```python
def __init__(self, patterns: List[str], cache_size: int = 10000):
    self.patterns = patterns
    # Pre-compile wildcard regex patterns
    self._compiled_wildcards: Dict[str, re.Pattern] = {}
    for pattern in patterns:
        if '*' in pattern:
            regex = self._wildcard_to_regex(pattern)
            self._compiled_wildcards[pattern] = re.compile(regex, re.IGNORECASE)
```

## Files Modified

**`src/leindex/ignore_patterns.py`**

1. **Lines 47-63:** Added pre-compiled wildcard patterns in `__init__`
2. **Lines 216-258:** Updated `_pattern_matches` to use pre-compiled regex
3. **Lines 515-585:** Rewrote `should_ignore_directory` with fast paths

## Expected Performance

- **Before fix:** 60+ seconds for 1396 directories
- **After fix:** <1 second for 1396 directories
- **Improvement:** 99% faster (1300x speedup)

## Verification

Run the test script:
```bash
python test_performance_fix.py /home/stan/Documents/Twt
```

Expected output:
```
✓ EXCELLENT: Scan completed in <1 second
Directories scanned: 1396
Scan rate: ~1500+ dirs/sec
```

## Key Insights

1. **Directory vs file patterns matter:** Glob patterns like `*.pyc` are file patterns, not directory patterns
2. **Check order is critical:** O(1) checks should happen before expensive pattern matching
3. **Regex compilation is expensive:** Pre-compile once, not on every match
4. **Specialized methods:** `should_ignore_directory` should be optimized for directories

## Testing Checklist

- [ ] Verify scan completes in <1 second
- [ ] Confirm `node_modules` is still ignored
- [ ] Confirm `.git` is still ignored
- [ ] Confirm `__pycache__` is still ignored
- [ ] Check that custom `.gitignore` patterns still work
- [ ] Verify no directories are incorrectly ignored
