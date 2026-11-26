//! Tests for `how_to_test`.

use super::*;
use std::time::{Duration, SystemTime};

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

/// Set a DIRECTORY's mtime to an explicit time. `File::set_modified` needs a
/// writable handle, and Linux refuses to open a directory for writing
/// (EISDIR), so go through `utimensat` on the path instead.
#[cfg(unix)]
fn set_dir_mtime(dir: &Path, time: SystemTime) {
    let ns: i64 = time
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_nanos()
        .try_into()
        .expect("mtime fits in i64 nanoseconds");
    let ts = libc::timespec {
        tv_sec: ns.div_euclid(1_000_000_000),
        tv_nsec: ns.rem_euclid(1_000_000_000),
    };
    let times = [ts, ts];
    let c_path = std::ffi::CString::new(dir.as_os_str().as_encoded_bytes()).unwrap();
    let rc = unsafe { libc::utimensat(libc::AT_FDCWD, c_path.as_ptr(), times.as_ptr(), 0) };
    assert_eq!(rc, 0, "utimensat failed for {}", dir.display());
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
    assert_eq!(xtask.command, "cargo test -p xtask");
}

#[test]
fn test_binary_only_member_command_drops_lib() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/cli/Cargo.toml"), &package("cli"));
    write(&root.join("crates/cli/src/main.rs"), "");
    let got = how_to_test(&root, Path::new("crates/cli/src/main.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p cli");
}

/// `cargo test --lib` on a package with no library target is a hard error
/// (`no library targets found`), so the flag must never be emitted for a
/// binary-only member — not even for a non-binary source path, which is what
/// a shared helper module under `src/` would produce.
#[test]
fn test_binary_only_member_never_emits_lib_for_any_source_path() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/cli/Cargo.toml"), &package("cli"));
    write(&root.join("crates/cli/src/main.rs"), "");
    write(&root.join("crates/cli/src/helper.rs"), "");
    for path in ["crates/cli/src/helper.rs", "crates/cli/src/main.rs"] {
        let got = how_to_test(&root, Path::new(path)).unwrap();
        assert_eq!(
            got.command, "cargo test -p cli",
            "--lib must be dropped for binary-only member, path {path}"
        );
    }
}

/// A `[lib] path` that is not the default `src/lib.rs` still means the package
/// HAS a library, so `--lib` stays valid.
#[test]
fn test_custom_lib_path_is_still_a_library_target() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(
        &root.join("crates/core/Cargo.toml"),
        &format!("{}[lib]\npath = \"src/core.rs\"\n", package("core")),
    );
    write(&root.join("crates/core/src/core.rs"), "");
    let got = how_to_test(&root, Path::new("crates/core/src/core.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p core --lib");
}

/// A `[lib]` section means a library target exists even without a file check
/// being decisive on the default path.
#[test]
fn test_explicit_lib_section_keeps_the_lib_flag() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(
        &root.join("crates/core/Cargo.toml"),
        &format!("{}[lib]\n", package("core")),
    );
    write(&root.join("crates/core/src/lib.rs"), "");
    let got = how_to_test(&root, Path::new("crates/core/src/lib.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p core --lib");
}

/// The `has_lib` fact is derived from `src/lib.rs`, which is stamped, so adding
/// a library to a previously binary-only member invalidates the cached answer
/// instead of leaving `--lib` off forever.
#[test]
fn test_adding_a_library_invalidates_the_cached_answer() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/cli/Cargo.toml"), &package("cli"));
    write(&root.join("crates/cli/src/main.rs"), "");
    let before = how_to_test(&root, Path::new("crates/cli/src/main.rs")).unwrap();
    assert_eq!(before.command, "cargo test -p cli");

    write(&root.join("crates/cli/src/lib.rs"), "");
    // Push the mtime forward so the invalidation cannot depend on timestamp
    // resolution: a same-second write must not be relied on.
    let later = SystemTime::now() + Duration::from_secs(5);
    let lib = root.join("crates/cli/src/lib.rs");
    std::fs::File::options()
        .write(true)
        .open(&lib)
        .unwrap()
        .set_modified(later)
        .unwrap();
    assert!(mtime(&lib).unwrap() > SystemTime::now());

    let after = how_to_test(&root, Path::new("crates/cli/src/main.rs")).unwrap();
    assert_eq!(
        after.command, "cargo test -p cli",
        "main.rs is still a binary source"
    );
    let lib_path = how_to_test(&root, Path::new("crates/cli/src/lib.rs")).unwrap();
    assert_eq!(
        lib_path.command, "cargo test -p cli --lib",
        "the new library target must be picked up after the cache invalidates"
    );
}

#[test]
fn test_integration_test_path_selects_the_test_target() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/beta/Cargo.toml"), &package("beta"));
    write(&root.join("crates/beta/tests/integration.rs"), "");
    let got = how_to_test(&root, Path::new("crates/beta/tests/integration.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p beta --test integration");
}

/// `tests/api/main.rs` is Cargo integration target **`api`**, not `main`.
///
/// `cargo test --test main` is rejected with `no test target named main`, so a
/// directory-based target must be named after its DIRECTORY.
#[test]
fn test_directory_based_integration_target_uses_the_directory_name() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/beta/Cargo.toml"), &package("beta"));
    write(&root.join("crates/beta/tests/api/main.rs"), "");
    let got = how_to_test(&root, Path::new("crates/beta/tests/api/main.rs")).unwrap();
    assert_eq!(
        got.command, "cargo test -p beta --test api",
        "target is the directory, never the filename stem"
    );
}

/// A helper module inside a `tests/<dir>/` target belongs to that target, so it
/// must not be turned into a selector named after the helper's own stem.
#[test]
fn test_helper_module_under_a_test_target_maps_to_that_target() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/beta/Cargo.toml"), &package("beta"));
    write(&root.join("crates/beta/tests/api/main.rs"), "");
    write(&root.join("crates/beta/tests/api/support.rs"), "");
    write(&root.join("crates/beta/tests/api/cases/nested.rs"), "");
    for path in [
        "crates/beta/tests/api/support.rs",
        "crates/beta/tests/api/cases/nested.rs",
    ] {
        let got = how_to_test(&root, Path::new(path)).unwrap();
        assert_eq!(
            got.command, "cargo test -p beta --test api",
            "helper path {path} must select its owning target"
        );
    }
}

/// A top-level `tests/<name>.rs` is still its own target — the directory rule
/// must not swallow the ordinary single-file case.
#[test]
fn test_top_level_integration_file_keeps_its_own_name() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/beta/Cargo.toml"), &package("beta"));
    write(&root.join("crates/beta/tests/cli_integration_test.rs"), "");
    let got = how_to_test(
        &root,
        Path::new("crates/beta/tests/cli_integration_test.rs"),
    )
    .unwrap();
    assert_eq!(
        got.command,
        "cargo test -p beta --test cli_integration_test"
    );
}

/// A DIRECTORY request for a binary-only package must not emit `--lib` either.
///
/// `how_to_test "."` (and any ordinary module such as `src/worker.rs`) used to
/// answer `cargo test -p <pkg> --lib`, which Cargo rejects with
/// `no library targets found` on a package that builds only a binary. The
/// unrestricted command runs the package's tests, so the flag must be omitted.
#[test]
fn test_binary_only_package_directory_request_omits_lib() {
    let (_guard, root) = project();
    write(&root.join("Cargo.toml"), &package("widget"));
    write(&root.join("src/main.rs"), "");
    write(&root.join("src/worker.rs"), "");
    // Project root, the package directory, and a plain module source.
    for path in [".", "src", "src/worker.rs", "src/main.rs"] {
        let got = how_to_test(&root, Path::new(path)).unwrap();
        assert_eq!(
            got.command, "cargo test -p widget",
            "binary-only package must never be told --lib, path {path}"
        );
    }
    // The same requests on a package WITH a library keep the restriction.
    write(&root.join("src/lib.rs"), "");
    let lib = how_to_test(&root, Path::new("src/lib.rs")).unwrap();
    assert_eq!(lib.command, "cargo test -p widget --lib");
}

#[test]
fn test_directory_request_starts_package_search_at_the_directory() {
    let (_guard, root) = project();
    write(
        &root.join("web/package.json"),
        r#"{"scripts":{"test":"vitest"}}"#,
    );
    let got = how_to_test(&root, Path::new("web")).unwrap();
    assert_eq!(got.command, "npm test");
    assert_eq!(got.cwd, PathBuf::from("web"));
}

#[test]
fn test_dotdot_path_resolves_to_the_real_owner() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/alpha/Cargo.toml"), &package("alpha"));
    write(&root.join("crates/beta/Cargo.toml"), &package("beta"));
    write(&root.join("crates/beta/src/lib.rs"), "");
    let got = how_to_test(&root, Path::new("crates/alpha/../beta/src/lib.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p beta --lib");
}

#[test]
fn test_traversing_outside_is_rejected_even_when_lexically_inside() {
    let (_guard, root) = project();
    std::fs::create_dir_all(root.join("a")).unwrap();
    let error = how_to_test(&root, Path::new("a/../../outside/src/file.ts")).unwrap_err();
    assert!(error.to_string().contains("outside the project"), "{error}");
}

#[test]
#[cfg(unix)]
fn test_new_workspace_member_dir_invalidates_cache() {
    let (_guard, root) = project();
    write(
        &root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\n",
    );
    write(&root.join("crates/a/Cargo.toml"), &package("a"));
    write(&root.join("crates/a/src/lib.rs"), "");
    let got = how_to_test(&root, Path::new("crates/a/src/lib.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p a --lib");

    // A new member appears after the cached discovery; push the wildcard
    // parent's mtime forward so the invalidation does not depend on clock
    // granularity. The parent is a directory, so set its mtime by path.
    write(&root.join("crates/b/Cargo.toml"), &package("b"));
    write(&root.join("crates/b/src/lib.rs"), "");
    set_dir_mtime(
        &root.join("crates"),
        SystemTime::now() + Duration::from_secs(10),
    );

    let got = how_to_test(&root, Path::new("crates/b/src/lib.rs")).unwrap();
    assert_eq!(got.command, "cargo test -p b --lib");
}

#[test]
fn test_script_wrapper_replaces_bare_cargo() {
    let (_guard, root) = project();
    write(&root.join("Cargo.toml"), &package("maestro"));
    write(&root.join("src/lib.rs"), "");
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
    write(&root.join("src/lib.rs"), "");
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
