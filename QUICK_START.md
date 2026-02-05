# LeIndex Quick Start (5-Phase First)

If you only read one page, read this one.

---

## 1) Install

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

Verify:

```bash
leindex --version
leindex phase --help
```

---

## 2) Index your project

```bash
leindex index /path/to/project
```

---

## 3) Run 5-phase analysis (recommended first step)

```bash
leindex phase --all --path /path/to/project
```

This gives a compact map of:
- structural health,
- dependency map,
- impact flow,
- hotspot risk,
- prioritized recommendations.

---

## 4) Then do targeted follow-up

```bash
leindex search "where auth decisions are enforced"
leindex analyze "how user-session invalidation works"
```

---

## 5) Why this order matters

Starting with phase analysis drastically reduces token-heavy blind exploration.

### Measured snapshot

On a 1,974-file repository:
- Phase summary: ~118 tokens
- Grep/manual triage sample: ~26,272 tokens
- Reduction: ~99.55%

Use phase analysis to narrow scope first; manually read only the focus files after.

---

## 6) MCP mode (AI assistants)

```bash
leindex mcp
# or
leindex serve --host 127.0.0.1 --port 47268
```

Available phase MCP tools:
- `leindex_phase_analysis`
- `phase_analysis` alias

---

## 7) One command cheat sheet

```bash
leindex index /path/to/project
leindex phase --all --path /path/to/project
leindex search "my question"
leindex analyze "deeper question"
leindex diagnostics
```
