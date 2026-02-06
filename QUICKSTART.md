# LeIndex Quickstart (Current Rust Build)

> This file is retained for compatibility. For the canonical flow, see `QUICK_START.md`.

## Install

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

## Core workflow

```bash
leindex index /path/to/project
leindex phase --all --path /path/to/project
leindex search "where is auth enforced"
leindex analyze "how refresh tokens are handled"
```

## Cargo install option

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

## Why phase-first?

Run `phase --all` before deep reading to reduce token load and focus manual review on hotspots/focus files.
