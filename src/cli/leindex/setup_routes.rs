//! Explicit embedding-route choices shared by flags and the interactive wizard.
use super::{SetupChoices, SetupError, setup_llama, wire_gemma_preset};
use crate::config::LeIndexConfig;
use dialoguer::{Confirm, Input, Select};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Default, clap::Args)]
pub struct SetupOptions {
    /// Embedding route: qwen3 (ONNX), embeddinggemma2 (managed GGUF), remote, or keep.
    #[arg(long, value_parser = ["qwen3", "embeddinggemma2", "remote", "keep"])]
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
    /// External OpenAI-compatible embeddings base URL, including /v1 when required.
    #[arg(long)]
    pub embedding_endpoint: Option<String>,
    /// Model identifier sent to the external embeddings server.
    #[arg(long)]
    pub embedding_model: Option<String>,
    /// Expected remote vector dimension; omit to preserve an existing external route or discover it.
    #[arg(long)]
    pub embedding_dimension: Option<usize>,
    /// Environment variable containing the embeddings API key (not the key itself); empty clears authentication.
    #[arg(long)]
    pub embedding_api_key_env: Option<String>,
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
            || self.embedding_doc_template.is_some()
            || self.embedding_query_template.is_some()
            || self.system_one
    }
}

fn error(message: impl Into<String>) -> SetupError {
    SetupError::Conflict {
        message: message.into(),
    }
}

pub(super) fn select_interactive() -> Result<SetupOptions, SetupError> {
    let existing = LeIndexConfig::load().map_err(|e| SetupError::ConfigRead(e.to_string()))?;
    let selection = Select::new()
        .with_prompt("Embedding model / inference route")
        .items(&[
            "EmbeddingGemma2 — managed llama.cpp server (GGUF)",
            "Qwen3 — local ONNX Runtime",
            "External OpenAI-compatible embeddings server",
            "Keep the existing embedding configuration",
        ])
        .default(
            if existing
                .indexing
                .embeddings
                .resolved_remote_with(&existing.services)
                .is_some()
            {
                3
            } else {
                0
            },
        )
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
    Ok(SetupOptions {
        embedding: Some(["embeddinggemma2", "qwen3", "remote", "keep"][selection].into()),
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
    configure_route(config, options, route)
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
    if remote_options_set(options) && route != "remote" {
        return Err(error(
            "External embeddings options require --embedding remote",
        ));
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
            config.indexing.embeddings.enable_hybrid = true;
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
            configure_external(config, options)?;
            Ok(true)
        }
        _ => Err(error("Unknown embedding route")),
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

fn prompt<T>(label: &str, default: T) -> Result<T, SetupError>
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

fn configure_args(
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
    if !args
        .iter()
        .any(|arg| arg == "--pooling" || arg.starts_with("--pooling="))
    {
        args.extend(["--pooling".into(), "mean".into()]);
    }
    if layers.is_some() {
        strip_gpu_options(args);
    }
    if let Some(layers) = layers.or_else(|| existing.is_none().then_some(if cpu { 0 } else { 99 }))
    {
        args.extend(["-ngl".into(), layers.to_string()]);
    }
    Ok(())
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
            args.drain(index..index + 2);
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
                previous
                    .resolved_remote_with(&config.services)
                    .map(|r| r.endpoint)
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

/// Interactive prompts for the external route; defaults come from the resolved route (CCN-gate extraction).
fn prompt_external_route(
    route: &mut ExternalRoute,
    previous: &crate::config::EmbeddingConfig,
) -> Result<(), SetupError> {
    route.endpoint = prompt(
        "Embeddings base URL (for example http://localhost:9000/v1)",
        route.endpoint.clone(),
    )?;
    route.model = prompt("Server model identifier", route.model.clone())?;
    let d: usize = prompt(
        "Expected dimension (0 = discover)",
        previous.remote_dimension.unwrap_or(0),
    )?;
    route.dimension = (d != 0).then_some(d);
    route.key_env = Input::<String>::new()
        .with_prompt("API key environment variable (blank for none)")
        .with_initial_text(&route.key_env)
        .allow_empty(true)
        .interact_text()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;
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
fn validate_external_route(
    route: &ExternalRoute,
    options: &SetupOptions,
) -> Result<(), SetupError> {
    validate_external_url(&route.endpoint)?;
    if route.model.trim().is_empty() {
        return Err(error("External embeddings require --embedding-model"));
    }
    if route.dimension == Some(0) {
        return Err(error("Embedding dimension must be nonzero"));
    }
    check_api_key_env(&route.key_env, options)
}

/// Builds the replacement embeddings config for the external route (CCN-gate extraction).
fn external_embedding_config(
    route: ExternalRoute,
    previous: &crate::config::EmbeddingConfig,
    options: &SetupOptions,
    same_route: bool,
) -> crate::config::EmbeddingConfig {
    crate::config::EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: "custom".into(),
        remote_service: None,
        remote_endpoint: Some(route.endpoint),
        remote_model: Some(route.model),
        remote_dimension: route.dimension,
        remote_api_key: if same_route
            && options.embedding_api_key_env.is_none()
            && !options.interactive
        {
            previous.remote_api_key.clone()
        } else {
            None
        },
        remote_api_key_env: (!route.key_env.is_empty()).then_some(route.key_env),
        remote_doc_template: (!route.doc.is_empty()).then_some(route.doc),
        remote_query_template: (!route.query.is_empty()).then_some(route.query),
        onnx_model_path: if same_route {
            previous.onnx_model_path.clone()
        } else {
            None
        },
    }
}

fn configure_external(
    config: &mut LeIndexConfig,
    options: &SetupOptions,
) -> Result<(), SetupError> {
    let previous = &config.indexing.embeddings;
    let same_route = previous
        .remote_endpoint
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty())
        && !previous.embeddinggemma2_preset_active();
    let mut route = external_route_from_flags(config, options, same_route);
    if options.interactive {
        prompt_external_route(&mut route, previous)?;
    }
    validate_external_route(&route, options)?;
    config.indexing.embeddings = external_embedding_config(route, previous, options, same_route);
    Ok(())
}

#[cfg(test)]
#[path = "setup_routes_test.rs"]
mod test;
