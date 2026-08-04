# WS12-13: Migration, Compatibility, Cleanup, Rollout

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Tie WS3–WS11 together behind progressive `LEINDEX_FEATURE_*` flags (spec §12.3), ship artifact-format migration with magic/version/checksum (§12.2) and a runnable rollback point at every phase, then run the full §13 verification matrix + soak before flipping default-on and eventually removing legacy paths.

**Architecture:** Each workstream is already feature-flagged in its own plan (`daemon-client`, `generation-readers`, `bounded-scheduler`, streaming-stage flags, `global-embed-cache`, validated model profile). WS12-13 owns the cross-cutting concerns: artifact format versioning + migration, protocol handshake (from WS3), the 10-phase rollout sequence, the §13 soak/fault matrix, and the default-on flip + legacy removal.

**Spec refs:** §11 (failure/recovery), §12 (compatibility/migration), §13 (verification matrix), §16 (acceptance gates).
**Depends on:** SP1–SP6 (all).
**Tech Stack:** existing `src/feature_flags.rs`, `.github/workflows/{release,rollback,quality,memory-budget,performance-regression}.yml`.

**Existing infra (reuse):**
- `src/feature_flags.rs` — `LEINDEX_FEATURE_*` env-var flags (NEURAL_SEARCH, REMOTE_EMBEDDINGS, CROSS_LANGUAGE, EXPERIMENTAL_HNSW, STREAMING_MCP, GLOBAL_AUTO_SYNC). Add new flags here.
- `.github/workflows/rollback.yml` + `release.yml` — rollback + release pipelines.
- WS3 `daemon/handshake.rs` — protocol versions (§12.1).
- WS4 `migrate.rs` + blob/manifest magic (`LIDX-BLB1`/`LIDX-GEN1`) — artifact versioning (§12.2).
- WS4 `retention.rs` — cleanup.

---

## File Structure

| File | Responsibility |
|---|---|
| `src/feature_flags.rs` | Add: `daemon_client`, `generation_readers`, `bounded_scheduler`, `streaming_pipeline`, `global_embed_cache`, `validated_model` |
| `src/migration/mod.rs` | Cross-version artifact migration registry |
| `src/migration/artifact.rs` | Magic/version/checksum validation + read-old paths |
| `src/cli/cleanup.rs` | Extended cleanup (CAS GC, stale generations, orphaned cache) |
| `tests/soak/reindex_loop_test.rs` | §13 #13: 100-reindex monotonic-growth |
| `tests/soak/idle_test.rs` | §13 #14: 1h / 24h idle |
| `tests/fault/*.rs` | §13 crash/cancel/corrupt scenarios |
| `docs/rollout/phase-*.md` | Per-phase rollout runbook + rollback point |

---

## Task 1: Feature-flag registry for all workstreams

**Files:** `src/feature_flags.rs`, `.env.example`

- [ ] **Step 1: Write failing test** — each new flag reads `LEINDEX_FEATURE_*`, defaults OFF (legacy behavior) when unset.
- [ ] **Step 2:** Add flags: `DAEMON_CLIENT` (WS3), `GENERATION_READERS` (WS4), `BOUNDED_SCHEDULER` (WS5), `STREAMING_SCAN`/`STREAMING_PARSE`/`STREAMING_PDG`/`STREAMING_TFIDF`/`STREAMING_NEURAL` (WS6-9), `GLOBAL_EMBED_CACHE` (WS10), `VALIDATED_MODEL` (WS11).
- [ ] **Step 3:** Document all in `.env.example` (default OFF / opt-in).
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(flags): feature-flag registry for WS3-WS11 rollout"
```

---

## Task 2: Artifact format versioning + validation (§12.2)

**Files:** `src/migration/artifact.rs`

- [ ] **Step 1: Write failing test** — every new format (CAS blob `LIDX-BLB1`, manifest `LIDX-GEN1`) has magic + version + checksum; reader rejects mismatched magic/version with actionable error; model/vector identity mismatch forces rebuild, never silent reuse (§12.2).

- [ ] **Step 2-4:** TDD. The WS3 handshake (§12.1) carries artifact-format version; mismatches fail safely.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(migration): artifact magic/version/checksum validation (§12.2)"
```

---

## Task 3: Read-old / build-new / switch migration

**Files:** `src/migration/mod.rs`, WS4 `migrate.rs`

- [ ] **Step 1: Write failing test** — read legacy generation (full-copy dir) → build new CAS manifest beside old → validate → switch `CURRENT` → preserve rollback generation. Idempotent + crash-safe (§12.2).

- [ ] **Step 2-4:** TDD. Reuses WS4 Task 10 migration sweep; here it's the cross-version registry (future v2→v3 migrations plug in).
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(migration): read-old/build-new/switch with rollback preservation"
```

---

## Task 4: Rollout phase runbooks + rollback points

**Files:** `docs/rollout/phase-{1..10}.md`

Spec §12.3 phases: (1) measurement-only baseline, (2) containment defaults, (3) daemon opt-in, (4) shared registry+scheduler, (5) streaming stage-by-stage, (6) shared worker+cache, (7) model bake-off, (8) daemon default-on, (9) legacy fallback period, (10) legacy removal.

- [ ] **Step 1:** For each phase, document: flags enabled, prerequisite phases, validation commands, success criteria, rollback procedure. Each phase MUST preserve an independently runnable rollback point (§12.3).
- [ ] **Step 2: Commit**

```bash
git commit -m "docs(rollout): §12.3 phase 1-10 runbooks with rollback points"
```

---

## Task 5: §13 verification matrix — scenarios 1-12 (functional)

**Files:** `tests/fault/*.rs`

- [ ] **Step 1:** Implement: (1) 3-harness/2-project mixed tools, (2) same-project simultaneous index, (3) different-project simultaneous index, (4) search during indexing (no-stall), (5) repeated identical index (coalesce), (6) changes mid-index (schedule follow-up), (7) worker cold/warm, (8) worker crash mid-batch, (9) daemon crash during each publication phase, (10) cancellation during each index phase, (11) huge file, (12) large repo.
- [ ] **Step 2:** Each captures correctness + latency + CPU + memory + threads + swap + GPU + disk-IO (§13 "For every scenario capture...").
- [ ] **Step 3: Commit**

```bash
git commit -m "test(rollout): §13 functional verification scenarios 1-12"
```

---

## Task 6: §13 verification matrix — scenarios 13-24 (soak/fault/edge)

**Files:** `tests/soak/*.rs`, `tests/fault/*.rs`

- [ ] **Step 1:** Implement: (13) 100 reindexes no monotonic RSS/swap growth, (14) 1h + 24h idle, (15) CPU-only provider, (16) MIGraphX provider, (17) CUDA provider (where CI exists), (18) cgroup memory-pressure, (19) corrupt/missing DB/mmap/checkpoint/model, (20) model/tokenizer/config migration, (21) old-shim/new-daemon + new-shim/old-daemon, (22) worktrees sharing content, (23) query cancellation/disconnect storm, (24) watcher event storm.
- [ ] **Step 2:** Every crash/cancel test preserves last valid generation (§16 reliability gate).
- [ ] **Step 3: Commit**

```bash
git commit -m "test(rollout): §13 soak/fault/edge scenarios 13-24"
```

---

## Task 7: Cleanup hardening (CAS GC + stale gens + orphaned cache)

**Files:** `src/cli/cleanup.rs`

- [ ] **Step 1: Write failing test** — `leindex cleanup` removes: orphaned CAS blobs (refcount 0), stale generations (not current/previous/leased), orphaned embed-cache rows, abandoned staging. Never removes leased/current/rollback generations (§16 reliability gate).
- [ ] **Step 2-4:** TDD. Unify with WS4 retention + WS10 cache compaction.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(cleanup): CAS GC + stale-gen + orphaned-cache sweep (never touches leased)"
```

---

## Task 8: Default-on flip (phase 8) + acceptance-gate audit

**Files:** feature flag defaults, `.github/workflows/*.yml`

- [ ] **Step 1:** Only after phases 1-7 pass their runbooks + §13 scenarios pass: flip each flag default ON.
- [ ] **Step 2:** Audit §16 acceptance gates: Resource (≤1 GiB steady + peak, no monotonic growth, idle CPU ~0, thread budgets, GPU counted), Performance (p50/95/99 no regression, responsive-during-index, wall-time no regression, CPU-sec/MiB improvement, no cold-start churn), Quality (aggregate + per-category gates, no stale/omitted/partial, identity reproducible, ablations pass), Reliability (crash/cancel preserve generation, no dup daemon/worker, no cross-project confusion, mismatches fail safely, cleanup safe).
- [ ] **Step 3:** Any unmet gate blocks the flip — record + remediate (anti-cheat: never manufacture a pass).
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(rollout): phase 8 default-on after §16 acceptance gates pass"
```

---

## Task 9: Legacy fallback period (phase 9) + removal (phase 10)

**Files:** guarded code paths.

- [ ] **Step 1:** Phase 9 — keep legacy paths reachable via explicit env (`LEINDEX_LEGACY=1`) during a fallback window.
- [ ] **Step 2:** Phase 10 — after the soak window + zero rollback requests, delete legacy paths (full-copy generations, heap-mirror reads, error-at-cap, count-only batching, per-harness inline server, FP16-only model).
- [ ] **Step 3:** Verify deletion passes the full validation suite (zero dead code / clippy warnings per AGENTS.md zero-tolerance).
- [ ] **Step 4: Commit**

```bash
git commit -m "refactor(rollout): phase 10 legacy path removal after fallback window"
```

---

## Task 10: Final validation + post-baseline capture (the "after")

- [ ] **Step 1: Validation suite**

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: Post-baseline capture** on the full default-on v2.0.0 build, same corpora + methodology as SP1 Task 10 Step 3 (the pre-anchor). Record to `docs/baselines/2026-08-04-post-v200.json`. Diff against `2026-08-04-pre-v190-anchor.json`.
- [ ] **Step 3:** Re-run WS1 memcheck on the default-on build; confirm ≤1 GiB aggregate (3 clients / 2 projects) and footprint targets.
- [ ] **Step 4:** Confirm every §16 gate has evidence files in `docs/baselines/`.
- [ ] **Step 5: Commit**

```bash
git commit -m "docs(rollout): post-v2.0.0 baseline + §16 gate evidence"
```

---

## Task 11: Generate shipped `BENCHMARKS.md`

**Files:** Create `BENCHMARKS.md` (repo root, shipped, referenced from README).

- [ ] **Step 1:** Digest `docs/baselines/2026-08-04-pre-v190-anchor.json` + `2026-08-04-post-v200.json` (+ per-workstream baselines) into a curated, human-readable `BENCHMARKS.md`. Required sections:
  - Index database size: v1.9.x vs v2.0.0 (this repo + large fixture)
  - Generation files: v1.9.x full-copy × N vs v2.0.0 CAS dedup (+ dedup ratio)
  - Total `.leindex/` storage: avg + peak, before/after
  - RAM: steady-state aggregate (daemon+worker+shims) + index-peak, before/after
  - Headline reductions (this repo: 2.5 GiB → ≤60 MiB; large project: 150 GiB+ → proportional)
  - §16 acceptance-gate summary (resource/perf/quality/reliability)
- [ ] **Step 2:** Every claim links to its evidence file (kept in `docs/baselines/` until Task 13 removes the raw files; `BENCHMARKS.md` itself is the shipped digest).
- [ ] **Step 3: Commit**

```bash
git add BENCHMARKS.md
git commit -m "docs(v2.0.0): ship curated BENCHMARKS.md (before/after storage + RAM)"
```

---

## Task 12: README + CHANGELOG radical marketing rewrite (FINAL tasks before completion marker)

**Files:** `README.md` (root), `packages/pypi-leindex/README.md`, `packages/npm-leindex-mcp/README.md`, `CHANGELOG.md`.

**Per AGENTS.md repo-hygiene:** keep the three README surfaces aligned; update all public MCP config examples together.

- [ ] **Step 1: CHANGELOG.md** — author the `## [2.0.0]` entry: the single-daemon architecture, sub-1 GiB target, CAS generations, the footprint reduction (cite `BENCHMARKS.md`), streaming pipeline, shared worker + global cache, model bake-off outcome, rollout. Hype-toned but evidence-backed.
- [ ] **Step 2: Root README.md** — radical marketing retune: hero headline, the resource story (17× → target overhead removal; 150 GiB+ → manageable), image/badge refresh, "why v2.0.0" section linking `BENCHMARKS.md`, updated install + MCP config examples, version-bumped feature list.
- [ ] **Step 3: Package READMEs** (PyPI + npm) — mirror the root README's resource/marketing story; align MCP config examples across all three (AGENTS.md).
- [ ] **Step 4: Version parity** — bump `Cargo.toml`, installer scripts, npm + PyPI metadata, in-repo version constants to `2.0.0` together (AGENTS.md repo hygiene).
- [ ] **Step 5: Commit**

```bash
git add README.md CHANGELOG.md packages/*/README.md Cargo.toml packages/*/package.json packages/*/pyproject.toml
git commit -m "docs(v2.0.0): radical README + CHANGELOG marketing rewrite + version bump to 2.0.0"
```

---

## Task 13: Remove tracking scaffolding (before final push)

**Per user mandate:** v2.0.0 ships as code + README + CHANGELOG + BENCHMARKS.md only.

- [ ] **Step 1:** Remove all planning/tracking scaffolding:
  - `docs/superpowers/plans/2026-08-04-*.md` (this effort's plans)
  - `docs/superpowers/specs/2026-08-04-*.md` (this effort's specs)
  - `docs/baselines/*` (raw measurement files — already digested into `BENCHMARKS.md` in Task 11)
  - Any handoff/progress tracking docs generated during execution
- [ ] **Step 2: Verify** `BENCHMARKS.md` references are self-contained (no dead links to removed `docs/baselines/` files — the digest must stand alone).
- [ ] **Step 3: Verify** no other tracked file references the removed scaffolding.
- [ ] **Step 4: Commit**

```bash
git rm -r docs/superpowers/plans/2026-08-04-*.md docs/superpowers/specs/2026-08-04-*.md docs/baselines/
git commit -m "chore(v2.0.0): remove planning + baseline scaffolding (shipped via BENCHMARKS.md)"
```

---

## Task 14: Master-plan completion marker + final push

- [ ] **Step 1:** Final validation sweep on the clean tree: `cargo fmt --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.
- [ ] **Step 2:** Confirm ship set = code + `README.md` + `CHANGELOG.md` + `BENCHMARKS.md` (+ package READMEs/metadata). No `docs/superpowers/` or `docs/baselines/` remains.
- [ ] **Step 3:** Final commit / push / tag `v2.0.0` via the release workflow (`.github/workflows/release.yml`).
- [ ] **Step 4:** Master plan marked COMPLETE.

---

## Handoff Summary (fill after execution)

```text
Workstream: WS12-13
Revision: 1.0
Invariant status: §16 all gates evidenced before default-on; rollback point per phase; legacy removed only after fallback window
Files changed: src/feature_flags.rs, src/migration/*, src/cli/cleanup.rs, tests/{fault,soak}/*, docs/rollout/*
Tests run/results: [fill — full §13 matrix]
Benchmark artifacts: docs/baselines/2026-08-04-ws12-13-* (per phase + final)
TBD resolutions: none inline
Decisions: [fill — phase pass/fail, gate evidence]
Unverified assumptions: [fill]
Known risks: default-on is irreversible-ish — gate strictly
Rollback: phase 9 LEINDEX_LEGACY=1; rollback.yml
Next: none — architecture complete at §16 gate pass
```

## Spec-coverage check (§12, §13, §16)

| Spec § | Task |
|---|---|
| §12.1 protocol versions (handshake) | WS3 + Task 2 |
| §12.2 artifact magic/version/checksum, read-old/build-new/switch, rollback, mismatch→rebuild | 2, 3 |
| §12.3 phases 1-10 each with rollback point | 4, 8, 9 |
| §13 scenarios 1-12 | 5 |
| §13 scenarios 13-24 | 6 |
| §13 evidence capture (mem/CPU/swap/GPU/disk per scenario) | 5, 6 |
| §11.1-11.4 failure/recovery | 5, 6 |
| §16 resource/perf/quality/reliability gates | 8 |
| cleanup never removes leased/current/rollback | 7 |

## Forbidden shortcuts (anti-cheat §2.1)

- Do NOT flip default-on with an unmet gate — record + remediate, never manufacture (§2.1 #12, #14).
- Do NOT remove legacy paths before the fallback window + reliability soak (§12.3 phase 9-10).
- Do NOT keep a legacy heavyweight path enabled while measuring only the optimized path (§2.1 #14) — measurement harness must exercise the production path.
- Do NOT skip any §13 scenario — all 24 required with full evidence.
