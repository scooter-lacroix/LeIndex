# Crate Unification Tzar Review

Date: 2026-03-11
Branch: `feature/unified-crate`
Scope: source-code-level review of the unified-crate work relative to `origin/master`
Status: `REQUEST CHANGES`

## Summary

The unified-crate branch is now materially stronger after the parser stack-overflow fix:

- indexing no longer aborts on a synthetic Rust project with 6,000 nested `if` blocks
- indexing no longer aborts on a synthetic C++ project with 6,000 nested `if` blocks
- the installed `leindex` binary completed a full index of `/home/scooter/Documents/Product/debugging/llvm-project` without a stack overflow

Even with that improvement, the branch still has release-impacting unification issues that should be addressed before the work is considered ready to ship broadly.

## Findings

### High

1. NPM MCP package downloads the wrong release binary version.
   Files:
   - [packages/npm-leindex-mcp/install.js](/mnt/WD-SSD/code_index_update/LeIndexer/packages/npm-leindex-mcp/install.js)
   - [packages/npm-leindex-mcp/package.json](/mnt/WD-SSD/code_index_update/LeIndexer/packages/npm-leindex-mcp/package.json)

   Details:
   - `package.json` is version `1.5.1`
   - `install.js` hardcodes `VERSION = '1.5.0'`
   - `npm install @leindex/mcp@1.5.1` can therefore fetch an older binary than the package metadata and docs imply

   Risk:
   - mismatched MCP behavior between npm wrapper and crate release
   - silent drift in tooling and bugfix coverage

2. Installer has an unbound-variable regression under `set -u`.
   Files:
   - [install.sh](/mnt/WD-SSD/code_index_update/LeIndexer/install.sh)

   Details:
   - `install_system_deps()` reads `INSTALL_DASHBOARD`
   - `INSTALL_DASHBOARD` is not initialized before that read
   - on a clean shell, the installer can abort before completing dependency setup

   Risk:
   - release-blocking install failure

3. NPM installer executes downloaded binaries without integrity verification.
   Files:
   - [packages/npm-leindex-mcp/install.js](/mnt/WD-SSD/code_index_update/LeIndexer/packages/npm-leindex-mcp/install.js)

   Details:
   - downloaded GitHub release binaries are made executable and invoked
   - no checksum or signature verification is performed first

   Risk:
   - supply-chain exposure for the publishable npm wrapper

### Medium

4. Crate packaging is too broad and leaks internal project artifacts.
   Files:
   - [Cargo.toml](/mnt/WD-SSD/code_index_update/LeIndexer/Cargo.toml)
   - [files.zip](/mnt/WD-SSD/code_index_update/LeIndexer/files.zip)
   - [UNIFICATION_PLAN.md](/mnt/WD-SSD/code_index_update/LeIndexer/UNIFICATION_PLAN.md)
   - [maestro/tracks.md](/mnt/WD-SSD/code_index_update/LeIndexer/maestro/tracks.md)

   Details:
   - the unified crate currently lacks a restrictive `include`/`exclude` packaging policy
   - `cargo package` can include internal planning docs and unrelated artifacts

   Risk:
   - larger crate payloads
   - internal repo material leaking into published release tarballs

5. Installer header examples point at the wrong branch name.
   Files:
   - [install.sh](/mnt/WD-SSD/code_index_update/LeIndexer/install.sh)

   Details:
   - header examples still reference `main`
   - the repo default branch is `master`

   Risk:
   - copy-paste install failures

6. NPM wrapper shell-interpolates argument strings unsafely.
   Files:
   - [packages/npm-leindex-mcp/index.js](/mnt/WD-SSD/code_index_update/LeIndexer/packages/npm-leindex-mcp/index.js)

   Details:
   - wrapper builds shell commands with `args.join(' ')`
   - arguments with spaces or shell metacharacters can break invocation or become injection vectors

   Risk:
   - incorrect execution and potential command injection

7. Runtime and docs still expose removed pre-unification identities.
   Files:
   - [src/server/handlers.rs](/mnt/WD-SSD/code_index_update/LeIndexer/src/server/handlers.rs)
   - [README.md](/mnt/WD-SSD/code_index_update/LeIndexer/README.md)

   Details:
   - some runtime responses and docs still present `leserve`/legacy identities even though distribution is now centered on `leindex`

   Risk:
   - operational ambiguity for users and monitoring

8. Feature-flag docs overpromise runnable binaries.
   Files:
   - [README.md](/mnt/WD-SSD/code_index_update/LeIndexer/README.md)
   - [Cargo.toml](/mnt/WD-SSD/code_index_update/LeIndexer/Cargo.toml)

   Details:
   - docs imply `minimal` and `server` are straightforward runnable build modes
   - the only binary target requires `cli`

   Risk:
   - user confusion and failed build expectations

## Verified Bugfix Work

The parser stack-overflow fix is implemented in:

- [src/parse/rust.rs](/mnt/WD-SSD/code_index_update/LeIndexer/src/parse/rust.rs)
- [src/parse/c.rs](/mnt/WD-SSD/code_index_update/LeIndexer/src/parse/c.rs)
- [src/parse/cpp.rs](/mnt/WD-SSD/code_index_update/LeIndexer/src/parse/cpp.rs)
- [src/parse/tests.rs](/mnt/WD-SSD/code_index_update/LeIndexer/src/parse/tests.rs)

The implementation replaces recursive indexing-time AST descent with iterative traversal in the Rust, C, and C++ parser hot paths.

## Verification Performed

- `cargo check`
- `cargo test --features cli stack_overflow_regression_tests`
- installed `leindex` force-indexed the synthetic deep Rust project successfully
- installed `leindex` completed a full index of `/home/scooter/Documents/Product/debugging/llvm-project`

## Remediation Plan

1. Fix the npm wrapper version drift by deriving the release version from `package.json`, then add a test that enforces installer/package version parity.
2. Initialize installer option variables up front and add a shell-level smoke test for `install.sh` under `set -u`.
3. Add binary integrity verification to the npm installer flow using published checksums.
4. Tighten crate packaging with an explicit `include` allowlist and verify with `cargo package --list`.
5. Correct branch-name references and other release/install documentation drift.
6. Replace shell-interpolated npm wrapper execution with `spawnSync`/`execFileSync`.
7. Normalize legacy runtime and documentation naming around `leindex`.
8. Reconcile feature-flag documentation with the actual binary targets and supported build modes.
