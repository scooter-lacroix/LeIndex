<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)

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

## Quick Start (2 minutes)

### Install

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/master/install.sh | bash
```

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
- **HTTP + WebSocket server** — `leserve` with live event streaming
- **Dashboard** — Bun + React operational UI with project metrics and graph telemetry
- **Low resource mode** — works on constrained hardware
- **Built in Rust** — fast indexing, low memory, safe concurrency

---

## Other Install Options

### crates.io

```bash
cargo install leindex
```

### From source

```bash
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
cargo build --release --workspace
```

### Dashboard (optional)

```bash
cd dashboard
bun install
bun run build
leindex dashboard
```

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

## Workspace Layout

LeIndex v1.5.0 is organized as a Rust workspace:

| Crate | Role |
|-------|------|
| `leparse` | Language parsing and signature extraction |
| `legraphe` | Graph construction and traversal |
| `lerecherche` | Retrieval, scoring, vector search |
| `lestockage` | SQLite persistence + storage |
| `lephase` | Additive phase analysis pipeline |
| `lepasserelle` | CLI + MCP protocol handlers |
| `leglobal` | Cross-project discovery/registry |
| `leserve` | HTTP/WebSocket API server |
| `leedit` | Edit preview/apply support |
| `levalidation` | Validation and guardrails |

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
