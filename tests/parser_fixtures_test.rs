//! Parser fixture tests (VAL-FIXTURES-001 through VAL-FIXTURES-003).
//!
//! These tests parse realistic Rust fixture files and verify that the
//! tree-sitter-based `RustParser` extracts the expected symbols with
//! correct names, qualified names, kinds (via `return_type`), byte ranges,
//! and method flags.

#![cfg(test)]

use std::path::PathBuf;

use leindex::parse::languages::RustParser;
use leindex::parse::traits::CodeIntelligence;

/// Return the absolute path to the repo root, computed from CARGO_MANIFEST_DIR.
fn repo_root() -> PathBuf {
    let dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(dir)
}

/// Read a fixture file as bytes, panicking with the path on failure.
fn read_fixture(rel: &str) -> Vec<u8> {
    let path = repo_root().join(rel);
    std::fs::read(&path).unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

// ============================================================================
// VAL-FIXTURES-001: askpass.rs parses struct, methods with qualified names,
// standalone functions.
// ============================================================================

#[test]
fn test_askpass_parses_struct() {
    let source = read_fixture("tests/fixtures/rust/askpass.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    let askpass = signatures
        .iter()
        .find(|s| s.name == "Askpass" && s.return_type.as_deref() == Some("struct"))
        .unwrap_or_else(|| panic!("expected Askpass struct, got: {signatures:?}"));

    // Byte range must be valid: start < end within source bounds.
    assert!(
        askpass.byte_range.0 < askpass.byte_range.1 && askpass.byte_range.1 <= source.len(),
        "invalid byte_range for Askpass: {:?}",
        askpass.byte_range
    );
}

#[test]
fn test_askpass_parses_methods_with_qualified_names() {
    let source = read_fixture("tests/fixtures/rust/askpass.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    // new: qualified_name=Askpass::new, is_method=true
    let new_method = signatures
        .iter()
        .find(|s| s.qualified_name == "Askpass::new")
        .unwrap_or_else(|| panic!("expected Askpass::new, got: {signatures:?}"));
    assert!(
        new_method.is_method,
        "Askpass::new should be flagged as a method (impl function)"
    );
    assert!(
        new_method.byte_range.0 < new_method.byte_range.1,
        "invalid byte_range for Askpass::new: {:?}",
        new_method.byte_range
    );

    // path: qualified_name=Askpass::path, is_method=true
    let path_method = signatures
        .iter()
        .find(|s| s.qualified_name == "Askpass::path")
        .unwrap_or_else(|| panic!("expected Askpass::path, got: {signatures:?}"));
    assert!(
        path_method.is_method,
        "Askpass::path should be flagged as a method (impl function)"
    );
    assert!(
        path_method.byte_range.0 < path_method.byte_range.1,
        "invalid byte_range for Askpass::path: {:?}",
        path_method.byte_range
    );
}

#[test]
fn test_askpass_parses_standalone_main() {
    let source = read_fixture("tests/fixtures/rust/askpass.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    // main should exist as a standalone function (not a method).
    let main_fn = signatures
        .iter()
        .find(|s| s.name == "main" && s.return_type.as_deref() != Some("struct"))
        .unwrap_or_else(|| panic!("expected standalone main function, got: {signatures:?}"));
    assert!(
        !main_fn.is_method,
        "main should NOT be flagged as a method (standalone function)"
    );
    assert!(
        main_fn.byte_range.0 < main_fn.byte_range.1,
        "invalid byte_range for main: {:?}",
        main_fn.byte_range
    );
}

// ============================================================================
// VAL-FIXTURES-002: nested_cli.rs parses nested modules, enum variants,
// associated functions without self.
// ============================================================================

#[test]
fn test_nested_cli_parses_cli_struct() {
    let source = read_fixture("tests/fixtures/rust/nested_cli.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    let cli = signatures
        .iter()
        .find(|s| s.name == "Cli" && s.return_type.as_deref() == Some("struct"))
        .unwrap_or_else(|| panic!("expected Cli struct, got: {signatures:?}"));

    assert!(
        cli.byte_range.0 < cli.byte_range.1,
        "invalid byte_range for Cli: {:?}",
        cli.byte_range
    );
}

#[test]
fn test_nested_cli_parses_commands_enum_and_variants() {
    let source = read_fixture("tests/fixtures/rust/nested_cli.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    // Commands enum
    let commands = signatures
        .iter()
        .find(|s| s.name == "Commands" && s.return_type.as_deref() == Some("enum"))
        .unwrap_or_else(|| panic!("expected Commands enum, got: {signatures:?}"));

    assert!(
        commands.byte_range.0 < commands.byte_range.1,
        "invalid byte_range for Commands: {:?}",
        commands.byte_range
    );

    // Install variant: qualified_name=Commands::Install, kind=enum_variant
    let install = signatures
        .iter()
        .find(|s| {
            s.qualified_name == "Commands::Install"
                && s.return_type.as_deref() == Some("enum_variant")
        })
        .unwrap_or_else(|| panic!("expected Commands::Install enum_variant, got: {signatures:?}"));
    assert!(
        install.byte_range.0 < install.byte_range.1,
        "invalid byte_range for Commands::Install: {:?}",
        install.byte_range
    );

    // Uninstall and Status variants should also be present.
    assert!(
        signatures
            .iter()
            .any(|s| s.qualified_name == "Commands::Uninstall"),
        "expected Commands::Uninstall variant"
    );
    assert!(
        signatures
            .iter()
            .any(|s| s.qualified_name == "Commands::Status"),
        "expected Commands::Status variant"
    );
}

#[test]
fn test_nested_cli_parses_config_module_and_config_struct() {
    let source = read_fixture("tests/fixtures/rust/nested_cli.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    // config module
    let config_mod = signatures
        .iter()
        .find(|s| s.name == "config" && s.return_type.as_deref() == Some("module"))
        .unwrap_or_else(|| panic!("expected config module, got: {signatures:?}"));
    assert!(
        config_mod.byte_range.0 < config_mod.byte_range.1,
        "invalid byte_range for config module: {:?}",
        config_mod.byte_range
    );

    // Config struct should have qualified_name config::Config
    let config = signatures
        .iter()
        .find(|s| s.qualified_name == "config::Config")
        .unwrap_or_else(|| {
            panic!("expected config::Config struct with qualified_name, got: {signatures:?}")
        });
    assert_eq!(
        config.return_type.as_deref(),
        Some("struct"),
        "config::Config should have return_type=struct"
    );
    assert!(
        config.byte_range.0 < config.byte_range.1,
        "invalid byte_range for config::Config: {:?}",
        config.byte_range
    );
}

#[test]
fn test_nested_cli_associated_functions_without_self_are_methods() {
    let source = read_fixture("tests/fixtures/rust/nested_cli.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    // Cli::from_env is an associated function without self — should still be
    // flagged as a method because it lives inside an impl block.
    let from_env = signatures
        .iter()
        .find(|s| s.qualified_name == "Cli::from_env")
        .unwrap_or_else(|| panic!("expected Cli::from_env, got: {signatures:?}"));
    assert!(
        from_env.is_method,
        "Cli::from_env (associated function without self) should be flagged is_method=true"
    );

    // Config::new is an associated function without self inside mod config.
    let config_new = signatures
        .iter()
        .find(|s| s.qualified_name == "config::Config::new")
        .unwrap_or_else(|| panic!("expected config::Config::new, got: {signatures:?}"));
    assert!(
        config_new.is_method,
        "config::Config::new (associated function without self) should be flagged is_method=true"
    );
}

// ============================================================================
// VAL-FIXTURES-003: flows.rs parses cross-function data dependencies and
// struct definitions.
// ============================================================================

#[test]
fn test_flows_parses_registry_record_struct() {
    let source = read_fixture("tests/fixtures/rust/flows.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    let record = signatures
        .iter()
        .find(|s| s.name == "RegistryRecord" && s.return_type.as_deref() == Some("struct"))
        .unwrap_or_else(|| panic!("expected RegistryRecord struct, got: {signatures:?}"));
    assert!(
        record.byte_range.0 < record.byte_range.1,
        "invalid byte_range for RegistryRecord: {:?}",
        record.byte_range
    );
}

#[test]
fn test_flows_parses_standalone_functions() {
    let source = read_fixture("tests/fixtures/rust/flows.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    // run_installation
    let run_installation = signatures
        .iter()
        .find(|s| s.name == "run_installation" && !s.is_method)
        .unwrap_or_else(|| panic!("expected run_installation function, got: {signatures:?}"));
    assert!(
        run_installation.byte_range.0 < run_installation.byte_range.1,
        "invalid byte_range for run_installation: {:?}",
        run_installation.byte_range
    );

    // verify_registry
    let verify_registry = signatures
        .iter()
        .find(|s| s.name == "verify_registry" && !s.is_method)
        .unwrap_or_else(|| panic!("expected verify_registry function, got: {signatures:?}"));
    assert!(
        verify_registry.byte_range.0 < verify_registry.byte_range.1,
        "invalid byte_range for verify_registry: {:?}",
        verify_registry.byte_range
    );

    // detect_arch
    let detect_arch = signatures
        .iter()
        .find(|s| s.name == "detect_arch" && !s.is_method)
        .unwrap_or_else(|| panic!("expected detect_arch function, got: {signatures:?}"));
    assert!(
        detect_arch.byte_range.0 < detect_arch.byte_range.1,
        "invalid byte_range for detect_arch: {:?}",
        detect_arch.byte_range
    );
}

#[test]
fn test_flows_run_installation_calls_verify_registry_and_detect_arch() {
    let source = read_fixture("tests/fixtures/rust/flows.rs");
    let parser = RustParser::new();
    let signatures = parser.get_signatures(&source).unwrap();

    let run_installation = signatures
        .iter()
        .find(|s| s.name == "run_installation")
        .unwrap_or_else(|| panic!("expected run_installation function, got: {signatures:?}"));

    // The calls list should include references to verify_registry and detect_arch.
    // The parser's call extraction may capture them as bare names or
    // receiver-qualified names; check for substring presence.
    assert!(
        run_installation
            .calls
            .iter()
            .any(|c| c.contains("verify_registry")),
        "run_installation should call verify_registry, calls: {:?}",
        run_installation.calls
    );
    assert!(
        run_installation
            .calls
            .iter()
            .any(|c| c.contains("detect_arch")),
        "run_installation should call detect_arch, calls: {:?}",
        run_installation.calls
    );
}
