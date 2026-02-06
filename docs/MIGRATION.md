# LeIndex Migration Guide (Rust Runtime)

Last updated: 2026-02-05

This guide replaces older Python-era migration instructions.

---

## 1) Backup local LeIndex state

```bash
mkdir -p ~/.leindex/backups
cp -r ~/.leindex ~/.leindex/backups/leindex-$(date +%Y%m%d-%H%M%S) || true
```

---

## 2) Install current LeIndex

### Recommended installer

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

### Cargo path

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

---

## 3) Verify runtime surface

```bash
leindex --version
leindex index --help
leindex search --help
leindex analyze --help
leindex phase --help
leindex mcp --help
```

---

## 4) Rebuild index data

If migrating from old data layouts, rebuild indexes:

```bash
leindex index /path/to/project
```

Then validate search/analysis:

```bash
leindex search "entry points"
leindex analyze "how request routing works"
```

---

## 5) Optional phased triage

```bash
leindex phase --all --path /path/to/project
```

Use this to narrow scope before manual review.

---

## 6) MCP migration

Run MCP server using current command:

```bash
leindex mcp
# or
leindex serve --host 127.0.0.1 --port 47268
```

Primary MCP tools:
- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`
- `leindex_phase_analysis` (`phase_analysis` alias)

---

## 7) Legacy note

Historical Python-era commands are no longer the supported installation path for this repository.
Use installer or Cargo workflows above.
