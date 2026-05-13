# Plan 3 - C-Phase ONNX Worker & R15 Bundling Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move ONNX inference out of the main daemon into a separate `leindex-embed` worker, keep the main process under the C-phase memory target, and ship the worker, models, and install-time bundle changes across Rust, npm, PyPI, and the public docs set.

**Architecture:** The main `leindex` binary becomes ONNX-light and talks to a separate `crates/leindex-embed/` worker over length-prefixed `postcard` frames on UDS / named pipe transport. The worker owns `ort`, `tokenizers`, execution-provider selection, model mmap, and idle shutdown. Release artifacts ship `leindex`, `leindex-embed`, and model files side by side. npm and PyPI package the same layout under `<pkg>/bin/` and `<pkg>/models/`. Public docs and installer scripts all describe the same bundle shape. Plan 0's memcheck harness is extended to measure the worker-active path, and the targeted-B residency work is assumed to already be in place.

**Tech Stack:** Rust 2021, `serde`, `postcard`, `ort`, `tokenizers`, `clap`, platform IPC (`std`, `libc`, `windows-sys`), shell installers, Node.js, Python packaging, GitHub Actions, `cargo`, `sha256sum`, `zip` / `tar`.

**Spec source:** `docs/superpowers/specs/2026-05-13-leindex-memory-reduction-design.md` §4.1-4.4 and §7.1-7.9, with the release / packaging surfaces tied back to §7.4, §7.7, and §7.8.

**Local exit criteria:**
- `cargo build --workspace --release` builds both the main daemon and `leindex-embed` cleanly.
- `cargo tree -p leindex --features onnx | rg 'ort|tokenizers'` prints nothing; the worker crate owns those deps.
- `cargo test -p leindex-embed` passes the protocol and transport smoke tests.
- `cargo test --features onnx --test onnx_worker_fallback` proves worker crash recovery falls back to TF-IDF for the affected batch.
- `cargo xtask memcheck` reports the C-phase fixture under the main / combined RSS targets from the spec, with worker-active samples captured in the report.
- The release workflow emits platform bundles that contain `leindex`, `leindex-embed`, and `models/` assets with SHA256 coverage.
- `npm test` and `python -m pytest packages/pypi-leindex/tests/test_bootstrap.py` pass after installer and bundle changes.
- `README.md`, `packages/npm-leindex-mcp/README.md`, `packages/pypi-leindex/README.md`, `docs/R15_MODEL_DISTRIBUTION.md`, and `docs/R15_IMPLEMENTATION_SUMMARY.md` all describe the same worker + bundle topology.

---

## File Structure

**Create:**
- `crates/leindex-embed/Cargo.toml` - separate worker crate, no in-tree `[[bin]]`
- `crates/leindex-embed/src/lib.rs` - worker library surface for protocol/runtime modules
- `crates/leindex-embed/src/main.rs` - worker entrypoint and CLI
- `crates/leindex-embed/src/protocol.rs` - postcard frame schema shared by worker tests
- `crates/leindex-embed/src/runtime.rs` - model load, EP selection, idle shutdown, health pings
- `crates/leindex-embed/src/transport.rs` - UDS / named pipe framing and request routing
- `crates/leindex-embed/tests/protocol_roundtrip.rs` - wire-schema and frame smoke test
- `crates/leindex-embed/tests/worker_smoke.rs` - spawn, request, and shutdown smoke test
- `src/search/onnx/client.rs` - main-process worker client and batch stitching
- `src/search/onnx/protocol.rs` - mirrored request / response types for the main crate
- `src/search/onnx/worker.rs` - worker lifecycle, socket discovery, and retry policy
- `tests/search/onnx_worker_fallback.rs` - end-to-end fallback test
- `scripts/download-models.sh` - release / CI model fetch and verification helper
- `scripts/convert-to-onnx.sh` - model conversion helper
- `scripts/quantize-onnx.sh` - int8 / bundle-size helper for the worker model set

**Modify:**
- `Cargo.toml` - move ONNX runtime deps off the main crate, add the new workspace member, add worker-facing IPC deps where needed
- `build.rs` - stop making the main crate depend on bundled ONNX files; keep verification in the worker / bundle path instead
- `src/search/onnx/mod.rs` - re-export worker-backed providers and protocol/client modules
- `src/search/onnx/qwen.rs` - delegate embedding calls to the worker client
- `src/search/onnx/reranker.rs` - delegate reranker calls to the worker client
- `src/cli/index_builder.rs` - route batch embedding and chunking into the worker-backed path
- `tools/memcheck/src/driver.rs` - sample worker-active runs and record combined / per-process RSS
- `tools/memcheck/src/report.rs` - add worker-aware fields to the per-phase JSON
- `tools/memcheck/workloads/small_repo.toml` - add a dedicated C-phase workload profile for worker exercise
- `docs/memory/budgets/current.json` - add C-phase ceilings for main and combined RSS, if the worker path changes the budget table
- `.github/workflows/release.yml` - build, bundle, checksum, and publish `leindex` + `leindex-embed` + `models`
- `install.sh` - install both executables and the bundled models
- `install_macos.sh` - install both executables and the bundled models on macOS
- `packages/npm-leindex-mcp/install.js` - download and verify both binaries plus models
- `packages/npm-leindex-mcp/package.json` - include worker binary and model assets in published files
- `packages/npm-leindex-mcp/README.md` - document the sidecar worker bundle and keep MCP examples aligned
- `packages/pypi-leindex/pyproject.toml` - include `bin/` and `models/` package data
- `packages/pypi-leindex/MANIFEST.in` - ship worker / model assets in the wheel and sdist
- `packages/pypi-leindex/src/leindex/bootstrap.py` - locate and launch the worker binary alongside the main binary
- `packages/pypi-leindex/README.md` - document the worker / model bundle and keep config examples aligned
- `README.md` - update the top-level install and embedding sections
- `models/README.md` - replace the manual-download story with the bundled-worker story
- `docs/R15_MODEL_DISTRIBUTION.md` - supersede the old single-binary model layout
- `docs/R15_IMPLEMENTATION_SUMMARY.md` - replace the old "in-process ONNX" summary with the worker topology
- `.github/pull_request_template.md` - note the bundle / baseline expectation if the release surface changes

**Test:**
- `crates/leindex-embed/tests/protocol_roundtrip.rs`
- `crates/leindex-embed/tests/worker_smoke.rs`
- `tests/search/onnx_worker_fallback.rs`
- `packages/npm-leindex-mcp/test.js`
- `packages/pypi-leindex/tests/test_bootstrap.py`
- `tools/memcheck/src/driver.rs` and the existing memcheck report / baseline outputs

---

## R15 Scope Map

| Spec slice | Implementation shape | Primary files |
|---|---|---|
| §7.1-7.3 | Separate worker process, postcard IPC, lifecycle, backpressure, crash fallback | `crates/leindex-embed/*`, `src/search/onnx/client.rs`, `src/search/onnx/worker.rs` |
| §7.4-7.6 | Bundled models, sidecar worker packaging, ORT execution-provider selection | `crates/leindex-embed/src/runtime.rs`, `scripts/*`, `.github/workflows/release.yml`, installer / package files |
| §7.7-7.8 | Rollout sequencing and doc cleanup | `docs/R15_MODEL_DISTRIBUTION.md`, `docs/R15_IMPLEMENTATION_SUMMARY.md`, `README.md`, package READMEs |
| §4.1-4.4 | RSS accounting and budget gates for the C-phase path | `tools/memcheck/*`, `docs/memory/budgets/current.json`, `docs/memory/baselines/*` |

---

## Risks and Mitigations

- Wire-schema drift between main and worker: use a versioned frame header plus roundtrip tests in both crates.
- Worker memory still stays resident after unload: keep the worker out-of-process, add idle teardown, and measure the embed path with memcheck.
- Bundle size grows too fast: ship the INT8 / smaller-model bundle only, keep reranker optional until quality data justifies it, and check archive contents in CI.
- Platform-specific installer bugs: verify the release matrix on Linux, macOS, and Windows with package-content smoke tests.
- Docs diverge from the actual bundle layout: update the root README, both package READMEs, and the R15 docs in the same pass.

---

## Task 1: Carve out the worker crate and the shared wire contract

**Files:**
- Create: `crates/leindex-embed/Cargo.toml`
- Create: `crates/leindex-embed/src/lib.rs`
- Create: `crates/leindex-embed/src/main.rs`
- Create: `crates/leindex-embed/src/protocol.rs`
- Create: `crates/leindex-embed/tests/protocol_roundtrip.rs`
- Modify: `Cargo.toml`
- Modify: `build.rs`
- Modify: `src/search/onnx/mod.rs`

- [ ] **Step 1: Confirm the main crate still owns ONNX deps before the split**

Run:
```bash
grep -n "onnx =" Cargo.toml
cargo tree -p leindex --features onnx | rg 'ort|tokenizers'
```

Expected: the current tree shows the ONNX deps in the main crate. That is the baseline to remove in this task.

- [ ] **Step 2: Add the worker workspace member and prune the main crate's ONNX baggage**

Update `Cargo.toml` so:
- `crates/leindex-embed` is a workspace member.
- the main crate's `onnx` feature no longer pulls `dep:ort` or `dep:tokenizers`.
- the main crate gains only the IPC / bundle-path dependencies it needs.

Expected shape after the edit:
```bash
cargo tree -p leindex --features onnx | rg 'ort|tokenizers'
cargo tree -p leindex-embed | rg 'ort|tokenizers'
```

Expected: first command prints nothing; second command shows the worker owns those deps.

- [ ] **Step 3: Add the worker crate skeleton**

Create `crates/leindex-embed/Cargo.toml` with:
- package name `leindex-embed`
- `publish = false`
- `serde`, `serde_json`, `postcard`, `clap`, `anyhow`, `tracing`
- `ort` and `tokenizers`
- platform IPC crates only where needed for the transport

Create `crates/leindex-embed/src/lib.rs` that exposes the protocol, runtime, and transport modules.

Create `crates/leindex-embed/src/main.rs` with CLI flags for:
- socket path / named pipe selection
- model directory
- execution provider selection
- idle timeout
- startup-report output

- [ ] **Step 4: Add a protocol roundtrip test**

Create `crates/leindex-embed/tests/protocol_roundtrip.rs` that:
- serializes `EmbedRequest`
- deserializes the frame back
- checks `batch_id`, `texts`, and the response variant for the same payload

Run:
```bash
cargo test -p leindex-embed --test protocol_roundtrip
```

Expected: `running 1 test` followed by `ok`.

- [ ] **Step 5: Move build-time model verification out of the main crate**

Modify `build.rs` so the main crate no longer panics just because ONNX model files are missing from `models/`.
Keep the verification in the worker build or release bundle path instead, where the model assets actually matter.

Run:
```bash
cargo build --workspace --release
```

Expected: the workspace builds without the main crate demanding local model files.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml build.rs src/search/onnx/ crates/leindex-embed/
git commit -m "feat(onnx-worker): scaffold worker crate and wire contract"
```

---

## Task 2: Implement the worker runtime, transport, and lifecycle

**Files:**
- Create: `crates/leindex-embed/src/runtime.rs`
- Create: `crates/leindex-embed/src/transport.rs`
- Create: `crates/leindex-embed/tests/worker_smoke.rs`
- Modify: `crates/leindex-embed/src/main.rs`

- [ ] **Step 1: Implement the worker process lifecycle**

`runtime.rs` should own:
- model mmap and ORT session setup
- tokenizer load
- EP selection (`auto`, `cpu`, `cuda`)
- startup JSON line reporting
- idle teardown after `LEINDEX_EMBED_IDLE_SECS`
- active-session health pings

The worker should print a single startup line that includes:
- chosen EP
- fallback reason, if any
- model name
- quantization mode
- warm-load latency

- [ ] **Step 2: Implement postcard framing over UDS / named pipe**

`transport.rs` should:
- use length-prefixed `postcard` frames
- support Unix Domain Socket on Linux / macOS
- support named pipe on Windows
- reject TCP
- keep the batch id on every request and response frame

The main-side batch buffer must remain under the spec's 1 MiB limit, and oversized requests should be split before they hit transport.

- [ ] **Step 3: Add a worker smoke test**

Create `crates/leindex-embed/tests/worker_smoke.rs` that:
- launches the worker with a temp socket path
- sends one embedding request
- receives a non-empty vector payload
- confirms the worker exits cleanly after idle timeout

Run:
```bash
cargo test -p leindex-embed
```

Expected: all worker tests PASS on the host platform.

- [ ] **Step 4: Verify the worker binary builds in isolation**

Run:
```bash
cargo build -p leindex-embed --release
```

Expected: `Compiling leindex-embed ... Finished` with no dependency spillover from the main crate.

- [ ] **Step 5: Commit**

```bash
git add crates/leindex-embed/
git commit -m "feat(onnx-worker): add runtime, transport, and smoke tests"
```

---

## Task 3: Route the main daemon through the worker and keep the fallback path alive

**Files:**
- Create: `src/search/onnx/client.rs`
- Create: `src/search/onnx/protocol.rs`
- Create: `src/search/onnx/worker.rs`
- Modify: `src/search/onnx/mod.rs`
- Modify: `src/search/onnx/qwen.rs`
- Modify: `src/search/onnx/reranker.rs`
- Modify: `src/cli/index_builder.rs`
- Create: `tests/search/onnx_worker_fallback.rs`

- [ ] **Step 1: Add the main-side worker client**

`client.rs` should:
- resolve the worker binary relative to the main binary and the bundle layout
- spawn the worker on first embed request
- reuse the same worker for later requests
- split oversized batches and stitch responses back together by `batch_id`
- stream the response directly into the destination embedding store

No `Vec<Vec<f32>>` intermediate should appear in the main path.

- [ ] **Step 2: Replace in-process provider calls**

`qwen.rs` and `reranker.rs` should delegate to the worker client instead of opening ONNX sessions in-process.

`mod.rs` should re-export the worker-backed types so the rest of the crate sees the same public surface.

- [ ] **Step 3: Keep TF-IDF fallback for worker failure**

The failure policy should be:
- retry once on worker crash
- on the second failure, fall back to TF-IDF for the affected batch
- log a warning that names the batch id and the failed worker path

This is the user-facing safety net for the C-phase rollout.

- [ ] **Step 4: Add a failure-mode integration test**

Create `tests/search/onnx_worker_fallback.rs` that:
- starts with a healthy worker
- forces a worker exit between batches
- asserts the main daemon still completes the request
- checks that the fallback path is used for the failed batch

Run:
```bash
cargo test --features onnx --test onnx_worker_fallback
```

Expected: the test passes and the logs include the fallback warning.

- [ ] **Step 5: Commit**

```bash
git add src/search/onnx/ src/cli/index_builder.rs tests/search/onnx_worker_fallback.rs
git commit -m "feat(onnx-worker): route embeddings through worker client"
```

---

## Task 4: Finalize model conversion, quantization, and bundle layout

**Files:**
- Create: `scripts/download-models.sh`
- Create: `scripts/convert-to-onnx.sh`
- Create: `scripts/quantize-onnx.sh`
- Modify: `models/README.md`
- Modify: `docs/R15_MODEL_DISTRIBUTION.md`
- Modify: `docs/R15_IMPLEMENTATION_SUMMARY.md`
- Modify: `crates/leindex-embed/src/runtime.rs`

- [ ] **Step 1: Define the bundled model set**

Lock the shipped model set for the C-phase bundle:
- primary embedding model
- optional reranker only if the quality gate justifies it
- tokenizer and license files

Keep the final bundle choice aligned with the spec's size levers and quality gate rather than hard-coding a new default too early.

- [ ] **Step 2: Add the conversion and packaging helpers**

The scripts should:
- download source model files from the documented upstream URLs
- convert them to ONNX
- quantize the bundle where applicable
- emit deterministic output paths under `models/`
- fail if the expected files are missing or the bundle size drifts beyond the agreed target

- [ ] **Step 3: Update the runtime model discovery**

`runtime.rs` should resolve models in this order:
- explicit environment override
- bundled `models/` next to the binary
- user cache fallback

The worker startup line should report which path won.

- [ ] **Step 4: Verify bundle contents**

Run:
```bash
bash scripts/download-models.sh
bash scripts/convert-to-onnx.sh
bash scripts/quantize-onnx.sh
du -sh models/*
sha256sum models/*
```

Expected:
- the bundle directory contains the worker-ready ONNX files
- the checksum output is stable for the committed assets
- the bundle size is consistent with the chosen model strategy

- [ ] **Step 5: Commit**

```bash
git add scripts/ models/ docs/R15_MODEL_DISTRIBUTION.md docs/R15_IMPLEMENTATION_SUMMARY.md crates/leindex-embed/src/runtime.rs
git commit -m "build(r15): add worker model bundle pipeline"
```

---

## Task 5: Update the release workflow and installers to ship the worker sidecar

**Files:**
- Modify: `.github/workflows/release.yml`
- Modify: `install.sh`
- Modify: `install_macos.sh`
- Modify: `packages/npm-leindex-mcp/install.js`
- Modify: `packages/npm-leindex-mcp/package.json`
- Modify: `packages/pypi-leindex/pyproject.toml`
- Modify: `packages/pypi-leindex/MANIFEST.in`
- Modify: `packages/pypi-leindex/src/leindex/bootstrap.py`

- [ ] **Step 1: Expand the release matrix to build both binaries**

`release.yml` should:
- build `leindex` and `leindex-embed` for each release target
- assemble platform bundles that include both executables and the model directory
- compute SHA256 checksums for every shipped artifact
- keep the existing release gating and version-detection behavior

- [ ] **Step 2: Teach the shell installers to install the sidecar**

`install.sh` and `install_macos.sh` should:
- install or refresh both `leindex` and `leindex-embed`
- place the models where the worker expects them
- preserve the existing user-facing prompts and path handling

- [ ] **Step 3: Teach npm to download the whole bundle**

`packages/npm-leindex-mcp/install.js` and `package.json` should:
- resolve both binaries from the release assets
- verify both checksums
- include `bin/leindex-embed` and the model assets in the published files list
- keep the MCP wrapper behavior unchanged for callers

- [ ] **Step 4: Teach PyPI to ship the same layout**

`packages/pypi-leindex/pyproject.toml`, `MANIFEST.in`, and `bootstrap.py` should:
- package `src/leindex/bin/` and `src/leindex/models/`
- launch the worker binary from the installed package layout
- keep the bootstrapper compatible with the existing Cargo-based install flow for the main binary

- [ ] **Step 5: Verify package contents**

Run:
```bash
node --check packages/npm-leindex-mcp/install.js
python -m py_compile packages/pypi-leindex/src/leindex/bootstrap.py
npm test --prefix packages/npm-leindex-mcp
python -m pytest packages/pypi-leindex/tests/test_bootstrap.py
python -m build packages/pypi-leindex
```

Expected:
- the npm test suite passes
- the PyPI bootstrap tests pass
- the built wheel and sdist contain the worker binary and `models/` contents

- [ ] **Step 6: Commit**

```bash
git add .github/workflows/release.yml install.sh install_macos.sh packages/npm-leindex-mcp/ packages/pypi-leindex/
git commit -m "ci(release): ship leindex-embed and model bundles"
```

---

## Task 6: Align the public docs and retire the old in-process R15 story

**Files:**
- Modify: `README.md`
- Modify: `packages/npm-leindex-mcp/README.md`
- Modify: `packages/pypi-leindex/README.md`
- Modify: `models/README.md`
- Modify: `docs/R15_MODEL_DISTRIBUTION.md`
- Modify: `docs/R15_IMPLEMENTATION_SUMMARY.md`
- Modify: `.github/pull_request_template.md`

- [ ] **Step 1: Update the top-level embedding section**

`README.md` should say:
- local ONNX now runs through the worker sidecar
- the bundle contains `leindex` and `leindex-embed`
- the existing MCP config examples stay valid
- the install sections still point to the same public entrypoints

- [ ] **Step 2: Keep the package READMEs aligned**

`packages/npm-leindex-mcp/README.md` and `packages/pypi-leindex/README.md` should:
- use the same bundle wording as the root README where the public install story overlaps
- keep the MCP config examples identical
- mention the sidecar worker only once in each doc, not as two competing stories

- [ ] **Step 3: Rewrite the R15 docs around the worker topology**

`docs/R15_MODEL_DISTRIBUTION.md` and `docs/R15_IMPLEMENTATION_SUMMARY.md` should:
- stop describing the old in-process ONNX model load as the final state
- explain the worker binary, bundled model paths, and release-time layout
- call out the remaining work only where it is still real

- [ ] **Step 4: Tighten the PR template note**

Update `.github/pull_request_template.md` so changes to:
- worker bundle layout
- model assets
- memory budgets
- installer scripts
trigger a visible baseline / justification note in the PR body.

- [ ] **Step 5: Verify doc parity**

Run:
```bash
rg -n "mcpServers|leindex-embed|models/" README.md packages/npm-leindex-mcp/README.md packages/pypi-leindex/README.md docs/R15_MODEL_DISTRIBUTION.md docs/R15_IMPLEMENTATION_SUMMARY.md models/README.md
```

Expected:
- all public MCP snippets still match
- the worker bundle is described consistently
- there is no stale "single binary owns ONNX" wording left in the public docs

- [ ] **Step 6: Commit**

```bash
git add README.md packages/npm-leindex-mcp/README.md packages/pypi-leindex/README.md models/README.md docs/R15_MODEL_DISTRIBUTION.md docs/R15_IMPLEMENTATION_SUMMARY.md .github/pull_request_template.md
git commit -m "docs(r15): align worker bundle and public install surfaces"
```

---

## Task 7: Extend memcheck and the C-phase acceptance gate

**Files:**
- Modify: `tools/memcheck/src/driver.rs`
- Modify: `tools/memcheck/src/report.rs`
- Modify: `tools/memcheck/workloads/small_repo.toml`
- Modify: `docs/memory/budgets/current.json`
- Modify: `docs/memory/baselines/small_repo/*.json`

- [ ] **Step 1: Teach memcheck about the worker-active path**

The memcheck driver should:
- capture the worker PID once the embed request starts
- record main RSS and worker RSS separately
- record a combined peak for the embed phase
- keep the existing per-phase JSON shape stable enough for the baseline files to remain human-readable

- [ ] **Step 2: Add a C-phase workload profile**

Update `tools/memcheck/workloads/small_repo.toml` so the C-phase fixture includes:
- an idle phase before the first embed request
- a phase that exercises the worker-backed embedding path
- a query or reindex phase that confirms the worker teardown / restart cycle

- [ ] **Step 3: Refresh the budget file and baselines only when the measurements justify it**

`docs/memory/budgets/current.json` should carry the C-phase ceiling for:
- main daemon idle RSS
- combined embed RSS while the worker is active

If the measured worker bundle changes the targets, update the budget and baseline files in the same change set with a clear justification.

- [ ] **Step 4: Run the regression gate**

Run:
```bash
cargo xtask memcheck
```

Expected:
- the worker-active phase prints an `[OK]`
- the report shows main vs worker RSS clearly enough to diagnose a regression
- a forced baseline regression fails with a `combined` or `main` mismatch, not a vague pass/fail

- [ ] **Step 5: Commit**

```bash
git add tools/memcheck/ docs/memory/budgets/ docs/memory/baselines/
git commit -m "test(r15): add worker-aware memory gate"
```

---

## Task 8: End-to-end verification and cleanup

- [ ] **Step 1: Build the whole workspace**

Run:
```bash
cargo build --workspace --release
```

Expected: both binaries compile, the worker crate does not leak ONNX deps into the main tree, and the release build is clean.

- [ ] **Step 2: Smoke the installer surfaces**

Run:
```bash
bash -n install.sh
bash -n install_macos.sh
node --check packages/npm-leindex-mcp/install.js
python -m py_compile packages/pypi-leindex/src/leindex/bootstrap.py
```

Expected: the scripts parse cleanly.

- [ ] **Step 3: Verify the published artifacts contain the worker sidecar**

Run:
```bash
npm pack --prefix packages/npm-leindex-mcp
python -m build packages/pypi-leindex
```

Then inspect the archive contents:
```bash
tar tf packages/npm-leindex-mcp/*.tgz | rg 'bin/leindex-embed|models/'
python -m zipfile -l packages/pypi-leindex/dist/*.whl | rg 'leindex/bin/leindex-embed|leindex/models/'
```

Expected: both package formats contain the worker binary and model assets.

- [ ] **Step 4: Verify the worker fallback and memory gate together**

Run:
```bash
cargo test --features onnx --test onnx_worker_fallback
cargo xtask memcheck
```

Expected:
- worker failure falls back to TF-IDF for the affected batch
- the C-phase memory targets stay green on the fixture

- [ ] **Step 5: Final commit if any cleanup remains**

```bash
git status
# If anything remains intentional: git add -A && git commit -m "chore(r15): finalize worker bundle rollout"
```

---

## Self-Review (mandatory; per writing-plans skill)

- [ ] Spec coverage: §4.1-4.4 and §7.1-7.9 are covered by Tasks 1-7. ✅
- [ ] Placeholder scan: no `TBD` / `TODO` / `implement later` markers in this plan. ✅
- [ ] Type consistency: worker request / response, bundle paths, and memcheck targets are named consistently across tasks. ✅
- [ ] All file paths are exact; all commands are runnable; all expected outputs are concrete.

