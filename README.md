<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)
[![Release](https://raw.githubusercontent.com/scooter-lacroix/LeIndex/badges/version-badge.svg)](https://github.com/scooter-lacroix/LeIndex/actions/workflows/release.yml)

</div>

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

LeIndex ships three first-class install paths. Pick one, then run `leindex setup`
to enable neural (semantic) search. TF-IDF (keyword) search works immediately
without setup.

**Option 1: cargo (recommended for Rust users)**

```bash
cargo install leindex
leindex setup
```

`cargo install` places both `leindex` and `leindex-embed` in `~/.cargo/bin/`.
The `setup` wizard installs ONNX Runtime via pip and downloads the
`qwen3-embed-0.6b.onnx` model to `~/.leindex/models/`.

**Option 2: npm (recommended for AI tools like Cursor, Claude Code, VS Code)**

```bash
npm install -g @leindex/mcp
npm run setup --prefix "$(npm root -g)/@leindex/mcp"
```

The npm package downloads a platform-specific bundle containing the main
binary, the ONNX worker (`leindex-embed`), bundled ORT libraries, and model
assets. `npm run setup` invokes the bundled `leindex setup` wizard.

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

The install script downloads a pre-built release bundle (binaries + bundled ORT
`lib/` + model assets), copies them into `~/.leindex/` and `~/.cargo/bin/`, then
runs `leindex setup --check` to report status. No Rust toolchain required.

> **Neural vs. TF-IDF**: TF-IDF (keyword) search works out of the box with no
> setup. `leindex setup` enables neural (semantic) search by installing ONNX
> Runtime and downloading the embedding model. See
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

It builds a semantic index of your repository that both developers and AI assistants can query to understand:

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
- **20 MCP tools** — read, analyze, edit preview/apply, rename, impact analysis
- **HTTP + WebSocket server** — available through the unified `leindex` server modules and commands
- **Dashboard** — Bun + React operational UI with project metrics and graph telemetry
- **Low resource mode** — works on constrained hardware
- **Built in Rust** — fast indexing, low memory, safe concurrency
- **Flexible embedding backends** — choose between TF-IDF, local ONNX models (`qwen3-embed-0.6b`), or remote cloud providers (OpenAI, Cohere)

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

Every MCP tool is also available from the CLI bridge:

```bash
leindex tools list
leindex tools help leindex-project-map
leindex tools run leindex-project-map --args '{"path":"src","depth":2}'
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
cargo build --release --features onnx
./target/release/leindex setup          # install ONNX Runtime + download qwen3-embed-0.6b model
```

The `onnx` feature uses the `load-dynamic` ORT strategy: no ONNX Runtime is
linked at build time, and the worker discovers the runtime shared library at
startup via a discovery chain (`ORT_DYLIB_PATH` env, config, `~/.leindex/lib/`,
sibling-to-binary, pip site-packages, system paths). See
[docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md) for the full chain and
troubleshooting.

Local models provide:
- Privacy (data never leaves your machine)
- No API costs
- Zero network latency
- Support for `qwen3-embed-0.6b` (default) and optional Qwen3-Reranker-0.6B
- Worker-sidecar ONNX inference keeps main process memory low

The worker binary (`leindex-embed`) is built alongside the main binary and is
discovered automatically at runtime. `leindex setup` installs the ONNX Runtime
pip package and downloads model assets to `~/.leindex/models/`.

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

If no embedding backend is configured (i.e. `leindex setup` has not been run),
LeIndex falls back to TF-IDF for keyword-based search. This works offline with
zero setup but lacks semantic understanding. A one-time notice points to
`leindex setup` to enable neural search.

---

## MCP Tools (20)

| Tool | Purpose |
|------|---------|
| `LeIndex [Context]` | Expand context around a code node via PDG |
| `LeIndex [Deep Analyze]` | Deep analysis: semantic + PDG traversal |
| `LeIndex [Diagnostics]` | Index health and stats |
| `LeIndex [Edit Apply]` | PRIMARY file editor (use instead of `edit_file`) |
| `LeIndex [Edit Preview]` | Preview a code edit with impact report |
| `LeIndex [File Summary]` | Structural file analysis |
| `LeIndex [Git Status]` | Git status with PDG structural analysis |
| `LeIndex [Grep Symbols]` | Structural symbol search |
| `LeIndex [Impact Analysis]` | Blast radius analysis |
| `LeIndex [Index]` | Index a project |
| `LeIndex [Phase Analysis]` | 5-phase additive analysis |
| `Phase Analysis` | Compatibility alias for `LeIndex [Phase Analysis]` (same handler, no-bracket title for legacy clients) |
| `LeIndex [Project Map]` | Annotated project structure |
| `LeIndex [Read File]` | PRIMARY file reader (replaces `Read`) |
| `LeIndex [Read Symbol]` | PRIMARY symbol reader (replaces `Read` for symbols) |
| `LeIndex [Rename Symbol]` | Rename across all references |
| `LeIndex [Search]` | Semantic code search |
| `LeIndex [Symbol Lookup]` | Symbol definition + callers/callees |
| `LeIndex [Text Search]` | PRIMARY text search (replaces `Grep`/`rg`) |
| `LeIndex [Write]` | Create or overwrite a file |

MCP tool names returned by `tools/list` are the exact strings emitted
by each handler (e.g. `leindex.index`, `leindex.search`,
`leindex.edit-preview`, `leindex.write`). The naming is a **mix** of
dotted and hyphenated forms — single-word tools use a dot
(`leindex.context`, `leindex.index`, `leindex.search`, `leindex.write`,
`leindex.diagnostics`), multi-word tools use hyphens
(`leindex.edit-preview`, `leindex.edit-apply`, `leindex.read-file`,
`leindex.symbol-lookup`, `leindex.phase-analysis`, etc.). Use these
exact names when calling `tools/call` — dispatch in
`handle_tool_call` is exact-equality on the handler name, so a
hyphen-vs-dot mismatch (e.g. `leindex-search` vs `leindex.search`)
returns `method-not-found`. The display form above (`LeIndex [...]`)
is the human-readable title; it is not accepted on the wire. The
underscore form (`leindex_edit_preview`) is only used by the CLI
bridge (`leindex tools help`, `leindex tools run`).

### Output formatting

- **MCP payloads** are trimmed to the minimum needed for an LLM: short
  snippets, capped counts, dropped internal byte ranges and verbose
  fields. No ANSI color, no UI chrome.
- **CLI output** is rendered for human reading: split-view color diffs
  for `LeIndex [Edit Preview]`, `LeIndex [Edit Apply]`, and
  `LeIndex [Rename Symbol]` (line numbers + `│` separator + paired
  `+`/`-` markers); tree-style map for `LeIndex [Project Map]`;
  structured tables for `LeIndex [Search]` and `LeIndex [Context]`.

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
- [docs/NEURAL_SETUP.md](docs/NEURAL_SETUP.md) — neural search setup and troubleshooting (CPU/GPU/AMD/NVIDIA)
- [dashboard/README.md](dashboard/README.md) — dashboard setup

---

## License

MIT
