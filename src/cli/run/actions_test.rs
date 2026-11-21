//! Tests for the `run` / `how_to_test` manage actions.

use super::*;

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    (dir, root)
}

#[tokio::test]
async fn test_run_action_executes_argv_and_returns_handle() {
    let (_guard, root) = project();
    let out = run_action(
        &root,
        &json!({"argv": ["cargo", "--version"], "name": "ver"}),
    )
    .await
    .unwrap();
    assert_eq!(out["exit_code"], 0);
    assert_eq!(out["log"], "log://ver");
    assert!(out["summary"].as_str().unwrap().contains("ver.log"));

    let read = run_action(&root, &json!({"log": "log://ver"}))
        .await
        .unwrap();
    assert!(read["text"].as_str().unwrap().starts_with("cargo "));
    assert!(read["note"].is_null());
}

#[tokio::test]
async fn test_run_action_rejects_bad_commands_and_argv() {
    let (_guard, root) = project();
    let denied = run_action(&root, &json!({"argv": ["rm", "-rf", "x"]}))
        .await
        .unwrap_err();
    assert!(denied.contains("not allowed"), "{denied}");
    let shell = run_action(&root, &json!({"command": "cargo; rm x"}))
        .await
        .unwrap_err();
    assert!(shell.contains("metacharacters"), "{shell}");
    let shape = run_action(&root, &json!({"argv": ["cargo", 1]}))
        .await
        .unwrap_err();
    assert!(shape.contains("array of strings"), "{shape}");
    let none = run_action(&root, &json!({})).await.unwrap_err();
    assert!(none.contains("no command"), "{none}");
}

#[tokio::test]
async fn test_log_read_announces_omitted_lines() {
    let (_guard, root) = project();
    let storage = diagnostics::storage_for(&root);
    let log = run_log::prepare(&storage, Some("long")).unwrap();
    let body: String = (1..=300).map(|n| format!("line {n}\n")).collect();
    std::fs::write(&log.path, body).unwrap();

    let out = run_action(&root, &json!({"log": "log://long", "tail": 50}))
        .await
        .unwrap();
    assert_eq!(out["lines_total"], 300);
    assert_eq!(out["lines_shown"], "251-300");
    assert!(
        out["note"]
            .as_str()
            .unwrap()
            .contains("250 earlier lines omitted")
    );
    assert!(out["text"].as_str().unwrap().starts_with("line 251"));
    let bad = run_action(&root, &json!({"log": "log://../etc/passwd"}))
        .await
        .unwrap_err();
    assert!(bad.contains("invalid log handle"), "{bad}");
}

#[tokio::test]
async fn test_diagnostics_mode_reads_the_store_and_explains_empty() {
    let (_guard, root) = project();
    let empty = run_action(&root, &json!({"diagnostics": "src/lib.rs"}))
        .await
        .unwrap();
    assert_eq!(empty["count"], 0);
    assert!(
        empty["note"]
            .as_str()
            .unwrap()
            .contains("run action=run check")
    );

    let storage = diagnostics::storage_for(&root);
    diagnostics::record(
        &storage,
        &[diagnostics::Diagnostic {
            file: root.join("src/lib.rs"),
            line: 7,
            severity: diagnostics::Severity::Warning,
            code: Some("dead_code".to_string()),
            message: "never used".to_string(),
        }],
    )
    .unwrap();
    let hit = run_action(
        &root,
        &json!({"diagnostics": "src/lib.rs", "from": 1, "to": 10}),
    )
    .await
    .unwrap();
    assert_eq!(hit["count"], 1);
    assert_eq!(hit["diagnostics"][0]["code"], "dead_code");
    assert_eq!(hit["diagnostics"][0]["severity"], "warning");
    assert!(hit["note"].is_null());
}

#[tokio::test]
async fn test_check_mode_reports_unsupported_files() {
    let (_guard, root) = project();
    write(&root.join("notes/a.txt"), "x");
    let error = run_action(&root, &json!({"check": "notes/a.txt"}))
        .await
        .unwrap_err();
    assert!(error.contains("unsupported"), "{error}");
}

#[test]
fn test_how_to_test_action_shapes_the_answer() {
    let (_guard, root) = project();
    write(&root.join("justfile"), "test:\n    echo\n");
    let out = how_to_test_action(&root, &json!({"path": "a.c"})).unwrap();
    assert_eq!(out["command"], "just test");
    assert_eq!(out["cwd"], ".");
    let none = how_to_test_action(&project().1, &json!({})).unwrap_err();
    assert!(none.contains("no test command found"), "{none}");
}
