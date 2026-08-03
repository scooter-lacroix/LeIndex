# LeIndex Memory-Pressure Remediation — Swap Saturation — 1.11.0

> **For agentic workers:** REQUIRED SUB-SKILL: superpowers:subagent-driven-development (recommended) or superpowers:executing-plans. Tasks use checkbox (`- [ ]`) syntax. Execution progress + cross-agent handoff is tracked in `docs/plans/memory-pressure-tracking.md`.

> **Sequencing gate:** Independent of the fragment 1.11.0 plan. This plan targets the SHARED merged tree (`feat/embed-merge-1.10.0`, PR #51) and is designed to land either as a follow-up commit batch on that PR or as the first batch of 1.11.0 — per maintainer + GrayHill agreement. It does NOT depend on fragment Tasks 3–7.

---

## 0. Problem statement

The workstation (62 GiB RAM / 31 GiB swap) is in permanent swap-thrash: **40 GiB used, 31/31 GiB swap full (88 MiB free)**. The dominant contributor is the `leindex` family — **8+ live `leindex mcp` instances ≈ 7.8 GiB RSS + ~24 GiB swap**, led by one `leindex mcp` (2.39 GiB RSS / **14.0 GiB swap**, alive 13h26m) and its `leindex-embed` worker (2.15 GiB RSS / **8.9 GiB swap**, 24.3 GiB VSZ). Every agent session spawns its own MCP server; the servers idle-forever while their agent parent lives; the loaded project engine (51 GiB on-disk index) is never unloaded; the worker's anonymous memory far exceeds the model size.

**Severity: Critical** — full-system thrash, all agents slowed, risk of OOM cascade (prior incident: 3 OOM kills + 1 SIGSEGV on this same workload, see `docs/findings/2026-05-06-leindex-memory-oom-investigation.md`).

Evidence capture: `docs/findings/2026-08-02-leindex-swap-saturation-investigation.md`.

---

## 1. Root-cause analysis (evidence-backed)

| RC | Root cause | Evidence | Mechanism |
|----|-----------|----------|-----------|
| RC-1 | No single-instance/reuse for `leindex mcp`; no process-level idle exit | 8 instances; codex holds 7; claude's server 13h26m | `cmd_mcp_stdio_impl` exits on stdin EOF but has no idle timeout; agent parents hold the pipe open for hours |
| RC-2 | Loaded project engine never unloaded in MCP process | claude mcp 2.39 GiB RSS / 17 GiB VSZ; project index 51 GiB | `ProjectRegistry::get_or_load()` loads engine on first tool call and retains it for process lifetime; `cleanup_stale_sessions` evicts sessions only |
| RC-3 | Worker anonymous memory ≫ model size | worker 2.15 GiB RSS / 8.9 GiB swap / 24.3 GiB VSZ vs ~0.6–2.4 GiB model | ORT per-core intra-op thread pools (MLAS/oneDNN per-thread buffers); no `with_intra_op_num_threads` cap; `with_memory_pattern(false)` already set |
| RC-4 | Stale daemon control artifacts linger after SIGKILL | `~/.leindex/run/*.{lock,sock,pid,status}` from Jul 24–26 | PDEATHSIG SIGKILLs the worker; sidecar files only cleaned when a client probes a dead PID |
| RC-5 | No memory-pressure guard at worker/MCP spawn | `MemoryCapGuard` wired only into `index` command | Nothing refuses to spawn a 2.4 GiB worker or load a 51 GiB-engine while swap is 100% |

**What already works (protect):** PDEATHSIG · daemon lock/sock/status/pid/start sidecars + stale-PID kill · worker 600s idle timeout · CLI `force_shutdown_daemon` · MCP stdin-EOF exit · MCP session idle eviction · memcheck worker-binary resolution.

---

## 2. Design decisions (engineering judgement, logic decided)

### D-1. MCP process-level idle exit — THE highest-impact, cheapest lever
- **What:** add `--mcp-idle-timeout-secs <N>` (default **1800 = 30 min**; `0` = disabled) to `leindex mcp`/`leindex mcp --socket`. When no MCP request (any tool/session activity) arrives for the window, exit 0 cleanly (persist nothing — index artifacts are on disk; MCP clients respawn the server on the next tool call, which is standard MCP semantics).
- **Why:** kills the 8-instance accumulation directly. Each server that goes idle for 30 min self-terminates, releasing its 2.4 GiB RSS + 14 GiB swap without needing any parent cooperation. Zero functional loss: agent clients (claude/codex/maestro) already handle server exit + respawn as the normal MCP lifecycle.
- **Where:** `cmd_mcp_stdio_impl` + `cmd_mcp_socket_impl` accept the knob (from `cli.rs` `Mcp` subcommand + config `[mcp] idle_timeout_secs`); a background task tracks `last_request_at` (already tracked per-session; hoist a process-level last-activity clock updated in `response_for_payload`), and on timeout calls `server.shutdown().await` / returns `Ok(())`.
- **Accepted trade-off:** a cold reload on next call (~index load latency, seconds). For the 51 GiB project this is seconds-to-tens-of-seconds; acceptable vs. permanent 14 GiB swap residency.
- **Interaction:** do NOT exit if a request is mid-flight; only check between requests.

### D-2. Loaded-engine idle eviction in ProjectRegistry
- **What:** loaded engines get a `last_used` timestamp; a periodic sweep (reuse the existing 60s cleanup task) drops engines idle > `ENGINE_MAX_IDLE` (default 600s) from the registry, releasing their mmaps/heap. Next tool call reloads via `get_or_load`.
- **Why:** caps per-process RSS even while the agent parent is alive and talking (sessions active but a particular project unused). Without this, one server that touched the 51 GiB project holds 2.4 GiB forever.
- **Where:** `src/cli/registry.rs` `ProjectRegistry` — add `evict_idle_engines(max_idle)`; wire into the existing `spawn_stdio_cleanup` interval task and the socket server's idle sweep.
- **Consistency guard:** eviction must be safe with in-flight tool calls — evict only engines with no active call (track an `active_calls` counter per project, as `server.rs` does for sessions).

### D-3. Single-instance per-project guard (stretch, high value)
- **What:** a project-scoped lockfile `~/.leindex/run/leindex-mcp-<project-hash>.lock` + `.pid` written by `cmd_mcp_stdio_impl` (and socket mode). On start: if an existing live instance owns the lock for the same canonical project path, log a warning and **exit 0** ("another leindex mcp already serves this project; reusing it"). If the PID is dead, steal the lock (write pid+start-time, mirroring the daemon's `daemon_pid_is_owned` logic).
- **Why:** prevents 8 duplicate servers for the same project. This is a **safe no-op** in the common case (each agent indexes its own project) and a real win when several agents work the same repo (our exact situation).
- **Where:** new small module `src/cli/mcp/lock.rs` (reuse `write_worker_pid`/start-time patterns from `worker_main.rs`); called at the top of both `cmd_mcp_*_impl`.
- **Risk:** must NOT break legitimate multi-project parallel servers (lock is per canonical project path, not global).

### D-4. Worker ORT memory tuning — thread cap first
- **What:** `SessionBuilder::with_intra_op_num_threads(cap)` where `cap = LEINDEX_WORKER_ORT_THREADS` env (default `min(num_cpus, 4)`; `0` = ORT default). Apply in `src/embed/runtime.rs` at every session-builder site (embedder + reranker + the CPU-fallback builders).
- **Why:** ORT's MLAS/oneDNN intra-op thread pools allocate per-thread buffers; on a many-core box (≥16) this is the most plausible contributor to the worker's multi-GB anonymous footprint. Capping to 4 keeps a 0.6B model's inference comfortably fast (embedding is memory-bound, not thread-bound) while collapsing the pool.
- **Also:** keep `with_memory_pattern(false)` (already set); keep `Level1` optimization (lower peak than Level3 for small models); do NOT enable GPU EP by default for the embed worker unless the user configured it (unchanged).
- **Measurement requirement:** memcheck phase compares worker RSS with cap=0 vs cap=4 vs cap=1 on the SAME batch workload; record numbers in the tracker before/after (see §4).

### D-5. Worker memory-pressure guard at spawn time
- **What:** before loading the model, the worker computes system `MemAvailable` (Linux `/proc/meminfo`) and `current_rss_mb()`; if `MemAvailable < LEINDEX_WORKER_MIN_AVAILABLE_MB` (default **2048**) the worker writes status `failed` ("system under memory pressure") and exits 0 — the client's existing fallback path degrades to TF-IDF/PDG (VAL-CPHASE-020).
- **Why:** prevents spawning another 2.4 GiB worker while the box is at 100% swap. Degrade gracefully instead of thrashing more.
- **Where:** `WorkerRuntime::new` start (socket + pipe), before ORT env/session creation; reuse `memory_cap::current_rss_mb` (feature-gate for non-cli builds: `memory_cap` is in `src/cli/` — worker is `src/embed/`; duplicate the tiny `/proc/meminfo` read in `src/embed/memguard.rs` or move `current_rss_mb`/`meminfo_available_mb` to a shared location the embed module can use without the cli feature).

### D-6. Worker RSS self-exit under a configurable cap
- **What:** `LEINDEX_WORKER_MAX_RSS_MB` (default 0 = disabled; doc default 4096). The socket accept loop already polls `is_idle_expired()`; add `is_over_rss_cap()` checked on the same tick: if RSS > cap, log + `shutdown_worker` (graceful, removes sidecars). The client's dead-worker path respawns a fresh worker on the next embed request.
- **Why:** bounds a long-lived worker even when it accumulates (arena growth, pathological inputs). Fresh-respawn is the reset.

### D-7. Stale daemon-artifact GC
- **What:** (a) in `cleanup_daemon_paths`-adjacent code, add `sweep_stale_daemon_artifacts()` that scans `~/.leindex/run/*.pid`, reads each pid+start, and removes `.lock/.sock/.status/.pid/.start` for dead PIDs (same `daemon_pid_is_owned` semantics); (b) call it opportunistically from `spawn_or_connect_daemon` before acquiring the spawn lock (cheap, bounded) and add a manual `leindex cleanup --stale-daemons` subcommand.
- **Why:** removes the Jul 24–26 debris and any future SIGKILL residue; keeps `run/` self-healing.

### D-8. memcheck harness — process-lifecycle phases + worker memory regression
- **What:** extend `tools/memcheck` with:
  - `mcp-idle-proliferation` phase: spawn 3 `leindex mcp` (same project), idle 2 min, assert (a) engine eviction keeps RSS < threshold, (b) after `--mcp-idle-timeout-secs=5` all exit and process count returns to 0.
  - `worker-ort-threads` phase: measure worker RSS after N embed batches with `LEINDEX_WORKER_ORT_THREADS` ∈ {0, 4, 1}; assert cap ≤ no-cap RSS (and log the deltas as the empirical record).
  - `stale-artifacts` phase: create dead-pid sidecars, run `leindex cleanup --stale-daemons`, assert removal.
- **Fix first:** verify `tools/memcheck/src/main.rs:182–223` worker-binary resolution actually resolves under the merged layout (the retired-subcrate binary no longer exists; it's `-p leindex --bin leindex-embed` now) and that the `u64::MAX` sentinels in `workload.rs` (pre-existing known issue) no longer fire. Run the suite before/after.

---

## 3. Task breakdown (step-by-step, with acceptance criteria)

> Each task is independently committable and reviewable. Ordering follows §3.1 dependencies. All tasks carry the SAME validation gate: `cargo fmt --all --check` · `cargo clippy -p leindex --features cli --all-targets -- -D warnings` · `cargo clippy -p leindex --no-default-features --features onnx --all-targets -- -D warnings` · lizard `-C 15` (0 warnings) · Large-File gate (≤2000 lines) · `cargo test -p leindex --features cli --lib` (full) · `cargo test -p leindex --no-default-features --features onnx` (worker-suite).

### T1 — Config plumbing for the four new knobs
- **Goal:** `[mcp] idle_timeout_secs` (default 1800), `[mcp] engine_max_idle_secs` (default 600), `LEINDEX_WORKER_ORT_THREADS` (default min(cores,4)), `LEINDEX_WORKER_MAX_RSS_MB` (default 0), `LEINDEX_WORKER_MIN_AVAILABLE_MB` (default 2048) — in `src/config.rs` (MCP section), `.env.example`, `leindex.toml.example`, and CLI flag `--mcp-idle-timeout-secs`.
- **Logic decided:** env vars override toml overrides defaults for worker knobs (worker has no toml access); MCP knobs live in toml + CLI flag (CLI flag wins).
- **Acceptance:** config round-trip tests; docs surfaces updated (README/dotfiles parity per AGENTS.md).
- **Files:** `src/config.rs`, `leindex.toml.example`, `.env.example`, `src/cli/cli.rs`, docs.

### T2 — MCP process-level idle exit (D-1)
> ✅ **DONE** — commit `478295ad` (with T3/T4). Unit tests: clock touch-reset, `idle_exit_due` logic, effective-timeout priority/zero.
- **Goal:** `cmd_mcp_stdio_impl` + `cmd_mcp_socket_impl` exit 0 after `idle_timeout_secs` of no requests.
- **Steps:** (1) hoist a process-level `last_request` Instant updated in `response_for_payload` (and socket handler entry); (2) in the stdio loop's idle wait (`read_stdio_input` returns `Skip`/would-block? — if it blocks, wrap reads with a timeout or poll a channel) add a deadline check; stdio read currently blocks — implement a select over `last_request` elapsed vs a background tick that triggers exit (tokio `select!` on a sleep vs. the next request via a mpsc feeding `response_for_payload`), mirroring how `server.rs` sessions are swept; (3) socket mode: reuse `run_socket` accept loop with the same idle clock.
- **Acceptance:** new unit tests with a 2s idle timeout: process exits after silence; a request at t=1s resets the clock; in-flight request is never interrupted. memcheck `mcp-idle-proliferation` phase green.
- **Files:** `src/cli/mcp_commands.rs`, `src/cli/mcp/server.rs`, `src/cli/mcp/server_test.rs`.

### T3 — Loaded-engine idle eviction (D-2)
> ✅ **DONE** — commit `478295ad`. `last_used` per project (touched on `get_or_load`); `evict_idle_engines` skips in-flight via `try_write`; wired into the stdio 60s cleanup task + socket accept-loop sweep. Tests: idle-removal + in-flight/recent skip.
- **Goal:** `ProjectRegistry` evicts engines idle > `engine_max_idle_secs`.
- **Steps:** (1) add `last_used` + `active_calls` to the per-project entry (registry.rs); (2) `evict_idle_engines()` drops entries with `active_calls == 0 && last_used.elapsed() > max_idle` (guard: engine's own Drop releases mmaps — verify `LeIndex`/`SearchEngine` Drop order frees the mmap twins; if not, add explicit `unload()`); (3) call from the existing 60s cleanup task (stdio + socket).
- **Acceptance:** unit test: load engine, idle-past-threshold, assert registry empty + RSS returned to baseline (memcheck phase). In-flight-call test: an active call blocks eviction.
- **Files:** `src/cli/registry.rs`, `src/cli/mcp_commands.rs` (cleanup wiring), tests.

### T4 — Single-instance per-project lock (D-3)
> ✅ **DONE (advisory)** — commit `478295ad`. Per GrayHill design-flaw resolution (msg 74): the second instance WARNS and continues — never exits — because a stdio server is 1:1 with its agent's pipe. Tests: sidecar write/release, live-sibling `AlreadyOwned`, dead-pid stale-steal, cross-project coexistence.
- **Goal:** second `leindex mcp` for the same project exits 0 with a warning.
- **Steps:** (1) new `src/cli/mcp/lock.rs` — `try_acquire(project_canonical)` writes `run/leindex-mcp-<hash>.{lock,pid,start}`; `pid_is_owned` reuses start-time comparison (port from `client_config::daemon_pid_is_owned`); (2) stale-steal on dead PID; (3) release on shutdown/exit (Drop guard + explicit on all exit paths).
- **Acceptance:** unit tests: second instance exits 0; dead-pid lock is stolen; different projects coexist; release on clean exit.
- **Files:** `src/cli/mcp/lock.rs`, `src/cli/mcp_commands.rs`, tests.

### T5 — Worker ORT thread cap (D-4)
- **Goal:** every session-builder site caps intra-op threads.
- **Steps:** (1) add `fn effective_ort_threads() -> usize` in `src/embed/runtime.rs` reading `LEINDEX_WORKER_ORT_THREADS` (parse; `0`→default) else `min(num_cpus, 4)`; (2) chain `.with_intra_op_num_threads(effective_ort_threads())` at the primary builder + the two CPU-fallback builders (`try_provider_or_cpu`, `maybe_missing_ep_fallback`); (3) also apply to the reranker session if it builds its own.
- **Acceptance:** memcheck `worker-ort-threads` phase records RSS for {0,4,1} and asserts cap ≤ no-cap; worker-suite (`onnx`) tests still green.
- **Files:** `src/embed/runtime.rs`, `src/embed/runtime_test.rs`, `tools/memcheck`.

### T6 — Worker memory-pressure guards (D-5, D-6)
- **Goal:** worker refuses to start under pressure; self-exits over RSS cap.
- **Steps:** (1) `src/embed/memguard.rs`: `meminfo_available_mb()` + reuse `current_rss_mb` logic (share via a new `src/embed/`-visible helper or duplicate 20 lines; decide: move `current_rss_mb` into a shared `src/memutil.rs` gated `#[cfg(any(feature="cli", feature="onnx"))]` so both cli and embed use one implementation — cleaner, no feature coupling); (2) `WorkerRuntime::new`: if `available < MIN` → status `failed` + exit 0 (client falls back); (3) socket accept loop tick: `rss > MAX_RSS` → `shutdown_worker`.
- **Acceptance:** unit tests with env-injected thresholds (temporarily overridable for tests: add `#[doc(hidden)]` env override `LEINDEX_WORKER_FORCE_AVAILABLE_MB` for testability); memcheck phase under artificial `MAX_RSS_MB=256` with a synthetic batch → worker exits and client respawns.
- **Files:** `src/embed/memguard.rs` (new), `src/memutil.rs` (new, if shared), `src/embed/runtime.rs`, `src/cli/memory_cap.rs` (repoint to shared util), tests.

### T7 — Stale daemon-artifact GC (D-7)
- **Goal:** dead-pid sidecars swept automatically + via `leindex cleanup --stale-daemons`.
- **Steps:** (1) `sweep_stale_daemon_artifacts()` in `src/search/onnx/client_config.rs` (scan `run/*.pid`; reuse `daemon_pid_is_owned`); (2) call before `DaemonSpawnLock::acquire`; (3) CLI subcommand `leindex cleanup --stale-daemons`.
- **Acceptance:** unit tests with synthetic dead sidecars; memcheck `stale-artifacts` phase green.
- **Files:** `src/search/onnx/client_config.rs`, `src/cli/cleanup.rs`, `src/cli/cli.rs`, tests.

### T8 — memcheck harness fixes + new phases (D-8)
> ⏳ **PARTIAL** — steps (1)-(2) verified + **T8a done** (commit `f7fad073`): worker-binary resolution (main.rs `resolve_worker_binary`) already handles the merged `-p leindex --bin leindex-embed` layout; sentinel root-cause documented (u64::MAX placeholders fire only when resolution fails); budget gate de-flaked by re-baselining embed_idle→8020 / embed_teardown→7900 and widening tolerance 5%→10%. `cargo test -p memcheck` 10/0. **Step-3 empirical baseline capture DONE (2026-08-03, user-directed)**: `cargo xtask memcheck --update-baseline` measured all 9 phases for real (worker-active phases resolve the merged worker) — idle_warm 8900 · idle_post 8912 · idle_final 8656 · embed_idle 8684 · embed_teardown 9104 · embed_active main 20240 / worker 2594916 / combined 2615156 KiB; re-gate **ALL PHASES PASSED ✓** · memcheck 10/0 · `rebaselined_note` justification restored on changed baselines. Remaining step-3 sub-item: add the `mcp-idle-proliferation` / `worker-ort-threads` / `stale-artifacts` phases and wire them into `harness_integration.rs` (feature deps T2/T5/T7 landed).
- **Goal:** harness resolves the merged worker binary; the 4 new phases run and assert.
- **Steps:** (1) ✅ fix `tools/memcheck/src/main.rs` resolution for `-p leindex --bin leindex-embed` layout; (2) ✅ resolve the `u64::MAX` sentinels in `workload.rs` (root-cause the sentinel firing — previously masked by the missing binary); (3) ⏳ add `mcp-idle-proliferation`, `worker-ort-threads`, `stale-artifacts` phases (deferred to T5/T7 batch); (4) ⏳ wire into `harness_integration.rs`.
- **Acceptance:** `cargo test -p memcheck` green on the merged tree; phases emit the empirical RSS/swap numbers recorded in the tracker.
- **Files:** `tools/memcheck/**`, `tools/xtask`.

### T9 — Docs (D-9, the memory guide + runbook)
- **Goal:** operators can act on a thrashing box.
- **Steps:** rewrite the stale `docs/MEMORY_MANAGEMENT.md` (it documents a v2.0 Python-era memory manager that does not match the Rust codebase — mark the mismatch and document the REAL lifecycle: MCP idle exit, engine eviction, worker threads/RSS caps, `leindex cleanup --stale-daemons`, `--max-memory`); add `docs/RUNBOOKS.md` section "System under swap pressure"; note the 51 GiB-index observation and index-size guidance (`leindex index --max-memory`, split large repos).
- **Acceptance:** docs reflect actual code; AGENTS.md command-validation passes.
- **Files:** `docs/MEMORY_MANAGEMENT.md`, `docs/RUNBOOKS.md`, `docs/CLI.md`.

### T10 — Integration + end-to-end validation
- **Goal:** all pieces behave together under a synthetic thrash scenario.
- **Steps:** (1) full gate (workspace fmt/clippy/tests + onnx suite + memcheck suite); (2) manual scenario on the box (with user consent + `--mcp-idle-timeout-secs=60`): spawn 2 MCP servers on the same project, idle 90s, assert: both exit; worker thread-cap RSS delta recorded; stale sidecars swept; no orphan `leindex*` left after parents exit; (3) record before/after swap pressure.
- **Acceptance:** process count returns to 0; RSS/swap deltas captured in tracker; zero regressions in the full gate.
- **Files:** none new (orchestration + evidence).

### T11 — Cross-review + PR inclusion
- **Goal:** changes are mutually reviewed and land in PR #51 (or the agreed branch).
- **Steps:** (1) Agent B implements T1–T8 (+T9); GrayHill reviews each commit batch (reciprocal, per the standing protocol); (2) Codex re-review on the pushed head **after a delay** (per the logged "header ≠ done" lesson); (3) incorporate into PR #51 (or the 1.11.0 branch per maintainer decision); (4) update this plan + tracker checkboxes.
- **Acceptance:** mutual sign-off; PR green; tracker fully checked.

## 3.1 Dependency graph

```
T1 (config) ─┬─> T2 (mcp idle) ─┐
             ├─> T3 (engine evict) ─┼─> T10 (integration) ─> T11 (PR)
             ├─> T4 (lock) ────────┘
             └─> T5 (ort threads) ─> T6 (mem guards) ─> T7 (gc)
T8 (memcheck) can start in parallel after T1 (needs the env knobs); fixes binary resolution first.
T9 (docs) anytime after T1. T10 is the final gate.
```

## 4. Empirical measurement protocol (the "never claim without data" rule)

Every claim below must be captured on the box before/after, recorded in the tracker:
1. **Baseline (already captured):** `free -h`; per-process RSS/swap/VSZ for the leindex family (§2 of the findings doc).
2. **Worker thread-cap delta:** same 500-batch synthetic workload under `LEINDEX_WORKER_ORT_THREADS ∈ {0, 4, 1}` → report worker RSS + swap after settle (2 min idle), via memcheck phase. Target: cap=4 RSS < cap=0 RSS; record the delta.
3. **MCP idle-exit:** `--mcp-idle-timeout-secs=30`, idle 60s → process exits 0; `pgrep -f 'leindex mcp'` count returns to 0 after all idle.
4. **Engine eviction:** load 51 GiB-project engine, idle past `engine_max_idle`, sample RSS before/after eviction (expect multi-GB release; the engine's mmap Drop must return RSS — verify with `/proc/<pid>/status` VmRSS).
5. **Post-remediation system state:** `free -h` with 8 prior-style instances spawned then idled → assert swap stays well under capacity and process count self-heals to 0.
6. Failure to reproduce a target improvement is itself a finding — record it, do not paper over it.

## 5. Territory / coordination map

| Area | Primary owner | Cross-review |
|------|--------------|--------------|
| MCP lifecycle (T2–T4) | Agent B | GrayHill |
| Worker ORT/guards (T5–T6) | Agent B (with GrayHill input on runtime.rs — embed-merge territory) | GrayHill |
| Daemon GC (T7) | Agent B | GrayHill |
| memcheck (T8) | Agent B | GrayHill |
| Config + docs (T1, T9) | Agent B | GrayHill |
| PR inclusion (T11) | Joint | Codex re-review |

**Sequencing rule (standing protocol):** Agent B implements in isolated commits; GrayHill reviews each batch before push; NO push until mutual sign-off; Codex re-check after a delay on the pushed head.

## 6. Risks & mitigations

| Risk | Likelihood | Mitigation |
|------|-----------|------------|
| MCP idle exit causes a cold-reload stall mid-agent-task | Low (exit only between requests, ≥30min idle) | D-1 only fires after a long quiet window; make default 1800s; document `0=off` |
| Engine eviction drops an mmap while a query is mid-flight | Low | `active_calls` counter gates eviction (D-2) |
| Single-instance lock wrongly blocks two legit servers | Low | Lock is per canonical project path; different paths unaffected; stale-PID steal (D-3) |
| Worker refuses to start under pressure → silent TF-IDF degrade | Medium | Logged `failed` status + `leindex setup check` diagnostics; configurable threshold; default only fires at <2 GiB available |
| Thread cap slows embedding on small batches | Low-Med | Memory-bound workload; benchmark in T8 and keep cap tunable; document trade-off |
| `current_rss_mb` feature-coupling between cli and embed | Medium | Shared `src/memutil.rs` gated `#[cfg(any(feature="cli", feature="onnx"))]` (T6 step 1) |
| memcheck sentinels fire again / harness flaky | Medium | Root-cause sentinels in T8 step 2 before adding phases; keep phases time-bounded |

## 7. Out of scope (deliberately)

- Changing the 51 GiB index format or mmap strategy (separate plan).
- Worker GPU/ROCm arena tuning beyond thread caps (needs MIGraphX profiling on the user's box; documented as a follow-up).
- Agent-side hygiene (claude/codex configs that reuse a shared socket server) — user/ops territory, documented in RUNBOOK.
