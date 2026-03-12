# Repo Hygiene

- Keep version parity across every published surface whenever `leindex` is version-bumped.
- Update `Cargo.toml`, installer scripts, npm package metadata, PyPI package metadata, and any in-repo version constants together in the same change.
- Keep the public README surfaces aligned: the root `README.md`, the PyPI README copy in `packages/pypi-leindex/README.md`, and the npm README in `packages/npm-leindex-mcp/README.md`.
- When MCP integration guidance changes, update all public MCP config examples in the README/docs set in the same pass.
