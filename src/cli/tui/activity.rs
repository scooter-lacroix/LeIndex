//! Durable index-run observation for the TUI.
//!
//! Split out of `tui.rs`. Reading `index-state.json` and probing the write lock
//! is the half of the TUI that touches the filesystem, and it has no draw code
//! of its own — [`IndexActivity`] is produced here and rendered elsewhere. The
//! split also keeps `tui.rs` under the line-count gate without a module that
//! does two unrelated jobs.

use std::path::Path;

/// One observation of an index run, read from state that outlives the process
/// that wrote it.
///
/// Deliberately not an `IndexJobSnapshot`: those live in the serving
/// `ProjectRegistry`'s in-memory map, so a second process sees none of them.
/// What *is* cross-process is the pair the registry's own progress updater
/// polls — `index-state.json` for the phase and the `index.lock` flock for
/// liveness.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexActivity {
    /// Phase vocabulary shared with `--memory-report` (`IndexPhase::as_str`).
    pub(crate) phase: &'static str,
    pub(crate) generation: u64,
    pub(crate) indexed_file_count: usize,
    /// Items the running phase has finished, and how many it must do. `Some`
    /// only while a phase is reporting — every phase transition and every
    /// completed publish clears it — so its presence is itself evidence that a
    /// phase is live now, not a number left over from a finished one.
    pub(crate) progress_done: usize,
    pub(crate) progress_total: Option<usize>,
    /// A live writer holds the project write lock. This is a fact read from
    /// the kernel, not an inference from how recent the phase looks: a
    /// crashed indexer leaves a stale phase behind but drops its flock, so
    /// phase alone cannot tell "running" apart from "died mid-run".
    pub(crate) writer_active: bool,
    /// Set when the lock could not be probed at all, so "not active" is never
    /// silently reported for "could not tell".
    pub(crate) probe_error: Option<String>,
    pub(crate) last_failure_phase: Option<&'static str>,
    pub(crate) last_failure: Option<String>,
}

/// Observe the current index run without an engine or a registry.
///
/// `None` means there is no usable durable state: the project has never
/// completed an index, or its state file is unreadable. `Some` reports whatever
/// the last run left behind, which may be a completed generation, an
/// interrupted one, or a run still in progress in another process.
pub(crate) fn read_activity(root: &Path) -> Option<IndexActivity> {
    // `resolve_existing_storage_path` only *finds* storage; it never creates a
    // directory or opens SQLite, so a TUI started on an unindexed project stays
    // read-only.
    let storage = crate::cli::leindex::resolve_existing_storage_path(root)?;
    let health = crate::cli::index_freshness::load_health(&storage)?;

    let (writer_active, probe_error) = probe_writer(&storage);
    Some(IndexActivity {
        phase: health.phase.as_str(),
        generation: health.generation,
        indexed_file_count: health.indexed_file_count,
        progress_done: health.progress_done,
        progress_total: health.progress_total,
        writer_active,
        probe_error,
        // `IndexPhase::as_str` is `&'static str`, so the failure phase needs no
        // allocation either.
        last_failure_phase: health.last_failure_phase.map(|phase| phase.as_str()),
        last_failure: health.last_failure,
    })
}

/// Ask the kernel whether a live writer holds the project lock.
///
/// This is the only honest liveness signal available across processes: the
/// phase in `index-state.json` is rewritten at phase boundaries, so a crashed
/// indexer leaves a phase that looks identical to a slow one. The flock is
/// released when the holder dies, so it answers the question the phase cannot.
///
/// Returns `(active, error)`. `try_acquire` reports `Ok(None)` when another
/// process holds the lock and `Ok(Some(guard))` when it is free (the guard is
/// dropped immediately). An error means "could not tell", which is reported
/// rather than folded into a misleading "not running".
pub(super) fn probe_writer(storage: &Path) -> (bool, Option<String>) {
    // `ProjectWriteLock::try_acquire` opens the lock file with `.create(true)`,
    // so probing an absent `index.lock` would CREATE it — a read-only status
    // pane must not mutate the project. No file means no writer: the indexer
    // creates it before it can hold anything.
    if !storage.join("index.lock").exists() {
        return (false, None);
    }
    match crate::cli::leindex::ProjectWriteLock::try_acquire(storage) {
        Ok(Some(_)) => (false, None),
        Ok(None) => (true, None),
        Err(error) => (false, Some(error.to_string())),
    }
}
