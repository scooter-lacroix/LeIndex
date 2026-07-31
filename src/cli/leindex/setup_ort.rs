use super::*;

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
pub(super) const MIN_ORT_VERSION: (u32, u32, u32) = (1, 20, 0);

/// Maximum supported ONNX Runtime major version. ORT 2.x will introduce ABI
/// breaking changes and is not yet released as of the ort crate 2.0.0-rc.12
/// pinning. We treat 2.0.0+ as "too new" and warn rather than silently accept.
pub(super) const MAX_ORT_MAJOR: u32 = 1;

/// Outcome of comparing a detected version against the supported range.
///
/// VAL-SETUP-022: Setup either upgrades an unsupported version or emits a
/// clear warning naming the detected and required versions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum VersionCompatibility {
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
pub(super) fn parse_version(s: &str) -> Option<(u32, u32, u32)> {
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
pub(super) fn check_ort_version_compatibility(detected: &str) -> VersionCompatibility {
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
pub(super) fn check_provider_available(provider: ExecutionProvider) -> bool {
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
pub(super) fn install_ort(provider: ExecutionProvider) -> Result<(), SetupError> {
    let package = provider.pip_package();
    let package_spec = pip_ort_package_spec(provider);

    println!("Installing {} via pip...", package_spec);

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
        .arg(&package_spec)
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

pub(super) fn pip_ort_package_spec(provider: ExecutionProvider) -> String {
    format!(
        "{}>={}.{}.{},<{}",
        provider.pip_package(),
        MIN_ORT_VERSION.0,
        MIN_ORT_VERSION.1,
        MIN_ORT_VERSION.2,
        MAX_ORT_MAJOR + 1
    )
}

/// Heuristic for detecting pip network/download errors in captured output.
///
/// VAL-SETUP-019's pip-analogue (VAL-SETUP-020 fail path): we want the error
/// message to mention connectivity and a remediation hint rather than just an
/// exit code.
pub(super) fn is_network_error(output: &str) -> bool {
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

pub(super) fn truncate_for_error(s: &str) -> String {
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
pub(super) fn find_pip() -> Option<(String, Vec<String>)> {
    // VAL-SETUP-021: Honor PIP_BIN first so users can point at a non-default pip.
    if let Ok(value) = std::env::var("PIP_BIN") {
        if !value.trim().is_empty() {
            if let Some((program, prefix)) = parse_pip_bin_override(&value) {
                if Command::new(&program)
                    .args(&prefix)
                    .arg("--version")
                    .stdin(std::process::Stdio::null())
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .map(|s| s.success())
                    .unwrap_or(false)
                {
                    return Some((program, prefix));
                }
                // PIP_BIN was set but the binary it points at is broken/missing.
                // Fall through to discovery but log a hint to stderr.
                eprintln!(
                    "warning: PIP_BIN is set to '{}' but invoking it failed; falling back to PATH discovery.",
                    value
                );
            } else {
                eprintln!(
                    "warning: PIP_BIN is set to an unsupported command shape; use /path/to/pip or \"python3 -m pip\". Falling back to PATH discovery."
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

pub(super) fn parse_pip_bin_override(value: &str) -> Option<(String, Vec<String>)> {
    let parts = split_pip_bin_override(value)?;
    let program = parts.first()?.clone();
    let args = &parts[1..];

    if args.is_empty() {
        return Some((program, Vec::new()));
    }

    if args == ["-m", "pip"] {
        return Some((program, vec!["-m".to_string(), "pip".to_string()]));
    }

    None
}

fn split_pip_bin_override(value: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote: Option<char> = None;

    for ch in value.trim().chars() {
        match quote {
            Some(q) if ch == q => quote = None,
            Some(_) => current.push(ch),
            None if ch == '\'' || ch == '"' => quote = Some(ch),
            None if ch.is_whitespace() => {
                if !current.is_empty() {
                    parts.push(std::mem::take(&mut current));
                }
            }
            None if matches!(ch, '|' | '&' | ';' | '<' | '>' | '`' | '\n' | '\r') => return None,
            None => current.push(ch),
        }
    }

    if quote.is_some() {
        return None;
    }
    if !current.is_empty() {
        parts.push(current);
    }
    if parts.is_empty() { None } else { Some(parts) }
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
        .or_else(|_| {
            std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .map(|home| PathBuf::from(home).join(".leindex"))
        })
        .ok();
    if let Some(home) = leindex_home {
        let dir = home.join("lib");
        if let Some(found) = find_ort_lib_in_dir(&dir) {
            return Some(found);
        }
    }

    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            if let Some(found) = find_ort_lib_in_dir(dir) {
                return Some(found);
            }
        }
    }

    // Try to find onnxruntime's capi directory via Python
    if let Some(path) = find_ort_lib_via_python() {
        return Some(path);
    }

    // Also check system path
    for path in &["/usr/local/lib", "/usr/lib"] {
        let dir = PathBuf::from(path);
        if let Some(found) = find_ort_lib_in_dir(&dir) {
            return Some(found);
        }
    }

    None
}

fn find_ort_lib_via_python() -> Option<PathBuf> {
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
                let dir = PathBuf::from(capi_dir);
                if dir.is_dir() {
                    if let Some(path) = find_ort_lib_in_dir(&dir) {
                        return Some(path);
                    }
                }
            }
        }
    }

    None
}

/// Prefer the platform's exact ORT name, then accept a versioned runtime.
pub(super) fn find_ort_lib_in_dir(dir: &Path) -> Option<PathBuf> {
    ort_lib_names()
        .iter()
        .map(|name| dir.join(name))
        .find(|path| path.exists())
        .or_else(|| scan_dir_for_ort_lib(dir))
}

/// Extract a numeric version key from an ORT library filename for sorting.
/// e.g., "libonnxruntime.so.1.20.0" → [1, 20, 0]. Unparseable suffixes yield
/// an empty key (sorts before real versions, never selected as newest).
fn ort_lib_version_key(path: &Path) -> Vec<u64> {
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    let parts: Vec<&str> = name.rsplitn(4, '.').collect();
    parts
        .iter()
        .rev()
        .filter_map(|s| s.parse::<u64>().ok())
        .collect()
}

/// Scan a directory for any loadable ORT runtime library, including versioned
/// pip-wheel sonames. Returns the highest-sorted match (newest version) so
/// setup records the same library the worker would load.
fn scan_dir_for_ort_lib(dir: &Path) -> Option<PathBuf> {
    let mut matches: Vec<PathBuf> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(is_ort_runtime_lib_name_for_setup)
                .unwrap_or(false)
        })
        .collect();
    // Sort by numeric version key, then by path for deterministic tie-breaking.
    matches.sort_by(|a, b| {
        ort_lib_version_key(a)
            .cmp(&ort_lib_version_key(b))
            .then_with(|| a.cmp(b))
    });
    matches.pop()
}

/// Platform-specific ORT library file names.
pub(super) fn ort_lib_names() -> &'static [&'static str] {
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
pub(super) fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    name == "libonnxruntime.so" || name.starts_with("libonnxruntime.so.")
}

#[cfg(target_os = "macos")]
pub(super) fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    name == "libonnxruntime.dylib"
        || (name.starts_with("libonnxruntime.") && name.ends_with(".dylib"))
}

#[cfg(target_os = "windows")]
pub(super) fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    name.eq_ignore_ascii_case("onnxruntime.dll")
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(super) fn is_ort_runtime_lib_name_for_setup(name: &str) -> bool {
    ort_lib_names().iter().any(|candidate| candidate == &name)
}
