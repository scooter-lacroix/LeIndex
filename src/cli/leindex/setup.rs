// Interactive and non-interactive setup wizard for LeIndex neural search
//
// Implements the `leindex setup` command flow:
//   - Interactive: prompts for neural? -> CPU/GPU -> AMD/NVIDIA
//   - Non-interactive: --neural/--no-neural, --cpu, --gpu <amd|nvidia>
//   - --check: read-only status report
//
// Writes config to ~/.leindex/config/leindex.toml (or $LEINDEX_HOME/config/).
// The main binary reads this config and passes settings to the worker via env vars.
//
// VAL-SETUP-001: Setup command registered and discoverable
// VAL-SETUP-002: Interactive flow asks neural question with Y default
// VAL-SETUP-003-008: Interactive flow branches
// VAL-SETUP-009-013: Non-interactive flags
// VAL-SETUP-014: --check mode
// VAL-SETUP-015: Conflict detection
// VAL-SETUP-023: Config persistence with correct schema
// VAL-SETUP-024: Idempotent re-runs
// VAL-SETUP-034: Surfaces full configuration status

use std::path::PathBuf;
use std::process::Command;

/// Execution provider selected during setup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProvider {
    /// CPU inference (works everywhere).
    Cpu,
    /// NVIDIA CUDA GPU.
    Cuda,
    /// AMD MIGraphX GPU (ROCm).
    Migraphx,
}

impl ExecutionProvider {
    /// The ORT pip package name for this provider.
    pub fn pip_package(&self) -> &'static str {
        match self {
            ExecutionProvider::Cpu => "onnxruntime",
            ExecutionProvider::Cuda => "onnxruntime-gpu",
            ExecutionProvider::Migraphx => "onnxruntime-migraphx",
        }
    }

    /// The config string value for this provider.
    pub fn config_value(&self) -> &'static str {
        match self {
            ExecutionProvider::Cpu => "cpu",
            ExecutionProvider::Cuda => "cuda",
            ExecutionProvider::Migraphx => "migraphx",
        }
    }
}

/// GPU vendor choice (from --gpu flag or interactive prompt).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuVendor {
    /// AMD GPU (ROCm/MIGraphX).
    Amd,
    /// NVIDIA GPU (CUDA).
    Nvidia,
}

/// User's setup choices resolved from flags or interactive prompts.
#[derive(Debug, Clone)]
pub struct SetupChoices {
    /// Whether neural embeddings should be enabled.
    pub neural_enabled: bool,
    /// The execution provider to use (None when neural is disabled).
    pub provider: Option<ExecutionProvider>,
}

/// The result of running setup.
#[derive(Debug, Clone)]
pub struct SetupResult {
    /// The choices that were applied.
    pub choices: SetupChoices,
    /// Path where config was written (None for --check mode).
    pub config_path: Option<PathBuf>,
    /// ORT dylib path discovered after installation (if any).
    pub ort_dylib_path: Option<PathBuf>,
    /// ORT (onnxruntime) version detected after install (VAL-SETUP-020).
    pub ort_version: Option<String>,
    /// Whether the model files are present.
    pub model_present: bool,
    /// Whether ORT (onnxruntime pip package) is installed.
    pub ort_installed: bool,
}

/// Resolve setup choices from CLI flags (non-interactive mode).
///
/// Returns an error if the flags are conflicting.
/// VAL-SETUP-009-013: Non-interactive flag handling
/// VAL-SETUP-015: Conflict detection
pub fn resolve_from_flags(
    neural: bool,
    no_neural: bool,
    cpu: bool,
    gpu: Option<GpuVendor>,
) -> Result<SetupChoices, SetupError> {
    // VAL-SETUP-015: --neural + --no-neural is a conflict
    if neural && no_neural {
        return Err(SetupError::Conflict {
            message: "Cannot use --neural and --no-neural together. Choose one.".to_string(),
        });
    }

    // VAL-SETUP-015: --cpu + --gpu is a conflict
    if cpu && gpu.is_some() {
        return Err(SetupError::Conflict {
            message: "Cannot use --cpu and --gpu together. Choose one execution provider."
                .to_string(),
        });
    }

    // --cpu or --gpu without --neural: imply --neural
    let effective_neural = neural || cpu || gpu.is_some();

    // --no-neural: disable neural, ignore provider flags
    if no_neural {
        return Ok(SetupChoices {
            neural_enabled: false,
            provider: None,
        });
    }

    if !effective_neural {
        // No neural-related flags at all
        return Err(SetupError::NoFlags);
    }

    // Determine the provider
    let provider = if cpu {
        Some(ExecutionProvider::Cpu)
    } else if let Some(vendor) = gpu {
        Some(match vendor {
            GpuVendor::Amd => ExecutionProvider::Migraphx,
            GpuVendor::Nvidia => ExecutionProvider::Cuda,
        })
    } else {
        // --neural without provider: default to CPU
        // VAL-SETUP-009: --neural with no GPU flags defaults to CPU
        Some(ExecutionProvider::Cpu)
    };

    Ok(SetupChoices {
        neural_enabled: true,
        provider,
    })
}

/// Run the interactive setup flow.
///
/// VAL-SETUP-002: Prompts neural? with Y default
/// VAL-SETUP-003-008: Branching logic
pub fn run_interactive_flow() -> Result<SetupChoices, SetupError> {
    use dialoguer::{Confirm, Select};

    // VAL-SETUP-002: "Do you want neural embeddings / enhanced semantic search?"
    println!("\nLeIndex Setup\n=============\n");
    println!("Neural embeddings provide semantic code search (find symbols by meaning).\n");

    let want_neural = Confirm::new()
        .with_prompt("Do you want neural embeddings / enhanced semantic search?")
        .default(true)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;

    if !want_neural {
        // VAL-SETUP-003: neural=No writes TF-IDF-only config
        return Ok(SetupChoices {
            neural_enabled: false,
            provider: None,
        });
    }

    // VAL-SETUP-004/005: CPU or GPU?
    let provider_items = vec![
        "CPU (works everywhere)",
        "GPU (faster, requires AMD/NVIDIA GPU)",
    ];

    let gpu_choice = Select::new()
        .with_prompt("CPU or GPU-based neural embeddings?")
        .items(&provider_items)
        .default(0)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;

    if gpu_choice == 0 {
        // VAL-SETUP-004: CPU selected
        return Ok(SetupChoices {
            neural_enabled: true,
            provider: Some(ExecutionProvider::Cpu),
        });
    }

    // VAL-SETUP-005: GPU -> AMD/NVIDIA/N/A
    let vendor_items = vec![
        "AMD (ROCm/MIGraphX)",
        "NVIDIA (CUDA)",
        "N/A (no usable GPU detected)",
    ];

    let vendor_choice = Select::new()
        .with_prompt("Which GPU vendor?")
        .items(&vendor_items)
        .default(0)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;

    // VAL-SETUP-006/007/008: vendor routing
    let provider = match vendor_choice {
        0 => ExecutionProvider::Migraphx, // VAL-SETUP-006: AMD -> MIGraphX
        1 => ExecutionProvider::Cuda,     // VAL-SETUP-007: NVIDIA -> CUDA
        _ => ExecutionProvider::Cpu,      // VAL-SETUP-008: N/A -> CPU fallback
    };

    Ok(SetupChoices {
        neural_enabled: true,
        provider: Some(provider),
    })
}

/// Check if the stdin is a terminal (interactive mode available).
pub fn is_interactive() -> bool {
    use std::io::IsTerminal;
    std::io::stdin().is_terminal()
}

/// Execute setup with the resolved choices.
///
/// Writes config, installs ORT (if neural), checks models.
/// VAL-SETUP-006/007/008: ORT pip install routing
/// VAL-SETUP-020: pip install onnxruntime succeeds, version recorded
/// VAL-SETUP-021: pip not found surfaces actionable error
/// VAL-SETUP-022: wrong ORT version triggers upgrade or clear warning
/// VAL-SETUP-023: Config written with correct schema
/// VAL-SETUP-024: Idempotent
pub fn execute_setup(choices: &SetupChoices) -> Result<SetupResult, SetupError> {
    // Check current state
    let ort_installed = check_ort_installed();
    let model_present = check_model_present();

    let (ort_dylib_path, ort_version) = if choices.neural_enabled {
        let provider = choices.provider.unwrap_or(ExecutionProvider::Cpu);

        // VAL-SETUP-022: Check version compatibility of any existing install
        // before deciding whether to (re)install. An incompatible version
        // triggers either an upgrade (when too old) or a clear warning (when
        // too new). This must run before install so we don't silently proceed
        // with a known-bad version.
        let pre_existing_version = get_ort_version();
        if let Some(ref detected) = pre_existing_version {
            match check_ort_version_compatibility(detected) {
                VersionCompatibility::Unsupported {
                    required_min,
                    reason,
                } => {
                    println!(
                        "  -> WARNING: Detected onnxruntime {}, but LeIndex requires {} ({}).",
                        detected, required_min, reason
                    );
                    println!("     Upgrading to a supported version...");
                    // Fall through to install/upgrade below.
                }
                VersionCompatibility::TooNew {
                    supported_max,
                    reason,
                } => {
                    println!(
                        "  -> WARNING: Detected onnxruntime {}, which is newer than the supported maximum ({}).",
                        detected, supported_max
                    );
                    println!("     Reason: {}.", reason);
                    println!("     Setup will continue, but if you hit ABI errors, pin onnxruntime to <= {}.", supported_max);
                    // We continue: too-new may still work, just warn.
                }
                VersionCompatibility::Supported => {
                    println!("  -> onnxruntime {} detected (compatible).", detected);
                }
            }
        }

        // VAL-SETUP-006/007/008/010/011/012: Install/maintain ORT for the provider
        if !ort_installed {
            // No ORT at all - install the appropriate package
            install_ort(provider)?;
        } else if provider != ExecutionProvider::Cpu {
            // ORT is installed but we want GPU. Check if the GPU variant
            // is needed by testing if the specific provider is available.
            if !check_provider_available(provider) {
                println!(
                    "  -> onnxruntime installed but {} not available; installing {} variant...",
                    provider.config_value(),
                    provider.pip_package()
                );
                install_ort(provider)?;
            } else {
                println!(
                    "  -> onnxruntime with {} already available.",
                    provider.config_value()
                );
            }
        } else if pre_existing_version.is_none() {
            // CPU was selected but the installed ORT couldn't report a version
            // (or was too old to import). Trigger an upgrade.
            println!("  -> onnxruntime installed but unimportable; upgrading...");
            install_ort(provider)?;
        }

        // Discover ORT dylib path and version after (potential) install
        let ort_path = discover_ort_path();
        let ort_ver = get_ort_version().or(pre_existing_version);
        (ort_path, ort_ver)
    } else {
        (None, None)
    };

    // Validate models whenever neural is enabled. We deliberately do NOT
    // short-circuit on `check_model_present()` returning true because:
    //   * VAL-SETUP-017 requires the second run to print "already present,
    //     checksum verified" so the user knows the file is integrity-checked
    //     and not just present on disk;
    //   * VAL-SETUP-018 requires us to detect a corrupted file on re-run and
    //     trigger a re-download before declaring success.
    //
    // Inside `ensure_models_present` we still print a per-file "already
    // present, checksum verified" line so the second run is informative
    // without doing any network round-trips.
    let model_present = if choices.neural_enabled {
        ensure_models_present()?
    } else {
        model_present
    };

    // Write the config
    let config = build_config(choices, ort_dylib_path.as_deref(), ort_version.as_deref());
    let (config, recovery) = merge_with_existing(config);
    let config_path = config
        .save()
        .map_err(|e| SetupError::ConfigWrite(e.to_string()))?;

    if let Some(action) = recovery {
        match action {
            RecoveryNotice::Migrated => {
                println!("  -> Existing config migrated to current schema.");
            }
            RecoveryNotice::RecoveredFromCorrupt(backup) => {
                println!(
                    "  -> Corrupted config detected and backed up to {}.",
                    backup.display()
                );
            }
        }
    }

    Ok(SetupResult {
        choices: choices.clone(),
        config_path: Some(config_path),
        ort_dylib_path,
        ort_version,
        model_present,
        ort_installed: choices.neural_enabled || ort_installed,
    })
}

/// Merge the new config with any existing config, preserving user settings where
/// reasonable and migrating stale schemas.
///
/// VAL-SETUP-024: Idempotent - re-running produces equivalent config
/// VAL-SETUP-029: Corrupted config recovered gracefully
/// VAL-SETUP-030: Stale config migrated
fn merge_with_existing(
    mut new_config: crate::cli::neural_config::LeIndexConfig,
) -> (
    crate::cli::neural_config::LeIndexConfig,
    Option<RecoveryNotice>,
) {
    // Try to load existing config with recovery
    match crate::cli::neural_config::LeIndexConfig::load_or_recover() {
        Ok((existing, action)) => {
            let notice = match action {
                crate::cli::neural_config::RecoveryAction::RecoveredFromCorrupt(backup) => {
                    Some(RecoveryNotice::RecoveredFromCorrupt(backup))
                }
                crate::cli::neural_config::RecoveryAction::Loaded => {
                    // VAL-SETUP-030: Preserve search/indexing settings from existing config
                    // unless the new config explicitly overrides them. Setup always
                    // writes neural settings; search/indexing are borrowed from existing.
                    if existing.search.search_mode != new_config.search.search_mode {
                        // Preserve existing search settings
                        new_config.search = existing.search;
                    }
                    if existing.indexing.batch_size != new_config.indexing.batch_size {
                        new_config.indexing = existing.indexing;
                    }

                    // Check for stale ort-lib path
                    if let Some(ref ort_path) = existing.neural.ort_dylib_path {
                        if ort_path.contains("ort-lib") {
                            Some(RecoveryNotice::Migrated)
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                crate::cli::neural_config::RecoveryAction::CreatedDefault => None,
            };
            (new_config, notice)
        }
        Err(_) => (new_config, None),
    }
}

/// Notice about config recovery/migration during merge.
#[derive(Debug, Clone)]
enum RecoveryNotice {
    /// Config from older version was migrated.
    Migrated,
    /// Corrupted config was backed up.
    RecoveredFromCorrupt(PathBuf),
}

/// Build the LeIndexConfig from setup choices.
fn build_config(
    choices: &SetupChoices,
    ort_dylib_path: Option<&std::path::Path>,
    ort_version: Option<&str>,
) -> crate::cli::neural_config::LeIndexConfig {
    use crate::cli::neural_config::{IndexingConfig, NeuralConfig, SearchConfig};

    let provider_str = choices.provider.map(|p| p.config_value()).unwrap_or("auto");

    crate::cli::neural_config::LeIndexConfig {
        neural: NeuralConfig {
            enabled: choices.neural_enabled,
            execution_provider: provider_str.to_string(),
            ort_dylib_path: ort_dylib_path.map(|p| p.display().to_string()),
            ort_version: ort_version.map(|s| s.to_string()),
            model_dir: crate::cli::neural_config::model_dir_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.leindex/models".to_string()),
        },
        search: SearchConfig::default(),
        indexing: IndexingConfig::default(),
    }
}

/// Check if onnxruntime is installed (any variant).
fn check_ort_installed() -> bool {
    get_ort_version().is_some()
}

/// Get the installed onnxruntime version by importing it via Python.
///
/// Returns `Some(version_string)` (e.g., "1.25.0") when onnxruntime can be
/// imported. VAL-SETUP-020: the returned string is recorded in the config so
/// subsequent setup runs and `--check` can report it without re-querying.
pub fn get_ort_version() -> Option<String> {
    let candidates = ["python3", "python"];
    for cmd in &candidates {
        let result = Command::new(cmd)
            .arg("-c")
            .arg("import onnxruntime; print(onnxruntime.__version__)")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();

        if let Ok(out) = result {
            if out.status.success() {
                let v = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if !v.is_empty() {
                    return Some(v);
                }
            }
        }
    }
    None
}

/// Minimum supported ONNX Runtime version (MAJOR.MINOR.PATCH).
///
/// LeIndex uses the `ort` crate which targets the ORT 1.x C API. Older ORT
/// versions (< 1.20.0) are missing APIs the worker depends on
/// (`OrtSessionOptionsAppendExecutionProvider_*`, lazy shape inference, etc.).
pub const MIN_ORT_VERSION: (u32, u32, u32) = (1, 20, 0);

/// Maximum supported ONNX Runtime major version. ORT 2.x will introduce ABI
/// breaking changes and is not yet released as of the ort crate 2.0.0-rc.12
/// pinning. We treat 2.0.0+ as "too new" and warn rather than silently accept.
pub const MAX_ORT_MAJOR: u32 = 1;

/// Outcome of comparing a detected version against the supported range.
///
/// VAL-SETUP-022: Setup either upgrades an unsupported version or emits a
/// clear warning naming the detected and required versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VersionCompatibility {
    /// Version is within the supported range.
    Supported,
    /// Version is too old. `required_min` names the minimum supported version
    /// and `reason` explains why upgrade is necessary.
    Unsupported {
        /// Minimum supported version string (e.g., "1.20.0").
        required_min: String,
        /// Human-readable reason the version is unsupported.
        reason: String,
    },
    /// Version is too new (major bump). May still work, but the user is warned.
    TooNew {
        /// Maximum supported version string (e.g., "1.x").
        supported_max: String,
        /// Human-readable reason the version is concerning.
        reason: String,
    },
}

/// Parse a semver-like version string ("1.25.0") into a (major, minor, patch)
/// tuple. Trailing pre-release/build metadata is ignored. Returns `None` when
/// the string cannot be parsed.
pub fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
    let core = s.split('-').next().unwrap_or(s);
    let core = core.split('+').next().unwrap_or(core);
    let mut parts = core.split('.');
    let major = parts.next()?.parse::<u32>().ok()?;
    let minor = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    let patch = parts.next().unwrap_or("0").parse::<u32>().ok()?;
    Some((major, minor, patch))
}

/// Compare a detected ORT version against the supported range.
///
/// VAL-SETUP-022: caller must surface the returned reason in the user-facing
/// log when the version is not `Supported`, and upgrade the install when
/// `Unsupported` is returned.
pub fn check_ort_version_compatibility(detected: &str) -> VersionCompatibility {
    let Some(version) = parse_version(detected) else {
        // Unparseable version string can't be trusted; treat as unsupported.
        return VersionCompatibility::Unsupported {
            required_min: format!(
                "{}.{}.{}",
                MIN_ORT_VERSION.0, MIN_ORT_VERSION.1, MIN_ORT_VERSION.2
            ),
            reason: format!(
                "detected version '{}' is not a recognized onnxruntime release",
                detected
            ),
        };
    };

    if version.0 > MAX_ORT_MAJOR {
        return VersionCompatibility::TooNew {
            supported_max: format!("{}.x", MAX_ORT_MAJOR),
            reason: format!(
                "ORT {}.{} introduces breaking ABI changes; expected <= {}.x",
                version.0, version.1, MAX_ORT_MAJOR
            ),
        };
    }

    // Within the supported major. Compare against MIN_ORT_VERSION.
    if version < MIN_ORT_VERSION {
        return VersionCompatibility::Unsupported {
            required_min: format!(
                "{}.{}.{}",
                MIN_ORT_VERSION.0, MIN_ORT_VERSION.1, MIN_ORT_VERSION.2
            ),
            reason: "this ORT build lacks APIs the worker depends on".to_string(),
        };
    }

    VersionCompatibility::Supported
}

/// Check if a specific execution provider is available in the installed ORT.
fn check_provider_available(provider: ExecutionProvider) -> bool {
    let provider_name = match provider {
        ExecutionProvider::Migraphx => "MIGraphXExecutionProvider",
        ExecutionProvider::Cuda => "CUDAExecutionProvider",
        ExecutionProvider::Cpu => return true, // CPU is always available
    };

    let check_script = format!(
        "import onnxruntime as ort; providers = ort.get_available_providers(); print('{}' in providers)",
        provider_name
    );

    for cmd in &["python3", "python"] {
        let result = Command::new(cmd)
            .arg("-c")
            .arg(&check_script)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();

        if let Ok(out) = result {
            if out.status.success() {
                let output = String::from_utf8_lossy(&out.stdout).trim().to_string();
                if output == "True" {
                    return true;
                }
            }
        }
    }

    false
}

/// Check if model files are present in the model directory.
///
/// VAL-SETUP-014: used by `--check` mode to flag model presence. We count a
/// file as "present" if it exists on disk; the checksum-aware variant is
/// `model_checksum_status()` which returns one of Ok/Mismatch/Unknown so the
/// caller can warn about a corrupted file without failing the existence check.
fn check_model_present() -> bool {
    let model_name = "qwen3-embed-0.6b.onnx";

    // Check via config module's model_dir_path
    if let Some(model_dir) = crate::cli::neural_config::model_dir_path() {
        if model_dir.join(model_name).exists() {
            return true;
        }
    }

    // Also check bundled models relative to the binary
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            if parent.join("models").join(model_name).exists() {
                return true;
            }
            if let Some(gp) = parent.parent() {
                if gp.join("models").join(model_name).exists() {
                    return true;
                }
            }
        }
    }

    false
}

/// Outcome of comparing the on-disk model file against the checksum manifest.
///
/// VAL-SETUP-014: `--check` mode reports `Mismatch` so the user knows the
/// model file is corrupted before re-running setup. VAL-SETUP-017/018 share
/// the same primitive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChecksumStatus {
    /// File is missing entirely.
    Missing,
    /// File exists and the manifest's checksum matches.
    Ok,
    /// File exists but no checksum entry is available.
    Unknown,
    /// File exists but its computed SHA256 differs from the manifest.
    Mismatch { expected: String, actual: String },
}

/// Check the model file's checksum status against the bundled manifest.
///
/// Walks the same model_dir / bundled / binary-side lookup as
/// `check_model_present()`. When a `checksums.sha256` sibling file exists,
/// its entry for `qwen3-embed-0.6b.onnx` is compared against the file's
/// computed SHA256.
pub fn model_checksum_status() -> ModelChecksumStatus {
    use crate::cli::leindex::model_download::{
        check_file_against_manifest, parse_checksums, CheckResult, MODEL_ONNX_FILENAME,
    };

    let model_dir = match crate::cli::neural_config::model_dir_path() {
        Some(d) => d,
        None => return ModelChecksumStatus::Missing,
    };

    let onnx_path = model_dir.join(MODEL_ONNX_FILENAME);
    if !onnx_path.exists() {
        return ModelChecksumStatus::Missing;
    }

    let manifest_path = model_dir.join("checksums.sha256");
    let manifest_str = match std::fs::read_to_string(&manifest_path) {
        Ok(s) => s,
        Err(_) => return ModelChecksumStatus::Unknown,
    };
    let manifest = parse_checksums(&manifest_str);

    match check_file_against_manifest(&onnx_path, &manifest) {
        Ok(CheckResult::Verified) => ModelChecksumStatus::Ok,
        Ok(CheckResult::NoEntry) => ModelChecksumStatus::Unknown,
        Ok(CheckResult::Mismatch { expected, actual }) => {
            ModelChecksumStatus::Mismatch { expected, actual }
        }
        Ok(CheckResult::Missing) => ModelChecksumStatus::Missing,
        Err(_) => ModelChecksumStatus::Unknown,
    }
}

/// Install ORT via pip for the given execution provider.
///
/// VAL-SETUP-006: AMD -> pip install onnxruntime-migraphx
/// VAL-SETUP-007: NVIDIA -> pip install onnxruntime-gpu
/// VAL-SETUP-008/010: CPU -> pip install onnxruntime
///
/// VAL-SETUP-020: Reports success with the installed version.
/// VAL-SETUP-021: `find_pip()` handles pip-not-found with PIP_BIN hint.
/// VAL-SETUP-022: Caller checks version compatibility after install.
///
/// The pip process output is captured (not inherited) so we can detect
/// network failures and surface them in the error message rather than a
/// generic exit-code-only failure.
fn install_ort(provider: ExecutionProvider) -> Result<(), SetupError> {
    let package = provider.pip_package();

    println!("Installing {} via pip...", package);

    // VAL-SETUP-021: find_pip knows about PIP_BIN, python -m pip, pip3, pip.
    let pip_cmd = find_pip().ok_or(SetupError::PipNotFound)?;

    // We use `--upgrade` so that a pre-existing too-old install (e.g., 1.10.0)
    // is replaced with a supported release (VAL-SETUP-022 upgrade path).
    //
    // Captured output (instead of inherited) lets us distinguish network
    // failures from genuine pip errors and include the relevant excerpt in the
    // error message.
    let result = Command::new(&pip_cmd.0)
        .args(&pip_cmd.1)
        .arg("install")
        .arg(package)
        .arg("--upgrade")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match result {
        Ok(out) if out.status.success() => {
            // VAL-SETUP-020: surface success, including the installed version
            // when pip prints it ("Successfully installed onnxruntime-1.25.0").
            let stdout = String::from_utf8_lossy(&out.stdout);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let combined = if stdout.is_empty() {
                stderr.to_string()
            } else {
                stdout.to_string()
            };

            // Best-effort version parse from pip's "Successfully installed ..." line.
            if let Some(version_line) = combined
                .lines()
                .find(|l| l.contains("Successfully installed") && l.contains(package))
            {
                println!("  -> {}", version_line.trim());
            } else {
                println!("  -> Successfully installed {}.", package);
            }
            Ok(())
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let stdout = String::from_utf8_lossy(&out.stdout);
            let combined = format!("{}\n{}", stdout, stderr);

            // Detect common network failures so we can give actionable guidance.
            if is_network_error(&combined) {
                return Err(SetupError::PipNetworkFailed {
                    package: package.to_string(),
                    exit_code: out.status.code().unwrap_or(-1),
                    output: truncate_for_error(&stderr),
                });
            }

            Err(SetupError::PipInstallFailed {
                package: package.to_string(),
                exit_code: out.status.code().unwrap_or(-1),
            })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Should not happen (find_pip verified the binary), but handle it.
            Err(SetupError::PipNotFound)
        }
        Err(e) => Err(SetupError::Io(format!("Failed to run pip: {}", e))),
    }
}

/// Heuristic for detecting pip network/download errors in captured output.
///
/// VAL-SETUP-019's pip-analogue (VAL-SETUP-020 fail path): we want the error
/// message to mention connectivity and a remediation hint rather than just an
/// exit code.
fn is_network_error(output: &str) -> bool {
    let lower = output.to_lowercase();
    const NETWORK_HINTS: &[&str] = &[
        "could not fetch url",
        "connection error",
        "connectionerror",
        "connectionrefusederror",
        "connection reset",
        "connection timed out",
        "connection broken",
        "ssl: certificate_verify_failed",
        "ssl certificate_verify_failed",
        "temporary failure in name resolution",
        "failed to establish a new connection",
        "max retries exceeded",
        "network is unreachable",
        "read timed out",
        "remotedisconnectederror",
        "newconnectionerror",
        "getaddrinfo failed",
        "name or service not known",
        "no such device or address",
    ];
    NETWORK_HINTS.iter().any(|hint| lower.contains(hint))
}

fn truncate_for_error(s: &str) -> String {
    const MAX_LINES: usize = 12;
    let lines: Vec<&str> = s.lines().collect();
    if lines.len() <= MAX_LINES {
        s.trim().to_string()
    } else {
        format!(
            "{}\n... ({} more lines truncated)",
            lines[..MAX_LINES].join("\n").trim(),
            lines.len() - MAX_LINES
        )
    }
}

/// Find the pip executable.
///
/// VAL-SETUP-021: PIP_BIN env var is checked first (it can either point at the
/// `pip` binary, or at a python interpreter prefixed with `-m pip`). After
/// that, we look for `pip3`, `pip`, and `python[3] -m pip` on PATH.
///
/// Returns `(program, prefix_args)` where `prefix_args` is the argument list
/// that must precede `install <package>` (e.g., `["-m", "pip"]` for
/// `python3 -m pip`).
fn find_pip() -> Option<(String, Vec<String>)> {
    // VAL-SETUP-021: Honor PIP_BIN first so users can point at a non-default pip.
    if let Ok(value) = std::env::var("PIP_BIN") {
        if !value.trim().is_empty() {
            // Split into program + leading args so the caller can append
            // "install <package>" to it. If PIP_BIN is a single token, we
            // treat it as the pip binary directly. If it contains spaces
            // (e.g., "/usr/bin/python3 -m pip"), we split program from args.
            let mut parts = value.split_whitespace();
            if let Some(program) = parts.next() {
                let prefix: Vec<String> = parts.map(|s| s.to_string()).collect();
                if Command::new(program)
                    .args(&prefix)
                    .arg("--version")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
                {
                    return Some((program.to_string(), prefix));
                }
                // PIP_BIN was set but the binary it points at is broken/missing.
                // Fall through to discovery but log a hint to stderr.
                eprintln!(
                    "warning: PIP_BIN is set to '{}' but invoking it failed; falling back to PATH discovery.",
                    value
                );
            }
        }
    }

    // Try pip3, pip directly
    for cmd in &["pip3", "pip"] {
        if Command::new(cmd)
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some((cmd.to_string(), Vec::new()));
        }
    }

    // Try python -m pip
    for py in &["python3", "python"] {
        if Command::new(py)
            .arg("-m")
            .arg("pip")
            .arg("--version")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
        {
            return Some((py.to_string(), vec!["-m".to_string(), "pip".to_string()]));
        }
    }

    None
}

/// Discover the ORT dylib path from pip installation.
fn discover_ort_path() -> Option<PathBuf> {
    // Try to find onnxruntime's capi directory via Python
    for py in &["python3", "python"] {
        let result = Command::new(py)
            .arg("-c")
            .arg("import os, onnxruntime.capi as c; print(os.path.dirname(c.__file__))")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null())
            .output();

        if let Ok(out) = result {
            if out.status.success() {
                let capi_dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
                let dir = PathBuf::from(&capi_dir);
                if dir.is_dir() {
                    // Look for the ORT shared library
                    for lib_name in ort_lib_names() {
                        let candidate = dir.join(lib_name);
                        if candidate.exists() {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }

    // Also check system path
    for path in &["/usr/local/lib", "/usr/lib"] {
        for lib_name in ort_lib_names() {
            let candidate = PathBuf::from(path).join(lib_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    None
}

/// Platform-specific ORT library file names.
fn ort_lib_names() -> &'static [&'static str] {
    #[cfg(target_os = "linux")]
    {
        &["libonnxruntime.so"]
    }
    #[cfg(target_os = "macos")]
    {
        &["libonnxruntime.dylib"]
    }
    #[cfg(target_os = "windows")]
    {
        &["onnxruntime.dll"]
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {
        &["libonnxruntime.so"]
    }
}

/// Ensure model files are present, downloading from HuggingFace if needed.
///
/// VAL-SETUP-016: First run downloads model files to `~/.leindex/models/`,
///   printing progress for each file fetched.
/// VAL-SETUP-017: Second run skips when the on-disk file's SHA256 matches the
///   entry in `checksums.sha256`.
/// VAL-SETUP-018: A corrupted file (checksum mismatch) is deleted and
///   re-downloaded, then verified again.
/// VAL-SETUP-019: Network failures surface an actionable error mentioning
///   connectivity and `LEINDEX_MODEL_PATH`; no partial file is left at the
///   canonical target path.
///
/// The flow prefers, in order:
///   1. Bundled model files (next to the running binary) — copy without
///      hitting the network. This makes the GitHub Release bundle surface
///      zero-network once `install.sh` places the binary + models together.
///   2. Verified existing files in the model directory.
///   3. HuggingFace CDN download (with retries).
///
/// `checksums.sha256` and `LICENSE` are treated as OPTIONAL because the
/// onnx-community/Qwen3-Embedding-0.6B-ONNX repo does not host either file.
/// When the manifest is unavailable, one is generated locally from the
/// downloaded files so second-run verification still works.
fn ensure_models_present() -> Result<bool, SetupError> {
    use crate::cli::leindex::model_download::{
        self, check_file_against_manifest, download_file_with_retry, iter_model_files,
        parse_checksums, CheckResult, DownloadOutcome, DEFAULT_DOWNLOAD_RETRIES,
    };

    let model_dir = crate::cli::neural_config::model_dir_path()
        .ok_or_else(|| SetupError::Io("Cannot resolve model directory".to_string()))?;

    // Create model directory up front so subsequent file operations can rely
    // on it existing.
    std::fs::create_dir_all(&model_dir)
        .map_err(|e| SetupError::Io(format!("Cannot create model dir: {}", e)))?;

    let manifest_path = model_dir.join("checksums.sha256");
    let model_onnx_name = model_download::MODEL_ONNX_FILENAME;

    // ── Step 1: copy from bundled location if present ────────────────────
    // Bundled files (GitHub Release bundle layout) are a no-network fast path.
    // We copy each missing file from the bundle, then fall through to the
    // network path only for anything still missing or corrupted.
    if let Some(bundled_dir) = model_download::find_bundled_models() {
        copy_bundled_models(&bundled_dir, &model_dir);
    }

    // ── Step 2: download / verify every file ────────────────────────────
    // The model triplet is split into "required" (onnx, tokenizer, config)
    // and "optional" (checksums.sha256, LICENSE) files. The onnx-community
    // HuggingFace repo does NOT ship `checksums.sha256` or `LICENSE`, so those
    // downloads tolerate 404 / network failure. When the manifest is missing
    // we generate it locally after the required downloads succeed, so
    // second-run verification (VAL-SETUP-017) still works.
    //
    // Per-file strategy:
    //   - Verified (checksum matches)   -> skip (VAL-SETUP-017)
    //   - NoEntry (no checksum to cmp)  -> keep if present, else download
    //   - Mismatch (checksum differs)   -> delete + re-download (VAL-SETUP-018)
    //   - Missing                       -> download (VAL-SETUP-016)
    let manifest_str = std::fs::read_to_string(&manifest_path).unwrap_or_default();
    let manifest = parse_checksums(&manifest_str);

    let mut downloaded_any = false;
    let mut model_present_after = false;

    for file in iter_model_files() {
        let dest = model_dir.join(file.local);
        // checksums.sha256 and LICENSE are not hosted by onnx-community; their
        // absence is non-fatal.
        let required = file.local != "checksums.sha256" && file.local != "LICENSE";

        match check_file_against_manifest(&dest, &manifest)
            .map_err(|e| SetupError::Io(format!("Cannot stat {}: {}", dest.display(), e)))?
        {
            CheckResult::Verified => {
                // VAL-SETUP-017: skip download, file already good.
                println!("  -> {} already present, checksum verified.", file.local);
                if file.local == model_onnx_name {
                    model_present_after = true;
                }
                continue;
            }
            CheckResult::NoEntry => {
                if dest.exists() {
                    println!(
                        "  -> {} present (no checksum entry; cannot verify).",
                        file.local
                    );
                    if file.local == model_onnx_name {
                        model_present_after = true;
                    }
                    continue;
                }
                // Fall through to the download branch.
            }
            CheckResult::Mismatch { expected, actual } => {
                // VAL-SETUP-018: checksum failure triggers re-download.
                println!(
                    "  -> WARNING: {} checksum mismatch (expected {}..., got {}...).",
                    file.local,
                    short_hash(&expected),
                    short_hash(&actual)
                );
                println!("     Removing corrupt file and re-downloading...");
                let _ = std::fs::remove_file(&dest);
            }
            CheckResult::Missing => {
                // VAL-SETUP-016: first-run download.
            }
        }

        // Download branch.
        let outcome: DownloadOutcome = match download_file_with_retry(
            file,
            &model_dir,
            Some(&manifest_path),
            DEFAULT_DOWNLOAD_RETRIES,
        ) {
            Ok(o) => o,
            Err(e) => {
                if !required {
                    println!(
                        "  -> {} not available on the CDN; skipping (non-fatal).",
                        file.local
                    );
                    continue;
                }
                return Err(map_model_download_error(e));
            }
        };

        // Re-read the manifest in case an earlier iteration fetched it, then
        // verify the freshly-downloaded file so we can surface an explicit
        // "verified" line for the user.
        let fresh_manifest_str =
            std::fs::read_to_string(&manifest_path).unwrap_or_else(|_| manifest_str.clone());
        let fresh_manifest = parse_checksums(&fresh_manifest_str);
        let recheck = check_file_against_manifest(&outcome.path, &fresh_manifest)
            .unwrap_or(CheckResult::Missing);

        match recheck {
            CheckResult::Verified => {
                println!("  -> {} downloaded, checksum verified.", file.local);
            }
            CheckResult::NoEntry => {
                println!(
                    "  -> {} downloaded (no checksum entry in manifest; cannot verify).",
                    file.local
                );
            }
            CheckResult::Mismatch { expected, actual } => {
                // VAL-SETUP-019: a freshly-downloaded file that still fails
                // the checksum check is suspicious (corrupted CDN mirror,
                // tampering, repo layout change). Surface it clearly for
                // required files; tolerate it for optional files.
                if required {
                    return Err(SetupError::ModelChecksumPostDownload {
                        file: file.local.to_string(),
                        expected,
                        actual,
                    });
                } else {
                    println!(
                        "  -> WARNING: {} downloaded but checksum mismatch; \
                         keeping anyway (non-required file).",
                        file.local
                    );
                }
            }
            CheckResult::Missing => {
                return Err(SetupError::Io(format!(
                    "Download reported success but file is missing: {}",
                    outcome.path.display()
                )));
            }
        }

        if file.local == model_onnx_name {
            model_present_after = true;
        }
        downloaded_any = true;
    }

    // ── Step 3: ensure a checksum manifest exists for future runs ────────
    // If we never obtained checksums.sha256 from the CDN (the onnx-community
    // repo does not host one), generate one locally from the files we just
    // downloaded. This makes VAL-SETUP-017 work on the second run: the
    // locally-generated manifest becomes the source of truth, and any future
    // corruption (VAL-SETUP-018) is detected against it.
    if !manifest_path.exists() {
        if let Err(e) = generate_local_checksum_manifest(&model_dir) {
            eprintln!(
                "warning: could not generate local checksum manifest ({}); \
                 future runs cannot verify file integrity until checksums.sha256 \
                 is present in {}.",
                e,
                model_dir.display()
            );
        } else {
            println!("  -> Generated local checksums.sha256 for future verification.");
        }
    }

    if downloaded_any {
        println!("\nModel files ready at {}", model_dir.display());
    }

    Ok(model_present_after)
}

/// Write a `checksums.sha256` file into `model_dir` by computing SHA256 of
/// each model file present. Used when the onnx-community CDN does not host a
/// manifest (it does not), so that subsequent setup runs can verify file
/// integrity (VAL-SETUP-017) and detect corruption (VAL-SETUP-018).
fn generate_local_checksum_manifest(model_dir: &std::path::Path) -> std::io::Result<()> {
    use crate::cli::leindex::model_download::{iter_model_files, sha256_of_file};
    let manifest_path = model_dir.join("checksums.sha256");
    let mut out = String::new();
    for file in iter_model_files() {
        if file.local == "checksums.sha256" {
            continue;
        }
        let path = model_dir.join(file.local);
        if path.exists() {
            let hash = sha256_of_file(&path)?;
            out.push_str(&hash);
            out.push_str("  ");
            out.push_str(file.local);
            out.push('\n');
        }
    }
    if out.is_empty() {
        return Ok(());
    }
    std::fs::write(&manifest_path, out)
}

/// Copy every model file present in `bundled_dir` into `dest_dir`, skipping
/// any that already exist in `dest_dir`. Used as a zero-network fast path when
/// the user is running from a release bundle layout.
fn copy_bundled_models(bundled_dir: &std::path::Path, dest_dir: &std::path::Path) {
    let mut copied_any = false;
    for file in crate::cli::leindex::model_download::iter_model_files() {
        let src = bundled_dir.join(file.local);
        let dst = dest_dir.join(file.local);
        if src.exists() && !dst.exists() {
            if !copied_any {
                println!(
                    "  -> Copying bundled model files from {}...",
                    bundled_dir.display()
                );
                copied_any = true;
            }
            match std::fs::copy(&src, &dst) {
                Ok(_) => {}
                Err(e) => {
                    eprintln!(
                        "warning: failed to copy {} from bundle ({}); will download instead.",
                        file.local, e
                    );
                }
            }
        }
    }
}

/// Strip a SHA256 hex string down to its first 12 chars for compact logging.
fn short_hash(hash: &str) -> String {
    if hash.len() <= 12 {
        hash.to_string()
    } else {
        format!("{}...", &hash[..12])
    }
}

/// Convert a [`model_download::ModelDownloadError`] into the equivalent
/// [`SetupError`] so the caller-facing Display impl stays uniform.
fn map_model_download_error(
    e: crate::cli::leindex::model_download::ModelDownloadError,
) -> SetupError {
    use crate::cli::leindex::model_download::ModelDownloadError as Mde;
    match e {
        Mde::CurlNotFound => SetupError::CurlNotFound,
        Mde::Io(path, msg) => SetupError::Io(format!("{}: {}", path.display(), msg)),
        Mde::DownloadFailed {
            file,
            url,
            exit_code,
            network,
        } => SetupError::ModelDownloadFailed {
            file,
            url,
            exit_code,
            network,
        },
    }
}

/// Print a status report without modifying anything.
///
/// VAL-SETUP-014: --check mode reads config and reports status
/// VAL-SETUP-020: Reports the ORT version (from config + live detection)
/// VAL-SETUP-034: Surfaces full configuration
pub fn run_check() -> Result<CheckResult, SetupError> {
    let (config, action) = crate::cli::neural_config::LeIndexConfig::load_or_recover()
        .map_err(|e| SetupError::ConfigRead(e.to_string()))?;

    let ort_installed = check_ort_installed();
    let live_version = get_ort_version();
    let model_present = check_model_present();
    // VAL-SETUP-014/018: checksum status is surfaced so a corrupted file
    // is visible from `--check` without needing to re-run setup.
    let checksum_status = model_checksum_status();
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

    // ORT status
    let ort_status = if ort_installed {
        "installed"
    } else {
        "not installed"
    };
    println!("ORT (onnxruntime): {}", ort_status);

    // VAL-SETUP-020: version reporting
    if let Some(ref version) = ort_version {
        println!("ORT version:        {}", version);

        // VAL-SETUP-022: surface compatibility verdict so `--check` can warn.
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

    if let Some(ref path) = ort_path {
        println!("ORT dylib path:     {}", path.display());
    } else if let Some(ref config_path) = config.neural.ort_dylib_path {
        println!("ORT dylib (config): {} [file missing]", config_path);
    } else {
        println!("ORT dylib path:     (not discovered)");
    }

    // Model status
    let model_status = if model_present { "present" } else { "absent" };
    println!("Model files:        {}", model_status);
    println!("Model directory:    {}", config.neural.model_dir);

    // VAL-SETUP-017/018: report the checksum verdict so users can tell whether
    // `~/.leindex/models/qwen3-embed-0.6b.onnx` is intact or needs re-download.
    match &checksum_status {
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

    // Search settings
    println!();
    println!("Search mode:        {}", config.search.search_mode);
    println!("Neural weight:      {}", config.search.neural_weight);

    // Recovery notice
    if let crate::cli::neural_config::RecoveryAction::RecoveredFromCorrupt(ref backup) = action {
        println!();
        println!(
            "WARNING: Previous config was corrupted. Backed up to: {}",
            backup.display()
        );
    }

    // Overall status
    println!();
    if fully_configured {
        println!("Status: Fully configured for neural search");
    } else if config.neural.enabled {
        println!("Status: Neural enabled but incomplete");
        if !ort_installed {
            println!("  -> Install ORT: leindex setup --neural --cpu");
        }
        if !model_present {
            println!("  -> Model files needed: run leindex setup --neural --cpu");
        }
    } else {
        println!("Status: TF-IDF only (neural not configured)");
        println!("  -> To enable neural search: leindex setup --neural --cpu");
    }

    // Config file path
    if let Some(path) = crate::cli::neural_config::config_file_path() {
        println!();
        println!("Config file: {}", path.display());
    }

    Ok(CheckResult {
        neural_enabled: config.neural.enabled,
        provider: config.neural.execution_provider.clone(),
        ort_installed,
        ort_version,
        ort_path,
        model_present,
        fully_configured,
    })
}

/// Result of --check mode.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct CheckResult {
    /// Whether neural is enabled in config.
    pub neural_enabled: bool,
    /// Configured execution provider string.
    pub provider: String,
    /// Whether ORT is installed.
    pub ort_installed: bool,
    /// Detected or recorded ORT version, if any (VAL-SETUP-020).
    pub ort_version: Option<String>,
    /// Discovered ORT dylib path.
    pub ort_path: Option<PathBuf>,
    /// Whether model files are present.
    pub model_present: bool,
    /// Whether all components are ready for neural search.
    pub fully_configured: bool,
}

/// Print a final summary after setup completes.
///
/// VAL-SETUP-020: Surfaces the ORT version.
/// VAL-SETUP-034: The summary surfaces all five pieces of status.
pub fn print_summary(result: &SetupResult) {
    println!("\nSetup Summary\n{}", "-".repeat(14));

    let neural_str = if result.choices.neural_enabled {
        "ON"
    } else {
        "OFF"
    };
    println!("Neural:       {}", neural_str);

    if let Some(provider) = result.choices.provider {
        println!("Provider:     {}", provider.config_value());
    }

    let ort_str = if result.ort_installed {
        "installed"
    } else {
        "not installed"
    };
    println!("ORT:          {}", ort_str);

    // VAL-SETUP-020: ORT version
    if let Some(ref version) = result.ort_version {
        println!("ORT version:  {}", version);
    }

    if let Some(ref path) = result.ort_dylib_path {
        println!("ORT path:     {}", path.display());
    }

    let model_str = if result.model_present {
        "present"
    } else {
        "absent"
    };
    println!("Model:        {}", model_str);

    if let Some(ref path) = result.config_path {
        println!("Config:       {}", path.display());
    }

    // Final status line
    println!();
    if result.choices.neural_enabled {
        if result.model_present && result.ort_installed {
            println!("Neural search is ready!");
        } else {
            let missing = if !result.ort_installed && !result.model_present {
                "ORT and model files"
            } else if !result.ort_installed {
                "ORT"
            } else {
                "model files"
            };
            println!(
                "Neural search is partially configured (missing: {})",
                missing
            );
            println!("Re-run: leindex setup --neural --cpu");
        }
    } else {
        println!("TF-IDF search is ready (neural disabled).");
        println!("To enable neural search later: leindex setup");
    }
}

/// Parse a GPU vendor string from CLI.
pub fn parse_gpu_vendor(s: &str) -> Result<GpuVendor, String> {
    match s.to_lowercase().as_str() {
        "amd" => Ok(GpuVendor::Amd),
        "nvidia" | "cuda" => Ok(GpuVendor::Nvidia),
        _ => Err(format!(
            "Invalid GPU vendor '{}'. Use 'amd' or 'nvidia'.",
            s
        )),
    }
}

/// Errors that can occur during setup.
#[derive(Debug)]
pub enum SetupError {
    /// Conflicting CLI flags.
    Conflict { message: String },
    /// No setup flags provided and not interactive.
    NoFlags,
    /// Interactive prompt failed.
    Interactive(String),
    /// Config write error.
    ConfigWrite(String),
    /// Config read error.
    ConfigRead(String),
    /// pip not found on PATH.
    ///
    /// VAL-SETUP-021: The Display impl names pip as missing and suggests
    /// install instructions + the PIP_BIN override.
    PipNotFound,
    /// pip install failed with a generic (non-network) error.
    PipInstallFailed { package: String, exit_code: i32 },
    /// pip install failed due to a network/connectivity problem.
    ///
    /// Surfaced distinctly from PipInstallFailed so we can give the user a
    /// clearer remediation hint (check connectivity / proxy / mirror).
    PipNetworkFailed {
        package: String,
        exit_code: i32,
        output: String,
    },
    /// curl is not on PATH, so the model download cannot start.
    ///
    /// VAL-SETUP-016/019: model download requires curl. The Display impl
    /// surfaces install instructions for each platform.
    CurlNotFound,
    /// Model file download failed.
    ///
    /// VAL-SETUP-019: when `network` is true, the message mentions
    /// connectivity and `LEINDEX_MODEL_PATH` so the user has an actionable
    /// remediation hint.
    ModelDownloadFailed {
        file: String,
        url: String,
        exit_code: i32,
        network: bool,
    },
    /// Post-download SHA256 mismatch even after a fresh download.
    ///
    /// Indicates CDN corruption, a repo-layout drift, or tampering. Surface
    /// both the expected and actual hashes so the user can compare against
    /// `models/checksums.sha256` manually.
    ModelChecksumPostDownload {
        file: String,
        expected: String,
        actual: String,
    },
    /// I/O error.
    Io(String),
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Conflict { message } => write!(f, "{}", message),
            SetupError::NoFlags => {
                write!(f, "No setup options specified. Use --neural, --no-neural, --cpu, --gpu, or --check. Run 'leindex setup --help' for details.")
            }
            SetupError::Interactive(msg) => {
                write!(f, "Interactive prompt failed: {}. If running in a non-interactive context, use flags like --neural --cpu.", msg)
            }
            SetupError::ConfigWrite(msg) => {
                write!(f, "Failed to write config: {}", msg)
            }
            SetupError::ConfigRead(msg) => {
                write!(f, "Failed to read config: {}", msg)
            }
            SetupError::PipNotFound => {
                // VAL-SETUP-021: actionable error including PIP_BIN and OS hints.
                write!(
                    f,
                    "pip not found on PATH. Install pip first:\n  \
                     - Debian/Ubuntu: sudo apt install python3-pip\n  \
                     - macOS/Linux (ensurepip): python3 -m ensurepip --upgrade\n  \
                     - Or download: https://pip.pypa.io/en/stable/installation/\n  \
                     Alternatively, set PIP_BIN=/path/to/pip \
                     (or PIP_BIN=\"python3 -m pip\") to point setup at a specific pip, \
                     or manually install onnxruntime and set ORT_DYLIB_PATH."
                )
            }
            SetupError::PipInstallFailed { package, exit_code } => {
                write!(
                    f,
                    "Failed to install {} via pip (exit code {}). \
                     Check your Python environment. \
                     If onnxruntime is already installed in another Python, \
                     set PIP_BIN or ORT_DYLIB_PATH to use it.",
                    package, exit_code
                )
            }
            SetupError::PipNetworkFailed {
                package,
                exit_code,
                output,
            } => {
                write!(
                    f,
                    "Network failure while installing {} via pip (exit code {}). \
                     Check your internet connection, proxy settings, or PyPI mirror. \
                     pip output:\n{}",
                    package, exit_code, output
                )
            }
            SetupError::CurlNotFound => {
                // VAL-SETUP-016/019: model download depends on curl.
                write!(
                    f,
                    "curl not found on PATH. curl is required to download model \
                     files (~600 MB) for neural search. Install curl:\n  \
                     - Debian/Ubuntu: sudo apt install curl\n  \
                     - macOS: curl ships with macOS (verify /usr/bin/curl)\n  \
                     - Windows 10+: curl.exe is preinstalled\n  \
                     Alternatively, copy model files manually to \
                     ~/.leindex/models/ and re-run `leindex setup --check`."
                )
            }
            SetupError::ModelDownloadFailed {
                file,
                url,
                exit_code,
                network,
            } => {
                if *network {
                    // VAL-SETUP-019: actionable connectivity-themed message.
                    write!(
                        f,
                        "Network failure downloading '{}' from {} (curl exit code {}). \
                         Check your internet connection, DNS, proxy settings, or \
                         the HuggingFace CDN status (https://status.huggingface.co). \
                         Re-run `leindex setup` to retry, or set LEINDEX_MODEL_PATH \
                         to point at an offline model directory containing '{}'.",
                        file, url, exit_code, file
                    )
                } else {
                    write!(
                        f,
                        "Failed to download '{}' from {} (curl exit code {}). \
                         The file may be temporarily unavailable on the CDN, or the \
                         repo layout changed. Re-run `leindex setup` to retry, or \
                         copy the model manually to ~/.leindex/models/. If you have \
                         an offline copy, set LEINDEX_MODEL_PATH.",
                        file, url, exit_code
                    )
                }
            }
            SetupError::ModelChecksumPostDownload {
                file,
                expected,
                actual,
            } => {
                write!(
                    f,
                    "Checksum mismatch after downloading '{}' (expected {}, got {}). \
                     This usually indicates a CDN mirror returned a corrupted file or \
                     the model repo layout changed. Wait a few minutes and re-run \
                     `leindex setup`, or copy the file manually from a trusted source \
                     to ~/.leindex/models/{}.",
                    file, expected, actual, file
                )
            }
            SetupError::Io(msg) => write!(f, "I/O error: {}", msg),
        }
    }
}

impl std::error::Error for SetupError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_neural_cpu() {
        // VAL-SETUP-010: --neural --cpu forces CPU
        let choices = resolve_from_flags(true, false, true, None).unwrap();
        assert!(choices.neural_enabled);
        assert_eq!(choices.provider, Some(ExecutionProvider::Cpu));
    }

    #[test]
    fn test_resolve_neural_gpu_amd() {
        // VAL-SETUP-011: --neural --gpu amd forces MIGraphX
        let choices = resolve_from_flags(true, false, false, Some(GpuVendor::Amd)).unwrap();
        assert!(choices.neural_enabled);
        assert_eq!(choices.provider, Some(ExecutionProvider::Migraphx));
    }

    #[test]
    fn test_resolve_neural_gpu_nvidia() {
        // VAL-SETUP-012: --neural --gpu nvidia forces CUDA
        let choices = resolve_from_flags(true, false, false, Some(GpuVendor::Nvidia)).unwrap();
        assert!(choices.neural_enabled);
        assert_eq!(choices.provider, Some(ExecutionProvider::Cuda));
    }

    #[test]
    fn test_resolve_no_neural() {
        // VAL-SETUP-013: --no-neural disables neural
        let choices = resolve_from_flags(false, true, false, None).unwrap();
        assert!(!choices.neural_enabled);
        assert!(choices.provider.is_none());
    }

    #[test]
    fn test_resolve_neural_default_cpu() {
        // VAL-SETUP-009: --neural alone defaults to CPU
        let choices = resolve_from_flags(true, false, false, None).unwrap();
        assert!(choices.neural_enabled);
        assert_eq!(choices.provider, Some(ExecutionProvider::Cpu));
    }

    #[test]
    fn test_conflict_neural_no_neural() {
        // VAL-SETUP-015: --neural + --no-neural is a conflict
        let result = resolve_from_flags(true, true, false, None);
        assert!(matches!(result, Err(SetupError::Conflict { .. })));
    }

    #[test]
    fn test_conflict_cpu_gpu() {
        // VAL-SETUP-015: --cpu + --gpu is a conflict
        let result = resolve_from_flags(false, false, true, Some(GpuVendor::Amd));
        assert!(matches!(result, Err(SetupError::Conflict { .. })));
    }

    #[test]
    fn test_cpu_implies_neural() {
        // --cpu without --neural should imply neural
        let choices = resolve_from_flags(false, false, true, None).unwrap();
        assert!(choices.neural_enabled);
        assert_eq!(choices.provider, Some(ExecutionProvider::Cpu));
    }

    #[test]
    fn test_gpu_implies_neural() {
        // --gpu without --neural should imply neural
        let choices = resolve_from_flags(false, false, false, Some(GpuVendor::Amd)).unwrap();
        assert!(choices.neural_enabled);
    }

    #[test]
    fn test_no_flags_errors() {
        let result = resolve_from_flags(false, false, false, None);
        assert!(matches!(result, Err(SetupError::NoFlags)));
    }

    #[test]
    fn test_parse_gpu_vendor_amd() {
        assert_eq!(parse_gpu_vendor("amd").unwrap(), GpuVendor::Amd);
        assert_eq!(parse_gpu_vendor("AMD").unwrap(), GpuVendor::Amd);
    }

    #[test]
    fn test_parse_gpu_vendor_nvidia() {
        assert_eq!(parse_gpu_vendor("nvidia").unwrap(), GpuVendor::Nvidia);
        assert_eq!(parse_gpu_vendor("cuda").unwrap(), GpuVendor::Nvidia);
    }

    #[test]
    fn test_parse_gpu_vendor_invalid() {
        assert!(parse_gpu_vendor("intel").is_err());
    }

    #[test]
    fn test_execution_provider_pip_package() {
        assert_eq!(ExecutionProvider::Cpu.pip_package(), "onnxruntime");
        assert_eq!(ExecutionProvider::Cuda.pip_package(), "onnxruntime-gpu");
        assert_eq!(
            ExecutionProvider::Migraphx.pip_package(),
            "onnxruntime-migraphx"
        );
    }

    #[test]
    fn test_execution_provider_config_value() {
        assert_eq!(ExecutionProvider::Cpu.config_value(), "cpu");
        assert_eq!(ExecutionProvider::Cuda.config_value(), "cuda");
        assert_eq!(ExecutionProvider::Migraphx.config_value(), "migraphx");
    }

    #[test]
    fn test_setup_error_display() {
        let err = SetupError::Conflict {
            message: "test conflict".to_string(),
        };
        assert!(err.to_string().contains("test conflict"));

        let err = SetupError::PipNotFound;
        assert!(err.to_string().contains("pip not found"));
    }

    // ── VAL-SETUP-016/017/018/019: model download error surface tests ──

    #[test]
    fn test_curl_not_found_error_mentions_curl() {
        // VAL-SETUP-016/019: curl-not-found error must name curl.
        let err = SetupError::CurlNotFound;
        let msg = err.to_string();
        assert!(msg.contains("curl not found"), "{}", msg);
        assert!(msg.contains("models/"), "{}", msg);
    }

    #[test]
    fn test_model_download_network_error_mentions_connectivity() {
        // VAL-SETUP-019: network-classified failure must mention connectivity
        // AND the LEINDEX_MODEL_PATH remediation hint.
        let err = SetupError::ModelDownloadFailed {
            file: "qwen3-embed-0.6b.onnx".to_string(),
            url: "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/onnx/model.onnx".to_string(),
            exit_code: 28,
            network: true,
        };
        let msg = err.to_string();
        assert!(msg.contains("Network failure"), "{}", msg);
        assert!(msg.contains("internet connection"), "{}", msg);
        assert!(msg.contains("LEINDEX_MODEL_PATH"), "{}", msg);
        assert!(msg.contains("huggingface.co"), "{}", msg);
        assert!(msg.contains("exit code 28"), "{}", msg);
    }

    #[test]
    fn test_model_download_generic_error_is_actionable() {
        // Non-network failure: should still name the URL and suggest re-run.
        let err = SetupError::ModelDownloadFailed {
            file: "tokenizer.json".to_string(),
            url: "https://huggingface.co/onnx-community/Qwen3-Embedding-0.6B-ONNX/resolve/main/tokenizer.json".to_string(),
            exit_code: 22,
            network: false,
        };
        let msg = err.to_string();
        // Generic branch does NOT mention "Network failure".
        assert!(!msg.contains("Network failure"), "{}", msg);
        assert!(msg.contains("tokenizer.json"), "{}", msg);
        assert!(msg.contains("Re-run"), "{}", msg);
    }

    #[test]
    fn test_model_checksum_post_download_error_names_file_and_hashes() {
        let err = SetupError::ModelChecksumPostDownload {
            file: "qwen3-embed-0.6b.onnx".to_string(),
            expected: "aaaa1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd"
                .to_string(),
            actual: "bbbb1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcd"
                .to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Checksum mismatch"), "{}", msg);
        assert!(msg.contains("qwen3-embed-0.6b.onnx"), "{}", msg);
        // Display prints the full hashes (no shortening in this variant).
        assert!(msg.contains("aaaa1234567890abcdef"), "{}", msg);
        assert!(msg.contains("bbbb1234567890abcdef"), "{}", msg);
        assert!(msg.contains("copy the file manually"), "{}", msg);
    }

    #[test]
    fn test_model_checksum_status_missing_for_clean_dir() {
        // VAL-SETUP-017: no model + no manifest -> Missing. We exercise this by
        // pointing LEINDEX_HOME at a fresh tempdir.
        let tmp = std::env::temp_dir().join(format!(
            "leindex-setup-test-mcs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("LEINDEX_HOME", &tmp);
        let status = model_checksum_status();
        std::env::remove_var("LEINDEX_HOME");
        let _ = std::fs::remove_dir_all(&tmp);
        assert_eq!(status, ModelChecksumStatus::Missing);
    }

    // ── VAL-SETUP-020/021/022: New error and version-compatibility tests ──

    #[test]
    fn test_pip_not_found_error_mentions_pip_bin() {
        // VAL-SETUP-021: error must mention PIP_BIN as a remediation.
        let err = SetupError::PipNotFound;
        let msg = err.to_string();
        assert!(
            msg.contains("PIP_BIN"),
            "PipNotFound must mention PIP_BIN: {}",
            msg
        );
        assert!(msg.contains("python3-pip") || msg.contains("ensurepip"));
    }

    #[test]
    fn test_pip_install_failed_error_mentions_package() {
        let err = SetupError::PipInstallFailed {
            package: "onnxruntime".to_string(),
            exit_code: 1,
        };
        let msg = err.to_string();
        assert!(msg.contains("onnxruntime"));
        assert!(msg.contains("exit code 1"));
    }

    #[test]
    fn test_pip_network_failed_error_mentions_network() {
        let err = SetupError::PipNetworkFailed {
            package: "onnxruntime".to_string(),
            exit_code: 1,
            output: "Could not fetch URL pypi.org".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Network failure"));
        assert!(msg.contains("onnxruntime"));
        assert!(msg.contains("internet connection"));
        assert!(msg.contains("pypi.org"));
    }

    #[test]
    fn test_parse_version_simple() {
        assert_eq!(parse_version("1.25.0"), Some((1, 25, 0)));
        assert_eq!(parse_version("1.20.0"), Some((1, 20, 0)));
        assert_eq!(parse_version("2.0.0"), Some((2, 0, 0)));
        assert_eq!(parse_version("0.9.9"), Some((0, 9, 9)));
    }

    #[test]
    fn test_parse_version_with_prerelease() {
        // Suffixes are ignored.
        assert_eq!(parse_version("1.25.0-rc1"), Some((1, 25, 0)));
        assert_eq!(parse_version("1.25.0+build42"), Some((1, 25, 0)));
        assert_eq!(parse_version("1.25.0-rc1+meta"), Some((1, 25, 0)));
    }

    #[test]
    fn test_parse_version_missing_patch_defaults_to_zero() {
        // Default missing minor/patch to 0 (semver-like leniency).
        assert_eq!(parse_version("1.25"), Some((1, 25, 0)));
        assert_eq!(parse_version("1"), Some((1, 0, 0)));
    }

    #[test]
    fn test_parse_version_invalid() {
        assert_eq!(parse_version("not-a-version"), None);
        assert_eq!(parse_version("v1.2.3"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn test_version_compatibility_supported() {
        // 1.20.0, 1.25.0, 1.99.99 are supported (within 1.x, >= MIN_ORT_VERSION).
        assert_eq!(
            check_ort_version_compatibility("1.20.0"),
            VersionCompatibility::Supported
        );
        assert_eq!(
            check_ort_version_compatibility("1.25.0"),
            VersionCompatibility::Supported
        );
        assert_eq!(
            check_ort_version_compatibility("1.99.99"),
            VersionCompatibility::Supported
        );
    }

    #[test]
    fn test_version_compatibility_too_old() {
        // 1.19.x and older are unsupported.
        let r = check_ort_version_compatibility("1.19.0");
        match r {
            VersionCompatibility::Unsupported {
                required_min,
                reason,
            } => {
                assert!(
                    required_min.contains("1.20.0"),
                    "required_min = {}",
                    required_min
                );
                assert!(!reason.is_empty());
            }
            other => panic!("expected Unsupported, got {:?}", other),
        }
    }

    #[test]
    fn test_version_compatibility_very_old() {
        // 0.x is definitely unsupported.
        let r = check_ort_version_compatibility("0.9.9");
        assert!(matches!(r, VersionCompatibility::Unsupported { .. }));
    }

    #[test]
    fn test_version_compatibility_too_new() {
        // 2.0.0+ is TooNew (ABI break).
        let r = check_ort_version_compatibility("2.0.0");
        match r {
            VersionCompatibility::TooNew {
                supported_max,
                reason,
            } => {
                assert!(
                    supported_max.contains("1."),
                    "supported_max = {}",
                    supported_max
                );
                assert!(reason.contains("ABI") || reason.contains("breaking"));
            }
            other => panic!("expected TooNew, got {:?}", other),
        }
    }

    #[test]
    fn test_version_compatibility_unparseable() {
        // Unparseable versions fall back to Unsupported.
        let r = check_ort_version_compatibility("garbage");
        assert!(matches!(r, VersionCompatibility::Unsupported { .. }));
    }

    #[test]
    fn test_is_network_error_detects_common_failures() {
        // VAL-SETUP-019-like network detection on pip output.
        assert!(is_network_error(
            "WARNING: Could not fetch URL https://pypi.org/onnxruntime"
        ));
        assert!(is_network_error(
            "ConnectionError: Failed to establish a new connection"
        ));
        assert!(is_network_error("ReadTimeoutError: read timed out"));
        assert!(is_network_error(
            "WARNING: Retrying (Retry(total=4)) after connection broken"
        ));
        // The MAX_RETRIES style.
        assert!(is_network_error(
            "HTTPSConnectionPool: Max retries exceeded with url: /simple/onnxruntime"
        ));
    }

    #[test]
    fn test_is_network_error_ignores_normal_output() {
        // Normal pip output is NOT a network error.
        assert!(!is_network_error(
            "Successfully installed onnxruntime-1.25.0"
        ));
        assert!(!is_network_error("Requirement already satisfied: numpy"));
        assert!(!is_network_error(""));
    }

    #[test]
    fn test_truncate_for_error_short() {
        let s = "line1\nline2\n";
        assert_eq!(truncate_for_error(s), "line1\nline2");
    }

    #[test]
    fn test_truncate_for_error_long() {
        let long = (0..25)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");
        let truncated = truncate_for_error(&long);
        assert!(truncated.contains("truncated"));
        assert!(truncated.contains("line 0"));
        // The originally-present tail is dropped.
        assert!(!truncated.contains("line 24"));
    }

    #[test]
    fn test_min_ort_version_constant_is_sensible() {
        // Guards against accidental breakage of the supported-range constant.
        // The constants are compile-time known; just exercise them so a future
        // edit that flips them nonsensically shows up in the test name.
        let _: (u32, u32, u32) = MIN_ORT_VERSION;
        let _: u32 = MAX_ORT_MAJOR;
        // Sanity: min version is 1.x.
        assert_eq!(MIN_ORT_VERSION.0, 1);
    }

    #[test]
    fn test_build_config_records_ort_version() {
        // VAL-SETUP-020: ort_version flows into the config written to disk.
        let choices = SetupChoices {
            neural_enabled: true,
            provider: Some(ExecutionProvider::Cpu),
        };
        let cfg = build_config(
            &choices,
            Some(std::path::Path::new("/usr/local/lib/libonnxruntime.so")),
            Some("1.25.0"),
        );
        assert_eq!(cfg.neural.ort_version.as_deref(), Some("1.25.0"));
        assert_eq!(
            cfg.neural.ort_dylib_path.as_deref(),
            Some("/usr/local/lib/libonnxruntime.so")
        );
        assert!(cfg.neural.enabled);
        assert_eq!(cfg.neural.execution_provider, "cpu");
    }

    #[test]
    fn test_build_config_without_version() {
        // When version detection failed, ort_version remains None but the rest
        // of the config is still valid.
        let choices = SetupChoices {
            neural_enabled: true,
            provider: Some(ExecutionProvider::Cpu),
        };
        let cfg = build_config(&choices, None, None);
        assert!(cfg.neural.ort_version.is_none());
        assert!(cfg.neural.ort_dylib_path.is_none());
    }

    // Use a process-shared lock so env-mutating tests serialize within the module.
    use std::sync::Mutex;
    static PIPE_ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_find_pip_honors_pip_bin_with_split() {
        // VAL-SETUP-021: PIP_BIN can point at "python3 -m pip" style.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        // /bin/true is a guaranteed-present binary that succeeds with whatever args,
        // so use it as a "pip" stand-in. We only check the parse logic.
        std::env::set_var("PIP_BIN", "/bin/true -m pip");
        // find_pip runs --version; /bin/true returns 0, so it should "succeed".
        let result = find_pip();
        std::env::remove_var("PIP_BIN");
        let (program, prefix) = result.expect("PIP_BIN should be honored");
        assert_eq!(program, "/bin/true");
        assert_eq!(prefix, vec!["-m".to_string(), "pip".to_string()]);
    }

    #[test]
    fn test_find_pip_honors_pip_bin_single_token() {
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        std::env::set_var("PIP_BIN", "/bin/true");
        let result = find_pip();
        std::env::remove_var("PIP_BIN");
        let (program, prefix) = result.expect("PIP_BIN single token should be honored");
        assert_eq!(program, "/bin/true");
        assert!(prefix.is_empty());
    }

    #[test]
    fn test_find_pip_empty_pip_bin_falls_through() {
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        std::env::set_var("PIP_BIN", "   ");
        // We can't assert what the fallback finds (system-dependent), but it
        // must not crash and must follow PIP_BIN's absence.
        let _ = find_pip();
        std::env::remove_var("PIP_BIN");
    }
}
