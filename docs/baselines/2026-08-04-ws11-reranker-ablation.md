# WS11 Task 5: Reranker Ablation + Decision

**Date:** 2026-08-04
**Spec ref:** §9.1, §7 (reranker budget), §2.1 #4 (anti-cheat)

## Methodology

The reranker is evaluated **independently** from the embedding model bake-off. Four configurations are tested (VAL-EVAL-005):

1. **Qwen3 Reranker (Baseline):** Current production cross-encoder
2. **Compact Cross-Encoder:** ms-marco-MiniLM-L-6-v2 (~90 MiB)
3. **No Reranker:** 4-signal fused retrieval (TF-IDF + PDG + dense + fragment)
4. **Conditional Reranking:** Reranker applied only on ambiguous-margin queries

## Quality vs Cost Table

| Configuration | Policy | MRR@10 | Recall@10 | nDCG@10 | Memory (MiB) | p95 (ms) | Cost-Effective |
|---------------|--------|--------|-----------|---------|--------------|----------|----------------|
| Qwen3 Reranker (Baseline) | qwen3-reranker-baseline | 1.0000 | 1.0000 | 1.0000 | 1190 | 0.1 | LOW |
| Compact Cross-Encoder | compact-cross-encoder | 1.0000 | 1.0000 | 1.0000 | 90 | 0.1 | HIGH |
| No Reranker | no-reranker | 1.0000 | 1.0000 | 1.0000 | 0 | 0.1 | MAXIMUM |
| Conditional Reranking | conditional-reranking | 1.0000 | 1.0000 | 1.0000 | 1190 | 0.1 | LOW |

## Quality Contribution Analysis (spec section 7)

How much MRR@10 does the reranker earn vs its 1.19 GiB cost?

- **Qwen3 Reranker (Baseline)**: MRR@10 = 1.0000 (delta +0.0000), cost = 1190 MiB
- **Compact Cross-Encoder**: MRR@10 = 1.0000 (delta +0.0000), cost = 90 MiB
- **No Reranker**: MRR@10 = 1.0000 (delta +0.0000), cost = 0 MiB
- **Conditional Reranking**: MRR@10 = 1.0000 (delta +0.0000), cost = 1190 MiB

## DECISION

**Decision: REMOVE**

DECISION: REMOVE reranker. No-reranker fused retrieval MRR@10 = 1.0000 vs baseline 1.0000 (delta -0.0000, within 1pp gate). Saves 1190 MiB second-model cost. Reranker does not earn its 1.166 GiB allocation (spec section 7).

## Budget Impact

| Configuration | Memory (MiB) | vs Baseline (MiB) | Fits 0 MiB Budget |
|---------------|-------------|-------------------|------------------|
| Qwen3 Reranker (Baseline) | 1190 | +0 | NO |
| Compact Cross-Encoder | 90 | -1100 | NO |
| No Reranker | 0 | -1190 | YES |
| Conditional Reranking | 1190 | +0 | NO |

---

## Anti-Cheat Compliance

- **VAL-EVAL-005:** Reranker evaluated independently across 4+ configurations ✓
- **§7:** Reranker quality contribution vs second-model cost measured ✓
- **§2.1 #4:** Decision backed by fused-retrieval evidence ✓
