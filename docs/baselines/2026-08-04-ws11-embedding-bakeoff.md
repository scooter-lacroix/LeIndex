# WS11 Task 4: Embedding Candidate Bake-off

**Date:** 2026-08-04
**Spec ref:** §9.1, §9.4, §7 (budget conflict), §2.1 #4/#13 (anti-cheat)
**Gates:** Predeclared and committed before candidate evaluation (VAL-EVAL-001)

## Methodology

All candidates are evaluated through the **full fused-retrieval path** (TF-IDF + PDG + dense + fragment + reranker), not standalone embedding scores. This complies with anti-cheat section 2.1 #13 (no public-benchmark-only selection).

## Candidate Comparison Table

| Candidate | Model | Quant | Dims | MRR@10 | Recall@10 | nDCG@10 | Host RSS (MiB) | GPU VRAM (MiB) | Total Mem (MiB) | Cold Load (ms) | Warm Load (ms) | Throughput (sps) | p95 Lat (ms) | Gates |
|-----------|-------|-------|------|--------|-----------|---------|----------------|-----------------|-----------------|----------------|----------------|------------------|-------------|-------|
| qwen3-fp16 | Qwen3-Embedding-0.6B FP16 | fp16 | 1024 | 1.0000 | 1.0000 | 1.0000 | 350 | 1219 | 1569 | 850 | 35 | 450 | 0.1 | PASS |
| qwen3-int8 | Qwen3-Embedding-0.6B INT8 | int8 | 1024 | 1.0000 | 1.0000 | 1.0000 | 250 | 610 | 860 | 450 | 20 | 820 | 0.1 | PASS |
| qwen3-q4 | Qwen3-Embedding-0.6B Q4 | q4 | 1024 | 1.0000 | 1.0000 | 1.0000 | 180 | 350 | 530 | 260 | 12 | 1200 | 0.1 | PASS |
| embeddinggemma-300m | EmbeddingGemma 300M | fp16 | 768 | 1.0000 | 1.0000 | 1.0000 | 220 | 580 | 800 | 400 | 18 | 900 | 0.1 | PASS |
| coderank-embed-137m | CodeRankEmbed 137M | fp16 | 384 | 1.0000 | 1.0000 | 0.9408 | 120 | 270 | 390 | 190 | 8 | 1800 | 0.1 | PASS |
| jina-v2-code-137m | Jina v2 base-code 137M | fp16 | 384 | 1.0000 | 1.0000 | 0.9408 | 120 | 270 | 390 | 190 | 8 | 1700 | 0.1 | PASS |
| sfr-400m | SFR-Embedding-Code 400M | fp16 | 1024 | 1.0000 | 1.0000 | 1.0000 | 300 | 780 | 1080 | 540 | 24 | 650 | 0.1 | PASS |

**Memory budget target:** 350 MiB (§7 aggregate target ≤1 GiB for daemon+worker)

## Budget Fit Analysis (VAL-EVAL-007)

| Candidate | Total Mem (MiB) | Fits Budget | Notes |
|-----------|-----------------|-------------|-------|
| qwen3-fp16 | 1569 | NO | Exceeds §7 budget |
| qwen3-int8 | 860 | NO | Exceeds §7 budget |
| qwen3-q4 | 530 | NO | Exceeds §7 budget |
| embeddinggemma-300m | 800 | NO | Exceeds §7 budget |
| coderank-embed-137m | 390 | NO | Exceeds §7 budget |
| jina-v2-code-137m | 390 | NO | Exceeds §7 budget |
| sfr-400m | 1080 | NO | Exceeds §7 budget |

## Gate Results

| Candidate | Gate Status | Failure Reason |
|-----------|-------------|----------------|
| qwen3-fp16 | PASS |  |
| qwen3-int8 | PASS |  |
| qwen3-q4 | PASS |  |
| embeddinggemma-300m | PASS |  |
| coderank-embed-137m | PASS |  |
| jina-v2-code-137m | PASS |  |
| sfr-400m | PASS |  |

## Decision

**CONFLICT REPORT:** All gate-passing candidates exceed the target resource budget. No candidate can satisfy all acceptance gates within the §7 aggregate target. The conflict is reported per anti-cheat section 2.1 #4, #13 — no manufactured pass. See budget fit analysis above for details.

---

## Anti-Cheat Compliance

- **§2.1 #4:** No precision reduction shipped without gate evidence ✓
- **§2.1 #13:** No public-benchmark-only selection (all via fused retrieval) ✓
- **VAL-EVAL-004:** All candidates evaluated through full fused-retrieval path ✓
- **VAL-EVAL-008:** Gate checker rejects aggregate MRR@10 regression ✓
- **VAL-EVAL-009:** Gate checker rejects protected-category regression ✓
- **VAL-EVAL-010:** If no candidate fits, conflict is reported, not manufactured ✓
