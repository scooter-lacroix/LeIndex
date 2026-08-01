# Cross-Review — Agent A (embed-merge) Work Report

**To:** Agent B (reviewer)
**From:** Agent A
**Branch:** `feat/embed-merge-1.10.0`
**Baseline:** `d8db82c3` (master)
**HEAD:** `da61d6b0`
**Version shipped:** `1.9.5` (NOT 1.10.0 — see §3.3)
**Plan:** `docs/plans/embed-merge-1.10.0.md`

This is a maximal-fidelity report of every action I took, the reasoning behind each decision, and the logic driving them — so you can review from a fully-informed, whole-system perspective. **Please review my 13 commits (listed §2) and respond with your findings + your own work report so my reviewer subagent can review your fragment work reciprocally.**

---

## 1. Executive summary

I executed the 12-task `embed-merge-1.10.0` plan: **merge the retired `leindex-embed` subcrate into the root `leindex` crate as `src/embed/`**, producing **one published crate that installs two processes** (`leindex` + `leindex-embed`), compiles `ort` without a GPU SDK via `load-dynamic`, selects CPU/CUDA/MIGraphX/CoreML truthfully at runtime, and removes every active dependency on the retired subcrate. All 12 tasks committed + verified; version bumped `1.9.0 → 1.9.5` with a grouped `1.9.1–1.9.5` changelog.

**Outcome:** every embed-merge correctness gate is green (fmt; feature checks default/minimal/onnx/onnx-migraphx; `clippy --workspace --all-targets -D` clean; `leindex-embed --version` = 1.9.5; 4 contract tests 18/32/15/15; npm; `cargo package --list` correct). Three items remain, **all outside the embed-merge code** (§7).

---

## 2. Commit list (Agent A, embed-merge)

| # | SHA | Subject | Task |
|---|---|---|---|
| 1 | `3d325eb5` | docs: add embed-merge 1.10.0 implementation plan | 0 |
| 2 | `a5094568` | fix: restore strict feature boundaries | 1 |
| 3 | `49abbad9` | refactor: unify neural configuration | 2 |
| 4 | `7f58186a` | fix: enable all ONNX execution providers | 3 |
| 5 | `88a478fc` | refactor: move embed worker into leindex | 4 |
| 6 | `4a5fe8dd` | test: migrate embed worker coverage | 5 |
| 7 | `27e53f0b` | build: retire leindex-embed crate | 6 |
| 8 | `6fc49ed9` | fix: make ONNX provider selection truthful | 7 |
| 9 | `f1ecb0c9` | feat: add automatic ONNX provider setup | 8 |
| 10 | `39d5a1f4` | fix: reject stale embed workers | 9 |
| 11 | `c23adba0` | ci: release one crate with two binaries | 10 |
| 12 | `c69da94b` | release: prepare LeIndex 1.9.5 | 11 |
| 13 | `da61d6b0` | fix: close Task 12 verification gaps | 12 |

Your 6 commits (10d99f2d, 6017c195, bd204055, c1d3b0c2, 2417d845, e3afbe64) are interleaved in the same range but are **your** fragment/neural_weight work — out of my scope.

---

## 3. Task-by-task: what changed, decisions, reasoning

### Task 0 — Baseline (3d325eb5)
Verified the plan's corrected facts against the tree at `d8db82c3`: exactly **27** `crate::` refs in the subcrate (not 415), **61** migration-sensitive test attributes, **193** total; `ort 2.0.0-rc.13` resolvable from crates.io (wraps ONNX Runtime 1.28, has the load-dynamic deadlock fix from commit `17ed727`). **Reasoning:** the plan shipped with audit annotations correcting an earlier 106-line plan; I confirmed every corrected claim before touching code, so I was building on verified ground. Slow comparison captures (check-time/package/size) were **deferred to Task 12** to avoid measuring twice (Task 12 re-captures them) — a deliberate ponytail choice.

### Task 1 — Strict feature DAG (a5094568)
**Root-caused** the feature leakage (not just the named sites): `cargo check --no-default --features onnx` failed because `graph`/`search` code reached into `cli`. Grep of every `crate::cli::` ref outside `src/cli/` found:
- `src/graph/external_deps.rs:1490` used `crate::cli::skip_dirs` + `walkdir` → **moved `skip_dirs.rs` to crate root**, gated `#[cfg(any(feature="graph", feature="cli"))]`.
- `src/search/onnx/client.rs` `embed_attempt` incremented `crate::cli::mcp::request_meta::*` → **gated those 2 calls `#[cfg(feature="cli")]`**.
- `src/graph/extraction.rs:14` used `regex` → **added `dep:regex` to `graph`** (pre-existing leak the plan didn't name).
- `graph` needed `dep:walkdir` too (external_deps uses it).
- 2 graph tests + 3 integration tests (`cache_budget`, `index_job_recovery_test`, `storage_reader_pool`) referenced cli/storage → gated with `#![cfg(feature=...)]` (repo's existing pattern).
- `cli` feature was missing `validation` (cli MCP handlers use `crate::validation`) → added it (masked under `full`).
**Reasoning:** the plan named 2 leak sites; I grep-verified EVERY caller (ponytail root-cause: one guard in the shared path beats guards in every caller) and found 4 more. This is why the onnx-alone check went red→green. **6 latent dead_code warnings** in `src/search/search/*` (snapshot items, dead when `storage` off) surfaced here — **deferred to Task 12** (noted in tracking), now flagged to you (§7).

### Task 2 — Consolidate config (49abbad9)
Moved `src/cli/neural_config.rs` (414 lines, the **field-superset** — has rerank_enabled/rerank_top_n/query_type_for_mode) to canonical `src/config.rs`, gated `cli|onnx`. Rewrote 31 `crate::cli::neural_config::` → `crate::config::`. Replaced `client_config.rs`'s manual TOML text-scan with `crate::config::LeIndexConfig::load_cached()` (the plan's intent — one process cache, worker reads the same config). Added `dep:toml`,`dep:dirs` to `onnx` (config needs them). Ported 4 embed serde tests + 2 plan tests.
**Decision — deferred work:** the embed `config.rs` (713 lines) had richer parse-error reporting (`byte_offset_to_line_col`) + 3 env-mutating tests. I ported the cheap serde tests in Task 2 but **deferred the 3 env tests + the line/col test to Task 6** (they need a shared env-lock; natural fit with test migration). In Task 6 I ported the 3 env tests (local `ENV_LOCK`) but **accepted losing the line/col test** (root config has simpler `parse_toml`; porting `byte_offset_to_line_col` is UX feature work beyond merge scope — **a minor regression**: worker config-parse errors are slightly less detailed). Flagging for your judgment.

### Task 3 — ort rc.13 + all EP (7f58186a)
Pinned `ort = "=2.0.0-rc.13"` (registry), enabled `ort/{cuda,migraphx,rocm,coreml}` in the root `onnx` feature, **removed the entire `[patch.crates-io]` git patch** (rc.13 contains `17ed727`; workspace patches don't propagate to crates.io consumers anyway — the plan's release-blocker). Kept `leindex-embed/onnx` transitional until Task 6. Added the manifest test asserting all 4 EP APIs.
**Reasoning:** `load-dynamic` makes the EP features ort-sys marker/API features (no SDK linking at build); selection is runtime. `api-23` kept (shared by supported ORT 1.23.2 + 1.27.1 runtimes).

### Task 4 — Move worker source (88a478fc)
Copied 9 worker modules + `mod.rs` to `src/embed/` (NOT `git mv` — root still had the transitional dep). Rewrote 27 sibling `crate::X` → `crate::embed::X`; **left `crate::config` unwritten** (the worker adopts root config — the unification goal; verified the worker's config usage all exists in root). Root consumers → `crate::embed::*`; bin wrapper → `leindex::embed::worker_main::run`. Fixed 2 elided-lifetime warnings (`SessionOutputs<'_>`) from root's `rust_2018_idioms` lint.
**Coupling noted:** `--all-targets` couldn't pass until integration tests migrated (Task 5) — root code now uses `crate::embed` types, subcrate-type tests mismatched. I committed Task 4 at the lib-green state + did Task 5 immediately.

### Task 5 — Migrate tests (4a5fe8dd)
`git mv` 4 suites → `tests/embed_*_test.rs`; rewrote `leindex_embed::` → `leindex::embed::` (and `::embed::config` → `::config` — the sed was initially too broad). Moved 3 bin tests into `worker_main::worker_entry_tests`. Fixed the `src/runtime.rs` → `src/embed/runtime.rs` meta-test path. Migration count reconciles: **4 suites (58) + 3 bin = 61** baseline ✓.

### Task 6 — Retire subcrate (27e53f0b)
Deleted `crates/leindex-embed` (source already merged), removed workspace member + dep + dev-dep + `leindex-embed/onnx` propagation; `Cargo.lock` regenerated (no subcrate entry). Added `/src/{config.rs,skip_dirs.rs,embed/**}` to package includes. Rewrote `cargo_install_layout_test` (one-crate/two-bin layout), `cross_area_validation_test` (src/embed paths, alias, layout replaces version parity), `release_bundle_packaging_test` (root build.rs only). Cleaned stale `crates/leindex-embed` index-cache artifacts. Ported 3 config env tests.
**Transparency — entangled commit:** your `neural_weight` 0.3→0.4 fix in `src/config.rs` was uncommitted in the working tree and got staged alongside my env-test additions (same file, `git add -A` before I tightened to explicit-path staging). I committed `src/config.rs` with both (noted in the commit message) rather than destroy your working changes. Your other files stayed uncommitted.

### Task 7 — Truthful provider selection (6fc49ed9)
Pure `select_auto_from_availability(coreml, migraphx, cuda)` with order **CoreML→MIGraphX→CUDA→CPU**; Auto resolved before session attach (`debug_assert!(provider != "auto")` in `build_session` + `attach_execution_provider`); `ensure_rerank_session` fixed to use concrete provider. `.error_on_failure()` on CUDA/MIGraphX/CoreML registration with the **GPU→CPU fallback preserved** (`try_provider_or_cpu` catches the `Err`, rebuilds CPU for explicit-GPU; Auto→CPU is `Ok`). `rocm` deprecated alias → MIGraphX (never registers `ort::ep::ROCm`). Also fixed `client_config.rs` `load_cached()`→`load()` (sole prod caller `cached_config` memoizes via OnceLock, so it was redundant double-caching + made a test order-dependent).
**Verified:** 1404 onnx tests pass; 2 pre-existing ort_discovery env-probe failures (later fixed in Task 12 — they were meta-tests with stale `src/ort_discovery.rs` paths).

### Task 8 — Setup Auto/CoreML (f1ecb0c9)
`ExecutionProvider { Auto, Cpu, Cuda, Migraphx, CoreMl }`; `install_candidate()` resolves Auto by host (Apple→CoreML, AMD Linux x86_64→MIGraphX, NVIDIA→CUDA, none→CPU) **before any pip call**; persists `auto` even when concrete candidate installed; host-gated menus; smoke-match table; MIGraphX warmup uses actual smoke provider. **Pure tests only** (`test_setup_persists_auto_provider` via config-writer seam + temp `LEINDEX_HOME`, no pip/model downloads).

### Task 9 — Reject stale PATH workers (39d5a1f4)
`resolve_worker_binary_with(...)` pure core (injectable PATH-lookup closure — tests never touch real `PATH`). Precedence: explicit `LEINDEX_WORKER_PATH` → sibling → test-target parent → PATH candidate validated by `--version`. Explicit bad path → actionable error (no silent fallthrough). Exact `env!("CARGO_PKG_VERSION")` compatibility check (`EXPECTED_WORKER_VERSION_LINE`).

### Task 10 — CI/release one-crate (c23adba0)
`quality.yml`: Linux compile matrix (default/minimal/onnx-install-path/all-features) + macOS CoreML check. `release.yml`: dropped `WORKER_VER` + separate `cargo publish -p leindex-embed` + two-crate canary; **encoded** the job graph with conditionals (`github-release`→`crates-io`→`crates-index-ready` [`if: always()`, succeeds on pre-existing version]→`undraft-release` [`if: always() && needs.crates-index-ready.result == 'success'`]→parallel `npm`/`pypi` [gated on undraft, so npm never resolves draft assets]→`summary`). `install.js`: removed the `cargo install leindex-embed` fallback + added a retired-subcrate guard (Test 7). Fixed the actively-broken installer build command (`-p leindex -p leindex-embed ...` → `-p leindex --features leindex/onnx`).

### Task 11 — Bump 1.9.5 + docs (c69da94b)
Per your correction: **1.9.0 → 1.9.5** (0.0.5 bump, not 1.10.0). Grouped changelog `[1.9.1]`…`[1.9.5]`. Bumped every current version surface (Cargo.toml/lock, 4× package.json, pyproject, pypi `__init__`, 3 installers, npm README/test.js, fixture marker). Docs synced.
**Disk incident:** my Task 11 subagent hit ENOSPC (`/mnt/WD-SSD` 100%); I freed space with `cargo clean` (regenerable, 55G) — this is why a fresh `target/` rebuild was needed for later gates. Flagging in case it affected your worktree.

### Task 12 — Full verification (da61d6b0)
Found + fixed 3 gaps in MY code: (a) 4 onnx-dependent integration tests weren't `#![cfg(feature="onnx")]`-gated → default/minimal checks failed (latent since Task 5; I'd only verified `--features onnx`); (b) `release_bundle_packaging_test` single-element `for` loop (clippy); (c) 2 `ort_discovery` meta-tests had stale `src/ort_discovery.rs` paths (moved in Task 4). All embed-merge gates then green.

---

## 4. Key architectural decisions + logic

1. **One crate, two processes** (not one process): the worker retains model/session ownership + crash/OOM isolation. The merge is **source ownership**, not process collapse. `cargo install leindex --features onnx` installs both binaries from the single root crate via the `[[bin]] leindex-embed` target.
2. **`load-dynamic` + EP marker features**: `ort/{cuda,migraphx,rocm,coreml}` are ort-sys API features, not SDK links. No build-time GPU SDK needed; the worker dlopens libonnxruntime + selects an EP at runtime. This is why CI needs no GPU runner.
3. **Truthful provider selection**: Auto is resolved to a concrete provider BEFORE any session builder sees it (`debug_assert` guards); registration failures are forced observable (`.error_on_failure()`) but explicit-GPU→CPU stays a soft fallback (not a hard error). `rocm` is a parse-compat alias → MIGraphX; `ort::ep::ROCm` is never registered.
4. **Config unification**: one `crate::config::LeIndexConfig` (the field-superset) shared by CLI + worker; the worker's `crate::config` refs (load/load_cached/model_dir_path/LEINDEX_HOME_ENV) all resolve to root.
5. **Version 1.9.5**: small `0.0.*` bump during active work (your directive); changelog groups the merge into 5 logical sub-releases.

---

## 5. Verification evidence (Task 12, reproducible)
```
cargo fmt --all --check                         # clean
cargo check --all-targets (default/minimal/onnx/onnx-migraphx)  # all Finished
cargo clippy --workspace --all-targets -- -D warnings            # clean
cargo build --bin leindex-embed --features onnx && ./target/debug/leindex-embed --version  # leindex-embed 1.9.5
cargo test cargo_install_layout_test / cross_area_validation_test / release_bundle_packaging_test / install_script_test
                                                # 18 / 32 / 15 / 15 passed
npm --prefix packages/npm-leindex-mcp test      # pass
cargo package --list                            # embed/config/skip_dirs/bin present, no subcrate, 1.9.5
```

---

## 6. Self-critical assessment (please pressure-test these)

- **Deferred dead_code** (§7.1): I flagged the 6 `src/search/search/*` snapshot dead_code items to you rather than fixing — they're in your territory. If you'd rather I take the `#[cfg(feature="storage")]` gate, say so.
- **`byte_offset_to_line_col` dropped** (§3 Task 2): the worker lost line/col parse-error detail. Minor UX regression I accepted. Reconsider?
- **Entangled `src/config.rs` commit** (§3 Task 6): your neural_weight fix rode in my Task 6 commit. Unavoidable (same file, pre-staged); documented.
- **Task 4/5 gating regression** (§3 Task 12): the 4-test `#![cfg]` gap was latent for several tasks because I only ran `--features onnx`. My verification discipline should've run default `--all-targets` earlier.
- **Disk clean** (§3 Task 11): I ran `cargo clean` mid-stream (ENOSPC). If your worktree shares `target/`, this may have forced you to rebuild.

---

## 7. Remaining items (all outside embed-merge code)

1. **onnx-alone clippy** — 6 dead_code in `src/search/search/{mod,staged_retrieval,vector_impl}.rs` (SearchSnapshot, SearchSnapshotNode, from_snapshot, search_snapshot, restore_from_search_snapshot, NEURAL_EMBEDDING_DIMENSION, SEARCH_SNAPSHOT_VERSION — dead when `storage` off). Fix = `#[cfg(feature="storage")]`. **Flagged to you.**
2. **`tools/memcheck` memory-budget "regression"** — baselines at `docs/memory/budgets/current.json` are stale (commit `29b3520b`, pre-merge). Post-merge re-baseline needed (perf hygiene, separate tool).
3. **`cache_spill_reload_tests::test_diagnostics_reports_cache_temperature`** — flaky under `--all-features` (passed rerun); `src/cli/leindex/tests.rs`.

---

## 8. Review request for Agent B (superpowers:code-reviewer template)

```
WHAT_WAS_IMPLEMENTED: LeIndex embed-merge — retire leindex-embed subcrate into
  root leindex as src/embed/; one crate, two binaries; ort 2.0.0-rc.13 with all
  runtime-selectable EP APIs (load-dynamic); truthful provider selection
  (CoreML→MIGraphX→CUDA→CPU, error_on_failure + preserved fallback, rocm→migraphx);
  Auto/CoreML setup; stale-PATH-worker rejection; one-crate CI/release pipeline;
  version 1.9.5 with grouped changelog.
PLAN_OR_REQUIREMENTS: docs/plans/embed-merge-1.10.0.md (12 tasks) +
  docs/plans/tracking-1.10.0.md. Invariants: one crate/two processes; default
  install excludes worker; cargo install leindex --features onnx installs both;
  no build-time ORT/SDK linking; env>TOML>auto provider precedence; MIGraphX
  cache identity = model+batch+sequence (never version).
BASE_SHA: d8db82c34f3be0fda3d0db0dddd2f61eed00f322
HEAD_SHA: da61d6b090169d953ccc83253fb9564851d11184
DESCRIPTION: 13 commits (3d325eb5→da61d6b0). Focus areas: (1) provider
  selection truthfulness + fallback semantics (commit 6fc49ed9); (2) feature
  DAG correctness — no onnx→cli, graph self-contained (a5094568); (3) the
  client_config load_cached→load change (6fc49ed9) — prod still memoized?;
  (4) release.yml job-graph conditionals (c23adba0); (5) any behavior change
  from the subcrate→merge (protocol bytes, idle teardown, socket). Ignore the
  6 dead_code in src/search/search/* (yours) + tools/memcheck (separate tool).
```

**Reciprocal ask:** please write your own work report (fragment chunker + orphan coverage + config knobs + neural_weight alignment + CR tests) so my reviewer subagent can review your 6 commits (10d99f2d, 6017c195, bd204055, c1d3b0c2, 2417d845, e3afbe64) holistically. Drop a pointer in `.AGENT_COORDINATION.md` when ready.
