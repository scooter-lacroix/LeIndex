// User-level neural search configuration schema for ~/.leindex/config/leindex.toml
//
// Canonical TOML schema for the user-level neural search configuration written
// by the `leindex setup` command. Shared by the CLI and the ONNX worker.
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
const DEFAULT_MODEL_NAME: &str = "qwen3-embed-0.6b";

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

    /// Installed ONNX Runtime version (e.g., "1.25.0").
    ///
    /// VAL-SETUP-020: Config records the ORT version discovered during setup
    /// so subsequent runs and `--check` can report it without re-querying pip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ort_version: Option<String>,

    /// Directory containing model files.
    #[serde(default = "default_model_dir")]
    pub model_dir: String,

    /// ONNX model stem to load from model directories.
    #[serde(default = "default_model_name")]
    pub model_name: String,
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

    /// Enable cross-encoder re-ranking of the top-N search results with the
    /// on-demand reranker (bge-reranker-base). Improves semantic accuracy for
    /// conceptual queries at the cost of ~1-2s added latency (CPU, top-N only).
    #[serde(default)]
    pub rerank_enabled: bool,

    /// Number of top results to re-rank. Re-ranking more improves recall at the
    /// top but costs more latency (one cross-encoder pass per candidate).
    #[serde(default = "default_rerank_top_n")]
    pub rerank_top_n: u32,

    /// Enable fragment-level (sub-symbol) embeddings. Off by default; the node
    /// index remains authoritative. When enabled, Tier-2/3 fragments participate
    /// in Semantic/hybrid retrieval.
    #[serde(default)]
    pub fragment_index_enabled: bool,

    /// Max bytes per fragment (≈ Warp 200 lines × 60 chars).
    #[serde(default = "default_fragment_max_bytes")]
    pub fragment_max_bytes: u64,

    /// Fusion weight for the fragment score component (0.0-1.0). Renormalization
    /// is gated on `fragment_index_enabled` (the master switch); this weight only
    /// scales the fragment component once enabled.
    #[serde(default = "default_fragment_weight")]
    pub fragment_weight: f64,

    /// Include Tier-3 module-level orphan regions.
    #[serde(default = "default_true")]
    pub fragment_orphan_enabled: bool,

    /// Naive 200-line chunking when a tree-sitter grammar is unavailable.
    #[serde(default = "default_true")]
    pub fragment_naive_fallback: bool,
}

/// Map a configured `search_mode` string to the default `QueryType` used when a
/// caller does not pass an explicit one.
///
/// - `hybrid` -> `None` (the composite default scoring arm: tfidf+neural+struct+text)
/// - `text`   -> `Text`   (lexical-only weighting)
/// - `neural` -> `Semantic` (neural-favoring weighting; degrades to tfidf-dominant
///   if no neural embeddings are indexed — see compute_score's
///   Semantic+!neural_available arm)
///
/// Unknown strings fall back to `None` (hybrid) rather than panicking on a user
/// typo. This is the single bridge between the `[search] search_mode` config
/// string and the ranking engine's `QueryType`.
pub fn query_type_for_mode(search_mode: &str) -> Option<crate::search::ranking::QueryType> {
    match search_mode {
        "text" => Some(crate::search::ranking::QueryType::Text),
        "neural" => Some(crate::search::ranking::QueryType::Semantic),
        _ => None, // "hybrid" and unknown -> composite default
    }
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
            ort_version: None,
            model_dir: default_model_dir(),
            model_name: default_model_name(),
        }
    }
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self {
            search_mode: default_search_mode(),
            neural_weight: default_neural_weight(),
            rerank_enabled: false,
            rerank_top_n: default_rerank_top_n(),
            fragment_index_enabled: false,
            fragment_max_bytes: default_fragment_max_bytes(),
            fragment_weight: default_fragment_weight(),
            fragment_orphan_enabled: default_true(),
            fragment_naive_fallback: default_true(),
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

fn default_model_name() -> String {
    DEFAULT_MODEL_NAME.to_string()
}

fn default_search_mode() -> String {
    "hybrid".to_string()
}

fn default_neural_weight() -> f64 {
    // Single source of truth for the hybrid neural blend. Must match the
    // scorer-side defaults (HybridScorer::for_code / HybridScoringWeights
    // both use 0.40) so a stock install behaves as documented.
    0.4
}

fn default_fragment_max_bytes() -> u64 {
    // ≈ Warp's 200 lines × 60 chars default chunk budget.
    12_000
}

fn default_fragment_weight() -> f64 {
    // Empirically tuned (fragment-embeddings 1.11.0): the MRR sweep over
    // 0.12/0.20/0.30/0.35/0.40 shows 0.35 delivers full conceptual-recall
    // (MRR 0.0 -> 1.0) while preserving node-rank exactly (1.0 -> 1.0).
    // 0.30 also flips the synthetic scenario, but it sits exactly at the
    // share-equality boundary (renormalized fragment share 0.30/1.30 == the
    // decoy's tfidf share 0.3/1.30), so the win is carried by the structural
    // tie-break and is fragile to renormalization-constant drift. 0.35 gives a
    // real ~3.7pp margin (0.35/1.35 ≈ 0.259 vs 0.3/1.35 ≈ 0.222) for a
    // negligible extra change to the blend.
    0.35
}

fn default_true() -> bool {
    true
}

fn default_rerank_top_n() -> u32 {
    // ponytail: 80 (was 20). Wider cross-encoder pool so conceptual queries
    // whose ideal node ranks 21-80 in dense retrieval reach the reranker.
    // Cross-encoder rerank literature plateaus ~100-200; 80 is the sweet spot.
    // Revisit if rerank latency regresses.
    80
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

    /// The hybrid neural-score weight as `f32` — the type scoring and embedder
    /// consumers need. Config stores it as `f64`; centralize the cast so all
    /// call sites agree (single source of truth, VAL-CONFIG).
    pub fn neural_weight_f32(&self) -> f32 {
        self.search.neural_weight as f32
    }

    /// Process-wide cached config read. Reads leindex.toml once on first access,
    /// then serves the cached value; falls back to `Default` on error. Use this
    /// on hot paths (query/index) so the documented `[search]` knobs are read
    /// without re-reading the file per call.
    pub fn load_cached() -> &'static LeIndexConfig {
        static CACHED: std::sync::OnceLock<LeIndexConfig> = std::sync::OnceLock::new();
        CACHED.get_or_init(|| {
            Self::load().unwrap_or_else(|err| {
                tracing::warn!(
                    error = %err,
                    "failed to load leindex.toml for caching; using defaults"
                );
                LeIndexConfig::default()
            })
        })
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
                write!(
                    f,
                    "Cannot resolve LeIndex home directory. Set LEINDEX_HOME or ensure HOME is set."
                )
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

// Alias for use in setup.rs as `crate::config_schema`.

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
            ort_version: Some("1.25.0".to_string()),
            model_dir: "/home/user/.leindex/models".to_string(),
            model_name: "qwen3-embed-0.6b".to_string(),
        };

        let toml_str = toml::to_string(&config).unwrap();
        assert!(toml_str.contains("enabled = true"));
        assert!(toml_str.contains("execution_provider = \"cpu\""));
        assert!(toml_str.contains("ort_dylib_path"));
        assert!(toml_str.contains("ort_version"));
        assert!(toml_str.contains("model_dir"));
    }

    #[test]
    fn test_parse_malformed_returns_error() {
        let bad_toml = "[neural\nenabled = true\n";
        let result = LeIndexConfig::parse_toml(bad_toml);
        assert!(result.is_err());
    }

    #[test]
    fn test_config_roundtrip_preserves_rerank_fields() {
        let mut config = LeIndexConfig::default();
        config.search.rerank_enabled = true;
        config.search.rerank_top_n = 80;
        let decoded: LeIndexConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert!(decoded.search.rerank_enabled);
        assert_eq!(decoded.search.rerank_top_n, 80);
    }

    #[test]
    fn test_default_execution_provider_is_auto() {
        assert_eq!(LeIndexConfig::default().neural.execution_provider, "auto");
    }

    #[test]
    fn test_default_neural_weight_is_0_4() {
        // VAL-CONFIG: config default must match the scorer-side defaults
        // (HybridScorer::for_code / HybridScoringWeights both use 0.40).
        assert_eq!(LeIndexConfig::default().search.neural_weight, 0.4);
    }

    #[test]
    fn test_config_missing_keys_uses_defaults() {
        // VAL-SETUP-030: stale config from older version gets defaults for new keys
        let toml_str = "[neural]\nenabled = true\n";
        let config: LeIndexConfig = toml::from_str(toml_str).unwrap();
        assert!(config.neural.enabled);
        assert_eq!(config.search.search_mode, "hybrid");
        assert_eq!(config.search.neural_weight, 0.4);
        assert_eq!(config.indexing.batch_size, 500);
        assert_eq!(config.neural.model_name, "qwen3-embed-0.6b");
    }

    #[test]
    fn test_config_empty_uses_defaults() {
        let config: LeIndexConfig = toml::from_str("").unwrap();
        assert!(!config.neural.enabled);
        assert_eq!(config.search.search_mode, "hybrid");
        assert_eq!(config.search.neural_weight, 0.4);
    }

    #[test]
    fn test_fragment_config_defaults() {
        let config = LeIndexConfig::default();
        assert!(!config.search.fragment_index_enabled);
        assert_eq!(config.search.fragment_max_bytes, 12_000);
        assert_eq!(config.search.fragment_weight, 0.35);
        assert!(config.search.fragment_orphan_enabled);
        assert!(config.search.fragment_naive_fallback);

        // Parse from empty TOML -> same defaults (backward compatible).
        let parsed: LeIndexConfig = toml::from_str("").unwrap();
        assert_eq!(parsed, config);
    }

    #[test]
    fn test_fragment_config_round_trip() {
        let mut config = LeIndexConfig::default();
        config.search.fragment_index_enabled = true;
        config.search.fragment_max_bytes = 24_000;
        config.search.fragment_weight = 0.20;
        config.search.fragment_orphan_enabled = false;
        config.search.fragment_naive_fallback = false;
        let decoded: LeIndexConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(config, decoded);
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
                model_name: "qwen3-embed-0.6b".to_string(),
            },
            search: SearchConfig {
                search_mode: "hybrid".to_string(),
                neural_weight: 0.35,
                rerank_enabled: true,
                rerank_top_n: 80,
                fragment_index_enabled: false,
                fragment_max_bytes: 12_000,
                fragment_weight: 0.35,
                fragment_orphan_enabled: true,
                fragment_naive_fallback: true,
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
    fn test_ort_dylib_path_skip_serializing_if_none() {
        let config = NeuralConfig {
            enabled: true,
            execution_provider: "cpu".to_string(),
            ort_dylib_path: None,
            ort_version: None,
            model_dir: "/models".to_string(),
            model_name: "qwen3-embed-0.6b".to_string(),
        };
        let toml_str = toml::to_string(&config).unwrap();
        assert!(!toml_str.contains("ort_dylib_path"));
    }

    /// Serialize env-var-mutating tests within the lib test binary.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn test_load_or_recover_corrupt_file() {
        let _g = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let config_path = tmp.path().join("config").join("leindex.toml");
        std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
        std::fs::write(&config_path, "[neural\nbroken toml").unwrap();
        // SAFETY: env mutation serialized by ENV_LOCK; single-threaded under the lock.
        unsafe { std::env::set_var(LEINDEX_HOME_ENV, tmp.path()) };
        let (config, action) = LeIndexConfig::load_or_recover().unwrap();
        assert!(matches!(action, RecoveryAction::RecoveredFromCorrupt(_)));
        assert!(!config.neural.enabled);
        assert!(config_path.with_extension("toml.bak").exists());
        unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
    }

    #[test]
    fn test_config_load_returns_default_when_missing() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var(LEINDEX_HOME_ENV, "/nonexistent/path/for/testing") };
        let (config, action) = LeIndexConfig::load_or_recover().unwrap();
        assert!(matches!(action, RecoveryAction::CreatedDefault));
        assert!(!config.neural.enabled);
        unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
    }

    #[test]
    fn test_resolve_leindex_home_env_override() {
        let _g = ENV_LOCK.lock().unwrap();
        unsafe { std::env::set_var(LEINDEX_HOME_ENV, "/custom/leindex") };
        assert_eq!(
            resolve_leindex_home(),
            Some(PathBuf::from("/custom/leindex"))
        );
        unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
    }
}
