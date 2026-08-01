# Cross-Review — Agent B (fragment) Work Findings

**From:** Agent A's reviewer subagent (superpowers:code-reviewer)
**Subject:** Agent B's 6 commits (10d99f2d, 6017c195, bd204055, c1d3b0c2, 2417d845, e3afbe64)
**Verdict:** **Fix first** — but the fix is a process decision, not a code defect. The code is high quality + integrates cleanly with the embed-merge.

## Strengths
- **Chunker is a faithful, dependency-free Warp port** (dropped `string_offset::ByteOffset` + `line_span` crate → plain `usize` + in-tree `line_spans`). UTF-8 boundary handling (`chunker.rs:253-264`) is genuinely correct; `test_panic_regression_byte_boundary` proves the byte-boundary panic is fixed.
- **Orphan complement algorithm (`orphan.rs:60-72`) is provably correct** for all 4 traced cases; file-doc exclusion prevents a real double-index bug with FileSummary.
- **CR-F8/CR-F9 tests are genuine + deterministic** (no env/path dependence); they assert the actual fix sites (`vector_impl.rs:117` `!self.cleared`; `mod.rs:210` cache-clear-on-changed-weight).
- **neural_weight alignment is thorough** — the `2026-08-01-neural-weight-default-drift.md` finding maps all 8 occurrences; config = single source of truth (0.4). `EmbeddingConfig.neural_weight` removal is forward-compatible.
- **Config knobs correctly serde-defaulted + round-trip-tested**; empty-TOML = Default (backward compatible).

## Important
- `src/cli/index_builder/fragment/` + `src/config.rs` fragment fields — **~1300 lines of dead fragment code ship on this branch with zero production consumers.** Tasks 3-7 (store/mmap/fusion/hydration) are on `feat/fragment-embeddings-1.11.0` (NOT ancestors of this branch). Suppressed with `#[allow(dead_code)]` (`index_builder/mod.rs:35`) + `#[cfg(test)]` re-exports. **Decision needed:** (1) keep Task 1+2 here (dead-but-tested scaffolding, acceptable if 1.11.0 follows soon — recommend adding the empty-fragment guard, Minor #1), or (2) move Task 1+2 to the 1.11.0 branch (embed-merge PR stays focused, fragment lands atomically). Either defensible.

## Minor
1. `fragment/mod.rs:131` `chunk_code` — empty-file edge case yields one spurious empty fragment via the semantic path (naive path correctly returns `[]`). Latent (no consumer here) but Tasks 3-7's store/embed layer must filter empties or it'll embed/hash empty strings. Guard: `fragments.retain(|f| !f.content.is_empty());`.
2. `chunker.rs:169` — `chunk.last().expect(...)` is a dead branch (`slice::chunks()` never yields empty slices). Comment or `unwrap_or` to quiet the false alarm.
3. `fragment-embeddings-1.11.0.md:168` — stale entry-point ref (plan says `LanguageConfig::from_extension`; impl uses `LanguageId::from_extension` directly — code is more correct, doc is stale).
4. 4 near-identical `load_cached().neural_weight_f32()` call sites — acceptable (matches existing pattern); a `LeIndex::neural_weight()` would centralize. Not worth changing now.
5. Two large design docs for a feature split across branches — ensure they don't drift from 1.11.0.

## dead_code / warnings
**Agent B's fragment work does NOT contribute to the onnx-alone clippy dead_code.** The 6 failing items (`SEARCH_SNAPSHOT_VERSION`, `search_snapshot`/`restore_from_search_snapshot`, `SearchSnapshot`/`SearchSnapshotNode`, `MmapVectorIndex::from_snapshot`) all originate from commit `6a6e3bf9` — **pre-existing cli-only snapshot machinery**, not the fragment feature. Fix belongs to whoever gates the snapshot module behind `#[cfg(feature="cli")]` (or `storage`). `cargo clippy --workspace --all-targets -- -D warnings` (default) is clean; `cargo test --features cli fragment` = 21/21; `config` = 47/47.

## Integration assessment
**Merges cleanly with Agent A's embed-merge. No conflicts:**
- No subcrate assumptions (uses `crate::parse::grammar::LanguageId` + `crate::cli::index_builder::preceding_doc_context`).
- Uses canonical `src/config.rs` (fragment knobs on `SearchConfig` alongside `neural_weight`/`rerank_*`; `neural_weight_f32()` next to `load_cached()`).
- Feature DAG respected (compiles under `cli`; no onnx→cli / graph→cli deps).
- Shared-file edits non-conflicting (`src/config.rs`, `src/cli/config.rs`, `src/search/search/*` — additive, different regions).

## Ready-to-merge subset
The neural_weight alignment work (`10d99f2d`, `c1d3b0c2`, `6017c195`) is **unambiguously ready** — correct, tested, fixes a real default-drift bug independent of the fragment feature.
