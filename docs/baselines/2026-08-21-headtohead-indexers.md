# Head-to-Head: LeIndex vs Real Indexers (shared corpus, shared metrics)

Every system ran the same queries over the same corpus directories; recall@10 (= hit@10) and MRR@10 use the same formulas as the in-tree eval harness. Tokens = chars/4 (rg/ctags/zoekt read top-3 files whole; leindex returns top-10 snippets).

## CoSQA subset (63 real web queries, Python)

| system | recall@10 | MRR@10 | avg tokens | queries |
|---|---:|---:|---:|---:|
| leindex (full hybrid, real binary) | 0.921 | 0.702 | 317 | 63 |
| ripgrep 15 (agent backend: aider/cline/kilo/roo) | 0.857 | 0.633 | 250 | 63 |
| universal-ctags 6.2 (symbol index) | 0.603 | 0.392 | 172 | 63 |
| zoekt (Sourcegraph, native AND keywords) | 0.000 | 0.000 | 555 | 63 |
| zoekt (Sourcegraph, OR-any-token parity semantics) | 0.079 | 0.015 | 1253 | 63 |

## LeIndex repository (real large repo, Rust)

| system | recall@10 | MRR@10 | avg tokens | queries |
|---|---:|---:|---:|---:|
| leindex (full hybrid, real binary) | 0.833 | 0.533 | 310 | 12 |
| ripgrep 15 (agent backend: aider/cline/kilo/roo) | 0.417 | 0.069 | 43727 | 12 |
| universal-ctags 6.2 (symbol index) | 0.750 | 0.381 | 73084 | 12 |
| zoekt (Sourcegraph, native AND keywords) | 0.750 | 0.438 | 14561 | 12 |
| zoekt (Sourcegraph, OR-any-token parity semantics) | 0.083 | 0.012 | 1474876 | 12 |

Notes: ripgrep/ctags rankings are the standard agent emulations (token bookkeeping over boolean matches, path-sorted ties); zoekt applies its own ranking. LeIndex ran the installed production binary with the full hybrid stack.
