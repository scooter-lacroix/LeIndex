# Cluster C — Neural publish atomicity, rollout-flag wiring, CI gate calibration

**Status:** verification-only root-cause analysis. **No implementation in this branch.**
**Base:** `f31f107e` (`docs/setup-overhaul`), worktree `/mnt/WD-SSD/Prod/LeIndex-issues`.
**Scope:** issues #103, #86, #87.

Every claim below is backed by a `path:line` reference that was read in this worktree. Where a
claim could not be verified in this environment it is labelled and the confirming step is named.

---

## Issue #103 — neural publish generation-scoped tier filenames and manifest swap

**Verdict: CONFIRMED** (invariant break real, causing code located) — **but narrower than the issue
text implies**: the offending files are the MRL *tier* sidecars, **not** the canonical
`neural_embeddings.bin`, which is already published atomically.

### Evidence (verified by reading)

The publish sequence has three phases. Quoted in execution order.

**Phase 0 — the entry point and the generation-scoped decision it does *not* make**
`src/cli/index_builder/persistence.rs:311-340`:

```
311: pub(crate) fn persist_neural_embeddings_to_mmap(
312:     search_engine: &SearchEngine,
313:     project_path: &Path,
314:     identity: Option<&str>,
315:     matryoshka: crate::config::MatryoshkaSetting,
316: ) -> Result<()> {
317:     let embeddings = search_engine.collect_neural_embeddings();
318:     let path = neural_mmap_embeddings_path(project_path);
...
330:     let staged = stage_neural_tiers(project_path, &path, identity, native_dim, &buckets)?;
331:     commit_neural_generation(
```

**Phase 1 — stage every tier at a sibling `.staged` path, then REOPEN to validate**
`persistence.rs:428-493`. Two distinct final paths, both FIXED:

```
436:     for (dim, rows) in buckets {
437:         let final_path = if *dim == native_dim {
438:             canonical_path.to_path_buf()            // .leindex/neural_embeddings.bin
439:         } else {
440:             neural_tier_mmap_embeddings_path(project_path, *dim)   // .leindex/neural_embeddings_{dim}.bin
441:         };
442:         let stage_path = neural_staging_path(&final_path);
443:         crate::search::vector::write_mmap_embeddings_with_identity(&stage_path, rows, identity)
...
445:         match crate::search::vector::MmapEmbeddingIndex::open(&stage_path) {
446:             Ok(written) if written.len() == rows.len() => {}
447:             Ok(written) => { return Err(...rows mismatch...); }
455:             Err(e) => { return Err(...reopen failed...); }
```

The FIXED paths are concretely:

- `persistence.rs:626-630` — `neural_tier_mmap_embeddings_path` → `.leindex/neural_embeddings_{dim}.bin`
- `persistence.rs:930-932` — `neural_mmap_embeddings_path` → `.leindex/neural_embeddings.bin`
- `persistence.rs:660-662` — `neural_generation_manifest_path` → `.leindex/neural_generation.bin`

**Phase 2 — rename each staged tier onto its FIXED path, sequentially**
`persistence.rs:531-539`:

```
531:     for tier in &staged {
532:         publish_staged_tier(tier)?;
533:         info!(
534:             count = tier.rows,
535:             dim = tier.dim,
536:             path = %tier.final_path.display(),
537:             "Published neural embeddings mmap tier"
538:         );
539:     }
```

`persistence.rs:601-622` — the rename itself, with its durability properties:

```
610:     // Same swap the mmap writer uses: a plain `rename` cannot replace a file a
611:     // serving process still has mapped on Windows, and these tiers are exactly
612:     // the files `MmapEmbeddingIndex` keeps mapped.
613:     crate::search::vector::replace_file_atomically(&tier.stage_path, &tier.final_path)
```

The stage file itself was made durable *before* the loop, at write time —
`src/search/vector.rs:924-950` (`write_atomic`) does `sync_all` on the staging file, then
`replace_file_atomically`, then `sync_parent_dir`:

```
938:         writer.get_ref().sync_all()?;
940:         replace_file_atomically(&staging, path)?;
941:         sync_parent_dir(path);
```

So each *individual* staging write is durable and each *individual* tier swap is atomic.
**The sequence of swaps is not.**

**Phase 3 — the manifest lands LAST, then superseded sidecars are deleted**
`persistence.rs:545-568`:

```
548:     let mut committed: Vec<(usize, usize)> = staged
549:         .iter()
550:         .filter(|tier| tier.rows > 0)
551:         .map(|tier| (tier.dim, tier.rows))
552:         .collect();
553:     if header_only {
554:         committed.push((native_dim, 0));
555:     }
556:     persist_neural_generation_manifest(storage_path, identity, matryoshka, &committed)?;
...
563:     let keep: Vec<usize> = staged
564:         .iter()
565:         .filter(|tier| tier.final_path != canonical_path)
566:         .map(|tier| tier.dim)
567:         .collect();
568:     remove_neural_tier_mmaps(project_path, &keep)?;
```

`persistence.rs:670-698` — the commit point. `fragment::atomic_write` is fsync + rename:

```
684:     let path = neural_generation_manifest_path(storage_path);
685:     let payload = bincode::serialize(&manifest).context("Failed to serialize neural manifest")?;
686:     fragment::atomic_write(&path, &payload).with_context(|| {
```

`src/cli/index_builder/fragment/mod.rs:205-232` is `temp → sync_all → rename`. Note
`fragment/mod.rs:228-230` explicitly omits the directory sync:

```
228:     // Directory sync omitted: the file-level fsync + rename is sufficient for
229:     // data durability of a rebuildable index artifact. The directory sync only
230:     // guarantees rename atomicity across power loss, which is not critical here.
```

**The code already documents this exact hole** — `persistence.rs:503-518`:

```
503: /// What that does NOT buy, stated plainly because the comment here once
504: /// overclaimed it: the commit is a sequence of renames onto FIXED paths, so a
505: /// crash between two of them leaves some tiers new and some previous while the
506: /// PREVIOUS manifest is still in force. The previous files are gone (the rename
507: /// replaced them), so there is no earlier generation to fall back to; the reader
508: /// compares that mixed set against the old manifest and disables any tier whose
509: /// ROW COUNT changed. A tier whose row count happens to be unchanged — the common
510: /// incremental case, same documents re-embedded — passes that check and is
511: /// served. ...
516: /// the manifest swap), which is a storage-layout change tracked in #103. A
517: /// publish must not run concurrently with a served index for exactly this
518: /// reason.
```

### True root cause

Not "the manifest is written too late". The manifest ordering is already correct and the manifest
is already atomic-plus-durable.

The real mechanism is a **missing identity layer in the path space**: every tier is addressed by
dimension only (`neural_embeddings_{dim}.bin`), so the *only* thing distinguishing generation A's
512-row file from generation B's 512-row file is its bytes. A publish therefore has no way to make
"the whole new set" visible at once; it can only flip files one at a time. Consequently:

1. **Mixed state exists and is reachable.** Crash (or `?`-propagation failure) between
   `publish_staged_tier` calls at `persistence.rs:531-539` leaves an arbitrary prefix of tiers at
   generation N+1 and the suffix at generation N, while `neural_generation.bin` still describes
   generation N.
2. **No reader can detect it in the general case.** The reader-side defences are
   (a) manifest row-count agreement and (b) the per-file identity trailer. Both fail in exactly the
   common incremental case:
   - a tier re-embedded under the *same* provider/model identity has the **same identity string**,
     so the identity gate passes on mixed bytes;
   - a tier whose *row count is unchanged* (the ordinary incremental re-index, same documents
     re-embedded with new/changed weights or a changed upstream graph subset that preserves
     per-tier cardinality) passes the row-count gate at `persistence.rs:885-896`.
3. **There is no fallback generation.** The rename at `persistence.rs:613` replaced the previous
   bytes in place. There is no generation-N copy anywhere on disk to fall back to — confirmed by
   `remove_neural_tier_mmaps` (`persistence.rs:749-765`) iterating the closed set `MRL_TIERS`
   (`src/search/mrl.rs:25` — `[128, 256, 512, 768]`) at the single flat `.leindex/` root with no
   generation directory anywhere in the path.

Secondary, same-shape defect — a **stale-manifest window with the opposite polarity**:
`persistence.rs:556` commits the new manifest, then `persistence.rs:568` deletes superseded
sidecars. A crash in between leaves a sidecar that the *new* manifest does not name. The reader
does reject it (`persistence.rs:871-878`, "not part of the committed generation; tier disabled"),
so it degrades rather than corrupts — but it is the same non-atomicity, and the existing test
`test_stale_sidecar_duplicating_the_canonical_dimension_is_not_returned_as_a_tier`
(`persistence.rs:1428`) exists precisely because of it.

Third, an adjacent durability asymmetry worth recording: the canonical file at
`persistence.rs:613` goes through `replace_file_atomically`, which on Unix is a bare
`std::fs::rename` (`src/search/vector.rs:969+`) with **no parent-directory fsync**, while the
staging `write_atomic` path does sync the parent (`vector.rs:941`). The rename is atomic in the
page cache; its durability across power loss depends on the filesystem. This is not the mixed-state
bug, but a fix that claims crash-safety must close it.

### Does a generation identity/marker already exist that lets a reader detect this?

**Partially — and the partial detection is misleading, which is the trap.**

- **Format:** `src/search/vector.rs:319-369` — 16-byte header
  `[0..4] b"LIEE"`, `[4..8] version`, `[8..12] node_count`, `[12..16] dimension`. Little-endian.
  Validated at `vector.rs:413-426`.
- **Identity:** an in-band *trailer*, written by `write_mmap_embeddings_with_identity`
  (`vector.rs:1081+`) and length-prefixed as a `u32` (`vector.rs:1139-1145`), read back into
  `MmapEmbeddingIndex::identity` (`vector.rs:386`, accessor at `vector.rs:711-713`).
- **Manifest:** `NeuralGenerationManifest` (`persistence.rs:642-654`) carries
  `schema_version` (const `1`, `persistence.rs:657`), `identity`, `matryoshka`, and
  `tiers: Vec<(usize, usize)>`. Loaded via `try_load_neural_generation_manifest`
  (`persistence.rs:718-744`), which fails *open* on schema mismatch or deserialize error.

Neither marker is a **generation number**. `identity` is a *content* identity
(provider/model/dimension/template) — deliberately so, per the rationale at
`persistence.rs:342-367`. `tiers` is a bag of `(dimension, row_count)` pairs. There is **no field
on disk that says "this set of files is generation G"**, therefore:

- same content + same cardinality across a torn publish ⇒ **indistinguishable**, served silently;
- the reader's own comment concedes the ceiling: `persistence.rs:509-513` — *"A tier whose row
  count happens to be unchanged ... passes that check and is served."*

There is, however, an existing generation counter that the fix should **reuse rather than
reinvent**: `src/storage/generation/lease.rs:37-46` `read_current_generation` (the `CURRENT` file,
ASCII digits) and `src/storage/generation/manifest.rs:107-...` `Manifest.generation: u64`
(*"Monotonically increasing generation number"*). The neural layer currently carries none of it.

### Minimal correct fix

This is **larger than one function**: it is a path-space change plus a reader change plus a
manifest schema bump. It is still narrowly bounded, because the tier path set is a closed constant
(`MRL_TIERS`, `src/search/mrl.rs:25`) and all accessors already funnel through the three path
helpers.

**1. Make every neural path generation-scoped** (the root fix).
Change the three helpers to take a generation token:

- `neural_mmap_embeddings_path(project_path, gen)` → `.leindex/neural_embeddings.{gen}.bin`
- `neural_tier_mmap_embeddings_path(project_path, dim, gen)` → `.leindex/neural_embeddings_{dim}.{gen}.bin`
- the staging path stays `.{name}.staged` (`persistence.rs:591-597`) so in-flight files are
  obviously not servable.

Generation token source: reuse `read_current_generation`'s `u64`, or mint a new one at publish and
record it in the manifest. Do **not** use `identity` — it is stable across exactly the case that
must be distinguished.

**2. Rewrite `commit_neural_generation` (`persistence.rs:519-577`) as:**
stage all tiers (already done, and already re-validated by reopen at `persistence.rs:445-461`) →
`sync_all` each staged file (already happens inside `write_atomic`, `vector.rs:938`) →
**one** `persist_neural_generation_manifest` write naming the exact generation-scoped filenames →
`sync_parent_dir` on `.leindex/` → *only then* delete the previous generation's files.

The commit point becomes a **single atomic rename of one small file**. Everything else is
invisible-until-referenced.

**3. Delete the old generation last, by manifest, not by dimension.** Replace the
`keep: Vec<usize>` dimension list at `persistence.rs:563-568` with "delete every
`neural_embeddings*` file in `.leindex/` whose name the *new* manifest does not name". This is what
makes the crash window harmless instead of merely detected, and it also collects orphaned
generations from repeated crashes instead of accumulating them.

**4. Bump `NEURAL_GENERATION_MANIFEST_SCHEMA_VERSION`** (`persistence.rs:657`) and add the
filename list to the manifest body:

```rust
struct NeuralGenerationManifest {
    schema_version: u32,          // 2
    generation: u64,              // NEW — the identity that was missing
    identity: Option<String>,
    matryoshka: String,
    tiers: Vec<(usize, usize)>,
    files: Vec<(usize, String)>,  // NEW — dim -> exact filename
}
```

The `files` field is what lets a reader open exactly what was committed rather than recomputing
names. Keep `tiers` for the row-count cross-check that already exists.

### Failing test to add

Module: `src/cli/index_builder/persistence.rs` `mod tests` (`persistence.rs:1104`, gated
`#[cfg(all(test, any(feature = "onnx", feature = "remote-embeddings")))]`).

**`test_torn_publish_never_serves_a_mixed_generation`** — the smallest test that fails today:

1. Publish generation A with **two** tiers, both row count `r`, both carrying identity `id`.
2. Simulate a tear: stage a generation B with the **same two dims, the same row count `r`, and the
   same identity `id`**, publish B **only through the canonical/first tier**, and abort before
   `persist_neural_generation_manifest`. (This mirrors the real crash point at
   `persistence.rs:531-539`.)
3. Call `load_neural_tier_mmaps_from_storage(&storage, Some("id"))` and assert that **every loaded
   index is from generation B, or none is** — never a mix.

**Current behaviour — the assertion fails:** the touched tier carries new bytes, the untouched tier
carries A's bytes, identity matches, row count matches, so both load. Assert on bytes, not on
`len()`, because `len()` is equal by construction:
`assert!(loaded.iter().all(|i| i.row_bytes_for("node-1") == b_generation_bytes), "…")`.

**Under the fix** the generation-scoped manifest names only B's files; the querying layer resolves
exactly those; the test passes without needing the crash to be re-runnable.

**Second test (cheap, catches the delete-order bug):**
**`test_generation_scoped_cleanup_keeps_only_the_committed_generation`** — after two complete
publishes, `.leindex/` contains exactly one `neural_embeddings*` file per tier named by the manifest
and nothing else (no `.staged`, no orphan from generation 1).

**Third test (durability, optional but stated):**
**`test_manifest_commit_syncs_the_storage_directory`** — a fix that drops `sync_parent_dir`
should fail.

### Blast radius / risks

**Readers that MUST change.** All tier access is funnelled, so this is a bounded list:

| Site | What it does | Why it must change |
|---|---|---|
| `persistence.rs:792-926` `load_neural_tier_mmaps_from_storage` | iterates `MRL_TIERS` and builds `neural_embeddings_{dim}.bin` (`persistence.rs:845`) | must open the manifest's `files` list instead of recomputing names; the `canonical_dim` duplicate check (`:839-857`) becomes structural |
| `persistence.rs:938-942` `try_load_neural_mmap_embeddings` | wrapper → `_from_storage` | follows the canonical path |
| `persistence.rs:946-972` `try_load_neural_mmap_embeddings_from_storage` | hardcodes `neural_embeddings.bin` (`:949`) | must resolve the generation's canonical filename |
| `src/cli/leindex/indexing/load.rs:194` (`restore_engine_from_snapshot`) | resolves canonical before sidecars | pass the loaded manifest through |
| `src/cli/leindex/indexing/load.rs:550,567` (`restore_persisted_neural_embeddings`) | same | same |
| `src/cli/leindex/indexing/load.rs:799` (`admitted_neural_tier_rows`) | counts reachable rows | same |
| `src/cli/leindex/indexing/neural_publish.rs:745-762` (`restore_neural_checkpoint`) | restores canonical then sidecars | same |
| `src/cli/index_builder/persistence.rs` `query_remote_embedder` (in `load.rs:839`) raw-reads the canonical for identity | identity now lives in the manifest | can read the manifest, simplifying it |

**On-disk format / migration.** Yes, this touches layout:
- `.leindex/neural_embeddings*.bin` → `.leindex/neural_embeddings*.<gen>.bin`.
- `neural_generation.bin` schema `1` → `2`.

Migration story that requires **no destructive sweep**: keep `try_load_neural_generation_manifest`
failing open, and treat *"manifest absent or schema 1"* as **the legacy layout** — resolve paths the
old way (`neural_embeddings.bin`, `neural_embeddings_{dim}.bin`). The next publish writes schema 2
and cleans up the legacy names via the manifest-driven delete. This is the same fail-open pattern
already used at `persistence.rs:723-733` and is strictly additive; no existing store breaks on
upgrade, and a downgrade still reads schema-2 files as legacy (it would simply ignore them, which is
safe because a re-index rebuilds).

**Cross-platform.** `publish_staged_tier` uses `replace_file_atomically` specifically because
Windows cannot `rename` over a mapped file (`vector.rs:952-969`). Generation-scoped names make that
problem *disappear* for the happy path — you write new names, you never replace a live mapped name —
but the delete-old step must tolerate `ERROR_SHARING_VIOLATION` on a still-mapped previous
generation. Do not treat a failed delete as a publish failure: log and let the next publish collect
it. Also note `fragment::atomic_write` removes the target first on Windows (`fragment/mod.rs:223-226`),
which puts a brief window where `neural_generation.bin` does not exist if the manifest is written
through that helper. **Do not write the manifest through `fragment::atomic_write`** unless that
Windows branch is removed; a missing manifest is read as "legacy layout", which would silently
resurrect stale files. This is a real trap the implementer must handle.

**Concurrency.** `persistence.rs:516-518` currently states the requirement in prose: *"A publish must
not run concurrently with a served index for exactly this reason."* The fix removes the need.
No lock file appears to exist today; the fix should not add a new locking requirement.

**Callers of the publish entry point** (all must accept the new generation token):
`persistence.rs:605` (`load.rs` `persist_neural_mmap_preserving_identity`),
`watcher_delta.rs:343-350` (`persist_and_publish_watcher_delta`),
`index_builder_test.rs:1043`, `load.rs:1015`.

**Related bug found while reading — out of scope for #103, record separately.** The second neural
publish path, `src/cli/leindex/indexing/layer_artifacts.rs:122-161` `write_neural_matrix`, writes
`neural_embeddings.bin` (line 156) **without** touching `neural_generation.bin`. It changes the
canonical file's identity and row set while leaving the previous manifest in force — a
manifest/content disagreement that the row-count gate at `persistence.rs:885-896` will then reject,
disabling tiers. It also emits **no tier sidecars at all**, so a generation restored through this
path loses every MRL tier. Any #103 fix has to decide whether this path publishes a manifest or
deliberately invalidates one; today it silently does neither. Flagging as a distinct issue.

### Open questions (unverified in this environment)

- **Is the layer-artifact path reachable in the current product flow?** `write_neural_matrix` is an
  unwrap-from-CAS hydration helper. I did not trace a runtime call site in this session. **To
  confirm:** `leindex_explore(mode="symbol_lookup", symbol="write_neural_matrix", include_callers=true)`.
  Its existence in `indexing/` strongly implies the post-CAS hydration route.
- **Does any deployment already have `.staged` leftovers** from a crashed publish? Those names
  (`.{name}.staged`, `persistence.rs:596`) are not cleaned by `remove_neural_tier_mmaps`, which only
  walks `MRL_TIERS`. A migration should sweep them.
- **Performance:** I did not measure the cost of generation-scoped names (one more file per tier per
  publish, plus a delete pass). It should be negligible against embedding cost, but **I did not
  measure it** — do not claim it is free.

---

## Issue #86 — rollout flags default ON but no production consumer

**Verdict: PARTIAL** — and the issue text is wrong in two of its five specifics.

- `BoundedScheduler`: **CONFIRMED** — default ON, **zero** production consumers. The issue's central
  claim is real for this flag.
- `StreamingScan`, `StreamingParse`, `StreamingTfidf`: **REFUTED as "no consumer"** — all three are
  wired and read on the production indexing path. They are also **default OFF**, not default ON.
- `StreamingNeural`: **REFUTED — the flag does not exist.** It was removed; neural publication is
  always-on streaming.

### Per-flag table (each verified individually)

| Flag | Compiled default | Read in production index path? | Site | Verdict |
|---|---|---|---|---|
| `BoundedScheduler` | **true** (`feature_flags.rs:237`) | **NO** | — | **CONFIRMED dead** |
| `StreamingScan` | **false** (`_ => false`, `feature_flags.rs:248`) | **YES** | selector `streaming/routes.rs:12`; consumer `indexing/mod.rs:793` | wired, opt-in |
| `StreamingParse` | **false** (same) | **YES** | selector `streaming/routes.rs:26`; consumer `indexing/mod.rs:1039` | wired, opt-in |
| `StreamingTfidf` | **false** (same) | **YES** | selector `streaming/routes.rs:40`; consumer `indexing/mod.rs:1375` | wired, opt-in |
| `StreamingNeural` | **n/a — no such variant** | n/a | — | **does not exist** |

### Evidence

**`BoundedScheduler` — default ON, no consumer.**

Declared at `src/feature_flags.rs:77-92`; default-on at `feature_flags.rs:237`:

```
235:             | Self::DaemonClient
236:             | Self::GenerationReaders
237:             | Self::BoundedScheduler
238:             | Self::StreamingPdg
```

The declaration's own doc admits it (`feature_flags.rs:83-91`):

```
 83:     /// **Status:** declared but NOT consumed — blocked, by design. There is no
 84:     /// honest consumer: `IndexJob::step` invokes `PhaseExecutor::run_phase`
 85:     /// wholesale and ignores `WorkBudget`, and the LeIndex pipeline phases
 86:     /// (`run_scan`, `run_parse`, `run_lexical`, `run_neural`) are monolithic
 87:     /// `&mut self` methods — there is no bounded stepping surface to route
 88:     /// through. Wiring this flag requires splitting the phase executor into
 89:     /// stepped jobs first; wrapping whole-phase methods would be decorative,
 90:     /// not bounded or fair.
```

That claim is **verified true**. `src/scheduler/index_job.rs:190`:

```
190:     fn step(&mut self, _budget: WorkBudget) -> anyhow::Result<Step<Self::Progress>> {
...
196:         let items = self
197:             .executor
198:             .run_phase(phase)
```

`_budget` — underscore-prefixed, never read. `WorkBudget`
(`src/scheduler/budget.rs:19-28`) carries `max_input_bytes`/`max_output_bytes`/`max_items`/
`max_cpu_time`; `IndexJob::step` honours **none** of them.

`.env.example` already states the truth:

```
WS5: Route heavy work through the fair bounded scheduler (DRR + admission).
Compiled default ON, but currently declared without a consumer.
# LEINDEX_FEATURE_BOUNDED_SCHEDULER=true
```

A full-repo search for `BoundedScheduler` returns **18 matches in 5 files**, every one of which is
declaration, `env_var`, `default_value`, `description`, `LEGACY_REVERTIBLE_FLAGS`, the `FlagStore`
warm-up list, `all_flags`, or a test. None is a production read. Independently, the production
index path never constructs a `Scheduler`: `src/cli/registry/index_jobs.rs:108-123` is a plain
`tokio::spawn` → `run_owned_index_work`, and the only `Scheduler::` uses in the tree are in
`src/scheduler/mod.rs`'s own tests and `tests/fault/`.

**`StreamingScan` — wired.** Selector `src/cli/leindex/indexing/streaming/routes.rs:11-17`:

```
11: pub(crate) fn scan_route_for_current_flag() -> ScanRoute {
12:     if FeatureFlag::StreamingScan.is_enabled() {
13:         ScanRoute::Streaming
```

Consumer `src/cli/leindex/indexing/mod.rs:793` (inside `run_scan`):

```
793:         let scan_route = streaming::routes::scan_route_for_current_flag();
794:         info!(
795:             "Index scan: {} route selected by LEINDEX_FEATURE_STREAMING_SCAN",
```

**`StreamingParse` — wired.** Selector `routes.rs:25-31`; consumer `mod.rs:1039` (inside `run_parse`):

```
1039:         let parse_route = streaming::routes::parse_route_for_current_flag();
1040:         info!(
1041:             "Index parse: {} route selected by LEINDEX_FEATURE_STREAMING_PARSE",
```

**`StreamingTfidf` — wired.** Selector `routes.rs:39-45`; consumer `mod.rs:1375` (inside
`build_lexical_embedder`):

```
1375:         let tfidf_route = streaming::routes::tfidf_route_for_current_flag();
1376:         info!(
1377:             "Index TF-IDF: {} route selected by LEINDEX_FEATURE_STREAMING_TFIDF",
```

(Note: the doc comments at `feature_flags.rs:121-129` say the consumer is
`src/cli/index_builder/mod.rs` — that's stale; the real consumer is
`src/cli/leindex/indexing/mod.rs:1375`. Documentation drift, not a wiring gap.)

**`StreamingNeural` — does not exist.** `FeatureFlag` (`feature_flags.rs:26-183`) has 23 variants —
count the `env_var` match arms at `feature_flags.rs:188-212`, which are exhaustive with no catch-all,
so the count is not an estimate. There is no `StreamingNeural`. A repo-wide search returns exactly
two hits, both prose, both saying
it was deleted: `BENCHMARKS.md:5` — *"Neural publication was gated by a `StreamingNeural` flag in
this measurement; it has since been folded into the always-on streaming path and the flag
removed"* — and `BENCHMARKS.md:11._Neural_publication_route_A/B_StreamingNeural_gate_removal`
(§258). `src/cli/leindex/indexing/neural_publish.rs` contains **zero** flag reads for neural
routing.

**Defaults, quoted** — `src/feature_flags.rs:221-249`:

```
223:             // Stable production capabilities default ON ...
231:             // Stable architecture capabilities with successful rollout gates
232:             // remain default ON and individually revertible. StreamingScan,
233:             // StreamingParse, and StreamingTfidf intentionally
234:             // remain OFF until their consumers complete a separate rollout.
```

`StreamingScan`/`StreamingParse`/`StreamingTfidf` are **not** in the `true` arm and fall through to
`_ => false` (`feature_flags.rs:248`). They are guarded by explicit tests:
`feature_flags.rs:797-799` (`test_pr86_consumer_flags_default_off`) and
`feature_flags.rs:770-771` (`test_rollout_flags_default_on_after_gates_passed`, which deliberately
lists `BoundedScheduler` but not the streaming three).

### True root cause

Two separate defects, both real, neither fixed by "make the flags do something":

1. **`BoundedScheduler` is a false operational kill-switch.** It is default-ON, in
   `LEGACY_REVERTIBLE_FLAGS` (`feature_flags.rs:377`), and warmed into the `FlagStore`
   (`feature_flags.rs:412`), which makes it appear in any flag enumeration. An operator reading
   `LEINDEX_FEATURE_BOUNDED_SCHEDULER=true` (default) or flipping it to `false` changes **nothing**.
   Worse, its presence in `LEGACY_REVERTIBLE_FLAGS` means `LEINDEX_LEGACY=1` purports to revert a
   behaviour that was never in effect.
2. **`StreamingNeural` is a phantom in the issue tracker, not in the code.** Docs/memory of the
   v2.0 rollout still reference it (`BENCHMARKS.md:5`, `:258`), which is how it ended up in the
   backlog. Nothing to wire.

### Minimal correct fix

Per flag — **not** a uniform answer:

- **`BoundedScheduler`: default it OFF and mark it clearly as not-yet-live.** Do **not** wire it.
  Wiring honestly requires splitting `PhaseExecutor::run_phase` (`scheduler/index_job.rs:95-107`)
  into budget-aware stepped work, which is the real WS5 task and is far larger than this cluster.
  A default-OFF flag that means "this capability is unavailable" is honest; a default-ON flag that
  is inert is a lie. **Also remove it from `LEGACY_REVERTIBLE_FLAGS`** (`feature_flags.rs:374-386`)
  so `LEINDEX_LEGACY=1` stops advertising a revert it cannot perform. Cheapest honest option, and
  it does not touch product behaviour at all.
- **`StreamingScan` / `StreamingParse` / `StreamingTfidf`: no change.** They are correctly wired,
  correctly default-OFF, and covered by both a selector unit test (`routes.rs:51-79`) and an
  end-to-end fixture test (`indexing/tests.rs:163`, `:202`, `:241`). The only edit warranted is
  fixing the stale consumer path in the `StreamingTfidf` doc comment (`feature_flags.rs:121-124`
  says `src/cli/index_builder/mod.rs`; actual is `src/cli/leindex/indexing/mod.rs:1375`).
- **`StreamingNeural`: delete the reference, not the flag.** Add a one-line note where the rollout
  is documented that the flag was folded into the always-on path (already stated at
  `BENCHMARKS.md:5`); ensure `.env.example` (`:110-115`) is the canonical list — it already says
  *"Neural publication always streams."*

### Failing test to add

Module: `src/feature_flags.rs` `mod test`.

**`test_default_on_flags_all_have_a_production_consumer`** — the structural test that makes the
whole class non-recurring. Maintain an explicit, reviewed allowlist of flags that are deliberately
declared-but-unconsumed, and assert:

1. every flag with `default_value() == true` is **not** in the allowlist, and
2. every flag in the allowlist has `default_value() == false`.

Today it **fails** on `BoundedScheduler` (default `true`, no consumer). It passes after the flag is
defaulted OFF and added to the allowlist. The allowlist is acceptable because such flags must be
named out loud in review — the bug is not "a flag with no consumer", it is "a flag that *claims* to
be on".

Cheap companion: **`test_bounded_scheduler_is_not_legacy_revertible`**, asserting
`is_legacy_revertible(&FeatureFlag::BoundedScheduler) == false`, which fails today
(`feature_flags.rs:377`).

Do **not** write a test asserting "every flag has a consumer" — that is unimplementable and would be
gamed.

### Blast radius / risks

- Changing `BoundedScheduler`'s default can only affect code that reads it — and by this analysis
  **nothing does**, so product risk is zero. The visible behavioural change is that
  `LEINDEX_LEGACY=1` no longer lists it as reverted.
- Tests to update, all of which currently assert the old default:
  `src/feature_flags.rs:770-771` (`test_rollout_flags_default_on_after_gates_passed`),
  `:843` and `:871` (`test_rollout_flags_default_off_with_legacy_mode`),
  `:915` (`test_legacy_unset_keeps_compiled_defaults`), and
  `tests/gate_audit_test.rs:234` (`test_gate_phase8_all_flags_default_on`).
  One of them presumably encodes "phase 8 = everything on", so the implementer must decide whether
  phase 8 is being retracted for this flag. **This is a policy call, not a code call** — surface it
  rather than quietly editing the assertions.
- `.env.example:110-115` verbatim-lists the defaults; keep it in sync (repo hygiene rule in
  `AGENTS.md`).
- References inside `src/cli/leindex/indexing/tests.rs:167,206,245,287` iterate
  `[StreamingScan, StreamingParse, StreamingTfidf, StreamingPdg, BoundedScheduler]` to force flags
  OFF for legacy-route fixtures. Those keep working regardless of the default flip.

### Open questions

- Whether `BoundedScheduler` should be **deleted** rather than defaulted OFF. Deletion is defensible
  under YAGNI, but it is referenced from `.env.example` and five test lists, and WS5 is a real
  roadmap item. Defaulting OFF + allowlisting is the smaller correct diff; deletion is the cleaner
  end state once the implementer confirms WS5 is not imminent. **I did not find a roadmap file that
  resolves this**, so I am not asserting one answer.
- `feature_flags.rs:371-373` says GA flags are excluded from `LEGACY_REVERTIBLE_FLAGS`, but its
  comment lists `StreamingMcp`/`GlobalAutoSync` while `StreamingScan` etc. are also absent. That's
  fine, just noting the comment enumerates a subset.

---

## Issue #87 — lizard CCN scoring regression under 1.24.1

**Verdict: REFUTED as filed.** The gate is **already pinned**, on this branch, to the calibrated
version. The reported mechanism (unpinned install picks up 1.24.1) is not what the workflow does.
What remains is a genuine but different, smaller gap: **supply-chain pinning and a missing
calibration record**, not a version-drift bug and certainly not a product-code problem.

### Evidence

`.github/workflows/quality.yml:110-126`:

```
110:   cyclomatic-complexity:
111:     name: Cyclomatic Complexity Check
...
118:       - name: Install lizard
119:         # PINNED: lizard 1.24 changed the Rust CCN scoring (closures/?
120:         # handling), flagging ~43 pre-existing functions repo-wide the day
121:         # it shipped — an unpinned gate that moves under the code is not a
122:         # gate. 1.23.0 is the version this tree's thresholds were calibrated
123:         # and reviewed against; the 1.24.1 finding list is tracked in the
124:         # follow-up issue before any bump (AGENTS.md: minimum 3-day release
125:         # age + explicit recalibration).
126:         run: pip install lizard==1.23.0 jinja2  # jinja2 required for lizard's HTML report (-o)
```

The gate itself, `quality.yml:141-148`:

```
141:           lizard \
142:             src/ \
143:             -C 15 \
144:             -x"*/tests/*" \
145:             -x"*/target/*" \
146:             -m \
147:             -i 0 \
148:             || (echo "::error::Cyclomatic complexity threshold exceeded (CCN > 15)" && exit 1)
```

Note `-i 0` makes lizard exit non-zero on any function above `-C 15`; the `-o`/`-m` report run at
`quality.yml:132-138` is separate because `-o` always exits 0.

**Version-drift mechanism: confirmed real, now closed.** `git log -S 'lizard==1.23.0'` yields exactly
one commit:

```
0f8b02d4 ci: pin lizard to 1.23.0 — 1.24.1 changed Rust CCN scoring repo-wide
```

`git merge-base --is-ancestor 0f8b02d4 HEAD` → **ancestor of `f31f107e`**. So on this branch the pin
is live. The commit message and the inline comment independently corroborate the reported mechanism
(unpinned `pip install lizard` sliding to 1.24.1 and re-flagging ~43 previously-passing functions),
which is why I record it as **historically confirmed** even though the current state refutes the
"is unpinned" claim.

**Scope check — exactly one install site:**

- `git grep -n 'lizard=='` → one hit, `quality.yml:126`.
- `git grep -n 'pip install lizard'` → one hit, the same line.
- `git grep -ni 'lizard|cyclomatic' :!.github` → no other *gate*; hits are prose in
  `brag-output/multi-client-deadlock-report.md:86,103`, `docs/API.md:816`, `docs/MCP.md:1974`,
  a baseline transcript, and `cyclomatic_complexity` **product fields** in
  `src/parse`/`src/cli/`/`src/server` (unrelated to the CI gate — that is LeIndex's own CCN metric).
- `git ls-files | grep -E 'requirements|constraints'` → **empty**. There is no requirements file and
  therefore no lockfile and no hash.

### Threshold or parser — which is wrong?

**Neither. The gate is right; the version pin is the control.** Stated explicitly:

> **Do not refactor product code to satisfy a changed parser.** If lizard 1.24.1 reclassifies
> closures or `?` handling and thereby reports a higher CCN for an unchanged Rust function, that is
> a *measurement* change, not a complexity regression. Rewriting working code to make a changed
> tool report the old number would (a) reward a tool upgrade with unnecessary churn, (b) invalidate
> every prior calibration review, and (c) leave the tree calibrated against a metric nobody has
> agreed to. The code under `src/parse`, `src/cli/mcp`, and `src/server` is **not** the thing to
> change here.

The `-C 15` threshold is likewise not the thing to move: the workflow's comment
(`quality.yml:122-123`) records that this tree's thresholds were *"calibrated and reviewed against"*
1.23.0, so changing the threshold silently decouples it from that review.

The corollary is that a lizard bump is a **deliberate, reviewed recalibration**, which is exactly
what `AGENTS.md`'s 3-day-minimum-release-age rule is for and what `quality.yml:123-125` already
promises.

### Minimal correct fix

Keep the pin. Close the two real gaps:

**1. Make the pin tamper-evident (the actual remaining hole).** `lizard==1.23.0` pins the version
but not the artifact. There is no hash, no constraints file, no lock. Supply-chain-wise that is a
version pin, not a content pin. Smallest correct change:

- Add a committed `requirements-ci.txt` containing `lizard==1.23.0` together with
  `--require-hashes`-style pinned hashes for the wheel, and change `quality.yml:126` to
  `pip install -r requirements-ci.txt jinja2`. (Also pin `jinja2`, which is currently floating —
  same class of hole, and it is only needed for the HTML report.)
- Alternatively, and cheaper: `pip install --no-deps lizard==1.23.0` to stop transitively floating
  dependencies. Either is defensible; the hash form is strictly better.

**2. Record the calibration baseline so the gate is auditable.** The comment says thresholds were
"calibrated and reviewed" against 1.23.0 but the *number* the calibration produced is nowhere. Add a
short `## Complexity gate calibration` section (in `AGENTS.md` or next to the workflow) stating:
tool version `1.23.0`, invocation exactly as in `quality.yml:132-148` (flags included — `-x` globs
and `-i 0` are part of the calibration, not decoration), threshold `CCN > 15`, the observable pass
condition (`lizard … -i 0` exits 0), and the bump procedure: run the new version, capture the
finding delta, justify or fix each *newly* flagged function, then update the pin and the rationale in
one commit.

**3. Keep the 1.24.1 finding list attached to the pin.** `quality.yml:123-124` says the 1.24.1
list "is tracked in the follow-up issue". Linking that issue id in the comment (or attaching the
list) turns the promise into something a future maintainer can act on. Nothing in the workflow
currently names it.

### Failing test to add

This is a CI-configuration issue, so a Rust "failing test" would be theatre. The honest check is a
**CI-invariant job** (a few lines of shell, run in `quality.yml`, no product code touched):

**`test_lizard_pin_is_exact`** — assert the install line pins a full `X.Y.Z`:

```
grep -qE 'pip install[^#]*lizard==[0-9]+\.[0-9]+\.[0-9]+' .github/workflows/quality.yml
```

Today this **passes** (`lizard==1.23.0`), so it is a **regression guard, not a failing test** — say
so plainly rather than inventing a red test. If the implementer adds the hash/requirements form, the
guard becomes "the requirements file exists and pins lizard exactly, and the workflow installs from
it".

If a Rust-side gate is preferred, the equivalent is a small test in the workspace that reads
`.github/workflows/quality.yml` as a string and asserts the exact pin — same assertion, colocated
with the build. That **would** have failed before `0f8b02d4`, which is the closest thing to a
reproducer this issue has.

### Blast radius / risks

- **Zero product risk.** No file under `src/`, `tests/`, `benches/`, or `Cargo.toml` needs to
  change. If an implementation agent proposes touching `src/parse`, `src/cli/mcp`, or `src/server`
  to satisfy lizard, that is a misread of this issue.
- **Pin bumps are the only future risk.** When lizard is bumped, the ~43-function delta will
  reappear; that is a *reviewed* event, not a surprise, provided the calibration record exists.
- **Windows/CI parity:** the gate runs on `ubuntu-latest` with Python `3.12` (`quality.yml:112,
  116-117`). `lizard` is pure Python, so no platform-specific scoring divergence is expected — but
  **I did not verify** that 1.23.0's Rust CCN scoring is identical across platforms, because I have
  no second runner here. Low risk, stated rather than assumed.
- Local note: the repo's default cargo toolchain does not have the components needed for the usual
  validation commands; that is unrelated to this issue but will bite whoever touches CI next.

### Open questions

- **Where is the "follow-up issue" that tracks the 1.24.1 finding list?** `quality.yml:123-124`
  references it by description only, no id. **To confirm:** search the issue tracker for the lizard
  1.24.1 findings. Until that list is attached, "1.23.0 is the calibrated version" is an assertion
  the repository cannot substantiate from its own contents.
- **Is `jinja2` needed at all?** It exists solely for `-o ./complexity-report.html`
  (`quality.yml:126` comment, `:132-138`). If the HTML artifact is not actually consumed, dropping
  it removes a floating dependency. Out of scope to decide here.

---

## Cross-cutting invariant

The three issues look unrelated but share one failure mode: **a state marker that is asserted but
not load-bearing.**

- **#103** — `neural_generation.bin` asserts "this is generation G" but cannot express *which files*
  are G. The marker names dimensions and row counts; the path space is generation-blind. So the
  marker can only *detect* a tear when cardinality changes, and is silent otherwise
  (`persistence.rs:509-513`). The path space is the state; the marker is a comment about it.
- **#86** — `LEINDEX_FEATURE_BOUNDED_SCHEDULER=true` asserts "the bounded scheduler is active". No
  code reads it. The flag is a comment about a state that does not exist.
- **#87** — `lizard -C 15` asserts "this tree's complexity is under the reviewed threshold". Before
  `0f8b02d4` the number it enforced depended on whatever PyPI served that morning. The gate was a
  comment about itself.

**The invariant to formalize: every persisted or operator-visible state indicator must be derived
from, and read back against, the state it claims to describe — or it must be deleted. A marker that
cannot be falsified by reading the state is documentation, not a control.**

Where it should live:

1. **Storage-layer rule**, next to the schema-version discipline already stated at
   `src/cli/index_builder/fragment/mod.rs:194-197`: *a manifest names its artifacts by exact path,
   never by a name derived at read time from a constant.* This is the rule #103 violates and the
   rule that would have prevented it. The same discipline already exists for the CAS generation
   layout (`src/storage/generation/manifest.rs` maps `LayerKind` → blob hash) — the neural tier
   layer simply wasn't brought along.
2. **Flag-layer rule**, as an executable check rather than prose: the allowlist test described in
   #86 (`test_default_on_flags_all_have_a_production_consumer`). A default-ON flag is a claim of
   operational safety; make the claim cost a test.
3. **Gate-layer rule**, in `AGENTS.md`: a CI gate that fails the build must pin the *tool artifact*
   (version + hash) and record the *calibration* (invocation, threshold, baseline). A gate whose
   verdict is not reproducible is not a gate.

A single sentence to put in `AGENTS.md`: **"A control (manifest, flag, or gate) must be derivable
from and checkable against the state it governs; if it cannot fail when the state is wrong, it is
not a control."**
