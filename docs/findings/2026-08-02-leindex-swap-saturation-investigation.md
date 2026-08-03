# LeIndex Swap-Saturation Investigation — Empirical Evidence

**Date:** 2026-08-02 (21:50–22:05 UTC)
**Host:** scooter's workstation — 62 GiB RAM, 31 GiB swap
**Tree:** `/mnt/WD-SSD/code_index_update/LeIndexer-release-1.8.4`, branch `feat/embed-merge-1.10.0`
**Reported by:** user (system-wide thrash; swap completely full)

---

## 1. System state (captured live)

```
              total     used    free   shared  buff/cache  available
Mem:          62Gi      40Gi    9.2Gi   197Mi   13Gi       22Gi
Swap:         31Gi      31Gi    88Mi      ← COMPLETELY FULL (thrashing)
```

## 2. Process inventory (leindex family, sorted by swap)

| PID | PPID | RSS | SWAP | VSZ | Command | Uptime |
|-----|------|-----|------|-----|---------|--------|
| 359987 | 359944 (claude) | 2.39 GiB | **14.0 GiB** | 17.3 GiB | `leindex mcp` | **13h26m** |
| 963617 | 359987 | 2.15 GiB | **8.9 GiB** | 24.3 GiB | `leindex-embed --socket …62adab…sock` | 2h42m |
| 801329 | 799868 (codex) | 2.22 GiB | 0.88 GiB | 3.7 GiB | `leindex mcp` | 4h42m |
| 1081909 | 799868 | 0.70 GiB | 0.14 GiB | 1.7 GiB | `leindex mcp` | — |
| 1051388 | 799868 | 0.51 GiB | 0.71 GiB | 2.0 GiB | `leindex mcp` | — |
| 1032135 | 799868 | 1.4 MiB | 0.47 GiB | 1.5 GiB | `leindex mcp` | — (fully swapped) |
| 1184391/1245885/1245994/361121 | codex/claude | 1–6 MiB | ~0 | ~1.05 GiB | `leindex mcp` | idle |

**Totals:** 8+ live `leindex mcp` instances (codex alone holds 7); leindex family ≈ 7.8 GiB RSS + ~24 GiB swap.

## 3. Root-cause evidence

### RC-1 — No single-instance/reuse for MCP mode; no process-level idle exit
- 8 `leindex mcp` instances coexist; every agent session spawns its own.
- `cmd_mcp_stdio_impl` (src/cli/mcp_commands.rs:66) DOES exit on stdin EOF and lazily loads projects — but the **process** has **no idle timeout**, so while the agent parent lives (hours), the server lives.
- `spawn_stdio_cleanup` only evicts *sessions* (300s idle), never the loaded project engine nor the process.

### RC-2 — Loaded project engine is never unloaded (per-process RSS amplifier)
- On first tool call, `ProjectRegistry::get_or_load()` loads the project engine (`LeIndex::load_from_storage`) and **keeps it forever** in that process.
- The active project's index is **51 GiB** on disk (`du -sh …/LeIndexer-release-1.8.4/.leindex` = 51G). Loading it yields 2.4 GiB RSS / 17 GiB VSZ per instance (mmap'd neural/fragment/snapshot artifacts).
- 8 instances × duplicated engine state ≈ the bulk of the leindex RSS.

### RC-3 — Worker anonymous memory far exceeds model size
- `leindex-embed` (socket daemon) 963617: 2.15 GiB RSS / 8.9 GiB swap / **24.3 GiB VSZ** after 2h42m.
- Model is qwen3-embed-0.6b (fp32 ≈ 2.4 GiB, int8 ≈ 0.6 GiB). The ~11 GiB of anonymous memory touched is NOT the model → ORT thread pool / arena / runtime overhead.
- `src/embed/runtime.rs` sets `with_memory_pattern(false)` + `with_optimization_level(Level1)` but **no intra-op thread cap** (`intra_op_num_threads` absent) → ORT defaults to per-core thread pools (MLAS/oneDNN per-thread buffers on a many-core box).
- ort = `2.0.0-rc.13` (Cargo.toml:308) → `SessionBuilder::with_intra_op_num_threads` available.

### RC-4 — Stale daemon control artifacts linger after SIGKILL
- `~/.leindex/run/` contains `.lock/.sock/.pid/.status` files from Jul 24–26 whose workers are long dead (PDEATHSIG SIGKILLs the worker when the parent dies; files are not removed by the kernel).
- Client-side cleanup exists (`availability()` dead-PID check + `cleanup_daemon_paths`) but only fires when a client probes; orphaned artifacts persist indefinitely otherwise.

### RC-5 — No memory-pressure guard at worker/MCP spawn time
- `memory_cap.rs` provides `current_rss_mb()`, `apply_hard_limit()` (RLIMIT_AS), `MemoryCapGuard` — wired only into the `index` command (`cli.rs:690`).
- Nothing prevents spawning a 2.4 GiB worker or loading a 51 GiB-project engine while the box is thrashing at 100% swap.

## 4. What already works (do not regress)
- Worker **PR_SET_PDEATHSIG** (worker_main.rs:76) — kernel-enforced parent-death SIGKILL (this alone prevents the historic 28-orphan/47 GB episode).
- Socket daemon **lock/sock/status/pid/start** sidecars + `DaemonSpawnLock` + `daemon_pid_is_owned` + `kill_stale_daemon_by_pid` (client.rs / client_config.rs).
- Worker **idle timeout** (600s for socket mode, runtime.rs:205).
- CLI one-shot `force_shutdown_daemon()` (cli.rs:709).
- MCP stdio **stdin-EOF exit** (mcp_commands.rs `StdioInput::End` → break).
- MCP **session** idle cleanup (300s, server.rs `cleanup_stale_sessions`).
- `tools/memcheck` worker binary resolution (main.rs:182–223) + worker-active phases (workload.rs).
- `MemoryCapGuard` infra (memory_cap.rs:108).

## 5. Upstream plan
See `docs/plans/memory-pressure-remediation.md` (design + tasks) and
`docs/plans/memory-pressure-tracking.md` (execution handoff tracker).
