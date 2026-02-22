# LeIndex CLI Reference

LeIndex provides a command-line interface for indexing, searching, and analyzing codebases with semantic understanding. Built on Tree-sitter for zero-copy AST parsing and HNSW vectors for fast semantic search.

## Installation

### From Source

```bash
# Clone and build
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
cargo build --release

# Binary location
./target/release/leindex --version
```

### Via Cargo

```bash
cargo install leindex
```

### One-Line Installer (Linux/macOS)

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

## Global Options

These options apply to all commands:

| Option | Short | Description |
|--------|-------|-------------|
| `--project <PATH>` | `-p` | Path to the project directory (defaults to current directory) |
| `--verbose` | `-v` | Enable debug-level logging to stderr |
| `--stdio` | | Compatibility flag; starts MCP stdio mode |
| `--help` | `-h` | Display help information |
| `--version` | `-V` | Print version information |

---

## Commands

### `leindex index`

Index a project for code search and analysis. Parses source files, builds the Program Dependence Graph (PDG), and creates the semantic search index.

#### Synopsis

```
leindex index [OPTIONS] <PATH>
```

#### Arguments

| Argument | Required | Description |
|----------|----------|-------------|
| `PATH` | Yes | Path to the project directory to index |

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--force` | false | Force re-indexing even if already indexed |
| `--progress` | false | Show detailed progress during indexing |

#### Examples

```bash
# Index the current directory
leindex index .

# Index a specific project
leindex index /home/user/my-project

# Force a complete re-index
leindex index --force /path/to/project

# Index with verbose output
leindex -v index /path/to/project

# Index with progress details
leindex index --progress /path/to/large-project
```

#### Output Format

```
✓ Indexing complete!
  Files parsed: 150
  Successful: 148
  Failed: 2
  Signatures: 2340
  PDG nodes: 1892
  PDG edges: 4521
  Indexed nodes: 1892
  Time: 1234ms
```

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Path not found or indexing failed |
| 2 | Invalid arguments |

---

### `leindex search`

Search indexed code using semantic understanding. Supports natural language queries and returns ranked results with relevance scores.

#### Synopsis

```
leindex search [OPTIONS] <QUERY>
```

#### Arguments

| Argument | Required | Description |
|----------|----------|-------------|
| `QUERY` | Yes | Search query (supports natural language) |

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--top-k <N>` | 10 | Maximum number of results to return |

#### Examples

```bash
# Basic search
leindex search "authentication"

# Natural language query
leindex search "how does user login work"

# Get more results
leindex search --top-k 25 "database connection"

# Search in a specific project
leindex -p /path/to/project search "error handling"

# Find API endpoints
leindex search "REST API routes"

# Locate configuration code
leindex search "where is config loaded"

# Search for patterns
leindex search "singleton pattern implementation"
```

#### Output Format

```
Found 3 result(s) for: 'authentication'

1. authenticate_user (src/auth/login.rs)
   ID: auth_login_rs_42
   Overall Score: 0.92
   Explanation: [Semantic: 0.88, Text: 0.95, Structural: 0.89]
   Context: pub fn authenticate_user(credentials: &Credentials) -> Result<User, AuthError>...

2. AuthMiddleware::validate (src/middleware/auth.rs)
   ID: middleware_auth_rs_18
   Overall Score: 0.85
   Explanation: [Semantic: 0.82, Text: 0.88, Structural: 0.84]
   Context: fn validate(&self, request: &Request) -> Result<(), Error>...
```

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success (may have zero results) |
| 1 | Project not indexed or search failed |
| 2 | Invalid query |

---

### `leindex analyze`

Perform deep code analysis with context expansion. Uses semantic search combined with Program Dependence Graph traversal to provide comprehensive understanding.

#### Synopsis

```
leindex analyze [OPTIONS] <QUERY>
```

#### Arguments

| Argument | Required | Description |
|----------|----------|-------------|
| `QUERY` | Yes | Analysis query describing what to understand |

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--tokens <N>` | 2000 | Maximum tokens for context expansion |

#### Examples

```bash
# Understand a feature
leindex analyze "how does authentication work"

# Deep dive into a module
leindex analyze "database connection pooling"

# Analyze error handling
leindex analyze "how are errors propagated"

# Understand data flow
leindex analyze "where does user input get validated"

# With more context
leindex analyze --tokens 4000 "request lifecycle"

# Analyze specific project
leindex -p /path/to/service analyze "API rate limiting"

# Understand dependencies
leindex analyze "what modules depend on config"

# Analyze architecture
leindex analyze "layer boundaries and data flow"
```

#### Output Format

```
Analysis Results for: 'how does authentication work'

Found 5 entry point(s)
Tokens used: 1847
Processing time: 89ms

Context:
The authentication system consists of three main components:

1. AuthMiddleware - Validates JWT tokens on incoming requests
   Located in src/middleware/auth.rs
   Dependencies: JWT_SECRET env var, User model

2. authenticate_user - Core login function
   Located in src/auth/login.rs
   Validates credentials against database

3. SessionManager - Handles user sessions
   Located in src/session/manager.rs
   Uses Redis for session storage...
```

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Project not indexed or analysis failed |
| 2 | Invalid query |

---

### `leindex phase`

Run the additive 5-phase analysis workflow for comprehensive codebase understanding. Each phase builds on the previous, progressively refining focus.

#### Synopsis

```
leindex phase [OPTIONS]
```

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--phase <N>` | - | Run specific phase (1-5) |
| `--all` | false | Run all phases sequentially |
| `--mode <MODE>` | balanced | Output format: `ultra`, `balanced`, or `verbose` |
| `--path <PATH>` | - | Path to analyze (defaults to project path) |
| `--max-files <N>` | 2000 | Maximum files to consider |
| `--max-focus-files <N>` | 20 | Maximum focus files in phase 3 |
| `--top-n <N>` | 10 | Top-N entries for ranking phases |
| `--max-chars <N>` | 12000 | Maximum output characters |
| `--include-docs` | false | Include markdown/text documentation |
| `--docs-mode <MODE>` | off | Docs inclusion: `off`, `markdown`, `text`, or `all` |
| `--no-incremental-refresh` | false | Disable incremental freshness checks |

#### Phase Descriptions

| Phase | Name | Description |
|-------|------|-------------|
| 1 | Overview | File distribution and structure analysis |
| 2 | Keywords | Term frequency and importance analysis |
| 3 | Hotspots | Focus file identification and ranking |
| 4 | Dependencies | Import/call graph analysis |
| 5 | Summary | Aggregated insights and recommendations |

#### Examples

```bash
# Run all phases
leindex phase --all

# Run specific phase
leindex phase --phase 3

# Ultra-compact output
leindex phase --all --mode ultra

# Verbose output with more detail
leindex phase --all --mode verbose

# Include documentation files
leindex phase --all --include-docs --docs-mode all

# Analyze specific path
leindex phase --all --path /path/to/module

# Large project with custom limits
leindex phase --all --max-files 5000 --max-focus-files 50

# More detailed rankings
leindex phase --all --top-n 20

# Disable incremental caching
leindex phase --all --no-incremental-refresh

# Combined options for thorough analysis
leindex phase --all --mode verbose --max-chars 20000 --include-docs
```

#### Output Format (Phase 3 Example)

```
=== PHASE 3: Hotspot Files ===

Analyzing 847 source files...

Top 10 Hotspot Files:
1. src/auth/middleware.rs (score: 0.94)
   Keywords: auth, jwt, validate, token, request
   Dependencies: 12 incoming, 4 outgoing

2. src/database/pool.rs (score: 0.89)
   Keywords: connection, pool, postgres, query
   Dependencies: 8 incoming, 15 outgoing

3. src/api/routes.rs (score: 0.87)
   Keywords: route, handler, endpoint, method
   Dependencies: 24 incoming, 6 outgoing
```

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Analysis failed or path not found |
| 2 | Invalid options (e.g., both `--phase` and `--all`) |

---

### `leindex diagnostics`

Display system diagnostics including index statistics, memory usage, and health status.

#### Synopsis

```
leindex diagnostics [OPTIONS]
```

#### Examples

```bash
# Basic diagnostics
leindex diagnostics

# For specific project
leindex -p /path/to/project diagnostics

# With verbose logging
leindex -v diagnostics
```

#### Output Format

```
LeIndex Diagnostics

Project: my-project
Path: /home/user/projects/my-project

Index Statistics:
  Files parsed: 150
  Successful: 148
  Failed: 2
  Total signatures: 2340
  PDG nodes: 1892
  PDG edges: 4521
  Indexed nodes: 1892

Memory Usage:
  Current: 45.23 MB
  Total: 64.00 MB
  Usage: 70.7%
```

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Success |
| 1 | Failed to get diagnostics |

---

### `leindex serve`

Start the MCP (Model Context Protocol) HTTP server for AI assistant integration. Provides REST endpoints for indexing, searching, and analysis.

#### Synopsis

```
leindex serve [OPTIONS]
```

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--host <HOST>` | 127.0.0.1 | Host address to bind to |
| `--port <PORT>` | 47268 | Port to listen on |

#### Examples

```bash
# Start on default port
leindex serve

# Custom host and port
leindex serve --host 0.0.0.0 --port 3000

# Using environment variable for port
LEINDEX_PORT=8080 leindex serve

# Bind to all interfaces
leindex serve --host 0.0.0.0

# Verbose server logging
leindex -v serve

# With specific project
leindex -p /path/to/project serve --port 4000
```

#### Output Format

```
LeIndex MCP Server

Server starting on http://127.0.0.1:47268

Available endpoints:
  POST /mcp           - JSON-RPC 2.0 endpoint
  GET  /mcp/tools/list - List available tools
  GET  /health         - Health check

Configuration:
  Port: 47268 (override with LEINDEX_PORT env var)

Press Ctrl+C to stop the server
```

#### HTTP Endpoints

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/mcp` | POST | JSON-RPC 2.0 request handler |
| `/mcp/tools/list` | GET | List available MCP tools |
| `/health` | GET | Server health check |

#### Exit Codes

| Code | Meaning |
|------|---------|
| 0 | Graceful shutdown |
| 1 | Server error or bind failure |

---

### `leindex mcp`

Run the MCP server in stdio mode for AI tool subprocess integration. Reads JSON-RPC from stdin and writes responses to stdout.

#### Synopsis

```
leindex mcp [OPTIONS]
```

#### Options

| Option | Default | Description |
|--------|---------|-------------|
| `--stdio` | false | Compatibility flag for some AI tools |

#### Examples

```bash
# Start MCP stdio server (default when no command specified)
leindex mcp

# Explicit stdio mode
leindex mcp --stdio

# For specific project
leindex -p /path/to/project mcp

# With verbose logging (to stderr)
leindex -v mcp
```

#### Claude Code Integration

Add to `~/.claude/claude_desktop_config.json`:

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp"]
    }
  }
}
```

#### Cursor Integration

```json
{
  "mcpServers": {
    "leindex": {
      "command": "leindex",
      "args": ["mcp", "-p", "/path/to/project"]
    }
  }
}
```

#### Output (to stderr)

```
[INFO] LeIndex MCP stdio server starting
[INFO] Project: /home/user/projects/my-project
[INFO] Reading JSON-RPC from stdin, writing to stdout
[INFO] Press Ctrl+C to stop
```

---

## Environment Variables

| Variable | Default | Description |
|----------|---------|-------------|
| `LEINDEX_PORT` | 47268 | Override default port for `serve` command |
| `RUST_LOG` | info | Logging level (debug, trace, warn, error) |

### Using Environment Variables

```bash
# Custom port
LEINDEX_PORT=8080 leindex serve

# Debug logging
RUST_LOG=debug leindex index .

# Combined
RUST_LOG=trace LEINDEX_PORT=3000 leindex -v serve
```

---

## Troubleshooting

### "Project not indexed" Error

**Problem:** Search or analyze commands report the project is not indexed.

**Solution:**
```bash
# Index the project first
leindex index /path/to/project

# Force re-index if corrupted
leindex index --force /path/to/project
```

### "Failed to canonicalize project path"

**Problem:** Path resolution fails.

**Solution:**
```bash
# Use absolute paths
leindex index /home/user/my-project

# Or ensure the path exists
ls -la /path/to/project
```

### High Memory Usage

**Problem:** Indexing large projects consumes too much memory.

**Solution:**
```bash
# Check current diagnostics
leindex diagnostics

# The system auto-spills to disk when threshold exceeded
# Monitor with verbose logging
RUST_LOG=debug leindex index --progress /path/to/large-project
```

### Port Already in Use

**Problem:** `serve` command fails with address in use.

**Solution:**
```bash
# Use a different port
leindex serve --port 3000

# Or via environment variable
LEINDEX_PORT=3000 leindex serve

# Find what's using the port
lsof -i :47268
```

### Slow Search Results

**Problem:** Search takes longer than expected.

**Solution:**
```bash
# Verify index is loaded
leindex diagnostics

# Reduce result count for faster response
leindex search --top-k 5 "query"

# Check if re-indexing is needed
leindex index --force /path/to/project
```

### Parsing Failures

**Problem:** Some files fail to parse during indexing.

**Solution:**
```bash
# Check diagnostics for failure count
leindex diagnostics

# Parsing failures are often due to:
# - Unsupported file types (only 12 languages supported)
# - Syntax errors in source files
# - Very large files

# The index continues with successful parses
# Check verbose output for details
leindex -v index /path/to/project
```

### MCP Connection Issues

**Problem:** AI tool cannot connect to MCP server.

**Solution:**
```bash
# Verify server is running
curl http://127.0.0.1:47268/health

# Check server logs
leindex -v serve

# Ensure correct configuration in AI tool
# For stdio mode, verify the command works directly:
echo '{"jsonrpc":"2.0","id":1,"method":"tools/list"}' | leindex mcp
```

### Phase Analysis Errors

**Problem:** `phase` command fails with invalid options.

**Solution:**
```bash
# Must specify either --phase or --all, not both
leindex phase --all           # Correct
leindex phase --phase 3       # Correct
leindex phase --phase 3 --all # ERROR

# Valid phase numbers are 1-5
leindex phase --phase 6       # ERROR

# Valid modes
leindex phase --all --mode ultra     # Correct
leindex phase --all --mode invalid   # ERROR
```

---

## Supported Languages

LeIndex supports semantic analysis for 12 programming languages:

| Language | Extensions |
|----------|------------|
| Python | `.py` |
| Rust | `.rs` |
| JavaScript | `.js`, `.mjs` |
| TypeScript | `.ts`, `.tsx` |
| Go | `.go` |
| Java | `.java` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp` |
| C# | `.cs` |
| Ruby | `.rb` |
| PHP | `.php` |
| Lua | `.lua` |
| Scala | `.scala` |

---

## See Also

- [Architecture Guide](ARCHITECTURE.md) — System design internals
- [API Reference](API.md) — Detailed API documentation
- [MCP Compatibility](MCP_COMPATIBILITY.md) — MCP server details
- [Configuration](CONFIGURATION.md) — Project configuration options
