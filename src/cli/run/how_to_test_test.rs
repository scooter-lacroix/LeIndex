//! Tests for `how_to_test`.

use super::*;
use std::time::Duration;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn package(name: &str) -> String {
    format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n")
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

#[test]
fn test_workspace_member_owns_its_files() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\", \"tools/xtask\"]\n",
    );
    write(&root.join("crates/alpha/Cargo.toml"), &package("alpha"));
    write(&root.join("crates/beta/Cargo.toml"), &package("beta"));
    write(&root.join("tools/xtask/Cargo.toml"), &package("xtask"));
    write(&root.join("crates/beta/src/lib.rs"), "");

    let got = how_to_test(&root, Path::new("crates/beta/src/lib.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p beta --lib");
    assert_eq!(got.cwd, PathBuf::from("."));
    let xtask = how_to_test(&root, Path::new("tools/xtask/src/main.rs")).unwrap();
    assert_eq!(xtask.command, "cargo test -p xtask --lib");
}

#[test]
fn test_script_wrapper_replaces_bare_cargo() {
    let (_guard, root) = project();
    write(&root.join("Cargo.toml"), &package("maestro"));
    write(&root.join("script/cargo-heavy"), "#!/bin/sh\n");
    let got = how_to_test(&root, Path::new("src/lib.rs")).unwrap();
    assert_eq!(got.command, "./script/cargo-heavy test -p maestro --lib");
    assert!(got.source.contains("wrapper"), "{}", got.source);
}

#[test]
fn test_nearest_package_json_test_script() {
    let (_guard, root) = project();
    write(
        &root.join("web/package.json"),
        r#"{"scripts":{"test":"vitest","build":"tsc"}}"#,
    );
    write(&root.join("web/src/app.ts"), "");
    let got = how_to_test(&root, Path::new("web/src/app.ts")).unwrap();
    assert_eq!(got.command, "npm test");
    assert_eq!(got.cwd, PathBuf::from("web"));
}

#[test]
fn test_fenced_doc_commands_become_the_gate_and_alternatives() {
    let (_guard, root) = project();
    write(
        &root.join("AGENTS.md"),
        "# Rules\n```bash\ncargo fmt --all --check\ncargo clippy --workspace -- -D warnings\n$ cargo test --workspace\nrm -rf /\n```\nprose cargo test outside a fence\n",
    );
    let got = how_to_test(&root, Path::new("README.md")).unwrap();
    assert_eq!(got.command, "cargo test --workspace");
    assert_eq!(
        got.also,
        vec![
            "cargo fmt --all --check",
            "cargo clippy --workspace -- -D warnings",
            "cargo test --workspace"
        ]
    );
}

#[test]
fn test_makefile_and_justfile_targets() {
    let (_guard, root) = project();
    write(
        &root.join("Makefile"),
        "CC := gcc\n.PHONY: test\ntest:\n\techo hi\n",
    );
    assert_eq!(
        how_to_test(&root, Path::new("a.c")).unwrap().command,
        "make test"
    );
    let (_guard2, root2) = project();
    write(&root2.join("justfile"), "test arg='x':\n    echo hi\n");
    assert_eq!(
        how_to_test(&root2, Path::new("a.c")).unwrap().command,
        "just test"
    );
}

#[test]
fn test_nothing_detected_is_an_explicit_error() {
    let (_guard, root) = project();
    let error = how_to_test(&root, Path::new("a.txt")).unwrap_err();
    assert!(
        error.to_string().contains("no test command found"),
        "{error}"
    );
}

#[test]
fn test_path_outside_project_is_rejected() {
    let (_guard, root) = project();
    let error = how_to_test(&root, Path::new("/etc/passwd")).unwrap_err();
    assert!(error.to_string().contains("outside the project"), "{error}");
}

#[test]
fn test_cache_is_reused_until_a_manifest_mtime_changes() {
    let (_guard, root) = project();
    let manifest = root.join("Cargo.toml");
    write(&manifest, &package("first"));
    let old_time = mtime(&manifest).unwrap();
    assert_eq!(
        how_to_test(&root, Path::new("src/lib.rs")).unwrap().command,
        "cargo test -p first --lib"
    );

    // Same mtime, different content: the cached answer is served.
    write(&manifest, &package("second"));
    std::fs::File::options()
        .write(true)
        .open(&manifest)
        .unwrap()
        .set_modified(old_time)
        .unwrap();
    assert_eq!(
        how_to_test(&root, Path::new("src/lib.rs")).unwrap().command,
        "cargo test -p first --lib",
        "unchanged mtime must hit the cache"
    );

    // Newer mtime: the cache is invalidated and rebuilt.
    std::fs::File::options()
        .write(true)
        .open(&manifest)
        .unwrap()
        .set_modified(old_time + Duration::from_secs(30))
        .unwrap();
    assert_eq!(
        how_to_test(&root, Path::new("src/lib.rs")).unwrap().command,
        "cargo test -p second --lib",
        "a changed mtime must invalidate the cache"
    );
}

#[test]
fn test_new_manifest_appearing_invalidates_the_cache() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/a/Cargo.toml"), &package("a"));
    assert!(how_to_test(&root, Path::new("README.md")).is_err());
    // A justfile that did not exist at the first discovery appears.
    write(&root.join("justfile"), "test:\n    echo\n");
    assert_eq!(
        how_to_test(&root, Path::new("README.md")).unwrap().command,
        "just test"
    );
}
