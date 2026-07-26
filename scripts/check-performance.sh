#!/usr/bin/env bash
set -euo pipefail

# Deterministic JSON gate for live/exact latency and core TF-IDF paths.
# The benchmark owns sampling, persistence, and threshold assertions; this
# wrapper deliberately has no wall-clock cancellation of indexing work.
export LC_ALL=C
export LEINDEX_ENFORCE_PERF=1
export LEINDEX_PERF_OUTPUT="${LEINDEX_PERF_OUTPUT:-target/leindex-performance.json}"

# Cargo's bench profile is optimized; this Cargo version rejects the
# unsupported `--release` flag for `cargo bench`.
cargo bench --all-features --bench mcp_tool_latency -- --noplot

test -s "$LEINDEX_PERF_OUTPUT"
echo "LeIndex performance report: $LEINDEX_PERF_OUTPUT"
