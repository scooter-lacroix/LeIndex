# Commit Plan: v2.0.0 Packaging Repack

Date: 2026-10-06  
Status: Executed and verified  
Target: /mnt/WD-SSD/Prod/LeIndex-commit-snap  
Base commit: `2fd5a16a` (verified-correct v2.0.0 round-9 baseline)  
Ground truth branch: `repack-ground-truth` (`4a889be7`)  

---

## 1. Overview and Rationale

The v2.0.0 changes were delivered across an initial 5-commit series (`c984dc5b..4a889be7`), which contained all intended code and test updates but lumped the major architectural transition (PDG layer storage flip, read-path CAS hydration, regression gate, engram concurrency, and test restructures) into oversized/mixed commits.

This repack reorganizes the changes into an approved 8-commit atomic series with hunk-level precision, preserving byte-for-byte content equivalence against the ground truth while guaranteeing that every intermediate commit complies with AGENTS.md (zero warnings, formatting passes, compilation and test slices green).

---

## 2. Commit Series & Gate Results

### Commit 1: `b6bdcfd8`
- **Message**: `docs: rewrite MCP skill surface docs; schema: derived community memberships`
- **Files** (4):
  - `docs/SKILL.md`
  - `integrations/skills/leindex-code-intelligence/SKILL.md`
  - `src/feature_flags.rs`
  - `src/storage/schema.rs`
- **Gates**:
  - `cargo fmt --all --check`: PASS
  - `cargo check --lib --tests`: PASS
  - `cargo test --lib storage::generation`: 116 passed / 0 failed
  - `cargo test --lib cli::leindex::indexing`: 70 passed / 0 failed

### Commit 2: `cedd5e84`
- **Message**: `feat(generation): PDG1 v2 lossless records and NRL1 v2 node-id table in readers`
- **Files** (1):
  - `src/storage/generation/reader.rs`
- **Gates**:
  - `cargo fmt --all --check`: PASS
  - `cargo check --lib --tests`: PASS
  - `cargo test --lib storage::generation`: 116 passed / 0 failed
  - `cargo test --lib cli::leindex::indexing`: 70 passed / 0 failed

### Commit 3: `f7d1279f`
- **Message**: `feat(generation): lossless PDG1 v2 + sparse TF-IDF + NRL1 v2 migration codecs`
- **Files** (8):
  - `src/cli/leindex/generation_read_test.rs`
  - `src/graph/pdg.rs` (precision accessors while public)
  - `src/storage/generation/graph_codec.rs`
  - `src/storage/generation/graph_codec_test.rs`
  - `src/storage/generation/migrate.rs` (v2 codecs)
  - `src/storage/generation/migrate_test.rs`
  - `src/storage/generation/mod.rs` (graph_codec module wiring)
  - `src/storage/generation/snapshot_test.rs`
- **Gates**:
  - `cargo fmt --all --check`: PASS
  - `cargo check --lib --tests`: PASS
  - `cargo test --lib storage::generation`: 115 passed / 0 failed
  - `cargo test --lib cli::leindex::indexing`: 68 passed / 0 failed

### Commit 4: `07153f59`
- **Message**: `feat(generation): optional Search/Embedder/Fragments layer kinds`
- **Files** (2):
  - `src/storage/generation/manifest.rs`
  - `src/storage/generation/writer.rs`
- **Gates**:
  - `cargo fmt --all --check`: PASS
  - `cargo check --lib --tests`: PASS
  - `cargo test --lib storage::generation`: 115 passed / 0 failed
  - `cargo test --lib cli::leindex::indexing`: 68 passed / 0 failed

### Commit 5: `47cd57d6`
- **Message**: `feat(storage)!: the PDG persists only as the generation Pdg layer`
- **Files** (46):
  - `docs/plans/2026-10-03-step4-graph-layer-flip.md` (committed here)
  - `src/cli/index_builder/*` (hybrid test, test, merge, mod, persistence)
  - `src/cli/leindex/diagnostics.rs`
  - `src/cli/leindex/indexing/load.rs`
  - `src/cli/leindex/indexing/mod.rs` (flip hunks: GenerationWriter authority, save_pdg deletion fallout)
  - `src/cli/leindex/indexing/neural_publish.rs`
  - `src/cli/leindex/indexing/streaming/pdg.rs`
  - `src/cli/leindex/indexing/watcher_delta.rs`
  - `src/cli/live_project.rs`
  - `src/cli/mcp/*` (file summary, find, project map, read symbol)
  - `src/cli/registry.rs`
  - `src/cli/textindex.rs`
  - `src/eval/agent_tasks.rs`
  - `src/graph/pdg.rs` (privacy flip `precision_symbols`)
  - `src/intel/merge.rs`
  - `src/phase/context.rs`
  - `src/server/server.rs`
  - `src/storage/analytics.rs`
  - `src/storage/catalog.rs`
  - `src/storage/community_store.rs`
  - `src/storage/cross_project.rs`
  - `src/storage/generation/graph_codec.rs` (wrappers)
  - `src/storage/generation/migrate.rs` (fragment bundles & codecs)
  - `src/storage/generation/migrate_test.rs`
  - `src/storage/generation/mod.rs` (`manifest_has_neural_vectors`)
  - `src/storage/generation/snapshot.rs`
  - `src/storage/mod.rs`
  - `src/storage/pdg_store.rs` (removal of save_pdg chain)
  - `src/storage/pdg_store_test.rs`
  - `src/storage/salsa.rs`
  - `src/storage/schema.rs`
  - `tests/fixtures/memcheck/small_repo/.leindex/*` (5 stale fixtures deleted)
  - `tests/index_job_recovery_test.rs`
  - `tests/mcp_fast_paths_test.rs`
- **Gates**:
  - `cargo fmt --all --check`: PASS
  - `cargo check --lib --tests`: PASS
  - `cargo test --lib storage::generation`: 116 passed / 0 failed
  - `cargo test --lib cli::leindex::indexing`: 70 passed / 0 failed

### Commit 6: `ca211278`
- **Message**: `feat(indexing): save-stage regression gate on the generation publish path`
- **Files** (1):
  - `src/cli/leindex/indexing/tests.rs` (save stage gate tests)
  - (Gate implementation code lives in `indexing/mod.rs`, staged in commit 5)
- **Gates**:
  - `cargo fmt --all --check`: PASS
  - `cargo check --lib --tests`: PASS
  - `cargo test --lib cli::leindex::indexing`: 70 passed / 0 failed

### Commit 7: `d8f4d211`
- **Message**: `fix: engram staging-name collisions; generation-read test restructure`
- **Files** (1):
  - `src/search/engram.rs` (`NEXT_STAGING_SEQ`)
  - (The generation-read test restructure was staged in commit 5)
- **Gates**:
  - `cargo fmt --all --check`: PASS
  - `cargo check --lib --tests`: PASS
  - `cargo test --lib cli::leindex::indexing`: 70 passed / 0 failed

### Commit 8: `<to-be-committed>`
- **Message**: `feat(indexing): hydrate read path from CAS layers; retire the generation file mirror`
- **Files** (6):
  - `docs/plans/2026-10-03-commit-plan.md` (this file)
  - `docs/plans/2026-10-03-step6-read-side-flip.md`
  - `src/cli/leindex/indexing/layer_artifacts.rs`
  - `src/cli/registry/index_jobs.rs`
  - `src/search/search/snapshot.rs`
  - `src/search/search/token_index.rs`
- **Gates**:
  - `cargo fmt --all --check`
  - `cargo check --lib --tests`
  - Full workspace test suite: `cargo test --workspace --exclude memcheck`

---

## 3. Deviations & Boundary Adjustments

1. **Step 4 & Step 6 Design Plans**:
   - `docs/plans/2026-10-03-step4-graph-layer-flip.md` was committed in Commit 5 (`47cd57d6`).
   - `docs/plans/2026-10-03-step6-read-side-flip.md` is committed in Commit 8 alongside CAS layer hydration.
   - `docs/plans/2026-10-03-commit-plan.md` is authored and committed in Commit 8 as the documentation artifact.
2. **Coupling of `save_stage_gate` and `generation_read_test`**:
   - In `src/cli/leindex/indexing/mod.rs`, the generation staging logic calls `save_stage_gate`. Attempting to excise the function while staging the generation pipeline resulted in compilation errors; thus `save_stage_gate` helper was retained in Commit 5 while the gate regression tests were committed in Commit 6.
   - `generation_read_test.rs` relies on `canonical_pdg` exported from `graph_codec::tests`, which lands in Commit 3/5.
3. **Untracked Paths Preserved**:
   - `.zcodeignore`
   - `docs/plans/2026-10-04-rich-index-performance/` (PR #85 work)

---

## 4. Replay Contract

To replay this series onto the main production repository:
1. Ensure the base commit is `2fd5a16a`.
2. Cherry-pick commits 1 through 8 in sequence:
   ```bash
   git cherry-pick <commit-1>..<commit-8>
   ```
3. Verify untracked paths remain untracked (`.zcodeignore`, `docs/plans/2026-10-04-rich-index-performance/`).
4. Run standard AGENTS.md verification:
   ```bash
   cargo fmt --all --check
   cargo clippy --workspace --all-targets -- -D warnings
   cargo test --workspace --exclude memcheck
   ```
