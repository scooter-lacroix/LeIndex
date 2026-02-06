# LeIndex Installation Guide (Rust)

Last updated: 2026-02-05

LeIndex supports two primary install paths:
1. One-line installer script
2. Cargo install

Both are first-class and kept up to date.

---

## Primary install commands (recommended)

### One-line installer

**Linux**
```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

**macOS**
```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install_macos.sh | bash
```

**Windows (PowerShell)**
```powershell
iwr https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.ps1 -UseBasicParsing | iex
```

### Cargo install (crates.io)

```bash
cargo install leindex
```

### Cargo install (Git source)

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

---

## Installer verification behavior

Installers verify runtime capability, not only binary presence:

- `leindex --version`
- `leindex phase --help`
- `leindex mcp --help`
- phase smoke test

---

## Post-install verification

```bash
leindex --version
leindex index --help
leindex search --help
leindex analyze --help
leindex phase --help
leindex mcp --help
```

---

## Manual build (from source)

```bash
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
cargo build --release --bins
```

Binary output:
- `target/release/leindex`

---

## Optional Turso remote configuration (opt-in)

Default runtime remains local plug-and-play.

Set remote Turso only if desired:

```bash
export LEINDEX_TURSO_URL="libsql://<db>.turso.io"
export LEINDEX_TURSO_AUTH_TOKEN="..."
# optional hot-tier memory budget (MiB)
export LEINDEX_HNSW_HOT_MB=256
```

---

## Release workflow

For release binary and crates.io workflow details:
- `docs/RELEASE_BINARY_WORKFLOW.md`
