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
const DEFAULT_MODEL_NAME: &str = "qwen3-embed-0.6b-dynamic-uint8";

/// The complete LeIndex neural search configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LeIndexConfig {
    /// Neural embedding configuration. Missing sections in existing TOML
    /// retain the legacy disabled state; fresh construction enables neural.
    #[serde(default = "default_legacy_neural_config")]
    pub neural: NeuralConfig,

    /// Search behavior configuration.
    #[serde(default)]
    pub search: SearchConfig,

    /// Indexing pipeline configuration.
    #[serde(default = "default_legacy_indexing_config")]
    pub indexing: IndexingConfig,

    /// MCP server lifecycle configuration.
    #[serde(default)]
    pub mcp: McpConfig,
}

/// Neural embeddings configuration (\[neural\] section).
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

/// Search behavior configuration (\[search\] section).
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

    /// Max bytes per fragment (≈ Warp 200 lines × 60 chars). 0 disables the
    /// byte cap — fragments are bounded by the line chunk only (Codex wave-3
    /// item 3: a zero limit previously looped forever in the byte splitter).
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

/// Indexing pipeline configuration (\[indexing\] section).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexingConfig {
    /// Batch size for embedding generation.
    #[serde(default = "default_batch_size")]
    pub batch_size: u64,

    /// Maximum number of files to index.
    #[serde(default = "default_max_files")]
    pub max_files: u64,

    /// Hybrid embedding provider configuration (\[indexing.embeddings\]).
    #[serde(default = "default_legacy_embedding_config")]
    pub embeddings: EmbeddingConfig,
}

/// Remote/hybrid embedding provider settings (\[indexing.embeddings\] section).
///
/// `remote_endpoint` + `enable_hybrid` switch the neural layer from the local
/// ONNX worker to a remote OpenAI-compatible `/v1/embeddings` endpoint
/// (llama-server, vLLM, sglang, Ollama, ...). Requires the
/// `remote-embeddings` build feature; without it the keys parse but stay
/// inert and indexing uses the local model as configured under `[neural]`.
///
/// `Default` selects the fresh-install preset. Serde defaults are deliberately
/// legacy/inert so existing full or partial TOML files do not opt in by being
/// upgraded.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingConfig {
    /// Master switch for hybrid (TF-IDF + neural/remote) embeddings.
    #[serde(default = "default_enable_hybrid")]
    pub enable_hybrid: bool,

    /// Provider preset filling unset keys below. Empty/`"custom"` = the
    /// generic OpenAI-compatible path (every key must be set explicitly).
    /// `"embeddinggemma-2"` fills endpoint/model/dimension/templates from
    /// the `EMBEDDINGGEMMA2_*` constants; explicit keys always win.
    #[serde(default = "default_remote_provider")]
    pub remote_provider: String,

    /// Base URL of an OpenAI-compatible embeddings endpoint
    /// (e.g. "http://127.0.0.1:8080/v1").
    #[serde(default)]
    pub remote_endpoint: Option<String>,

    /// Bearer token for the remote endpoint (local servers usually need none).
    #[serde(default)]
    pub remote_api_key: Option<String>,

    /// Model name sent as the "model" field of the /v1/embeddings payload
    /// (llama-server accepts the served model name or alias).
    #[serde(default)]
    pub remote_model: Option<String>,

    /// Dimension override for the remote provider. Unset = measured from the
    /// first successful embed response (runtime-true); never enforced before
    /// the first response.
    #[serde(default)]
    pub remote_dimension: Option<usize>,

    /// Template applied to DOCUMENT texts before they leave the machine
    /// (`{text}` placeholder; prefix-only templates may omit it). Unset =
    /// preset template when a preset is active, else raw text (back-compat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_doc_template: Option<String>,

    /// Template applied to QUERY texts (asymmetric retrieval prefixes).
    /// Same `{text}` / preset / back-compat rules as `remote_doc_template`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_query_template: Option<String>,

    /// Local ONNX model path override (reserved; the local model is normally
    /// resolved from `[neural] model_dir`/`model_name`).
    #[serde(default)]
    pub onnx_model_path: Option<String>,
}

/// `embeddinggemma-2` preset: loopback endpoint on llama-server's standard
/// port. Runtime selection remains subject to the bakeoff; keep 8765 reserved
/// for the owner's mcp_agent_mail service.
pub const EMBEDDINGGEMMA2_DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8080/v1";

/// `embeddinggemma-2` preset: model name sent in the embeddings payload.
pub const EMBEDDINGGEMMA2_MODEL: &str = "embeddinggemma-2";

/// `embeddinggemma-2` preset: native (untruncated) embedding dimension.
pub const EMBEDDINGGEMMA2_DIMENSION: usize = 768;

/// `embeddinggemma-2` DOCUMENT template, code-search task, no title
/// available. Source: google/embeddinggemma-2 model card (Best Practices →
/// Prefix Notation & Usage table, CodeRetrieval row): document instruction
/// `title: {title or filename} | text: {code}`, with `title: none` mandated
/// when no title is available — the provider call site receives raw text
/// only, so `title: none` is correct here. llama.cpp applies NO server-side
/// template (content-only passthrough in `/props`), so the client owns it.
pub const EMBEDDINGGEMMA2_DOC_TEMPLATE: &str = "title: none | text: {text}";

/// `embeddinggemma-2` QUERY template, code-search task. Same source (model
/// card CodeRetrieval row): `task: code retrieval | query: {query}`.
pub const EMBEDDINGGEMMA2_QUERY_TEMPLATE: &str = "task: code retrieval | query: {text}";

/// A preset-resolved remote configuration: every unset key already filled.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedRemote {
    /// Effective endpoint (explicit config, or preset default).
    pub endpoint: String,
    /// Effective model name (explicit config, or preset).
    pub model: Option<String>,
    /// Effective dimension override (explicit config, or preset).
    pub remote_dimension: Option<usize>,
    /// Effective document template (explicit config, or preset; None = raw).
    pub doc_template: Option<String>,
    /// Effective query template (explicit config, or preset; None = raw).
    pub query_template: Option<String>,
}

impl EmbeddingConfig {
    /// Human-readable provider label for diagnostics ("embeddinggemma-2"
    /// preset active, else "custom").
    pub fn remote_preset_label(&self) -> &str {
        if self.embeddinggemma2_preset_active() {
            "embeddinggemma-2"
        } else {
            "custom"
        }
    }

    /// Whether the `embeddinggemma-2` preset is selected (spelling variants
    /// accepted; case-insensitive).
    fn embeddinggemma2_preset_active(&self) -> bool {
        matches!(
            self.remote_provider.trim().to_ascii_lowercase().as_str(),
            "embeddinggemma-2" | "embeddinggemma2"
        )
    }

    /// Resolve the remote ladder configuration: preset fills unset keys,
    /// explicit config always wins. Returns `None` when the ladder is off
    /// (flag false) or nothing can produce an endpoint (no explicit endpoint
    /// and no preset default).
    pub fn resolved_remote(&self) -> Option<ResolvedRemote> {
        if !self.enable_hybrid {
            return None;
        }
        let preset = self.embeddinggemma2_preset_active();
        let endpoint = match self.remote_endpoint.as_deref().map(str::trim) {
            Some(e) if !e.is_empty() => Some(e.to_string()),
            _ => preset.then(|| EMBEDDINGGEMMA2_DEFAULT_ENDPOINT.to_string()),
        }?;
        let fill = |value: &Option<String>, preset_value: &str| match value {
            Some(v) if !v.trim().is_empty() => Some(v.clone()),
            _ if preset => Some(preset_value.to_string()),
            _ => None,
        };
        Some(ResolvedRemote {
            endpoint,
            model: fill(&self.remote_model, EMBEDDINGGEMMA2_MODEL),
            doc_template: fill(&self.remote_doc_template, EMBEDDINGGEMMA2_DOC_TEMPLATE),
            query_template: fill(&self.remote_query_template, EMBEDDINGGEMMA2_QUERY_TEMPLATE),
            remote_dimension: self
                .remote_dimension
                .or_else(|| preset.then_some(EMBEDDINGGEMMA2_DIMENSION)),
        })
    }

    /// True when the remote ladder is constructible: enabled AND an endpoint
    /// resolves (explicit `remote_endpoint`, or the preset's default port).
    /// (remote-embeddings construction precondition.)
    pub fn remote_endpoint_configured(&self) -> bool {
        self.resolved_remote().is_some()
    }
}

impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            enable_hybrid: true,
            remote_provider: "embeddinggemma-2".to_string(),
            remote_endpoint: None,
            remote_api_key: None,
            remote_model: None,
            remote_dimension: None,
            remote_doc_template: None,
            remote_query_template: None,
            onnx_model_path: None,
        }
    }
}

fn default_legacy_embedding_config() -> EmbeddingConfig {
    EmbeddingConfig {
        enable_hybrid: default_enable_hybrid(),
        remote_provider: default_remote_provider(),
        remote_endpoint: None,
        remote_api_key: None,
        remote_model: None,
        remote_dimension: None,
        remote_doc_template: None,
        remote_query_template: None,
        onnx_model_path: None,
    }
}

const fn default_enable_hybrid() -> bool {
    false
}

fn default_remote_provider() -> String {
    "custom".to_string()
}

/// MCP server lifecycle configuration (\[mcp\] section).
///
/// Memory-pressure remediation (1.11.0): MCP servers spawned by AI agents were
/// accumulating (8+ instances, 2.4 GiB RSS each) because the process had no
/// idle exit and loaded project engines were never unloaded. These knobs let
/// an operator (or agent config) bound a server's lifetime and footprint.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct McpConfig {
    /// Exit the MCP server process after this many seconds with no requests.
    /// `0` disables idle exit (the server lives until stdin EOF, today's
    /// behavior). MCP clients respawn the server on the next tool call, so
    /// this releases a swapped-out idle server's memory without losing
    /// functionality. Default 1800 (30 min).
    #[serde(default = "default_mcp_idle_timeout_secs")]
    pub idle_timeout_secs: u64,

    /// Unload a loaded project engine from the ProjectRegistry after this many
    /// seconds idle (0 = keep loaded once touched). Reloaded on the next tool
    /// call. Caps per-process RSS while the spawning agent stays alive.
    /// Default 600 (10 min).
    #[serde(default = "default_mcp_engine_max_idle_secs")]
    pub engine_max_idle_secs: u64,

    /// What a long-lived MCP server loads in the background right after the
    /// client's `initialize`, for the default project (only when it is
    /// already indexed — never builds an index):
    /// `"full"` (default) the PDG, then the search engine; `"graph"` the PDG
    /// only; `"off"` nothing (everything loads on first use).
    ///
    /// Cold hydration is the only slow part of a first call; the model's
    /// think-time after the handshake is long enough to hide it.
    #[serde(default = "default_mcp_prewarm")]
    pub prewarm: String,
}

// ── Defaults ─────────────────────────────────────────────────────────────

fn default_legacy_neural_config() -> NeuralConfig {
    NeuralConfig {
        enabled: false,
        ..NeuralConfig::default()
    }
}

impl Default for NeuralConfig {
    fn default() -> Self {
        Self {
            enabled: true,
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
            embeddings: EmbeddingConfig::default(),
        }
    }
}

fn default_legacy_indexing_config() -> IndexingConfig {
    IndexingConfig {
        batch_size: default_batch_size(),
        max_files: default_max_files(),
        embeddings: default_legacy_embedding_config(),
    }
}

impl Default for McpConfig {
    fn default() -> Self {
        Self {
            idle_timeout_secs: default_mcp_idle_timeout_secs(),
            engine_max_idle_secs: default_mcp_engine_max_idle_secs(),
            prewarm: default_mcp_prewarm(),
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

fn default_mcp_idle_timeout_secs() -> u64 {
    // 30 min: long enough that active agent sessions never hit a cold reload,
    // short enough that an idle swapped-out server self-terminates instead of
    // holding multi-GB of swap for hours (memory-pressure remediation 1.11.0).
    1800
}

fn default_mcp_prewarm() -> String {
    "full".to_string()
}

fn default_mcp_engine_max_idle_secs() -> u64 {
    // 10 min: release a loaded project engine (and its mmaps) after this much
    // quiet, reloading on the next tool call.
    600
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
                    "failed to load leindex.toml for caching; preserving legacy disabled defaults"
                );
                Self {
                    neural: default_legacy_neural_config(),
                    indexing: default_legacy_indexing_config(),
                    ..Self::default()
                }
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
                // Propagate a backup-rename failure instead of discarding it: the
                // old `let _ =` would report RecoveredFromCorrupt while the corrupt
                // original is still in place (no backup was actually written), so a
                // later setup could overwrite the corrupt file without preserving it.
                // (Codex P2 review.)
                if let Err(e) = std::fs::rename(&config_path, &backup_path) {
                    return Err(ConfigError::Io(
                        config_path,
                        format!(
                            "config corrupted ({parse_err}); backup rename to {} failed: {e}",
                            backup_path.display()
                        ),
                    ));
                }
                tracing::warn!(
                    "Config corrupted: {}. Backed up to {}",
                    parse_err,
                    backup_path.display()
                );
                Ok((
                    Self {
                        neural: default_legacy_neural_config(),
                        indexing: default_legacy_indexing_config(),
                        ..Self::default()
                    },
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
    fn test_fresh_neural_default_and_legacy_toml_boundary() {
        let fresh = LeIndexConfig::default();
        assert!(fresh.neural.enabled);
        assert!(fresh.indexing.embeddings.enable_hybrid);
        assert_eq!(
            fresh.indexing.embeddings.remote_preset_label(),
            "embeddinggemma-2"
        );
        for existing in [
            "",
            "[neural]\nexecution_provider = \"cpu\"\n",
            "[neural]\nenabled = false\n",
        ] {
            let parsed: LeIndexConfig = toml::from_str(existing).expect("existing TOML");
            assert!(!parsed.neural.enabled);
        }
        let explicit: LeIndexConfig =
            toml::from_str("[neural]\nenabled = true\n").expect("opt-in TOML");
        assert!(explicit.neural.enabled);
    }

    #[test]
    fn test_neural_config_schema() {
        let config = NeuralConfig {
            enabled: true,
            execution_provider: "cpu".to_string(),
            ort_dylib_path: Some("/usr/local/lib/libonnxruntime.so".to_string()),
            ort_version: Some("1.25.0".to_string()),
            model_dir: "/home/user/.leindex/models".to_string(),
            model_name: "qwen3-embed-0.6b-dynamic-uint8".to_string(),
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
        assert_eq!(config.neural.model_name, "qwen3-embed-0.6b-dynamic-uint8");
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

        // Existing TOML retains legacy embeddings; search defaults are unchanged.
        let parsed: LeIndexConfig = toml::from_str("").unwrap();
        assert_eq!(parsed.search, config.search);
        assert!(!parsed.indexing.embeddings.enable_hybrid);
        assert_eq!(parsed.indexing.embeddings.remote_preset_label(), "custom");
        assert!(config.indexing.embeddings.enable_hybrid);
        assert_eq!(
            config.indexing.embeddings.remote_preset_label(),
            "embeddinggemma-2"
        );
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
                embeddings: EmbeddingConfig::default(),
            },
            mcp: McpConfig {
                idle_timeout_secs: 3600,
                engine_max_idle_secs: 1200,
                prewarm: "graph".to_string(),
            },
        };
        let toml_str = toml::to_string_pretty(&config).unwrap();
        let parsed: LeIndexConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(config, parsed);
    }

    #[test]
    fn test_mcp_config_defaults() {
        // Memory-pressure remediation 1.11.0 (D-1/D-2).
        let config = LeIndexConfig::default();
        assert_eq!(config.mcp.idle_timeout_secs, 1800);
        assert_eq!(config.mcp.engine_max_idle_secs, 600);
        assert_eq!(config.mcp.prewarm, "full");

        // Parse from empty TOML -> same defaults (backward compatible: existing
        // configs without an [mcp] section keep working).
        let parsed: LeIndexConfig = toml::from_str("").unwrap();
        assert_eq!(parsed.mcp, McpConfig::default());
        let legacy = "[neural]\nenabled = true\n";
        let parsed_legacy: LeIndexConfig = toml::from_str(legacy).unwrap();
        assert_eq!(parsed_legacy.mcp, McpConfig::default());
    }

    #[test]
    fn test_mcp_config_round_trip() {
        let mut config = LeIndexConfig::default();
        config.mcp.idle_timeout_secs = 60;
        config.mcp.engine_max_idle_secs = 0;
        let decoded: LeIndexConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert_eq!(config.mcp, decoded.mcp);
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
        assert!(config.neural.enabled);
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
