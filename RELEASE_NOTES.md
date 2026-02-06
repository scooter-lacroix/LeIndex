# LeIndex Release Notes

## 0.1.0 (Rust runtime baseline)

Release date: 2026-02-05

### Highlights
- Full Rust workspace runtime across parse/graph/search/storage/orchestration crates.
- Additive 5-phase triage mode (`leindex phase`) available through CLI and MCP.
- Existing `index`, `search`, `analyze`, `context`, and `diagnostics` surfaces preserved.
- Installer scripts now verify real capabilities (phase/mcp/smoke), not version only.

### Installation

#### Installer scripts

```bash
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/leindex/main/install.sh | bash
```

#### Cargo

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

(After crates.io publish)

```bash
cargo install leindex
```

### Verification

```bash
leindex --version
leindex index --help
leindex search --help
leindex analyze --help
leindex phase --help
leindex mcp --help
```

### More docs
- `INSTALLATION.md`
- `API.md`
- `ARCHITECTURE.md`
- `docs/RELEASE_BINARY_WORKFLOW.md`
- `docs/COMPONENT_STATUS.md`
