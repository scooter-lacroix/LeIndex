//! Process-environment, liveness and ORT-preflight helpers for the worker client.
//!
//! Extracted from `client.rs` by pure code motion (Large File Detection gate:
//! `client.rs` crossed 2000 lines). None of these touch `EmbeddingClient`
//! state; `client.rs` glob-imports them so every call site keeps its path.
//! The one non-motion change: the "Classify a `std::io::Error`" doc comment had
//! been left stacked on top of `migraphx_loader_dirs`; it now sits on the
//! function it describes.

use super::*;

/// ROCm/MIGraphX library directories that must be on the worker's loader
/// path for the MIGraphX execution provider to load.
///
/// ORT's `libonnxruntime_providers_migraphx.so` links against
/// `libmigraphx*.so` variant libraries that ROCm installs under
/// `<rocm>/lib/migraphx/lib` — a directory the dynamic loader does not
/// search by default. When the worker is spawned without that directory on
/// `LD_LIBRARY_PATH`, provider registration fails with
/// "libmigraphx_tf.so.<ver>: cannot open shared object file" and the worker
/// silently falls back to CPU inference (100-1000x slower than GPU).
/// Returning the existing dirs here lets the spawn path prepend them so
/// provider loading matches a working shell setup. The MIGraphX compile
/// probe child inherits the worker environment and benefits identically.
pub(super) fn migraphx_loader_dirs() -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = Vec::new();
    for key in ["ROCM_PATH", "HIP_PATH"] {
        if let Ok(value) = std::env::var(key) {
            if !value.is_empty() {
                let root = PathBuf::from(value);
                if !roots.contains(&root) {
                    roots.push(root);
                }
            }
        }
    }
    let default_rocm = PathBuf::from("/opt/rocm");
    if !roots.contains(&default_rocm) {
        roots.push(default_rocm);
    }

    let mut dirs: Vec<PathBuf> = Vec::new();
    for root in roots {
        for sub in ["lib/migraphx/lib", "lib"] {
            let dir = root.join(sub);
            if dir.is_dir() && !dirs.contains(&dir) {
                dirs.push(dir);
            }
        }
    }
    dirs
}

/// Classify a `std::io::Error` as one that indicates the worker process has
/// died (closed the connection or crashed).
///
/// VAL-DEADWORKER-001: When the worker process dies, `read_exact` on the
/// `UnixStream` returns one of these error kinds. This is a deterministic
/// transport-level signal, not an arbitrary wall-clock timeout.
pub(super) fn is_worker_dead_io_error(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::UnexpectedEof
            | std::io::ErrorKind::BrokenPipe
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::ConnectionAborted
    )
}

/// Why a readiness wait must stop, when it can be known before the deadline.
///
/// The daemon writes its pid before binding the socket (`run_socket_worker`),
/// so a missing pid file means liveness is UNKNOWN, not dead — only positive
/// evidence of death aborts the wait.
#[cfg(unix)]
pub(super) fn readiness_abort_reason(
    observed_child: Option<&mut Child>,
    socket_path: &Path,
) -> Option<String> {
    // A child we spawned is authoritative: the OS reports its exit directly.
    if let Some(child) = observed_child {
        return match child.try_wait() {
            Ok(Some(status)) => Some(format!(
                "worker daemon exited before reaching readiness: {status}"
            )),
            Ok(None) => None,
            Err(error) => Some(format!("failed to poll worker daemon liveness: {error}")),
        };
    }
    // A daemon we attached to has no child handle, so the pid file it wrote is
    // the only death signal available: it records the pid, and the accept loop
    // unlinks it as part of shutdown.
    let recorded = std::fs::read_to_string(socket_path.with_extension("pid"))
        .ok()
        .and_then(|value| value.trim().parse::<libc::pid_t>().ok())?;
    // EPERM means the pid exists under another user; only ESRCH is proof.
    recorded_daemon_is_gone(recorded).then_some(format!(
        "worker daemon (pid {recorded}) exited before reaching readiness"
    ))
}

/// Probe a recorded daemon pid for existence without racing pid recycling.
///
/// `kill(pid, 0)` alone cannot see a single-threaded worker exit that the
/// supervisor has not reaped yet, so `/proc` state is consulted instead, and a
/// zombie counts as gone: it is dead, only its exit status is pending.
#[cfg(target_os = "linux")]
pub(super) fn recorded_daemon_is_gone(pid: libc::pid_t) -> bool {
    use std::os::raw::c_int;

    if pid <= 0 {
        return true;
    }
    let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) else {
        return true;
    };
    // Field 3 is `state`; the comm field (2) is parenthesised and may contain
    // spaces, so parse after the final ")".
    let state = stat
        .rsplit_once(") ")
        .and_then(|(_, fields)| fields.split_whitespace().next());
    if matches!(state, Some("Z" | "X")) {
        return true;
    }
    // Reaping can clear /proc between the read and the probe; either answer
    // now means the same thing.
    let unreachable = unsafe { libc::kill(pid, 0 as c_int) != 0 };
    unreachable && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

#[cfg(all(unix, not(target_os = "linux")))]
pub(super) fn recorded_daemon_is_gone(pid: libc::pid_t) -> bool {
    pid <= 0 || unsafe { libc::kill(pid, 0) != 0 }
}

/// Locally stat-able ORT candidates, probed without spawning Python.
///
/// Deliberately narrower than `ort_discovery`: the full chain falls back to a
/// `python3` subprocess to locate a pip install (`discover_pip_lib`), which has
/// no place on the embed request path. System directories are left to
/// `discover_path_only`, which is reached exactly when every higher-priority
/// source has already missed.
#[cfg(feature = "onnx")]
pub(super) fn local_ort_candidate_exists() -> bool {
    crate::embed::ort_discovery::last_outcome().is_some_and(|found| found.path.exists())
        || crate::embed::ort_discovery::discover_candidates()
            .into_iter()
            .any(|(_, path)| path.exists())
        || crate::embed::ort_discovery::discover_pip_lib_filesystem().is_some()
}

/// Report whether this host has no ORT runtime a worker could load.
///
/// The daemon binds its socket, fails ORT/model init, and shuts itself down; a
/// wait for it can only end at the deadline. Deciding this BEFORE the spawn
/// keeps that deadline out of the picture entirely. `last_outcome().is_some()`
/// mirrors `provider.rs::is_ort_available`: an ORT runtime already committed in
/// THIS process answers without touching disk.
#[cfg(feature = "onnx")]
pub(super) fn host_lacks_onnx_runtime() -> bool {
    if let Some(override_unavailable) = neural_preflight_override() {
        return override_unavailable;
    }
    if crate::embed::ort_discovery::last_outcome().is_some() || local_ort_candidate_exists() {
        return false;
    }
    crate::embed::ort_discovery::discover_path_only().is_none()
}

/// Test seam: overrides ORT availability for the current thread only.
///
/// Preflight runs on the caller's thread, so `None` leaves production behaviour
/// untouched and a thread-local keeps parallel tests from fighting over it.
#[cfg(feature = "onnx")]
pub(super) fn neural_preflight_override() -> Option<bool> {
    NEURAL_PREFLIGHT_OVERRIDE.with(std::cell::Cell::get)
}

#[cfg(feature = "onnx")]
thread_local! {
    /// `Some(true)` forces "no ORT runtime", `Some(false)` forces "available".
    pub(super) static NEURAL_PREFLIGHT_OVERRIDE: std::cell::Cell<Option<bool>> =
        const { std::cell::Cell::new(None) };
}
