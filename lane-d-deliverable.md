# Lane D — d-git — deliverable note (SPEC.md format)

Worktree /mnt/WD-SSD/Prod/LeIndex-rm-d-git, branch roadmap/d-git, env slot 4.
Commits (oldest → newest): a50725fb (1/6 apply engine), 39be7c3d
(orchestrator wip snapshot of lane work), 9448b3d5 (2/6 wiring + §3.11 +
§4.5 defaults), 88c854f0 (3/6 docs), d3a1f5e9 (4/6 gate fixes), e471915a
(5/6 required-case tests + show measurement); this note is the tip commit.
Branch NOT pushed per SPEC.

## What landed
- §5 apply: preview_token binding (argv+HEAD oid+index digest, reject on
  drift), confirm:true safe subset (add/commit/push), rails (force-push =
  force:true + confirm_text equal to the plan-printed string; reset --hard /
  clean -fd / branch -d need hard/clean/delete:true AND are token-only —
  branch delete via target:"-d <branch>" + delete:true), bulk-add exclusion
  of secrets/runtime artefacts with warnings + excluded list + explicit-path
  and allow_sensitive overrides, plan lists exactly what will be staged.
- commit: paths = git pathspec commit (commits exactly those working-tree
  paths, other staged content stays staged; atomic, no temp index —
  justified in code + tested), message, trailers via --trailer (one per
  line, single-line validated); result <shorthash> <oneline> + files-changed.
- push: --porcelain remote ref range + upstream ahead/behind after push.
- Hooks/env: stdin closed, GIT_TERMINAL_PROMPT=0, GIT_ASKPASS=echo,
  GIT_EDITOR=true, no pager, 300s timeout, failures verbatim + bounded +
  truncation count.
- §3.11: Changed Symbols maps diff HUNKS (reused -U0 parse_hunks) to
  symbol byte spans in git status — one new fn (symbol_hunk_ranges) + the
  enrich fn edited; untracked/no-hunk falls back to file-level;
  impact_summary.symbol_mapping="hunk".
- §4.5: diff include_patch now defaults FALSE (long-hunk summaries
  changes.long_hunks with lines/at/kind/in-fn); log per-commit
  files_changed; show stat-by-default, patch opt-in. Docs: grouped.rs
  router + branch summaries, SKILL.md git section + precedence line,
  docs/MCP.md five-tools table + git_diff section + new git_mutations
  section.

## Measurement (brief §4.5)
Compact show: 3,588 chars. include_patch:true: 24,658 chars. Reduction
85.4% (fresh lane binary over stdio MCP, inline server, 40-file commit).
Brief's 13.6K reference point: patch output is now opt-in.

## Gates (env slot 4, final run after last edit)
- cargo fmt --all --check → clean, exit 0.
- cargo clippy --workspace --all-targets -- -D warnings → Finished, zero
  warnings/errors.
- cargo test --workspace --exclude memcheck → test result: FAILED. 3043
  passed; 2 failed; 5 ignored; 0 measured; 0 filtered out; finished in
  481.02s.

## The 2 failures — investigated, NOT mine to silence (fix-or-track)
1. cli::leindex::generation_read_tests::test_layer_search_hydration_
   matches_graph_rebuild. DETERMINISTIC (identical score bits every run:
   layered 1041369580/1040893938 [2 hits] vs rebuilt 1058311974/1057994879/
   1050246270 [3 hits] for "database connection"). Root cause diagnosed:
   the test pins NeuralSearch/RemoteEmbeddings OFF, but on this host an
   embedding route still resolves and the layer-restored engine spawns the
   embed worker via current_exe re-exec with --internal-embed-worker; under
   cargo test current_exe IS the libtest harness, which rejects the token
   ("error: Unrecognized option: 'internal-embed-worker'" precedes the
   panic). Rebuild path stays TF-IDF → engine mismatch → diverged results.
   The test's own doc comment names this hazard class; fix belongs to the
   generation-readers lane (test isolation or worker spawn guard).
2. cli::mcp::server::tests::test_process_idle_clock_touch_resets_idle —
   timing-sensitive idle-clock test in the MCP server lane; passes solo
   (0.03s), failed once under full-suite parallel load. Same treatment:
   owning lane's timing budget, not assertion-weakened here.

## Test-case coverage vs the brief's 13 required cases
All present as *_test.rs / test_ fns (git_apply.rs, git_handler.rs,
git_diff_handler.rs): token + confirm add/commit, token-rejected-after-HEAD-
moves (new), commit-paths exclusivity + trailers, push range, force-push
refusal, reset --hard refusal, secret excluded w/ warning, runtime lock
excluded, hook failure verbatim, hunk→symbol exactness, compact show no
patch (new). Temp git repos throughout; bare-remote push parses the
porcelain range (file:// transport only, no network).
