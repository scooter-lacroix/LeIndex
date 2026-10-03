//! External SCIP indexer discovery and bounded subprocess execution.

use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::process::CommandExt;

/// A discovered external indexer command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexerSpec {
    /// Language identifier used for discovery.
    pub language: String,
    /// Executable path.
    pub executable: PathBuf,
    /// Fixed arguments before project/output arguments.
    pub args: Vec<OsString>,
}

/// Result of a bounded indexer run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexerRun {
    /// Output path reported by the indexer.
    pub output: PathBuf,
    /// Wall-clock duration.
    pub elapsed: Duration,
}

/// Discover a language-specific indexer using env override then PATH.
pub fn discover_indexer(language: &str) -> Option<IndexerSpec> {
    let normalized = language.to_ascii_uppercase().replace('-', "_");
    let override_name = format!("LEINDEX_SCIP_{normalized}_BIN");
    let executable = env::var_os(&override_name)
        .map(PathBuf::from)
        .or_else(|| default_executable(language).and_then(which))?;
    Some(IndexerSpec {
        language: language.to_ascii_lowercase(),
        executable,
        args: default_args(language),
    })
}

/// Run one indexer with timeout and serialized caller ownership.
///
/// The generic invocation contract is `fixed_args project_root output`. Rust is
/// the one explicit exception: rust-analyzer expects
/// `scip project_root --output output`.
pub fn run_indexer(
    spec: &IndexerSpec,
    project_root: &Path,
    output: &Path,
    timeout: Duration,
) -> std::io::Result<IndexerRun> {
    let _run_guard = indexer_run_lock()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // Do not accept an artifact left by a previous invocation when the current
    // indexer exits successfully without writing the requested output.
    match std::fs::remove_file(output) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let started = Instant::now();
    let mut command = Command::new(&spec.executable);
    append_invocation_args(&mut command, spec, project_root, output);
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        // Never leave a piped stderr unread: a chatty indexer must not block
        // before the timeout watcher can reap it.
        .stderr(Stdio::null());
    configure_process_group(&mut command);
    let mut child = command.spawn()?;

    loop {
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => {
                let _ = terminate_process_group(&mut child);
                return Err(error);
            }
        };
        if let Some(status) = status {
            if !status.success() {
                return Err(std::io::Error::other(format!(
                    "SCIP indexer exited unsuccessfully: {status}"
                )));
            }
            if !output.is_file() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!(
                        "SCIP indexer did not create output file {}",
                        output.display()
                    ),
                ));
            }
            return Ok(IndexerRun {
                output: output.to_path_buf(),
                elapsed: started.elapsed(),
            });
        }

        let elapsed = started.elapsed();
        if elapsed >= timeout {
            let cleanup_error = terminate_process_group(&mut child).err();
            let message = match cleanup_error {
                Some(error) => format!(
                    "SCIP indexer timed out after {timeout:?}; process cleanup failed: {error}"
                ),
                None => format!("SCIP indexer timed out after {timeout:?}"),
            };
            return Err(std::io::Error::new(std::io::ErrorKind::TimedOut, message));
        }

        let remaining = timeout.saturating_sub(elapsed);
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

fn append_invocation_args(
    command: &mut Command,
    spec: &IndexerSpec,
    project_root: &Path,
    output: &Path,
) {
    if spec.language.eq_ignore_ascii_case("rust") {
        command
            .args(&spec.args)
            .arg(project_root)
            .arg("--output")
            .arg(output);
    } else {
        command.args(&spec.args).arg(project_root).arg(output);
    }
}

fn indexer_run_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    #[cfg(not(unix))]
    let _ = command;
}

fn terminate_process_group(child: &mut Child) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let process_group = child.id() as libc::pid_t;
        let kill_result = unsafe { libc::killpg(process_group, libc::SIGKILL) };
        if kill_result == -1 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::NotFound {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
        child.wait().map(|_| ())
    }

    #[cfg(not(unix))]
    {
        child.kill()?;
        child.wait().map(|_| ())
    }
}

fn default_executable(language: &str) -> Option<&'static str> {
    match language.to_ascii_lowercase().as_str() {
        "rust" => Some("rust-analyzer"),
        "typescript" | "javascript" => Some("scip-typescript"),
        "python" => Some("scip-python"),
        "java" | "scala" | "kotlin" => Some("scip-java"),
        "c" | "cpp" => Some("scip-clang"),
        "csharp" => Some("scip-dotnet"),
        "ruby" => Some("scip-ruby"),
        "php" => Some("scip-php"),
        "dart" => Some("scip-dart"),
        _ => None,
    }
}

fn default_args(language: &str) -> Vec<OsString> {
    if language.eq_ignore_ascii_case("rust") {
        vec![OsString::from("scip")]
    } else {
        Vec::new()
    }
}

fn which(executable: &str) -> Option<PathBuf> {
    let executable = PathBuf::from(executable);
    if executable.is_absolute() || executable.components().count() > 1 {
        return executable.is_file().then_some(executable);
    }
    let paths = env::var_os("PATH")?;
    env::split_paths(&paths)
        .map(|dir| dir.join(&executable))
        .find(|candidate| candidate.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unknown_language_falls_back_silently() {
        assert!(discover_indexer("go").is_none());
    }

    #[test]
    fn test_environment_override_is_honored() {
        let _guard = crate::feature_flags::lock_flag_tests();
        let path = PathBuf::from("/tmp/leindex-scip-fixture");
        unsafe { env::set_var("LEINDEX_SCIP_RUST_BIN", &path) };
        let spec = discover_indexer("rust").unwrap();
        assert_eq!(spec.executable, path);
        unsafe { env::remove_var("LEINDEX_SCIP_RUST_BIN") };
    }

    #[cfg(unix)]
    fn shell_fixture(script: &str) -> (tempfile::TempDir, PathBuf) {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("fixture.sh");
        std::fs::write(&executable, format!("#!/bin/sh\n{script}\n")).unwrap();
        let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&executable, permissions).unwrap();
        (directory, executable)
    }

    #[cfg(unix)]
    #[test]
    fn test_rust_indexer_uses_output_flag() {
        let (directory, executable) = shell_fixture(
            r#"
if [ "$1" != "scip" ] || [ ! -d "$2" ] || [ "$3" != "--output" ] || [ -z "$4" ]; then
    exit 41
fi
: > "$4"
"#,
        );
        let project_root = directory.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        let output = directory.path().join("output.scip");
        let spec = IndexerSpec {
            language: "rust".to_owned(),
            executable,
            args: vec![OsString::from("scip")],
        };

        let run = run_indexer(&spec, &project_root, &output, Duration::from_secs(1)).unwrap();
        assert_eq!(run.output, output);
        assert!(output.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn test_nonzero_exit_is_error_even_when_output_exists() {
        let (directory, executable) = shell_fixture(
            r#"
: > "$2"
# Write enough diagnostics to expose an unread stderr pipe.
dd if=/dev/zero bs=65536 count=32 >&2 2>/dev/null
exit 41
"#,
        );
        let project_root = directory.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        let output = directory.path().join("output.scip");
        let spec = IndexerSpec {
            language: "python".to_owned(),
            executable,
            args: Vec::new(),
        };

        let error = run_indexer(&spec, &project_root, &output, Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Other);
        assert!(error.to_string().contains("exited unsuccessfully"));
        assert!(output.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn test_generic_indexer_uses_project_then_output() {
        let (directory, executable) = shell_fixture(": > \"$2\"");
        let project_root = directory.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        let output = directory.path().join("output.scip");
        let spec = IndexerSpec {
            language: "python".to_owned(),
            executable,
            args: Vec::new(),
        };

        let run = run_indexer(&spec, &project_root, &output, Duration::from_secs(1)).unwrap();
        assert_eq!(run.output, output);
        assert!(output.is_file());
    }

    #[cfg(unix)]
    #[test]
    fn test_successful_indexer_without_output_is_error() {
        let (directory, executable) = shell_fixture("exit 0");
        let project_root = directory.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        let output = directory.path().join("output.scip");
        let spec = IndexerSpec {
            language: "python".to_owned(),
            executable,
            args: Vec::new(),
        };

        let error = run_indexer(&spec, &project_root, &output, Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(error.to_string().contains("did not create output file"));
    }

    #[cfg(unix)]
    #[test]
    fn test_stale_output_is_removed_before_successful_run() {
        let (directory, executable) = shell_fixture("exit 0");
        let project_root = directory.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        let output = directory.path().join("output.scip");
        std::fs::write(&output, b"stale output").unwrap();
        let spec = IndexerSpec {
            language: "python".to_owned(),
            executable,
            args: Vec::new(),
        };

        let error = run_indexer(&spec, &project_root, &output, Duration::from_secs(1)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[test]
    fn test_timeout_kills_process_group_descendants() {
        let (directory, executable) = shell_fixture(
            r#"
(sleep 1; touch "$1/descendant-survived") &
while :; do sleep 1; done
"#,
        );
        let project_root = directory.path().join("project");
        std::fs::create_dir(&project_root).unwrap();
        let output = directory.path().join("output.scip");
        let spec = IndexerSpec {
            language: "python".to_owned(),
            executable,
            args: Vec::new(),
        };

        let error =
            run_indexer(&spec, &project_root, &output, Duration::from_millis(50)).unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        std::thread::sleep(Duration::from_millis(1_200));
        assert!(!project_root.join("descendant-survived").exists());
    }
}
