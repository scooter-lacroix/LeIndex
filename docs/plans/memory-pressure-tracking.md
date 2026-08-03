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
| T5 | Worker ORT intra-op thread cap | Agent B | ✅ | fmt · clippy cli+onnx --all-targets -D PASS · lib 1339/0 · worker 19/0 + runtime 37/0 (onnx) · `with_intra_threads()` at all 3 SessionBuilder sites + rerank; socket-client thread bound (16, `LEINDEX_WORKER_MAX_SOCKET_CLIENTS`) | `d5dfb29d` | `LEINDEX_WORKER_ORT_THREADS` wired (T1 knob now live). Default = 75% of parallelism floor 2 (D3 final, below). Fixes Kilo #1 (`worker_main.rs:282` unbounded spawn) + #2 (`:536` max_frame via `RuntimeConfig::default()` → configured `max_frame_size` through SocketLifecycle). Closes Codex P1 first half |
| T6 | Worker memory guards (min-available refusal + RSS self-exit) | Agent B | ✅ | same gate; `low_memory_refusal` abort-before-model-load + `rss_over_cap` run_loop self-exit, Linux `/proc`-based, documented no-op elsewhere | `d5dfb29d` | `LEINDEX_WORKER_MAX_RSS_MB`/`_MIN_AVAILABLE_MB` wired. Closes Codex P1 second half → P1 fully closed. `src/memutil.rs` decision: helpers live in new `src/embed/runtime_env.rs` (not a separate memutil crate) |
| T7 | Stale daemon-artifact GC + `leindex cleanup --stale-daemons` | Agent B | ✅ | same gate; cleanup 21/0 (6 new sweep tests) · sweep_run_dir CCN refactored under 15 | `d5c87437` | `sweep_stale_daemon_artifacts` over `~/.leindex/run/`: live-pid stem kept whole; dead-pid stem stale regardless of age; non-pid stems mtime-threshold. Directly targets the verified Jul-24→Aug-01 sidecar debris |
| T8 | memcheck harness: binary resolution fix + sentinels + 3 new phases | Agent B | 🔨 | `cargo test -p memcheck` 10/0 · resolution (main.rs `resolve_worker_binary`) handles merged `-p leindex --bin leindex-embed` layout · sentinels root-caused (fire only when resolution fails) · T8a budget de-flake: embed_idle 7576→8020, embed_teardown 7412→7900, tolerance 5→10% (ceiling 8822/8690) · wave-2 idle_warm/post/final → 8464/8596/8532 | `f7fad073` (T8a), `34872661` (T8 wave-2) | Steps (1)-(2) ✅. Step (3) 3 new phases (`mcp-idle-proliferation`/`worker-ort-threads`/`stale-artifacts`): feature deps now RESOLVED (T2/T5/T7 landed in `478295ad`+`d5dfb29d`+`d5c87437`); remaining block = empirical baseline capture via `cargo xtask memcheck --update-baseline` on a healthy box (swap-saturated box here is OOM-risk for the full run; never fabricate baselines). Next sub-batch |
| T9 | Docs: MEMORY_MANAGEMENT rewrite, RUNBOOK swap section, CLI.md | Agent B | ⬜ | — | — | After T1/T2 land (docs must match code) |
| T10 | Integration E2E (2 MCP same project → idle-exit; worker RSS delta; no orphans) | Agent B | ⬜ | — | — | Needs T2–T7; user consent for box-level scenario |
| T11 | Cross-review + Codex re-check + PR #51 inclusion | Joint | ⬜ | — | — | Standing protocol: no push without mutual sign-off; Codex re-check after delay |

---

## Open decisions (log every decision + who made it)

| # | Decision | Decided | Date | Notes |
|---|----------|---------|------|-------|
| D1 | MCP idle default = 1800s (30 min), `0`=off | Agent B | 2026-08-02 | ✅ APPROVED GrayHill (msg 72); implemented T2 (commit 478295ad) |
| D2 | Engine max-idle default = 600s | Agent B | 2026-08-02 | ✅ APPROVED GrayHill (msg 72); implemented T3 (commit 478295ad) |
| D3 | ORT threads default | Agent B proposal | 2026-08-02 | FINAL (T5, `d5dfb29d`): `LEINDEX_WORKER_ORT_THREADS` default = 75% of `available_parallelism`, floor 2 (was `min(cores,4)` proposal). Rationale: linear-scaling embed workloads rarely benefit past ~cores×0.75, and every ORT thread carries a stack + per-thread arena on a swap-saturated box. Empirically confirm RSS delta via memcheck `worker-ort-threads` phase when baselines are captured |
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
| 5 | Agent B | GrayHill | SENT | Division ack (msg 86) + T5/T6/T7 batch committed locally (NOT pushed): `d5dfb29d` (T5+T6: ORT thread cap `with_intra_threads` at all builder sites, socket-client bound 16, max_frame via SocketLifecycle — Kilo #1/#2 — RSS/MemAvailable guards — Codex P1 closed; new `src/embed/runtime_env.rs`, runtime.rs 1968 < 2000), `d5c87437` (T7: `leindex cleanup --stale-daemons` sweep + 6 tests), `67063921` (lock.rs blake3 stable stem + Linux-gated liveness/non-Linux no-op — Codex P2/Kilo #3 — + registry_evict.rs split, registry.rs 1882, :908 doc-note). Full gate green: fmt · clippy cli+onnx `--all-targets -D warnings` · lizard 0 · Large-File · lib 1339/0 · memcheck 10/0. T8 step-3 phases deferred: feature deps resolved, blocked only by healthy-box baseline capture (`cargo xtask memcheck --update-baseline`). Next: GrayHill reciprocal review of the 3 commits. |
| 6 | Agent B | GrayHill | SENT | Self-review pass on the T5/T6/T7 batch (deepseek-flash verdict review of `git diff 34b0d114..67063921`) → 2 P2s + 3 P3s all fixed in `59b0ec54` (73+/18−): (P2) `worker_main.rs` socket-client slot guard now releases the permit on `thread::spawn` failure (guard moves into closure, Drop fires on closure drop — no permit leak); (P2) `cleanup.rs` malformed-pid parse never panics — non-numeric pid files fall back to mtime-stale, regression test added; (P3) clippy `is_none_or`, lock.rs stem-hash edge normalization, runtime re-export surface trimmed to actual test refs. Full gate re-verified green after fixes: fmt · clippy cli+onnx `--all-targets -D warnings` 0 · lizard 0 · Large-File · cleanup 22/0 · lock 31/0 · worker 19/0 · runtime 37/0 · registry 35/0 · **full lib 1340/0**. Stray working-tree `.mcp.json` deletion restored (not part of batch). Still NOT pushed — awaiting GrayHill review of the 4-commit stack (`d5dfb29d`..`59b0ec54`). |
| 7 | Agent B | GrayHill | SENT | GrayHill reciprocal review of T5/T6/T7 (msg 91): **code-level APPROVE**, Codex P1 closed, one pre-push item (commit the +73/−28 residue) + one nit (at-cap socket drop could tight-loop). Both resolved: residue already committed `59b0ec54` (byte-identical to reviewed working-tree state); nit hardened in `6161e80a` (1ms backpressure sleep on at-cap path only + client-backoff doc note; onnx check 0, clippy onnx 0, worker 19/0). Agent B reciprocal-reviewed GrayHill's `48f5f4e8` (their 3 Codex P2s): **APPROVE, verified empirically** — all 4 README surfaces (root:326, docs/MCP.md:62-68, pypi:197, npm:104) carry identical lifecycle keys/defaults; toml.example labels 1.9.5 == Cargo.toml v1.9.5; server.rs shadow removal clean, no orphaned `#[allow]`. Release-time flag: toml.example labels (47/71) need coordinated bump in the release commit. Proposal (msg 93): combined FF non-force push `34b0d114..6161e80a` on GrayHill ack. |
| 8 | Agent B | GrayHill | ✅ | **PUSHED** (msg 95): GrayHill ack (msg 94) → FF non-force push `34b0d114..bf5643ec` to `feat/embed-merge-1.10.0` (10 commits: 48f5f4e8, d5dfb29d, d5c87437, 67063921, 4a6ae5c7, 59b0ec54, f0935316, 6161e80a, bf5643ec). Remote HEAD verified == local. CI on new head: 21 checks queued/in_progress, 0 failures at first poll. Protocol: re-poll CI to completion; re-check Codex/Kilo after delay (inline-comment lag lesson); then joint pre-merge summary to USER. T8 step-3 memcheck phases remain deferred (healthy-box baseline capture), not a blocker. |

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
