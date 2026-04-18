//! Multi-project registry with low-overhead per-project coordination.
//!
//! `ProjectRegistry` replaces the old singleton `Arc<Mutex<LeIndex>>` global.
//! It keeps up to `max_projects` projects in memory simultaneously and evicts
//! the least-recently-used project when capacity is reached.
//!
//! ## Concurrency model
//!
//! * **Outer map** (`RwLock<HashMap<...>>`)
//!   - Read-lock for fast project lookup.
//!   - Write-lock only for insert/remove operations.
//!
//! * **Per-project state** (`Arc<Mutex<LeIndex>>`)
//!   - `LeIndex` is `Send` but not `Sync` (rusqlite internals), so per-project
//!     access is coordinated with a mutex.
//!
//! * **ASAP indexing consolidation** (`index_slots`)
//!   - Concurrent indexing requests for the same project share a per-project
//!     slot lock so only one rebuild runs at a time.
//!   - Waiters re-check index status after acquiring the slot and return cached
//!     stats when possible.

use crate::cli::errors::detect_corruption;
use crate::cli::leindex::{IndexStats, LeIndex};
use crate::cli::mcp::protocol::JsonRpcError;
use crate::cli::watcher::IndexWatcher;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// Default maximum number of projects kept in memory simultaneously.
pub const DEFAULT_MAX_PROJECTS: usize = 5;

/// A handle to one project's `LeIndex`.
pub type ProjectHandle = Arc<Mutex<LeIndex>>;

/// Multi-project registry.
pub struct ProjectRegistry {
    /// Canonical path -> project handle.
    projects: RwLock<HashMap<PathBuf, ProjectHandle>>,

    /// LRU order tracker. Most-recently-used at the back.
    lru_order: Mutex<VecDeque<PathBuf>>,

    /// Which project to use when `project_path` is omitted.
    default_project: RwLock<Option<PathBuf>>,

    /// Per-project indexing slots used to consolidate concurrent reindex requests.
    index_slots: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,

    /// Maximum number of projects to keep in memory.
    max_projects: usize,

    /// File watchers per project (kept alive by registry).
    watchers: Mutex<HashMap<PathBuf, IndexWatcher>>,

    /// Per-project staleness cache: (timestamp, stale_result).
    /// Avoids re-computing is_stale_fast on every tool call within a 2-second window.
    stale_cache: RwLock<HashMap<PathBuf, (std::time::Instant, bool)>>,
}

impl ProjectRegistry {
    /// Create a new registry with the given project capacity.
    pub fn new(max_projects: usize) -> Self {
        Self {
            projects: RwLock::new(HashMap::new()),
            lru_order: Mutex::new(VecDeque::new()),
            default_project: RwLock::new(None),
            index_slots: Mutex::new(HashMap::new()),
            max_projects,
            watchers: Mutex::new(HashMap::new()),
            stale_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Create a registry pre-loaded with one project (the initial startup project).
    pub fn with_initial_project(max_projects: usize, leindex: LeIndex) -> Self {
        let path = leindex.project_path().to_path_buf();
        let handle: ProjectHandle = Arc::new(Mutex::new(leindex));

        let mut map = HashMap::new();
        map.insert(path.clone(), handle.clone());

        let mut lru = VecDeque::new();
        lru.push_back(path.clone());

        let mut slots = HashMap::new();
        slots.insert(path.clone(), Arc::new(Mutex::new(())));
        let mut watchers = HashMap::new();
        watchers.insert(
            path.clone(),
            IndexWatcher::start(path.clone(), handle.clone())
                .expect("failed to start watcher for initial project"),
        );

        Self {
            projects: RwLock::new(map),
            lru_order: Mutex::new(lru),
            default_project: RwLock::new(Some(path)),
            index_slots: Mutex::new(slots),
            max_projects,
            watchers: Mutex::new(watchers),
            stale_cache: RwLock::new(HashMap::new()),
        }
    }

    /// Get an existing project, or create + load from storage (no auto-index).
    ///
    /// If `project_path` is `None`, returns the current default project.
    pub async fn get_or_load(
        &self,
        project_path: Option<&str>,
    ) -> Result<ProjectHandle, JsonRpcError> {
        let canonical = self.resolve_path(project_path).await?;

        {
            let projects = self.projects.read().await;
            if let Some(handle) = projects.get(&canonical) {
                self.touch_lru(&canonical).await;
                self.set_default(&canonical).await;
                return Ok(handle.clone());
            }
        }

        self.create_and_insert(canonical).await
    }

    /// Get or create a project, auto-indexing if it has no stored index.
    pub async fn get_or_create(
        &self,
        project_path: Option<&str>,
    ) -> Result<ProjectHandle, JsonRpcError> {
        let handle = self.get_or_load(project_path).await?;

        // Get canonical path for stale cache key
        let canonical = {
            let idx = handle.lock().await;
            idx.project_path().to_path_buf()
        };

        let (needs_index, needs_refresh) = {
            let idx = handle.lock().await;
            let not_indexed = !idx.is_indexed();

            // Check stale cache first (2-second TTL)
            let stale = if not_indexed {
                false
            } else {
                let cache = self.stale_cache.read().await;
                if let Some((ts, result)) = cache.get(&canonical) {
                    if ts.elapsed() < std::time::Duration::from_secs(2) {
                        *result
                    } else {
                        // Cache expired — compute fresh
                        drop(cache);
                        let fresh = idx.is_stale_fast();
                        self.stale_cache.write().await.insert(canonical.clone(), (std::time::Instant::now(), fresh));
                        fresh
                    }
                } else {
                    // No cache entry — compute and cache
                    drop(cache);
                    let fresh = idx.is_stale_fast();
                    self.stale_cache.write().await.insert(canonical.clone(), (std::time::Instant::now(), fresh));
                    fresh
                }
            };
            (not_indexed, stale)
        };

        if needs_index {
            let _ = self.index_handle(&handle, false).await?;
            // Invalidate stale cache after reindex
            self.stale_cache.write().await.remove(&canonical);
        } else if needs_refresh {
            let _ = self.index_handle(&handle, false).await?;
            debug!("Auto-refreshed stale index");
            // Invalidate stale cache after reindex
            self.stale_cache.write().await.remove(&canonical);
        }

        Ok(handle)
    }

    /// Explicitly index a project, with consolidation for concurrent requests.
    pub async fn index_project(
        &self,
        project_path: Option<&str>,
        force_reindex: bool,
    ) -> Result<IndexStats, JsonRpcError> {
        let handle = self.get_or_load(project_path).await?;
        self.index_handle(&handle, force_reindex).await
    }

    /// Number of projects currently in memory.
    pub async fn len(&self) -> usize {
        self.projects.read().await.len()
    }

    /// List all loaded project paths (for diagnostics).
    pub async fn loaded_projects(&self) -> Vec<PathBuf> {
        self.projects.read().await.keys().cloned().collect()
    }

    /// Explicitly evict a project from memory. Its data remains on disk.
    pub async fn evict(&self, path: &Path) {
        let removed = {
            let mut projects = self.projects.write().await;
            projects.remove(path)
        };

        if let Some(handle) = removed {
            if let Ok(mut idx) = handle.try_lock() {
                let _ = idx.close();
            }
            info!("Evicted project: {}", path.display());
        }

        let mut lru = self.lru_order.lock().await;
        lru.retain(|p| p != path);

        let mut slots = self.index_slots.lock().await;
        slots.remove(path);

        let mut watchers = self.watchers.lock().await;
        watchers.remove(path);
    }

    /// Resolve an optional `project_path` string to a canonical `PathBuf`.
    async fn resolve_path(&self, project_path: Option<&str>) -> Result<PathBuf, JsonRpcError> {
        if let Some(raw) = project_path {
            Path::new(raw).canonicalize().map_err(|e| {
                JsonRpcError::invalid_params(format!(
                    "Cannot resolve project_path '{}': {}",
                    raw, e
                ))
            })
        } else {
            let default = self.default_project.read().await;
            default.clone().ok_or_else(|| {
                JsonRpcError::invalid_params(
                    "No project_path provided and no project has been loaded yet. \
                     Pass project_path on the first call.",
                )
            })
        }
    }

    /// Create a new `LeIndex`, attempt to load from storage, and insert into
    /// the registry. Evicts LRU if at capacity.
    async fn create_and_insert(&self, canonical: PathBuf) -> Result<ProjectHandle, JsonRpcError> {
        self.evict_lru_if_needed().await;

        {
            let projects = self.projects.read().await;
            if let Some(handle) = projects.get(&canonical) {
                self.touch_lru(&canonical).await;
                self.set_default(&canonical).await;
                return Ok(handle.clone());
            }
        }

        let mut leindex = LeIndex::new(&canonical).map_err(|e| {
            JsonRpcError::init_failed(&canonical.display().to_string(), &e.to_string())
        })?;
        // PDG loading is deferred to first actual query (lazy loading).
        // This avoids loading 10-50MB PDG on operations that only check staleness.

        // Corruption detection and auto-repair
        let corruption =
            detect_corruption(&canonical).unwrap_or(crate::cli::errors::CorruptionStatus::Healthy);
        if !corruption.is_usable() {
            warn!(
                "Corruption detected in {}: {}. Auto-repairing...",
                canonical.display(),
                corruption.message()
            );
            let storage_path = canonical.join(".leindex");
            let _ = std::fs::remove_dir_all(&storage_path);

            let mut fresh = LeIndex::new(&canonical).map_err(|e| {
                JsonRpcError::init_failed(
                    &canonical.display().to_string(),
                    &format!("Original: {}. After wipe: {}", corruption.message(), e),
                )
            })?;
            fresh.index_project(true).map_err(|e| {
                JsonRpcError::indexing_failed(format!("Auto-repair reindex failed: {}", e))
            })?;
            leindex = fresh;
        }

        let handle: ProjectHandle = Arc::new(Mutex::new(leindex));

        {
            let mut projects = self.projects.write().await;
            projects.insert(canonical.clone(), handle.clone());
        }

        // Start file watcher for auto-reindex
        {
            let mut watchers = self.watchers.lock().await;
            if !watchers.contains_key(&canonical) {
                if let Ok(w) = IndexWatcher::start(canonical.clone(), handle.clone()) {
                    watchers.insert(canonical.clone(), w);
                }
            }
        }

        self.touch_lru(&canonical).await;
        self.set_default(&canonical).await;

        let mut slots = self.index_slots.lock().await;
        slots
            .entry(canonical.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())));

        info!(
            "Loaded project into registry: {} ({} total)",
            canonical.display(),
            self.projects.read().await.len()
        );

        Ok(handle)
    }

    /// Build a fresh index for the project behind `handle`, then swap it in.
    ///
    /// Uses a per-project slot lock so concurrent index requests coalesce.
    async fn index_handle(
        &self,
        handle: &ProjectHandle,
        force_reindex: bool,
    ) -> Result<IndexStats, JsonRpcError> {
        let project_path = {
            let idx = handle.lock().await;
            idx.project_path().to_path_buf()
        };

        let slot = self.index_slot_for(&project_path).await;
        let _slot_guard = slot.lock().await;

        if !force_reindex {
            let cached = {
                let idx = handle.lock().await;
                if idx.is_indexed() && !idx.is_stale_fast() {
                    Some(idx.get_stats().clone())
                } else {
                    None
                }
            };

            if let Some(stats) = cached {
                return Ok(stats);
            }
        }

        debug!(
            "Indexing project (consolidated): {} force_reindex={}",
            project_path.display(),
            force_reindex
        );

        let path_for_blocking = project_path.clone();
        let stats = tokio::task::spawn_blocking(move || {
            let mut temp = LeIndex::new(&path_for_blocking).map_err(|e| {
                JsonRpcError::init_failed(&path_for_blocking.display().to_string(), &e.to_string())
            })?;
            temp.index_project(force_reindex)
                .map_err(|e| JsonRpcError::indexing_failed(format!("Indexing failed: {}", e)))
        })
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))??;

        let mut fresh = LeIndex::new(&project_path).map_err(|e| {
            JsonRpcError::init_failed(&project_path.display().to_string(), &e.to_string())
        })?;
        fresh.load_from_storage().map_err(|e| {
            JsonRpcError::indexing_failed(format!("Failed to load indexed data: {}", e))
        })?;

        {
            let mut idx = handle.lock().await;
            *idx = fresh;
        }

        Ok(stats)
    }

    /// Get/create the per-project indexing slot.
    async fn index_slot_for(&self, path: &Path) -> Arc<Mutex<()>> {
        let mut slots = self.index_slots.lock().await;
        slots
            .entry(path.to_path_buf())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    /// Move `path` to the back of the LRU queue (most-recently-used).
    async fn touch_lru(&self, path: &Path) {
        let mut lru = self.lru_order.lock().await;
        lru.retain(|p| p != path);
        lru.push_back(path.to_path_buf());
    }

    /// Update the default project path.
    async fn set_default(&self, path: &Path) {
        let mut default = self.default_project.write().await;
        *default = Some(path.to_path_buf());
    }

    /// Evict the least-recently-used project if we're at or over capacity.
    async fn evict_lru_if_needed(&self) {
        let current_count = self.projects.read().await.len();
        if current_count < self.max_projects {
            return;
        }

        let evict_path = {
            let mut lru = self.lru_order.lock().await;
            lru.pop_front()
        };

        if let Some(path) = evict_path {
            let removed = {
                let mut projects = self.projects.write().await;
                projects.remove(&path)
            };

            if let Some(handle) = removed {
                if let Ok(mut idx) = handle.try_lock() {
                    let _ = idx.close();
                }
            }

            let mut slots = self.index_slots.lock().await;
            slots.remove(&path);

            info!(
                "Evicted LRU project: {} (capacity: {})",
                path.display(),
                self.max_projects
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_registry_creation() {
        let registry = ProjectRegistry::new(5);
        assert_eq!(registry.len().await, 0);
    }

    #[tokio::test]
    async fn test_registry_no_default_project_error() {
        let registry = ProjectRegistry::new(5);
        let result = registry.get_or_load(None).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_registry_nonexistent_path_error() {
        let registry = ProjectRegistry::new(5);
        let result = registry.get_or_load(Some("/nonexistent/path/12345")).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_registry_with_initial_project() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        assert_eq!(registry.len().await, 1);
        let handle = registry.get_or_load(None).await;
        assert!(handle.is_ok());
    }

    #[tokio::test]
    async fn test_registry_same_project_returns_same_handle() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let path_str = tmp.path().to_string_lossy().to_string();
        let h1 = registry.get_or_load(Some(&path_str)).await.unwrap();
        let h2 = registry.get_or_load(Some(&path_str)).await.unwrap();

        assert!(Arc::ptr_eq(&h1, &h2));
    }

    #[tokio::test]
    async fn test_registry_two_different_projects() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp1.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(tmp2.path().join("b.rs"), "fn b() {}\n").unwrap();

        let leindex = LeIndex::new(tmp1.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let p2 = tmp2.path().to_string_lossy().to_string();
        let h2 = registry.get_or_load(Some(&p2)).await.unwrap();

        assert_eq!(registry.len().await, 2);

        let p1 = tmp1.path().to_string_lossy().to_string();
        let h1 = registry.get_or_load(Some(&p1)).await.unwrap();
        assert!(!Arc::ptr_eq(&h1, &h2));
    }

    #[tokio::test]
    async fn test_registry_eviction_at_capacity() {
        let dirs: Vec<_> = (0..3)
            .map(|i| {
                let d = tempfile::tempdir().unwrap();
                std::fs::write(d.path().join(format!("f{}.rs", i)), "fn f() {}\n").unwrap();
                d
            })
            .collect();

        let leindex = LeIndex::new(dirs[0].path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(2, leindex);

        let p1 = dirs[1].path().to_string_lossy().to_string();
        let _ = registry.get_or_load(Some(&p1)).await.unwrap();
        assert_eq!(registry.len().await, 2);

        let p2 = dirs[2].path().to_string_lossy().to_string();
        let _ = registry.get_or_load(Some(&p2)).await.unwrap();
        assert_eq!(registry.len().await, 2);

        let loaded = registry.loaded_projects().await;
        let canonical0 = dirs[0].path().canonicalize().unwrap();
        assert!(!loaded.contains(&canonical0));
    }

    #[tokio::test]
    async fn test_registry_evicted_project_reloads() {
        let dirs: Vec<_> = (0..3)
            .map(|i| {
                let d = tempfile::tempdir().unwrap();
                std::fs::write(d.path().join(format!("f{}.rs", i)), "fn f() {}\n").unwrap();
                d
            })
            .collect();

        let leindex = LeIndex::new(dirs[0].path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(2, leindex);

        let p1 = dirs[1].path().to_string_lossy().to_string();
        let _ = registry.get_or_load(Some(&p1)).await.unwrap();

        let p2 = dirs[2].path().to_string_lossy().to_string();
        let _ = registry.get_or_load(Some(&p2)).await.unwrap();

        let p0 = dirs[0].path().to_string_lossy().to_string();
        let result = registry.get_or_load(Some(&p0)).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_registry_default_project_tracks_last_used() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp1.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(tmp2.path().join("b.rs"), "fn b() {}\n").unwrap();

        let leindex = LeIndex::new(tmp1.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let h1 = registry.get_or_load(None).await.unwrap();
        let path1 = h1.lock().await.project_path().to_path_buf();
        assert_eq!(path1, tmp1.path().canonicalize().unwrap());

        let p2 = tmp2.path().to_string_lossy().to_string();
        let _ = registry.get_or_load(Some(&p2)).await.unwrap();

        let h2 = registry.get_or_load(None).await.unwrap();
        let path2 = h2.lock().await.project_path().to_path_buf();
        assert_eq!(path2, tmp2.path().canonicalize().unwrap());
    }
}
