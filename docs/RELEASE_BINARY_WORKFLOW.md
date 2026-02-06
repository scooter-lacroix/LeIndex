# LeIndex Release + Binary Workflow (Rust)

Last updated: 2026-02-05

This document describes how to ship LeIndex through both Cargo and GitHub release binaries.

---

## 1) Pre-release validation

```bash
cargo fmt --all
cargo check --workspace
cargo test --workspace
cargo check -p leindex -p lephase -p lerecherche -p lestockage
```

Optional install validation:

```bash
cargo install --path crates/lepasserelle --locked --force --root /tmp/leindex-release-check
/tmp/leindex-release-check/bin/leindex --version
/tmp/leindex-release-check/bin/leindex phase --help
/tmp/leindex-release-check/bin/leindex mcp --help
```

---

## 2) Package validation (publishability)

For **GitHub binary releases**, packaging checks are optional; build/test/install checks are the hard gate.

For **crates.io publishing**, crates must be published in dependency order because `leindex` depends on workspace crates:

1. `leparse`
2. `legraphe`
3. `lerecherche`
4. `lestockage`
5. `lephase`
6. `leindex`

Example first-step validation:

```bash
cargo package -p leparse
```

Then publish downstream crates after upstream crates are available on crates.io.

---

## 3) Cargo install paths

### Current (works immediately)

```bash
cargo install --git https://github.com/scooter-lacroix/LeIndex.git --locked --bin leindex
```

### After crates.io publish

```bash
cargo install leindex
```

---

## 4) Build release binaries

Build at minimum for:
- Linux x86_64
- macOS arm64/x86_64
- Windows x86_64

Example local build:

```bash
cargo build --release -p leindex
```

Binary output:
- `target/release/leindex` (or `leindex.exe` on Windows)

---

## 5) Checksums

```bash
sha256sum leindex-* > checksums.txt
```

Publish checksums with binary assets.

---

## 6) GitHub release process

1. Bump `workspace.package.version`.
2. Commit + tag (`vX.Y.Z`).
3. Push tag.
4. Create GitHub Release from tag.
5. Upload binaries + `checksums.txt`.
6. Verify installer scripts resolve the new artifact/tag.

---

## 7) Installer verification gate

Install scripts must continue validating:
- `leindex --version`
- `leindex phase --help`
- `leindex mcp --help`
- phase smoke run

If any check fails, installer should fail fast.
