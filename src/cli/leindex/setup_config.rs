// `leindex config` — typed inspection and surgical editing of leindex.toml.
//
// Contract (system-one-gate split):
// - [`validate`] is PURE: structural invariants only. It never touches the
//   filesystem, never requires a model on disk, and never requires an API
//   secret (an env-var *name* is data, not a credential; readiness probing is
//   `leindex setup --check`'s job, not this module's).
// - Config reads are pure: `LeIndexConfig::load_from_path` (config.rs:935)
//   only. This module never calls `load_or_recover`.
// - Set/Unset edit the TOML document generically, then deserialize it into
//   the typed config and run [`validate`] BEFORE writing. Unknown keys are
//   rejected by an explicit schema map (never silently ignored), so the
//   typed re-serialization in `save` can never silently drop parts of a
//   user's file. `config unset` is the repair path: when the edited
//   document still fails validation only because of dangling
//   cross-references that predate the edit, strict validation is deferred
//   (see `commit_table_at`) so the documented escape hatch cannot dead-end
//   on a second dangling reference.
// - Secret redaction: `indexing.embeddings.remote_api_key` is replaced with
//   `[REDACTED]` for every output path (Show/Get).
//
// Invariant sources are runtime consumers, not guesses:
// - search_mode: config.rs `query_type_for_mode` ("hybrid"|"text"|"neural")
// - neural.execution_provider: setup.rs:248 + neural_publish.rs:98
//   ("cpu"|"auto"|"cuda"|"rocm"|"migraphx"|"coreml")
// - mcp.prewarm: cli/registry.rs:425-429 ("full"|"graph"|"off"|"false"|
//   "none"|"0", case-insensitive)
// - system_one mode/provider: `Mode::parse` / `Provider::parse` in
//   cli/mcp/system_one. That module is `#[cfg(feature = "system-one")]`;
//   the section is inert without the feature, so those enum checks are
//   feature-gated too. Structural checks (URLs, references, ranges) are not.
// - probabilities: neural_weight, fragment_weight, relevant_min,
//   instructions_max, tau_apply must lie in [0, 1].
// - URLs: remote_endpoint and every base_url must parse as http(s) via
//   `reqwest::Url` (reqwest is a hard dep of the cli feature via search).
// - nonzero invariants: indexing.batch_size (chunking), every service
//   port (a port-0 service can never be started or resolved), service
//   start_timeout_secs, and system_one.timeout_ms.

use anyhow::{Context, Result, bail};
use clap::Subcommand;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::cli::leindex::setup::SetupError;
use crate::config::{LeIndexConfig, config_file_path};

/// Placeholder written in place of a redacted secret.
const REDACTED: &str = "[REDACTED]";

const SEARCH_MODES: &[&str] = &["hybrid", "text", "neural"];
const EXECUTION_PROVIDERS: &[&str] = &["cpu", "auto", "cuda", "rocm", "migraphx", "coreml"];
const PREWARM_MODES: &[&str] = &["full", "graph", "off", "false", "none", "0"];

// Explicit schema map for dotted-key path validation. The drift test in
// setup_config_test.rs asserts these sets against the serde-serialized
// defaults, so a field added to config.rs breaks a test here instead of
// being silently rejected (or silently dropped) by `config set`.
const TOP_SECTIONS: &[&str] = &[
    "neural",
    "search",
    "indexing",
    "mcp",
    "system_one",
    "services",
];
const NEURAL_FIELDS: &[&str] = &[
    "enabled",
    "execution_provider",
    "ort_dylib_path",
    "ort_version",
    "model_dir",
    "model_name",
];
const SEARCH_FIELDS: &[&str] = &[
    "search_mode",
    "neural_weight",
    "rerank_enabled",
    "rerank_top_n",
    "fragment_index_enabled",
    "fragment_max_bytes",
    "fragment_weight",
    "fragment_orphan_enabled",
    "fragment_naive_fallback",
];
const INDEXING_FIELDS: &[&str] = &["batch_size", "max_files", "embeddings"];
const EMBEDDINGS_FIELDS: &[&str] = &[
    "enable_hybrid",
    "remote_service",
    "remote_provider",
    "remote_endpoint",
    "remote_api_key",
    "remote_api_key_env",
    "remote_model",
    "remote_dimension",
    "remote_doc_template",
    "remote_query_template",
    "onnx_model_path",
];
const MCP_FIELDS: &[&str] = &["idle_timeout_secs", "engine_max_idle_secs", "prewarm"];
const SYSTEM_ONE_FIELDS: &[&str] = &[
    "mode",
    "provider",
    "base_url",
    "model",
    "api_key_env",
    "timeout_ms",
    "pre_gate",
    "post_gate",
    "relevant_min",
    "instructions_max",
    "top_n",
    "tau_apply",
    "service",
    "active_preset",
    "presets",
];
const SERVICE_FIELDS: &[&str] = &[
    "binary",
    "model",
    "alias",
    "host",
    "port",
    "extra_args",
    "auto_start",
    "start_timeout_secs",
];
const PRESET_FIELDS: &[&str] = &["provider", "model", "base_url", "service", "api_key_env"];

/// Fields omitted from the serde-serialized default (skipped when `None`/
/// empty), so `config set` must accept them even though reflection of the
/// default TOML does not show them.
fn optional_keys_at(path: &str) -> &'static [&'static str] {
    match path {
        "neural" => &["ort_dylib_path", "ort_version"],
        "indexing.embeddings" => &[
            "remote_service",
            "remote_endpoint",
            "remote_api_key",
            "remote_api_key_env",
            "remote_model",
            "remote_dimension",
            "remote_doc_template",
            "remote_query_template",
            "onnx_model_path",
        ],
        "system_one" => &[
            "base_url",
            "model",
            "api_key_env",
            "service",
            "active_preset",
            "presets",
        ],
        _ => &[],
    }
}

fn section_field(section: &str, field: &str) -> bool {
    let fields = match section {
        "neural" => NEURAL_FIELDS,
        "search" => SEARCH_FIELDS,
        "indexing" => INDEXING_FIELDS,
        "mcp" => MCP_FIELDS,
        "system_one" => SYSTEM_ONE_FIELDS,
        _ => return false,
    };
    fields.contains(&field)
}

/// Whether a dotted key path exists in the LeIndexConfig schema. Dynamic
/// sections (`services.<name>`, `system_one.presets.<name>`) accept any
/// instance name; name safety is enforced by [`validate`] before saving.
fn is_known_path(segments: &[&str]) -> bool {
    match segments {
        [section] => TOP_SECTIONS.contains(section),
        [section, field] => {
            if *section == "services" {
                return true;
            }
            section_field(section, field) || optional_keys_at(section).contains(field)
        }
        ["indexing", "embeddings", field] => {
            EMBEDDINGS_FIELDS.contains(field)
                || optional_keys_at("indexing.embeddings").contains(field)
        }
        ["services", _name, field] => SERVICE_FIELDS.contains(field),
        ["system_one", "presets", _name] => true,
        ["system_one", "presets", _name, field] => PRESET_FIELDS.contains(field),
        _ => false,
    }
}

/// Structural invariants for a fully-deserialized config. Pure: same input,
/// same result, no I/O. Does NOT require model files or API secrets.
///
/// Validation failures reuse [`SetupError::Conflict`] as the string-carrying
/// variant (this split cannot add variants to setup.rs); every message is
/// prefixed so callers can distinguish them from flag conflicts.
pub fn validate(config: &LeIndexConfig) -> Result<(), SetupError> {
    validate_search(config)?;
    validate_neural(config)?;
    validate_indexing(config)?;
    validate_mcp(config)?;
    validate_system_one(config)?;
    validate_services(config)?;
    if config.system_one.timeout_ms == 0 {
        return Err(invalid(
            "system_one.timeout_ms must be non-zero (requests would time out instantly)"
                .to_string(),
        ));
    }
    if config.system_one.top_n < 2 {
        return Err(invalid(
            "system_one.top_n must be at least 2 (fewer judges no meaningful ranking)".to_string(),
        ));
    }
    Ok(())
}

// CCN-gate extraction: wraps a message in the shared `invalid config:` Conflict error.
fn invalid(msg: String) -> SetupError {
    SetupError::Conflict {
        message: format!("invalid config: {msg}"),
    }
}

// CCN-gate extraction: [search] mode enum and probability bounds.
fn validate_search(config: &LeIndexConfig) -> Result<(), SetupError> {
    if !SEARCH_MODES.contains(&config.search.search_mode.as_str()) {
        return Err(invalid(format!(
            "search.search_mode {:?} is not one of {SEARCH_MODES:?}",
            config.search.search_mode
        )));
    }
    if !(0.0..=1.0).contains(&config.search.neural_weight) {
        return Err(invalid(format!(
            "search.neural_weight {} is outside [0, 1]",
            config.search.neural_weight
        )));
    }
    if !(0.0..=1.0).contains(&config.search.fragment_weight) {
        return Err(invalid(format!(
            "search.fragment_weight {} is outside [0, 1]",
            config.search.fragment_weight
        )));
    }
    Ok(())
}

// CCN-gate extraction: [neural] execution provider enum.
fn validate_neural(config: &LeIndexConfig) -> Result<(), SetupError> {
    if !EXECUTION_PROVIDERS.contains(&config.neural.execution_provider.as_str()) {
        return Err(invalid(format!(
            "neural.execution_provider {:?} is not one of {EXECUTION_PROVIDERS:?}",
            config.neural.execution_provider
        )));
    }
    Ok(())
}

// CCN-gate extraction: [indexing] batch size, then [indexing.embeddings].
fn validate_indexing(config: &LeIndexConfig) -> Result<(), SetupError> {
    if config.indexing.batch_size == 0 {
        return Err(invalid("indexing.batch_size must be non-zero".to_string()));
    }
    validate_embeddings(config)
}

// CCN-gate extraction: [indexing.embeddings] hybrid route, endpoint, env name and service reference.
fn validate_embeddings(config: &LeIndexConfig) -> Result<(), SetupError> {
    let embeddings = &config.indexing.embeddings;
    if embeddings.enable_hybrid && embeddings.resolved_remote_with(&config.services).is_none() {
        return Err(invalid(
            "indexing.embeddings.enable_hybrid is true but no endpoint resolves \
             (set remote_endpoint, remote_service, or a recognized preset)"
                .to_string(),
        ));
    }
    if let Some(endpoint) = non_empty(embeddings.remote_endpoint.as_deref()) {
        validate_url("indexing.embeddings.remote_endpoint", endpoint)?;
    }
    // Validated raw (no non_empty filter): Some("") is not a valid env NAME.
    if let Some(name) = embeddings.remote_api_key_env.as_deref() {
        check_env_var_name("indexing.embeddings.remote_api_key_env", name)?;
    }
    if let Some(service) = non_empty(embeddings.remote_service.as_deref()) {
        if CrossRef::EmbeddingService(service.to_string()).is_dangling(config) {
            return Err(invalid(format!(
                "indexing.embeddings.remote_service references missing [services.{service}] \
                 (add it, or run `leindex config unset indexing.embeddings.remote_service`)"
            )));
        }
    }
    Ok(())
}

// CCN-gate extraction: [mcp] prewarm mode (case-insensitive).
fn validate_mcp(config: &LeIndexConfig) -> Result<(), SetupError> {
    let prewarm = config.mcp.prewarm.trim().to_ascii_lowercase();
    if !PREWARM_MODES.contains(&prewarm.as_str()) {
        return Err(invalid(format!(
            "mcp.prewarm {:?} is not one of {PREWARM_MODES:?} (case-insensitive)",
            config.mcp.prewarm
        )));
    }
    Ok(())
}

// CCN-gate extraction: [system_one] section; structural checks are feature-independent, enum checks are not.
fn validate_system_one(config: &LeIndexConfig) -> Result<(), SetupError> {
    validate_system_one_refs(config)?;
    validate_system_one_intervals(config)?;
    validate_presets(config)?;
    #[cfg(feature = "system-one")]
    validate_system_one_modes(config)?;
    Ok(())
}

// CCN-gate extraction: [system_one] URL, env name, service and active_preset references.
fn validate_system_one_refs(config: &LeIndexConfig) -> Result<(), SetupError> {
    if let Some(base_url) = non_empty(config.system_one.base_url.as_deref()) {
        validate_url("system_one.base_url", base_url)?;
    }
    // Validated raw (no non_empty filter): Some("") is not a valid env NAME.
    if let Some(name) = config.system_one.api_key_env.as_deref() {
        check_env_var_name("system_one.api_key_env", name)?;
    }
    if let Some(service) = non_empty(config.system_one.service.as_deref()) {
        if CrossRef::SystemService(service.to_string()).is_dangling(config) {
            return Err(invalid(format!(
                "system_one.service references missing [services.{service}] \
                 (add it, or run `leindex config unset system_one.service`)"
            )));
        }
    }
    if let Some(preset) = non_empty(config.system_one.active_preset.as_deref()) {
        if CrossRef::ActivePreset(preset.to_string()).is_dangling(config) {
            return Err(invalid(format!(
                "system_one.active_preset references missing [system_one.presets.{preset}] \
                 (add it, or run `leindex config unset system_one.active_preset`)"
            )));
        }
    }
    Ok(())
}

// CCN-gate extraction: [system_one] relevance, instruction and apply thresholds in [0, 1].
fn validate_system_one_intervals(config: &LeIndexConfig) -> Result<(), SetupError> {
    for (field, value) in [
        ("relevant_min", config.system_one.relevant_min),
        ("instructions_max", config.system_one.instructions_max),
        ("tau_apply", config.system_one.tau_apply),
    ] {
        if !(0.0..=1.0).contains(&value) {
            return Err(invalid(format!(
                "system_one.{field} {value} is outside [0, 1]"
            )));
        }
    }
    Ok(())
}

// CCN-gate extraction: [system_one.presets.<name>] name, URL, env name, service and provider.
fn validate_presets(config: &LeIndexConfig) -> Result<(), SetupError> {
    for (name, preset) in &config.system_one.presets {
        check_instance_name("system_one.presets", name)?;
        if let Some(base_url) = non_empty(preset.base_url.as_deref()) {
            validate_url(&format!("system_one.presets.{name}.base_url"), base_url)?;
        }
        // Validated raw (no non_empty filter): Some("") is not a valid env NAME.
        if let Some(env_name) = preset.api_key_env.as_deref() {
            check_env_var_name(&format!("system_one.presets.{name}.api_key_env"), env_name)?;
        }
        if let Some(service) = non_empty(preset.service.as_deref()) {
            if CrossRef::PresetService(service.to_string()).is_dangling(config) {
                return Err(invalid(format!(
                    "system_one.presets.{name}.service references missing [services.{service}] \
                     (add it, or run `leindex config unset system_one.presets.{name}.service`)"
                )));
            }
        }
        #[cfg(feature = "system-one")]
        {
            if let Some(provider) = non_empty(preset.provider.as_deref()) {
                if crate::cli::mcp::system_one::types::Provider::parse(provider).is_none() {
                    return Err(invalid(format!(
                        "system_one.presets.{name}.provider {provider:?} is not one of \
                         laya/jev/typesafe/openai"
                    )));
                }
            }
        }
    }
    Ok(())
}

// CCN-gate extraction: [system_one] mode and provider enums (parsers live behind the system-one feature).
#[cfg(feature = "system-one")]
fn validate_system_one_modes(config: &LeIndexConfig) -> Result<(), SetupError> {
    if crate::cli::mcp::system_one::Mode::parse(&config.system_one.mode).is_none() {
        return Err(invalid(format!(
            "system_one.mode {:?} is not one of off/shadow/enforce",
            config.system_one.mode
        )));
    }
    if crate::cli::mcp::system_one::types::Provider::parse(&config.system_one.provider).is_none() {
        return Err(invalid(format!(
            "system_one.provider {:?} is not one of laya/jev/typesafe/openai",
            config.system_one.provider
        )));
    }
    Ok(())
}

// CCN-gate extraction: [services.<name>] safe instance name, non-zero port and start timeout.
fn validate_services(config: &LeIndexConfig) -> Result<(), SetupError> {
    for (name, service) in &config.services {
        check_instance_name("services", name)?;
        if service.port == 0 {
            return Err(invalid(format!(
                "services.{name}.port must be non-zero (a port-0 service can never \
                 be started or resolved)"
            )));
        }
        if service.start_timeout_secs == 0 {
            return Err(invalid(format!(
                "services.{name}.start_timeout_secs must be non-zero (the startup wait \
                 would time out before the service could ever answer)"
            )));
        }
    }
    Ok(())
}

fn non_empty(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|v| !v.is_empty())
}

/// Environment variable NAMES are pure identifiers: `[A-Za-z_][A-Za-z0-9_]*`.
/// Only the name is validated here — whether the variable is set in the
/// environment is a runtime concern, not a config-file invariant.
fn check_env_var_name(field: &str, name: &str) -> Result<(), SetupError> {
    let mut chars = name.chars();
    let valid = matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
    if !valid {
        return Err(SetupError::Conflict {
            message: format!(
                "invalid config: {field} {name:?} is not a valid environment variable name \
                 ([A-Za-z_][A-Za-z0-9_]*)"
            ),
        });
    }
    Ok(())
}

fn check_instance_name(section: &str, name: &str) -> Result<(), SetupError> {
    let safe = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-');
    if !safe {
        return Err(SetupError::Conflict {
            message: format!(
                "invalid config: {section} name {name:?} must be a safe filename \
                 ([A-Za-z0-9_-], non-empty)"
            ),
        });
    }
    Ok(())
}

fn validate_url(field: &str, value: &str) -> Result<(), SetupError> {
    let err = |msg: String| SetupError::Conflict {
        message: format!("invalid config: {field} {value:?}: {msg}"),
    };
    let url = reqwest::Url::parse(value).map_err(|e| err(e.to_string()))?;
    if url.scheme() != "http" && url.scheme() != "https" {
        return Err(err("scheme must be http or https".to_string()));
    }
    if url.host_str().is_none_or(|host| host.is_empty()) {
        return Err(err("URL has no host".to_string()));
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(err(
            "URL must not embed credentials, query, or fragment; configure authentication separately".to_string(),
        ));
    }
    Ok(())
}

/// `leindex config` subcommands.
#[derive(Debug, Subcommand)]
pub enum ConfigAction {
    /// Print the effective configuration with secrets redacted.
    Show,
    /// Print one value by dotted key path (e.g. mcp.prewarm).
    Get {
        /// Dotted key path, e.g. services.gemma.port.
        key: String,
    },
    /// Set one value by dotted key path; value is parsed as TOML
    /// (quote strings: --value '"custom"').
    Set {
        /// Dotted key path, e.g. search.neural_weight.
        key: String,
        /// TOML value to assign.
        value: String,
    },
    /// Remove one key (or section) by dotted key path.
    Unset {
        /// Dotted key path to remove.
        key: String,
    },
    /// Validate the configuration file's structure and invariants.
    Validate,
    /// Print the configuration file path.
    Path,
}

/// Run one config action against the resolved LEINDEX_HOME config file.
pub fn run(action: ConfigAction) -> Result<()> {
    match action {
        ConfigAction::Show => {
            let path = config_path()?;
            println!("{}", show_at(&path)?);
        }
        ConfigAction::Get { key } => {
            let path = config_path()?;
            println!("{}", get_at(&path, &key)?);
        }
        ConfigAction::Set { key, value } => {
            let path = config_path()?;
            let saved = apply_set_at(&path, &key, &value)?;
            println!("set {key} in {}", saved.display());
        }
        ConfigAction::Unset { key } => {
            let path = config_path()?;
            let saved = apply_unset_at(&path, &key)?;
            println!("removed {key} from {}", saved.display());
        }
        ConfigAction::Validate => {
            let path = config_path()?;
            let config = LeIndexConfig::load_from_path(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            validate(&config)?;
            println!("OK: {}", path.display());
        }
        ConfigAction::Path => {
            println!("{}", config_path()?.display());
        }
    }
    Ok(())
}

fn config_path() -> Result<PathBuf> {
    config_file_path().ok_or_else(|| {
        anyhow::anyhow!("cannot resolve the LeIndex home directory; set LEINDEX_HOME or HOME")
    })
}

// ── Path-parameterized internals (tests use these; run() supplies the
//    LEINDEX_HOME-derived path in production). ────────────────────────────

/// Rendered TOML of the config at `path`, with `remote_api_key` redacted.
fn show_at(path: &Path) -> Result<String> {
    let config = load_config_at(path)?;
    let value = redacted_value(&config)?;
    toml::to_string_pretty(&value).context("failed to render config")
}

/// Rendered TOML of one dotted key from the config at `path` (redacted).
fn get_at(path: &Path, key: &str) -> Result<String> {
    let segments = key_segments(key)?;
    if !is_known_path(&segments) {
        bail!("unknown config key '{key}': not a section or field of LeIndexConfig");
    }
    let config = load_config_at(path)?;
    let value = redacted_value(&config)?;
    let mut current = &value;
    for segment in &segments {
        current = current
            .get(segment)
            .ok_or_else(|| anyhow::anyhow!("key '{key}' is not set"))?;
    }
    Ok(current.to_string())
}

/// Set one dotted key, then typed-deserialize + validate + save. The file is
/// only rewritten when the full edited config deserializes and validates.
fn apply_set_at(path: &Path, key: &str, value: &str) -> Result<PathBuf> {
    let segments = key_segments(key)?;
    if !is_known_path(&segments) {
        bail!(
            "unknown config key '{key}': not a section or field of LeIndexConfig \
             (see 'leindex config show' for the schema)"
        );
    }
    let new_value =
        parse_toml_value(value).with_context(|| format!("invalid TOML value for '{key}'"))?;
    let mut root = load_table_at(path)?;
    let table = ensure_table_path(&mut root, &segments[..segments.len() - 1], key)?;
    table.insert(segments[segments.len() - 1].to_string(), new_value);
    commit_table_at(path, &root, key, None)
}

/// Remove one dotted key (or section), then typed-deserialize + save. Unset
/// is the repair path for dangling cross-references — see
/// [`commit_table_at`] for the deferred-validation rule.
fn apply_unset_at(path: &Path, key: &str) -> Result<PathBuf> {
    let segments = key_segments(key)?;
    if !is_known_path(&segments) {
        bail!("unknown config key '{key}': not a section or field of LeIndexConfig");
    }
    if !path.exists() {
        bail!("config file does not exist: {}", path.display());
    }
    let mut root = load_table_at(path)?;
    // Dangling references BEFORE the edit: the repair path only defers
    // validation for dangling references it did not create (see
    // `commit_table_at`). A pre-edit document that fails to deserialize
    // surfaces at commit time; an empty set here just disables deferral.
    let pre_dangling = toml::to_string(&root)
        .ok()
        .and_then(|rendered| {
            let pre: Result<LeIndexConfig, _> = toml::from_str(&rendered);
            pre.ok().map(|pre| dangling_references(&pre))
        })
        .unwrap_or_default();
    let mut table = &mut root;
    for segment in &segments[..segments.len() - 1] {
        table = match table.get_mut(*segment) {
            Some(value) if value.is_table() => value.as_table_mut().expect("checked table"),
            Some(_) => bail!("'{key}': '{segment}' exists but is not a table"),
            None => bail!("key '{key}' is not set"),
        };
    }
    let last = segments[segments.len() - 1];
    if table.remove(last).is_none() {
        bail!("key '{key}' is not set");
    }
    commit_table_at(path, &root, key, Some(&pre_dangling))
}

fn key_segments(key: &str) -> Result<Vec<&str>> {
    let segments: Vec<&str> = key.split('.').map(str::trim).collect();
    if key.trim().is_empty() || segments.iter().any(|s| s.is_empty()) {
        bail!(
            "invalid key {key:?}: use dotted paths like 'services.gemma.port' \
             or 'system_one.presets.jev.provider'"
        );
    }
    if segments.len() > 4 {
        bail!("key {key:?} is too deep (maximum is <section>.<name>.<field>)");
    }
    Ok(segments)
}

/// Parse a user-supplied scalar/table as TOML via the `value = ...` wrapper,
/// so "8765" is an integer and '"text"' is a string — never a raw string.
fn parse_toml_value(raw: &str) -> Result<toml::Value> {
    let wrapper = format!("value = {raw}");
    let mut table: toml::Table = toml::from_str(&wrapper)
        .with_context(|| format!("{raw:?} is not valid TOML (quote strings: '\"text\"')"))?;
    table.remove("value").ok_or_else(|| {
        anyhow::anyhow!("internal error: 'value' key missing from the wrapper table")
    })
}

/// Config table at `path`; the serde default config when the file is absent
/// (matching `LeIndexConfig::load_from_path`'s pure-read behavior).
fn load_table_at(path: &Path) -> Result<toml::Table> {
    if !path.exists() {
        let default_str = toml::to_string_pretty(&LeIndexConfig::default())
            .context("failed to serialize the default config")?;
        return toml::from_str(&default_str).context("failed to parse the default config");
    }
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    toml::from_str(&contents).with_context(|| {
        format!(
            "failed to parse {}: fix or remove the file (config set never recovers silently)",
            path.display()
        )
    })
}

fn ensure_table_path<'a>(
    root: &'a mut toml::Table,
    segments: &[&str],
    key: &str,
) -> Result<&'a mut toml::Table> {
    let mut current = root;
    for segment in segments {
        if let Some(existing) = current.get(*segment) {
            if !existing.is_table() {
                bail!("'{key}': '{segment}' exists but is not a table");
            }
        } else {
            current.insert(segment.to_string(), toml::Value::Table(toml::Table::new()));
        }
        let next = current
            .get_mut(*segment)
            .expect("table existed or was just inserted")
            .as_table_mut()
            .expect("checked table above");
        current = next;
    }
    Ok(current)
}

/// Shared write path: sweep for unknown keys, typed-deserialize, validate,
/// then save. The file is never rewritten on any failure. `repair` marks the
/// `config unset` path and carries the dangling references of the PRE-EDIT
/// document: unset is itself the escape hatch for dangling cross-references,
/// so when the edited document still fails validation ONLY because of
/// dangling references that predate the edit, strict validation is deferred —
/// refusing to save because *another* pre-existing dangling reference remains
/// would recreate the dead end. Dangling references created by the edit
/// (deleting a service that is still referenced) are never tolerated, every
/// other invariant (ranges, enums, nonzero bounds) stays strict in repair
/// mode, and unknown-key and type checks always run.
fn commit_table_at(
    path: &Path,
    root: &toml::Table,
    key: &str,
    repair: Option<&[CrossRef]>,
) -> Result<PathBuf> {
    sweep_table_schema(root, &mut Vec::new())?;
    let rendered = toml::to_string(root).context("failed to render the edited document")?;
    let config: LeIndexConfig = toml::from_str(&rendered)
        .map_err(|e| anyhow::anyhow!("'{key}' would make the config invalid: {e}"))?;
    if let Err(err) = validate(&config) {
        let post_dangling = dangling_references(&config);
        let repairable = repair.is_some_and(|pre_dangling| {
            is_reference_error(&err)
                && !post_dangling.is_empty()
                && post_dangling
                    .iter()
                    .all(|reference| pre_dangling.contains(reference))
        });
        if !repairable {
            return Err(err.into());
        }
    }
    save_config_at(path, &config)
}

/// One cross-reference between config sections, as checked by [`validate`].
/// Shared by the strict validators and the unset repair path's deferral rule
/// (see `commit_table_at`) so the two predicates cannot drift.
#[derive(Debug, PartialEq, Eq)]
enum CrossRef {
    /// `indexing.embeddings.remote_service` → `services.<name>`.
    EmbeddingService(String),
    /// `system_one.service` → `services.<name>`.
    SystemService(String),
    /// `system_one.active_preset` → `system_one.presets.<name>`.
    ActivePreset(String),
    /// `system_one.presets.<name>.service` → `services.<other>`.
    PresetService(String),
}

impl CrossRef {
    /// Dangling when the referenced name is non-empty (post-trim) and its
    /// target section does not exist. This is the exact predicate the
    /// reference validators branch on.
    fn is_dangling(&self, config: &LeIndexConfig) -> bool {
        match self {
            CrossRef::EmbeddingService(name)
            | CrossRef::SystemService(name)
            | CrossRef::PresetService(name) => {
                let name = name.trim();
                !name.is_empty() && !config.services.contains_key(name)
            }
            CrossRef::ActivePreset(name) => {
                let name = name.trim();
                !name.is_empty() && !config.system_one.presets.contains_key(name)
            }
        }
    }
}

/// Every cross-reference present in `config` (valid or dangling), in
/// declaration order; `[system_one.presets]` follows BTreeMap order.
fn cross_references(config: &LeIndexConfig) -> Vec<CrossRef> {
    let mut references = Vec::new();
    if let Some(service) = non_empty(config.indexing.embeddings.remote_service.as_deref()) {
        references.push(CrossRef::EmbeddingService(service.to_string()));
    }
    if let Some(service) = non_empty(config.system_one.service.as_deref()) {
        references.push(CrossRef::SystemService(service.to_string()));
    }
    if let Some(preset) = non_empty(config.system_one.active_preset.as_deref()) {
        references.push(CrossRef::ActivePreset(preset.to_string()));
    }
    for preset in config.system_one.presets.values() {
        if let Some(service) = non_empty(preset.service.as_deref()) {
            references.push(CrossRef::PresetService(service.to_string()));
        }
    }
    references
}

/// Every dangling cross-reference in `config` (its target section missing).
fn dangling_references(config: &LeIndexConfig) -> Vec<CrossRef> {
    cross_references(config)
        .into_iter()
        .filter(|reference| reference.is_dangling(config))
        .collect()
}

/// Whether a validation failure is one the unset repair path may defer: a
/// dangling cross-reference or the hybrid ladder failing to resolve because
/// of one. Every other invariant stays strict even in repair mode.
fn is_reference_error(err: &SetupError) -> bool {
    let message = err.to_string();
    message.contains("references missing") || message.contains("no endpoint resolves")
}

/// Reject every unknown section/field anywhere in the document BEFORE saving:
/// `save` re-serializes the typed config, which would otherwise silently drop
/// keys serde ignores. Explicit error beats silent data loss.
fn sweep_table_schema<'a>(table: &'a toml::Table, prefix: &mut Vec<&'a str>) -> Result<()> {
    for (name, value) in table {
        prefix.push(name);
        if !is_known_path(prefix) {
            bail!(
                "unknown config key '{}' in leindex.toml; move or remove it first \
                 (saving would otherwise drop it)",
                prefix.join(".")
            );
        }
        if let toml::Value::Table(nested) = value {
            sweep_table_schema(nested, prefix)?;
        }
        prefix.pop();
    }
    Ok(())
}

/// Pure read of the typed config at `path` (defaults when absent).
fn load_config_at(path: &Path) -> Result<LeIndexConfig> {
    LeIndexConfig::load_from_path(path)
        .with_context(|| format!("failed to read {}", path.display()))
}

/// Serialize a config to a TOML value for display. Redactions applied here
/// (the single sink shared by `show` and `get`):
/// - `indexing.embeddings.remote_api_key` — always masked;
/// - `services.<name>.extra_args` — any value following a secret-bearing flag
///   (`--api-key`, `--api-key-file`, `--host`, `--embedding-api-key`) is
///   masked, since llama-server-compatible daemons accept inline secrets and
///   `config show` prints the rendered command line.
fn redacted_value(config: &LeIndexConfig) -> Result<toml::Value> {
    const SECRET_FLAGS: &[&str] = &[
        "--api-key",
        "--api-key-file",
        "--host",
        "--embedding-api-key",
    ];
    let rendered = toml::to_string_pretty(config).context("failed to render config")?;
    let mut value: toml::Value =
        toml::from_str(&rendered).context("failed to parse rendered config")?;
    if let Some(key) = value
        .get_mut("indexing")
        .and_then(|v| v.get_mut("embeddings"))
        .and_then(|v| v.get_mut("remote_api_key"))
    {
        *key = toml::Value::String(REDACTED.to_string());
    }
    if let Some(services) = value.get_mut("services").and_then(|v| v.as_table_mut()) {
        for (_, service) in services.iter_mut() {
            let Some(args) = service.get_mut("extra_args").and_then(|v| v.as_array_mut()) else {
                continue;
            };
            let mut redact_next = false;
            for item in args.iter_mut() {
                let arg = item.as_str().unwrap_or_default().to_string();
                if redact_next {
                    *item = toml::Value::String(REDACTED.to_string());
                    redact_next = false;
                } else {
                    // Joined (`--flag=value`) and bare (`--flag value`) forms
                    // of a secret-bearing flag are both masked; the joined
                    // form carries its own value, so it does not consume the
                    // next argument.
                    let flag = arg.split('=').next().unwrap_or_default();
                    if flag != arg {
                        if SECRET_FLAGS.contains(&flag) {
                            *item = toml::Value::String(format!("{flag}={REDACTED}"));
                        }
                    } else {
                        redact_next = SECRET_FLAGS.contains(&flag);
                    }
                }
            }
        }
    }
    Ok(value)
}

/// Write the typed config to `path` atomically: same NamedTempFile-persist
/// mechanism as `LeIndexConfig::save`, but against an explicit path so tests
/// can exercise it inside an isolated tempdir without touching LEINDEX_HOME
/// (a direct `config.save()` call here would write the user's real file).
fn save_config_at(path: &Path, config: &LeIndexConfig) -> Result<PathBuf> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    let rendered = toml::to_string_pretty(config).context("failed to serialize config")?;
    let mut temp = tempfile::NamedTempFile::new_in(path.parent().unwrap_or_else(|| Path::new(".")))
        .with_context(|| format!("failed to create a temp file next to {}", path.display()))?;
    temp.write_all(rendered.as_bytes())
        .and_then(|()| temp.flush())
        .with_context(|| format!("failed to buffer the config for {}", path.display()))?;
    temp.as_file()
        .sync_all()
        .with_context(|| format!("failed to sync the config for {}", path.display()))?;
    temp.persist(path)
        .map_err(|e| anyhow::anyhow!("failed to persist {}: {}", path.display(), e))
        .map(|_| ())?;
    Ok(path.to_path_buf())
}

#[cfg(test)]
#[path = "setup_config_test.rs"]
mod test;
