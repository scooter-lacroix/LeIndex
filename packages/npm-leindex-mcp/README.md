# @leindex/mcp

**LeIndex MCP Server - Auto-installing binary wrapper**

A lightweight npm package that automatically downloads and configures LeIndex for use as an MCP (Model Context Protocol) server in AI coding tools.

## What is This?

This package provides the **leanest** LeIndex distribution:
- ✅ MCP server functionality (stdio mode)
- ✅ Auto-downloads LeIndex binary on install
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
      "args": ["@leindex/mcp"]
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

---

## MCP Configuration Examples

### Cursor IDE

Add to Cursor settings (`~/.cursor/mcp.json` or Settings → MCP):

```json
{
  "mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["@leindex/mcp"]
    }
  }
}
```

### Claude Code

Add to `~/.config/claude-code/mcp_servers.json`:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "npx",
      "args": ["@leindex/mcp"]
    }
  }
}
```

### Zed IDE

Add to `~/.config/zed/settings.json`:

```json
{
  "context_servers": {
    "leindex": {
      "command": {
        "path": "npx",
        "args": ["@leindex/mcp"]
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
      "args": ["@leindex/mcp"]
    }
  }
}
```

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
      "args": ["@leindex/mcp"]
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
| **Dashboard** | ❌ No | ✅ Yes (`leindex dashboard`) |
| **HTTP Server** | ❌ No | ✅ Yes (`leindex serve`) |
| **CLI Tools** | ❌ No | ✅ Yes (`leindex search`, `index`, etc.) |
| **Binary Size** | ~32MB (single binary with all features) | ~32MB (single binary with all features) |
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

1. **On `npm install`**: The `postinstall` script downloads the appropriate LeIndex binary for your platform (macOS/Linux/Windows, x64/arm64)
2. **Binary Storage**: Downloaded to `node_modules/@leindex/mcp/bin/`
3. **MCP Mode**: When called via `npx @leindex/mcp`, launches LeIndex in MCP stdio mode
4. **Updates**: By default the installer resolves the GitHub `latest` release and verifies the downloaded binary against `SHA256SUMS`

To pin a specific binary release instead of `latest`:

```bash
LEINDEX_BINARY_VERSION=1.5.2 npm install @leindex/mcp
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

Once configured, your AI tool can use these LeIndex tools:

| Tool | Purpose |
|------|---------|
| `leindex_index` | Index a project |
| `leindex_search` | Semantic code search |
| `leindex_deep_analyze` | Deep analysis with PDG |
| `leindex_context` | Expand context around symbol |
| `leindex_file_summary` | Structural file analysis |
| `leindex_symbol_lookup` | Symbol definition lookup |
| `leindex_grep_symbols` | Symbol search |
| `leindex_edit_preview` | Preview edits |
| `leindex_edit_apply` | Apply code edits |
| `leindex_rename_symbol` | Rename symbols |
| `leindex_impact_analysis` | Blast radius analysis |
| `leindex_diagnostics` | Health check |

---

## License

MIT - See [LICENSE](../../LICENSE)

---

## Links

- [GitHub Repository](https://github.com/scooter-lacroix/leindex)
- [Full Documentation](https://github.com/scooter-lacroix/leindex#readme)
- [MCP Documentation](https://github.com/scooter-lacroix/leindex/blob/master/docs/MCP.md)
