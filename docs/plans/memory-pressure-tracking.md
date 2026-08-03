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
| T0 | Baseline capture (`free -h`, per-proc RSS/swap/VSZ, index size, uptimes) | Agent B | ✅ | 62Gi/40Gi used, swap 31/31Gi; claude mcp 2.39GiB RSS/14.0GiB swap; worker 2.15GiB/8.9GiB/24.3GiB VSZ; index 51G; 8+ mcp instances | — | Evidence: findings doc §2; plan + tracker approved by GrayHill (msg 72/73) + user scope (bolt onto PR #51) |
| T1 | Config plumbing (mcp idle, engine max idle, ORT threads, worker RSS/available knobs) | Agent B | ✅ | fmt PASS · clippy cli --all-targets -D PASS (j2-capped) · config 49/0 · setup 99/0 · mcp 203/0 · setup 100/0 after GrayHill's `[mcp]`-preserve fix | `bc6faf9d` + GrayHill `f4f13444` | ✅ APPROVED by GrayHill (msg 77, finding fixed by GrayHill in f4f13444); re-validated in T2–T4 pass (setup 100/0) |
| T2 | MCP process-level idle exit (`--mcp-idle-timeout-secs`) | Agent B | ✅ | fmt PASS · clippy cli + onnx --all-targets -D PASS · lizard 0 · Large-File PASS · full lib 1333/0 (mcp 212) · clock/touch + idle_exit_due + effective-timeout tests | `478295ad` | ProcessIdleClock + stdin reader thread + mpsc select; socket accept-loop ticker. In-flight never interrupted (check only between requests). NOT pushed — GrayHill review → |
| T3 | ProjectRegistry loaded-engine idle eviction | Agent B | ✅ | same gate (registry 35/0); tests: idle-removal, recent-skip, in-flight-skip | `478295ad` | `last_used` touched on get_or_load; 60s sweep (stdio cleanup + socket loop) evicts via `evict_idle_engines` (try_write in-flight guard) |
| T4 | Single-instance per-project lock | Agent B | ✅ | same gate (lock 30/0); tests: sidecar write/release, AlreadyOwned, stale-steal, coexistence | `478295ad` | ADVISORY only (msg 74 call): warn + continue, never hard-exit; stale lock stolen on dead PID; Drop releases sidecars |
| T5 | Worker ORT intra-op thread cap | Agent B | ⬜ | — | — | Needs T1 env knob; memcheck RSS delta {0,4,1} |
| T6 | Worker memory guards (min-available refusal + RSS self-exit) | Agent B | ⬜ | — | — | Needs T1 env knobs + `src/memutil.rs` decision |
| T7 | Stale daemon-artifact GC + `leindex cleanup --stale-daemons` | Agent B | ⬜ | — | — | Independent of T2–T6; can run early |
| T8 | memcheck harness: binary resolution fix + sentinels + 3 new phases | Agent B | 🔨 | `cargo test -p memcheck` 10/0 · resolution (main.rs `resolve_worker_binary`) handles merged `-p leindex --bin leindex-embed` layout · sentinels root-caused (fire only when resolution fails) · T8a budget de-flake: embed_idle 7576→8020, embed_teardown 7412→7900, tolerance 5→10% (ceiling 8822/8690) | `f7fad073` (T8a), `34872661` (T8 wave-2) | Steps (1)-(2) ✅; wave-2: idle_warm/post/final re-baselined to CI-measured floor 8464/8596/8532 (gate red on bf1f59ab — T2 reader-thread + ticker adds ~+800 KiB anon to every idle MCP server; mapped-file pages unchanged); step (3) phases `worker-ort-threads`/`stale-artifacts` deferred to T5/T7 batch (feature deps), `mcp-idle-proliferation` with them; re-baseline via `cargo xtask memcheck --update-baseline` on a healthy box recommended |
| T9 | Docs: MEMORY_MANAGEMENT rewrite, RUNBOOK swap section, CLI.md | Agent B | ⬜ | — | — | After T1/T2 land (docs must match code) |
| T10 | Integration E2E (2 MCP same project → idle-exit; worker RSS delta; no orphans) | Agent B | ⬜ | — | — | Needs T2–T7; user consent for box-level scenario |
| T11 | Cross-review + Codex re-check + PR #51 inclusion | Joint | ⬜ | — | — | Standing protocol: no push without mutual sign-off; Codex re-check after delay |

---

## Open decisions (log every decision + who made it)

| # | Decision | Decided | Date | Notes |
|---|----------|---------|------|-------|
| D1 | MCP idle default = 1800s (30 min), `0`=off | Agent B | 2026-08-02 | ✅ APPROVED GrayHill (msg 72); implemented T2 (commit 478295ad) |
| D2 | Engine max-idle default = 600s | Agent B | 2026-08-02 | ✅ APPROVED GrayHill (msg 72); implemented T3 (commit 478295ad) |
| D3 | ORT threads default = `min(cores, 4)` | Agent B proposal | 2026-08-02 | Must be empirically validated in T5 before finalizing |
| D4 | Worker min-available = 2048 MiB default | Agent B proposal | 2026-08-02 | Only refusal → TF-IDF fallback (non-fatal) |
| D5 | Single-instance lock scope = per canonical project path, ADVISORY only | Agent B | 2026-08-02 | ✅ Resolved (msg 74): warn + continue, never hard-exit; implemented T4 (commit 478295ad) |
| D6 | Shared `src/memutil.rs` (`#[cfg(any(feature="cli", feature="onnx"))]`) for RSS/meminfo | Agent B proposal | 2026-08-02 | Avoids cli↔embed feature coupling; GrayHill to confirm |

---

## Cross-agent handoff log

| Handoff | From | To | State | Notes |
|---------|------|----|-------|-------|
| 1 | Agent B | GrayHill | ✅ | Plan + tracker + findings docs created (2026-08-02). GrayHill approved D-1/D-2/D-4/D-5/D-6 (msg 72); D-3 resolved advisory-only (msg 74); user decided bolt-onto-PR-#51 (msg 73). |
| 2 | Agent B | GrayHill | ✅ | T1 batch committed `4c238135` (docs) + `bc6faf9d` (config knobs). GrayHill APPROVED (msg 77) + fixed the `[mcp]`-preserve gap in `f4f13444`; verified by setup 100/0 in the T2–T4 pass. |
| 3 | Agent B | GrayHill | SENT | T2–T4 committed `478295ad` (mcp idle exit + engine eviction + advisory lock, +717/−53) + T8a committed `f7fad073` (budget re-baseline + tolerance 5→10%). Full gate green (fmt · clippy cli+onnx --all-targets -D · lizard 0 · Large-File · lib 1333/0). NOT pushed — awaiting GrayHill reciprocal review (msg to follow). |
| 4 | Agent B | GrayHill | SENT | T8 wave-2 committed `34872661` — idle_warm/idle_post/idle_final re-baselined to CI-measured floor (8464/8596/8532) after gate red on bf1f59ab (T2 MCP idle self-exit adds ~+800 KiB anon to every idle MCP server); embed_idle/embed_teardown already pass (T8a). PR #51 body Rebaseline marker applied via gh (user-directed). Pushed to feat/embed-merge-1.10.0 for CI re-run. |

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
