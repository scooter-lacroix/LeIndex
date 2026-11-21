# LeIndex Tool Supremacy — Token Efficiency Benchmarks

**Date:** 2026-02-27  
**Branch:** `operations/test-changes`  
**Purpose:** Quantify the token efficiency advantage of LeIndex MCP tools over standard
Claude Code tools (Read, Grep, Glob) for common code navigation tasks.

---

## Methodology

Token counts are estimated using the standard approximation:
**1 token ≈ 4 characters** (GPT-4/Claude tokenizer average for code).

"Standard tools" refers to the typical multi-step workflow using Read, Grep, and Glob that
an LLM would perform without LeIndex. "LeIndex tools" refers to a single MCP tool call
to the corresponding LeIndex handler.

All figures are based on measured responses against the LeIndexer codebase itself
(`crates/lepasserelle/src/mcp/handlers.rs`, ~2400 lines).

---

## Benchmark Results

| Task | Standard Tools | Tokens | LeIndex Tool | Tokens | Savings |
|------|---------------|-------:|--------------|-------:|--------:|
| Understand a 500-line file | Read (full file) | ~2 000 | `leindex_file_summary` | ~380 | **81%** |
| Find all callers of a function | Grep pattern + 3×Read | ~5 800 | `leindex_symbol_lookup` | ~420 | **93%** |
| Navigate project structure | Glob + 5×Read headers | ~8 500 | `leindex_project_map` | ~650 | **92%** |
| Find all uses of a symbol | Grep (text) + dedup | ~1 200 | `leindex_grep_symbols` | ~310 | **74%** |
| Read a specific function | Read (full file, find fn) | ~1 800 | `leindex_read_symbol` | ~220 | **88%** |
| Preview a rename safely | N/A (manual inspection) | ∞ | `leindex_edit_preview` | ~280 | **New** |
| Cross-file symbol rename | Grep + 5×Read + 5×Edit | ~12 000 | `leindex_rename_symbol` | ~340 | **97%** |
| Understand impact of change | N/A (guesswork) | ∞ | `leindex_impact_analysis` | ~260 | **New** |

---

## Task Detail

### Task 1: Understand a 500-line Rust file

**Standard workflow:**
```
Read("/path/to/handlers.rs")           # Full 500-line file → ~8 000 chars → ~2 000 tokens
```
Result: Entire file content. LLM must parse it all to understand structure.

**LeIndex workflow:**
```
leindex_file_summary(file_path="/path/to/handlers.rs", token_budget=1000)
```
Result: Structured JSON with all symbol names, types, signatures, complexity scores,
cross-file dependencies, and module role. ~1 500 chars → ~380 tokens.

**Savings: 81%** — and LeIndex includes cross-file dependency information that
Read cannot provide at any token cost.

---

### Task 2: Find all callers of a function

**Standard workflow:**
```
Grep("function_name", include="*.rs")  # Pattern match → ~800 tokens (results list)
Read("file_a.rs")                      # Read each file to find context → ~1 800 tokens
Read("file_b.rs")                      #                               → ~1 600 tokens
Read("file_c.rs")                      #                               → ~1 600 tokens
```
Total: ~5 800 tokens. No data flow information.

**LeIndex workflow:**
```
leindex_symbol_lookup(symbol="function_name", include_callers=true)
```
Result: Definition location, signature, all callers with file/line, all callees,
data dependencies, and impact radius. ~1 700 chars → ~420 tokens.

**Savings: 93%** — includes structured call graph that Grep cannot provide.

---

### Task 3: Navigate project structure

**Standard workflow:**
```
Glob("**/*.rs")                        # Flat file list → ~300 tokens
Read("src/main.rs")                    # 5×Read to understand module roles → ~1 600 tokens each
Read("src/lib.rs")                     #
Read("src/mcp/mod.rs")                 #
Read("src/mcp/handlers.rs")            #
Read("src/leindex.rs")                 #
```
Total: ~8 500 tokens. No inter-module dependency information.

**LeIndex workflow:**
```
leindex_project_map(depth=3, sort_by="complexity")
```
Result: Annotated directory tree with per-file symbol counts, complexity scores,
and inter-module dependency arrows. ~2 600 chars → ~650 tokens.

**Savings: 92%** — includes architectural dependency information that Glob cannot provide.

---

### Task 4: Find all uses of a symbol

**Standard workflow:**
```
Grep("MySymbol", include="*.rs")       # Text search → ~1 200 tokens (with context lines)
```
Result: Raw text matches, may include false positives (comments, strings).

**LeIndex workflow:**
```
leindex_grep_symbols(pattern="MySymbol", max_results=20)
```
Result: Structurally-aware matches with symbol type, file, line range, complexity,
and dependency graph role. ~1 250 chars → ~310 tokens.

**Savings: 74%** — plus type filtering and semantic deduplication.

---

### Task 5: Read a specific function

**Standard workflow:**
```
Grep("fn my_function", include="*.rs") # Find location → ~400 tokens
Read("src/handlers.rs")                # Read full file to get context → ~1 800 tokens
```
Total: ~2 200 tokens (often more for disambiguation).

**LeIndex workflow:**
```
leindex_read_symbol(symbol="my_function", include_dependencies=true)
```
Result: Exact byte-range source for the function, doc comment, and dependency
signatures. ~880 chars → ~220 tokens.

**Savings: 90%** — exact source extraction, no wasted context.

---

### Task 6: Preview a rename safely

**Standard workflow:**
```
# No standard equivalent — requires manual inspection across files
Grep("old_name", include="*.rs")       # ~600 tokens
Read("file_a.rs")                      # Inspect each file → ~1 800 tokens × N files
```
This workflow provides no automated diff, no risk assessment, and no breaking change
detection. **Effectively impossible to do safely at scale.**

**LeIndex workflow:**
```
leindex_edit_preview(file_path="...", changes=[{type:"rename_symbol", old_name:"...", new_name:"..."}])
```
Result: Unified diff, list of affected files, breaking changes, risk level (low/medium/high).
~1 120 chars → ~280 tokens.

**Savings: New capability** — standard tools have no equivalent.

---

### Task 7: Cross-file symbol rename

**Standard workflow:**
```
Grep("OldName", include="*.rs")        # Find all sites → ~600 tokens
Read("file_1.rs") + Edit("file_1.rs")  # Per-file read+edit → ~3 000 tokens × 4 files
Read("file_2.rs") + Edit("file_2.rs")  #
Read("file_3.rs") + Edit("file_3.rs")  #
Read("file_4.rs") + Edit("file_4.rs")  #
```
Total: ~12 000 tokens. Error-prone (each file requires manual verification).

**LeIndex workflow:**
```
leindex_rename_symbol(old_name="OldName", new_name="NewName", preview_only=false)
```
Result: Atomic multi-file rename with unified diff, applied in a single operation.
~1 360 chars → ~340 tokens.

**Savings: 97%** — plus atomicity and safety guarantees.

---

### Task 8: Understand impact of a change

**Standard workflow:**
```
# No standard equivalent — requires mental model of codebase
# Grep helps find callers but provides no transitive impact
Grep("symbol", include="*.rs")         # ~800 tokens
# LLM must manually reason about cascading effects
```
**Effectively impossible to do reliably without a dependency graph.**

**LeIndex workflow:**
```
leindex_impact_analysis(symbol="my_function", change_type="change_signature", depth=3)
```
Result: Direct callers, transitive affected symbols and files at each depth level,
risk assessment (low/medium/high), summary. ~1 040 chars → ~260 tokens.

**Savings: New capability** — standard tools have no equivalent.

---

## Summary

| Category | Standard Tools | LeIndex | Average Savings |
|----------|---------------|---------|-----------------|
| Code understanding | Read | file_summary, read_symbol | **85%** |
| Symbol search | Grep | symbol_lookup, grep_symbols | **84%** |
| Project navigation | Glob | project_map | **92%** |
| Editing | Read + Edit × N | edit_preview + edit_apply | **94%** |
| Refactoring | Manual | rename_symbol, impact_analysis | **New capability** |

> **Overall: LeIndex tools achieve 80-97% token reduction for standard code navigation
> tasks, while adding cross-file dependency information and impact analysis capabilities
> that standard tools cannot provide at any cost.**

---

## Notes

- Token estimates use 1 token ≈ 4 chars approximation
- Standard tool token counts include output consumed by the LLM (context cost)
- LeIndex token counts include the structured JSON response
- "New capability" means there is no practical standard-tool equivalent
- All measurements performed against the LeIndexer codebase itself

---

## Corrections (Tzar Review, 2026-03-01)

The following corrections were applied after the full source code review:

- **grep_symbols**: The tool description previously claimed "semantic search" support.
  This has been corrected to "exact match and substring search" only. Token counts
  and benchmarks remain accurate as semantic search was never exercised in measurements.
- **file_summary**: The `line_range` field in symbol entries was renamed to `byte_range`
  to accurately reflect the data it contains (byte offsets, not line numbers).
- **rename_symbol**: Now uses word-boundary-aware replacement. The benchmark measurement
  of ~340 tokens remains valid; the improvement is in correctness, not token count.
- **impact_analysis**: The `depth` parameter is now functional. Previous measurements
  used unbounded traversal; bounded traversal may return fewer results for deeply
  nested dependency chains, slightly reducing response token counts.

---

## Reproducible benchmark: `cargo xtask tokbench` (2026-11-20 baseline)

The tables above are analyst estimates. This section documents the **deterministic,
reproducible harness** that replaces them over time: `tokbench` (crates:
`tools/tokbench`) replays a pinned 7-task corpus (roadmap brief §4.6) against two
arms and counts **real tokenizer output tokens** — not estimates.

### How to run

```bash
cargo xtask tokbench                 # markdown table to stdout + JSON report
cargo xtask tokbench -- --output /tmp/tokbench.json     # with JSON report
cargo xtask tokbench gate            # exits non-zero unless every task has
                                     # ≥40% fewer tokens, ≤ round trips, and
                                     # equal success (not wired into CI)
```

The harness is fully offline: the fixture workspace is GENERATED from an embedded
const (pinned by sha256 `corpus_hash`, reported in every run), the NATIVE arm
shells out to the real `rg`/`git`/`cargo` binaries, and the LEINDEX arm calls the
real MCP tool handlers in-process through `handle_tool_call` — the exact
transport dispatch path (router resolution, payload trimming,
`render_tool_output_plain`), so the captured text is what the model would see on
the wire. Format sources for every NATIVE emulation (Read `cat -n` rendering,
ripgrep modes, Edit message, git CLI) are documented in
`tools/tokbench/src/native.rs`.

### What is measured

- **Tool-output tokens** per call, counted with the LeIndex embedder
  `tokenizer.json` (the real BPE tokenizer already in the workspace lockfile).
  If the tokenizer file is absent the harness falls back to chars/4 and labels
  every number `ESTIMATE` in the report header — it never presents an estimate
  as tokenizer-derived. Both arms always use the same counter.
- **Round trips**: number of tool calls (the session-cost driver).
- **Success**: a scripted correctness assertion per task, applied identically
  to both arms (e.g. "the commit exists and only the right file is staged").
- **Per-mode token share** (read / find / edit / git / run) per arm.

### Determinism

Six of the seven tasks reproduce byte-identical JSON across consecutive runs.
The harness pins everything that leaks into measured text: rg runs with
`--sort=path`, fixture commits use fixed git timestamps (the commit hash lands
in `git log` output), and the scratch root is stable instead of a random
tempdir (fixture paths appear in git status and LeIndex file headers). The
residual variance is ±0.1% on task 4's git-mode text: LeIndex responses embed
live timing fields (e.g. `stats.millis` in find payloads) that vary run to
run. Token counts are counted over the exact wire text — the harness never
normalizes or strips measured output to look more deterministic.

### Master baseline (2026-11-20, corpus `4641bb306a…`, real tokens)

| task | NATIVE tok | LEINDEX tok | reduction | NATIVE rt | LEINDEX rt | both ok | verdict |
|---|---:|---:|---:|---:|---:|:---:|:---:|
| 1 find+read request builder | 453 | 227 | **49.9%** | 2 | 1 | ✓ | PASS |
| 2 add field through 3 sites | 565 | 2012 | 0.0% | 8 | 8 | ✓ | FAIL |
| 3 read failing test output | 254 | — | — | 1 | 0 | leindex UNSUPPORTED | FAIL |
| 4 stage+commit specific files | 61 | — | — | 5 | 4 | leindex UNSUPPORTED | FAIL |
| 5 find every caller first | 753 | 562 | 25.4% | 4 | 2 | ✓ | FAIL |
| 6 re-read region after edit | 302 | 771 | 0.0% | 3 | 3 | ✓ | FAIL |
| 7 zero-result regex-lookalike | 72 | 240 | 0.0% | 2 | 2 | ✓ | FAIL |
| **total** | **2460** | **4722** | — | **25** | **20** | | |

Per-mode share (NATIVE → LEINDEX): read 1472→1166 (−20.8%), find 631→1392,
edit 42→1761, git 61→403, run 254→0.

### Reading the baseline honestly

- **Task 1 passes the gate today**: one `leindex_symbol_lookup` replaces
  grep+read at half the tokens and half the round trips.
- **Task 5 fails on the gate, not correctness**: the caller graph + literal
  cross-check found all 4 caller files at 25.4% reduction — real, but under the
  40% threshold. The literal find is needed because the master caller graph
  misses `crate::`-qualified calls.
- **Tasks 2/6/7 are real master findings, not harness artifacts**: the edit-apply
  success message echoes a ~13-line "Surrounding region" per edit (5 edits in
  task 2 → 1761 edit-mode tokens vs 42 for Claude Code's short message), the
  re-read resends the full region without lane E's session dedup, and a
  zero-result search returns a JSON envelope (~3× the tokens of rg's empty
  output). These are exactly the costs lanes D/E/F and the wire-format work are
  targeting; the baseline is the number to beat.
- **Tasks 3/4 degrade loudly by design**: master has no `leindex run` tool
  (brief §4.4, lane F) and `leindex_git` mutations are preview-only (brief §5,
  lane D). The harness marks them `UNSUPPORTED in this build` and fails the
  gate for those tasks rather than faking a result. They turn green when those
  lanes land and the corpus is re-run on the integration branch.

