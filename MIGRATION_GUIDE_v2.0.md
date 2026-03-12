# Migration Guide: Workspace to Unified Crate

## Overview

LeIndex has transitioned from a 10-crate workspace structure to a single unified crate. This guide helps you migrate your code and workflows.

## What's Changed

### Before (Workspace)
```toml
[dependencies]
leparse = "1.5"
lerecherche = "1.5"
lestockage = "1.5"
```

### After (Unified)
```toml
[dependencies]
leindex = "1.5.2"
```

## Import Path Changes

### Option 1: New Style (Recommended)

```rust
// Before
use leparse::Parser;
use legraphe::GraphBuilder;
use lestockage::Storage;
use lerecherche::SearchEngine;

// After
use leindex::parse::Parser;
use leindex::graph::GraphBuilder;
use leindex::storage::Storage;
use leindex::search::SearchEngine;
```

### Option 2: Backward Compatible

```rust
// These aliases still work but are hidden in docs
use leindex::leparse::Parser;
use leindex::legraphe::GraphBuilder;
use leindex::lestockage::Storage;
use leindex::lerecherche::SearchEngine;
```

## Feature Flags

Use features to reduce compile time and binary size:

```toml
[dependencies]
# Full functionality (default)
leindex = "1.5.2"

# Library use only (smaller)
leindex = { version = "1.5.2", default-features = false, features = ["parse", "search"] }

# CLI only
leindex = { version = "1.5.2", default-features = false, features = ["cli"] }

# CLI plus HTTP server modules
leindex = { version = "1.5.2", default-features = false, features = ["cli", "server"] }
```

### Available Features

| Feature | Description | Dependencies |
|---------|-------------|--------------|
| `full` | All features (default) | All below |
| `parse` | Code parsing, tree-sitter grammars | None |
| `graph` | PDG, graph operations | parse |
| `storage` | SQLite storage | parse, graph |
| `search` | Vector search, HNSW, INT8 quantization | parse, graph |
| `phase` | 5-phase analysis pipeline | parse, graph, search, storage |
| `cli` | CLI tool, MCP server | All above |
| `server` | HTTP/WebSocket server | storage, graph, search |
| `edit` | Code editing utilities | storage, graph, parse |
| `validation` | Index validation | parse, storage, graph |

## Binary Changes

The unified crate now distributes a single binary:

- `leindex` — main CLI entrypoint

Legacy `leserve` and `leedit` behavior now lives behind `leindex` subcommands and modules:

- `leindex serve` replaces the old standalone HTTP server entrypoint
- editing functionality remains available through the unified crate modules, not a separate shipped binary

### Installation

```bash
# Install from crates.io
cargo install leindex

# Install specific binaries only
cargo install leindex --bin leindex --features cli
```

## API Changes

### Module Structure

| Old Crate | New Module | Alias |
|-----------|------------|-------|
| `leparse` | `leindex::parse` | `leindex::leparse` |
| `legraphe` | `leindex::graph` | `leindex::legraphe` |
| `lestockage` | `leindex::storage` | `leindex::lestockage` |
| `lerecherche` | `leindex::search` | `leindex::lerecherche` |
| `lephase` | `leindex::phase` | `leindex::lephase` |
| `lepasserelle` | `leindex::cli` | `leindex::lepasserelle` |
| `leglobal` | `leindex::global` | `leindex::leglobal` |
| `leserve` | `leindex::server` | `leindex::leserve` |
| `leedit` | `leindex::edit` | `leindex::leedit` |
| `levalidation` | `leindex::validation` | `leindex::levalidation` |

### Public API Re-exports

For convenience, commonly used types are re-exported:

```rust
// These are directly available
use leindex::SearchEngine;  // From search module
use leindex::Cli;           // From cli module
```

## Environment Variables

The same environment variables work as before:

| Name | Required | Description | Default |
|------|----------|-------------|---------|
| `LEINDEX_HOME` | No | Storage/index home directory | `~/.leindex` |
| `LEINDEX_PORT` | No | HTTP server port | `47268` |

## Troubleshooting

### "Could not find crate"

If you see:
```
error: could not find `X` in `leindex`
```

Make sure the feature is enabled:
```toml
[dependencies]
leindex = { version = "1.5.2", features = ["cli", "server"] }
```

### Import errors

If your old imports don't work:
1. Try the backward-compatible aliases (`leindex::leparse`)
2. Or update to new style (`leindex::parse`)

### Binary not found

If `cargo install leindex` doesn't install binaries:
```bash
# Install the unified CLI binary explicitly
cargo install leindex --bin leindex --features cli

# Add server modules when you need `leindex serve`
cargo install leindex --bin leindex --features "cli server"
```

## Rollback

If you need to use the old workspace version temporarily:

```toml
[dependencies]
leparse = "=1.5.0"
lerecherche = "=1.5.0"
# ... etc
```

Pin to the last workspace release (1.5.0) until you can migrate.

## Support

- [GitHub Issues](https://github.com/scooter-lacroix/LeIndex/issues)
- [Documentation](https://docs.rs/leindex)
- [MCP Catalog](https://glama.ai/mcp/servers/leindex)
