# WS11: Model/Reranker Conversion + Bake-off

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development.

**Goal:** Select the production embedding model profile and reranker policy via a LeIndex-specific fused-retrieval evaluation, **before** any precision reduction reaches default builds. The §7 budget ledger proves FP16 Qwen3 + FP16 reranker (each ~1.19 GiB) do not fit the 1 GiB aggregate target — so WS11 must produce a measured winner among quantized/compact/no-reranker options.

**Architecture:** Build a labeled LeIndex evaluation corpus + harness; record predeclared statistical gates; run candidates; pick the end-to-end fused-retrieval winner (not standalone embedding score). Quantize the winner; ship behind the WS4 INT8 read path.

**Spec refs:** §7 (budget conflict), §9 (full eval design), §2.1 #4/#13 (no precision cut without proof; no public-benchmark-only selection).
**Depends on:** SP3a (INT8 read path + quantized format), SP5 (global cache, so candidates share infra), SP4 (streaming pipeline).
**Tech Stack:** Rust eval harness, ONNX quantization tools, existing fused retrieval.

**Existing infra (reuse):**
- Fused retrieval (TF-IDF + PDG + dense + fragment + reranker) in `src/search/`.
- `HybridEmbedder`, `EmbeddingClient` (`src/cli/index_builder/hybrid.rs`, `src/search/onnx/`).
- Reranker path.
- Model download/setup (`src/cli/leindex/model_download.rs`, `setup_ort.rs`, `embed/model_path.rs`).

**Non-negotiable (anti-cheat §2.1 #4, #13):** No precision reduction or model swap ships without passing the gates below. Public MTEB/CodeSearchNet numbers shortlist only — they do NOT select.

---

## File Structure

| File | Responsibility |
|---|---|
| `eval/corpus/` | Labeled query set (§9.2 categories) + fixed splits |
| `eval/harness.rs` | Runs candidates against fused retrieval; emits metrics JSON |
| `eval/metrics.rs` | Recall@k, MRR@10, nDCG@10, per-category, ablation |
| `eval/gates.rs` | Predeclared acceptance gates (§9.4) + variance bands |
| `eval/candidates.rs` | Model/reranker candidate registry |
| `eval/report.rs` | Concise + machine-readable report |

---

## Task 1: Predeclare acceptance gates + variance bands

**Files:** `eval/gates.rs`

**Critical (§9.4):** gates are recorded BEFORE seeing candidate outcomes.

- [ ] **Step 1: Write failing test** — gate struct + checker; a candidate violating aggregate MRR@10 regression fails; a candidate with >1pp protected-category regression fails; variance bands computed from baseline repeated runs.

```rust
pub struct Gates {
    pub aggregate_mrr10_max_regression: f64,    // 0.0 = no regression allowed
    pub protected_category_max_regression_pp: f64, // 1.0 (one percentage point)
    pub p95_latency_max_regression_pct: f64,
    pub wall_time_max_regression_pct: f64,
    pub zero_result_forbidden: bool,
}
```

- [ ] **Step 2-4:** TDD. Run baseline (FP16 Qwen3) 5× to establish variance; set bands from observed stddev.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(eval): predeclared acceptance gates + baseline variance bands"
```

---

## Task 2: LeIndex evaluation corpus (§9.2)

**Files:** `eval/corpus/`

- [ ] **Step 1:** Assemble labeled cases for every §9.2 category: NL→symbol, exact/partial identifier, concept→impl, error/log→origin, caller/callee/data-flow, interface→impl, config/doc→code, similar-algo-different-name, same-name-different-behavior, changed/deleted/freshness, large/generated distractors, multi-language (Rust/TS/Python/Go/Java/C/C++), cross-language, hard negatives.
- [ ] **Step 2:** Use repository-held labels + fixed train/eval splits. Add anonymized real query patterns where privacy permits.
- [ ] **Step 3: Write failing test** — harness loads corpus, verifies category coverage + split integrity.
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(eval): §9.2 labeled corpus across all retrieval categories"
```

---

## Task 3: Metrics + harness

**Files:** `eval/metrics.rs`, `eval/harness.rs`, `eval/report.rs`

- [ ] **Step 1: Write failing test** — Recall@1/5/10, MRR@10, nDCG@10, relevant-file + relevant-symbol recall, per-category, confidence intervals; fused ablation (drop TF-IDF / PDG / dense / fragment / reranker).
- [ ] **Step 2-4:** TDD. Harness indexes the corpus with a candidate profile, runs the eval split, emits JSON.
- [ ] **Step 5: Commit**

```bash
git commit -m "feat(eval): metrics harness with fused ablation"
```

---

## Task 4: Embedding candidate bake-off

**Files:** `eval/candidates.rs`

**Likely order (§9.4):** Qwen3-INT8 → EmbeddingGemma 300M → CodeRankEmbed/Jina 137M → existing SFR 400M.

- [ ] **Step 1:** Acquire/convert candidates: Qwen3-Embedding-0.6B FP16 (baseline), INT8, Q4 (where runtime supports), EmbeddingGemma 300M, CodeRankEmbed 137M, Jina v2 base-code 137M, existing SFR 400M.
- [ ] **Step 2:** Run each through the harness; record metrics + memory (host RSS + GPU VRAM) + cold/warm load + batch throughput across token-length distribution.
- [ ] **Step 3:** Apply gates. Shortlist candidates passing aggregate + protected-category gates.
- [ ] **Step 4:** Write `docs/baselines/2026-08-04-ws11-embedding-bakeoff.md` with full table.
- [ ] **Step 5: Commit**

```bash
git commit -m "docs(ws11): embedding candidate bake-off results"
```

---

## Task 5: Reranker ablation + decision

**Files:** `eval/harness.rs` (ablation), `eval/report.rs`

- [ ] **Step 1:** Evaluate independently: Qwen3 reranker baseline; compact cross-encoder candidates; **no-reranker fused retrieval**; conditional reranking (ambiguous margins only).
- [ ] **Step 2:** Measure reranker's quality contribution vs its second-model cost (spec §7 — reranker may not earn its 1.19 GiB).
- [ ] **Step 3: DECIDE** — keep / replace / remove reranker based on fused-retrieval equivalence under target resources. Record decision with evidence.
- [ ] **Step 4: Commit**

```bash
git commit -m "docs(ws11): reranker ablation + keep/replace/remove decision"
```

---

## Task 6: Quantize winner + INT8 read-path integration

**Files:** WS4 INT8 read path; model conversion artifacts.

- [ ] **Step 1:** Quantize the winning embedding model to INT8 (and Q4 if runtime-correct); validate the WS4 Task 12 INT8 SIMD read path against the winner.
- [ ] **Step 2:** Confirm the winner+profile fits the §7 budget ledger (host RSS + GPU VRAM counted).
- [ ] **Step 3:** If no candidate fits the budget at acceptable quality → **report the conflict with evidence** (§2 anti-cheat: never manufacture a passing metric). Escalate: revise the §7 target explicitly, or accept a documented quality delta.
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(ws11): quantized winner + INT8 read-path integration + budget fit"
```

---

## Task 7: Production default + rollout gating

**Files:** `src/embed/model_path.rs`, config defaults, feature flag.

- [ ] **Step 1:** Set the validated profile as the production default behind the WS4/WS6-9/WS10 feature flags (default OFF until WS12 rollout phase).
- [ ] **Step 2:** Update model-digest reporting in worker health (WS10 Task 4).
- [ ] **Step 3: Validation suite + full eval re-run** (sanity).
- [ ] **Step 4: Commit**

```bash
git commit -m "feat(ws11): validated model profile as production default (flagged off)"
```

---

## Handoff Summary (fill after execution)

```text
Workstream: WS11
Revision: 1.0
Invariant status: anti-cheat §2.1 #4/#13 — no precision/model change without gate evidence; no public-benchmark-only selection; conflicts reported not manufactured
Files changed: eval/*, src/embed/model_path.rs, config defaults
Tests run/results: [fill — every candidate + ablation]
Benchmark artifacts: docs/baselines/2026-08-04-ws11-embedding-bakeoff.md, ws11-reranker-ablation.md
TBD resolutions: embedding winner=[fill]; reranker policy=[fill]; quantization=[fill]
Decisions: [fill — each with evidence + gate pass]
Unverified assumptions: [fill — e.g., Q4 runtime correctness on MIGraphX]
Known risks: corpus bias; gates must be re-confirmed if corpus changes
Rollback: feature flag OFF = FP16 Qwen3 baseline (legacy)
Next: SP7 (WS12-13) ships the validated profile behind rollout phases
```

## Spec-coverage check (§9)

| §9 requirement | Task |
|---|---|
| §9.1 embedding candidates (7) | 4 |
| §9.1 reranker candidates (4) | 5 |
| §9.2 corpus categories | 2 |
| §9.3 metrics (Recall/MRR/nDCG/ablation) | 3 |
| §9.4 predeclared gates + variance | 1 |
| §9.4 likely eval order | 4 |
| §7 budget conflict resolution | 6 |
| §2.1 #13 no public-bench-only | all (LeIndex corpus) |

## Forbidden shortcuts (anti-cheat §2.1)

- Do NOT pick a model from public MTEB numbers without LeIndex fused-retrieval evidence (§2.1 #13).
- Do NOT reduce precision/dim/context without gate pass (§2.1 #4).
- Do NOT manufacture a passing metric if physics conflicts with budget — report it (§2, §7).
- Do NOT keep a legacy heavyweight path enabled while measuring only the optimized path (§2.1 #14).
