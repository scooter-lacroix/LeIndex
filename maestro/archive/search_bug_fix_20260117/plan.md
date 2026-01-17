# Plan: search_bug_fix_20260117

## Phase 1: Fix Bug #2 - InvalidPatternError Handler [checkpoint: Phase 1 complete]

### Task 1.1: Fix e.message Attribute Error
- [x] Task: Fix InvalidPatternError handler in server.py line 2738
  - File: src/leindex/server.py
  - Change: Replace `{e.message}` with `{str(e)}` in the error return dict
  - Verification: lsp_diagnostics clean on server.py

---

## Phase 2: Fix Bug #1 - search_code_advanced Parameters [checkpoint: Phase 2 complete]

### Task 2.1: Extend search_code_advanced Signature
- [x] Task: Add missing parameters to search_code_advanced function signature

### Task 2.2: Pass Boosting Parameters to Backend
- [x] Task: Update search_code_advanced to pass parameters to backend search

## Phase 3: Verification [checkpoint: Phase 3 complete]

### Task 3.1: Verify search_content Tool Works
- [x] Task: Test search_content with action="search" executes without errors

### Task 3.2: Verify cross_project_search_tool Error Handling
- [x] Task: Test cross_project_search_tool with invalid pattern

## Phase 4: Code Review

### Task 4.1: Mandatory Code Review
- [ ] Task: Launch codex-reviewer agent for validation
  - Review scope: All changes to server.py
  - Verify: Bug fixes are correct and complete
