//! Cargo install layout tests (VAL-CARGO-001..013).
//!
//! These tests statically verify that the root `Cargo.toml` is configured so
//! `cargo install leindex` (and the `--features onnx` / `--features
//! onnx-migraphx` variants) produces correct results without requiring ORT
//! at build time.
//!
//! Key invariants:
//!   * `cargo install leindex --features onnx` installs BOTH `leindex` and
//!     `leindex-embed` binaries (VAL-CARGO-002/005).
//!   * `cargo install leindex` (default features) installs `leindex`
//!     (VAL-CARGO-001/004).
//!   * The worker binary is gated by `required-features = ["onnx"]` so the
//!     default install is unaffected.
//!   * The `onnx` feature propagates to `leindex-embed/onnx` so the installed
//!     worker has real ONNX inference support (not a dummy fallback).
//!   * No `ORT_LIB_PATH` / `ORT_PREFER_DYNAMIC_LINK` entries remain in
//!     `.cargo/config.toml` (VAL-ORT-003 / VAL-DOCS-008/011).
//!   * No `$ORIGIN` rpath in build scripts (VAL-RELEASE-012).
//!   * No `ort-lib/` directory exists (VAL-ORT-004 / VAL-DOCS-009/010).
//!   * The worker binary exposes `--version` (VAL-CARGO-005 evidence).
//!
//! Because `cargo install` cannot run inside `cargo test`, we verify the
//! Cargo.toml metadata that drives the install layout. The end-to-end
//! install was validated manually during feature development.

#![cfg(test)]

use std::path::PathBuf;

/// Return the absolute path to the repo root.
fn repo_root() -> PathBuf {
    let dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(dir)
}

/// Read the root Cargo.toml as a string.
fn root_cargo_toml() -> String {
    let path = repo_root().join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Read the leindex-embed subcrate Cargo.toml as a string.
fn embed_cargo_toml() -> String {
    let path = repo_root()
        .join("crates")
        .join("leindex-embed")
        .join("Cargo.toml");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

/// Read the root .cargo/config.toml.
fn cargo_config_toml() -> String {
    let path = repo_root().join(".cargo").join("config.toml");
    std::fs::read_to_string(&path).unwrap_or_default()
}

/// Extract all `[[bin]]` blocks from a Cargo.toml string, correctly
/// stopping at the next `[[...]]` or `[...]` table header. Each returned
/// string contains all key=value lines belonging to one block.
fn bin_blocks(cargo_toml: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in cargo_toml.lines() {
        let t = line.trim();
        if t == "[[bin]]" {
            if let Some(b) = current.take() {
                blocks.push(b.join("\n"));
            }
            current = Some(Vec::new());
        } else if t.starts_with("[[")
            || (t.starts_with('[') && !t.starts_with("[[") && current.is_some())
        {
            // New table header: close current block
            if let Some(b) = current.take() {
                blocks.push(b.join("\n"));
            }
        } else if let Some(ref mut b) = current {
            if !t.is_empty() && !t.starts_with('#') {
                b.push(line);
            }
        }
    }
    if let Some(b) = current {
        blocks.push(b.join("\n"));
    }
    blocks
}

// ============================================================================
// VAL-CARGO-001/002/003: Both main binary and worker binary declared
// ============================================================================

mod binary_targets {
    use super::*;

    /// VAL-CARGO-002/005: The root package declares a `leindex-embed`
    /// `[[bin]]` target so `cargo install leindex --features onnx` installs
    /// both binaries. Without this, `cargo install` only installs binaries
    /// from the root crate's own `[[bin]]` targets (not from subcrates).
    #[test]
    fn root_declares_leindex_embed_bin_target() {
        let toml = root_cargo_toml();
        let bins = bin_blocks(&toml);

        let embed_bin = bins.iter().find(|b| b.contains("name = \"leindex-embed\""));
        assert!(
            embed_bin.is_some(),
            "Root Cargo.toml MUST declare a [[bin]] target named 'leindex-embed' \
             so cargo install co-installs the worker. Found bin blocks: {:?}",
            bins.iter()
                .map(|b| b.lines().next().unwrap_or(""))
                .collect::<Vec<_>>()
        );

        let embed = embed_bin.unwrap();
        // The path should point to the root src/bin/ wrapper
        assert!(
            embed.contains("path = \"src/bin/leindex-embed.rs\""),
            "leindex-embed bin target should point to src/bin/leindex-embed.rs, got: {}",
            embed
        );
        // Must be gated by required-features = ["onnx"]
        assert!(
            embed.contains("required-features = [\"onnx\"]"),
            "leindex-embed bin target must have required-features = [\"onnx\"], got: {}",
            embed
        );
    }

    /// VAL-CARGO-001/004: The root package also declares the `leindex`
    /// main binary.
    #[test]
    fn root_declares_leindex_bin_target() {
        let toml = root_cargo_toml();
        let bins = bin_blocks(&toml);

        let main_bin = bins.iter().find(|b| b.contains("name = \"leindex\""));
        assert!(
            main_bin.is_some(),
            "Root Cargo.toml MUST declare [[bin]] 'leindex'. Found: {:?}",
            bins.iter()
                .map(|b| b.lines().next().unwrap_or(""))
                .collect::<Vec<_>>()
        );
    }

    /// VAL-CARGO-005: There should be exactly two bin targets in the root.
    #[test]
    fn root_has_exactly_two_bin_targets() {
        let toml = root_cargo_toml();
        let bins = bin_blocks(&toml);
        assert_eq!(
            bins.len(),
            2,
            "Expected exactly 2 [[bin]] targets (leindex, leindex-embed), got {}: {:?}",
            bins.len(),
            bins.iter()
                .map(|b| b.lines().next().unwrap_or(""))
                .collect::<Vec<_>>()
        );
    }

    /// VAL-CARGO-002: The root wrapper source exists and calls the shared run fn.
    #[test]
    fn root_embed_wrapper_source_exists() {
        let path = repo_root().join("src").join("bin").join("leindex-embed.rs");
        assert!(path.exists(), "src/bin/leindex-embed.rs must exist");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        assert!(
            src.contains("leindex_embed::worker_main::run"),
            "Wrapper must call leindex_embed::worker_main::run()"
        );
    }

    /// VAL-CARGO-005: The shared worker_main module exists in leindex-embed.
    #[test]
    fn worker_main_module_exists() {
        let path = repo_root()
            .join("crates")
            .join("leindex-embed")
            .join("src")
            .join("worker_main.rs");
        assert!(
            path.exists(),
            "crates/leindex-embed/src/worker_main.rs must exist"
        );
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        assert!(src.contains("pub fn run()"));
        assert!(
            src.contains("--version"),
            "worker_main must handle --version"
        );
    }

    /// VAL-CARGO-005: The lib.rs exports worker_main.
    #[test]
    fn lib_exports_worker_main() {
        let path = repo_root()
            .join("crates")
            .join("leindex-embed")
            .join("src")
            .join("lib.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        assert!(
            src.contains("pub mod worker_main"),
            "lib.rs must export worker_main module"
        );
    }
}

// ============================================================================
// VAL-CARGO-002: The onnx feature propagates to leindex-embed/onnx
// ============================================================================

mod feature_propagation {
    use super::*;

    /// VAL-CARGO-002: The root `onnx` feature MUST enable `leindex-embed/onnx`
    /// so the installed worker binary has real ONNX inference support.
    /// Without this propagation, the worker would compile without onnx and
    /// produce dummy zeros instead of real embeddings.
    #[test]
    fn onnx_feature_propagates_to_leindex_embed() {
        let toml = root_cargo_toml();

        // Find the onnx feature line
        let onnx_line = toml
            .lines()
            .find(|l| l.trim_start().starts_with("onnx = ["))
            .expect("root Cargo.toml must define an 'onnx' feature");

        assert!(
            onnx_line.contains("leindex-embed/onnx"),
            "The root 'onnx' feature MUST include 'leindex-embed/onnx' so the worker \
             binary gets ONNX support. Got: {}",
            onnx_line
        );
    }

    /// VAL-CARGO-003: onnx-migraphx builds on onnx and adds migraphx.
    #[test]
    fn onnx_migraphx_feature_includes_migraphx() {
        let toml = root_cargo_toml();
        let migraphx_line = toml
            .lines()
            .find(|l| l.trim_start().starts_with("onnx-migraphx = ["))
            .expect("root Cargo.toml must define 'onnx-migraphx' feature");

        assert!(migraphx_line.contains("leindex-embed/onnx-migraphx"));
    }

    /// VAL-ORT-001/002: The ort crate uses load-dynamic (not download-binaries).
    /// We check only the active `ort = ...` dependency line, ignoring comments.
    #[test]
    fn ort_uses_load_dynamic() {
        let toml = embed_cargo_toml();

        // Find the ort dependency line (not a comment)
        let ort_line = toml
            .lines()
            .find(|l| {
                let t = l.trim();
                t.starts_with("ort = ") || t.starts_with("ort = {")
            })
            .expect("leindex-embed must declare an ort dependency");

        assert!(
            ort_line.contains("load-dynamic"),
            "leindex-embed ort dependency must use load-dynamic feature. Got: {}",
            ort_line
        );
        assert!(
            !ort_line.contains("download-binaries"),
            "leindex-embed ort dependency must NOT use download-binaries. Got: {}",
            ort_line
        );
        assert!(
            !ort_line.contains("copy-dylibs"),
            "leindex-embed ort dependency must NOT use copy-dylibs. Got: {}",
            ort_line
        );
    }
}

// ============================================================================
// VAL-CARGO-006: leindex setup subcommand is declared
// ============================================================================

mod setup_command {
    use super::*;

    /// VAL-CARGO-006: `leindex --help` lists `setup` and `leindex setup --help`
    /// exits 0. We statically verify the Setup variant is declared in cli.rs.
    #[test]
    fn setup_command_declared_in_cli() {
        let cli_path = repo_root().join("src").join("cli").join("cli.rs");
        let src = std::fs::read_to_string(&cli_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", cli_path.display()));

        assert!(
            src.contains("Setup {") || src.contains("Setup{"),
            "cli.rs must declare a Setup subcommand variant"
        );

        // VAL-CARGO-006 evidence: flags must include --neural, --cpu, --gpu, --no-neural, --check
        for flag in ["--neural", "--cpu", "--gpu", "--no-neural", "--check"] {
            let as_long = format!("long = \"{}\"", flag.trim_start_matches("--"));
            assert!(
                src.contains(&as_long),
                "cli.rs setup must declare flag '{}' (as '{}')",
                flag,
                as_long
            );
        }
    }
}

// ============================================================================
// VAL-CARGO-011: No ORT vendored into ~/.cargo/bin/
// (Static check: no build mechanism copies .so files into install root)
// ============================================================================

mod no_ort_vendoring {
    use super::*;

    /// VAL-CYARGO-011 / VAL-DOCS-008: No ORT_LIB_PATH or ORT_PREFER_DYNAMIC_LINK
    /// remain in .cargo/config.toml.
    #[test]
    fn cargo_config_has_no_ort_env_entries() {
        let cfg = cargo_config_toml();
        assert!(
            !cfg.contains("ORT_LIB_PATH"),
            ".cargo/config.toml must not set ORT_LIB_PATH (obsolete under load-dynamic)"
        );
        assert!(
            !cfg.contains("ORT_PREFER_DYNAMIC_LINK"),
            ".cargo/config.toml must not set ORT_PREFER_DYNAMIC_LINK"
        );
    }

    /// VAL-DOCS-009 / VAL-ORT-004: The ort-lib/ directory must not exist.
    #[test]
    fn ort_lib_directory_removed() {
        let ort_lib = repo_root().join("ort-lib");
        assert!(
            !ort_lib.exists(),
            "ort-lib/ directory must not exist (it was the old build-time ORT cache)"
        );
    }

    /// VAL-DOCS-010: No source references to ort-lib/ remain.
    #[test]
    fn no_source_references_to_ort_lib() {
        // Check build scripts
        for rel in ["build.rs", "crates/leindex-embed/build.rs"] {
            let path = repo_root().join(rel);
            if let Ok(src) = std::fs::read_to_string(&path) {
                // Allow references in comments explaining what was removed
                for line in src.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//") || trimmed.starts_with("#") {
                        continue;
                    }
                    assert!(
                        !trimmed.contains("ort-lib"),
                        "Build script {} references ort-lib/ in code: {}",
                        rel,
                        trimmed
                    );
                }
            }
        }
    }

    /// VAL-RELEASE-012: No $ORIGIN rpath in build scripts.
    #[test]
    fn build_scripts_have_no_origin_rpath() {
        for rel in ["build.rs", "crates/leindex-embed/build.rs"] {
            let path = repo_root().join(rel);
            if let Ok(src) = std::fs::read_to_string(&path) {
                for line in src.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with("//") {
                        continue;
                    }
                    assert!(
                        !trimmed.contains("$ORIGIN"),
                        "Build script {} still uses $ORIGIN rpath: {}",
                        rel,
                        trimmed
                    );
                }
            }
        }
    }
}

// ============================================================================
// VAL-CARGO-012/013: Version parity
// ============================================================================

mod version_parity {
    use super::*;

    /// Extract the version = "..." line from a TOML string.
    fn extract_version(toml: &str) -> Option<String> {
        for line in toml.lines() {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("version = ") {
                let v = rest.trim().trim_matches('"');
                if !v.is_empty() {
                    return Some(v.to_string());
                }
            }
        }
        None
    }

    /// VAL-DOCS-005 / VAL-CARGO-012: Root and subcrate versions must match.
    #[test]
    fn versions_match_across_crates() {
        let root = root_cargo_toml();
        let embed = embed_cargo_toml();
        let root_v = extract_version(&root).expect("root Cargo.toml missing version");
        let embed_v = extract_version(&embed).expect("embed Cargo.toml missing version");
        assert_eq!(
            root_v, embed_v,
            "Version mismatch: root={} embed={}",
            root_v, embed_v
        );
    }
}
