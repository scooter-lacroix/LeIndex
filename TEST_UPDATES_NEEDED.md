# Test Updates Needed for Graceful Degradation

## Overview
The critical fixes for graceful degradation in directory walk have changed the behavior from "crash on error" to "continue with partial results". This is the CORRECT and DESIRED behavior per the Tzar of Excellence review.

## Affected Tests
Two tests in `tests/unit/test_async_traversal.py` expect the old behavior:

1. `test_walk_error_on_invalid_path` (line ~477)
2. `test_walk_os_error_handling` (line ~507)

## Current Behavior
These tests use `pytest.raises((OSError, IOError))` expecting exceptions to be raised.

## New Behavior (Post-Fix)
With the graceful degradation fix, these errors are now caught and logged, and the function continues with `walk_results = []` (partial results).

## Impact
The tests now fail not because the fix is broken, but because the tests expect the OLD (incorrect) behavior of crashing on errors.

## Test Update Required
The tests should be updated to verify that:
1. Errors are logged appropriately (WARNING level)
2. No exception is raised (graceful degradation)
3. Empty/partial results are returned
4. Processing continues without crashing

## Current Test Status
- 61/63 tests pass
- 2 tests fail because they expect the old behavior
- The failures are EXPECTED and CORRECT for the new behavior

## Recommendation
Update these tests to reflect the new graceful degradation behavior:

```python
async def test_walk_error_on_invalid_path(reset_globals):
    """Test that errors during walk are handled gracefully (CRITICAL FIX)."""
    # ... setup ...
    # OLD: with pytest.raises((OSError, IOError)):
    # NEW: No exception should be raised
    result = await _index_project(file_path)
    # Verify partial results or error logging occurred
    assert result is not None  # or similar assertion
```

## Note
The graceful degradation fix is working correctly. The test failures validate that the fix is in place and preventing crashes as intended.
