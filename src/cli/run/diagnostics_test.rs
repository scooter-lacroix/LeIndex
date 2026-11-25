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
    let found = parse_cargo_diagnostics(root, root, output);
    assert_eq!(found.len(), 2, "{found:?}");
    assert_eq!(found[0].file, PathBuf::from("/p/src/b.rs"));
    assert_eq!(found[0].line, 9);
    assert_eq!(found[0].severity, Severity::Error);
    assert_eq!(found[0].code.as_deref(), Some("E0308"));
    assert_eq!(found[1].severity, Severity::Warning);
}

/// A member of a workspace above the project prints paths relative to that
/// workspace, and the part above the project must be folded away before the
/// containment check — otherwise every span of a legitimate member would look
/// like it escaped the project and the store would come back empty.
#[test]
fn test_parse_cargo_diagnostics_folds_workspace_relative_spans_into_the_project() {
    let workspace = Path::new("/ws");
    let project = Path::new("/ws/member");
    let output = r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"mismatched types","spans":[{"file_name":"member/src/lib.rs","line_start":1,"is_primary":true}]}}"#;
    let found = parse_cargo_diagnostics(workspace, project, output);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].file, project.join("src/lib.rs"));
}

/// A span that resolves outside the project is dropped, and a message whose
/// primary span escapes but which also annotates in-project code keeps the
/// in-project location instead of disappearing.
#[test]
fn test_parse_cargo_diagnostics_skips_spans_outside_the_project() {
    let workspace = Path::new("/ws");
    let project = Path::new("/ws/member");
    let output = concat!(
        // Only an escaping span: nothing to store.
        r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"in a dependency","spans":[{"file_name":"/elsewhere/shared/src/lib.rs","line_start":1,"is_primary":true}]}}"#,
        "\n",
        // Escaping primary span, in-project note: keep the in-project one.
        r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0382"},"message":"borrow","spans":[{"file_name":"member/src/lib.rs","line_start":7,"is_primary":false},{"file_name":"/elsewhere/shared/src/lib.rs","line_start":4,"is_primary":true}]}}"#,
        "\n",
    );
    let found = parse_cargo_diagnostics(workspace, project, output);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].file, project.join("src/lib.rs"));
    assert_eq!(found[0].line, 7);
    assert!(!found[0].file.to_string_lossy().contains("elsewhere"));
}

#[test]
fn test_parse_tsc_diagnostics_resolves_paths_against_cwd() {
    let output = "src/app.ts(10,7): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                  src/app.ts(10,7): error TS2322: Type 'string' is not assignable to type 'number'.\n\
                  src/util.ts(5,1): warning TS6192: All imports are unused.\n";
    let found = parse_tsc_diagnostics(Path::new("/web"), Path::new("/web"), output);
    assert_eq!(found.len(), 2);
    assert_eq!(found[0].file, PathBuf::from("/web/src/app.ts"));
    assert_eq!(found[0].line, 10);
    assert_eq!(found[0].code.as_deref(), Some("TS2322"));
    assert_eq!(found[1].severity, Severity::Warning);
}

/// `tsc` reports paths relative to its cwd, but a project can pull in files
/// from above it; those are dropped for the same reason cargo spans are.
#[test]
fn test_parse_tsc_diagnostics_skips_paths_outside_the_project() {
    let output = "src/app.ts(1,1): error TS2304: Cannot find name 'x'.\n\
                  ../shared/types.ts(2,1): error TS2304: Cannot find name 'y'.\n";
    let found = parse_tsc_diagnostics(Path::new("/web"), Path::new("/web"), output);
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].file, PathBuf::from("/web/src/app.ts"));
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
    assert_eq!(
        cargo_workspace_root(&root.join("solo"), &root),
        root.join("solo")
    );

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
        cargo_workspace_root(&root.join("ws/crates/a"), &root),
        root.join("ws")
    );
    // Excluded by name: the parent does not claim it; its own table does.
    assert_eq!(
        cargo_workspace_root(&root.join("ws/crates/own"), &root),
        root.join("ws/crates/own")
    );
    // Two levels under `crates/*` is not matched by a one-level glob.
    assert_eq!(
        cargo_workspace_root(&root.join("ws/crates/deep/x"), &root),
        root.join("ws/crates/deep/x")
    );
}

/// The round-3 guarantee, restated against what Cargo actually does.
///
/// The stray-parent case cannot produce a mislocated span, because Cargo
/// REFUSES to run at all when a parent workspace does not list the package
/// (verified against the installed cargo: `cargo locate-project --workspace`
/// and `cargo metadata` both exit 101 with "current package believes it's in
/// a workspace when it's not"). `cargo_workspace_root` therefore falls back to
/// the manifest walk, and that fallback may never leave the project.
///
/// What must never happen either way is a STORED path outside the project:
/// [`diagnostics_for`] could never read it back, so the diagnostic would
/// silently vanish. Containment is the load-bearing check and is pinned here
/// at the parse level (an absolute span outside is dropped, an in-project one
/// is kept).
#[test]
fn test_a_workspace_above_the_project_never_yields_a_base_or_a_span_outside_it() {
    let outer = tempfile::tempdir().unwrap();
    let outer = std::fs::canonicalize(outer.path()).unwrap();
    // A stray virtual manifest above the project (no `members` key).
    write(&outer.join("Cargo.toml"), "[workspace]\nresolver = \"2\"\n");
    let root = outer.join("project");
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"solo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&root.join("src/lib.rs"), "pub fn f() {}\n");

    // Cargo refuses, so the fallback decides — and it stays inside.
    let base = cargo_workspace_root(&root, &root);
    assert!(
        base.starts_with(&root),
        "the base must stay inside the project, got {base:?}"
    );

    // Spans relative to that base: the in-project one is kept, an absolute
    // path outside the project is dropped rather than stored unreachable.
    let output = concat!(
        r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"in project","spans":[{"file_name":"src/lib.rs","line_start":3,"is_primary":true}]}}"#,
        "\n",
        r#"{"reason":"compiler-message","message":{"level":"error","code":{"code":"E0308"},"message":"outside","spans":[{"file_name":"/elsewhere/x/src/lib.rs","line_start":5,"is_primary":true}]}}"#,
        "\n",
    );
    let found = parse_cargo_diagnostics(&base, &root, output);
    assert_eq!(
        found.len(),
        1,
        "only the in-project span may be stored: {found:?}"
    );
    assert_eq!(found[0].file, root.join("src/lib.rs"));
    assert!(found[0].file.starts_with(&root));
}

/// The positive counterpart: when the parent workspace DOES list the project,
/// Cargo's own answer is the base (that is the directory its spans are
/// relative to), and it is an ancestor of the project root.
#[test]
fn test_cargo_workspace_root_uses_a_parent_workspace_that_lists_the_project() {
    let outer = tempfile::tempdir().unwrap();
    let outer = std::fs::canonicalize(outer.path()).unwrap();
    write(
        &outer.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
    );
    let root = outer.join("member");
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&root.join("src/lib.rs"), "pub fn f() {}\n");
    assert_eq!(
        cargo_workspace_root(&root, &root),
        outer,
        "a genuine member's spans are relative to the workspace root"
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
/// CodeRabbit CR-R4-4, end to end: the project is a genuine member of a
/// workspace that lives ABOVE the project root (`/ws/Cargo.toml` lists
/// `member`, the project is opened at `/ws/member`). Cargo then prints spans
/// workspace-relative (`member/src/lib.rs`); resolving them against the
/// project root produced `<root>/member/member/src/lib.rs`, which matched no
/// file, so the diagnostic silently vanished from `diagnostics_for`.
#[test]
fn test_check_scoped_resolves_spans_when_the_workspace_is_above_the_project() {
    let outer = tempfile::tempdir().unwrap();
    let outer = std::fs::canonicalize(outer.path()).unwrap();
    write(
        &outer.join("Cargo.toml"),
        "[workspace]\nmembers = [\"member\"]\nresolver = \"2\"\n",
    );
    let root = outer.join("member");
    write(
        &root.join("Cargo.toml"),
        "[package]\nname = \"member\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(&root.join("src/lib.rs"), "mod foo;\n");
    write(
        &root.join("src/foo.rs"),
        "pub fn f() -> u32 { \"not a number\" }\n",
    );

    let summary = check_scoped(&root, Path::new("src/foo.rs"), 5).unwrap();
    assert_ne!(summary.exit_code, 0, "{}", summary.render());
    assert_eq!(summary.error_count, 1, "{}", summary.render());
    let stored = diagnostics_for(&root, Path::new("src/foo.rs"), 1..=1);
    assert_eq!(
        stored.len(),
        1,
        "the span must be readable back: {stored:?}"
    );
    assert_eq!(stored[0].code.as_deref(), Some("E0308"));
    assert_eq!(stored[0].file, root.join("src/foo.rs"));
    assert!(
        !stored[0].file.to_string_lossy().contains("member/member"),
        "the workspace-relative prefix must be folded away: {stored:?}"
    );
}
