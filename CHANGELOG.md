# Changelog

All notable changes to LeIndex are documented here.

The project now uses a Rust workspace runtime. Legacy Python-era installation instructions have been removed from active docs.

## [0.1.0] - 2026-02-05

### Added
- `lephase` crate with additive 5-phase analysis mode (CLI + MCP).
- MCP phase tools: `leindex_phase_analysis` and alias `phase_analysis`.
- Incremental freshness protections and parse-failure-safe updates.
- Import relinking improvements with bounded candidate ranking and orphan cleanup.
- Installer verification gates for `phase`/`mcp` and smoke-run checks.
- Cargo install path via package `leindex` (`crates/lepasserelle`).

### Improved
- PDG merge/relink behavior and edge deduplication.
- Cache mismatch/corruption handling with safe misses.
- Parser import/signature extraction edge-case handling.
- Documentation now presents 5-phase as additive to core index/search/analyze system.

### Validation
- Workspace checks/tests updated.
- Focused verification:
  - `cargo check -p leindex -p lephase -p lerecherche -p lestockage`
  - `cargo test -p lerecherche -p lestockage`

### Notes
- For installation/release details see:
  - `INSTALLATION.md`
  - `docs/RELEASE_BINARY_WORKFLOW.md`
  - `docs/COMPONENT_STATUS.md`
