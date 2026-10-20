//! `leindex setup --check`: read-only status report.
//!
//! Extracted from `setup.rs` (Large-File gate, >2000-line limit). The check
//! path reads config and prints ORT/model/search/system-one status without
//! modifying anything (VAL-SETUP-014/020/034).

use super::SetupError;
use super::setup_models::{
    ModelChecksumStatus, check_model_present_for_name, model_checksum_status_for_name,
};
use super::setup_ort::{
    VersionCompatibility, check_ort_version_compatibility, discover_ort_path, get_ort_version,
};
use super::{setup_status, setup_system_one};
use std::path::PathBuf;

/// Result of --check mode.
#[derive(Debug, Clone)]
pub struct CheckResult {
    /// Whether all components are ready for neural search.
    pub fully_configured: bool,
}

fn print_check_ort_status(
    ort_installed: bool,
    ort_version: Option<&str>,
    ort_path: Option<&std::path::Path>,
    configured_path: Option<&str>,
) {
    let ort_status = if ort_installed {
        "installed"
    } else {
        "not installed"
    };
    println!("ORT (onnxruntime): {}", ort_status);

    if let Some(version) = ort_version {
        println!("ORT version:        {}", version);
        match check_ort_version_compatibility(version) {
            VersionCompatibility::Supported => {}
            VersionCompatibility::Unsupported {
                required_min,
                reason,
            } => {
                println!(
                    "  -> WARNING: detected {} but LeIndex requires {} ({}).",
                    version, required_min, reason
                );
                println!("     Re-run `leindex setup --neural --cpu` to upgrade.");
            }
            VersionCompatibility::TooNew {
                supported_max,
                reason,
            } => {
                println!(
                    "  -> WARNING: {} is newer than the supported maximum ({}). {}",
                    version, supported_max, reason
                );
            }
        }
    } else if ort_installed {
        println!("ORT version:        (unable to determine)");
    }

    if let Some(path) = ort_path {
        println!("ORT dylib path:     {}", path.display());
    } else if let Some(config_path) = configured_path {
        println!("ORT dylib (config): {} [file missing]", config_path);
    } else {
        println!("ORT dylib path:     (not discovered)");
    }
}

fn print_check_overall_status(
    fully_configured: bool,
    neural_enabled: bool,
    ort_installed: bool,
    model_present: bool,
    model_name: &str,
) {
    if fully_configured {
        println!("Status: Fully configured for neural search");
    } else if neural_enabled {
        println!("Status: Neural enabled but incomplete");
        if !ort_installed {
            println!("  -> Install ORT: leindex setup --neural --cpu");
        }
        if !model_present {
            println!(
                "  -> Model files needed for {}: run leindex setup --neural",
                model_name
            );
        }
    } else {
        println!("Status: TF-IDF only (neural not configured)");
        println!("  -> To enable neural search: leindex setup --neural --cpu");
    }
}

fn print_check_model_checksum(status: &ModelChecksumStatus) {
    match status {
        ModelChecksumStatus::Ok => {
            println!("Model checksum:     verified (matches checksums.sha256)");
        }
        ModelChecksumStatus::Unknown => {
            println!("Model checksum:     no manifest entry (cannot verify)");
        }
        ModelChecksumStatus::Mismatch { expected, actual } => {
            println!(
                "Model checksum:     MISMATCH (expected {}..., got {}...).",
                &expected[..expected.len().min(12)],
                &actual[..actual.len().min(12)],
            );
            println!("     Re-run `leindex setup --neural --cpu` to re-download.");
        }
        ModelChecksumStatus::Missing => {
            // Already reported via Model files: absent.
        }
    }
}

fn print_check_search_settings(search: &crate::config::SearchConfig) {
    println!();
    println!("Search mode:        {}", search.search_mode);
    println!("Neural weight:      {}", search.neural_weight);
    println!(
        "Rerank enabled:     {}",
        if search.rerank_effectively_enabled() {
            "ON"
        } else {
            "OFF"
        }
    );
    println!("Rerank backend:     {}", search.rerank_backend);
    // A configured `system-one` backend is only constructible with the feature
    // compiled in AND `LEINDEX_FEATURE_SYSTEM_ONE` set. Without this line the
    // report says "backend: system-one, enabled: OFF" with no reason, or — the
    // bug this guards — "enabled: ON" while `asker_from_config` refuses and
    // every search silently falls open.
    if search.rerank_backend == "system-one" && !crate::config::system_one_rerank_available() {
        println!(
            "  -> system-one is unavailable: {}",
            if cfg!(feature = "system-one") {
                format!(
                    "set {}=1 to enable the experimental gate",
                    crate::feature_flags::FeatureFlag::SystemOne.env_var()
                )
            } else {
                "this binary was built without the `system-one` feature".to_string()
            }
        );
    }
    println!(
        "Fragment index:     {}",
        if search.fragment_index_enabled {
            "ON (sub-symbol semantic chunks)"
        } else {
            "OFF (node-level index authoritative)"
        }
    );
    println!("Fragment weight:    {}", search.fragment_weight);
    println!("Fragment max bytes: {}", search.fragment_max_bytes);
    println!(
        "Fragment orphan:    {}",
        if search.fragment_orphan_enabled {
            "ON"
        } else {
            "OFF"
        }
    );
    println!(
        "Fragment naive fallback: {}",
        if search.fragment_naive_fallback {
            "ON"
        } else {
            "OFF"
        }
    );
}

/// Report the remote (system-one / gemma) setup status and return whether it is fully configured.
///
/// `ready` is the remote-route verdict from `setup_status::remote_status`.
fn print_check_remote_status(config: &crate::config::LeIndexConfig, ready: bool) -> bool {
    print_check_search_settings(&config.search);
    let system_ready = setup_system_one::print_status(config);
    let system_enabled = !matches!(config.system_one.effective().mode.as_str(), "" | "off");
    // A static "active" verdict is not proof the provider answers: when
    // the gate is fully configured, run the same never-spawning probe as
    // the wizard so `--check` reports reachability, not just configuration.
    let probe_ok = if system_ready {
        match setup_system_one::check_probe(config) {
            Ok(()) => {
                println!("  probe: provider answered");
                true
            }
            Err(e) => {
                println!("  probe: FAILED ({e})");
                false
            }
        }
    } else {
        true
    };
    if let Some(path) = crate::config::config_file_path() {
        println!("Config file: {}", path.display());
    }
    ready && (!system_enabled || (system_ready && probe_ok))
}

/// Print a status report without modifying anything.
///
/// VAL-SETUP-014: --check mode reads config and reports status
/// VAL-SETUP-020: Reports the ORT version (from config + live detection)
/// VAL-SETUP-034: Surfaces full configuration
pub fn run_check() -> Result<CheckResult, SetupError> {
    let config =
        crate::config::LeIndexConfig::load().map_err(|e| SetupError::ConfigRead(e.to_string()))?;

    if let Some(ready) = setup_status::remote_status(&config)? {
        return Ok(CheckResult {
            fully_configured: print_check_remote_status(&config, ready),
        });
    }

    let live_version = get_ort_version();
    let ort_installed = live_version.is_some();
    let model_present = check_model_present_for_name(&config.neural.model_name);
    // VAL-SETUP-014/018: checksum status is surfaced so a corrupted file
    // is visible from `--check` without needing to re-run setup.
    let checksum_status = model_checksum_status_for_name(&config.neural.model_name);
    let ort_path = discover_ort_path().or_else(|| {
        config
            .neural
            .ort_dylib_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.exists())
    });
    // Prefer the live-detected version; fall back to the recorded one.
    let ort_version = live_version
        .clone()
        .or_else(|| config.neural.ort_version.clone());

    // VAL-SETUP-018: a checksum mismatch on the model file means the install
    // is not actually ready even though the file is present; --check must
    // surface the corruption instead of reporting "fully configured".
    let fully_configured = config.neural.enabled
        && ort_installed
        && model_present
        && !matches!(checksum_status, ModelChecksumStatus::Mismatch { .. });

    // Print the report
    println!("\nLeIndex Setup Status\n{}", "=".repeat(20));
    println!();

    // Neural status
    let neural_status = if config.neural.enabled { "ON" } else { "OFF" };
    println!("Neural embeddings: {}", neural_status);

    // Provider
    println!("Execution provider: {}", config.neural.execution_provider);

    print_check_ort_status(
        ort_installed,
        ort_version.as_deref(),
        ort_path.as_deref(),
        config.neural.ort_dylib_path.as_deref(),
    );

    // Model status
    let model_status = if model_present { "present" } else { "absent" };
    println!("Model files:        {}", model_status);
    println!("Model name:         {}", config.neural.model_name);
    println!("Model directory:    {}", config.neural.model_dir);

    // VAL-SETUP-017/018: report the checksum verdict so users can tell whether
    // `~/.leindex/models/qwen3-embed-0.6b.onnx` is intact or needs re-download.
    print_check_model_checksum(&checksum_status);

    // Search settings
    print_check_search_settings(&config.search);

    // Overall status
    println!();
    print_check_overall_status(
        fully_configured,
        config.neural.enabled,
        ort_installed,
        model_present,
        &config.neural.model_name,
    );

    // Config file path
    if let Some(path) = crate::config::config_file_path() {
        println!();
        println!("Config file: {}", path.display());
    }

    Ok(CheckResult { fully_configured })
}
