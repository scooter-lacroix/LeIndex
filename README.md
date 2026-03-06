<div align="center">

<img src="leindex.jpeg" alt="LeIndex" width="500"/>

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=flat-square&logo=rust)](https://www.rust-lang.org/)
[![License](https://img.shields.io/badge/License-MIT%20%7C%20Apache--2.0-blue?style=flat-square)](LICENSE)
[![MCP](https://img.shields.io/badge/MCP-Server-purple?style=flat-square)](https://modelcontextprotocol.io)

**Understand large codebases instantly.**

LeIndex is a semantic code intelligence engine that lets you search code by **meaning**, not just keywords. 

Instead of digging through files with grep, you can ask:
* "Where is authentication enforced?"
* "How does session validation work?"
* "What code handles rate limiting?"

LeIndex surfaces the real implementation—even if those exact words never appear in the code.

</div>

---

## The "Aha" Example

```bash
leindex search "where is authentication enforced"
```

**Possible results:**
```text
src/middleware/auth.rs
src/security/session_validator.rs
src/api/login_handler.rs
```

LeIndex finds relevant code based on intent and structure, not just matching strings.

## Quick Start

Index a project:
```bash
leindex index /path/to/project
```

Search your code:
```bash
leindex search "authentication flow"
```

Run deeper analysis:
```bash
leindex analyze "how authorization is enforced"
```

## Use Cases

LeIndex is useful for:
* Understanding unfamiliar repositories
* Onboarding onto large codebases
* Analyzing legacy systems
* Supporting AI coding assistants
* Cross-repository code discovery

## Why This Matters

Modern codebases are too large for traditional search tools. AI coding assistants also struggle with large repositories because they lack true structural understanding. LeIndex bridges this gap by providing semantic indexing and structured code analysis that both developers and AI tools can natively query.

## What LeIndex Does

LeIndex helps you understand large codebases faster by providing:
* Semantic code search
* Deep code analysis
* Dependency and impact tracing
* Cross-project indexing
* AI tool integration via MCP

### How it Works (Under the Hood)
For the technically curious, LeIndex achieves this using:
* Tree-sitter parsing for syntax awareness
* Program Dependency Graph (PDG) analysis
* Semantic embeddings and vector search
* Fast, low-resource Rust indexing pipelines

## MCP Integration

LeIndex exposes tools for AI coding assistants via the Model Context Protocol (MCP). This allows tools like Claude, Cursor, or Codex to natively search code semantically, analyze dependencies, retrieve context, safely preview edits, and apply structured changes.

<details>
<summary><b>View all 16 MCP Tools</b></summary>

* `leindex_index`
* `leindex_search`
* `leindex_deep_analyze`
* `leindex_context`
* `leindex_diagnostics`
* `leindex_phase_analysis`
* `phase_analysis` (alias)
* `leindex_file_summary`
* `leindex_symbol_lookup`
* `leindex_project_map`
* `leindex_grep_symbols`
* `leindex_read_symbol`
* `leindex_edit_preview`
* `leindex_edit_apply`
* `leindex_rename_symbol`
* `leindex_impact_analysis`
</details>

## Dashboard Observability

LeIndex includes a local observability dashboard to give you deep visibility into your code intelligence engine. It provides:
* Codebase inventory and project indexing metrics
* Dependency graph volume and telemetry
* Cache performance and hit-rate snapshots
* External dependency counters
* Real-time analysis events over WebSocket

To start the dev server:
```bash
leindex dashboard
```

## Architecture & Workspace Layout

LeIndex v1.5.0 is built as a Rust workspace to separate concerns cleanly:

* `leparse`: language parsing and signature extraction
* `legraphe`: graph construction and traversal
* `lerecherche`: retrieval / scoring / vector search internals
* `lestockage`: SQLite persistence + storage primitives
* `lephase`: additive phase analysis pipeline
* `lepasserelle`: CLI + MCP protocol handlers
* `leglobal`: cross-project discovery/registry support
* `leserve`: HTTP/WebSocket API server
* `leedit`: edit-preview/apply support
* `levalidation`: validation and guardrails

## Install

### Option A: One-line installer (Recommended)
```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/master/install.sh | bash
```
*Installs the CLI to `/usr/local/bin` and dashboard assets to `~/.leindex/dashboard`.*

### Option B: Crates.io
```bash
cargo install leindex
```
*Note: crates.io installs the CLI/MCP binaries. Dashboard assets are distributed from repository installs rather than bundled in the crate artifact.*

## Development

```bash
# Build and test backend
cargo build --workspace
cargo test --workspace

# Build frontend dashboard
cd dashboard
bun install
bun run build
```

## Docs
* [ARCHITECTURE.md](ARCHITECTURE.md)
* [API.md](API.md)
* [docs/MCP.md](docs/MCP.md)
* [dashboard/README.md](dashboard/README.md)

## License
MIT OR Apache-2.0
