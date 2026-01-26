# Specification: lepasserelle - Integration & API Layer

**Track ID:** `lepasserelle_20250125`
**Track Type:** Standard Track
**Status:** Pending Rewrite
**Created:** 2025-01-25
**Updated:** 2025-01-26 (Rewritten for Pure Rust)
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

### Vision

`lepasserelle` (French for "The Bridge") is the **Integration & API Layer** of the LeIndex Rust Renaissance. It brings together all core crates (leparse, legraphe, lerecherche, lestockage) into a cohesive system with CLI and MCP server interfaces.

**IMPORTANT:** This is a **100% Pure Rust implementation**. All Python/PyO3 code from the prototype has been removed.

### The "Why"

**Current State:**
- Four independent crates with no unified interface
- No way to use LeIndex from command line
- No MCP server for AI assistant integration
- Memory management exists but not integrated

**Target State:**
- Unified `LeIndex` API that orchestrates all crates
- CLI interface: `leindex index`, `leindex search`, `leindex analyze`
- Pure Rust MCP server with JSON-RPC 2.0
- Memory-aware operations with automatic cache spilling

### Key Principles

1. **Pure Rust** - Zero Python dependencies, native performance
2. **Unified API** - Single entry point for all LeIndex functionality
3. **CLI First** - Command-line interface as primary user interaction
4. **MCP Server** - Native Rust JSON-RPC server for AI integration
5. **Memory Awareness** - Automatic spilling at 90% RSS threshold

---

## Functional Requirements

### FR-1 Core Orchestration

- `LeIndex` struct with project management
- `index_project(path)` - Full pipeline: parse → graph → index → store
- `search(query, top_k)` - Unified search interface
- `analyze(query, budget)` - Deep analysis with PDG expansion
- `get_diagnostics()` - Project statistics and health

### FR-2 CLI Interface

- `leindex index <path>` - Index a project
- `leindex search <query>` - Search indexed code
- `leindex analyze <query>` - Deep code analysis
- `leindex diagnostics` - System health check
- `leindex serve` - Start MCP server

### FR-3 MCP Server

- Native Rust JSON-RPC 2.0 server
- `leindex_deep_analyze` tool
- `leindex_search` tool
- `leindex_context` tool
- `leindex_index` tool
- `leindex_diagnostics` tool

### FR-4 Memory Management

- RSS monitoring with process memory access
- 90% threshold spilling logic
- PDG cache spilling (unload from memory)
- HNSW vector cache spilling
- Lazy reloading from storage

### FR-5 Configuration

- TOML/JSON project configuration
- Language filtering (which languages to parse)
- Path exclusions (.git, node_modules, target, etc.)
- Token budget settings
- Storage backend selection (SQLite/Turso)

### FR-6 Error Handling

- Graceful handling of parse failures
- Partial indexing (continue on error)
- Corruption detection and recovery
- Detailed error reporting with context

---

## Non-Functional Requirements

### Performance Targets

- **Indexing:** <60s for 50K files (match Python baseline)
- **Search:** <100ms P95 latency
- **Memory:** 10x reduction vs Python (400→32 bytes/node)
- **MCP Server:** <50ms response time for local requests

### Quality Requirements

- **Test Coverage:** >90% for integration layer
- **Integration Tests:** Full pipeline tests
- **Code Quality:** Pass clippy with no warnings
- **Documentation:** Full rustdoc API docs

---

## Acceptance Criteria

**AC-1 Core Orchestration**
- [ ] `LeIndex` struct implemented with all core methods
- [ ] `index_project()` executes full pipeline correctly
- [ ] `search()` returns relevant results
- [ ] `analyze()` expands context using PDG
- [ ] Errors handled gracefully with recovery

**AC-2 CLI Interface**
- [ ] All CLI commands implemented and working
- [ ] Progress indicators during indexing
- [ ] JSON output mode for scripting
- [ ] Help text and usage examples

**AC-3 MCP Server**
- [ ] JSON-RPC 2.0 server listening on port
- [ ] All MCP tools implemented
- [ ] SSE support for streaming responses
- [ ] Error handling with proper JSON-RPC errors

**AC-4 Memory Management**
- [ ] RSS monitoring working
- [ ] Spilling activates at 90% threshold
- [ ] Cache reloading from storage working
- [ ] Memory usage tracked and reported

**AC-5 Configuration**
- [ ] TOML configuration loading working
- [ ] All config options respected
- [ ] Default configuration sensible
- [ ] Validation with clear error messages

---

## Dependencies

### Internal Dependencies (All Required)
- `leparse` - Parsing engine for indexing
- `legraphe` - PDG for context expansion
- `lerecherche` - Semantic search
- `lestockage` - Persistence layer

### External Rust Crates
- `tokio` - Async runtime
- `axum` or `warp` - HTTP server for MCP
- `jsonrpc` - JSON-RPC 2.0 implementation
- `clap` - CLI argument parsing
- `serde` / `serde_json` - Serialization
- `thiserror` - Error handling
- `tracing` - Structured logging
- `toml` - Configuration parsing
- `psutil` (Rust port) - RSS monitoring

---

## Architecture Overview

```
┌─────────────────────────────────────────────────────────────┐
│                        lepasserelle                          │
├─────────────────────────────────────────────────────────────┤
│                                                               │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────────────┐ │
│  │   CLI       │  │  MCP Server │  │   LeIndex API       │ │
│  │  Interface  │  │ (JSON-RPC)  │  │  (Orchestration)    │ │
│  └──────┬──────┘  └──────┬──────┘  └──────────┬──────────┘ │
│         │                │                    │             │
│         └────────────────┴────────────────────┘             │
│                              │                               │
│         ┌────────────────────┴────────────────────┐         │
│         │         Integration Layer               │         │
│         └────────────────────┬────────────────────┘         │
│                              │                               │
│         ┌────────────────────┼────────────────────┐         │
│         │                    │                    │         │
│    ┌────▼────┐         ┌────▼────┐         ┌────▼────┐    │
│    │ leparse │         │legraphe │         │lerecherche│    │
│    └────┬────┘         └────┬────┘         └────┬────┘    │
│         │                    │                    │         │
│         └────────────────────┼────────────────────┘         │
│                              │                               │
│                    ┌─────────▼─────────┐                    │
│                    │    lestockage     │                    │
│                    │   (Persistence)   │                    │
│                    └───────────────────┘                    │
│                                                               │
│  ┌─────────────────────────────────────────────────────┐   │
│  │              Memory Management                       │   │
│  │  (RSS monitoring, cache spilling, lazy loading)     │   │
│  └─────────────────────────────────────────────────────┘   │
│                                                               │
└───────────────────────────────────────────────────────────────┘
```

---

## Out of Scope

- **No Python/PyO3** - Pure Rust only
- **No Web UI** - CLI/MCP only
- **No Distributed Indexing** - Single-machine only
- **No Real-time Updates** - Batch indexing only

---

## Implementation Phases

1. **Phase 1:** Remove PyO3, establish pure Rust foundation
2. **Phase 4:** Core orchestration (LeIndex API)
3. **Phase 2:** MCP server
4. **Phase 3:** CLI interface
5. **Phase 5:** Memory management (port existing, extend)
6. **Phase 6:** Testing & documentation

**Note:** Phases numbered by implementation priority, not numerical order.
