# Memory-Pressure Remediation — Execution Tracker & Handoff

> Companion to `docs/plans/memory-pressure-remediation.md` (design + task detail). This file is the **single source of truth for execution progress** and the **cross-agent handoff contract**: each task's owner, status, validation evidence, and review state. Update the table on every state change; a task is only DONE when its acceptance criteria + validation gate are recorded here and the cross-reviewer has signed off.

**Plan:** LeIndex memory-pressure remediation (swap saturation). **Branch:** `feat/embed-merge-1.10.0` (shared tree, PR #51). **Baseline evidence:** `docs/findings/2026-08-02-leindex-swap-saturation-investigation.md`.

---

## Legend
- **Status:** ⬜ not started · 🔨 in progress · ✅ done · 🔁 in review · ⛔ blocked
- **Gate:** fmt `--check` · clippy `--all-targets -D` (cli + onnx) · lizard `-C15` (0) · Large-File (≤2000) · lib tests · onnx suite · memcheck suite
- **Handoff:** the exact commit/state the next reviewer starts from + the next reviewer's expected action.

---

## Master table

| # | Task | Owner | Status | Empirical/validation evidence | Commit | Handoff notes |
|---|------|-------|--------|-------------------------------|--------|---------------|
| T0 | Baseline capture (`free -h`, per-proc RSS/swap/VSZ, index size, uptimes) | Agent B | ✅ | 62Gi/40Gi used, swap 31/31Gi; claude mcp 2.39GiB RSS/14.0GiB swap; worker 2.15GiB/8.9GiB/24.3GiB VSZ; index 51G; 8+ mcp instances | — | Evidence: findings doc §2; handoff = plan approved by GrayHill before T1 |
| T1 | Config plumbing (mcp idle, engine max idle, ORT threads, worker RSS/available knobs) | Agent B | ⬜ | — | — | GrayHill: review config schema + dotfile parity |
| T2 | MCP process-level idle exit (`--mcp-idle-timeout-secs`) | Agent B | ⬜ | — | — | Needs T1 knob; unit test with 2s window |
| T3 | ProjectRegistry loaded-engine idle eviction | Agent B | ⬜ | — | — | Needs T1 knob; RSS-baseline measurement required (51G project) |
| T4 | Single-instance per-project lock | Agent B | ⬜ | — | — | Stretch; confirm scope with maintainer |
| T5 | Worker ORT intra-op thread cap | Agent B | ⬜ | — | — | Needs T1 env knob; memcheck RSS delta {0,4,1} |
| T6 | Worker memory guards (min-available refusal + RSS self-exit) | Agent B | ⬜ | — | — | Needs T1 env knobs + `src/memutil.rs` decision |
| T7 | Stale daemon-artifact GC + `leindex cleanup --stale-daemons` | Agent B | ⬜ | — | — | Independent of T2–T6; can run early |
| T8 | memcheck harness: binary resolution fix + sentinels + 3 new phases | Agent B | ⬜ | — | — | Fix resolution FIRST; verify sentinels stop firing |
| T9 | Docs: MEMORY_MANAGEMENT rewrite, RUNBOOK swap section, CLI.md | Agent B | ⬜ | — | — | After T1/T2 land (docs must match code) |
| T10 | Integration E2E (2 MCP same project → idle-exit; worker RSS delta; no orphans) | Agent B | ⬜ | — | — | Needs T2–T7; user consent for box-level scenario |
| T11 | Cross-review + Codex re-check + PR #51 inclusion | Joint | ⬜ | — | — | Standing protocol: no push without mutual sign-off; Codex re-check after delay |

---

## Open decisions (log every decision + who made it)

| # | Decision | Decided | Date | Notes |
|---|----------|---------|------|-------|
| D1 | MCP idle default = 1800s (30 min), `0`=off | Agent B proposal | 2026-08-02 | Awaiting GrayHill + maintainer sign-off in plan review |
| D2 | Engine max-idle default = 600s | Agent B proposal | 2026-08-02 | Tuned with memcheck phase |
| D3 | ORT threads default = `min(cores, 4)` | Agent B proposal | 2026-08-02 | Must be empirically validated in T5 before finalizing |
| D4 | Worker min-available = 2048 MiB default | Agent B proposal | 2026-08-02 | Only refusal → TF-IDF fallback (non-fatal) |
| D5 | Single-instance lock scope = per canonical project path | Agent B proposal | 2026-08-02 | Stretch; maintainer to confirm worth in PR scope |
| D6 | Shared `src/memutil.rs` (`#[cfg(any(feature="cli", feature="onnx"))]`) for RSS/meminfo | Agent B proposal | 2026-08-02 | Avoids cli↔embed feature coupling; GrayHill to confirm |

---

## Cross-agent handoff log

| Handoff | From | To | State | Notes |
|---------|------|----|-------|-------|
| 1 | Agent B | GrayHill | SENT | Plan + tracker + findings docs created (2026-08-02). Request: review D1–D6 + territory map before T1. Agent B holds all implementation until alignment. |

---

## Validation gate template (paste into the task row when done)

```
fmt:                 PASS/FAIL
clippy cli -D all:   PASS/FAIL
clippy onnx -D all:  PASS/FAIL
lizard C15:          N warnings
Large-File:          PASS/FAIL
lib tests:           N passed / M failed
onnx suite:          N passed / M failed
memcheck suite:      N passed / M failed
empirical deltas:    <recorded numbers>
```
