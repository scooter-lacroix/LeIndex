//! Install script tests (VAL-RELEASE-019, VAL-RELEASE-020).
//!
//! These tests statically validate `install.sh` to ensure the installer
//! correctly consumes the release bundle distribution model introduced
//! by the load-dynamic ORT migration and the interactive `leindex setup`
//! command.
//!
//! Invariants tested:
//!
//!   * install.sh copies bundled ORT libs to `~/.leindex/lib/`
//!     (the GitHub Release bundle install path with pre-built `lib/`)
//!   * install.sh copies bundled models to `~/.leindex/models/`
//!     (supports both the bundle and local-repo build paths)
//!   * install.sh runs `leindex setup --check` after install to report
//!     neural search status, suggesting `leindex setup` when unconfigured
//!   * install.sh contains NO `ORT_LIB_PATH` or `ORT_PREFER_DYNAMIC_LINK`
//!     references (obsolete under load-dynamic)
//!   * install.sh's cargo build line uses the `onnx` (load-dynamic) feature
//!     and does NOT pass `onnx-migraphx` (the MIGraphX provider is a
//!     runtime-only concern handled by the discovered ORT library)
//!
//! The contract is documented in
//! `validation-contract.md` (Area: RELEASE, VAL-RELEASE-019/020) and
//! `architecture.md` (Section 3: Release Pipeline).

#![cfg(test)]

use std::path::PathBuf;

/// Return the absolute path to the repo root, computed from CARGO_MANIFEST_DIR
/// (the directory containing Cargo.toml of the `leindex` package under test).
fn repo_root() -> PathBuf {
    let dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(dir)
}

/// Read install.sh as a string. Panics with a clear diagnostic if the file
/// is missing so test failures point at the actual cause.
fn install_sh() -> String {
    let path = repo_root().join("install.sh");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

// ============================================================================
// ORT environment variable invariants (no build-time linking)
// ============================================================================

mod ort_env_invariants {
    use super::*;

    /// No line in install.sh may set `ORT_LIB_PATH`. Under the load-dynamic
    /// ort strategy, the worker discovers and dlopens libonnxruntime at
    /// runtime via the ort_discovery module — no build-time env var is
    /// needed, and its presence would indicate an incomplete migration.
    #[test]
    fn no_ort_lib_path_references() {
        let sh = install_sh();
        assert!(
            !sh.contains("ORT_LIB_PATH"),
            "install.sh must not contain ORT_LIB_PATH (obsolete under load-dynamic)"
        );
    }

    /// No line in install.sh may set `ORT_PREFER_DYNAMIC_LINK`. This env var
    /// was used by the old download-binaries + copy-dylibs strategy and has
    /// no effect under load-dynamic. Its presence would indicate stale
    /// references from the pre-migration distribution model.
    #[test]
    fn no_ort_prefer_dynamic_link_references() {
        let sh = install_sh();
        assert!(
            !sh.contains("ORT_PREFER_DYNAMIC_LINK"),
            "install.sh must not contain ORT_PREFER_DYNAMIC_LINK (obsolete under load-dynamic)"
        );
    }
}

// ============================================================================
// Build line invariants (load-dynamic, not onnx-migraphx)
// ============================================================================

mod build_line_invariants {
    use super::*;

    /// The `cargo build` invocation in install.sh must use the `onnx`
    /// feature (which carries the `load-dynamic` ort strategy), NOT
    /// `onnx-migraphx`. The MIGraphX (AMD GPU) provider is a runtime-only
    /// concern: it is provided by the onnxruntime-migraphx pip wheel or
    /// the bundled `lib/` directory (discovered at runtime). Building
    /// with `onnx` (load-dynamic) produces a binary that can use CPU,
    /// CUDA, or MIGraphX ORT at runtime depending on which library is
    /// discovered.
    ///
    /// We check that the actual `cargo build` command line contains
    /// `--features` with `onnx` and does NOT contain `onnx-migraphx` in
    /// the feature list.
    #[test]
    fn build_line_uses_onnx_not_onnx_migraphx() {
        let sh = install_sh();

        // Find the cargo build line. It must reference the onnx feature.
        let has_cargo_build = sh
            .lines()
            .any(|line| line.contains("cargo build") && line.contains("--features"));
        assert!(
            has_cargo_build,
            "install.sh must contain a cargo build command with --features"
        );

        // The build line must use onnx (load-dynamic), not onnx-migraphx.
        // We look at the actual cargo build invocation. A comment line
        // may mention onnx-migraphx (explaining why we DON'T use it),
        // so we filter non-comment, non-empty lines that carry the build.
        let build_lines: Vec<&str> = sh
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with('#')
            })
            .filter(|line| line.contains("cargo build") && line.contains("--features"))
            .collect();

        assert!(
            !build_lines.is_empty(),
            "install.sh must have at least one non-comment cargo build line with --features"
        );

        for line in &build_lines {
            // Must use onnx feature (load-dynamic)
            assert!(
                line.contains("onnx"),
                "cargo build line must reference the 'onnx' feature (load-dynamic): {line}"
            );
            // Must NOT enable onnx-migraphx as a build feature.
            // The feature is passed as leindex-embed/onnx-migraphx or
            // onnx-migraphx; either form indicates building the provider
            // bindings which is not needed under load-dynamic.
            assert!(
                !line.contains("onnx-migraphx"),
                "cargo build line must NOT contain onnx-migraphx (runtime-only under load-dynamic): {line}"
            );
        }
    }

    /// The build line must use `load-dynamic` compatible features. We verify
    /// that it does not pull in download-binaries or copy-dylibs (which would
    /// re-introduce build-time ORT linking).
    #[test]
    fn build_line_does_not_use_download_binaries_or_copy_dylibs() {
        let sh = install_sh();
        let build_lines: Vec<&str> = sh
            .lines()
            .filter(|line| {
                let trimmed = line.trim_start();
                !trimmed.starts_with('#')
            })
            .filter(|line| line.contains("cargo build") && line.contains("--features"))
            .collect();

        for line in &build_lines {
            assert!(
                !line.contains("download-binaries"),
                "cargo build line must not enable download-binaries: {line}"
            );
            assert!(
                !line.contains("copy-dylibs"),
                "cargo build line must not enable copy-dylibs: {line}"
            );
        }
    }
}

// ============================================================================
// Bundle consumption: lib/ and models/ install paths
// ============================================================================

mod bundle_consumption {
    use super::*;

    /// install.sh must copy bundled ORT runtime libraries from the release
    /// bundle's `lib/` directory to `~/.leindex/lib/`. This is the
    /// zero-setup path: the GitHub Release bundle includes pre-built ORT
    /// `.so` files, and installing them to `~/.leindex/lib/` means the
    /// worker's discovery chain finds ORT without requiring `pip install`
    /// or `leindex setup`.
    ///
    /// We verify install.sh contains a function or logic block that copies
    /// files matching `libonnxruntime` to the LEINDEX_HOME `lib/` subdirectory.
    #[test]
    fn copies_bundled_ort_libs_to_leindex_lib() {
        let sh = install_sh();

        // There must be a function that installs ORT libraries.
        assert!(
            sh.contains("install_ort_libraries"),
            "install.sh must define an install_ort_libraries function"
        );

        // It must target the LEINDEX_HOME lib/ subdirectory. The discovery
        // chain searches ~/.leindex/lib/ (from ort_discovery.rs).
        assert!(
            sh.contains("LEINDEX_HOME") && sh.contains("/lib"),
            "install.sh must reference LEINDEX_HOME and a lib/ subdirectory for ORT install"
        );

        // It must look for libonnxruntime files.
        assert!(
            sh.contains("libonnxruntime"),
            "install.sh must look for libonnxruntime shared library files"
        );
    }

    /// install.sh must copy bundled models to `~/.leindex/models/`. This
    /// applies to both the bundle install path (models/ in the archive)
    /// and the local-repo build path (models/ checked out alongside source).
    #[test]
    fn copies_bundled_models_to_leindex_models() {
        let sh = install_sh();

        // There must be a function that installs model assets.
        assert!(
            sh.contains("install_model_assets"),
            "install.sh must define an install_model_assets function"
        );

        // It must target the LEINDEX_HOME models/ subdirectory.
        assert!(
            sh.contains("models"),
            "install.sh must install model assets to a models/ directory"
        );

        // It must reference the primary model file.
        assert!(
            sh.contains("qwen3-embed-0.6b.onnx"),
            "install.sh must reference the qwen3-embed-0.6b.onnx model file"
        );
    }

    /// install.sh should prefer the pre-built release bundle (fast path,
    /// no Rust toolchain needed) before falling back to building from
    /// source. This is the distribution model described in architecture.md
    /// Section 3: install.sh "downloads (or builds) the bundle, copies lib/
    /// and models/ into ~/.leindex/".
    #[test]
    fn prefers_release_bundle_download() {
        let sh = install_sh();

        // There must be logic to download/extract the release bundle.
        assert!(
            sh.contains("try_install_from_release_bundle")
                || sh.contains("release bundle")
                || sh.contains("download_bundle"),
            "install.sh should attempt to download the pre-built release bundle"
        );
    }
}

// ============================================================================
// Post-install setup check
// ============================================================================

mod setup_check {
    use super::*;

    /// install.sh must run `leindex setup --check` after install to report
    /// the neural search configuration status. If neural search is not
    /// configured, it should suggest `leindex setup`. This bridges the
    /// installer to the interactive setup wizard introduced in Milestone 2.
    #[test]
    fn runs_setup_check_after_install() {
        let sh = install_sh();

        // Must invoke `leindex setup --check` (or via the binary path).
        assert!(
            sh.contains("setup --check") || sh.contains("setup\", \"--check"),
            "install.sh must run 'leindex setup --check' after install"
        );
    }

    /// After running the check, install.sh must suggest `leindex setup`
    /// when neural search is not configured. This provides actionable
    /// guidance for the user to enable semantic search.
    #[test]
    fn suggests_leindex_setup_when_unset() {
        let sh = install_sh();

        assert!(
            sh.contains("leindex setup"),
            "install.sh should mention 'leindex setup' for enabling neural search"
        );
    }
}

// ============================================================================
// Version parity
// ============================================================================

mod version_parity {
    use super::*;

    /// install.sh SCRIPT_VERSION must match the Cargo.toml version.
    /// Per AGENTS.md, all published surfaces must stay in version lock-step.
    #[test]
    fn script_version_matches_cargo_toml() {
        let sh = install_sh();
        let cargo_toml_path = repo_root().join("Cargo.toml");
        let cargo_toml = std::fs::read_to_string(&cargo_toml_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", cargo_toml_path.display()));

        let cargo_version = cargo_toml
            .lines()
            .find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("version = ")
                    .map(|v| v.trim_matches('"'))
            })
            .expect("Cargo.toml must have a top-level version = \"...\" line");

        let script_version = sh
            .lines()
            .find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("readonly SCRIPT_VERSION=")
                    .or_else(|| trimmed.strip_prefix("SCRIPT_VERSION="))
                    .map(|v| v.trim_matches('"'))
            })
            .expect("install.sh must define SCRIPT_VERSION=\"...\"");

        assert_eq!(
            cargo_version, script_version,
            "install.sh SCRIPT_VERSION ({script_version}) must match Cargo.toml version ({cargo_version})"
        );
    }
}
