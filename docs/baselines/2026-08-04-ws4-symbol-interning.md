# WS4 Symbol String Interning — Decision

**Task:** WS4 Task 13 (TBD resolution, spec §10 item 3)
**Date:** 2026-08-05
**Status:** DECISION: adopt mmap'd symbol-string interning table; keep `symbols` as a separate blob (not folded into `pdg`)

## Context

The PDG/symbol layers store a symbol record per indexed node. Each record
carries a `symbol_name` and `file_path` String. In a naive layout every record
would embed its full string inline; because many nodes share the same name
(e.g. common helper/identifiers across files) and — overwhelmingly — the same
file path (dozens of symbols per file), the naive layout **repeats** those
strings. WS4 Task 13 measured this repeated-String duplication across the
real catalog and adopted an mmap'd string-interning table **iff** duplication
`>= 20%` of the PDG blob size.

## Measurement (this repo's real catalog)

Counted across the live migration source (the legacy catalog mirrored into the
CAS Db blob — 28,734 `intel_nodes`), which is exactly the dataset the `Symbols`
layer encodes. The interner interns both `symbol_name` and `file_path` (see
`migrate.rs::encode_symbols_layer`).

| String field | Total raw bytes (inline, repeated) | Deduplicated bytes (interned once) | Duplication |
|---|---|---|---|
| `symbol_name` (28,734 rows, 9,179 distinct) | 467,509 | 224,161 | 243,348 |
| `file_path` (28,734 rows, 418 distinct) | 1,920,319 | 34,964 | 1,885,355 |
| **combined** | **2,387,828** | **259,125** | **2,128,703** |

Blob sizes (generation 627):
- **PDG blob: 2,732,598 B**
- Symbols blob: 1,025,606 B

### Gate check (`duplication >= 20% of PDG blob size`)
- 20% of PDG blob = `0.20 × 2,732,598 = 546,520 B`.
- Measured duplication = **2,128,703 B = 77.9% of the PDG blob size** (`file_path`
  alone contributes 1,885,355 B = 69.0%).
- `2,128,703 B ≥ 546,520 B` → **gate passes** by a margin of ~3.9×.

## Decision 1 — adopt mmap'd interning

**Adopt the mmap'd symbol-string interning table.** The measured duplication
(77.9% of the PDG blob size) far exceeds the 20% adoption gate. The generated
`Symbols` layer therefore stores a compact string table (each unique string
once) plus a fixed-size per-symbol record of small integer IDs (`name_id`,
`file_path_id`) — see `migrate.rs::encode_symbols_layer` and the mmap'd
`SymbolReader`. This is the design already shipped in WS4 Tasks 5–6 and Task 10.

## Decision 2 — symbols: separate blob, not folded into pdg

**Keep `symbols` as a separate blob (`LIDX-SYM1`), not folded into `pdg`
(`LIDX-PDG1`).**

Rationale:
- **Independent access.** Symbol lookup/inventory search (`SymbolReader`) is
  served from the symbol inventory alone; folding it into `pdg` would force a
  reader to touch/validate the whole graph for every symbol query.
- **Independent lifecycle + content-addressed dedup.** A separate blob lets the
  CAS content-hash and retain each layer independently — symbol metadata and
  the PDG graph change at different rates during re-index.
- **Independent mmap + validation.** Each layer blob carries its own content
  hash and is validated/mapped separately, keeping `ReaderError::BadHeader`
  checks scoped per layer.

The two are related by node id (the symbol records in the graph reference the
same nodes), but they are stored and served as distinct blobs.

## Rollback / revisit

The threshold is a decision-time gate, not a runtime assertion. If a future
corpus had near-unique symbol strings (duplication `< 20%`), the interner could
be bypassed with an inline-string encoding while keeping the same `SymbolReader`
interface. The `Symbols`-vs-`pdg` split is a one-time structural decision; no
revisit is triggered by the current data.
