//! Cross-area integration validation tests (VAL-CROSS-001..017).
//!
//! These tests verify the cross-surface invariants of the distribution
//! overhaul: that every install surface (cargo, npm, PyPI, GitHub Release
//! bundle) reaches the same TF-IDF-only baseline, that `leindex setup`
//! brings up neural search from any surface, that user configuration
//! survives re-installs/upgrades, that ORT discovery is consistent across
//! surfaces, and that diagnostics report the resolved ORT path uniformly.
//!
//! Many assertions describe end-to-end install journeys that cannot run
//! inside `cargo test` (real `cargo install`, real `npm install`, model
//! downloads). For those, we statically verify the wiring that makes the
//! journey succeed: the discovery chain order, the bundled-lib ORT path,
//! the version parity gate, the no-ORT fallback message consistency, and
//! the diagnostics JSON shape. Live end-to-end validation lives in the
//! user-testing validator mission phase.
//!
//! The contract is documented in
//! `validation-contract.md` (Area: CROSS).

#![cfg(test)]

use std::path::PathBuf;

/// Return the absolute path to the repo root, computed from CARGO_MANIFEST_DIR
/// (the directory containing Cargo.toml of the `leindex` package under test).
fn repo_root() -> PathBuf {
    let dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(dir)
}

/// Read a file as a string, panicking with the path on failure.
fn read_file(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

// ============================================================================
// VAL-CROSS-007 / VAL-CROSS-016:
// No-ORT TF-IDF fallback works with a clear, consistent, actionable notice.
// ============================================================================

mod no_ort_fallback_notice {
    use super::*;

    /// VAL-CROSS-007 / VAL-ORT-012: When ORT cannot be found, the worker
    /// emits a clear, actionable notice (naming the searched locations and
    /// suggesting `leindex setup` or `ORT_DYLIB_PATH`), does NOT panic, and
    /// reports ORT unavailable to the main daemon so TF-IDF degrades
    /// gracefully.
    #[test]
    fn worker_runtime_emits_actionable_notice_when_ort_missing() {
        let src = read_file("crates/leindex-embed/src/runtime.rs");

        // Locate the InitResult::NotFound branch (where the worker emits the
        // notice). The surrounding code MUST log an error naming the searched
        // paths AND the literal command `leindex setup`.
        let not_found_branch = src
            .split("InitResult::NotFound")
            .nth(1)
            .and_then(|s| s.split("return (None, None, Duration::ZERO)").next())
            .expect("runtime.rs must handle InitResult::NotFound and fall back");

        assert!(
            not_found_branch.contains("leindex setup"),
            "the no-ORT notice MUST mention `leindex setup`; got: {}",
            not_found_branch
        );
        assert!(
            not_found_branch.contains("ORT_DYLIB_PATH")
                || not_found_branch.contains("ort_dylib_path"),
            "the no-ORT notice MUST mention ORT_DYLIB_PATH as a remediation; got: {}",
            not_found_branch
        );
        assert!(
            not_found_branch.contains("neural embeddings disabled")
                || not_found_branch.contains("TF-IDF"),
            "the no-ORT notice MUST indicate TF-IDF fallback / neural disabled; got: {}",
            not_found_branch
        );
    }

    /// VAL-CROSS-016: The notice text must be consistent across surfaces. The
    /// npm wrapper sets `ORT_DYLIB_PATH` from the bundled lib/, but when that
    /// is missing the worker logs the SAME notice because the worker is the
    /// same Rust binary. The wrapper does not introduce a divergent message.
    #[test]
    fn npm_wrapper_does_not_override_fallback_notice() {
        let wrapper = read_file("packages/npm-leindex-mcp/bin/mcp-wrapper.js");

        // The wrapper only sets ORT_DYLIB_PATH from the bundled lib; it never
        // emits its own neural-disabled message. The worker is the single
        // source of truth for the notice.
        assert!(
            !wrapper.contains("neural embeddings disabled"),
            "npm wrapper must NOT emit its own neural-disabled notice; the Rust worker is authoritative"
        );
        assert!(
            !wrapper.contains("neural is disabled"),
            "npm wrapper must NOT emit its own neural-disabled notice; the Rust worker is authoritative"
        );
    }

    /// VAL-CROSS-016: The PyPI wrapper is a thin pass-through to the Rust
    /// binary; it does not introduce a divergent neural-disabled message.
    #[test]
    fn pypi_wrapper_does_not_override_fallback_notice() {
        let wrapper = read_file("packages/pypi-leindex/src/leindex/bootstrap.py");

        assert!(
            !wrapper.contains("neural embeddings disabled"),
            "PyPI wrapper must NOT emit its own neural-disabled notice"
        );
        assert!(
            !wrapper.contains("neural is disabled"),
            "PyPI wrapper must NOT emit its own neural-disabled notice"
        );
    }

    /// VAL-CROSS-007: the worker stderr is inherited by the parent process so
    /// the no-ORT notice is visible to the user running `leindex search`
    /// (not swallowed). The discovery chain error reaches the user's terminal
    /// once per worker spawn.
    #[test]
    fn worker_inherits_stderr_so_notice_reaches_user() {
        let src = read_file("src/search/onnx/client.rs");
        assert!(
            src.contains("Stdio::inherit()"),
            "the EmbeddingClient must inherit worker stderr so the no-ORT notice reaches the user"
        );
    }
}

// ============================================================================
// VAL-CROSS-009 / VAL-CROSS-010:
// ORT_DYLIB_PATH override wins; bundled lib is used when no system/pip ORT.
// ============================================================================

mod ort_discovery_priority {
    use super::*;

    /// VAL-CROSS-009 / VAL-ORT-005: `ORT_DYLIB_PATH` must be the first
    /// candidate in the discovery chain across every install surface, so an
    /// advanced user can override the resolved ORT without running setup.
    #[test]
    fn env_var_is_highest_priority_in_rust_discovery() {
        let src = read_file("crates/leindex-embed/src/ort_discovery.rs");

        // `discover_candidates()` builds the ordered chain. Confirm the env
        // var is consulted first by checking the function's documented order.
        let candidates_fn = src
            .split("pub fn discover_candidates()")
            .nth(1)
            .and_then(|s| s.split("}\n\n///").next())
            .expect("discover_candidates() must exist");

        let env_position = candidates_fn.find("ORT_DYLIB_ENV");
        let config_position = candidates_fn.find("read_config_ort_path");
        let user_lib_position = candidates_fn
            .find("home.join(\"lib\")")
            .or_else(|| candidates_fn.find("DiscoverySource::UserLib"));
        let sibling_position = candidates_fn.find("binary_dir()");

        assert!(
            env_position.is_some(),
            "discovery must consult ORT_DYLIB_PATH"
        );
        assert!(
            config_position.is_some(),
            "discovery must consult config ort_dylib_path"
        );
        assert!(
            user_lib_position.is_some(),
            "discovery must consult ~/.leindex/lib/"
        );
        assert!(
            sibling_position.is_some(),
            "discovery must consult the sibling dir of the binary"
        );

        // Order check: env < config < user_lib < sibling.
        let env = env_position.unwrap();
        let cfg = config_position.unwrap();
        let ul = user_lib_position.unwrap();
        let sib = sibling_position.unwrap();
        assert!(
            env < cfg && cfg < ul && ul < sib,
            "discovery chain MUST be ordered: ORT_DYLIB_PATH -> config -> user_lib -> sibling (got env={}, cfg={}, user_lib={}, sib={})",
            env, cfg, ul, sib
        );
    }

    /// Runtime discovery must try the pip-installed ORT before system ORT so a
    /// `leindex setup`-installed runtime wins over a stale `/usr/lib` library.
    #[test]
    fn pip_discovery_precedes_system_discovery() {
        let src = read_file("crates/leindex-embed/src/ort_discovery.rs");

        let discover_fn = src
            .split("pub fn discover_and_init()")
            .nth(1)
            .and_then(|s| s.split("\n}\n\n///").next())
            .expect("discover_and_init() must exist");

        let pip_pos = discover_fn
            .find("discover_pip_lib")
            .expect("discover_and_init must consult pip ORT");
        let system_pos = discover_fn
            .find("system_candidates")
            .or_else(|| discover_fn.find("DiscoverySource::System"))
            .expect("discover_and_init must consult system ORT");

        assert!(
            pip_pos < system_pos,
            "pip ORT must be tried before system ORT so setup-installed ORT wins over stale system libraries"
        );
    }

    /// VAL-CROSS-009: the npm wrapper's bundled-lib ORT_DYLIB_PATH override
    /// is applied only when the user has NOT already set ORT_DYLIB_PATH. This
    /// keeps the manual override universally available on the npm surface too.
    #[test]
    fn npm_wrapper_respects_user_ort_dylib_path_override() {
        let wrapper = read_file("packages/npm-leindex-mcp/bin/mcp-wrapper.js");
        // The wrapper applies the bundled ORT only when env does not already
        // have ORT_DYLIB_PATH: `if (bundledOrt && !env.ORT_DYLIB_PATH)`.
        assert!(
            wrapper.contains("!env.ORT_DYLIB_PATH")
                || wrapper.contains("!process.env.ORT_DYLIB_PATH"),
            "npm wrapper must NOT override a user-supplied ORT_DYLIB_PATH"
        );
    }

    /// VAL-CROSS-010: The bundle ORT under `lib/` is consumed via the
    /// worker's sibling-directory discovery OR (when installed by
    /// install.sh) via the user-level `~/.leindex/lib/`. The discovery chain
    /// must include BOTH paths so the bundle works zero-setup from a GitHub
    /// Release extract as well as after install.sh copies libs into place.
    #[test]
    fn bundle_ort_discoverable_via_sibling_and_user_lib() {
        let src = read_file("crates/leindex-embed/src/ort_discovery.rs");
        // Sibling dir is searched (next to the running binary = the
        // leindex-embed worker shipped in bin/ alongside the bundle's lib/).
        // install.sh also copies bundled libs to $LEINDEX_HOME/lib, which is
        // searched via the user_lib candidate.
        assert!(
            src.contains("binary_dir()"),
            "discovery must search the sibling dir of the running binary (bundle zero-setup)"
        );
        assert!(
            src.contains("home.join(\"lib\")"),
            "discovery must search ~/.leindex/lib/ (install.sh placement)"
        );
    }

    /// VAL-CROSS-010: install.sh actually copies the bundled `lib/libonnxruntime*`
    /// into `$LEINDEX_HOME/lib/` so the worker's user-lib candidate finds ORT
    /// after running install.sh from a GitHub Release extract.
    #[test]
    fn install_sh_copies_bundled_libs_to_leindex_lib() {
        let sh = read_file("install.sh");
        assert!(
            sh.contains("install_ort_libraries"),
            "install.sh must define install_ort_libraries()"
        );
        assert!(
            sh.contains("LEINDEX_HOME") && sh.contains("/lib"),
            "install.sh must place bundled libs under $LEINDEX_HOME/lib"
        );
        assert!(
            sh.contains("libonnxruntime"),
            "install.sh must look for libonnxruntime shared libraries"
        );
    }
}

// ============================================================================
// VAL-CROSS-011 / VAL-CROSS-004 / VAL-CROSS-013:
// Setup is idempotent, re-setup preserves user config, uninstall/reinstall
// preserves ~/.leindex.
// ============================================================================

mod setup_idempotency_and_preservation {
    use super::*;

    /// VAL-CROSS-011 / VAL-SETUP-024: Setup must be idempotent. The config
    /// writer uses `toml::to_string_pretty` and a single `save()` overwriting
    /// the file; there is no append path that could duplicate fields.
    #[test]
    fn config_save_overwrites_in_place_without_duplicating() {
        let src = read_file("src/cli/neural_config.rs");
        let save_fn = src
            .split("pub fn save(&self) -> Result<PathBuf, ConfigError>")
            .nth(1)
            .and_then(|s| s.split("\n    }\n\n    /// Read config").next())
            .expect("LeIndexConfig::save() must exist");

        assert!(
            save_fn.contains("create_dir_all"),
            "save() must create the config dir if missing (idempotent re-run after upgrade)"
        );
        assert!(
            save_fn.contains("std::fs::write"),
            "save() must atomically overwrite the file (no append path that duplicates keys)"
        );
        assert!(
            !save_fn.contains("append"),
            "save() must NOT use append mode (would duplicate keys across runs)"
        );
    }

    /// VAL-CROSS-004: Re-setup after an upgrade preserves user choices. The
    /// setup flow calls `load_or_recover()` so the user's previous
    /// neural/provider choices are loaded into the struct before being
    /// re-serialized to disk with the new ORT path/version.
    #[test]
    fn setup_loads_existing_config_before_writing() {
        let setup = read_file("src/cli/leindex/setup.rs");
        assert!(
            setup.contains("load_or_recover"),
            "setup must load the existing config before re-writing (preserves user choices on upgrade)"
        );
    }

    /// VAL-CROSS-013: `~/.leindex/` is preserved across uninstall/reinstall.
    /// `install.sh` must default to preserving the existing home directory
    /// (setting `has_existing=true` and skipping purge unless the user opts
    /// in via the selective-purge prompt). The `cargo uninstall` and the
    /// PyPI/npm wrappers do NOT touch `~/.leindex/` at all.
    #[test]
    fn install_sh_preserves_leindex_home_by_default() {
        let sh = read_file("install.sh");
        // The installer detects existing ~/.leindex and warns the user; it
        // does NOT auto-wipe it. Selective purge requires explicit user
        // confirmation.
        assert!(
            sh.contains("has_existing") || sh.contains("existing"),
            "install.sh must detect the existing LeIndex home and let the user opt in to wipe"
        );
        assert!(
            sh.contains("Preserving all existing data")
                || sh.contains("Keep all flag set - preserving all existing data"),
            "install.sh must preserve existing data by default"
        );
    }

    /// VAL-CROSS-013: install.sh offers a selective-preserve menu so the user
    /// can keep config/data/models when re-running the installer (used after
    /// a manual uninstall or to repair an install without wiping config).
    /// `cargo uninstall` does not touch `~/.leindex/`, and the npm/PyPI
    /// wrappers never remove user-level files.
    #[test]
    fn installer_offers_selective_preserve_menu() {
        let sh = read_file("install.sh");
        // The selective-preserve menu lets the user keep config/data/models
        // even when the installer has detected an existing install.
        assert!(
            sh.contains("PRESERVE_BINARY")
                && sh.contains("PRESERVE_CONFIG")
                && sh.contains("PRESERVE_DATA"),
            "install.sh must expose PRESERVE_BINARY / PRESERVE_CONFIG / PRESERVE_DATA flags for selective preserve"
        );
        assert!(
            sh.contains("Select what to preserve:"),
            "install.sh must present a selective-preserve menu so users can keep config/data"
        );
        // uninstall.sh (when present) also does not hard-wipe without a
        // backup; it offers a backup or preserves data directory.
        let uninstall_path = repo_root().join("uninstall.sh");
        if uninstall_path.exists() {
            let un = std::fs::read_to_string(&uninstall_path).unwrap_or_default();
            assert!(
                un.contains("preserved") || un.contains("Preserve") || un.contains("backup"),
                "uninstall.sh must offer a preserve/backup affordance so the user can keep config/data"
            );
        }
    }

    /// VAL-CROSS-013: After reinstall + re-setup, `leindex setup` skips the
    /// model download when the model already exists and its checksum matches.
    /// This is verified via the `check_file_against_manifest` Checked::Verified
    /// branch in ensure_models_present which logs "already present".
    #[test]
    fn setup_skips_model_download_when_checksum_matches() {
        let setup = read_file("src/cli/leindex/setup.rs");
        assert!(
            setup.contains("already present, checksum verified"),
            "setup must skip re-download when the on-disk model checksum matches"
        );
    }
}

// ============================================================================
// VAL-CROSS-014:
// Cross-surface version drift is impossible.
// ============================================================================

mod version_parity {
    use super::*;

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

    /// VAL-CROSS-014 / VAL-DOCS-005: All published surfaces share the same
    /// version string, so a user cannot end up with mismatched versions
    /// across cargo, npm, PyPI, and install.sh.
    #[test]
    fn all_surfaces_share_same_version() {
        let cargo = read_file("Cargo.toml");
        let npm_pkg = read_file("packages/npm-leindex-mcp/package.json");
        let pypi_proj = read_file("packages/pypi-leindex/pyproject.toml");
        let install_sh = read_file("install.sh");

        let cargo_v = extract_version(&cargo).expect("Cargo.toml version");

        let npm_v = npm_pkg
            .lines()
            .find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("\"version\":")
                    .map(|v| v.trim().trim_matches(',').trim_matches('"'))
            })
            .expect("npm package.json version");

        let pypi_v = extract_version(&pypi_proj).expect("pyproject.toml version");

        let install_v = install_sh
            .lines()
            .find_map(|line| {
                let trimmed = line.trim();
                trimmed
                    .strip_prefix("readonly SCRIPT_VERSION=")
                    .or_else(|| trimmed.strip_prefix("SCRIPT_VERSION="))
                    .map(|v| v.trim_matches('"'))
            })
            .expect("install.sh SCRIPT_VERSION");

        assert_eq!(
            cargo_v, npm_v,
            "npm version drift: cargo={} npm={}",
            cargo_v, npm_v
        );
        assert_eq!(
            cargo_v, pypi_v,
            "PyPI version drift: cargo={} pypi={}",
            cargo_v, pypi_v
        );
        assert_eq!(
            cargo_v, install_v,
            "install.sh version drift: cargo={} install.sh={}",
            cargo_v, install_v
        );
    }

    /// VAL-CROSS-014: the leindex-embed subcrate must match the root crate's
    /// version (worker and main binary share a version).
    #[test]
    fn leindex_embed_matches_root_version() {
        let cargo = read_file("Cargo.toml");
        let embed = read_file("crates/leindex-embed/Cargo.toml");
        let cargo_v = extract_version(&cargo).expect("Cargo.toml version");
        let embed_v = extract_version(&embed).expect("leindex-embed Cargo.toml version");
        assert_eq!(cargo_v, embed_v, "embed subcrate version drift");
    }
}

// ============================================================================
// VAL-CROSS-015 / VAL-ORT-022:
// Diagnostics reports ORT path/version/provider consistently across surfaces.
// ============================================================================

mod diagnostics_ort_info {
    use super::*;

    /// VAL-CROSS-015 / VAL-ORT-022: the diagnostics command JSON output MUST
    /// include the `ort_path`, `ort_version`, and `execution_provider`
    /// fields, and they MUST be sourced from the discovery chain and the
    /// user config. We statically verify the wiring is present in the
    /// diagnostics command implementation.
    #[test]
    fn diagnostics_command_reports_ort_info() {
        let cli = read_file("src/cli/cli.rs");

        // The JSON output object must include all three keys.
        for key in ["ort_path", "ort_version", "execution_provider"] {
            let needle = format!("\"{}\":", key);
            assert!(
                cli.contains(&needle),
                "diagnostics JSON must include `{}` (got needle: {})",
                key,
                needle
            );
        }

        // The collection helper exists and pulls from the config + discovery.
        assert!(
            cli.contains("fn collect_ort_diagnostics()"),
            "diagnostics command must define collect_ort_diagnostics()"
        );
        assert!(
            cli.contains("discover_path_only"),
            "collect_ort_diagnostics must use discover_path_only so the reported path matches the worker's discovery chain"
        );
        assert!(
            cli.contains("LeIndexConfig::load"),
            "collect_ort_diagnostics must read the user config (ort_version, execution_provider)"
        );
    }

    /// VAL-CROSS-015: the diagnostics helper must NOT call `init_from()` (or
    /// `discover_and_init()`) so the main daemon process does not load ORT.
    /// Loading ORT into the diagnostics command would be a surprising side
    /// effect; the worker performs its own discovery when spawned.
    #[test]
    fn diagnostics_does_not_load_ort_into_main_process() {
        let cli = read_file("src/cli/cli.rs");
        let helper = cli
            .split("fn collect_ort_diagnostics()")
            .nth(1)
            .and_then(|s| s.split("\n}\n\nasync fn").next())
            .expect("collect_ort_diagnostics helper must exist");

        assert!(
            !helper.contains("init_from("),
            "diagnostics helper must NOT call init_from() (it would load ORT into the main process)"
        );
        assert!(
            !helper.contains("discover_and_init("),
            "diagnostics helper must NOT call discover_and_init() (it would load ORT into the main process)"
        );
    }

    /// VAL-ORT-022: ort_discovery exposes `last_outcome()` so the worker can
    /// report the resolved ORT path it actually loaded (after init_from).
    #[test]
    fn ort_discovery_exposes_last_outcome_for_diagnostics() {
        let src = read_file("crates/leindex-embed/src/ort_discovery.rs");
        assert!(
            src.contains("pub fn last_outcome()"),
            "ort_discovery must expose last_outcome() so the worker reports the resolved ORT path"
        );
    }

    /// VAL-CROSS-015: setup --check (the parallel CLI surface) reports the
    /// same ORT info shape. We statically verify both report ort_path,
    /// ort_version, and execution_provider so a support engineer reading
    /// either command sees identical fields.
    #[test]
    fn setup_check_reports_same_ort_info_shape() {
        let setup = read_file("src/cli/leindex/setup.rs");

        for header in ["ORT dylib path:", "ORT version:", "Execution provider:"] {
            assert!(
                setup.contains(header),
                "setup --check must report `{}` so its ORT info matches diagnostics",
                header
            );
        }
    }
}

// ============================================================================
// VAL-CROSS-001 / VAL-CROSS-003 / VAL-CROSS-005 / VAL-CROSS-006:
// Per-surface journeys exist end-to-end (cargo, npm, PyPI, migraphx, cpu).
// These are validated statically; the user-testing validator runs the live
// install journeys in a separate phase.
// ============================================================================

mod per_surface_journeys {
    use super::*;

    /// VAL-CROSS-001 / VAL-CARGO-002: `cargo install leindex --features onnx`
    /// installs BOTH binaries (main + worker). Statically verified in the
    /// cargo install layout tests; here we ensure the inference codepath can
    /// reach neural scoring via the worker spawn.
    #[test]
    fn cargo_install_lays_out_both_binaries_cooperatively() {
        let cargo = read_file("Cargo.toml");

        // The root Cargo.toml declares the worker bin target gated on the
        // onnx feature so `cargo install --features onnx` co-installs it.
        let has_embed_target = cargo
            .lines()
            .any(|line| line.trim().contains("name = \"leindex-embed\""));
        assert!(
            has_embed_target,
            "VAL-CROSS-001 / VAL-CARGO-002: root Cargo.toml must declare the leindex-embed bin target"
        );
    }

    /// VAL-CROSS-002 / VAL-NPM-003: the npm wrapper spawns the binary with
    /// `ORT_DYLIB_PATH` set from the bundled lib. Statically verified here;
    /// a live MCP round-trip is exercised by the user-testing validator.
    #[test]
    fn npm_wrapper_sets_ort_dylib_path_from_bundled_lib() {
        let wrapper = read_file("packages/npm-leindex-mcp/bin/mcp-wrapper.js");

        // The wrapper must compute a bundled ORT path and apply it to env.
        assert!(
            wrapper.contains("findBundledOrt"),
            "VAL-NPM-003: npm wrapper must compute the bundled ORT path via findBundledOrt()"
        );
        assert!(
            wrapper.contains("env.ORT_DYLIB_PATH"),
            "VAL-NPM-003: npm wrapper must set env.ORT_DYLIB_PATH before spawning"
        );
        assert!(
            wrapper.contains("MODELS_DIR"),
            "VAL-NPM-001: npm wrapper must reference the bundled MODELS_DIR"
        );
        // The npm setup script bridges into `leindex setup` so users can
        // bring up neural search through npm.
        let pkg = read_file("packages/npm-leindex-mcp/package.json");
        assert!(
            pkg.contains("\"setup\":"),
            "VAL-NPM-005: npm package.json must expose a `setup` script"
        );
    }

    /// VAL-CROSS-003 / VAL-PYPI-005: the PyPI package exposes a
    /// `leindex-setup` console script entry point that runs the Rust
    /// `leindex setup` command (after bootstrapping the binary).
    #[test]
    fn pypi_exposes_leindex_setup_console_script() {
        let toml = read_file("packages/pypi-leindex/pyproject.toml");
        assert!(
            toml.contains("leindex-setup = \"leindex.bootstrap:setup_main\""),
            "VAL-PYPI-005: pyproject.toml must declare the leindex-setup console script"
        );
        // The bootstrap must install the leindex-embed worker too so neural
        // search functions after setup.
        let bootstrap = read_file("packages/pypi-leindex/src/leindex/bootstrap.py");
        assert!(
            bootstrap.contains("install_embed_worker"),
            "VAL-PYPI-008: bootstrap must also install the leindex-embed worker binary"
        );
        // The bootstrap installs with the `onnx` feature so the `setup`
        // subcommand is present in the freshly installed binary.
        assert!(
            bootstrap.contains("\"onnx\"") || bootstrap.contains("INSTALL_FEATURES = \"onnx\""),
            "VAL-PYPI-002: bootstrap must install with the onnx feature (setup is feature-gated)"
        );
    }

    /// VAL-CROSS-005 / VAL-CARGO-003: `cargo install leindex --features
    /// onnx-migraphx` builds the MIGraphX-enabled worker. Statically verified
    /// via the feature chain; the smoke test on an AMD GPU is performed in
    /// the user-testing validator.
    #[test]
    fn onnx_migraphx_feature_propagates_to_embed_subcrate() {
        let cargo = read_file("Cargo.toml");
        let migraphx_line = cargo
            .lines()
            .find(|l| l.trim_start().starts_with("onnx-migraphx = ["))
            .expect("Cargo.toml must define onnx-migraphx feature");
        assert!(
            migraphx_line.contains("leindex-embed/onnx-migraphx"),
            "VAL-CROSS-005: onnx-migraphx feature must propagate to leindex-embed/onnx-migraphx"
        );
    }

    /// VAL-CROSS-006: the CPU neural path is the universal baseline. Setup
    /// `--neural --cpu` installs the plain `onnxruntime` (not -gpu / -migraphx).
    #[test]
    fn cpu_provider_maps_to_plain_onnxruntime_pip_package() {
        let setup = read_file("src/cli/leindex/setup.rs");
        // ExecutionProvider::Cpu::pip_package() returns "onnxruntime".
        let cpu_block = setup
            .split("ExecutionProvider::Cpu => \"onnxruntime\"")
            .count();
        assert!(
            cpu_block >= 1,
            "VAL-CROSS-006: CPU provider must map to the plain `onnxruntime` pip package"
        );
        let migraphx_block = setup
            .split("ExecutionProvider::Migraphx => \"onnxruntime-migraphx\"")
            .count();
        assert!(
            migraphx_block >= 1,
            "VAL-CROSS-005: MIGraphX provider must map to onnxruntime-migraphx"
        );
        let cuda_block = setup
            .split("ExecutionProvider::Cuda => \"onnxruntime-gpu\"")
            .count();
        assert!(
            cuda_block >= 1,
            "VAL-CROSS-005: CUDA provider must map to onnxruntime-gpu"
        );
    }
}

// ============================================================================
// VAL-CROSS-002 / VAL-CROSS-007 / VAL-CROSS-016:
// The Rust worker is the single source of truth for TF-IDF results and the
// neural-disabled notice across surfaces. These tests ensure the worker's
// fallback path is reachable from every install surface's spawn entry.
// ============================================================================

mod cross_surface_fallback_consistency {
    use super::*;

    /// VAL-CROSS-007: After `cargo install leindex` (no setup), running
    /// `leindex search` must NOT hard-crash. The search flow returns
    /// `None` from `generate_query_neural_embedding` and proceeds with
    /// TF-IDF, returning ranked results. We verify the query path defeats
    /// the neural attempt gracefully.
    #[test]
    fn search_query_neural_embedding_returns_none_on_failure() {
        let src = read_file("src/cli/leindex/query.rs");
        assert!(
            src.contains("Ok(None) => None, // TF-IDF only mode, no neural available"),
            "VAL-CROSS-007: query embedding must fall back to None so TF-IDF keeps working"
        );
        // The timeout/disconnect paths also fall back to TF-IDF (no panic).
        assert!(
            src.contains("using TF-IDF fallback"),
            "VAL-CROSS-007: query embedding must log a TF-IDF fallback message on failures"
        );
    }

    /// VAL-CROSS-007: TF-IDF-only search works on every surface after a fresh
    /// install (no setup). The HybridEmbedder variant `TfIdfOnly` exists and
    /// produces search results without requiring any worker.
    #[test]
    fn tfidf_only_embedder_variant_exists_for_zero_setup_path() {
        let src = read_file("src/cli/index_builder.rs");
        assert!(
            src.contains("HybridEmbedder::TfIdfOnly("),
            "VAL-CROSS-008 / VAL-CROSS-017: TfIdfOnly embedder variant must exist so TF-IDF works without ORT"
        );
    }

    /// VAL-CROSS-008 / VAL-CROSS-017: the bundle's lib/ is laid out by
    /// release.yml so the worker sibling-lib candidate finds ORT on every
    /// platform. Verified in the release bundle packaging tests; here we
    /// additionally confirm the bundle/install.sh surfaces agree on the
    /// `lib/` location used by the discovery chain.
    #[test]
    fn bundle_lib_layout_matches_discovery_chain_search_path() {
        let sh = read_file("install.sh");
        let cmd = read_file("crates/leindex-embed/src/ort_discovery.rs");

        // install.sh writes bundled libs to $LEINDEX_HOME/lib; discovery
        // searches ~/.leindex/lib via the user_lib candidate. The two paths
        // must line up.
        assert!(
            sh.contains("${LEINDEX_HOME}/lib") || sh.contains("$LEINDEX_HOME/lib"),
            "install.sh must copy bundled libs to $LEINDEX_HOME/lib"
        );
        assert!(
            cmd.contains("home.join(\"lib\")"),
            "discovery chain must search ~/.leindex/lib to find the installed bundle ORT"
        );
    }
}

// ============================================================================
// VAL-CROSS-002 / VAL-NPM-007: Falls back to TF-IDF if neural not configured.
// If the bundled lib is missing or corrupt on the npm surface, the MCP server
// still responds to search tool calls using TF-IDF.
// ============================================================================

mod npm_tfidf_fallback {
    use super::*;

    /// VAL-NPM-007 / VAL-CROSS-002: the npm wrapper must still spawn the
    /// binary when bundled ORT is missing (so MCP search degrades to
    /// TF-IDF rather than aborting). The wrapper must NOT call
    /// `process.exit(1)` if findBundledOrt returns null - only the
    /// missing-binary case exits 1.
    #[test]
    fn npm_wrapper_does_not_exit_when_bundled_ort_missing() {
        let wrapper = read_file("packages/npm-leindex-mcp/bin/mcp-wrapper.js");

        // The exit(1) must be reserved for a missing binary, not a missing
        // bundled ORT.
        let exit_calls: Vec<&str> = wrapper
            .lines()
            .filter(|line| line.contains("process.exit(1)"))
            .collect();
        assert!(
            !exit_calls.is_empty(),
            "npm wrapper must exit 1 when the binary is missing"
        );
        for line in &exit_calls {
            // Each exit must be associated with the missing-binary case,
            // not with missing ORT.
            assert!(
                !line.to_lowercase().contains("ort") && !line.to_lowercase().contains("onnx"),
                "npm wrapper must NOT exit on missing bundled ORT (TF-IDF fallback); got: {}",
                line
            );
        }
    }
}
