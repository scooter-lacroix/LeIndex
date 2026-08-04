//! Captures the §14 environment record: hardware, kernel, allocator env,
//! provider config, git revision, corpus hash, model digests.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvironmentCapture {
    pub kernel: String,
    pub hardware: HardwareInfo,
    pub allocator_env: HashMap<String, String>,
    pub git_revision: String,
    pub corpus_tree_oid: String,
    pub provider_config: HashMap<String, String>,
    pub model_digests: ModelDigests,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct HardwareInfo {
    pub cpu_model: String,
    pub cpu_count: usize,
    pub mem_total_kib: u64,
    pub arch: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ModelDigests {
    pub embed_model: String,
    pub embed_onnx_sha256: Option<String>,
    pub reranker_model: Option<String>,
}

impl EnvironmentCapture {
    pub fn capture(workspace: &Path, fixture: &Path) -> std::io::Result<Self> {
        let kernel = read_kernel_release().unwrap_or_default();
        let hardware = read_hardware_info();
        let allocator_env = read_allocator_env();
        let git_revision = read_git_revision(workspace).unwrap_or_default();
        let corpus_tree_oid = read_corpus_tree_oid(workspace, fixture).unwrap_or_default();
        let provider_config = read_provider_config();
        let model_digests = read_model_digests();

        Ok(Self {
            kernel,
            hardware,
            allocator_env,
            git_revision,
            corpus_tree_oid,
            provider_config,
            model_digests,
        })
    }
}

fn read_kernel_release() -> std::io::Result<String> {
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::uname(&mut buf) };
    if rc != 0 {
        let output = std::process::Command::new("uname").arg("-r").output()?;
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }
    Ok(cstr_from_buf(&buf.release))
}

fn cstr_from_buf(buf: &[libc::c_char]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|&&c| c != 0)
        .map(|&c| c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn read_hardware_info() -> HardwareInfo {
    HardwareInfo {
        cpu_model: read_cpuinfo_model().unwrap_or_default(),
        cpu_count: num_cpus(),
        mem_total_kib: read_meminfo_total().unwrap_or(0),
        arch: read_arch(),
    }
}

fn read_cpuinfo_model() -> std::io::Result<String> {
    let content = std::fs::read_to_string("/proc/cpuinfo")?;
    for line in content.lines() {
        if line.starts_with("model name") {
            if let Some(colon) = line.find(':') {
                return Ok(line[colon + 1..].trim().to_string());
            }
        }
    }
    Ok(String::new())
}

fn num_cpus() -> usize {
    let n = unsafe { libc::sysconf(libc::_SC_NPROCESSORS_ONLN) };
    if n > 0 {
        n as usize
    } else {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    }
}

fn read_meminfo_total() -> std::io::Result<u64> {
    let content = std::fs::read_to_string("/proc/meminfo")?;
    for line in content.lines() {
        if line.starts_with("MemTotal:") {
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(v) = parts[1].parse::<u64>() {
                    return Ok(v);
                }
            }
        }
    }
    Ok(0)
}

fn read_arch() -> String {
    let mut buf: libc::utsname = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::uname(&mut buf) };
    if rc == 0 {
        cstr_from_buf(&buf.machine)
    } else {
        std::env::consts::ARCH.to_string()
    }
}

fn read_allocator_env() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for &key in &[
        "MALLOC_ARENA_MAX",
        "MALLOC_CONF",
        "LD_PRELOAD",
        "LD_LIBRARY_PATH",
    ] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env
}

fn read_provider_config() -> HashMap<String, String> {
    let mut env = HashMap::new();
    for &key in &[
        "LEINDEX_ONNX_INFERENCE_BATCH_SIZE",
        "LEINDEX_ONNX_INFERENCE_SEQUENCE_LENGTH",
        "LEINDEX_WORKER_ORT_THREADS",
        "LEINDEX_WORKER_EXECUTION_PROVIDER",
        "LEINDEX_EMBED_DAEMON",
        "LEINDEX_TOKIO_WORKERS",
    ] {
        if let Ok(val) = std::env::var(key) {
            env.insert(key.to_string(), val);
        }
    }
    env
}

fn read_model_digests() -> ModelDigests {
    ModelDigests {
        embed_model: std::env::var("LEINDEX_MODEL_NAME").unwrap_or_default(),
        embed_onnx_sha256: std::env::var("LEINDEX_MODEL_SHA256").ok(),
        reranker_model: std::env::var("LEINDEX_RERANKER_MODEL").ok(),
    }
}

fn read_git_revision(workspace: &Path) -> std::io::Result<String> {
    let output = std::process::Command::new("git")
        .arg("rev-parse")
        .arg("HEAD")
        .current_dir(workspace)
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn read_corpus_tree_oid(workspace: &Path, fixture: &Path) -> std::io::Result<String> {
    let rel = fixture
        .strip_prefix(workspace)
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|_| Path::new(".").to_path_buf());

    if rel.as_os_str().is_empty() || rel == Path::new(".") {
        let output = std::process::Command::new("git")
            .arg("rev-parse")
            .arg("HEAD^{tree}")
            .current_dir(workspace)
            .output()?;
        return Ok(String::from_utf8_lossy(&output.stdout).trim().to_string());
    }

    let tree_arg = format!("HEAD:{}", rel.display());
    let output = std::process::Command::new("git")
        .arg("rev-parse")
        .arg(&tree_arg)
        .current_dir(workspace)
        .output()?;
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if oid.is_empty() || !output.status.success() {
        let output2 = std::process::Command::new("git")
            .arg("rev-parse")
            .arg("HEAD^{tree}")
            .current_dir(workspace)
            .output()?;
        return Ok(String::from_utf8_lossy(&output2.stdout).trim().to_string());
    }
    Ok(oid)
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_capture_records_kernel_and_git() {
        let tmp = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        // Configure git for the test environment
        std::process::Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::fs::write(tmp.path().join("a.txt"), "x").unwrap();
        std::process::Command::new("git")
            .args(["add", "a.txt"])
            .current_dir(tmp.path())
            .status()
            .unwrap();
        std::process::Command::new("git")
            .args(["commit", "-m", "init"])
            .current_dir(tmp.path())
            .status()
            .unwrap();

        let cap = EnvironmentCapture::capture(tmp.path(), tmp.path()).unwrap();
        assert!(!cap.kernel.is_empty(), "kernel should be non-empty");
        assert!(
            !cap.git_revision.is_empty(),
            "git_revision should be non-empty"
        );
        assert!(cap.hardware.cpu_count > 0, "cpu_count should be positive");
        assert!(
            !cap.hardware.cpu_model.is_empty(),
            "cpu_model should be non-empty"
        );
        assert!(
            cap.hardware.mem_total_kib > 0,
            "mem_total should be positive"
        );
        assert!(!cap.hardware.arch.is_empty(), "arch should be non-empty");
        assert!(
            !cap.corpus_tree_oid.is_empty(),
            "corpus_tree_oid should be non-empty"
        );
    }

    #[test]
    fn test_environment_capture_default() {
        let cap = EnvironmentCapture::default();
        assert!(cap.kernel.is_empty());
        assert_eq!(cap.hardware.cpu_count, 0);
    }

    #[test]
    fn test_hardware_info_serde_roundtrip() {
        let hw = HardwareInfo {
            cpu_model: "Test CPU".to_string(),
            cpu_count: 8,
            mem_total_kib: 16384000,
            arch: "x86_64".to_string(),
        };
        let json = serde_json::to_string(&hw).unwrap();
        let deserialized: HardwareInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, hw);
    }

    #[test]
    fn test_environment_capture_serde_roundtrip() {
        let mut alloc_env = HashMap::new();
        alloc_env.insert("MALLOC_ARENA_MAX".to_string(), "2".to_string());
        let cap = EnvironmentCapture {
            kernel: "5.15.0".to_string(),
            hardware: HardwareInfo {
                cpu_model: "Test".to_string(),
                cpu_count: 4,
                mem_total_kib: 8000000,
                arch: "x86_64".to_string(),
            },
            allocator_env: alloc_env,
            git_revision: "abc123".to_string(),
            corpus_tree_oid: "tree456".to_string(),
            provider_config: HashMap::new(),
            model_digests: ModelDigests {
                embed_model: "qwen3".to_string(),
                embed_onnx_sha256: Some("sha".to_string()),
                reranker_model: None,
            },
        };
        let json = serde_json::to_string(&cap).unwrap();
        let deserialized: EnvironmentCapture = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, cap);
    }

    /// VAL-BASE-009: When MALLOC_ARENA_MAX is set in the environment, the
    /// allocator_env capture includes it with the correct value.
    /// VAL-CONT-001: The allocator_env capture records the setting.
    #[test]
    fn test_allocator_env_captures_malloc_arena_max() {
        // SAFETY: env mutation is process-global, but the normal test runner
        // does not set MALLOC_ARENA_MAX for these unit tests. We save and
        // restore to avoid side-effects.
        let saved = std::env::var("MALLOC_ARENA_MAX").ok();
        // SAFETY: no other thread is reading MALLOC_ARENA_MAX in this test.
        unsafe {
            std::env::set_var("MALLOC_ARENA_MAX", "2");
        }
        let env_map = read_allocator_env();
        assert_eq!(
            env_map.get("MALLOC_ARENA_MAX"),
            Some(&"2".to_string()),
            "allocator_env should contain MALLOC_ARENA_MAX=2"
        );
        // Restore original state.
        // SAFETY: same justification.
        unsafe {
            match &saved {
                Some(v) => std::env::set_var("MALLOC_ARENA_MAX", v),
                None => std::env::remove_var("MALLOC_ARENA_MAX"),
            }
        }
    }

    /// VAL-BASE-009: When MALLOC_ARENA_MAX is unset, allocator_env does not
    /// contain the key (no phantom entries).
    #[test]
    fn test_allocator_env_omits_unset_malloc_arena_max() {
        let saved = std::env::var("MALLOC_ARENA_MAX").ok();
        // SAFETY: no other thread reads this var in this test.
        unsafe {
            std::env::remove_var("MALLOC_ARENA_MAX");
        }
        let env_map = read_allocator_env();
        assert!(
            !env_map.contains_key("MALLOC_ARENA_MAX"),
            "allocator_env should not contain MALLOC_ARENA_MAX when unset"
        );
        // Restore.
        // SAFETY: same justification.
        unsafe {
            if let Some(v) = &saved {
                std::env::set_var("MALLOC_ARENA_MAX", v);
            }
        }
    }
}
