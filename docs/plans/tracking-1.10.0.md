# LeIndex 1.10.0 — Embed-Merge Orchestrator Tracking

> Branch: `feat/embed-merge-1.10.0` (off master `d8db82c3`).
> Plan: `docs/plans/embed-merge-1.10.0.md`. Coordination: `.AGENT_COORDINATION.md`.
> Orchestrator delegates one task at a time to a dedicated agent, then verifies.

## Status

| Task | Commit | Status | Gate |
|---|---|---|---|
| 0 Baseline + rc.13 | 3d325eb5 | ✅ done | 27/61/193; rc.13 on crates.io |
| 1 Feature DAG | a5094568 | ✅ done | graph/search/onnx(no-def)/default checks |
| 2 Consolidate config | 49abbad9 | ✅ done | cli+onnx check; config tests |
| 3 ort rc.13 + all EP | 7f58186a | ✅ done | onnx+onnx-migraphx checks; 4 manifest tests |
| 4 Move worker src→embed | 88a478fc | ✅ done | lib+bin; crate::embed::* path audit |
| 5 Migrate tests | 4a5fe8dd | ✅ done | 5 suites (82 tests); 61 count |
| 6 Retire subcrate | 27e53f0b | ✅ done | 3 contract tests; zero-match gate |
| 7 Truthful provider | 6fc49ed9 | ✅ done (verified) | provider(37)/runtime_config(2)/auto_(6)/rocm(1); 1404 onnx pass, 2 pre-existing env-probe fails |
| 8 Setup Auto/CoreML | f1ecb0c9 | ✅ done (verified) | 98 setup tests; config_values + persists_auto pass; pure (no pip) |
| 9 Reject stale PATH workers | 39d5a1f4 | ✅ done (verified) | 9 worker_binary tests; onnx_worker_fallback 24 pass (no regression) |
| 10 CI/release ownership | c23adba0 | ✅ done (verified) | 4 contract tests (15/18/15/32); install.js valid + npm guard test; YAML OK; zero active cargo install leindex-embed |
| 11 Bump 1.9.5 + docs | c69da94b | ✅ done (verified) | version 1.9.5; grouped 1.9.1-1.9.5 changelog; 4 contract tests + npm; parity grep clean |
| 12 Full verification | — | ⬜ pending | fmt/clippy -D/test workspace |

## Deferred / notes
- **Task 12 dead_code:** 6 warnings in `src/search/search/{mod,staged_retrieval,vector_impl}.rs` (snapshot items dead when `storage` off). Fix: `#[cfg(feature="storage")]` gate. Trips `clippy --no-default-features --features onnx -D warnings`.
- **byte_offset_to_line_col** line/col parse-error test NOT ported (root config simpler parse_toml; minor UX regression).
- **Parallel agent** editing `src/cli/config.rs` + `src/search/search/mod.rs` (neural_weight 0.3→0.4). Do NOT commit their uncommitted changes; stage explicit paths only.

## Per-task gate convention
Each task agent must: read its plan section, implement, run its stated `cargo test`/`cargo check` gates, commit, and report the commit SHA + gate output. Orchestrator verifies before advancing.
