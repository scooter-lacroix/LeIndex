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
    assert_eq!(
        cargo,
        Scope::Cargo {
            package: "inner".to_string(),
            dir: root.join("crates/inner"),
            workspace_root: root.clone(),
        }
    );
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

#[test]
fn test_concurrent_record_writers_all_publish_a_valid_store() {
    let dir = tempfile::tempdir().unwrap();
    let storage = storage_for(dir.path());
    let root = dir.path().to_path_buf();
    let mut writers = Vec::new();
    for writer in 0..16 {
        let storage = storage.clone();
        let root = root.clone();
        writers.push(std::thread::spawn(move || {
            let diagnostic = Diagnostic {
                file: root.join(format!("src/w{writer}.rs")),
                line: writer + 1,
                severity: Severity::Warning,
                code: Some("w".to_string()),
                message: format!("writer {writer}"),
            };
            record(&storage, &[diagnostic])
        }));
    }
    for handle in writers {
        handle.join().unwrap().unwrap();
    }
    // The published store must be valid JSON with diagnostics from at least
    // one writer (the last publisher wins; earlier renames must not fail).
    let raw =
        std::fs::read(crate::cli::run::run_log::logs_dir(&storage).join("last-diagnostics.json"))
            .unwrap();
    let value: serde_json::Value = serde_json::from_slice(&raw).unwrap();
    assert!(
        value["diagnostics"]
            .as_array()
            .is_some_and(|d| !d.is_empty()),
        "{value}"
    );
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

/// A test-target file (`tests/*.rs`) is not compiled by plain `cargo check`;
/// the scoped check must include it, or a broken test would read as a pass.
#[test]
fn test_check_scoped_compiles_test_targets() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"target_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write(&root.join("src/lib.rs"), "pub fn ok() {}\n");
    write(
        &root.join("tests/broken_test.rs"),
        "#[test]\nfn broken() { let n: u32 = \"not a number\"; drop(n); }\n",
    );

    let summary = check_scoped(&root, Path::new("tests/broken_test.rs"), 5).unwrap();
    assert_ne!(summary.exit_code, 0, "{}", summary.render());
    assert_eq!(summary.error_count, 1, "{}", summary.render());
    let stored = diagnostics_for(&root, Path::new("tests/broken_test.rs"), 1..=5);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].severity, Severity::Error);
    assert_eq!(stored[0].line, 2);
}

/// Cargo prints diagnostic `file_name` relative to the WORKSPACE root, which
/// for a real member is an ancestor of the member directory the check ran
/// in. Resolving against the member cwd stored `<root>/member/member/...`,
/// which matched no file, so the diagnostic vanished from `diagnostics_for`
/// and the read overlay.
#[test]
fn test_check_scoped_resolves_cargo_paths_against_the_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\n",
    );
    write(
        &root.join("member/Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&root.join("member/src/lib.rs"), "mod foo;\n");
    write(
        &root.join("member/src/foo.rs"),
        "pub fn f() -> u32 { \"not a number\" }\n",
    );

    let summary = check_scoped(&root, Path::new("member/src/foo.rs"), 5).unwrap();
    assert_ne!(summary.exit_code, 0, "{}", summary.render());
    assert_eq!(summary.error_count, 1, "{}", summary.render());
    // Stored under the real file, so a later read of that file finds it.
    let stored = diagnostics_for(&root, Path::new("member/src/foo.rs"), 1..=1);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].code.as_deref(), Some("E0308"));
    assert_eq!(stored[0].file, root.join("member/src/foo.rs"));
    // And never under the doubled path the cwd-based base produced.
    assert!(
        !stored[0].file.to_string_lossy().contains("member/member"),
        "{stored:?}"
    );
}

/// The three workspace shapes Cargo resolves, which decide the base:
/// a package that is its own workspace root, a package listed by an
/// ancestor's `members`, and a package an ancestor's `exclude` pushes out.
#[test]
fn test_cargo_workspace_root_follows_manifest_claims() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();

    // Stand-alone package: no [workspace] anywhere => its own root.
    write(
        &root.join("solo/Cargo.toml"),
        "[package]\nname = \"solo\"\n",
    );
    assert_eq!(cargo_workspace_root(&root.join("solo")), root.join("solo"));

    // A member with its own `[workspace]` table is its own root even under
    // a parent workspace (the shape the earlier fixture had).
    write(
        &root.join("ws/Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\nexclude = [\"crates/own\"]\n",
    );
    write(
        &root.join("ws/crates/a/Cargo.toml"),
        "[package]\nname = \"a\"\n",
    );
    write(
        &root.join("ws/crates/own/Cargo.toml"),
        "[package]\nname = \"own\"\n[workspace]\n",
    );
    write(
        &root.join("ws/crates/deep/x/Cargo.toml"),
        "[package]\nname = \"deep\"\n",
    );
    // Listed by `crates/*`: the parent is the root.
    assert_eq!(
        cargo_workspace_root(&root.join("ws/crates/a")),
        root.join("ws")
    );
    // Excluded by name: the parent does not claim it; its own table does.
    assert_eq!(
        cargo_workspace_root(&root.join("ws/crates/own")),
        root.join("ws/crates/own")
    );
    // Two levels under `crates/*` is not matched by a one-level glob.
    assert_eq!(
        cargo_workspace_root(&root.join("ws/crates/deep/x")),
        root.join("ws/crates/deep/x")
    );
}

/// A workspace member nested under the project root is only reachable when
/// Cargo runs from inside that member: Cargo searches upward from the
/// working directory, never downward into children.
#[test]
fn test_check_scoped_runs_member_check_from_the_owning_crate() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // The project root holds a virtual manifest that does NOT list the
    // member, mirroring a monorepo where the member is discovered by its own
    // manifest rather than by the root's `members` list.
    write(&root.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\n");
    write(
        &root.join("backend/Cargo.toml"),
        "[package]\nname = \"backend\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write(
        &root.join("backend/src/lib.rs"),
        "pub fn f() -> u32 { \"not a number\" }\n",
    );

    let summary = check_scoped(&root, Path::new("backend/src/lib.rs"), 5).unwrap();
    assert_ne!(summary.exit_code, 0, "{}", summary.render());
    assert_eq!(summary.error_count, 1, "{}", summary.render());
    let stored = diagnostics_for(&root, Path::new("backend/src/lib.rs"), 1..=1);
    assert_eq!(stored.len(), 1, "{stored:?}");
    assert_eq!(stored[0].code.as_deref(), Some("E0308"));
}

/// Each scoped check must read back only the log its own run produced. With
/// a fixed log name, two concurrent checks would share one file and the
/// slower one could parse the other's output as its own diagnostics.
#[test]
fn test_check_scoped_uses_a_unique_log_per_invocation() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    // Two members so each check compiles only its own package's sources.
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"a\", \"b\"]\n",
    );
    for member in ["a", "b"] {
        write(
            &root.join(member).join("Cargo.toml"),
            &format!("[package]\nname = \"{member}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        );
        write(
            &root.join(member).join("src/lib.rs"),
            &format!("pub fn f() -> u32 {{ \"{member}\" }}\n"),
        );
    }

    let first = check_scoped(&root, Path::new("a/src/lib.rs"), 5).unwrap();
    let second = check_scoped(&root, Path::new("b/src/lib.rs"), 5).unwrap();
    let first_log = first.log_path.expect("first log");
    let second_log = second.log_path.expect("second log");
    assert_ne!(first_log, second_log, "scoped checks shared one log file");
    // Neither check may write to the old fixed name.
    for log in [&first_log, &second_log] {
        assert_ne!(
            log.file_name().and_then(|n| n.to_str()),
            Some("check-scoped.log")
        );
    }
    // Each summary's log holds that check's own primary span and not the
    // other package's.
    let first_text = std::fs::read_to_string(&first_log).unwrap();
    assert!(first_text.contains("a/src/lib.rs"), "{first_text}");
    assert!(!first_text.contains("b/src/lib.rs"), "{first_text}");
    let second_text = std::fs::read_to_string(&second_log).unwrap();
    assert!(second_text.contains("b/src/lib.rs"), "{second_text}");
    assert!(!second_text.contains("a/src/lib.rs"), "{second_text}");
}
