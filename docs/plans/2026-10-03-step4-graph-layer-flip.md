# Step 4 — Full flip: the PDG layer becomes the sole graph store (Option B)

Status: spec for implementation. Owner: implementing agent. Reviewer: orchestrator.
Everything here is binding. Where the spec says "verify", verify — do not trust prose.

## 0. State you inherit (uncommitted working tree — do NOT stash/commit/reset)

Branch v2.0.0, repo /mnt/WD-SSD/Prod/LeIndex. The working tree carries uncommitted
steps 1–3 plus fixes; build on top, never revert:

- `src/storage/generation/reader.rs`: PDG1 **v2** lossless records — `PDG_NODE_V2_LEN`
  (9×u32/node), `PDG_EDGE_META_LEN` (5×u32/edge), `PDG_V2_NONE`/NaN sentinels,
  `PdgReader::node_full/edge_meta/version`, v1-vs-v2 dispatch (v1 accessor refuses v2).
- `src/storage/generation/migrate.rs`: `encode_pdg_layer_v2(&Connection)` (catalog →
  v2 payload; storage-vocabulary type codes, unknown type = hard error),
  `encode_tfidf_layer` (dense LIEE → sparse triples), `encode_neural_layer`
  (neural_embeddings.bin → NRL1 v2 + node-id table), `load_node_id_map`,
  fragment bundle `encode_fragment_bundle` (`LIDX-FRG1`).
- `src/storage/generation/manifest.rs` + `writer.rs`: optional `LayerKind`s
  Search/Embedder/Fragments (validate requires only the 5 core; `layer_hashes`
  includes optionals so leases/GC cover them).
- `src/cli/leindex/indexing/mod.rs`: `stage_generation_layers` (CAS-stages all
  layers; today the Pdg layer is encoded **from the SQL catalog** — this spec
  changes that), `promote_generation_snapshot` now calls `writer.publish` —
  GenerationWriter is the single publication authority (manifest, then atomic
  CURRENT swap last).
- `src/search/engram.rs`: process-global `NEXT_STAGING_SEQ` (staging-name
  collision fix).
- Unrelated approved fixes also in tree (feature_flags docs, phase/context
  deletions-only persist, schema backfill hard-error). Leave untouched.

Handoff validation status: fmt clean, clippy 0 warnings,
`cargo test --workspace --exclude memcheck` = 2578 passed / 0 failed.

### 0.1 Partial work from a prior agent (verify, then continue — tree does NOT compile)

A previous agent began P0–P1 and stopped on tool failures without any verification:

- `docs/plans/...step4...md` — P0 inventory table appended (review it, extend if stale).
- `src/storage/generation/graph_codec.rs` — NEW, 321 lines, exists but is
  **unintegrated and unverified** (not referenced by any module tree yet).
  Read it fully before trusting it; fix or rewrite as needed.
- `src/graph/pdg.rs` — `precision_symbols` field made private (D2 half-applied):
  **no accessor was added**, so 11 call sites fail E0616 (diagnostics.rs,
  intel/merge.rs, phase/context.rs, pdg_store_test.rs, pdg_test.rs, …).
  `grep -rn precision_symbols src/` to enumerate.
- `migrate.rs` — `StringInterner` visibility changed to `pub(super)` (graph_codec
  imports it; keep or adjust).
- Compile state: `cargo check --lib --tests` fails with 11 E0616 errors.

**First actions, in order:** (1) add the D2 accessor
(`pub fn precision_symbols(&self) -> &HashSet<String>`, plus a `Clear`-style or
mutating accessor ONLY if a non-test caller needs one — tests may keep direct
access via the accessor; `pdg_store_test.rs`'s `.clear()` needs a mutation path),
update all 11 sites; (2) `CARGO_TARGET_DIR=/tmp/ldx-target cargo check --lib
--tests` green; (3) wire `graph_codec.rs` into the module tree; (4) THEN resume
P1 verification of the codec itself (round-trip tests D1) before D3.

## 1. Objective

1. **Write**: the in-memory `ProgramDependenceGraph` persists ONLY as the PDG1 v2
   CAS layer. No `intel_nodes`/`intel_edges` row writes anywhere on the save path.
2. **Read**: hydration builds the graph from the layer; SQL `load_pdg` remains ONLY
   as the legacy-store fallback (no manifest / generation-readers off).
3. Every SQL graph-query site (28 known, inventory in §6) migrates to the
   in-memory/layer-backed graph.
4. Dead write machinery is deleted together with its tests. No `#[allow(dead_code)]`,
   no skips, no weakened assertions (repo zero-tolerance policy, AGENTS.md).

## 2. Format freedom (important)

PDG1 v2 is **unreleased** — no v2 blob exists outside this branch. You may extend
the v2 record layout (e.g. add an interned `qualified_name` column if a consumer
needs it) without a version bump. v1 read compatibility must remain (v1 blobs are
in the wild); v1 stays read-only.

## 3. Hard design decisions

### D1 — New codec module `src/storage/generation/graph_codec.rs`

- `encode_pdg_v2_from_graph(&ProgramDependenceGraph) -> Result<Vec<u8>>`:
  iterates graph nodes, interns `node.id`, `name`, `file_path`, `language`
  (+`qualified_name` only if D6 shows a consumer), type code = the same storage
  vocabulary as `migrate.rs::node_type_code_v2` (share it, don't duplicate),
  `complexity`/`byte_range`/precision-flag from the Node + the graph's precision
  set. Duplicate `node.id` → last row wins (SQL upsert parity; verify with a test).
  Edge endpoints reference nodes by interned `node.id`; metadata Option sentinels
  exactly as the catalog encoder does.
- Decode: `PdgReader::to_program_dependence_graph(&self)` (or free fn in the same
  module) — `add_node_without_trigrams` per node, map interned-id → petgraph
  `NodeId` via the node-id string, add edges with metadata, `mark_precision_symbol`
  for flagged nodes, `rebuild_trigram_index()` at the end. Unknown type code = error.
- Tests: encode → CAS blob (`cas::blob::encode_blob`) → `PdgReader` → decode →
  compare against the original with the `canonical_pdg` helper (currently in
  `src/cli/leindex/generation_read_test.rs` — move it to a shared `#[cfg(test)]`
  module, don't duplicate). Cover: every Node field, all-metadata-present edge,
  all-absent edge, confidence NaN sentinel, precision markers, duplicate-id
  last-wins, unknown-type error.

### D2 — Precision accessor

Add a public read accessor for the precision set on `ProgramDependenceGraph`
(`pub fn precision_symbols(&self) -> &HashSet<String>` or an iterator). No
`pub(crate)` field poke, no making the field pub.

### D3 — Publish encodes from memory

`stage_generation_layers` stops encoding the Pdg layer from the catalog
(`encode_pdg_layer_v2(&conn)` is removed from this path) and uses D1 with the
in-memory graph, threaded from pipeline state through `publish_generation_snapshot`
(it can reach `self.pipeline`/state.pdg — thread `&ProgramDependenceGraph` down).
The Symbols layer encoder also reads catalog rows today — after the flip the
catalog has none, so encode Symbols from the in-memory graph too (it only needs
name/type/file/complexity, all on Node). Migration keeps its catalog-based
encoders unchanged (legacy catalogs DO have rows).

### D4 — Hydration flip

When a generation manifest + Pdg layer exists (generation-readers path in
`try_hydrate_from_generation_inner` and `load_from_storage_inner_at`), build the
graph via D1 decode instead of `load_pdg`. No manifest / flag off → existing SQL
path unchanged. Parity test: index the fixture project, then
`canonical_pdg(layer-hydrated instance) == canonical_pdg(sql-hydrated instance)`
using the flag-override pattern in `test_read_path_bit_for_bit_equivalence`.

### D5 — Write flip (audit every side effect at every call site)

- `index_builder::save_to_storage` stops calling `pdg_store::save_pdg`. Callers:
  `src/cli/leindex/indexing/mod.rs` (~persist phase), `watcher_delta.rs` (~322),
  `src/phase/context.rs` (several — see precision durability below).
- Graph-pruning calls (`delete_file_data*` graph arms) go away where no rows
  exist; `indexed_files` deletions/upserts STAY (they are the freshness
  mechanism and are independent of graph rows).
- `save_trigram_index*` calls: drop, but first verify `load_pdg`'s fallback
  rebuilds when the row is absent (it has a `_ => rebuild` arm — confirm).
- Audit every `persisted_*` helper / fingerprint that reads intel tables
  "from storage AFTER save" (e.g. `persisted_search_identity`,
  `pdg_search_fingerprint` call sites in `index_builder/hybrid.rs`,
  `watcher_delta.rs`): switch to the in-memory graph.
- `pdg_store::pdg_exists` and any freshness helper counting `intel_nodes`:
  semantics must move to `indexed_files` / manifest presence — verify what each
  caller actually needs.
- Precision durability (phase context): precision markers are now layer-only.
  Phase enrichment must persist by republishing the graph through the generation
  publish path (stage + publish a new generation with the re-encoded graph). If
  that proves structurally impossible without damaging the design, STOP that
  sub-item per §8 and report — do not half-flip.

### D6 — Query-site migration (inventory first; see §6)

- Replace SQL graph reads with queries over the hydrated in-memory PDG. Daemon
  handlers run in a process holding hydrated `LeIndex` instances — wire the
  registry access rather than re-hydrating per request.
- Node-type strings map through the same storage `NodeType` vocabulary (share
  the mapping with D1; unknown = error).
- `community_store`: membership tables are derived data and may remain SQL, but
  node-membership validation joins must use the in-memory node set.
- `salsa.rs`: reproduce the SQL's recursive semantics exactly against the
  in-memory graph; leave a comment stating the replaced query and its traversal
  equivalent.
- `server.rs` cross-project sync (`INSERT OR IGNORE … SELECT` across ATTACHed
  DBs): per-project stores no longer carry rows, so feed the cross-project
  store by materializing the source project's graph from its in-memory/layer
  graph via batched INSERT. The cross-project store keeps its SQL shape — it is
  an aggregation cache, not a generation store. The `COUNT/MAX(precision)` probe
  at server.rs:~418 → in-memory equivalents.

### D7 — Deletions

After D5, delete unreferenced write machinery in `pdg_store.rs`
(`save_pdg` chain, node/edge upsert + diff + bulk-delete helpers, the sqlite3
trace harness tests that exist solely to count write statements). Load-side code
stays (legacy fallback). Prove zero dead code via clippy — no `#[allow]`.

## 4. Sequencing (every phase ends compiling, fmt+clippy clean, tests green)

- **P0** Inventory (§6 grep, incl. tests): classify every site
  read/write/cross-project/derived; append the table to this file.
- **P1** Codec D1–D3 + tests.
- **P2** Hydration D4 + parity test.
- **P3** Write flip D5 + e2e proof (§5.4).
- **P4** Query sites D6.
- **P5** Deletions D7.
- **P6** Full validation gate (§5).

## 5. Validation gate (must pass before reporting done)

1. `cargo fmt --all` then `cargo fmt --all --check`.
2. `CARGO_TARGET_DIR=/tmp/ldx-target cargo clippy --workspace --all-targets -- -D warnings`.
3. `CARGO_TARGET_DIR=/tmp/ldx-target cargo test --workspace --exclude memcheck`.
4. Release e2e: `CARGO_TARGET_DIR=/tmp/ldx-target-rel cargo build --release --bin leindex`;
   copy the repo's tracked files to a scratch dir (`git ls-files -z | xargs -0 -I{}
   cp --parents {} /tmp/<scratch>/`), run `HOME=/tmp/ldx-home …/leindex index
   <scratch>` → exit 0; `generations/<N>/manifest` exists; `CURRENT` set;
   `retention --report` parses; `search "generation manifest"` returns results;
   `sqlite3 <scratch>/.leindex/generations/<N>/leindex.db "select count(*) from
   intel_nodes"` == **0**; `select count(*) from indexed_files` > 0; a second
   `index` run is a no-op (incremental freshness intact).
5. Parity (§ D4) and codec round-trip tests green in the suite.
6. Collect numbers for the later timing gate: total index wall-clock, publish
   stage wall-clock, and confirm no `save_pdg diff summary` line appears in the
   log. Put the numbers in the final report.

## 6. P0 inventory seed (verify — do not trust)

```
grep -rn "FROM intel_nodes\|FROM intel_edges\|INTO intel" --include="*.rs" src/
```
### Verified P0 inventory (2026-10-03)

The source-level search was run for `FROM intel_nodes`, `FROM intel_edges`, and
`INTO intel` in `src/`, then broadened to `intel_nodes|intel_edges` to catch
JOIN/UPDATE/DELETE and dynamic/schema/test hits. Counts below are logical
production query/write sites, not token occurrences (one SQL statement can
mention a table more than once). `src/storage/generation/load.rs` named in the
handoff does not exist; the corresponding generation hydration logic is in
`src/cli/leindex/indexing/load.rs` and `indexing/mod.rs`.

| File / site (verified function or SQL block) | Kind | Classification / disposition |
| --- | --- | --- |
| `storage/catalog.rs::find_symbol` | read | D6 graph query; resolve from hydrated PDG, preserve two project IDs, exact/case-insensitive ordering and 200-row cap. |
| `storage/catalog.rs::find_symbols_matching` | read | D6 graph query; preserve literal substring matching, priority ordering and cap. |
| `storage/catalog.rs::symbols_in_file` | read | D6 graph query; preserve file filter/range ordering and cap. |
| `storage/catalog.rs::count_symbols_in_file` | read | D6 graph query; count matching PDG nodes without a row cap. |
| `storage/analytics.rs::count_nodes_by_type` | read | D6; group resident PDG node types using storage vocabulary. |
| `storage/analytics.rs::complexity_distribution` | read | D6; preserve SQL CASE buckets and ordering. |
| `storage/analytics.rs::count_edges_by_type` | read | D6; group resident PDG edges. |
| `storage/analytics.rs::get_hotspots` | read | D6; preserve complexity threshold, caller fan-out and sort semantics. |
| `storage/salsa.rs::QueryInvalidation::get_affected_nodes` | read | D6; replace file-node hash query with graph-derived equivalent; retain `analysis_cache` SQL. |
| `storage/community_store.rs::apply_assignments` | derived read/write | Keep community labels/telemetry SQL; move membership assignment storage off `intel_nodes` to derived membership records; validate against in-memory node IDs. |
| `storage/community_store.rs::save_communities_by_node_id` | derived read/write | Replace `intel_nodes` ID lookup with the passed PDG node set; persist derived membership/labels only. |
| `storage/community_store.rs::load_community_memberships` | derived read | Hydrate membership into PDG from derived membership store, intersecting with current node IDs. |
| `server/handlers.rs::list_codebases` | read | D6 aggregate: node/edge counts come from registered in-memory projects; file counts/metadata remain SQL. |
| `server/handlers.rs::get_codebase` | read | Same counts for one project; metadata remains SQL. |
| `server/handlers.rs::query_codebase_metrics` | read | Node/edge/import counts from each resident PDG; external refs/dependency links and indexed files remain SQL. |
| `server/handlers.rs::query_language_distribution` | read | D6 aggregate across resident PDGs. |
| `server/handlers.rs::dashboard_overview` import-edge count | read | D6 graph edge count filtered to Import; unrelated telemetry stays SQL. |
| `server/handlers.rs::query_graph_nodes` | read | D6; preserve project selection and 1000-node bound. |
| `server/handlers.rs::query_graph_links` | read | D6; preserve project endpoint selection and 5000-edge bound. |
| `server/server.rs::ingest_project_db` attached-DB graph transfer | cross-project | Per-project generation DBs no longer own graph rows; materialize source layer/PDG into the server aggregation store in batches. Other project metadata/indexed-files/global-symbol/reference/dependency copies remain SQL. |
| `server/server.rs` startup `COUNT(*) / MAX(precision)` probe | cross-project | Replace with hydrated source PDG node count and precision marker count. |
| `cli/mcp/project_map_handler.rs::community_grouped_response` | derived read | Read file/community mapping from the resident PDG community map; labels remain in `intel_communities`. |
| `cli/textindex.rs::symbols_from_db` | read | D6 symbol-span source; use the Symbols generation layer (including byte ranges) instead of catalog graph rows. Its test fixture SQL is setup only. |
| `cli/leindex/diagnostics.rs::collect_precision_diagnostics` | read/fallback | Resident-PDG branch is already authoritative; replace one-shot SQL count/language fallback with layer hydration (do not silently return zero on a missing/corrupt layer). |
| `storage/pdg_store.rs::load_pdg`, `load_nodes`, `read_edges` | legacy read | Retain only as legacy-store fallback when generation readers are disabled or no manifest exists; generation hydration must not call it when a Pdg layer is available. |
| `storage/pdg_store.rs::save_pdg` chain, node/edge diff/upsert/delete, trigram SQL save | write | D5/D7: remove from the save path and delete unreferenced machinery/tests. Keep indexed-files CRUD. |
| `storage/pdg_store.rs::pdg_exists` and file/project graph deletes | read/write helper | Replace graph presence with `indexed_files`/manifest semantics; delete graph-delete calls, retain indexed-file freshness operations. |
| `storage/generation/migrate.rs` catalog encoders and `load_node_id_map` | migration read | Intentionally unchanged: legacy migration input is a catalog that still contains graph rows. |
| `storage/schema.rs` table definitions, schema backfills and schema migration tests | schema/migration | Intentionally retained for opening/upgrading legacy stores; not a production graph save/query path. |
| `storage/nodes.rs`, `storage/edges.rs`, `storage/pdg_store_test.rs`, `indexing/tests.rs`, `community_store.rs` tests, `server/server.rs` tests | tests/setup | Fixtures/assertions or legacy bridge tests; classify individually during D5/D7. Remove write-only trace-harness tests with the writer; do not remove schema/migration/derived-data coverage. |

Verification also confirmed `load_pdg` currently rebuilds trigrams when no
persisted index exists (`pdg_store.rs::load_pdg`, `_ =>
pdg.rebuild_trigram_index()`), so dropping trigram persistence does not lose
fuzzy-search correctness. The exact SQL token count exceeds the spec's rough
"~28" because JOIN predicates, query copies, and test/schema SQL produce
multiple matches per logical site; the table classifies production sites and
non-query exceptions rather than treating the rough count as a gate.

## 7. Landmines (hard-won; obey)

- **Never use the repo's own `target/`** — it holds artifacts from an
  incompatible rustc and fails with E0514. Always
  `CARGO_TARGET_DIR=/tmp/ldx-target` (warm dev cache) and
  `CARGO_TARGET_DIR=/tmp/ldx-target-rel` (release). 
- Workspace tests ≈ 3–4 min; release build ≈ 2 min — detach long commands.
- The engram concurrency test was flaky before the in-tree fix; if it fails,
  re-run once before diagnosing.
- Never run the binary against the real repo's `.leindex` — always a scratch copy.
- `sqlite3` CLI is available for row assertions. `memcheck` suite is out of scope.
- Do not commit, stash, or reset. Final state = diff on top of HEAD.

## 8. Escalation

If any D-decision proves impossible without breaking documented behavior
(especially D5 precision durability or D6 cross-project), STOP that sub-item,
keep everything else green, and report the blocker precisely with the code
reference. Never half-implement, never weaken a test to pass.

## Out of scope

memcheck acceptance gate; remote/Turso storage; enforcing the step-5 timing
threshold (collect numbers only); releasing or version-bumping anything.
