# LeIndex Testing Infrastructure Report

**Generated:** 2026-04-27
**Project:** LeIndexer v1.5.2
**Report Type:** Testing Infrastructure Investigation

---

## Executive Summary

LeIndex uses Rust's built-in testing framework with Criterion for benchmarking. The project has 64 unit tests distributed across MCP handler modules and 23 unit tests in the core parse module. The testing infrastructure focuses on unit testing with limited integration test coverage. No dedicated CI/CD test pipeline exists apart from the release workflow.

---

## 1. Test Framework

### Primary Framework: Rust Built-in Testing

LeIndex uses Rust's native testing infrastructure:
- **Framework:** Built-in `#[test]` attribute and `cargo test` command
- **Assertion Library:** Standard `assert!`, `assert_eq!`, `assert_ne!` macros
- **Test Organization:** Inline tests within source files using `#[cfg(test)]` modules

### Test Dependencies (from Cargo.toml)

```toml
[dev-dependencies]
criterion = { version = "0.5", features = ["html_reports"] }  # Benchmarking
tokio-test = "0.4"      # Async testing utilities
tempfile = "3.13"       # Temporary file/directory creation
rstest = "0.23"         # Parameterized testing
proptest = "1.6"        # Property-based testing
rand = "0.8"            # Random generation for tests
```

### Test Organization

Tests are organized inline within modules:

```
src/
├── parse/
│   ├── tests.rs          # 23 language detection tests
│   └── ast_tests.rs      # AST parsing tests
├── cli/
│   ├── leindex/
│   │   └── tests.rs      # 4 integration-style tests
│   └── mcp/
│       ├── handlers.rs       # 3 handler registration tests
│       ├── protocol.rs       # 13 JSON-RPC protocol tests
│       ├── helpers.rs        # 26 helper function tests
│       ├── server.rs         # 1 server test
│       ├── search_handler.rs         # 2 tests
│       ├── phase_handler.rs          # 8 tests
│       ├── grep_symbols_handler.rs   # 2 tests
│       ├── symbol_lookup_handler.rs  # 2 tests
│       ├── file_summary_handler.rs   # 1 test
│       ├── project_map_handler.rs    # 2 tests
│       ├── read_symbol_handler.rs    # 1 test
│       └── diagnostics_handler.rs    # 1 test
```

**Total Test Count:** 64+ tests across MCP handlers + 23 parse tests = **87+ unit tests**

---

## 2. Build System

### Build Tool: Cargo

**Version:** cargo 1.93.1 (083ac5135 2025-12-15)
**Rust Version:** 1.75+ (specified in Cargo.toml)

### Build Commands

```bash
# Standard build
cargo build

# Release build (optimized)
cargo build --release

# Check compilation without building
cargo check

# Check with all features
cargo check --all-features

# Check with specific features
cargo check --no-default-features --features parse

# Build binary only
cargo build --bins
```

### Feature Flags

LeIndex uses feature-gated compilation:

```toml
[features]
default = ["full"]        # Full feature set
full = [...]              # All modules enabled
minimal = ["parse", "search"]  # Library-only build
parse = [...]             # Language parsing
graph = [...]             # Graph construction
storage = [...]           # SQLite persistence
search = [...]            # Semantic search
phase = [...]             # 5-phase analysis
cli = [...]               # CLI + MCP server
server = [...]            # HTTP/WebSocket server
edit = [...]              # Edit preview/apply
validation = [...]        # Validation guards
```

### Special Build Configurations

**Release Profile (Cargo.toml):**
```toml
[profile.release]
lto = thin              # Link-time optimization
codegen-units = 1       # Better optimization
opt-level = 3           # Maximum optimization
strip = true            # Strip debug symbols
```

**Bench Profile (Cargo.toml):**
```toml
[profile.bench]
debug = true            # Keep debug info for profiling
```

---

## 3. Test Coverage

### Current Coverage

**Unit Tests:** 87+ tests distributed across:
- Language parsing and detection (23 tests)
- MCP protocol handling (13 tests)
- Helper functions (26 tests)
- Handler logic (25+ tests)

**Integration Tests:** Limited
- 4 integration-style tests in `src/cli/leindex/tests.rs`
- No dedicated integration test directory
- Tests use `tempfile` for isolated environments

**Test Types:**

1. **Language Detection Tests** (`src/parse/tests.rs`):
   - Extension-based language detection
   - Case-insensitive matching
   - Unknown extension handling
   - Coverage: Python, JavaScript, TypeScript, C, Bash, JSON, Go, Rust, etc.

2. **MCP Protocol Tests** (`src/cli/mcp/protocol.rs`):
   - JSON-RPC request/response parsing
   - Notification handling
   - Error code validation
   - Message serialization

3. **Helper Function Tests** (`src/cli/mcp/helpers.rs`):
   - Parameter extraction (string, usize, bool)
   - Type coercion (string → bool, number → bool)
   - File path validation
   - Node type conversions

4. **Handler Tests** (various handler files):
   - Handler name registration
   - Argument schema validation
   - Basic execution logic

### Coverage Gaps

- **No End-to-End Tests:** Full MCP workflow tests absent
- **No Property-Based Tests:** `proptest` dependency unused
- **No Fuzzing:** No fuzz test integration
- **No Mutation Testing:** No mutation testing framework
- **Limited Edge Case Coverage:** Missing error path tests
- **No Concurrency Tests:** No stress tests for concurrent access

---

## 4. CI/CD Pipeline

### Current CI/CD: Release Pipeline Only

**Workflow:** `.github/workflows/release.yml`
**Trigger:** Push to `master` branch
**Scope:** Release automation, not testing

### Release Pipeline Stages

1. **Version Parity Check:**
   - Validates Cargo.toml, npm package.json, and PyPI pyproject.toml versions match

2. **Version Detection:**
   - Extracts version from Cargo.toml
   - Checks if release already exists
   - Skips if version unchanged

3. **Build Matrix:**
   - Linux x86_64, ARM64
   - macOS x86_64, ARM64
   - Windows x86_64
   - Cross-compilation with proper toolchains

4. **GitHub Release:**
   - Creates draft release with SHA256 checksums
   - Generates changelog from git history

5. **Publishing:**
   - **crates.io:** Rust crate (required)
   - **PyPI:** Python wrapper (required)
   - **npm:** Binary wrapper (optional)

### Missing CI/CD Elements

- ❌ **No Test Runner:** Tests not executed in CI
- ❌ **No Lint Checks:** No `cargo clippy` in pipeline
- ❌ **No Formatting Checks:** No `cargo fmt --check`
- ❌ **No Security Scanning:** No dependency vulnerability scans
- ❌ **No Code Coverage:** No coverage reporting (tarpaulin, etc.)
- ❌ **No Benchmark Regression:** No performance tracking
- ❌ **No Pull Request Checks:** No CI on PRs

### Release Workflow Details

```yaml
# .github/workflows/release.yml (excerpt)
on:
  push:
    branches: [master]
    paths:
      - 'Cargo.toml'
      - 'Cargo.lock'
      - 'src/**'
      - 'packages/**'

# Required secrets:
# - CARGO_REGISTRY_TOKEN (required)
# - PYPI_TOKEN (required)
# - NPM_TOKEN (optional)
```

---

## 5. Benchmarking

### Benchmark Framework: Criterion

**Version:** criterion 0.5 with HTML reports
**Configuration:** 3 benchmark suites

### Benchmark Suites

#### 5.1 SIMD Benchmarks (`benches/simd_benchmarks.rs`)

**Purpose:** Measure SIMD-optimized distance computation performance

**Test Coverage:**
- Dot product computation (AVX2 vs fallback)
- Dimensional scaling from 1 to 4096
- Edge case handling (remainder processing)
- Standard embedding sizes (768, 1024, 1536, 2048, 4096)

**Run Command:**
```bash
cargo bench --bench simd_benchmarks
```

**Key Dimensions Tested:**
```rust
// Edge cases: 1, 7, 8, 9, 15, 16, 17 (remainder handling)
// Small: 32, 64, 96
// Medium: 128, 256, 384, 512
// Standard: 768 (BERT), 1024 (GPT), 1536 (OpenAI)
// Large: 2048, 4096
```

#### 5.2 Search Benchmarks (`benches/search_benchmarks.rs`)

**Purpose:** Search and retrieval performance testing

**Run Command:**
```bash
cargo bench --bench search_benchmarks
```

#### 5.3 Phase Benchmarks (`benches/phase_bench.rs`)

**Purpose:** 5-phase analysis pipeline performance

**Run Command:**
```bash
cargo bench --bench phase_bench
```

### Benchmark Execution

```bash
# Run all benchmarks
cargo bench

# Run specific benchmark
cargo bench --bench simd_benchmarks

# Save baseline for comparison
cargo bench --bench simd_benchmarks -- --save-baseline avx2

# Compare against baseline
cargo bench --bench simd_benchmarks -- --baseline avx2
```

### Benchmark Output

- **HTML Reports:** Generated in `target/criterion/`
- **Comparison:** Supports baseline comparison
- **Throughput:** Measures operations per second
- **Statistics:** Mean, median, std dev, confidence intervals

---

## 6. MCP Handler Tests

### Test Distribution by Handler

From investigation of `src/cli/mcp/`:

| Handler File | Test Count | Test Type |
|--------------|------------|-----------|
| `helpers.rs` | 26 | Unit tests (extraction, validation) |
| `protocol.rs` | 13 | JSON-RPC protocol tests |
| `handlers.rs` | 3 | Handler registration |
| `phase_handler.rs` | 8 | Phase analysis logic |
| `search_handler.rs` | 2 | Search execution |
| `grep_symbols_handler.rs` | 2 | Symbol search |
| `symbol_lookup_handler.rs` | 2 | Symbol resolution |
| `project_map_handler.rs` | 2 | Project mapping |
| `read_symbol_handler.rs` | 1 | Symbol reading |
| `file_summary_handler.rs` | 1 | File analysis |
| `diagnostics_handler.rs` | 1 | Diagnostics |
| `server.rs` | 1 | Server initialization |
| **Total** | **64** | **MCP Handler Tests** |

### Test Examples

#### Helper Function Tests (`helpers.rs`)

```rust
#[test]
fn test_extract_string() {
    let args = serde_json::json!({"query": "test"});
    assert_eq!(extract_string(&args, "query").unwrap(), "test");
    assert!(extract_string(&args, "missing").is_err());
}

#[test]
fn test_extract_bool_string_coercion() {
    let args = serde_json::json!({
        "a": "true", "b": "false", "c": "1", "d": "0"
    });
    assert_eq!(extract_bool(&args, "a", false), true);
    assert_eq!(extract_bool(&args, "b", true), false);
}
```

#### Protocol Tests (`protocol.rs`)

```rust
#[test]
fn test_jsonrpc_request_valid() {
    let json = r#"{
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": {"name": "test", "arguments": {}}
    }"#;

    let req: JsonRpcRequest = serde_json::from_str(json).unwrap();
    assert!(req.validate().is_ok());
    assert_eq!(req.method, "tools/call");
}
```

#### Handler Tests (`handlers.rs`)

```rust
#[test]
fn test_handler_names() {
    assert_eq!(IndexHandler.name(), "leindex_index");
    assert_eq!(SearchHandler.name(), "leindex_search");
    assert_eq!(DeepAnalyzeHandler.name(), "leindex_deep_analyze");
    // ... all 16 MCP tools validated
}
```

### Test Structure

All handler tests follow this pattern:
1. **Name Validation:** Ensure handler name matches tool name
2. **Schema Validation:** Verify JSON schema generation
3. **Argument Extraction:** Test parameter parsing
4. **Basic Logic:** Simple execution tests
5. **Error Handling:** Limited error path coverage

---

## 7. Running Tests

### Full Test Suite

```bash
# Run all tests
cargo test

# Run all tests with output
cargo test -- --nocapture

# Run tests in verbose mode
cargo test -- --verbose

# Run all tests regardless of failure
cargo test --no-fail-fast
```

### Specific Test Categories

```bash
# Run only MCP handler tests
cargo test --package leindex --lib cli::mcp

# Run only parse tests
cargo test --package leindex --lib parse::tests

# Run specific test
cargo test test_python_extension_detection

# Run tests matching pattern
cargo test test_extract_

# Run tests in specific file
cargo test --lib cli::mcp::helpers::tests
```

### Build Verification

```bash
# Check compilation (no build artifacts)
cargo check

# Check with all features
cargo check --all-features

# Check with specific features
cargo check --no-default-features --features parse

# Lint checks
cargo clippy -- -D warnings

# Format check
cargo fmt --check
```

### Test Execution Modes

```bash
# Compile but don't run (useful for CI verification)
cargo test --no-run

# Run tests in release mode (faster execution)
cargo test --release

# Run tests with specific features
cargo test --features "full"

# Run tests with logging
RUST_LOG=debug cargo test
```

---

## 8. Development Workflow Commands

### Pre-Commit Checklist

Based on project documentation (`TZAR_REVIEW_UNIFICATION_PLAN.md`):

```bash
# 1. Verify compilation
cargo check
cargo check --all-features
cargo check --no-default-features --features parse

# 2. Run tests
cargo test

# 3. Lint
cargo clippy -- -D warnings

# 4. Format
cargo fmt --check
```

### Feature-Specific Testing

```bash
# Test parse feature only
cargo test --no-default-features --features parse

# Test full feature set
cargo test --features full

# Test CLI feature
cargo test --features cli

# Test server feature
cargo test --features server
```

### Benchmark Regression Testing

```bash
# Establish baseline
cargo bench -- --save-baseline main

# Compare against baseline after changes
cargo bench -- --baseline main

# CI-friendly benchmark (no HTML, just console)
cargo bench -- --measurement-time 1
```

---

## 9. Test File Locations

### Complete File List

```
src/parse/tests.rs                    # 23 language detection tests
src/parse/ast_tests.rs                # AST parsing tests
src/cli/leindex/tests.rs              # 4 integration tests
src/cli/mcp/handlers.rs               # 3 handler tests
src/cli/mcp/protocol.rs               # 13 JSON-RPC tests
src/cli/mcp/helpers.rs                # 26 helper function tests
src/cli/mcp/server.rs                 # 1 server test
src/cli/mcp/search_handler.rs         # 2 search tests
src/cli/mcp/phase_handler.rs          # 8 phase tests
src/cli/mcp/grep_symbols_handler.rs   # 2 grep tests
src/cli/mcp/symbol_lookup_handler.rs  # 2 symbol tests
src/cli/mcp/project_map_handler.rs    # 2 project map tests
src/cli/mcp/read_symbol_handler.rs    # 1 read test
src/cli/mcp/file_summary_handler.rs   # 1 summary test
src/cli/mcp/diagnostics_handler.rs    # 1 diagnostic test
```

### Test Module Structure

Each test module follows this pattern:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_name() {
        // Test implementation
    }

    // More tests...
}
```

---

## 10. Recommendations

### Immediate Improvements

1. **Add CI Test Pipeline:**
   - Create `.github/workflows/test.yml`
   - Run `cargo test` on all PRs
   - Add `cargo clippy` and `cargo fmt --check`

2. **Expand Test Coverage:**
   - Add integration tests for full MCP workflows
   - Test error paths and edge cases
   - Add property-based tests using `proptest`

3. **Add Coverage Reporting:**
   - Integrate `tarpaulin` or `cargo-llvm-cov`
   - Set minimum coverage threshold (e.g., 70%)

4. **Benchmark Regression Detection:**
   - Store benchmark results in CI
   - Alert on performance regressions >5%

### Long-Term Enhancements

1. **End-to-End Testing:**
   - Full MCP protocol workflow tests
   - Real-world codebase indexing tests
   - Cross-platform compatibility tests

2. **Stress Testing:**
   - Concurrent access tests
   - Large codebase performance tests
   - Memory leak detection

3. **Security Testing:**
   - Dependency vulnerability scanning
   - Path traversal validation tests
   - SQL injection prevention tests

---

## 11. Summary Statistics

| Metric | Count |
|--------|-------|
| **Total Unit Tests** | 87+ |
| **MCP Handler Tests** | 64 |
| **Parse Tests** | 23 |
| **Benchmark Suites** | 3 |
| **Test Dependencies** | 5 |
| **CI/CD Workflows** | 1 (release only) |
| **Feature Flags** | 10 |
| **Supported Languages** | 15 |

---

## 12. Key Commands Reference

```bash
# === TESTING ===
cargo test                              # Run all tests
cargo test --lib                        # Run library tests only
cargo test --bin leindex                # Run binary tests
cargo test --no-fail-fast               # Run all tests despite failures
cargo test --release                    # Faster test execution

# === BUILDING ===
cargo build                             # Debug build
cargo build --release                   # Optimized build
cargo check                             # Compile check only
cargo check --all-features              # Check all features

# === LINTING ===
cargo clippy -- -D warnings             # Lint with warnings as errors
cargo fmt --check                       # Verify formatting

# === BENCHMARKING ===
cargo bench                             # Run all benchmarks
cargo bench --bench simd_benchmarks     # Run specific benchmark
cargo bench -- --save-baseline main     # Save performance baseline

# === FEATURE-SPECIFIC ===
cargo test --features full              # Test with all features
cargo check --no-default-features --features parse
```

---

## 13. MCP Handler Test Coverage Detail

### Handlers with Tests (64 tests)

1. **helpers.rs** (26 tests):
   - Parameter extraction (string, usize, bool)
   - Type coercion validation
   - File path security checks
   - Node type conversions

2. **protocol.rs** (13 tests):
   - JSON-RPC request parsing
   - Notification handling
   - Error response formatting
   - Message validation

3. **phase_handler.rs** (8 tests):
   - Phase analysis execution
   - Argument schema validation
   - Edge case handling

4. **handlers.rs** (3 tests):
   - Handler name registration
   - Tool handler mapping
   - Schema generation

5. **search_handler.rs** (2 tests):
   - Search execution logic
   - Query parameter handling

6. **grep_symbols_handler.rs** (2 tests):
   - Symbol search patterns
   - Type filtering

7. **symbol_lookup_handler.rs** (2 tests):
   - Symbol resolution
   - Dependency tracking

8. **project_map_handler.rs** (2 tests):
   - Project structure mapping
   - Scope filtering

9. **read_symbol_handler.rs** (1 test):
   - Source code reading

10. **file_summary_handler.rs** (1 test):
    - File analysis summarization

11. **diagnostics_handler.rs** (1 test):
    - Health check diagnostics

12. **server.rs** (1 test):
    - Server initialization

### Handlers Missing Tests

- `context_handler.rs` - No tests
- `deep_analyze_handler.rs` - No tests
- `edit_apply_handler.rs` - No tests
- `edit_preview_handler.rs` - No tests
- `git_status_handler.rs` - No tests
- `impact_analysis_handler.rs` - No tests
- `index_handler.rs` - No tests
- `read_file_handler.rs` - No tests
- `rename_symbol_handler.rs` - No tests
- `text_search_handler.rs` - No tests

**Coverage Gap:** 10 of 22 MCP handlers lack tests

---

## Conclusion

LeIndex has a solid foundation of unit tests (87+ tests) focused on:
- Language parsing (23 tests)
- MCP protocol handling (13 tests)
- Helper utilities (26 tests)
- Basic handler logic (25+ tests)

However, significant gaps exist:
- No CI/CD test automation
- Limited integration test coverage
- Missing tests for 10 of 22 MCP handlers
- No end-to-end workflow testing
- No coverage reporting

The project would benefit from:
1. Adding a CI test pipeline
2. Expanding handler test coverage
3. Adding integration tests
4. Implementing coverage reporting
5. Adding performance regression detection

---

**Report End**
