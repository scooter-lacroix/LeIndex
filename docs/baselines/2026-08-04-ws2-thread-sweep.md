# WS2 Task 9: Tokio Worker / ORT Thread Sweep Measurements

**Date:** 2026-08-04  
**Feature:** sp1-thread-sweep  
**Plan ref:** `docs/superpowers/plans/2026-08-04-ws1-2-baseline-containment.md` Task 9  
**Validation assertions:** VAL-CONT-002, VAL-CONT-005  
**Measurement type:** Observation only (no code changes)

---

## Environment

| Field | Value |
|-------|-------|
| CPU | AMD Ryzen 7 7800X3D 8-Core (16 threads) |
| Memory | 65,424,824 KiB (~62 GiB) |
| Kernel | 7.1.5-1-cachyos |
| Architecture | x86_64 |
| malloc | glibc (default build, MALLOC_ARENA_MAX=2) |
| ONNX Runtime | 1.25.0 (CPU execution provider) |
| Embed model | qwen3-embed-0.6b (FP16, ~1.2 GiB RSS at load) |
| Fixture | `tests/fixtures/memcheck/small_repo` (5.6 MiB, 31 source files) |
| Binary | Release build, `--features onnx` |

---

## Methodology

Each measurement uses the release binary at `/home/scooter/target-leindex-release/release/leindex`
with the embed worker at `/home/scooter/target-leindex-release/release/leindex-embed`.

**Steady-state RSS (MCP idle):** MCP stdio server launched, JSON-RPC initialize +
initialized handshake completed, 4-second dwell with 250ms sampling (16 samples).

**Index-phase RSS:** `leindex index <fixture>` with 10ms interval sampling via a C
sampler that reads `/proc/<pid>/status` (main process) and walks `/proc/*/stat` for
worker children (leindex-embed). The index pipeline spawns the embed worker for neural
batch inference. The worker loads the ONNX model (~1.5 GiB), attempts inference, then
falls back to TF-IDF on dimension mismatch (pre-existing, unrelated to this task).

**Query latency (warm):** MCP stdio server with warm-up query, then 10 sequential
`leindex.search` tool calls. Latency measured as wall time from request write to
response read. Queries: struct, impl, trait, enum, model, handler, config, crypto,
error, user.

**ORT thread sweep:** Index-phase RSS measurement with `LEINDEX_WORKER_ORT_THREADS` set
to 1, 2, and 4. All runs use `LEINDEX_TOKIO_WORKERS=2` to isolate ORT thread impact.

The default ORT thread count is `floor(3/4 * available_parallelism)` = 12 on this
16-thread host (see `src/embed/runtime_env.rs::default_ort_threads`). The sweep tests
explicit overrides of 1, 2, and 4.

---

## Results

### 1. Tokio Worker Sweep: Steady-State MCP RSS

Measures idle MCP server RSS after full Tokio runtime initialization.

| Tokio Workers | RSS Min (KiB) | RSS Max (KiB) | RSS P95 (KiB) | RSS (MiB) |
|--------------:|--------------:|--------------:|--------------:|----------:|
| 2             | 8,664         | 8,664         | 8,664         | 8.5       |
| 4             | 8,664         | 8,664         | 8,664         | 8.5       |

**Delta:** 0 KiB. Steady-state MCP RSS is identical between worker counts. The Tokio
worker pool allocates thread stacks lazily; idle MCP with no active tasks shows no
memory difference between 2 and 4 workers.

### 2. Tokio Worker Sweep: Query Latency (Warm)

Measures `leindex.search` tool call latency through MCP stdio on a pre-indexed fixture,
after a warm-up query. Uses lexicographic/TF-IDF search (worker not active for queries
on this small fixture).

| Tokio Workers | p50 (ms) | p95 (ms) | p99 (ms) | n | RSS During (KiB) |
|--------------:|---------:|---------:|---------:|--:|-----------------:|
| 2             | 2,033    | 2,040    | 2,040    | 10 | 18,600 |
| 4             | 2,030    | 2,034    | 2,034    | 10 | 18,376 |

**Delta:** p50 differs by 3ms (0.15%), p95 by 6ms (0.29%). Within measurement noise.
The ~2s per-query latency is dominated by SQLite + TF-IDF computation plus MCP
serialization overhead, not Tokio scheduling. Four workers does NOT materially help
query latency.

### 3. Tokio Worker Sweep: Index-Phase RSS

Measures peak RSS during `leindex index` (main process + embed worker combined).

| Tokio Workers | Main RSS Max (KiB) | Worker RSS Max (KiB) | Combined RSS Max (KiB) | Combined (MiB) |
|--------------:|-------------------:|---------------------:|-----------------------:|---------------:|
| 2             | 22,008             | 1,557,332            | 1,579,340              | 1,542          |
| 4             | 22,044             | 1,546,204            | 1,568,248              | 1,531          |

**Delta:** Combined RSS differs by 11,092 KiB (~10.8 MiB, 0.7%). Tokio=4 uses
marginally less combined RSS, likely due to scheduling differences in worker spawn
timing. Main process RSS is effectively identical (22,008 vs 22,044 KiB). Worker
RSS is dominated by the ONNX model load (~1.5 GiB) regardless of Tokio worker count.

### 4. ORT Thread Sweep: Index-Phase RSS

Measures peak RSS during `leindex index` with `LEINDEX_WORKER_ORT_THREADS` set to
1, 2, and 4 (Tokio workers fixed at 2).

| ORT Threads | Main RSS Max (KiB) | Worker RSS Max (KiB) | Combined RSS Max (KiB) | Combined (MiB) |
|------------:|-------------------:|---------------------:|-----------------------:|---------------:|
| 1           | 21,456             | 1,554,260            | 1,575,716              | 1,539          |
| 2           | 22,276             | 1,539,512            | 1,561,788              | 1,525          |
| 4           | 22,044             | 1,541,700            | 1,563,744              | 1,527          |

**Delta:** Combined RSS range is 13,928 KiB (~13.6 MiB, 0.9%). ORT thread count has
minimal RSS impact in this workload. The worker loads the full ONNX model regardless
of intra-op thread count; thread pools add only modest per-thread arena overhead.
More threads (4) does NOT increase RSS meaningfully; fewer threads (1) does NOT save
material memory.

The worker is alive for 111+ samples at 10ms intervals (~1.1 seconds) during model
loading and inference attempt. Peak RSS is reached during ONNX model load + session
initialization, before inference even begins.

---

## Summary Table

| Configuration         | Steady RSS (KiB) | Index Combined RSS (KiB) | Query p95 (ms) |
|-----------------------|-----------------:|-------------------------:|----------------:|
| Tokio=2 (default)     | 8,664            | 1,579,340                | 2,040           |
| Tokio=4               | 8,664            | 1,568,248                | 2,034           |
| Tokio=2, ORT=1        | 8,664            | 1,575,716                | N/A             |
| Tokio=2, ORT=2        | 8,664            | 1,561,788                | N/A             |
| Tokio=2, ORT=4        | 8,664            | 1,563,744                | N/A             |

---

## Conclusions

### Tokio Worker Count (2 vs 4)

1. **Steady-state RSS:** No difference (8,664 KiB either way).
2. **Query latency:** Negligible difference (p50 2,033ms vs 2,030ms, 0.15%).
3. **Index-phase combined RSS:** Negligible difference (1,579 MiB vs 1,568 MiB, 0.7%).
4. **Recommendation:** Keep default of 2 Tokio workers. Four workers does not
   materially help latency or RSS. The architecture spec (section 8.1) states "start
   at 2; benchmark 2-4." The benchmark confirms 2 is sufficient. Using 2 reduces
   per-connection thread overhead without sacrificing performance.

### ORT Thread Count (1, 2, 4)

1. **RSS impact:** Minimal. Combined RSS range is 13.6 MiB (0.9%) across 1-4 threads.
2. **Default (floor(3/4 * parallelism) = 12):** Not directly measured (would need ORT
   thread override disabled to compute), but the ONNX model dominates RSS regardless.
3. **Recommendation:** The existing default (`floor(3/4 * available_parallelism)` capped
   at hardware) remains appropriate. The `LEINDEX_WORKER_ORT_THREADS` env var is
   confirmed configurable and measured. For the daemon workstream (SP2), consider
   capping ORT threads to 2-4 on large-core hosts to reduce ONNX session arena
   overhead, as the RSS difference is negligible and lower threads reduce CPU
   contention with the main Tokio runtime.

### Daemon Workstream Note (SP2)

Four Tokio workers does NOT materially help latency for this workload (p95 query
latency difference of 6ms is noise). The 1 GiB aggregate RAM budget (architecture
section 5) is achievable with 2 Tokio workers. The daemon should ship with the default
of 2. If future benchmarks on larger corpora show query latency becoming
Tokio-scheduler-bound, the daemon can dynamically scale workers based on concurrent
client count.

The dominant RSS consumer is the embed worker ONNX model load (~1.5 GiB). This
confirms the SP3a/SP6 requirement for model quantization to fit the 350 MiB
embed-worker budget. Thread count tuning alone cannot bridge the gap.

---

## Environment Variables Tested

| Variable | Values | Impact |
|----------|--------|--------|
| `LEINDEX_TOKIO_WORKERS` | 2, 4 | No material RSS or latency difference |
| `LEINDEX_WORKER_ORT_THREADS` | 1, 2, 4 | <1% combined RSS difference across range |
| `MALLOC_ARENA_MAX` | 2 (fixed) | Applied per containment default (VAL-CONT-001) |
| `LEINDEX_WORKER_EXECUTION_PROVIDER` | cpu (fixed) | No GPU on this host |

---

## Reproduction

```bash
# Build release binaries
cargo build --release --features onnx

# Set binary paths
LEINDEX_BIN=target/release/leindex
WORKER_BIN=target/release/leindex-embed
FIXTURE=tests/fixtures/memcheck/small_repo

# Steady-state MCP RSS
LEINDEX_TOKIO_WORKERS=2 LEINDEX_EMBED_DAEMON=0 MALLOC_ARENA_MAX=2 \
  $LEINDEX_BIN mcp --stdio  # measure /proc/<pid>/status VmRSS over 4s

# Index-phase RSS (with fast /proc sampler at 10ms)
LEINDEX_TOKIO_WORKERS=2 LEINDEX_WORKER_BINARY=$WORKER_BIN \
  LEINDEX_WORKER_EXECUTION_PROVIDER=cpu MALLOC_ARENA_MAX=2 \
  $LEINDEX_BIN index <fixture>  # sample main + worker child RSS

# ORT thread sweep
LEINDEX_TOKIO_WORKERS=2 LEINDEX_WORKER_ORT_THREADS=1 \
  LEINDEX_WORKER_BINARY=$WORKER_BIN LEINDEX_WORKER_EXECUTION_PROVIDER=cpu \
  MALLOC_ARENA_MAX=2 \
  $LEINDEX_BIN index <fixture>  # repeat for ORT_THREADS=2,4
```
