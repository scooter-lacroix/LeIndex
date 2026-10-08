# PR #86 — Wire the five unconsumed feature flags (consumer work)

Status: spec for implementation. Owner: implementing agent. Reviewer: orchestrator.
Binding. Where it says verify, verify — line references are approximate; re-check with search.

## 0. Base and state

Branch `feat/flag-consumers` at `bed53e82` (= origin/master, post-PR-85). Working tree
clean. PR #85 merged the storage flip; the five flags below were declared then never
consumed — their annotations say so and point at issue #86. THIS PR is that consumer
work; the annotations get updated as each flag gains a consumer.

The five flags (src/feature_flags.rs ~:84-116; env names :165-170):
`BoundedScheduler`, `StreamingScan`, `StreamingParse`, `StreamingTfidf`, `StreamingNeural`.

Existing pattern to replicate — `StreamingPdg`:
- `pdg_route_for_current_flag()` (indexing/mod.rs ~:1635) reads the flag → returns
  `PdgBuildRoute::{Streaming,Legacy}`.
- The phase consults it at exactly one dispatch point (`run_pdg` →
  `build_changed_file_pdg`), logs the chosen route, and both routes are
  exercised by tests (see `use_streaming` handling in indexing/mod.rs and
  streaming tests).

Streaming stage implementations EXIST and are tested (WS6-9), currently uncalled
from the pipeline: `src/cli/leindex/indexing/streaming/{scan,parse,tfidf,neural}.rs`
(entry fns: `stream_scan`, `stream_parse` + `chunk_scan_records`, `streaming_tfidf`,
`enrich_neural_streaming` + `batch_inputs`). The job is wiring, not building.

## 1. Design decisions (all binding)

### D1 — One dispatch point per phase, flag-defaults preserved
Each of `run_scan` / `run_parse` / `run_lexical` / `run_neural` gains a
`*_route_for_current_flag()` style helper (or equivalent inline consult) and a single
branch. Defaults MUST remain as today (all five flags currently default OFF → legacy
paths stay default). Each dispatch logs which route ran (`info!` with the flag name).
No behavior change with flags off — byte-for-byte pipeline behavior, existing tests
prove it.

### D2 — StreamingScan consumer
`stream_scan` produces `ScanRecord`s (path/hash/size/lang/mtime) via a
`ScanRecordWriter`. `run_scan`'s current path builds `source_files_with_hashes`
(Vec<(PathBuf, String)>) via `collect_source_files_with_hashes` (which also caches
bodies when asked). Wire: when the flag is on, drive `stream_scan` and bridge its
records into the same downstream shape the phase expects (`scan_checkpoint` needs
path+hash pairs). Do NOT retain source bodies (VAL-STREAM-012) — the streaming
route must set the body-caching argument false. Note `collect_source_files_with_hashes`
currently returns Result<Vec<...>> and fail-fasts on unreadable files — the streaming
route must preserve that fail-fast semantic (an unreadable file aborts the run, it is
never silently skipped; ScanStats.errors exists but the pipeline route must still
propagate hard IO errors — decide and document: hard errors abort, only
transient/partial-read tolerance may count).

### D3 — StreamingParse consumer
`stream_parse` is chunked by `ParseBudget` (default 50 files / 10MiB per chunk).
Current `run_parse` parses via `crate::parse::parallel` (rayon, returns
`ParsingResult`s with signatures + source_bytes). The PDG phase CONSUMES those
ParsingResults (`build_changed_file_pdg(parsing_results, use_streaming)`), so the
streaming parse route must either produce the same `ParsingResult` shape (preferred —
keeps `build_changed_file_pdg` untouched) or the PDG route must accept the streaming
records; prefer the former. Preserve the phase's checkpoint/resume contract
(`resumed_parse`, scan-hash filter) — the streaming route writes the same checkpoint
artifacts the resume logic reads.

### D4 — StreamingTfidf consumer
`run_lexical` currently builds TF-IDF via `index_nodes_tfidf_only` (two-pass streaming
vocab per the A+ log line). `streaming_tfidf` (freeze_vocab_idf → compute_tfidf_row)
is the WS6-9 route. Wire as the flag-on route inside `build_lexical_embedder` /
`run_lexical`, producing the same artifacts the snapshot/embedder persistence expects
(dense 768-d vectors keyed by node id — see `encode_tfidf_layer` in migrate.rs for the
downstream format). Equivalence requirement: identical vocab+idf inputs must produce
identical vectors across routes (assert in a test).

### D5 — StreamingNeural consumer
`run_neural` enriches embeddings via the embed worker. `enrich_neural_streaming` +
`batch_inputs` (BatchBudget) is the streaming route. Wire the flag-on path in
`neural_publish.rs::run_neural`; preserve the neural checkpoint contract
(NeuralCheckpoint rows/hash) and the admission/hoisting outcomes (total_admitted etc.
in the phase log lines must stay truthful under both routes).

### D6 — BoundedScheduler consumer
`src/scheduler/` has IndexJob/PhaseExecutor/BoundedJob machinery (WS5) — verify what
`BoundedScheduler` was declared to gate: route `index_project_inner`'s phase stepping
through the bounded scheduler when the flag is on. If the scheduler's PhaseExecutor
surface does not fit `index_project_inner` without a rewrite, wire the smallest honest
consumer (e.g. the heavy phases route their work as stepped BoundedJobs) and document
the boundary in the flag doc. Do NOT force-fit by restructuring the whole pipeline.

### D7 — Flag annotations + docs (explicit brief requirement)
For each of the five flags in feature_flags.rs: replace the "declared but not yet
consumed … issue #86" annotation with the real consumer (fn + file) and how it's
gated/tested. Keep env-var docs accurate (defaults OFF). Also update the flag table
in docs if one exists (search for where StreamingPdg is documented as consumed and
mirror it).

### D8 — Tests (per flag)
For each flag: (a) default-off behavior identical (existing suite covers — do not
weaken); (b) flag-on route works: index a fixture project with the env var set, assert
the artifacts and search still work end-to-end; (c) at least one targeted unit test of
the dispatch helper (route selection under env set/unset — mirror how StreamingPdg's
route fn is tested). Use `set_flag_override_for_test` / `lock_flag_tests` discipline
that already exists in indexing tests.

## 2. Sequencing (every commit green: fmt, clippy -D warnings, tests)

- P0: read streaming/{scan,parse,tfidf,neural}.rs + their tests fully; read
  run_scan/run_parse/run_lexical/run_neural fully; read scheduler/{index_job,admission,budget}.rs.
- P1: D7 first for documentation truth (annotations updated as consumers land —
  final state matters, commit order is yours; one commit per flag is cleanest:
  5 commits + optionally 1 for shared test fixtures).
- P2: wire flags in dependency order: Scan → Parse → Tfidf → Neural → BoundedScheduler
  (later phases consume earlier shapes).
- P3: full CI gate (below) + release e2e on a scratch copy.

## 3. Validation gate (all green before reporting; quote outputs)

1. `cargo fmt --all --check`
2. `export PATH=~/.rustup/toolchains/1.98.0-x86_64-unknown-linux-gnu/bin:$PATH` (whole
   bin dir — clippy mixes toolchains otherwise)
3. `cargo clippy --workspace --all-targets -- -D warnings`
4. `CARGO_TARGET_DIR=/mnt/WD-SSD/ldx-target cargo test --workspace --exclude memcheck`
   AND the same with `--all-features` (feature-gated tests don't compile under
   default features — this hid a passing test once)
5. `python3 -m lizard src/ -C 15 -x"*/tests/*" -x"*/target/*" -m -i 0` — every
   function ≤15 CCN (CI pins lizard==1.23.0; check `python3 -m lizard --version`).
   Extract named helpers for row-reader closures (lizard counts `?`-heavy closures
   in the ENCLOSING function).
6. No `src/**/*.rs` over 2000 lines (indexing/mod.rs is near the limit — split new
   dispatch helpers into streaming/mod.rs or a new file if needed).
7. `cargo test --lib grouped` (tools/list payload ≤13000 bytes).
8. Release e2e, scratch copy + isolated HOME (never the real .leindex):
   (a) flags off: index → search → second-index no-op — behavior unchanged;
   (b) consumed flags ON (Scan/Parse/Tfidf/Neural + StreamingPdg): same flow
   green, and log lines prove each streaming route ran; force re-index with
   flags on also green. BoundedScheduler has no consumer in this PR, so it
   produces no route evidence — its limitation is recorded separately (D6:
   annotation + PR body).

## 4. Landmines (from the adoption brief — binding)

- CARGO_TARGET_DIR NEVER under /tmp (31G tmpfs; ENOSPC → misleading cargo parse
  errors). Use /mnt/WD-SSD/ldx-target.
- Toolchain PATH export above; whole bin dir.
- `git add -u` skips NEW files — add new files explicitly.
- mcp_commands bounds (64/256, -32009, was_queued discipline) — only relevant if you
  touch dispatch; you shouldn't.
- Watcher delta: incoming-only edge snapshots — if your parse changes feed the
  watcher, preserve that contract.
- Issue #87's 43 functions: DO NOT fix (lizard-1.24 rescore list, deliberately
  deferred). If your new code trips lizard at 1.23.0, extract helpers — don't touch #87's list.
- Issue #88 textindex v4 (ctime_ns / Windows change_time): coordinate if you touch
  FileIdentity — you shouldn't, but the streaming scan's mtime field is adjacent;
  don't "fix" FileIdentity here.
- Version: NO version bump in this PR (2.0.0 is current and released line; any bump
  is a separate release decision).
- Never run the binary against the real repo's .leindex; scratch copies only.
- Zero clippy warnings, no #[allow], no weakened tests (AGENTS.md binding).

## 4.1 Codebase API notes (current master — verified)

- `ProjectWriteLock` lives in `src/storage/project_lock.rs`, re-exported from
  `cli::leindex::mod`. `migrate_legacy_store` and `persist_graph_via_generation`
  take it — don't double-acquire.
- Read path hydrates from CAS layers; the generation file mirror is RETIRED — never
  code against `generations/<N>/embeddings.bin` etc.
- `SearchOutput.stopped_by_deadline`, `TrigramIndex::remove_nodes`,
  `repath_nodes_to_file_path` — exist; don't be surprised by them in neighboring code.

## 5. Deliverable & reporting

Commits: one per flag (5) + optional fixtures commit, messages `feat(indexing): consume
the <Flag> flag — <route>` style. Final report: per-flag consumer summary (fn + file +
how gated/tested), all gate outputs quoted, e2e transcript lines proving both routes,
lizard output clean. Then the orchestrator opens PR #86 with the body per the brief.
