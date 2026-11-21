//! Tests for the sandbox (`src/cli/run/sandbox.rs`).

use super::run_log;
use super::sandbox::*;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn request<'a>(
    root: &'a Path,
    storage: &'a Path,
    argv: Option<&'a [String]>,
    command: Option<&'a str>,
) -> RunRequest<'a> {
    RunRequest {
        project_root: root,
        storage,
        argv,
        command,
        cwd: None,
        timeout_secs: Some(60),
        name: None,
    }
}

fn argv(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|part| (*part).to_string()).collect()
}

#[tokio::test]
async fn test_run_rejects_non_allow_listed_programs() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        argv(&["rm", "-rf", "x"]),
        argv(&["sh", "-c", "id"]),
        argv(&["python", "-c", "print(1)"]),
    ] {
        let error = run(request(dir.path(), dir.path(), Some(&bad), None))
            .await
            .unwrap_err();
        assert!(
            matches!(error, RunError::NotAllowed(_)),
            "{bad:?} -> {error}"
        );
    }
}

#[tokio::test]
async fn test_run_rejects_shell_metacharacters_in_single_string() {
    let dir = tempfile::tempdir().unwrap();
    for bad in [
        "cargo; rm -rf x",
        "$(id)",
        "cargo test | cat",
        "cargo `id`",
        "cargo test > out",
        "cargo 'a'",
    ] {
        let error = run(request(dir.path(), dir.path(), None, Some(bad)))
            .await
            .unwrap_err();
        assert!(
            matches!(error, RunError::ShellMetacharacters(_)),
            "{bad} -> {error}"
        );
    }
}

#[tokio::test]
async fn test_run_rejects_cwd_escape() {
    let outer = tempfile::tempdir().unwrap();
    let project = outer.path().join("project");
    std::fs::create_dir_all(&project).unwrap();
    let args = argv(&["cargo", "--version"]);
    for cwd in [PathBuf::from(".."), outer.path().to_path_buf()] {
        let mut req = request(&project, outer.path(), Some(&args), None);
        req.cwd = Some(&cwd);
        let error = run(req).await.unwrap_err();
        assert!(
            matches!(error, RunError::PathEscape(_)),
            "{cwd:?} -> {error}"
        );
    }
}

#[tokio::test]
async fn test_run_rejects_script_escape_and_missing_script() {
    let dir = tempfile::tempdir().unwrap();
    let escape = argv(&["script/../../evil"]);
    let error = run(request(dir.path(), dir.path(), Some(&escape), None))
        .await
        .unwrap_err();
    assert!(matches!(error, RunError::NotAllowed(_)), "{error}");
    let missing = argv(&["./script/nope"]);
    let error = run(request(dir.path(), dir.path(), Some(&missing), None))
        .await
        .unwrap_err();
    assert!(matches!(error, RunError::MissingScript(_)), "{error}");
}

#[tokio::test]
async fn test_run_cargo_version_is_summarised_with_log() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    let args = argv(&["cargo", "--version"]);
    let mut req = request(dir.path(), &storage, Some(&args), None);
    req.name = Some("cargo-version");
    let summary = run(req).await.unwrap();
    assert_eq!(summary.exit_code, 0);
    let log = summary.log_path.clone().unwrap();
    assert_eq!(
        log,
        std::fs::canonicalize(&storage)
            .unwrap()
            .join("logs/cargo-version.log")
    );
    assert!(summary.render().ends_with(log.to_str().unwrap()));
    let stored = run_log::read_by_handle(&storage, "log://cargo-version").unwrap();
    assert!(stored.starts_with("cargo "), "{stored}");
}

/// End-to-end: a real failing `cargo test` on a fixture crate runs through the
/// sandbox, is summarised by the CargoTest parser, and the rendered summary
/// stays under the 1200-char cap.
#[tokio::test]
async fn test_real_failing_cargo_test_render_is_small() {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let storage = root.join("store");
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"failing_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "#[cfg(test)]\nmod tests {\n    #[test]\n    fn passes() { assert!(true); }\n    #[test]\n    fn fails() { assert_eq!(1 + 1, 3); }\n}\n",
    )
    .unwrap();
    let args = argv(&["cargo", "test"]);
    let mut req = request(&root, &storage, Some(&args), None);
    req.name = Some("failing-probe");
    req.timeout_secs = Some(600);
    let summary = run(req).await.unwrap();
    assert_ne!(summary.exit_code, 0);
    assert_eq!(summary.failures.len(), 1, "{}", summary.render());
    let rendered = summary.render();
    let chars = rendered.chars().count();
    println!("MEASURED_RENDER_CHARS={chars}");
    assert!(
        chars <= 1200,
        "render exceeded cap: {chars} chars\n{rendered}"
    );
}

#[tokio::test]
async fn test_run_scrubs_secrets_before_storing() {
    let dir = tempfile::tempdir().unwrap();
    let script_dir = dir.path().join("script");
    std::fs::create_dir_all(&script_dir).unwrap();
    let script = script_dir.join("leak");
    std::fs::write(&script, "#!/bin/sh\necho password=hunter2secret\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let args = argv(&["./script/leak"]);
    let mut req = request(dir.path(), dir.path(), Some(&args), None);
    req.name = Some("leak");
    let summary = run(req).await.unwrap();
    let stored = std::fs::read_to_string(summary.log_path.unwrap()).unwrap();
    assert!(!stored.contains("hunter2secret"), "{stored}");
    assert!(stored.contains("[REDACTED]"), "{stored}");
}

#[tokio::test]
async fn test_run_timeout_kills_descendant_holding_the_pipes() {
    let dir = tempfile::tempdir().unwrap();
    let script_dir = dir.path().join("script");
    std::fs::create_dir_all(&script_dir).unwrap();
    let script = script_dir.join("hang");
    // The direct child exits at once; the backgrounded sleep keeps stdout open.
    std::fs::write(&script, "#!/bin/sh\nsleep 60 &\nexit 0\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let args = argv(&["script/hang"]);
    let mut req = request(dir.path(), dir.path(), Some(&args), None);
    req.timeout_secs = Some(1);
    let started = Instant::now();
    let summary = run(req).await.unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(20),
        "deadline did not cover the pipes"
    );
    assert!(
        summary.headline.contains("timed out after 1s"),
        "{}",
        summary.headline
    );
}

#[test]
fn test_log_names_cannot_traverse() {
    assert!(run_log::sanitize_name("g6-clippy").is_some());
    assert!(run_log::sanitize_name("../x").is_none());
    assert!(run_log::sanitize_name("a/b").is_none());
    assert!(run_log::sanitize_name("").is_none());
}

#[test]
fn test_retention_keeps_newest_logs() {
    let dir = tempfile::tempdir().unwrap();
    for index in 0..5 {
        let log = run_log::prepare(dir.path(), Some(&format!("r{index}"))).unwrap();
        std::fs::write(&log.path, "x").unwrap();
        std::thread::sleep(Duration::from_millis(20));
    }
    run_log::enforce_retention(dir.path(), 2).unwrap();
    let left = std::fs::read_dir(run_log::logs_dir(dir.path()))
        .unwrap()
        .count();
    assert_eq!(left, 2);
    assert!(run_log::read_by_handle(dir.path(), "log://r4").is_ok());
}
