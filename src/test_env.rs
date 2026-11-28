//! One mutex for every test that mutates process-global environment state.
//!
//! `std::env::set_var`/`remove_var` mutate the whole process, and `cargo test`
//! runs tests on parallel threads, so two tests touching the environment at the
//! same time are a data race. A module-local lock does not fix that: it only
//! excludes its own module's tests, while every other module's tests keep
//! running. Seven such locks had accumulated in this crate's test binary
//! (`config`, `setup_test`, `indexing::tests`, the ORT `client_test`, the MCP
//! `server_test`, `memory_report`, and `embed::runtime_test`), which is no
//! mutual exclusion at all between modules.
//!
//! The observable symptom is CI-only flakes: the suite passes at
//! `--test-threads=1` and fails under the parallel runner depending on
//! scheduling. A failure on PR #105 traced straight to it.
//! `cli::mcp::search_handler`'s handler reads embedding config through
//! `LeIndexConfig::load_cached()` (`cli/registry.rs`), which serves a
//! process-global snapshot. A test in `config.rs` that set `LEINDEX_HOME` to its
//! own tempdir installed a snapshot rooted at a path it then deleted, and the
//! concurrent search read it. Because `CONFIG_REFRESH_THROTTLE` is 2 seconds,
//! once such a snapshot is trusted its probe is skipped for the rest of the
//! throttle window and every other test in the binary observes it.
//!
//! Two env locks deliberately stay separate, because they cannot share this one:
//! the `bin/leindex` tests (a separate test target, so a separate process) and
//! `cli::registry::auto_index_env_lock` (async, held across `.await`).
//!
//! This module is compiled out of release builds; it exists only to give tests
//! one lock to share.

use std::sync::{Mutex, MutexGuard, PoisonError};

/// Process-wide test mutex, private so every acquisition goes through [`EnvLock`].
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// Shared handle to the process-wide test env mutex.
///
/// A unit struct rather than a `Mutex` so `.lock()` returns `LockResult` exactly
/// as `std::sync::Mutex::lock` does, which keeps existing call sites
/// (`.lock().unwrap()` and `.lock().unwrap_or_else(|p| p.into_inner())`)
/// compiling unchanged. Poisoning stays the caller's call: recovering keeps a
/// panicking test from cascading into unrelated failures.
#[derive(Debug, Clone, Copy)]
pub struct EnvLock;

impl EnvLock {
    /// Acquire the shared test lock, blocking until it is free.
    ///
    /// Mirrors [`Mutex::lock`]. Blocking rather than failing is the point: a
    /// test that finds the lock held should wait its turn, not panic.
    #[allow(clippy::type_complexity)]
    pub fn lock(&self) -> Result<MutexGuard<'static, ()>, PoisonError<MutexGuard<'static, ()>>> {
        ENV_LOCK.lock()
    }
}

/// Environment variable naming the REAL project root inside a re-run child.
pub const SELF_TEST_PROJECT_ROOT: &str = "LEINDEX_SELF_TEST_PROJECT_ROOT";

/// Environment variable naming the DECOY repository inside a re-run child.
pub const SELF_TEST_DECOY_ROOT: &str = "LEINDEX_SELF_TEST_DECOY_ROOT";

/// Re-run the test named `test_name` in a FRESH process, with `vars` set in
/// that CHILD's environment only, and return the child's own harness output.
///
/// Why this exists at all: [`EnvLock`] serialises tests that MUTATE the
/// environment, but it cannot serialise the READERS. Unrelated threads keep
/// calling `getenv` — Rust's own runtime, native libraries, and every child
/// process another test spawns — and Rust 1.88 documents concurrent
/// `set_var`/`remove_var` as undefined behaviour precisely because of them.
/// A lock around the writers does not fix a concurrent reader, so a test that
/// needs a poisoned environment must not poison the runner's.
///
/// `Command::env` gives the child its own environment, so the assertions run
/// against exactly the poisoned values while this process is never written to
/// and nothing has to be restored afterwards. `output()` captures the child's
/// report, so an assertion that fails inside the child fails the parent.
pub fn run_self_test_with_env(
    test_name: &str,
    vars: &[(&str, &std::ffi::OsStr)],
) -> std::process::Output {
    let exe = std::env::current_exe().expect("the test binary path");
    let mut command = std::process::Command::new(exe);
    command.args(["--exact", test_name, "--test-threads=1"]);
    for (key, value) in vars {
        command.env(key, value);
    }
    command.output().expect("re-running this test in a child")
}
