#![cfg(feature = "cli")]

use std::env;
use std::fs;
use std::process::Command;

use leindex::cli::LeIndex;
use tempfile::TempDir;

const HELPER_TEST_NAME: &str = "helper_index_deep_nested_project";
const HELPER_ENV: &str = "LEINDEX_STACK_HELPER";
const PROJECT_ENV: &str = "LEINDEX_STACK_PROJECT";

fn write_deep_rust_file(temp_dir: &TempDir, depth: usize) {
    let mut source = String::from("fn main() {\n");
    let mut indent = String::from("    ");

    for _ in 0..depth {
        source.push_str(&format!("{indent}if true {{\n"));
        indent.push_str("    ");
    }

    source.push_str(&format!("{indent}let _x = 1;\n"));

    for _ in 0..depth {
        indent.truncate(indent.len() - 4);
        source.push_str(&format!("{indent}}}\n"));
    }

    source.push_str("}\n");

    fs::write(temp_dir.path().join("deep.rs"), source).unwrap();
}

fn write_deep_cpp_file(temp_dir: &TempDir, depth: usize) {
    let mut source = String::from("int main() {\n");
    let mut indent = String::from("    ");

    for _ in 0..depth {
        source.push_str(&format!("{indent}if (true) {{\n"));
        indent.push_str("    ");
    }

    source.push_str(&format!("{indent}return 0;\n"));

    for _ in 0..depth {
        indent.truncate(indent.len() - 4);
        source.push_str(&format!("{indent}}}\n"));
    }

    source.push_str("}\n");

    fs::write(temp_dir.path().join("deep.cpp"), source).unwrap();
}

#[test]
fn helper_index_deep_nested_project() {
    if env::var_os(HELPER_ENV).is_none() {
        return;
    }

    let project_path = env::var(PROJECT_ENV).unwrap();
    let mut index = LeIndex::new(&project_path).unwrap();
    let stats = index.index_project(true).unwrap();

    assert_eq!(stats.failed_parses, 0);
    assert_eq!(stats.successful_parses, 1);
}

#[test]
fn test_index_deep_nested_rust_project_does_not_overflow_stack() {
    let temp_dir = TempDir::new().unwrap();
    write_deep_rust_file(&temp_dir, 6000);

    let output = Command::new(env::current_exe().unwrap())
        .env(HELPER_ENV, "1")
        .env(PROJECT_ENV, temp_dir.path())
        .arg("--exact")
        .arg(HELPER_TEST_NAME)
        .arg("--nocapture")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "child process failed\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn test_index_deep_nested_cpp_project_does_not_overflow_stack() {
    let temp_dir = TempDir::new().unwrap();
    write_deep_cpp_file(&temp_dir, 6000);

    let output = Command::new(env::current_exe().unwrap())
        .env(HELPER_ENV, "1")
        .env(PROJECT_ENV, temp_dir.path())
        .arg("--exact")
        .arg(HELPER_TEST_NAME)
        .arg("--nocapture")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "child process failed\nstatus: {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
