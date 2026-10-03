// LeIndex CLI Binary
//
// Main entry point for the leindex command-line tool.

use leindex::cli::cli;

// When the `memprof` feature is enabled, use jemalloc with heap profiling
// support as the global allocator. This allows engineers to generate detailed
// heap profiles via `MALLOC_CONF` environment variables without affecting
// default builds or CI.
//
// VAL-MEASURE-025: memprof is opt-in and build-time gated.
// VAL-MEASURE-026: Building with --features memprof succeeds.
#[cfg(feature = "memprof")]
#[global_allocator]
static GLOBAL: tikv_jemallocator::Jemalloc = tikv_jemallocator::Jemalloc;

/// Environment variable name for configuring the Tokio worker thread count.
///
/// Spec §8.1: "Daemon Tokio workers: start at 2; benchmark 2-4."
/// When unset, zero, or non-numeric, defaults to [`DEFAULT_TOKIO_WORKERS`].
const TOKIO_WORKERS_ENV: &str = "LEINDEX_TOKIO_WORKERS";

/// Default Tokio worker thread count when the env var is unset or invalid.
///
/// Spec §8.1: start at 2 workers rather than `available_parallelism()`.
/// This is a containment default that caps per-process Tokio memory overhead
/// without using a feature flag (per user direction: runtime containment
/// defaults are default-on).
const DEFAULT_TOKIO_WORKERS: usize = 2;

/// Determine the Tokio worker thread count from the environment.
///
/// Reads [`TOKIO_WORKERS_ENV`] and parses it as a positive integer.
/// Falls back to [`DEFAULT_TOKIO_WORKERS`] (2) when the variable is unset,
/// zero, negative, or non-numeric.
///
/// VAL-BASE-008: default is 2; env-configurable via `LEINDEX_TOKIO_WORKERS`.
fn configured_worker_count() -> usize {
    std::env::var(TOKIO_WORKERS_ENV)
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_TOKIO_WORKERS)
}

/// Build a multi-threaded Tokio runtime with the configured worker count.
///
/// Replaces `#[tokio::main]` which defaults to `available_parallelism()`.
/// The manual builder caps the worker pool at [`DEFAULT_TOKIO_WORKERS`] by
/// default (spec §8.1 containment), while remaining env-configurable for
/// benchmark sweeps (`LEINDEX_TOKIO_WORKERS=N`).
fn build_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(configured_worker_count())
        .enable_all()
        .build()
        .expect("failed to build Tokio runtime")
}

fn main() -> anyhow::Result<()> {
    // Single-binary worker mode: `leindex --internal-embed-worker [args...]`
    // re-executes this same executable as the ONNX embed worker. Token-only
    // (never an env var — a leaked env value would silently turn every
    // invocation into the worker). The token is stripped before dispatch so
    // worker_main's position-based parsing sees a clean argv; `run_from` -> !
    #[cfg(feature = "onnx")]
    if leindex::embed::worker_main::is_internal_worker_invocation() {
        let mut argv: Vec<String> = std::env::args().collect();
        argv.remove(1);
        leindex::embed::worker_main::run_from(argv);
    }
    #[cfg(not(feature = "onnx"))]
    {
        // Without the onnx feature the worker does not exist; a stray token
        // falls through to clap's unknown-argument error.
    }
    // Stdio MCP launches go through the user's daemon when one is available.
    // This runs before the async runtime, the config loader and the CLI parser
    // exist: the shim is a couple of threads copying bytes, and it must be
    // ready to answer the client within a millisecond or two.
    #[cfg(all(feature = "daemon-client", unix))]
    {
        let args: Vec<String> = std::env::args().skip(1).collect();
        if let Some(project) = leindex::cli::daemon::client::eligible_project(&args) {
            match leindex::cli::daemon::client::run(project) {
                leindex::cli::daemon::client::Outcome::Done(status) => {
                    std::process::exit(status);
                }
                leindex::cli::daemon::client::Outcome::Fallback(reason) => {
                    eprintln!("leindex: running standalone ({reason})");
                }
            }
        }
    }
    let rt = build_runtime();
    rt.block_on(cli::main())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::Mutex;

    /// Serialize env-var tests so that concurrent test threads do not race
    /// on set/remove of `LEINDEX_TOKIO_WORKERS`. Env mutation is process-global.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// VAL-BASE-008: When the env var is unset, the default worker count is 2.
    #[test]
    fn test_default_workers_is_two() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: We hold ENV_LOCK so no other test touches this var concurrently.
        unsafe {
            std::env::remove_var("LEINDEX_TOKIO_WORKERS");
        }
        assert_eq!(configured_worker_count(), DEFAULT_TOKIO_WORKERS);
        assert_eq!(configured_worker_count(), 2);
    }

    /// VAL-BASE-008: A positive integer is respected.
    #[test]
    fn test_explicit_worker_count_is_respected() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: We hold ENV_LOCK.
        unsafe {
            std::env::set_var("LEINDEX_TOKIO_WORKERS", "4");
        }
        assert_eq!(configured_worker_count(), 4);
        // SAFETY: We hold ENV_LOCK.
        unsafe {
            std::env::remove_var("LEINDEX_TOKIO_WORKERS");
        }
    }

    /// VAL-BASE-008: Zero falls back to 2.
    #[test]
    fn test_zero_workers_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: We hold ENV_LOCK.
        unsafe {
            std::env::set_var("LEINDEX_TOKIO_WORKERS", "0");
        }
        assert_eq!(configured_worker_count(), 2);
        // SAFETY: We hold ENV_LOCK.
        unsafe {
            std::env::remove_var("LEINDEX_TOKIO_WORKERS");
        }
    }

    /// VAL-BASE-008: A non-integer value falls back to 2.
    #[test]
    fn test_non_numeric_workers_falls_back_to_default() {
        let _guard = ENV_LOCK.lock().unwrap();
        // SAFETY: We hold ENV_LOCK.
        unsafe {
            std::env::set_var("LEINDEX_TOKIO_WORKERS", "abc");
        }
        assert_eq!(configured_worker_count(), 2);
        // SAFETY: We hold ENV_LOCK.
        unsafe {
            std::env::remove_var("LEINDEX_TOKIO_WORKERS");
        }
    }
}
