# B — Daemon lifecycle: Issue #96 (readiness stall) & Issue #98 (cleanup ownership)

Branch under test: `fix/backlog-86-103-rootcause` @ `f31f107e` (worktree `/mnt/WD-SSD/Prod/LeIndex-issues`).
Analysis method: LeIndex MCP (`leindex_explore`) read-only; no code changes. Verdicts are based on the code actually read; no runtime reproduction of the stall was attempted on this machine (GPU contention, no clean ORT-less fixture).

---

## Issue #96 — memcheck index/reindex stall on ORT-less runners

**Verdict:** CONFIRMED (with one precision: the 240s figure is the *sum of two phase deadlines*, not two deadlines inside a single command; and the burn is conditional — the code also contains a fast-fail path that can win the race).

**Evidence (verified by reading):**

- Constants, `src/search/onnx/client_config.rs`:
  - `62: pub(super) const DAEMON_BIND_WAIT_SECS: u64 = 2;`
  - `68: pub(super) const DAEMON_HEALTH_WAIT: Duration = Duration::from_millis(250);`
  - `71: pub(super) const DAEMON_READINESS_POLL: Duration = Duration::from_millis(25);`
  - `84: pub(super) const DAEMON_READY_MAX_WAIT: Duration = Duration::from_secs(120);` → **120 s per wait site**. `2 × 120 s = 240 s`.
- Two wait sites, both gated on `DAEMON_READY_MAX_WAIT`:
  - `src/search/onnx/client.rs:346` (in `ensure_worker_ready`, outer loop): polls `self.availability()`; on `Initializing` + daemon mode, checks `started.elapsed() >= DAEMON_READY_MAX_WAIT` → warn + `return Ok(())` (soft fallback); sleeps `DAEMON_READINESS_POLL` (25 ms) per iteration otherwise.
  - `src/search/onnx/client.rs:752` (in `wait_for_daemon_ready`): same 120 s deadline → `return Err(ClientError::Worker(WorkerError { kind: ErrorKind::OnnxRuntime, ... }))` (hard error); poll interval again `DAEMON_READINESS_POLL` (client.rs:779/803/809/814/817). The hard error propagates to `spawn_locked_daemon` (`client.rs:646-652`), which kills the child, cleans socket paths, and returns the error; `ensure_worker_ready` then swallows it (`client.rs:330-336`) and reports neural unavailable. Net effect: the *process survives* (TF-IDF fallback preserved), but 120 s of wall clock were burned per trigger.
- Where 240 s comes from in memcheck: `tools/memcheck/src/workload.rs:68-90` (`CANONICAL_PHASES`) contains both `"index"` and `"reindex"`; each phase runs a **fresh** `leindex` process (`tools/memcheck/src/workload.rs:211-213` runs `leindex index <fixture> --force` for reindex; module doc at lines 4-6 “Each phase launches a fresh leindex process”). Each fresh process can burn one 120 s deadline → **240 s across the two indexing phases**. The current (extended) phase list also contains `incremental_*` and `full_index_run2/3`, so the stall compounds beyond 240 s on the full list.
- Does the daemon ever become Ready without ORT? No (onnx build): `WorkerRuntime::is_neural_ready()` = `session.is_some() && tokenizer.is_some()` (`src/embed/runtime.rs`, `is_neural_ready`); `init_onnx` returns `(None, None, …)` immediately when `discover_and_init()` is `NotFound` (`src/embed/runtime/onnx_session.rs:34-46`); `spawn_runtime_init` then transitions to `Failed` and writes status `failed` (`src/embed/worker_main.rs:502-508`). So the Ready state is unreachable without ORT — any wait for it is a dead path by construction.
- The root-cause mechanism (why the wait actually burns the full deadline despite the worker “failing fast”):
  1. Daemon binds socket + writes status `initializing` **before** ORT/model init (`src/embed/worker_main.rs:276-285, 359, 380-381`), so the client’s first connect usually succeeds and the client enters `wait_for_daemon_ready`.
  2. On init failure, `lifecycle.set_failed(...)` then the accept loop sees `lifecycle.is_failed()` and **shuts the whole daemon down**, unlinking socket/pid/status (`src/embed/worker_main.rs:405-408`, `shutdown_worker` at 519-532).
  3. `wait_for_daemon_ready` only watches the socket (`client.rs:747-820`); it has **no child/pid liveness check**. After the daemon self-terminates, every subsequent connect fails `NotFound`/`ConnectionRefused` → sleeps 25 ms → retries → **until the 120 s deadline**. The transient `Failed` health frame is only observable in a tiny window before teardown; if the client misses it, it blind-waits the full 120 s. (The comment at `client_config.rs:82-83` claims “a genuinely broken worker still fails fast (init reports Failed immediately rather than stalling to this deadline)” — the code path above shows the Failed report and the daemon’s death race; the client loses the race whenever it is not mid-poll on the socket during that instant.)
  4. There is also a fast-fail path: if the daemon dies *before the client’s first successful connect*, `spawn_locked_daemon`’s bind loop detects the exit via `child.try_wait()` and errors quickly (`client.rs:671-679`). So per phase the outcome is a race — observed 240 s implies both index and reindex lost it.
- ORT availability check before the wait? No. `EmbeddingClient::new()` (`client_config.rs`) just constructs the handle; NOTHING consults `ort_discovery` before `ensure_worker_ready` spawns/wait (clients `spawn_worker` → `spawn_or_connect_daemon` → `wait_for_daemon_ready` unconditionally). `ort_discovery::discover_path_only()` exists and is cheap (stat-only chain: env → config → `~/.leindex/lib` → sibling → pip filesystem scan → system; `src/embed/ort_discovery.rs`, `discover_path_only`, `discover_candidates`, `discover_pip_lib_filesystem`; note `discover_pip_lib` uses a Python subprocess only as last resort) and is already used by diagnostics (`src/cli/cli.rs:1405`) and setup (`src/cli/leindex/setup_ort.rs`). The embedding client just doesn’t call it.

**True root cause:** The readiness gate’s failure handling is asynchronous and lossy. The daemon converts “neural unavailable” into *self-termination and socket unlink* instead of keeping the socket alive to serve a `Failed` health frame, while the client’s wait loop treats “socket gone” as “keep retrying”, not as “terminal”. Combined with no pre-spawn ORT preflight, an ORT-less environment pays the full 120 s deadline per daemon spawn, and memcheck’s canonical workload has two indexing phases → 240 s. The suspect areas listed in the issue are partially right: `client_config.rs` holds the constants, but the lossy wait lives in `client.rs` (`ensure_worker_ready` / `wait_for_daemon_ready`) and the teardown-on-failed lives in `worker_main.rs`; `load.rs` is only a pass-through (it upgrades the embedder to `hybrid_local` at `load.rs:467` but never waits itself).

**Minimal correct fix:** Two complementary, small changes (client-side; no worker/persistence changes):
1. **Fail fast before spawning** when ORT is provably absent: in `EmbeddingClient::ensure_worker_ready` (or at `ensure_worker`), first call `crate::embed::ort_discovery::discover_path_only()`; if `None`, log info + `return Ok(())` (neural unavailable) without spawning or waiting. This kills the 240 s path entirely on ORT-less runners. Keep status worker-side failure handling (daemon teardown) unchanged.
2. **Make the wait lossless**: in `wait_for_daemon_ready` (client.rs:747), when a reconnect fails with `NotFound`/`ConnectionRefused` **after the first successful connect**, treat a vanished daemon as terminal and return the same `Worker(OnnxRuntime)` error immediately instead of retrying to the deadline (or: thread the spawned child into the wait and poll `child.try_wait()`; a dead child ⇒ terminal). This removes the dead-path burn even when the preflight misses an exotic install.
- Note on the preflight: use `discover_path_only()` (path stats), **never** `discover_and_init()` in the main CLI process (it dlopens; diagnostics already enforce “do not load ORT into main process”, see `tests/cross_area_validation_test.rs` “diagnostics_does_not_load_ort_into_main_process”). Verify `discover_path_only()` covers the same candidate space as `discover_and_init()` (notably the final bare-name loader fallback at `ort_discovery.rs:779-787`, which `discover_path_only()` does not perform) before gating on it, or the preflight can false-negative a system-loader-only install.

**Failing test to add:**
- Module `src/search/onnx/client_test.rs` (or a new `daemon_readiness_test.rs`):
  1. `test_wait_for_daemon_ready_aborts_when_daemon_vanishes`: open a `UnixStream::pair()`-style fake server that accepts once and immediately closes/exits; assert `wait_for_daemon_ready` returns `Err` in **< 5 s**. Today it loops ~120 s → FAILS today, passes after fix 2.
  2. `test_ensure_worker_ready_skips_spawn_when_ort_absent`: set `LEINDEX_HOME` to an empty temp dir, unset `ORT_DYLIB_PATH` (and no config/sibling libs); assert `ensure_worker_ready()` returns `Ok(())` in **< 2 s** and the worker handle stays `None` (no child spawned). Today it spawns a daemon (and either fails fast or burns 120 s) → FAILS today, passes after fix 1.

**Blast radius / risks:**
- Fix 1 changes the neural fallback decision for every ORT-less deployment: TF-IDF-only results (same as the 120 s-timeout outcome today), but instantly. Mount touch `src/search/onnx/client.rs` / `client_config.rs`; all neural entry points (`hybrid_local`, `neural_status`, `embed_with_retry`) inherit it. No persistence/on-disk format impact.
- Risk: preflight false-negative for exotic-but-working ORT installs (bare-name loader fallback; unusual `ORT_DYLIB_PATH`-equivalent setups) — mitigate by keeping fix 2 so even a wrong preflight cannot regress to a *stall* (only to a failed attempt).
- Fix 2 narrows the wait; it can only speed up the failure path. No cross-platform risk beyond Unix-socket semantics (the code is already `#[cfg(unix)]`).
- Feature gating: `discover_path_only()` is `#[cfg(feature="onnx")]`-gated internally; the reference code must keep the non-onnx build compiling (in non-onnx builds `is_neural_ready()` is trivially true — `runtime.rs` — so daemons report Ready instantly and there is no stall; fix 1 should be `cfg(feature="onnx")`-scoped or a no-op when the feature is off).

**Open questions:**
- Exact in-field 240 s vs 120 s/480 s depends on whether both phases lose the race and how many embed triggers fire per process (`embed_attempt_shard` calls `ensure_worker_ready()` per attempt — `client.rs:1538` — so repeated triggers can burn multiple 120 s windows in one process; 240 s is a floor, not a ceiling, on the current extended phase list). Unverified here without an ORT-less memcheck run; the mechanism is code-verified.
- `DAEMON_LOCK_WAIT_SECS` (used at `client.rs:577`) was not quoted — not part of the stall path; left unread.

---

## Issue #98 — daemon cleanup falsely recognizes unrelated processes via arbitrary mcp/leindex arguments

**Verdict:** CONFIRMED (the predicate is exactly a per-argument substring match; the consequence currently possible is the **false-PROTECT** direction only — the false-KILL direction is blocked by separate, stronger identity checks).

**Evidence (verified by reading):**

- Exact predicate, `src/cli/cleanup.rs:361-367` (`pid_is_alive`, Linux):
  ```rust
  let cmdline = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
  Some(cmdline.is_some_and(|raw| {
      let command = String::from_utf8_lossy(&raw);
      command
          .split('\0')
          .any(|arg| arg.contains("leindex") || arg.contains("mcp"))
  }))
  ```
  Match semantics: per argv element (split on NUL), **raw substring containment** — not equality, not executable basename, not `comm`. A process whose argv is `["/bin/echo", "leindex-study"]` or `["vim", "docs/mcp.md"]` matches.
- Same flawed substring predicate is duplicated in `src/cli/mcp/lock.rs:234-240` (`pid_is_owned`, secondary sanity after start-time match). `client_config.rs::daemon_pid_is_owned` (454-493) uses a different, stronger chain: start-time sidecar match + cmdline arg contains `"leindex-embed"` + `/proc/<pid>/status` euid == geteuid. `terminate_superseded_daemons` (client_config.rs:391-451) uses `/proc/<pid>/comm` prefix `leindex-embed` (+ `kill(pid,0)` liveness) before `SIGTERM`.
- Callers of the loose `pid_is_alive` (all **protection** gates, none signal):
  - `cleanup.rs::stem_liveness` (547-566): a live pid protects the whole `~/.leindex/run/` stem from sweeping;
  - `cleanup.rs::is_daemon_endpoint_stale` (507-535): `pid_is_alive == Some(true)` ⇒ `daemon.endpoint` sidecar kept;
  - `mcp/lock.rs::try_acquire_in_dir_linux` (≈108/124): `pid_is_owned` prevents stealing a lock from a “live” owner.
- The test in `cleanup.rs:1501` even documents the flaw: “the test runner process contains ‘leindex’ in its args, so this should pass … test runners are not named leindex, so on Linux the PID check will fail” — i.e. the substring check is doing the job of an identity check.

**True root cause:** Ownership is being inferred from *argument content* (`contains("leindex") || contains("mcp")` on individual argv elements) instead of from *process identity* (executable path, `/proc/<pid>/exe`, process start time vs recorded `.start` sidecar, invocation markers). Any unrelated live process (editor/grep/shell wrapper whose argv merely mentions those words, or the memcheck harness runner itself) that occupies a recycled PID makes the sidecar/lock owner look alive forever.

**Consequences — quantified by direction:**
- **False-PROTECT (stale artifacts survive): possible today.** If a live unrelated process has a matching substring arg AND is the PID recorded in a stale `.pid`/endpoint sidecar, `pid_is_alive` returns `Some(true)` ⇒ stale sidecars are never swept (and, where start-time matches or via `pid_is_owned` in lock.rs, a dead daemon’s lock is not stolen). This is the direction the issue reports; severity is low (disk litter, stale lock warnings), not data loss.
- **False-KILL (destroying an unrelated process): not reachable via this bug.** `cleanup` never signals processes (no `kill`/`SIGTERM`/`SIGKILL` anywhere in `src/cli/cleanup.rs` — verified by search). The two places that do signal (`kill_stale_daemon_by_pid` — client_config.rs:511-551 — and `terminate_superseded_daemons` — client_config.rs:391-451) gate on the much stronger `daemon_pid_is_owned` / `comm`-prefix checks; they do not use the loose `mcp`/`leindex` substring, do they additionally require start-time and/or uid agreement. So a recycled PID cannot be signalled unless it also matches the recorded start time (effectively impossible for a recycled PID) — the kill path is safe.

**Minimal correct fix:** Replace the substring check in `cleanup.rs::pid_is_alive` (and mirror in `mcp/lock.rs::pid_is_owned`) with an identity check:
1. Resolve `/proc/<pid>/exe` (readlink) and require the basename to match a known LeIndex binary set (`leindex`, `leindex-embed`, `leindexd`, or the resolved `std::env::current_exe()` file name of the running process), **plus**
2. When a `.start` sidecar exists for the stem, require `/proc/<pid>/stat` start time (field 22) to equal the sidecar value (this is the strongest PID-recycling guard; already implemented as `proc_start_time` in `lock.rs:244-248` and inside `daemon_pid_is_owned`), and/or require daemon invocation markers (`--socket <path under ~/.leindex/run/>` + `INTERNAL_WORKER_TOKEN`).
Suggested home for the shared predicate: a single helper (e.g. in `src/cli/cleanup.rs` exported `pub(crate)`, or the existing `client_config.rs` family) used by `pid_is_alive`, `lock::pid_is_owned`, and optionally `daemon_pid_is_owned` so the three copies converge. This is a pure protection-logic fix; no signal paths, on-disk formats, or generation/manifest layouts change.

**Failing test to add:**
- Module `src/cli/cleanup.rs` tests (Linux-gated): spawn a real child whose executable is *not* a LeIndex binary but whose argv contains the words, e.g. `Command::new("/bin/sleep").arg("leindex-999").arg("mcp")`; assert `pid_is_alive(child.id()) == Some(false)`. Today `Some(true)` → FAILS today, passes after the fix. Add the lock.rs twin: after writing a matching `.start` sidecar for an unrelated process, `pid_is_owned` must be `false`.

**Blast radius / risks:**
- The fix only changes *when* sidecars are protected/stolen; it can only make cleanup more aggressive against non-daemon PIDs. Risk: if the exe check is too strict (e.g. symlinked installs, `busybox`-style names, mac/Windows non-Linux fallbacks), live daemons could be misclassified dead and their sidecars swept — preserve the existing non-Linux fallback (`None` → mtime) and keep the `.start` check as the primary gate, with the exe check as secondary. Feature/portability: `/proc/<pid>/exe` is Linux-only, same as the current code’s cfg.
- All four call sites share the same correction direction; no persistence format impact.

---

## Cross-cutting invariant

Both issues are instances of one missing contract: **daemon/worker sidecar ownership must be decided by process identity, never by argument content**. The invariant to formalize:

> A recorded PID `P` (from a `.pid`, `.start`, or `daemon.endpoint` sidecar) owns that sidecar set iff:
> 1. `P` exists in `/proc` (or `kill(P,0)` succeeds), and
> 2. `P`’s `/proc/<pid>/stat` start time equals the `.start` sidecar value when one exists, and
> 3. `P`’s executable identity (`/proc/<pid>/exe` basename, or `comm` prefix) resolves to a LeIndex binary, and
> 4. if available, the invocation markers match (`INTERNAL_WORKER_TOKEN`, `--socket` under `~/.leindex/run/`).

Where it should live: a single shared `pub(crate)` helper (e.g. `src/cli/cleanup.rs` or a new `src/cli/daemon/ownership.rs`), consumed by `cleanup::pid_is_alive`, `mcp/lock::pid_is_owned`, `client_config::daemon_pid_is_owned`, and `terminate_superseded_daemons` so the four predicates cannot drift again. The wait-loop invariant from #96 (`readiness wait must terminate on daemon death, not on timeout alone`) belongs next to `DAEMON_READY_MAX_WAIT` in `client_config.rs` as a documented contract for the next reader.
