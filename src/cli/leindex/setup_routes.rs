//! Explicit embedding-route choices shared by flags and the interactive wizard.
use super::{SetupChoices, SetupError, setup_llama, wire_gemma_preset};
use crate::config::LeIndexConfig;
use dialoguer::{Confirm, Input, Select};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, clap::Args)]
pub struct SetupOptions {
    /// Embedding route: qwen3 (ONNX), embeddinggemma2 (managed GGUF), remote,
    /// or any provider preset name (gemini, voyage, mistral, jina, together,
    /// nomic, openai, cohere).
    ///
    /// Not a clap `value_parser` list: the accepted provider names come from
    /// [`crate::config::REMOTE_PRESETS`], and duplicating them here is a second
    /// copy that drifts the moment a provider is added. Validation happens in
    /// [`validate_route_options`], which is reached on every path.
    #[arg(long)]
    pub embedding: Option<String>,
    /// Absolute llama-server binary or installation directory (managed Gemma).
    #[arg(long, conflicts_with = "download_llama")]
    pub llama_server: Option<PathBuf>,
    /// Download a separate, verified official llama.cpp runtime: cpu, vulkan, or cuda.
    #[arg(long, value_parser = ["cpu", "vulkan", "cuda"], conflicts_with = "llama_server")]
    pub download_llama: Option<String>,
    /// Existing EmbeddingGemma2 GGUF; otherwise download the pinned preset model.
    #[arg(long)]
    pub gguf: Option<PathBuf>,
    /// Managed embedding server loopback listen host; public binding requires an external authenticated server.
    #[arg(long)]
    pub embedding_host: Option<String>,
    /// Managed embedding server listen port (not the MCP port).
    #[arg(long, value_parser = clap::value_parser!(u16).range(1..))]
    pub embedding_port: Option<u16>,
    /// llama.cpp GPU offload layers; 0 selects CPU (independent of ONNX providers).
    #[arg(long)]
    pub gpu_layers: Option<u32>,
    /// Whether the managed embedding service may start on demand.
    #[arg(long, action = clap::ArgAction::Set)]
    pub auto_start: Option<bool>,
    /// External embeddings base URL, including /v1 when required. Omit to use
    /// the selected provider preset's documented endpoint.
    #[arg(long)]
    pub embedding_endpoint: Option<String>,
    /// Model identifier sent to the external embeddings server. Omit to use
    /// the selected provider preset's documented model.
    #[arg(long)]
    pub embedding_model: Option<String>,
    /// Expected remote vector dimension; omit to preserve an existing external route or discover it.
    #[arg(long)]
    pub embedding_dimension: Option<usize>,
    /// Environment variable containing the embeddings API key (not the key itself); empty clears authentication.
    #[arg(long)]
    pub embedding_api_key_env: Option<String>,
    /// The embeddings API key ITSELF, stored in the config as `remote_api_key`.
    ///
    /// Prefer `--embedding-api-key-env` when the key already lives in the
    /// environment: a literal token is written to disk in plain text. This flag
    /// exists because a user whose key is in a password manager otherwise had to
    /// leave setup and run `leindex config set` — the manual step setup removes.
    /// The wizard offers the same choice as a prompt.
    #[arg(long)]
    pub embedding_api_key: Option<String>,
    /// Document prefix/template; {text} is substituted, omission preserves, empty clears.
    #[arg(long)]
    pub embedding_doc_template: Option<String>,
    /// Query prefix/template; {text} is substituted, omission preserves, empty clears.
    #[arg(long)]
    pub embedding_query_template: Option<String>,
    /// Open the System One configuration wizard (requires a terminal).
    #[arg(long)]
    pub system_one: bool,
    /// Whether setup is running the interactive wizard.
    #[arg(skip)]
    pub interactive: bool,
    /// Search mode: hybrid, text, or neural.
    #[arg(long)]
    pub search_mode: Option<String>,
    /// Hybrid neural score weight.
    #[arg(long)]
    pub neural_weight: Option<f64>,
    /// Enable cross-encoder reranking.
    #[arg(long, action = clap::ArgAction::Set)]
    pub rerank_enabled: Option<bool>,
    /// Reranking backend: onnx, system-one, or off.
    #[arg(long)]
    pub rerank_backend: Option<String>,
    /// Number of candidates to rerank.
    #[arg(long)]
    pub rerank_top_n: Option<u32>,
    /// Enable fragment indexing.
    #[arg(long, action = clap::ArgAction::Set)]
    pub fragment_index_enabled: Option<bool>,
    /// Maximum fragment size in bytes.
    #[arg(long)]
    pub fragment_max_bytes: Option<u64>,
    /// Fragment fusion weight.
    #[arg(long)]
    pub fragment_weight: Option<f64>,
    /// Include orphan fragments.
    #[arg(long, action = clap::ArgAction::Set)]
    pub fragment_orphan_enabled: Option<bool>,
    /// Use naive fragment fallback.
    #[arg(long, action = clap::ArgAction::Set)]
    pub fragment_naive_fallback: Option<bool>,
    /// Embedding/index batch size.
    #[arg(long)]
    pub batch_size: Option<u64>,
    /// Maximum files to index.
    #[arg(long)]
    pub max_files: Option<u64>,
    /// MCP idle exit timeout in seconds.
    #[arg(long)]
    pub mcp_idle_timeout_secs: Option<u64>,
    /// MCP engine unload timeout in seconds.
    #[arg(long)]
    pub mcp_engine_max_idle_secs: Option<u64>,
    /// MCP prewarm mode.
    #[arg(long)]
    pub mcp_prewarm: Option<String>,
    /// Enable automatic indexing on first tool use.
    #[arg(long, action = clap::ArgAction::Set)]
    pub mcp_auto_index: Option<bool>,
    /// System One mode.
    #[arg(long)]
    pub system_one_mode: Option<String>,
    /// System One provider.
    #[arg(long)]
    pub system_one_provider: Option<String>,
    /// System One endpoint.
    #[arg(long)]
    pub system_one_endpoint: Option<String>,
    /// System One model.
    #[arg(long)]
    pub system_one_model: Option<String>,
    /// System One API-key environment variable.
    #[arg(long)]
    pub system_one_api_key_env: Option<String>,
    /// System One timeout in milliseconds.
    #[arg(long)]
    pub system_one_timeout_ms: Option<u64>,
    /// System One pre-gate switch.
    #[arg(long, action = clap::ArgAction::Set)]
    pub system_one_pre_gate: Option<bool>,
    /// System One post-gate switch.
    #[arg(long, action = clap::ArgAction::Set)]
    pub system_one_post_gate: Option<bool>,
    /// System One relevance threshold.
    #[arg(long)]
    pub system_one_relevant_min: Option<f32>,
    /// System One instruction threshold.
    #[arg(long)]
    pub system_one_instructions_max: Option<f32>,
    /// System One top-N limit.
    #[arg(long)]
    pub system_one_top_n: Option<usize>,
    /// System One auto-apply threshold.
    #[arg(long)]
    pub system_one_tau_apply: Option<f32>,
    /// Managed service startup timeout in seconds.
    #[arg(long)]
    pub service_start_timeout_secs: Option<u64>,
}

impl SetupOptions {
    pub fn has_flags(&self) -> bool {
        self.embedding.is_some()
            || self.llama_server.is_some()
            || self.download_llama.is_some()
            || self.gguf.is_some()
            || self.embedding_host.is_some()
            || self.embedding_port.is_some()
            || self.gpu_layers.is_some()
            || self.auto_start.is_some()
            || self.embedding_endpoint.is_some()
            || self.embedding_model.is_some()
            || self.embedding_dimension.is_some()
            || self.embedding_api_key_env.is_some()
            || self.embedding_api_key.is_some()
            || self.embedding_doc_template.is_some()
            || self.embedding_query_template.is_some()
            || self.system_one
            || self.search_mode.is_some()
            || self.neural_weight.is_some()
            || self.rerank_enabled.is_some()
            || self.rerank_backend.is_some()
            || self.rerank_top_n.is_some()
            || self.fragment_index_enabled.is_some()
            || self.fragment_max_bytes.is_some()
            || self.fragment_weight.is_some()
            || self.fragment_orphan_enabled.is_some()
            || self.fragment_naive_fallback.is_some()
            || self.batch_size.is_some()
            || self.max_files.is_some()
            || self.mcp_idle_timeout_secs.is_some()
            || self.mcp_engine_max_idle_secs.is_some()
            || self.mcp_prewarm.is_some()
            || self.mcp_auto_index.is_some()
            || self.system_one_mode.is_some()
            || self.system_one_provider.is_some()
            || self.system_one_endpoint.is_some()
            || self.system_one_model.is_some()
            || self.system_one_api_key_env.is_some()
            || self.system_one_timeout_ms.is_some()
            || self.system_one_pre_gate.is_some()
            || self.system_one_post_gate.is_some()
            || self.system_one_relevant_min.is_some()
            || self.system_one_instructions_max.is_some()
            || self.system_one_top_n.is_some()
            || self.system_one_tau_apply.is_some()
            || self.service_start_timeout_secs.is_some()
    }
}

fn error(message: impl Into<String>) -> SetupError {
    SetupError::Conflict {
        message: message.into(),
    }
}

/// Local routes plus every provider preset, in menu order.
///
/// The picker and the `--embedding` flag are driven from this one list so a
/// provider cannot appear in the menu while being rejected by the flag layer
/// (which is exactly what happens when the two are written separately).
/// `"remote"` stays as the generic OpenAI-compatible entry for endpoints that
/// are not one of the known providers.
pub(super) fn route_names() -> Vec<&'static str> {
    let mut names = vec!["embeddinggemma2", "qwen3", "remote"];
    names.extend(crate::config::REMOTE_PRESETS.iter().map(|p| p.name));
    names.push("keep");
    names
}

/// Human label for one route, as shown in the picker.
pub(super) fn route_label(route: &str) -> String {
    match route {
        "embeddinggemma2" => "EmbeddingGemma2 - managed llama.cpp server (GGUF)".to_string(),
        "qwen3" => "Qwen3 - local ONNX Runtime".to_string(),
        "remote" => "Custom OpenAI-compatible embeddings server".to_string(),
        "keep" => "Keep the existing embedding configuration".to_string(),
        name => match crate::config::REMOTE_PRESETS
            .iter()
            .find(|p| p.name == name)
        {
            Some(preset) => format!(
                "{} - {} ({}-dim)",
                preset.name, preset.model, preset.dimension
            ),
            // Unreachable while `route_label` is only called with
            // `route_names()` entries; shown if that contract ever breaks
            // rather than panicking in the wizard.
            None => name.to_string(),
        },
    }
}

pub(super) fn select_interactive() -> Result<SetupOptions, SetupError> {
    let existing = LeIndexConfig::load().map_err(|e| SetupError::ConfigRead(e.to_string()))?;
    let routes = route_names();
    let items: Vec<String> = routes.iter().map(|route| route_label(route)).collect();
    // Default to "keep" when a remote route already resolves, else the first
    // entry (the managed Gemma preset).
    let default_index = if existing
        .indexing
        .embeddings
        .resolved_remote_with(&existing.services)
        .is_some()
    {
        routes.iter().position(|r| *r == "keep").unwrap_or(0)
    } else {
        0
    };
    let selection = Select::new()
        .with_prompt("Embedding model / inference route")
        .items(&items)
        .default(default_index)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
    Ok(SetupOptions {
        embedding: Some(routes[selection].into()),
        interactive: true,
        ..Default::default()
    })
}

/// Apply explicit intent AFTER loading existing configuration and BEFORE any provisioning.
pub(super) fn configure(
    config: &mut LeIndexConfig,
    choices: &SetupChoices,
) -> Result<bool, SetupError> {
    let options = &choices.options;
    let route = if choices.remote_gemma {
        "embeddinggemma2"
    } else {
        options.embedding.as_deref().unwrap_or("qwen3")
    };
    validate_route_options(choices, route)?;
    if !choices.neural_enabled {
        return disable_neural(config, choices);
    }
    require_neural(crate::feature_flags::FeatureFlag::NeuralSearch.is_enabled())?;
    if options.llama_server.is_some() && options.download_llama.is_some() {
        return Err(error("--llama-server conflicts with --download-llama"));
    }
    let remote = configure_route(config, options, route)?;
    apply_general_options(config, options)?;
    Ok(remote)
}

/// Apply non-route setup values to the shared config model.
fn apply_general_options(
    config: &mut LeIndexConfig,
    options: &SetupOptions,
) -> Result<(), SetupError> {
    if let Some(mode) = options.search_mode.as_deref() {
        if !matches!(mode, "hybrid" | "text" | "neural") {
            return Err(error("search mode must be hybrid, text, or neural"));
        }
        config.search.search_mode = mode.to_string();
    }
    if let Some(weight) = options.neural_weight {
        if !(0.0..=1.0).contains(&weight) {
            return Err(error("neural weight must be between 0 and 1"));
        }
        config.search.neural_weight = weight;
    }
    if let Some(enabled) = options.rerank_enabled {
        config.search.rerank_enabled = enabled;
    }
    if let Some(backend) = options.rerank_backend.as_deref() {
        if !matches!(backend, "onnx" | "system-one" | "off") {
            return Err(error("rerank backend must be onnx, system-one, or off"));
        }
        config.search.rerank_backend = backend.to_string();
    }
    if let Some(top_n) = options.rerank_top_n {
        if top_n == 0 {
            return Err(error("rerank top-n must be nonzero"));
        }
        config.search.rerank_top_n = top_n;
    }
    if let Some(enabled) = options.fragment_index_enabled {
        config.search.fragment_index_enabled = enabled;
    }
    if let Some(bytes) = options.fragment_max_bytes {
        config.search.fragment_max_bytes = bytes;
    }
    if let Some(weight) = options.fragment_weight {
        if !(0.0..=1.0).contains(&weight) {
            return Err(error("fragment weight must be between 0 and 1"));
        }
        config.search.fragment_weight = weight;
    }
    if let Some(enabled) = options.fragment_orphan_enabled {
        config.search.fragment_orphan_enabled = enabled;
    }
    if let Some(enabled) = options.fragment_naive_fallback {
        config.search.fragment_naive_fallback = enabled;
    }
    if let Some(batch_size) = options.batch_size {
        if batch_size == 0 {
            return Err(error("batch size must be nonzero"));
        }
        config.indexing.batch_size = batch_size;
    }
    if let Some(max_files) = options.max_files {
        config.indexing.max_files = max_files;
    }
    if let Some(value) = options.mcp_idle_timeout_secs {
        config.mcp.idle_timeout_secs = value;
    }
    if let Some(value) = options.mcp_engine_max_idle_secs {
        config.mcp.engine_max_idle_secs = value;
    }
    if let Some(value) = options.mcp_prewarm.as_deref() {
        if !matches!(value, "full" | "graph" | "off" | "false" | "none" | "0") {
            return Err(error("MCP prewarm must be full, graph, or off"));
        }
        config.mcp.prewarm = value.to_string();
    }
    if let Some(value) = options.mcp_auto_index {
        config.mcp.auto_index = value;
    }

    let system_one = &mut config.system_one;
    if let Some(value) = options.system_one_mode.as_deref() {
        if !matches!(value, "off" | "shadow" | "enforce") {
            return Err(error("System One mode must be off, shadow, or enforce"));
        }
        system_one.mode = value.to_string();
    }
    if let Some(value) = options.system_one_provider.as_deref() {
        system_one.provider = value.to_string();
    }
    if let Some(value) = options.system_one_endpoint.as_deref() {
        system_one.base_url = (!value.trim().is_empty()).then(|| value.to_string());
    }
    if let Some(value) = options.system_one_model.as_deref() {
        system_one.model = (!value.trim().is_empty()).then(|| value.to_string());
    }
    if let Some(value) = options.system_one_api_key_env.as_deref() {
        system_one.api_key_env = (!value.trim().is_empty()).then(|| value.to_string());
    }
    if let Some(value) = options.system_one_timeout_ms {
        if value == 0 {
            return Err(error("System One timeout must be nonzero"));
        }
        system_one.timeout_ms = value;
    }
    if let Some(value) = options.system_one_pre_gate {
        system_one.pre_gate = value;
    }
    if let Some(value) = options.system_one_post_gate {
        system_one.post_gate = value;
    }
    if let Some(value) = options.system_one_relevant_min {
        if !(0.0..=1.0).contains(&value) {
            return Err(error(
                "System One relevance threshold must be between 0 and 1",
            ));
        }
        system_one.relevant_min = value;
    }
    if let Some(value) = options.system_one_instructions_max {
        if !(0.0..=1.0).contains(&value) {
            return Err(error(
                "System One instruction threshold must be between 0 and 1",
            ));
        }
        system_one.instructions_max = value;
    }
    if let Some(value) = options.system_one_top_n {
        if value < 2 {
            return Err(error("System One top-n must be at least 2"));
        }
        system_one.top_n = value;
    }
    if let Some(value) = options.system_one_tau_apply {
        if !(0.0..=1.0).contains(&value) {
            return Err(error("System One apply threshold must be between 0 and 1"));
        }
        system_one.tau_apply = value;
    }
    if let Some(value) = options.service_start_timeout_secs {
        if value == 0 {
            return Err(error("managed service startup timeout must be nonzero"));
        }
        if let Some(service) = config.services.get_mut("gemma") {
            service.start_timeout_secs = value;
        }
    }
    Ok(())
}

/// True when `route` names a remote provider (the generic entry or a preset).
fn is_remote_route(route: &str) -> bool {
    route == "remote"
        || crate::config::REMOTE_PRESETS
            .iter()
            .any(|preset| preset.name == route)
}

/// Rejects embedding option/route mismatches before neural-disable or provisioning (CCN-gate extraction).
fn validate_route_options(choices: &SetupChoices, route: &str) -> Result<(), SetupError> {
    let options = &choices.options;
    if choices.remote_gemma
        && options
            .embedding
            .as_deref()
            .is_some_and(|r| r != "embeddinggemma2")
    {
        return Err(error(
            "--remote-gemma conflicts with the selected --embedding route",
        ));
    }
    if managed_options_set(options) && route != "embeddinggemma2" {
        return Err(error(
            "Managed-server options require --embedding embeddinggemma2",
        ));
    }
    if remote_options_set(options) && !is_remote_route(route) {
        return Err(error(
            "External embeddings options require --embedding remote or a provider name",
        ));
    }
    if !is_remote_route(route) && !matches!(route, "qwen3" | "embeddinggemma2" | "keep") {
        let names = route_names().join(", ");
        return Err(error(format!(
            "Unknown embedding route {route:?}; expected one of {names}"
        )));
    }
    Ok(())
}

/// Handles `--no-neural`: rejects embedding options, otherwise disables hybrid search (CCN-gate extraction).
fn disable_neural(config: &mut LeIndexConfig, choices: &SetupChoices) -> Result<bool, SetupError> {
    let options = &choices.options;
    if managed_options_set(options)
        || remote_options_set(options)
        || choices.remote_gemma
        || options.embedding.is_some()
    {
        return Err(error("Embedding options conflict with --no-neural"));
    }
    config.indexing.embeddings.enable_hybrid = false;
    Ok(false)
}

/// True when any managed llama.cpp server option was given (CCN-gate extraction).
fn managed_options_set(options: &SetupOptions) -> bool {
    options.llama_server.is_some()
        || options.download_llama.is_some()
        || options.gguf.is_some()
        || options.embedding_host.is_some()
        || options.embedding_port.is_some()
        || options.gpu_layers.is_some()
        || options.auto_start.is_some()
}

/// True when any external-embeddings option was given (CCN-gate extraction).
fn remote_options_set(options: &SetupOptions) -> bool {
    options.embedding_endpoint.is_some()
        || options.embedding_model.is_some()
        || options.embedding_dimension.is_some()
        || options.embedding_api_key_env.is_some()
        || options.embedding_api_key.is_some()
        || options.embedding_doc_template.is_some()
        || options.embedding_query_template.is_some()
}

/// Dispatches the validated embedding route to its provisioning or keep/qwen3 path (CCN-gate extraction).
fn configure_route(
    config: &mut LeIndexConfig,
    options: &SetupOptions,
    route: &str,
) -> Result<bool, SetupError> {
    match route {
        "qwen3" => {
            config.indexing.embeddings.remote_provider = "custom".into();
            config.indexing.embeddings.remote_endpoint = None;
            config.indexing.embeddings.remote_service = None;
            // Local ONNX route: `enable_hybrid` is the remote-takeover switch
            // (`remote_endpoint` + `enable_hybrid` switch the neural layer to
            // a remote endpoint), and validate_embeddings rejects hybrid
            // without a resolvable endpoint — the local route must leave it
            // off or every subsequent validate bricks the config.
            config.indexing.embeddings.enable_hybrid = false;
            Ok(false)
        }
        "keep" => {
            let remote = config
                .indexing
                .embeddings
                .resolved_remote_with(&config.services)
                .is_some();
            if remote {
                require_remote()?;
            }
            Ok(remote)
        }
        "embeddinggemma2" => {
            require_remote()?;
            configure_gemma(config, options)?;
            Ok(true)
        }
        "remote" => {
            require_remote()?;
            configure_external(config, "custom", options)?;
            Ok(true)
        }
        // Any provider preset: the same external route, but keyed on the
        // provider name so the resolved endpoint/model/dimension come from
        // `REMOTE_PRESETS` and the runtime selects the matching wire format.
        provider => {
            if !crate::config::REMOTE_PRESETS
                .iter()
                .any(|preset| preset.name == provider)
            {
                return Err(error("Unknown embedding route"));
            }
            require_remote()?;
            configure_external(config, provider, options)?;
            Ok(true)
        }
    }
}

fn require_neural(enabled: bool) -> Result<(), SetupError> {
    if !enabled {
        return Err(error(
            "Neural search is disabled by LEINDEX_FEATURE_NEURAL_SEARCH; enable it before provisioning",
        ));
    }
    Ok(())
}

fn require_remote() -> Result<(), SetupError> {
    if !cfg!(feature = "remote-embeddings") {
        return Err(error(
            "This binary lacks remote-embeddings; install a standard build",
        ));
    }
    if !crate::feature_flags::FeatureFlag::RemoteEmbeddings.is_enabled() {
        return Err(error(
            "Remote embeddings are disabled by LEINDEX_FEATURE_REMOTE_EMBEDDINGS; enable the flag or choose qwen3",
        ));
    }
    Ok(())
}

pub(super) fn prompt<T>(label: &str, default: T) -> Result<T, SetupError>
where
    T: Clone + std::fmt::Display + std::str::FromStr,
    T::Err: std::fmt::Display,
{
    Input::<T>::new()
        .with_prompt(label)
        .default(default)
        .interact_text()
        .map_err(|e| SetupError::Interactive(e.to_string()))
}

fn configure_gemma(config: &mut LeIndexConfig, options: &SetupOptions) -> Result<(), SetupError> {
    let existing = config.services.get("gemma");
    let mut service = existing.cloned().unwrap_or_default();
    service.host = options.embedding_host.clone().unwrap_or(service.host);
    service.port = options.embedding_port.unwrap_or(if service.port == 0 {
        8080
    } else {
        service.port
    });
    service.auto_start = options
        .auto_start
        .unwrap_or_else(|| existing.is_none_or(|service| service.auto_start));
    let layers = if options.interactive {
        gemma_interactive_service(&mut service, options)?
    } else {
        options.gpu_layers
    };
    validate_managed(&service)?;
    check_existing_gguf(options, &service)?;
    configure_args(
        &mut service.extra_args,
        layers,
        options.download_llama.as_deref() == Some("cpu"),
    )?;
    let gguf = choose_gemma_gguf(options, &service)?;
    resolve_gemma_binary(&mut service, options)?;
    let gguf = resolve_gemma_gguf(gguf)?;
    service.model = Some(gguf.display().to_string());
    if service.alias.is_none() {
        service.alias = Some(crate::config::EMBEDDINGGEMMA2_MODEL.into());
    }
    config.services.insert("gemma".into(), service);
    wire_gemma_embeddings(config, gguf);
    Ok(())
}

/// Interactive prompts for the managed gemma server; returns the GPU layer choice (CCN-gate extraction).
fn gemma_interactive_service(
    service: &mut crate::config::ServiceConfig,
    options: &SetupOptions,
) -> Result<Option<u32>, SetupError> {
    service.host = prompt("Embedding server loopback host", service.host.clone())?;
    service.port = prompt("Embedding server listen port", service.port)?;
    let current_layers = gpu_layers(&service.extra_args)?.unwrap_or(
        if options.download_llama.as_deref() == Some("cpu") {
            0
        } else {
            99
        },
    );
    let layers = Some(prompt(
        "GPU offload layers (0 = CPU)",
        options.gpu_layers.unwrap_or(current_layers),
    )?);
    service.auto_start = Confirm::new()
        .with_prompt("Allow LeIndex to start the embedding server on demand?")
        .default(service.auto_start)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
    service.start_timeout_secs = prompt(
        "Server startup timeout (seconds)",
        service.start_timeout_secs,
    )?;
    Ok(layers)
}

/// Rejects a pre-configured GGUF path that is not an absolute regular file (CCN-gate extraction).
fn check_existing_gguf(
    options: &SetupOptions,
    service: &crate::config::ServiceConfig,
) -> Result<(), SetupError> {
    if let Some(path) = options
        .gguf
        .as_deref()
        .or_else(|| service.model.as_deref().map(Path::new))
    {
        check_gguf_file(path)?;
    }
    Ok(())
}

fn check_gguf_file(path: &Path) -> Result<(), SetupError> {
    if !path.is_absolute() || !path.is_file() {
        return Err(error("Existing GGUF must be an absolute regular file path"));
    }
    Ok(())
}

/// Chooses the GGUF to use: flag, existing config, or interactive selection (CCN-gate extraction).
fn choose_gemma_gguf(
    options: &SetupOptions,
    service: &crate::config::ServiceConfig,
) -> Result<Option<PathBuf>, SetupError> {
    let mut gguf = options
        .gguf
        .clone()
        .or_else(|| service.model.as_ref().map(PathBuf::from));
    if options.interactive {
        let use_existing = Confirm::new()
            .with_prompt("Use an existing EmbeddingGemma2 GGUF?")
            .default(gguf.as_ref().is_some_and(|p| p.is_file()))
            .interact()
            .map_err(|e| SetupError::Interactive(e.to_string()))?;
        gguf = if use_existing {
            Some(PathBuf::from(prompt(
                "Absolute GGUF file",
                gguf.as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default(),
            )?))
        } else {
            None
        };
    }
    if let Some(path) = &gguf {
        check_gguf_file(path)?;
    }
    Ok(gguf)
}

/// Resolves the llama-server binary for the managed gemma service (CCN-gate extraction).
fn resolve_gemma_binary(
    service: &mut crate::config::ServiceConfig,
    options: &SetupOptions,
) -> Result<(), SetupError> {
    service.binary = if let Some(path) = &options.llama_server {
        setup_llama::ensure_binary(path)?
    } else if let Some(backend) = &options.download_llama {
        setup_llama::install_official(backend)?
    } else if options.interactive {
        setup_llama::configure_binary(&service.binary)?
    } else if Path::new(&service.binary).is_absolute() {
        setup_llama::ensure_binary(Path::new(&service.binary))?
    } else {
        return Err(error(
            "Choose --llama-server /absolute/path or --download-llama cpu|vulkan|cuda; setup never guesses a llama.cpp installation",
        ));
    };
    Ok(())
}

/// Canonicalizes a chosen GGUF or downloads the pinned preset; must be a regular file (CCN-gate extraction).
fn resolve_gemma_gguf(gguf: Option<PathBuf>) -> Result<PathBuf, SetupError> {
    let gguf = match gguf {
        Some(path) => std::fs::canonicalize(&path)
            .map_err(|e| SetupError::Io(format!("Cannot resolve GGUF {}: {e}", path.display())))?,
        None => super::ensure_embeddinggemma2_gguf_present(
            &crate::config::model_dir_path()
                .ok_or_else(|| error("Cannot resolve model directory"))?,
        )?,
    };
    if !gguf.is_file() {
        return Err(error("GGUF model must be a regular file"));
    }
    Ok(gguf)
}

/// Points the embeddings config at the managed gemma service (CCN-gate extraction).
fn wire_gemma_embeddings(config: &mut LeIndexConfig, gguf: PathBuf) {
    // Switching to a managed route clears stale explicit overrides that otherwise win.
    if !config.indexing.embeddings.embeddinggemma2_preset_active() {
        config.indexing.embeddings = crate::config::EmbeddingConfig::default();
    }
    config.indexing.embeddings.remote_endpoint = None;
    config.indexing.embeddings.remote_service = Some("gemma".into());
    wire_gemma_preset(config, &gguf);
    if config
        .indexing
        .embeddings
        .remote_model
        .as_deref()
        .is_none_or(|model| model == crate::config::EMBEDDINGGEMMA2_MODEL)
    {
        config.indexing.embeddings.remote_model = config.services["gemma"].served_name();
    }
}

fn validate_managed(service: &crate::config::ServiceConfig) -> Result<(), SetupError> {
    if service.port == 0 || service.start_timeout_secs == 0 {
        return Err(error(
            "Managed server requires a nonzero port and startup timeout",
        ));
    }
    let loopback = service.host == "localhost"
        || service
            .host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|address| address.is_loopback());
    if !loopback {
        return Err(error(
            "Managed embedding servers must bind a loopback address; use an external authenticated server for public binding",
        ));
    }
    if service.extra_args.iter().any(|arg| {
        arg == "--host"
            || arg.starts_with("--host=")
            || arg == "--port"
            || arg.starts_with("--port=")
    }) {
        return Err(error(
            "Managed embedding extra_args must not override the configured host or port",
        ));
    }
    Ok(())
}

fn gpu_option(arg: &str) -> bool {
    matches!(arg, "-ngl" | "--gpu-layers" | "--n-gpu-layers")
}

fn gpu_layers(args: &[String]) -> Result<Option<u32>, SetupError> {
    let mut layers = None;
    for (index, arg) in args.iter().enumerate() {
        let value = if gpu_option(arg) {
            Some(
                args.get(index + 1)
                    .map(String::as_str)
                    .ok_or_else(|| error("GPU layer argument is missing its value"))?,
            )
        } else {
            arg.split_once('=')
                .filter(|(key, _)| gpu_option(key))
                .map(|(_, value)| value)
        };
        if let Some(value) = value {
            if layers.is_some() {
                return Err(error("Managed server has duplicate GPU layer options"));
            }
            layers = Some(
                value
                    .parse()
                    .map_err(|_| error("GPU layer count must be a nonnegative integer"))?,
            );
        }
    }
    Ok(layers)
}

pub(super) fn configure_args(
    args: &mut Vec<String>,
    layers: Option<u32>,
    cpu: bool,
) -> Result<(), SetupError> {
    let existing = gpu_layers(args)?;
    if cpu && layers.or(existing).is_some_and(|layers| layers != 0) {
        return Err(error(
            "CPU runtime conflicts with nonzero GPU offload layers; set --gpu-layers 0 explicitly",
        ));
    }
    validate_pooling_value(args)?;
    if !args
        .iter()
        .any(|arg| matches!(arg.as_str(), "--embedding" | "--embeddings"))
    {
        args.push("--embedding".into());
    }
    // EmbeddingGemma2 is mean-pooled: that is what the shipped preset and
    // `benches/gemma_placement.py` both drive. An explicit `--pooling` always
    // wins, so this only fills a gap.
    if !args
        .iter()
        .any(|arg| arg == "--pooling" || arg.starts_with("--pooling="))
    {
        args.extend(["--pooling".into(), "mean".into()]);
    }
    if layers.is_some() {
        strip_gpu_options(args);
    }
    // Device and batch placement: the device is deliberately NOT set (see
    // above), but the batch ceilings ARE, because they must cover the largest
    // document the client will send or llama-server answers 500 and the caller
    // counts that against its failure budget.
    if let Some(layers) = layers.or_else(|| existing.is_none().then_some(if cpu { 0 } else { 99 }))
    {
        args.extend(["-ngl".into(), layers.to_string()]);
    }
    add_if_absent(args, &BATCH_FLAGS, &EMBEDDING_BATCH.to_string());
    // `-ub` is the binding ceiling (llama-server rejects any single input with
    // more tokens than the LOGICAL batch), so it is raised to the floor rather
    // than merely filled in: a value written by an earlier LeIndex build must
    // not survive a re-run. See `raise_flag_to_at_least`.
    raise_flag_to_at_least(args, &UBATCH_FLAGS, EMBEDDING_UBATCH_MIN);
    // `-b` has TWO constraints and they are not the same number:
    //
    //  1. llama-server refuses to start unless `-b >= -ub`. The floor is the
    //     EFFECTIVE `-ub`, not `EMBEDDING_UBATCH_MIN` — an operator who raised
    //     `-ub` above the floor would otherwise keep a `-b` that cannot boot.
    //  2. A physical ceiling below `EMBEDDING_UBATCH_MIN` cannot be operator
    //     intent (the same reasoning that makes it the floor for `-ub`), so it
    //     is our own stale default and is raised to `EMBEDDING_BATCH`. That is
    //     what makes a repaired install converge on a fresh one instead of
    //     running with a smaller batch than the measured optimum.
    let effective_ubatch = flag_value(args, &UBATCH_FLAGS)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(EMBEDDING_UBATCH_MIN);
    let stale_batch = flag_value(args, &BATCH_FLAGS)
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|batch| batch < EMBEDDING_UBATCH_MIN);
    let physical_floor = if stale_batch {
        EMBEDDING_BATCH.max(effective_ubatch)
    } else {
        effective_ubatch
    };
    raise_flag_to_at_least(args, &BATCH_FLAGS, physical_floor);
    Ok(())
}

/// Physical/logical batch ceilings for the managed embedding server.
///
/// llama-server rejects any single input with more tokens than the LOGICAL
/// batch (`-ub`), answering HTTP 500 ("input (N tokens) is too large to
/// process. increase the physical batch size"). The client, meanwhile, admits
/// texts up to `max_seq_len = 64 * 1024` BYTES (`BatchBudget` in
/// neural_publish.rs). Those two ceilings were tuned independently and
/// disagreed: measured on this corpus, a 64 KiB Rust document tokenizes to
/// 21,846 tokens, so the previously configured `-ub 24576` left only ~12%
/// headroom and rejected real documents — 92,605 rejections in one run. Each
/// rejection is an HTTP 500 that the caller's failure budget counts, so
/// oversized inputs alone could freeze the neural layer.
///
/// `-ub 32768` is the measured sweet spot: it accepts the full 64 KiB
/// maximum with ~50% headroom and is FASTER than the smaller value
/// (305-324 rows/s vs 207-213) while keeping Vulkan pinned-memory pressure to
/// one benign warning at load. Above it, memory warnings triple and throughput
/// stops improving, so this is not an artificial cap — it is the point where
/// more buys nothing.
///
/// The physical batch ceiling (`-b`) applied to a managed embedding server,
/// and the value a stale `-b` is raised to.
///
/// It is NOT a floor for a deliberate value: an operator who set `-b` at or
/// above [`EMBEDDING_UBATCH_MIN`] keeps their number, and the only absolute
/// requirement enforced is llama-server's `-b >= -ub` (see `configure_args`).
/// The distinction matters — treating this as a blanket floor would silently
/// undo a deliberate tightening.
const EMBEDDING_BATCH: u64 = 65536;

/// The logical batch ceiling (`-ub`) applied to a managed embedding server, and
/// the smallest value one may be left with.
///
/// Derived from the client's admission contract, not guessed: `BatchBudget`
/// (neural_publish.rs) admits texts up to `max_seq_len = 64 * 1024` BYTES, and a
/// 64 KiB Rust document tokenizes to ~21,846 tokens. A server whose `-ub` sits
/// below that rejects real documents with HTTP 500, and every rejection spends
/// the caller's sustained-failure budget until the neural layer freezes — which
/// is precisely what happened (92,605 rejections in one measured run).
///
/// 32768 leaves ~50% headroom over the measured worst case and is FASTER than
/// the smaller 24576 it replaced (305-324 rows/s vs 207-213) while keeping
/// Vulkan pinned-memory pressure to one benign warning at load. Above it, memory
/// warnings triple and throughput stops improving, so this is not an arbitrary
/// cap: it is where more stops buying anything.
///
/// This is deliberately a MINIMUM as well as a default, applied through
/// [`raise_flag_to_at_least`]: a value this file wrote in an earlier release
/// must not survive a re-run.
const EMBEDDING_UBATCH_MIN: u64 = 32768;

/// Every spelling of each llama-server batch option, CANONICAL FIRST.
///
/// `-b`/`--batch-size` and `-ub`/`--ubatch-size` are ONE option each in
/// llama-server (`common/arg.cpp`), and every occurrence assigns, so the LAST
/// spelling present wins. Matching only the canonical form — which this module
/// did — makes setup append a `-b` that silences an operator's deliberate
/// `--batch-size`, the exact inverse of the contract below. `gpu_option` already
/// matches three spellings for the same reason.
///
/// The first entry is the spelling written when the option is absent.
const BATCH_FLAGS: [&str; 2] = ["-b", "--batch-size"];
const UBATCH_FLAGS: [&str; 2] = ["-ub", "--ubatch-size"];

/// Append `flags[0] value` unless any spelling of the option is already present.
///
/// The operator's explicit value always wins: this only supplies a default for
/// an option they have not expressed an opinion on, mirroring the `--pooling`
/// contract above. An operator who wrote `--batch-size 65536` has expressed an
/// opinion just as clearly as one who wrote `-b 65536`.
fn add_if_absent(args: &mut Vec<String>, flags: &[&str], value: &str) {
    let inline: Vec<String> = flags.iter().map(|flag| format!("{flag}=")).collect();
    let present = args.iter().any(|arg| {
        flags.contains(&arg.as_str())
            || inline.iter().any(|prefix| arg.starts_with(prefix.as_str()))
    });
    if !present {
        args.extend([flags[0].to_string(), value.to_string()]);
    }
}

/// The EFFECTIVE value of an option, supporting both `flag value` and
/// `flag=value` in every spelling — the LAST occurrence wins.
///
/// Last, not first, because that is what llama-server does (`common/arg.cpp`
/// assigns on every occurrence). Repairing the first of two conflicting
/// spellings would leave the server running on the one this module never saw.
fn flag_value<'a>(args: &'a [String], flags: &[&str]) -> Option<&'a str> {
    let inline: Vec<String> = flags.iter().map(|flag| format!("{flag}=")).collect();
    let mut effective = None;
    for (index, arg) in args.iter().enumerate() {
        let value = if flags.contains(&arg.as_str()) {
            args.get(index + 1).map(String::as_str)
        } else {
            inline
                .iter()
                .find_map(|prefix| arg.strip_prefix(prefix.as_str()))
        };
        if value.is_some() {
            effective = value;
        }
    }
    effective
}

/// Raise an option to at least `minimum`, keeping any larger operator value.
///
/// [`add_if_absent`] cannot do this: it treats the presence of an option as
/// operator intent, so a value a PREVIOUS LeIndex version wrote is preserved
/// forever. That makes a default that was measured wrong permanently sticky —
/// re-running setup, the one action a user takes to pick up a corrected
/// default, would change nothing.
///
/// That is exactly how the embedding batch ceilings went wrong: `-b 24576
/// -ub 24576` were written into `[services.gemma].extra_args` by an earlier
/// build, and llama-server answers HTTP 500 for any input larger than `-ub`
/// while this client admits documents up to 64 KiB (~21,846 tokens). The
/// rejections spent the neural layer's sustained-failure budget and froze it.
///
/// Treating those numbers as sacred would strand every existing install on the
/// broken values, so a BELOW-minimum value is raised and a warning names what
/// changed. A value at or above `minimum` is left alone: an operator who
/// raised it deliberately is never overridden, they simply keep their headroom.
/// An unparseable value is also left alone — provenance is unknowable, so it is
/// reported and never silently rewritten.
///
/// The value is repaired **in the spelling it was found in**, and no canonical
/// alias is appended beside it. Every occurrence of a llama-server option
/// assigns, so appending `-b` next to an operator's `--batch-size` would leave
/// the operator's value silently outvoted by the LAST occurrence — the repair
/// has to fix the argument that actually wins, not add a competitor to it.
///
/// Only occurrences BELOW `minimum` are rewritten. A config holding both
/// `-ub 65536` and a stale `--ubatch-size 24576` has one occurrence that is
/// deliberate and one that is not; blanket-rewriting both would lower the
/// operator's headroom, which this function promises never to do.
fn raise_flag_to_at_least(args: &mut Vec<String>, flags: &[&str], minimum: u64) {
    let Some(current) = flag_value(args, flags) else {
        add_if_absent(args, flags, &minimum.to_string());
        return;
    };
    let Ok(parsed) = current.parse::<u64>() else {
        tracing::warn!(
            flags = ?flags,
            value = current,
            "Managed server has a non-numeric batch ceiling; leaving it unchanged"
        );
        return;
    };
    if parsed >= minimum {
        return;
    }
    let raises = |value: &str| value.parse::<u64>().is_ok_and(|value| value < minimum);
    let inline: Vec<String> = flags.iter().map(|flag| format!("{flag}=")).collect();
    let replacement = minimum.to_string();
    let mut index = 0;
    while index < args.len() {
        if flags.contains(&args[index].as_str()) {
            // `-ub 24576`: replace the value in the following slot, replacing
            // the SLOT rather than draining it, so arity never changes.
            if args.get(index + 1).is_some_and(|value| raises(value)) {
                args[index + 1] = replacement.clone();
            }
            index += 2;
            continue;
        }
        if let Some(prefix) = inline
            .iter()
            .find(|prefix| args[index].starts_with(prefix.as_str()))
            .filter(|prefix| raises(&args[index][prefix.len()..]))
        {
            args[index] = format!("{prefix}{replacement}");
        }
        index += 1;
    }
    tracing::warn!(
        flags = ?flags,
        was = parsed,
        now = minimum,
        "Raised a managed-server batch ceiling that was below the largest document this \
         client sends; llama-server answers HTTP 500 for inputs over the logical batch and \
         the caller spends its neural failure budget on them"
    );
}

/// Rejects a `--pooling` flag that has no value (CCN-gate extraction).
fn validate_pooling_value(args: &[String]) -> Result<(), SetupError> {
    if let Some(index) = args.iter().position(|arg| arg == "--pooling") {
        if args
            .get(index + 1)
            .is_none_or(|value| value.starts_with('-'))
        {
            return Err(error("Pooling argument requires a value"));
        }
    }
    Ok(())
}

/// Removes every GPU-layer option (both `-ngl N` and `--opt=N` forms) before re-adding it (CCN-gate extraction).
fn strip_gpu_options(args: &mut Vec<String>) {
    let mut index = 0;
    while index < args.len() {
        if gpu_option(&args[index]) {
            // A trailing flag with no value has no pair to drain.
            let end = (index + 2).min(args.len());
            args.drain(index..end);
        } else if args[index]
            .split_once('=')
            .is_some_and(|(key, _)| gpu_option(key))
        {
            args.remove(index);
        } else {
            index += 1;
        }
    }
}

/// Remote-route fields resolved from flags with fallback to the previous route (CCN-gate extraction).
struct ExternalRoute {
    endpoint: String,
    model: String,
    dimension: Option<usize>,
    key_env: String,
    /// A bearer token pasted into the wizard, distinct from [`Self::key_env`].
    ///
    /// `config.indexing.embeddings.remote_api_key` is a LITERAL token and has
    /// always been honoured at request time, but setup could only ever preserve
    /// or clear it (see the `remote_api_key` assignment in `build_embedding_config`).
    /// A user with a key in hand — a password manager, an invoice email — had no
    /// way to enter it: the only prompt asked for a VARIABLE NAME, so the
    /// documented path was "run `leindex config set indexing.embeddings.remote_api_key`",
    /// which is exactly the manual step setup exists to remove.
    key_literal: String,
    doc: String,
    query: String,
}

/// Resolves each external-route field from flags, falling back to the previous route (CCN-gate extraction).
fn external_route_from_flags(
    config: &LeIndexConfig,
    options: &SetupOptions,
    same_route: bool,
) -> ExternalRoute {
    let previous = &config.indexing.embeddings;
    ExternalRoute {
        endpoint: options
            .embedding_endpoint
            .clone()
            .or_else(|| {
                // Only carry the previous ENDPOINT forward when this is the same
                // route. The resolved ladder is not gated on `same_route`, so
                // reading it unconditionally would inherit the fresh-config
                // EmbeddingGemma2 loopback endpoint into an unrelated provider
                // route and pin it at a dead local port.
                same_route.then(|| {
                    previous
                        .resolved_remote_with(&config.services)
                        .map(|r| r.endpoint)
                })?
            })
            .unwrap_or_default(),
        model: options
            .embedding_model
            .clone()
            .or_else(|| previous.remote_model.clone())
            .unwrap_or_default(),
        dimension: options.embedding_dimension.or(if same_route {
            previous.remote_dimension
        } else {
            None
        }),
        key_env: options
            .embedding_api_key_env
            .clone()
            .or_else(|| {
                same_route
                    .then(|| previous.remote_api_key_env.clone())
                    .flatten()
            })
            .unwrap_or_default(),
        // ONLY what was given this run. The "keep the stored token" case is the
        // third branch of `remote_api_key` below; carrying `previous` here as
        // well duplicated that rule and then outranked an explicit
        // `--embedding-api-key-env ""`, turning a deliberate clear into a
        // silent re-store.
        key_literal: options.embedding_api_key.clone().unwrap_or_default(),
        doc: options
            .embedding_doc_template
            .clone()
            .or_else(|| {
                same_route
                    .then(|| previous.remote_doc_template.clone())
                    .flatten()
            })
            .unwrap_or_default(),
        query: options
            .embedding_query_template
            .clone()
            .or_else(|| {
                same_route
                    .then(|| previous.remote_query_template.clone())
                    .flatten()
            })
            .unwrap_or_default(),
    }
}

/// Offer the models the endpoint actually serves.
///
/// Returns the chosen identifier, or `None` when discovery could not produce a
/// usable list — in which case the caller keeps the existing free-text prompt
/// instead of failing. This ordering matters: discovery is a convenience, and
/// an endpoint that hides its catalogue (or has no listing at all) must not
/// make `leindex setup` unusable.
///
/// The candidates come from the provider's own response, so a model this
/// installation has never heard of is still selectable, and an ID the preset
/// table got wrong never is.
#[cfg(feature = "remote-embeddings")]
fn discover_models(
    route: &ExternalRoute,
    preset: Option<&crate::config::RemotePreset>,
) -> Vec<String> {
    use crate::cli::index_builder::hybrid::block_on_remote;
    use crate::search::onnx::remote::{RemoteEmbeddingConfig, RemoteProvider};

    let endpoint = route.endpoint.trim();
    if endpoint.is_empty() {
        return Vec::new();
    }
    // Resolve the wire from the SAME table the runtime uses, so discovery
    // speaks the protocol the embedder will speak. Building the variant by
    // hand here would let a native-Gemini route be listed over the
    // OpenAI-compatible path and quietly return a different catalogue.
    let wire = preset.map_or(crate::config::RemoteWire::OpenAiCompat, |p| p.wire);
    let provider = match wire {
        crate::config::RemoteWire::Gemini => RemoteProvider::Gemini {
            model: route.model.clone(),
        },
        crate::config::RemoteWire::OpenAiCompat => RemoteProvider::Custom {
            endpoint: endpoint.to_string(),
        },
    };
    // An unset key is not an error to report here: the route may legitimately
    // be unauthenticated, and the smoke test is what validates credentials.
    let api_key = std::env::var(route.key_env.trim())
        .ok()
        .filter(|value| !value.trim().is_empty())
        .filter(|_| !route.key_env.trim().is_empty());
    let config = RemoteEmbeddingConfig {
        provider,
        api_key,
        // Listing is a page of metadata; a slow answer here should not stall
        // the wizard, and the caller falls back to free text.
        timeout_secs: 10,
        max_retries: 0,
        base_url: (wire == crate::config::RemoteWire::Gemini).then(|| endpoint.to_string()),
        ..Default::default()
    };
    block_on_remote(async move {
        config.list_models().await.map_err(|error| {
            // Discovery failures are advisory; surface the reason if a caller
            // ever wants it, but never mark the route permanent.
            crate::cli::index_builder::hybrid::RemoteBridgeError::Transient(error)
        })
    })
    .unwrap_or_else(|error| {
        // Say WHY the listing is empty before falling back to free text. A
        // silent fallback is indistinguishable from an endpoint that serves no
        // catalogue, and that ambiguity is what makes an unauthenticated
        // listing (HTTP 401) look like normal setup. `tracing` rather than
        // `eprintln!`: it is this tree's logging convention, it honours the log
        // filter, and it cannot interleave with a transport that owns the raw
        // stdio stream. The setup wizard still falls through to the free-text
        // prompt either way.
        tracing::warn!(
            endpoint = %endpoint,
            %error,
            "Model list unavailable; falling back to manual model entry"
        );
        Vec::new()
    })
}

/// Nothing to discover in a build without remote embeddings.
///
/// The stub keeps [`prompt_model`] single-path: it always consults discovery and
/// always falls back to free text, so the feature gate exists in exactly one
/// place instead of wrapping the caller's body in `#[cfg]` and leaving a dead
/// `preset` parameter behind in the other build.
#[cfg(not(feature = "remote-embeddings"))]
fn discover_models(
    _route: &ExternalRoute,
    _preset: Option<&crate::config::RemotePreset>,
) -> Vec<String> {
    Vec::new()
}

/// Prompt for the model identifier, preferring a discovered list (CCN-gate extraction).
fn prompt_model(
    route: &mut ExternalRoute,
    preset: Option<&crate::config::RemotePreset>,
) -> Result<(), SetupError> {
    let mut models = discover_models(route, preset);
    if !models.is_empty() {
        models.sort();
        models.dedup();
        // Keep the configured/preset model addressable even when the
        // endpoint's listing omits it (some servers list only loaded
        // models, or hide aliases): offering a list that cannot express
        // the current value would silently change the user's route.
        if !route.model.trim().is_empty() && !models.iter().any(|m| m == &route.model) {
            models.insert(0, route.model.clone());
        }
        route.model = super::setup_picker::pick_model(&models, &route.model)?;
        return Ok(());
    }
    // No listing available (or built without remote-embeddings): the free-text
    // prompt is the whole contract, so it must stay reachable.
    route.model = super::setup_picker::prompt_free_text(&route.model)?;
    Ok(())
}

/// Prompt for the endpoint, model, and dimension of a free-form external route.
fn prompt_route_details(
    route: &mut ExternalRoute,
    preset: Option<&crate::config::RemotePreset>,
) -> Result<(), SetupError> {
    route.endpoint = prompt(
        "Embeddings base URL (for example http://localhost:9000/v1)",
        route.endpoint.clone(),
    )?;
    // The model listing is authenticated with the key named here, so the name
    // must be known before discovery runs.
    prompt_key_env(route)?;
    prompt_model(route, preset)?;
    // Default to the RESOLVED route dimension, not the previous config's: the
    // caller has already folded flag-over-preset-over-previous precedence into
    // `route.dimension`, so reading `previous` here would show the outgoing
    // provider's width as the recommendation for the incoming one.
    let d: usize = prompt(
        "Expected dimension (0 = discover)",
        route.dimension.unwrap_or(0),
    )?;
    route.dimension = (d != 0).then_some(d);
    Ok(())
}

/// Prompt for the API-key variable, skipping the question when the key is
/// already in the environment.
///
/// A preset names the variable it expects, so a user who has exported
/// `GEMINI_API_KEY` should not be asked to confirm that they did. When the
/// variable is absent the prompt stays — silently persisting a name that
/// resolves to nothing would fail later, at ingest, with a message about
/// authentication rather than about setup.
fn prompt_key_env(route: &mut ExternalRoute) -> Result<(), SetupError> {
    let key_env = route.key_env.trim().to_string();
    let detected = std::env::var(&key_env)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    // A key already in the environment (or one the user just pasted) needs no
    // question: only ask when there is genuinely nothing to work with. A preset
    // names the variable it expects, and a user who exported it must not be made
    // to answer a menu about it.
    if detected || !route.key_literal.trim().is_empty() {
        return Ok(());
    }
    const STORE: &str = "Store the key in the config (plain text)";
    const VAR: &str = "Use an environment variable";
    const NONE: &str = "No authentication";
    let items = [STORE, VAR, NONE];
    // Default to the env var when one is already configured (a re-run must not
    // silently rewrite anyone's credential strategy), otherwise store.
    let default = if detected || !key_env.is_empty() {
        1
    } else {
        0
    };
    let selection = Select::new()
        .with_prompt("Embeddings API key")
        .items(&items)
        .default(default)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
    match selection {
        // One key, one source of truth: clear the variable name so the literal
        // actually wins. `remote_api_key` takes precedence at request time, but
        // leaving `remote_api_key_env` set would make `leindex config show` and
        // a later re-run disagree about where the credential comes from.
        0 => {
            route.key_env.clear();
            route.key_literal = Input::<String>::new()
                .with_prompt("Paste the API key (saved to the config in plain text)")
                .with_initial_text(&route.key_literal)
                .allow_empty(true)
                .interact_text()
                .map_err(|e| SetupError::Interactive(e.to_string()))?;
        }
        1 => {
            route.key_literal.clear();
            prompt_key_var(route)?;
        }
        _ => {
            route.key_literal.clear();
            route.key_env.clear();
        }
    }
    Ok(())
}

/// Prompt for the API-key variable name, skipping the question when the key is
/// already in the environment.
///
/// A preset names the variable it expects, so a user who has exported
/// `GEMINI_API_KEY` should not be asked to confirm that they did. When the
/// variable is absent the prompt stays — silently persisting a name that
/// resolves to nothing would fail later, at ingest, with a message about
/// authentication rather than about setup.
fn prompt_key_var(route: &mut ExternalRoute) -> Result<(), SetupError> {
    let key_env = route.key_env.trim().to_string();
    let detected = std::env::var(&key_env)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .is_some();
    if !key_env.is_empty() && detected {
        return Ok(());
    }
    let label = if key_env.is_empty() {
        "API key environment variable (blank for none)".to_string()
    } else {
        format!("{key_env} is not set - API key environment variable (blank for none)")
    };
    route.key_env = Input::<String>::new()
        .with_prompt(label)
        .with_initial_text(&route.key_env)
        .allow_empty(true)
        .interact_text()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
    Ok(())
}

/// Prompt for the asymmetric doc/query templates, keeping resolved defaults.
fn prompt_templates(route: &mut ExternalRoute) -> Result<(), SetupError> {
    route.doc = Input::<String>::new()
        .with_prompt("Document template (blank = raw text)")
        .with_initial_text(&route.doc)
        .allow_empty(true)
        .interact_text()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
    route.query = Input::<String>::new()
        .with_prompt("Query template (blank = raw text)")
        .with_initial_text(&route.query)
        .allow_empty(true)
        .interact_text()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
    Ok(())
}

/// Interactive prompts for the external route; defaults come from the resolved route.
///
/// A preset route is zero-config: endpoint, model, dimension, and key variable
/// all come from the preset table, so the wizard walks straight to the model
/// picker. Overriding is still one keystroke away, because a self-hosted
/// gateway or a pinned catalogue is a legitimate deviation from the table —
/// just not what most people running `leindex setup gemini` want to be asked.
fn prompt_external_route(
    route: &mut ExternalRoute,
    preset: Option<&crate::config::RemotePreset>,
) -> Result<(), SetupError> {
    if let Some(preset) = preset {
        let use_defaults = Confirm::new()
            .with_prompt(format!(
                "Use {} defaults ({} · {})?",
                preset.name, preset.endpoint, preset.model
            ))
            .default(true)
            .interact()
            .map_err(|e| SetupError::Interactive(e.to_string()))?;
        if !use_defaults {
            prompt_route_details(route, Some(preset))?;
            return prompt_templates(route);
        }
        // Keep the table's endpoint/model/dimension and ask only what the
        // table cannot know: which served model to use, and the templates.
        prompt_key_env(route)?;
        prompt_model(route, Some(preset))?;
        return Ok(());
    }
    prompt_route_details(route, None)?;
    prompt_templates(route)
}

/// Requires an http(s) URL with a host and no credentials, query, or fragment (CCN-gate extraction).
fn validate_external_url(endpoint: &str) -> Result<(), SetupError> {
    let url =
        reqwest::Url::parse(endpoint).map_err(|e| error(format!("Invalid embeddings URL: {e}")))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(error(
            "Embeddings URL requires http(s), a host, and no credentials, query, or fragment",
        ));
    }
    Ok(())
}

/// Requires the named API-key variable to be set when authentication was requested (CCN-gate extraction).
fn check_api_key_env(key_env: &str, options: &SetupOptions) -> Result<(), SetupError> {
    if !key_env.is_empty() && (options.embedding_api_key_env.is_some() || options.interactive) {
        // Store only the variable name; runtime resolves the value for every process.
        if std::env::var(key_env)
            .ok()
            .filter(|v| !v.trim().is_empty())
            .is_none()
        {
            return Err(error(format!(
                "API key environment variable {key_env} is unset or empty"
            )));
        }
    }
    Ok(())
}

/// Validates the resolved external route in the same order as before (CCN-gate extraction).
///
/// `preset_supplies_model` relaxes the model check for a known provider: the
/// preset names the model, so demanding `--embedding-model` would make every
/// preset route unusable without restating what the table already knows.
fn validate_external_route(
    route: &ExternalRoute,
    options: &SetupOptions,
    preset_supplies_model: bool,
) -> Result<(), SetupError> {
    // A preset route resolves its endpoint from the table, so an empty endpoint
    // is only an error when nothing else can supply one.
    if !route.endpoint.trim().is_empty() {
        validate_external_url(&route.endpoint)?;
    } else if !preset_supplies_model {
        return Err(error(
            "External embeddings require --embedding-endpoint (or a provider preset)",
        ));
    }
    if route.model.trim().is_empty() && !preset_supplies_model {
        return Err(error("External embeddings require --embedding-model"));
    }
    if route.dimension == Some(0) {
        return Err(error("Embedding dimension must be nonzero"));
    }
    check_api_key_env(&route.key_env, options)
}

/// Builds the replacement embeddings config for the external route (CCN-gate extraction).
///
/// `provider` is the selected preset name or `"custom"`. The preset supplies
/// endpoint/model/dimension/templates at RESOLVE time, so this deliberately
/// leaves those unset when the flags did not name them: persisting the preset's
/// values here would freeze today's model into the config and a later preset
/// update would never reach this installation.
fn external_embedding_config(
    provider: &str,
    route: ExternalRoute,
    previous: &crate::config::EmbeddingConfig,
    options: &SetupOptions,
    same_route: bool,
) -> crate::config::EmbeddingConfig {
    let has_literal_token = !route.key_literal.trim().is_empty();
    crate::config::EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: provider.into(),
        matryoshka: if same_route {
            previous.matryoshka
        } else {
            crate::config::MatryoshkaSetting::Off
        },
        remote_service: None,
        remote_endpoint: (!route.endpoint.trim().is_empty()).then_some(route.endpoint),
        remote_model: (!route.model.trim().is_empty()).then_some(route.model),
        remote_dimension: route.dimension,
        // A literal token, written only when the user gave one (`--embedding-api-key`
        // or the wizard prompt). It MUST outrank both the previous value and the
        // preset's `remote_api_key_env` fallback below: a key the user just typed
        // is the more specific intent, and storing it here is what lets a pasted
        // token work without `leindex config set`. An empty flag clears it, which
        // is how a user drops a stored secret.
        remote_api_key: if !route.key_literal.trim().is_empty() {
            Some(route.key_literal.trim().to_string())
        } else if options.embedding_api_key.is_some()
            || options
                .embedding_api_key_env
                .as_deref()
                .is_some_and(str::is_empty)
        {
            // An explicit empty `--embedding-api-key-env ""` is how a user drops
            // authentication, so it must outrank "keep what was stored".
            None
        } else if same_route && options.embedding_api_key_env.is_none() && !options.interactive {
            previous.remote_api_key.clone()
        } else {
            None
        },
        // Fall back to the preset's conventional variable (GEMINI_API_KEY, ...)
        // so a preset route authenticates without the user spelling it out.
        //
        // A literal token suppresses the fallback entirely. `remote_api_key`
        // outranks `remote_api_key_env` at request time, so keeping a variable
        // name beside a pasted token leaves two sources of truth that disagree
        // the moment the variable is exported, and `config show` reports a
        // credential location that is not the one in use.
        remote_api_key_env: if has_literal_token {
            None
        } else {
            (!route.key_env.is_empty())
                .then_some(route.key_env)
                .or_else(|| {
                    crate::config::REMOTE_PRESETS
                        .iter()
                        .find(|preset| preset.name == provider)
                        .and_then(|preset| preset.api_key_env)
                        .map(str::to_string)
                })
        },
        remote_doc_template: (!route.doc.is_empty()).then_some(route.doc),
        remote_query_template: (!route.query.is_empty()).then_some(route.query),
        // Deliberately NOT persisted: the preset owns the provider's per-request
        // ceiling and resolves it at call time, so writing today's number here
        // would freeze it exactly the way the other preset fields are avoided.
        //
        // The token limits are skipped for the same reason, and one step
        // further: `leindex setup` never asks about them, so writing None would
        // be harmless but writing anything would be inventing a value. Preset
        // (or the EmbeddingGemma family default) fills them during resolution.
        remote_batch_limit: None,
        remote_max_input_tokens: None,
        remote_request_token_budget: None,
        onnx_model_path: if same_route {
            previous.onnx_model_path.clone()
        } else {
            None
        },
    }
}

fn configure_external(
    config: &mut LeIndexConfig,
    provider: &str,
    options: &SetupOptions,
) -> Result<(), SetupError> {
    let previous = &config.indexing.embeddings;
    // "Same route" means the user is re-running setup on the SAME external
    // route; it is independent of which provider is now selected, because
    // switching provider is itself a route change.
    //
    // The endpoint check alone is NOT sufficient: a fresh config ships with the
    // EmbeddingGemma2 preset, whose loopback endpoint resolves, so reusing it
    // would carry `http://127.0.0.1:8080/v1` into a Gemini or Voyage route and
    // silently pin the new provider at a dead local port.
    let same_route = previous
        .remote_endpoint
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && !previous.embeddinggemma2_preset_active()
        && previous
            .remote_provider
            .trim()
            .eq_ignore_ascii_case(provider);
    let mut route = external_route_from_flags(config, options, same_route);
    let preset = crate::config::REMOTE_PRESETS
        .iter()
        .find(|preset| preset.name == provider);
    // Seed the prompt defaults from the preset so the wizard offers the
    // provider's documented endpoint/model/dimension instead of empty boxes
    // the user has to look up. An explicit flag already won above.
    if let Some(preset) = preset {
        if route.endpoint.trim().is_empty() {
            route.endpoint = preset.endpoint.to_string();
        }
        if route.model.trim().is_empty() {
            route.model = preset.model.to_string();
        }
        if route.dimension.is_none() {
            route.dimension = Some(preset.dimension);
        }
        if route.key_env.trim().is_empty() {
            route.key_env = preset.api_key_env.unwrap_or_default().to_string();
        }
    }
    if options.interactive {
        prompt_external_route(&mut route, preset)?;
    }
    // A preset route that kept the offered defaults persists NO endpoint or
    // model: the table is the source of truth, so `remote_provider = "gemini"`
    // alone must remain a working route, and a later correction to the table
    // has to reach this installation. Anything the user actually changed is
    // persisted, because that is real intent the table does not know.
    if let Some(preset) = preset {
        if route.endpoint == preset.endpoint {
            route.endpoint.clear();
        }
        if route.model == preset.model {
            route.model.clear();
        }
        if route.dimension == Some(preset.dimension) {
            route.dimension = None;
        }
    }
    validate_external_route(&route, options, preset.is_some())?;
    config.indexing.embeddings =
        external_embedding_config(provider, route, previous, options, same_route);
    Ok(())
}

#[cfg(test)]
#[path = "setup_routes_test.rs"]
mod test;
