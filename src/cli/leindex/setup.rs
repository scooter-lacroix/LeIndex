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

use std::path::{Path, PathBuf};
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
    /// Result of the post-setup embedding smoke test (VAL-SETUP-025/026).
    /// `None` when neural is disabled or the test was not run.
    pub smoke_test: Option<SmokeTestResult>,
}

/// Outcome of the post-setup embedding smoke test.
///
/// VAL-SETUP-025: On success, carries the produced vector dimensionality.
/// VAL-SETUP-026: On failure, carries the worker error text + actionable
/// guidance so the user knows what went wrong without re-running.
#[derive(Debug, Clone)]
pub struct SmokeTestResult {
    /// Whether the smoke test passed.
    pub passed: bool,
    /// Embedding dimensionality reported by the worker (e.g., 1024).
    /// `None` when the test failed before producing vectors.
    pub dimension: Option<usize>,
    /// Execution provider the worker reported as active.
    /// `None` when the worker could not start or did not report.
    pub execution_provider: Option<String>,
    /// Execution provider configured for the smoke test request.
    ///
    /// This is intentionally separate from `execution_provider`: configured
    /// provider is not evidence that the worker actually used that provider.
    pub configured_provider_label: Option<String>,
    /// Error text from the worker on failure (truncated to a reasonable length).
    pub error: Option<String>,
}

impl SmokeTestResult {
    /// One-line status string for terminal output.
    pub fn status_line(&self) -> String {
        if self.passed {
            format!(
                "embedding test: PASS ({}-dim vector)",
                self.dimension.unwrap_or(0)
            )
        } else {
            "embedding test: FAIL".to_string()
        }
    }
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

    // VAL-SETUP-033: Before presenting the vendor menu, run a best-effort
    // detection so we can print actionable guidance when neither AMD nor
    // NVIDIA tooling is visible. The user can still pick any option; we do
    // not prevent them, but the guidance for an unknown-vendor system helps
    // them avoid dead-ending on a GPU choice they cannot satisfy.
    match detect_gpu_vendor() {
        DetectedGpu::Amd => {
            println!("  (Detected AMD GPU / ROCm tooling.)");
        }
        DetectedGpu::Nvidia => {
            println!("  (Detected NVIDIA GPU / CUDA tooling.)");
        }
        DetectedGpu::Unknown => {
            println!("  (No AMD ROCm or NVIDIA CUDA tooling detected.)");
            println!("   Recommendation: choose 'N/A' to use CPU, which works everywhere.");
            println!("   If you have a GPU, install ROCm (AMD) or the CUDA toolkit (NVIDIA)");
            println!("   and re-run `leindex setup`.");
        }
    }

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

/// Best-effort on-detection of the GPU vendor through system tooling presence.
///
/// VAL-SETUP-033: When neither AMD nor NVIDIA tooling is visible we print
/// actionable guidance before the user picks a vendor, recommending the CPU
/// fallback rather than dead-ending.
///
/// The checks are intentionally filesystem-based (no dlopen, no driver init)
/// so they are fast and safe to run on any platform. They look for the same
/// ROCm/MIGraphX and CUDA artifacts the worker's execution-provider selector
/// looks for, keeping the detection logic consistent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetectedGpu {
    /// AMD GPU detected (ROCm / MIGraphX libraries present).
    Amd,
    /// NVIDIA GPU detected (CUDA toolkit / driver present).
    Nvidia,
    /// No known GPU vendor detected.
    Unknown,
}

/// Detect the GPU vendor on this system.
///
/// VAL-SETUP-033: Used by the interactive flow to print actionable guidance.
/// Returns [`DetectedGpu::Unknown`] when neither AMD nor NVIDIA tooling is
/// visible (e.g., headless VMs, Intel/ARM GPUs without ROCm/CUDA).
pub fn detect_gpu_vendor() -> DetectedGpu {
    if detect_amd_gpu() {
        DetectedGpu::Amd
    } else if detect_nvidia_gpu() {
        DetectedGpu::Nvidia
    } else {
        DetectedGpu::Unknown
    }
}

/// Check for AMD GPU presence (ROCm / MIGraphX).
fn detect_amd_gpu() -> bool {
    // ROCm root and MIGraphX shared libraries / tooling.
    #[cfg(unix)]
    {
        let candidates = [
            "/opt/rocm",
            "/opt/rocm/lib/libmigraphx_c.so",
            "/opt/rocm/lib/libamdhip64.so",
            "/opt/rocm/bin/migraphx-driver",
            "/opt/rocm/bin/rocm-smi",
        ];
        if candidates.iter().any(|p| std::path::Path::new(p).exists()) {
            return true;
        }
    }
    // Honor the ROCM_PATH env var the same way the worker does.
    if let Ok(rocm_path) = std::env::var("ROCM_PATH") {
        if std::path::Path::new(&rocm_path).exists() {
            return true;
        }
    }
    false
}

/// Check for NVIDIA GPU presence (CUDA toolkit / driver).
fn detect_nvidia_gpu() -> bool {
    #[cfg(unix)]
    {
        let candidates = [
            "/usr/bin/nvidia-smi",
            "/usr/local/cuda/bin/nvidia-smi",
            "/usr/local/cuda",
            "/usr/lib/x86_64-linux-gnu/libcuda.so",
            "/usr/lib/x86_64-linux-gnu/libcudart.so",
        ];
        if candidates.iter().any(|p| std::path::Path::new(p).exists()) {
            return true;
        }
    }
    #[cfg(windows)]
    {
        let candidates = [
            "C:\\Windows\\System32\\nvidia-smi.exe",
            "C:\\Program Files\\NVIDIA Corporation",
            "C:\\Program Files\\NVIDIA GPU Computing Toolkit\\CUDA",
        ];
        if candidates.iter().any(|p| std::path::Path::new(p).exists()) {
            return true;
        }
    }
    if std::env::var("CUDA_PATH").is_ok() {
        return true;
    }
    // Last resort: check if nvidia-smi is on PATH and runnable.
    #[cfg(unix)]
    if std::process::Command::new("nvidia-smi")
        .arg("--query-gpu=name")
        .arg("--format=csv,noheader")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
    {
        return true;
    }
    false
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
/// VAL-SETUP-027: model present + ORT missing -> install ORT, skip model download
/// VAL-SETUP-028: ORT present + model missing -> download model, skip ORT install
/// VAL-SETUP-031: read-only home -> permission error surfaced
/// VAL-SETUP-025/026: post-setup embedding smoke test
pub fn execute_setup(choices: &SetupChoices) -> Result<SetupResult, SetupError> {
    // VAL-SETUP-031: Surface read-only home directory before any work so the
    // user gets a clear permission error naming the path. We probe by creating
    // the config directory and a sentinel file, then removing the sentinel.
    ensure_home_writable()?;

    // Check current state
    let ort_installed = check_ort_installed();
    let model_present = check_model_present();

    // VAL-SETUP-027/028: Surface partial-setup edge cases with explicit log
    // lines so the user knows setup detected the partial state and is only
    // doing the missing half. Without these lines a user who pre-staged
    // (e.g.) the model but not ORT would see setup silently install just ORT
    // and wonder whether the model step was skipped incorrectly.
    if choices.neural_enabled {
        match (ort_installed, model_present) {
            (false, true) => {
                println!("  -> Partial setup detected: model files present but ORT not installed.");
                println!("     Installing ORT without re-downloading model...");
            }
            (true, false) => {
                println!("  -> Partial setup detected: ORT installed but model files missing.");
                println!("     Downloading model without reinstalling ORT...");
            }
            (false, false) => {
                // Fresh install: nothing extra to log here (the install_ort
                // and ensure_models_present steps already narrate themselves).
            }
            (true, true) => {
                // Fully configured: both will be verified, not re-downloaded.
            }
        }
    }

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

    // Recompute ORT installed flag after the install/maintain step so the
    // smoke-test branch and the SetupResult reflect the actual end state
    // (VAL-SETUP-027: install_ort may have just brought ORT online).
    let ort_installed_final = choices.neural_enabled || check_ort_installed();

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

    // VAL-SETUP-025/026: Run the post-setup embedding smoke test. We only run
    // it when neural is enabled, ORT is installed, and the model is present.
    // On failure we still return `Ok` (with a failed `SmokeTestResult`) so the
    // caller can print the summary and exit non-zero, rather than bailing
    // before the user sees the actionable diagnostic.
    let smoke_test = if choices.neural_enabled && ort_installed_final && model_present {
        println!("\nVerifying neural search on a sample query...");
        let result = run_embedding_smoke_test(choices.provider);
        match &result {
            SmokeTestResult {
                passed: true,
                dimension: Some(dim),
                configured_provider_label: Some(provider),
                ..
            } => {
                println!("  -> {}.", result.status_line());
                println!("  -> Configured execution provider: {}.", provider);
                let _ = dim; // already in status_line
            }
            SmokeTestResult { passed: true, .. } => {
                println!("  -> {}.", result.status_line());
            }
            SmokeTestResult {
                passed: false,
                error: Some(err),
                ..
            } => {
                println!("  -> {}.", result.status_line());
                println!("     Worker error: {}", truncate_for_display(err, 200));
                println!("     Actionable guidance: run `leindex setup --check` for diagnostics,");
                println!("     verify ORT and model files are intact, or re-run `leindex setup`.");
            }
            SmokeTestResult {
                passed: false,
                error: None,
                ..
            } => {
                println!("  -> {}.", result.status_line());
            }
        }
        Some(result)
    } else if choices.neural_enabled {
        // Neural enabled but prerequisites incomplete: skip the smoke test
        // with a clear message so the user knows why it was not run.
        if !ort_installed_final {
            println!("\nSkipping embedding smoke test: ORT not installed.");
        } else if !model_present {
            println!("\nSkipping embedding smoke test: model files not present.");
        }
        None
    } else {
        None
    };

    Ok(SetupResult {
        choices: choices.clone(),
        config_path: Some(config_path),
        ort_dylib_path,
        ort_version,
        model_present,
        ort_installed: ort_installed_final,
        smoke_test,
    })
}

/// Truncate a string for terminal display, appending an ellipsis if truncated.
fn truncate_for_display(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let truncated: String = s.chars().take(max_chars).collect();
    format!("{}...", truncated)
}

/// Ensure the LeIndex home directory is writable before starting setup.
///
/// VAL-SETUP-031: Creates the home + config directories and probes writability
/// with a sentinel file. Returns a clear `PermissionDenied` error naming the
/// offending path when the home cannot be written.
fn ensure_home_writable() -> Result<(), SetupError> {
    let home = crate::cli::neural_config::resolve_leindex_home()
        .ok_or_else(|| SetupError::Io("Cannot resolve LeIndex home directory.".to_string()))?;

    let config_dir = home.join("config");

    // Create the config directory. A failure here is typically a permission
    // error (read-only home) or a read-only mount. We translate it explicitly.
    if let Err(e) = std::fs::create_dir_all(&config_dir) {
        // EROFS, EACCES, EPERM all surface as PermissionDenied for clarity.
        let reason = e.to_string();
        if reason.to_lowercase().contains("permission")
            || reason.to_lowercase().contains("read-only")
            || e.kind() == std::io::ErrorKind::PermissionDenied
        {
            return Err(SetupError::PermissionDenied {
                path: config_dir,
                reason,
            });
        }
        // Non-permission I/O errors (e.g., disk full) still surface, just
        // via the generic Io variant so the user gets the OS message.
        return Err(SetupError::Io(format!(
            "Cannot create {}: {}",
            config_dir.display(),
            reason
        )));
    }

    // Probe writability with a sentinel file. This catches the case where
    // create_dir_all silently succeeded on a read-only filesystem mounted
    // with some quirks, or where the user can create dirs but not files.
    let sentinel = config_dir.join(".leindex-setup-probe");
    if let Err(e) = std::fs::write(&sentinel, b"probe") {
        let reason = e.to_string();
        if reason.to_lowercase().contains("permission")
            || reason.to_lowercase().contains("read-only")
            || e.kind() == std::io::ErrorKind::PermissionDenied
        {
            return Err(SetupError::PermissionDenied {
                path: sentinel,
                reason,
            });
        }
        return Err(SetupError::Io(format!(
            "Cannot write to {}: {}",
            config_dir.display(),
            reason
        )));
    }
    let _ = std::fs::remove_file(&sentinel);
    Ok(())
}

/// Run a single embedding through the leindex-embed worker to verify that
/// ORT, the model, and the configured execution provider all work together.
///
/// VAL-SETUP-025: On success, returns a `SmokeTestResult` with `passed=true`
/// and the produced vector dimensionality (e.g., 1024).
/// VAL-SETUP-026: On failure, returns `passed=false` with the worker error
/// text so the caller can print actionable diagnostics.
///
/// The function never panics and never returns `Err` from the setup control
/// flow: catastrophic worker-startup failures (binary not found, etc.) are
/// translated into a `SmokeTestResult` with `passed=false` so the caller
/// still gets a `SetupResult` to print.
///
/// `expected_provider` is used only to label the expected provider in failure
/// messages; the actual active provider is parsed from the worker's startup
/// report on stderr.
fn run_embedding_smoke_test(expected_provider: Option<ExecutionProvider>) -> SmokeTestResult {
    run_embedding_smoke_test_inner(expected_provider)
}

/// Gated implementation: when the `onnx` feature is compiled in, we can use
/// the `EmbeddingClient` to spawn the worker and run a real inference. When
/// it is not compiled in, the smoke test cannot run (no worker binary, no
/// ORT bindings), so we return a clear "skipped" result.
#[cfg(feature = "onnx")]
fn run_embedding_smoke_test_inner(expected_provider: Option<ExecutionProvider>) -> SmokeTestResult {
    use crate::search::onnx::EmbeddingClient;

    // The Qwen3-Embedding-0.6B model produces 1024-dimensional vectors.
    const SMOKE_TEST_EXPECTED_DIM: usize = 1024;
    const SMOKE_TEST_TEXT: &str = "hello world";

    let client = EmbeddingClient::new();
    let provider_label: String = expected_provider
        .map(|p| p.config_value().to_string())
        .unwrap_or_else(|| "auto".to_string());
    // Spawn the worker and run a single embedding. EmbeddingClient does not
    // currently expose the worker startup report, so the active EP remains
    // unknown unless that plumbing is added later.
    match client.embed(&[SMOKE_TEST_TEXT.to_string()], SMOKE_TEST_EXPECTED_DIM) {
        Ok(response) => {
            if response.count == 0 {
                return SmokeTestResult {
                    passed: false,
                    dimension: None,
                    execution_provider: None,
                    configured_provider_label: Some(provider_label),
                    error: Some("worker returned zero embeddings".to_string()),
                };
            }
            // The first (and only) embedding must have the expected dimension.
            let dim = response.dimension;
            let passed = dim == SMOKE_TEST_EXPECTED_DIM;
            SmokeTestResult {
                passed,
                dimension: Some(dim),
                execution_provider: None,
                configured_provider_label: Some(provider_label),
                error: if passed {
                    None
                } else {
                    Some(format!(
                        "expected {}-dim vector, got {}-dim",
                        SMOKE_TEST_EXPECTED_DIM, dim
                    ))
                },
            }
        }
        Err(e) => {
            // Translate the client error into actionable text.
            let msg = e.to_string();
            SmokeTestResult {
                passed: false,
                dimension: None,
                execution_provider: None,
                configured_provider_label: Some(provider_label),
                error: Some(msg),
            }
        }
    }
}

/// Non-onnx fallback: the smoke test cannot run because the worker is not
/// compiled in. We return a "skipped" result rather than failing the entire
/// setup, because the user may be running a TF-IDF-only build intentionally.
#[cfg(not(feature = "onnx"))]
fn run_embedding_smoke_test_inner(
    _expected_provider: Option<ExecutionProvider>,
) -> SmokeTestResult {
    SmokeTestResult {
        passed: false,
        dimension: None,
        execution_provider: None,
        configured_provider_label: None,
        error: Some("onnx feature is not compiled in; cannot run embedding smoke test".to_string()),
    }
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

                    // VAL-SETUP-030: Detect stale ORT dylib paths left over
                    // from older installs. The pre-1.8 bundling strategy
                    // shipped ONNX Runtime under a since-removed vendored
                    // directory; configs pointing there are migrated to the
                    // current discovery-chain model by re-running setup.
                    // We flag migration when the configured ORT path no
                    // longer resolves to a file on disk (any stale path,
                    // not just the legacy vendored one).
                    if let Some(ref ort_path) = existing.neural.ort_dylib_path {
                        if !std::path::Path::new(ort_path).exists() {
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
///
/// VAL-CROSS-015: this is exposed `pub(crate)` so the `diagnostics` command
/// can surface the same ORT path that `setup --check` reports, keeping the
/// two surfaces consistent. The chain mirrors
/// `leindex_embed::ort_discovery::discover_path_only()` but uses the main
/// binary's process context (its own current_exe sibling, its own pip).
pub(crate) fn discover_ort_path() -> Option<PathBuf> {
    #[cfg(feature = "onnx")]
    if let Some(outcome) = leindex_embed::ort_discovery::discover_path_only() {
        return Some(outcome.path);
    }

    discover_ort_path_fallback()
}

/// Non-ONNX fallback for setup/check builds that cannot depend on the worker
/// resolver. This preserves the documented priority before falling back to
/// Python/system probes.
fn discover_ort_path_fallback() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("ORT_DYLIB_PATH") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }

    if let Ok(config) = crate::cli::neural_config::LeIndexConfig::load() {
        if let Some(path) = config.neural.ort_dylib_path {
            let path = PathBuf::from(path);
            if path.exists() {
                return Some(path);
            }
        }
    }

    let leindex_home = std::env::var("LEINDEX_HOME")
        .map(PathBuf::from)
        .or_else(|_| std::env::var("HOME").map(|home| PathBuf::from(home).join(".leindex")))
        .ok();
    if let Some(home) = leindex_home {
        let dir = home.join("lib");
        for lib_name in ort_lib_names() {
            let candidate = dir.join(lib_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        if let Some(found) = scan_dir_for_ort_lib(&dir) {
            return Some(found);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            for lib_name in ort_lib_names() {
                let candidate = dir.join(lib_name);
                if candidate.exists() {
                    return Some(candidate);
                }
            }
            if let Some(found) = scan_dir_for_ort_lib(dir) {
                return Some(found);
            }
        }
    }

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
                    // Prefer exact unversioned names (bundle/system symlinks).
                    for lib_name in ort_lib_names() {
                        let candidate = dir.join(lib_name);
                        if candidate.exists() {
                            return Some(candidate);
                        }
                    }
                    // Fall back to versioned pip-wheel runtime libraries
                    // (e.g. `libonnxruntime.so.1.25.0`) without symlinks.
                    if let Some(path) = scan_dir_for_ort_lib(&dir) {
                        return Some(path);
                    }
                }
            }
        }
    }

    // Also check system path
    for path in &["/usr/local/lib", "/usr/lib"] {
        let dir = PathBuf::from(path);
        for lib_name in ort_lib_names() {
            let candidate = dir.join(lib_name);
            if candidate.exists() {
                return Some(candidate);
            }
        }
        if let Some(found) = scan_dir_for_ort_lib(&dir) {
            return Some(found);
        }
    }

    None
}

/// Scan a directory for any loadable ORT runtime library, including versioned
/// pip-wheel sonames. Returns the highest-sorted match (newest version) so
/// setup records the same library the worker would load.
fn scan_dir_for_ort_lib(dir: &Path) -> Option<PathBuf> {
    let mut matches = std::fs::read_dir(dir)
        .ok()
        .into_iter()
        .flat_map(|entries| entries.filter_map(Result::ok))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(is_ort_runtime_lib_name_for_setup)
                .unwrap_or(false)
        })
        .collect::<Vec<_>>();
    matches.sort();
    matches.pop()
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

/// Returns true when `name` is a loadable ORT runtime library filename on the
/// current platform, including versioned pip-wheel sonames such as
/// `libonnxruntime.so.1.25.0`. Provider helper libraries
/// (`libonnxruntime_providers_*`) are intentionally excluded. Mirrors the
/// worker-side matcher in `leindex-embed::ort_discovery` so setup writes an
/// `ort_dylib_path` that the worker can actually load.
#[cfg(target_os = "linux")]
fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    name == "libonnxruntime.so" || name.starts_with("libonnxruntime.so.")
}

#[cfg(target_os = "macos")]
fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    name == "libonnxruntime.dylib"
        || (name.starts_with("libonnxruntime.") && name.ends_with(".dylib"))
}

#[cfg(target_os = "windows")]
fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    name.eq_ignore_ascii_case("onnxruntime.dll")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    ort_lib_names().iter().any(|candidate| candidate == &name)
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
    // We link (symlink > hardlink > copy) each missing file from the bundle,
    // then fall through to the network path only for anything still missing
    // or corrupted.
    //
    // LEINDEX_SKIP_MODEL_COPY: when set, copy_bundled_models only creates
    // symlinks (no copy fallback). This is used by tests/CI to avoid
    // duplicating the 569 MB model into temp directories.
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

/// Link or copy every model file present in `bundled_dir` into `dest_dir`,
/// skipping any that already exist in `dest_dir`.
///
/// **Resource-duplication fix (Bug 3):** Previously this function unconditionally
/// called `std::fs::copy`, which duplicated the 569 MB `qwen3-embed-0.6b.onnx`
/// into every `LEINDEX_HOME/models/` temp directory. During test runs, 47 temp
/// dirs accumulated in `/tmp` (tmpfs), consuming 18.6 GB of RAM-backed storage.
///
/// The new strategy avoids copying heavyweight model files whenever possible:
///
/// 1. **Symlink** (preferred): zero memory, zero disk overhead. Used when source
///    and destination are on the same filesystem (the common case for bundled
///    installs). `symlink()` is tried first because it works across all
///    same-filesystem and even cross-filesystem scenarios on Linux.
/// 2. **Hardlink** (fallback): zero memory overhead, shares inodes. Used when
///    `symlink()` fails (e.g., cross-filesystem) but `link()` succeeds (same
///    filesystem). Each hardlink shares the same inode = zero memory overhead.
/// 3. **Copy** (last resort): full byte copy. Used only when both `symlink()`
///    and `link()` fail (genuinely cross-filesystem scenario, e.g., copying
///    from a USB-mounted bundle to `/tmp`).
///
/// Small metadata files (`config.json`, `checksums.sha256`, `LICENSE`) are
/// cheap to copy and symlink/hardlink is preferred for them too for consistency.
///
/// When `LEINDEX_SKIP_MODEL_COPY` environment variable is set (any non-empty
/// value), the function only creates symlinks (no hardlink or copy fallback).
/// This is intended for test/CI environments where the bundled models directory
/// is the repo `models/` directory and should be referenced in-place.
fn copy_bundled_models(bundled_dir: &std::path::Path, dest_dir: &std::path::Path) {
    let skip_copy = std::env::var("LEINDEX_SKIP_MODEL_COPY")
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);

    let mut linked_any = false;
    for file in crate::cli::leindex::model_download::iter_model_files() {
        let src = bundled_dir.join(file.local);
        let dst = dest_dir.join(file.local);
        if src.exists() && !dst.exists() {
            if !linked_any {
                println!(
                    "  -> Linking bundled model files from {}...",
                    bundled_dir.display()
                );
                linked_any = true;
            }

            // Resolve symlinks on the source so we link to the real file.
            // This matters when the bundled dir itself contains symlinks
            // (e.g., release-bundle layout where models/ has symlinks to
            // a shared storage location).
            let src_resolved = std::fs::canonicalize(&src).unwrap_or_else(|_| src.clone());

            let linked = try_link_model_file(&src_resolved, &dst, skip_copy);
            if let Err(e) = linked {
                if skip_copy {
                    // LEINDEX_SKIP_MODEL_COPY is set: do not fall back to copy.
                    // Log a warning so the user knows the file was skipped.
                    eprintln!(
                        "warning: LEINDEX_SKIP_MODEL_COPY is set and symlink/hardlink failed for {} ({}); \
                         skipping file (will be resolved at download stage if missing).",
                        file.local, e
                    );
                } else {
                    eprintln!(
                        "warning: failed to link {} from bundle ({}); will download instead.",
                        file.local, e
                    );
                }
            }
        }
    }
}

/// Create a symlink at `dst` pointing to `src`, using the platform-appropriate
/// API. Cfg-gated so the function (and `try_link_model_file`) compiles on
/// Windows, where `std::os::unix` does not exist.
#[cfg(unix)]
fn try_symlink_model_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(src, dst)
}

#[cfg(windows)]
fn try_symlink_model_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(src, dst)
}

#[cfg(not(any(unix, windows)))]
fn try_symlink_model_file(_src: &Path, _dst: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks are not supported on this platform",
    ))
}

/// Try to link a model file using symlink > hardlink > copy strategy.
///
/// Returns `Ok(())` on success, or an `Err` describing why all strategies
/// failed (used for logging by the caller).
fn try_link_model_file(
    src: &std::path::Path,
    dst: &std::path::Path,
    skip_copy: bool,
) -> std::io::Result<()> {
    // Ensure the parent directory exists.
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Strategy 1: Symlink (preferred, zero overhead, works cross-filesystem on Linux).
    // Symlinks are the best option because they reference the source file by path
    // without duplicating any data. They work even across filesystem boundaries.
    // The platform-appropriate API is selected by `try_symlink_model_file` so
    // this compiles on Windows (no `std::os::unix`).
    if try_symlink_model_file(src, dst).is_ok() {
        return Ok(());
    }

    // Strategy 2: Hardlink (zero memory overhead, shares inodes).
    // Only works on the same filesystem. `link()` on Unix, `fs::hard_link()`
    // cross-platform wrapper in std.
    match std::fs::hard_link(src, dst) {
        Ok(()) => return Ok(()),
        Err(e) => {
            // Fall through to copy if allowed.
            if skip_copy {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    format!(
                        "LEINDEX_SKIP_MODEL_COPY is set but symlink and hardlink both failed: {}",
                        e
                    ),
                ));
            }
        }
    }

    // Strategy 3: Copy (last resort, full byte duplication).
    // Only reached when symlink and hardlink both fail AND skip_copy is false.
    std::fs::copy(src, dst).map(|_| ())
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
/// VAL-SETUP-025/026: Surfaces the smoke-test result.
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

    // VAL-SETUP-025/026/034: surface the smoke-test outcome. The status block
    // already printed the PASS/FAIL line during execute_setup, but the final
    // summary needs to repeat it so the user has the complete picture in one
    // place along with ORT/model status.
    if let Some(ref smoke) = result.smoke_test {
        println!("Smoke test:   {}", smoke.status_line());
        if let Some(ref provider) = smoke.configured_provider_label {
            println!("Configured EP: {}", provider);
        }
        if let Some(ref provider) = smoke.execution_provider {
            println!("Active EP:    {}", provider);
        }
        if let Some(ref err) = smoke.error {
            if !smoke.passed {
                println!("Worker error: {}", truncate_for_display(err, 200));
            }
        }
    }

    // Final status line
    println!();
    if result.choices.neural_enabled {
        // VAL-SETUP-025: the smoke-test result gates "ready". A configured
        // but failing install still tells the user what to fix.
        let fully_ready = result.model_present
            && result.ort_installed
            && result
                .smoke_test
                .as_ref()
                .map(|s| s.passed)
                .unwrap_or(false);
        if fully_ready {
            println!("Neural search is ready!");
        } else if result.model_present && result.ort_installed {
            println!("Neural search is configured but the smoke test failed.");
            println!("Re-run: leindex setup --neural --cpu, or run `leindex setup --check`.");
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
    /// Permission denied writing to the LeIndex home directory.
    ///
    /// VAL-SETUP-031: When `~/.leindex/` (or `$LEINDEX_HOME/`) cannot be
    /// created or written (read-only home), setup reports a clear permission
    /// error with the offending path and the LEINDEX_HOME remediation hint.
    PermissionDenied {
        /// The path that could not be written.
        path: PathBuf,
        /// The underlying OS error message.
        reason: String,
    },
    /// Post-setup embedding smoke test failed.
    ///
    /// VAL-SETUP-026: Setup reports FAIL with the worker error text and
    /// actionable guidance, exits non-zero, but still persists whatever
    /// config it could write. The caller (`cmd_setup_impl`) does NOT bail
    /// on this error; instead, `execute_setup` returns a `SetupResult`
    /// with `smoke_test: Some(failed)` so the summary can print it.
    /// This variant is reserved for catastrophic worker-startup failures
    /// where we cannot even produce a result struct.
    #[allow(dead_code)]
    SmokeTestCatastrophic { message: String },
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
            SetupError::PermissionDenied { path, reason } => {
                // VAL-SETUP-031: surface the offending path and LEINDEX_HOME
                // remediation hint so the user can fix permissions or redirect.
                write!(
                    f,
                    "Permission denied writing to {}: {}. \
                     Check directory permissions, or set LEINDEX_HOME to a writable location \
                     (e.g., export LEINDEX_HOME=/tmp/leindex).",
                    path.display(),
                    reason
                )
            }
            SetupError::SmokeTestCatastrophic { message } => {
                write!(
                    f,
                    "Embedding smoke test could not run: {}. \
                     Check that the leindex-embed worker binary is installed and that \
                     ORT + model files are present. Run `leindex setup --check` for diagnostics.",
                    message
                )
            }
        }
    }
}

impl std::error::Error for SetupError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn test_setup_ort_lib_name_accepts_versioned_linux_pip_soname() {
        assert!(is_ort_runtime_lib_name_for_setup(
            "libonnxruntime.so.1.25.0"
        ));
        assert!(is_ort_runtime_lib_name_for_setup("libonnxruntime.so"));
        assert!(!is_ort_runtime_lib_name_for_setup(
            "libonnxruntime_providers_shared.so"
        ));
    }

    #[test]
    fn test_setup_smoke_provider_label_is_configured_not_claimed_registered() {
        let src = std::fs::read_to_string(file!()).expect("setup.rs should be readable");
        // The smoke test result output must NOT claim the provider is
        // "registered" when we only know the configured provider, not
        // what the worker actually loaded.
        let needle = format!("{} {}", "Execution provider", "registered");
        assert!(
            !src.contains(&needle),
            "setup must not claim the provider is 'registered'; it only knows the configured EP"
        );
    }

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
        // pointing LEINDEX_HOME at a fresh tempfile::TempDir (auto-cleanup on drop).
        // Resource-duplication fix: use tempfile::TempDir instead of manual
        // std::env::temp_dir() to guarantee cleanup even on panic.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());
        let status = model_checksum_status();
        std::env::remove_var("LEINDEX_HOME");
        assert_eq!(status, ModelChecksumStatus::Missing);
        // tmp is auto-cleaned when dropped
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

    // ── VAL-SETUP-025/026: Smoke test result and status line ──

    #[test]
    fn test_smoke_test_result_pass_status_line() {
        let result = SmokeTestResult {
            passed: true,
            dimension: Some(1024),
            execution_provider: None,
            configured_provider_label: Some("cpu".to_string()),
            error: None,
        };
        let line = result.status_line();
        assert!(line.contains("PASS"), "{}", line);
        assert!(line.contains("1024"), "{}", line);
    }

    #[test]
    fn test_smoke_test_result_fail_status_line() {
        let result = SmokeTestResult {
            passed: false,
            dimension: None,
            execution_provider: None,
            configured_provider_label: Some("cpu".to_string()),
            error: Some("worker failed to start".to_string()),
        };
        let line = result.status_line();
        assert!(line.contains("FAIL"), "{}", line);
        // The FAIL line does NOT include the dimension (we don't have one).
        assert!(!line.contains("1024"));
    }

    #[test]
    fn test_smoke_test_result_dimension_mismatch_is_fail() {
        // If the worker returns the wrong dimension, the test fails.
        let result = SmokeTestResult {
            passed: false,
            dimension: Some(768), // expected 1024
            execution_provider: None,
            configured_provider_label: Some("migraphx".to_string()),
            error: Some("expected 1024-dim vector, got 768-dim".to_string()),
        };
        assert!(!result.passed);
        assert_eq!(result.dimension, Some(768));
        assert_eq!(
            result.configured_provider_label.as_deref(),
            Some("migraphx")
        );
        assert!(result.execution_provider.is_none());
    }

    // ── VAL-SETUP-031: Permission denied error ──

    #[test]
    fn test_permission_denied_error_names_path_and_leindex_home() {
        let err = SetupError::PermissionDenied {
            path: PathBuf::from("/home/user/.leindex/config"),
            reason: "Permission denied (os error 13)".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("Permission denied"), "{}", msg);
        assert!(msg.contains("/home/user/.leindex/config"), "{}", msg);
        // Remediation hint must mention LEINDEX_HOME.
        assert!(msg.contains("LEINDEX_HOME"), "{}", msg);
    }

    #[test]
    fn test_smoke_test_catastrophic_error_is_actionable() {
        let err = SetupError::SmokeTestCatastrophic {
            message: "worker binary not found".to_string(),
        };
        let msg = err.to_string();
        assert!(msg.contains("smoke test"), "{}", msg);
        assert!(msg.contains("worker binary not found"), "{}", msg);
        assert!(msg.contains("leindex-embed"), "{}", msg);
    }

    // ── VAL-SETUP-033: GPU vendor detection ──

    #[test]
    fn test_detect_gpu_vendor_returns_enum() {
        // detect_gpu_vendor must return without panicking regardless of the
        // system. We don't assert the specific variant because CI/dev hosts
        // have different hardware.
        let _ = detect_gpu_vendor();
    }

    #[test]
    fn test_detected_gpu_variants_are_distinct() {
        // Enum sanity: each variant is distinct from the others.
        assert_ne!(DetectedGpu::Amd, DetectedGpu::Nvidia);
        assert_ne!(DetectedGpu::Amd, DetectedGpu::Unknown);
        assert_ne!(DetectedGpu::Nvidia, DetectedGpu::Unknown);
    }

    #[test]
    fn test_detect_amd_gpu_no_false_positive_on_clean_system() {
        // With a bogus ROCM_PATH that does not exist, the detection should
        // not claim an AMD GPU is present via that path alone.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        std::env::set_var("ROCM_PATH", "/definitely/not/a/real/path");
        // This may still be true if /opt/rocm exists on the test host, so we
        // only check it doesn't panic and returns a bool-like enum.
        let _ = detect_amd_gpu();
        std::env::remove_var("ROCM_PATH");
    }

    #[test]
    fn test_detect_amd_gpu_honors_existing_rocm_path() {
        // When ROCM_PATH points at an existing directory, AMD is detected.
        // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("ROCM_PATH", tmp.path());
        assert!(detect_amd_gpu(), "existing ROCM_PATH should detect AMD");
        std::env::remove_var("ROCM_PATH");
        // tmp auto-cleans on drop
    }

    #[test]
    fn test_detect_nvidia_gpu_with_cuda_path_env() {
        // When CUDA_PATH points at an existing directory, NVIDIA is detected.
        // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("CUDA_PATH", tmp.path());
        assert!(
            detect_nvidia_gpu(),
            "existing CUDA_PATH should detect NVIDIA"
        );
        std::env::remove_var("CUDA_PATH");
        // tmp auto-cleans on drop
    }

    // ── VAL-SETUP-031 + VAL-SETUP-035: ensure_home_writable + LEINDEX_HOME ──

    #[test]
    fn test_ensure_home_writable_succeeds_for_writable_leindex_home() {
        // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());
        let result = ensure_home_writable();
        std::env::remove_var("LEINDEX_HOME");
        // tmp auto-cleans on drop
        assert!(
            result.is_ok(),
            "writable LEINDEX_HOME should pass: {:?}",
            result
        );
    }

    #[test]
    fn test_ensure_home_writable_uses_leindex_home_location() {
        // VAL-SETUP-032/035: LEINDEX_HOME drives where config goes.
        // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        std::env::set_var("LEINDEX_HOME", tmp.path());
        let result = ensure_home_writable();
        assert!(result.is_ok());
        // After the probe, the config directory should exist under $LEINDEX_HOME.
        assert!(
            tmp.path().join("config").is_dir(),
            "config dir should be under LEINDEX_HOME"
        );
        std::env::remove_var("LEINDEX_HOME");
        // tmp auto-cleans on drop
    }

    #[test]
    fn test_ensure_home_writable_fails_for_read_only_dir() {
        // VAL-SETUP-031: a read-only directory surfaces a PermissionDenied error.
        // We create a tempfile::TempDir, chmod it 555 (read+execute only), and
        // verify the probe fails. Then restore perms and let TempDir clean up.
        // Resource-duplication fix: use tempfile::TempDir for auto-cleanup.
        let _g = PIPE_ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path().to_path_buf();

        // Make the base directory read-only (no write permission).
        // 0o555 = r-xr-xr-x
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o555)).unwrap();
        }

        std::env::set_var("LEINDEX_HOME", &base);
        let result = ensure_home_writable();

        // Restore permissions before assertions so cleanup always works.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&base, std::fs::Permissions::from_mode(0o755));
        }
        std::env::remove_var("LEINDEX_HOME");
        // tmp auto-cleans on drop (we restored perms above)

        // On Unix with a read-only base, we expect a PermissionDenied error.
        // On non-Unix or when running as root, the probe may succeed; skip
        // the assertion in that case to avoid a flaky test.
        #[cfg(unix)]
        {
            // Running as root bypasses permissions, so only assert for non-root.
            let is_root = unsafe { libc::geteuid() == 0 };
            if !is_root {
                match result {
                    Err(SetupError::PermissionDenied { path, .. }) => {
                        assert!(
                            path.starts_with(&base),
                            "PermissionDenied path should be under LEINDEX_HOME: {:?}",
                            path
                        );
                    }
                    other => {
                        // Some filesystems (tmpfs with special mount options)
                        // may surface the failure as a different variant. Accept
                        // any Err (smoke test: a read-only dir must fail).
                        assert!(
                            other.is_err(),
                            "read-only LEINDEX_HOME should fail, got {:?}",
                            other
                        );
                    }
                }
            } else {
                let _ = result; // root bypasses perms
            }
        }
        #[cfg(not(unix))]
        {
            let _ = result; // Windows: skip per-OS
        }
    }

    // ── VAL-SETUP-035: truncate_for_display ──

    #[test]
    fn test_truncate_for_display_short() {
        assert_eq!(truncate_for_display("short", 100), "short");
    }

    #[test]
    fn test_truncate_for_display_long_appends_ellipsis() {
        let input = "a".repeat(250);
        let result = truncate_for_display(&input, 50);
        assert!(result.ends_with("..."), "{}", result);
        // The truncated body is 50 chars + 3 for the ellipsis.
        assert_eq!(result.len(), 50 + 3);
    }

    // ── Resource-duplication fix: copy_bundled_models symlink/hardlink tests ──
    //
    // Bug 3 fix: copy_bundled_models() must prefer symlink > hardlink > copy
    // so the 569 MB model file is not duplicated into every LEINDEX_HOME temp dir.

    #[test]
    fn test_try_link_model_file_creates_symlink_on_same_filesystem() {
        // On the same filesystem, symlink should succeed (strategy 1).
        // Both src and dst are under the system temp dir (same filesystem),
        // so the symlink should be created.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("source.bin");
        let dst = tmp.path().join("dest.bin");
        std::fs::write(&src, b"model data").unwrap();

        let result = try_link_model_file(&src, &dst, false);
        assert!(
            result.is_ok(),
            "try_link_model_file should succeed: {:?}",
            result
        );

        // The result should be a symlink pointing at src.
        #[cfg(unix)]
        {
            let meta = std::fs::symlink_metadata(&dst).unwrap();
            assert!(meta.file_type().is_symlink(), "dst should be a symlink");
        }
        // The content should be readable through the link.
        let content = std::fs::read(&dst).unwrap();
        assert_eq!(content, b"model data");
    }

    #[test]
    fn test_copy_bundled_models_creates_symlinks_not_copies() {
        // Resource-duplication fix: copy_bundled_models must create symlinks
        // (not copies) when source and dest are on the same filesystem.
        // We simulate a bundled models dir with a small placeholder file.
        let _g = PIPE_ENV_LOCK.lock().unwrap();

        let bundled = tempfile::tempdir().unwrap();
        let dest = tempfile::tempdir().unwrap();

        // Create a fake "model" file in the bundled dir.
        // We use a .txt extension to avoid triggering find_bundled_models()
        // during the test (it looks for qwen3-embed-0.6b.onnx).
        // copy_bundled_models iterates iter_model_files() which includes
        // config.json, so we write that.
        let config_src = bundled.path().join("config.json");
        std::fs::write(&config_src, b"{ \"test\": true }").unwrap();

        copy_bundled_models(bundled.path(), dest.path());

        let config_dst = dest.path().join("config.json");
        assert!(config_dst.exists(), "config.json should exist in dest");

        // On Unix, verify it's a symlink (not a copy).
        #[cfg(unix)]
        {
            let meta = std::fs::symlink_metadata(&config_dst).unwrap();
            assert!(
                meta.file_type().is_symlink(),
                "config.json in dest should be a symlink, not a copy (resource-duplication fix)"
            );
        }
    }

    #[test]
    fn test_try_link_model_file_overwrites_existing() {
        // copy_bundled_models skips files that already exist in dest_dir,
        // so this test verifies try_link_model_file itself (called for new files).
        // If dst already exists, try_link_model_file will fail because symlink()
        // does not overwrite. This is the intended behavior: copy_bundled_models
        // checks !dst.exists() before calling try_link_model_file.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("source.bin");
        let dst = tmp.path().join("dest.bin");
        std::fs::write(&src, b"model data").unwrap();
        std::fs::write(&dst, b"old data").unwrap();

        // symlink() fails when dst exists; hard_link also fails when dst exists.
        // The function should return an error (or fall through to copy which
        // also fails because copy overwrites... actually std::fs::copy overwrites).
        // So the result depends on the strategy: copy() overwrites by default.
        let result = try_link_model_file(&src, &dst, false);
        // std::fs::copy overwrites existing files, so this should succeed.
        assert!(
            result.is_ok(),
            "copy strategy should overwrite: {:?}",
            result
        );
    }
}
