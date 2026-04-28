# Stack Overflow Fix - Deeply Nested Rust Parsing

## Problem Summary

The test `test_cli_index_handles_deeply_nested_rust_without_aborting` was failing with a stack overflow error when indexing deeply nested Rust code (6000 levels of nested if statements).

### Test Details
- **Location**: `tests/cli_integration_test.rs::cli_workflow_tests::test_cli_index_handles_deeply_nested_rust_without_aborting`
- **Error**: `thread 'tokio-runtime-worker' has overflowed its stack`
- **Exit Code**: 134 (SIGABRT)

## Root Cause Analysis

The stack overflow was caused by **recursive function calls** in the Rust parser when processing deeply nested AST structures. Two functions were identified:

1. **`calculate_complexity`** (line 677 in `src/parse/rust.rs`)
   - Recursively traverses the tree-sitter AST to calculate complexity metrics
   - With 6000 levels of nesting, this creates 6000 stack frames

2. **`build_cfg_recursive`** (line 757 in `src/parse/rust.rs`)
   - Recursively builds the control flow graph (CFG)
   - Also creates stack frames proportional to nesting depth

Both functions were called during the indexing pipeline, and the tokio runtime worker thread (which has a limited stack size) would overflow when processing the deeply nested test file.

## Solution Implemented

Converted both recursive functions to **iterative implementations** using explicit stacks:

### 1. `calculate_complexity` - Now Iterative
```rust
fn calculate_complexity(
    node: &tree_sitter::Node<'_>,
    metrics: &mut ComplexityMetrics,
    depth: usize,
) {
    // Use a stack-based approach with explicit traversal to avoid recursion
    let mut stack: Vec<(tree_sitter::Node<'_>, usize)> = Vec::new();
    stack.push((*node, depth));

    while let Some((current_node, current_depth)) = stack.pop() {
        // ... process node ...
        
        // Push children onto stack in reverse order to process them left-to-right
        let mut cursor = current_node.walk();
        let mut children: Vec<tree_sitter::Node<'_>> = current_node.children(&mut cursor).collect();
        children.reverse();
        
        for child in children {
            if child.kind() == "function_item" && current_node.kind() == "block" {
                continue;
            }
            stack.push((child, current_depth + 1));
        }
    }
}
```

### 2. `build_cfg_recursive` → `build_cfg_iterative`
```rust
fn build_cfg_iterative(
    &mut self,
    root_node: &tree_sitter::Node<'_>,
    entry_block: usize,
) -> Result<()> {
    use std::collections::VecDeque;

    let mut work_queue: VecDeque<(tree_sitter::Node<'_>, usize)> = VecDeque::new();
    work_queue.push_back((*root_node, entry_block));

    while let Some((node, current_block)) = work_queue.pop_front() {
        match node.kind() {
            // ... handle different node types ...
            _ => {
                // Add children to the work queue
                let mut cursor = node.walk();
                for child in node.children(&mut cursor) {
                    work_queue.push_back((child, current_block));
                }
            }
        }
    }
    Ok(())
}
```

## Key Changes

1. **Replaced recursion with explicit stacks**: Both functions now use `Vec` or `VecDeque` to manage the traversal state instead of relying on the call stack.

2. **Maintained processing order**: Children are pushed in reverse order to maintain left-to-right processing (for the stack-based approach).

3. **Preserved all original logic**: The actual processing of nodes (complexity calculation, CFG building) remains unchanged - only the traversal mechanism was updated.

## Test Results

### Before Fix
```
thread 'tokio-runtime-worker' has overflowed its stack
fatal runtime error: stack overflow, aborting
test result: FAILED
```

### After Fix
```
running 1 test
test cli_workflow_tests::test_cli_index_handles_deeply_nested_rust_without_aborting ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 35 filtered out; finished in 31.88s
```

### Regression Testing
All 36 CLI integration tests pass:
- 36 passed; 0 failed; 0 ignored

All 27 Rust parser unit tests pass:
- 27 passed; 0 failed; 0 ignored

## Performance Impact

- **Memory**: Iterative approach uses heap-allocated stacks instead of call stack, which is more scalable for deep nesting.
- **Speed**: The test completes in ~32 seconds for 6000 levels of nesting (acceptable for this edge case).
- **Normal cases**: No performance degradation for typical codebases with normal nesting levels.

## Side Effects & Trade-offs

### Positive
- ✅ Eliminates stack overflow for arbitrarily deep nesting
- ✅ More robust handling of edge cases
- ✅ No changes to public API or behavior
- ✅ All existing tests pass

### Considerations
- The iterative implementation is slightly more verbose than the recursive version
- Uses more heap memory for the explicit stack (but this is bounded and manageable)
- The 6000-level test case is an artificial extreme; real code rarely exceeds 100 levels of nesting

## Files Modified

- `src/parse/rust.rs`:
  - Converted `calculate_complexity` to iterative (lines 687-743)
  - Renamed `build_cfg_recursive` to `build_cfg_iterative` and converted to iterative (lines 775-807)
  - Updated `build_from_node` to call the new iterative method (line 773)

## Conclusion

The fix successfully addresses the stack overflow issue by converting recursive AST traversal algorithms to iterative implementations. This allows the parser to handle arbitrarily deeply nested code structures without exceeding the tokio runtime worker thread's stack limits.
