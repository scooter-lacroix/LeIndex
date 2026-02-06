# LeIndex Advanced Quick Start Guide (Rust)

This guide replaces older Python-era scanner examples.

## 1) Index and analyze

```bash
leindex index /path/to/project
leindex phase --all --path /path/to/project
```

## 2) High-value modes

```bash
# Dependency-only view
leindex phase --phase 2 --path /path/to/project

# Hotspot-only view
leindex phase --phase 4 --path /path/to/project --top-n 20
```

## 3) Incremental behavior

By default, LeIndex uses incremental refresh when possible:
- changed files are reparsed,
- deleted files are removed from PDG/storage,
- parse failures do not clobber prior valid graph state.

Use full refresh when needed:

```bash
leindex phase --all --path /path/to/project --no-incremental-refresh
```

## 4) Optional docs analysis

Docs analysis is explicit opt-in:

```bash
leindex phase --all --path /path/to/project --include-docs --docs-mode markdown
```

## 5) MCP usage

```bash
leindex mcp
```

Phase tools exposed to assistants:
- `leindex_phase_analysis`
- `phase_analysis`

## 6) Cargo install

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```
