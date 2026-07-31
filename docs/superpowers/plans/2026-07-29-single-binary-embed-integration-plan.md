# Implementation Plan — Single-Binary Embed Integration

> Status: **DRAFT for review — not yet implemented.** This plan operationalizes the
> architecture recommendation recorded in `TASKLIST.md` (Active remediation, line 43).
> No code changes are made until this plan is approved.

## 1. Goal

Ship **one executable (`leindex`)** that hosts both the CLI/MCP host and the ONNX
embed worker. The separately-published `leindex-embed` executable is removed; the
worker is reached by `leindex` **re-executing itself in a hidden worker mode**.

End state:
- `cargo install leindex --features onnx` installs exactly one binary (`leindex`).
- `leindex` spawns its worker by re-invoking its own `current_exe()` with a hidden
  mode token, so there is no orphan-subprocess assumption and no second binary to
  discover on `PATH`.
- Every current worker isolation semantic is preserved (§4.4).

## 2. Current Architecture (verified)

- Root `Cargo.toml` declares two `[[bin]]` targets: `leindex` (`src/bin/leindex.rs`)
  and `leindex-embed` (`src/bin/leindex-embed.rs` — the cargo-install wrapper).
- The `crates/leindex-embed` crate declares its own `[[bin]] leindex-embed`
  (`crates/leindex-embed/src/bin/leindex-embed.rs`).
- All three worker entry points converge on `worker_main::run() -> !`
  (`crates/leindex-embed/src/worker_main.rs:42`), which:
  - handles `--version`/`-V` (prints `leindex-embed <ver>`, `VAL-CARGO-005`),
  - parses `--socket <path>` (`parse_socket_arg`),
  - installs `PR_SET_PDEATHSIG` (Linux) *before* any allocation,
  - branches to stdio worker (`run()`) or socket daemon (`run_socket_worker`).
- The ONNX client discovers the worker binary via
  `resolve_worker_binary()` (`src/search/onnx/client_config.rs:84`): looks for
  `leindex-embed` beside `current_exe()` (and its parent dir), then falls back to
  `which::which("leindex-embed")`.
- The client spawns it with `Command::new(worker_path)` in two modes:
  `spawn_pipe_worker` (`client.rs:439`) and `spawn_locked_daemon` (`client.rs:517`).

## 3. Target Architecture

```
            ┌──────────────────────── leindex (single binary) ───────────────────────┐
  argv[1]   │                                                                         │
  = normal  │  main() ──► clap ──► CLI / MCP host                                     │
            │                                                                         │
  argv[1]   │  main() ──► (hidden sniff) ──► worker_main::run()  ◄── re-exec of self  │
  = hidden  │                                  (no clap, no logging-init conflict)    │
            └─────────────────────────────────────────────────────────────────────────┘
```

- **Hidden worker mode**: before clap/setup, `main()` inspects `argv`. If the first
  real arg is an internal token (e.g. `--internal-embed-worker`) **or** the env var
  `LEINDEX_INTERNAL_WORKER=1` is set, `main()` calls `worker_main::run()` directly and
  never reaches clap. The token is undocumented and not advertised in `--help`.
- **Self-spawn**: `resolve_worker_binary()` returns `std::env::current_exe()` (the
  `leindex` binary itself). The spawn command appends the hidden worker-mode token
  plus the existing `--socket`/stdio args.
- **Distinct process name**: the worker child calls `prctl(PR_SET_NAME, b"leindex-embed")`
  (Linux) at the top of `worker_main::run()` so `ps`/monitoring/logs still identify it
  as the embed worker even though the executable is `leindex`.

## 4. Detailed Changes (file-by-file)

### 4.1 Worker-mode entry wiring
- **`src/bin/leindex.rs`** (the `leindex` main): before any async/clap setup, add:
  ```rust
  fn main() {
      // Hidden internal worker mode: re-exec of self to host the embed worker.
      if worker_main::is_internal_worker_invocation() {
          worker_main::run(); // -> !
      }
      // ... existing leindex main (clap, telemetry, dispatch) unchanged ...
  }
  ```
- **`crates/leindex-embed/src/worker_main.rs`**: add
  `pub fn is_internal_worker_invocation() -> bool` that returns true when
  `std::env::args().nth(1) == Some("--internal-embed-worker".into())` **or**
  `std::env::var("LEINDEX_INTERNAL_WORKER").ok().as_deref() == Some("1")`.
  Existing `run()` body is unchanged (still handles `--version`, `--socket`,
  PDEATHSIG, stdio/socket dispatch). **Preserve**: the `--version` path must still
  print `leindex-embed <ver>` for `VAL-CARGO-005` (release-bundle verification
  scripts call the worker with `--version`).

### 4.2 Worker discovery + spawn
- **`src/search/onnx/client_config.rs` `resolve_worker_binary()`**: replace the
  sibling/`which` lookup with `std::env::current_exe()` (the `leindex` binary),
  preserving the existing `Result<PathBuf, io::Error>` signature and the
  "not found" error wording (update message text to reference `leindex`).
- **`src/search/onnx/client.rs`** `spawn_pipe_worker` / `spawn_locked_daemon` /
  `configure_worker_command`: prepend/append the hidden worker-mode token to the
  spawned `Command` args so the re-executed `leindex` enters worker mode. Concretely,
  the `Command::new(worker_path)` args become
  `["--internal-embed-worker", "--socket", <path>]` (socket mode) or
  `["--internal-embed-worker"]` (stdio mode). No other spawn semantics change.

### 4.3 Binary target + packaging removal
- **Root `Cargo.toml`**: delete the `[[bin]] name="leindex-embed"` block
  (`src/bin/leindex-embed.rs`). Keep `[[bin]] leindex`.
- **`src/bin/leindex-embed.rs`**: delete (was the cargo-install wrapper).
- **`crates/leindex-embed/Cargo.toml`**: delete its `[[bin]] leindex-embed` block.
  The crate becomes a **library only** (still a workspace dep of the root, gated by
  the `onnx` feature). `crates/leindex-embed/src/bin/leindex-embed.rs` is deleted.
- **Version parity**: keep `crates/leindex-embed` `version` in lockstep with the
  root crate (AGENTS version-parity rule) — the crate still exists as a lib.

### 4.4 Isolation semantics to preserve (verification checklist)
Each must remain green after the change:
- `PR_SET_PDEATHSIG` (Linux) installed before any allocation in the worker child —
  orphan prevention on parent SIGKILL/crash/OOM. (`worker_main.rs`, unchanged.)
- `setsid()` for socket/daemon workers (detach from parent process group).
- Idle-cleanup timeout (`run_socket_worker` idle expiry).
- Bind-before-init health: socket listener bound + status file written **before**
  ORT/model init so health probes answer during the multi-minute MIGraphX compile.
- Resident sessions + socket sharing across batches.
- Crash/OOM isolation: worker is a separate **process** (re-exec of self preserves
  this — it is still a distinct OS process, only the executable path changes).
- Explicit worker override: keep any `LEINDEX_EMBED_DAEMON` / explicit-path override
  (`client_config.rs:3`) working — if set, it should still point at an explicit
  executable (now typically `leindex`).
- `current_exe()` fallback: the new `resolve_worker_binary` is *itself* current_exe,
  so the "fall back to PATH" branch is only needed if `current_exe()` fails (keep a
  `which::which("leindex")` fallback for that edge case).
- Distinct process name via `prctl(PR_SET_NAME)` (new, §3).

### 4.5 Docs / installers / release
- **README** (root + `packages/pypi-leindex` + `packages/npm-leindex-mcp`): remove
  references to a separate `leindex-embed` binary; state one binary.
- **Installer scripts** (`scripts/`, `install.sh` etc.): stop expecting/installing a
  second binary; the release bundle ships one binary.
- **`.github/workflows/release.yml`**: drop the `leindex-embed` binary from the
  cross-platform build matrix + SHA256 checksum list + GitHub Release assets. Update
  the `VAL-RELEASE-002`/`VAL-CARGO-005` verification step to invoke
  `leindex --internal-embed-worker --version` (or keep a thin `--version` passthrough).
- **`.github/workflows/docs.yml`** AGENTS-command validator: update any command that
  referenced `leindex-embed`.
- **`xtask`** (if present): update any binary-packaging task.
- **PyPI bootstrap / npm**: remove second-binary expectations.
- **`tools/memcheck`**: update fixtures/workload that assumed two binaries
  (`run_workload` / the memcheck harness referenced `leindex-embed`).

### 4.6 Tests
- Existing worker tests (`crates/leindex-embed/tests/*`, `tools/memcheck/tests/*`)
  currently spawn `leindex-embed`; repoint them to spawn `leindex --internal-embed-worker`.
- The onnx-client tests that exercise `resolve_worker_binary`/spawn must assert the
  worker is `current_exe()` + hidden-mode token.
- Keep a regression test that the worker still prints `leindex-embed <ver>` on
  `--version` (release verification contract).

## 5. Migration Sequence (one PR, ordered)

1. **Add the hidden-mode plumbing (non-breaking):**
   - `worker_main::is_internal_worker_invocation()` + call from `src/bin/leindex.rs`.
   - `prctl(PR_SET_NAME)` in `worker_main::run()`.
   - Switch `resolve_worker_binary` → `current_exe()` + spawn token.
   - At this point `leindex` can self-host the worker, but `leindex-embed` still exists.
2. **Repoint tests + installers/docs to the self-spawn path** (still both binaries present).
3. **Remove the `leindex-embed` binary targets** (`[[bin]]` blocks + `src/bin/leindex-embed.rs`
   in root and crate). Crate becomes lib-only.
4. **Release pipeline + checksums + verification scripts** updated to one binary.
5. **Full gate run** (§6).

Steps 1–2 are independently revertible; step 3 is the point of no return for the
external binary contract.

## 6. Verification

- `cargo build --release --features onnx` produces exactly one binary in
  `target/release/leindex` (no `leindex-embed`).
- `cargo test --workspace --features onnx` (once the pre-existing `ort`/`runtime.rs`
  build break is resolved — tracked separately) passes the worker/spawn/idle/teardown
  suite via the self-spawn path.
- Manual: `leindex index` on an onnx-enabled project spawns a child `leindex`
  process whose `comm` is `leindex-embed` (verify via `/proc/<pid>/comm`), the child
  dies when the parent is SIGKILL'd (PDEATHSIG), and idle teardown still fires.
- `leindex --internal-embed-worker --version` prints `leindex-embed <ver>`.
- Quality gates: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets
  -- -D warnings`, `lizard -C 15`, large-file check, jscpd — all unchanged/green by
  this refactor (it deletes code, does not add files).

## 7. Risks & Mitigations

| Risk | Mitigation |
|---|---|
| `current_exe()` is unreliable on some platforms (symlink, /proc) | Keep `which("leindex")` PATH fallback; the hidden-mode token is what selects worker behavior, not the path. |
| Hidden arg leaking into `--help` / confusing users | Token is undocumented; clap never sees it (`main` short-circuits before clap). |
| Loss of the `leindex-embed --version` release contract | Keep `worker_main::run()`'s `--version` handler; verification invokes the hidden-mode `--version`. |
| Monitoring/log tooling expecting a `leindex-embed` process | `prctl(PR_SET_NAME)` keeps the child's visible name as `leindex-embed`. |
| Memcheck/PYPI/npm harnesses hard-coding two binaries | §4.5/§4.6 repoint them in the same PR. |
| Pre-existing `ort` onnx-feature build break blocks verification | That breakage (`runtime.rs`/`ort` Tensor generics) is **out of scope** and tracked separately; this plan's compile check uses the default build + a targeted onnx build once the ort issue is fixed. |

## 8. Out of Scope

- The pre-existing onnx-feature build breakage (`runtime.rs`/`ort`) — separate task.
- The jscpd <5% gate, test-convention renames, and the in-flight `graph/extraction.rs`
  call-resolution regression — separate tasks, unaffected by this change.
- Neural embedding/reranking quality changes — none; only the process boundary moves.

## 9. Open Questions for Reviewer

1. **Hidden-mode mechanism**: prefer the undocumented CLI token
   (`--internal-embed-worker`), the env var (`LEINDEX_INTERNAL_WORKER=1`), or both?
   (Plan assumes both, token taking precedence.)
2. **Keep the `crates/leindex-embed` crate as a lib**, or inline its sources into the
   root crate and delete the crate entirely? (Plan assumes keep-as-lib — smaller diff,
   preserves the `onnx` feature gate boundary.)
3. **Process name**: is `prctl(PR_SET_NAME, "leindex-embed")` acceptable, or should the
   child advertise `leindex:embed` / similar?
4. Should the explicit-worker-override env (`LEINDEX_EMBED_DAEMON`) be retired now that
   the worker is always self, or kept as an escape hatch for debugging?
