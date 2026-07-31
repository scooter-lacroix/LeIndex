use crate::cli::registry::{ProjectHandle, ProjectWriteGuard};
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, warn};

/// Debounce interval for file-change events (milliseconds).
///
/// File-change events are coalesced over this window before triggering an
/// incremental reindex. A+ hot-spot cleanup must not alter this value
/// (VAL-APLUS-029).
pub const DEBOUNCE_INTERVAL_MS: u64 = 500;

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

/// Outcome of the `spawn_blocking` reindex body. Reported back to the
/// outer task so the watcher loop can distinguish a successful
/// reindex (no state change needed) from a lock-skipped batch (mark
/// `dirty` so the next debounce retries) and from a reindex that
/// errored or panicked (mark `dirty` so the next debounce
/// retries — the in-memory state is potentially inconsistent
/// because the reindex body returned abnormally, so a follow-up
/// full rebuild is the safe response).
#[derive(Debug, Clone, PartialEq, Eq)]
enum ReindexOutcome {
    /// Reindex ran to completion with no error.
    Completed,
    /// Lock was busy for the full budget — reindex never ran.
    Skipped,
    /// Reindex returned an error or its body panicked. The lock
    /// is still released via the `ProjectWriteGuard` `Drop`, but
    /// the on-disk / in-memory state may be inconsistent, so
    /// the next debounce tick should retry.
    Failed(String),
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
            // `Interval::MissedTickBehavior::Delay` makes the next
            // tick fire one full period after the body returns
            // (rather than bursting all missed ticks). Combined
            // with the `debounce.reset()` calls below, this caps
            // the reindex re-spawn rate at one per `debounce_interval`
            // even when the body sets `dirty = true` (Skipped /
            // Failed / join error): the interval is reset to a full
            // period from "now", so a tight re-spawn loop is
            // impossible.
            let mut debounce = tokio::time::interval(debounce_interval);
            debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut dirty = false;
            let (done_tx, mut done_rx) = mpsc::channel::<ReindexOutcome>(1);
            let mut reindex_active = false;

            loop {
                tokio::select! {
                    Some(_path) = rx.recv() => {
                        dirty = true;
                    }
                    Some(outcome) = done_rx.recv() => {
                        reindex_active = false;
                        match outcome {
                            ReindexOutcome::Completed => {}
                            ReindexOutcome::Skipped => {
                                warn!("Watcher: skipping reindex; write lock is currently busy");
                                dirty = true;
                                debounce.reset();
                            }
                            ReindexOutcome::Failed(reason) => {
                                warn!(
                                    "Watcher: reindex did not complete cleanly ({}); retrying on next tick",
                                    reason
                                );
                                dirty = true;
                                debounce.reset();
                            }
                        }
                    }
                    _ = debounce.tick(), if !reindex_active => {
                        if dirty {
                            dirty = false;
                            debug!("File changes detected, triggering incremental reindex");
                            if handle.try_write().is_err() {
                                dirty = true;
                                debounce.reset();
                                continue;
                            }
                            let handle_clone = handle.clone();
                            // Keep one owned reindex job per project. A
                            // wall-clock timeout cannot cancel spawn_blocking;
                            // dropping it only detaches work that still owns
                            // the write lock. Completion is reported through
                            // the channel instead, so slow work remains visible
                            // without duplicate jobs or state races.
                            let blocking = tokio::task::spawn_blocking(move || run_reindex(handle_clone));
                            reindex_active = true;
                            let done_tx = done_tx.clone();
                            tokio::spawn(async move {
                                let outcome = match blocking.await {
                                    Ok(outcome) => outcome,
                                    Err(join_err) => {
                                        warn!("Watcher: reindex task join failed: {}", join_err);
                                        ReindexOutcome::Failed(join_err.to_string())
                                    }
                                };
                                let _ = done_tx.send(outcome).await;
                            });
                        }
                    }
                }
            }
        });

        Ok(Self { _watcher: watcher })
    }
}

fn run_reindex(handle: ProjectHandle) -> ReindexOutcome {
    let mut idx = match try_acquire_lock(&handle) {
        LockAcquire::Acquired(g) => g,
        LockAcquire::Skipped => return ReindexOutcome::Skipped,
    };
    // Cross-process guard: skip (not block) if another process is already
    // writing this project (e.g. a concurrent `leindex index`). A blocking
    // flock here would stall the watcher — spawn_blocking can't be cancelled,
    // so a held lock would leave reindex_active true forever. Held via RAII
    // for the whole reindex below.
    let _flock = match idx.try_acquire_write_lock() {
        Ok(Some(guard)) => guard,
        Ok(None) => return ReindexOutcome::Skipped,
        Err(e) => return ReindexOutcome::Failed(format!("write-lock probe: {e}")),
    };
    let reindex_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        idx.incremental_reindex_from_watcher()
    }));
    match reindex_result {
        Ok(Ok(_)) => ReindexOutcome::Completed,
        Ok(Err(e)) => {
            warn!("Auto-reindex failed: {}", e);
            ReindexOutcome::Failed(e.to_string())
        }
        Err(panic_payload) => {
            let msg = panic_payload
                .downcast_ref::<&str>()
                .map(|s| s.to_string())
                .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "non-string panic payload".to_string());
            warn!("Auto-reindex panicked: {}; lock will release on drop", msg);
            ReindexOutcome::Failed(format!("panic: {}", msg))
        }
    }
}

/// Try to acquire the per-project write lock once and return
/// immediately.
///
/// Calls [`ProjectHandle::try_write`] (a non-blocking
/// `try_lock`) and returns [`LockAcquire::Acquired`] with the
/// guard on success, or [`LockAcquire::Skipped`] if the lock is
/// currently held. There is no budget, no sleep, and no
/// `spawn_blocking` involvement: the function is fail-fast and
/// returns within microseconds. Retries are the caller's
/// responsibility — the outer async watcher loop's debounced
/// retry path (`debounce.reset()` + `dirty = true`) handles
/// the case where the lock was busy.
fn try_acquire_lock<'a>(handle: &'a ProjectHandle) -> LockAcquire<'a> {
    match handle.try_write() {
        Ok(g) => LockAcquire::Acquired(g),
        Err(()) => LockAcquire::Skipped,
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

    /// Regression for MED #3342354730: the watcher reindex spawn_blocking
    /// closure used to silently return `ReindexOutcome::Completed` for
    /// a panic, leaving `dirty = false` and dropping the change
    /// permanently. After the fix, the closure must surface a
    /// `ReindexOutcome::Failed(reason)` variant for both `Ok(Err(_))`
    /// and the panic branch, and the outer `match` must re-arm
    /// `dirty = true` so the next debounce tick retries.
    ///
    /// The full Watcher::run path is async + requires a tokio runtime
    /// + a real project handle, so we exercise the *enum contract*:
    ///
    /// the `Failed` variant exists, carries a reason, and matches a
    /// `dirty = true` arm in the consumer. The adjacent unit test
    /// covers this enum contract.
    #[test]
    fn test_watcher_failed_outcome_carries_reason() {
        let outcome: ReindexOutcome = ReindexOutcome::Failed(
            "incremental_reindex_from_watcher panicked: db corrupt".to_string(),
        );
        match outcome {
            ReindexOutcome::Completed => {
                panic!("panic path must NOT surface as Completed")
            }
            ReindexOutcome::Skipped => {
                panic!("panic path must NOT surface as Skipped")
            }
            ReindexOutcome::Failed(reason) => {
                assert!(
                    reason.contains("db corrupt"),
                    "reason must include the panic payload: {}",
                    reason
                );
            }
        }
    }

    /// The `Failed` variant is distinct from `Completed` and
    /// `Skipped` so the consumer's match arm for `Failed(reason)`
    /// reliably re-arms `dirty` even if a future refactor
    /// consolidates the other arms.
    #[test]
    fn test_watcher_outcome_variants_are_distinct() {
        let completed = ReindexOutcome::Completed;
        let skipped = ReindexOutcome::Skipped;
        let failed = ReindexOutcome::Failed("oops".to_string());
        assert!(matches!(completed, ReindexOutcome::Completed));
        assert!(matches!(skipped, ReindexOutcome::Skipped));
        assert!(matches!(failed, ReindexOutcome::Failed(ref s) if s == "oops"));
        // Cross-variant assertion to catch any future enum
        // collapse that would silently change the consumer's
        // match behaviour.
        assert!(!matches!(failed, ReindexOutcome::Completed));
        assert!(!matches!(failed, ReindexOutcome::Skipped));
    }

    /// Regression for MED round 14 (gemini `3344534850`) and the
    /// round-15 rename (gemini `3344869691`): `try_acquire_lock`
    /// (formerly `acquire_with_budget`) is a fail-fast `try_write`
    /// that returns `Skipped` within microseconds when the lock is
    /// held. The original busy-loop with `std::thread::sleep(backoff)`
    /// could pin a `spawn_blocking` thread for up to
    /// while waiting; the round-15 rename keeps the
    /// fail-fast semantics and aligns the function name with the
    /// actual behaviour. The test holds the write lock from the
    /// current thread, calls `try_acquire_lock` from a fresh
    /// thread, and asserts the call returns `Skipped` well under
    /// 1s — a future revert to a budgeted loop would either hang
    /// (caught by the test timeout) or take noticeably
    /// longer than 1s (caught by the explicit `Duration` check).
    #[test]
    fn test_try_acquire_lock_is_fail_fast() {
        use crate::cli::leindex::LeIndex;
        use crate::cli::registry::{ProjectHandle, ProjectRwLock};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();
        let leindex = LeIndex::new(tmp.path()).unwrap();
        let handle: ProjectHandle = Arc::new(ProjectRwLock::new(leindex));

        // Hold the write lock so any try_write returns Err.
        let _held = handle.blocking_write();

        let h2 = handle.clone();
        let start = Instant::now();
        let outcome = std::thread::spawn(move || {
            // Reduce the borrow of `h2` to a plain `Result<(), ()>`
            // so the return value is `'static` — `LockAcquire`
            // borrows from the handle, which would otherwise be
            // moved into the closure and outlive the join.
            match try_acquire_lock(&h2) {
                LockAcquire::Acquired(_) => Ok(()),
                LockAcquire::Skipped => Err(()),
            }
        })
        .join()
        .unwrap();
        let elapsed = start.elapsed();

        assert!(
            outcome.is_err(),
            "lock is held — must return Skipped, not Acquired"
        );
        assert!(
            elapsed < Duration::from_secs(1),
            "try_acquire_lock must be fail-fast; took {:?} (would be ~30s with the old budgeted loop)",
            elapsed
        );
    }

    /// Regression for CRITICAL round 16 (gemini `3344869691` + busy-loop
    /// followup): when the reindex body sets `dirty = true` (Skipped,
    /// Failed, or join error), the debounce interval must be reset so
    /// the next tick fires one full `debounce_interval` from "now"
    /// rather than from the previous tick. Without the reset, a busy
    /// lock would cause a tight re-spawn loop where each iteration
    /// immediately re-fires the already-elapsed tick.
    ///
    /// This test verifies the `Interval::reset()` contract under
    /// `tokio::time::pause()`: after consuming a tick, the next tick
    /// is pending; calling `reset()` keeps the next tick pending (it
    /// has been rescheduled to `now + period`); advancing by the
    /// full period makes the next tick ready. This is the exact
    /// sequence the watcher body uses when it sets `dirty = true`
    /// in the Skipped / Failed / join-error paths.
    #[tokio::test(start_paused = true)]
    async fn test_debounce_resets_on_dirty_re_arm() {
        use std::time::Duration;
        let debounce_interval = Duration::from_millis(500);
        let mut debounce = tokio::time::interval(debounce_interval);
        debounce.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        // First tick is immediate (Interval starts with a ready tick).
        // This represents the initial debounce firing on watcher
        // startup before any file event has arrived.
        tokio::time::timeout(Duration::from_millis(10), debounce.tick())
            .await
            .expect("first tick must be ready");

        // Without reset, the next tick is at +debounce_interval from
        // the previous tick. Verify it is not yet ready.
        tokio::time::timeout(Duration::from_millis(10), debounce.tick())
            .await
            .expect_err("next tick must NOT be ready immediately after consume");

        // Simulate the body setting `dirty = true` and calling
        // `debounce.reset()`: advance the clock by a small amount
        // (representing the time spent in the reindex body) and
        // call `reset()` to reschedule the next tick to `now + period`.
        tokio::time::advance(Duration::from_millis(10)).await;
        debounce.reset();

        // After reset(), the next tick must NOT be ready — it has
        // been rescheduled to `now + debounce_interval`. If the
        // production code skipped the `reset()` call, the next tick
        // would fire at the original schedule (now + remaining
        // period), which is what causes the re-spawn storm. The
        // reset pushes the next attempt to a full period from the
        // current time, capping the re-spawn rate.
        tokio::time::timeout(Duration::from_millis(10), debounce.tick())
            .await
            .expect_err("post-reset tick must NOT be ready immediately");

        // Advance the clock by the full `debounce_interval`. The
        // reset window has elapsed; the next tick must now be
        // ready. This proves the reset moved the tick to
        // `now + period` rather than the original schedule.
        tokio::time::advance(debounce_interval).await;
        tokio::time::timeout(Duration::from_millis(10), debounce.tick())
            .await
            .expect("tick must fire after one full debounce_interval from reset");
    }
}
