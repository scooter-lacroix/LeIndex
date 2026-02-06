# 5-Phase Token Efficiency Benchmark (Public)

Date: 2026-02-05

This benchmark is intended for non-technical decision-makers who need to understand practical cost/latency impact.

> Note: this document benchmarks one LeIndex mode (5-phase triage). It does not replace core LeIndex capabilities (index/search/analyze/diagnostics/MCP).
---

## Scenario

Goal: quickly understand high-impact code areas in a large repository without flooding an LLM context window.

Repository analyzed: this LeIndex repository
- Files parsed: **1,974**

---

## Method A — LeIndex 5-phase summary

Command:

```bash
leindex phase --all --path . --mode balanced --max-chars 12000
```

Observed:
- Runtime: **18.647s**
- Output size: **473 characters**
- Approx tokens: **~118** (chars/4 approximation)

---

## Method B — Grep + manual file triage

Representative triage flow:

1. Run 3 grep/ripgrep passes for key symbols and integration points.
2. Open 15 highest-hit files.
3. Read first ~220 lines of each file.

Observed:
- Grep output: **24,055 chars (~6,014 tokens)**
- Manual-read snippet volume: **81,034 chars (~20,258 tokens)**
- Combined: **105,089 chars (~26,272 tokens)**

---

## Result

| Method | Approx Tokens |
|---|---:|
| 5-phase summary | ~118 |
| Grep + manual triage | ~26,272 |

Token reduction from 5-phase-first workflow:

- **~26,154 tokens saved**
- **~99.55% less token volume before deep analysis**

---

## Does this replace manual reading?

No.

- **Better than manual-first** for triage speed, breadth, and consistency.
- **Worse than manual reading** for final intent interpretation and nuanced edge cases.

Best workflow:
1. Run 5-phase.
2. Use phase focus files/hotspots to choose where to read manually.
3. Perform final code-level verification by human review.

---

## Why this matters urgently

LLM workflows fail when context is bloated early.

The 5-phase-first pattern keeps context compact, allowing:
- lower token spend,
- faster answer cycles,
- less analyst fatigue,
- fewer missed high-risk files.

For teams running repeated impact investigations daily, this is not incremental — it is operationally significant.
