//! Route-aware, non-starting setup diagnostics. Local ONNX checks stay with setup.

use super::{SetupError, SmokeTestResult};
use crate::config::{LeIndexConfig, ResolvedRemote, ServiceConfig};
use std::path::Path;

/// `None` delegates to local ONNX diagnostics; `Some` is this route's readiness.
/// Checks may make bounded HTTP requests, but never start a managed service.
pub(super) fn remote_status(config: &LeIndexConfig) -> Result<Option<bool>, SetupError> {
    let neural_flag = crate::feature_flags::FeatureFlag::NeuralSearch.is_enabled();
    let remote_flag = crate::feature_flags::FeatureFlag::RemoteEmbeddings.is_enabled();
    status_with(
        config,
        neural_flag,
        remote_flag,
        cfg!(feature = "remote-embeddings"),
        || {
            #[cfg(feature = "remote-embeddings")]
            {
                super::run_remote_smoke_test_inner(
                    &config.indexing.embeddings,
                    &config.services,
                    false,
                )
            }
            #[cfg(not(feature = "remote-embeddings"))]
            {
                unreachable!("remote probe is gated by the compiled capability")
            }
        },
    )
}

fn status_with(
    config: &LeIndexConfig,
    neural_flag: bool,
    remote_flag: bool,
    remote_build: bool,
    probe: impl FnOnce() -> SmokeTestResult,
) -> Result<Option<bool>, SetupError> {
    println!("Neural configured: {}", config.neural.enabled);
    println!("Neural runtime flag: {neural_flag}");
    if !config.neural.enabled {
        println!("Status: Ready for lexical search (neural OFF; no runtime or model required)");
        return Ok(Some(true));
    }
    let remote = config
        .indexing
        .embeddings
        .resolved_remote_with(&config.services);
    if !remote_requested(config, remote.is_some()) {
        return Ok(local_route_status(neural_flag));
    }
    println!("Neural route: remote (ORT and local Qwen model are not required)");
    if !remote_capabilities_enabled(neural_flag, remote_flag, remote_build) {
        return Ok(Some(false));
    }
    let Some(remote) = remote else {
        println!("Status: Remote neural configuration has no resolvable endpoint");
        return Ok(Some(false));
    };
    let endpoint = parse_remote_endpoint(&remote.endpoint)?;
    println!("Remote endpoint: {endpoint}");
    print_remote_details(&remote);
    if remote.remote_dimension == Some(0) {
        println!("Status: Remote dimension must be greater than zero");
        return Ok(Some(false));
    }
    if !remote_service_ready(config) {
        return Ok(Some(false));
    }
    let result = probe();
    let passed = remote_probe_passed(&remote, &result);
    print_probe_outcome(passed, &result);
    Ok(Some(passed))
}

// CCN-gate extraction: lexical-only verdict when no remote route is requested; `None` defers to local ONNX.
fn local_route_status(neural_flag: bool) -> Option<bool> {
    if !neural_flag {
        println!(
            "Status: Neural search disabled by its runtime flag; lexical search remains available"
        );
        return Some(false);
    }
    None
}

// CCN-gate extraction: a remote route is requested when hybrid is enabled and an endpoint source is set.
fn remote_requested(config: &LeIndexConfig, resolved: bool) -> bool {
    let embeddings = &config.indexing.embeddings;
    embeddings.enable_hybrid
        && (resolved || embeddings.remote_endpoint.is_some() || embeddings.remote_service.is_some())
}

// CCN-gate extraction: prints remote build/runtime capability; false when either gate is closed.
fn remote_capabilities_enabled(neural_flag: bool, remote_flag: bool, remote_build: bool) -> bool {
    println!("Remote build capability: {remote_build}");
    println!("Remote runtime flag: {remote_flag}");
    if !neural_flag || !remote_flag || !remote_build {
        println!(
            "Status: Remote neural search is configured but disabled by build/runtime capabilities"
        );
        return false;
    }
    true
}

// CCN-gate extraction: parses the remote endpoint, rejecting credentials, query and fragment.
fn parse_remote_endpoint(raw: &str) -> Result<reqwest::Url, SetupError> {
    // Refuse to print URL credentials or query values into diagnostics.
    let endpoint = reqwest::Url::parse(raw)
        .map_err(|_| SetupError::Io("Remote endpoint is not a valid URL".into()))?;
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(SetupError::Io("Remote endpoint must be an HTTP(S) URL without credentials, query, or fragment; configure authentication separately".into()));
    }
    Ok(endpoint)
}

// CCN-gate extraction: prints the resolved remote model, dimension and template status.
fn print_remote_details(remote: &ResolvedRemote) {
    println!(
        "Remote model: {}",
        remote.model.as_deref().unwrap_or("provider default")
    );
    println!(
        "Expected dimension: {}",
        remote
            .remote_dimension
            .map(|value| value.to_string())
            .unwrap_or_else(|| "measured by probe".into())
    );
    println!(
        "Document template: {}",
        if remote.doc_template.is_some() {
            "configured"
        } else {
            "none"
        }
    );
    println!(
        "Query template: {}",
        if remote.query_template.is_some() {
            "configured"
        } else {
            "none"
        }
    );
}

// CCN-gate extraction: external endpoint takes precedence; otherwise checks the managed service without starting it.
fn remote_service_ready(config: &LeIndexConfig) -> bool {
    let embeddings = &config.indexing.embeddings;
    let explicit_endpoint = embeddings
        .remote_endpoint
        .as_deref()
        .is_some_and(|value| !value.trim().is_empty());
    if explicit_endpoint {
        println!(
            "Service lifecycle: external endpoint takes precedence; no managed service checked or started"
        );
        return true;
    }
    let Some(name) = embeddings
        .remote_service
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
    else {
        return true;
    };
    let Some(service) = config.services.get(name) else {
        println!("Status: Referenced remote service is missing");
        return false;
    };
    println!("Managed service: {name}; auto_start={}", service.auto_start);
    println!(
        "Managed platform capability: {}",
        if cfg!(target_os = "linux") {
            "Linux supported"
        } else {
            "unsupported; use external HTTP"
        }
    );
    if !managed_files_ready(service) {
        println!("Status: Managed runtime/model paths are incomplete or unsupported");
        return false;
    }
    println!(
        "Managed HTTP health: {}",
        if crate::services::http_ready(&service.host, service.port) {
            "ready"
        } else {
            "not ready (check does not start services)"
        }
    );
    true
}

// CCN-gate extraction: probe passes only with a measured non-zero dimension matching any configured one.
fn remote_probe_passed(remote: &ResolvedRemote, result: &SmokeTestResult) -> bool {
    result.passed
        && !result.skipped
        && result.dimension.is_some_and(|dimension| dimension > 0)
        && remote
            .remote_dimension
            .is_none_or(|expected| result.dimension == Some(expected))
}

// CCN-gate extraction: prints the probe verdict, measured dimension and remediation hint.
fn print_probe_outcome(passed: bool, result: &SmokeTestResult) {
    println!(
        "Remote embedding probe: {}",
        if passed {
            "PASS"
        } else {
            "FAIL (not verified)"
        }
    );
    if let Some(dimension) = result.dimension {
        println!("Measured dimension: {dimension}");
    }
    if !passed {
        // Provider errors can contain URLs/credentials. Keep detailed errors in the
        // production scrubbed logging path rather than printing arbitrary payloads.
        println!(
            "Status: Remote neural search is not verified; check endpoint, authentication, model and vector dimension"
        );
    } else {
        println!("Status: Remote neural search verified by an embedding response");
    }
}

fn managed_files_ready(service: &ServiceConfig) -> bool {
    if !cfg!(target_os = "linux") || service.port == 0 {
        return false;
    }
    let binary = Path::new(&service.binary);
    let binary_ready = binary.is_absolute() && binary.is_file();
    #[cfg(unix)]
    let binary_ready = {
        use std::os::unix::fs::PermissionsExt;
        binary_ready
            && std::fs::metadata(binary)
                .is_ok_and(|metadata| metadata.permissions().mode() & 0o111 != 0)
    };
    let model_ready = service.model.as_deref().is_some_and(|model| {
        let model = Path::new(model);
        model.is_absolute() && model.is_file()
    });
    println!(
        "Managed binary: {}",
        if binary_ready {
            "present and executable"
        } else {
            "missing, nonabsolute or nonexecutable"
        }
    );
    println!(
        "Managed model: {}",
        if model_ready {
            "present"
        } else {
            "missing or nonabsolute"
        }
    );
    binary_ready && model_ready
}

#[cfg(test)]
#[path = "setup_status_test.rs"]
mod test;
