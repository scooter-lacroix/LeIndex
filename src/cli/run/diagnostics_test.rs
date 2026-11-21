//! Tests for scoped checks and the diagnostics store.

use super::*;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn test_parse_cargo_diagnostics_keeps_primary_span_and_dedupes() {
    let root = Path::new("/p");
    let output = concat!(
        r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types","spans":[{"file_name":"src/a.rs","line_start":3,"is_primary":false},{"file_name":"src/b.rs","line_start":9,"is_primary":true}]}}"#,
        "\n",
        r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types","spans":[{"file_name":"src/b.rs","line_start":9,"is_primary":true}]}}"#,
        "\n",
        r#"{"reason":"compiler-message","message":{"level":"warning","code":{"code":"unused_variables"},"message":"unused","spans":[{"file_name":"src/b.rs","line_start":12,"is_primary":true}]}}"#,
        "\n",
        r#"{"reason":"compiler-message","message":{"level":"error","code":null,"message":"aborting due to 1 previous error","spans":[]}}"#,
        "\n",
    );
    let found = parse_cargo_diagnostics(root, output);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].file, PathBuf::from("/p/src/b.rs"));
    assert_eq!(found[0].line, 9);
    assert_eq!(found[0].severity, Severity::Error);
    assert_eq!(found[0].code.as_deref(), Some("E0308"));
    assert_eq!(found[1].severity, Severity::Warning);
}

#[test]
fn test_parse_tsc_diagnostics_resolves_paths_against_cwd() {
    let output = "src/app.ts(10,7): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                  src/app.ts(10,7): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                  src/util.ts(5,1): warning TS6192: All imports are unused.\n";
    let found = parse_tsc_diagnostics(Path::new("/web"), output);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].file, PathBuf::from("/web/src/app.ts"));
    assert_eq!(found[0].line, 10);
    assert_eq!(found[0].code.as_deref(), Some("TS2322"));
    assert_eq!(found[1].severity, Severity::Warning);
}

#[test]
fn test_find_scope_prefers_nearest_package_and_skips_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/inner\"]\n",
    );
    write(
        &root.join("crates/inner/Cargo.toml"),
        "[package]\nname = \"inner\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&root.join("crates/inner/src/lib.rs"), "");
    write(&root.join("web/package.json"), "{}");
    write(&root.join("web/src/app.ts"), "");

    let cargo = find_scope(&root, &root.join("crates/inner/src/lib.rs")).unwrap();
    assert_eq!(cargo, Scope::Cargo("inner".to_string()));
    let tsc = find_scope(&root, &root.join("web/src/app.ts")).unwrap();
    assert_eq!(tsc, Scope::Tsc(root.join("web")));
}

#[test]
fn test_check_scoped_unsupported_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir.path().join("notes/readme.txt"), "x");
    let error = check_scoped(dir.path(), Path::new("notes/readme.txt"), 5).unwrap_err();
    assert!(error.to_string().contains("unsupported"), "{error}");
    assert!(error.to_string().contains("readme.txt"), "{error}");
}

#[test]
fn test_check_scoped_rejects_files_outside_the_project() {
    let outer = tempfile::tempdir().unwrap();
    let project = outer.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    write(&outer.path().join("stray.rs"), "");
    let error = check_scoped(&project, &outer.path().join("stray.rs"), 5).unwrap_err();
    assert!(error.to_string().contains("outside the project"), "{error}");
}

#[test]
fn test_record_then_diagnostics_for_filters_file_and_lines() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let storage = storage_for(root);
    let at = |line, code: &str| Diagnostic {
        file: root.join("src/lib.rs"),
        line,
        severity: Severity::Warning,
        code: Some(code.to_string()),
        message: "m".to_string(),
    };
    let other = Diagnostic {
        file: root.join("src/other.rs"),
        ..at(5, "x")
    };
    record(&storage, &[at(5, "a"), at(20, "b"), other]).unwrap();

    let hits = diagnostics_for(root, Path::new("src/lib.rs"), 1..=10);
    assert_eq!(hits.len(), 1);
    assert_eq!(hits[0].code.as_deref(), Some("a"));
    assert!(diagnostics_for(root, Path::new("src/lib.rs"), 100..=200).is_empty());
    assert!(diagnostics_for(root, Path::new("src/none.rs"), 1..=999).is_empty());
}

#[test]
fn test_diagnostics_for_is_empty_without_a_store() {
    let dir = tempfile::tempdir().unwrap();
    assert!(diagnostics_for(dir.path(), Path::new("a.rs"), 1..=10).is_empty());
}

/// Real `cargo check` on a one-file crate with a type error: the diagnostic
/// is parsed, stored, and read back through `diagnostics_for`.
#[test]
fn test_check_scoped_real_cargo_error_is_stored_and_readable() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"scoped_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write(
        &root.join("src/lib.rs"),
        "pub fn f() -> u32 { \"not a number\" }\n",
    );

    let summary = check_scoped(&root, Path::new("src/lib.rs"), 5).unwrap();
    assert_ne!(summary.exit_code, 0);
    assert_eq!(summary.error_count, 1, "{}", summary.render());

    let stored = diagnostics_for(&root, Path::new("src/lib.rs"), 1..=1);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].code.as_deref(), Some("E0308"));
    assert_eq!(stored[0].severity, Severity::Error);
}
