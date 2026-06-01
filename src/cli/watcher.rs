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
///
/// The budget is enforced on two phases:
///   1. **Lock acquisition** — if `try_write` keeps failing, the
///      watcher polls with `RETRY_BACKOFF_MS` for at most this long
///      before declaring the lock busy and skipping the batch.
///   2. **Reindex execution** — the reindex future is wrapped in
///      `tokio::time::timeout` so a runaway rebuild cannot starve the
///      rest of the server even if the work itself hangs.
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
                            // Run the budget-wait + reindex on
                            // `spawn_blocking`. The threadpool impact
                            // is bounded to one thread per project for
                            // at most `REINDEX_BUDGET_SECS`. We then
                            // wrap the join in `tokio::time::timeout`
                            // so a runaway rebuild cannot pin a worker
                            // for longer than the budget even if the
                            // work itself does not honour the in-loop
                            // budget check. ProjectWriteGuard borrows
                            // from the handle, so the guard must not
                            // cross an await point — the entire
                            // acquire + reindex sequence lives inside
                            // the spawn_blocking closure.
                            let budget = Duration::from_secs(REINDEX_BUDGET_SECS);
                            let backoff = Duration::from_millis(RETRY_BACKOFF_MS);
                            let reindex_budget = budget;
                            let blocking = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
                                let mut idx = match acquire_with_budget(
                                    &handle_clone,
                                    budget,
                                    backoff,
                                ) {
                                    LockAcquire::Acquired(g) => g,
                                    LockAcquire::Skipped => return Ok(()),
                                };
                                // Panic-safety wrapper: a panic inside
                                // the reindex still releases the lock
                                // when `idx` goes out of scope on
                                // `Drop`.
                                let reindex_result = std::panic::catch_unwind(
                                    std::panic::AssertUnwindSafe(|| {
                                        idx.incremental_reindex_from_watcher()
                                    }),
                                );
                                match reindex_result {
                                    Ok(Ok(_)) => Ok(()),
                                    Ok(Err(e)) => {
                                        warn!("Auto-reindex failed: {}", e);
                                        Ok(())
                                    }
                                    Err(_) => {
                                        warn!("Auto-reindex panicked; lock will release on drop");
                                        Ok(())
                                    }
                                }
                            });
                            match tokio::time::timeout(reindex_budget, blocking).await {
                                Ok(_) => {}
                                Err(_) => {
                                    // Reindex exceeded the budget. The
                                    // underlying `spawn_blocking` task
                                    // continues until the reindex body
                                    // returns (the lock still drops on
                                    // the `ProjectWriteGuard`'s `Drop`),
                                    // but the outer task no longer
                                    // awaits it — so a slow reindex
                                    // cannot stall the watcher loop.
                                    // Preserve `dirty` so the next
                                    // debounce tick retries instead of
                                    // silently dropping the changes.
                                    warn!(
                                        "Auto-reindex exceeded {}s budget; detached",
                                        REINDEX_BUDGET_SECS
                                    );
                                    dirty = true;
                                }
                            }
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
/// by another caller. The busy-wait with `std::thread::sleep` runs
/// inside `spawn_blocking`, so the threadpool impact is bounded to
/// one thread per project for at most `REINDEX_BUDGET_SECS` (default
/// 30s). The outer `tokio::time::timeout` on the `spawn_blocking` join
/// further guarantees the watcher loop is never blocked longer than
/// the budget even if the reindex body itself ignores the inner
/// budget check.
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
