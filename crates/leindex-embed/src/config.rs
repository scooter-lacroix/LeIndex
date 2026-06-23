// TOML configuration schema for LeIndex neural search settings
//
// This module defines the schema for `~/.leindex/config/leindex.toml` (or
// `$LEINDEX_HOME/config/leindex.toml`). The file is written by the
// `leindex setup` command and read by:
//   1. The ORT discovery chain (`ort_discovery::read_config_ort_path`)
//   2. The worker runtime (via `LeIndexConfig::load`)
//
// VAL-SETUP-023: Config written with correct schema
// VAL-SETUP-024: Idempotent re-runs produce equivalent config
// VAL-SETUP-029: Corrupted config recovered gracefully
// VAL-SETUP-030: Stale config migrated/overwritten
// VAL-SETUP-032: LEINDEX_HOME override honored

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Environment variable for the LeIndex home directory override.
pub const LEINDEX_HOME_ENV: &str = "LEINDEX_HOME";

/// Default model directory relative to LeIndex home.
const DEFAULT_MODEL_DIR_SUFFIX: &str = "models";

/// Default execution provider string.
const DEFAULT_EXECUTION_PROVIDER: &str = "auto";

/// Default search mode.
const DEFAULT_SEARCH_MODE: &str = "hybrid";

/// Default neural weight in hybrid search.
const DEFAULT_NEURAL_WEIGHT: f64 = 0.3;

/// Default batch size for indexing.
const DEFAULT_BATCH_SIZE: u64 = 500;

/// Default maximum number of files to index.
const DEFAULT_MAX_FILES: u64 = 50_000;

/// The complete LeIndex configuration, serialized as TOML.
///
/// VAL-SETUP-023: The schema includes [neural], [search], and [indexing] sections.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LeIndexConfig {
    /// Neural embedding configuration.
    #[serde(default)]
    pub neural: NeuralConfig,

    /// Search behavior configuration.
    #[serde(default)]
    pub search: SearchConfig,

    /// Indexing pipeline configuration.
    #[serde(default)]
    pub indexing: IndexingConfig,
}

/// Neural embeddings configuration (`[neural]` section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NeuralConfig {
    /// Whether neural embeddings are enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Execution provider: "cpu", "cuda", "migraphx", or "auto".
    #[serde(default = "default_execution_provider")]
    pub execution_provider: String,

    /// Path to the libonnxruntime shared library discovered during setup.
    /// The ORT discovery chain reads this as its second-priority source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ort_dylib_path: Option<String>,

    /// Installed ONNX Runtime version (e.g., "1.25.0"), recorded by setup.
    ///
    /// VAL-SETUP-020: Config records the ORT version so diagnostics and
    /// `--check` can report it without re-querying pip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ort_version: Option<String>,

    /// Directory containing model files (ONNX model, tokenizer, etc.).
    #[serde(default = "default_model_dir")]
    pub model_dir: String,
}

/// Search behavior configuration (`[search]` section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchConfig {
    /// Search mode: "hybrid", "text", or "neural".
    #[serde(default = "default_search_mode")]
    pub search_mode: String,

    /// Weight of the neural score in hybrid mode (0.0-1.0).
    #[serde(default = "default_neural_weight")]
    pub neural_weight: f64,
}

/// Indexing pipeline configuration (`[indexing]` section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexingConfig {
    /// Batch size for embedding generation during indexing.
    #[serde(default = "default_batch_size")]
    pub batch_size: u64,

    /// Maximum number of source files to index in a single run.
    #[serde(default = "default_max_files")]
    pub max_files: u64,
}

// ── Default implementations ──────────────────────────────────────────────

impl Default for NeuralConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            execution_provider: default_execution_provider(),
            ort_dylib_path: None,
            ort_version: None,
            model_dir: default_model_dir(),
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            search_mode: default_search_mode(),
            neural_weight: default_neural_weight(),
        }
    }
}

impl Default for IndexingConfig {
    fn default() -> Self {
        Self {
            batch_size: default_batch_size(),
            max_files: default_max_files(),
        }
    }
}

// ── Default value functions for serde ────────────────────────────────────

fn default_execution_provider() -> String {
    DEFAULT_EXECUTION_PROVIDER.to_string()
}

fn default_model_dir() -> String {
    // Resolve at call time so LEINDEX_HOME is honored.
    resolve_leindex_home()
        .map(|h| h.join(DEFAULT_MODEL_DIR_SUFFIX).display().to_string())
        .unwrap_or_else(|| format!("~/.leindex/{}", DEFAULT_MODEL_DIR_SUFFIX))
}

fn default_search_mode() -> String {
    DEFAULT_SEARCH_MODE.to_string()
}

fn default_neural_weight() -> f64 {
    DEFAULT_NEURAL_WEIGHT
}

fn default_batch_size() -> u64 {
    DEFAULT_BATCH_SIZE
}

fn default_max_files() -> u64 {
    DEFAULT_MAX_FILES
}

// ── Path resolution ──────────────────────────────────────────────────────

/// Resolve the LeIndex home directory.
///
/// VAL-SETUP-032: `$LEINDEX_HOME` takes precedence over `~/.leindex`.
pub fn resolve_leindex_home() -> Option<PathBuf> {
    if let Ok(custom) = std::env::var(LEINDEX_HOME_ENV) {
        let p = PathBuf::from(custom);
        if p.is_absolute() {
            return Some(p);
        }
    }
    dirs::home_dir().map(|h| h.join(".leindex"))
}

/// Get the path to the config file.
pub fn config_file_path() -> Option<PathBuf> {
    resolve_leindex_home().map(|h| h.join("config").join("leindex.toml"))
}

/// Get the path to the model directory.
pub fn model_dir_path() -> Option<PathBuf> {
    resolve_leindex_home().map(|h| h.join(DEFAULT_MODEL_DIR_SUFFIX))
}

// ── Config I/O ──────────────────────────────────────────────────────────

impl LeIndexConfig {
    /// Write the configuration to the TOML file.
    ///
    /// VAL-SETUP-023: Creates the config directory if it doesn't exist.
    /// VAL-SETUP-024: Overwrites existing config safely (idempotent).
    /// VAL-SETUP-032: Honors LEINDEX_HOME override.
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        let config_path = config_file_path().ok_or(ConfigError::NoHomeDir)?;

        // Create parent directory if it doesn't exist
        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Io(config_path.clone(), e.to_string()))?;
        }

        let toml_str =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;

        std::fs::write(&config_path, toml_str)
            .map_err(|e| ConfigError::Io(config_path.clone(), e.to_string()))?;

        tracing::debug!("config written to {}", config_path.display());
        Ok(config_path)
    }

    /// Read the configuration from the TOML file.
    ///
    /// Returns `Default` if the file doesn't exist.
    /// VAL-SETUP-029: Returns error on malformed TOML (caller decides recovery).
    /// VAL-SETUP-030: Merges defaults for missing keys.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from_path(&config_file_path().ok_or(ConfigError::NoHomeDir)?)
    }

    /// Load config from an explicit path (for testing).
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            tracing::debug!(
                "config file not found at {}, using defaults",
                path.display()
            );
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.to_path_buf(), e.to_string()))?;

        Self::parse_toml(&contents).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))
    }

    /// Parse TOML string into config.
    ///
    /// VAL-SETUP-029: Returns a clear error on malformed TOML.
    /// VAL-SETUP-030: Missing keys use serde defaults (migration).
    fn parse_toml(toml_str: &str) -> Result<Self, String> {
        toml::from_str(toml_str).map_err(|e| {
            let span = e.span().unwrap_or(0..0);
            let (line, column) = byte_offset_to_line_col(toml_str, span.start);
            format!(
                "Failed to parse leindex.toml:\n  {}\n  Line {}, column {}",
                e.message(),
                line,
                column
            )
        })
    }

    /// Try to load config, but on parse failure, back up the corrupt file
    /// and return a fresh default.
    ///
    /// VAL-SETUP-029: "backs up and overwrites with a fresh config"
    pub fn load_or_recover() -> Result<(Self, RecoveryAction), ConfigError> {
        let config_path = match config_file_path() {
            Some(p) => p,
            None => return Ok((Self::default(), RecoveryAction::CreatedDefault)),
        };

        if !config_path.exists() {
            return Ok((Self::default(), RecoveryAction::CreatedDefault));
        }

        let contents = match std::fs::read_to_string(&config_path) {
            Ok(c) => c,
            Err(e) => {
                return Err(ConfigError::Io(
                    config_path,
                    format!("Cannot read config file: {}", e),
                ));
            }
        };

        match Self::parse_toml(&contents) {
            Ok(config) => Ok((config, RecoveryAction::Loaded)),
            Err(parse_err) => {
                // VAL-SETUP-029: Back up the corrupt config before overwriting
                let backup_path = config_path.with_extension("toml.bak");
                match std::fs::rename(&config_path, &backup_path) {
                    Ok(()) => {
                        tracing::warn!(
                            "Config file was corrupted: {}. Backed up to {}",
                            parse_err,
                            backup_path.display()
                        );
                        Ok((
                            Self::default(),
                            RecoveryAction::RecoveredFromCorrupt(backup_path),
                        ))
                    }
                    Err(rename_err) => {
                        tracing::warn!(
                            "Config file was corrupted: {}; failed to back up {} to {}: {}",
                            parse_err,
                            config_path.display(),
                            backup_path.display(),
                            rename_err
                        );
                        std::fs::remove_file(&config_path).map_err(|remove_err| {
                            ConfigError::Io(
                                config_path.clone(),
                                format!(
                                    "Failed to back up corrupt config ({}) and failed to remove it ({})",
                                    rename_err, remove_err
                                ),
                            )
                        })?;
                        Ok((Self::default(), RecoveryAction::CreatedDefault))
                    }
                }
            }
        }
    }
}

fn byte_offset_to_line_col(input: &str, byte_offset: usize) -> (usize, usize) {
    let capped = byte_offset.min(input.len());
    let mut line = 1;
    let mut column = 1;

    for (idx, ch) in input.char_indices() {
        if idx >= capped {
            break;
        }
        if ch == '\n' {
            line += 1;
            column = 1;
        } else {
            column += 1;
        }
    }

    (line, column)
}

/// What happened during `load_or_recover()`.
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// Config loaded successfully from disk.
    Loaded,
    /// No config file existed; defaults returned.
    CreatedDefault,
    /// Config was corrupt; backed up and defaults returned.
    /// Contains the path to the backup file.
    RecoveredFromCorrupt(PathBuf),
}

/// Errors that can occur during config I/O.
#[derive(Debug, Clone)]
pub enum ConfigError {
    /// Cannot resolve the LeIndex home directory.
    NoHomeDir,
    /// I/O error reading/writing the config file.
    Io(PathBuf, String),
    /// TOML serialization error.
    Serialize(String),
    /// TOML parse error.
    Parse(PathBuf, String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NoHomeDir => {
                write!(f, "Cannot resolve LeIndex home directory. Set LEINDEX_HOME or ensure HOME is set.")
            }
            ConfigError::Io(path, msg) => {
                write!(f, "I/O error on {}: {}", path.display(), msg)
            }
            ConfigError::Serialize(msg) => {
                write!(f, "Failed to serialize config: {}", msg)
            }
            ConfigError::Parse(path, msg) => {
                write!(f, "Failed to parse {}: {}", path.display(), msg)
            }
        }
    }
}

impl std::error::Error for ConfigError {}

#[cfg(test)]
mod tests {
    use super::*;

    // Use the crate-level shared lock so env-mutating tests serialize across modules.
    use crate::test_util::ENV_TEST_LOCK;

    #[test]
    fn test_default_config_serializes_round_trip() {
        let config = LeIndexConfig::default();
        let toml_str = toml::to_string(&config).unwrap();
        let parsed: LeIndexConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_neural_config_schema() {
        let config = NeuralConfig {
            enabled: true,
            execution_provider: "cpu".to_string(),
            ort_dylib_path: Some("/usr/local/lib/libonnxruntime.so".to_string()),
            ort_version: Some("1.25.0".to_string()),
            model_dir: "/home/user/.leindex/models".to_string(),
        };

        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("enabled = true"));
        assert!(toml_str.contains("execution_provider = \"cpu\""));
        assert!(toml_str.contains("ort_dylib_path"));
        assert!(toml_str.contains("model_dir"));
    }

    #[test]
    fn test_config_missing_keys_uses_defaults() {
        // VAL-SETUP-030: stale config from older version gets defaults for new keys
        let toml_str = "[neural]\nenabled = true\n";
        let config: LeIndexConfig = toml::from_str(toml_str).unwrap();
        assert!(config.neural.enabled);
        // Defaults should fill in missing keys
        assert_eq!(config.search.search_mode, "hybrid");
        assert_eq!(config.indexing.batch_size, 500);
    }

    #[test]
    fn test_config_empty_uses_defaults() {
        let config: LeIndexConfig = toml::from_str("").unwrap();
        assert!(!config.neural.enabled);
        assert_eq!(config.search.search_mode, "hybrid");
    }

    #[test]
    fn test_parse_malformed_toml_returns_error() {
        // VAL-SETUP-029: corrupted config detected
        let bad_toml = "[neural\nenabled = true\n"; // Missing closing bracket
        let result = LeIndexConfig::parse_toml(bad_toml);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("Failed to parse"));
    }

    #[test]
    fn test_parse_malformed_toml_reports_line_and_column() {
        let bad_toml = "[neural]\nenabled = true\nexecution_provider = @\n";
        let result = LeIndexConfig::parse_toml(bad_toml);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(
            err.contains("Line 3, column 22"),
            "expected real line/column in error, got: {}",
            err
        );
    }

    #[test]
    fn test_full_config_round_trip() {
        let config = LeIndexConfig {
            neural: NeuralConfig {
                enabled: true,
                execution_provider: "migraphx".to_string(),
                ort_dylib_path: Some("/usr/local/lib/libonnxruntime.so.1.25.0".to_string()),
                ort_version: Some("1.25.0".to_string()),
                model_dir: "/home/user/.leindex/models".to_string(),
            },
            search: SearchConfig {
                search_mode: "hybrid".to_string(),
                neural_weight: 0.35,
            },
            indexing: IndexingConfig {
                batch_size: 1000,
                max_files: 100_000,
            },
        };

        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: LeIndexConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_load_or_recover_corrupt_file() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config").join("leindex.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "[neural\nbroken toml").unwrap();

        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        let (config, action) = LeIndexConfig::load_or_recover().unwrap();

        assert!(matches!(action, RecoveryAction::RecoveredFromCorrupt(_)));
        assert!(!config.neural.enabled); // Default

        // Backup file should exist
        let backup = config_path.with_extension("toml.bak");
        assert!(backup.exists());

        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_config_load_returns_default_when_missing() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, "/nonexistent/path/for/testing");
        let (config, action) = LeIndexConfig::load_or_recover().unwrap();
        assert!(matches!(action, RecoveryAction::CreatedDefault));
        assert!(!config.neural.enabled);
        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_ort_dylib_path_skip_serializing_if_none() {
        let config = NeuralConfig {
            enabled: true,
            execution_provider: "cpu".to_string(),
            ort_dylib_path: None,
            ort_version: None,
            model_dir: "/models".to_string(),
        };
        let toml_str = toml::to_string(&config).unwrap();
        assert!(!toml_str.contains("ort_dylib_path"));
    }

    #[test]
    fn test_resolve_leindex_home_env_override() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, "/custom/leindex");
        assert_eq!(
            resolve_leindex_home(),
            Some(PathBuf::from("/custom/leindex"))
        );
        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_resolve_leindex_home_relative_ignored() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, "relative/path");
        // Should fall back to home dir, not use the relative path
        assert!(resolve_leindex_home().is_some());
        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_config_file_path() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, "/tmp/testhome");
        assert_eq!(
            config_file_path(),
            Some(PathBuf::from("/tmp/testhome/config/leindex.toml"))
        );
        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_save_and_load_round_trip() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        let config = LeIndexConfig {
            neural: NeuralConfig {
                enabled: true,
                execution_provider: "cpu".to_string(),
                ort_dylib_path: Some("/usr/lib/libonnxruntime.so".to_string()),
                ort_version: Some("1.25.0".to_string()),
                model_dir: tmp.path().join("models").display().to_string(),
            },
            ..Default::default()
        };

        config.save().unwrap();
        let loaded = LeIndexConfig::load().unwrap();

        assert_eq!(config, loaded);

        std::env::remove_var(LEINDEX_HOME_ENV);
    }

    #[test]
    fn test_idempotent_save() {
        // VAL-SETUP-024: re-running save produces identical config
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var(LEINDEX_HOME_ENV, tmp.path());

        let config = LeIndexConfig {
            neural: NeuralConfig {
                enabled: true,
                execution_provider: "cpu".to_string(),
                ort_dylib_path: Some("/usr/lib/libonnxruntime.so".to_string()),
                ort_version: Some("1.25.0".to_string()),
                model_dir: "/models".to_string(),
            },
            ..Default::default()
        };

        // First save
        config.save().unwrap();
        let first = LeIndexConfig::load().unwrap();

        // Second save (idempotent)
        config.save().unwrap();
        let second = LeIndexConfig::load().unwrap();

        assert_eq!(first, second);

        std::env::remove_var(LEINDEX_HOME_ENV);
    }
}
