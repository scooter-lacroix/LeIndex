//! D-2 idle-engine eviction for [`ProjectRegistry`] (memory-pressure
//! remediation).
//!
//! Extracted into a sibling module so `registry.rs` stays comfortably under
//! the 2000-line Large-File gate while the MCP lifecycle batch (T2–T7) keeps
//! adding eviction-adjacent code.

use std::path::Path;

use crate::cli::registry::ProjectRegistry;

impl ProjectRegistry {
    /// Record that `path` was just used (D-2 idle-eviction clock).
    pub(crate) async fn touch_last_used(&self, path: &Path) {
        self.last_used
            .write()
            .await
            .insert(path.to_path_buf(), std::time::Instant::now());
    }

    /// Evict loaded engines that have been idle (no `get_or_load`/touch) for
    /// longer than `max_idle`. Projects with an active call (lock held) are
    /// skipped. Returns the number of projects evicted.
    ///
    /// D-2 memory-pressure remediation: a long-lived MCP process that touched
    /// a large project (e.g. the 51 GiB-index workstation project) must not
    /// retain that engine's mmaps/heap for the process lifetime. The next
    /// tool call transparently reloads via `get_or_load`.
    ///
    /// **Atomicity (Codex P2):** every candidate is checked and evicted under
    /// ONE `last_used` read + `projects` write hold. `get_or_load` clones the
    /// project `Arc` *and* refreshes `last_used` together under its own read
    /// lock, so a fresh timestamp observed under our held locks provably means
    /// an `Arc` is outstanding for this project — we skip it rather than close
    /// storage out from under the pending request. `try_write()` additionally
    /// skips a caller that is currently inside the inner lock. Together these
    /// close the window in which an eviction could hand a later-locking caller
    /// a closed index (previously the freshness check, the removal, and the
    /// close each ran under separate lock acquisitions).
    pub async fn evict_idle_engines(&self, max_idle: std::time::Duration) -> usize {
        let candidates: Vec<std::path::PathBuf> = {
            let last_used = self.last_used.read().await;
            let projects = self.projects.read().await;
            projects
                .keys()
                .filter(|path| {
                    last_used
                        .get(*path)
                        .map(|last| last.elapsed() > max_idle)
                        .unwrap_or(true)
                })
                .cloned()
                .collect()
        };

        let mut evicted = 0;
        for path in candidates {
            // One atomic sequence per candidate (Codex P2). Lock order is
            // `projects` write FIRST, then `last_used` read — matching
            // `get_or_load` (projects.read -> last_used.write via
            // `touch_last_used`), so the sweep can never deadlock against an
            // in-flight tool call. Holding the projects write lock through the
            // checks + remove + close means no `get_or_load` read section can
            // be in flight: the freshness value we read is authoritative, and
            // no new Arc can be cloned between the removal and the close.
            let mut projects = self.projects.write().await;
            let last_used = self.last_used.read().await;

            if !projects.contains_key(&path) {
                continue;
            }
            // A `get_or_load` landed since the snapshot: it refreshed
            // `last_used`, so an Arc is outstanding — never close storage out
            // from under that pending request.
            let touched_recently = last_used
                .get(&path)
                .map(|last| last.elapsed() <= max_idle)
                .unwrap_or(true);
            if touched_recently {
                continue;
            }
            // A caller is currently inside the inner lock: in-flight request.
            let in_flight = projects
                .get(&path)
                .is_some_and(|handle| handle.try_write().is_err());
            if in_flight {
                continue;
            }
            // Under the held write lock the key cannot vanish between the
            // checks and the removal; still, degrade gracefully rather than
            // panicking the sweep task if the invariant ever breaks.
            let Some(handle) = projects.remove(&path) else {
                continue;
            };
            match handle.try_write() {
                Ok(mut idx) => {
                    if let Err(e) = idx.close() {
                        tracing::warn!(
                            "Failed to close storage for evicted project {}: {}",
                            path.display(),
                            e
                        );
                    }
                }
                // A caller acquired the inner lock between our checks (a
                // pre-existing Arc holder, e.g. a long-parked handler): leave
                // the index alive; storage closes when its last Arc drops.
                Err(()) => tracing::debug!(
                    "Skipped close for in-flight evicted project {}",
                    path.display()
                ),
            }
            tracing::info!("Evicted project: {}", path.display());
            drop(projects);
            drop(last_used);
            self.cleanup_evicted(&path).await;
            evicted += 1;
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::leindex::LeIndex;

    #[tokio::test]
    async fn test_evict_idle_engines_removes_idle_project() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);
        let canonical = tmp.path().canonicalize().unwrap();
        assert_eq!(registry.len().await, 1);

        // Age the D-2 timestamp past the idle window, then sweep.
        registry.last_used.write().await.insert(
            canonical.clone(),
            std::time::Instant::now() - std::time::Duration::from_secs(3600),
        );
        let evicted = registry
            .evict_idle_engines(std::time::Duration::from_secs(600))
            .await;
        assert_eq!(evicted, 1);
        assert_eq!(registry.len().await, 0);
        // The evicted project reloads transparently on the next request.
        let handle = registry.get_or_load(None).await.unwrap();
        assert_eq!(handle.read().await.project_path(), &canonical);
    }

    #[tokio::test]
    async fn test_evict_idle_engines_skips_recent_and_in_flight() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);
        let canonical = tmp.path().canonicalize().unwrap();

        // Recently-touched project: not idle, must survive the sweep.
        registry.touch_last_used(&canonical).await;
        let evicted = registry
            .evict_idle_engines(std::time::Duration::from_secs(600))
            .await;
        assert_eq!(evicted, 0);
        assert_eq!(registry.len().await, 1);

        // In-flight project (an active tool call holds the lock): even with a
        // stale timestamp, eviction must skip it (D-2 consistency guard).
        let handle = registry.get_or_load(None).await.unwrap();
        // get_or_load touches last_used, so age the timestamp AFTER the load,
        // then hold the read lock to simulate an active tool call.
        registry.last_used.write().await.insert(
            canonical.clone(),
            std::time::Instant::now() - std::time::Duration::from_secs(3600),
        );
        let _active_call = handle.read().await;
        let evicted = registry
            .evict_idle_engines(std::time::Duration::from_secs(600))
            .await;
        assert_eq!(evicted, 0, "in-flight engine must not be evicted");
        assert_eq!(registry.len().await, 1);
        drop(_active_call);

        // After the call completes, the stale project is evictable again.
        let evicted = registry
            .evict_idle_engines(std::time::Duration::from_secs(600))
            .await;
        assert_eq!(evicted, 1);
        assert_eq!(registry.len().await, 0);
    }
}
