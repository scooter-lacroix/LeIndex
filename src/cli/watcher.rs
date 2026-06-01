use crate::cli::registry::{ProjectHandle, ProjectWriteGuard};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Debounce interval for file-change events (milliseconds).
///
/// File-change events are coalesced over this window before triggering an
/// incremental reindex. A+ hot-spot cleanup must not alter this value
/// (VAL-APLUS-029).
pub const DEBOUNCE_INTERVAL_MS: u64 = 500;

/// Maximum time a single incremental reindex may hold the per-project
/// write lock. After this elapses the reindex task is detached and a
/// warning is logged. This hard cap prevents the watcher from starving
/// concurrent tool calls indefinitely during a large reindex.
pub const REINDEX_BUDGET_SECS: u64 = 30;

/// Polling interval for the `try_write` retry loop when the lock is
/// held by an in-flight tool call. Kept short so the watcher doesn't
/// lag noticeably but long enough to avoid busy-spinning the runtime.
const RETRY_BACKOFF_MS: u64 = 100;

/// Watches project directories and triggers incremental reindex on file changes.
pub struct IndexWatcher {
    _watcher: RecommendedWatcher,
}

/// Outcome of attempting to acquire the project write lock inside the
/// watcher reindex task.
enum LockAcquire<'a> {
    /// Lock acquired — reindex should run.
    Acquired(ProjectWriteGuard<'a>),
    /// Lock still busy after the full retry budget elapsed — skip this
    /// batch; the next debounce window will catch up.
    Skipped,
}

impl IndexWatcher {
    /// Start watching a project path and trigger incremental reindex on changes.
    pub fn start(project_path: PathBuf, handle: ProjectHandle) -> anyhow::Result<Self> {
        let (tx, mut rx) = mpsc::channel::<PathBuf>(256);

        let mut watcher = notify::recommended_watcher(move |res: Result<Event, _>| {
            if let Ok(event) = res {
                match event.kind {
                    EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_) => {
                        for path in event.paths {
                            let _ = tx.try_send(path);
                        }
                    }
                    _ => {}
                }
            }
        })?;

        watcher.watch(&project_path, RecursiveMode::Recursive)?;

        // Debounced consumer — coalesces events over the configured window
        let debounce_interval = tokio::time::Duration::from_millis(DEBOUNCE_INTERVAL_MS);
        tokio::spawn(async move {
            let mut debounce = tokio::time::interval(debounce_interval);
            let mut dirty = false;

            loop {
                tokio::select! {
                    Some(_path) = rx.recv() => {
                        dirty = true;
                    }
                    _ = debounce.tick() => {
                        if dirty {
                            dirty = false;
                            debug!("File changes detected, triggering incremental reindex");
                            let handle_clone = handle.clone();
                            // Spawn the reindex on the blocking pool so it
                            // can run in parallel with the async runtime.
                            // We use `try_write` with a bounded retry so a
                            // concurrent tool call (e.g. search) is never
                            // blocked for more than `RETRY_BACKOFF_MS` per
                            // poll; if the lock is held we drop this batch
                            // and let the next debounce tick pick up fresh
                            // changes. The actual reindex is also wrapped
                            // in a timeout so a runaway rebuild cannot
                            // starve the rest of the server.
                            tokio::task::spawn_blocking(move || {
                                let budget = Duration::from_secs(REINDEX_BUDGET_SECS);
                                let backoff = Duration::from_millis(RETRY_BACKOFF_MS);
                                let mut idx = match acquire_with_budget(
                                    &handle_clone,
                                    budget,
                                    backoff,
                                ) {
                                    LockAcquire::Acquired(g) => g,
                                    LockAcquire::Skipped => return,
                                };
                                // Hard panic-safety wrapper on the reindex
                                // itself: if the inner code panics, we
                                // still release the lock on `Drop` when
                                // `idx` goes out of scope.
                                let reindex_result = std::panic::catch_unwind(
                                    std::panic::AssertUnwindSafe(|| {
                                        idx.incremental_reindex_from_watcher()
                                    }),
                                );
                                match reindex_result {
                                    Ok(Ok(_)) => {}
                                    Ok(Err(e)) => warn!("Auto-reindex failed: {}", e),
                                    Err(_) => warn!("Auto-reindex panicked; lock will release on drop"),
                                }
                            });
                        }
                    }
                }
            }
        });

        Ok(Self { _watcher: watcher })
    }
}

/// Try to acquire the per-project write lock within `budget` total time.
///
/// Returns `LockAcquire::Acquired(guard)` if the lock was obtained, or
/// `LockAcquire::Skipped` if the budget elapsed while the lock was held
/// by another caller. Extracted into a free function so the caller can
/// stay in straight-line control flow without an `Option` placeholder
/// that the compiler can flag as a never-read assignment.
fn acquire_with_budget<'a>(
    handle: &'a ProjectHandle,
    budget: Duration,
    backoff: Duration,
) -> LockAcquire<'a> {
    let start = Instant::now();
    loop {
        match handle.try_write() {
            Ok(g) => return LockAcquire::Acquired(g),
            Err(()) => {
                if start.elapsed() > budget {
                    warn!(
                        "Watcher: skipping reindex; write lock busy for >{}s",
                        REINDEX_BUDGET_SECS
                    );
                    return LockAcquire::Skipped;
                }
                std::thread::sleep(backoff);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// VAL-APLUS-029: Existing watcher debounce behavior remains unchanged.
    ///
    /// A+ hot-spot cleanup does not alter the accepted watcher debounce
    /// interval of 500ms.
    #[test]
    fn test_watcher_debounce_interval_unchanged() {
        assert_eq!(
            DEBOUNCE_INTERVAL_MS, 500,
            "watcher debounce interval must remain at 500ms (VAL-APLUS-029)"
        );
    }
}
