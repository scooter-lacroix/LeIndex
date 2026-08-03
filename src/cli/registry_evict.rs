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
    /// **Known benign race (Kilo review item, documented-and-leave):** the
    /// candidate set is snapshotted under the `last_used` read lock, then each
    /// project is checked + evicted afterward. A `get_or_load` that lands
    /// between the snapshot and the `try_write` guard re-touches `last_used`,
    /// but the in-flight `try_write()` guard below still prevents the
    /// destructive case (tearing down a mid-call engine). The residual is a
    /// benign evict-and-reload-on-next-call, which the D-2 design tolerates.
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
            // Skip projects with an in-flight call: the mutex is held by an
            // active tool handler, so eviction must not tear it down mid-use.
            let in_flight = {
                let projects = self.projects.read().await;
                match projects.get(&path) {
                    Some(handle) => handle.try_write().is_err(),
                    None => false,
                }
            };
            if in_flight {
                continue;
            }
            self.evict(&path).await;
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
