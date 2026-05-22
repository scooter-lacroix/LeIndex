use crate::cli::registry::ProjectHandle;
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
                            tokio::task::spawn_blocking(move || {
                                let mut idx = handle_clone.blocking_write();
                                if let Err(e) = idx.incremental_reindex_from_watcher() {
                                    warn!("Auto-reindex failed: {}", e);
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
