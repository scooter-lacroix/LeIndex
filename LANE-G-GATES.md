# Lane G gate record — §6.2 caller call-sites, §6.3 usage kinds, §6.4 tests_for

Branch: `roadmap/g-symbols`
Scope: `src/cli/mcp/{call_sites,usages,tests_mode,symbol_lookup_handler,mod}.rs`, `src/parse/rust.rs`

All commands run in the lane worktree after the final edit, before commit.

```text
$ cargo fmt --all --check
(exited 0 — no output, no diffs)

$ cargo clippy -p leindex --all-targets -- -D warnings
(exited 0 — no warnings)

$ cargo test -p leindex --lib 'cli::mcp::'
test result: ok. 429 passed; 0 failed; 0 ignored; 0 measured; 2644 filtered out; finished in 182.60s

$ cargo test -p leindex --lib 'parse::rust'
test result: ok. 21 passed; 0 failed; 0 ignored; 0 measured; 3052 filtered out; finished in 0.00s
```

Key regression tests behind the §6.2–6.4 acceptance:

- `test_relations_callers_carry_call_sites_ranked_and_paged` — callers carry
  `path:line` + `call_site` (including calls inside `assert_eq!` token
  trees), tests rank last, budget fill, `callers_next_offset` paging; a
  paged walk via `caller_offset` covers every caller exactly once.
- `test_matches_macro_body_surfaces_token_level_sites` — `matches!` bodies
  surface token-level sites (the `expect_path_sep` dead-state-machine fix).
- `test_construct_sites_exclude_comments_strings_and_ctor_calls` — comments,
  strings and `Foo::new()` never become construct sites.
- `test_tests_body_direct_and_transitive_with_exact_commands` — direct test
  at depth 1 and transitive test through a wrapper, direct first, exact
  `cargo test -p proj --lib … -- --exact` command.
