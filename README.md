<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.85%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)
[![Release](https://raw.githubusercontent.com/scooter-lacroix/LeIndex/badges/version-badge.svg)](https://github.com/scooter-lacroix/LeIndex/actions/workflows/release.yml)

</div>

# LeIndex

**One daemon. One GiB. One hundred times less disk.**

LeIndex 2.0.0 is a semantic code intelligence engine built for multi-agent
development. Three concurrent agent harnesses now share one user-scoped
daemon instead of spawning three heavyweight processes. The result, measured
on this repository:

- **10 to 20 GiB RSS per harness is gone.** Aggregate steady-state RAM is at
  or below 1 GiB for three clients across two projects.
- **`.leindex/` is 15x smaller.** 2.9 GiB to 190 MiB on this repo; the 150
  GiB+ production case scales proportionally.
- **Indexing never errors on memory pressure.** The admission controller
  defers work instead of failing valid repos.
- **Retrieval quality is unchanged.** MRR@10 = 1.0000 against the v1.9.5
  baseline. Same symbols, same scores, same file paths.

Every number above is reproduced in [`BENCHMARKS.md`](BENCHMARKS.md) with the
methodology, the pre-v1.9.0 anchor, and the post-v2.0.0 baseline captured on
the identical corpus and hardware.

---

## Why v2.0.0

LeIndex 1.9.x had a resource crisis. Each agent harness spawned its own
`leindex mcp --stdio` process: three live processes were observed at 15.6 GiB,
10.0 GiB, and 1.2 GiB RSS concurrently. `.leindex/` occupied 2.5 GiB on a
419-file repo. Large projects saw 150 GiB+ footprints. The root causes were
architectural: per-harness process multiplication, allocator arena blowup,
corpus-wide materialization in the indexing pipeline, no content-addressed
dedup, and two FP16 models that could not fit a 1 GiB target.

v2.0.0 removes the bloat rather than rationing useful behavior:

| What changed | v1.9.x | v2.0.0 |
|---|---|---|
| Process model | N heavyweight `leindex mcp` processes | 1 user-scoped `leindexd` + N tiny stdio shims (~8 MiB each) |
| Generation storage | full-copy dirs (6 copies on disk) | content-addressed CAS blobs (zero duplication) |
| Indexing pipeline | materialize whole corpus per stage | streaming bounded chunks (RSS flat at 4 MiB) |
| Memory cap | `Err` when `--max-memory` exceeded | `Admit` / `Defer` / `Reduce`, never `Err` |
| Embed model | Qwen3 FP16 (1.19 GiB) + reranker (1.19 GiB) | CodeRankEmbed 137M INT8 (255 MiB), reranker removed |
| Job retention | unbounded historical accumulation (2 GiB / 115 jobs) | 128 MiB cap per project, completed jobs deleted on publish |

Read the full before/after resource story, including the model bake-off,
CAS engineering decisions, and section-16 acceptance-gate evidence, in
[`BENCHMARKS.md`](BENCHMARKS.md).

---

## Post-install fixes (v2.0.0 branch)

Five fixes were applied after the initial v2.0.0 release (commits
`fa24b963`..`cb336753`). Users running v2.0.0 should pull the latest on the
`v2.0.0` branch to pick up these corrections.

1. **ONNX embed batching (CRITICAL)** — A missing `else` branch in the embed
   batch loop caused CPU and CUDA providers to skip all sub-batches after the
   first. Neural search silently fell back to TF-IDF even when the ONNX worker
   reported `Ready`. Every sub-batch is now processed. Fixed-batch providers
   (MIGraphX/ROCm) now receive padding and trim to satisfy their fixed input
   shape requirement.

2. **PDG storage performance** — `save_nodes` and `save_edges` now use batched
   SQLite INSERTs (500 per statement). `SerializablePDG` serialization is
   clone-free via a borrowed-reference shim. `merge_pdgs` uses move semantics.
   The streaming PDG path routes through `build_fragment_from_parsed` directly,
   eliminating an intermediate allocation.

3. **Directory exclusion gaps** — `packages/` added to `SKIP_DIRS`. The git
   scan path now applies the same hidden-directory and `SKIP_DIRS`
   post-filtering as the non-git path. Previously, `node_modules` and other
   skip-listed directories inside git-tracked subtrees were indexed.

4. **Log flooding** — Duplicate `node_id` WARN downgraded to DEBUG. Daemon and
   worker default to WARN (was INFO). The daemon now respects `RUST_LOG` /
   `EnvFilter`. Per-file read INFO downgraded to DEBUG.

5. **Analysis output** — `format_analysis_output` now prints each search result
   with file path, symbol name, type, line number, and score. Context budget
   increased from 300 to 2000 characters. Results appear before the context
   section. Empty results display "No results found".

See [`CHANGELOG.md`](CHANGELOG.md) for the full entry.

---

## Performance & ONNX shape fixes (v2.0.0)

Three fixes were applied after the post-install round (commits
`ce8ab3f8`..`5cdb39e4`). Users running v2.0.0 should pull the latest on the
`v2.0.0` branch to pick up these performance and correctness improvements.

1. **ONNX batch shape fix (CRITICAL)** — Non-dynamic models (e.g.
   MIGraphX/ROCm) now correctly use `batch_size=1` instead of being
   incorrectly forced to `batch_size=8`. The previous behavior caused shape
   mismatch warnings and silent TF-IDF fallback on AMD GPU providers. The
   `-dynamic` suffix is now checked before applying the MIGraphX batch size
   override, so dynamic models retain their fixed-batch padding path while
   non-dynamic models use the correct single-row input shape.

2. **Storage performance** — SQLite `PRAGMA synchronous` is now `NORMAL`
   (was `FULL`), trading theoretical durability for a significant write
   throughput increase with negligible risk on local SSDs. Per-file operations
   (node/edge saves, fragment writes) are now wrapped in batch transactions
   instead of individual auto-commits, reducing fsync round-trips. A redundant
   double fsync in the fragment write path has been removed.

3. **Pipeline performance** — `enriched_node_content` is now computed once
   per node and cached (was recomputed three times per node across PDG
   construction, hashing, and embedding). PDG construction and file hashing
   are parallelized with rayon, utilizing all available CPU cores for the
   CPU-bound parsing and blake3 hashing stages.

See [`CHANGELOG.md`](CHANGELOG.md) for the full entry.

---

## Demo: finding logic that grep and LLMs miss

Imagine a codebase where authentication is implemented like this:

```rust
fn validate_session(req: Request) -> Result<User> { ... }
fn verify_token(token: &str) -> bool { ... }
fn authorize_user(user: &User, action: Action) -> bool { ... }
```

None of these functions contain the word **"authentication"**.

### grep

```bash
grep -r "authentication" src/
# (no matches)
```

### LeIndex

```bash
leindex search "where is authentication enforced"
```

```
src/security/session_validator.rs    validate_session    (0.92)
src/auth/token_verifier.rs           verify_token        (0.87)
src/middleware/auth_gate.rs           authorize_user      (0.84)
```

LeIndex finds the correct logic because it searches by **semantic intent**, not string matches.

It works across multiple repositories too:

```bash
leindex search "where are API rate limits enforced"
```

```
gateway/middleware/rate_limit.rs      throttle_request     (0.91)
api/server/request_throttle.go        limit_handler        (0.88)
auth/session_policy.rs                enforce_policy       (0.83)
```

---

## 90%+ Token Savings for AI Coding Tools

When an LLM reads your code with standard tools, it burns tokens on entire files just to understand one function. LeIndex returns **only what matters** — structured, context-aware results instead of raw file dumps.

| Task | Standard Tools | LeIndex | Savings |
|------|---------------:|--------:|--------:|
| Understand a 500-line file | ~2,000 tokens | ~380 tokens | **81%** |
| Find all callers of a function | ~5,800 tokens | ~420 tokens | **93%** |
| Navigate project structure | ~8,500 tokens | ~650 tokens | **92%** |
| Cross-file symbol rename | ~12,000 tokens | ~340 tokens | **97%** |

Every tool call is **context-aware** — not atomic. When you look up a symbol, you don't just get its definition. You get its callers, callees, data dependencies, and impact radius. When you summarize a file, you get cross-file relationships that `Read` can never provide at any token cost. One LeIndex call replaces chains of `Grep → Read → Read → Read`.

> See [full benchmarks](docs/TOOL_SUPREMACY_BENCHMARKS.md) for methodology and detailed comparisons.

---

## Quick Start (2 minutes)

### Install

LeIndex ships three first-class install paths. Pick one, then run `leindex setup`
to provision the default hybrid neural path. TF-IDF retrieval and PDG
relationships are always built first and remain queryable if a provider is
unavailable.

> **v2.0.0 default model.** The setup wizard provisions
> [CodeRankEmbed 137M](https://huggingface.co/) (INT8 quantized, ~135 MiB host
> RSS), selected through LeIndex's fused-retrieval evaluation. The
> [Qwen3 Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) FP16 model
> remains available via `leindex setup --model qwen3` for users who want the
> heavier baseline; see [`BENCHMARKS.md`](BENCHMARKS.md) Section 8 for the full
> bake-off.

**Option 1: cargo (recommended for Rust users)**

```bash
cargo install leindex
cargo install leindex --features onnx --force (to ensure appropriate onnx runtime is installed)
leindex setup
```

`cargo install` places both `leindex` and `leindex-embed` in `~/.cargo/bin/`.
The `setup` wizard selects CPU, NVIDIA CUDA, or AMD ROCm/MIGraphX, installs
the matching ONNX Runtime package, and downloads
[Qwen3 Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) from
Hugging Face via Hugging Face CLI into `~/.leindex/models/`. If `hf` is not
installed, setup installs `huggingface_hub` through Python/pip first.

**Option 2: npm (recommended for AI tools like Cursor, Claude Code, VS Code)**

```bash
npm install -g @leindex/mcp
npm run setup --prefix "$(npm root -g)/@leindex/mcp"
```

The npm package downloads a platform-specific bundle containing the main
binary, the ONNX worker (`leindex-embed`), and bundled ORT libraries.
`npm run setup` invokes the bundled `leindex setup` wizard to select the
provider and provision model files outside the npm package.

**Option 3: PyPI (recommended for Python users)**

```bash
pip install leindex
leindex setup
```

The PyPI package installs a small Python launcher that bootstraps the real Rust
`leindex` binary into `~/.cargo/bin` via `cargo install` on first run, then runs
`leindex setup` to configure neural search. If Cargo is missing, the launcher
explains the requirement and points to https://rustup.rs.

**Alternative: install script (GitHub Release bundle with zero-build install)**

```bash
curl -fsSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.sh -o install-leindex.sh
bash install-leindex.sh
leindex setup
```

The install script downloads a pre-built release bundle (binaries plus bundled
ORT `lib/`), copies it into `~/.leindex/` and `~/.cargo/bin/`, then runs
`leindex setup --check` to report status. Models are never shipped in release
artifacts; the explicit `leindex setup` command downloads the correct model.

> **Core plus neural**: TF-IDF and PDG structural retrieval are mandatory
> LeIndex result layers. With ONNX enabled, the default `auto` provider starts
> and awaits the neural worker during indexing and semantic retrieval, so
> results use all three signals. A terminal provider failure preserves the
> complete TF-IDF/PDG result. See
> [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md) for CPU/GPU/AMD/NVIDIA paths and
> troubleshooting.

**Environment Variables:**

| Name | Required | Description | Default |
|------|----------|-------------|---------|
| `LEINDEX_HOME` | No | Override storage/index home directory | `~/.leindex` |
| `LEINDEX_PORT` | No | Override HTTP server port | `47500` |
| `ORT_DYLIB_PATH` | No | Override ONNX Runtime library path | (discovered) |

### Index and search

```bash
# Index your project
leindex index /path/to/project

# Search by meaning
leindex search "authentication flow"

# Deep structural analysis
leindex analyze "how authorization is enforced"
```

That's it. You're searching by meaning.

---

## What LeIndex Is Useful For

- **Understanding unfamiliar codebases** — ask questions instead of reading every file
- **Onboarding new engineers** — find relevant code without tribal knowledge
- **Exploring legacy systems** — surface logic buried in decades of code
- **AI coding assistants** — give LLMs real structural context via MCP
- **Cross-project search** — query across multiple repositories simultaneously

---

## Built for AI-Assisted Development

Modern AI coding tools struggle with large codebases because they lack global structural context.

LeIndex provides that missing layer.

It builds one shared TF-IDF + PDG + neural index of your repository when ONNX
is enabled; the neural vectors are attached to those same nodes:

- where logic lives
- how components interact
- what code paths enforce behavior

LeIndex runs as an **MCP server**, allowing tools like **Claude Code**, **Cursor**, and other MCP-compatible agents to explore your codebase with semantic understanding.

```bash
# Start MCP stdio mode (for Claude Code / Cursor)
leindex mcp

# Or run the HTTP MCP server
leindex serve --host 127.0.0.1 --port 47500
```

```text
Claude: "Where is request validation implemented?"

LeIndex MCP → src/http/request_validator.rs
              src/middleware/input_guard.rs
```

---

## How It Works

LeIndex builds one node-level index of your codebase: tree-sitter symbols feed
the core TF-IDF lexical corpus and Program Dependence Graph (PDG); the
configured neural worker is actively evaluated for the same nodes and joined
to semantic scoring after it reaches `Ready` (terminal failure preserves the
TF-IDF/PDG core).

This allows queries to match:

- **code intent** — what the code does, not what it's named
- **related logic paths** — follow data flow and control flow
- **implementation patterns** — structural similarity across files

Indexes can span multiple repositories, enabling cross-project search.

```
Codebase → Tree-sitter Parser → PDG Builder → Semantic Index → Query Engine → Results
```

---

## Features

- **Single-daemon architecture (v2.0.0)** — one user-scoped `leindexd` serves all
  agent harnesses via tiny stdio shims (~8 MiB each). No more per-harness
  heavyweight processes. Aggregate steady-state RAM <= 1 GiB for three clients.
- **Content-addressed immutable generations (v2.0.0)** — generation layers are
  blake3-hashed CAS blobs referenced by mmap'd manifests. Two no-op reindexes
  produce byte-identical hashes; zero duplication across generations.
- **Streaming bounded indexing pipeline (v2.0.0)** — RSS is structurally
  independent of corpus size (max 60 KiB delta across all indexing phases).
  Reads never block on the writer Mutex via generation leases.
- **Defer, do not error (v2.0.0)** — the admission controller returns only
  `Admit` / `Defer` / `Reduce`. Valid repos that previously failed on memory
  pressure now defer and eventually complete.
- **Global content-addressed embedding cache (v2.0.0)** — cross-project dedup
  with 100% hit ratio on identical content. No duplicate ONNX inference.
- **Core hybrid retrieval** — TF-IDF lexical matching plus PDG structure on every applicable result
- **Hybrid neural scoring** — local ONNX similarity over the same symbols, with TF-IDF/PDG fallback
- **Validated model profile (v2.0.0)** — CodeRankEmbed 137M INT8 selected via
  fused-retrieval evaluation; reranker removed after ablation showed zero MRR
  contribution. Both fits within the 350 MiB embed worker budget.
- **Fragment embeddings (opt-in)** — sub-symbol semantic chunks (tree-sitter) + module-level orphan coverage, content-hash-addressed for idempotent incremental indexing; all-local, no remote service
- **5-phase analysis** — additive multi-pass codebase analysis pipeline
- **Cross-project indexing** — search across multiple repos at once
- **20 MCP tools** — read, analyze, edit preview/apply, rename, impact analysis
- **HTTP + WebSocket server** — available through the unified `leindex` server modules and commands
- **Dashboard** — Bun + React operational UI with project metrics and graph telemetry
- **Built in Rust** — fast indexing, low memory, safe concurrency
- **Flexible embedding backends** — choose between TF-IDF, local ONNX models (`coderank-embed-137m`, `qwen3-embed-0.6b`), or remote cloud providers (OpenAI, Cohere)

---

## Other Install Options

### crates.io

```bash
cargo install leindex
leindex setup          # enable neural search
```

### PyPI

```bash
pip install leindex
leindex setup          # enable neural search
```

This package is a bootstrap wrapper for the Rust release. It keeps using the unified
`leindex` command, installs the binary into `~/.cargo/bin`, and then forwards all CLI
arguments to the real Rust executable. Run `leindex setup` after install to configure
neural embeddings (see [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md)).

### From source

```bash
git clone https://github.com/scooter-lacroix/LeIndex.git
cd LeIndex
cargo build --release --features onnx
./target/release/leindex setup          # enable neural search
```

This produces both `target/release/leindex` (main binary) and `target/release/leindex-embed` (ONNX worker). The worker must be discoverable alongside the main binary or in `PATH` for local ONNX inference. The `--features onnx` flag enables the `load-dynamic` ONNX Runtime strategy: no ORT is linked at build time, and the worker discovers the runtime `.so`/`.dylib`/`.dll` at runtime via the discovery chain (see [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md)).

**Feature flags:** Use `--features` to customize the build:
- `full` (default) — Full library plus the `leindex` CLI binary
- `minimal` — Library-focused parse/search build slice; does not produce the `leindex` binary by itself
- `cli` — Required feature for the `leindex` binary target
- `server` — Enables the HTTP/WebSocket server library modules; combine with `cli` for a runnable binary

### MCP Server Integration

For AI coding tools, the recommended integration path is the npm MCP wrapper so the client
resolves the published MCP entrypoint directly:

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

If you intentionally installed the full Rust binary via `cargo install leindex`,
`install.sh`, or the PyPI bootstrapper, you can replace `npx -y @leindex/mcp`
with `leindex mcp`.

**Server lifecycle:** long-running MCP servers self-exit after `[mcp] idle_timeout_secs` (default `1800`; `0`=off) and evict idle loaded engines after `[mcp] engine_max_idle_secs` (default `600`) to avoid swap accumulation; override per-invocation with `--mcp-idle-timeout-secs`. See [docs/MCP.md](docs/MCP.md).

Every MCP tool is also available from the CLI bridge:

```bash
leindex tools list
leindex tools help leindex-project-map
leindex tools run leindex-project-map --args '{"path":"src","depth":2}'
```

`leindex.index` is an owned start/poll job. Its default response is a
`job_id` plus phase/status snapshot (`wait=false`); poll with the same
`job_id` or pass `wait=true` for an explicit blocking CLI-style call. MCP
requests do not cancel indexing, persistence, or publication at a wall-clock
deadline. Every applicable retrieval response reports core `tfidf_status` and
`pdg_status`; `neural_status` reports the configured neural provider state.

<details>
<summary><b>Zed IDE</b></summary>

Add to `~/.config/zed/settings.json`:

```json
{
  "context_servers": {
    "leindex": {
      "command": {
        "path": "npx",
        "args": ["-y", "@leindex/mcp"]
      }
    }
  }
}
```
</details>

<details>
<summary><b>Cursor IDE</b></summary>

Add to Cursor settings (`settings.json`):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["-y", "@leindex/mcp"],
      "env": {}
    }
  }
}
```
</details>

<details>
<summary><b>VS Code</b></summary>

Requires the [Model Context Protocol](https://marketplace.visualstudio.com/items?itemName=modelcontextprotocol.vscode-mcp) extension.

Configure in `settings.json`:

```json
{
  "mcp.mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["-y", "@leindex/mcp"]
    }
  }
}
```
</details>

<details>
<summary><b>Claude Code</b></summary>

Add to `~/.claude/settings.json` or project-local `.claude/settings.json`:

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
</details>
