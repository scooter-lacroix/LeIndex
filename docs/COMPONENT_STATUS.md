# LeIndex Component Status

Last updated: 2026-02-05

This status summary confirms `lerecherche` and `lestockage` are implemented runtime components, not temporary placeholders.

---

## lerecherche (search/ranking)

Implemented modules:
- `query` (intent parsing + validation)
- `search` (hybrid engine, text+semantic)
- `ranking` (weighted scoring/rerank)
- `vector` (exact vector index)
- `hnsw` (approximate ANN index)
- `tiered` (memory-budgeted HNSW hot tier + Turso cold tier spill)
- `semantic` (semantic processor/context support)

Validation evidence:
- `cargo test -p lerecherche`
- Result: **88 unit tests + 12 integration tests passed**

---

## lestockage (persistent storage)

Implemented modules:
- `schema` (SQLite schema + initialization)
- `nodes`, `edges` stores
- `pdg_store` (save/load/delete persistent PDG)
- `global_symbols` (cross-project symbol table)
- `cross_project` resolver
- `salsa` incremental cache
- `turso_config` hybrid/local/remote storage config + local→remote vector migration bridge
- Default remains local-only; remote Turso is opt-in

Validation evidence:
- `cargo test -p lestockage`
- Result: **48 unit tests + 11 integration tests passed**

---

## Package/runtime integration

Build validation:

```bash
cargo check -p leindex -p lephase -p lerecherche -p lestockage
```

Install validation:

```bash
cargo install --path crates/lepasserelle --locked --force --root /tmp/leindex-install-test
```

These checks confirm the crates are wired into the current runtime and install flow.
