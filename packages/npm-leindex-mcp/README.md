# @leindex/mcp

**LeIndex MCP Server - Auto-installing binary wrapper**

A lightweight npm package that automatically downloads and configures LeIndex for use as an MCP (Model Context Protocol) server in AI coding tools.

## Worker Architecture (Plan 3)

- **Version parity** with Cargo: npm package version matches `Cargo.toml` — bundles always stay in sync.
- **Worker bundle topology**: auto-downloads the platform-native `leindex` and `leindex-embed` binaries plus ONNX Runtime libraries; models are provisioned separately by setup.
- **Memory targets**: idle_warm ~9852 KiB, index ~20168 KiB, query ~13480 KiB (within A+ bands).
- **Install** (MCP): `npx -y @leindex/mcp`.

## What is This?

This package provides the **leanest** LeIndex distribution:
- ✅ MCP server functionality (stdio mode)
- ✅ Auto-downloads LeIndex binary bundle on install
- ✅ Includes ONNX worker binary (`leindex-embed`) for local semantic search
- ✅ Provisions [Qwen3 Embedding](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) from Hugging Face via Hugging Face CLI with `npm run setup`
- ✅ Works with Cursor, Claude Code, Zed, VS Code, and other MCP clients
- ❌ No dashboard
- ❌ No HTTP server (`leindex serve`)
- ❌ No CLI tools (`leindex search`, `leindex index`, etc.)

**Use this if:** You want LeIndex as an MCP server in your AI coding tool, managed entirely through npm.

**Don't use this if:** You need the full LeIndex CLI, dashboard, or HTTP server.

---

## Installation

### As an MCP Server (recommended)

Add to your MCP configuration in your AI tool:

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

The binary will be automatically downloaded on first use.

### As a Project Dependency

```bash
npm install --save-dev @leindex/mcp
# or
yarn add --dev @leindex/mcp
# or
pnpm add --save-dev @leindex/mcp
```

### Enabling Neural Search

TF-IDF lexical retrieval and PDG relationships are mandatory LeIndex result
layers. With ONNX enabled, the setup wizard provisions the default hybrid
neural scorer over the same nodes:

```bash
# project dependency
npm run setup --prefix node_modules/@leindex/mcp

# global install
npm run setup --prefix "$(npm root -g)/@leindex/mcp"
```

This invokes the bundled `leindex setup` command. It selects CPU, NVIDIA CUDA,
or AMD ROCm/MIGraphX, installs the matching ONNX Runtime when necessary, and
downloads Qwen3 Embedding from Hugging Face via Hugging Face CLI under
`~/.leindex/models/`. Model files are never stored in the npm package. The
bundled `lib/` directory provides the platform runtime baseline. See
[docs/NEURAL_SETUP.md](https://github.com/scooter-lacroix/LeIndex/blob/master/docs/NEURAL_SETUP.md)
for CPU/GPU/AMD/NVIDIA paths and troubleshooting.

CPU and CUDA use dynamic batches up to 32. MIGraphX uses a stable 8-by-128
profile warmed during setup and reused through its compiled cache and resident
worker. Indexing and semantic requests start/await the configured worker when
it is cold; terminal provider failure preserves the core TF-IDF/PDG result.

### Fragment Embeddings (1.11+, opt-in)

LeIndex 1.11 adds a fully-local **fragment embedding layer**: large symbols
are split into tree-sitter semantic chunks (plus module-level orphan regions)
and embedded with the same local Qwen3 worker. Fragments are
content-hash-addressed (blake3), so incremental indexing is idempotent and
deduplicated, and no remote service is involved. Enable it in
`~/.leindex/config/leindex.toml` under `[search]` with `fragment_index_enabled
= true` (plus `fragment_weight`, `fragment_max_bytes`,
`fragment_orphan_enabled`, `fragment_naive_fallback`). Off by default; the
node-level index remains authoritative.

---

## MCP Configuration Examples

### Cursor IDE

Add to Cursor settings (`~/.cursor/mcp.json` or Settings → MCP):

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

### Claude Code

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
- Install `integrations/skills/leindex-toolkit/` as a Claude Code skill
- Merge `integrations/claude-code/settings.example.json` to add the LeIndex reminder hook

### Zed IDE

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

### VS Code (with MCP extension)

Add to `.vscode/settings.json`:

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

### Agent Guidance Packs

- Claude Code: shared skill plus reminder hook
- Codex: install `integrations/skills/leindex-toolkit/` into `~/.codex/skills/leindex-toolkit/`
- Gemini CLI, Amp, OpenCode, Qwen, and iFlow: reuse the shared skill text as project instructions or agent rules
- Full instructions: `docs/AGENT_GUIDANCE.md`

### Claude Desktop

Add to Claude Desktop config:

- **macOS**: `~/Library/Application Support/Claude/claude_desktop_config.json`
- **Windows**: `%APPDATA%\Claude\claude_desktop_config.json`
- **Linux**: `~/.config/Claude/claude_desktop_config.json`

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

---

## Comparison: NPM Package vs Full Installation

| Feature | `@leindex/mcp` (npm) | `cargo install leindex` (full) |
|---------|---------------------|-------------------------------|
| **MCP Server** | ✅ Yes | ✅ Yes |
| **Auto-install** | ✅ Downloads on npm install | ❌ Manual install |
| **ONNX Worker** | ✅ Bundled (`leindex-embed`) | ✅ Built from source |
| **Model Assets** | Installed by `npm run setup` | Installed by `leindex setup` |
| **Dashboard** | ❌ No | ✅ Yes (`leindex dashboard`) |
| **HTTP Server** | ❌ No | ✅ Yes (`leindex serve`) |
| **CLI Tools** | ❌ No | ✅ Yes (`leindex search`, `index`, etc.) |
| **Update Method** | `npm update` | `cargo install leindex` |
| **Best For** | AI tool integration | Full development workflow |

### When to Use NPM Package

- You're using LeIndex **exclusively** through an MCP client (Cursor, Claude Code, etc.)
- You want **automatic updates** through npm
- You don't need the CLI, dashboard, or HTTP server
- You want the **leanest** installation

### When to Use Full Installation

- You use LeIndex **CLI tools** directly
- You need the **dashboard** for project metrics
- You want to run the **HTTP server** for remote access
- You prefer **cargo/rust** ecosystem for management

---

## How It Works

1. **On `npm install`**: The postinstall script downloads the platform-specific LeIndex bundle with the main binary, `leindex-embed`, and ONNX Runtime libraries.
2. **Model setup**: `npm run setup` downloads Qwen3 through Hugging Face CLI into `$LEINDEX_HOME/models`, outside `node_modules`.
3. **MCP mode**: `npx -y @leindex/mcp` launches LeIndex over stdio. Unix requests reuse a resident local embedding worker.
4. **Updates**: The installer resolves the GitHub `latest` release and verifies the bundle against `SHA256SUMS`.
5. **Fallback**: Older releases can fall back to a bare main binary; an unreachable release endpoint falls back to `cargo install`.

To pin a specific binary release instead of `latest`:

```bash
LEINDEX_BINARY_VERSION=1.9.0 npm install @leindex/mcp
```

---

## Requirements

- **Node.js**: >= 16.0.0
- **Platforms**: macOS, Linux, Windows
- **Architectures**: x64, arm64

---

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `LEINDEX_HOME` | Storage directory for indexes | `~/.leindex` |

Note: Unlike the full installation, this package does not use `LEINDEX_PORT` (no HTTP server).

---

## Troubleshooting

### Binary Not Found

```bash
# Re-run install script
npm install

# Or manually download
cargo install leindex
```

### Permission Denied (Linux/macOS)

```bash
chmod +x node_modules/@leindex/mcp/bin/leindex
```

### Platform Not Supported

The npm package supports:
- macOS (x64, arm64)
- Linux (x64, arm64)
- Windows (x64, arm64)

For other platforms, install via cargo:
```bash
cargo install leindex
```

---

## Available MCP Tools

Once configured, your AI tool can use these LeIndex tools (full list of
20 — see [root README](https://github.com/scooter-lacroix/LeIndex#mcp-tools-20)
for the complete table):

| Display name | Internal name | Purpose |
|---|---|---|
| `LeIndex [Context]` | `leindex.context` | Expand context around a code node via PDG |
| `LeIndex [Deep Analyze]` | `leindex.deep-analyze` | Deep analysis: semantic + PDG traversal |
| `LeIndex [Diagnostics]` | `leindex.diagnostics` | Index health and stats |
| `LeIndex [Edit Apply]` | `leindex.edit-apply` | PRIMARY file editor (use instead of `edit_file`) |
| `LeIndex [Edit Preview]` | `leindex.edit-preview` | Preview a code edit with impact report |
| `LeIndex [File Summary]` | `leindex.file-summary` | Structural file analysis |
| `LeIndex [Git Status]` | `leindex.git-status` | Git status with PDG structural analysis |
| `LeIndex [Grep Symbols]` | `leindex.grep-symbols` | Structural symbol search |
| `LeIndex [Impact Analysis]` | `leindex.impact-analysis` | Blast radius analysis |
| `LeIndex [Index]` | `leindex.index` | Index a project |
| `LeIndex [Phase Analysis]` | `leindex.phase-analysis` | 5-phase additive analysis |
| `Phase Analysis` | `leindex.phase-analysis` | Compatibility alias for `LeIndex [Phase Analysis]` (same handler, no-bracket title for legacy clients) |
| `LeIndex [Project Map]` | `leindex.project-map` | Annotated project structure |
| `LeIndex [Read File]` | `leindex.read-file` | PRIMARY file reader (replaces `Read`) |
| `LeIndex [Read Symbol]` | `leindex.read-symbol` | PRIMARY symbol reader (replaces `Read` for symbols) |
| `LeIndex [Rename Symbol]` | `leindex.rename-symbol` | Rename across all references |
| `LeIndex [Search]` | `leindex.search` | Semantic code search |
| `LeIndex [Symbol Lookup]` | `leindex.symbol-lookup` | Symbol definition + callers/callees |
| `LeIndex [Text Search]` | `leindex.text-search` | PRIMARY text search (replaces `Grep`/`rg`) |
| `LeIndex [Write]` | `leindex.write` | Create or overwrite a file |

`leindex.index` is a registry-owned start/poll job: it returns a `job_id` and
phase/status snapshot by default (`wait=false`). Poll that job or pass
`wait=true`; MCP requests never cancel indexing or publication at a wall-clock
deadline. Results expose core TF-IDF/PDG status and configured neural provider
status.

### Output formatting

- **MCP payloads** are trimmed to the minimum needed for an LLM: short
  snippets, capped counts, dropped internal byte ranges and verbose
  fields. No ANSI color, no UI chrome.
- **CLI output** (when invoked via `cargo install leindex`) is rendered
  for human reading: split-view color diffs, tree-style maps, structured
  tables.

---

## License

MIT - See [LICENSE](../../LICENSE)

---

## Links

- [GitHub Repository](https://github.com/scooter-lacroix/LeIndex)
- [Full Documentation](https://github.com/scooter-lacroix/LeIndex#readme)
- [MCP Documentation](https://github.com/scooter-lacroix/LeIndex/blob/master/docs/MCP.md)
