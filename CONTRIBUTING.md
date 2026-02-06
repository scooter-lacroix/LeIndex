# Contributing to LeIndex

Thanks for contributing.

LeIndex is a Rust workspace. Contributions should target current Rust crates and avoid reintroducing deprecated Python-era assumptions.

---

## 1) Setup

```bash
git clone https://github.com/scooter-lacroix/LeIndex.git
cd LeIndex
cargo check --workspace
cargo test --workspace
```

---

## 2) Workspace layout

- `crates/leparse` – parsing/signatures
- `crates/legraphe` – PDG + graph intelligence
- `crates/lerecherche` – ranking/search processors
- `crates/lestockage` – persistence/schema/stores
- `crates/lephase` – optional 5-phase triage subsystem
- `crates/lepasserelle` (package `leindex`) – CLI + MCP orchestration

---

## 3) Coding expectations

- Keep interfaces additive unless a breaking change is explicitly approved.
- Preserve existing CLI/MCP behavior where possible.
- Include tests for edge cases and regressions.
- Prefer clear failure modes over silent behavior changes.

---

## 4) Validation before PR

Minimum:

```bash
cargo fmt
cargo check --workspace
cargo test --workspace
```

For phase-related changes also run:

```bash
cargo test -p lephase -- --nocapture
cargo test -p legraphe extraction -- --nocapture
```

---

## 5) Docs policy

- Keep docs aligned with current Rust runtime behavior.
- Do not reintroduce deprecated legacy distribution/install instructions unless explicitly approved.
- Mark historical notes clearly if retaining old migration context.

---

## 6) Commit/PR guidelines

- Small, focused commits.
- PR description should include:
  - what changed,
  - why,
  - validation commands and results,
  - compatibility notes.

---

## 7) Security / sensitive data

Do not commit local-only artifacts, private data, or machine-specific logs/cache.
Use `.gitignore`/`.git/info/exclude` for local-only paths.

