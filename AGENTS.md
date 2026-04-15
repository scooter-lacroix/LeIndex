# Repo Hygiene

- Keep version parity across every published surface whenever `leindex` is version-bumped.
- Update `Cargo.toml`, installer scripts, npm package metadata, PyPI package metadata, and any in-repo version constants together in the same change.
- Keep the public README surfaces aligned: the root `README.md`, the PyPI README copy in `packages/pypi-leindex/README.md`, and the npm README in `packages/npm-leindex-mcp/README.md`.
- When MCP integration guidance changes, update all public MCP config examples in the README/docs set in the same pass.

## Release Pipeline

- The automated release workflow is `.github/workflows/release.yml`.
- It triggers on pushes to `master` and detects new versions by checking if a `v{version}` tag already exists.
- The pipeline builds cross-platform binaries (Linux x86_64/ARM64, macOS x86_64/ARM64, Windows x86_64), creates a GitHub Release with SHA256 checksums, then publishes to crates.io, npm, and PyPI in parallel.
- Required secrets: `CARGO_REGISTRY_TOKEN`, `NPM_TOKEN`, `PYPI_TOKEN`.
- Version parity is enforced at CI time — the npm and PyPI jobs validate their `package.json` / `pyproject.toml` versions match `Cargo.toml` before publishing.
