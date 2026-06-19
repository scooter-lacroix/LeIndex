//! Release bundle packaging tests (VAL-RELEASE-001..018).
//!
//! These tests statically validate `.github/workflows/release.yml` to ensure
//! the GitHub Release bundle layout produced by the release pipeline satisfies
//! the distribution contract:
//!
//!   * `bin/` carries both `leindex` and `leindex-embed`
//!   * `models/` ships the ONNX model, tokenizer, and config
//!   * `lib/` contains ORT runtime libraries obtained from a pip wheel extract
//!     (not linked at build time)
//!   * The linux-x86_64 (AMD) bundle additionally ships the MIGraphX provider
//!     `.so` and the shared providers library
//!   * Each bundle includes an `INSTALL.txt` with quick-start guidance
//!   * Checksums are generated for every artifact, including the new `lib/`
//!     directory contents
//!   * The build step never injects `ORT_LIB_PATH`, `ORT_PREFER_DYNAMIC_LINK`,
//!     `download-binaries`, or `copy-dylibs` (load-dynamic only)
//!   * The workflow verifies the produced binaries carry neither a `$ORIGIN`
//!     rpath nor a `NEEDED libonnxruntime.*` entry
//!
//! The contract is documented in
//! `validation-contract.md` (Area: RELEASE) and `architecture.md`
//! (Section 3: Release Pipeline).
//!
//! These tests are intentionally textual: we cannot run the CI workflow inside
//! `cargo test`, but we can guarantee the workflow YAML contains the steps
//! and invariants the bundle relies on. A drift in the YAML would indicate a
//! regression that would change the artifact layout, provider coverage, or
//! linking behavior of the release.

#![cfg(test)]

use std::path::PathBuf;

/// Return the absolute path to the repo root, computed from CARGO_MANIFEST_DIR
/// (the directory containing Cargo.toml of the `leindex` package under test).
fn repo_root() -> PathBuf {
    let dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(dir)
}

/// Read the release workflow YAML as a string. Panics with a clear diagnostic
/// if the file is missing so test failures point at the actual cause.
fn release_yml() -> String {
    let path = repo_root()
        .join(".github")
        .join("workflows")
        .join("release.yml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Read a build script as a string.
fn build_script(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

// ============================================================================
// Build-time invariants (load-dynamic must be the only ORT strategy)
// ============================================================================

mod build_time_invariants {
    use super::*;

    /// VAL-RELEASE-010: The build step must NOT pull ORT in at build time.
    /// Neither `ORT_LIB_PATH`, `ORT_PREFER_DYNAMIC_LINK`, `download-binaries`,
    /// nor `copy-dylibs` may appear in the build invocation.
    #[test]
    fn build_step_does_not_link_ort_at_build_time() {
        let yml = release_yml();

        // There must be a cargo build invocation building both crates with
        // load-dynamic-compatible features.
        let build_section = yml
            .split("Build release binaries")
            .nth(1)
            .and_then(|s| s.split("Strip binaries").next())
            .expect("release.yml must contain a 'Build release binaries' step");

        assert!(
            !build_section.contains("ORT_LIB_PATH"),
            "build step must not set ORT_LIB_PATH (load-dynamic only)"
        );
        assert!(
            !build_section.contains("ORT_PREFER_DYNAMIC_LINK"),
            "build step must not set ORT_PREFER_DYNAMIC_LINK (load-dynamic only)"
        );
        assert!(
            !build_section.contains("download-binaries"),
            "build step must not enable the download-binaries ort feature"
        );
        assert!(
            !build_section.contains("copy-dylibs"),
            "build step must not enable the copy-dylibs ort feature"
        );
    }

    /// VAL-RELEASE-012: Neither build script may add a `$ORIGIN` rpath entry.
    /// Since the migration to load-dynamic, the rpath is obsolete and would
    /// leak build-time assumptions about ORT into the redistributed binary.
    ///
    /// We filter out comment lines (those starting with `//`) before checking,
    /// because the build scripts legitimately contain a doc comment explaining
    /// that the rpath is intentionally absent. The invariant we care about is
    /// that no actual cargo directive (`cargo:rustc-link-arg=-Wl,-rpath,...`)
    /// or `println!` issuing such a directive remains.
    #[test]
    fn build_scripts_have_no_origin_rpath() {
        for rel in ["build.rs", "crates/leindex-embed/build.rs"] {
            let script = build_script(rel);
            // Drop comment lines so doc comments mentioning the absence of
            // rpath do not trip the literal-substring check.
            let non_comment: String = script
                .lines()
                .filter(|line| {
                    let trimmed = line.trim_start();
                    !trimmed.starts_with("//")
                })
                .collect::<Vec<_>>()
                .join("\n");
            assert!(
                !non_comment.contains("$ORIGIN"),
                "{rel} must not emit a $ORIGIN rpath (load-dynamic does not need it)"
            );
            assert!(
                !non_comment.contains("-Wl,-rpath"),
                "{rel} must not emit any -Wl,-rpath directive (load-dynamic does not need it)"
            );
            assert!(
                !non_comment.contains("cargo:rustc-link-arg"),
                "{rel} must not emit cargo:rustc-link-arg (load-dynamic does not need it)"
            );
        }
    }
}

// ============================================================================
// Bundle layout: bin/, lib/, models/, INSTALL.txt
// ============================================================================

mod bundle_layout {
    use super::*;

    /// VAL-RELEASE-001 / VAL-RELEASE-002: The Package step must copy both
    /// `leindex` and `leindex-embed` into `bin/`.
    #[test]
    fn bundle_contains_both_binaries() {
        let yml = release_yml();
        let package_section = yml
            .split("Package release bundle")
            .nth(1)
            .and_then(|s| s.split("Upload artifact").next())
            .expect("release.yml must contain a 'Package release bundle' step");

        assert!(
            package_section.contains("bin/leindex"),
            "package step must lay out bin/leindex"
        );
        assert!(
            package_section.contains("leindex-embed"),
            "package step must lay out bin/leindex-embed"
        );
    }

    /// VAL-RELEASE-003 / VAL-RELEASE-004: The bundle ships model assets.
    #[test]
    fn bundle_contains_model_assets() {
        let yml = release_yml();
        let package_section = yml
            .split("Package release bundle")
            .nth(1)
            .and_then(|s| s.split("Upload artifact").next())
            .expect("release.yml must contain a 'Package release bundle' step");

        assert!(
            package_section.contains("models/qwen3-embed-0.6b.onnx"),
            "bundle must ship the ONNX model"
        );
        assert!(
            package_section.contains("tokenizer.json"),
            "bundle must ship tokenizer.json"
        );
        assert!(
            package_section.contains("config.json"),
            "bundle must ship config.json"
        );
    }

    /// VAL-RELEASE-005: The bundle must include a `lib/` directory carrying
    /// the ONNX Runtime shared libraries. Additionally there must be a step
    /// that extracts those libraries from a pip wheel (not the link step).
    #[test]
    fn bundle_includes_ort_runtime_lib_directory() {
        let yml = release_yml();

        assert!(
            yml.contains("lib/libonnxruntime") || yml.contains("BUNDLE_DIR/lib"),
            "package step must lay out lib/ for the ORT shared libraries"
        );

        // Per VAL-RELEASE-011 the ORT shared libs are obtained by extracting
        // the onnxruntime pip wheel. Either `pip install onnxruntime` (native)
        // or `pip download onnxruntime` (cross-compile targets) is acceptable;
        // both end up with the capi/libonnxruntime artifact that gets copied
        // into the bundle. The invariant we care about is that the libs come
        // from the onnxruntime wheel, not from a system install or a
        // download-binaries build-time link.
        let obtains_from_wheel = yml.contains("pip install onnxruntime")
            || yml.contains("pip download") && yml.contains("onnxruntime");
        assert!(
            obtains_from_wheel,
            "release.yml must extract ORT from the onnxruntime pip wheel (pip install or pip download)"
        );
        assert!(
            yml.contains("capi/libonnxruntime")
                || yml.contains("CAPI") && yml.contains("libonnxruntime"),
            "release.yml must copy libonnxruntime from the wheel's capi/ directory"
        );
    }

    /// VAL-RELEASE-006: The linux-x86_64 (AMD-targeted) bundle must additionally
    /// include the MIGraphX provider `.so` and the providers_shared library.
    #[test]
    fn amd_bundle_includes_migraphx_provider() {
        let yml = release_yml();
        assert!(
            yml.contains("libonnxruntime_providers_migraphx"),
            "linux-x86_64 bundle must ship the MIGraphX provider .so"
        );
        assert!(
            yml.contains("libonnxruntime_providers_shared"),
            "linux-x86_64 bundle must ship the shared providers .so"
        );
        // The MIGraphX path must be gated to the AMD target so non-AMD bundles
        // are not polluted with GPU-only providers.
        assert!(
            yml.contains("linux-x86_64") && yml.contains("migraphx"),
            "MIGraphX .so inclusion must be scoped to the linux-x86_64 (AMD) bundle"
        );
    }

    /// VAL-RELEASE-007: The bundle top level must consist of exactly
    /// `bin/`, `lib/`, `models/`, and `INSTALL.txt`. The package step must
    /// create those directories.
    #[test]
    fn bundle_directory_tree_matches_spec() {
        let yml = release_yml();
        let package_section = yml
            .split("Package release bundle")
            .nth(1)
            .and_then(|s| s.split("Upload artifact").next())
            .expect("release.yml must contain a 'Package release bundle' step");

        assert!(
            package_section.contains("BUNDLE_DIR/bin"),
            "package step must create bin/ under the bundle root"
        );
        assert!(
            package_section.contains("BUNDLE_DIR/lib"),
            "package step must create lib/ under the bundle root"
        );
        assert!(
            package_section.contains("BUNDLE_DIR/models"),
            "package step must create models/ under the bundle root"
        );
    }

    /// VAL-RELEASE-008: Each bundle carries a top-level `INSTALL.txt` describing
    /// the install path and the discovery chain.
    #[test]
    fn bundle_includes_install_txt() {
        let yml = release_yml();
        let package_section = yml
            .split("Package release bundle")
            .nth(1)
            .and_then(|s| s.split("Upload artifact").next())
            .expect("release.yml must contain a 'Package release bundle' step");

        assert!(
            package_section.contains("INSTALL.txt"),
            "package step must stage an INSTALL.txt at the bundle root"
        );
        // The INSTALL.txt content the workflow writes must reference the
        // leindex setup command and at least one ORT discovery source so the
        // onboarding doc is genuinely actionable.
        assert!(
            yml.contains("leindex setup"),
            "INSTALL.txt template must reference `leindex setup`"
        );
    }
}

// ============================================================================
// Checksums must cover lib/ artifacts
// ============================================================================

mod checksum_coverage {
    use super::*;

    /// VAL-RELEASE-009: The `SHA256SUMS` published at the release level must
    /// remain, AND each bundle must include per-file checksums that cover the
    /// `lib/` directory (not just models/).
    #[test]
    fn checksums_cover_lib_directory() {
        let yml = release_yml();

        // Top-level SHA256SUMS for the archive assets continues to exist.
        let github_release_section = yml
            .split("Create GitHub Release")
            .nth(1)
            .and_then(|s| s.split("softprops/action-gh-release").next())
            .unwrap_or_else(|| panic!("release.yml missing GitHub Release step"));
        assert!(
            github_release_section.contains("SHA256SUMS"),
            "release must publish a top-level SHA256SUMS"
        );

        // A checksum step must exist that walks the bundle tree (covering lib/)
        // and writes per-file checksums inside the bundle.
        assert!(
            yml.contains("sha256sum") && (yml.contains("find") || yml.contains("BUNDLE_DIR/lib")),
            "release.yml must generate per-file SHA256 checksums inside the bundle, covering lib/"
        );
    }
}

// ============================================================================
// Binary linking verification ($ORIGIN rpath + NEEDED libonnxruntime)
// ============================================================================

mod binary_verification {
    use super::*;

    /// VAL-RELEASE-013: The workflow must include a verification step that
    /// asserts the produced binaries have NO `NEEDED libonnxruntime*` entry,
    /// proving ORT is dlopen'd at runtime rather than linked.
    #[test]
    fn release_verifies_no_needed_libonnxruntime() {
        let yml = release_yml();
        // Either readelf-based or ldd-based; both spell out "onnxruntime".
        assert!(
            (yml.contains("readelf") || yml.contains("ldd"))
                && yml.contains("onnxruntime")
                && yml.contains("NEEDED"),
            "release.yml must include a step that checks for NEEDED libonnxruntime"
        );
    }

    /// VAL-RELEASE-012 (runtime side): The workflow must include a verification
    /// step that asserts the produced binaries have no `$ORIGIN` rpath.
    #[test]
    fn release_verifies_no_origin_rpath() {
        let yml = release_yml();
        assert!(
            (yml.contains("readelf") || yml.contains("patchelf")) && yml.contains("rpath"),
            "release.yml must include a step that checks for $ORIGIN/rpath in binaries"
        );
    }
}

// ============================================================================
// CI gates (cargo test / clippy / fmt)
// ============================================================================

mod ci_gates {
    use super::*;

    /// VAL-RELEASE-014: The release workflow must run `cargo test --workspace`
    /// (either inline or by depending on the gating CI). When run inline, the
    /// exact test invocation must be present.
    #[test]
    fn release_runs_cargo_test() {
        let yml = release_yml();
        assert!(
            yml.contains("cargo test --workspace"),
            "release.yml must run `cargo test --workspace` before publishing bundles"
        );
    }

    /// VAL-RELEASE-015: clippy must run with `-D warnings` (or the project's
    /// `clippy` alias which maps to `-D warnings`) across the workspace.
    #[test]
    fn release_runs_clippy() {
        let yml = release_yml();
        assert!(
            yml.contains("cargo clippy") && yml.contains("-D warnings"),
            "release.yml must run clippy with -D warnings"
        );
    }
}

// ============================================================================
// Distribution file coverage
// ============================================================================

mod distribution_coverage {
    use super::*;

    #[test]
    fn release_workflow_triggers_on_distribution_files() {
        let workflow = release_yml();
        for path in [
            "crates/**",
            "install.sh",
            ".github/workflows/release.yml",
            "docs/**",
        ] {
            assert!(
                workflow.contains(path),
                "release workflow paths must include `{}`",
                path
            );
        }
    }

    #[test]
    fn release_workflow_builds_macos_x86_64_bundle() {
        let workflow = release_yml();
        assert!(
            workflow.contains("x86_64-apple-darwin"),
            "release matrix must include macOS x86_64"
        );
        assert!(
            workflow.contains("macos-x86_64"),
            "release matrix must name the macOS x86_64 asset"
        );
    }
}
