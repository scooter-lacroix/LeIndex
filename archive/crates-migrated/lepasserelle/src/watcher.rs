use crate::registry::ProjectHandle;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use tokio::sync::mpsc;
use tracing::{debug, warn};

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

        // Debounced consumer — coalesces events over 500ms window
        tokio::spawn(async move {
            let mut debounce = tokio::time::interval(tokio::time::Duration::from_millis(500));
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
                                let mut idx = handle_clone.blocking_lock();
                                if let Err(e) = idx.index_project(false) {
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
