# LeIndex Documentation and Repository Cleanup Plan

**Goal:** Update documentation and archive legacy Python code to reflect the current pure Rust implementation.

**Key Finding:** The binary is already correctly named `leindex` (Cargo.toml line 15: `name = "leindex"`). No command name changes needed.

**Critical Corrections:**
- Python systems: 100% re-implemented in Rust with full feature parity
- Maestro: Development framework - keep product docs, archive dev infrastructure
- **Turso for BOTH vectors AND metadata** - Single unified storage backend; current implementation has HNSW in-memory, Turso incomplete

---

## Phase 1: Archive Legacy Python Code

### 1.1 Create Archive Directory Structure
```
.archive/python-legacy/
├── src/leindex/          # Entire Python package (165+ files)
├── tests/                 # All Python tests (36+ files)
├── examples/              # Python examples (8 files)
├── scripts/               # Utility scripts
├── packaging/            # pyproject.toml, egg-info
└── root-scripts/         # Root level .py files (7 files)
```

### 1.2 Move Files to Archive
**Files to move:**
- `src/leindex/` → `.archive/python-legacy/src/leindex/`
- `tests/` → `.archive/python-legacy/tests/` (exclude Rust tests in `crates/`)
- `examples/` → `.archive/python-legacy/examples/`
- `src/scripts/` → `.archive/python-legacy/scripts/`
- `pyproject.toml` → `.archive/python-legacy/packaging/`
- Root `*.py` files → `.archive/python-legacy/root-scripts/`

**Maestro files to keep as product docs:**
- `maestro/product.md` - Product vision, keep as is
- `maestro/product-guidelines.md` - Design principles, keep as is
- `maestro/tech-stack.md` - Technology decisions, keep as is

**Maestro development infrastructure to archive:**
- `maestro/critical_think/` → Archive (prototype dev tool)
- `maestro/workflow.md` → Archive (internal procedures)
- `maestro/workflow-config.json` → Archive
- `maestro/setup_state.json` → Archive
- `maestro/code_styleguides/` → Archive
- `.maestro/` → Remove (runtime state only)

### 1.3 Update .gitignore
Ensure `.archive/` is already in .gitignore (line 128: confirmed present).

---

## Phase 2: Update Installation Scripts

### 2.1 Update install.sh
**Current lines 13-19, 869-902** reference Python `pip install leindex`

**Changes needed:**
1. Remove Python detection/venv setup
2. Add Rust toolchain detection (`cargo --version`)
3. Change installation from `pip install` to `cargo build --release`
4. Update binary path from Python venv to `target/release/leindex`
5. Keep all AI tool MCP configurations (they already use `leindex` command)

### 2.2 Update install_macos.sh
Same changes as install.sh for macOS compatibility.

### 2.3 Update install.ps1
Same changes for Windows PowerShell.

---

## Phase 3: Rewrite Main Documentation

### 3.1 Update README.md
**Current state:** Lines 1-733 describe Python v2.0.2 with LEANN/Tantivy stack.

**New structure:**
```markdown
# LeIndex

[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange?style=for-the-badge)]
[![Version](https://img.shields.io/badge/Version-0.1.0-blue?style=for-the-badge)]

## What is LeIndex?

LeIndex is a **pure Rust** code search and analysis engine...

## Quick Start

### Prerequisites
- Rust 1.75+
- Cargo

### Installation
\`\`\`bash
cargo install --path crates/lepasserelle
# OR
cargo build --release --bins
\`\`\`

### Usage
\`\`\`bash
leindex index /path/to/project
leindex search "authentication"
leindex diagnostics
\`\`\`

## Architecture

LeIndex consists of 5 Rust crates:
- **leparse** - Tree-sitter based parsing (11+ languages)
- **legraphe** - Program Dependence Graph
- **lerecherche** - In-memory HNSW vector search (temporary)
- **lestockage** - Storage layer (Turso/libsql integration planned)
- **lepasserelle** - CLI and MCP server

## Migration from Python

See [MIGRATION.md](MIGRATION.md) for migrating from Python v2.0.2.
\`\`\`

### 3.2 Create INSTALLATION_RUST.md
New file covering:
- Prerequisites (Rust toolchain, system dependencies)
- Installation methods:
  - `cargo install --path .`
  - Build from source: `cargo build --release`
  - Release binaries (when published)
- Platform-specific notes
- Verification: `leindex --version`

### 3.3 Update ARCHITECTURE.md
**Current:** Lines 1-100 show Python stack with LEANN/Tantivy.

**New content:**
```
┌─────────────────────────────────────────────────────────┐
│           LeIndex Rust Architecture                      │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌─────────┐ │
│  │   MCP    │  │   CLI    │  │  lepass │  │ lestock │ │
│  │  Server  │  │   Tool   │  │  erille │  │   age   │ │
│  │  (axum)  │  │  (clap)  │  │         │  │(SQLite) │ │
│  └────┬─────┘  └────┬─────┘  └────┬────┘  └────┬────┘ │
│       │             │            │            │      │
│  ┌────▼─────────────▼────────────▼────────────▼───┐  │
│  │              lepasserelle crate                 │  │
│  │         (orchestration & config)               │  │
│  └────┬──────────────┬──────────────┬────────────┘  │
│       │              │              │               │
│  ┌────▼───┐   ┌─────▼─────┐   ┌────▼────┐  ┌─────▼──┐│
│  │leparse │   │ legraphe  │   │ lerech  │  │ Turso  ││
│  │Parsing │   │    PDG    │   │  HNSW   │  │Vectors ││
│  │(tree-  │   │  (petgraph│   │(hnsw_rs)│  │  TODO  ││
│  │ sitter) │   │   embed)  │   │ IN-MEM  │  │        ││
│  └────────┘   └───────────┘   └─────────┘  └─────────┘│
│                                                         │
│  Vector Search: HNSW (in-memory, temporary)            │
│  Unified Storage: Turso/libsql (vectors + metadata)    │
│  Current State: Turso configured but not implemented   │
└─────────────────────────────────────────────────────────┘
```

**Key Architecture Points:**
- Vector embeddings currently stored in-memory via HNSW (temporary)
- **Target architecture: Turso/libsql as unified storage for BOTH vectors AND metadata**
- Turso configured but not implemented (requires vec0 extension, F32_BLOB columns)

### 3.4 Create MIGRATION.md
New file covering:
- Python v2.0.2 → Rust v0.1.0 migration
- Breaking changes
- Data migration (if applicable)
- Configuration changes
- Rollback instructions

### 3.5 Update API.md
**Current:** Lines 1-100 describe Python API.

**New content:** Document Rust CLI commands and programmatic usage.

---

## Phase 4: Create Crate-Level Documentation

### 4.1 Create Individual Crate READMEs
- `crates/leparse/README.md` - Zero-copy AST extraction
- `crates/legraphe/README.md` - PDG analysis
- `crates/lerecherche/README.md` - HNSW vector search (temporary)
- `crates/lestockage/README.md` - Storage layer (Turso/libsql planned)
- `crates/lepasserelle/README.md` - CLI & MCP bridge

### 4.2 Update Workspace Documentation
- Update `LEPARSE_IMPLEMENTATION_SUMMARY.md` if needed
- Add `RUST_ARCHITECTURE.md` with detailed crate interaction diagrams
- Document Turso integration status (configured, not implemented)

---

## Phase 5: Update AI Tool Compatibility Documentation

### 5.1 MCP Server Status
The MCP server at `lepasserelle/src/mcp/` provides:
- `leindex_index` - Index projects
- `leindex_search` - Semantic search
- `leindex_deep_analyze` - Deep analysis
- `leindex_context` - Context expansion
- `leindex_diagnostics` - System health

**All existing tool configs use `command: "leindex"` which is correct.**

### 5.2 Document Verified Tools
Create `MCP_COMPATIBILITY.md` listing:
- ✅ Confirmed working tools
- ⚠️ Tools needing verification
- ❌ Known incompatible tools

---

## Phase 6: Update Version and Changelog

### 6.1 Update CHANGELOG.md
Add new section:
```markdown
## [0.1.0] - 2025-01-26

### Added
- Pure Rust implementation with 5 crates
- Zero-copy AST extraction via leparse (11+ languages)
- PDG analysis via legraphe (petgraph-based)
- In-memory HNSW vector search via lerecherche (temporary)
- MCP server via lepasserelle (axum-based)
- Advanced memory management with cache spilling
- Project configuration with TOML support
- Turso/libsql configuration (implementation planned)

### Changed
- **BREAKING:** Complete rewrite from Python to Rust
- Binary now compiled Rust, not Python package
- Installation via `cargo install` instead of `pip install`
- Vector search: In-memory HNSW instead of LEANN (temporary)
- Storage architecture: Turso/libsql unified storage planned (vectors + metadata)
- 100% feature parity achieved (for completed features)

### Removed
- Python LEANN/Tantivy/DuckDB dependencies
- All Python source code (archived)
- PyO3 bindings (unused vestigial dependency removed)

### Known Limitations
- **Turso/libsql unified storage** configured but not yet implemented
  - Current: HNSW in-memory vectors only
  - Planned: vec0 extension with F32_BLOB for vectors + metadata
- Swift/Kotlin/Dart parsers temporarily disabled (tree-sitter version conflicts)
```

---

## Phase 7: Clean Up Workspace Dependencies

### 7.1 Remove Unused PyO3 Dependency
**File:** `Cargo.toml` line 24

```toml
# REMOVE this line:
pyo3 = { version = "0.23", features = ["extension-module"] }
```

PyO3 is not used by any crate - vestigial dependency from Python era.

---

## Files to Modify

### Create New Files
- `.archive/python-legacy/` (directory structure)
- `.archive/maestro-dev/` (development infrastructure)
- `INSTALLATION_RUST.md`
- `MIGRATION.md`
- `MCP_COMPATIBILITY.md`
- `RUST_ARCHITECTURE.md`
- `crates/*/README.md` (5 files)
- `REPO_CLEANUP_PLAN.md` (this file)

### Modify Existing Files
- `README.md` - Complete rewrite
- `install.sh` - Lines 13-19, 869-902
- `install_macos.sh` - Same sections
- `install.ps1` - Same sections
- `ARCHITECTURE.md` - Complete rewrite with correct diagram
- `API.md` - Rewrite for Rust
- `CHANGELOG.md` - Add v0.1.0 section with Known Limitations
- `Cargo.toml` - Remove pyo3 dependency (line 24)
- `.maestro/tracks/lepasserelle_20250125/track.md` - Update with Turso TODO status

### Move to Archive
- `src/leindex/` → `.archive/python-legacy/src/leindex/`
- `tests/` → `.archive/python-legacy/tests/` (Rust tests in `crates/` stay)
- `examples/` → `.archive/python-legacy/examples/`
- `pyproject.toml` → `.archive/python-legacy/packaging/`
- Root `*.py` files → `.archive/python-legacy/root-scripts/`
- `maestro/critical_think/` → `.archive/maestro-dev/`
- `maestro/workflow.md` → `.archive/maestro-dev/`
- `maestro/code_styleguides/` → `.archive/maestro-dev/`
- `.maestro/` → Remove (runtime state)

### Keep (Product Documentation)
- `maestro/product.md` - Keep as product documentation
- `maestro/product-guidelines.md` - Keep as design reference
- `maestro/tech-stack.md` - Keep as architecture doc
- `maestro/tracks.md` - Keep as development history
- `maestro/tracks/` - Keep as development history
- `maestro/archive/` - Already archived, keep as is

---

## Verification Steps

1. **Build verification:**
   ```bash
   cargo build --release --bins
   ./target/release/leindex --version
   ```

2. **MCP server verification:**
   ```bash
   ./target/release/leindex  # Should show CLI help
   # Verify MCP server starts correctly
   ```

3. **Documentation verification:**
   - All references to Python removed from main README
   - Installation instructions work for Rust
   - Architecture diagram shows Turso as unified storage (vectors + metadata)
   - All broken internal links fixed

4. **Archive verification:**
   - No Python files remain in root/`src/`/`tests/`
   - Archive directory properly gitignored
   - No Rust files accidentally archived
   - Product docs (maestro/product.md, etc.) preserved

5. **Dependency verification:**
   ```bash
   cargo tree | grep pyo3  # Should return nothing
   ```

---

## Order of Operations

1. **First:** Update track file with Turso TODO status
2. **Second:** Create archive structure and move Python files
3. **Third:** Archive Maestro development infrastructure, keep product docs
4. **Fourth:** Update installation scripts (test locally)
5. **Fifth:** Remove pyo3 from Cargo.toml
6. **Sixth:** Rewrite main documentation (README, ARCHITECTURE, API)
7. **Seventh:** Create new documentation files
8. **Eighth:** Update CHANGELOG and version info
9. **Last:** Verification and testing

---

## Summary

**Critical Finding:** Binary name is already `leindex` - no command changes needed anywhere.

**Architecture Status:**
- Vector search: In-memory HNSW (hnsw_rs crate) - temporary implementation
- **Target: Turso/libsql unified storage** for BOTH vectors AND metadata (configured but not implemented)
- Current state: Turso configured but requires vec0 extension, F32_BLOB columns, vector_distance() queries
- Reference: `Rust_ref(DO_NOT_DIRECTLY_USE)/leindex/rust/` has migration framework only
- Task: Create `crates/lestockage/src/turso_vector.rs` for vector operations

**Main Work:**
- Archive 200+ Python files
- Archive Maestro development infrastructure (keep product docs)
- Remove pyo3 dependency from workspace
- Rewrite outdated documentation
- Update 3 installation scripts
- Create 6 new documentation files
- Update lepasserelle track with Turso TODO status

**No changes needed to:**
- Binary name (already `leindex`)
- MCP tool configurations (already use `leindex`)
- CLI command structure (already correct)
