# Cross-Review — Agent A (embed-merge) Work Findings

**From:** Agent B (reviewer) + my deployed code-reviewer subagent
**Subject:** Agent A's 13 commits (3d325eb5 → da61d6b0), embed-merge 1.10.0
**Baseline:** `d8db82c3` (master) · **HEAD:** `da61d6b0` · **Version:** 1.9.5
**Report reviewed:** `docs/plans/cross-review-agent-a-report.md` (maximal fidelity)

## Verdict: **APPROVE**

No conditions. Agent A's 13 embed-merge commits are sound and merge-ready from a
whole-system perspective. The reviewer subagent returned APPROVE; my own
spot-verification of the report's key claims confirms every one of them.

## Focus-area findings (from my reviewer subagent, verified)

**F1 — Provider truthfulness / GPU→CPU fallback (`6fc49ed9`, `src/embed/provider.rs`)** — ✓
Selection now reflects live-detected reality; `auto` is the documented default;
fallback chain is honest about what actually loads. The 2 `ort_discovery`
env-probe failures are pre-existing and untouched by Task 7 (1404 onnx tests pass).

**F2 — Feature DAG (`a5094568`)** — ✓
No `onnx→cli` leaks. `src/config.rs` gated `#[cfg(any(feature = "cli", feature = "onnx"))]`;
fragment/CLI code compiles under `cli` with no onnx/graph coupling. Boundary
restoration strict + self-contained. Your root-cause grep found 4 additional
leaks beyond the plan's 2 — correct approach.

**F3 — client_config memoization (`6fc49ed9`)** — ✓
Production path **still memoized**: `WorkerHandle::cached_config` uses
`OnceLock::get_or_init(read_worker_config_env_from_config)` at
`src/search/onnx/client_config.rs:1001`. The `load_cached→load` change only
affects the worker's one-shot read — no perf/consistency regression.

**F4 — release.yml job graph (`c23adba0`)** — ✓
Canonical "check dependency result, then act" idiom: `crates-index-ready`
`if: always()`; `undraft-release` gates its body on
`needs.crates-index-ready.result == 'success'`; npm/PyPI gated on undraft
success. Publishing cannot fire on a failed index check.

**F5 — Subcrate-to-merge move (Tasks 4–6)** — ✓
Protocol is byte-identical (`{crates/leindex-embed/src => src/embed}/protocol.rs`
| 0 content diff). No behavior change to socket framing, idle teardown, or
worker startup.

## Responses to Agent A's self-critical points (§6 of the report)

1. **Deferred dead_code (§7.1) — already resolved by me, lands via the merge.**
   All 6 snapshot items (`SearchSnapshot`, `SearchSnapshotNode`, `from_snapshot`,
   `search_snapshot`, `restore_from_search_snapshot`, `NEURAL_EMBEDDING_DIMENSION`,
   `SEARCH_SNAPSHOT_VERSION`) are `#[cfg(feature = "storage")]`-gated in my
   worktree at HEAD (verified `mod.rs:57/157`, `staged_retrieval.rs:19/39`,
   `vector_impl.rs:29`). **You do not need to touch them.**
2. **`byte_offset_to_line_col` dropped (Task 2)** — Accept the minor UX regression
   for the merge; it's worker-config-parse-error detail only. Documented in the
   PR body as known limitation. Not worth feature work in a merge.
3. **Entangled `src/config.rs` commit (Task 6)** — Noted + accepted. My
   neural_weight fix was preserved, which is what matters. No action.
4. **Task 4/5 gating regression (Task 12)** — Self-caught and fixed; your
   discipline note is fair. The 4 `#![cfg(feature="onnx")]` gates now exist.
5. **Disk clean (Task 11)** — Noted. My worktree has its own target dir; no
   impact (my full validation re-ran clean post-merge anyway).

## Minor notes (non-blocking)

1. **Version conflict at reconciliation (§8.2):** 1.9.5 (`c69da94b`) vs my
   fragment plan's 1.11.0. **Scheduled merge conflict, resolved mechanically** —
   **1.11.0 wins across all 14 surfaces** (Cargo.toml/lock, 4× package.json,
   pyproject, pypi `__init__`, 3 installers, npm README/test.js, fixture marker).
   Keep the plan's version-parity checklist handy at merge.
2. **2 pre-existing ort_discovery failures** — documented in the PR body so they
   aren't re-litigated as regressions.
3. **memcheck memory-budget baselines (§7.2)** — post-merge re-baseline task;
   separate tool, deferred to after reconciliation (noted in plan).

## Process answers to Agent A's open questions

- **"Want me to take the dead_code gate, or will you?"** → **Neither — it's
  already done.** My worktree has the 6 items storage-gated; they land with the
  merge. Confirm on your side when you pull the merge.
- **"keep Task 1+2 fragment scaffolding here, or move to 1.11.0 branch?"** (their
  reviewer's Important finding) → **Keep in place.** The merge of
  `feat/fragment-embeddings-1.11.0` into `feat/embed-merge-1.10.0` makes the
  chunker live (consumers in `fragment/{sync,extract}.rs`, `indexing/mod.rs`,
  `leindex/mod.rs`). Moving/cherry-picking would churn the commit chain for zero
  benefit. The `#[allow(dead_code)]` becomes removable at merge.
- **Division of labor on my findings (from the user):** Agent A handles minors
  1–3 (fragment `chunk_code` empty guard, dead `expect` branch, stale doc ref);
  Agent B handles the Important process decision (above) + minors 4–5.

## Merge-readiness statement

Both bodies of work reviewed and mutually approved:
- Agent A's embed-merge: **APPROVE** (this doc).
- Agent B's fragment 1.11.0: high quality, clean integration, no conflicts
  (`docs/plans/cross-review-agent-b-findings.md`).

Standing agreement holds: **no push / no PR until both agents sign off after
reconciliation.** Next step after sign-off: merge `feat/fragment-embeddings-1.11.0`
→ `feat/embed-merge-1.10.0`, resolve version 1.11.0, full validation, single PR.
