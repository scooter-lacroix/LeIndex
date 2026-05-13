# LeIndex Memory Investigation Report

**Date:** 2026-05-06  
**Host:** Lacroix (CachyOS Linux, Arch-based)  
**Kernel:** 6.18.26-1-cachyos-lts / 7.0.3-1-cachyos  
**RAM:** 54 GB physical + 54 GB zram swap  
**LeIndex Version:** 1.6.1 (installed via cargo at `~/.cargo/bin/leindex`)

---

## 1. Executive Summary

**LeIndex is confirmed as the root cause of repeated system crashes.** The Linux OOM killer terminated `leindex` processes **three separate times** in a single session (boot `-2`, May 5–6 2026), each instance consuming between **28–38 GB RAM** before being killed. A fourth crash event was a **SIGSEGV (segmentation fault)** caused by a memory corruption bug in LeIndex's deallocation path (`__libc_free`). Combined with a system swappiness of 150, the memory exhaustion caused cascading desktop failures — killing Wayland, Qt applications, and triggering rapid reboots.

---

## 2. OOM Kill Events (Boot -2, May 5–6 2026)

### Kill #1 — 01:49:57 EDT
```
Out of memory: Killed process 12619 (leindex)
  total-vm: 86,715,224 kB (~83 GB virtual)
  anon-rss: 30,207,080 kB (~29 GB physical)
  pgtables: 154,296 kB
  oom_score_adj: 200
  cgroup: app-ghostty-surface-transient-2639.scope
```
**RSS at kill: ~29 GB.** One of five leindex instances running. PID 12619 was the dominant consumer with 7.5M pages.

### Kill #2 — 02:00:52 EDT (11 minutes later)
```
Out of memory: Killed process 37995 (leindex)
  total-vm: 95,059,964 kB (~91 GB virtual)
  anon-rss: 39,975,888 kB (~38 GB physical)
  pgtables: 170,804 kB
  oom_score_adj: 200
  cgroup: app-ghostty-surface-transient-25594.scope
```
**RSS at kill: ~38 GB.** This is the largest LeIndex memory consumption recorded — 70% of system RAM.

### Kill #3 — 02:09:31 EDT (9 minutes later)
```
Out of memory: Killed process 64794 (leindex)
  total-vm: 79,570,776 kB (~76 GB virtual)
  anon-rss: 30,303,436 kB (~29 GB physical)
  pgtables: 140,228 kB
  oom_score_adj: 200
  cgroup: app-ghostty-surface-transient-53073.scope
```
**RSS at kill: ~29 GB.** Note: at the time of this kill, another leindex process (PID 66456) was also consuming **2.3 GB**, and PID 72859 had **36 MB** in the same OOM scan.

---

## 3. SIGSEGV Crash (Same Boot, 03:20:33 EDT)

```
Process 185717 (leindex) terminated abnormally with signal 11/SEGV

Stack trace of thread 194849:
  #0  __libc_free (libc.so.6 + 0xbf027)
  #1  n/a (leindex + 0x199e182)
  #2  n/a (leindex + 0x185bfe9)
  #3  n/a (leindex + 0x1901a9c)
  #4  n/a (leindex + 0x181ef0a)
  #5  n/a (leindex + 0x1c87f09)
  #6  n/a (leindex + 0x1c8929c)
  #7  n/a (leindex + 0x1c3f6d4)
```

**Crash in `__libc_free`** — this is a classic indicator of **heap corruption**: either a double-free, use-after-free, or buffer overflow corrupting malloc metadata. This is a **software defect in LeIndex**, not a system issue. The crash occurred ~70 minutes after the last OOM kill, suggesting LeIndex was re-launched and hit a memory corruption path during normal operation.

---

## 4. Current Running State (As of 2026-05-06 ~21:00 EDT)

| PID | Command | RSS | %MEM | Uptime | OOM Score |
|-----|---------|-----|------|--------|-----------|
| 689399 | `leindex mcp` | 643 MB | 1.1% | 2h10m | 804 (adj: 200) |
| 1008908 | `leindex mcp` | 505 MB | 0.8% | 25m | 802 (adj: 200) |
| 1012398 | `leindex mcp` | 505 MB | 0.8% | 21m | 802 (adj: 200) |
| 689382 | `maestro mcp proxy leindex` | 4 MB | 0.0% | 2h10m | 800 |
| 773838 | `maestro mcp proxy leindex` | 13 MB | 0.0% | 1h20m | 800 |
| 817461 | `maestro mcp proxy leindex` | 13 MB | 0.0% | 59m | 800 |
| 1004878 | `maestro mcp proxy leindex` | 13 MB | 0.0% | 28m | 800 |

**Total current LeIndex memory: ~1.6 GB RSS across 7 processes (3 leindex + 4 maestro proxies).**

This is **normal and healthy**. The problem is not that LeIndex always uses excessive memory — it's that under certain conditions (likely indexing large codebases or receiving concurrent requests), a single instance can balloon to 29–38 GB.

---

## 5. Index Database Sizes

### Active Indexes (Persistent Workspaces)

| Path | Size | Notes |
|------|------|-------|
| `/home/scooter/Documents/Product/.leindex/` | **727 MB** | Largest active index |
| `/home/scooter/.leindex/` | 237 MB | Home directory index |
| `/home/scooter/Stan-s-ML-Stack/.leindex/` | 2.8 MB | ML stack project |
| `/home/scooter/.factory/.leindex/` | 5.5 MB | Factory project |
| `/home/scooter/.config/opencode/.leindex/` | 4.2 MB | OpenCode config |
| `/home/scooter/.omp/.leindex/` | 264 KB | OMX project |
| Others | < 1 MB each | Various small indexes |

### Stale/Cached Indexes

| Path | Size | Notes |
|------|------|-------|
| `~/.claude/tmp/llvm-project-source-mirror/.leindex/` | **605 MB** | LLVM source mirror — likely stale |
| `~/.claude/tmp/leindex/llvm-project-*/.leindex/` | 344 KB | LLVM partial index |
| `~/.claude/tmp/lephase-phase*` | ~160 KB each | **100+ stale phase indexes** from Claude sessions |
| `~/.claude/tmp/.tmp*/.leindex/` | Various | ~20 stale temp session indexes |

**Total stale index disk usage: estimated ~700 MB+ in `~/.claude/tmp/` alone.**

---

## 6. Root Cause Analysis

### Primary Cause: Unbounded Memory Growth During Indexing

LeIndex appears to have **no memory cap** during indexing operations. When indexing a large or complex codebase, a single `leindex mcp` process can grow to consume 29–38 GB of RAM — exhausting both physical RAM (54 GB) and zram swap (54 GB) before the OOM killer intervenes.

### Contributing Factors

| Factor | Value | Impact |
|--------|-------|--------|
| **vm.swappiness** | 150 (very high) | Causes aggressive swap-thrashing before OOM, freezing the system for minutes |
| **zram swap only** | 54 GB compressed in RAM | Swap pressure competes with applications for physical RAM |
| **oom_score_adj** | 200 (elevated) | LeIndex is a *preferred* OOM target, but by the time it's killed, the system is already unresponsive |
| **Multiple concurrent instances** | 3 active `leindex mcp` processes | If multiple instances trigger unbounded growth simultaneously, OOM is guaranteed |
| **Heap corruption bug** | SIGSEGV in `__libc_free` | Use-after-free or double-free defect — suggests memory management issues in LeIndex's Rust code (possibly via FFI or unsafe blocks) |

### Attack Pattern

1. LeIndex begins indexing a large codebase (e.g., `/home/scooter/Documents/Product/` at 727 MB index)
2. Memory grows unbounded — RSS climbs from ~500 MB to 29–38 GB over minutes
3. System exhaustes RAM → begins swap-thrashing (swappiness=150 makes this worse)
4. Desktop freezes (Wayland compositor starved)
5. OOM killer fires, kills leindex — but system is already in a degraded state
6. Cascading failures: Qt apps crash, coredumps flood disk, user forced to reboot

---

## 7. Hypothesis: Specific Trigger Conditions

Based on the data, the unbounded growth likely occurs when:

1. **Large monorepo indexing**: The `/home/scooter/Documents/Product/` index is 727 MB — this is an unusually large code index. When LeIndex processes queries against this index or rebuilds it, it may load the entire database into memory without pagination or streaming.

2. **Concurrent MCP requests**: Multiple `maestro mcp proxy leindex` instances suggest LeIndex is being called by multiple AI agents/tools simultaneously. If two instances both try to index or query the same large workspace, memory usage doubles.

3. **LLVM project indexing**: The 605 MB LLVM source mirror in `~/.claude/tmp/` indicates an attempt to index the entire LLVM codebase — one of the largest C++ projects in existence. This alone could trigger the 38 GB memory spike.

---

## 8. Recommendations

### Immediate Mitigations

| # | Action | Priority |
|---|--------|----------|
| 1 | **Set `vm.swappiness=60`** (already done via sysctl.d) | ✅ Done |
| 2 | **Create a systemd memory limit for leindex**: `systemd-run --user --scope -p MemoryMax=8G leindex mcp` | 🔴 Critical |
| 3 | **Clean stale indexes**: `rm -rf ~/.claude/tmp/lephase-* ~/.claude/tmp/.tmp* ~/.claude/tmp/leindex/llvm-*` | 🟡 Medium |
| 4 | **Remove the LLVM source mirror index** (605 MB, likely unnecessary) | 🟡 Medium |

### Long-Term Fixes

| # | Action | Owner |
|---|--------|-------|
| 1 | Add `--max-memory` flag to LeIndex to cap RSS during indexing | LeIndex dev |
| 2 | Implement streaming/paginated query results instead of loading full index | LeIndex dev |
| 3 | Investigate the `__libc_free` SIGSEGV — file bug with stack trace | LeIndex dev |
| 4 | Add memory monitoring to maestro's LeIndex integration (auto-restart if RSS > threshold) | Maestro dev |
| 5 | Consider using `cgroups` memory limits on the Ghostty scope that hosts leindex | User |

### systemd Memory Limit (Recommended Configuration)

Create `~/.config/systemd/user/leindex-memory.conf`:
```ini
[Service]
MemoryHigh=6G
MemoryMax=8G
MemorySwapMax=2G
```

Or wrap invocations:
```bash
systemd-run --user --scope -p MemoryMax=8G -p MemorySwapMax=2G leindex mcp
```

---

## 9. Data Sources

- `journalctl -b -2` — OOM kill logs and stack traces
- `/proc/*/oom_score`, `/proc/*/oom_score_adj` — OOM scoring
- `ps aux` — Current process state
- `coredumpctl list` — 5 coredumps (4x rustc SIGABRT, 1x librewolf SIGSEGV)
- `find ~/.claude/tmp -name 'leindex.db'` — 100+ stale index databases
- `du -sh` on all `.leindex/` directories — Index size mapping

---

## 10. Conclusion

**The issue is validated and real.** LeIndex has an unbounded memory growth defect that, combined with high swappiness and zram-only swap, causes complete system lockups. The SIGSEGV crash in `__libc_free` further confirms memory management issues in the application. Until LeIndex implements memory caps, the system must enforce external limits via cgroups or systemd to prevent recurrence.

**Severity: Critical** — causes full system unresponsiveness requiring hard reboots, data loss risk from unclean shutdowns, and cascading application failures.
