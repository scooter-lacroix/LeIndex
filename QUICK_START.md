# LeIndex Quick Start (Core Workflow)

This is the shortest path to getting value from the full LeIndex system.

---

## 1) Install

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

Verify:

```bash
leindex --version
```

---

## 2) Index your project

```bash
leindex index /path/to/project
```

---

## 3) Use the primary analysis tools

```bash
# Semantic search
leindex search "where auth decisions are enforced"

# Deep analysis + context expansion
leindex analyze "how user-session invalidation works"

# System diagnostics
leindex diagnostics
```

---

## 4) Use 5-phase mode when you need scoped triage

```bash
# Full phased triage
leindex phase --all --path /path/to/project

# Single phase example: dependency map
leindex phase --phase 2 --path /path/to/project
```

Use this mode to reduce blind exploration and focus manual reading.

---

## 5) MCP mode (AI assistants)

```bash
leindex mcp
# or
leindex serve --host 127.0.0.1 --port 47268
```

Phase tools are additive (`leindex_phase_analysis`, `phase_analysis`) and do not replace core index/search/analyze tools.
