# WS3: User-Scoped Daemon + Stdio Shim

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace the per-harness heavyweight `leindex --stdio` model with ONE user-scoped `leindexd` daemon + thin stdio shims that forward MCP/JSON-RPC frames to it. Preserve the GrayHill invariant (stdio process is 1:1 with the agent's pipe — never hard-exit the shim).

**Architecture:** A new `leindexd` binary wraps the *existing* `McpServer::run_socket` loop. A new feature-flagged shim path in `cmd_mcp_stdio_impl` discovers the daemon endpoint via a run-dir sidecar, connects a `UnixStream`, and forwards frames using the *existing* `read_socket_frame`/`write_socket_frame` helpers. A new hard single-winner startup lock ensures only one daemon spawns per user. A new handshake module carries protocol versions.

**Spec refs:** §4.1 (stdio shim), §4.2 (leindexd), §12.1 (protocol), §12.3 (rollout phase 3: daemon opt-in with legacy artifacts).

**Tech Stack:** Rust, tokio, clap, bincode (existing), std::os::unix::net.

**Existing infra (DO NOT reinvent):**
- `McpServer::run_socket` (`src/cli/mcp/server.rs:1213`) — UnixListener accept loop, `SocketCleanupGuard`, D-1 idle self-exit, D-2 engine eviction, `handle_socket_connection`. **This is the daemon core already.**
- `read_socket_frame` / `write_socket_frame` (`src/cli/mcp/server.rs:1350,1474`) — MCP/JSON-RPC framing over a stream. **This is the shim↔daemon transport.**
- `McpProjectLock` (`src/cli/mcp/lock.rs`) — run-dir sidecar pattern (`~/.leindex/run/...`), pid start-time liveness. **Pattern to reuse for endpoint discovery; advisory lock stays untouched.**
- `leindex cleanup --stale-daemons` (`src/cli/cleanup.rs`) — run-dir sweep.
- `McpServer`, `SERVER_STATE: OnceLock<Arc<ProjectRegistry>>`, `HANDLERS`, `ProcessIdleClock`.

**What does NOT exist (the actual work):** `leindexd` binary; shim forwarder path in `cmd_mcp_stdio_impl`; hard single-winner daemon startup lock; protocol-version handshake module.

**Non-negotiable invariants:**
- Shim never hard-exits (preserves GrayHill invariant, `mcp_commands.rs:68` comment).
- Daemon startup does NOT eagerly load any project or model (spec §4.2).
- Legacy `leindex --stdio` (full inline server) remains the default behind a feature flag; shim is opt-in (spec §12.3 phase 3).
- No retrieval behavior disabled, skipped, or shrunk (anti-cheat §2.1).

---

## Task 1: Daemon endpoint discovery + hard single-winner startup lock

**Why:** Spec §4.2: "One daemon runs per OS user." Need a hard lock so the first shim to need a daemon wins the spawn race; the rest connect. This is distinct from the advisory per-project `McpProjectLock`.

**Files:**
- Create: `src/cli/daemon/endpoint.rs` (endpoint discovery + liveness)
- Create: `src/cli/daemon/startup_lock.rs` (hard single-winner lock)
- Create: `src/cli/daemon/mod.rs`
- Modify: `src/cli/mod.rs` (add `pub mod daemon;`)

- [ ] **Step 1: Write failing test** (`src/cli/daemon/startup_lock.rs`)

```rust
//! Hard single-winner lock for daemon startup. Unlike `McpProjectLock`
//! (advisory, per-project), this lock has exactly one winner: the process
//! that acquires it owns the daemon role until its pid goes dead.

use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Sidecar contents: socket path + pid + start time + protocol version.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct DaemonEndpoint {
    pub socket_path: PathBuf,
    pub pid: u32,
    pub pid_start_time_ms: u64,
    pub protocol_version: u32,
    pub leindex_version: String,
}

/// Lock outcome for the daemon-startup race.
#[derive(Debug)]
pub enum StartupOutcome {
    /// This process won — it must spawn/serve the daemon.
    Won(DaemonEndpoint),
    /// Another live daemon already exists — connect to it.
    Connect(DaemonEndpoint),
}

pub fn resolve_endpoint(run_dir: &Path, protocol_version: u32) -> io::Result<StartupOutcome> {
    todo!()
}

#[cfg(test)]
mod test {
    use super::*;
    use std::fs;

    #[test]
    fn test_first_caller_wins_second_connects() {
        let dir = tempfile::tempdir().unwrap();
        // First resolution wins.
        let r1 = resolve_endpoint(dir.path(), 1).unwrap();
        let ep1 = match r1 { StartupOutcome::Won(ep) => ep, _ => panic!("expected Won") };
        // Simulate the daemon writing its sidecar (normally done at bind time).
        write_endpoint_sidecar(dir.path(), &ep1).unwrap();
        // Second resolution connects.
        let r2 = resolve_endpoint(dir.path(), 1).unwrap();
        assert!(matches!(r2, StartupOutcome::Connect(_)));
    }

    #[test]
    fn test_dead_pid_is_stolen() {
        let dir = tempfile::tempdir().unwrap();
        let ep = DaemonEndpoint {
            socket_path: dir.path().join("d.sock"),
            pid: 999_999, // almost certainly dead
            pid_start_time_ms: 0,
            protocol_version: 1,
            leindex_version: "test".into(),
        };
        write_endpoint_sidecar(dir.path(), &ep).unwrap();
        let r = resolve_endpoint(dir.path(), 1).unwrap();
        assert!(matches!(r, StartupOutcome::Won(_))); // stolen
    }

    fn write_endpoint_sidecar(dir: &Path, ep: &DaemonEndpoint) -> io::Result<()> {
        fs::write(dir.join("daemon.endpoint"), serde_json::to_vec(ep)?)
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test daemon::startup_lock`
Expected: FAIL (todo!() panic)

- [ ] **Step 3: Implement `resolve_endpoint`**

Read `~/.leindex/run/daemon.endpoint` (JSON `DaemonEndpoint`). If present: check liveness via `/proc/<pid>/stat` start time (reuse the pattern from `mcp/lock.rs::try_acquire_in_dir_linux`). If live and protocol version matches → `Connect`. If dead or version mismatch → steal (delete sidecar) → `Won`. If absent → `Won`. The winner writes the sidecar atomically *after* binding the socket (done in Task 3). Liveness check is Linux-only; macOS/Windows fall back to "pid exists" via `kill(pid, 0)`.

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test daemon::startup_lock`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add src/cli/daemon/
git commit -m "feat(daemon): add hard single-winner startup lock + endpoint discovery"
```

---

## Task 2: Protocol-version handshake module

**Why:** Spec §12.1: handshake carries LeIndex version, daemon protocol version, artifact format version, worker protocol version, capabilities. Needed before shim↔daemon comms.

**Files:**
- Create: `src/cli/daemon/handshake.rs`
- Modify: `src/cli/daemon/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
//! Protocol-version handshake (spec §12.1).
use serde::{Deserialize, Serialize};

pub const DAEMON_PROTOCOL_VERSION: u32 = 1;
pub const ARTIFACT_FORMAT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Handshake {
    pub leindex_version: String,
    pub daemon_protocol_version: u32,
    pub artifact_format_version: u32,
    pub worker_protocol_version: u32,
    pub capabilities: Vec<String>,
}

#[derive(Debug, PartialEq)]
pub enum HandshakeError {
    ProtocolMismatch { client: u32, daemon: u32 },
    ArtifactMismatch { client: u32, daemon: u32 },
}

impl Handshake {
    pub fn current() -> Self { todo!() }
    pub fn validate_against(&self, other: &Handshake) -> Result<(), HandshakeError> { todo!() }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn test_matching_handshake_ok() {
        let h = Handshake::current();
        assert!(h.validate_against(&h).is_ok());
    }
    #[test]
    fn test_protocol_version_mismatch_rejected() {
        let mut a = Handshake::current();
        let mut b = a.clone();
        b.daemon_protocol_version = a.daemon_protocol_version + 1;
        assert_eq!(a.validate_against(&b), Err(HandshakeError::ProtocolMismatch {
            client: a.daemon_protocol_version, daemon: b.daemon_protocol_version
        }));
    }
}
```

- [ ] **Step 2: Run to verify fail → Step 3: Implement** (`current()` reads `env!("CARGO_PKG_VERSION")`, constants; `validate_against` compares major versions).
- [ ] **Step 4: Verify pass → Step 5: Commit**

```bash
git commit -m "feat(daemon): add protocol-version handshake (spec §12.1)"
```

---

## Task 3: `leindexd` binary

**Why:** Spec §4.2. The daemon wraps the existing `McpServer::run_socket`. New binary; reuses all server internals.

**Files:**
- Create: `src/bin/leindexd.rs`
- Modify: `Cargo.toml` (add `[[bin]] name = "leindexd"`)

- [ ] **Step 1: Write failing test** — a smoke test that spawns `leindexd`, connects to the socket, sends `initialize`, and gets a response. (Lives in `tests/leindexd_smoke_test.rs`.)

```rust
// tests/leindexd_smoke_test.rs
#[tokio::test]
#[cfg(unix)]
async fn test_daemon_serves_initialize_over_socket() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("d.sock");
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_leindexd"))
        .arg("--socket").arg(&socket)
        .arg("--idle-timeout-secs").arg("5")
        .spawn().expect("spawn leindexd");
    // wait for socket to appear
    for _ in 0..50 {
        if socket.exists() { break; }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let mut s = tokio::net::UnixStream::connect(&socket).await.expect("connect");
    // send initialize frame, read response (reuse write_socket_frame framing)
    // ... assert response contains "capabilities" ...
    child.kill().ok();
}
```

- [ ] **Step 2: Run to verify fail** (binary doesn't exist).

- [ ] **Step 3: Implement `src/bin/leindexd.rs`**

```rust
// src/bin/leindexd.rs
// Minimal daemon entry: parse --socket + --idle-timeout-secs, acquire the
// hard startup lock (Task 1), build McpServer, call run_socket, write the
// endpoint sidecar after bind. NO eager project/model load (spec §4.2).

use leindex::cli::mcp::server::{McpServer, McpServerConfig};
use leindex::cli::daemon::{endpoint, startup_lock, handshake};

#[tokio::main(worker_threads = 2)] // §8.1: daemon starts at 2
async fn main() -> anyhow::Result<()> {
    // parse args (socket path, idle timeout) — clap or manual
    let socket_path = parse_socket_arg();
    let idle_timeout = parse_idle_timeout();

    // Hard startup lock: refuse to run if a live daemon exists.
    let run_dir = leindex::config::resolve_leindex_home()
        .ok_or_else(|| anyhow::anyhow!("no leindex home"))?
        .join("run");
    let outcome = startup_lock::resolve_endpoint(&run_dir, handshake::DAEMON_PROTOCOL_VERSION)?;
    match outcome {
        startup_lock::StartupOutcome::Connect(ep) => {
            anyhow::bail!("daemon already live at {} (pid {}); refusing second instance",
                ep.socket_path.display(), ep.pid);
        }
        startup_lock::StartupOutcome::Won(_) => { /* proceed */ }
    }

    let server = McpServer::new(McpServerConfig::default())?;
    // Write endpoint sidecar AFTER run_socket binds (run_socket owns the bind).
    // Refactor: run_socket returns the bound path, or accept a callback.
    server.run_socket(&socket_path, idle_clock, idle_timeout).await
}
```

NOTE on sidecar write timing: `run_socket` binds internally. Either (a) refactor `run_socket` to accept a "post-bind" callback that writes the sidecar, or (b) have `leindexd` pre-bind and pass the listener. Prefer (a) — minimal change. Add a `post_bind: Option<Box<dyn Fn(&Path)>>` param to `run_socket` OR write the sidecar from a small race-free helper using the bound pid + a known socket path (the socket_path IS known before bind, so write the sidecar immediately after `UnixListener::bind` succeeds inside `run_socket` via a hook). Document the chosen approach in the task.

- [ ] **Step 4: Run smoke test to verify pass.**
- [ ] **Step 5: Verify no eager load** — add an assertion in the smoke test that RSS stays flat (no project open) until a tool call arrives. Use the memcheck sampler.

- [ ] **Step 6: Commit**

```bash
git add src/bin/leindexd.rs Cargo.toml tests/leindexd_smoke_test.rs
git commit -m "feat(daemon): add leindexd binary wrapping McpServer::run_socket"
```

---

## Task 4: Stdio shim forwarder (feature-flagged)

**Why:** Spec §4.1 + §12.3 phase 3 (daemon opt-in). Convert `cmd_mcp_stdio_impl` so that when the `daemon-client` feature is on, it forwards to `leindexd` instead of running the full server.

**Files:**
- Modify: `src/cli/mcp_commands.rs` (`cmd_mcp_stdio_impl` gains a shim branch)
- Create: `src/cli/daemon/shim.rs` (forwarder)
- Modify: `Cargo.toml` (add `[features] daemon-client = []`)

- [ ] **Step 1: Write failing test**

```rust
// src/cli/daemon/shim.rs
//! Forwards MCP/JSON-RPC frames from stdin/stdout to the daemon socket.
//! Reuses read_socket_frame/write_socket_frame framing.

use std::io::{self};
use tokio::io::{AsyncRead, AsyncWrite};

pub async fn forward_stdio_to_daemon(
    endpoint: &super::startup_lock::DaemonEndpoint,
) -> anyhow::Result<()> {
    todo!()
}

#[cfg(test)]
mod test {
    #[tokio::test]
    #[cfg(unix)]
    async fn test_shim_forwards_initialize_and_response() {
        // start leindexd on a socket, run forward_stdio_to_daemon against it
        // piped through a duplex, assert initialize request flows daemon-bound
        // and the response flows client-bound.
    }
}
```

- [ ] **Step 2: Verify fail → Step 3: Implement forwarder**

Connect `UnixStream` to `endpoint.socket_path`. Spawn two tasks: (a) stdin → socket (frame-preserving passthrough — the shim need not parse JSON-RPC, just forward bytes/frames faithfully), (b) socket → stdout. Preserve client request IDs (spec §4.1). Reconnect once after daemon restart when safe (spec §4.1).

- [ ] **Step 4: Verify pass.**

- [ ] **Step 5: Wire shim branch into `cmd_mcp_stdio_impl`**

```rust
// mcp_commands.rs, top of cmd_mcp_stdio_impl
#[cfg(feature = "daemon-client")]
{
    let home = crate::config::resolve_leindex_home();
    if let Some(home) = home {
        let run_dir = home.join("run");
        match crate::cli::daemon::startup_lock::resolve_endpoint(
            &run_dir, crate::cli::daemon::handshake::DAEMON_PROTOCOL_VERSION)
        {
            Ok(crate::cli::daemon::startup_lock::StartupOutcome::Connect(ep)) => {
                return crate::cli::daemon::shim::forward_stdio_to_daemon(&ep).await;
            }
            Ok(crate::cli::daemon::startup_lock::StartupOutcome::Won(_)) => {
                // Spawn leindexd in the background, wait for socket, then forward.
                crate::cli::daemon::spawn::spawn_and_wait(&run_dir).await?;
                // re-resolve, forward
            }
            Err(e) => { tracing::warn!("daemon endpoint discovery failed: {e}; falling back to inline server"); }
        }
    }
    // Fall through to legacy inline server.
}
// ... existing inline server code unchanged ...
```

The shim is opt-in via `--features daemon-client`. Default build = legacy inline server. This matches rollout phase 3 (spec §12.3).

- [ ] **Step 6: Verify both paths** — default build runs inline; `--features daemon-client` build forwards.
- [ ] **Step 7: Commit**

```bash
git commit -m "feat(daemon): add feature-flagged stdio shim forwarder to leindexd"
```

---

## Task 5: Daemon spawn helper

**Why:** When the shim wins the startup race (`Won`), it must spawn `leindexd` and wait for the socket before forwarding. Spec §4.1: shim "Starts daemon under a cross-process startup lock when absent."

**Files:**
- Create: `src/cli/daemon/spawn.rs`

- [ ] **Step 1: Write failing test** — spawn helper launches `leindexd`, blocks until socket appears (bounded wait), returns the endpoint. Test against a mock that just creates the socket.
- [ ] **Step 2-4:** Implement, verify fail/pass. Use `Command::new("leindexd")` with `--socket` derived from a temp path under run-dir; poll for socket existence with timeout; re-read sidecar for the endpoint.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(daemon): add spawn-and-wait helper for shim-driven daemon start"
```

---

## Task 6: Version-mismatch + stale-endpoint cleanup

**Why:** Spec §12.1: incompatible protocol versions must "fail with actionable instructions." Spec §11.1: abandoned staging cleanup. The existing `leindex cleanup --stale-daemons` must also sweep stale `daemon.endpoint` sidecars.

**Files:**
- Modify: `src/cli/cleanup.rs` (`sweep_run_dir` already iterates run-dir; add `daemon.endpoint` to recognized stems)
- Modify: `src/cli/daemon/shim.rs` (emit actionable error on `HandshakeError`)

- [ ] **Step 1: Write failing test** — `sweep_run_dir` removes a dead-pid `daemon.endpoint`; keeps a live-pid one. Reuse the existing test shape in `cleanup.rs` (`test_sweep_keeps_live_pid_stem`).
- [ ] **Step 2-4:** Implement, verify.
- [ ] **Step 5: Add actionable handshake error** — on `ProtocolMismatch`, print: "Client daemon protocol v{client} cannot talk to daemon v{daemon}. Restart leindexd: `leindex cleanup --stale-daemons && leindexd`".
- [ ] **Step 6: Commit**

```bash
git commit -m "feat(daemon): version-mismatch errors + stale endpoint cleanup"
```

---

## Task 7: Full validation + daemon-vs-inline measurement

- [ ] **Step 1: Validation suite**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
Expected: PASS (zero warnings).

- [ ] **Step 2: Measure shim RSS vs inline RSS** (uses WS1 memcheck)

Build both: `cargo build --release` and `cargo build --release --features daemon-client`. Run memcheck contention_3c_2p phase against each. Record:
- Inline (legacy): 3 heavyweight processes, combined RSS.
- Daemon: 1 daemon + 3 shims, combined RSS.

Save to `docs/baselines/2026-08-04-ws3-daemon-vs-inline.json`.

- [ ] **Step 3: Verify §4.1 shim target (5–15 MiB RSS)**

Shim RSS must be 5–15 MiB. If not, profile and trim (shim should hold no registry, no SQLite, no PDG — only the UnixStream + framing buffers).

- [ ] **Step 4: Verify §4.2 no eager load** — daemon RSS stays flat until first tool call (smoke test from Task 3 Step 5).

- [ ] **Step 5: Commit**

```bash
git add docs/baselines/2026-08-04-ws3-daemon-vs-inline.json
git commit -m "docs(ws3): record daemon vs inline RSS measurements"
```

---

## Handoff Summary (fill after execution)

```text
Workstream: WS3
Revision: 1.0
Invariant status: GrayHill invariant preserved (shim never hard-exits); no behavior disabled/skipped/shrunk; legacy default retained
Files changed: src/bin/leindexd.rs, src/cli/daemon/{mod,endpoint,startup_lock,handshake,shim,spawn}.rs, src/cli/mcp_commands.rs, src/cli/cleanup.rs, Cargo.toml
Tests run/results: [fill]
Benchmark artifacts: docs/baselines/2026-08-04-ws3-daemon-vs-inline.json
Before/after resource table: [fill — inline 3× heavyweight vs 1 daemon + 3 shims]
Before/after quality table: N/A (no retrieval behavior changed; same McpServer serves)
Unverified assumptions: [fill — e.g., run_socket post-bind hook shape; macOS kill(pid,0) liveness]
Known risks: daemon-client feature must remain OFF by default until WS12 rollout phase 3
Rollback: cargo build without daemon-client = legacy inline server, unchanged
Next workstream prerequisites: WS4 (shared registry) needs the daemon binary serving; WS5 (scheduler) needs daemon admission hook
```

---

## Spec-coverage check (§4.1, §4.2, §12.1, §12.3)

| Spec requirement | Task |
|---|---|
| §4.1 shim: parse/emit MCP framing | Task 4 (forwarder, byte-faithful) |
| §4.1 shim: discover daemon endpoint | Task 1 (endpoint discovery) |
| §4.1 shim: start daemon under startup lock when absent | Tasks 1, 5 |
| §4.1 shim: preserve request IDs | Task 4 (passthrough) |
| §4.1 shim: reconnect once after restart | Task 4 |
| §4.1 shim: reject incompatible versions with actionable instructions | Task 6 |
| §4.1 shim: no registry/SQLite/PDG/Tokio pool | Task 7 Step 3 (verified) |
| §4.1 5–15 MiB RSS target | Task 7 Step 3 |
| §4.2 one daemon per user + protocol version | Tasks 1, 3 |
| §4.2 no eager project/model load | Task 3 Step 5 |
| §4.2 exit after inactivity | Reuses existing ProcessIdleClock |
| §12.1 handshake (LeIndex/daemon/artifact/worker/capabilities) | Task 2 |
| §12.3 phase 3: daemon opt-in with legacy artifacts | Task 4 (feature flag, default OFF) |

---

## Forbidden shortcuts (anti-cheat §2.1)

- Do NOT disable any tool handler in the daemon vs inline (same `all_tool_handlers()` serves both).
- Do NOT reduce the daemon to a subset of tools to save RSS.
- Do NOT skip the handshake to save startup latency.
- Do NOT hold a second ProjectRegistry in the shim.
- Do NOT silently fall back to inline server when the daemon is reachable (only fall back on discovery failure, with a warning).
