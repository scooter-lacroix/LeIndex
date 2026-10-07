<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)
[![Release](https://raw.githubusercontent.com/scooter-lacroix/LeIndex/badges/version-badge.svg)](https://github.com/scooter-lacroix/LeIndex/actions/workflows/release.yml)

</div>

# LeIndex

**Semantic code intelligence for humans and AI agents — a Program Dependence Graph, hybrid neural search, and 18 structural tools behind one small daemon.**

Ask LeIndex where authentication lives, what breaks if you rename a function,
or how the indexer resumes across phases — and get the symbols, callers, and
source back in milliseconds, not a dump of files to read.

```text
you:     "where is request validation enforced?"

LeIndex: src/http/request_validator.rs    validate_session   (0.92)
         src/middleware/input_guard.rs    authorize_user     (0.84)
         callers: 3 · callees: 7 · blast radius: 12 files
```

---

## v2.0.0 — the resource revolution, completed

v2.0.0 rebuilt LeIndex around one idea: **a code index should never be the
biggest process on your machine.** The architecture work (one user-scoped
daemon + tiny stdio shims, content-addressed generation storage, a streaming
bounded indexing pipeline, an admit/defer/reduce memory controller) is
benchmarked in [`BENCHMARKS.md`](BENCHMARKS.md) — including MRR@10 = 1.0000
against the v1.9.5 baseline, so none of it cost retrieval quality. The
final rounds of hardening, all measured on this repository's own source
(457 files / 20,500 nodes / 122,000 edges):

| | before | after |
|---|---:|---:|
| Re-index tail after "saving to storage..." | 85.7 s | **5.0 s (17x)** |
| Neural embeddings recomputed per re-index | 10,242 rows | **only changed content** |
| Concurrent embed daemons after a config change | stacked (OOM'd a 64 GiB box) | **1, RSS-capped, idle-evicted** |
| Silent degradation in tool responses | empty relations as fact | **explicit notes + direction labels** |
| One-shot CLI vs MCP feature parity | enrichment silently skipped | **same enrichment, honest budgets** |

How: a **client-side content-addressed embedding cache** (probe hits locally,
embed only misses — an all-hit run never loads the model), edge-level **PDG
diffing** on save (unchanged graphs write nothing), a **single-daemon policy**
with zombie-aware cleanup and a sibling-aware memory floor, and honest
reporting end to end (impact direction labels, `impact_note` when a zero
could mean degradation, `signature_scope` on incremental counts).

### The intelligence stack behind it

- **37 active language grammars** today — Rust, TypeScript/JavaScript, Go,
  Python, Java, C/C++, C#, Ruby, PHP, Swift, Kotlin, Scala, Elixir, Erlang,
  Haskell, Zig, R, and more — on a roadmap to **100+**; each grammar is an
  individually gated crate that cannot regress the build
  ([`docs/LANGUAGES.md`](docs/LANGUAGES.md)).
- **Documentation is first-class**: markdown/rst/adoc/txt files index as
  heading-section nodes, so "how do generations work" retrieves the exact
  ARCHITECTURE section — not the whole handbook.
- **Leiden community detection** (feature-flagged, default on) clusters the
  call/data/containment graph; `project-map` can group by community and
  `impact-analysis` reports community crossings.
- **SCIP precision tier** (on by default; `LEINDEX_FEATURE_PRECISION_INGEST=false` disables):
  when a language indexer such as `rust-analyzer scip` is present, LeIndex
  merges its exact definitions and relationships into the PDG — upgrading
  heuristic edges to confidence 1.0 and marking precision-confirmed symbols.
  Missing indexers degrade silently to the tree-sitter tier.
- **Engram query phrase-book** (opt-in: `LEINDEX_FEATURE_ENGRAM=1`): a persistent,
  content-addressed table of neural query embeddings under `~/.leindex/engram/`.
  A repeated query is served without waking the embedder (no worker spawn, no
  network round trip); keys include the embedder identity, so a changed model
  never serves stale vectors. Bounded (20,000 rows / 256 MiB, LRU) and reported in
  `leindex_analyze mode=diagnostics`. Query embeddings only; index-time neural
  reuse is the existing global embed cache.

The retrieval quality claims are gated by a deterministic benchmark —
internal fixtures plus a vendored subset of the CoSQA (ACL 2021)
human-annotated query/code benchmark — see
[`docs/baselines/AGENT_TASKS_METHODOLOGY.md`](docs/baselines/AGENT_TASKS_METHODOLOGY.md).

---

## What agents see: 4 MCP tools

Four routers instead of twenty schemas — a `mode` / `action` argument picks the
operation, every other argument passes straight through:

| Tool | Selector | Branches |
|---|---|---|
| `leindex_explore` | `mode` | `search` (hybrid semantic, default) · `find` (exact text / regex / symbol names — indexed, always live, **any path on disk**) · `symbol_lookup` · `read_file` · `read_symbol` · `project_map` · `file_summary` · `context` |
| `leindex_analyze` | `mode` | `deep` (semantic + PDG traversal, default) · `impact` (transitive blast radius) · `diagnostics` · `git_status` · `git_diff` (PDG-enriched) |
| `leindex_edit` | `action` (required) | `preview` → `apply` (dry-run supported) · `rename` (atomic multi-file, preview-first) · `write` |
| `leindex_manage` | `action` | `index` (pollable job, default) · `phase` (5-phase architectural review) |

`find` is a native trigram index (the technique behind Zoekt) built with each
index run: about a millisecond on a 700-file repository, results always read from
the live file, and any directory outside the index is scanned on the fly with no
setup. Every branch takes `tier` (`l0` card · `l1` overview · `l2` full detail),
and argument detail lives in the `leindex://tools/guide` MCP resource. The
original per-tool names (`leindex_search`, `leindex_edit_apply`, …) still work
as direct calls.

Every tool is also on the CLI — the same handlers, the same output:

```bash
leindex tools list --verbose
leindex tools inspect leindex_explore
leindex tools run leindex_explore --set mode=find --set pattern=retry
leindex tools run leindex_explore --args '{"mode":"search","query":"retry policy","top_k":5}'
```

---

## 90%+ token savings for AI coding tools

Standard tools burn context reading whole files to find one function. LeIndex
returns the structured answer:

| Task | Standard tools | LeIndex | Savings |
|------|---------------:|--------:|--------:|
| Understand a 500-line file | ~2,000 tokens | ~380 tokens | **81%** |
| Find all callers of a function | ~5,800 tokens | ~420 tokens | **93%** |
| Navigate project structure | ~8,500 tokens | ~650 tokens | **92%** |
| Cross-file symbol rename | ~12,000 tokens | ~340 tokens | **97%** |

Each call is **context-aware, not atomic**: symbol lookups return callers,
callees, data dependencies, and impact radius; file summaries return
cross-file relationships; renames return a previewed multi-file diff. One
LeIndex call replaces chains of `Grep → Read → Read → Read`.

> Methodology: [`docs/TOOL_SUPREMACY_BENCHMARKS.md`](docs/TOOL_SUPREMACY_BENCHMARKS.md).

---

## Quick start

**Install** (pick one — then run `leindex setup` to provision the neural path):

```bash
# cargo (Rust users)
cargo install leindex
cargo install leindex --features onnx --force   # ensure the ONNX runtime is linked
leindex setup

# npm (Cursor / Claude Code / VS Code users)
npm install -g @leindex/mcp
npm run setup --prefix "$(npm root -g)/@leindex/mcp"

# PyPI (Python users; bootstraps the Rust binary via cargo)
pip install leindex && leindex setup

# prebuilt release bundle
curl -fsSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.sh -o install-leindex.sh
bash install-leindex.sh && leindex setup
```

The setup wizard picks CPU, NVIDIA CUDA, or AMD ROCm/MIGraphX, installs the
matching ONNX Runtime, and downloads the model into `~/.leindex/models/`.
TF-IDF and the PDG are always built first and stay queryable even if the
neural provider is unavailable — neural scoring attaches when the worker is
ready and falls back cleanly when it is not. Default model: CodeRankEmbed
137M INT8 (~135 MiB); `leindex setup --model qwen3` selects the heavier
Qwen3 baseline (see [`BENCHMARKS.md`](BENCHMARKS.md) §8 for the bake-off).

**Index and search:**

```bash
leindex index /path/to/project     # ~seconds; re-indexes are delta-priced
leindex search "authentication flow"
leindex analyze "how authorization is enforced"
```

**Connect your agent** (MCP stdio):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["-y", "@leindex/mcp"]
    }
  }
}
```

Replace `npx -y @leindex/mcp` with `leindex mcp` if you installed the binary
directly. Zed, Cursor, VS Code, and Claude Code configuration snippets are in
[`docs/MCP.md`](docs/MCP.md). Long-running servers self-exit after
`[mcp] idle_timeout_secs` (default 1800) and evict idle engines after
`engine_max_idle_secs` (default 600).

**Environment variables:** `LEINDEX_HOME` (storage root, default
`~/.leindex`), `LEINDEX_PORT` (HTTP server, default 47500), `ORT_DYLIB_PATH`
(ONNX Runtime override). Memory rails — `LEINDEX_WORKER_MAX_RSS_MB` (default
10240), `LEINDEX_WORKER_MIN_AVAILABLE_MB` (default 2048),
`LEINDEX_REGISTRY_MAX_HEAP_MB` (default 1536), the Engram bounds
`LEINDEX_ENGRAM_MAX_ENTRIES` (20000) and `LEINDEX_ENGRAM_MAX_MB` (256) — plus the escape hatches
`LEINDEX_CLI_SHUTDOWN_DAEMON`, `LEINDEX_ALLOW_MULTIPLE_EMBED_DAEMONS`, and
the `LEINDEX_FEATURE_*` rollout kills documented in
[`docs/NEURAL_SETUP.md`](docs/NEURAL_SETUP.md).

---

## How it works

Tree-sitter parses every source file into symbols that feed two layers over
the same nodes: a TF-IDF lexical corpus and a Program Dependence Graph
(call edges, data flow, containment). The configured neural worker embeds
the same nodes for semantic scoring, joined after readiness — terminal
failure preserves the complete TF-IDF/PDG result.

```
Codebase → Tree-sitter → PDG + TF-IDF (core, always) → neural join → hybrid ranking
```

- **Indexing is resumable and delta-priced.** Phase checkpoints
  (scan → parse → pdg → lexical → neural) resume after interruption;
  re-indexes only re-embed changed content; unchanged PDG edges write
  nothing (`save_pdg` diffs at the edge level).
- **Generations are immutable and content-addressed.** Publication uses a
  staging→promote pattern with blake3-hashed layers; no-op reindexes are
  byte-identical, and `leindex retention --gc` keeps the store bounded.
- **Memory is bounded by construction.** Streaming pipeline, admission
  control (admit/defer/reduce — never error), a single RSS-capped embed
  daemon with idle eviction, and byte-budgeted registry eviction.

---

## Use cases

- **Understanding unfamiliar codebases** — ask questions instead of reading every file
- **Onboarding** — find relevant code without tribal knowledge
- **Legacy exploration** — surface logic buried in decades of code
- **AI coding assistants** — give LLMs real structural context over MCP
- **Refactoring with confidence** — impact analysis and previews before touching disk
- **Cross-project search** — query multiple repositories at once

---

## Under the hood

- **18 MCP tools** — search, navigation, analysis, and safe editing (preview/dry-run/preview-only defaults on every destructive path)
- **Honest degradation everywhere** — direction labels on impact figures, notes when enrichment was skipped, `signature_scope` on incremental counts, freshness footers separated from machine-readable JSON
- **5-phase analysis** — structural scan, dependency map, logic flow, critical path, synthesis; freshness-aware and incremental
- **Dashboard** — Bun + React operational UI with project metrics and graph telemetry
- **HTTP + WebSocket server** — `leindex serve`
- **Flexible embedding backends** — TF-IDF, local ONNX models, or remote providers (OpenAI, Cohere)
- **Built in Rust** — fast, low-memory, safe concurrency

---

## Learn more

- [`BENCHMARKS.md`](BENCHMARKS.md) — resource benchmarks, model bake-off, acceptance-gate evidence
- [`docs/NEURAL_SETUP.md`](docs/NEURAL_SETUP.md) — CPU/GPU/AMD/NVIDIA provider setup and troubleshooting
- [`docs/MCP.md`](docs/MCP.md) — MCP integration for every major client
- [`CHANGELOG.md`](CHANGELOG.md) — full release history, including the v2.0.0 post-release fix rounds
- [`ARCHITECTURE.md`](ARCHITECTURE.md) / [`RUST_ARCHITECTURE.md`](RUST_ARCHITECTURE.md) — system design

## License

MIT OR Apache-2.0 — see [LICENSE](LICENSE).
