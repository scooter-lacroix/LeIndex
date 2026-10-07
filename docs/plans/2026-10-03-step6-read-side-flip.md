# Step 6 — Read-side flip: hydrate from CAS layers, retire the file mirror

Status: spec for implementation. Owner: implementing agent. Reviewer: orchestrator.
Binding. Where it says verify, verify.

## 0. State you inherit (uncommitted working tree — never stash/commit/reset)

Branch v2.0.0, /mnt/WD-SSD/Prod/LeIndex. On top of HEAD sits the completed,
gate-verified step 1–5 arc (2569/0 workspace, release e2e green):

- Graph persists ONLY as PDG1 v2 CAS layers; `save_pdg` is deleted; hydration
  decodes the layer (`graph_codec.rs`); 28 SQL graph-query sites migrated.
- `publish_generation_snapshot` stages ALL layers into CAS via
  `stage_generation_layers` (5 core + optional Search/Embedder/Fragments when
  artifacts exist) and `GenerationWriter::publish` commits manifest + CURRENT.
- Save-stage timing gate lives in the publish path (`save_stage_gate`).
- The **file mirror is still written**: `prepare_generation_snapshot` copies
  `leindex.db`, `embeddings.bin`, `search_snapshot.bin`, `tfidf_embedder.bin`,
  (+`neural_embeddings.bin` when neural ran) into `generations/<N>/`.

Measured on a fresh scratch index of this repo (release build, post-flip):

- `generations/1/` = 54 MB (embeddings.bin 42M, search_snapshot.bin 6.5M,
  leindex.db ~5M — small now because intel rows are gone)
- `.leindex/cas/` = 23 MB (7 blobs: 5 core + Search + Embedder)
- `du -sh .leindex` = 57 MB — LESS than 54+23. **Anomaly to resolve first**:
  determine whether CAS blobs or mirror files are hardlinks (check
  `stat` link counts, `du -l`), and state the true incremental cost of a
  generation in your P0 report. Do not build on an unexplained number.

## 1. Objective

1. Every generation-read path hydrates from the CAS layers named in the
   manifest — no consumer reads `generations/<N>/{search_snapshot.bin,
   embeddings.bin, tfidf_embedder.bin, leindex.db, neural_embeddings.bin}`.
2. Publish stops writing the mirror. A generation directory becomes:
   `manifest` + `index-state.json` + `index_stats.json` (KB-scale metadata).
3. Footprint: a fresh index of this repo lands ≤ 30 MB apparent in
   `.leindex/` (CAS 23 MB + metadata + top-level working set), and a second
   published generation adds ~0 incremental MB when content is unchanged
   (CAS dedup) — assert both.
4. Legacy stores (no manifest) keep working through the existing SQL/mirror
   fallback paths, untouched.

## 2. Decisions

### D1 — P0 consumer map (append to this file)

Enumerate every reader of the mirror files. Known seeds, verify with search:
- `src/cli/leindex/indexing/load.rs` — `load_snapshot_engine(artifact_path)`,
  `try_load_*_mmap_embeddings_from_storage`, fragment twins; note the comment
  in `try_hydrate_from_generation_inner` admitting the snapshot temp dir
  "holds no search-snapshot/embedder artifacts, so hydration uses the rebuild
  path" — that rebuild is what the layers should replace.
- `src/cli/registry/index_jobs.rs` — `refresh_loaded_from_active_generation`
  and the `neural_embeddings.bin` presence probe (~line 270).
- `src/cli/leindex/indexing/watcher_delta.rs` — artifact reads.
- `tests/index_job_recovery_test.rs` — asserts `generations/1/search_snapshot.bin`
  is a file (contract changes; update to the layer contract).
Classify each: hydrate-from-layer / probe-manifest / test-update.

### D2 — Layer hydration

`GenerationSnapshot` already mmaps Tfidf/Neural/Pdg/Symbols and materializes
Db. Extend it (or add a sibling in `snapshot.rs`) to serve:
- **Search**: CAS blob bytes → the same decode `load_snapshot_engine` applies
  to `search_snapshot.bin` (find the decoder; feed it blob bytes).
- **Embedder**: Embedder blob → `TfIdfEmbedder` deserialize (today reads
  `tfidf_embedder.bin`).
- **Neural presence**: manifest `LayerKind::Neural` hash != empty-neural hash,
  replacing the file-existence probe in registry/index_jobs.rs.
Wire these into `try_hydrate_from_generation_inner` /
`load_from_storage_inner_at` so the generation-read path restores search from
layers instead of rebuilding. Flag-off / no-manifest → existing paths unchanged.

### D3 — Stop writing the mirror

`prepare_generation_snapshot` drops the artifact copies (keep
`index-state.json`, `index_stats.json`, the WAL checkpoint, dir fsync).
Fragments: the Fragments layer already bundles them — the mirror copies go;
cold-start fragment hydration reads the bundle (decode_fragment_bundle exists,
currently `#[cfg(test)]` — productionize it).

### D4 — Retention & tests

- Retention/GC already pin optional layers via `layer_hashes` — verify a
  leased generation survives GC with the mirror gone.
- Update every test that asserts mirror files (recovery, fault scenarios,
  soak). Assertions move to: manifest lists the layer, blob exists in CAS,
  layer round-trips. Do not delete coverage — relocate it.

## 3. Validation gate (all must pass, quoted in the final report)

1. `cargo fmt --all --check` clean.
2. `CARGO_TARGET_DIR=/tmp/ldx-target cargo clippy --workspace --all-targets -- -D warnings` exit 0.
3. `CARGO_TARGET_DIR=/tmp/ldx-target cargo test --workspace --exclude memcheck` — 0 failed.
4. Release e2e on a scratch copy (never the real `.leindex`, isolated HOME):
   index → exit 0; `generations/1/` contains ONLY
   {manifest, index-state.json, index_stats.json}; `intel_nodes`=0;
   `indexed_files`>0; search returns results **from layer hydration**
   (verify via log or a debug counter, not just exit code); second index run
   no-op; force re-index publishes generation 2 whose incremental footprint
   (unchanged content) is < 1 MB; `du -sh .leindex` ≤ 30 MB.
5. State the resolved hardlink anomaly and the true per-generation cost.

## 4. Landmines

- Repo's own `target/` is poisoned (E0514) — always `/tmp/ldx-target` /
  `/tmp/ldx-target-rel` (cold after the /tmp wipe; first build ~8 min, detach).
- Never run the binary against the real repo's `.leindex`.
- One writer: you are the only agent in the tree. Shell only for
  cargo/git/sqlite3 — search/read/edit via the structured tools.
- No `#[allow]`, no weakened assertions, no skipped tests.
- The save-stage gate (`save_stage_gate`) must not trip in the release e2e;
  if your changes slow publish past budget, that's a finding, not a knob.

## 5. Escalation

If layer hydration cannot reproduce search results bit-comparably with the
mirror path (quality regression), STOP, leave the mirror on, report the delta
with numbers. A correct-but-bigger store beats a smaller wrong one.

## 6. P0 report (implementing agent)

### 6.1 Hardlink anomaly — RESOLVED: there are no hardlinks

Measured on the preserved scratch `/tmp/ldx-scratch-w/.leindex` (release build
10:51, no source file newer than the binary):

- `stat -c %h` on all 7 CAS blobs and on `embeddings.bin`, `leindex.db`,
  `search_snapshot.bin` (top-level AND `generations/1/` copies): link count
  **1** everywhere, all distinct inodes. Nothing is hardlinked.
- The "57M" is a `du` artifact: `du -sh generations/1 cas .leindex` counts each
  inode once across arguments, so the trailing `.leindex` entry reports only
  what the first two did not cover — the **top-level working set**
  (`embeddings.bin` 43M, `search_snapshot.bin` 7M, `leindex.db` 4M,
  `textindex/` 2.8M). 54 + 23 + 57 was never "less than"; it was three
  disjoint sets printed by a dedup-ing tool.
- True standalone numbers (`du -sk`): `.leindex` = **135,796 KB (133 MB)** =
  generations 55,028 + cas 22,836 + top-level 57,932.

True per-generation cost TODAY: ~54 MB mirror (never deduplicated — plain
`fs::copy`) + CAS blobs that dedup on unchanged content. After D3 the
per-generation cost is KB-scale metadata plus changed CAS blobs.

### 6.2 Gate finding: `du -sh .leindex <= 30MB` is unreachable by D3 alone

After D3 the tree is CAS 23 MB + top-level working set 58 MB + metadata
= **~81 MB**. The spec's own §1.3 budget line ("CAS 23 MB + metadata +
top-level working set") sums to >= 81 MB, not <= 30 MB. Getting to 30 MB
requires retiring the top-level `embeddings.bin` / `search_snapshot.bin` /
`leindex.db` working copies (i.e. incremental indexing sourcing its working
state from CAS layers) — a separate design, not covered by D1-D4. Needs an
orchestrator ruling before the §3.4 `du` assertion can be meaningful.

### 6.3 Consumer map (verified by search against the dirty tree)

Classes: **H** hydrate-from-layer, **P** probe-manifest, **M** move off the
generation-dir file (materialize the Db layer from CAS), **T** test-update,
**N** no change needed.

| # | Site | What it reads | Class |
|---|------|---------------|-------|
| 1 | `indexing/load.rs` `load_snapshot_engine` (L167) → `restore_engine_from_snapshot` | `search_snapshot.bin`, `tfidf_embedder.bin`, `embeddings.bin`, `neural_embeddings.bin`, fragment files via `index_builder::try_load_*_from_storage(artifact_path)` | **H** |
| 2 | `index_builder/persistence.rs` `try_load_search_snapshot_from_storage` (~L189, bincode `SearchSnapshot`); `tfidf.rs` `load_from_artifact_path` (~L359); `index_builder/mod.rs` `try_load_mmap_embeddings_from_storage` (~L1892); `persistence.rs` neural (~L356) / fragment (~L447) twins | the decoders for #1 — all take a *directory*, read a fixed filename | **H** (add bytes-based decode twins; the dir-based ones stay for the legacy/top-level path) |
| 3 | `indexing/load.rs` `hydrate_search_engine_from_loaded_pdg` (L107) | `artifact_path = active_storage_path()` = `generations/N/` → mirror files | **H** |
| 4 | `indexing/mod.rs` `try_hydrate_from_generation_inner` (L1497) | passes the snapshot temp dir (holds only `leindex.db`) as `artifact_path` → search always REBUILDS from PDG (the admitted gap) | **H** (primary D2 wiring point) |
| 5 | `indexing/mod.rs` `load_from_active_storage` (~L1428-1450) | flag-off/no-manifest fallback: `active_storage_path()` + `generations/N/leindex.db` + mirror artifacts | **N** for legacy no-manifest stores; see 6.4 for flag-off |
| 6 | `cli/registry/index_jobs.rs` `finish_owned_index` (~L265-280) | probes `generations/N/neural_embeddings.bin` non-empty → `mark_neural_published` | **P** (manifest `LayerKind::Neural` hash != empty-neural hash) |
| 7 | `cli/registry.rs` `refresh_loaded_from_active_generation` (L1515) / `force_refresh…` | no file read; calls `load_from_active_storage` | **N** |
| 8 | `cli/registry.rs` `prewarm_project` (L460) | `active_storage().join("leindex.db").is_file()` as the "is indexed" probe | **M** (missed by the seed list) |
| 9 | `cli/registry.rs` `restore_latest_generation` (~L1715-1745) | corruption recovery: copies `generations/N/leindex.db` over a bad root DB | **M** (must write the Db-layer blob instead; missed by the seed list) |
| 10 | `cli/live_project.rs` `active_storage` (L34-47) | selects a generation dir only if `leindex.db` AND `index-state.json` are files there | **M** — after D3 this silently falls back to the MUTABLE root for every live tool (missed by the seed list) |
| 11 | `cli/mcp/read_symbol_handler.rs` (L130), `file_summary_handler.rs` (L132), `find_handler.rs` (L84) | `live.active_storage().join("leindex.db")` direct open | **M** (same root cause as #10) |
| 12 | `cli/leindex/mod.rs` `active_storage_path` (L909), `active_has_indexed_files` (L917) | delegate to #10; open `active.join("leindex.db")` | **M** |
| 13 | `phase/context.rs` `persist_graph_via_generation` (~L412-420) | WRITES `generations/N/leindex.db` mirror "for flag-off readers" | **M** (second mirror writer, outside `prepare_generation_snapshot`) |
| 14 | `indexing/mod.rs` `prepare_generation_snapshot` (L327-380) | WRITES mirror: db, search_snapshot, embeddings, tfidf_embedder, neural, 4 fragment files | **D3 target** |
| 15 | `indexing/watcher_delta.rs` | no mirror read; writes top-level artifacts then `publish_generation_snapshot` (L385) | **N** (inherits D3) |
| 16 | `indexing/neural_publish.rs` (~L492) | builds `PublishedGeneration.storage_path` for a dir only; no read | **N** |
| 17 | `tests/index_job_recovery_test.rs` L196 | `assert!(generations/1/search_snapshot.bin .is_file())` | **T** → manifest lists Search layer + blob exists + decodes |
| 18 | `src/cli/live_project.rs` test (L79), `registry_test.rs` (L77), `indexing/tests.rs` (L136/450/672), `migration/migration_test.rs` (L57), `tests/fault/scenarios_07_12_test.rs` (L329-337), `tests/soak/scenarios_19_24_test.rs` (L108) | build/inspect `generations/N/*` | **T** — each to be read and classified in D4 (NOT yet verified which assert mirror files) |

### 6.4 Spec contradiction to resolve: flag-off readers vs. a metadata-only dir

§2 D2 says "Flag-off / no-manifest -> existing paths unchanged" while §1.2/§3.4
require the generation dir to hold ONLY {manifest, index-state.json,
index_stats.json}. `LEINDEX_FEATURE_GENERATION_READERS=false` /
`LEINDEX_LEGACY=1` (flag default ON, `feature_flags.rs`) reads
`generations/N/leindex.db` + mirror artifacts. Both cannot hold. Proposed
resolution (implemented unless overruled): no-manifest legacy stores keep the
mirror path untouched; for manifest stores the flag-off path materializes the
Db layer from CAS (rows 8-12) and the flag-off search path takes the existing
rebuild-from-PDG route (slower, correct). Mirror-writing is not retained.

## 7. Implementation status (final)

D1-D4 implemented and gate-verified, except the §3.4 `du -sh .leindex <= 30MB`
assertion, which is unreachable by D1-D4 (see §6.2; measured 79M = CAS 21M +
top-level working set 58M).

- **D2**: `layer_artifacts.rs` materializes Search/Embedder (verbatim), Tfidf
  (sparse→dense LIEE keyed by Pdg-layer node order), Neural (row→node-id
  re-keying), and Fragments (`decode_fragment_bundle`, productionized) into
  the snapshot's own tempdir, so the SAME legacy decoders and freshness checks
  run on generation reads (`try_hydrate_from_generation_inner`). Absent layers
  fall back to the rebuild path. Neural presence for the job state comes from
  the manifest hash (`manifest_has_neural_vectors`), replacing the
  `neural_embeddings.bin` file probe.
- **D3**: `prepare_generation_snapshot` copies only KB-scale metadata
  (health + stats); all payload goes through CAS staging. The phase-graph
  publisher's `generations/N/leindex.db` mirror is gone too.
- **M-class sites** (§6.3 rows 8-12): `LiveProject::active_storage` accepts a
  manifest as the completion marker; new `catalog_db()` serves freshness
  reads from the legacy mirror or the mutable root; `restore_latest_generation`
  recovers the catalog from the CAS Db layer when no mirror exists.
- **Dedup fixes required by the "<1MB incremental" gate** (found by diffing
  gen1/gen2 layer hashes on the real repo): (1) the staged Db layer is now
  canonicalized — `project_metadata` collapsed to the `_0` identity with all
  `project_id` references rewritten, volatile clocks/counters zeroed, and the
  insertion-order-sensitive tables rebuilt through a sorted temp table
  (`rebuild_table_sorted`; plain VACUUM preserves b-tree cell order);
  (2) the Search snapshot is now canonical — dictionary sorted
  lexicographically (`to_dictionary`) and nodes sorted by id at serialize
  time. Both were per-run nondeterminism that made identical content hash
  differently, defeating CAS dedup (+10.4MB/generation before the fix, 0KB
  after).
- **Flag-off resolution** (§6.4): implemented as proposed — legacy no-manifest
  stores untouched; manifest stores keep flag-off graph reads on the SQL
  catalog fallback and flag-off search on the rebuild route.
- **Tests**: `test_layer_search_hydration_matches_graph_rebuild` proves the
  §5 equivalence (layer-restored vs rebuilt search: same ids, bit-identical
  scores, mmap-backed engine asserted); mirror-file assertions in
  `indexing/tests.rs` and `tests/index_job_recovery_test.rs` relocated to the
  manifest+blob contract (coverage moved, none deleted).

## 8. Orchestrator ruling (2026-10-06, post-verification)

Gate item 4's `du -sh .leindex` ≤ 30 MB is a SPEC ERROR, not an
implementation miss: the ceiling was derived from the pre-anomaly "57 MB"
figure, which §6.1 proved was du's cross-argument inode dedup hiding the true
135 MB total. The operative objectives are all met and independently
re-verified by the orchestrator (fresh scratch, fresh process):

- per-generation cost: 54 MB fs::copy mirror → 12 KB metadata ✓
- cross-generation duplication: +10.4 MB/gen (dedup failure) → 0 KB ✓
- store footprint is O(1) in generations: 77 MB total after gen 2 ✓
- against the original user budget (≤ 100 MB always-on store): 77 MB ✓

Remaining 77 MB = canonical CAS 20.2 MB + writer working set ~54 MB
(embeddings.bin 43.9 + search_snapshot.bin 6.7 + leindex.db 3.4) + metadata.
Eliminating the working set requires the indexer to source its mutable state
from CAS (read-back writer) — a deeper design change, deliberately out of
scope for D1–D4. Recorded as future work, not a regression.

Gate verdict: PASS with this erratum. Step 6 complete.
