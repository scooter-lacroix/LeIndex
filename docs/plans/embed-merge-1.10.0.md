# LeIndex Embed Merge and ONNX Provider Repair — 1.10.0 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Publish LeIndex 1.10.0 as one crates.io crate that still installs two processes (`leindex` and `leindex-embed`), compiles `onnx` without a GPU SDK, selects CPU/CUDA/MIGraphX/CoreML truthfully at runtime, and removes every active dependency on the retired `leindex-embed` crate.

**Architecture:** Merge source ownership, not process boundaries. Move worker code under `src/embed/`, preserve the worker executable and IPC isolation, move the superset TOML schema to shared `src/config.rs`, and explicitly rewrite the 27 internal `crate::...` paths instead of adding root alias modules. Upgrade to published `ort = 2.0.0-rc.13`, enable every referenced EP API from `leindex/onnx`, and remove the non-transitive workspace patch.

**Tech stack:** Rust 2024, Cargo feature/workspace/package contracts, `ort` load-dynamic API, bincode IPC, GitHub Actions, npm/PyPI wrappers.

---

## Audit annotations — fact corrections to original 106-line plan

**Evidence date:** 2026-08-01. **Tree:** `master` at `d8db82c3`. Line numbers below are pre-implementation and must be paired with symbol names after moves.

| Original claim | Verdict | Correction |
|---|---|---|
| 415 internal `crate::` refs | **False** | Exact scan finds **27** under `crates/leindex-embed/src/**/*.rs`. Rewrite explicitly; do not create root aliases. |
| CLI config mirrors embed config | **False** | CLI's 414-line schema has `rerank_enabled`, `rerank_top_n`, `query_type_for_mode`, and default 80; embed's 713-line copy omits them. Move CLI superset to root and retain one cache. |
| Config may live behind `onnx` | **False** | Default `full` enables `cli`, not `onnx`. Canonical config must compile for `cli OR onnx`. |
| Keep rc.12 git patch | **Release blocker** | Cargo patches do not propagate to consumers. `ort` rc.13 (published 2026-07-28) contains commit `17ed727`'s load-dynamic deadlock fix and is older than repository's three-day minimum on 2026-08-01. Pin rc.13 and remove patch. |
| Delete subcrate tests | **Coverage loss** | Migrate four integration suites (1,958 lines) plus 72 test lines from the 94-line subcrate binary. |
| Strict ONNX build is install simulation | **False** | It currently fails at `external_deps.rs:1490,1494` and `client.rs:1242,1289` because graph/search leak CLI-only symbols. Repair feature DAG. A workspace build is still not a packaged install. |
| Precedence is setup > env > TOML > auto | **False** | Setup writes TOML. Runtime is `LEINDEX_WORKER_EXECUTION_PROVIDER > TOML > auto selection > CPU outcome`. |
| Add first-class ROCm setup | **Rejected** | Runtime maps `rocm` to MIGraphX and never registers `ort::ep::ROCm`. Keep deprecated config/env alias only. |
| Reuse runtime selector before ORT install | **Unsafe** | `is_available()` needs initialized ORT. Setup chooses distribution from host/platform, then verifies loaded runtime and session. |
| Registration failure is observable | **False** | `ExecutionProviderDispatch` fails silently by default. Add `.error_on_failure()` after `.build()`. |
| Remove worker strip/verify/bundle | **False** | Those operate on preserved binary. Remove only crate publish/version/canary logic. |
| Quality has line-length exclusion | **False** | Stale paths are Lizard args at `quality.yml:68,78`. |
| Bump workflow reads embed manifest | **False** | It never does; it instead misses root/dashboard/pi surfaces. |
| Drop `/crates/**` from package include | **False** | No such entry exists. Add `/src/embed/**`, `/src/config.rs`, `/src/skip_dirs.rs`. |
| `docs/ARCHITECTURE.md` | **False path** | Correct file is root `ARCHITECTURE.md:1`. |
| Compiles on any host | **Qualified** | `load-dynamic` disables build-time ORT/SDK linking. Cross-platform source cfgs still need CI; runtime needs compatible EP-enabled ORT and vendor libraries. |

### Upstream facts to preserve

- rc.13 gates `ep::{CUDA,MIGraphX,ROCm,CoreML}` behind corresponding Cargo features.
- EP features are `ort-sys` marker/API features; with `load-dynamic`, they do not link CUDA/ROCm/MIGraphX/CoreML SDKs.
- `is_available()` only shows loaded ORT advertises EP. Session registration/inference is authoritative.
- Strict dispatch syntax is:

```rust
ort::ep::CUDA::default().build().error_on_failure()
```

- CPU is valid successful `auto` result, not an error.

## File inventory and current line counts

| Current file | Lines | Destination/responsibility |
|---|---:|---|
| `Cargo.toml` | 519 | One-crate features/deps/package/two bins |
| `Cargo.lock` | 5,320 | Remove subcrate package; registry rc.13 |
| `src/cli/neural_config.rs` | 414 | Move to canonical `src/config.rs` |
| `src/cli/skip_dirs.rs` | 51 | Move to shared `src/skip_dirs.rs` |
| `crates/leindex-embed/src/lib.rs` | 70 | `src/embed/mod.rs` without duplicate config |
| `batch.rs` | 467 | `src/embed/batch.rs` |
| `config.rs` | 713 | Delete after tests reconcile into root superset |
| `model_path.rs` | 335 | `src/embed/model_path.rs` |
| `ort_discovery.rs` | 1,203 | `src/embed/ort_discovery.rs` |
| `protocol.rs` | 627 | `src/embed/protocol.rs` |
| `provider.rs` | 638 | `src/embed/provider.rs` |
| `runtime.rs` | 1,940 | `src/embed/runtime.rs` |
| `runtime_test.rs` | 546 | `src/embed/runtime_test.rs` |
| `startup.rs` | 331 | `src/embed/startup.rs` |
| `worker_main.rs` | 695 | `src/embed/worker_main.rs` |
| subcrate `src/bin/leindex-embed.rs` | 94 | Move test module; delete duplicate main |
| `bundle_pipeline.rs` | 88 | `tests/embed_bundle_pipeline_test.rs` |
| `migraphx_dynamic.rs` | 424 | `tests/embed_migraphx_dynamic_test.rs` |
| `protocol_roundtrip.rs` | 397 | `tests/embed_protocol_roundtrip_test.rs` |
| `worker_lifecycle.rs` | 1,049 | `tests/embed_worker_lifecycle_test.rs` |
| `tests/onnx_worker_fallback.rs` | 804 | Rewrite imports |
| `tests/cargo_install_layout_test.rs` | 455 | Rewrite package contracts |
| `tests/release_bundle_packaging_test.rs` | 439 | Preserve two-bin bundle |
| `tests/cross_area_validation_test.rs` | 868 | Rewrite seven old paths |
| `src/cli/leindex/setup.rs` | 1,697 | Provider setup |
| `src/cli/leindex/setup_ort.rs` | 615 | ORT package/probe |
| `src/search/onnx/client.rs` | 1,951 | Parent precedence/spawn |
| `src/search/onnx/client_config.rs` | 928 | Discovery/cache; remove TOML scanner |
| `release.yml` / `quality.yml` / `bump-version.yml` | 953 / 360 / 201 | One-crate release/guards/parity |
| three public READMEs | 784 / 639 / 339 | Synchronized public contract |

## Non-negotiable invariants

1. One published crate, **two processes**. Worker retains model/session ownership and crash/OOM isolation.
2. Default install excludes ONNX dependencies and worker binary.
3. `cargo install leindex --features onnx` installs both binaries.
4. Root `build.rs` adds no ORT linking/rpath.
5. Discovery remains env → TOML → user lib → sibling lib → pip → system.
6. Env wins over TOML; direct worker gets same TOML fallback.
7. MIGraphX cache identity remains model + batch + sequence, never LeIndex version.
8. Diagnostics use `discover_path_only`; main process must not initialize ORT.
9. Protocol bytes, ordering, fallback, idle teardown, and socket behavior stay stable.
10. No test disappears merely because its directory disappears.

---

### Task 0: Capture baseline and establish rc.13 release gate

**Files:** Read `Cargo.toml:211-229,305-308,506-519`; generate ignored evidence only under `target/embed-merge-baseline/`.

- [ ] Run and record:

```bash
mkdir -p target/embed-merge-baseline
git status --short
git rev-parse HEAD
rg -n 'crate::' crates/leindex-embed/src --glob '*.rs' \
  | tee target/embed-merge-baseline/crate-paths.txt
wc -l target/embed-merge-baseline/crate-paths.txt
wc -l crates/leindex-embed/src/*.rs crates/leindex-embed/tests/*.rs
# Migration-sensitive suites + binary wrapper: expected 61.
rg -n '^\s*#\[(tokio::)?test\]' crates/leindex-embed/tests \
  crates/leindex-embed/src/bin/leindex-embed.rs \
  | tee target/embed-merge-baseline/migration-test-attributes.txt
# Entire retired crate, including unit tests embedded in copied modules: expected 193.
rg -n '^\s*#\[(tokio::)?test\]' crates/leindex-embed/src crates/leindex-embed/tests \
  | tee target/embed-merge-baseline/all-test-attributes.txt
wc -l target/embed-merge-baseline/{migration-test-attributes,all-test-attributes}.txt
cargo test -p leindex-embed --features onnx -- --list \
  | tee target/embed-merge-baseline/embed-test-list.txt
```

Expected: 27 path-match lines, 61 migration-sensitive test attributes, and 193 total test attributes. The source counts are authoritative deletion gates even if the harness-list command cannot finish because runtime/model prerequisites are absent. Evidence remains only under ignored `target/`.

- [ ] Record current boundaries without weakening assertions:

```bash
cargo check -p leindex --features onnx
cargo check -p leindex --no-default-features --features onnx
```

First may pass because git patch masks registry behavior; second currently exposes CLI/walkdir leakage.

- [ ] Capture comparison data:

```bash
/usr/bin/time -v cargo check -p leindex --features onnx \
  2>target/embed-merge-baseline/check-time.txt
cargo tree -p leindex --features onnx | wc -l \
  | tee target/embed-merge-baseline/dependency-lines.txt
cargo package -p leindex --allow-dirty --list \
  >target/embed-merge-baseline/package-files.txt
cargo package -p leindex --allow-dirty
stat -c '%s %n' target/package/leindex-1.9.0.crate \
  | tee target/embed-merge-baseline/crate-size.txt
```

- [ ] Confirm rc.13 resolves from crates.io and supports current APIs. Stop if not; adapt code to rc.13, never restore non-transitive patch as release solution.

### Task 1: Repair strict feature DAG before moving code

**Files:** Move `src/cli/skip_dirs.rs` → `src/skip_dirs.rs`; modify `src/lib.rs`, `src/cli/mod.rs`, 8 skip-dir callsites, `external_deps.rs:1467-1540`, `client.rs:1235-1290`, `Cargo.toml:106-158`.

- [ ] Establish failing gate:

```bash
cargo check -p leindex --all-targets --no-default-features --features onnx
```

- [ ] Move shared exclusions and declare:

```rust
/// Shared directory exclusions used by graph and CLI traversals.
#[cfg(any(feature = "graph", feature = "cli"))]
pub mod skip_dirs;
```

Delete `pub mod skip_dirs;` from `src/cli/mod.rs`; rewrite `crate::cli::skip_dirs::SKIP_DIRS` to `crate::skip_dirs::SKIP_DIRS`. Preserve constant contents exactly.

- [ ] Give graph its real dependency:

```toml
graph = ["parse", "dep:petgraph", "dep:walkdir"]
```

Do not make `onnx` imply `cli`.

- [ ] Gate only MCP metrics in `EmbeddingClient::embed_attempt`:

```rust
#[cfg(feature = "cli")]
crate::cli::mcp::request_meta::NEURAL_REQUESTS
    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

// existing inference + tracing

#[cfg(feature = "cli")]
crate::cli::mcp::request_meta::record_neural_ms(neural_ms);
```

- [ ] Verify and commit:

```bash
cargo check -p leindex --no-default-features --features graph
cargo check -p leindex --no-default-features --features search
cargo check -p leindex --all-targets --no-default-features --features onnx
cargo check -p leindex --all-targets
git add Cargo.toml src
git commit -m "fix: restore strict feature boundaries"
```

If graph tests reference CLI helpers, gate only those tests or move smallest shared helper; never add graph→CLI dependency.

### Task 2: Consolidate configuration without losing fields/cache

**Files:** Move `src/cli/neural_config.rs` (414) → `src/config.rs`; later delete embed copy (713); modify `Cargo.toml`, `src/lib.rs`, `src/cli/mod.rs`, 33 root references, and embed config callers.

- [ ] Before exposing the shared module to strict `onnx`, add `"dep:toml"` and `"dep:dirs"` to the root `onnx` feature. Both dependencies are already optional root dependencies but currently activate only through `cli`. Keep the existing `cli` activations. This is a transitional manifest edit retained by Task 3.

- [ ] Add tests to superset before move:

```rust
#[test]
fn test_config_roundtrip_preserves_rerank_fields() {
    let mut config = LeIndexConfig::default();
    config.search.rerank_enabled = true;
    config.search.rerank_top_n = 80;
    let decoded: LeIndexConfig =
        toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert!(decoded.search.rerank_enabled);
    assert_eq!(decoded.search.rerank_top_n, 80);
}

#[test]
fn test_default_execution_provider_is_auto() {
    assert_eq!(LeIndexConfig::default().neural.execution_provider, "auto");
}
```

- [ ] `git mv src/cli/neural_config.rs src/config.rs`. Do not start from narrower embed copy.

- [ ] Declare:

```rust
/// User configuration shared by CLI and ONNX worker code.
#[cfg(any(feature = "cli", feature = "onnx"))]
pub mod config;
```

Remove `pub mod neural_config;`; rewrite `crate::cli::neural_config::` → `crate::config::`.

- [ ] Preserve the superset's existing process cache at current `neural_config.rs:256-267`; do **not** add a second module-static cache. Retain this implementation verbatim through the move:

```rust
pub fn load_cached() -> &'static LeIndexConfig {
    static CACHED: std::sync::OnceLock<LeIndexConfig> = std::sync::OnceLock::new();
    CACHED.get_or_init(|| {
        Self::load().unwrap_or_else(|err| {
            tracing::warn!(
                error = %err,
                "failed to load leindex.toml for caching; using defaults"
            );
            LeIndexConfig::default()
        })
    })
}
```

Port embed cache/recovery assertions to this method. Env-mutating tests must share one lock and must not expect `OnceLock` reset.

- [ ] Replace `client_config.rs` line scanner:

```rust
pub(super) fn read_worker_config_env_from_config() -> WorkerConfigEnv {
    let config = crate::config::LeIndexConfig::load_cached();
    let provider = config.neural.execution_provider.trim().to_ascii_lowercase();
    WorkerConfigEnv {
        ort_dylib_path: config.neural.ort_dylib_path.clone(),
        execution_provider: (provider != "auto" && !provider.is_empty()).then_some(provider),
        model_name: (!config.neural.model_name.trim().is_empty())
            .then(|| config.neural.model_name.clone()),
    }
}
```

Keep `auto` omitted from child env. Remove parser helpers only when unused.

- [ ] Port embed-only config/cache/recovery tests now; retain the old copy only as a transitional package implementation until Task 6 deletes the retired crate. Verify:

```bash
cargo test -p leindex --features cli config
cargo check -p leindex --no-default-features --features cli
cargo check -p leindex --no-default-features --features onnx
rg -n 'cli::neural_config|mod neural_config' src
git add src
git commit -m "refactor: unify neural configuration"
```

Expected final search: no active matches.

### Task 3: Upgrade ort and prepare the transitional ONNX feature

**Files:** Root `Cargo.toml:211-229,249-325,506-519`; temporary subcrate `crates/leindex-embed/Cargo.toml:18-53`; generated `Cargo.lock`; `cargo_install_layout_test.rs:217-289`.

**Dependency-order rule:** This task makes registry rc.13 and all EP APIs available, but it must **not** remove the old crate dependency yet. Existing root client/bin imports still use `leindex_embed` until Task 4. Task 6 removes the temporary dependency after source and tests have moved.

- [ ] First extend the static manifest test to require `ort/{cuda,migraphx,rocm,coreml}` in root `onnx` while temporarily retaining `leindex-embed/onnx`. Confirm the new EP assertions fail. Defer the assertion forbidding `leindex-embed/onnx` until Task 6.

- [ ] Replace features:

```toml
onnx = [
    "search",
    "dep:ort",
    "ort/half",
    "ort/cuda",
    "ort/migraphx",
    "ort/rocm",
    "ort/coreml",
    "dep:tokenizers",
    "dep:ndarray",
    "dep:half",
    "dep:which",
    "dep:toml",
    "dep:dirs",
    # Transitional until Task 6; keeps current imports buildable during Tasks 2–4.
    "leindex-embed/onnx",
]

# Legacy build-command alias; `onnx` now compiles every runtime-selectable EP.
onnx-migraphx = ["onnx"]
```

- [ ] Add direct deps, with EP flags only above:

```toml
ort = {
    version = "=2.0.0-rc.13",
    optional = true,
    default-features = false,
    features = ["ndarray", "std", "tracing", "load-dynamic", "api-23"],
}
tokenizers = { version = "0.20", optional = true }
ndarray = { version = "0.17", optional = true }
half = { version = "2.1", optional = true, default-features = false }
```

- [ ] In the temporary subcrate manifest, pin the same exact rc.13 and enable `cuda`, `migraphx`, `rocm`, and `coreml` through its `onnx` feature so the still-active source compiles during the transition. Remove the entire root `[patch.crates-io]`; never replace it with another git patch.

- [ ] Retain both root dependency and dev-dependency entries for `leindex-embed` through Task 5. Add a nearby `# Transitional: remove in Task 6 after imports/tests move` comment so a small model does not mistake them for final architecture. Retain root `[[bin]] leindex-embed` permanently.

- [ ] Resolve/prove registry source:

```bash
cargo update -p ort --precise 2.0.0-rc.13
cargo tree -p leindex --no-default-features --features onnx -i ort
rg -n 'git\+https://github.com/pykeio/ort' Cargo.lock
cargo check -p leindex --all-targets --no-default-features --features onnx
cargo check -p leindex --all-targets --no-default-features --features onnx-migraphx
git add Cargo.toml Cargo.lock tests/cargo_install_layout_test.rs
git commit -m "fix: enable all ONNX execution providers"
```

Expected: both packages resolve registry rc.13, no pykeio git source, no E0433, no SDK linker requirement. The lockfile still contains a workspace `leindex-embed` package until Task 6; that is intentional.

### Task 4: Move worker source with explicit module paths

**Files:** Create temporary copies at `src/embed/{mod,batch,model_path,ort_discovery,protocol,provider,runtime,runtime_test,startup,worker_main}.rs`; modify root lib/bin and listed consumers. Keep the old crate buildable through test migration; Task 6 records the final copies as Git renames when deleting tracked originals.

- [ ] Copy everything except duplicate config and duplicate binary. **Do not `git mv` yet**: root still carries a transitional dependency and Task 5 needs the old package test harness for comparison.

```bash
mkdir -p src/embed
cp crates/leindex-embed/src/lib.rs src/embed/mod.rs
cp crates/leindex-embed/src/{batch,model_path,ort_discovery,protocol,provider,runtime,runtime_test,startup,worker_main}.rs src/embed/
```

Remove `pub mod config` and config re-exports from `mod.rs`; convert opening description to `//!` docs.

- [ ] Add root declaration:

```rust
/// ONNX worker protocol and implementation.
///
/// Public for package worker binary and integration tests; not stable user API.
#[cfg(feature = "onnx")]
#[doc(hidden)]
pub mod embed;
```

Do **not** add root aliases.

- [ ] Rewrite all 27 internal paths:

```text
crate::batch         -> crate::embed::batch
crate::model_path    -> crate::embed::model_path
crate::ort_discovery -> crate::embed::ort_discovery
crate::protocol      -> crate::embed::protocol
crate::provider      -> crate::embed::provider
crate::runtime       -> crate::embed::runtime
crate::startup       -> crate::embed::startup
crate::test_util     -> crate::embed::test_util
crate::config        -> crate::config
```

Example:

```rust
use crate::embed::model_path::ModelResolver;
use crate::embed::protocol::{BatchId, EmbedRequest, EmbedResponse};
use crate::embed::provider::ExecutionProviderSelector;
```

- [ ] Root wrapper becomes:

```rust
fn main() {
    leindex::embed::worker_main::run()
}
```

Preserve binary name/required feature.

- [ ] Update consumers at current locations: `cli.rs:1158`; `setup_ort.rs:428,432`; `hybrid.rs:318`; `client.rs:33-36,1540`; `client_config.rs:166-170,266-267`. Library code uses `crate::embed`; integration tests use `leindex::embed`.

- [ ] Move tests from subcrate binary lines 23-94 into `worker_main.rs` test module (or declared `worker_main_test.rs`). Preserve one-request and run-loop tests.

- [ ] Verify before deleting shell:

```bash
cargo check -p leindex --all-targets --features onnx
cargo build -p leindex --bin leindex-embed --features onnx
./target/debug/leindex-embed --version
rg -n 'crate::(batch|model_path|ort_discovery|protocol|provider|runtime|startup|test_util)' src/embed
git add src
git commit -m "refactor: move embed worker into leindex"
```

Every final rg result must start `crate::embed::...`.

### Task 5: Migrate every test before deleting crate

**Files:** Four subcrate suites; `tests/onnx_worker_fallback.rs` (804); worker unit tests.

- [ ] Load the complete baseline captured in Task 0. Do not rescan only name prefixes: the old name-based scan found 55 migration functions but missed six tests. Baselines are **61 migration-sensitive attributes** (four integration suites + binary wrapper) and **193 total attributes** (including unit tests embedded in copied modules). If Task 0 did not produce all three evidence files, stop and regenerate them before moving tests.

- [ ] Move with required names:

```bash
git mv crates/leindex-embed/tests/bundle_pipeline.rs tests/embed_bundle_pipeline_test.rs
git mv crates/leindex-embed/tests/migraphx_dynamic.rs tests/embed_migraphx_dynamic_test.rs
git mv crates/leindex-embed/tests/protocol_roundtrip.rs tests/embed_protocol_roundtrip_test.rs
git mv crates/leindex-embed/tests/worker_lifecycle.rs tests/embed_worker_lifecycle_test.rs
```

- [ ] Rewrite `leindex_embed::...` → `leindex::embed::...` in moved suites, copied binary tests, and `onnx_worker_fallback.rs`. Rename the six pre-existing nonconforming test functions to descriptive `test_*` names without changing assertions. Preserve runtime skip conditions; never convert real failures to skips.

- [ ] Lifecycle still resolves/spawns `leindex-embed`; update only package ownership assumptions. Preserve Windows `.exe` behavior.

- [ ] Run:

```bash
cargo test -p leindex --features onnx --test embed_protocol_roundtrip_test
cargo test -p leindex --features onnx --test embed_bundle_pipeline_test
cargo test -p leindex --features onnx --test embed_worker_lifecycle_test
cargo test -p leindex --features onnx --test embed_migraphx_dynamic_test
cargo test -p leindex --features onnx --test onnx_worker_fallback
```

- [ ] Recount moved + worker tests and reconcile every difference with baseline. Commit:

```bash
git add tests src/embed/worker_main.rs
git commit -m "test: migrate embed worker coverage"
```

### Task 6: Remove subcrate shell and rewrite package contracts

**Files:** Delete remaining subcrate manifest/build/bin; modify root manifest/lock; three static contract tests.

- [ ] Add package includes:

```toml
    "/src/config.rs",
    "/src/skip_dirs.rs",
    "/src/embed/**",
```

There is no `/crates/**` line to remove.

- [ ] Keep:

```toml
[[bin]]
name = "leindex-embed"
path = "src/bin/leindex-embed.rs"
required-features = ["onnx"]
doc = false
```

- [ ] Now remove the transitional root dependency/dev-dependency, `"leindex-embed/onnx"` feature propagation, workspace member, and any obsolete manifest assertions. Regenerate `Cargo.lock` and assert its package table no longer contains `name = "leindex-embed"`.

- [ ] Inspect `git status --short crates/leindex-embed` and `git ls-files crates/leindex-embed`. Delete only the now-copied tracked source/tests/manifests with `git rm`; preserve and report any unknown or untracked data. Remove empty directories afterward. Never broad-delete unknown data.

- [ ] Rewrite `cargo_install_layout_test` to assert:

```rust
assert!(!repo_root().join("crates/leindex-embed").exists());
assert!(repo_root().join("src/embed/mod.rs").is_file());
assert!(repo_root().join("src/embed/worker_main.rs").is_file());
assert!(repo_root().join("src/bin/leindex-embed.rs").is_file());
```

Require wrapper `leindex::embed::worker_main::run`; remove root/subcrate version parity; inspect only root `build.rs` for ORT links/rpath.

- [ ] In `cross_area_validation_test.rs`, map seven old runtime/discovery paths to `src/embed/...` and replace version parity with one-crate/two-bin assertion. In bundle test, inspect root build script only but keep both bundle binaries.

- [ ] Regenerate and verify:

```bash
cargo check -p leindex --features onnx
! rg -n 'name = "leindex-embed"' Cargo.lock
! rg -n 'leindex-embed/onnx|leindex_embed::' Cargo.toml src tests
# The retained binary target must still be present.
rg -n '^name = "leindex-embed"$' Cargo.toml
cargo test -p leindex --test cargo_install_layout_test
cargo test -p leindex --test cross_area_validation_test
cargo test -p leindex --test release_bundle_packaging_test
git add Cargo.toml Cargo.lock src tests crates/leindex-embed
git commit -m "build: retire leindex-embed crate"
```

Old crate package/dependency/symbol matches must be gone from the scoped files; binary-name strings remain valid. Do not include `.github` in this Task 6 zero-match gate because workflow cleanup is Task 10.

### Task 7: Make provider selection truthful and portable

**Files:** `embed/provider.rs:68-385`; `embed/runtime.rs:202-294,389-512,687-875`; `client_config.rs:105-147,865-916`; tests.

- [ ] Extract pure auto helper and add hardware-independent test:

```rust
#[test]
fn test_auto_order() {
    assert_eq!(select_auto_from_availability(true, true, true), Provider::CoreMl);
    assert_eq!(select_auto_from_availability(false, true, true), Provider::Migraphx);
    assert_eq!(select_auto_from_availability(false, false, true), Provider::Cuda);
    assert_eq!(select_auto_from_availability(false, false, false), Provider::Cpu);
}
```

- [ ] Normalize once with `trim().to_ascii_lowercase()`. Accept `auto`, `cpu`, `cuda`, legacy `gpu`, `migraphx`, deprecated alias `rocm`, `coreml`. Unknown explicit value gets actionable CPU-fallback reason.

- [ ] Direct-worker precedence:

```rust
let execution_provider = std::env::var("LEINDEX_WORKER_EXECUTION_PROVIDER")
    .ok()
    .filter(|value| !value.trim().is_empty())
    .or_else(|| {
        let value = crate::config::LeIndexConfig::load_cached()
            .neural.execution_provider.trim().to_ascii_lowercase();
        (!value.is_empty()).then_some(value)
    })
    .unwrap_or_else(|| "auto".to_string());
```

Tests prove env > TOML > auto using shared env lock.

- [ ] Auto order: usable CoreML → MIGraphX → CUDA → CPU. CoreML requires platform support **and** initialized-runtime availability. Auto→CPU returns `Ok`, not fallback error.

- [ ] Keep `rocm` parser compatibility but resolve to MIGraphX/CPU; never register `ort::ep::ROCm` or advertise `onnxruntime-rocm`.

- [ ] Force registration errors:

```rust
ort::ep::CUDA::default().build().error_on_failure()
build_migraphx_ep().error_on_failure()
ort::ep::CoreML::default().build().error_on_failure()
```

Preserve common session options when rebuilding CPU fallback.

- [ ] Resolve Auto before attachment; remove `"migraphx" | "auto"` arm. Unresolved Auto is internal error, not implicit MIGraphX. Audit **every** session builder, including lazy `WorkerRuntime::ensure_rerank_session`: it currently passes configured `auto` directly. Change it to use `self.provider_runtime_status.execution_provider.as_str()` after selection. Add a hardware-independent test proving embed and rerank session construction receive only concrete `cpu`, `cuda`, `migraphx`, or `coreml`, never `auto`.

- [ ] Report requested, active concrete provider, and fallback reason separately. Explicit GPU→CPU preserves current neural fallback; Auto→CPU is normal.

- [ ] Run/commit:

```bash
cargo test -p leindex --features onnx provider
cargo test -p leindex --features onnx runtime_config_provider
cargo test -p leindex --features onnx auto_
cargo test -p leindex --features onnx rocm_alias
git add src/embed src/search/onnx
git commit -m "fix: make ONNX provider selection truthful"
```

### Task 8: Extend setup with Auto/CoreML, not ROCm

**Files:** `setup.rs:33-62,141-185,203-357,603-734,831-1013,1094-1201`; `setup_ort.rs:125-256,428-455`; `setup_models.rs:27-41,549-556`; `cli.rs:1474-1485`.

- [ ] Add mapping test and enum:

```rust
pub enum ExecutionProvider { Auto, Cpu, Cuda, Migraphx, CoreMl }

#[test]
fn test_execution_provider_config_values() {
    assert_eq!(ExecutionProvider::Auto.config_value(), "auto");
    assert_eq!(ExecutionProvider::Cpu.config_value(), "cpu");
    assert_eq!(ExecutionProvider::Cuda.config_value(), "cuda");
    assert_eq!(ExecutionProvider::Migraphx.config_value(), "migraphx");
    assert_eq!(ExecutionProvider::CoreMl.config_value(), "coreml");
}
```

- [ ] Package mapping:

```rust
Self::Auto | Self::Cpu | Self::CoreMl => "onnxruntime",
Self::Cuda => "onnxruntime-gpu",
Self::Migraphx => "onnxruntime-migraphx",
```

Do not call `Auto.pip_package()` before resolving install candidate.

- [ ] Bare `--neural` becomes Auto; explicit CPU/AMD/NVIDIA unchanged. Update VAL-SETUP-009 tests/docs.

- [ ] Separate configured policy from install candidate:

```text
explicit provider -> same
Auto + Apple -> CoreML
Auto + AMD on supported Linux x86_64 -> MIGraphX
Auto + NVIDIA -> CUDA
Auto + no usable accelerator -> CPU
```

Persist `auto` even when concrete package candidate was installed.

- [ ] Menus: Apple offers Auto/CoreML/CPU. Other supported hosts offer Auto/CPU/CUDA and MIGraphX only on supported platforms. Do not offer impossible combinations.

- [ ] Probe order: host/vendor → install distribution → discover/init dylib → provider availability → real session/inference smoke test. Runtime selector is not pre-install authority.

- [ ] Python mapping:

```rust
ExecutionProvider::CoreMl => "CoreMLExecutionProvider",
ExecutionProvider::Cuda => "CUDAExecutionProvider",
ExecutionProvider::Migraphx => "MIGraphXExecutionProvider",
ExecutionProvider::Cpu => return true,
ExecutionProvider::Auto => return false,
```

- [ ] Smoke match: cpu→cpu, cuda→cuda, migraphx→migraphx, legacy rocm→migraphx, coreml→coreml, auto→any concrete including CPU.

- [ ] MIGraphX warmup uses **actual smoke-test provider**, not requested enum. Warm only active MIGraphX.

- [ ] Test/commit using pure tests only:

```bash
cargo test -p leindex --features onnx setup
cargo test -p leindex --features onnx test_execution_provider_config_values
cargo test -p leindex --features onnx test_setup_persists_auto_provider
git add src/cli
git commit -m "feat: add automatic ONNX provider setup"
```

Implement `test_setup_persists_auto_provider` through the existing config-writer seam and a temp `LEINDEX_HOME`; it must not invoke pip or model downloads. Do **not** run bare `cargo run --features onnx` (two binaries make it ambiguous), and do not invoke live setup outside a disposable Python virtualenv because setup can run `pip install --upgrade`. Real install/smoke belongs only to provisioned Task 12 jobs; if manually exercised there, use `cargo run --bin leindex --features onnx -- setup --neural` with isolated `HOME`, `LEINDEX_HOME`, virtualenv, and `PIP_BIN`.

### Task 9: Reject stale PATH workers with minimal overhead

**Files:** `client_config.rs:70-103`; resolution tests.

- [ ] Test order: valid explicit `LEINDEX_WORKER_PATH` → sibling → test target parent → PATH candidate validated by `--version`.

- [ ] Explicit bad path returns actionable error; never silently falls through.

- [ ] Validate only PATH fallback by running `<candidate> --version`; require successful output `leindex-embed <compatible-version>`. Use exact `env!("CARGO_PKG_VERSION")` unless protocol compatibility is explicitly versioned. Sibling/override avoid extra process.

- [ ] Test stale PATH rejection with temp executable, plus Windows `.exe` helper; never use user's PATH.

```bash
cargo test -p leindex --features onnx worker_binary
git add src/search/onnx/client_config.rs
git commit -m "fix: reject stale embed workers"
```

### Task 10: Update CI/release/path ownership

**Files:** `quality.yml` (360), `release.yml` (953), performance workflow (36), Dependabot, bump workflow (201), active comments/tests.

- [ ] Add compile-oriented Linux matrix:

```yaml
matrix:
  include:
    - { name: default, args: "" }
    - { name: minimal, args: "--no-default-features --features minimal" }
    - { name: onnx-install-path, args: "--no-default-features --features onnx" }
    - { name: all-features, args: "--all-features" }
```

Each runs:

```bash
cargo check -p leindex --all-targets $ARGS
cargo test -p leindex --no-run $ARGS
```

ONNX row also checks legacy `onnx-migraphx`. Do not execute runtime ONNX tests without ORT/models.

- [ ] Add one `macos-latest` CoreML compile check with strict ONNX features. No GPU runner needed.

- [ ] Remove old embed Lizard args (`src/` already covers moved code); replace performance trigger with `src/embed/**`; remove `/crates/leindex-embed` Dependabot entry.

- [ ] Release: remove only `WORKER_VER`, separate `cargo publish -p leindex-embed`, its visibility wait, and two-crate canary. Keep root `-p leindex --bins --features leindex/onnx`, both strip/verify loops, and both bundle copies.

- [ ] One-crate canary:

```bash
cargo search leindex 2>/dev/null | grep -q "^leindex = \"${VERSION}\""
```

- [ ] Encode, do not merely document, this job graph: `create-release` (draft) → `publish-crates` → `crates-index-ready` → `undraft-release` → parallel `publish-npm`/`publish-pypi` → final canaries/summary. `crates-index-ready` must succeed both when this run publishes and when version already exists; `undraft-release` must not be skipped in the already-published case. npm must never point at private draft assets.

- [ ] Expand bump workflow to root/dashboard/pi/npm JSON, PyPI project/runtime, three installers, Cargo lock. It never had embed-manifest reads. Prose remains manual but parity gate must catch stale current markers.

- [ ] In `packages/npm-leindex-mcp/install.js:865-871`, delete the second active `cargo install leindex-embed --features onnx --force` fallback. At current line 654 and in the deleted fallback, replace misleading “in-process fallback” claims with an actionable missing-worker error/fallback statement that matches actual client behavior. The preceding `cargo install leindex --force --features onnx` must install both binaries. Add a static test/guard rejecting active command/package forms `cargo install leindex-embed`, `-p leindex-embed`, and `leindex-embed/onnx` while allowing executable-name references.

- [ ] Update active old-path comments at `install.sh:749`, npm `install.js:30`, wrapper `:29`, root worker comments, client config comments, and current TASKLIST/Tracker entries. Historical changelog/plans remain unchanged.

```bash
cargo test -p leindex --test release_bundle_packaging_test
cargo test -p leindex --test cargo_install_layout_test
git add .github install.sh packages/npm-leindex-mcp src TASKLIST.md Tracker.md tests
git commit -m "ci: release one crate with two binaries"
```

### Task 11: Bump all 1.10.0 surfaces and synchronize docs

**Files:** Machine versions, three READMEs, current install/architecture/setup/performance docs, changelog/release notes.

- [ ] Set 1.10.0 in:

```text
Cargo.toml:3
package.json:3
dashboard/package.json:4
pi/package.json:3
packages/npm-leindex-mcp/package.json:3
packages/npm-leindex-mcp/README.md:228
packages/npm-leindex-mcp/test.js:105,108
packages/pypi-leindex/pyproject.toml:7
packages/pypi-leindex/src/leindex/__init__.py:9
install.sh:4,28
install_macos.sh:4,20
install.ps1:3,24,27
```

Regenerate `Cargo.lock`; never hand-edit it. Confirm no `leindex-embed` package entry.

- [ ] Classify fixture `tests/fixtures/memcheck/small_repo/.leindex/.leindex-artifact-marker:3`: bump if current writer output; retain with test comment if backward-compat data.

- [ ] Prepend `[1.10.0]` changelog and rewrite release notes from verified behavior: one crate/two binaries, rc.13 fix, all EP APIs, Auto/CoreML, deprecated rocm alias, no build-time SDK, unchanged runtime requirements.

- [ ] Update current versions/content in `INSTALLATION.md:3`, `INSTALLATION_RUST.md:3`, root `ARCHITECTURE.md:1`, `docs/MCP.md` near 1720, `docs/NEURAL_SETUP.md:3,38`, `docs/PERFORMANCE_BENCHMARKS.md:3,69`.

- [ ] Synchronize `README.md`, PyPI README, npm README per AGENTS. Explain one crate/two processes, install command, worker, provider values, runtime requirements. Preserve valid binary/bundle references.

- [ ] Do not mass-replace historical changelog/migration/plans, Go fixture versions, or ORT sort fixture data.

- [ ] Search every remaining active 1.9/path match and classify:

```bash
rg -n '1\.9\.0|crates/leindex-embed' README.md packages/pypi-leindex/README.md \
  packages/npm-leindex-mcp/README.md INSTALLATION.md INSTALLATION_RUST.md \
  ARCHITECTURE.md RELEASE_NOTES.md docs/NEURAL_SETUP.md \
  docs/PERFORMANCE_BENCHMARKS.md
```

- [ ] Commit all surfaces together:

```bash
git add Cargo.toml Cargo.lock package.json dashboard/package.json pi/package.json \
  packages install.sh install_macos.sh install.ps1 CHANGELOG.md RELEASE_NOTES.md \
  README.md INSTALLATION.md INSTALLATION_RUST.md ARCHITECTURE.md docs tests/fixtures
git commit -m "release: prepare LeIndex 1.10.0"
```

### Task 12: Real package/install/performance/full verification

**Files:** No planned source changes. Evidence under ignored `target/embed-merge-verification/`.

- [ ] Active stale-reference audit:

```bash
test ! -e crates/leindex-embed
! rg -n 'leindex_embed::|leindex-embed/onnx|leindex-embed/onnx-migraphx|crates/leindex-embed' \
  Cargo.toml src tests .github install.sh install_macos.sh install.ps1 packages \
  README.md ARCHITECTURE.md docs/NEURAL_SETUP.md
```

- [ ] Format/feature boundaries:

```bash
cargo fmt --all --check
cargo check -p leindex --all-targets
cargo check -p leindex --all-targets --no-default-features --features minimal
cargo check -p leindex --all-targets --no-default-features --features onnx
cargo check -p leindex --all-targets --no-default-features --features onnx-migraphx
cargo test -p leindex --no-run --no-default-features --features onnx
```

- [ ] Worker ownership/version:

```bash
cargo build -p leindex --bin leindex-embed --no-default-features --features onnx
./target/debug/leindex-embed --version
```

Expected `leindex-embed 1.10.0`.

- [ ] Focused contracts/npm:

```bash
cargo test -p leindex --test cargo_install_layout_test
cargo test -p leindex --test release_bundle_packaging_test
cargo test -p leindex --test install_script_test
cargo test -p leindex --test cross_area_validation_test
npm --prefix packages/npm-leindex-mcp test
npm --prefix packages/npm-leindex-mcp pack --dry-run
```

- [ ] Package content:

```bash
mkdir -p target/embed-merge-verification
cargo package -p leindex --allow-dirty --list \
  | tee target/embed-merge-verification/package-files.txt
grep -q '^src/embed/mod.rs$' target/embed-merge-verification/package-files.txt
grep -q '^src/embed/worker_main.rs$' target/embed-merge-verification/package-files.txt
grep -q '^src/config.rs$' target/embed-merge-verification/package-files.txt
grep -q '^src/bin/leindex-embed.rs$' target/embed-merge-verification/package-files.txt
! grep -q '^crates/leindex-embed/' target/embed-merge-verification/package-files.txt
cargo package -p leindex --allow-dirty
```

- [ ] Real locked and unlocked packaged installs:

```bash
rm -rf target/package-install-locked target/package-install-unlocked
cargo install --path target/package/leindex-1.10.0 \
  --root target/package-install-locked --features onnx --locked
cargo install --path target/package/leindex-1.10.0 \
  --root target/package-install-unlocked --features onnx
test -x target/package-install-locked/bin/leindex
test -x target/package-install-locked/bin/leindex-embed
test -x target/package-install-unlocked/bin/leindex-embed
target/package-install-locked/bin/leindex-embed --version
```

Both must resolve registry rc.13. This is cargo-install simulation.

- [ ] Default package install excludes worker:

```bash
rm -rf target/package-install-default
cargo install --path target/package/leindex-1.10.0 \
  --root target/package-install-default --locked
test -x target/package-install-default/bin/leindex
test ! -e target/package-install-default/bin/leindex-embed
```

- [ ] Compare size/performance:

```bash
/usr/bin/time -v cargo check -p leindex --features onnx \
  2>target/embed-merge-verification/check-time.txt
cargo tree -p leindex --features onnx | wc -l \
  | tee target/embed-merge-verification/dependency-lines.txt
stat -c '%s %n' target/package/leindex-1.10.0.crate \
  | tee target/embed-merge-verification/crate-size.txt
ls -lh target/debug/leindex-embed
```

Investigate material unexpected growth. Crate source size should rise; default dependency activation must not.

- [ ] Optional provisioned runtime tests: on matching hosts run CPU plus available CUDA/MIGraphX/CoreML smoke tests. Record requested/active/fallback, startup, first inference, RSS/GPU memory. Hardware absence is not failure; false active-provider reporting is.

- [ ] Mandatory AGENTS suite exactly:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Zero warnings/errors/failures. Diagnose every discovered issue; never suppress or call pre-existing.

- [ ] Additional ONNX gates:

```bash
cargo clippy -p leindex --all-targets \
  --no-default-features --features onnx -- -D warnings
cargo test --workspace --all-features
```

- [ ] Final review:

```bash
git status --short
git diff --check
git log --oneline --decorate -12
```

No publish/push without explicit approval.

## Stop/rollback conditions

- Stop before deletion if migrated test inventory shrinks.
- Stop before version bump if unlocked packaged install resolves rc.12, git-patched ORT, or fails EP compilation.
- Stop before release if a supported target cannot compile strict ONNX.
- Stop if health can report GPU after strict registration failed.
- Roll back via reviewable task commits; never force-push or overwrite 1.9.0 artifacts.

## Final self-review checklist

- [ ] Every original phase is represented.
- [ ] Every deleted source/test file has destination or explicit reason.
- [ ] No placeholders (`TBD`, `TODO`, “handle edge cases”, “write tests”) remain.
- [ ] Current line ranges are paired with post-move symbols.
- [ ] Config fields/types stay consistent across tasks.
- [ ] One crate and two processes are never conflated.
- [ ] Performance work stays bounded: one config cache/parser, truthful EP fallback, actual-provider warmup, no version-keyed MIGraphX cache, compile-only generic CI.
- [ ] Full AGENTS validation is final gate.
- [ ] Implementation does not push, publish, or delete shared data without approval.

Plan complete. Recommended execution: subagent-driven task-by-task implementation with review after each commit; inline execution is acceptable if checkpoints remain intact.
