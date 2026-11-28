//! Format-fidelity tests for the NATIVE arm emulations (assignment: "unit
//! tests for the native-arm emulations (format fidelity)").

use super::*;
use std::path::Path;

/// Write `contents` to a one-file tempdir and return its dir + file path.
fn temp_file(contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("sample.rs");
    std::fs::write(&path, contents).expect("write sample");
    (dir, path)
}

#[test]
fn test_read_lines_renders_cat_n_format() {
    let (_dir, path) = temp_file("alpha\nbeta\n");
    let out = read_lines(&path, None, None).expect("read");
    // Claude Code Read: 6-wide right-aligned number, TAB, line, newline.
    assert_eq!(out, "     1\talpha\n     2\tbeta\n");
}

#[test]
fn test_read_lines_offset_and_limit_bounds() {
    let (_dir, path) = temp_file("one\ntwo\nthree\n");
    let out = read_lines(&path, Some(2), Some(1)).expect("read");
    // 1-based offset: starts at line 2, limit 1 line.
    assert_eq!(out, "     2\ttwo\n");
}

#[test]
fn test_read_lines_default_limit_is_2000() {
    let body = (0..2001)
        .map(|i| format!("line{i}"))
        .collect::<Vec<_>>()
        .join("\n");
    let (_dir, path) = temp_file(&body);
    let out = read_lines(&path, None, None).expect("read");
    assert_eq!(out.lines().count(), 2000, "default limit is 2000 lines");
    assert!(out.contains("     1\tline0"));
    assert!(out.contains("  2000\tline1999"));
    assert!(!out.contains("line2000"), "line 2001 must be cut");
}

#[test]
fn test_edit_message_mirrors_claude_code_shape() {
    let msg = edit_message();
    assert!(msg.contains("has been edited"), "got: {msg}");
}

/// A developer's ripgrep configuration must not leak into the benchmark:
/// `RIPGREP_CONFIG_PATH` with `--max-count=1` would otherwise cap every
/// helper at one match per file and fail tasks for reasons unrelated to the
/// fixture.
#[test]
fn test_grep_ignores_ripgrep_config_path() {
    let (dir, path) = temp_file("needle\nneedle\nneedle\n");
    let config = dir.path().join("rgrc");
    std::fs::write(&config, "--max-count=1\n").unwrap();
    let Some(mut command) = rg_command() else {
        eprintln!("skipping: rg not on PATH");
        return;
    };
    // Run through the shared constructor exactly as the helpers do, with the
    // hostile configuration active for the child.
    command
        .env("RIPGREP_CONFIG_PATH", &config)
        .arg("-c")
        .arg("needle")
        .arg(&path);
    let out = command.output().expect("rg runs");
    let text = String::from_utf8_lossy(&out.stdout);
    let count = |text: &str| -> usize {
        text.rsplit(':')
            .next()
            .and_then(|n| n.trim().parse().ok())
            .unwrap_or(0)
    };
    assert_eq!(
        count(&text),
        3,
        "rg config leaked into the benchmark: {text}"
    );

    // Control: the same configuration DOES cap a plain ripgrep invocation,
    // so the assertion above is proof of `--no-config`, not of an inert file.
    let rg = {
        let mut found = None;
        if let Some(paths) = std::env::var_os("PATH") {
            for dir in std::env::split_paths(&paths) {
                let candidate = dir.join("rg");
                if is_executable(&candidate) {
                    found = Some(candidate);
                    break;
                }
            }
        }
        found.expect("rg located above")
    };
    let control = std::process::Command::new(rg)
        .env("RIPGREP_CONFIG_PATH", &config)
        .arg("-c")
        .arg("needle")
        .arg(&path)
        .output()
        .expect("rg runs");
    let control_text = String::from_utf8_lossy(&control.stdout);
    assert_eq!(
        count(&control_text),
        1,
        "control: RIPGREP_CONFIG_PATH should cap a plain rg at one match: {control_text}"
    );
}

#[test]
fn test_git_captures_real_cli_output() {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = dir.path();
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(root)
            .output()
            .expect("git spawn")
    };
    for args in [
        ["init"].as_slice(),
        // Same fixture-local isolation as `corpus::materialize`: never
        // require the developer's signing key for a throwaway commit.
        &["config", "commit.gpgSign", "false"][..],
        &["config", "user.email", "t@example.test"][..],
        &["config", "user.name", "t"][..],
        ["add", "."].as_slice(),
        ["commit", "--allow-empty", "-m", "tokbench fidelity"].as_slice(),
    ] {
        let out = run(args);
        assert!(out.status.success(), "git {args:?} setup failed");
    }
    // Real invocation through the emulation: verbatim stdout.
    let log = git(&["log", "--oneline", "-1"], Path::new(root)).expect("git log");
    assert!(log.contains("tokbench fidelity"), "got: {log}");
    // Failure path: stderr is surfaced, never swallowed.
    let missing = git(&["show", "--stat", "deadbeef"], Path::new(root)).expect("git runs");
    assert!(
        missing.contains("unknown revision") || missing.contains("fatal"),
        "got: {missing}"
    );
}
