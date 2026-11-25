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
    // The direct child exits at once; the backgrounded sleep keeps stdout
    // open and records its pid so the test can prove it was terminated.
    std::fs::write(
        &script,
        "#!/bin/sh\nsleep 60 &\necho $! > descendant.pid\nexit 0\n",
    )
    .unwrap();
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
    // The whole process group must be gone, not just the direct child. The
    // old implementation asked tokio for the child's pid after `wait()` had
    // already reaped it, got `None`, and skipped the group signal, leaving
    // this `sleep` alive for the rest of its 60 seconds.
    let pid_file = dir.path().join("descendant.pid");
    let pid: i32 = std::fs::read_to_string(&pid_file)
        .expect("descendant pid recorded")
        .trim()
        .parse()
        .expect("numeric pid");
    let mut gone = false;
    for _ in 0..100 {
        // SAFETY: `kill(pid, 0)` only probes existence; no signal is sent.
        let alive = unsafe { libc::kill(pid, 0) == 0 };
        if !alive {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(
        gone,
        "descendant sleep (pid {pid}) survived the timeout kill"
    );
}

/// stdout and stderr are collected in separate buffers and stored as stdout
/// followed by stderr, so a stderr line written while a structured stdout
/// record is in flight can never split that record.
#[tokio::test]
async fn test_run_keeps_stdout_records_intact_when_stderr_interleaves() {
    let dir = tempfile::tempdir().unwrap();
    let script_dir = dir.path().join("script");
    std::fs::create_dir_all(&script_dir).unwrap();
    let script = script_dir.join("interleave");
    // Half a JSON record, a stderr line, then the rest of the record.
    let body = format!(
        "#!/bin/sh\nprintf '%s' '{}'\necho 'warning: something on stderr' >&2\nprintf '%s\n' '{}'\n",
        r#"{"reason":"compiler-message","message":{"level"#, r#"":"warning"}}"#,
    );
    std::fs::write(&script, body).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let args = argv(&["script/interleave"]);
    let mut req = request(dir.path(), dir.path(), Some(&args), None);
    req.name = Some("interleave");
    let summary = run(req).await.unwrap();
    let stored = std::fs::read_to_string(summary.log_path.unwrap()).unwrap();
    let mut lines = stored.lines();
    assert_eq!(
        lines.next(),
        Some("{\"reason\":\"compiler-message\",\"message\":{\"level\":\"warning\"}}"),
        "stdout record was split by stderr: {stored:?}"
    );
    assert_eq!(lines.next(), Some("warning: something on stderr"));
}

#[test]
fn test_finish_log_redacts_every_secret_on_a_line() {
    // Distinct multi-character values: a one-letter value would collide with
    // ordinary words in the scrubbed text and make the survival check
    // meaningless (e.g. `y` is part of `key`).
    let raw =
        b"Bearer s3cr3tA Bearer s3cr3tB\nkey=s3cr3tC key=s3cr3tD\ntoken=s3cr3tE token=s3cr3tF\n";
    let out = finish_log(raw, 0, None);
    assert_eq!(
        out,
        "Bearer [REDACTED] Bearer [REDACTED]\n\
         key=[REDACTED] key=[REDACTED]\n\
         token=[REDACTED] token=[REDACTED]\n"
    );
    assert_eq!(out.matches("[REDACTED]").count(), 6, "{out}");
    for secret in [
        "s3cr3tA", "s3cr3tB", "s3cr3tC", "s3cr3tD", "s3cr3tE", "s3cr3tF",
    ] {
        assert!(!out.contains(secret), "{secret} survived: {out}");
    }
}

#[test]
fn test_log_names_cannot_traverse() {
    assert!(run_log::sanitize_name("g6-clippy").is_some());
    assert!(run_log::sanitize_name("../x").is_none());
    assert!(run_log::sanitize_name("a/b").is_none());
    assert!(run_log::sanitize_name("").is_none());
}

#[test]
fn test_anonymous_log_names_are_unique_and_reserved() {
    // Two anonymous runs prepared back to back (same millisecond, same
    // process) must never share a path: the second write would otherwise
    // overwrite the first run's output under the same `log://` handle.
    let dir = tempfile::tempdir().unwrap();
    let mut paths = Vec::new();
    for _ in 0..64 {
        let log = run_log::prepare(dir.path(), None).unwrap();
        assert!(
            log.path.is_file(),
            "anonymous log must be reserved on prepare: {}",
            log.path.display()
        );
        paths.push(log.path);
    }
    let mut unique = paths.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(unique.len(), paths.len(), "anonymous log names collided");
    // The generated name stays a valid handle.
    let first = paths[0]
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|n| n.strip_suffix(".log"))
        .unwrap();
    assert!(
        run_log::sanitize_name(first).is_some(),
        "generated name {first} is not a valid handle"
    );
    assert!(run_log::read_by_handle(dir.path(), &run_log::handle(first)).is_ok());
}

#[test]
fn test_explicit_log_name_replaces_earlier_log() {
    // A named run is an address the caller chose; it must still be allowed
    // to replace an earlier log of the same name (not be refused as a
    // collision).
    let dir = tempfile::tempdir().unwrap();
    let first = run_log::prepare(dir.path(), Some("check-scoped")).unwrap();
    std::fs::write(&first.path, "first").unwrap();
    let second = run_log::prepare(dir.path(), Some("check-scoped")).unwrap();
    assert_eq!(first.path, second.path);
    std::fs::write(&second.path, "second").unwrap();
    assert_eq!(
        run_log::read_by_handle(dir.path(), "log://check-scoped").unwrap(),
        "second"
    );
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

/// Codex round 6 (id 4688809343): `--message-format=json` is a CARGO option,
/// so appending it put it AFTER a `--` separator where it is handed to
/// clippy-driver, which rejects it ("Unrecognized option", exit 101). The
/// documented gate `cargo clippy --workspace --all-targets -- -D warnings` is
/// exactly that shape, so the strict gate became unrunnable through `run`.
#[test]
fn test_json_format_lands_before_the_argument_separator() {
    assert_eq!(
        with_json_format(argv(&[
            "cargo",
            "clippy",
            "--workspace",
            "--",
            "-D",
            "warnings"
        ])),
        argv(&[
            "cargo",
            "clippy",
            "--workspace",
            "--message-format=json",
            "--",
            "-D",
            "warnings",
        ]),
        "the flag must precede `--`"
    );
    // No separator: appended at the end, unchanged behaviour.
    assert_eq!(
        with_json_format(argv(&["cargo", "check"])),
        argv(&["cargo", "check", "--message-format=json"])
    );
    // Already present (either side of the separator): never duplicated.
    assert_eq!(
        with_json_format(argv(&["cargo", "build", "--message-format=json"])),
        argv(&["cargo", "build", "--message-format=json"])
    );
    // Not a compiling subcommand: untouched.
    assert_eq!(
        with_json_format(argv(&["cargo", "test", "--", "-D"])),
        argv(&["cargo", "test", "--", "-D"])
    );
}

/// The transformation has to make the REAL gate runnable, not just look right:
/// without it, `cargo clippy ... -- -D warnings --message-format=json` exits
/// 101 with "Unrecognized option"; with it the same command succeeds.
#[test]
fn test_json_format_keeps_the_strict_clippy_gate_executable() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join("src")).unwrap();
    std::fs::write(
        dir.path().join("Cargo.toml"),
        "[package]\nname = \"gate_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    let raw = vec![
        "cargo".to_string(),
        "clippy".to_string(),
        "--workspace".to_string(),
        "--all-targets".to_string(),
        "--".to_string(),
        "-D".to_string(),
        "warnings".to_string(),
    ];
    let rewritten = with_json_format(raw.clone());
    let run = |argv: &[String]| {
        std::process::Command::new(&argv[0])
            .args(&argv[1..])
            .current_dir(dir.path())
            .output()
            .expect("cargo available")
    };
    let broken = run(&[raw.clone(), vec!["--message-format=json".to_string()]].concat());
    assert!(
        !broken.status.success(),
        "precondition: the appended form must fail (it is the bug), stderr={}",
        String::from_utf8_lossy(&broken.stderr)
    );
    let fixed = run(&rewritten);
    assert!(
        fixed.status.success(),
        "the rewritten gate must run: stderr={}",
        String::from_utf8_lossy(&fixed.stderr)
    );
    assert!(
        String::from_utf8_lossy(&fixed.stdout).contains("build-finished"),
        "cargo must actually emit its JSON records"
    );
}
