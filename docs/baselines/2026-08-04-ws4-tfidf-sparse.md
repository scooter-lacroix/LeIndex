# WS4 Sparse TF-IDF — Dense vs Sparse — Decision

**Task:** WS4 Task 13 (TBD resolution, spec §10 item 3)
**Date:** 2026-08-05
**Status:** DECISION: adopt sparse TF-IDF storage (CSR, term-indexed)

## Context

The read model exposes TF-IDF scores per document and term (`TfidfReader`).
The pre-TBD design stored TF-IDF as a **dense** `docs × terms` matrix of f32
weights. WS4 Task 13 measured dense-vs-sparse on this repo and adopted sparse
storage **iff** two gates both hold: (1) serialized size shrinks `>= 30%`, and
(2) the query-suite top-10 ranking is identical between the two
representations.

> Note on the migrated store: the legacy catalog carries no per-document TF-IDF
> vector table (dense query vectors live in the Neural layer as embeddings;
> legacy kept only the vocab + IDF). WS4 Task 10 therefore staged an empty
> TF-IDF layer. The dense-vs-sparse comparison below is therefore measured by
> computing TF-IDF directly over this repository's own source corpus (333 Rust
> source files, one document per file) — the authentic "on this repo" signal
> for a term–document matrix.

## Measurement (this repo's source corpus)

Corpus = 333 Rust source files under `src/`, `benches/`, `tests/` (one document
per file). Tokenization = lower-cased identifier/word tokens
`[A-Za-z_][A-Za-z0-9_]*`. TF-IDF weight `w = (1 + ln(tf)) · idf`, with
`idf = ln((N+1)/(df+1)) + 1`. Serialized-size model uses f32 weights (4 B).

| Quantity | Value |
|---|---|
| documents (files) | 333 |
| vocabulary terms | 17,354 |
| matrix cells (`docs × vocab`) | 5,778,882 |
| non-zero entries | 109,295 |
| sparsity | 98.11% |
| **dense bytes** (f32, full matrix) | **23,115,528 B (≈23.1 MB)** |
| **sparse bytes** (CSR per-nnz doc/term/weight + row offsets) | **1,312,876 B (≈1.3 MB)** |
| **size reduction** | **94.32%** |
| sparse as % of dense | 5.68% |
| query-suite top-10 matches (dense vs sparse) | **120 / 120 identical** |

### Size gate (`>= 30%`)
Sparse is 23.1 MB → 1.3 MB: a **94.32% reduction** (~17.6× smaller), far above
the 30% threshold. This is sustained across corpora because source-code
term–document matrices are ~98% empty (only the terms actually present in a
file have non-zero weights).

### Correctness gate (identical top-10)
A fixed query suite of 12 high-document-frequency terms, each ranked across all
333 documents by TF-IDF weighted dot product against the query. The sparse
(CSR) representation is **exactly equivalent** to the dense one (same matrix
values, same arithmetic), so the top-10 lists are bit-identical: **120/120
matched**.

## Implementation notes

Sparse TF-IDF is stored as a CSR-style array of `(doc_id u32, term_id u32,
weight f32)` entries (12 bytes per non-zero) plus a per-document row-offset
array — exactly the layout the `TfidfReader` mmaps. A term dictionary maps
term → id; the idf vector is stored once. This avoids touching the 17k × 333
dense slab on every read while preserving exact score semantics.

## Decision

**Adopt sparse (CSR) TF-IDF storage.**

Both adoption gates are met decisively: size shrinks **94.32%** (`≥ 30%`
required) and the query-suite top-10 ranking is **identical** (120/120). Sparse
storage becomes the default layout for the `LIDX-TFID` layer written by the
WS4 writer.
