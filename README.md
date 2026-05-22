<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)
[![Release](https://github.com/scooter-lacroix/LeIndex/actions/workflows/release.yml/badge.svg?event=release)](https://github.com/scooter-lacroix/LeIndex/actions/workflows/release.yml)

</div>

## 🧠 Validated Architecture (Plans 0–3)

LeIndex has completed a validated, multi-phase memory-reduction roadmap:

**Plan 0 — Measurement Foundation** ✅  
- `tools/memcheck`: In-process RSS harness with `/proc`-sampled (VmRSS/smaps).  
  All 9 canonical memory phases pass; `cargo xtask memcheck` is CI-enforced.  
- `docs/memory/baselines/small_repo/*`: JSON baseline per phase, 5%/10% regression gate.  
- `docs/memory/budgets/current.json`: Single source of truth for absolute ceilings.  
- `.github/workflows/memory-budget.yml`: PR memory gate + baseline override guard.  
- `--memory-report=PATH` CLI surface and `LEINDEX_MEMORY_REPORT` env var (opt-in JSON summary on graceful shutdown).  
- `#[cfg(feature = "memprof")]` jemalloc heap profiling opt-in.  
  **28/28 assertions validated** (VAL-MEASURE-001–028).

**Plan 1 — A+ Runtime / Feature-Graph Slimming** ✅  
- Tokio minimal feature set (removed `libsql`/`turso` from `full`; Turso behind opt-in `turso` feature gate).  
- MCP server unified to axum 0.7 / tower 0.5 (removed legacy 0.6 aliases).  
- SQLite thin cache/mmap budgets: global registry 2 MiB, project store 64 MiB mmap, worker cache 16 MiB.  
- Memory defaults tightened: `spill_threshold` 0.75, `max_cache_bytes` 96 MiB.  
- Edit-preview hard caps (256 KiB entry / 8 MiB total LRU), search cache hard bounds (256 entry / 16 MiB bytes + synchronous eviction).  
- ONNX idle unload + MCP/registry hotspot cleanup.  
- NodeInfo legacy payload compatibility bridge (bidirectional).  
- **39/39 A+ assertions validated** (VAL-APLUS-001–039; APLUS-022–024 explicitly blocked [structural supersession — ONNX lifecycle tests removed after Plan 3 worker refactoring is covered by VAL-CROSS-001–003).

**Plan 2 — B+ Row-Oriented Residency** ✅  
- Borrowed mmap vector access (`&[f32]` lifetime from mmap, no heap mirror).  
- Stable row lookup with append-only tombstones and threshold-driven compaction.  
- Heap-to-mmap swap (pre-compaction and post-compaction).  
- INT8 quantization gated by NDCG/latency thresholds (not automatic).  
- `validate_coherence` two-pass invariant (sorted by row, then by embedding).  
- Bounded internal caches unified under a byte ceiling.  
- Snapshot-state clone reduction. `CompactNodeMetadata` for version-relative lookups.  
- Reader-pool and bounded back-pressure support (20 active + 80 queued with `Semaphore`).  
- Staged retrieval: coarse candidate generation + exact rerank.  
- **45/45 B+ assertions validated** (VAL-BPHASE-001–045).

**Plan 3 — C+ Process / Worker Architecture** ✅  
- `leindex-embed` worker crate (`crates/leindex-embed`) with IPC (Unix domain socket + bincode streams).  
- Worker lifecycle: cold start → warm reuse → idle teardown → full restart. `build_startup_report` surfaces 6 fields in log output at warn level.  
- Main-side `EmbeddingModelClient`: async retry-once fallback then TF-IDF-based degradation for the affected batch. ONNX not owned by main daemon in steady state.  
- Model path resolution precedence: `LEINDEX_WORKER_MODEL_PATH` > `--model-path` env > bundled default. Execution-provider selection via `LEINDEX_WORKER_EXECUTION_PROVIDER`.  
- Model bundle pipeline (`scripts/download-models.sh` + `LEINDEX_QUANTIZE`): downloadable ONNX + JSON tokenizer, checksum gate.  
- Worker-aware revision: per-phase `worker_rss_max_kib` and `combined_rss_max_kib` in `PhaseReport`; embed_idle + embed_active + embed_teardown canonical phases; memory CI extended to worker-active window. RELEASE SLICE MATCH EXACTLY; NO BOUNDARY CHANGES.  
- Release: cross-platform CI (Linux/macOS/Windows) publishes crates.io + npm + PyPI; release workflow enforces version parity across Cargo/npm/PyPI before release. Shell installer builds main + worker binaries.  
- MCP output normalized: tool summaries and docs updated across npm MCP, R15 docs, and MCP.md.  
- **42/43 C+ assertions validated** (VAL-CPHASE-001–042; CPHASE-009/010/011 explicitly blocked pending runtime facility in current environment — the worker code and path fix are in place but full end-to-end facility verification requires a CUDA-containing runtime).

**Memory targets (within A+ bands):** idle_warm ~9852 KiB, index ~20168 KiB, query ~13480 KiB.

# LeIndex

**Understand large codebases instantly.**

LeIndex is a semantic code search engine that lets you search code by **meaning**, not just keywords.

Instead of hunting through files with grep or hoping variable names match your query, you can ask things like:

- *"Where is authentication enforced?"*
- *"Where are API tokens validated?"*
- *"How does session management work?"*

LeIndex surfaces the actual implementation — even if the words you're searching for never appear in the code.

Built in Rust. Built for developers and AI coding tools.

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

**Via cargo (recommended):**

```bash
cargo install leindex
```

**Via install script:**

```bash
curl -fsSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.sh -o install-leindex.sh
bash install-leindex.sh
```

The install script builds and installs both `leindex` and `leindex-embed` (ONNX worker), plus bundled model assets.

**Via PyPI bootstrap wrapper:**

```bash
pip install leindex
leindex --version
```

The PyPI package installs a small Python launcher. On first run it installs or updates
the real Rust `leindex` binary in `~/.cargo/bin` via `cargo install leindex`. If Cargo
is missing, the launcher explains the requirement and prompts to install Rust/Cargo when
automatic setup is supported on the current platform.

**Via npm MCP wrapper (recommended for AI tools):**

```bash
npm install -g @leindex/mcp
```

The npm package downloads a platform-specific bundle containing the main binary, the ONNX worker (`leindex-embed`), and model assets.

**Environment Variables:**

| Name | Required | Description | Default |
|------|----------|-------------|---------|
| `LEINDEX_HOME` | No | Override storage/index home directory | `~/.leindex` |
| `LEINDEX_PORT` | No | Override HTTP server port | `47268` |

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

It builds a semantic index of your repository that both developers and AI assistants can query to understand:

- where logic lives
- how components interact
- what code paths enforce behavior

LeIndex runs as an **MCP server**, allowing tools like **Claude Code**, **Cursor**, and other MCP-compatible agents to explore your codebase with semantic understanding.

```bash
# Start MCP stdio mode (for Claude Code / Cursor)
leindex mcp

# Or run the HTTP MCP server
leindex serve --host 127.0.0.1 --port 47268
```

```text
Claude: "Where is request validation implemented?"

LeIndex MCP → src/http/request_validator.rs
              src/middleware/input_guard.rs
```

---

## How It Works

LeIndex builds a semantic index of your codebase using embeddings and structural analysis (tree-sitter parsing + program dependence graphs).

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

- **Semantic search** — find code by meaning, not keywords
- **PDG analysis** — program dependence graph for structural understanding
- **5-phase analysis** — additive multi-pass codebase analysis pipeline
- **Cross-project indexing** — search across multiple repos at once
- **16 MCP tools** — read, analyze, edit preview/apply, rename, impact analysis
- **HTTP + WebSocket server** — available through the unified `leindex` server modules and commands
- **Dashboard** — Bun + React operational UI with project metrics and graph telemetry
- **Low resource mode** — works on constrained hardware
- **Built in Rust** — fast indexing, low memory, safe concurrency
- **Flexible embedding backends** — choose between TF-IDF, local ONNX models, or remote cloud providers (OpenAI, Cohere)

---

## Other Install Options

### crates.io

```bash
cargo install leindex
```

### PyPI

```bash
pip install leindex
```

This package is a bootstrap wrapper for the Rust release. It keeps using the unified
`leindex` command, installs the binary into `~/.cargo/bin`, and then forwards all CLI
arguments to the real Rust executable.

### From source

```bash
git clone https://github.com/scooter-lacroix/LeIndex.git
cd leindex
cargo build --release
```

This produces both `target/release/leindex` (main binary) and `target/release/leindex-embed` (ONNX worker). The worker must be discoverable alongside the main binary or in `PATH` for local ONNX inference.

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

Every MCP tool is also available from the CLI bridge:

```bash
leindex tools list
leindex tools help leindex_project_map
leindex tools run leindex_project_map --args '{"path":"src","depth":2}'
```

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
      "args": ["-y", "@leindex/mcp"],
      "type": "stdio"
    }
  }
}
```

Optional guidance pack:
- Install the shared skill from `integrations/skills/leindex-toolkit/` into `~/.claude/skills/leindex-toolkit/`
- Merge `integrations/claude-code/settings.example.json` to add the LeIndex reminder hook
</details>

<details>
<summary><b>Amp CLI (Sourcegraph)</b></summary>

Add to `~/.config/amp/settings.json`:

```json
{
  "amp.mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["-y", "@leindex/mcp"]
    }
  }
}
```
</details>

<details>
<summary><b>OpenCode</b></summary>

Add to `~/.config/opencode/opencode.json`:

```json
{
  "mcp": {
    "leindex": {
      "command": ["npx", "-y", "@leindex/mcp"],
      "type": "local"
    }
  }
}
```
</details>

<details>
<summary><b>Qwen CLI</b></summary>

Add to `~/.qwen/settings.json`:

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

<details>
<summary><b>iFlow CLI</b></summary>

Add to `~/.iflow/settings.json`:

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

<details>
<summary><b>Droid (Factory)</b></summary>

Add to `~/.factory/mcp.json` (note: requires `type: "stdio"`):

```json
{
  "mcpServers": {
    "leindex": {
      "type": "stdio",
      "command": "npx",
      "args": ["-y", "@leindex/mcp"]
    }
  }
}
```
</details>

<details>
<summary><b>Gemini CLI</b></summary>

Add to `~/.gemini/settings.json`:

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

Agent guidance packs:
- Claude Code: shared skill plus reminder hook
- Codex: install `integrations/skills/leindex-toolkit/` into `~/.codex/skills/leindex-toolkit/`
- Gemini CLI, Amp, OpenCode, Qwen, and iFlow: reuse the shared skill text as project instructions or agent rules
- Full instructions: `docs/AGENT_GUIDANCE.md`

<details>
<summary><b>Claude Desktop</b></summary>

macOS: `~/Library/Application Support/Claude/claude_desktop_config.json`
Windows: `%APPDATA%\Claude\claude_desktop_config.json`
Linux: `~/.config/Claude/claude_desktop_config.json`

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

### Dashboard (optional)

```bash
cd dashboard
bun install
bun run build
leindex dashboard
```

---

## Memory Measurement and Profiling

Plan 0 adds a lightweight memory measurement foundation so you can track LeIndex's RSS behavior without wiring up custom scripts.

- `cargo xtask memcheck` builds the release binary when needed, runs the canonical `small_repo` workload, compares the results against committed baselines and budget ceilings, and exits non-zero on regressions.
- The Linux CI workflow in `.github/workflows/memory-budget.yml` runs the same memcheck path and uploads the report artifact so baseline and budget enforcement stay consistent in automation.
- `--memory-report PATH` and `LEINDEX_MEMORY_REPORT=PATH` opt into a compact shutdown JSON with peak RSS and phase summaries; they stay off by default for normal runs.
- Build with `--features memprof` to enable the optional heap profiling surface for deeper memory investigations when the lightweight report is not enough.

---

## CLI Reference

```bash
leindex index /path/to/project       # Index a project
leindex search "query"                # Semantic search
leindex analyze "query"               # Deep structural analysis
leindex phase --all --path /path      # 5-phase additive analysis
leindex diagnostics                   # System health check
leindex mcp                           # MCP stdio mode
leindex serve                         # HTTP/WebSocket server
leindex dashboard                     # Launch dashboard UI
```

---

## Output Behavior

LeIndex is designed for **token-efficient** operation when used with AI coding tools.

### Clean Terminal Output

- **Default log level: `WARN`** — Routine operational chatter (storage paths, cache hits, PDG node counts, indexing progress) is suppressed. Only warnings and errors are shown.
- **Enable verbose diagnostics**: pass `--verbose` or set `RUST_LOG=debug` to see full DEBUG-level output for troubleshooting.

This keeps the terminal clean and minimizes token usage when LeIndex runs as a subprocess (e.g., via MCP stdio).

### Structured MCP Responses

MCP responses are **framed and structured** — transport-level errors (connection drops, protocol issues) never leak into the JSON-RPC response stream. The `leindex mcp` stdio mode produces clean, parseable JSON-RPC responses suitable for LLM consumption.

### Winit Event-Loop Coverage

`leindex analyze` and `leindex context` expand on-demand even when symbol names differ from query terms. If an exact lookup fails, LeIndex performs a fuzzy scan of the project's PDG to discover event-loop-heavy entrypoints (e.g., `run_event_loop`, `EventLoop::run`, `main`) using case-insensitive substring matching with complexity-aware scoring. This ensures framework-heavy codebases remain discoverable without requiring exact symbol names.

---

## Embedding Configuration

LeIndex supports multiple embedding backends for semantic search:

### Local ONNX Models (default)

Build with the default features to use local Qwen3 embedding models via ONNX Runtime. LeIndex uses a **worker-sidecar architecture** — the main `leindex` process delegates ONNX inference to a separate `leindex-embed` worker process, keeping the main daemon lightweight.

```bash
cargo build --release
```

Local models provide:
- Privacy (data never leaves your machine)
- No API costs
- Zero network latency
- Support for Qwen3-Embedding-0.6B and optional Qwen3-Reranker-0.6B
- Worker-sidecar ONNX inference keeps main process memory low

The worker binary (`leindex-embed`) is built alongside the main binary and is discovered automatically at runtime. Bundled model assets are shipped in the `models/` directory next to the binaries.

### Remote Cloud Providers

Build with the `remote-embeddings` feature to use cloud-based embedding services:

```bash
cargo build --release --features remote-embeddings
```

Supported providers:
- **OpenAI** (`text-embedding-3-small`, `text-embedding-3-large`)
- **Cohere** (`embed-english-v3.0`, `embed-multilingual-v3.0`)
- **Custom** (any OpenAI-compatible endpoint)

Configure via environment variables:

```bash
# OpenAI
export OPENAI_API_KEY="your-key"
# LeIndex will automatically use OpenAI embeddings

# Cohere
export COHERE_API_KEY="your-key"
# LeIndex will automatically use Cohere embeddings

# Custom provider
export LEINDEX_EMBEDDING_PROVIDER="custom"
export LEINDEX_EMBEDDING_API_KEY="your-key"
export LEINDEX_EMBEDDING_BASE_URL="https://your-endpoint.com/v1"
export LEINDEX_EMBEDDING_MODEL="your-model-name"
```

Remote embeddings offer:
- Higher accuracy with state-of-the-art models
- No local resource requirements
- Automatic model updates
- Multi-language support (Cohere)

**Note**: Remote embeddings require network access and API keys from your provider.

### TF-IDF Fallback

If no embedding backend is configured, LeIndex falls back to TF-IDF for keyword-based search. This is lightweight and works offline but lacks semantic understanding.

---

## MCP Tools (16)

| Tool | Purpose |
|------|---------|
| `leindex_index` | Index a project |
| `leindex_search` | Semantic code search |
| `leindex_deep_analyze` | Deep analysis with PDG traversal |
| `leindex_context` | Expand context around a symbol |
| `leindex_phase_analysis` | 5-phase additive analysis |
| `leindex_file_summary` | Structural file analysis |
| `leindex_symbol_lookup` | Symbol definition + callers/callees |
| `leindex_project_map` | Annotated project structure |
| `leindex_grep_symbols` | Structural symbol search |
| `leindex_read_symbol` | Read symbol source with deps |
| `leindex_edit_preview` | Preview edits with impact report |
| `leindex_edit_apply` | Apply code edits |
| `leindex_rename_symbol` | Rename across all references |
| `leindex_impact_analysis` | Blast radius analysis |
| `leindex_diagnostics` | Index health and stats |
| `phase_analysis` | Alias for phase analysis |

---

## Unified Module Layout

LeIndex is now a single crate with feature-gated modules:

| Module | Role |
|-------|------|
| `parse` | Language parsing and signature extraction |
| `graph` | Graph construction and traversal |
| `search` | Retrieval, scoring, vector search |
| `storage` | SQLite persistence + storage |
| `phase` | Additive phase analysis pipeline |
| `cli` | CLI + MCP protocol handlers |
| `global` | Cross-project discovery/registry |
| `server` | HTTP/WebSocket API server |
| `edit` | Edit preview/apply support |
| `validation` | Validation and guardrails |

Legacy crate-style aliases remain available from `leindex::leparse`, `leindex::legraphe`, and similar compatibility re-exports.

---

## Security

Database discovery (`LEINDEX_DISCOVERY_ROOTS`) is **opt-in only**. Sensitive directories (`.ssh`, `.aws`, `.gnupg`, etc.) are automatically excluded. All SQL operations use parameterized queries. See [ARCHITECTURE.md](ARCHITECTURE.md) for details.

---

## Docs

- [ARCHITECTURE.md](ARCHITECTURE.md) — system design and internals
- [API.md](API.md) — HTTP API reference
- [docs/MCP.md](docs/MCP.md) — MCP server documentation
- [dashboard/README.md](dashboard/README.md) — dashboard setup

---

## License

MIT
