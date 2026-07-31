# Runbooks and Incident Response

## Overview

This document provides operational runbooks for common incidents and maintenance tasks. Link to external runbooks or wikis as they evolve.

## On-Call Procedures

### Alert Routing

- **CI/CD failures**: Routed via GitHub Actions to the repository maintainer (@scooter-lacroix).
- **Release failures**: Routed via the observability workflow's `notify-deployment` job to the configured `MONITORING_WEBHOOK_URL` (Slack/Discord).
- **Security vulnerabilities**: Routed via Dependabot security alerts to the repository maintainer.
- **Performance regressions**: Detected by the `performance-regression.yml` workflow, annotated on PRs.

## Runbooks

### RB-001: Release Pipeline Failure

**Symptom**: The "Release" GitHub Actions workflow fails.

**Steps**:
1. Check which job failed: `detect`, `lint-and-test`, `build`, `crates-publish`, `npm-publish`, or `pypi-publish`.
2. If `detect` failed: Verify version parity across all surfaces (Cargo.toml, package.json, pyproject.toml, install scripts).
3. If `lint-and-test` failed: Run `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && cargo test --workspace` locally.
4. If `build` failed: Check cross-compilation dependencies for the failing target.
5. If publish failed: Verify registry tokens (CARGO_REGISTRY_TOKEN, PYPI_TOKEN, NPM_TOKEN secrets).
6. After fixing, push to `master` to re-trigger the pipeline.

### RB-002: Performance Regression

**Symptom**: The "Performance Regression" workflow reports a slowdown exceeding threshold.

**Steps**:
1. Identify the benchmark that regressed (search, SIMD, edit, or MCP latency).
2. Check the PR that triggered the regression.
3. Run the affected benchmark locally: `cargo bench --bench <name>`.
4. Compare with the baseline in `benchmarks/`.
5. If real: revert the offending change or optimize the code path.
6. Document the fix in the PR description with before/after numbers.

### RB-003: Out of Memory During Indexing

**Symptom**: `leindex index` crashes with OOM on large repositories.

**Steps**:
1. Check the repository size and file count.
2. Verify the memory budget workflow is passing (`.github/workflows/memory-budget.yml`).
3. If the index exceeds memory limits, reduce `--max-files` or use the streaming indexer.
4. Consider increasing the system's swap or using a machine with more RAM.
5. File an issue with `type:bug` and `area:search` if the OOM is unexpected.

### RB-004: MCP Server Connection Issues

**Symptom**: AI agent cannot connect to the LeIndex MCP server.

**Steps**:
1. Verify the leindex binary is installed: `leindex --version`.
2. Check the MCP configuration in the client (see `docs/MCP.md`).
3. Verify the project is indexed: `leindex status`.
4. Check logs with `RUST_LOG=debug leindex mcp` for error messages.
5. If neural search is required, verify ONNX models: `leindex setup --check`.
6. Check for log scrubbing issues: ensure no credentials are leaking in debug logs (see `src/observability.rs` LogScrubber).

### RB-005: Dependency Vulnerability Alert

**Symptom**: Dependabot or GitHub security advisory reports a vulnerable dependency.

**Steps**:
1. Check the advisory severity (critical, high, medium, low).
2. Review the affected code paths (is the vulnerable function actually called?).
3. For critical/high: update the dependency immediately, run tests, and release a patch.
4. For medium/low: schedule the update for the next release.
5. Verify the updated dependency does not introduce new vulnerabilities with `cargo audit`.

### RB-006: Git History Bloat or LFS Issue

**Symptom**: Repository clone is slow or large files are causing issues.

**Steps**:
1. Check repository size on GitHub.
2. Identify large files: `git rev-list --objects --all | sort -k 2 | uniq -f 1 | sort -rn | head -20`.
3. If large binary files exist, consider Git LFS (`.gitattributes` with `filter=lfs`).
4. The quality workflow's large-file-check job catches files >1MB before merge.

## External Resources

- **GitHub Actions**: https://github.com/scooter-lacroix/LeIndex/actions
- **Releases**: https://github.com/scooter-lacroix/LeIndex/releases
- **Security Advisories**: https://github.com/scooter-lacroix/LeIndex/security/advisories
- **Performance Benchmarks**: See `benchmarks/` directory and `BENCHMARKS.md`

## Post-Incident Review Template

After resolving an incident:

1. Create a GitHub issue with label `type:bug` (or `type:security` for security incidents).
2. Document the timeline, root cause, and resolution.
3. Add any new runbook entries to this file.
4. Update monitoring/alerting if the incident could have been caught earlier.
5. Consider adding a regression test to prevent recurrence.
