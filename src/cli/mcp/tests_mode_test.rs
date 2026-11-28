//! Tests for the tests-mode locator (`symbol_lookup` `kind=tests`).
//!
//! Split out of `tests_mode.rs`, which passed the 2000-line limit. The
//! module is included via `#[path = "tests_mode_test.rs"] mod test;`;
//! everything here reaches the private functions through `super::*`.
use super::*;

fn node(name: &str, file: &str, language: &str, byte_range: (usize, usize)) -> Node {
    Node {
        id: format!("{file}::{name}"),
        node_type: crate::graph::pdg::NodeType::Function,
        name: name.to_string(),
        file_path: std::sync::Arc::from(file),
        byte_range,
        complexity: 1,
        language: language.to_string(),
    }
}

/// A real tempdir manifest, so `cargo_package`'s nearest-ancestor walk has
/// something true to find — the command must carry the real crate name.
fn manifest_dir(name: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("src")).expect("src dir");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\n"),
    )
    .expect("manifest");
    dir
}

fn parse_rust(source: &str) -> crate::cli::mcp::call_sites::FileSource {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(
            &crate::parse::grammar::LanguageId::from_extension("rs")
                .unwrap()
                .from_cache()
                .unwrap(),
        )
        .unwrap();
    crate::cli::mcp::call_sites::FileSource {
        text: source.to_string(),
        tree: parser.parse(source, None),
    }
}

#[test]
fn test_unit_test_command_is_exact_with_module_path() {
    let dir = manifest_dir("proj");
    let source = "mod inner {\n    #[cfg(test)]\n    mod tests {\n        #[test]\n        fn test_x() {\n            assert!(true);\n        }\n    }\n}\n";
    let src = parse_rust(source);
    let byte_range = (source.find("fn test_x").unwrap(), source.len());
    let test_fn = node(
        "test_x",
        &dir.path().join("src/inner.rs").display().to_string(),
        "rust",
        byte_range,
    );
    let cmd = command_for(&test_fn, Some(5), Some(&src));
    assert_eq!(
        cmd, "cargo test -p proj --lib inner::tests::test_x -- --exact",
        "command: {cmd}"
    );
}

fn parse_python(source: &str) -> crate::cli::mcp::call_sites::FileSource {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&crate::parse::traits::languages::python::language())
        .unwrap();
    crate::cli::mcp::call_sites::FileSource {
        text: source.to_string(),
        tree: parser.parse(source, None),
    }
}

/// A real crate on disk whose unit tests live in out-of-line modules.
fn crate_with_out_of_line_tests() -> tempfile::TempDir {
    let dir = manifest_dir("outline");
    let src = dir.path().join("src");
    std::fs::write(
        src.join("lib.rs"),
        "pub mod foo;\npub mod nested;\n#[path = \"renamed.rs\"]\npub mod aliased;\n",
    )
    .unwrap();
    // src/foo.rs declares a child of its own -> src/foo/child.rs
    std::fs::write(
            src.join("foo.rs"),
            "mod child;\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_foo() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
    std::fs::create_dir_all(src.join("foo")).unwrap();
    std::fs::write(
            src.join("foo").join("child.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_child() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
    // src/nested/mod.rs style
    std::fs::create_dir_all(src.join("nested")).unwrap();
    std::fs::write(
            src.join("nested").join("mod.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_modrs() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
    std::fs::write(
            src.join("renamed.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_aliased() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
    // A root-level test, and an inline module inside the root.
    std::fs::write(
            src.join("lib.rs"),
            "pub mod foo;\npub mod nested;\n#[path = \"renamed.rs\"]\npub mod aliased;\n\nmod inline_root {\n    mod tests {\n        #[test]\n        fn test_in_inline_root() {\n            assert!(true);\n        }\n    }\n}\n#[test]\nfn test_at_root() {\n    assert!(true);\n}\n",
        )
        .unwrap();
    dir
}

fn command_for_test_in(dir: &tempfile::TempDir, rel: &str, name: &str) -> String {
    command_for_test_at(dir, &format!("src/{rel}"), name)
}

fn command_for_test_at(dir: &tempfile::TempDir, rel: &str, name: &str) -> String {
    let file = dir.path().join(rel);
    let source = std::fs::read_to_string(&file).unwrap();
    let src = parse_rust(&source);
    let start = source.find(&format!("fn {name}")).unwrap();
    let test_fn = node(
        name,
        &file.display().to_string(),
        "rust",
        (start, source.len()),
    );
    command_for(&test_fn, Some(1), Some(&src))
}

/// A pytest method needs its class in the node id, read from the live
/// tree; a module-level test stays `file::test`. The command must be a
/// single node id with no `or` and no line suffix.
#[test]
fn test_pytest_node_id_includes_class_ancestry_and_is_executable() {
    let dir = manifest_dir("pyproj");
    let file = dir.path().join("tests").join("core_test.py");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let source = "class TestOuter:\n    class TestInner:\n        def test_nested(self):\n            assert True\n\n    def test_method(self):\n        assert True\n\ndef test_plain():\n    assert True\n";
    std::fs::write(&file, source).unwrap();
    let src = parse_python(source);
    let py = |name: &str| {
        node(
            name,
            &file.display().to_string(),
            "python",
            (source.find(name).unwrap(), source.len()),
        )
    };
    let shown = file.display().to_string();
    assert_eq!(
        command_for(&py("test_nested"), Some(3), Some(&src)),
        format!("pytest {shown}::TestOuter::TestInner::test_nested")
    );
    assert_eq!(
        command_for(&py("test_method"), Some(6), Some(&src)),
        format!("pytest {shown}::TestOuter::test_method")
    );
    assert_eq!(
        command_for(&py("test_plain"), Some(9), Some(&src)),
        format!("pytest {shown}::test_plain")
    );
    // Without a tree it degrades to the function alone — still one id.
    assert_eq!(
        command_for(&py("test_method"), Some(6), None),
        format!("pytest {shown}::test_method")
    );
    // Execute the generated command with real pytest (if installed) on a
    // deliberately failing method: it must collect and run that test.
    if std::process::Command::new("python3")
        .arg("-m")
        .arg("pytest")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let failing = source.replace(
            "    def test_method(self):\n        assert True",
            "    def test_method(self):\n        assert False",
        );
        std::fs::write(&file, failing).unwrap();
        let out = std::process::Command::new("python3")
            .args([
                "-m",
                "pytest",
                "-q",
                "tests/core_test.py::TestOuter::test_method",
            ])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("1 failed"),
            "pytest must run exactly that test: {text}"
        );
        assert!(!out.status.success());
    }
}

/// For a test in `src/foo.rs`, the enclosing `foo` module is declared in
/// `src/lib.rs`, not in the file. The filter used to come out as
/// `tests::test_x`; `--exact` then matched nothing and cargo exited 0
/// after running zero tests, so a failing test read as a pass.
#[test]
fn test_unit_test_filter_includes_out_of_line_module_ancestry() {
    let dir = crate_with_out_of_line_tests();
    assert_eq!(
        command_for_test_in(&dir, "foo.rs", "test_in_foo"),
        "cargo test -p outline --lib foo::tests::test_in_foo -- --exact"
    );
    // Child of a non-mod-rs file: `src/foo.rs` -> `src/foo/child.rs`.
    assert_eq!(
        command_for_test_in(&dir, "foo/child.rs", "test_in_child"),
        "cargo test -p outline --lib foo::child::tests::test_in_child -- --exact"
    );
    // `mod.rs` style.
    assert_eq!(
        command_for_test_in(&dir, "nested/mod.rs", "test_in_modrs"),
        "cargo test -p outline --lib nested::tests::test_in_modrs -- --exact"
    );
    // `#[path = "…"]` alias keeps the declared name, not the file name.
    assert_eq!(
        command_for_test_in(&dir, "renamed.rs", "test_in_aliased"),
        "cargo test -p outline --lib aliased::tests::test_in_aliased -- --exact"
    );
    // Root-level and inline-in-root tests are unchanged.
    assert_eq!(
        command_for_test_in(&dir, "lib.rs", "test_at_root"),
        "cargo test -p outline --lib test_at_root -- --exact"
    );
    assert_eq!(
        command_for_test_in(&dir, "lib.rs", "test_in_inline_root"),
        "cargo test -p outline --lib inline_root::tests::test_in_inline_root -- --exact"
    );
}

/// The filter must actually run the test: `--exact` with a wrong path
/// runs zero tests and still exits 0. Run the generated command against
/// real cargo on a FAILING test and require a non-zero exit.
#[test]
fn test_generated_exact_filter_runs_the_test_under_real_cargo() {
    let dir = crate_with_out_of_line_tests();
    // Make the out-of-line test fail.
    let foo = dir.path().join("src/foo.rs");
    let text = std::fs::read_to_string(&foo)
        .unwrap()
        .replace("assert!(true)", "assert!(false)");
    std::fs::write(&foo, text).unwrap();
    let command = command_for_test_in(&dir, "foo.rs", "test_in_foo");
    assert!(command.starts_with("cargo test -p outline"), "{command}");
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(&command)
        .current_dir(dir.path())
        .env("CARGO_TERM_COLOR", "never")
        .output()
        .expect("cargo available");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("test foo::tests::test_in_foo ... FAILED"),
        "the test must actually run: {stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!output.status.success(), "a failing test must fail the run");
}

/// A unit test in `src/main.rs` belongs to the BINARY target. `--lib`
/// runs zero tests there and exits 0, so a failing binary test read as a
/// pass; the command must select `--bin <package>` instead.
#[test]
fn test_binary_unit_tests_select_the_bin_target() {
    let dir = manifest_dir("binpkg");
    let src = dir.path().join("src");
    std::fs::write(
            src.join("main.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_bin() {\n        assert!(true);\n    }\n}\n\nfn main() {}\n",
        )
        .unwrap();
    assert_eq!(
        command_for_test_in(&dir, "main.rs", "test_in_bin"),
        "cargo test -p binpkg --bin binpkg tests::test_in_bin -- --exact"
    );
    // A `[[bin]]` with an explicit path is a binary too.
    std::fs::write(src.join("lib.rs"), "pub mod tool;\n").unwrap();
    std::fs::write(
            src.join("tool.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_tool() {\n        assert!(true);\n    }\n}\n",
        )
        .unwrap();
    std::fs::write(
        src.join("lib.rs"),
        "pub mod tool;\n#[test]\nfn test_in_lib() {\n    assert!(true);\n}\n",
    )
    .unwrap();
    assert_eq!(
        command_for_test_in(&dir, "lib.rs", "test_in_lib"),
        "cargo test -p binpkg --lib test_in_lib -- --exact"
    );
    assert_eq!(
        command_for_test_in(&dir, "tool.rs", "test_in_tool"),
        "cargo test -p binpkg --lib tool::tests::test_in_tool -- --exact"
    );

    // Real cargo, with BOTH a lib and a bin in the package (the case the
    // reviewer describes): make the binary test fail. The generated
    // command must run it and fail...
    std::fs::write(
            src.join("main.rs"),
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn test_in_bin() {\n        assert!(false);\n    }\n}\n\nfn main() {}\n",
        )
        .unwrap();
    let command = command_for_test_in(&dir, "main.rs", "test_in_bin");
    // Run the generated command directly (the generated form has no
    // quoting), so the test does not depend on `sh` finding cargo.
    let cargo = std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths)
                .map(|d| d.join("cargo"))
                .find(|c| c.is_file())
        })
        .unwrap_or_default();
    let Some(cargo) = cargo else {
        eprintln!("skipping real-cargo check: cargo not on PATH");
        return;
    };
    let run = |cmd: &str| {
        // `cargo` is the first word; resolve it to the absolute path found
        // above so the test does not depend on the harness's PATH lookup.
        let words = cmd.split_whitespace().skip(1);
        let program = cargo.clone();
        let out = std::process::Command::new(program)
            .args(words)
            .current_dir(dir.path())
            .env("CARGO_TERM_COLOR", "never")
            .output()
            .unwrap();
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (out.status.success(), text)
    };
    let (ok, text) = run(&command);
    assert!(
        text.contains("test tests::test_in_bin ... FAILED"),
        "binary test must run ({command}): {text}"
    );
    assert!(!ok, "a failing binary test must fail the run");
    // ...while the old `--lib` form runs ZERO tests and exits 0, which is
    // exactly the silent pass the finding describes.
    let (ok, text) = run("cargo test -p binpkg --lib tests::test_in_bin -- --exact");
    assert!(
        ok && text.contains("0 passed"),
        "control: --lib runs no binary tests: {text}"
    );
}

/// A stale PDG offset must not panic the tests classifier (same class as
/// the caller-list fix): the indexed file's `fn` byte offset, applied to
/// a live file whose leading comment grew by a multi-byte character,
/// lands inside that character.
#[test]
fn test_tests_classifier_tolerates_a_stale_multibyte_offset() {
    // Indexed: `//` + 9x `x` + `\n` (12 bytes) => `fn` starts at byte 12.
    let indexed = "//xxxxxxxxx\nfn test_x() {}\n";
    let stale = indexed.find("fn test_x").unwrap();
    assert_eq!(stale, 12);
    // Live: the last two `x` became `é` (2 bytes each), so `é` spans
    // bytes 9..13 and the stale byte 12 is INSIDE the second `é`.
    let live = "//xxxxxxx\u{e9}\u{e9}\nfn test_x() {}\n";
    assert!(
        !live.is_char_boundary(stale),
        "fixture must put the stale offset inside a multi-byte char"
    );
    // Must not panic: the offset floors to byte 9 and the scan simply
    // finds no attribute there ("no evidence"), never a crash.
    let _ = attributes_say_test(live, stale);
    // With an attribute above the fn and the REAL offset, it classifies.
    let with_attr = format!("#[test]\n{live}");
    let real = with_attr.find("fn test_x").unwrap();
    assert!(attributes_say_test(&with_attr, real).is_some());
}

#[test]
fn test_root_level_unit_test_has_no_module_prefix() {
    let dir = manifest_dir("proj");
    let source = "#[test]\nfn test_root() {\n    assert!(true);\n}\n";
    let src = parse_rust(source);
    let byte_range = (source.find("fn test_root").unwrap(), source.len());
    let test_fn = node(
        "test_root",
        &dir.path().join("src/lib.rs").display().to_string(),
        "rust",
        byte_range,
    );
    let cmd = command_for(&test_fn, Some(2), Some(&src));
    assert_eq!(
        cmd, "cargo test -p proj --lib test_root -- --exact",
        "command: {cmd}"
    );
}

#[test]
fn test_integration_test_command_uses_test_target() {
    let dir = manifest_dir("proj");
    let test_fn = node(
        "test_api",
        &dir.path().join("tests/api_test.rs").display().to_string(),
        "rust",
        (0, 1),
    );
    let cmd = command_for(&test_fn, None, None);
    assert_eq!(
        cmd, "cargo test -p proj --test api_test test_api",
        "cmd: {cmd}"
    );
}

/// A manifest can RENAME an integration target:
/// `[[test]] name = "acceptance" path = "tests/api_test.rs"`.
///
/// The filename stem is then NOT a target — `cargo test --test api_test`
/// is rejected with `no test target named api_test`, listing `acceptance`.
/// Verified against real cargo in both directions; the declared name is
/// the only answer that runs the tests.
#[test]
fn test_manifest_declared_test_target_wins_over_the_filename_stem() {
    let dir = manifest_dir("proj");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n\
             [[test]]\nname = \"acceptance\"\npath = \"tests/api_test.rs\"\n",
    )
    .unwrap();
    let tests = dir.path().join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    let file = tests.join("api_test.rs");
    std::fs::write(&file, "#[test]\nfn test_api() {}\n").unwrap();

    assert_eq!(
        integration_target_name(&file.display().to_string()).as_deref(),
        Some("acceptance"),
        "the declared [[test]] name is the target Cargo builds"
    );

    let test_fn = node("test_api", &file.display().to_string(), "rust", (0, 1));
    let cmd = command_for(&test_fn, None, None);
    assert_eq!(
        cmd, "cargo test -p proj --test acceptance test_api",
        "never the filename stem: {cmd}"
    );
}

/// A `[[test]]` without a `path` defaults to `tests/<name>.rs`, so that
/// file belongs to the declared target too.
#[test]
fn test_manifest_declared_target_without_a_path_claims_tests_named_after_it() {
    let dir = manifest_dir("proj");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n\
             [[test]]\nname = \"smoke\"\n",
    )
    .unwrap();
    let tests = dir.path().join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    let file = tests.join("smoke.rs");
    std::fs::write(&file, "#[test]\nfn test_smoke() {}\n").unwrap();

    assert_eq!(
        integration_target_name(&file.display().to_string()).as_deref(),
        Some("smoke"),
        "the default path tests/<name>.rs still resolves to the target"
    );
}

/// The declaration covers the file it names, and nothing else: a sibling
/// `tests/other_test.rs` keeps filename inference (its own target).
#[test]
fn test_manifest_declaration_does_not_capture_undeclared_siblings() {
    let dir = manifest_dir("proj");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"proj\"\nversion = \"0.1.0\"\n\n\
             [[test]]\nname = \"acceptance\"\npath = \"tests/api_test.rs\"\n",
    )
    .unwrap();
    let tests = dir.path().join("tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("api_test.rs"), "").unwrap();
    let other = tests.join("other_test.rs");
    std::fs::write(&other, "").unwrap();

    assert_eq!(
        integration_target_name(&other.display().to_string()).as_deref(),
        Some("other_test"),
        "an undeclared sibling is still its own auto-discovered target"
    );
}

/// Codex round 8 (id 4691102595): `tests/<dir>/` with a `main.rs` is ONE
/// Cargo target named after the DIRECTORY. The selector used the file stem,
/// so `tests/api/main.rs` produced `--test main`, which matches no target
/// and exits 0 having run nothing — a non-existent test read as a pass.
#[test]
fn test_directory_integration_target_is_named_after_the_directory() {
    let dir = manifest_dir("proj");
    let api = dir.path().join("tests").join("api");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(api.join("main.rs"), "mod handshake;\n").unwrap();
    std::fs::write(api.join("handshake.rs"), "fn helper() {}\n").unwrap();

    // The entry point and a sibling module both belong to target `api`.
    for file in ["main.rs", "handshake.rs"] {
        let test_fn = node(
            "test_handshake",
            &api.join(file).display().to_string(),
            "rust",
            (0, 1),
        );
        let cmd = command_for(&test_fn, None, None);
        assert_eq!(
            cmd, "cargo test -p proj --test api test_handshake",
            "{file} belongs to the `api` target: {cmd}"
        );
    }
}

/// The other half of the same review finding: without a `main.rs`,
/// `tests/<dir>/` is NOT a target — its files are helper modules compiled
/// into whichever targets `mod` them. Naming `--test <dir>` there would
/// point at a target Cargo does not have, so the command falls back to
/// `--tests`, which runs the test wherever it was compiled in.
#[test]
fn test_helper_module_without_main_rs_falls_back_to_all_test_targets() {
    let dir = manifest_dir("proj");
    let common = dir.path().join("tests").join("common");
    std::fs::create_dir_all(&common).unwrap();
    std::fs::write(common.join("mod.rs"), "fn fixture() {}\n").unwrap();

    let test_fn = node(
        "test_fixture",
        &common.join("mod.rs").display().to_string(),
        "rust",
        (0, 1),
    );
    let cmd = command_for(&test_fn, None, None);
    assert_eq!(
        cmd, "cargo test -p proj --tests test_fixture",
        "a helper module has no target of its own: {cmd}"
    );
}

/// CodeRabbit round 10 (id 4697090278): the other half. `tests/api/main.rs`
/// EXISTS, so the directory IS a target — but it does not declare THIS
/// file. `tests/api/shared_test.rs` is imported by `tests/other_test.rs`
/// via `#[path]`, so it is a helper that belongs to `other_test`, not to
/// `api`. Claiming `--test api` selected a target that does not contain
/// the test, and the command exited 0 having run nothing.
#[test]
fn test_a_shared_file_beside_a_main_rs_that_does_not_declare_it_falls_back() {
    let dir = manifest_dir("proj");
    let tests = dir.path().join("tests");
    let api = tests.join("api");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(api.join("main.rs"), "fn main() {}\n").unwrap();
    std::fs::write(api.join("shared_test.rs"), "#[test]\nfn test_shared() {}\n").unwrap();
    std::fs::write(
        tests.join("other_test.rs"),
        "#[path = \"api/shared_test.rs\"]\nmod shared_test;\n",
    )
    .unwrap();

    let test_fn = node(
        "test_shared",
        &api.join("shared_test.rs").display().to_string(),
        "rust",
        (0, 1),
    );
    assert_eq!(
        integration_target_name_for_test(&api.join("shared_test.rs").display().to_string()),
        None,
        "a main.rs that does not declare the file does not own it"
    );
    let cmd = command_for(&test_fn, None, None);
    assert_eq!(
        cmd, "cargo test -p proj --tests test_shared",
        "the fallback runs the test wherever it was compiled in: {cmd}"
    );
}

/// Kilo round 10 (id 4696869415) flagged that `main.rs` is checked with a
/// bare `is_file()`, which is CWD-relative for a relative path. Verified
/// rather than assumed:
///
/// * for an ABSOLUTE `file` — what the PDG stores, and what `call_sites`
///   produces by joining `root` — the manifest is built from the same
///   segments and is absolute too, so the check is already CWD-independent
///   and the finding is latent, not live. No change needed, and this test
///   pins that from two different CWDs.
/// * for a purely RELATIVE `file` the check IS CWD-anchored — but no code
///   could fix that, because nothing can recover the crate root from
///   `tests/api/handshake.rs` alone. Attempting a root-join made the
///   relative case no better while adding nothing for the absolute one, so
///   it was reverted rather than shipped as dead code.
///
/// `set_current_dir` is process-global: serialised on a lock and restored
/// on drop, including on panic.
#[test]
fn test_a_directory_integration_target_is_found_from_any_cwd() {
    // `set_current_dir` is process-global, and the lock is the CRATE-WIDE
    // one (CodeRabbit round 11, id 4698955238): a module-local `CWD_LOCK`
    // only excluded this module's tests, so `registry_test`'s CWD read
    // could still observe this change and compare against the wrong
    // directory. Same lock as every other process-global mutation.
    let _guard = crate::test_env::EnvLock
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    struct Restore(std::path::PathBuf);
    impl Drop for Restore {
        fn drop(&mut self) {
            let _ = std::env::set_current_dir(&self.0);
        }
    }
    let original = std::env::current_dir().unwrap();
    let _restore = Restore(original);

    let dir = manifest_dir("proj");
    let api = dir.path().join("tests").join("api");
    std::fs::create_dir_all(&api).unwrap();
    std::fs::write(api.join("main.rs"), "mod handshake;\n").unwrap();
    std::fs::write(api.join("handshake.rs"), "fn helper() {}\n").unwrap();

    // The contract that matters: an ABSOLUTE path is CWD-independent and
    // the directory target is recognised from anywhere.
    let absolute = api.join("handshake.rs").display().to_string();
    let elsewhere = tempfile::tempdir().unwrap();
    let from_root = integration_target_name(&absolute);
    std::env::set_current_dir(elsewhere.path()).unwrap();
    let from_elsewhere = integration_target_name(&absolute);
    assert_eq!(
        from_root.as_deref(),
        Some("api"),
        "recognised from the project: {from_root:?}"
    );
    assert_eq!(
        from_elsewhere, from_root,
        "an absolute path must not depend on the process CWD: root={from_root:?} elsewhere={from_elsewhere:?}"
    );

    // The stated limitation: a bare relative path is CWD-anchored, and no
    // crate-root resolution can rescue it. It resolves only while the CWD
    // is the project.
    let relative = "tests/api/handshake.rs";
    assert!(
        integration_target_name(relative).is_none(),
        "a bare relative path has no crate root to resolve against"
    );
    std::env::set_current_dir(dir.path()).unwrap();
    assert_eq!(
        integration_target_name(relative).as_deref(),
        Some("api"),
        "the same relative path resolves once the CWD is the project"
    );
}

#[test]
fn test_pytest_and_jest_commands() {
    let py = node(
        "test_round_trip",
        "/proj/tests/core_test.py",
        "python",
        (0, 1),
    );
    // A single executable node id: never `a or b` (pytest would take the
    // literal `or` as a path) and no `:line` suffix (not a node id).
    assert_eq!(
        command_for(&py, Some(12), None),
        "pytest /proj/tests/core_test.py::test_round_trip"
    );
    let js = node(
        "should round trip",
        "/proj/src/x.test.js",
        "javascript",
        (0, 1),
    );
    assert_eq!(
        command_for(&js, None, None),
        "npx jest /proj/src/x.test.js -t should round trip"
    );
}

#[test]
fn test_classify_prefers_attributes_then_names_then_paths() {
    let src = "#[test]\nfn test_a() {}\n\n#[cfg(unix)]\nfn helper() {}\n";
    let attr_fn = node(
        "irrelevant_name",
        "/proj/src/lib.rs",
        "rust",
        (src.find("fn helper").unwrap(), src.len()),
    );
    // Attribute above helper is #[cfg(unix)] — not a test...
    assert_eq!(classify_test(&attr_fn, Some(src)), None);
    let attr_test = node(
        "irrelevant_name",
        "/proj/src/lib.rs",
        "rust",
        (src.find("fn test_a").unwrap(), src.len()),
    );
    assert_eq!(classify_test(&attr_test, Some(src)), Some("#[test]"));
    // Name convention without attributes.
    let named = node("test_only_name", "/proj/src/lib.rs", "rust", (0, 1));
    assert_eq!(classify_test(&named, Some("")), Some("name convention"));
    // Test-file path convention.
    let in_file = node("verify", "/proj/tests/api_test.rs", "rust", (0, 1));
    assert_eq!(classify_test(&in_file, Some("")), Some("test file"));
    // Non-test production fn with no evidence.
    let prod = node("compute", "/proj/src/lib.rs", "rust", (0, 1));
    assert_eq!(classify_test(&prod, Some("")), None);
}

#[test]
fn test_callers_by_depth_orders_direct_first_and_dedups() {
    let mut pdg = crate::graph::pdg::ProgramDependenceGraph::new();
    let mk = |pdg: &mut crate::graph::pdg::ProgramDependenceGraph, name: &str| {
        pdg.add_node(node(name, "/p/src/lib.rs", "rust", (0, 1)))
    };
    let leaf = mk(&mut pdg, "leaf");
    let mid = mk(&mut pdg, "mid");
    let top = mk(&mut pdg, "top");
    pdg.add_call_edges(vec![(mid, leaf), (top, mid), (leaf, leaf)]);
    let by_depth = callers_by_depth(&pdg, leaf, 3);
    assert_eq!(by_depth.len(), 2, "self-loop deduped: {by_depth:?}");
    assert_eq!(by_depth[0].1, 1);
    assert_eq!(by_depth[1].1, 2);
}

/// Same-named methods in different classes must not be conflated. The
/// intended definition is selected POSITIONALLY from the PDG byte range;
/// when position cannot decide, the ambiguity is reported instead of a
/// guessed id (the traversal used to return whichever match it saw last,
/// so TestA's test could be reported as TestB's — or vice versa).
#[test]
fn test_pytest_same_named_methods_in_different_classes_are_disambiguated() {
    let dir = manifest_dir("pyshared");
    let file = dir.path().join("tests").join("shared_test.py");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    let source = "class TestFirst:\n    def test_shared(self):\n        assert True\n\nclass TestSecond:\n    def test_shared(self):\n        assert True\n";
    std::fs::write(&file, source).unwrap();
    let src = parse_python(source);
    let shown = file.display().to_string();
    let first = source.find("def test_shared").unwrap();
    let second = source[first + 1..].find("def test_shared").unwrap() + first + 1;
    // A point INSIDE each definition. The PDG range is the definition's
    // span up to its end byte, but the selection must only depend on
    // containment, so a degenerate point inside the body is enough.
    let at = |start: usize| node("test_shared", &shown, "python", (start, start));
    assert_eq!(
        command_for(&at(first + "def ".len()), Some(2), Some(&src)),
        format!("pytest {shown}::TestFirst::test_shared")
    );
    assert_eq!(
        command_for(&at(second + "def ".len()), Some(6), Some(&src)),
        format!("pytest {shown}::TestSecond::test_shared")
    );
    // Offset 0 lands on the class header, not a definition: the id must
    // NOT be guessed — both candidates are disclosed instead.
    let ambiguous = command_for(&at(0), Some(1), Some(&src));
    assert!(
        ambiguous.starts_with("pytest (ambiguous pytest id for test_shared"),
        "ambiguous: {ambiguous}"
    );
    assert!(ambiguous.contains(&format!("{shown}::TestFirst::test_shared")));
    assert!(ambiguous.contains(&format!("{shown}::TestSecond::test_shared")));

    // Execute the generated command with real pytest (if installed) on a
    // failing FIRST definition: the intended test must be the one that
    // runs, and the run must fail.
    if std::process::Command::new("python3")
        .arg("-m")
        .arg("pytest")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success())
    {
        let failing = source.replacen(
            "    def test_shared(self):\n        assert True",
            "    def test_shared(self):\n        assert False",
            1,
        );
        std::fs::write(&file, &failing).unwrap();
        let failing_src = parse_python(&failing);
        let command = command_for(&at(first + "def ".len()), Some(2), Some(&failing_src));
        assert_eq!(command, format!("pytest {shown}::TestFirst::test_shared"));
        let out = std::process::Command::new("sh")
            .arg("-c")
            .arg(&command)
            .current_dir(dir.path())
            .output()
            .expect("pytest runnable");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains("1 failed"),
            "the intended definition must run: {text}"
        );
        assert!(!out.status.success());
    }
}

/// CR-R4-2: a `[lib]` that sits next to a `[[bin]]` is still the library.
/// The library used to be recognised only while no `[[bin]]` existed, so
/// every package with both classified its library tests as binary tests
/// (and then ran zero tests under `--bin`).
///
/// CR-R4-3: `--bin` must carry the OWNING binary target's name. Using the
/// package name is right only when no `[[bin]]` renamed the target.
#[test]
fn test_lib_target_survives_a_coexisting_bin_target() {
    let dir = manifest_dir("both");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"both\"\nversion = \"0.1.0\"\n\n\
             [lib]\npath = \"core/api.rs\"\n\n\
             [[bin]]\nname = \"runner\"\npath = \"src/entry.rs\"\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("core")).unwrap();
    std::fs::write(
        dir.path().join("core/api.rs"),
        "#[test]\nfn test_in_lib() {}\n",
    )
    .unwrap();
    std::fs::write(
        dir.path().join("src/entry.rs"),
        "#[test]\nfn test_in_bin() {}\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.path().join("src/bin")).unwrap();
    std::fs::write(
        dir.path().join("src/bin/worker.rs"),
        "#[test]\nfn test_in_worker() {}\n",
    )
    .unwrap();

    // The library stays the library despite the coexisting `[[bin]]`.
    assert_eq!(
        command_for_test_at(&dir, "core/api.rs", "test_in_lib"),
        "cargo test -p both --lib test_in_lib -- --exact"
    );
    // A `[[bin]]` with its own name and path is selected by THAT name.
    assert_eq!(
        command_for_test_at(&dir, "src/entry.rs", "test_in_bin"),
        "cargo test -p both --bin runner test_in_bin -- --exact"
    );
    // An auto-discovered `src/bin/<name>.rs` binary carries its stem.
    assert_eq!(
        command_for_test_at(&dir, "src/bin/worker.rs", "test_in_worker"),
        "cargo test -p both --bin worker test_in_worker -- --exact"
    );
}

#[test]
fn test_cargo_package_reads_nearest_manifest() {
    let dir = tempfile::tempdir().expect("tempdir");
    let nested = dir.path().join("crates/sub");
    std::fs::create_dir_all(&nested).expect("dirs");
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"fixture_pkg\"\n",
    )
    .unwrap();
    let file = nested.join("src/lib.rs");
    let name = cargo_package(file.to_str().expect("utf8 path"));
    assert_eq!(name.as_deref(), Some("fixture_pkg"));
    // A path with no manifest above it yields None.
    assert_eq!(cargo_package("/definitely/not/a/crate/x.rs"), None);
}
