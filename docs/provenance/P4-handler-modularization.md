# Provenance: P4 — MCP Handler Modularization

## Source
- **File**: `src/cli/mcp/handlers.rs` (5617 lines, 20 handler structs, 43 tests)
- **Commit**: `85a425a` (HEAD of `feature/unified-crate`)

## Target
- `src/cli/mcp/handlers.rs` — slim to ~250 lines (ToolHandler enum + all_tool_handlers() dispatch)
- `src/cli/mcp/helpers.rs` — shared utilities (~400 lines)
- 18 individual `*_handler.rs` files — each handler struct + impl + tests

## Spec Bible
- `docs/LEINDEX_REFACTORING_GUIDE.md` Section 3, Step 4

## Approach
- **Preserve enum-based dispatch**: The `ToolHandler` enum and `all_tool_handlers()` function remain in `handlers.rs`. The previous agent introduced a trait-based `ToolHandler` in `mod.rs` — this is reverted because the refactoring guide specifies keeping the enum approach, and it avoids the complexity of `Arc<dyn Trait>` adapters.
- **Each handler → own file**: Handler structs and their `impl` blocks move to `src/cli/mcp/{name}_handler.rs`. Each file uses `use super::helpers::*;` for shared utilities.
- **Shared helpers → helpers.rs**: All utility functions (`extract_string`, `resolve_scope`, `wrap_with_meta`, `parse_edit_changes`, etc.) move to `helpers.rs`.
- **Tests migrate with handlers**: Each handler's unit tests move to its respective file. Cross-cutting tests (handler names, argument schemas) move to `mod.rs`.

## Translation Rules
1. Handler struct definitions are identical copies — no signature changes.
2. `impl HandlerStruct` blocks are identical copies — no behavioral changes.
3. Helper functions are moved verbatim with `pub(crate)` visibility.
4. The `ToolHandler` enum dispatch methods (`name()`, `description()`, `argument_schema()`, `execute()`) remain in `handlers.rs` as match arms.
5. All 43 tests must be accounted for — none dropped.

## Handler File Mapping

| Handler | Source Lines (handlers.rs) | Target File |
|---------|---------------------------|-------------|
| IndexHandler | 269-341 | index_handler.rs |
| SearchHandler | 342-535 | search_handler.rs |
| DeepAnalyzeHandler | 536-607 | deep_analyze_handler.rs |
| ContextHandler | 608-678 | context_handler.rs |
| PhaseAnalysisHandler + Alias | 679-1010 | phase_handler.rs |
| DiagnosticsHandler | 1034-1124 | diagnostics_handler.rs |
| FileSummaryHandler | 1253-1455 | file_summary_handler.rs |
| SymbolLookupHandler | 1456-1792 | symbol_lookup_handler.rs |
| ProjectMapHandler | 1793-2094 | project_map_handler.rs |
| GrepSymbolsHandler | 2095-2635 | grep_symbols_handler.rs |
| ReadSymbolHandler | 2636-2901 | read_symbol_handler.rs |
| EditPreviewHandler | 3234-3413 | edit_preview_handler.rs |
| EditApplyHandler | 3414-3601 | edit_apply_handler.rs |
| RenameSymbolHandler | 3602-3792 | rename_symbol_handler.rs |
| ImpactAnalysisHandler | 3793-3962 | impact_analysis_handler.rs |
| TextSearchHandler | 3963-4261 | text_search_handler.rs |
| ReadFileHandler | 4289-4653 | read_file_handler.rs |
| GitStatusHandler | 4654-4912 | git_status_handler.rs |

## Shared Helpers (→ helpers.rs)

| Function | Source Line |
|----------|------------|
| extract_string | 198 |
| extract_usize | 214 |
| extract_bool | 227 |
| validate_file_within_project | 246 |
| node_type_str | 1125 |
| resolve_scope | 1138 |
| wrap_with_meta | 1172 |
| read_source_snippet | 1191 |
| byte_range_to_line_range | 1205 |
| get_direct_callers | 1240 |
| parse_edit_changes | 2902 |
| apply_changes_in_memory | 3041 |
| replace_whole_word | 3102 |
| normalise_ws | 3143 |
| find_normalised_whitespace | 3165 |
| make_diff | 3218 |
| glob_match | 4262 |
| phase_analysis_schema | (in phase_handler.rs) |

## Verification
- `cargo check` — 0 errors
- `cargo check --tests` — 0 errors
- `cargo test --lib -- cli::mcp` — all tests pass
- `cargo clippy --all-targets --all-features` — clean
- `handlers.rs` < 300 lines
- No behavioral changes from HEAD
