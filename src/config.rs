//! User-level neural search configuration schema for `~/.leindex/config/config.yaml`.
//!
//! The YAML document is the v2 source of truth. The typed Rust fields below are
//! the runtime-owned subset; unknown v2 sections are preserved during migration
//! and every subsequent save.

use cache::{probe_due, refresh_cached_config};
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// Environment variable for the LeIndex home directory override.
pub const LEINDEX_HOME_ENV: &str = "LEINDEX_HOME";

/// How often `load_cached()` may stat the config file to detect an edit.
///
/// The stat is the only cost of keeping a long-lived process in sync with
/// `leindex.toml`; throttling it means the hot path pays one relaxed atomic load
/// in the common case. Chosen well below the MCP idle timeout so an edit lands
/// while the user is still looking at the file, and well above the per-query
/// call rate so the stat never shows up in a profile.
const CONFIG_REFRESH_THROTTLE: Duration = Duration::from_secs(2);

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

    /// System-One gate configuration.
    #[serde(default)]
    pub system_one: SystemOneConfig,

    /// Managed model-server services (\[services.<name>\] sections). Each
    /// names a llama-server-compatible process LeIndex can start, stop, and
    /// health-check on the user's behalf; consumers reference a service by
    /// name (`[system_one] service`, `[indexing.embeddings] remote_service`)
    /// and derive their endpoint from it. Empty by default: no config ever
    /// gains a managed process by being upgraded.
    #[serde(default)]
    pub services: std::collections::BTreeMap<String, ServiceConfig>,
}

/// One managed model-server process (\[services.<name>\] section).
///
/// Describes how to spawn a llama-server-compatible binary (`binary -m model
/// --host host --port port <extra_args>`), whether LeIndex may start it on
/// demand, and how long a start may take (large GGUF loads are slow). The
/// service never runs unless a consumer references it by name.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ServiceConfig {
    /// Binary to execute (PATH-resolved). Default: "llama-server".
    #[serde(default = "default_service_binary")]
    pub binary: String,

    /// Model file path passed via `-m` (required for spawning; referencing a
    /// service without one only derives its endpoint, it can never start).
    #[serde(default)]
    pub model: Option<String>,

    /// Served model alias (`--alias`); also the payload `model` fallback for
    /// consumers that did not set one. Default: model file stem.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alias: Option<String>,

    /// Listen host passed via `--host`. Default: "127.0.0.1".
    #[serde(default = "default_service_host")]
    pub host: String,

    /// Listen port passed via `--port` (required).
    #[serde(default)]
    pub port: u16,

    /// Extra CLI arguments appended verbatim (e.g. "--ctx-size", "4096",
    /// "--embeddings").
    #[serde(default)]
    pub extra_args: Vec<String>,

    /// Allow LeIndex to start this service on demand when a consumer that
    /// references it finds the endpoint down. Default: false — referencing a
    /// service only derives its endpoint unless this is set.
    #[serde(default)]
    pub auto_start: bool,

    /// How long a start may wait for the port to accept connections before
    /// giving up (and terminating the attempt). Default: 120s (GGUF loads).
    #[serde(default = "default_service_start_timeout_secs")]
    pub start_timeout_secs: u64,
}

impl Default for ServiceConfig {
    fn default() -> Self {
        Self {
            binary: default_service_binary(),
            model: None,
            alias: None,
            host: default_service_host(),
            port: 0,
            extra_args: Vec::new(),
            auto_start: false,
            start_timeout_secs: default_service_start_timeout_secs(),
        }
    }
}

fn default_service_binary() -> String {
    "llama-server".to_string()
}

fn default_service_host() -> String {
    "127.0.0.1".to_string()
}

const fn default_service_start_timeout_secs() -> u64 {
    120
}

impl ServiceConfig {
    /// `http://host:port` — the base URL consumers derive from this service.
    /// IPv6 literals are bracketed (`http://[::1]:8080`) so the URL parses.
    pub fn base_url(&self) -> String {
        let host = if self.host.contains(':') {
            format!("[{}]", self.host)
        } else {
            self.host.clone()
        };
        format!("http://{}:{}", host, self.port)
    }

    /// The payload model name consumers fall back to: explicit alias, else
    /// the model file stem, else nothing.
    pub fn served_name(&self) -> Option<String> {
        if let Some(alias) = self.alias.as_deref() {
            if !alias.trim().is_empty() {
                return Some(alias.to_string());
            }
        }
        self.model.as_deref().map(|path| {
            std::path::Path::new(path)
                .file_stem()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| path.to_string())
        })
    }
}

/// One saved System-One provider profile (\[system_one.presets.<name>\]).
///
/// While `[system_one] active_preset = "<name>"` names an existing preset,
/// every field the preset sets overrides the section's own value — switching
/// providers is a one-line change and the wire templates follow the provider.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct SystemOnePreset {
    /// Provider: "laya", "jev" or "openai".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,

    /// Model name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Base URL (overrides the provider default).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Name of a `[services.<name>]` entry the endpoint derives from.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,

    /// Environment variable holding the API key (remote providers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,
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

    /// Which reranker backend consumes the top-N candidates: `onnx` (default;
    /// local cross-encoder via the neural worker), `system-one` (LLM judge
    /// through the `[system_one]` provider; reranks regardless of
    /// `rerank_enabled`), or `off` (rerank disabled even when `rerank_enabled`
    /// is true).
    #[serde(default = "default_rerank_backend")]
    pub rerank_backend: String,

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

    /// Name of a `[services.<name>]` entry the endpoint derives from.
    /// Sits below an explicit `remote_endpoint` in the resolution ladder:
    /// explicit endpoint > service base URL > preset default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_service: Option<String>,

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

    /// Environment variable containing the remote API key; preferred over storing secrets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_api_key_env: Option<String>,

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

    /// Hard cap on inputs per request for the remote endpoint. Unset = the
    /// provider preset's documented limit (see [`RemotePreset::batch_limit`]),
    /// else no cap.
    ///
    /// Exists because the caller's batch size is tuned for throughput on a
    /// LOCAL server and knows nothing about a remote provider's protocol
    /// limits. Left unset with a known preset, the preset wins; set it
    /// explicitly to override a preset you know better than, or to bound a
    /// `custom` endpoint that has no preset to consult.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_batch_limit: Option<usize>,

    /// Largest token count ONE input may occupy. Unset = the provider preset's
    /// documented window, else no limit.
    ///
    /// A count cap (`remote_batch_limit`) cannot express this: one enormous
    /// document can exceed a model's window on its own. Set it for a `custom`
    /// endpoint whose model window is known, so oversized documents are
    /// truncated deliberately instead of costing a rejected round trip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_max_input_tokens: Option<usize>,

    /// Largest token count ONE REQUEST may carry in total. Unset = the
    /// provider preset's documented budget, else no limit.
    ///
    /// This is the limit that row counts miss entirely, and its absence was a
    /// real outage: a hosted EmbeddingGemma route rejected every 32-document
    /// batch with `The prompt is too long ... Shorten it and retry` because
    /// ~7k tokens of input overflowed a 2,048-token request window, even though
    /// 32 was comfortably under the count cap. Without it the client can only
    /// learn the ceiling by having requests rejected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_request_token_budget: Option<usize>,

    /// Local ONNX model path override (reserved; the local model is normally
    /// resolved from `[neural] model_dir`/`model_name`).
    #[serde(default)]
    pub onnx_model_path: Option<String>,

    /// EmbeddingGemma2 Matryoshka slicing policy (see
    /// [`crate::search::mrl::MatryoshkaSetting`]).
    /// `Off` (default) preserves pre-MRL behavior byte-for-byte.
    #[serde(default)]
    pub matryoshka: MatryoshkaSetting,
}

/// `embeddinggemma-2` preset: loopback endpoint on llama-server's standard
/// port. Runtime selection remains subject to the bakeoff; keep 8765 reserved
/// for the owner's mcp_agent_mail service.
pub const EMBEDDINGGEMMA2_DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8080/v1";

/// `embeddinggemma-2` preset: model name sent in the embeddings payload.
pub const EMBEDDINGGEMMA2_MODEL: &str = "embeddinggemma-2";

/// EmbeddingGemma family token window.
///
/// 2,048 is the documented sequence length for the whole EmbeddingGemma
/// family, and the hosted OpenAI-compatible proxies enforce it as a REQUEST
/// total as well as a per-input limit: a 32-document batch of ~882 B documents
/// (~7k tokens) is rejected wholesale with `The prompt is too long for
/// embeddinggemma-300m on the routes that remain. Shorten it and retry`.
///
/// This is NOT the same quantity as `batch_limit`. `batch_limit` counts
/// DOCUMENTS; this counts TOKENS. A route can be far under the document cap and
/// still be rejected here, so request width has to be packed against this.
pub const EMBEDDINGGEMMA_REQUEST_TOKEN_BUDGET: usize = 2048;

/// `embeddinggemma-2` preset: native (untruncated) embedding dimension.
pub const EMBEDDINGGEMMA2_DIMENSION: usize = 768;

/// `[indexing.embeddings] matryoshka` policy value.
///
/// The type lives in `search::mrl` next to the slicing code that consumes it:
/// that module is reachable from the `search` feature alone, whereas this
/// module only exists under `cli`/`onnx`, so defining it here made the core
/// search module depend on a feature-gated one and broke the minimal build.
pub use crate::search::mrl::MatryoshkaSetting;

/// `embeddinggemma-2` DOCUMENT template, code-search task. Source:
/// google/embeddinggemma-2 model card (Best Practices → Prefix Notation &
/// Usage table, CodeRetrieval row): document instruction `title: {title or
/// filename} | text: {code}`. Call sites thread the file's project-relative
/// path as `{title}` (see `apply_template`); llama.cpp applies NO server-side
/// template (content-only passthrough in `/props`), so the client owns it.
pub const EMBEDDINGGEMMA2_DOC_TEMPLATE: &str = "title: {title} | text: {text}";

/// `embeddinggemma-2` QUERY template, code-search task. Same source (model
/// card CodeRetrieval row): `task: code retrieval | query: {query}`.
pub const EMBEDDINGGEMMA2_QUERY_TEMPLATE: &str = "task: code retrieval | query: {text}";

/// Whether a model identifier names an EmbeddingGemma2 model.
///
/// Hosts spell it `text-embedding-embeddinggemma-2`, `embeddinggemma-2`,
/// `embeddinggemma2`, or a quantized file stem (`embeddinggemma-2-Q8_0`), so
/// compare on the normalized substring instead of enumerating spellings. Used by
/// [`EmbeddingConfig::embeddinggemma2_preset_active`] to recognize a `custom`
/// provider that serves this model, where the provider name carries no
/// information and `remote_model` is the only identification available.
fn is_embeddinggemma2_model_name(model: &str) -> bool {
    let normalized: String = model
        .trim()
        .to_ascii_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    normalized.contains("embeddinggemma")
}

// Remote provider presets (`RemoteWire`, `RemotePreset`, `REMOTE_PRESETS`) live
// in `config::remote_presets`; re-exported so `crate::config::REMOTE_PRESETS`
// and friends keep their paths.
mod remote_presets;
pub use remote_presets::{REMOTE_PRESETS, RemotePreset, RemoteWire};

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
    /// Wire family the resolved endpoint speaks. Derived from the selected
    /// preset, never from the endpoint string: an operator who explicitly
    /// points `gemini` at a proxy still wants the native Gemini request shape.
    pub wire: RemoteWire,
    /// Hard cap on inputs per request, or `None` when neither the config nor
    /// the preset knows one. Consumed by the index builder to size remote
    /// batches, and enforced again inside the provider.
    pub batch_limit: Option<usize>,
    /// Asymmetric-retrieval field values (`input_type`) for the document and
    /// query sides, from the preset. `None` means the provider does not take
    /// one and the field is omitted from the request entirely.
    pub input_type_document: Option<String>,
    /// Query-side `input_type` (e.g. Cohere's `search_query`, Voyage's `query`),
    /// from the preset; `None` omits the field from query requests.
    pub input_type_query: Option<String>,
    /// Token ceiling for ONE input, or `None` when neither the config nor the
    /// preset knows one. Used to truncate a document before sending rather than
    /// discovering the limit from a rejection.
    pub max_input_tokens: Option<usize>,
    /// Token ceiling for ONE REQUEST (all inputs summed), or `None` when
    /// unknown. Used to pack request width; a row-count cap cannot express it.
    pub request_token_budget: Option<usize>,
}

impl EmbeddingConfig {
    /// Resolve a key without persisting the environment's secret value.
    pub fn resolved_api_key(&self) -> Result<Option<String>, String> {
        if let Some(name) = &self.remote_api_key_env {
            return std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(Some)
                .ok_or_else(|| {
                    format!("Embedding API key environment variable {name} is unset or empty")
                });
        }
        Ok(self.remote_api_key.clone())
    }

    /// Human-readable provider label for diagnostics: the recognized preset
    /// name (`"gemini"`, `"voyage"`, `"embeddinggemma-2"`, ...) when one is
    /// selected, else `"custom"`.
    ///
    /// This used to answer `"custom"` for everything that was not the
    /// EmbeddingGemma2 preset, which made `leindex diagnostics` report a
    /// provider it had never inspected.
    pub fn remote_preset_label(&self) -> &str {
        if self.embeddinggemma2_preset_active() {
            "embeddinggemma-2"
        } else {
            self.remote_preset().map_or("custom", |preset| preset.name)
        }
    }

    /// The provider preset selected by `remote_provider`, if it names one.
    ///
    /// Case-insensitive; an unknown or empty value is `None`, which keeps the
    /// generic OpenAI-compatible path (every key must be set explicitly).
    pub fn remote_preset(&self) -> Option<&'static RemotePreset> {
        let name = self.remote_provider.trim().to_ascii_lowercase();
        REMOTE_PRESETS.iter().find(|preset| preset.name == name)
    }

    /// Effective Matryoshka slicing for this configuration. `Dynamic` and the
    /// fixed dimensions only engage when BOTH hold:
    ///
    /// 1. the EmbeddingGemma2 preset is selected — its MRL training is what makes
    ///    truncation quality-preserving; any other provider keeps full-dimension
    ///    rows regardless of the setting, and
    /// 2. the `LEINDEX_FEATURE_MATRYOSHKA` rollout flag is armed — the capability
    ///    is experimental, so per AGENTS.md it defaults OFF and a configured
    ///    policy is inert until the flag is exported. Without this gate, editing
    ///    a config file would be enough to turn the capability on fleet-wide and
    ///    there would be no per-deployment kill switch.
    pub fn effective_matryoshka(&self) -> MatryoshkaSetting {
        if self.embeddinggemma2_preset_active()
            && crate::feature_flags::FeatureFlag::Matryoshka.is_enabled()
        {
            self.matryoshka
        } else {
            MatryoshkaSetting::Off
        }
    }

    /// Whether the `embeddinggemma-2` preset is selected (spelling variants
    /// accepted; case-insensitive).
    ///
    /// The provider name is not the only way to select it. A `custom` provider
    /// pointed at a hosted OpenAI-compatible EmbeddingGemma2 endpoint must name
    /// its model in `remote_model` (there is no preset to fill it), and that
    /// spelling is just as authoritative: `remote_provider = "custom"` +
    /// `remote_model = "embeddinggemma-2"` is how a third-party host running this
    /// model is reached. Reading only the provider name left
    /// `doc_template`/`query_template` as `None` on that route, so the corpus and
    /// every query were embedded with NO task prefixes — the inputs the model
    /// card says produce degraded vectors. It also made
    /// [`Self::remote_preset_label`] report `"embeddinggemma-2"` while the
    /// templates stayed `None`, i.e. the config disagreed with itself.
    ///
    /// Only an otherwise-unknown provider gets the model-name fallback: a
    /// recognized preset (`openai`, `voyage`, ...) already defines the wire and
    /// its own templating, so a contradictory `remote_model` there is an operator
    /// error that must not silently switch templates underneath it.
    pub fn embeddinggemma2_preset_active(&self) -> bool {
        if matches!(
            self.remote_provider.trim().to_ascii_lowercase().as_str(),
            "embeddinggemma-2" | "embeddinggemma2" | "embeddinggemma-300m" | "embeddinggemma300m"
        ) {
            return true;
        }
        self.remote_preset().is_none()
            && self
                .remote_model
                .as_deref()
                .is_some_and(is_embeddinggemma2_model_name)
    }

    /// Resolve the remote ladder configuration: preset fills unset keys,
    /// explicit config always wins. Endpoint ladder: explicit
    /// `remote_endpoint` > `remote_service` base URL > preset default.
    /// Returns `None` when the ladder is off (flag false) or nothing can
    /// produce an endpoint.
    ///
    /// Services are read from the process-wide cached config (the daemon
    /// serves every consumer from it); tests use [`Self::resolved_remote_with`].
    pub fn resolved_remote(&self) -> Option<ResolvedRemote> {
        self.resolved_remote_with(&LeIndexConfig::load_cached().services)
    }

    /// Pure [`Self::resolved_remote`] with an explicit services map.
    pub fn resolved_remote_with(
        &self,
        services: &std::collections::BTreeMap<String, ServiceConfig>,
    ) -> Option<ResolvedRemote> {
        if !self.enable_hybrid {
            return None;
        }
        let gemma = self.embeddinggemma2_preset_active();
        // A recognized provider preset also supplies a default endpoint, so
        // `remote_provider = "gemini"` alone is a working route. `custom` (and
        // any unrecognized value) deliberately resolves to no preset: a bare
        // endpoint must be spelled out, which is what the generic route always
        // required.
        let preset = self.remote_preset();
        let service = self
            .remote_service
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .and_then(|name| services.get(name));
        // Ladder: explicit endpoint > service base URL > preset default. The
        // EmbeddingGemma2 preset is the one provider LeIndex itself spawns, so
        // an existing `remote_service` outranks its loopback default.
        let endpoint = match self.remote_endpoint.as_deref().map(str::trim) {
            Some(e) if !e.is_empty() => Some(e.to_string()),
            _ => service.map(|s| format!("{}/v1", s.base_url())).or_else(|| {
                preset
                    .map(|p| p.endpoint.to_string())
                    .or_else(|| gemma.then(|| EMBEDDINGGEMMA2_DEFAULT_ENDPOINT.to_string()))
            }),
        }?;
        // Each key falls back to the preset value when unset. `gemma` is folded
        // into `preset_model`/`preset_dimension` so the EmbeddingGemma2 preset
        // and the provider table share one fill path instead of two.
        let fill = |value: &Option<String>, preset_value: Option<&str>| match value {
            Some(v) if !v.trim().is_empty() => Some(v.clone()),
            _ => preset_value.map(str::to_string),
        };
        let preset_model = preset.map(|p| p.model).or(if gemma {
            Some(EMBEDDINGGEMMA2_MODEL)
        } else {
            None
        });
        let preset_dimension = preset
            .map(|p| p.dimension)
            .or_else(|| gemma.then_some(EMBEDDINGGEMMA2_DIMENSION));
        Some(ResolvedRemote {
            endpoint,
            // A managed service knows the model it actually serves, which
            // outranks a table entry.
            model: fill(&self.remote_model, preset_model)
                .or_else(|| service.and_then(|s| s.served_name())),
            doc_template: fill(
                &self.remote_doc_template,
                gemma.then_some(EMBEDDINGGEMMA2_DOC_TEMPLATE),
            ),
            query_template: fill(
                &self.remote_query_template,
                gemma.then_some(EMBEDDINGGEMMA2_QUERY_TEMPLATE),
            ),
            remote_dimension: self.remote_dimension.or(preset_dimension),
            wire: preset.map_or(RemoteWire::OpenAiCompat, |p| p.wire),
            // Explicit config > preset. `custom`/unknown providers have no
            // preset, so an unset override leaves the cap unknown rather than
            // inventing one: a wrong cap that is too high fails whole batches,
            // so the safer default is "no opinion" (the caller then relies on
            // error-driven bisection, which is slow but correct).
            batch_limit: self.remote_batch_limit.or(preset.map(|p| p.batch_limit)),
            // Preset-only: these name the provider's own vocabulary
            // (`search_document`, `retrieval.passage`, ...), so there is nothing
            // for an operator to override with without inventing a mapping.
            input_type_document: preset
                .and_then(|p| p.input_type_document)
                .map(str::to_string),
            input_type_query: preset.and_then(|p| p.input_type_query).map(str::to_string),
            // Token limits: explicit config > preset > gemma family default.
            //
            // `gemma` is folded in so a `custom` provider naming an
            // EmbeddingGemma model gets the 2,048-token window even with no
            // preset row. Without this a custom route had no limits at all, and
            // request width was decided purely by document count — which is how
            // a 32-document batch of ~7k tokens was sent to an endpoint that
            // accepts 2,048 and rejected every time.
            max_input_tokens: self.remote_max_input_tokens.or_else(|| {
                preset
                    .map(|p| p.max_input_tokens)
                    .or(gemma.then_some(EMBEDDINGGEMMA_REQUEST_TOKEN_BUDGET))
            }),
            request_token_budget: self.remote_request_token_budget.or_else(|| {
                preset
                    .map(|p| p.request_token_budget)
                    .or(gemma.then_some(EMBEDDINGGEMMA_REQUEST_TOKEN_BUDGET))
            }),
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
            remote_service: None,
            remote_endpoint: None,
            remote_api_key: None,
            remote_api_key_env: None,
            remote_model: None,
            remote_dimension: None,
            remote_doc_template: None,
            remote_query_template: None,
            remote_batch_limit: None,
            remote_max_input_tokens: None,
            remote_request_token_budget: None,
            onnx_model_path: None,
            matryoshka: MatryoshkaSetting::Off,
        }
    }
}

fn default_legacy_embedding_config() -> EmbeddingConfig {
    EmbeddingConfig {
        enable_hybrid: default_enable_hybrid(),
        remote_provider: default_remote_provider(),
        matryoshka: MatryoshkaSetting::Off,
        remote_service: None,
        remote_endpoint: None,
        remote_api_key: None,
        remote_api_key_env: None,
        remote_model: None,
        remote_dimension: None,
        remote_doc_template: None,
        remote_query_template: None,
        remote_batch_limit: None,
        remote_max_input_tokens: None,
        remote_request_token_budget: None,
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

    /// Index a project on first use from a tool call.
    ///
    /// Default `false`: a tool call is not consent to spend minutes of CPU and
    /// disk, and a read (`git status`, `read_file`, text search) is not a
    /// request to index. Tools that REQUIRE an index answer with their own
    /// "not indexed" error and remediation; indexing is an explicit act.
    ///
    /// `LEINDEX_AUTO_INDEX=1` overrides this to `true`.
    #[serde(default)]
    pub auto_index: bool,
}

// ── Defaults ─────────────────────────────────────────────────────────────

/// System-One gate configuration (\[system_one\] section).
///
/// Experimental: a typed-decision model (local Laya, TypeSafe Jev, OpenAI
/// Decisions) validates tool calls before they run and classifies search
/// results before they reach the calling model. Defaults are inert
/// (`mode = "off"`); invariants are documented in
/// `src/cli/mcp/system_one/mod.rs`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SystemOneConfig {
    /// `off` (default), `shadow` (decide and log only) or `enforce`.
    #[serde(default)]
    pub mode: String,

    /// `laya` (local, default), `jev` or `openai`.
    #[serde(default = "default_system_one_provider")]
    pub provider: String,

    /// Provider base URL; unset = the provider's documented default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,

    /// Model name; unset = the provider's documented default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,

    /// Name of the environment variable holding the API key (never the key
    /// itself). Required for the remote providers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key_env: Option<String>,

    /// Per-request timeout in milliseconds. Over budget = fail open.
    #[serde(default = "default_system_one_timeout_ms")]
    pub timeout_ms: u64,

    /// Validate (and when enforcing, repair) tool calls before execution.
    #[serde(default = "default_true")]
    pub pre_gate: bool,

    /// Classify (and when enforcing, filter and re-order) search results.
    #[serde(default = "default_true")]
    pub post_gate: bool,

    /// Post-gate: drop a result when P(relevant) is below this.
    #[serde(default = "default_system_one_relevant_min")]
    pub relevant_min: f32,

    /// Post-gate: drop a result when P(contains instructions) exceeds this.
    #[serde(default = "default_system_one_instructions_max")]
    pub instructions_max: f32,

    /// Post-gate: judge at most this many results per call.
    #[serde(default = "default_system_one_top_n")]
    pub top_n: usize,

    /// Pre-gate: auto-apply a model-chosen repair at or above this confidence.
    #[serde(default = "default_system_one_tau_apply")]
    pub tau_apply: f32,

    /// Name of a `[services.<name>]` entry the endpoint derives from.
    /// `[system_one] base_url` (or the preset's) still wins when set; the
    /// service supplies the base URL otherwise and (when `auto_start`) can be
    /// started on demand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service: Option<String>,

    /// Name of a profile under `[system_one.presets]` whose set fields
    /// override this section's. Switching providers end-to-end is changing
    /// this one line; wire templates follow the provider automatically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_preset: Option<String>,

    /// Saved provider profiles (`[system_one.presets.<name>]` sections).
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub presets: std::collections::BTreeMap<String, SystemOnePreset>,
}

impl SystemOneConfig {
    /// Apply the active preset overlay: every field the preset sets overrides
    /// this section's value. Unset provider-specific fields inherit only
    /// when the provider is unchanged; switching providers uses that provider's
    /// defaults rather than retaining the old endpoint, model, or credentials.
    /// An `active_preset` naming a missing preset is ignored (warned by the
    /// consumer) — config stays usable.
    pub fn effective(&self) -> Self {
        let Some(name) = self.active_preset.as_deref().map(str::trim) else {
            return self.clone();
        };
        let Some(preset) = self.presets.get(name) else {
            return self.clone();
        };
        let provider = preset
            .provider
            .clone()
            .unwrap_or_else(|| self.provider.clone());
        // Match the provider parser's case/whitespace and Jev alias rules.
        let provider_identity = |value: &str| match value.trim().to_ascii_lowercase().as_str() {
            "typesafe" => "jev".to_string(),
            value => value.to_string(),
        };
        let same_provider = provider_identity(&provider) == provider_identity(&self.provider);
        let overlay = |value: &Option<String>, base: &Option<String>| {
            value
                .clone()
                .or_else(|| if same_provider { base.clone() } else { None })
        };
        Self {
            provider,
            model: overlay(&preset.model, &self.model),
            base_url: overlay(&preset.base_url, &self.base_url),
            service: overlay(&preset.service, &self.service),
            api_key_env: overlay(&preset.api_key_env, &self.api_key_env),
            ..self.clone()
        }
    }
}

impl Default for SystemOneConfig {
    fn default() -> Self {
        Self {
            mode: String::new(),
            provider: default_system_one_provider(),
            base_url: None,
            model: None,
            api_key_env: None,
            timeout_ms: default_system_one_timeout_ms(),
            pre_gate: true,
            post_gate: true,
            relevant_min: default_system_one_relevant_min(),
            instructions_max: default_system_one_instructions_max(),
            top_n: default_system_one_top_n(),
            tau_apply: default_system_one_tau_apply(),
            service: None,
            active_preset: None,
            presets: std::collections::BTreeMap::new(),
        }
    }
}

fn default_system_one_provider() -> String {
    "laya".to_string()
}

fn default_system_one_timeout_ms() -> u64 {
    150
}

fn default_system_one_relevant_min() -> f32 {
    0.45
}

fn default_system_one_instructions_max() -> f32 {
    0.70
}

fn default_system_one_top_n() -> usize {
    10
}

fn default_system_one_tau_apply() -> f32 {
    0.80
}

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
            rerank_backend: default_rerank_backend(),
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
            auto_index: false,
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

fn default_rerank_backend() -> String {
    "onnx".to_string()
}

/// Allowed values for `[search] rerank_backend`.
pub const RERANK_BACKENDS: &[&str] = &["onnx", "system-one", "off"];

/// Whether the `system-one` rerank backend is ARMED: this build carries the
/// `system-one` feature and the `LEINDEX_FEATURE_SYSTEM_ONE` rollout flag is on.
///
/// This is a necessary condition, not proof that the judge will run. The
/// dispatch builds its asker through `asker_from_config`, which refuses a config
/// it cannot construct a provider for — an unparseable `[system_one] provider`,
/// or a remote provider whose `api_key_env` names an unset or empty variable.
/// Nothing ON/OFF folds those in, and that is deliberate: `rerank_enabled` for
/// the `onnx` backend already means "config says to try", not "the worker came
/// up", and a provider-key check here would have to read `[system_one]` on a
/// path that does not carry the config. So read this as "the rollout permits
/// system-one", never as "the reranker will be constructed".
///
/// `Effective: rerank ON/OFF` in `setup` output inherits the same caveat: ON
/// means the stage is armed, not that a key is present. The reason line the
/// checker prints for an unarmed backend is what separates the two — it names
/// the feature or the env var, so an ON with no reason line did not fail a gate.
pub fn system_one_rerank_available() -> bool {
    if !cfg!(feature = "system-one") {
        return false;
    }
    crate::feature_flags::FeatureFlag::SystemOne.is_enabled()
}

impl SearchConfig {
    /// Whether the rerank stage runs at all under the configured backend:
    /// `off` never reranks; `system-one` reranks regardless of
    /// `rerank_enabled` — but only when the backend is actually constructible
    /// (see [`system_one_rerank_available`]), otherwise it degrades to no
    /// reranking exactly as the dispatch does; `onnx` (and any unknown string,
    /// treated as the default) reranks only when `rerank_enabled` is true. The
    /// single semantic bridge consumed by the rerank dispatch, the over-fetch
    /// decision, and the search-cache key fold.
    pub fn rerank_effectively_enabled(&self) -> bool {
        match self.rerank_backend.as_str() {
            "off" => false,
            "system-one" => system_one_rerank_available(),
            _ => self.rerank_enabled,
        }
    }
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

/// Read the global per-file admission limit from canonical v2 YAML.
///
/// This is deliberately a best-effort fallback for projects without a local
/// `.leindex/config.toml`; malformed or missing global config leaves the safe
/// project default in place.
pub fn global_max_file_size() -> Option<u64> {
    let path = config_file_path()?;
    let contents = std::fs::read_to_string(path).ok()?;
    let document: YamlValue = serde_yaml::from_str(&contents).ok()?;
    document
        .get("file_filtering")
        .or_else(|| document.get("size_limits"))
        .and_then(|section| section.get("max_file_size"))
        .and_then(YamlValue::as_u64)
}

/// Get the canonical v2 YAML config path.
pub fn config_file_path() -> Option<PathBuf> {
    resolve_leindex_home().map(|h| h.join("config").join("config.yaml"))
}

/// Get the legacy Rust TOML config path used by pre-v2.1.1 installs.
pub fn legacy_config_file_path() -> Option<PathBuf> {
    resolve_leindex_home().map(|h| h.join("config").join("leindex.toml"))
}

/// Get the path to the model directory.
pub fn model_dir_path() -> Option<PathBuf> {
    resolve_leindex_home().map(|h| h.join(DEFAULT_MODEL_DIR_SUFFIX))
}

// The cached-config machinery (statics, throttled probe, install path,
// `ConfigGuard`) lives in `config::cache` — see that module for the reload
// contract and the ownership rationale behind the `Arc` snapshots.
mod cache;

pub use cache::ConfigGuard;

const V2_CONFIG_TEMPLATE: &str = include_str!("../config.yaml");

fn merge_yaml(base: &mut YamlValue, overlay: YamlValue) {
    match (base, overlay) {
        (YamlValue::Mapping(base_map), YamlValue::Mapping(overlay_map)) => {
            for (key, value) in overlay_map {
                match base_map.get_mut(&key) {
                    Some(existing) => merge_yaml(existing, value),
                    None => {
                        base_map.insert(key, value);
                    }
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn write_yaml_atomic(path: &Path, value: &YamlValue) -> Result<(), ConfigError> {
    let parent = path.parent().ok_or(ConfigError::NoHomeDir)?;
    std::fs::create_dir_all(parent)
        .map_err(|e| ConfigError::Io(path.to_path_buf(), e.to_string()))?;
    let yaml = serde_yaml::to_string(value).map_err(|e| ConfigError::Serialize(e.to_string()))?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)
        .map_err(|e| ConfigError::Io(path.to_path_buf(), e.to_string()))?;
    temporary
        .write_all(yaml.as_bytes())
        .and_then(|_| temporary.as_file().sync_all())
        .map_err(|e| ConfigError::Io(path.to_path_buf(), e.to_string()))?;
    temporary
        .persist(path)
        .map_err(|e| ConfigError::Io(path.to_path_buf(), e.error.to_string()))?;
    Ok(())
}

fn migrate_legacy_toml() -> Result<Option<PathBuf>, ConfigError> {
    let yaml_path = config_file_path().ok_or(ConfigError::NoHomeDir)?;
    let legacy_path = legacy_config_file_path().ok_or(ConfigError::NoHomeDir)?;
    if yaml_path.exists() || !legacy_path.exists() {
        return Ok(None);
    }

    let contents = std::fs::read_to_string(&legacy_path)
        .map_err(|e| ConfigError::Io(legacy_path.clone(), e.to_string()))?;
    let legacy: toml::Value = toml::from_str(&contents)
        .map_err(|e| ConfigError::Parse(legacy_path.clone(), e.to_string()))?;
    let mut document: YamlValue = serde_yaml::from_str(V2_CONFIG_TEMPLATE)
        .map_err(|e| ConfigError::Parse(yaml_path.clone(), e.to_string()))?;
    let legacy_yaml =
        serde_yaml::to_value(legacy).map_err(|e| ConfigError::Serialize(e.to_string()))?;
    merge_yaml(&mut document, legacy_yaml);
    write_yaml_atomic(&yaml_path, &document)?;

    let migrated_path = legacy_path.with_extension("toml.migrated");
    std::fs::rename(&legacy_path, &migrated_path)
        .map_err(|e| ConfigError::Io(legacy_path, e.to_string()))?;
    Ok(Some(migrated_path))
}

impl LeIndexConfig {
    /// Write the canonical v2 YAML document, preserving every section that the
    /// typed Rust runtime does not own.
    pub fn save(&self) -> Result<PathBuf, ConfigError> {
        let config_path = config_file_path().ok_or(ConfigError::NoHomeDir)?;
        let mut document = if config_path.exists() {
            let contents = std::fs::read_to_string(&config_path)
                .map_err(|e| ConfigError::Io(config_path.clone(), e.to_string()))?;
            serde_yaml::from_str(&contents)
                .map_err(|e| ConfigError::Parse(config_path.clone(), e.to_string()))?
        } else {
            serde_yaml::from_str(V2_CONFIG_TEMPLATE)
                .map_err(|e| ConfigError::Parse(config_path.clone(), e.to_string()))?
        };
        let typed =
            serde_yaml::to_value(self).map_err(|e| ConfigError::Serialize(e.to_string()))?;
        merge_yaml(&mut document, typed);
        write_yaml_atomic(&config_path, &document)?;
        Ok(config_path)
    }

    /// Read the canonical v2 YAML config, migrating a legacy TOML install first.
    pub fn load() -> Result<Self, ConfigError> {
        migrate_legacy_toml()?;
        Self::load_from_path(&config_file_path().ok_or(ConfigError::NoHomeDir)?)
    }

    /// The hybrid neural-score weight as `f32` — the type scoring and embedder
    /// consumers need. Config stores it as `f64`; centralize the cast so all
    /// call sites agree (single source of truth, VAL-CONFIG).
    pub fn neural_weight_f32(&self) -> f32 {
        self.search.neural_weight as f32
    }

    /// Process-wide cached config read that stays in sync with the file.
    ///
    /// Reads `leindex.toml` on first access and again whenever the file's
    /// `(mtime, size, inode)` changes, so a config edit reaches long-lived
    /// processes (the MCP daemon lives for `[mcp] idle_timeout_secs`) instead of
    /// being frozen at startup. Before the reload support, an edit — switching
    /// `remote_service`, changing `remote_model`, retuning `[services.gemma]`
    /// args — silently kept serving the old route for the life of the process.
    ///
    /// Use this for a short read. For a snapshot that must survive across
    /// statements — where a concurrent reload would otherwise change the config
    /// mid-operation — hold the handle from [`Self::load_cached_arc`] instead;
    /// it keeps that exact snapshot alive for as long as you need it.
    ///
    /// Cost: one uncontended read lock plus an `Arc` clone. The stat that detects
    /// an edit runs at most once per [`CONFIG_REFRESH_THROTTLE`].
    pub fn load_cached() -> ConfigGuard {
        ConfigGuard(Self::load_cached_arc())
    }

    /// The cached config as an owned, refcounted snapshot.
    ///
    /// Prefer this over [`Self::load_cached`] whenever the value must be read
    /// more than once, or read across an `await`, a lock acquisition, or any
    /// other point where a reload could install a different snapshot. Holding the
    /// `Arc` pins the exact config seen here; the previous snapshot is freed when
    /// its last holder drops it, so no reload leaks.
    pub fn load_cached_arc() -> Arc<LeIndexConfig> {
        if probe_due() {
            if let Some(refreshed) = refresh_cached_config() {
                return refreshed;
            }
        }
        // `probe_due()` returns true while the slot is empty, so a successful
        // `refresh_cached_config` above installs the first snapshot. The only way
        // to reach this clone with an empty slot is a total load failure that was
        // survived by falling back to defaults, which `refresh_cached_config`
        // installs rather than leaves empty.
        cache::installed_snapshot().expect("cached config installed by refresh_cached_config")
    }

    /// The keys that change which vector space an embedding belongs to, or which
    /// endpoint produces it. Compared across a reload so a user who edits one of
    /// these gets told that in-flight caches may now be cold.
    fn route_identity_keys(&self) -> Vec<(&'static str, String)> {
        let embeddings = &self.indexing.embeddings;
        let resolved = embeddings.resolved_remote_with(&self.services);
        vec![
            (
                "[indexing.embeddings] enable_hybrid",
                embeddings.enable_hybrid.to_string(),
            ),
            (
                "[indexing.embeddings] remote_service",
                embeddings.remote_service.clone().unwrap_or_default(),
            ),
            (
                "[indexing.embeddings] remote_provider",
                embeddings.remote_provider.clone(),
            ),
            (
                "[indexing.embeddings] remote_model",
                resolved
                    .as_ref()
                    .and_then(|r| r.model.clone())
                    .unwrap_or_default(),
            ),
            (
                "[indexing.embeddings] endpoint",
                resolved.map(|r| r.endpoint).unwrap_or_default(),
            ),
            ("[neural] model_name", self.neural.model_name.clone()),
            (
                "[neural] execution_provider",
                self.neural.execution_provider.clone(),
            ),
            ("[neural] enabled", self.neural.enabled.to_string()),
        ]
    }

    /// Names of the route keys whose values differ between two configs.
    fn route_identity_changes(&self, other: &LeIndexConfig) -> Vec<&'static str> {
        let before = self.route_identity_keys();
        let after = other.route_identity_keys();
        before
            .into_iter()
            .zip(after)
            .filter_map(|((key, old), (_, new))| (old != new).then_some(key))
            .collect()
    }

    /// Load from explicit path.
    pub fn load_from_path(path: &Path) -> Result<Self, ConfigError> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let contents = std::fs::read_to_string(path)
            .map_err(|e| ConfigError::Io(path.to_path_buf(), e.to_string()))?;

        if path.extension().and_then(|extension| extension.to_str()) == Some("yaml")
            || path.extension().and_then(|extension| extension.to_str()) == Some("yml")
        {
            serde_yaml::from_str(&contents)
                .map_err(|e| ConfigError::Parse(path.to_path_buf(), e.to_string()))
        } else {
            Self::parse_toml(&contents).map_err(|e| ConfigError::Parse(path.to_path_buf(), e))
        }
    }

    fn parse_toml(toml_str: &str) -> Result<Self, String> {
        toml::from_str(toml_str).map_err(|e| format!("Failed to parse leindex.toml: {}", e))
    }

    /// Load or recover from corruption.
    ///
    /// VAL-SETUP-029: Backs up corrupt config and returns defaults.
    pub fn load_or_recover() -> Result<(Self, RecoveryAction), ConfigError> {
        match migrate_legacy_toml() {
            Ok(Some(migrated)) => {
                let path = config_file_path().ok_or(ConfigError::NoHomeDir)?;
                let config = Self::load_from_path(&path)?;
                return Ok((config, RecoveryAction::MigratedFromLegacy(migrated)));
            }
            Ok(None) => {}
            Err(error) => {
                if !matches!(error, ConfigError::Parse(_, _)) {
                    return Err(error);
                }
                let legacy_path = legacy_config_file_path().ok_or(ConfigError::NoHomeDir)?;
                if legacy_path.exists() {
                    let backup_path = legacy_path.with_extension("toml.bak");
                    std::fs::rename(&legacy_path, &backup_path).map_err(|rename_error| {
                        ConfigError::Io(
                            legacy_path.clone(),
                            format!(
                                "config corrupted ({error}); backup rename failed: {rename_error}"
                            ),
                        )
                    })?;
                    return Ok((
                        Self {
                            neural: default_legacy_neural_config(),
                            indexing: default_legacy_indexing_config(),
                            ..Self::default()
                        },
                        RecoveryAction::RecoveredFromCorrupt(backup_path),
                    ));
                }
                return Err(error);
            }
        }
        let config_path = config_file_path().ok_or(ConfigError::NoHomeDir)?;
        if !config_path.exists() {
            return Ok((Self::default(), RecoveryAction::CreatedDefault));
        }
        match Self::load_from_path(&config_path) {
            Ok(config) => Ok((config, RecoveryAction::Loaded)),
            Err(parse_err) => {
                let backup_path = config_path.with_extension("yaml.bak");
                std::fs::rename(&config_path, &backup_path).map_err(|e| {
                    ConfigError::Io(
                        config_path.clone(),
                        format!("config corrupted ({parse_err}); backup rename failed: {e}"),
                    )
                })?;
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
    /// Legacy TOML was migrated to the canonical YAML document.
    MigratedFromLegacy(PathBuf),
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
        config.search.rerank_backend = "system-one".to_string();
        let decoded: LeIndexConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
        assert!(decoded.search.rerank_enabled);
        assert_eq!(decoded.search.rerank_top_n, 80);
        assert_eq!(decoded.search.rerank_backend, "system-one");
    }

    #[test]
    fn test_rerank_backend_semantics() {
        // Default: onnx, rerank off unless rerank_enabled.
        let mut config = LeIndexConfig::default();
        assert_eq!(config.search.rerank_backend, "onnx");
        assert!(!config.search.rerank_effectively_enabled());
        // onnx + rerank_enabled => rerank on.
        config.search.rerank_enabled = true;
        assert!(config.search.rerank_effectively_enabled());
        // system-one implies rerank regardless of rerank_enabled — but only when
        // the backend is constructible: the flag must be on AND the feature
        // compiled in. A config asking for it otherwise reports OFF, matching
        // the dispatch, which refuses to construct the reranker.
        //
        // Driven through `with_flag_override`, which takes the test lock and
        // restores the previous state on unwind: a failing assertion here must
        // not leak a `SystemOne = true` override into every later test in the
        // crate. Not nested — the lock is not reentrant — so the two sides are
        // sequential, and the config is mutated in between while the lock is
        // free.
        config.search.rerank_backend = "system-one".to_string();
        config.search.rerank_enabled = false;
        crate::feature_flags::with_flag_override(
            crate::feature_flags::FeatureFlag::SystemOne,
            false,
            || {
                assert!(
                    !config.search.rerank_effectively_enabled(),
                    "system-one without LEINDEX_FEATURE_SYSTEM_ONE cannot rerank"
                );
            },
        );
        crate::feature_flags::with_flag_override(
            crate::feature_flags::FeatureFlag::SystemOne,
            true,
            || {
                assert_eq!(
                    config.search.rerank_effectively_enabled(),
                    cfg!(feature = "system-one"),
                    "available system-one reranks regardless of rerank_enabled; a build \
                     without the feature still cannot"
                );
            },
        );
        // off is an explicit no-rerank even with rerank_enabled set.
        config.search.rerank_backend = "off".to_string();
        config.search.rerank_enabled = true;
        assert!(!config.search.rerank_effectively_enabled());
        // Unknown strings are treated as the onnx default (rerank_enabled gates).
        config.search.rerank_backend = "bogus".to_string();
        assert!(config.search.rerank_effectively_enabled());
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
                rerank_backend: "onnx".to_string(),
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
                auto_index: false,
            },
            system_one: SystemOneConfig::default(),
            services: std::collections::BTreeMap::new(),
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
    fn test_legacy_toml_migrates_to_v2_yaml_without_dropping_sections() {
        let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let tmp = tempfile::tempdir().unwrap();
        let config_dir = tmp.path().join("config");
        std::fs::create_dir_all(&config_dir).unwrap();
        let legacy_path = config_dir.join("leindex.toml");
        std::fs::write(
            &legacy_path,
            "[neural]\nenabled = false\n\n[indexing.embeddings]\nremote_provider = \"custom\"\n",
        )
        .unwrap();
        unsafe { std::env::set_var(LEINDEX_HOME_ENV, tmp.path()) };

        let (config, action) = LeIndexConfig::load_or_recover().unwrap();
        assert!(!config.neural.enabled);
        assert!(matches!(action, RecoveryAction::MigratedFromLegacy(_)));
        let yaml_path = config_dir.join("config.yaml");
        assert!(yaml_path.exists());
        assert!(!legacy_path.exists());
        assert!(legacy_path.with_extension("toml.migrated").exists());
        let yaml = std::fs::read_to_string(&yaml_path).unwrap();
        assert!(yaml.contains("dal_settings:"));
        assert!(yaml.contains("vector_store:"));
        assert!(yaml.contains("services:"));
        assert!(yaml.contains("  laya:"));
        assert!(yaml.contains("  gemma:"));
        assert!(yaml.contains("remote_provider: custom"));

        let (_, second_action) = LeIndexConfig::load_or_recover().unwrap();
        assert!(matches!(second_action, RecoveryAction::Loaded));
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

    fn service(
        name: &str,
        host: &str,
        port: u16,
        model: Option<&str>,
        alias: Option<&str>,
    ) -> (String, ServiceConfig) {
        (
            name.to_string(),
            ServiceConfig {
                host: host.to_string(),
                port,
                model: model.map(str::to_string),
                alias: alias.map(str::to_string),
                ..ServiceConfig::default()
            },
        )
    }

    #[test]
    fn test_service_base_url_and_served_name() {
        let (_, laya) = service(
            "laya",
            "127.0.0.1",
            8181,
            Some("/models/laya-bf16.gguf"),
            None,
        );
        assert_eq!(laya.base_url(), "http://127.0.0.1:8181");
        // No alias: served name falls back to the model file stem.
        assert_eq!(laya.served_name().as_deref(), Some("laya-bf16"));
        let (_, aliased) = service("e", "127.0.0.1", 8080, None, Some("embeddinggemma-2"));
        assert_eq!(aliased.served_name().as_deref(), Some("embeddinggemma-2"));
    }

    /// A resolved route must carry the provider's per-request input ceiling, or
    /// the index builder sends a throughput-tuned batch into an API that rejects
    /// it: Gemini answers anything over 100 with
    /// `BatchEmbedContentsRequest.requests: at most 100 requests can be in one
    /// batch`, which 128-document batches hit on every request.
    #[test]
    fn test_resolved_remote_carries_the_preset_batch_limit() {
        let services = std::collections::BTreeMap::new();
        let gemini = EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "gemini".into(),
            ..EmbeddingConfig::default()
        };
        assert_eq!(
            gemini.resolved_remote_with(&services).unwrap().batch_limit,
            Some(100),
            "the gemini preset must cap a request at the API's documented maximum"
        );
        // An explicit override wins over the preset either way.
        let lowered = EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "gemini".into(),
            remote_batch_limit: Some(25),
            ..EmbeddingConfig::default()
        };
        assert_eq!(
            lowered.resolved_remote_with(&services).unwrap().batch_limit,
            Some(25)
        );
        // A provider with no preset and no override resolves no cap rather than
        // inventing one: an over-large guess fails whole batches.
        let custom = EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "custom".into(),
            remote_endpoint: Some("http://127.0.0.1:9/v1".into()),
            ..EmbeddingConfig::default()
        };
        assert_eq!(
            custom.resolved_remote_with(&services).unwrap().batch_limit,
            None
        );
    }

    #[test]
    fn test_resolved_remote_endpoint_ladder() {
        let mut services: std::collections::BTreeMap<String, ServiceConfig> =
            std::collections::BTreeMap::new();
        services.insert(
            "embed".to_string(),
            ServiceConfig {
                port: 8123,
                ..ServiceConfig::default()
            },
        );
        // Service derives the endpoint when no explicit one is set.
        let via_service = EmbeddingConfig {
            enable_hybrid: true,
            remote_service: Some("embed".into()),
            ..EmbeddingConfig::default()
        };
        let resolved = via_service.resolved_remote_with(&services).unwrap();
        assert_eq!(resolved.endpoint, "http://127.0.0.1:8123/v1");
        // Explicit endpoint beats the service.
        let explicit_wins = EmbeddingConfig {
            enable_hybrid: true,
            remote_service: Some("embed".into()),
            remote_endpoint: Some("http://explicit:9/v1".into()),
            ..EmbeddingConfig::default()
        };
        assert_eq!(
            explicit_wins
                .resolved_remote_with(&services)
                .unwrap()
                .endpoint,
            "http://explicit:9/v1"
        );
        // Preset default applies when neither is set.
        let preset_only = EmbeddingConfig {
            enable_hybrid: true,
            ..EmbeddingConfig::default()
        };
        assert_eq!(
            preset_only
                .resolved_remote_with(&services)
                .unwrap()
                .endpoint,
            EMBEDDINGGEMMA2_DEFAULT_ENDPOINT
        );
    }

    #[test]
    fn test_resolved_remote_model_falls_back_to_service() {
        let mut services: std::collections::BTreeMap<String, ServiceConfig> =
            std::collections::BTreeMap::new();
        services.insert(
            "embed".to_string(),
            ServiceConfig {
                model: Some("/models/embeddinggemma-2-Q8_0.gguf".into()),
                ..ServiceConfig::default()
            },
        );
        let cfg = EmbeddingConfig {
            enable_hybrid: true,
            remote_service: Some("embed".into()),
            remote_provider: "custom".into(),
            ..EmbeddingConfig::default()
        };
        // Custom preset (no model fill) + no remote_model -> service stem.
        assert_eq!(
            cfg.resolved_remote_with(&services)
                .unwrap()
                .model
                .as_deref(),
            Some("embeddinggemma-2-Q8_0")
        );
    }

    /// A `custom` provider that names an EmbeddingGemma2 model must get the
    /// task templates. It has no preset row, so `remote_model` is the only
    /// identification available; reading only `remote_provider` left both
    /// templates `None` and embedded the whole corpus with no task prefix.
    #[test]
    fn test_custom_provider_serving_embeddinggemma2_gets_task_templates() {
        let cfg = EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "custom".into(),
            remote_endpoint: Some("https://api.example.com/v1".into()),
            remote_model: Some("embeddinggemma-2".into()),
            ..EmbeddingConfig::default()
        };
        let resolved = cfg.resolved_remote_with(&Default::default()).unwrap();
        assert_eq!(
            resolved.doc_template.as_deref(),
            Some(EMBEDDINGGEMMA2_DOC_TEMPLATE)
        );
        assert_eq!(
            resolved.query_template.as_deref(),
            Some(EMBEDDINGGEMMA2_QUERY_TEMPLATE)
        );
        // The label and the templates now agree instead of contradicting.
        assert_eq!(cfg.remote_preset_label(), "embeddinggemma-2");

        // Host spellings: vendor prefix and a quantized file stem.
        for model in [
            "text-embedding-embeddinggemma-2",
            "embeddinggemma2",
            "embeddinggemma-2-Q8_0",
        ] {
            let spelled = EmbeddingConfig {
                remote_model: Some(model.into()),
                ..cfg.clone()
            };
            assert!(
                spelled.embeddinggemma2_preset_active(),
                "expected {model} to select the embeddinggemma-2 templates"
            );
        }
    }

    /// A recognized preset owns its own templating. A contradictory
    /// `remote_model` on it must NOT silently swap in Gemma templates.
    #[test]
    fn test_known_preset_is_not_overridden_by_model_name() {
        let cfg = EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "openai".into(),
            remote_model: Some("embeddinggemma-2".into()),
            ..EmbeddingConfig::default()
        };
        assert!(!cfg.embeddinggemma2_preset_active());
        let resolved = cfg.resolved_remote_with(&Default::default()).unwrap();
        assert!(resolved.doc_template.is_none());
        assert!(resolved.query_template.is_none());
    }

    /// A `custom` provider serving some other model stays untemplated.
    #[test]
    fn test_custom_provider_with_unrelated_model_stays_untemplated() {
        let cfg = EmbeddingConfig {
            enable_hybrid: true,
            remote_provider: "custom".into(),
            remote_endpoint: Some("https://api.example.com/v1".into()),
            remote_model: Some("some-other-embedder".into()),
            ..EmbeddingConfig::default()
        };
        assert!(!cfg.embeddinggemma2_preset_active());
        let resolved = cfg.resolved_remote_with(&Default::default()).unwrap();
        assert!(resolved.doc_template.is_none());
        assert!(resolved.query_template.is_none());
    }

    #[test]
    fn test_system_one_preset_overlay_swaps_provider() {
        let mut cfg = SystemOneConfig {
            provider: "laya".into(),
            mode: "enforce".into(),
            ..SystemOneConfig::default()
        };
        cfg.presets.insert(
            "openai-prod".into(),
            SystemOnePreset {
                provider: Some("openai".into()),
                model: Some("gpt-6-luna".into()),
                api_key_env: Some("OPENAI_API_KEY".into()),
                ..SystemOnePreset::default()
            },
        );
        cfg.active_preset = Some("openai-prod".into());
        let effective = cfg.effective();
        assert_eq!(effective.provider, "openai");
        assert_eq!(effective.model.as_deref(), Some("gpt-6-luna"));
        // Section-level fields survive when the preset leaves them unset.
        assert_eq!(effective.mode, "enforce");
        // Clearing the preset (or naming a missing one) reverts to laya.
        cfg.active_preset = Some("nope".into());
        assert_eq!(cfg.effective().provider, "laya");
    }

    #[test]
    fn test_system_one_provider_change_clears_unset_provider_fields() {
        let mut cfg = SystemOneConfig {
            provider: "laya".into(),
            model: Some("local-model".into()),
            base_url: Some("http://127.0.0.1:8000".into()),
            service: Some("laya".into()),
            api_key_env: Some("LOCAL_API_KEY".into()),
            mode: "enforce".into(),
            active_preset: Some("remote".into()),
            ..SystemOneConfig::default()
        };
        cfg.presets.insert(
            "remote".into(),
            SystemOnePreset {
                provider: Some("openai".into()),
                ..SystemOnePreset::default()
            },
        );
        let effective = cfg.effective();
        assert_eq!(effective.provider, "openai");
        assert_eq!(effective.model, None);
        assert_eq!(effective.base_url, None);
        assert_eq!(effective.service, None);
        assert_eq!(effective.api_key_env, None);
        assert_eq!(effective.mode, "enforce");

        let preset = cfg.presets.get_mut("remote").unwrap();
        preset.model = Some("remote-model".into());
        preset.base_url = Some("https://remote.example".into());
        preset.service = Some("remote-service".into());
        preset.api_key_env = Some("REMOTE_API_KEY".into());
        let effective = cfg.effective();
        assert_eq!(effective.model.as_deref(), Some("remote-model"));
        assert_eq!(
            effective.base_url.as_deref(),
            Some("https://remote.example")
        );
        assert_eq!(effective.service.as_deref(), Some("remote-service"));
        assert_eq!(effective.api_key_env.as_deref(), Some("REMOTE_API_KEY"));
    }

    #[test]
    fn test_system_one_same_provider_presets_inherit_unset_fields() {
        let mut cfg = SystemOneConfig {
            provider: "jev".into(),
            model: Some("base-model".into()),
            base_url: Some("https://base.example".into()),
            service: Some("base-service".into()),
            api_key_env: Some("BASE_API_KEY".into()),
            active_preset: Some("same".into()),
            ..SystemOneConfig::default()
        };
        for provider in [None, Some("jev"), Some(" JEV "), Some("typesafe")] {
            cfg.presets.insert(
                "same".into(),
                SystemOnePreset {
                    provider: provider.map(str::to_string),
                    ..SystemOnePreset::default()
                },
            );
            let effective = cfg.effective();
            assert_eq!(effective.model, cfg.model);
            assert_eq!(effective.base_url, cfg.base_url);
            assert_eq!(effective.service, cfg.service);
            assert_eq!(effective.api_key_env, cfg.api_key_env);
        }
    }

    #[test]
    fn test_service_base_url_brackets_ipv6_host() {
        let v6 = ServiceConfig {
            host: "::1".into(),
            port: 8080,
            ..ServiceConfig::default()
        };
        assert_eq!(v6.base_url(), "http://[::1]:8080");
        let v4 = ServiceConfig {
            host: "127.0.0.1".into(),
            port: 8080,
            ..ServiceConfig::default()
        };
        assert_eq!(v4.base_url(), "http://127.0.0.1:8080");
    }

    /// The reload contract: a config edit must be observed by a process that
    /// already cached a config. This is the regression guard for the bug where
    /// `load_cached()` was a `OnceLock` and a long-lived MCP daemon kept serving
    /// the startup route forever after `leindex.toml` changed.
    ///
    /// Serialised on the module [`ENV_LOCK`] because it mutates `LEINDEX_HOME`,
    /// which is process-global and read by every other config-path test.
    #[test]
    fn test_load_cached_observes_a_config_edit() {
        let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
        let sandbox = cache::CacheSandbox::new();

        let home = tempfile::tempdir().expect("config home");
        let config_dir = home.path().join("config");
        std::fs::create_dir_all(&config_dir).expect("config dir");
        let config_path = config_dir.join("config.yaml");

        // SAFETY: serialised by ENV_LOCK; no other test reads LEINDEX_HOME
        // concurrently while this guard is held.
        unsafe { std::env::set_var(LEINDEX_HOME_ENV, home.path()) };

        std::fs::write(
            &config_path,
            "indexing:\n  embeddings:\n    enable_hybrid: true\n    remote_service: gemma\n    remote_provider: embeddinggemma-2\n",
        )
        .expect("initial config");

        let first = LeIndexConfig::load_cached();
        assert_eq!(
            first.indexing.embeddings.remote_service.as_deref(),
            Some("gemma"),
            "initial load must see the written config"
        );

        // Rewrite with a different route, then force the throttle open. Order
        // matters: the write must land first so the stamp read during the probe
        // sees the new file.
        std::fs::write(
            &config_path,
            "indexing:\n  embeddings:\n    enable_hybrid: true\n    remote_service: other\n    remote_provider: custom\n",
        )
        .expect("edited config");
        sandbox.reopen_throttle();

        let second = LeIndexConfig::load_cached();
        assert_eq!(
            second.indexing.embeddings.remote_service.as_deref(),
            Some("other"),
            "load_cached must observe the edit instead of serving the cached config"
        );

        // The snapshot taken before the reload must still be valid — a reload
        // must not free a config another caller is holding — and must still
        // carry the value it was loaded with.
        assert_eq!(
            first.indexing.embeddings.remote_service.as_deref(),
            Some("gemma"),
            "a snapshot taken before a reload must stay valid and unchanged"
        );

        // A malformed rewrite must NOT drop the process to defaults mid-session.
        std::fs::write(&config_path, "not: [valid yaml").expect("broken config");
        sandbox.reopen_throttle();
        let third = LeIndexConfig::load_cached();
        assert_eq!(
            third.indexing.embeddings.remote_service.as_deref(),
            Some("other"),
            "a parse failure must keep the last good config, not fall back to defaults"
        );
    }

    /// Route-affecting keys are exactly what earns the "caches may be cold" WARN.
    #[test]
    fn test_route_identity_changes_names_the_route_keys() {
        let base = LeIndexConfig::default();
        assert!(
            base.route_identity_changes(&base).is_empty(),
            "a config must not report changes against itself"
        );

        let mut changed = base.clone();
        changed.indexing.embeddings.remote_service = Some("gemma".to_string());
        let keys = base.route_identity_changes(&changed);
        assert!(
            keys.contains(&"[indexing.embeddings] remote_service"),
            "changing remote_service must be reported, got {keys:?}"
        );

        let mut model_changed = base.clone();
        model_changed.neural.model_name = "some-other-model".to_string();
        let keys = base.route_identity_changes(&model_changed);
        assert!(
            keys.contains(&"[neural] model_name"),
            "changing the local model must be reported, got {keys:?}"
        );

        // A purely cosmetic section must not look like a route change.
        let mut unrelated = base.clone();
        unrelated.mcp.idle_timeout_secs = unrelated.mcp.idle_timeout_secs.wrapping_add(1);
        assert!(
            base.route_identity_changes(&unrelated).is_empty(),
            "unrelated edits must not be reported as route changes"
        );
    }
}
