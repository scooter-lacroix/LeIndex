// Model path resolution for the worker process
//
// VAL-CPHASE-010: Worker model resolution uses the documented precedence:
// 1. Explicit env override (LEINDEX_MODEL_PATH)
// 2. Bundled models near the binary
// 3. User cache fallback (~/.leindex/models/)
//
// The resolver is used by the worker runtime to locate model and tokenizer
// files without requiring the main daemon to pass paths explicitly.

use std::path::{Path, PathBuf};

/// Error during model path resolution.
#[derive(Debug, Clone)]
pub struct ModelResolutionError {
    pub message: String,
}

impl std::fmt::Display for ModelResolutionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "model resolution error: {}", self.message)
    }
}

impl std::error::Error for ModelResolutionError {}

/// Resolves model file paths using the documented precedence chain.
pub struct ModelResolver;

impl ModelResolver {
    /// Resolve the ONNX model file path for the given model name.
    ///
    /// VAL-CPHASE-010: Uses the precedence chain:
    /// 1. LEINDEX_MODEL_PATH env override
    /// 2. Bundled models directory (relative to the worker binary)
    /// 3. User cache directory (~/.leindex/models/)
    pub fn resolve(model_name: &str) -> Result<PathBuf, ModelResolutionError> {
        let model_filename = format!("{}.onnx", model_name);

        // 1. Explicit env override
        if let Ok(path) = std::env::var("LEINDEX_MODEL_PATH") {
            let model_path = PathBuf::from(path).join(&model_filename);
            if model_path.exists() {
                tracing::debug!("model resolved via env override: {}", model_path.display());
                return Ok(model_path);
            }
        }

        // 2. Bundled models (relative to the running binary)
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent() {
                let bundled_dir = parent.join("models");
                let model_path = bundled_dir.join(&model_filename);
                if model_path.exists() {
                    tracing::debug!("model resolved via bundled path: {}", model_path.display());
                    return Ok(model_path);
                }
            }
        }

        // 3. User cache fallback
        if let Some(home) = dirs::home_dir() {
            let user_models = home.join(".leindex").join("models");
            let model_path = user_models.join(&model_filename);
            if model_path.exists() {
                tracing::debug!("model resolved via user cache: {}", model_path.display());
                return Ok(model_path);
            }
        }

        Err(ModelResolutionError {
            message: format!(
                "model '{}' not found in any standard location (env, bundled, user cache)",
                model_name
            ),
        })
    }

    /// Resolve the tokenizer file path for the given model name.
    ///
    /// Uses the same precedence chain as model resolution.
    pub fn resolve_tokenizer(model_name: &str) -> Result<PathBuf, ModelResolutionError> {
        // Tokenizer is typically shared across model variants
        let _ = model_name; // Model name may be used for variant-specific tokenizers in future

        // 1. Explicit env override
        if let Ok(path) = std::env::var("LEINDEX_MODEL_PATH") {
            let tokenizer_path = PathBuf::from(path).join("tokenizer.json");
            if tokenizer_path.exists() {
                return Ok(tokenizer_path);
            }
        }

        // 2. Bundled models
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent() {
                let bundled_dir = parent.join("models");
                let tokenizer_path = bundled_dir.join("tokenizer.json");
                if tokenizer_path.exists() {
                    return Ok(tokenizer_path);
                }
            }
        }

        // 3. User cache fallback
        if let Some(home) = dirs::home_dir() {
            let user_models = home.join(".leindex").join("models");
            let tokenizer_path = user_models.join("tokenizer.json");
            if tokenizer_path.exists() {
                return Ok(tokenizer_path);
            }
        }

        Err(ModelResolutionError {
            message: "tokenizer not found in any standard location".to_string(),
        })
    }

    /// Determine the source of a resolved path for reporting.
    ///
    /// Returns one of: "env_override", "bundled", "user_cache".
    pub fn source_for_path(path: &Path) -> &'static str {
        // Check env override first — if the env var is set and the path is
        // rooted under it, report as env_override regardless of whether the
        // file also happens to live near the binary.
        if let Ok(env_path) = std::env::var("LEINDEX_MODEL_PATH") {
            let env_dir = PathBuf::from(env_path);
            if path.starts_with(&env_dir) {
                return "env_override";
            }
        }

        // Check if it's near the binary
        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(parent) = exe_path.parent() {
                if path.starts_with(parent) {
                    return "bundled";
                }
            }
        }

        "user_cache"
    }
}

/// All tests in this module mutate the `LEINDEX_MODEL_PATH` env var and must
/// not run concurrently.  We use a single test-serialising attribute so that
/// `cargo test -- --test-threads=N` still works correctly.
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Global lock serialising env-var mutation across all model_path tests.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_resolve_model_not_found() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Clear any env override
        std::env::remove_var("LEINDEX_MODEL_PATH");

        let result = ModelResolver::resolve("nonexistent-model-xyz");
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.message.contains("nonexistent-model-xyz"));
        assert!(err.message.contains("not found"));
    }

    #[test]
    fn test_resolve_tokenizer_not_found() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("LEINDEX_MODEL_PATH");

        let result = ModelResolver::resolve_tokenizer("nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().message.contains("tokenizer not found"));
    }

    #[test]
    fn test_resolve_with_env_override_missing_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Set env to a temp dir that doesn't have the model
        let temp_dir = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_MODEL_PATH", temp_dir.path());

        let result = ModelResolver::resolve("test-model");
        // Should still fail because the file doesn't exist in the temp dir
        assert!(result.is_err());

        std::env::remove_var("LEINDEX_MODEL_PATH");
    }

    #[test]
    fn test_resolve_with_env_override_existing_file() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let model_file = temp_dir.path().join("test-model.onnx");
        std::fs::write(&model_file, b"fake model").unwrap();

        std::env::set_var("LEINDEX_MODEL_PATH", temp_dir.path());

        let result = ModelResolver::resolve("test-model");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), model_file);

        std::env::remove_var("LEINDEX_MODEL_PATH");
    }

    #[test]
    fn test_resolve_tokenizer_with_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        let tokenizer_file = temp_dir.path().join("tokenizer.json");
        std::fs::write(&tokenizer_file, b"{}").unwrap();

        std::env::set_var("LEINDEX_MODEL_PATH", temp_dir.path());

        let result = ModelResolver::resolve_tokenizer("test");
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), tokenizer_file);

        std::env::remove_var("LEINDEX_MODEL_PATH");
    }

    #[test]
    fn test_source_for_path_env_override() {
        let _guard = ENV_LOCK.lock().unwrap();
        let temp_dir = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_MODEL_PATH", temp_dir.path());

        let path = temp_dir.path().join("model.onnx");
        assert_eq!(ModelResolver::source_for_path(&path), "env_override");

        std::env::remove_var("LEINDEX_MODEL_PATH");
    }

    #[test]
    fn test_source_for_path_user_cache() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::remove_var("LEINDEX_MODEL_PATH");
        let path = PathBuf::from("/some/random/path/model.onnx");
        assert_eq!(ModelResolver::source_for_path(&path), "user_cache");
    }

    #[test]
    fn test_model_resolution_error_display() {
        let err = ModelResolutionError {
            message: "test error".to_string(),
        };
        assert!(err.to_string().contains("test error"));
    }
}
