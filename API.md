# LeIndex API Reference (Current Rust Runtime)

Last updated: 2026-02-05

This reference covers the full LeIndex surface: indexing, search, deep analysis, diagnostics, MCP integration, and additive 5-phase triage.

---

## 1) CLI surface

## `leindex index <path>`
Index a project for search/analysis.

```bash
leindex index /path/to/project
```

---

## `leindex search <query>`
Semantic search across indexed code.

```bash
leindex search "where retries are handled"
```

---

## `leindex analyze <query>`
Deep analysis with expanded context.

```bash
leindex analyze "how connection pooling works"
```

---

## `leindex diagnostics`
Inspect runtime/index health.

```bash
leindex diagnostics
```

---

## `leindex phase` (additive triage mode)
Run structured 5-phase analysis for scoped impact mapping.

```bash
# Full run
leindex phase --all --path /path/to/project

# Single phase
leindex phase --phase 4 --path /path/to/project
```

Common options:
- `--mode ultra|balanced|verbose`
- `--max-files <n>`
- `--max-focus-files <n>`
- `--top-n <n>`
- `--max-chars <n>`
- `--include-docs` + `--docs-mode off|markdown|text|all`
- `--no-incremental-refresh`

---

## `leindex mcp` / `leindex serve`
Run MCP interfaces.

```bash
leindex mcp
# or
leindex serve --host 127.0.0.1 --port 47268
```

---

## 2) MCP tools

Primary tools:
- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`

Additive phase tools:
- `leindex_phase_analysis`
- `phase_analysis` alias

---

## 3) Phase-analysis MCP request shape

```json
{
  "phase": "all",
  "path": "/path/to/project",
  "mode": "balanced",
  "top_n": 10,
  "max_files": 2000,
  "max_focus_files": 20,
  "max_chars": 12000,
  "include_docs": false,
  "docs_mode": "off"
}
```

`phase` accepts `1..5` or `"all"`.

---

## 4) Runtime vector-tier configuration

Vector indexing uses tiered HNSW (hot memory) + Turso-backed cold spill.

Defaults:
- local-only mode (no remote requirement)
- hot-tier memory budget: 256 MiB

Optional env vars:
- `LEINDEX_HNSW_HOT_MB`
- `LEINDEX_TURSO_URL`
- `LEINDEX_TURSO_AUTH_TOKEN`

---

## 5) Output guidance

- Use `search` / `analyze` for targeted technical questions.
- Use `phase` when you need broad triage in low-token form before deeper reading.
- Keep manual file reading for final correctness and design decisions.
