//! Interactive configuration, status, and smoke test for the `[system_one]` gate.
//!
//! Config-only: the caller merges and saves `LeIndexConfig`. The one exception
//! is the managed llama-server binary, which `setup_llama::configure_binary` may
//! download after the user confirms it in its own prompt.

use super::SetupError;
use super::setup_llama::configure_binary;
use crate::config::{LeIndexConfig, ServiceConfig, SystemOneConfig};
use crate::feature_flags::FeatureFlag;
use dialoguer::{Confirm, Input, Select};
use std::path::Path;

#[cfg(feature = "system-one")]
use crate::cli::mcp::system_one::client::{HttpAsker, default_base_url, default_model};
#[cfg(feature = "system-one")]
use crate::cli::mcp::system_one::types::{Asker, Provider, Question, Request};
#[cfg(feature = "system-one")]
use serde_json::json;
#[cfg(feature = "system-one")]
use std::time::Duration;

const MODES: [&str; 3] = ["off", "shadow", "enforce"];
const PROVIDERS: [&str; 3] = ["laya", "jev", "openai"];
// CCN-gate extraction: llama-server flag spellings shared by the managed-service prompts.
const GPU_FLAGS: [&str; 3] = ["--n-gpu-layers", "-ngl", "--gpu-layers"];
const CTX_FLAGS: [&str; 2] = ["--ctx-size", "-c"];

fn conflict(message: impl Into<String>) -> SetupError {
    SetupError::Conflict {
        message: message.into(),
    }
}

fn prompt_error(error: impl std::fmt::Display) -> SetupError {
    SetupError::Interactive(error.to_string())
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// `Some(true)` for an enabling mode, `Some(false)` for off, `None` if unknown.
/// Mirrors `Mode::parse`, which is compiled only with the `system-one` feature.
fn mode_enabled(mode: &str) -> Option<bool> {
    match mode.trim().to_ascii_lowercase().as_str() {
        "" | "off" => Some(false),
        "shadow" | "enforce" => Some(true),
        _ => None,
    }
}

/// `Some(remote)` for a provider the gate accepts, `None` otherwise. Mirrors
/// `Provider::parse` (feature-gated), so the wizard works in every build.
fn provider_remote(provider: &str) -> Option<bool> {
    match provider.trim().to_ascii_lowercase().as_str() {
        "laya" => Some(false),
        "jev" | "typesafe" | "openai" => Some(true),
        _ => None,
    }
}

fn mode_index(mode: &str) -> usize {
    match mode.trim().to_ascii_lowercase().as_str() {
        "shadow" => 1,
        "enforce" => 2,
        _ => 0,
    }
}

fn provider_index(provider: &str) -> usize {
    match provider.trim().to_ascii_lowercase().as_str() {
        "jev" | "typesafe" => 1,
        "openai" => 2,
        _ => 0,
    }
}

/// An environment variable NAME: ASCII `[A-Za-z_][A-Za-z0-9_]*`. Anything else
/// (hyphens, dots, digit-first, non-ASCII, `=`, whitespace) is rejected so a
/// pasted key is never stored.
fn is_env_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Strip `user:password@` from a URL so status output never echoes credentials.
fn redact_userinfo(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_string();
    };
    let (scheme, rest) = url.split_at(scheme_end + 3);
    let authority_end = rest.find('/').unwrap_or(rest.len());
    match rest[..authority_end].rfind('@') {
        Some(at) => format!("{scheme}<redacted>@{}", &rest[at + 1..]),
        None => url.to_string(),
    }
}

/// Every reason the `[system_one]` section (after its preset overlay) cannot run.
/// Thresholds are probabilities or budgets with the runtime's own invariants;
/// no arbitrary upper bounds are imposed.
fn section_issues(config: &LeIndexConfig) -> Vec<String> {
    let section = config.system_one.effective();
    let mut issues = mode_provider_issues(&section);
    issues.extend(threshold_issues(&section));
    issues.extend(url_issues(&section));
    issues.extend(service_issues(config, &section));
    issues
}

/// CCN-gate extraction from `section_issues`: mode, provider, and key-env name.
fn mode_provider_issues(section: &SystemOneConfig) -> Vec<String> {
    let mut issues = Vec::new();
    if mode_enabled(&section.mode).is_none() {
        issues.push(format!(
            "unknown mode '{}' (use off|shadow|enforce)",
            section.mode
        ));
    }
    match provider_remote(&section.provider) {
        None => issues.push(format!(
            "unknown provider '{}' (use laya|jev|openai)",
            section.provider
        )),
        Some(true) if section.api_key_env.as_deref().is_none_or(str::is_empty) => {
            issues.push(
                "remote provider requires api_key_env (the NAME of an environment variable)"
                    .to_string(),
            );
        }
        Some(_) => {}
    }
    if let Some(name) = section.api_key_env.as_deref() {
        if !name.is_empty() && !is_env_name(name) {
            issues.push("api_key_env must be an environment variable NAME, not a key".to_string());
        }
    }
    issues
}

/// CCN-gate extraction from `section_issues`: timeout, top_n, and probability thresholds.
fn threshold_issues(section: &SystemOneConfig) -> Vec<String> {
    let mut issues = Vec::new();
    if section.timeout_ms == 0 {
        issues.push("timeout_ms must be at least 1".to_string());
    }
    if section.top_n < 2 {
        issues.push("top_n must be at least 2".to_string());
    }
    for (label, value) in [
        ("relevant_min", section.relevant_min),
        ("instructions_max", section.instructions_max),
        ("tau_apply", section.tau_apply),
    ] {
        if !(0.0..=1.0).contains(&value) {
            issues.push(format!("{label} must be a probability between 0 and 1"));
        }
    }
    issues
}

/// CCN-gate extraction from `section_issues`: base_url scheme.
fn url_issues(section: &SystemOneConfig) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(url) = section.base_url.as_deref() {
        if !(url.starts_with("http://") || url.starts_with("https://")) {
            issues.push("base_url must start with http:// or https://".to_string());
        }
    }
    issues
}

/// CCN-gate extraction from `section_issues`: the named managed service reference.
fn service_issues(config: &LeIndexConfig, section: &SystemOneConfig) -> Vec<String> {
    let mut issues = Vec::new();
    if let Some(name) = section.service.as_deref() {
        match config.services.get(name) {
            None => issues.push(format!("service '{name}' has no [services.{name}] entry")),
            Some(service) => {
                if section.base_url.is_none() && service.port == 0 {
                    issues.push(format!("service '{name}' port must be set (non-zero)"));
                }
                if service.auto_start && service.model.is_none() {
                    issues.push(format!("service '{name}' auto_start needs a model path"));
                }
                // Model-path existence is an environment check, not a config
                // invariant: it would fail any config synced from another
                // machine and would wrongly gate `check_probe`, which spawns
                // nothing. `start_locked` → `wait_healthy` surfaces a real
                // diagnostic when a spawn attempt actually needs the file.
            }
        }
    }
    issues
}

/// Refuse to enable the gate in a build compiled without `system-one`.
fn enable_guard(mode: &str) -> Result<(), SetupError> {
    if mode_enabled(mode) == Some(true) && !cfg!(feature = "system-one") {
        return Err(conflict(
            "System-One gate is not compiled into this build; rebuild with `--features system-one` or keep mode off",
        ));
    }
    Ok(())
}

/// Clear `active_preset` when this run changed the section. Saved presets are never touched.
fn finish_section(original: &SystemOneConfig, mut edited: SystemOneConfig) -> SystemOneConfig {
    if edited != *original {
        edited.active_preset = None;
    }
    edited
}

/// Replace the first matching `flags <value>` pair (or `flag=value`), keeping every other argument.
fn set_extra_arg(args: &mut Vec<String>, flags: &[&str], value: &str) {
    let is_flag = |arg: &str| {
        flags
            .iter()
            .any(|flag| arg == *flag || arg.starts_with(&format!("{flag}=")))
    };
    let old = std::mem::take(args);
    let mut rest = old.into_iter();
    while let Some(arg) = rest.next() {
        if !is_flag(&arg) {
            args.push(arg);
        } else if !arg.contains('=') {
            rest.next();
        }
    }
    args.push(flags[0].to_string());
    args.push(value.to_string());
}

fn extra_arg_value(args: &[String], flags: &[&str]) -> Option<String> {
    args.iter().enumerate().find_map(|(i, arg)| {
        flags
            .contains(&arg.as_str())
            .then(|| args.get(i + 1).cloned())
            .flatten()
    })
}

/// CCN-gate extraction from `configure_managed`: service name prompt.
fn prompt_service_name(config: &LeIndexConfig) -> Result<String, SetupError> {
    let current_name = config
        .system_one
        .service
        .clone()
        .unwrap_or_else(|| "laya".to_string());
    let name = Input::<String>::new()
        .with_prompt("Service name ([services.<name>])")
        .default(current_name)
        .validate_with(|s: &String| -> Result<(), &str> {
            if s.trim().is_empty() || s.contains(char::is_whitespace) {
                Err("use a non-empty name without spaces")
            } else {
                Ok(())
            }
        })
        .interact_text()
        .map_err(prompt_error)?;
    Ok(name.trim().to_string())
}

/// CCN-gate extraction from `configure_managed`: absolute GGUF model path prompt.
fn prompt_model_path(service: &ServiceConfig) -> Result<String, SetupError> {
    let mut model = Input::<String>::new()
        .with_prompt("Absolute path to the Laya GGUF model file")
        .validate_with(|p: &String| -> Result<(), &str> {
            let path = Path::new(p.trim());
            if path.is_absolute() && path.is_file() {
                Ok(())
            } else {
                Err("must be an absolute path to an existing file")
            }
        });
    if let Some(existing) = service.model.clone() {
        model = model.default(existing);
    }
    model.interact_text().map_err(prompt_error)
}

/// CCN-gate extraction from `configure_managed`: GPU layers and context size prompts.
fn prompt_runtime_args(service: &ServiceConfig) -> Result<(String, String), SetupError> {
    let gpu_layers = Input::<String>::new()
        .with_prompt("GPU layers (integer, auto, or all)")
        .default(
            extra_arg_value(&service.extra_args, &GPU_FLAGS).unwrap_or_else(|| "0".to_string()),
        )
        .validate_with(|v: &String| -> Result<(), &str> {
            let v = v.trim();
            if v == "auto" || v == "all" || v.parse::<u32>().is_ok() {
                Ok(())
            } else {
                Err("use a non-negative integer, auto, or all")
            }
        })
        .interact_text()
        .map_err(prompt_error)?;
    let context = Input::<String>::new()
        .with_prompt("Context size (tokens)")
        .default(
            extra_arg_value(&service.extra_args, &CTX_FLAGS).unwrap_or_else(|| "4096".to_string()),
        )
        .validate_with(|v: &String| -> Result<(), &str> {
            match v.trim().parse::<u32>() {
                Ok(n) if n > 0 => Ok(()),
                _ => Err("must be a positive integer"),
            }
        })
        .interact_text()
        .map_err(prompt_error)?;
    Ok((gpu_layers, context))
}

fn validate_listen_host(value: &str) -> Result<(), &'static str> {
    let host = value.trim();
    if host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
    {
        Ok(())
    } else {
        Err("managed services require a loopback host such as 127.0.0.1 or ::1")
    }
}

/// CCN-gate extraction from `configure_managed`: listen host and port prompts.
fn prompt_listen(service: &ServiceConfig) -> Result<(String, u16), SetupError> {
    let host = Input::<String>::new()
        .with_prompt("Listen host")
        .default(service.host.clone())
        .validate_with(|v: &String| -> Result<(), &str> { validate_listen_host(v) })
        .interact_text()
        .map_err(prompt_error)?;
    let mut port_prompt = Input::<u16>::new()
        .with_prompt("Listen port")
        .validate_with(|v: &u16| -> Result<(), &str> {
            if *v > 0 {
                Ok(())
            } else {
                Err("must be a non-zero port")
            }
        });
    if service.port > 0 {
        port_prompt = port_prompt.default(service.port);
    }
    let port = port_prompt.interact_text().map_err(prompt_error)?;
    Ok((host, port))
}

/// CCN-gate extraction from `configure_managed`: on-demand start and model load timeout prompts.
fn prompt_lifecycle(service: &ServiceConfig) -> Result<(bool, u64), SetupError> {
    let auto_start = Confirm::new()
        .with_prompt("Start this service on demand when it is down?")
        .default(service.auto_start)
        .interact()
        .map_err(prompt_error)?;
    let start_timeout = Input::<u64>::new()
        .with_prompt("Seconds to wait for the model to load")
        .default(service.start_timeout_secs)
        .validate_with(|v: &u64| -> Result<(), &str> {
            if *v >= 1 {
                Ok(())
            } else {
                Err("must be at least 1 second")
            }
        })
        .interact_text()
        .map_err(prompt_error)?;
    Ok((auto_start, start_timeout))
}

fn configure_managed(config: &mut LeIndexConfig) -> Result<(), SetupError> {
    let name = prompt_service_name(config)?;

    let mut service = config.services.get(&name).cloned().unwrap_or_default();
    service.binary = configure_binary(&service.binary)?;
    let model = prompt_model_path(&service)?;
    let (gpu_layers, context) = prompt_runtime_args(&service)?;
    let (host, port) = prompt_listen(&service)?;
    let (auto_start, start_timeout) = prompt_lifecycle(&service)?;

    set_extra_arg(&mut service.extra_args, &GPU_FLAGS, gpu_layers.trim());
    set_extra_arg(&mut service.extra_args, &CTX_FLAGS, context.trim());
    service.model = Some(model.trim().to_string());
    service.host = host.trim().to_string();
    service.port = port;
    service.auto_start = auto_start;
    service.start_timeout_secs = start_timeout;
    config.services.insert(name.clone(), service);

    // The managed endpoint wins over stale external settings: clear them so the
    // service's derived base URL and served model name are the ones used.
    config.system_one.service = Some(name);
    config.system_one.base_url = None;
    config.system_one.model = None;
    config.system_one.api_key_env = None;
    Ok(())
}

/// CCN-gate extraction from `configure_external`: endpoint base URL prompt.
fn prompt_base_url(section: &SystemOneConfig) -> Result<String, SetupError> {
    let mut endpoint = Input::<String>::new()
        .with_prompt("Endpoint base URL")
        .validate_with(|u: &String| -> Result<(), &str> {
            if u.starts_with("http://") || u.starts_with("https://") {
                Ok(())
            } else {
                Err("must start with http:// or https://")
            }
        });
    if let Some(existing) = section.base_url.clone() {
        endpoint = endpoint.default(existing);
    }
    endpoint.interact_text().map_err(prompt_error)
}

/// CCN-gate extraction from `configure_external`: API key environment variable NAME prompt.
fn prompt_key_env(section: &SystemOneConfig, remote: bool) -> Result<String, SetupError> {
    let mut key_env = Input::<String>::new()
        .with_prompt(if remote {
            "Environment variable NAME holding the API key (not the key)"
        } else {
            "API key environment variable NAME (empty = none)"
        })
        .allow_empty(true)
        .validate_with(|n: &String| -> Result<(), &str> {
            if n.is_empty() || is_env_name(n) {
                Ok(())
            } else {
                Err("must be an environment variable NAME, not a key")
            }
        });
    if let Some(existing) = section.api_key_env.clone() {
        key_env = key_env.default(existing);
    }
    key_env.interact_text().map_err(prompt_error)
}

/// CCN-gate extraction from `configure_external`: warn when the named key variable is unusable here.
fn warn_key_env_state(key_env: &str) {
    if key_env.is_empty() {
        return;
    }
    match std::env::var(key_env) {
        Ok(v) if v.is_empty() => println!(
            "warning: {key_env} is empty in this environment; the gate stays inactive until it is set"
        ),
        Err(_) => println!(
            "warning: {key_env} is not set in this environment; the gate stays inactive until it is set"
        ),
        Ok(_) => {}
    }
}

fn configure_external(section: &mut SystemOneConfig, provider: &str) -> Result<(), SetupError> {
    let base_url = prompt_base_url(section)?;

    let mut model = Input::<String>::new()
        .with_prompt("Model name (empty = provider default)")
        .allow_empty(true);
    if let Some(existing) = section.model.clone() {
        model = model.default(existing);
    }
    let model = model.interact_text().map_err(prompt_error)?;

    let key_env = prompt_key_env(section, provider != "laya")?;
    warn_key_env_state(&key_env);

    section.service = None;
    section.base_url = Some(base_url.trim().to_string());
    section.model = Some(model.trim().to_string()).filter(|m| !m.is_empty());
    section.api_key_env = Some(key_env.trim().to_string()).filter(|k| !k.is_empty());
    Ok(())
}

fn prob_input(prompt: &str, current: f32) -> Result<f32, SetupError> {
    Input::<f32>::new()
        .with_prompt(prompt)
        .default(current)
        .validate_with(|v: &f32| -> Result<(), &str> {
            if (0.0..=1.0).contains(v) {
                Ok(())
            } else {
                Err("must be a probability between 0 and 1")
            }
        })
        .interact_text()
        .map_err(prompt_error)
}

fn configure_thresholds(section: &mut SystemOneConfig) -> Result<(), SetupError> {
    let change = Confirm::new()
        .with_prompt("Change timeout, gate toggles, or thresholds?")
        .default(false)
        .interact()
        .map_err(prompt_error)?;
    if !change {
        return Ok(());
    }
    section.timeout_ms = Input::<u64>::new()
        .with_prompt("Per-request timeout in ms (over budget fails open)")
        .default(section.timeout_ms)
        .validate_with(|v: &u64| -> Result<(), &str> {
            if *v >= 1 {
                Ok(())
            } else {
                Err("must be at least 1 ms")
            }
        })
        .interact_text()
        .map_err(prompt_error)?;
    section.pre_gate = Confirm::new()
        .with_prompt("Pre-gate: validate tool calls before they run")
        .default(section.pre_gate)
        .interact()
        .map_err(prompt_error)?;
    section.post_gate = Confirm::new()
        .with_prompt("Post-gate: classify search results")
        .default(section.post_gate)
        .interact()
        .map_err(prompt_error)?;
    section.tau_apply = prob_input(
        "Pre-gate: auto-apply a repair at or above this confidence",
        section.tau_apply,
    )?;
    section.relevant_min = prob_input(
        "Post-gate: drop a result when P(relevant) is below",
        section.relevant_min,
    )?;
    section.instructions_max = prob_input(
        "Post-gate: drop a result when P(contains instructions) exceeds",
        section.instructions_max,
    )?;
    section.top_n = Input::<usize>::new()
        .with_prompt("Post-gate: results judged per call")
        .default(section.top_n)
        .validate_with(|v: &usize| -> Result<(), &str> {
            if *v >= 2 {
                Ok(())
            } else {
                Err("must be at least 2")
            }
        })
        .interact_text()
        .map_err(prompt_error)?;
    Ok(())
}

/// Interactive `[system_one]` setup. Works on a candidate copy so a rejected
/// answer leaves `config` untouched. The caller saves it.
pub(super) fn configure_interactive(config: &mut LeIndexConfig) -> Result<(), SetupError> {
    let original = config.system_one.clone();
    let mut candidate = config.clone();

    let mode_choice = Select::new()
        .with_prompt("System-One gate mode")
        .items(&[
            "off (inert)",
            "shadow (decide and log only)",
            "enforce (apply decisions)",
        ])
        .default(mode_index(&original.mode))
        .interact()
        .map_err(prompt_error)?;
    let mode = MODES[mode_choice];
    enable_guard(mode)?;
    candidate.system_one.mode = mode.to_string();

    if mode != "off" {
        let provider_choice = Select::new()
            .with_prompt("Provider")
            .items(&["laya (local)", "jev (TypeSafe, remote)", "openai (remote)"])
            .default(provider_index(&original.provider))
            .interact()
            .map_err(prompt_error)?;
        let provider = PROVIDERS[provider_choice];
        candidate.system_one.provider = provider.to_string();

        let managed = provider == "laya"
            && Select::new()
                .with_prompt("Laya backend")
                .items(&[
                    "managed: LeIndex runs llama-server with a local GGUF",
                    "external: an existing HTTP endpoint",
                ])
                .default(if original.service.is_some() { 0 } else { 1 })
                .interact()
                .map_err(prompt_error)?
                == 0;
        if managed {
            configure_managed(&mut candidate)?;
        } else {
            configure_external(&mut candidate.system_one, provider)?;
        }
        configure_thresholds(&mut candidate.system_one)?;
    }

    candidate.system_one = finish_section(&original, candidate.system_one);
    let issues = section_issues(&candidate);
    if !issues.is_empty() {
        return Err(conflict(issues.join("; ")));
    }
    *config = candidate;
    Ok(())
}

/// Redacted, secret-free status. Returns true only when the gate is fully
/// working: enabled mode, compiled feature, runtime flag, and no config issues.
pub(super) fn print_status(config: &LeIndexConfig) -> bool {
    let section = config.system_one.effective();
    let issues = section_issues(config);
    let build = cfg!(feature = "system-one");
    let runtime = FeatureFlag::SystemOne.is_enabled();
    let mode_on = mode_enabled(&section.mode) == Some(true);

    let mode_label = if section.mode.trim().is_empty() {
        "off"
    } else {
        section.mode.trim()
    };
    println!("System One");
    println!("  mode: {mode_label}");
    println!("  provider: {}", section.provider);
    println!("  build (system-one feature): {}", yes_no(build));
    println!(
        "  runtime flag (LEINDEX_FEATURE_SYSTEM_ONE): {}",
        yes_no(runtime)
    );
    println!("  endpoint: {}", endpoint_label(config, &section));
    if let Some(name) = section.api_key_env.as_deref() {
        let set = std::env::var(name).is_ok_and(|v| !v.is_empty());
        println!(
            "  api key env {name}: {}",
            if set { "set" } else { "unset" }
        );
    }
    if let Some(name) = section.service.as_deref() {
        if let Some(service) = config.services.get(name) {
            println!(
                "  service {name}: binary={} model={} port={} auto_start={}",
                service.binary,
                service.model.as_deref().unwrap_or("(none)"),
                service.port,
                service.auto_start
            );
        }
    }
    // A build compiled without the feature can never reach `active`, and no
    // amount of config editing changes that. Name the remedy here: a failing
    // exit code with zero `issue:` lines is an unactionable failure, and this
    // was exactly that before `full` started including `system-one`.
    if !build {
        println!(
            "  issue: System-One gate is not compiled into this build; rebuild with \
             `--features system-one` (included in `full`)"
        );
    }
    for issue in &issues {
        println!("  issue: {issue}");
    }
    let ready = mode_on && build && runtime && issues.is_empty();
    if ready {
        println!("  status: active");
    } else {
        println!("  status: INACTIVE, not fully working");
    }
    ready
}

fn endpoint_label(config: &LeIndexConfig, section: &SystemOneConfig) -> String {
    if let Some(url) = section.base_url.as_deref() {
        return redact_userinfo(url);
    }
    if let Some(service) = section
        .service
        .as_deref()
        .and_then(|name| config.services.get(name))
    {
        return format!("{} (from service)", service.base_url());
    }
    "provider default".to_string()
}

/// Minimal yes/no round trip through the production client. Fails loudly: there
/// is no fail-open here, because the user asked whether the provider works.
/// Starts the managed service first when it is configured with `auto_start`.
pub(super) fn smoke_test(config: &LeIndexConfig) -> Result<(), SetupError> {
    smoke_test_with(config, true)
}

/// Same round trip as [`smoke_test`] but never starts a managed service, so the
/// `--check` probe spawns nothing: a stopped service fails here.
pub(super) fn check_probe(config: &LeIndexConfig) -> Result<(), SetupError> {
    smoke_test_with(config, false)
}

fn smoke_test_with(config: &LeIndexConfig, start_managed: bool) -> Result<(), SetupError> {
    #[cfg(not(feature = "system-one"))]
    {
        let _ = (config, start_managed);
        Err(conflict(
            "System-One gate is not compiled into this build; rebuild with `--features system-one`",
        ))
    }
    #[cfg(feature = "system-one")]
    {
        smoke_test_built(config, start_managed)
    }
}

/// CCN-gate extraction from `smoke_test_built`: resolves the configured managed service.
/// A start-capable run may spawn it; a probe run only validates the model path.
#[cfg(feature = "system-one")]
fn resolve_service<'a>(
    config: &'a LeIndexConfig,
    section: &SystemOneConfig,
    start_managed: bool,
) -> Result<Option<&'a ServiceConfig>, SetupError> {
    let Some(name) = section.service.as_deref() else {
        return Ok(None);
    };
    let svc = config
        .services
        .get(name)
        .ok_or_else(|| conflict(format!("service '{name}' has no entry")))?;
    if !start_managed {
        // check_probe spawns nothing, so a missing model file can never
        // recover: report it as the precise config defect it is, before
        // dialing a port that will not answer. The start path instead
        // lets `ensure_running` → `wait_healthy` surface the failure.
        if let Some(model) = svc.model.as_deref() {
            if !Path::new(model).is_file() {
                return Err(conflict(format!("service '{name}' model file not found")));
            }
        }
    } else {
        // ensure_running only spawns when auto_start and model are both set.
        crate::services::ensure_running(name, svc)
            .map_err(|e| SetupError::Io(format!("service '{name}': {e}")))?;
    }
    Ok(Some(svc))
}

/// CCN-gate extraction from `smoke_test_built`: section > service > provider default.
#[cfg(feature = "system-one")]
fn resolve_endpoint(
    section: &SystemOneConfig,
    service: Option<&ServiceConfig>,
    provider: Provider,
) -> (String, String) {
    let base_url = section
        .base_url
        .clone()
        .or_else(|| service.map(ServiceConfig::base_url))
        .unwrap_or_else(|| default_base_url(provider).to_string());
    let model = section
        .model
        .clone()
        .or_else(|| service.and_then(ServiceConfig::served_name))
        .unwrap_or_else(|| default_model(provider).to_string());
    (base_url, model)
}

/// CCN-gate extraction from `smoke_test_built`: one yes/no round trip through the provider client.
#[cfg(feature = "system-one")]
fn ask_verify(asker: &HttpAsker, timeout: Duration, timeout_ms: u64) -> Result<(), SetupError> {
    let request = Request {
        state: json!({"check": "leindex setup smoke test"}),
        questions: vec![(
            "ok".to_string(),
            Question::YesNo {
                instructions: "The input text is the literal phrase: leindex setup smoke test."
                    .to_string(),
            },
        )],
    };
    let fut = async { tokio::time::timeout(timeout, asker.ask(&request)).await };
    // Drive the probe on a thread that owns a scratch current-thread runtime.
    // This is the shape used by `index_builder::hybrid::block_on_remote`, and it
    // is the only one that is correct in every context the probe runs from:
    //   * standalone (interactive wizard) — trivially fine;
    //   * inside the CLI's multi-threaded runtime — `block_on` directly panics
    //     with "Cannot start a runtime from within a runtime";
    //   * inside a CURRENT-THREAD runtime — `Handle::try_current()` still returns
    //     `Ok`, so a `block_in_place` branch would be taken and would panic,
    //     because a current-thread runtime has no blocking pool to hop to.
    // The future borrows `asker` and `request`, so `thread::scope` (not `spawn`)
    // avoids cloning them. The closure's own error type is stated explicitly:
    // left to inference it is ambiguous against the `?` inside the scope.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| SetupError::Io(format!("cannot start smoke-test runtime: {e}")))?;
    let joined = std::thread::scope(|scope| scope.spawn(move || runtime.block_on(fut)).join());
    let outcome =
        joined.map_err(|_| SetupError::Io("smoke-test bridge thread panicked".to_string()))?;
    let response = match outcome {
        Err(_) => {
            return Err(SetupError::Io(format!(
                "smoke test timed out after {timeout_ms} ms"
            )));
        }
        Ok(Err(error)) => {
            return Err(SetupError::Io(format!(
                "smoke test request failed: {error}"
            )));
        }
        Ok(Ok(response)) => response,
    };
    response.p_yes("ok").map(|_| ()).ok_or_else(|| {
        SetupError::Io("provider answered without a usable yes/no decision".to_string())
    })
}

#[cfg(feature = "system-one")]
fn smoke_test_built(config: &LeIndexConfig, start_managed: bool) -> Result<(), SetupError> {
    let issues = section_issues(config);
    if !issues.is_empty() {
        return Err(conflict(issues.join("; ")));
    }
    if !FeatureFlag::SystemOne.is_enabled() {
        return Err(conflict(
            "System-One runtime flag is off; set LEINDEX_FEATURE_SYSTEM_ONE=1 to test the gate",
        ));
    }
    let section = config.system_one.effective();
    if mode_enabled(&section.mode) != Some(true) {
        return Err(conflict(
            "System-One mode is off; set shadow or enforce before smoke testing",
        ));
    }
    let provider = Provider::parse(&section.provider)
        .ok_or_else(|| conflict(format!("unknown provider '{}'", section.provider)))?;

    let service = resolve_service(config, &section, start_managed)?;
    let (base_url, model) = resolve_endpoint(&section, service, provider);

    let api_key = section
        .api_key_env
        .as_deref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|key| !key.is_empty());
    if provider.is_remote() && api_key.is_none() {
        return Err(conflict(
            "remote provider needs its api_key_env variable set in this environment",
        ));
    }

    let timeout = Duration::from_millis(section.timeout_ms);
    let asker = HttpAsker::new(provider, &base_url, &model, api_key, timeout)
        .map_err(|e| SetupError::Io(format!("cannot build provider client: {e}")))?;
    ask_verify(&asker, timeout, section.timeout_ms)
}

#[cfg(test)]
#[path = "setup_system_one_test.rs"]
mod test;
