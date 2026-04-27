# Plan: Version Bump GitHub Actions Workflow

## Phase 1: Scaffold Workflow File

- [x] Task: Create `.github/workflows/bump-version.yml` skeleton — DONE
- [x] Task: Add step — Checkout repository — DONE
- [x] Task: Add step — Validate semver format — DONE
- [x] Task: Add step — Extract current version from Cargo.toml — DONE
- [x] Task: Add step — Validate version is higher than current — DONE
- [x] Task: Add step — Bump version in Cargo.toml — DONE
- [x] Task: Add step — Bump version in npm package.json — DONE
- [x] Task: Add step — Bump version in PyPI pyproject.toml — DONE
- [x] Task: Add step — Verify version parity across all three files — DONE
- [x] Task: Add step — Commit and push — DONE
- [x] Task: Add step — Print summary — DONE

## Phase 2: Validation

- [x] Task: Validate YAML syntax — DONE (python3 yaml.safe_load passed)
- [x] Task: Dry-run sed commands against real project files locally — DONE (all 3 files updated correctly)
- [ ] Task: Verify workflow dispatchability on GitHub — requires push to remote

## Phase 3: Integration Verification

- [x] Task: Confirm release.yml triggers after bump-version push — DONE (release.yml paths include Cargo.toml)
- [ ] Task: End-to-end test — requires merge to master + manual trigger
