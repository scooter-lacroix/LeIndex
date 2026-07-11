// Model resolution contract tests.
//
// Published bundles never contain model assets. Setup-managed models resolve
// from LEINDEX_MODEL_PATH or the user model directory, and missing assets fail
// with actionable errors.

use std::fs;
use std::sync::Mutex;

use leindex_embed::ModelResolver;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn model_resolver_fails_on_missing_model() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    std::env::remove_var("LEINDEX_MODEL_PATH");

    let result = ModelResolver::resolve("nonexistent-model-xyz-abc");
    let error = result.expect_err("missing model must fail");
    assert!(error.message.contains("not found"));
    assert!(error.message.contains("nonexistent-model-xyz-abc"));
}

#[test]
fn missing_env_override_falls_through() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let temp_dir = tempfile::tempdir().unwrap();
    std::env::set_var(
        "LEINDEX_MODEL_PATH",
        temp_dir.path().join("missing").to_str().unwrap(),
    );

    let result = ModelResolver::resolve("nonexistent-env-fallthrough-model");
    assert!(result.is_err());

    std::env::remove_var("LEINDEX_MODEL_PATH");
}

#[test]
fn setup_managed_directory_resolves_model_and_tokenizer() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let model_dir = tempfile::tempdir().unwrap();
    let model_path = model_dir.path().join("qwen3-embed-0.6b-dynamic.onnx");
    let tokenizer_path = model_dir.path().join("tokenizer.json");
    fs::write(&model_path, b"test model").unwrap();
    fs::write(&tokenizer_path, b"{}").unwrap();

    std::env::set_var("LEINDEX_MODEL_PATH", model_dir.path());
    assert_eq!(
        ModelResolver::resolve("qwen3-embed-0.6b-dynamic").unwrap(),
        model_path
    );
    assert_eq!(
        ModelResolver::resolve_tokenizer("qwen3-embed-0.6b-dynamic").unwrap(),
        tokenizer_path
    );

    std::env::remove_var("LEINDEX_MODEL_PATH");
}

#[test]
fn explicit_model_override_has_highest_precedence() {
    let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let explicit_dir = tempfile::tempdir().unwrap();
    let explicit_model = explicit_dir.path().join("precedence-test.onnx");
    fs::write(&explicit_model, b"explicit model").unwrap();

    std::env::set_var("LEINDEX_MODEL_PATH", explicit_dir.path());
    let resolved = ModelResolver::resolve("precedence-test").unwrap();
    assert_eq!(resolved, explicit_model);
    assert_eq!(
        ModelResolver::source_for_path(&resolved),
        "env_override",
        "explicit setup/development override should be reported"
    );

    std::env::remove_var("LEINDEX_MODEL_PATH");
}
