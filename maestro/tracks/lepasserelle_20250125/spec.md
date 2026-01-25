# Specification: lepasserelle - Bridge & Integration

**Track ID:** `lepasserelle_20250125`
**Track Type:** Standard Track
**Status:** New
**Created:** 2025-01-25
**Parent Track:** `leindex_rust_refactor_20250125`

---

## Overview

### Vision

`lepasserelle` (French for "The Bridge") is the Bridge & Integration layer of the LeIndex Rust Renaissance. It creates PyO3 FFI bindings for Python-Rust interop and implements the unified MCP tool.

### The "Why"

**Current State:**
- Python-only implementation
- No Rust integration
- Memory pressure issues

**Target State:**
- PyO3 FFI bindings for seamless Python-Rust interop
- Unified MCP tool: `leindex_deep_analyze`
- Memory-aware spilling and resource management
- Zero-copy data transfer via mmap

### Key Principles

1. **Transparent Integration** - Python calls Rust seamlessly
2. **Unified MCP Tool** - Single tool for deep code analysis
3. **Memory Awareness** - Spilling at 90% RSS threshold
4. **Zero-Copy Transfer** - mmap for large data, no unnecessary copies

---

## Functional Requirements

### FR-1 PyO3 FFI Bindings

- Create `leindex_rust` Python module
- Expose `RustAnalyzer` class
- Expose `build_weighted_context` function
- Python-friendly error handling

### FR-2 Zero-Copy Data Transfer

- mmap for passing large source files
- Shared memory buffers
- Zero-copy embedding transfer
- Optimize FFI boundary crossings

### FR-3 Unified MCP Tool

- Implement `leindex_deep_analyze` tool
- Add semantic search entry point
- Integrate Rust graph expansion
- Create LLM-ready summary format

### FR-4 Memory Management

- RSS monitoring
- 90% threshold spilling logic
- PDG cache clearing
- DuckDB cache spilling
- Python gc coordination

### FR-5 Error Handling

- Rust error types with `thiserror`
- Convert to Python exceptions
- Structured logging
- Debug/trace modes

---

## Non-Functional Requirements

### Performance Targets

- **FFI Overhead:** Minimal overhead across Python-Rust boundary
- **Memory Management:** Spilling activates at 90% RSS threshold
- **Zero-Copy:** No unnecessary copies across FFI

### Quality Requirements

- **Test Coverage:** >95% for all FFI operations
- **Contract Tests:** FFI boundary contract tests
- **Code Quality:** Pass clippy with no warnings

---

## Acceptance Criteria

**AC-1 PyO3 Bindings**
- [ ] PyO3 bindings callable from Python without errors
- [ ] `RustAnalyzer` class exposed and working
- [ ] Error handling working correctly

**AC-2 MCP Tool**
- [ ] MCP tool `leindex_deep_analyze` functional
- [ ] Semantic search entry point working
- [ ] LLM-ready summaries generated

**AC-3 Memory Management**
- [ ] Memory spilling activates at 90% RSS threshold
- [ ] PDG cache clearing working
- [ ] Python gc coordination working

**AC-4 Zero-Copy Transfer**
- [ ] Zero-copy transfer verified (no unnecessary copies)
- [ ] mmap working for large files
- [ ] Shared memory buffers working

---

## Dependencies

### Internal Dependencies
- `lerecherche_20250125` - Requires search functionality for MCP tool
- `lestockage_20250125` - Requires storage for caching

### External Rust Crates
- `pyo3` (Python bindings)
- `mmap` (memory mapping)
- `thiserror` (error handling)
- `tracing` (structured logging)
- `psutil` (via PyO3 for RSS monitoring)

---

## Out of Scope

- **No Web UI** - CLI/MCP only
- **No Other Language FFIs** - Python only via PyO3
- **No Distributed FFI** - Single-machine only
