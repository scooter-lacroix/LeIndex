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
/// VAL-SETUP-023: Config written with correct schema
/// VAL-SETUP-024: Idempotent
pub fn execute_setup(choices: &SetupChoices) -> Result<SetupResult, SetupError> {
    // Check current state
    let ort_installed = check_ort_installed();
    let model_present = check_model_present();

    let ort_dylib_path = if choices.neural_enabled {
        let provider = choices.provider.unwrap_or(ExecutionProvider::Cpu);

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
        }

        // Discover ORT dylib path
        discover_ort_path()
    } else {
        None
    };

    // Check models when neural is enabled
    let model_present = if choices.neural_enabled && !model_present {
        ensure_models_present()?
    } else {
        model_present
    };

    // Write the config
    let config = build_config(choices, ort_dylib_path.as_deref());
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
) -> crate::cli::neural_config::LeIndexConfig {
    use crate::cli::neural_config::{IndexingConfig, NeuralConfig, SearchConfig};

    let provider_str = choices.provider.map(|p| p.config_value()).unwrap_or("auto");

    crate::cli::neural_config::LeIndexConfig {
        neural: NeuralConfig {
            enabled: choices.neural_enabled,
            execution_provider: provider_str.to_string(),
            ort_dylib_path: ort_dylib_path.map(|p| p.display().to_string()),
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
    // Try importing onnxruntime via Python
    let candidates = ["python3", "python"];
    for cmd in &candidates {
        let result = Command::new(cmd)
            .arg("-c")
            .arg("import onnxruntime; print(onnxruntime.__version__)")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();

        if let Ok(out) = result {
            if out.status.success() {
                return true;
            }
        }
    }
    false
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

/// Install ORT via pip for the given execution provider.
///
/// VAL-SETUP-006: AMD -> pip install onnxruntime-migraphx
/// VAL-SETUP-007: NVIDIA -> pip install onnxruntime-gpu
/// VAL-SETUP-008/010: CPU -> pip install onnxruntime
fn install_ort(provider: ExecutionProvider) -> Result<(), SetupError> {
    let package = provider.pip_package();

    println!("Installing {} via pip...", package);

    // Find pip
    let pip_cmd = find_pip().ok_or(SetupError::PipNotFound)?;

    let result = Command::new(&pip_cmd.0)
        .args(&pip_cmd.1)
        .arg("install")
        .arg(package)
        .arg("--upgrade")
        .status();

    match result {
        Ok(status) if status.success() => {
            println!("  -> Successfully installed {}.", package);
            Ok(())
        }
        Ok(status) => Err(SetupError::PipInstallFailed {
            package: package.to_string(),
            exit_code: status.code().unwrap_or(-1),
        }),
        Err(e) => Err(SetupError::Io(format!("Failed to run pip: {}", e))),
    }
}

/// Find the pip executable.
///
/// Returns (program, args_before_install) where args_before_install is the
/// prefix arguments (e.g., ["-m", "pip"] for `python3 -m pip`).
fn find_pip() -> Option<(String, Vec<String>)> {
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

/// Ensure model files are present. Download from local bundle if available.
fn ensure_models_present() -> Result<bool, SetupError> {
    let model_name = "qwen3-embed-0.6b.onnx";
    let tokenizer_name = "tokenizer.json";

    let model_dir = crate::cli::neural_config::model_dir_path()
        .ok_or_else(|| SetupError::Io("Cannot resolve model directory".to_string()))?;

    // Create model directory
    std::fs::create_dir_all(&model_dir)
        .map_err(|e| SetupError::Io(format!("Cannot create model dir: {}", e)))?;

    // Try to copy from bundled location
    let bundled = find_bundled_models();

    if let Some(bundled_dir) = bundled {
        let model_src = bundled_dir.join(model_name);
        let model_dst = model_dir.join(model_name);

        if model_src.exists() && !model_dst.exists() {
            println!("  -> Copying model files from {}...", bundled_dir.display());

            // Copy model
            if let Err(e) = std::fs::copy(&model_src, &model_dst) {
                return Err(SetupError::Io(format!("Failed to copy model: {}", e)));
            }

            // Copy tokenizer
            let tok_src = bundled_dir.join(tokenizer_name);
            let tok_dst = model_dir.join(tokenizer_name);
            if tok_src.exists() {
                let _ = std::fs::copy(&tok_src, &tok_dst);
            }

            // Copy config.json if present
            let cfg_src = bundled_dir.join("config.json");
            let cfg_dst = model_dir.join("config.json");
            if cfg_src.exists() {
                let _ = std::fs::copy(&cfg_src, &cfg_dst);
            }

            // Copy checksums if present
            let chk_src = bundled_dir.join("checksums.sha256");
            let chk_dst = model_dir.join("checksums.sha256");
            if chk_src.exists() {
                let _ = std::fs::copy(&chk_src, &chk_dst);
            }

            return Ok(true);
        }
    }

    // If model already exists, we're good
    if model_dir.join(model_name).exists() {
        return Ok(true);
    }

    // Model not found and no bundled source - this is expected for first-time
    // download which is handled by a separate feature. For now, warn but continue.
    println!("  -> WARNING: Model files not found. Download will be available in a future update.");
    println!("     Model dir: {}", model_dir.display());
    println!("     You can manually copy model files to this directory.");

    Ok(false)
}

/// Find bundled model files relative to the binary.
fn find_bundled_models() -> Option<PathBuf> {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            // Check exe_parent/models (target/release/models)
            let candidate = parent.join("models");
            if candidate.join("qwen3-embed-0.6b.onnx").exists() {
                return Some(candidate);
            }
            // Check exe_parent/../models (target/models)
            if let Some(gp) = parent.parent() {
                let candidate = gp.join("models");
                if candidate.join("qwen3-embed-0.6b.onnx").exists() {
                    return Some(candidate);
                }
                // Check workspace root (target/../../models)
                if let Some(ggp) = gp.parent() {
                    let candidate = ggp.join("models");
                    if candidate.join("qwen3-embed-0.6b.onnx").exists() {
                        return Some(candidate);
                    }
                }
            }
        }
    }
    None
}

/// Print a status report without modifying anything.
///
/// VAL-SETUP-014: --check mode reads config and reports status
/// VAL-SETUP-034: Surfaces full configuration
pub fn run_check() -> Result<CheckResult, SetupError> {
    let (config, action) = crate::cli::neural_config::LeIndexConfig::load_or_recover()
        .map_err(|e| SetupError::ConfigRead(e.to_string()))?;

    let ort_installed = check_ort_installed();
    let model_present = check_model_present();
    let ort_path = discover_ort_path().or_else(|| {
        config
            .neural
            .ort_dylib_path
            .as_ref()
            .map(PathBuf::from)
            .filter(|p| p.exists())
    });

    let is_fully_configured = config.neural.enabled && ort_installed && model_present;

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
    if is_fully_configured {
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
        ort_path,
        model_present,
        fully_configured: is_fully_configured,
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
    /// Discovered ORT dylib path.
    pub ort_path: Option<PathBuf>,
    /// Whether model files are present.
    pub model_present: bool,
    /// Whether all components are ready for neural search.
    pub fully_configured: bool,
}

/// Print a final summary after setup completes.
///
/// VAL-SETUP-034: The summary surface all five pieces of status.
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
    PipNotFound,
    /// pip install failed.
    PipInstallFailed { package: String, exit_code: i32 },
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
                write!(f, "pip not found on PATH. Install pip first (e.g., 'python -m ensurepip' on Linux) or set PIP_BIN. Alternatively, manually install onnxruntime and set ORT_DYLIB_PATH.")
            }
            SetupError::PipInstallFailed { package, exit_code } => {
                write!(
                    f,
                    "Failed to install {} via pip (exit code {}). Check your Python environment and network connection.",
                    package, exit_code
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
}
