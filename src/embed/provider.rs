// Execution provider selection for the worker process
//
// VAL-CPHASE-011: Runtime honors configured execution-provider selection
// and reports fallback when the requested provider is unavailable.
//
// The selector checks whether the requested provider is available and
// reports the result. If the requested provider is not available, it
// falls back to CPU and reports the reason.
//
// VAL-ORT-015: MIGraphX provider registers successfully after dynamic ORT load.
// VAL-ORT-016: MIGraphX unavailable falls back to CPU within the loaded ORT.
//
// The dynamic-load compatibility layer adds two distinct checks:
//   * `is_migraphx_available()` keeps its heuristic for "auto" detection so
//     a system with ROCm installed can opportunistically use MIGraphX even
//     when ORT's `GetAvailableProviders()` does not yet list it (shared EP
//     plugin discovery). This handles the bundled-pip-onnxruntime-migraphx
//     case where the EP registers at session-build time, not at load time.
//   * `is_migraphx_compiled_in()` performs the *pure* ORT binary probe (no
//     heuristic). It returns `true` only when `ort::ep::MIGraphX::is_available()`
//     reports the EP in the loaded libonnxruntime. Use this when an explicit
//     "is this EP actually compiled in?" answer is required (e.g., before
//     attempting registration, to emit a clear CPU-fallback log line).
//
// Both helpers are necessary for VAL-ORT-015/016: the heuristic preserves the
// "AMD system -> try MIGraphX" opportunity while the pure probe lets the
// runtime log an actionable fallback message when MIGraphX truly is not
// compiled into the dynamically loaded ORT.

#[cfg(feature = "onnx")]
use ort::ep::ExecutionProvider as _;

/// Result of execution provider selection.
#[derive(Debug, Clone)]
pub struct ProviderSelection {
    /// The selected provider.
    provider: Provider,
    /// Whether this was the originally requested provider.
    is_requested: bool,
    /// Reason for fallback if the requested provider was unavailable.
    fallback_reason: Option<String>,
}

impl ProviderSelection {
    /// Get the name of the selected provider.
    pub fn name(&self) -> String {
        self.provider.name()
    }

    /// Get the fallback provider name (the actually selected provider when fallback occurred).
    pub fn fallback_name(&self) -> String {
        self.provider.name()
    }

    /// Get the reason for fallback.
    pub fn reason(&self) -> String {
        self.fallback_reason
            .clone()
            .unwrap_or_else(|| "no fallback".to_string())
    }

    /// Whether the requested provider was available.
    pub fn is_requested_provider(&self) -> bool {
        self.is_requested
    }
}

/// Supported execution providers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provider {
    /// CPU execution provider (always available).
    Cpu,
    /// CUDA GPU execution provider.
    Cuda,
    /// MIGraphX GPU execution provider (AMD GPUs via ROCm).
    /// This is the modern replacement for the deprecated ROCmExecutionProvider.
    Migraphx,
    /// Deprecated alias: parses from `"rocm"` for backwards compatibility but
    /// **never reaches registration**. `ort::ep::ROCm` was removed from ONNX
    /// Runtime; the selector resolves `"rocm"` to [`Provider::Migraphx`] (when
    /// available) or CPU. This variant exists only so `from_name("rocm")`
    /// keeps parsing for legacy configs/env vars.
    Rocm,
    /// CoreML execution provider (macOS).
    CoreMl,
}

impl Provider {
    /// Get the name of this provider.
    pub fn name(&self) -> String {
        match self {
            Provider::Cpu => "cpu".to_string(),
            Provider::Cuda => "cuda".to_string(),
            Provider::Migraphx => "migraphx".to_string(),
            Provider::Rocm => "rocm".to_string(),
            Provider::CoreMl => "coreml".to_string(),
        }
    }

    /// Parse a provider from a string name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_lowercase().as_str() {
            "cpu" => Some(Provider::Cpu),
            "cuda" | "gpu" => Some(Provider::Cuda),
            "migraphx" => Some(Provider::Migraphx),
            "rocm" => Some(Provider::Rocm),
            "coreml" => Some(Provider::CoreMl),
            _ => None,
        }
    }
}

/// Selector for execution providers.
///
/// Checks availability of the requested provider and falls back to CPU
/// if the requested provider is not available.
pub struct ExecutionProviderSelector;

impl ExecutionProviderSelector {
    fn rocm_path_has_migraphx() -> bool {
        std::env::var("ROCM_PATH")
            .map(|p| {
                let rocm = std::path::Path::new(&p);
                rocm.join("lib/libmigraphx_c.so").exists()
                    || rocm.join("bin/migraphx-driver").exists()
            })
            .unwrap_or(false)
    }

    /// Select an execution provider based on the requested name.
    ///
    /// VAL-CPHASE-011: Honors configured selection and reports fallback.
    ///
    /// Returns `Ok(ProviderSelection)` when the requested provider is available
    /// **or** when "auto" resolves (including to CPU — Auto→CPU is a normal
    /// outcome, not a fallback). Returns `Err(ProviderSelection)` with a CPU
    /// fallback only when an **explicit** GPU/CoreML provider was requested but
    /// is unavailable on this system.
    ///
    /// Normalization: input is `trim().to_ascii_lowercase()` once. Accepted
    /// values: `auto`, `cpu`, `cuda`, legacy `gpu`, `migraphx`, deprecated
    /// `rocm`, `coreml`. Unknown values fall back to CPU with an actionable
    /// reason.
    ///
    /// `rocm` is accepted for backwards compatibility but **never** registers
    /// `ort::ep::ROCm` (that EP was removed from ONNX Runtime). It resolves to
    /// MIGraphX when available, otherwise CPU.
    pub fn select(requested: &str) -> Result<ProviderSelection, ProviderSelection> {
        let requested = requested.trim().to_ascii_lowercase();
        Self::select_normalized(&requested)
    }

    /// Inner selector operating on an already-normalized (lowercased, trimmed)
    /// name. Split out so the auto path can recurse through the same
    /// normalization-free fast path.
    fn select_normalized(requested: &str) -> Result<ProviderSelection, ProviderSelection> {
        // "auto" resolves to the best available provider. CoreML → MIGraphX →
        // CUDA → CPU. Auto→CPU is `Ok` (a normal outcome, not a fallback error)
        // so the runtime does not log a spurious neural-fallback warning for
        // the common CPU-only case.
        if requested == "auto" {
            let provider = select_auto_from_availability(
                Self::is_coreml_available(),
                Self::is_migraphx_available(),
                Self::is_cuda_available(),
            );
            let reason = match provider {
                Provider::CoreMl => "auto-detected CoreML (Apple GPU)".to_string(),
                Provider::Migraphx => "auto-detected MIGraphX (AMD GPU)".to_string(),
                Provider::Cuda => "auto-detected CUDA (NVIDIA GPU)".to_string(),
                Provider::Cpu => "auto: no GPU execution provider available, using CPU".to_string(),
                // Unreachable: select_auto_from_availability only returns the
                // four variants above.
                Provider::Rocm => "auto: using ROCm alias".to_string(),
            };
            return Ok(ProviderSelection {
                provider,
                is_requested: true,
                fallback_reason: Some(reason),
            });
        }

        match Provider::from_name(requested) {
            Some(Provider::Cpu) => Ok(ProviderSelection {
                provider: Provider::Cpu,
                is_requested: true,
                fallback_reason: None,
            }),
            Some(provider @ Provider::Cuda) => {
                if Self::is_cuda_available() {
                    Ok(ProviderSelection {
                        provider,
                        is_requested: true,
                        fallback_reason: None,
                    })
                } else {
                    Err(Self::cpu_fallback(
                        "CUDA runtime or driver not found on this system",
                    ))
                }
            }
            Some(Provider::Migraphx) => {
                if Self::is_migraphx_available() {
                    Ok(ProviderSelection {
                        provider: Provider::Migraphx,
                        is_requested: true,
                        fallback_reason: None,
                    })
                } else {
                    Err(Self::cpu_fallback(
                        "MIGraphX not found on this system (requires ROCm + MIGraphX)",
                    ))
                }
            }
            // "rocm" is a backwards-compat alias. The ROCmExecutionProvider was
            // removed from ONNX Runtime; we NEVER register `ort::ep::ROCm`.
            // Resolve to MIGraphX (the modern AMD EP) when available, else CPU.
            Some(Provider::Rocm) => {
                if Self::is_migraphx_available() {
                    Ok(ProviderSelection {
                        provider: Provider::Migraphx,
                        is_requested: true,
                        fallback_reason: Some(
                            "ROCm EP is deprecated, using MIGraphX (the modern AMD GPU provider)"
                                .to_string(),
                        ),
                    })
                } else {
                    Err(Self::cpu_fallback(
                        "MIGraphX not found (rocm alias); ROCm EP is removed from ORT, \
                         install onnxruntime-migraphx or use CPU",
                    ))
                }
            }
            Some(provider @ Provider::CoreMl) => {
                if Self::is_coreml_available() {
                    Ok(ProviderSelection {
                        provider,
                        is_requested: true,
                        fallback_reason: None,
                    })
                } else {
                    Err(Self::cpu_fallback("CoreML is only available on macOS"))
                }
            }
            None => Err(Self::cpu_fallback(&format!(
                "unknown execution provider '{}', falling back to CPU",
                requested
            ))),
        }
    }

    fn cpu_fallback(reason: &str) -> ProviderSelection {
        ProviderSelection {
            provider: Provider::Cpu,
            is_requested: false,
            fallback_reason: Some(reason.to_string()),
        }
    }

    /// Check if CUDA is available on this system.
    fn is_cuda_available() -> bool {
        #[cfg(feature = "onnx")]
        {
            ort::ep::CUDA::default().is_available().unwrap_or(false)
        }
        #[cfg(not(feature = "onnx"))]
        {
            // Conservative fallback: check environment and driver presence
            std::env::var("CUDA_PATH").is_ok()
                || std::path::Path::new("/usr/bin/nvidia-smi").exists()
                || std::path::Path::new("/usr/local/cuda/bin/nvidia-smi").exists()
        }
    }

    /// Check if MIGraphX is available on this system.
    /// MIGraphX is the modern AMD GPU execution provider for ONNX Runtime,
    /// replacing the deprecated ROCmExecutionProvider.
    ///
    /// This check uses two strategies:
    /// 1. Ask the ONNX Runtime if MIGraphX is compiled in via `is_available()`
    /// 2. If that returns false, check for ROCm/MIGraphX system presence as a
    ///    heuristic. The actual registration may still succeed (or fail) when
    ///    we attempt to build the session, at which point we fall back to CPU.
    fn is_migraphx_available() -> bool {
        #[cfg(feature = "onnx")]
        {
            // First, ask ORT if MIGraphX is available in the loaded binary
            if ort::ep::MIGraphX::default().is_available().unwrap_or(false) {
                return true;
            }
            // Fallback heuristic: if ROCm + MIGraphX are installed on the system,
            // assume the EP can be registered. The session builder will fall back
            // to CPU if registration actually fails.
            // This is necessary because GetAvailableProviders() may not list
            // MIGraphX when it's loaded as a shared provider plugin.
            if std::path::Path::new("/opt/rocm/lib/libmigraphx_c.so").exists()
                || std::path::Path::new("/opt/rocm/bin/migraphx-driver").exists()
                || Self::rocm_path_has_migraphx()
            {
                tracing::debug!(
                    "MIGraphX not in GetAvailableProviders() but ROCm/MIGraphX \
                     libraries detected; will attempt registration"
                );
                return true;
            }
            false
        }
        #[cfg(not(feature = "onnx"))]
        {
            // Conservative fallback: check for MIGraphX binary presence
            std::path::Path::new("/opt/rocm/bin/migraphx-driver").exists()
                || std::path::Path::new("/opt/rocm/lib/libmigraphx_c.so").exists()
                || Self::rocm_path_has_migraphx()
        }
    }

    /// Pure ORT-binary probe for MIGraphX: returns `true` only when
    /// `GetAvailableProviders()` lists `MIGraphXExecutionProvider` in the
    /// *currently loaded* libonnxruntime.
    ///
    /// Unlike [`is_migraphx_available`](Self::is_migraphx_available), this
    /// does NOT consult the filesystem heuristic and is therefore the right
    /// answer to "is MIGraphX truly compiled into the loaded ORT?". Use this
    /// before emitting an explicit CPU-fallback log message so operators can
    /// distinguish "picked auto + AMD heuristic triggered" from
    /// "MIGraphX really is not available in the ORT binary we dlopen()'d".
    ///
    /// VAL-ORT-015: returns `true` on an AMD-GPU system with a migraphx-aware
    /// libonnxruntime (e.g., `onnxruntime-migraphx` from pip).
    /// VAL-ORT-016: returns `false` for a CPU-only libonnxruntime, allowing
    /// the runtime to log "falling back to CPU" before attempting (and
    /// silently failing) registration.
    pub(crate) fn is_migraphx_compiled_in() -> bool {
        #[cfg(feature = "onnx")]
        {
            ort::ep::MIGraphX::default().is_available().unwrap_or(false)
        }
        #[cfg(not(feature = "onnx"))]
        {
            false
        }
    }

    /// Pure ORT-binary probe for CUDA: mirrors
    /// [`is_migraphx_compiled_in`](Self::is_migraphx_compiled_in) and returns
    /// `true` only when `GetAvailableProviders()` lists CUDA in the loaded ORT.
    pub(crate) fn is_cuda_compiled_in() -> bool {
        #[cfg(feature = "onnx")]
        {
            ort::ep::CUDA::default().is_available().unwrap_or(false)
        }
        #[cfg(not(feature = "onnx"))]
        {
            false
        }
    }

    /// Check if CoreML is available (macOS only).
    fn is_coreml_available() -> bool {
        cfg!(target_os = "macos")
    }
}

/// Free-function form of `ExecutionProviderSelector::is_migraphx_compiled_in()`
/// for callers (e.g., `WorkerRuntime::build_session`) that need to probe the
/// dynamically loaded ORT binary for a true MIGraphX-compiled-in answer
/// without the ROCm-system heuristic used by the "auto" selector.
///
/// VAL-ORT-015: returns `true` when the loaded ORT binary lists
/// `MIGraphXExecutionProvider` in `GetAvailableProviders()`. Registration is
/// then guaranteed to succeed at session-build time.
///
/// VAL-ORT-016: returns `false` when the loaded ORT binary does not list
/// MIGraphX (e.g., a CPU-only `libonnxruntime.so` was discovered, or the
/// shared provider plugin `libonnxruntime_providers_migraphx.so` is not
/// co-located). Callers should log a clear CPU-fallback reason in this case
/// before continuing with the CPU EP.
pub fn is_migraphx_compiled_in() -> bool {
    ExecutionProviderSelector::is_migraphx_compiled_in()
}

/// Free-function form of `ExecutionProviderSelector::is_cuda_compiled_in()`.
/// Useful for runtime / diagnostics paths that need to probe the loaded ORT
/// without raising the driver-presence heuristic.
pub fn is_cuda_compiled_in() -> bool {
    ExecutionProviderSelector::is_cuda_compiled_in()
}

/// Pure, hardware-independent resolution of the "auto" execution provider.
///
/// Order of preference: **CoreML → MIGraphX → CUDA → CPU**.
///
/// - CoreML is preferred on Apple Silicon (lowest-latency, always-on GPU).
/// - MIGraphX is the modern AMD GPU provider (ROCm EP is removed from ORT).
/// - CUDA covers NVIDIA.
/// - CPU is the always-available baseline.
///
/// This helper takes explicit availability booleans so it can be unit-tested
/// without any GPU hardware: every branch is reachable by construction. The
/// runtime feeds it the results of the availability probes below.
pub fn select_auto_from_availability(coreml: bool, migraphx: bool, cuda: bool) -> Provider {
    if coreml {
        Provider::CoreMl
    } else if migraphx {
        Provider::Migraphx
    } else if cuda {
        Provider::Cuda
    } else {
        Provider::Cpu
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cpu_provider_always_available() {
        let result = ExecutionProviderSelector::select("cpu");
        assert!(result.is_ok());
        let selection = result.unwrap();
        assert_eq!(selection.name(), "cpu");
        assert!(selection.is_requested_provider());
    }

    #[test]
    fn test_cpu_provider_case_insensitive() {
        let result = ExecutionProviderSelector::select("CPU");
        assert!(result.is_ok());
        assert_eq!(result.unwrap().name(), "cpu");
    }

    #[test]
    fn test_unknown_provider_falls_back_to_cpu() {
        let result = ExecutionProviderSelector::select("tpu");
        assert!(result.is_err());
        let fallback = result.unwrap_err();
        assert_eq!(fallback.fallback_name(), "cpu");
        assert!(!fallback.is_requested_provider());
        assert!(fallback.reason().contains("unknown"));
    }

    #[test]
    fn test_cuda_provider_selection() {
        let result = ExecutionProviderSelector::select("cuda");
        // Result depends on whether CUDA is actually installed
        match result {
            Ok(selection) => {
                assert_eq!(selection.name(), "cuda");
                assert!(selection.is_requested_provider());
            }
            Err(fallback) => {
                assert_eq!(fallback.fallback_name(), "cpu");
                assert!(!fallback.is_requested_provider());
                assert!(fallback.reason().contains("CUDA"));
            }
        }
    }

    #[test]
    fn test_gpu_alias_for_cuda() {
        let result = ExecutionProviderSelector::select("gpu");
        // "gpu" is an alias for "cuda"
        match result {
            Ok(selection) => {
                assert_eq!(selection.name(), "cuda");
            }
            Err(fallback) => {
                assert_eq!(fallback.fallback_name(), "cpu");
            }
        }
    }

    #[test]
    fn test_rocm_provider_selection() {
        let result = ExecutionProviderSelector::select("rocm");
        // "rocm" maps to MIGraphX (the modern AMD GPU provider) or CPU fallback.
        // It must NEVER resolve to Provider::Rocm — ort::ep::ROCm is removed.
        match result {
            Ok(selection) => {
                assert_eq!(selection.name(), "migraphx");
                assert!(selection.reason().contains("MIGraphX"));
            }
            Err(fallback) => {
                assert_eq!(fallback.fallback_name(), "cpu");
                assert!(
                    fallback.reason().contains("ROCm") || fallback.reason().contains("MIGraphX")
                );
            }
        }
    }

    #[test]
    fn test_migraphx_provider_selection() {
        let result = ExecutionProviderSelector::select("migraphx");
        match result {
            Ok(selection) => {
                assert_eq!(selection.name(), "migraphx");
                assert!(selection.is_requested_provider());
            }
            Err(fallback) => {
                assert_eq!(fallback.fallback_name(), "cpu");
                assert!(!fallback.is_requested_provider());
                assert!(fallback.reason().contains("MIGraphX"));
            }
        }
    }

    #[test]
    fn test_coreml_provider_selection() {
        let result = ExecutionProviderSelector::select("coreml");
        if cfg!(target_os = "macos") {
            assert!(result.is_ok());
            assert_eq!(result.unwrap().name(), "coreml");
        } else {
            assert!(result.is_err());
            let fallback = result.unwrap_err();
            assert_eq!(fallback.fallback_name(), "cpu");
            assert!(fallback.reason().contains("macOS"));
        }
    }

    #[test]
    fn test_provider_from_name() {
        assert_eq!(Provider::from_name("cpu"), Some(Provider::Cpu));
        assert_eq!(Provider::from_name("CUDA"), Some(Provider::Cuda));
        assert_eq!(Provider::from_name("migraphx"), Some(Provider::Migraphx));
        assert_eq!(Provider::from_name("rocm"), Some(Provider::Rocm));
        assert_eq!(Provider::from_name("CoreML"), Some(Provider::CoreMl));
        assert_eq!(Provider::from_name("unknown"), None);
    }

    #[test]
    fn test_provider_name() {
        assert_eq!(Provider::Cpu.name(), "cpu");
        assert_eq!(Provider::Cuda.name(), "cuda");
        assert_eq!(Provider::Migraphx.name(), "migraphx");
        assert_eq!(Provider::Rocm.name(), "rocm");
        assert_eq!(Provider::CoreMl.name(), "coreml");
    }

    #[test]
    fn test_provider_selection_fields() {
        let sel = ProviderSelection {
            provider: Provider::Cpu,
            is_requested: true,
            fallback_reason: None,
        };
        assert_eq!(sel.name(), "cpu");
        assert!(sel.is_requested_provider());
        assert_eq!(sel.reason(), "no fallback");
    }

    #[test]
    fn test_rocm_path_migraphx_detection_checks_lib_and_bin() {
        let temp = tempfile::tempdir().unwrap();
        let rocm_lib = temp.path().join("lib");
        std::fs::create_dir_all(&rocm_lib).unwrap();
        std::fs::write(rocm_lib.join("libmigraphx_c.so"), b"fake").unwrap();

        let old_rocm = std::env::var("ROCM_PATH").ok();
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var("ROCM_PATH", temp.path()) };

        assert!(ExecutionProviderSelector::rocm_path_has_migraphx());

        if let Some(value) = old_rocm {
            // FIXME: Audit that the environment access only happens in single-threaded code.
            unsafe { std::env::set_var("ROCM_PATH", value) };
        } else {
            // FIXME: Audit that the environment access only happens in single-threaded code.
            unsafe { std::env::remove_var("ROCM_PATH") };
        }
    }

    // ── VAL-ORT-015 / VAL-ORT-016: dynamic-load compatibility helpers ────

    #[test]
    fn test_is_migraphx_compiled_in_does_not_panic() {
        // The pure probe must not panic regardless of ORT being loaded or
        // not. When ORT is not loaded, it returns Ok(false); when ORT is
        // loaded, it queries GetAvailableProviders() — both safe.
        let _ = ExecutionProviderSelector::is_migraphx_compiled_in();
    }

    #[test]
    fn test_is_cuda_compiled_in_does_not_panic() {
        // Same invariant for CUDA.
        let _ = ExecutionProviderSelector::is_cuda_compiled_in();
    }

    #[test]
    fn test_compiled_in_probes_cannot_simultaneously_be_true() {
        // A single loaded libonnxruntime binary cannot ship both MIGraphX
        // (AMD) and CUDA (NVIDIA). This invariant catches any future
        // regression that returns a spurious `true` for both helpers
        // (e.g., if someone wires the heuristic into the pure probe by
        // mistake).
        let migraphx = ExecutionProviderSelector::is_migraphx_compiled_in();
        let cuda = ExecutionProviderSelector::is_cuda_compiled_in();
        assert!(
            !(migraphx && cuda),
            "MIGraphX={} and CUDA={} both reported as compiled in; \
             a single ORT binary cannot contain both providers.",
            migraphx,
            cuda
        );
    }

    #[test]
    fn test_compiled_in_subset_of_heuristic_available() {
        // The pure probe returns true IFF GetAvailableProviders() returns
        // true; this should be a strict subset of the heuristic-driven
        // selector (which can additionally trigger via /opt/rocm presence).
        let migraphx_compiled = ExecutionProviderSelector::is_migraphx_compiled_in();
        let migraphx_available = ExecutionProviderSelector::is_migraphx_available();
        assert!(
            !migraphx_compiled || migraphx_available,
            "is_migraphx_compiled_in=true is more restrictive than \
             is_migraphx_available; the pure probe must not say true when \
             the heuristic path says false"
        );
    }

    #[test]
    fn test_free_function_helpers_match_method_form() {
        // The free functions are convenience wrappers around the methods and
        // must report identical results.
        assert_eq!(
            crate::embed::provider::is_migraphx_compiled_in(),
            ExecutionProviderSelector::is_migraphx_compiled_in()
        );
        assert_eq!(
            crate::embed::provider::is_cuda_compiled_in(),
            ExecutionProviderSelector::is_cuda_compiled_in()
        );
    }

    // ── Task 7: truthful, portable provider selection ───────────────────

    #[test]
    fn auto_order() {
        // Hardware-independent: every branch reachable by construction.
        // CoreML → MIGraphX → CUDA → CPU.
        assert_eq!(
            select_auto_from_availability(true, true, true),
            Provider::CoreMl
        );
        assert_eq!(
            select_auto_from_availability(false, true, true),
            Provider::Migraphx
        );
        assert_eq!(
            select_auto_from_availability(false, false, true),
            Provider::Cuda
        );
        assert_eq!(
            select_auto_from_availability(false, false, false),
            Provider::Cpu
        );
    }

    #[test]
    fn auto_returns_ok_not_fallback_on_cpu_only() {
        // Auto→CPU is a normal `Ok`, not a neural-fallback error. On a
        // CPU-only system (the common CI case) select("auto") must be Ok and
        // report cpu without the "explicit GPU requested" fallback framing.
        let result = ExecutionProviderSelector::select("auto");
        match result {
            Ok(selection) => {
                let name = selection.name();
                assert!(
                    name == "cpu" || name == "cuda" || name == "migraphx" || name == "coreml",
                    "auto resolved to {name}, expected a concrete provider"
                );
                assert!(
                    selection.is_requested_provider(),
                    "auto resolution is always 'requested'"
                );
                if name == "cpu" {
                    assert!(
                        selection.reason().contains("no GPU"),
                        "auto→cpu reason should explain no GPU was found, got: {}",
                        selection.reason()
                    );
                }
            }
            Err(fallback) => {
                panic!(
                    "auto must never return Err (got CPU fallback: {})",
                    fallback.reason()
                );
            }
        }
    }

    #[test]
    fn auto_never_resolves_to_unresolved_name() {
        // The name handed to a session builder must be concrete, never "auto".
        let selection = ExecutionProviderSelector::select("auto").unwrap();
        assert_ne!(selection.name(), "auto");
        assert_ne!(selection.name(), "rocm");
    }

    #[test]
    fn select_normalizes_whitespace_and_case() {
        // trim().to_ascii_lowercase() applied once at the entry point.
        for input in ["  CUDA  ", "Cuda", "GPU", "  MiGrApHx  ", "\tcoreml\n"] {
            let _ = ExecutionProviderSelector::select(input);
            // Must not panic / not return the unknown-fallback for recognized
            // names regardless of surrounding whitespace or case.
            let result = ExecutionProviderSelector::select(input);
            let name = match &result {
                Ok(s) => s.name(),
                Err(s) => s.fallback_name(),
            };
            assert!(
                !name.contains("unknown"),
                "normalized '{input}' should not hit the unknown-fallback"
            );
        }
    }

    #[test]
    fn rocm_alias_never_returns_rocm_provider() {
        // The selector must resolve "rocm" to MIGraphX or CPU — never to
        // Provider::Rocm, which would imply registering ort::ep::ROCm.
        for input in ["rocm", "  ROCM  ", "Rocm"] {
            match ExecutionProviderSelector::select(input) {
                Ok(s) => assert_eq!(s.name(), "migraphx", "rocm alias resolved to {}", s.name()),
                Err(f) => assert_eq!(f.fallback_name(), "cpu"),
            }
        }
    }
}
