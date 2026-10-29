//! GPU-vendor filesystem probes for the setup wizard.
//!
//! Moved out of `setup.rs` by code motion (Large File Detection gate). Kept
//! separate from `DetectedGpu` / `default_gpu_vendor_index`, which stay in the
//! parent because `setup_test.rs` names them directly.

/// Check for AMD GPU presence (ROCm / MIGraphX).
pub(crate) fn detect_amd_gpu() -> bool {
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
    if let Ok(rocm_path) = std::env::var("ROCM_PATH")
        && std::path::Path::new(&rocm_path).exists()
    {
        return true;
    }
    false
}

/// Check for NVIDIA GPU presence (CUDA toolkit / driver).
pub(crate) fn detect_nvidia_gpu() -> bool {
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
