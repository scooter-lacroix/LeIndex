# Findings: `neural_weight` default drift (0.3 config vs 0.4 scorer)

**Date:** 2026-08-01
**Branch:** `feat/embed-merge-1.10.0` (0 commits behind `origin/master`)
**Status:** Investigation + fix **implemented 2026-08-01** (config default → 0.4, dead `EmbeddingConfig.neural_weight` deleted, example/docs/CHANGELOG aligned; validated: fmt/clippy clean, 1238 lib tests pass).
**Cross-refs:** `docs/plans/fragment-embeddings-1.11.0.md` (audit row), `docs/plans/pr32-coderabbit-deferred-remediation-plan.md` (CR-F9), `docs/findings/2026-08-01-hash-embeddings-warp-evaluation.md` (§General improvements).

---

## TL;DR

The user-facing config default for the neural hybrid weight is **0.3** (`src/config.rs`),
while every scoring-side default in the `search` crate and the index-builder embedder is
**0.4 / 0.40**. At runtime the **config value (0.3) always wins** because the CLI calls
`SearchEngine::set_neural_weight(cfg.search.neural_weight)` unconditionally — but that
makes the actual hybrid blend differ from the scorer's documented intent, and two of the
0.4 constants (`EmbeddingConfig.neural_weight`, `HybridEmbedder`'s embedder weight) are
**dead weight** that is never read by scoring. A third value (**0.6**) ships in
`leindex.toml.example`. Fix: pick config as the single source of truth, align the example,
and delete/route the dead constants.

## Evidence map — every `neural_weight` default

| # | Location | Value | Live? | Role |
|---|----------|-------|-------|------|
| 1 | `src/config.rs:177` `default_neural_weight() -> f64` | **0.3** | ✅ live | Canonical user config default (`[search] neural_weight`) |
| 2 | `src/search/search/mod.rs:155,197` (`SearchEngine::new`/`with_dimension`) | **0.4** | 🟡 fallback | Library-user default; overridden by CLI |
| 3 | `src/search/ranking.rs:89` `HybridScorer::for_code()` | **0.40** | 🟡 intent | Documented "optimized for code search" scorer; not used verbatim in runtime arm |
| 4 | `src/cli/index_builder/hybrid.rs:59-68` `HybridScoringWeights::default()` | **0.40** | ⚪ never consumed by scoring | Only referenced in `hybrid.rs`, a comment at `cli/config.rs:530`, + `index_builder/tests.rs` |
| 5 | `src/cli/index_builder/hybrid.rs:108` `hybrid_local(.., None)` → `unwrap_or(0.40)` | **0.40** | ⚪ dead | `HybridEmbedder` internal weight; never consumed by scoring |
| 6 | `src/cli/config.rs:529` `EmbeddingConfig::default_neural_weight() -> f32` | **0.4** | ⚪ dead | Legacy `ProjectConfig.embeddings`; field never read |
| 7 | `leindex.toml.example:45` | **0.6** | 📄 doc | Example ships a third value — matches neither 0.3 nor 0.4 |
| 8 | `docs/NEURAL_SETUP.md:166` | **0.3** | 📄 doc | Consistent with code default #1 |

## Runtime flow — which value actually wins

```
LeIndex::new()                                   src/cli/leindex/mod.rs:521-526
  SearchEngine::new()                            neural_weight = 0.4   (field default)
  search_engine.set_neural_weight(
      LeIndexConfig::load_cached().search.neural_weight as f32)   → 0.3 (default #1)
```

- `set_neural_weight` (`src/search/search/mod.rs:206-213`) clamps to `[0,1]` and
  clears the in-memory query cache when the value changes → **config is authoritative**.
- The hybrid (`None` query_type, neural available) scoring arm derives weights from
  `self.neural_weight`:
  `remaining = 1.0 - neural_weight` →
  `(tfidf: remaining*0.5, neural: neural_weight, structural: remaining*0.25, text: remaining*0.25)`
  (`src/search/search/mod.rs:1552-1562`).
  - **At 0.3 (runtime):** `(0.35, 0.30, 0.175, 0.175)` — TF-IDF-dominant blend.
  - **At 0.4 (scorer intent):** `(0.30, 0.40, 0.15, 0.15)` — exactly `HybridScorer::for_code()`.
- `QueryType::Semantic`/`Text`/`Exact`/`Structural` arms use **hardcoded** weight tuples
  and ignore `neural_weight` entirely (`scoring_weights`, mod.rs:1546-1566).

## Observable effects of the drift

1. **Behavior vs documentation mismatch.** A stock install (no `neural_weight` in config)
   runs at 0.3 (TF-IDF 0.35 / neural 0.30), while `for_code()`'s docstring advertises
   0.40. Semantic queries in default hybrid mode get **less neural weight** than the
   scoring docs imply.
2. **Cache-key already includes the weight (CR-F9 resolved in-tree).** The result-cache
   key folds in `neural_weight` (`src/cli/index_builder/mod.rs:1517-1526` `nw={}`, and
   `LeIndex::search_cache_key_for` at `src/cli/leindex/mod.rs:643-652`), and
   `set_neural_weight` clears the in-memory cache on change. **CR-F9 in
   `pr32-coderabbit-deferred-remediation-plan.md` is already fixed in the current tree**
   — the plan can be marked done for that item. Note the tree actually went *beyond* the
   plan's stated scope: the plan called "including `neural_weight` in the cache key" out
   of scope, yet the v2 key (`nw={}`) does include it.
3. **Dead constants invite future confusion.** #4/#5/#6 look like live knobs but are never
   read: `HybridScoringWeights` appears only in `hybrid.rs` + tests; `HybridEmbedder`
   `scoring_weights()`/`neural_weight()` are referenced only from `index_builder/tests.rs`
   (lines 986, 988, 1020, 1025, 1115+); `EmbeddingConfig.neural_weight` is never read
   (grep for `.embeddings.` only hits `vector.rs`/`graph/embedding.rs` internals).
4. **Example file ships a third value (0.6).** A user copying `leindex.toml.example` gets
   0.6 — double the config default. `docs/NEURAL_SETUP.md` says 0.3.

## Fix (implemented 2026-08-01)

### Decision: config is the single source of truth

Align the runtime default **up** to **0.4** so the effective blend matches the scorer's
documented intent (`for_code()` = 0.40) and the `SearchEngine` field default already in
the code.

**Code changes (small, mechanical):**

1. `src/config.rs:177` — `default_neural_weight() -> f64 { 0.4 }`.
   - One-line change. Existing users with an explicit `neural_weight` in their config are
     unaffected (config value is respected regardless of default).
   - Users *without* an explicit value see the intended 0.4 blend — which is the blend the
     scorer docs always advertised.
   - **This is a behavior change for default-config users** (the hybrid blend shifts
     toward neural), so it needs a `CHANGELOG.md`/`RELEASE_NOTES.md` entry, and
     `docs/NEURAL_SETUP.md:166` must be updated from 0.3 to 0.4 in the same pass.
2. `leindex.toml.example:45` — `neural_weight = 0.4` (was 0.6), matching the new default.
3. `src/search/search/mod.rs:126-128` — update the field doc comment: "Default 0.4
   preserves prior behavior when unset" → "Default 0.4; the CLI overrides this from
   `[search] neural_weight` (config is authoritative)."
4. `src/cli/config.rs` — delete `EmbeddingConfig.neural_weight` + its
   `default_neural_weight()` (dead; grep-confirmed never read). If API-compat is a
   concern for downstream consumers, mark `#[deprecated]` instead of removing.
5. `src/cli/index_builder/hybrid.rs` — either wire `hybrid_local`'s weight from config
   at the 3 call sites (`load.rs:134,270`, `indexing/mod.rs:1399`) or drop the field:
   the embedder weight is unused by scoring. Minimal-risk option: keep the field but
   pass `Some(cfg.search.neural_weight as f32)` at the call sites so the getter reflects
   reality.

**Tests to add:**

- `src/config.rs` — assert `LeIndexConfig::default().search.neural_weight == 0.4` and
  that a config without the key parses to 0.4 (extend `test_config_missing_keys_uses_defaults`).
- `src/cli/index_builder/tests.rs` — assert `HybridEmbedder::hybrid_local(tfidf, None)`
  reports `neural_weight()` = the config default (0.4) once wired.

### Alternative (lower-risk, keep 0.3)

Keep 0.3 as the config default (it biases hybrid toward TF-IDF, which is safer for
noisy/missing neural coverage), and instead:
- change `HybridScorer::for_code()`/`HybridScoringWeights::default()` comments to say
  "legacy default; runtime uses `[search] neural_weight` (0.3)", and
- align `leindex.toml.example` to **0.3**.
- Same dead-constant cleanup (#4/#5/#6).

Trade-off: keeps today's observed behavior identical (zero regression risk) but leaves a
permanent 0.3-vs-0.4 cosmetic mismatch between config default and scorer "intent" docs.

### Non-negotiable regardless of choice

- `leindex.toml.example` must equal the config default (kill the 0.6).
- Dead constants (#4/#5/#6) are removed or routed so future readers can't mistake them
  for live knobs.
- One comment in each file states where the authoritative value comes from.

## Sequencing

This is a small, self-contained fix — no dependency on the 1.10.0 embed-merge plan or the
1.11.0 fragment plan (which already lists this as an audit row). It can land as its own
PR at any time, or ride along with 1.11.0 per the audit-table note in
`docs/plans/fragment-embeddings-1.11.0.md:26`.

## Verification plan

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The config unit tests (`src/config.rs` `mod tests`) cover default parsing; the
index-builder tests cover `HybridScoringWeights`/`neural_weight()` getters.
