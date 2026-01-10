# Performance Regression Fix: Ignore Pattern Matching

## Executive Summary

**Problem:** After expanding `DEFAULT_EXCLUDES` with glob patterns (`*.pyc`, `*.log`, etc.), the `set_path` operation regressed from 0.046s to 60+ seconds.

**Root Cause:** The `should_ignore_directory` method was calling PatternTrie for EVERY directory, which performed expensive regex matching against 20+ glob patterns for EVERY directory check.

**Solution:** Implemented fast-path O(1) set lookups for common directories before falling back to PatternTrie, plus pre-compiled regex patterns for wildcards.

**Result:** Expected performance restored to <1 second for directory scanning.

---

## Root Cause Analysis

### The Performance Regression

When `DEFAULT_EXCLUDES` was expanded from ~40 simple directory names to ~60+ patterns including glob patterns like:

- `*.pyc`, `*.pyo`, `*.pyd` (Python bytecode)
- `*.class`, `*.jar`, `*.war` (Java compiled files)
- `*.log`, `*.tmp` (temporary files)
- `*.db`, `*.sqlite`, `*.sqlite3` (databases)
- `*.egg-info`, `*.whl` (Python packages)

### Why This Caused Slowdown

**Before expansion:** `DEFAULT_EXCLUDES` contained mostly simple directory names (`.git`, `node_modules`, etc.). PatternTrie could handle these efficiently with its trie structure.

**After expansion:** Every directory check triggered the following expensive operations:

1. **Line 528 in original code:** `if self.should_ignore(dir_path):`
   - This called PatternTrie's `should_ignore` method
   - PatternTrie checked the directory against ALL 60+ patterns

2. **PatternTrie's `_pattern_matches` method** (lines 207-244):
   - For patterns with wildcards: Converted to regex on-the-fly
   - Called `re.search()` for EVERY wildcard pattern
   - No caching of compiled regex patterns

3. **The compounding problem:**
   - 1396 directories × 20+ glob patterns = **27,000+ regex searches**
   - Each regex compilation = `re.escape()` + string replacement
   - Each regex search = full string match attempt

### Performance Breakdown

**Before expansion (simple directory names only):**
- Trie matching: O(m) where m = directory name length
- No regex compilation needed
- Result: 0.046 seconds

**After expansion (with glob patterns):**
- For each directory: Check against 20+ glob patterns
- Each glob pattern: Compile regex + search
- Result: 60+ seconds (1300x slower!)

---

## The Fix

### 1. Fast-Path Directory Name Checks (O(1) lookup)

**File:** `src/leindex/ignore_patterns.py:515-585`

**Change:** Moved directory name checks BEFORE PatternTrie matching:

```python
def should_ignore_directory(self, dir_path: str) -> bool:
    dir_name = os.path.basename(dir_path)

    # FAST PATH 1: O(1) set lookup for common directories
    dir_only_excludes = {
        '.git', 'node_modules', '__pycache__', 'venv', 'build',
        'dist', 'target', 'vendor', '.vscode', '.idea', ...
        # ~60 common directory names
    }

    if dir_name in dir_only_excludes:
        return True  # O(1) lookup - NO PatternTrie call!

    # FAST PATH 2: Hidden directory check
    if dir_name.startswith('.') and dir_name not in {'.', '..'}:
        if dir_name not in {'.github', '.vscode', '.config'}:
            return True  # O(1) lookup - NO PatternTrie call!

    # SLOW PATH: Only reached for uncommon directories
    return self.should_ignore(dir_path)  # PatternTrie call
```

**Performance gain:** Most directories match in FAST PATH 1 or 2, avoiding expensive PatternTrie calls entirely.

### 2. Pre-Compiled Wildcard Patterns

**File:** `src/leindex/ignore_patterns.py:47-63`

**Change:** Pre-compile all wildcard regex patterns during initialization:

```python
def __init__(self, patterns: List[str], cache_size: int = 10000):
    self.patterns = patterns
    self.trie: Dict = self._build_trie(patterns)
    self._cache: OrderedDict = OrderedDict()
    self._cache_size = cache_size
    self._hits = 0
    self._misses = 0

    # PRE-COMPILE wildcard regex patterns
    self._compiled_wildcards: Dict[str, re.Pattern] = {}
    for pattern in patterns:
        if '*' in pattern:
            regex_pattern = self._wildcard_to_regex(pattern)
            try:
                self._compiled_wildcards[pattern] = re.compile(regex_pattern, re.IGNORECASE)
            except re.error:
                pass
```

**File:** `src/leindex/ignore_patterns.py:216-258`

**Change:** Use pre-compiled regex in `_pattern_matches`:

```python
def _pattern_matches(self, pattern: str, path: str) -> bool:
    if '*' in pattern:
        # Use pre-compiled regex (O(1) lookup + fast search)
        if pattern in self._compiled_wildcards:
            if self._compiled_wildcards[pattern].search(path):
                return True
        else:
            # Fallback to on-the-fly compilation
            regex_pattern = self._wildcard_to_regex(pattern)
            try:
                if re.search(regex_pattern, path, re.IGNORECASE):
                    return True
            except re.error:
                pass
    # ... rest of method
```

**Performance gain:** Regex compilation happens ONCE at initialization instead of on EVERY pattern match.

---

## Expected Performance Improvements

### Before Fix

- **Directory checking:** 60+ seconds for 1396 directories
- **Pattern matching:** 27,000+ regex compilations + searches
- **Bottleneck:** Every directory checked against 20+ glob patterns

### After Fix

- **Directory checking:** <1 second for 1396 directories (99% faster)
- **Pattern matching:** ~20 regex compilations at init + ~100 searches (only for uncommon directories)
- **Fast path:** Most directories match O(1) set lookups

### Performance Breakdown by Path

**FAST PATH 1 (set lookup):**
- Matches: `node_modules`, `.git`, `__pycache__`, `venv`, etc.
- Performance: O(1) ~ 0.0001ms per lookup
- Expected hit rate: ~80-90% of directories

**FAST PATH 2 (hidden directory check):**
- Matches: `.vscode`, `.idea`, `.next`, etc.
- Performance: O(1) ~ 0.0001ms per lookup
- Expected hit rate: ~5-10% of directories

**SLOW PATH (PatternTrie):**
- Matches: Custom gitignore patterns, unusual directories
- Performance: O(n×m) where n = patterns, m = path length
- Expected hit rate: ~1-5% of directories

---

## Verification Steps

### Test the Fix

```bash
# Test with the Twt project that was timing out
cd /mnt/e0f7c1a8-b834-4827-b579-0251b006bc1f/code_index_update/LeIndexer

# Run a test set_path operation
python -c "
import asyncio
from leindex.ignore_patterns import IgnorePatternMatcher
from leindex.parallel_scanner import ParallelScanner

async def test():
    base_path = '/home/stan/Documents/Twt'
    ignore_matcher = IgnorePatternMatcher(base_path)
    scanner = ParallelScanner(max_workers=4, timeout=30.0, ignore_matcher=ignore_matcher)
    results = await scanner.scan(base_path)
    print(f'Scanned {len(results)} directories successfully')

asyncio.run(test())
"
```

### Expected Results

- **Before fix:** 60+ seconds timeout
- **After fix:** <1 second completion
- **Directories indexed:** 1396 (same as before expansion)
- **Ignored directories:** `node_modules`, `.git`, etc. still correctly ignored

---

## Files Modified

1. **`src/leindex/ignore_patterns.py`**
   - Lines 47-63: Added pre-compiled wildcard patterns
   - Lines 515-585: Rewrote `should_ignore_directory` with fast paths
   - Lines 216-258: Updated `_pattern_matches` to use pre-compiled regex

---

## Key Insights

### 1. Directory vs File Patterns Matter

Glob patterns like `*.pyc`, `*.log`, `*.tmp` are FILE patterns, not directory patterns. Checking them against every directory is wasteful.

### 2. Pattern Matching Order is Critical

Fast O(1) checks should happen BEFORE expensive O(n×m) pattern matching. Most common directories can be matched with simple string comparison.

### 3. Regex Compilation is Expensive

Compiling regex patterns on every match is a performance killer. Pre-compilation at initialization is essential for wildcard-heavy pattern sets.

### 4. Specialized Methods Are Faster

The `should_ignore_directory` method should be optimized for directories, not just a wrapper around the general `should_ignore` method.

---

## Future Optimizations

If performance is still an issue with very large codebases:

1. **Add more directory patterns to fast path:**
   - Identify commonly-ignored directories in your projects
   - Add them to the `dir_only_excludes` set

2. **Profile PatternTrie usage:**
   - Add logging to track how often each path is hit
   - Focus optimization efforts on the most common paths

3. **Consider a Bloom filter:**
   - For very large pattern sets, a Bloom filter could provide O(1) approximate matching
   - Would trade some memory for faster negative checks

4. **Cache directory results:**
   - Cache the result of `should_ignore_directory` for recently-seen directories
   - Similar to PatternTrie's existing LRU cache

---

## Conclusion

The performance regression was caused by checking file glob patterns against every directory. The fix implements fast-path O(1) lookups for common directories before falling back to PatternTrie, plus pre-compiles wildcard regex patterns for better performance.

**Expected performance:** <1 second for directory scanning (99% improvement from 60+ seconds).

**Key lesson:** When optimizing pattern matching, always consider the characteristics of what you're matching (directories vs files) and optimize for the common case.
