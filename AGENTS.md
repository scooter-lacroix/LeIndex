# Engineering Principles

## Zero-Tolerance Policy on Discovered Issues

"Pre-existing" is NEVER an acceptable reason to leave an issue unfixed. Every discovered issue (clippy warning, test failure, lint error, build break, type error, code smell) requires complete and thorough investigation and debugging, regardless of whether it predates the current task.

When you encounter an issue:
1. Investigate the root cause fully, do not guess.
2. Fix it, even if it is outside the scope of your current task.
3. If the fix is genuinely high-risk (e.g., requires a large refactor in unrelated code), document it as a tracked issue (GitHub issue with `type:bug`) rather than silently leaving it.
4. Never suppress a lint, add a skip marker, or disable a check to make an issue "disappear" without understanding why it exists.
5. Never assume "it was like this before" exempts you from fixing it. Prior state is not a justification for continued brokenness.

When tests fail, you must first evaluate whether the failure is caused by a structural defect in the test itself (e.g., brittle assertions, missing fixtures, incorrect setup, testing implementation details instead of behavior) or whether the test is functioning correctly and revealing an underlying system issue (e.g., a real bug, a regression, a broken invariant). Only after determining which case applies should you proceed with investigation and remediation:
- If the test is structurally defective: fix the test so it correctly exercises the intended behavior. Do not delete or disable it.
- If the test is revealing a real system issue: fix the underlying system so the test passes. Do not weaken the assertion or relax the expected value to make the failure go away.
- In both cases: investigate thoroughly, write down the diagnosis in the commit message or PR description, and verify the fix with the full test suite.

This policy applies to: clippy warnings, compiler warnings, test failures, lint errors, formatting violations, type errors, security findings, dead code, and any other code quality signal discovered during work on this repository.

## Validating Changes

Before completing any task, run the full validation suite:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Zero warnings. Zero errors. No exceptions for "pre-existing" issues.

# Repo Hygiene

- Keep version parity across every published surface whenever `leindex` is version-bumped.
- Update `Cargo.toml`, installer scripts, npm package metadata, PyPI package metadata, and any in-repo version constants together in the same change.
- Keep the public README surfaces aligned: the root `README.md`, the PyPI README copy in `packages/pypi-leindex/README.md`, and the npm README in `packages/npm-leindex-mcp/README.md`.
- When MCP integration guidance changes, update all public MCP config examples in the README/docs set in the same pass.

## Secrets Management

- Never commit secrets (API keys, tokens, passwords) to the repository.
- `.env` files are gitignored. Use `.env.example` as a template for required environment variables.
- In CI, use GitHub Actions secrets (`secrets.*`) — never hardcode sensitive values in workflow files.
- For remote storage (Turso/libSQL), provide credentials via `TURSO_URL` and `TURSO_AUTH` environment variables set from secrets manager, not from committed config.
- The observability module (`src/observability.rs`) includes a `LogScrubber` that redacts Bearer tokens, API keys, passwords, and URL credentials from log output.
- When adding new configuration that accepts secrets, document it in `.env.example` with the actual value redacted.
- Minimum dependency release age: wait at least 3 days after a new crate release before bumping its version, to mitigate supply chain risk. Dependabot PRs for new releases should sit for review before merging.

## Testing Conventions

- Rust test files MUST follow the `*_test.rs` naming convention (e.g., `parser_test.rs`, `search_test.rs`).
- Integration tests live in the top-level `tests/` directory with descriptive names ending in `_test.rs` (e.g., `cli_integration_test.rs`).
- Unit tests are embedded in source files under `#[cfg(test)] mod test { ... }` modules.
- Test function names must start with `test_` and be descriptive (e.g., `test_parse_rust_function`, `test_search_returns_results`).
- Property-based tests use `proptest!` macro; benchmark tests live in `benches/`.
- CI enforces these conventions via the quality workflow.

## Documentation Generation

- API documentation is auto-generated from Rust doc comments via `cargo doc --workspace --no-deps`.
- The `.github/workflows/docs.yml` workflow builds and publishes documentation on every push to `master`.
- CLI help text is auto-generated from clap derive macros and can be refreshed with `cargo run --features cli -- --help`.

## AGENTS.md Validation

- The `.github/workflows/docs.yml` workflow validates that commands referenced in AGENTS.md are valid and that links resolve.
- If you add or change documentation commands, verify them locally with: `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace`.

## Release Pipeline

- The automated release workflow is `.github/workflows/release.yml`.
- It triggers on pushes to `master` and detects new versions by checking if a `v{version}` tag already exists.
- The pipeline builds cross-platform binaries (Linux x86_64/ARM64, macOS x86_64/ARM64, Windows x86_64), creates a GitHub Release with SHA256 checksums, then publishes to crates.io, npm, and PyPI in parallel.
- Required secrets: `CARGO_REGISTRY_TOKEN` (required), `PYPI_TOKEN` (optional), `NPM_TOKEN` (optional — npm publish is skipped gracefully if not set).
- Version parity is enforced at CI time — the npm and PyPI jobs validate their `package.json` / `pyproject.toml` versions match `Cargo.toml` before publishing.

## Progressive Rollout

Experimental features ship behind feature flags (see `src/feature_flags.rs`) controlled by `LEINDEX_FEATURE_*` environment variables. New capabilities default off and can be enabled per-deployment before broad release. The `release.yml` rollback workflow and `rollback.yml` manual trigger provide fast revert if issues are detected post-release.

## Privacy

LeIndex processes source code files locally. It does not collect, transmit, or store personally identifiable information (PII). No telemetry is sent to external servers unless explicitly configured by the user via OpenTelemetry or Sentry environment variables.

