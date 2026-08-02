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

#[path = "setup_models.rs"]
mod setup_models;
#[path = "setup_ort.rs"]
mod setup_ort;
use setup_models::*;
pub(crate) use setup_ort::discover_ort_path;
pub use setup_ort::get_ort_version;
use setup_ort::*;

/// Execution provider selected during setup.
///
/// `Auto` is the policy that defers concrete-provider choice to the runtime
/// selector (CoreML → MIGraphX → CUDA → CPU). It is the default for bare
/// `--neural`. The configured value persisted to `leindex.toml` is `"auto"`
/// even when setup installed a concrete package candidate (e.g., onnxruntime-gpu)
/// derived from host detection — the runtime re-resolves on each launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionProvider {
    /// Automatic selection: runtime probes CoreML/MIGraphX/CUDA and falls back to CPU.
    ///
    /// `Auto` has no fixed pip package; callers must resolve a concrete
    /// **install candidate** via [`install_candidate`] before invoking
    /// `pip_package()` / `install_ort()`.
    Auto,
    /// CPU inference (works everywhere).
    Cpu,
    /// NVIDIA CUDA GPU.
    Cuda,
    /// AMD MIGraphX GPU (ROCm).
    Migraphx,
    /// Apple CoreML GPU (macOS only).
    CoreMl,
}

impl ExecutionProvider {
    /// The ORT pip package name for this provider.
    ///
    /// `Auto` maps to the plain `onnxruntime` package, but this value must not
    /// be used to drive an install decision before resolving the install
    /// candidate via [`install_candidate`] — `Auto` may need a GPU package on
    /// an NVIDIA host. Callers that install ORT pass a concrete candidate.
    pub fn pip_package(&self) -> &'static str {
        match self {
            ExecutionProvider::Auto | ExecutionProvider::Cpu | ExecutionProvider::CoreMl => {
                "onnxruntime"
            }
            ExecutionProvider::Cuda => "onnxruntime-gpu",
            ExecutionProvider::Migraphx => "onnxruntime-migraphx",
        }
    }

    /// The config string value for this provider.
    pub fn config_value(&self) -> &'static str {
        match self {
            ExecutionProvider::Auto => "auto",
            ExecutionProvider::Cpu => "cpu",
            ExecutionProvider::Cuda => "cuda",
            ExecutionProvider::Migraphx => "migraphx",
            ExecutionProvider::CoreMl => "coreml",
        }
    }

    /// Whether this provider is a concrete (non-`Auto`) value.
    ///
    /// Install/availability paths require a concrete provider; `Auto` must be
    /// resolved through [`install_candidate`] first. Exercised by the
    /// `install_candidate` unit tests.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn is_concrete(self) -> bool {
        !matches!(self, ExecutionProvider::Auto)
    }
}

/// Resolve the concrete install candidate for a setup provider.
///
/// Explicit providers (`Cpu`/`Cuda`/`Migraphx`/`CoreMl`) resolve to themselves.
/// `Auto` resolves via host/vendor detection:
///
/// - Auto + Apple (macOS) → `CoreMl`
/// - Auto + AMD on supported Linux x86_64 → `Migraphx`
/// - Auto + NVIDIA → `Cuda`
/// - Auto + no usable accelerator → `Cpu`
///
/// The configured policy persisted to `leindex.toml` remains `"auto"`; this
/// helper only decides which pip package / availability probe applies during
/// the install phase. Probe order: host/vendor → install distribution →
/// discover/init dylib → provider availability → real session/inference smoke
/// test. The runtime selector remains the pre-install authority is NOT granted
/// here — this is the install-candidate resolver, not the runtime selector.
pub fn install_candidate(provider: ExecutionProvider) -> ExecutionProvider {
    match provider {
        concrete @ (ExecutionProvider::Cpu
        | ExecutionProvider::Cuda
        | ExecutionProvider::Migraphx
        | ExecutionProvider::CoreMl) => concrete,
        ExecutionProvider::Auto => {
            // Apple → CoreML (lowest-latency, always-on GPU on Apple Silicon).
            if cfg!(target_os = "macos") {
                ExecutionProvider::CoreMl
            } else if cfg!(all(target_os = "linux", target_arch = "x86_64")) && detect_amd_gpu() {
                ExecutionProvider::Migraphx
            } else if detect_nvidia_gpu() {
                ExecutionProvider::Cuda
            } else {
                ExecutionProvider::Cpu
            }
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

/// Runtime state prepared for a neural setup.
#[derive(Debug)]
struct NeuralRuntimeState {
    ort_dylib_path: Option<PathBuf>,
    ort_version: Option<String>,
    ort_installed: bool,
    model_present: bool,
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
    /// Whether the smoke test was skipped (e.g., compiled without `onnx`).
    pub skipped: bool,
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
    /// Human-readable note for skipped or special-case results.
    pub note: Option<String>,
}

const QWEN3_EMBEDDING_DIMENSION: usize = 1024;

impl SmokeTestResult {
    #[cfg_attr(not(feature = "onnx"), allow(dead_code))]
    fn from_embedding_outcome(
        dimension: usize,
        execution_provider: Option<String>,
        configured_provider_label: Option<String>,
    ) -> Self {
        let mut error = if dimension == QWEN3_EMBEDDING_DIMENSION {
            None
        } else {
            Some(format!(
                "expected {}-dim vector, got {}-dim",
                QWEN3_EMBEDDING_DIMENSION, dimension
            ))
        };

        if error.is_none() {
            if let Some(configured) = configured_provider_label.as_deref() {
                let active = execution_provider.as_deref();
                // VAL-SETUP smoke match: the configured label must agree with
                // the provider the worker actually loaded.
                //   cpu→cpu, cuda→cuda, migraphx→migraphx, legacy rocm→migraphx,
                //   coreml→coreml, auto→any concrete provider (incl. CPU).
                let provider_matches = matches!(
                    (configured, active),
                    ("cpu", Some("cpu"))
                        | ("cuda", Some("cuda"))
                        | ("migraphx", Some("migraphx"))
                        | ("rocm", Some("migraphx" | "rocm"))
                        | ("coreml", Some("coreml"))
                        | ("auto", Some("cpu" | "cuda" | "migraphx" | "coreml"))
                );
                // Only flag a mismatch for labels that assert a concrete shape.
                // `cpu` is permissive (CPU is always a valid outcome), so it is
                // excluded from the needs-match set just like the original.
                let needs_match =
                    matches!(configured, "migraphx" | "rocm" | "cuda" | "coreml" | "auto");
                if needs_match && !provider_matches {
                    error = Some(format!(
                        "configured execution provider {} but worker reported {}",
                        configured,
                        active.unwrap_or("none")
                    ));
                }
            }
        }

        Self {
            passed: error.is_none(),
            skipped: false,
            dimension: Some(dimension),
            execution_provider,
            configured_provider_label,
            error,
            note: None,
        }
    }

    /// One-line status string for terminal output.
    pub fn status_line(&self) -> String {
        if self.skipped {
            return "embedding test: SKIP".to_string();
        }
        if self.passed {
            match self.dimension {
                Some(dim) => format!("embedding test: PASS ({}-dim vector)", dim),
                None => "embedding test: PASS (dimension unavailable)".to_string(),
            }
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
        // VAL-SETUP-009: --neural with no provider flags becomes Auto. The
        // concrete install candidate is resolved from host detection during
        // execute_setup; the persisted config value stays "auto" so the runtime
        // re-resolves on each launch and survives hardware changes.
        Some(ExecutionProvider::Auto)
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

    // VAL-SETUP-004/005: provider menu. Auto is always offered and is the
    // default (mirrors bare `--neural`). The remaining options are gated by
    // host so we never offer an impossible combination:
    //   * Apple (macOS) → Auto / CoreML / CPU
    //   * Linux x86_64  → Auto / CPU / CUDA / MIGraphX
    //   * other hosts   → Auto / CPU / CUDA
    let detected_vendor = detect_gpu_vendor();

    // Build the menu. Each entry pairs a display label with the provider it
    // resolves to so the routing table below stays a single source of truth.
    let menu: Vec<(&'static str, ExecutionProvider)> = if cfg!(target_os = "macos") {
        vec![
            (
                "Auto (recommended; uses CoreML on Apple Silicon)",
                ExecutionProvider::Auto,
            ),
            ("CoreML (Apple GPU)", ExecutionProvider::CoreMl),
            ("CPU (works everywhere)", ExecutionProvider::Cpu),
        ]
    } else {
        let mut entries: Vec<(&'static str, ExecutionProvider)> = vec![
            (
                "Auto (recommended; detects CUDA/MIGraphX at runtime)",
                ExecutionProvider::Auto,
            ),
            ("CPU (works everywhere)", ExecutionProvider::Cpu),
            ("NVIDIA CUDA", ExecutionProvider::Cuda),
        ];
        // MIGraphX is only offered on supported platforms (Linux x86_64).
        if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
            entries.push(("AMD MIGraphX (ROCm)", ExecutionProvider::Migraphx));
        }
        entries
    };

    // VAL-SETUP-033: print best-effort detection guidance before the prompt.
    match detected_vendor {
        DetectedGpu::Amd => println!("  (Detected AMD GPU / ROCm tooling.)"),
        DetectedGpu::Nvidia => println!("  (Detected NVIDIA GPU / CUDA tooling.)"),
        DetectedGpu::Unknown => {
            if !cfg!(target_os = "macos") {
                println!("  (No AMD ROCm or NVIDIA CUDA tooling detected.)");
                println!("   Recommendation: choose 'Auto' or 'CPU'.");
            }
        }
    }

    let labels: Vec<&'static str> = menu.iter().map(|(label, _)| *label).collect();
    let default_idx = 0; // Auto is the default.

    let choice = Select::new()
        .with_prompt("Which execution provider?")
        .items(&labels)
        .default(default_idx)
        .interact()
        .map_err(|e| SetupError::Interactive(e.to_string()))?;

    let provider = menu[choice].1;

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

/// Index into the (removed) vendor menu for a detected GPU. Retained for the
/// detection-mapping test; the interactive flow now builds a host-gated menu.
#[cfg(test)]
fn default_gpu_vendor_index(detected: DetectedGpu) -> usize {
    match detected {
        DetectedGpu::Amd => 0,
        DetectedGpu::Nvidia => 1,
        DetectedGpu::Unknown => 2,
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

    let desired_model_name = model_name_for_provider(choices.provider);
    let NeuralRuntimeState {
        ort_dylib_path,
        ort_version,
        ort_installed,
        model_present,
    } = prepare_neural_runtime(choices, desired_model_name)?;

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
    let smoke_test = if choices.neural_enabled && ort_installed && model_present {
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
                if let Some(active) = &result.execution_provider {
                    println!("  -> Active execution provider: {}.", active);
                }
                let _ = dim; // already in status_line
            }
            SmokeTestResult {
                skipped: true,
                note: Some(note),
                ..
            } => {
                println!("  -> {}.", result.status_line());
                println!("     {}", truncate_for_display(note, 200));
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
        if !ort_installed {
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
        ort_installed,
        smoke_test,
    })
}

fn prepare_neural_runtime(
    choices: &SetupChoices,
    model_name: &str,
) -> Result<NeuralRuntimeState, SetupError> {
    let pre_existing_version = get_ort_version();
    let initial_ort_installed = pre_existing_version.is_some();
    let model_present = check_model_present_for_name(model_name);

    if !choices.neural_enabled {
        return Ok(NeuralRuntimeState {
            ort_dylib_path: None,
            ort_version: None,
            ort_installed: initial_ort_installed,
            model_present,
        });
    }

    // VAL-SETUP-027/028: Surface partial-setup edge cases with explicit log
    // lines so the user knows setup detected the partial state and is only
    // doing the missing half. Without these lines a user who pre-staged
    // (e.g.) the model but not ORT would see setup silently install just ORT
    // and wonder whether the model step was skipped incorrectly.
    match (initial_ort_installed, model_present) {
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

    // Resolve the concrete install candidate. Auto must NOT reach install_ort /
    // pip_package directly: it has no fixed package. The candidate is derived
    // from host detection (Auto+Apple→CoreML, Auto+AMD→MIGraphX, Auto+NVIDIA→
    // CUDA, Auto+none→CPU). The persisted config_value remains "auto" via
    // build_config which reads choices.provider (the policy), not this candidate.
    let configured = choices.provider.unwrap_or(ExecutionProvider::Cpu);
    let provider = install_candidate(configured);

    // VAL-SETUP-022: Check version compatibility of any existing install
    // before deciding whether to (re)install. An incompatible version
    // triggers either an upgrade (when too old) or a clear warning (when
    // too new). This must run before install so we don't silently proceed
    // with a known-bad version. `pre_existing_version` was already computed
    // above via `get_ort_version()`.
    let mut upgrade_unsupported_ort = false;
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
                upgrade_unsupported_ort = true;
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
                println!(
                    "     Setup will continue, but if you hit ABI errors, pin onnxruntime to <= {}.",
                    supported_max
                );
                // We continue: too-new may still work, just warn.
            }
            VersionCompatibility::Supported => {
                println!("  -> onnxruntime {} detected (compatible).", detected);
            }
        }
    }

    // VAL-SETUP-006/007/008/010/011/012: Install/maintain ORT for the provider
    if !initial_ort_installed || upgrade_unsupported_ort {
        // No ORT at all, or an installed ORT is too old for the worker API:
        // install/upgrade the appropriate package.
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

    // Discover ORT dylib path and version after (potential) install.
    let ort_dylib_path = discover_ort_path();
    let post_install_version = get_ort_version();
    let ort_installed = post_install_version.is_some();
    let ort_version = post_install_version.or(pre_existing_version);

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
    let model_present = ensure_models_present(choices.provider, model_name)?;

    Ok(NeuralRuntimeState {
        ort_dylib_path,
        ort_version,
        ort_installed,
        model_present,
    })
}

#[cfg(test)]
fn should_install_ort_for_existing_state(
    ort_installed: bool,
    provider: ExecutionProvider,
    pre_existing_version: Option<&str>,
    provider_available: bool,
) -> bool {
    if !ort_installed {
        return true;
    }
    if pre_existing_version
        .map(|version| {
            matches!(
                check_ort_version_compatibility(version),
                VersionCompatibility::Unsupported { .. }
            )
        })
        .unwrap_or(false)
    {
        return true;
    }
    if provider != ExecutionProvider::Cpu && !provider_available {
        return true;
    }
    provider == ExecutionProvider::Cpu && pre_existing_version.is_none()
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
    let home = crate::config::resolve_leindex_home()
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

    const SMOKE_TEST_TEXT: &str = "hello world";

    let client = EmbeddingClient::new_pipe();
    let provider_label: String = expected_provider
        .map(|p| p.config_value().to_string())
        .unwrap_or_else(|| "auto".to_string());
    match client.embed(&[SMOKE_TEST_TEXT.to_string()], QWEN3_EMBEDDING_DIMENSION) {
        Ok(response) => {
            let active_provider =
                client.wait_for_active_execution_provider(std::time::Duration::from_millis(500));
            if response.count == 0 {
                return SmokeTestResult {
                    passed: false,
                    skipped: false,
                    dimension: None,
                    execution_provider: active_provider,
                    configured_provider_label: Some(provider_label),
                    error: Some("worker returned zero embeddings".to_string()),
                    note: None,
                };
            }
            SmokeTestResult::from_embedding_outcome(
                response.dimension,
                active_provider,
                Some(provider_label),
            )
        }
        Err(e) => {
            // Translate the client error into actionable text.
            let msg = e.to_string();
            let active_provider = client.active_execution_provider();
            SmokeTestResult {
                passed: false,
                skipped: false,
                dimension: None,
                execution_provider: active_provider,
                configured_provider_label: Some(provider_label),
                error: Some(msg),
                note: None,
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
        passed: true, // Don't fail setup: binary works for TF-IDF, ORT loaded at runtime
        skipped: true,
        dimension: None,
        execution_provider: None,
        configured_provider_label: None,
        error: None,
        note: Some(
            "Binary compiled without --features onnx; smoke test skipped. \
             ORT and models are loaded at runtime by the leindex-embed worker. \
             To verify neural search: leindex search \"test\" --project ."
                .to_string(),
        ),
    }
}

/// Merge the new config with any existing config, preserving user settings where
/// reasonable and migrating stale schemas.
///
/// VAL-SETUP-024: Idempotent - re-running produces equivalent config
/// VAL-SETUP-029: Corrupted config recovered gracefully
/// VAL-SETUP-030: Stale config migrated
fn merge_with_existing(
    mut new_config: crate::config::LeIndexConfig,
) -> (crate::config::LeIndexConfig, Option<RecoveryNotice>) {
    // Try to load existing config with recovery
    match crate::config::LeIndexConfig::load_or_recover() {
        Ok((existing, action)) => {
            let notice = match action {
                crate::config::RecoveryAction::RecoveredFromCorrupt(backup) => {
                    Some(RecoveryNotice::RecoveredFromCorrupt(backup))
                }
                crate::config::RecoveryAction::Loaded => {
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
                crate::config::RecoveryAction::CreatedDefault => None,
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
) -> crate::config::LeIndexConfig {
    use crate::config::{IndexingConfig, NeuralConfig, SearchConfig};

    let provider_str = choices.provider.map(|p| p.config_value()).unwrap_or("auto");

    crate::config::LeIndexConfig {
        neural: NeuralConfig {
            enabled: choices.neural_enabled,
            execution_provider: provider_str.to_string(),
            ort_dylib_path: ort_dylib_path.map(|p| p.display().to_string()),
            ort_version: ort_version.map(|s| s.to_string()),
            model_dir: crate::config::model_dir_path()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "~/.leindex/models".to_string()),
            model_name: model_name_for_provider(choices.provider).to_string(),
        },
        search: SearchConfig::default(),
        indexing: IndexingConfig::default(),
    }
}

fn print_check_ort_status(
    ort_installed: bool,
    ort_version: Option<&str>,
    ort_path: Option<&Path>,
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
        if search.rerank_enabled { "ON" } else { "OFF" }
    );
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

/// Print a status report without modifying anything.
///
/// VAL-SETUP-014: --check mode reads config and reports status
/// VAL-SETUP-020: Reports the ORT version (from config + live detection)
/// VAL-SETUP-034: Surfaces full configuration
pub fn run_check() -> Result<CheckResult, SetupError> {
    let (config, action) = crate::config::LeIndexConfig::load_or_recover()
        .map_err(|e| SetupError::ConfigRead(e.to_string()))?;

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

    // Recovery notice
    if let crate::config::RecoveryAction::RecoveredFromCorrupt(ref backup) = action {
        println!();
        println!(
            "WARNING: Previous config was corrupted. Backed up to: {}",
            backup.display()
        );
    }

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

/// Result of --check mode.
#[derive(Debug, Clone)]

pub struct CheckResult {
    /// Whether all components are ready for neural search.
    pub fully_configured: bool,
}

/// Pre-compile MIGraphX kernels by spawning the embed worker and sending a
/// warmup inference request.
///
/// VAL-DAEMON-007: This follows the 5-step lifecycle:
/// 1. Spawn worker
/// 2. Wait for Ready
/// 3. Send embed request (dummy text)
/// 4. Wait for response
/// 5. Graceful shutdown
///
/// The MIGraphX compilation is triggered by the inference request, and the
/// compiled kernels are cached in the `ORT_MIGraphX_MODEL_CACHE_PATH`
/// directory. Subsequent index runs skip the compilation step entirely.
#[cfg(feature = "onnx")]
pub fn run_warmup() -> Result<(), SetupError> {
    use crate::search::onnx::EmbeddingClient;

    // Pipe mode (not the persistent socket daemon): the MIGraphX cold JIT compile
    // happens during this single warmup inference (~300 s) and `send_and_receive`
    // blocks on `rx.recv()` with NO timeout, so the compile completes and
    // `with_save_model` persists the `.mxr`. The daemon path cannot do this — its
    // init compile is killed by the 120s readiness gate. After this pre-seeds the
    // cache, `leindex index` daemons `with_load_model` the `.mxr` and start fast.
    run_warmup_inner(&EmbeddingClient::new_pipe())
}

/// Inner warmup logic, separated for testability.
///
/// VAL-DAEMON-007: This follows the 5-step lifecycle:
/// 1. Spawn worker
/// 2. Wait for Ready
/// 3. Send embed request (dummy text)
/// 4. Wait for response
/// 5. Graceful shutdown
///
/// The MIGraphX compilation is triggered by the inference request, and the
/// compiled kernels are cached in the `ORT_MIGraphX_MODEL_CACHE_PATH`
/// directory. Subsequent index runs skip the compilation step entirely.
#[cfg(feature = "onnx")]
fn run_warmup_inner(client: &crate::search::onnx::EmbeddingClient) -> Result<(), SetupError> {
    println!("  -> Starting MIGraphX warmup pre-compilation...");

    // T4: prune stale MIGraphX cache profile dirs (left by a prior package
    // version, batch size, or sequence length) so the cache tree holds at most
    // the one live profile. The worker prunes `.mxr` files within the active
    // profile on startup; this cleans the sibling profile dirs themselves.
    if let Some(model) = client.configured_model_name() {
        let removed = crate::search::onnx::prune_stale_migraphx_profiles(&model);
        if removed > 0 {
            println!("  -> Pruned {removed} stale MIGraphX cache profile(s)");
        }
    }

    // Step 1-2: Ensure worker is spawned and ready.
    // Use embed() which internally calls ensure_worker_ready().
    // Step 3-4: Send a dummy embed request to trigger MIGraphX compilation.
    const WARMUP_TEXT: &str = "warmup compilation text";
    let result = client.embed(&[WARMUP_TEXT.to_string()], 1024);

    // Step 5: Graceful shutdown.
    client.kill_worker();

    match result {
        Ok(response) => {
            println!(
                "  -> MIGraphX warm compilation complete (dim={}, count={})",
                response.dimension, response.count
            );
            tracing::info!("MIGraphX warm compilation complete");
            Ok(())
        }
        Err(e) => {
            let msg = e.to_string();
            // Check if the worker reported a provider. If it's CPU, the
            // warmup is still "successful" in the sense that the worker
            // ran - MIGraphX just wasn't available.
            let provider = client.active_execution_provider();
            if provider.as_deref() == Some("cpu") {
                println!("  -> MIGraphX warmup skipped: worker fell back to CPU provider");
                return Ok(());
            }
            Err(SetupError::Io(format!("MIGraphX warmup failed: {}", msg)))
        }
    }
}

fn print_smoke_summary(smoke: &SmokeTestResult) {
    println!("Smoke test:   {}", smoke.status_line());
    if let Some(ref provider) = smoke.configured_provider_label {
        println!("Configured EP: {}", provider);
    }
    if let Some(ref provider) = smoke.execution_provider {
        println!("Active EP:    {}", provider);
    }
    if smoke.skipped {
        if let Some(ref note) = smoke.note {
            println!("Note:         {}", truncate_for_display(note, 200));
        }
    } else if let Some(ref err) = smoke.error {
        if !smoke.passed {
            println!("Worker error: {}", truncate_for_display(err, 200));
        }
    }
}

fn print_summary_ready_status(result: &SetupResult) {
    if result.choices.neural_enabled {
        let fully_ready = result.model_present
            && result.ort_installed
            && result
                .smoke_test
                .as_ref()
                .map(|smoke| smoke.passed)
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
        print_smoke_summary(smoke);
    }

    // Final status line
    println!();
    print_summary_ready_status(result);
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
    /// The configured model variant cannot be found or prepared.
    ModelUnavailable {
        model_name: String,
        model_dir: PathBuf,
    },
    /// The configured model name is not one of the supported profiles.
    InvalidModelName {
        model_name: String,
        accepted_names: String,
    },
    /// Hugging Face CLI is required for model installation.
    HuggingFaceCliNotFound,
    /// Hugging Face CLI failed to download the selected model repository.
    HuggingFaceDownloadFailed {
        repository: String,
        exit_code: i32,
        output: String,
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
}

impl std::fmt::Display for SetupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SetupError::Conflict { message } => write!(f, "{}", message),
            SetupError::NoFlags => {
                write!(
                    f,
                    "No setup options specified. Use --neural, --no-neural, --cpu, --gpu, or --check. Run 'leindex setup --help' for details."
                )
            }
            SetupError::Interactive(msg) => {
                write!(
                    f,
                    "Interactive prompt failed: {}. If running in a non-interactive context, use flags like --neural --cpu.",
                    msg
                )
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
            SetupError::ModelUnavailable {
                model_name,
                model_dir,
            } => {
                write!(
                    f,
                    "Model '{}' is not available in {}. \
                     LeIndex requires a validated dynamic Qwen3 ONNX model plus \
                     tokenizer.json and config.json. Re-run `leindex setup`, or set \
                     LEINDEX_MODEL_PATH to a directory containing '{}.onnx' and the metadata files.",
                    model_name,
                    model_dir.display(),
                    model_name
                )
            }
            SetupError::InvalidModelName {
                model_name,
                accepted_names,
            } => write!(
                f,
                "Unsupported model name '{}'. Accepted model names are {}.",
                model_name, accepted_names
            ),
            SetupError::HuggingFaceCliNotFound => {
                write!(
                    f,
                    "Hugging Face CLI was not found. Install it with \
                     `python3 -m pip install --upgrade huggingface_hub`, ensure `hf` \
                     or `huggingface-cli` is on PATH, then rerun `leindex setup`. \
                     Set HF_BIN to use a specific executable."
                )
            }
            SetupError::HuggingFaceDownloadFailed {
                repository,
                exit_code,
                output,
            } => {
                write!(
                    f,
                    "Hugging Face CLI failed to download '{}' (exit code {}). \
                     Check network access, authentication, and available disk space, \
                     then rerun `leindex setup`. Output:\n{}",
                    repository, exit_code, output
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
        }
    }
}

impl std::error::Error for SetupError {}

#[cfg(test)]
#[path = "setup_test.rs"]
mod tests;
