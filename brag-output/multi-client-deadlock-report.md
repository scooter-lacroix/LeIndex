# Multi-client indexing deadlock / bricked-generation — root-cause fix report

Worktree: `/mnt/WD-SSD/Prod/LeIndex-s1-wt` (branch `system-one-gate`, PR #95 open). Nothing committed.

## Incident class

Two or more LeIndex clients (MCP servers / `leindexd` daemon / CLI) racing the same
project store on first use produced, pre-fix: permanent `project not indexed` answers,
tool-call stalls, duplicated cold-start index builds, forked daemons, and — worst case —
a bricked generation (CURRENT swap published from two writers). Each root cause below
is an interleaving, not a logic bug in isolation; each fix has one regression test that
was **proven to fail on pre-fix code** (mutation applied, test run, failure captured,
fix restored, test re-run green).

## Root causes and fixes

| RC | Root cause (interleaving) | Fix | Site |
|----|---------------------------|-----|------|
| RC1 | Writer holds `index.lock` forever (wedged/killed mid-write); every other client blocks **forever** on blocking `acquire()` — permanent stall | `acquire_bounded()`: try_acquire poll with timeout (default 300s, `LEINDEX_WRITE_LOCK_TIMEOUT_MS`, test-only atomic override); on expiry returns `[transient:lock-contention]` naming the lock file; blocking `acquire()` deleted; all callers migrated | `src/storage/project_lock.rs` (+ callers: `cleanup.rs`, `cli/storage_cleanup.rs`, `phase/context.rs`, `storage/generation/migrate.rs`) |
| RC2 | N concurrent cold-start tool calls each ran `LeIndex::new` + corruption repair **inline on async workers**; repair is a full `index_project(true)` → worker-pool starvation, plus N duplicate builds | `creation_flights: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>` single-flight in `create_and_insert` (entry-keyed, lock, double-check under flight); build+repair folded into `build_leindex` assoc fn run inside `spawn_blocking` | `src/cli/registry.rs` |
| RC3 | Graph-only hydration path (GenReaders) leaves `pdg` resident but engine empty; rebuild gate keyed only on `active_has_indexed_files()` → search tools silently return empty **forever** (permanent lie) | Gate: `search_engine.is_empty() && (pdg.is_some() ‖ active_has_indexed_files())`; early-return arm requires both pdg and non-empty engine | `src/cli/leindex/mod.rs` `ensure_analysis_context_loaded` |
| RC4a | Publish copies `index_stats.json` root→generation; root copy lost/emptied → `load_stats_from_path` reports 0 → `is_indexed()==false` forever even though the committed generation is complete | Fallback to `generations/<CURRENT>/index_stats.json` when root copy missing or `indexed_nodes==0` | `src/cli/leindex/mod.rs` `load_stats_from_path` |
| RC4b | Resident instance created **before** any index existed has `hydrated_generation==None`; the "generation advanced" eviction can never fire (None never > disk) and cooldown suppresses auto-index → permanent "not indexed" | Never-hydrated eviction arm: `!is_indexed() && CURRENT>0 && generation_has_indexed_nodes()` → evict + `create_and_insert` (livelock guard: 0-node generations never evict-churn) | `src/cli/registry.rs` `get_or_load` |
| RC5a | `flock` locks the **inode**, not the path: operator deletes wedged `index.lock`, second writer re-creates it and both publish → WAL-bricked CURRENT | `LockIdentity (dev,ino)` captured at acquire; `still_guards()` zero-pair passes / metadata-miss fails; pre-publish promote gate refuses if lock file identity changed | `src/storage/project_lock.rs`, `src/cli/leindex/indexing/mod.rs` |
| RC5b | `run_socket` unconditionally `remove_file` on an existing socket path: unlinks a **live** daemon's socket, binds a second daemon at the same path → forked fleet racing one store | 250ms `UnixStream::connect` probe before unlink; live listener → `anyhow::bail!("refusing to bind … pkill leindexd")`; dead socket → removed | `src/cli/mcp/server.rs` `run_socket` |
| RC6 | `detect_corruption` probed only `project/.leindex`; stores under `LEINDEX_HOME`/XDG never matched → corruption undetected, auto-repair never fired | Probe `resolve_existing_storage_path(project)` (in-project → LEINDEX_HOME → XDG → tmp), fallback `project/.leindex` | `src/cli/errors.rs` |
| Daemon lock | `leindexd`: open→flock race — lock file deleted+recreated between open and flock leaves daemon holding an orphaned inode while another daemon wins the fresh path | Post-flock `fstat(fd)` vs `stat(path)` inode identity check, `ErrorKind::Other` bail; `main()` distinguishes inode-race refusal from "daemon already live" (sidecar message) | `src/bin/leindexd.rs` `acquire_daemon_lock` |
| Cleanup/GC race | **Verified safe, no fix needed**: sweep only touches `leindex-embed-`/`leindex-mcp-` stems; `daemon.endpoint` pid-checked; `index.lock` lives inside project storage; retention runs under index lock | — | — |

## Fix ↔ test mapping (all pre-fix failures captured)

| Test | Guards | Pre-fix proof (verbatim) | Post-fix |
|------|--------|--------------------------|----------|
| T1 `test_acquire_bounded_times_out_behind_wedged_holder` (`project_lock.rs`) | RC1 | `FAILED. … 'acquire_bounded must terminate'` (recv_timeout expiry, 10s) — mutation: timeout condition → `false` | `1 passed` |
| T5 `test_still_guards_detects_lock_file_replacement` (`project_lock.rs`) | RC5a | guard stayed `true` after lock file delete+recreate — mutation: `still_guards` → `if true return true` | `1 passed` |
| T2 `test_engine_rebuilt_after_graph_only_hydration` (`generation_read_test.rs`) | RC3 | `0 passed; 1 failed` — mutation: gate drops `pdg.is_some()` | `1 passed` 1.22s |
| T3 `test_stats_fallback_to_generation_copy` (`generation_read_test.rs`) | RC4a | `0 passed; 1 failed` — mutation: generation-fallback block deleted | `1 passed` 1.39s |
| T4 `test_never_hydrated_resident_picks_up_external_index` (`registry_test.rs`) | RC4b | `0 passed; 1 failed` — mutation: never-hydrated arm → `false` | `1 passed` 0.62s |
| T7 `test_concurrent_cold_start_creations_join_one_build` (`registry_test.rs`) | RC2 | `0 passed; 1 failed` (0.80s, `Arc::ptr_eq`) — mutation: single-flight removed, every caller builds directly | `1 passed` 0.62s |
| T6 `test_run_socket_refuses_to_steal_live_socket` (`server_test.rs`, unix) | RC5b | test **hung** until killed at 120s (`Terminated`) — pre-fix code stole the live socket, bound, entered accept-loop forever (the daemon-fork itself) — mutation: unconditional `remove_file` | `1 passed` 0.00s |
| T8 `test_detect_corruption_probes_resolved_storage_root` (`errors.rs` tests) | RC6 | `0 passed; 1 failed` — mutation: `leindex_dir = project.join(".leindex")` | `1 passed` 0.00s |

Pre-fix proof batches: A (`project_lock.rs`: T1+T5 → `2 failed`), B (`mod.rs`+`registry.rs`+`errors.rs`: T2, T3, T4, T8 — four separate runs, each `0 passed; 1 failed`), C (`server.rs`+`registry.rs`: T6 hang-killed, T7 failed). After each batch the fix was restored and the tests re-run green before proceeding.

## Deterministic interleavings (per RC)

- **RC1**: W1 opens+flocks `index.lock`, stalls (SIGSTOP / long index). W2..Wn call
  `acquire()` → block on flock with no timeout → MCP tool calls queue forever.
  Post-fix: W2..Wn poll try_acquire for the bounded window, then surface
  `[transient:lock-contention]` naming `index.lock` and `LEINDEX_WRITE_LOCK_TIMEOUT_MS`.
- **RC2**: n cold-start tool calls race `get_or_create` on a fresh store. Pre-fix each
  ran `LeIndex::new` + inline corruption repair on an async worker → n duplicate full
  index builds, worker pool starved, daemon unresponsive. Post-fix: all n join one
  `creation_flights` entry; the winner builds inside `spawn_blocking`; joiners hit the
  double-check and return the winner's `Arc` (T7 asserts `Arc::ptr_eq` + registry.len()==1).
- **RC3**: GenReaders hydrates PDG from a committed generation (graph-only path);
  `active_has_indexed_files()` false because `has_indexed_files` counts the root
  `leindex.db`, not the generation copy → engine never rebuilt → every search empty,
  forever, while symbol tools work fine. Probe-verified state on the fixture:
  `pdg_resident=true engine_empty=true active_has_files=false`.
- **RC4a**: publish stages generation metadata root→`generations/<G>`; a crash or a
  second writer's vacuum empties the root `index_stats.json` → next `LeIndex::new`
  loads 0 nodes → `is_indexed()==false` → cooldown suppresses re-index → permanent
  "not indexed". Post-fix: stats restored from the generation copy (the committed
  generation is the authority; CURRENT only swaps after complete publish).
- **RC4b**: resident instance created pre-index (`hydrated_generation=None`);
  external CLI indexes and publishes G=1. The advance-check `disk > hydrated` can
  never fire (None), cooldown suppresses auto-index → every tool call answers
  "project not indexed" until process restart. Post-fix: never-hydrated arm evicts
  and re-creates when disk CURRENT carries a non-empty generation.
- **RC5a**: W1 holds `index.lock` (inode A). Operator deletes the path. W2 re-creates
  (inode B), flocks it, both believe they are the sole writer, both publish CURRENT →
  bricked WAL. Post-fix: promote gate re-checks `still_guards` immediately before the
  CURRENT swap; identity mismatch → run fails as transient lock-contention instead of
  publishing from two writers.
- **RC5b**: daemon D1 bound socket S. D2 starts, `run_socket` sees S exists, unlinks
  it, binds a fresh socket at the same path. D1 serves on the unlinked inode; D2 on
  the new one; both race the store. Post-fix: D2's 250ms connect probe reaches D1's
  live listener (kernel completes the handshake regardless of accept) → D2 refuses.
- **RC6**: store redirected to XDG (or LEINDEX_HOME). Client `detect_corruption`
  probes `project/.leindex`, finds nothing → `Healthy` → garbage `leindex.db` at the
  real store never triggers repair. Post-fix: probe resolves the actual storage root.

## Gates (verbatim)

### lizard — `lizard -C 15 -m -i 0 <9 touched files>`
```
9 file analyzed.
==============================================================
NLOC    Avg.NLOC  AvgCCN  Avg.token  function_cnt    file
--------------------------------------------------------------
    193      19.6     3.4      144.9         9     src/bin/leindexd.rs
    420       9.5     1.8       66.9        33     src/cli/errors.rs
    762      12.0     2.7       80.9        58     src/cli/leindex/mod.rs
    439      35.4     1.7      236.8        11     src/cli/leindex/generation_read_test.rs
   1203      27.0     3.5      175.3        41     src/cli/mcp/server.rs
    456      21.5     1.2      182.8        20     src/cli/mcp/server_test.rs
   1217      18.7     3.3      129.9        61     src/cli/registry.rs
    600      15.6     1.3      157.6        36     src/cli/registry_test.rs
    296      16.4     2.4      105.7        15     src/storage/project_lock.rs

===============================================================================================================
No thresholds exceeded (cyclomatic_complexity > 15 or length > 1000 or nloc > 1000000 or parameter_count > 100)
==========================================================================================
Total nloc   Avg.NLOC  AvgCCN  Avg.token   Fun Cnt  Warning cnt   Fun Rt   nloc Rt
------------------------------------------------------------------------------------------
      5586      17.8     2.5      129.7      284            0      0.00    0.00
```

### cargo fmt --all --check
```
$ cargo fmt --all && cargo fmt --all --check && echo "FMT CLEAN"
FMT CLEAN
```
(9 format diffs in my files were normalized by `cargo fmt --all` first; re-check clean.)

### clippy — set 1 (docs.yml gate: default features)
```
$ cargo clippy --workspace --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 24.69s
```
One warning surfaced and was fixed during the gate (not suppressed):
`leindexd.rs:106` `bind_instead_of_map` — `and_then(|m| Some((m.dev(), m.ino())))`
→ `map(|m| (m.dev(), m.ino()))`.

### clippy — set 2 (release.yml gate: all features)
```
$ cargo clippy --workspace --all-features --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 49.38s
```
Note: CI has no `--no-default-features` clippy row; its compile matrix
(`--no-default-features --features minimal`) was verified instead:
```
$ cargo check -p leindex --all-targets --no-default-features --features minimal
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 7.23s
```
(`--all-targets --no-default-features` without features cannot compile several
integration tests that unconditionally import feature-gated modules — E0433 in
`tests/search_*.rs`, `tests/parser_fixtures_test.rs` — the documented minimal
feature set is the supported narrow build.)

### cargo test --workspace --exclude memcheck
```
$ cargo test --workspace --exclude memcheck
WORKSPACE_EXIT=0
```
44 suites, all `test result: ok`, 0 failed. Totals: **3134 passed, 39 ignored, 0 failed**.
Largest suites (verbatim):
```
test result: ok. 2634 passed; 0 failed; 4 ignored; 0 measured; 0 filtered out; finished in 19.28s   (leindex lib)
test result: ok.  36 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 1.64s
test result: ok.  34 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.20s
test result: ok.  32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
test result: ok.  30 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.23s
```
Operational note: the first run failed at link stage — `/mnt/WD-SSD` hit 100% full
(`No space left on device`, `ld terminated with signal 7 [Bus error]`), which also
manifested as a spurious `rust-lld` crash backtrace. Reclaimed 19G by deleting
`target/debug/incremental` (cargo treats missing incremental cache as cold;
nothing else touched), then the full suite linked and passed. The concurrent-agent
disk usage is the cause; not a code issue.

## Residual limits (documented, not fixed)

- **Adversarial deletion of both daemon lock and socket files** cannot be fully
  prevented with pure file-based locking: the inode check catches the open→flock
  swap race; the socket probe covers the common live-listener case; an adversary
  deleting BOTH still forks the fleet. Full prevention needs a kernel-object
  (abstract-socket / lockless pidfd) approach — out of scope here.
- **Windows `still_guards`**: uses `MetadataExt` (unix). On Windows the identity
  pair is absent → zero-pair passes (documented conservative default). Windows
  file-index APIs (`GetFileInformationByHandle`) are the follow-up if/when a
  Windows writer path ships.
- **RC1 timeout default 300s**: a genuinely long index run longer than 300s on a
  contended store surfaces as transient lock-contention to that caller; the
  operator can raise `LEINDEX_WRITE_LOCK_TIMEOUT_MS`. Deliberate trade: bounded
  failure over infinite hang.
- **memcheck acceptance gate not run** (contract excludes it; requires release
  binary + serialized 13-test suite, ~21m).

## Rejected YAGNIs

- Periodic daemon-lock inode re-check after acquisition — the socket probe covers
  the real failure path (second daemon binding); post-acquisition churn without a
  second binder is harmless.
- Windows `still_guards` file-index API — no Windows writer path exists today;
  unstable API surface; conservative default documented instead.
- `creation_flights` map eviction — bounded by `max_projects` churn (LRU already
  caps residency); entries are one `Arc<Mutex<()>>` each.
- Abstract-socket namespace for the daemon socket — Linux-only, breaks the
  filesystem-discoverable sidecar contract; probe is sufficient for the incident.
- Per-flight cancellation propagation (joiners aborted if the winner fails) —
  joiners today fall through to their own build on winner error, which is correct
  (single-flight is an optimization, not an invariant).

## Diff ownership (nothing committed)

`git status --porcelain` shows 17 modified files + `brag-output/` untracked.

Mine (14 + tests):
`src/bin/leindexd.rs`, `src/cli/errors.rs`, `src/cli/leindex/mod.rs`,
`src/cli/leindex/generation_read_test.rs`, `src/cli/leindex/indexing/mod.rs` (RC5a
pre-publish gate), `src/cli/mcp/search_handler.rs` (never-indexed vs
indexed-but-unhydrated error guidance), `src/cli/mcp/server.rs`,
`src/cli/mcp/server_test.rs`, `src/cli/registry.rs`, `src/cli/registry_test.rs`,
`src/cli/cleanup.rs`, `src/cli/cli/storage_cleanup.rs`, `src/phase/context.rs`,
`src/storage/generation/migrate.rs` (last four: RC1 `acquire`→`acquire_bounded`
caller migrations), `src/storage/project_lock.rs`.

Concurrent agent's (untouched by me): `.github/workflows/memory-budget.yml`
(memcheck timeout 10m→30m), `Cargo.toml` (tar 0.4.44→0.4.45), `Cargo.lock`.
