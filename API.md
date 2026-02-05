# LeIndex API Reference (CLI + MCP)

Last updated: 2026-02-05

This is the user-facing API for LeIndex’s Rust runtime, including the new **5-phase analysis system**.

---

## 1) CLI commands

## `leindex index <path>`
Index a project.

```bash
leindex index /path/to/project
```

Options (common in current builds):
- `--force` reindex from scratch

---

## `leindex search <query>`
Semantic search.

```bash
leindex search "where is request validation done"
```

---

## `leindex analyze <query>`
Deep analysis with context expansion.

```bash
leindex analyze "how does auth token refresh work"
```

---

## `leindex diagnostics`
Runtime/index health and statistics.

```bash
leindex diagnostics
```

---

## `leindex phase` (new)
Run additive 5-phase analysis.

### Full analysis
```bash
leindex phase --all --path /path/to/project
```

### Single phase
```bash
leindex phase --phase 2 --path /path/to/project
```

### Important options
- `--mode ultra|balanced|verbose`
- `--max-files <n>`
- `--max-focus-files <n>`
- `--top-n <n>`
- `--max-chars <n>`
- `--include-docs` + `--docs-mode off|markdown|text|all`
- `--no-incremental-refresh`

---

## `leindex mcp` / `leindex serve`
Run MCP server interfaces.

```bash
leindex mcp
# or
leindex serve --host 127.0.0.1 --port 47268
```

---

## 2) MCP tools

Core tools:
- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`

5-phase tools:
- `leindex_phase_analysis`
- `phase_analysis` (alias)

---

## 3) MCP phase-analysis schema

Request shape (conceptual):

```json
{
  "phase": 1,
  "path": "/abs/or/relative/project/path",
  "mode": "balanced",
  "max_files": 2000,
  "max_focus_files": 20,
  "top_n": 10,
  "max_chars": 12000,
  "include_docs": false,
  "docs_mode": "off"
}
```

`phase` accepts `1..5` or `"all"`.

---

## 4) 5-phase report semantics

Response includes:
- project id + generation hash
- executed phases
- cache-hit indicator
- changed/deleted counts
- per-phase summaries
- compact human-readable `formatted_output`

Designed for LLM-friendly triage: concise, structured, and low-token.

---

## 5) Token efficiency benchmark snapshot

Measured on a 1,974-file repository:

- `leindex phase --all` output: **473 chars (~118 tokens)**
- Typical grep/manual triage sample: **105,089 chars (~26,272 tokens)**

Approx reduction: **~99.55%** before deep manual review.

---

## 6) Practical guidance

- Use `phase --all` first when starting analysis.
- Use `search` and `analyze` for targeted deep dives.
- Use manual file reading for final correctness decisions.

LeIndex is best used as a **scope compressor** before expensive LLM context expansion.
