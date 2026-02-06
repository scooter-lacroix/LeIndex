# LeIndex Installation Guide (Rust)

Last updated: 2026-02-05

This is the **current** installation path for LeIndex.

Default behavior remains plug-and-play local. Remote Turso usage is optional/opt-in.

---

## Fast install (recommended)

### Linux

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

### macOS

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install_macos.sh | bash
```

### Windows (PowerShell)

```powershell
iwr https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.ps1 -UseBasicParsing | iex
```

---

## What the installer now verifies

Installers perform feature verification, not just version checks:

- `leindex --version`
- `leindex phase --help`
- `leindex mcp --help`
- A temporary **phase-analysis smoke test**

This ensures your live install actually includes 5-phase analysis capabilities.

---

## Cargo install options

### Install from Git (works now)

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

### Install from crates.io (after publish)

```bash
cargo install leindex
```

Release/binary publishing process: `docs/RELEASE_BINARY_WORKFLOW.md`

## Manual build (from source)

```bash
git clone https://github.com/scooter-lacroix/leindex.git
cd leindex
cargo build --release --bins
```

Binary location:
- `target/release/leindex`

---

## Post-install quick verification

```bash
leindex --version
leindex phase --help
leindex index /path/to/project
leindex phase --all --path /path/to/project
```

## Optional Turso remote configuration (opt-in)

Default is local-only tiered vector storage (plug-and-play).

Set these only if you want remote Turso:

```bash
export LEINDEX_TURSO_URL="libsql://<db>.turso.io"
export LEINDEX_TURSO_AUTH_TOKEN="..."
# Optional hot-tier budget in MiB (default 256)
export LEINDEX_HNSW_HOT_MB=256
```

---

## MCP quick start

```bash
leindex mcp
# or
leindex serve --host 127.0.0.1 --port 47268
```

Tools include:
- `leindex_index`
- `leindex_search`
- `leindex_deep_analyze`
- `leindex_context`
- `leindex_diagnostics`
- `leindex_phase_analysis` (`phase_analysis` alias)

---

## Troubleshooting

### `leindex` not found

- Restart terminal/session.
- Verify install location:
  - Linux default: `/usr/local/bin/leindex`
  - macOS script default: `$HOME/.leindex/bin/leindex`
  - Windows script default: `%USERPROFILE%\.leindex\bin\leindex.exe`

### Rust missing

Installers will prompt/install rustup where appropriate. You can also install manually:
- https://rustup.rs/

### Phase command missing

Run installer again. The updated installer now fails verification if `leindex phase` is unavailable.
