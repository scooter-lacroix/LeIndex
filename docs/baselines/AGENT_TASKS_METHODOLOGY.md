# Agent-Task Benchmark Methodology (W6)

This document defines the deterministic benchmark that backs the agent-task
reports in this directory:

- `2026-08-20-w6-agent-tasks.md` — internal fixture suite (3 repos, 29 tasks)
- `2026-08-20-w6-cosqa-external.md` — external validation on CoSQA real data

Implementation: `src/eval/agent_tasks.rs` (internal suite) and
`src/eval/external_suite.rs` (CoSQA), gated by
`tests/agent_tasks_benchmark_test.rs` in the ws11 report-test pattern.

## What is measured

An AI coding agent issues a query and needs the ground-truth symbol or
documentation section within its context window. Four numbers capture that
experience per backend:

| metric | definition |
|---|---|
| Recall@10 | fraction of ground-truth items in the top-10 returned |
| MRR@10 | reciprocal rank of the first ground-truth hit, capped at 10 |
| nDCG@10 | discounted gain over the top-10 (binary relevance) |
| token cost | payload characters / 4 (the 4-chars-per-token convention) |
| tool calls | calls the backend needs to answer |

Metric functions are shared with the WS11 harness (`src/eval/metrics.rs`).

## Backends

**LeIndex (deterministic lexical emulation).** The production TF-IDF
signal (`TfIdfEmbedder`, the same zero-setup path `tfidf_only` deployments
use — L2-normalized, production `tokenize_code`) fused into the composite
score shape the search tool documents: `0.45 · cosine + 0.35 ·
query-token-coverage + 0.20 · identifier-name-match`. No neural embedding
runs in this harness (it must be hermetic and CI-safe), so these numbers are
the **lexical floor** of the deployed hybrid.

**Naive baseline (deterministic naive-tools emulation).** The `ls` + `grep` +
`Read` workflow: rank files by `2·(filename token hits) + (content token
hits)`, read up to 3 whole files that have any hit, and return their symbols
or doc sections in file declaration order. Token cost is the sum of whole
file contents; tool calls are `2 + files read`.

## Internal fixture suite

Three deterministic fixture repositories exercise the failure modes agents
actually hit:

1. **leindex-self-mirror** — Rust code-intelligence service mirroring
   LeIndex's own domain (precision ingest, PDG, communities, storage), 21
   symbols incl. realistic distractors sharing query vocabulary.
2. **polyglot-checkout** — TypeScript/Python/Go checkout service
   (payments, fraud risk, shipping), 20 symbols, cross-language tasks.
3. **docs-corpus** — markdown/rst handbook with ADRs; ground truth is doc
   **sections** (`file#heading`), measuring the docs tier. Architecture
   ground truths sit deep in a 12-section document — the whole-file-read
   failure mode.

29 tasks across categories: natural-language → symbol, exact identifier,
concept, error-string → origin, and doc-section retrieval. Fixture files
carry realistic filler (headers, imports, tests) so naive's whole-file read
costs reflect production file sizes.

## External suite: CoSQA (vendored)

Internal fixtures can unconsciously favor the tool being measured. The suite
therefore also runs on **real human-annotated data**: CoSQA (ACL 2021,
Huang et al., 20,604 web-query/code pairs). A deterministic every-8th-record
subset (63 records) of the official `cosqa-retrieval-test-500.json`
retriever test split is vendored at `src/eval/corpus/cosqa/` under the C-UDA 1.0
license with full provenance (see `src/eval/corpus/cosqa/README.md`).
Each record is one query with one relevant document, so Recall@10 = hit@10.

### The wider public landscape ($0, authoritative)

| suite | what it offers | status in this repo |
|---|---|---|
| **CoSQA** (ACL 2021) | 20,604 human-annotated web queries ↔ Python code; purpose-built retriever test splits | **vendored subset in-tree**; full split usable out-of-band |
| **CodeSearchNet** (GitHub, 2019) | the canonical code-search corpus (6 languages) + 99-query challenge set with expert relevance judgments | corpus too large to vendor; out-of-band protocol below |
| **CodeXGLUE** (Microsoft) | CSN/WebQuery retrieval tasks on the above | license requires per-user download agreement; out-of-band |
| **CodeQueries** (Meta, 2022) | CodeQL-derived semantic queries over Python with positives+negatives | large; out-of-band |
| **CoSQA+** (IEEE TSE 2026) | disambiguated multi-query CoSQA extension | out-of-band; tracked |

Selection rationale: CoSQA is authoritative (human annotations, ACL), small
enough to vendor hermetically, and explicitly designed for retriever
evaluation. The others either cannot be redistributed under their terms or
are too large for a test-gated in-tree benchmark.

### Out-of-band protocol (larger suites)

For full-split or CodeSearchNet/CodeXGLUE evaluation: download the suite,
export records to `{id, query, code}` JSON, and feed them through
`external_suite::run_cosqa`-style loaders (the module isolates the runner
from the source). Results belong in a dated report here with the suite,
split, and commit recorded.

## Gates (recorded before results)

The integration test enforces, on every run:

1. Internal aggregate: LeIndex Recall@10 **strictly greater** than naive,
   MRR@10 ≥ naive.
2. Token cost: LeIndex avg tokens **strictly less** than naive (the
   token-savings claim, measured not asserted).
3. Docs tier: LeIndex doc-section Recall@10 **strictly greater** than naive
   — the docs tier must beat whole-file reads on deep sections.
4. External CoSQA: LeIndex MRR@10 ≥ naive on real queries (lexical floor
   must not lose to raw token counting).
5. Determinism: two runs produce identical aggregates.

## Head-to-head with real indexers

`tools/headtohead/headtohead.py` runs the SAME queries through REAL external
systems — the installed LeIndex binary (full hybrid), ripgrep 15 (the search
backend aider/cline/kilo/roo agents shell out to), Universal Ctags 6.2, and
Sourcegraph's zoekt (both its native AND-keyword mode and OR-any-token parity
mode) — over two corpora: the materialized CoSQA subset and the LeIndex
repository itself. Identical metric formulas; tokens = chars/4.

Results live in the dated `docs/baselines/*-headtohead-indexers.md` report
(regenerate with `python3 tools/headtohead/headtohead.py`; external binaries
are optional — missing systems are skipped and recorded). The harness is
informational evidence, not CI-gated: the gated comparison remains the
LeIndex-vs-naive baseline above, and external rankings depend on locally
installed tool versions.

Representative findings (2026-08-21 run): LeIndex led every system on both
corpora — CoSQA 0.921 recall@10 / 0.702 MRR vs ripgrep 0.857/0.633, ctags
0.603/0.392, zoekt-AND 0.000 (long natural-language ANDs match nothing),
zoekt-OR 0.079 (disjunctions flood its ranking); repo corpus 0.833/0.533 at
~310 tokens per query vs zoekt-AND 0.750/0.438 at ~14.6k tokens, ctags
0.750/0.381 at ~73k, ripgrep 0.417/0.069 at ~44k. zoekt OR-mode on the repo
averaged ~1.47M tokens per query — the disjunction flood made nearly every
file a hit.

## Known limitations

- Lexical-only: the deployed hybrid (neural embeddings + reranker) is
  deliberately not exercised — that comparison is the WS11 bake-off's job.
- Cost models are analytic (chars/4, fixed snippet sizes), not wall-clock
  tokenization of a specific model.
- CoSQA is Python-only; internal fixtures cover the multi-language surface.
