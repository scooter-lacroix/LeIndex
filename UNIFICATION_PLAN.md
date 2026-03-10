# LeIndex Crate Unification Plan

**Objective:** Merge all 10 workspace crates into a single unified `leindex` crate while maintaining `cargo install leindex` functionality.

**Status:** Draft Plan  
**Created:** 2026-02-22  
**Complexity:** High (Major architectural change)  
**Estimated Duration:** 2-3 days

---

## Table of Contents

1. [Current State Analysis](#1-current-state-analysis)
2. [Target Architecture](#2-target-architecture)
3. [Dependency Graph](#3-dependency-graph)
4. [Detailed Migration Steps](#4-detailed-migration-steps)
5. [File Structure Mapping](#5-file-structure-mapping)
6. [Import Transformations](#6-import-transformations)
7. [Cargo.toml Consolidation](#7-cargotoml-consolidation)
8. [Binary Migration](#8-binary-migration)
9. [Test & Benchmark Migration](#9-test--benchmark-migration)
10. [Feature Flag Strategy](#10-feature-flag-strategy)
11. [Edge Cases & Handling](#11-edge-cases--handling)
12. [Verification Checklist](#12-verification-checklist)
13. [Rollback Plan](#13-rollback-plan)
14. [MCP Catalog Readiness](#14-mcp-catalog-readiness)

---

## 1. Current State Analysis

### 1.1 Existing Workspace Structure

```
/
├── Cargo.toml          # Workspace definition
├── crates/
│   ├── leparse/        # Base parsing crate (no internal deps)
│   ├── legraphe/       # Graph operations (depends on leparse)
│   ├── lestockage/     # Storage layer (depends on leparse, legraphe)
│   ├── lerecherche/    # Search engine (depends on leparse, legraphe)
│   ├── lephase/        # Phase management (depends on leparse, legraphe, lerecherche, lestockage)
│   ├── lepasserelle/   # CLI interface (depends on leparse, legraphe, lerecherche, lestockage, lephase)
│   ├── leglobal/       # Global operations (depends on lestockage)
│   ├── leserve/        # Server binary (depends on lestockage, legraphe, lerecherche)
│   ├── leedit/         # Editor (depends on lestockage, legraphe, leparse)
│   └── levalidation/   # Validation (depends on leparse, lestockage, legraphe)
```

### 1.2 Problems with Current Structure

1. **Publishing Complexity:** 10 separate crates must be published in strict dependency order
2. **Version Management:** Each crate requires version bumps and coordination
3. **Installation UX:** Users cannot simply `cargo install leindex`
4. **Workspace Overhead:** Complex workspace configuration maintenance
5. **Dependency Hell:** Internal path dependencies block publishing without version specs

### 1.3 Benefits of Unification

1. ✅ **Simple Installation:** `cargo install leindex` works immediately
2. ✅ **Single Version:** One version number for entire project
3. ✅ **Simpler CI/CD:** No workspace matrix builds
4. ✅ **Easier Documentation:** Single crate to document
5. ✅ **Better Optimization:** Compiler can optimize across modules

### 1.4 Trade-offs

1. ⚠️ **Slower Incremental Builds:** All code recompiles on any change
2. ⚠️ **No Independent Versioning:** All modules share version
3. ⚠️ **Larger Binary Size:** Unless using feature flags effectively
4. ⚠️ **Large Refactoring Effort:** All imports must be updated

---

## 2. Target Architecture

### 2.1 Unified Crate Structure

```
/
├── Cargo.toml              # Single crate definition
├── src/
│   ├── lib.rs             # Module exports & re-exports
│   ├── bin/
│   │   ├── leindex.rs     # CLI binary (from lepasserelle)
│   │   ├── leserve.rs     # Server binary (from leserve)
│   │   └── leedit.rs      # Editor binary (from leedit)
│   ├── parse/             # Former leparse
│   │   └── mod.rs
│   ├── graph/             # Former legraphe
│   │   └── mod.rs
│   ├── storage/           # Former lestockage
│   │   └── mod.rs
│   ├── search/            # Former lerecherche
│   │   └── mod.rs
│   ├── phase/             # Former lephase
│   │   └── mod.rs
│   ├── cli/               # Former lepasserelle
│   │   └── mod.rs
│   ├── global/            # Former leglobal
│   │   └── mod.rs
│   ├── server/            # Former leserve (lib part)
│   │   └── mod.rs
│   ├── edit/              # Former leedit (lib part)
│   │   └── mod.rs
│   └── validation/        # Former levalidation
│       └── mod.rs
├── tests/                 # All integration tests
├── benches/               # All benchmarks
└── README.md
```

### 2.2 Module Feature Mapping

| Old Crate    | New Module | Feature Flag  | Dependencies                    |
|--------------|------------|---------------|--------------------------------|
| leparse      | parse      | parse         | None (base)                    |
| legraphe     | graph      | graph         | parse                          |
| lestockage   | storage    | storage       | parse, graph                   |
| lerecherche  | search     | search        | parse, graph                   |
| lephase      | phase      | phase         | parse, graph, search, storage  |
| lepasserelle | cli        | cli           | parse, graph, search, storage, phase |
| leglobal     | global     | global        | storage                        |
| leserve      | server     | server        | storage, graph, search         |
| leedit       | edit       | edit          | storage, graph, parse          |
| levalidation | validation | validation    | parse, storage, graph          |

---

## 3. Dependency Graph

```
                    ┌─────────────┐
                    │   leparse   │
                    │   (base)    │
                    └──────┬──────┘
                           │
           ┌───────────────┘
           │
    ┌──────▼──────┐
    │  legraphe   │
    └──────┬──────┘
           │
     ┌─────┴─────┬──────────────┐
     │           │              │
┌────▼────┐ ┌────▼─────┐  ┌─────▼──────┐
│lestockage│ │lerecherche│  │ lephase    │
└────┬────┘ └──────────┘  └─────┬──────┘
     │                          │
     │    ┌─────────────────────┘
     │    │
     │    │    ┌──────────────┐
     │    │    │ lepasserelle │
     │    │    │    (CLI)     │
     │    │    └──────────────┘
     │    │
┌────┴────┴────┬──────────────┐
│  leglobal    │   leserve    │
│  leedit      │ levalidation │
└──────────────┴──────────────┘
```

**Publishing Order (if keeping workspace):**
1. leparse (base)
2. legraphe (needs leparse)
3. lestockage (needs leparse, legraphe)
4. lerecherche (needs leparse, legraphe)
5. lephase (needs leparse, legraphe, lerecherche, lestockage)
6. lepasserelle (needs leparse, legraphe, lerecherche, lestockage, lephase)
7. leglobal (needs lestockage)
8. leserve (needs lestockage, legraphe, lerecherche)
9. leedit (needs lestockage, legraphe, leparse)
10. levalidation (needs leparse, lestockage, legraphe)

---

## 4. Detailed Migration Steps

### Phase 1: Preparation (30 minutes)

#### 4.1.1 Branch Setup
```bash
# Ensure clean working directory
git status

# Create backup branch
git checkout -b backup/pre-unification-$(date +%Y%m%d)
git push origin backup/pre-unification-$(date +%Y%m%d)

# Create feature branch for unification
git checkout -b feature/unified-crate
git push -u origin feature/unified-crate
```

#### 4.1.2 Inventory Current State
```bash
# List all source files
crates=(leparse legraphe lestockage lerecherche lephase lepasserelle leglobal leserve leedit levalidation)
for crate in "${crates[@]}"; do
    echo "=== $crate ==="
    find crates/$crate/src -name "*.rs" | wc -l
    find crates/$crate/src -name "*.rs" -exec wc -l {} + | tail -1
done

# List all dependencies
cat crates/*/Cargo.toml | grep -E "^\[dependencies\]" -A 100 | grep -E "^\[|^$" -v | sort -u

# Check for build.rs files
find crates -name "build.rs"

# Check for static files/assets
find crates -type f \( -name "*.proto" -o -name "*.json" -o -name "*.yaml" -o -name "*.toml" \) | grep -v Cargo.toml
```

**Additional MCP catalog-readiness inventory:**

```bash
# Check root compliance artifacts
ls LICENSE glama.json 2>/dev/null || true

# Check current MCP capability surface
rg -n '"initialize"|"tools/list"|"prompts/list"|"resources/list"' crates/lepasserelle/src

# Check existing install/documentation references
rg -n "install.sh|install_macos.sh|install.ps1|cargo install leindex|LEINDEX_HOME|LEINDEX_PORT" \
  README.md INSTALLATION.md docs/MCP.md docs/CLI.md crates/lepasserelle/src

# Check for existing prompt/resource/skill content
find . -maxdepth 3 \( -iname '*prompt*' -o -iname '*resource*' -o -iname '*skill*' -o -name 'glama.json' \)
```

#### 4.1.3 Backup Workspace Configuration
```bash
# Backup workspace Cargo.toml
cp Cargo.toml Cargo.toml.workspace-backup

# Document current versions
for crate in "${crates[@]}"; do
    version=$(grep "^version" crates/$crate/Cargo.toml | head -1 | cut -d'"' -f2)
    echo "$crate: $version"
done > crate-versions-backup.txt
```

Also back up catalog-facing docs before editing:

```bash
cp README.md README.md.pre-unification-backup
cp INSTALLATION.md INSTALLATION.md.pre-unification-backup
cp docs/MCP.md docs/MCP.md.pre-unification-backup
```

### Phase 2: Create Unified Directory Structure (1 hour)

#### 4.2.1 Create Module Directories
```bash
# Create src module structure
mkdir -p src/{parse,graph,storage,search,phase,cli,global,server,edit,validation,bin}

# Create test structure
mkdir -p tests/{parse,graph,storage,search,phase,cli,global,server,edit,validation,common}

# Create benchmark structure
mkdir -p benches
```

#### 4.2.2 Copy Source Files with Transformations

**For each crate, execute the following:**

**leparse → parse:**
```bash
crate="leparse"
module="parse"

# Copy all source files
find crates/$crate/src -name "*.rs" | while read file; do
    dest="src/$module/$(basename $file)"
    cp "$file" "$dest"
done

# Handle subdirectories
if [ -d "crates/$crate/src" ]; then
    for dir in crates/$crate/src/*/; do
        if [ -d "$dir" ]; then
            dirname=$(basename "$dir")
            mkdir -p "src/$module/$dirname"
            cp -r "$dir"* "src/$module/$dirname/"
        fi
    done
fi
```

**Transform imports in parse module:**
```bash
# No internal dependencies to update for leparse
# Only update any self-references if they exist
find src/$module -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
```

**legraphe → graph:**
```bash
crate="legraphe"
module="graph"

# Copy files
find crates/$crate/src -name "*.rs" | while read file; do
    dest="src/$module/$(basename $file)"
    cp "$file" "$dest"
done

# Copy subdirectories
for dir in crates/$crate/src/*/; do
    if [ -d "$dir" ]; then
        dirname=$(basename "$dir")
        mkdir -p "src/$module/$dirname"
        cp -r "$dir"* "src/$module/$dirname/"
    fi
done
```

**Transform imports in graph module:**
```bash
# Update external crate references
find src/$module -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/$module -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;

# Update any crate:: references to self module
find src/$module -name "*.rs" -exec sed -i 's/use crate::/use crate::graph::/g' {} \;
```

**Repeat for all remaining crates:**

**lestockage → storage:**
```bash
# Copy files...
# Transform imports:
# use lestockage:: → use crate::storage::
# use leparse:: → use crate::parse::
# use legraphe:: → use crate::graph::
```

**lerecherche → search:**
```bash
# Copy files...
# Transform imports:
# use lerecherche:: → use crate::search::
# use leparse:: → use crate::parse::
# use legraphe:: → use crate::graph::
# Note: Includes quantization/ subdirectory
```

**lephase → phase:**
```bash
# Copy files...
# Transform imports:
# use lephase:: → use crate::phase::
# use leparse:: → use crate::parse::
# use legraphe:: → use crate::graph::
# use lerecherche:: → use crate::search::
# use lestockage:: → use crate::storage::
```

**lepasserelle → cli:**
```bash
# Copy lib files (not main.rs)
# Transform imports:
# use lepasserelle:: → use crate::cli::
# Plus all dependency updates
```

**leglobal → global:**
```bash
# Copy files...
# Transform imports:
# use leglobal:: → use crate::global::
# use lestockage:: → use crate::storage::
```

**leserve → server:**
```bash
# Copy lib files (not main.rs)
# Transform imports:
# use leserve:: → use crate::server::
# use lestockage:: → use crate::storage::
# use legraphe:: → use crate::graph::
# use lerecherche:: → use crate::search::
```

**leedit → edit:**
```bash
# Copy lib files (not main.rs)
# Transform imports:
# use leedit:: → use crate::edit::
# use lestockage:: → use crate::storage::
# use legraphe:: → use crate::graph::
# use leparse:: → use crate::parse::
```

**levalidation → validation:**
```bash
# Copy files...
# Transform imports:
# use levalidation:: → use crate::validation::
# use leparse:: → use crate::parse::
# use lestockage:: → use crate::storage::
# use legraphe:: → use crate::graph::
```

### Phase 3: Create Unified lib.rs (30 minutes)

**File: src/lib.rs**

```rust
//! LeIndex - Unified semantic code search engine
//!
//! This crate provides a complete code indexing and search solution
//! with the following modules:
//!
//! - `parse`: Code parsing and AST generation
//! - `graph`: Dependency graph construction
//! - `storage`: Persistent storage layer
//! - `search`: Vector search engine with INT8 quantization
//! - `phase`: Multi-phase indexing pipeline
//! - `cli`: Command-line interface
//! - `global`: Global operations
//! - `server`: HTTP API server
//! - `edit`: Code editing utilities
//! - `validation`: Index validation tools
//!
//! ## Installation
//!
//! ```bash
//! cargo install leindex
//! ```
//!
//! ## Usage
//!
//! ```rust
//! use leindex::search::SearchEngine;
//!
//! // Initialize search
//! let engine = SearchEngine::new();
//! ```

#![cfg_attr(docsrs, feature(doc_auto_cfg))]
#![warn(missing_docs)]
#![warn(rust_2018_idioms)]

// Core modules
#[cfg(feature = "parse")]
pub mod parse;

#[cfg(feature = "graph")]
pub mod graph;

#[cfg(feature = "storage")]
pub mod storage;

#[cfg(feature = "search")]
pub mod search;

// Extended modules
#[cfg(feature = "phase")]
pub mod phase;

#[cfg(feature = "cli")]
pub mod cli;

#[cfg(feature = "global")]
pub mod global;

#[cfg(feature = "server")]
pub mod server;

#[cfg(feature = "edit")]
pub mod edit;

#[cfg(feature = "validation")]
pub mod validation;

// Re-exports for backward compatibility
// Users can use either `leindex::parse` or `leindex::leparse`

#[cfg(feature = "parse")]
#[doc(hidden)]
pub use parse as leparse;

#[cfg(feature = "graph")]
#[doc(hidden)]
pub use graph as legraphe;

#[cfg(feature = "storage")]
#[doc(hidden)]
pub use storage as lestockage;

#[cfg(feature = "search")]
#[doc(hidden)]
pub use search as lerecherche;

#[cfg(feature = "phase")]
#[doc(hidden)]
pub use phase as lephase;

#[cfg(feature = "cli")]
#[doc(hidden)]
pub use cli as lepasserelle;

#[cfg(feature = "global")]
#[doc(hidden)]
pub use global as leglobal;

#[cfg(feature = "server")]
#[doc(hidden)]
pub use server as leserve;

#[cfg(feature = "edit")]
#[doc(hidden)]
pub use edit as leedit;

#[cfg(feature = "validation")]
#[doc(hidden)]
pub use validation as levalidation;

// Public API re-exports for convenience

#[cfg(feature = "search")]
pub use search::SearchEngine;

#[cfg(feature = "cli")]
pub use cli::Cli;
```

### Phase 4: Create Unified Cargo.toml (1 hour)

**File: Cargo.toml**

```toml
[package]
name = "leindex"
version = "0.1.0"
edition = "2021"
rust-version = "1.75"
authors = [
    "Your Name <your.email@example.com>",
    "Maestro <maestro@omxp.ai>",
]
description = "High-performance semantic code search engine with INT8 quantization and HNSW indexing"
documentation = "https://docs.rs/leindex"
readme = "README.md"
homepage = "https://github.com/scooter-lacroix/LeIndex"
repository = "https://github.com/scooter-lacroix/LeIndex.git"
license = "MIT"
keywords = [
    "search",
    "code",
    "semantic",
    "vector",
    "indexing",
    "hnsw",
    "quantization",
    "language-server",
]
categories = [
    "development-tools",
    "text-processing",
    "data-structures",
    "algorithms",
]

[features]
default = ["full"]

# Full feature set
full = [
    "parse",
    "graph",
    "storage",
    "search",
    "phase",
    "cli",
    "global",
    "server",
    "edit",
    "validation",
]

# Minimal feature set (parsing and search only)
minimal = ["parse", "search"]

# Individual modules
parse = []
graph = ["parse"]
storage = ["parse", "graph"]
search = ["parse", "graph"]
phase = ["parse", "graph", "search", "storage"]
cli = ["parse", "graph", "search", "storage", "phase"]
global = ["storage"]
server = ["storage", "graph", "search", "tokio/full", "axio/full"]
edit = ["storage", "graph", "parse"]
validation = ["parse", "storage", "graph"]

[dependencies]
# Core dependencies (all features)
thiserror = "2.0"
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }

# Async runtime (server, phase features)
tokio = { version = "1.42", features = ["rt-multi-thread"], optional = true }

# HTTP server (server feature)
axum = { version = "0.7", optional = true }
tower = { version = "0.5", optional = true }
tower-http = { version = "0.6", features = ["cors", "trace"], optional = true }

# CLI (cli feature)
clap = { version = "4.5", features = ["derive", "env"], optional = true }

# Vector operations (search feature)
wide = "0.7"

# Serialization (storage feature)
bincode = { version = "1.3", optional = true }

# Parsing (parse feature)
tree-sitter = { version = "0.25", optional = true }
tree-sitter-python = { version = "0.23", optional = true }
tree-sitter-rust = { version = "0.23", optional = true }
tree-sitter-javascript = { version = "0.23", optional = true }
tree-sitter-typescript = { version = "0.23", optional = true }
tree-sitter-go = { version = "0.23", optional = true }

# Graph (graph feature)
petgraph = { version = "0.7", optional = true }

# Database (storage feature)
sqlx = { version = "0.8", features = ["runtime-tokio", "sqlite"], optional = true }

# Validation (validation feature)
jsonschema = { version = "0.28", optional = true }

[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }
tokio-test = "0.4"
tempfile = "3.15"
proptest = "1.6"

# Binary targets
[[bin]]
name = "leindex"
path = "src/bin/leindex.rs"
required-features = ["cli"]

[[bin]]
name = "leserve"
path = "src/bin/leserve.rs"
required-features = ["server"]

[[bin]]
name = "leedit"
path = "src/bin/leedit.rs"
required-features = ["edit"]

[[bench]]
name = "quantization"
path = "benches/quantization.rs"
harness = false
required-features = ["search"]

[[bench]]
name = "search"
path = "benches/search.rs"
harness = false
required-features = ["search"]

[[bench]]
name = "phase"
path = "benches/phase.rs"
harness = false
required-features = ["phase"]

[profile.release]
lto = "thin"
codegen-units = 1
opt-level = 3
strip = true

[profile.bench]
debug = true
```

**Note:** The above dependencies should be derived from the union of all current crate dependencies. Specific versions and features need to be verified against current Cargo.toml files.

### Phase 5: Migrate Binary Targets (30 minutes)

**Catalog-readiness note:** Preserve the CLI/runtime entrypoints that external MCP catalogs
will reference:

- `leindex mcp --stdio`
- `leindex serve --host <host> --port <port>`

Do not rename or remove these commands during unification without updating all catalog
metadata and documentation in the same change set.

**5.1 Create Binary Files**

**src/bin/leindex.rs** (from lepasserelle/src/main.rs):
```rust
//! LeIndex CLI binary

use leindex::cli::{Cli, run};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    run(cli).await
}
```

**src/bin/leserve.rs** (from leserve/src/main.rs):
```rust
//! LeIndex server binary

use leindex::server::run_server;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    run_server().await
}
```

**src/bin/leedit.rs** (from leedit/src/main.rs):
```rust
//! LeIndex editor binary

use leindex::edit::run_editor;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    run_editor()
}
```

**5.2 Update Binary Source Files**

For each binary source file, update imports:
```bash
# Update leindex.rs
sed -i 's/use lepasserelle::/use leindex::cli::/g' src/bin/leindex.rs

# Update leserve.rs
sed -i 's/use leserve::/use leindex::server::/g' src/bin/leserve.rs

# Update leedit.rs
sed -i 's/use leedit::/use leindex::edit::/g' src/bin/leedit.rs
```

### Phase 6: Migrate Tests (1 hour)

**6.1 Unit Tests**
Unit tests embedded in source files (#[cfg(test)] mod tests) move with the code. No action needed.

**6.2 Integration Tests**

```bash
# Migrate leparse tests
cp crates/leparse/tests/* tests/parse/ 2>/dev/null || true

# Migrate legraphe tests
cp crates/legraphe/tests/* tests/graph/ 2>/dev/null || true

# Continue for all crates...
```

**6.3 Update Test Imports**

```bash
# Update all test files to use new import paths
find tests -name "*.rs" -exec sed -i 's/use leparse::/use leindex::parse::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use legraphe::/use leindex::graph::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use lestockage::/use leindex::storage::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use lerecherche::/use leindex::search::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use lephase::/use leindex::phase::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use lepasserelle::/use leindex::cli::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use leglobal::/use leindex::global::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use leserve::/use leindex::server::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use leedit::/use leindex::edit::/g' {} \;
find tests -name "*.rs" -exec sed -i 's/use levalidation::/use leindex::validation::/g' {} \;
```

### Phase 7: Migrate Benchmarks (30 minutes)

```bash
# Copy all benchmark files
cp crates/lerecherche/benches/* benches/ 2>/dev/null || true
cp crates/lephase/benches/* benches/ 2>/dev/null || true

# Update benchmark imports (same pattern as tests)
find benches -name "*.rs" -exec sed -i 's/use leparse::/use leindex::parse::/g' {} \;
find benches -name "*.rs" -exec sed -i 's/use lerecherche::/use leindex::search::/g' {} \;
# ... etc for all crates
```

### Phase 8: Handle Special Cases (1 hour)

**8.1 Build Scripts (build.rs)**

If any crate has a build.rs:
```bash
# Check for build.rs files
find crates -name "build.rs"

# If found, merge into single build.rs at root
cat > build.rs << 'EOF'
use std::process::Command;

fn main() {
    // Protobuf generation (if levalidation has it)
    #[cfg(feature = "validation")]
    {
        println!("cargo:rerun-if-changed=proto/");
        // ... protobuf generation logic
    }
    
    // Version info
    let output = Command::new("git")
        .args(["describe", "--tags", "--always"])
        .output()
        .ok();
    
    if let Some(output) = output {
        let git_version = String::from_utf8_lossy(&output.stdout);
        println!("cargo:rustc-env=GIT_VERSION={}", git_version.trim());
    }
}
EOF
```

**8.2 Static Files/Assets**

If crates have static files:
```bash
# Create assets directory
mkdir -p assets

# Copy static files from all crates
find crates -type f \( -name "*.proto" -o -name "*.json" -o -name "*.yaml" \) | while read file; do
    cp "$file" assets/
done

# Update include paths in code
find src -name "*.rs" -exec sed -i 's|include_str!("|include_str!("../assets/|g' {} \;
find src -name "*.rs" -exec sed -i 's|include_bytes!("|include_bytes!("../assets/|g' {} \;
```

**8.3 Protobuf Files**

If using protobuf:
```bash
# Move proto files
mkdir -p proto
cp crates/*/proto/* proto/ 2>/dev/null || true

# Update build.rs with tonic/prost if needed
```

### Phase 9: Update Documentation (30 minutes)

**9.1 Update Root README.md**

Change installation instructions:
```markdown
## Installation

```bash
cargo install leindex
```

## Usage

```bash
# Initialize index
leindex init

# Start server
leindex serve

# Search
leindex search "your query"
```
```

In the same pass, add/update:

- MCP catalog-facing install section
- explicit stdio and HTTP startup examples
- LICENSE reference
- environment variable table for `LEINDEX_HOME` and `LEINDEX_PORT`

**9.2 Update CHANGELOG.md**

Add entry for unification:
```markdown
## [Unreleased] - Unified Crate

### Changed
- Merged all workspace crates into single `leindex` crate
- Simplified installation: `cargo install leindex`
- Feature flags now control module inclusion

### Migration Guide
Users using individual crates should update imports:
- `use leparse::X` → `use leindex::parse::X`
- Or use backward-compatible: `use leindex::leparse::X`
```

**9.3 Create Migration Guide**

Create `MIGRATION_v0.1.md`:
```markdown
# Migration Guide: Workspace to Unified Crate

## Import Changes

### Before
```rust
use leparse::Parser;
use legraphe::GraphBuilder;
use lerecherche::SearchEngine;
```

### After (Option 1: New style)
```rust
use leindex::parse::Parser;
use leindex::graph::GraphBuilder;
use leindex::search::SearchEngine;
```

### After (Option 2: Backward compatible)
```rust
use leindex::leparse::Parser;
use leindex::legraphe::GraphBuilder;
use leindex::lerecherche::SearchEngine;
```

## Cargo.toml Changes

### Before
```toml
[dependencies]
leparse = "0.1"
lerecherche = "0.1"
```

### After
```toml
[dependencies]
leindex = "0.1"
```

## Feature Selection

Use features to reduce compile time/binary size:

```toml
[dependencies]
leindex = { version = "0.1", default-features = false, features = ["parse", "search"] }
```
```

**9.4 Add MCP Catalog Metadata and Content**

Create or update the following catalog-facing assets:

- `glama.json`
- one repository-visible LeIndex MCP usage skill/guide
- prompt definitions for:
  - LeIndex Quickstart
  - LeIndex Investigation Workflow
- resource definitions/content for:
  - Quickstart / Usage Guide
  - Server Configuration Reference

These assets must stay aligned with the unified crate's actual CLI entrypoints and MCP
capability surface.

### Phase 10: Cleanup (30 minutes)

**10.1 Remove Workspace Structure**
```bash
# Remove old crates directory (already backed up)
mv crates archive/crates-migrated

# Remove workspace Cargo.toml.backup if verification passes
rm Cargo.toml.workspace-backup
```

**10.2 Update .gitignore**
```bash
# Add if not present
echo "/archive/" >> .gitignore
echo "*.backup" >> .gitignore
```

**10.3 Clean Build Artifacts**
```bash
cargo clean
rm -rf target/
```

---

## 5. File Structure Mapping

### Complete Mapping Table

| Source Path | Destination Path | Notes |
|-------------|------------------|-------|
| crates/leparse/src/*.rs | src/parse/*.rs | Base module |
| crates/leparse/src/*/*.rs | src/parse/*/*.rs | Submodules |
| crates/legraphe/src/*.rs | src/graph/*.rs | Update imports |
| crates/lestockage/src/*.rs | src/storage/*.rs | Update imports |
| crates/lerecherche/src/*.rs | src/search/*.rs | Includes quantization/ |
| crates/lephase/src/*.rs | src/phase/*.rs | Update imports |
| crates/lepasserelle/src/lib.rs | src/cli/mod.rs | CLI library |
| crates/lepasserelle/src/main.rs | src/bin/leindex.rs | CLI binary |
| crates/leglobal/src/*.rs | src/global/*.rs | Update imports |
| crates/leserve/src/lib.rs | src/server/mod.rs | Server library |
| crates/leserve/src/main.rs | src/bin/leserve.rs | Server binary |
| crates/leedit/src/lib.rs | src/edit/mod.rs | Edit library |
| crates/leedit/src/main.rs | src/bin/leedit.rs | Edit binary |
| crates/levalidation/src/*.rs | src/validation/*.rs | Update imports |
| crates/*/tests/*.rs | tests/*/*.rs | Organized by module |
| crates/*/benches/*.rs | benches/*.rs | Flat structure |
| crates/*/Cargo.toml | (merged) | See section 7 |

---

## 6. Import Transformations

### 6.1 External Crate → Internal Module

All imports from workspace crates become internal module imports:

```rust
// BEFORE
use leparse::Parser;
use legraphe::Graph;
use lestockage::Storage;
use lerecherche::SearchEngine;

// AFTER
use crate::parse::Parser;
use crate::graph::Graph;
use crate::storage::Storage;
use crate::search::SearchEngine;
```

### 6.2 Within Module Self-References

Update self-references within modules:

```rust
// In parse module
// BEFORE (in leparse)
use crate::SomeType;

// AFTER (in unified crate)
use crate::parse::SomeType;
```

### 6.3 External Dependencies

Third-party dependencies remain unchanged:

```rust
use serde::{Deserialize, Serialize};
use tokio::runtime::Runtime;
// ... unchanged
```

### 6.4 Sed Commands for Bulk Replacement

```bash
#!/bin/bash
# transform-imports.sh

# Core modules
find src/parse -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/graph -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/graph -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/storage -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/storage -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/storage -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/search -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;
find src/search -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/search -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use lephase::/use crate::phase::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use lepasserelle::/use crate::cli::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use lephase::/use crate::phase::/g' {} \;
find src/global -name "*.rs" -exec sed -i 's/use leglobal::/use crate::global::/g' {} \;
find src/global -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/server -name "*.rs" -exec sed -i 's/use leserve::/use crate::server::/g' {} \;
find src/server -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/server -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/server -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;
find src/edit -name "*.rs" -exec sed -i 's/use leedit::/use crate::edit::/g' {} \;
find src/edit -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/edit -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/edit -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/validation -name "*.rs" -exec sed -i 's/use levalidation::/use crate::validation::/g' {} \;
find src/validation -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/validation -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/validation -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
```

---

## 7. Cargo.toml Consolidation

### 7.1 Dependency Merging Strategy

For each dependency across all crates:
1. Take the highest version requirement
2. Merge all features (union)
3. Mark as optional if only used by some features

### 7.2 Dependency Analysis Script

```bash
#!/bin/bash
# analyze-deps.sh

echo "=== Dependency Analysis ==="
for crate in crates/*/; do
    echo ""
    echo "--- $(basename $crate) ---"
    grep -A 100 "^\[dependencies\]" "$crate/Cargo.toml" | grep -E "^[a-z].*=" | head -20
done

echo ""
echo "=== Unique Dependencies ==="
cat crates/*/Cargo.toml | grep -E "^\[dependencies\]" -A 1000 | grep -E "^[a-zA-Z].*=" | sed 's/=.*//' | sort -u
```

### 7.3 Feature Dependencies

```toml
[features]
# Each feature enables its dependencies
default = ["full"]
full = [
    "parse",
    "graph",
    "storage",
    "search",
    "phase",
    "cli",
    "global",
    "server",
    "edit",
    "validation",
]

# Parse is base, no dependencies
parse = ["tree-sitter", "tree-sitter-python", "tree-sitter-rust"]

# Graph depends on parse
graph = ["parse", "petgraph"]

# Storage depends on parse and graph
storage = ["parse", "graph", "bincode", "sqlx"]

# Search depends on parse and graph
search = ["parse", "graph", "wide"]

# Phase depends on parse, graph, search, storage
phase = ["parse", "graph", "search", "storage", "tokio"]

# CLI depends on all core modules
cli = ["parse", "graph", "search", "storage", "phase", "clap", "tokio"]

# Global depends on storage
global = ["storage"]

# Server depends on storage, graph, search
server = ["storage", "graph", "search", "tokio", "axum", "tower", "tower-http"]

# Edit depends on storage, graph, parse
edit = ["storage", "graph", "parse", "tokio"]

# Validation depends on parse, storage, graph
validation = ["parse", "storage", "graph", "jsonschema"]
```

---

## 8. Binary Migration

### 8.1 Binary Source Files

**src/bin/leindex.rs:**
```rust
//! LeIndex CLI - Main command-line interface

#![allow(unused_imports)]

use clap::Parser;
use leindex::cli::{Cli, Commands, run_command};
use tracing::{info, error};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt::init();
    
    // Parse CLI arguments
    let cli = Cli::parse();
    
    info!("LeIndex CLI starting");
    
    // Run command
    match run_command(cli.command).await {
        Ok(_) => {
            info!("Command completed successfully");
            Ok(())
        }
        Err(e) => {
            error!("Command failed: {}", e);
            Err(e)
        }
    }
}
```

**src/bin/leserve.rs:**
```rust
//! LeIndex Server - HTTP API server

use leindex::server::{ServerConfig, run_server};
use tracing::info;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt::init();
    
    info!("LeIndex Server starting");
    
    let config = ServerConfig::from_env();
    run_server(config).await
}
```

**src/bin/leedit.rs:**
```rust
//! LeIndex Editor - Code editing utilities

use leindex::edit::Editor;
use std::env;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    
    let editor = Editor::new();
    
    if args.len() < 2 {
        eprintln!("Usage: leedit <command> [args...]");
        std::process::exit(1);
    }
    
    match args[1].as_str() {
        "format" => editor.format(&args[2..]),
        "lint" => editor.lint(&args[2..]),
        _ => {
            eprintln!("Unknown command: {}", args[1]);
            std::process::exit(1);
        }
    }
}
```

### 8.2 Binary Configuration

```toml
[[bin]]
name = "leindex"
path = "src/bin/leindex.rs"
required-features = ["cli"]
doc = false

[[bin]]
name = "leserve"
path = "src/bin/leserve.rs"
required-features = ["server"]
doc = false

[[bin]]
name = "leedit"
path = "src/bin/leedit.rs"
required-features = ["edit"]
doc = false
```

### 8.3 MCP Catalog-Facing Runtime Contract

The unified crate must preserve the runtime commands that MCP catalogs and clients will use:

- **stdio transport:** `leindex mcp --stdio`
- **HTTP transport:** `leindex serve --host 127.0.0.1 --port 47268`

The unified binary/CLI layer should continue to support:

- `LEINDEX_HOME` for storage path override
- `LEINDEX_PORT` for HTTP port override

If any user-facing command or default changes, the same change must also update:

- `glama.json`
- README / INSTALLATION / MCP documentation
- MCP smoke tests

---

## 9. Test & Benchmark Migration

### 9.1 Test Organization

```
tests/
├── common/
│   └── mod.rs           # Shared test utilities
├── parse/
│   └── integration_tests.rs
├── graph/
│   └── integration_tests.rs
├── storage/
│   └── integration_tests.rs
├── search/
│   └── quantization_tests.rs
├── phase/
│   └── pipeline_tests.rs
└── e2e/
    └── end_to_end.rs    # Full workflow tests
```

### 9.2 Test Utilities Module

**tests/common/mod.rs:**
```rust
//! Common test utilities

use std::path::PathBuf;
use tempfile::TempDir;

/// Create a temporary directory for tests
pub fn temp_dir() -> TempDir {
    tempfile::tempdir().expect("Failed to create temp dir")
}

/// Get the crate root path
pub fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Setup test logging
pub fn init_test_logging() {
    let _ = tracing_subscriber::fmt::try_init();
}
```

### 9.3 Benchmark Organization

```
benches/
├── quantization.rs      # INT8 quantization benchmarks
├── search.rs            # Vector search benchmarks
├── phase.rs             # Phase pipeline benchmarks
└── common.rs            # Shared benchmark utilities
```

### 9.4 MCP Protocol and Catalog Tests

Add explicit MCP catalog-readiness coverage:

- stdio initialization advertises `tools`, `prompts`, and `resources`
- `tools/list` remains backward compatible
- `prompts/list` and `prompts/get` work
- `resources/list` and `resources/read` work
- invalid prompt/resource identifiers return structured JSON-RPC errors
- `glama.json` parses and matches documented commands/env vars

---

## 10. Feature Flag Strategy

### 10.1 Feature Combinations

| Use Case | Features to Enable |
|----------|-------------------|
| Full functionality | `full` (default) |
| Library use only | `parse`, `search` |
| CLI tool only | `cli` |
| Server only | `server` |
| Minimal build | `parse`, `storage` |

### 10.2 Feature Testing Matrix

Test all feature combinations in CI:

```yaml
# .github/workflows/features.yml
strategy:
  matrix:
    features:
      - "--no-default-features --features parse"
      - "--no-default-features --features parse,search"
      - "--no-default-features --features full"
      - "--all-features"
```

### 10.3 Conditional Compilation

```rust
// In source files
#[cfg(feature = "search")]
pub use search::SearchEngine;

#[cfg(all(feature = "search", feature = "storage"))]
pub mod indexed_search;

#[cfg(not(feature = "search"))]
compile_error!("Feature 'search' required for this module");
```

---

## 11. Edge Cases & Handling

### 11.1 Name Collisions

**Problem:** Two crates may have modules with the same name.

**Solution:** Namespacing under parent module prevents collisions:

```rust
// These are distinct:
leindex::parse::error::ParseError
leindex::graph::error::GraphError
leindex::storage::error::StorageError
```

### 11.2 Type Name Conflicts

**Problem:** Two crates may define types with the same name.

**Solution:** Use fully qualified paths:

```rust
// Instead of ambiguous:
use leindex::Config;

// Use specific:
use leindex::parse::Config as ParseConfig;
use leindex::server::Config as ServerConfig;
```

### 11.3 Re-export Conflicts

**Problem:** Crate re-exports may conflict.

**Solution:** Use #[doc(hidden)] for backward compatibility re-exports:

```rust
#[cfg(feature = "parse")]
#[doc(hidden)]
pub use parse as leparse;  // Backward compat

#[cfg(feature = "parse")]
pub mod parse;  // New preferred
```

### 11.4 Build Script Dependencies

**Problem:** Multiple crates have build.rs with different requirements.

**Solution:** Merge into single build.rs with conditional logic:

```rust
// build.rs
fn main() {
    #[cfg(feature = "validation")]
    generate_protobuf();
    
    #[cfg(feature = "parse")]
    embed_tree_sitter_grammars();
    
    // Always
    generate_version_info();
}
```

### 11.5 Circular Dependencies

**Problem:** Before migration, circular deps between crates.

**Solution:** Unification eliminates circular deps as everything is one crate. If circular logic exists, refactor:

```rust
// Instead of: crate A depends on B, B depends on A
// Create shared module:
pub mod shared {
    pub struct CommonType;
}

// Both A and B use:
use crate::shared::CommonType;
```

### 11.6 Feature Flag Circular Dependencies

**Problem:** Feature A requires B, B requires A.

**Solution:** Define feature hierarchy clearly:

```toml
[features]
# Base features (no internal deps)
parse = []

# Features with dependencies
graph = ["parse"]  # OK: parse has no deps
search = ["parse", "graph"]  # OK: both are base or depend on base
phase = ["parse", "graph", "search"]  # OK: DAG structure
```

### 11.7 Binary Feature Requirements

**Problem:** Binary requires features not enabled.

**Solution:** Use required-features in Cargo.toml:

```toml
[[bin]]
name = "leserve"
required-features = ["server"]
```

When building without the feature:
```bash
$ cargo build --bin leserve --no-default-features --features parse
error: target `leserve` requires the features: `server`
Consider enabling them by passing, e.g., `--features="server"`
```

### 11.8 Doc Test Failures

**Problem:** Doc tests use old import paths.

**Solution:** Update all doc examples:

```rust
/// # Examples
///
/// ```
/// // OLD
/// // use leparse::Parser;
/// 
/// // NEW
/// use leindex::parse::Parser;
/// 
/// let parser = Parser::new();
/// ```
```

### 11.9 Linker Errors

**Problem:** Duplicate symbols from static libraries.

**Solution:** Ensure no duplicate native dependencies:

```toml
# In Cargo.toml, use single version of native deps
[dependencies]
tree-sitter = "0.25"  # Only here, not in individual features
```

### 11.10 Test Isolation

**Problem:** Tests share global state between runs.

**Solution:** Use unique temporary directories:

```rust
#[test]
fn test_isolated() {
    let temp = tempfile::tempdir().unwrap();
    // Use temp.path() for all file operations
}
```

---

## 12. Verification Checklist

### 12.1 Compilation Checks

- [ ] `cargo check` passes
- [ ] `cargo check --all-features` passes
- [ ] `cargo check --no-default-features --features parse` passes
- [ ] `cargo check --no-default-features --features full` passes
- [ ] `cargo clippy -- -D warnings` passes
- [ ] `cargo check --bins` passes (all binaries compile)
- [ ] `cargo check --examples` passes (if any examples)

### 12.2 Build Checks

- [ ] `cargo build` succeeds
- [ ] `cargo build --release` succeeds
- [ ] `cargo build --all-features` succeeds
- [ ] Binary sizes are reasonable (compare to before)
- [ ] No linker errors

### 12.3 Test Checks

- [ ] `cargo test` passes (all unit tests)
- [ ] `cargo test --all-features` passes
- [ ] `cargo test --workspace` removed (not applicable)
- [ ] Integration tests pass
- [ ] Doc tests pass (`cargo test --doc`)

### 12.4 Benchmark Checks

- [ ] `cargo bench` runs successfully
- [ ] Benchmarks produce comparable results to pre-migration
- [ ] No benchmark compilation errors

### 12.5 Binary Checks

- [ ] `cargo run --bin leindex -- --help` works
- [ ] `cargo run --bin leserve -- --help` works (if applicable)
- [ ] `cargo run --bin leedit -- --help` works (if applicable)
- [ ] Binaries run without runtime errors

### 12.6 Documentation Checks

- [ ] `cargo doc` generates documentation
- [ ] No documentation warnings
- [ ] All public APIs are documented
- [ ] README.md is accurate
- [ ] CHANGELOG.md has migration notes

### 12.7 Publishing Checks

- [ ] `cargo publish --dry-run` passes
- [ ] Package size is reasonable (< 10MB)
- [ ] All necessary files are included
- [ ] .gitignore excludes build artifacts

### 12.8 Installation Checks

- [ ] `cargo install --path .` works
- [ ] Installed binaries are in PATH
- [ ] `leindex --version` shows correct version
- [ ] `leindex --help` shows help

### 12.9 Backward Compatibility Checks

- [ ] Re-exports work: `use leindex::leparse::X`
- [ ] New imports work: `use leindex::parse::X`
- [ ] Existing code examples still compile

### 12.10 Feature Flag Checks

- [ ] Each feature compiles independently
- [ ] Feature combinations work
- [ ] Default features are reasonable
- [ ] No feature conflicts

### 12.11 MCP Catalog Readiness Checks

- [ ] `glama.json` exists at repo root
- [ ] `glama.json` is valid JSON and stays in sync with README / INSTALLATION / MCP docs
- [ ] `glama.json` includes install methods for shell, macOS, PowerShell, and `cargo install leindex`
- [ ] `glama.json` documents runtime commands for `leindex mcp --stdio` and `leindex serve`
- [ ] Root `LICENSE` remains present and referenced by catalog-facing metadata/docs
- [ ] MCP `initialize` advertises `tools`, `prompts`, and `resources`
- [ ] `prompts/list` returns at least two prompts
- [ ] `prompts/get` returns valid prompt payloads for each declared prompt
- [ ] `resources/list` returns at least two resources
- [ ] `resources/read` returns valid resource payloads for each declared resource
- [ ] A LeIndex MCP usage skill exists in repository-visible form
- [ ] The skill is mirrored as at least one MCP prompt and one MCP resource
- [ ] Environment variable documentation includes `LEINDEX_HOME` and `LEINDEX_PORT`
- [ ] Existing friendly installation flows still work after unification

---

## 13. Rollback Plan

### 13.1 Before Migration

```bash
# 1. Create comprehensive backup
git checkout -b backup/pre-unification-$(date +%Y%m%d)
git push origin backup/pre-unification-$(date +%Y%m%d)

# 2. Document current state
git log --oneline -20 > pre-migration-git-log.txt
cargo tree > pre-migration-dependency-tree.txt

# 3. Tag current release
git tag v0.1.0-workspace-final
git push origin v0.1.0-workspace-final
```

### 13.2 During Migration Issues

**If compilation fails:**
```bash
# Stay on feature branch, fix issues iteratively
# Don't commit broken state
```

**If tests fail:**
```bash
# Identify failing tests
# Decide: fix test or fix code
# Don't disable tests without documenting why
```

### 13.3 Complete Rollback

**Scenario: Migration fails catastrophically**

```bash
# 1. Abandon current branch
git checkout main
git branch -D feature/unified-crate  # Delete problematic branch

# 2. Restore from backup
git checkout backup/pre-unification-$(date +%Y%m%d)
git checkout -b feature/unified-crate-v2

# 3. Alternative approach: try again with different strategy
# OR: Abandon unification, keep workspace
```

### 13.4 Partial Rollback

**Scenario: Published but issues discovered**

```bash
# 1. Yank problematic version from crates.io
cargo yank --version 0.2.0

# 2. Fix issues in new branch
git checkout -b hotfix/unification-issues

# 3. Publish fixed version
cargo publish

# 4. Update users via CHANGELOG and GitHub releases
```

### 13.5 Gradual Migration Alternative

If full unification is too risky:

1. **Phase 1:** Create facade crate (leindex) that re-exports all
2. **Phase 2:** Publish facade, keep workspace
3. **Phase 3:** Gradually merge crates into facade
4. **Phase 4:** Remove workspace when ready

This is less risky but more complex to maintain during transition.

---

## 14. MCP Catalog Readiness

### 14.1 Objective

After crate unification is complete, LeIndex must be ready for MCP catalog pages such as
Glama, MCPHub, LobeHub, and similar future directories. The unified crate should not only
compile and install cleanly, but also expose the metadata, prompts, resources, skill
guidance, and server configuration details that catalog ecosystems expect.

This is a portability goal, not a one-off integration for a single site.

### 14.2 Current State to Preserve

The repository already satisfies part of the catalog-readiness baseline and these items
must survive the unification unchanged or be updated without regressions:

1. **Friendly installation methods already exist:**
   - `install.sh`
   - `install_macos.sh`
   - `install.ps1`
   - `cargo install leindex`
2. **LICENSE already exists at repo root**
3. **MCP transports already exist:**
   - stdio via `leindex mcp --stdio`
   - HTTP via `leindex serve`

These should be treated as preserve-and-validate work, not redesigned unnecessarily.

### 14.3 Required Deliverable: `glama.json`

Add a root-level `glama.json` file as the canonical machine-readable metadata file for MCP
catalogs.

It must include at minimum:

- Server name and short description
- Repository URL
- License identifier/reference
- Version source
- Supported transports:
  - `stdio`
  - `http`
- Friendly installation methods:
  - shell installer
  - macOS installer
  - PowerShell installer
  - `cargo install leindex`
  - optional git install
- Runtime commands:
  - `leindex mcp --stdio`
  - `leindex serve --host <host> --port <port>`
- Environment variables table:
  - `LEINDEX_HOME` — optional — overrides storage root — default unset
  - `LEINDEX_PORT` — optional — overrides HTTP port — default `47268`

`glama.json` must remain synchronized with README, INSTALLATION, and MCP documentation.

### 14.4 Skill Requirement

LeIndex must provide at least one skill for catalog compliance.

Minimum required skill:

- **LeIndex MCP Usage Skill**

This skill should explain:

- When to use `leindex_search` vs `leindex_grep_symbols`
- When to use `leindex_deep_analyze` vs `leindex_context`
- How auto-indexing works
- A recommended first-pass investigation workflow for a new codebase

Because MCP does not define a first-class runtime skill primitive, implement this in
three aligned forms:

1. Repository-visible skill/guide document
2. MCP prompt
3. MCP resource

This keeps the same guidance usable across catalogs with different feature models.

### 14.5 Prompt Support

Expand the MCP server surface so the unified crate supports prompts in addition to tools.

Required protocol additions:

- `prompts/list`
- `prompts/get`

Required initialization change:

- `initialize` must advertise prompt capability alongside tools/resources

Minimum shipped prompts:

1. **LeIndex Quickstart**
2. **LeIndex Investigation Workflow**

Each prompt should include:

- Purpose
- When to use it
- Suggested tool sequence
- Example starter instruction

### 14.6 Resource Support

Expand the MCP server surface so the unified crate supports resources.

Required protocol additions:

- `resources/list`
- `resources/read`

Required initialization change:

- `initialize` must advertise resource capability alongside tools/prompts

Minimum shipped resources:

1. **Quickstart / Usage Guide**
2. **Server Configuration Reference**

Suggested stable resource URIs:

- `leindex://guides/quickstart`
- `leindex://config/server`

Resource content should be concise, attachable, and stored in version-controlled form.

### 14.7 Friendly Installation Methods

The unification must preserve easy installation and deployment paths for catalog users.

Validation requirements:

- Existing installer scripts still install a working `leindex` binary
- `cargo install leindex` continues to work for the unified crate
- Documentation examples continue to point to the correct binary and subcommands
- Catalog metadata references real, tested installation methods

### 14.8 LICENSE and Repository Metadata

The root `LICENSE` file must remain present after unification and be referenced in:

- README
- `glama.json`
- Any catalog-facing server metadata/docs

This is a mandatory preservation requirement.

### 14.9 Server Configuration Documentation

The unified plan must explicitly document the environment variables required or accepted by
the server.

Minimum documented variables:

| Name | Required | Description | Default |
|------|----------|-------------|---------|
| `LEINDEX_HOME` | No | Override storage/index home directory | unset |
| `LEINDEX_PORT` | No | Override HTTP serve port for `leindex serve` | `47268` |

This table must appear in both documentation and `glama.json`.

### 14.10 Catalog-Facing MCP Contract Changes

The unified crate should move from a tools-only advertised MCP capability surface to:

- `tools`
- `prompts`
- `resources`

Required new MCP methods:

- `prompts/list`
- `prompts/get`
- `resources/list`
- `resources/read`

Backward compatibility requirement:

- Existing `tools/list` and `tools/call` behavior must remain unchanged for current clients

### 14.11 Test Plan Additions

Add the following verification work to the unification implementation:

#### Protocol tests

- `initialize` returns tools + prompts + resources capabilities
- `prompts/list` returns the expected prompt set
- `prompts/get` returns valid content for each prompt
- `resources/list` returns the expected resource set
- `resources/read` returns valid content for each resource
- Invalid prompt/resource identifiers return structured JSON-RPC errors

#### Metadata tests

- `glama.json` parses successfully
- Required metadata keys are present
- Install commands in metadata map to real CLI commands
- Environment variable docs match actual runtime behavior

#### Regression tests

- `tools/list` still works for existing MCP clients
- `tools/call` still works for existing MCP clients
- Current installer flows remain valid after unification

### 14.12 Release Gate

The unified crate should not be considered complete for release/catalog publication until
all of the following are true:

1. `glama.json` exists and is valid
2. MCP prompts/resources are implemented and advertised
3. At least one LeIndex usage skill is present
4. INSTALLATION / README / MCP docs / metadata agree on install and runtime instructions
5. LICENSE and server configuration documentation remain intact

---

## Appendix A: Migration Command Reference

### A.1 One-Shot Migration Script

```bash
#!/bin/bash
# migrate-to-unified.sh

set -e

echo "=== LeIndex Unification Script ==="

# Phase 1: Backup
echo "Creating backup..."
git checkout -b backup/pre-unification-$(date +%Y%m%d)
git checkout -

# Phase 2: Create structure
echo "Creating directory structure..."
mkdir -p src/{parse,graph,storage,search,phase,cli,global,server,edit,validation,bin}
mkdir -p tests/{common,parse,graph,storage,search,phase,cli,global,server,edit,validation,e2e}
mkdir -p benches

# Phase 3: Copy and transform
echo "Migrating source files..."
for crate in leparse legraphe lestockage lerecherche lephase lepasserelle leglobal leserve leedit levalidation; do
    echo "  Processing $crate..."
    # (Copy and transform commands from Section 4)
done

# Phase 4: Create new Cargo.toml
echo "Creating unified Cargo.toml..."
# (Create Cargo.toml from Section 4)

# Phase 5: Create lib.rs
echo "Creating unified lib.rs..."
# (Create lib.rs from Section 4)

# Phase 6: Migrate binaries
echo "Migrating binaries..."
# (Copy and transform binaries)

# Phase 7: Migrate tests
echo "Migrating tests..."
# (Copy and transform tests)

# Phase 8: Verify
echo "Verifying migration..."
cargo check 2>&1 | head -50 || true

echo "=== Migration Complete ==="
echo "Next steps:"
echo "1. Review and fix any compilation errors"
echo "2. Run cargo test"
echo "3. Update documentation"
echo "4. Publish to crates.io"
```

### A.2 Import Fix Script

```bash
#!/bin/bash
# fix-imports.sh

echo "Fixing imports in src/..."

# Parse module
find src/parse -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;

# Graph module
find src/graph -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/graph -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;

# Storage module
find src/storage -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/storage -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/storage -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;

# Search module
find src/search -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;
find src/search -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/search -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;

# Phase module
find src/phase -name "*.rs" -exec sed -i 's/use lephase::/use crate::phase::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;
find src/phase -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;

# CLI module
find src/cli -name "*.rs" -exec sed -i 's/use lepasserelle::/use crate::cli::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/cli -name "*.rs" -exec sed -i 's/use lephase::/use crate::phase::/g' {} \;

# Global module
find src/global -name "*.rs" -exec sed -i 's/use leglobal::/use crate::global::/g' {} \;
find src/global -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;

# Server module
find src/server -name "*.rs" -exec sed -i 's/use leserve::/use crate::server::/g' {} \;
find src/server -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/server -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/server -name "*.rs" -exec sed -i 's/use lerecherche::/use crate::search::/g' {} \;

# Edit module
find src/edit -name "*.rs" -exec sed -i 's/use leedit::/use crate::edit::/g' {} \;
find src/edit -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/edit -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;
find src/edit -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;

# Validation module
find src/validation -name "*.rs" -exec sed -i 's/use levalidation::/use crate::validation::/g' {} \;
find src/validation -name "*.rs" -exec sed -i 's/use leparse::/use crate::parse::/g' {} \;
find src/validation -name "*.rs" -exec sed -i 's/use lestockage::/use crate::storage::/g' {} \;
find src/validation -name "*.rs" -exec sed -i 's/use legraphe::/use crate::graph::/g' {} \;

echo "Imports fixed!"
```

---

## Appendix B: Post-Migration Tasks

### B.1 Immediate Tasks

1. **Update CI/CD:**
   - Remove workspace matrix builds
   - Add feature flag testing matrix
   - Update artifact paths

2. **Update Documentation:**
   - Update README.md installation instructions
   - Update API.md references
   - Update CONTRIBUTING.md for new structure
   - Update ARCHITECTURE.md
   - Add/update `glama.json`
   - Add MCP prompt/resource/skill documentation
   - Document `LEINDEX_HOME` and `LEINDEX_PORT`

3. **Update Scripts:**
   - Update publish script (now single crate)
   - Update install scripts
   - Update benchmark scripts
   - Add metadata validation script/check for `glama.json`

### B.2 Short-Term Tasks

1. **Optimize Build Times:**
   - Profile build with `cargo build -Z timings`
   - Consider splitting heavy dependencies
   - Optimize feature flags

2. **Code Cleanup:**
   - Remove dead code discovered during migration
   - Consolidate duplicate utilities
   - Unify error handling

3. **API Review:**
   - Review public API surface
   - Consider deprecating old re-exports
   - Document breaking changes

### B.3 Long-Term Tasks

1. **Performance:**
   - Compare pre/post migration benchmarks
   - Optimize if regressions found

2. **Modularization:**
   - If build times suffer, consider workspace again
   - But keep unified for publishing

3. **Documentation:**
   - Create comprehensive migration guide for users
   - Document new feature flags
   - Update examples

---

## Conclusion

This unification plan provides a comprehensive roadmap for merging 10 workspace crates into a single `leindex` crate. The plan includes:

- **Detailed file mappings** for all 10 crates
- **Step-by-step migration phases** with time estimates
- **Comprehensive import transformation** rules
- **Feature flag strategy** for modular compilation
- **Edge case handling** for 10+ scenarios
- **Complete verification checklist** with 40+ items
- **Full rollback plan** for risk mitigation

**Key Success Factors:**
1. Thorough backup before starting
2. Iterative testing at each phase
3. Comprehensive import transformations
4. Clear feature flag dependencies
5. Complete verification before publishing

**Expected Outcome:**
- Single `cargo install leindex` command works
- Simplified versioning and publishing
- Maintained backward compatibility via re-exports
- Feature flags allow selective module inclusion
- Improved user experience

---

**Document Version:** 1.0  
**Last Updated:** 2026-02-22  
**Status:** Ready for Implementation
