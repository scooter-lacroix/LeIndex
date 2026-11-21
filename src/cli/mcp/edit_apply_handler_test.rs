use super::*;
use crate::cli::mcp::helpers::test_registry_for;
use tempfile::tempdir;

/// A minimal one-file cargo package that edits cleanly but FAILS `cargo
/// check` when the edit introduces a type error. `cargo check -p` on a
/// dependency-free crate works fully offline.
fn scoped_probe() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"scoped_apply_probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::create_dir_all(root.join("src")).unwrap();
    let lib = root.join("src/lib.rs");
    std::fs::write(&lib, "pub fn f() -> u32 {\n    42\n}\n").unwrap();
    (dir, lib)
}

/// Cross-lane contract B→F: `validate:\"check\"` must run lane F's scoped
/// check after a successful apply and surface the diagnostics in the apply
/// echo. Fails if unwired (no `check` key in the response).
#[tokio::test]
async fn test_edit_apply_validate_check_surfaces_scoped_diagnostics() {
    let (dir, lib) = scoped_probe();
    let args = serde_json::json!({
        "file_path": lib.to_string_lossy(),
        "project_path": dir.path().to_string_lossy(),
        "old_text": "42",
        "new_text": "\"not a number\"",
        "validate": "check",
    });
    let response = EditApplyHandler
        .execute(&test_registry_for(dir.path()), args)
        .await
        .unwrap();
    let check = response["check"]
        .as_object()
        .expect("validate:\"check\" must attach a check section to the apply echo");
    let files = check["files"].as_array().expect("per-file check results");
    assert_eq!(files.len(), 1, "{response}");
    let file_check = &files[0];
    assert_ne!(file_check["exit_code"].as_i64(), Some(0), "{response}");
    assert!(
        file_check["error_count"].as_u64().unwrap_or(0) >= 1,
        "{response}"
    );
    assert!(
        file_check["headline"].as_str().unwrap().contains("error"),
        "{response}"
    );
}

/// The write is durable before the check runs: a type-error edit still lands
/// on disk, and the check echo reports it instead of rejecting the apply.
#[tokio::test]
async fn test_edit_apply_validate_check_write_is_durable_and_echo_reports() {
    let (dir, lib) = scoped_probe();
    let args = serde_json::json!({
        "file_path": lib.to_string_lossy(),
        "project_path": dir.path().to_string_lossy(),
        "old_text": "42",
        "new_text": "\"not a number\"",
        "validate": "check",
    });
    let response = EditApplyHandler
        .execute(&test_registry_for(dir.path()), args)
        .await
        .unwrap();
    assert_eq!(response["success"], true, "{response}");
    assert_eq!(response["changes_applied"], 1, "{response}");
    let on_disk = std::fs::read_to_string(&lib).unwrap();
    assert!(
        on_disk.contains("\"not a number\""),
        "the edit must be durable even when the check flags it: {on_disk}"
    );
}
