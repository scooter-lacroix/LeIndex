//! Cargo install layout tests (VAL-CARGO-001..013).
//!
//! One published crate, two installed processes. These tests statically verify
//! that the root `Cargo.toml` is configured so `cargo install leindex` (and the
//! `--features onnx` / `--features onnx-migraphx` variants) produces correct
//! results without requiring ORT at build time.
//!
//! Key invariants:
//!   * `cargo install leindex --features onnx` installs BOTH `leindex` and
//!     `leindex-embed` binaries from the single root crate (VAL-CARGO-002/005).
//!   * `cargo install leindex` (default features) installs `leindex`
//!     (VAL-CARGO-001/004).
//!   * The worker binary is gated by `required-features = ["onnx"]` so the
//!     default install is unaffected.
//!   * The worker source lives at `src/embed/` (no separate subcrate); the
//!     `onnx` feature compiles every runtime-selectable EP API.
//!   * No `ORT_LIB_PATH` / `ORT_PREFER_DYNAMIC_LINK` entries remain in
//!     `.cargo/config.toml` (VAL-ORT-003 / VAL-DOCS-008/011).
//!   * No `$ORIGIN` rpath in the build script (VAL-RELEASE-012).
//!   * No `ort-lib/` directory exists (VAL-ORT-004 / VAL-DOCS-009/010).
//!   * The worker binary exposes `--version` (VAL-CARGO-005 evidence).
//!
//! Because `cargo install` cannot run inside `cargo test`, we verify the
//! Cargo.toml metadata that drives the install layout. The end-to-end install
//! was validated manually during feature development.

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
        } else if let Some(ref mut b) = current
            && !t.is_empty()
            && !t.starts_with('#')
        {
            b.push(line);
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

    /// Single-binary (2026-08-20): the worker is reached by `leindex
    /// --internal-embed-worker` re-exec. A separate `leindex-embed` [[bin]]
    /// must NOT return — shipping two binaries reintroduces the
    /// discovery/version-mismatch failure class the re-exec removes.
    #[test]
    fn test_root_has_no_embed_bin_target() {
        let cargo = std::fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
        assert!(
            !cargo.contains("name = \"leindex-embed\""),
            "Root Cargo.toml must NOT declare a [[bin]] target named 'leindex-embed' \
             (worker mode is a hidden re-exec of the single binary)"
        );
        assert!(
            !repo_root()
                .join("src")
                .join("bin")
                .join("leindex-embed.rs")
                .exists(),
            "src/bin/leindex-embed.rs must not exist"
        );
    }

    /// (leindex, leindexd).
    #[test]
    fn root_has_exactly_two_bin_targets() {
        let toml = root_cargo_toml();
        let bins = bin_blocks(&toml);
        assert_eq!(
            bins.len(),
            2,
            "Expected exactly 2 [[bin]] targets (leindex, leindexd), got {}: {:?}",
            bins.len(),
            bins.iter()
                .map(|b| b.lines().next().unwrap_or(""))
                .collect::<Vec<_>>()
        );
    }

    /// VAL-CARGO-005: The shared worker_main module exists in src/embed.
    #[test]
    fn worker_main_module_exists() {
        let path = repo_root().join("src").join("embed").join("worker_main.rs");
        assert!(
            path.exists(),
            "src/embed/worker_main.rs must exist (worker source merged into root)"
        );
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        assert!(src.contains("pub fn run()"));
        assert!(
            src.contains("--version"),
            "worker_main must handle --version"
        );
    }

    /// VAL-CARGO-005: The embed module exports worker_main.
    #[test]
    fn lib_exports_worker_main() {
        let path = repo_root().join("src").join("embed").join("mod.rs");
        let src = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()));
        assert!(
            src.contains("pub mod worker_main"),
            "src/embed/mod.rs must export worker_main module"
        );
    }
}

// ============================================================================
// VAL-CARGO-002: The onnx feature compiles every runtime-selectable EP
// ============================================================================

mod feature_propagation {
    use super::*;

    /// VAL-ORT-EP: root onnx enables every runtime-selectable execution-provider
    /// API (cuda, migraphx, rocm, coreml) so the worker can register any of them.
    #[test]
    fn onnx_feature_enables_all_execution_providers() {
        let toml = root_cargo_toml();
        let onnx_line = toml
            .lines()
            .find(|l| l.trim_start().starts_with("onnx = ["))
            .expect("root Cargo.toml must define an 'onnx' feature");
        for ep in ["ort/cuda", "ort/migraphx", "ort/rocm", "ort/coreml"] {
            assert!(
                onnx_line.contains(ep),
                "root 'onnx' feature MUST include '{}'. Got: {}",
                ep,
                onnx_line
            );
        }
    }

    /// One-crate invariant: the retired worker subcrate no longer exists,
    /// so the onnx feature must NOT reference it.
    #[test]
    fn onnx_feature_has_no_leindex_embed_propagation() {
        let toml = root_cargo_toml();
        let onnx_line = toml
            .lines()
            .find(|l| l.trim_start().starts_with("onnx = ["))
            .expect("root Cargo.toml must define an 'onnx' feature");
        assert!(
            !onnx_line.contains("leindex-embed"),
            "one-crate layout: onnx feature must not reference the retired subcrate. Got: {}",
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

        assert!(
            migraphx_line.contains("\"onnx\""),
            "onnx-migraphx is now a legacy alias for the onnx feature (which includes \
             ort/migraphx). Got: {}",
            migraphx_line
        );
    }

    /// VAL-ORT-001/002: The ort crate uses load-dynamic (not download-binaries).
    /// We check only the active `ort = ...` dependency line in the root manifest.
    #[test]
    fn ort_uses_load_dynamic() {
        let toml = root_cargo_toml();

        let ort_line = toml
            .lines()
            .find(|l| {
                let t = l.trim();
                t.starts_with("ort = ") || t.starts_with("ort = {")
            })
            .expect("root Cargo.toml must declare an ort dependency");

        assert!(
            ort_line.contains("load-dynamic"),
            "root ort dependency must use load-dynamic feature. Got: {}",
            ort_line
        );
        assert!(
            !ort_line.contains("download-binaries"),
            "root ort dependency must NOT use download-binaries. Got: {}",
            ort_line
        );
        assert!(
            !ort_line.contains("copy-dylibs"),
            "root ort dependency must NOT use copy-dylibs. Got: {}",
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
    /// exits 0. We statically verify the Setup variant is declared. The clap
    /// derive surface lives in cli_commands.rs (extracted from cli.rs by the
    /// large-file gate); the dispatch match arm remains in cli.rs.
    #[test]
    fn setup_command_declared_in_cli() {
        let cli_path = repo_root().join("src").join("cli").join("cli.rs");
        let src = std::fs::read_to_string(&cli_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", cli_path.display()));

        assert!(
            src.contains("Commands::Setup {"),
            "cli.rs must dispatch a Setup subcommand"
        );

        // VAL-CARGO-006 evidence: flags must include --neural, --cpu, --gpu, --no-neural, --check.
        // The declarations live in cli_commands.rs since the derive-surface extraction.
        let commands_path = repo_root().join("src").join("cli").join("cli_commands.rs");
        let commands_src = std::fs::read_to_string(&commands_path)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", commands_path.display()));

        assert!(
            commands_src.contains("Setup {"),
            "cli_commands.rs must declare a Setup subcommand variant"
        );

        // Restrict the flag check to the Setup variant's body (the variant
        // begins at `Setup {` and ends at the next top-level variant or the
        // closing of the enum, whichever comes first).
        let setup_body = commands_src
            .split("Setup {")
            .nth(1)
            .and_then(|rest| rest.split("    }").next())
            .unwrap_or("");
        for flag in ["--neural", "--cpu", "--gpu", "--no-neural", "--check"] {
            let as_long = format!("long = \"{}\"", flag.trim_start_matches("--"));
            assert!(
                setup_body.contains(&as_long),
                "cli setup must declare flag '{}' (as '{}')",
                flag,
                as_long
            );
        }
    }
}

// ============================================================================
// VAL-CARGO-011: No ORT vendored into ~/.cargo/bin/
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

    /// VAL-DOCS-010: No source references to ort-lib/ remain in the root build script.
    #[test]
    fn no_source_references_to_ort_lib() {
        let path = repo_root().join("build.rs");
        if let Ok(src) = std::fs::read_to_string(&path) {
            for line in src.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") || trimmed.starts_with("#") {
                    continue;
                }
                assert!(
                    !trimmed.contains("ort-lib"),
                    "Build script references ort-lib/ in code: {}",
                    trimmed
                );
            }
        }
    }

    /// VAL-RELEASE-012: No $ORIGIN rpath in the root build script.
    #[test]
    fn build_scripts_have_no_origin_rpath() {
        let path = repo_root().join("build.rs");
        if let Ok(src) = std::fs::read_to_string(&path) {
            for line in src.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("//") {
                    continue;
                }
                assert!(
                    !trimmed.contains("$ORIGIN"),
                    "Build script still uses $ORIGIN rpath: {}",
                    trimmed
                );
            }
        }
    }
}

// ============================================================================
// VAL-CARGO-012/013: One-crate / two-bin layout
// ============================================================================

mod one_crate_layout {
    use super::*;

    /// VAL-CARGO-012: The retired leindex-embed subcrate is gone; worker source
    /// lives at src/embed/ (single-binary re-exec; no wrapper).
    #[test]
    fn one_crate_two_bin_layout() {
        let root = repo_root();
        assert!(
            !root.join("crates").join("leindex-embed").exists(),
            "crates/leindex-embed must not exist (subcrate retired into src/embed)"
        );
        assert!(root.join("src").join("embed").join("mod.rs").is_file());
        assert!(
            root.join("src")
                .join("embed")
                .join("worker_main.rs")
                .is_file()
        );
        assert!(
            !root
                .join("src")
                .join("bin")
                .join("leindex-embed.rs")
                .is_file(),
            "single-binary: the wrapper must stay removed (worker is a re-exec)"
        );
    }
}

// ============================================================================
// Neural setup docs packaging
// ============================================================================

mod docs_packaging {
    use super::*;

    #[test]
    fn crate_package_includes_neural_setup_doc() {
        let cargo = root_cargo_toml();
        assert!(
            cargo.contains("/docs/NEURAL_SETUP.md"),
            "crate package include list must contain docs/NEURAL_SETUP.md because README points to it"
        );
    }

    /// Package includes must ship the merged worker source + shared config/skip_dirs.
    #[test]
    fn crate_package_includes_embed_source() {
        let cargo = root_cargo_toml();
        for inc in ["/src/embed/**", "/src/config.rs", "/src/skip_dirs.rs"] {
            assert!(
                cargo.contains(inc),
                "crate package include list must contain '{}' (merged worker source)",
                inc
            );
        }
    }
}

// ============================================================================
// `full` completeness — the default feature set must not silently drop a feature
// ============================================================================

mod feature_completeness {
    use super::*;

    /// Every feature defined in the root `[features]` table, as
    /// (name, value) with a multi-line array collapsed onto one line. The
    /// `toml` crate is an optional dependency and is not available to
    /// integration tests, so this reads the table structurally instead.
    fn feature_table(cargo_toml: &str) -> Vec<(String, String)> {
        /// Bracket depth of `value`, counting only unquoted `[` and `]`.
        fn open_brackets(value: &str) -> usize {
            let mut depth = 0i32;
            let mut in_string = false;
            for ch in value.chars() {
                match ch {
                    '"' => in_string = !in_string,
                    '[' if !in_string => depth += 1,
                    ']' if !in_string => depth -= 1,
                    _ => {}
                }
            }
            depth.max(0) as usize
        }

        let mut out: Vec<(String, String)> = Vec::new();
        let mut in_features = false;
        let mut current: Option<(String, String)> = None;

        for line in cargo_toml.lines() {
            let t = line.trim();
            if t.starts_with('[') && open_brackets(t) == 0 && !t.contains('=') {
                // A table header, not an array continuation (`full = [` ... `]`).
                if let Some(c) = current.take() {
                    out.push(c);
                }
                in_features = t == "[features]";
                continue;
            }
            if !in_features || t.is_empty() || t.starts_with('#') {
                continue;
            }
            let continuing = current
                .as_ref()
                .is_some_and(|(_, value)| open_brackets(value) > 0);
            if continuing {
                let (_, value) = current.as_mut().expect("checked by `continuing`");
                value.push(' ');
                value.push_str(t);
            } else {
                let (name, value) = t
                    .split_once('=')
                    .unwrap_or_else(|| panic!("unparsable [features] line: {t}"));
                if let Some(c) = current.take() {
                    out.push(c);
                }
                current = Some((name.trim().to_string(), value.trim().to_string()));
            }
        }
        if let Some(c) = current.take() {
            out.push(c);
        }
        out
    }

    /// VAL-CARGO-014: every feature the crate defines is a member of `full`.
    ///
    /// A feature omitted from `full` is silently compiled out of a default
    /// build, and the failure surfaces far away as an `INACTIVE` setup check
    /// with no `issue:` line. That is exactly how `system-one` and `onnx` were
    /// lost. `default` selects the set, `minimal` is the deliberate opt-out,
    /// and `full` is the umbrella itself — those three are not members.
    #[test]
    fn test_full_feature_set_includes_every_defined_feature() {
        let features = feature_table(&root_cargo_toml());
        let full = features
            .iter()
            .find(|(name, _)| name == "full")
            .map(|(_, value)| value.clone())
            .expect("root Cargo.toml must define a 'full' feature");

        let exempt = ["default", "full", "minimal"];
        let missing: Vec<&str> = features
            .iter()
            .map(|(name, _)| name.as_str())
            .filter(|name| !exempt.contains(name))
            .filter(|name| !full.contains(&format!("\"{name}\"")))
            .collect();

        assert!(
            missing.is_empty(),
            "root 'full' feature MUST list every defined feature; missing: {missing:?}.\n\
             full = {full}"
        );
    }
}
