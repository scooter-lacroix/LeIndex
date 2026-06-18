// Neural search configuration schema for ~/.leindex/config/leindex.toml
//
// This module defines the TOML schema for the user-level neural search
// configuration written by the `leindex setup` command. It mirrors the
// schema in `crates/leindex-embed/src/config.rs` for cross-crate consistency.
//
// VAL-SETUP-023: Config written with correct schema
// VAL-SETUP-024: Idempotent re-runs
// VAL-SETUP-029: Corrupted config recovered gracefully
// VAL-SETUP-030: Stale config migrated/overwritten
// VAL-SETUP-032: LEINDEX_HOME override honored

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Environment variable for the LeIndex home directory override.
pub const LEINDEX_HOME_ENV: &str = "LEINDEX_HOME";

/// Default model directory relative to LeIndex home.
const DEFAULT_MODEL_DIR_SUFFIX: &str = "models";

/// The complete LeIndex neural search configuration.
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

/// Neural embeddings configuration ([neural] section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NeuralConfig {
    /// Whether neural embeddings are enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Execution provider: "cpu", "cuda", "migraphx", or "auto".
    #[serde(default = "default_execution_provider")]
    pub execution_provider: String,

    /// Path to libonnxruntime shared library.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ort_dylib_path: Option<String>,

    /// Directory containing model files.
    #[serde(default = "default_model_dir")]
    pub model_dir: String,
}

/// Search behavior configuration ([search] section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SearchConfig {
    /// Search mode: "hybrid", "text", or "neural".
    #[serde(default = "default_search_mode")]
    pub search_mode: String,

    /// Neural score weight in hybrid mode (0.0-1.0).
    #[serde(default = "default_neural_weight")]
    pub neural_weight: f64,
}

/// Indexing pipeline configuration ([indexing] section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexingConfig {
    /// Batch size for embedding generation.
    #[serde(default = "default_batch_size")]
    pub batch_size: u64,

    /// Maximum number of files to index.
    #[serde(default = "default_max_files")]
    pub max_files: u64,
}

// ── Defaults ─────────────────────────────────────────────────────────────

impl Default for NeuralConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            execution_provider: default_execution_provider(),
            ort_dylib_path: None,
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

fn default_execution_provider() -> String {
    "auto".to_string()
}

fn default_model_dir() -> String {
    resolve_leindex_home()
        .map(|h| h.join(DEFAULT_MODEL_DIR_SUFFIX).display().to_string())
        .unwrap_or_else(|| format!("~/.leindex/{}", DEFAULT_MODEL_DIR_SUFFIX))
}

fn default_search_mode() -> String {
    "hybrid".to_string()
}

fn default_neural_weight() -> f64 {
    0.3
}

fn default_batch_size() -> u64 {
    500
}

fn default_max_files() -> u64 {
    50_000
}

// ── Path resolution ──────────────────────────────────────────────────────

/// Resolve the LeIndex home directory.
///
/// VAL-SETUP-032: $LEINDEX_HOME takes precedence over ~/.leindex.
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
    /// Write config to TOML file.
    ///
    /// VAL-SETUP-023: Creates config directory if missing.
    /// VAL-SETUP-024: Overwrites safely (idempotent).
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        let config_path = config_file_path().ok_or(ConfigError::NoHomeDir)?;

        if let Some(parent) = config_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ConfigError::Io(config_path.clone(), e.to_string()))?;
        }

        let toml_str =
            toml::to_string_pretty(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;

        std::fs::write(&config_path, toml_str)
            .map_err(|e| ConfigError::Io(config_path.clone(), e.to_string()))?;

        Ok(config_path)
    }

    /// Read config from TOML file. Returns Default if not present.
    pub fn load() -> Result<Self, ConfigError> {
        Self::load_from_path(&config_file_path().ok_or(ConfigError::NoHomeDir)?)
    }

    /// Load from explicit path.
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.to_path_buf(), e.to_string()))?;

        Self::parse_toml(&contents).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))
    }

    fn parse_toml(toml_str: &str) -> Result<Self, String> {
        toml::from_str(toml_str).map_err(|e| format!("Failed to parse leindex.toml: {}", e))
    }

    /// Load or recover from corruption.
    ///
    /// VAL-SETUP-029: Backs up corrupt config and returns defaults.
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
                let backup_path = config_path.with_extension("toml.bak");
                let _ = std::fs::rename(&config_path, &backup_path);
                tracing::warn!(
                    "Config corrupted: {}. Backed up to {}",
                    parse_err,
                    backup_path.display()
                );
                Ok((
                    Self::default(),
                    RecoveryAction::RecoveredFromCorrupt(backup_path),
                ))
            }
        }
    }
}

/// Config recovery action during load_or_recover.
#[derive(Debug, Clone)]
pub enum RecoveryAction {
    /// Config loaded successfully.
    Loaded,
    /// No config file existed.
    CreatedDefault,
    /// Config was corrupt; backed up. Contains backup path.
    RecoveredFromCorrupt(PathBuf),
}

/// Config I/O errors.
#[derive(Debug, Clone)]
pub enum ConfigError {
    /// Cannot resolve home directory.
    NoHomeDir,
    /// I/O error.
    Io(PathBuf, String),
    /// Serialization error.
    Serialize(String),
    /// Parse error.
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

/// Alias for use in setup.rs as `crate::config_schema`.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_round_trip() {
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
            model_dir: "/home/user/.leindex/models".to_string(),
        };

        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("enabled = true"));
        assert!(toml_str.contains("execution_provider = \"cpu\""));
        assert!(toml_str.contains("ort_dylib_path"));
        assert!(toml_str.contains("model_dir"));
    }

    #[test]
    fn test_parse_malformed_returns_error() {
        let bad_toml = "[neural\nenabled = true\n";
        let result = LeIndexConfig::parse_toml(bad_toml);
        assert!(result.is_err());
    }
}
