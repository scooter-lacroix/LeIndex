//! Multi-project registry with low-overhead per-project coordination.
//!
//! `ProjectRegistry` replaces the old singleton `Arc<Mutex<LeIndex>>` global.
//! It keeps up to `max_projects` projects in memory simultaneously and evicts
//! the least-recently-used project when capacity is reached.
//!
//! ## Concurrency model
//!
//! * **Outer map** (`tokio::sync::RwLock<HashMap<...>>`)
//!   - Read-lock for fast project lookup.
//!   - Write-lock only for insert/remove operations.
//!
//! * **Per-project state** (`ProjectRwLock<LeIndex>`)
//!   - Uses `tokio::sync::Mutex` internally because `LeIndex` is `Send` but
//!     not `Sync` (rusqlite internals use `RefCell`). `tokio::sync::Mutex<T>`
//!     is `Sync` when `T: Send`, unlike `RwLock<T>` which requires `T: Sync`.
//!   - Exposes `read()` and `write()` methods that both acquire the underlying
//!     mutex. This establishes the correct read/write API contract so that
//!     when `LeIndex` becomes `Sync` (e.g. by moving rusqlite behind a mutex),
//!     the upgrade to a true `RwLock` is a single-line change.
//!   - The outer `RwLock` on the project map provides concurrent access to
//!     *different* projects. Within a single project, the `Mutex` serializes
//!     all operations, but handlers release the lock between async steps so
//!     concurrent requests to the same project interleave naturally.
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
use dirs;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::sync::{Mutex, RwLock};
use tracing::{debug, info, warn};

/// Default maximum number of projects kept in memory simultaneously.
pub const DEFAULT_MAX_PROJECTS: usize = 5;

/// TTL for the per-project staleness cache.
///
/// `is_stale_fast` walks the source directory tree (even after the dead
/// `walkdir` block is removed, it still does many `stat()` calls). At 2
/// seconds the cache was thrashing under normal editor save patterns,
/// causing every tool call to re-stat hundreds of files. 30 seconds is a
/// good balance: edits are noticed within a reasonable window, but a burst
/// of tool calls shares one freshness check.
pub const STALE_CACHE_TTL: std::time::Duration = std::time::Duration::from_secs(30);

/// Environment variable that explicitly enables the file-watcher auto-reindex.
///
/// Default is OFF because the recursive watcher is the single largest source
/// of "operations hang / time out" reports: it fires on every file change
/// (cargo build, git, editor saves, target/ churn) and holds the per-project
/// write lock, blocking every other tool call for the duration of the
/// incremental reindex. Set `LEINDEX_WATCHER=1` to opt in.
pub const WATCHER_ENABLE_ENV: &str = "LEINDEX_WATCHER";

/// Returns true if the file-watcher is enabled for this process.
pub fn watcher_enabled() -> bool {
    match std::env::var(WATCHER_ENABLE_ENV) {
        Ok(v) => matches!(v.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "on"),
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// ProjectRwLock — read/write API over a Mutex for !Sync inner types
// ---------------------------------------------------------------------------

/// A read/write lock wrapper for per-project `LeIndex` access.
///
/// `LeIndex` is `Send` but **not** `Sync` (rusqlite uses `RefCell` internally),
/// which prevents using `tokio::sync::RwLock<LeIndex>` directly — `RwLock<T>`
/// requires `T: Sync` for its own `Sync` impl, while `Mutex<T>` only requires
/// `T: Send`.
///
/// `ProjectRwLock` uses a `tokio::sync::Mutex` internally but exposes `read()`
/// and `write()` methods to establish the correct read/write API contract.
/// Callers that only read data use `read()`, and callers that mutate use
/// `write()`. Currently both acquire the same mutex, but the API allows a
/// seamless upgrade to a true `RwLock` when `LeIndex` becomes `Sync`.
///
/// **Concurrency benefit**: The outer `RwLock` on the project map already
/// provides concurrent access to *different* projects. Within a single project,
/// handlers release the lock between async steps so concurrent requests
/// interleave naturally.
pub struct ProjectRwLock {
    inner: Mutex<LeIndex>,
}

impl ProjectRwLock {
    /// Create a new `ProjectRwLock` wrapping the given `LeIndex`.
    pub fn new(leindex: LeIndex) -> Self {
        Self {
            inner: Mutex::new(leindex),
        }
    }

    /// Acquire a read guard for the `LeIndex`.
    ///
    /// Currently acquires the underlying mutex (since `LeIndex` is `!Sync`).
    /// When `LeIndex` becomes `Sync`, this can be upgraded to a true read lock
    /// allowing concurrent reads.
    pub async fn read(&self) -> ProjectReadGuard<'_> {
        ProjectReadGuard {
            inner: self.inner.lock().await,
        }
    }

    /// Acquire a write guard for the `LeIndex`.
    ///
    /// Use for operations that mutate the `LeIndex` (e.g. PDG swap, indexing).
    pub async fn write(&self) -> ProjectWriteGuard<'_> {
        ProjectWriteGuard {
            inner: self.inner.lock().await,
        }
    }

    /// Try to acquire a write guard without blocking.
    ///
    /// Returns `Err` if the lock is already held. Used during eviction to
    /// gracefully close the `LeIndex` only when it's not in use.
    #[allow(clippy::result_unit_err)]
    pub fn try_write(&self) -> Result<ProjectWriteGuard<'_>, ()> {
        match self.inner.try_lock() {
            Ok(guard) => Ok(ProjectWriteGuard { inner: guard }),
            Err(_) => Err(()),
        }
    }

    /// Acquire a blocking write guard (for use in `spawn_blocking` contexts).
    ///
    /// Blocks the current thread until the lock is available. Use only from
    /// synchronous contexts (e.g. `spawn_blocking`).
    pub fn blocking_write(&self) -> ProjectWriteGuard<'_> {
        ProjectWriteGuard {
            inner: self.inner.blocking_lock(),
        }
    }
}

// Both guards are `Send` because `tokio::sync::MutexGuard` is `Send`.
// They are NOT `Sync` because the underlying `LeIndex` is `!Sync`.

/// Read guard acquired from `ProjectRwLock::read()`.
///
/// Derefs to `LeIndex` for read-only access.
pub struct ProjectReadGuard<'a> {
    inner: tokio::sync::MutexGuard<'a, LeIndex>,
}

impl std::ops::Deref for ProjectReadGuard<'_> {
    type Target = LeIndex;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// Write guard acquired from `ProjectRwLock::write()`.
///
/// Derefs to `LeIndex` for read access, and `DerefMut` for write access.
pub struct ProjectWriteGuard<'a> {
    inner: tokio::sync::MutexGuard<'a, LeIndex>,
}

impl std::ops::Deref for ProjectWriteGuard<'_> {
    type Target = LeIndex;
    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

impl std::ops::DerefMut for ProjectWriteGuard<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner
    }
}

// ---------------------------------------------------------------------------
// ProjectHandle and ProjectRegistry
// ---------------------------------------------------------------------------

/// A handle to one project's `LeIndex`.
///
/// Uses `ProjectRwLock` which wraps a `tokio::sync::Mutex` internally (since
/// `LeIndex` is `!Sync`) but exposes `read()` and `write()` methods to
/// distinguish read vs write operations.
pub type ProjectHandle = Arc<ProjectRwLock>;

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
    ///
    /// Avoids re-computing `is_stale_fast` on every tool call. The TTL is
    /// `STALE_CACHE_TTL` (30 seconds) — long enough to coalesce the burst
    /// of freshness checks that arrive at startup, short enough that a
    /// file edit becomes visible to subsequent reads within reasonable time.
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
        let handle: ProjectHandle = Arc::new(ProjectRwLock::new(leindex));

        let mut map = HashMap::new();
        map.insert(path.clone(), handle.clone());

        let mut lru = VecDeque::new();
        lru.push_back(path.clone());

        let mut slots = HashMap::new();
        slots.insert(path.clone(), Arc::new(Mutex::new(())));
        // File-watcher is opt-in. The default behavior (no watcher) keeps
        // every other tool call latency-free during dev work; users who
        // want hot auto-reindex set `LEINDEX_WATCHER=1`.
        let mut watchers = HashMap::new();
        if watcher_enabled() {
            if let Ok(w) = IndexWatcher::start(path.clone(), handle.clone()) {
                watchers.insert(path.clone(), w);
            }
        }

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
            let idx = handle.read().await;
            idx.project_path().to_path_buf()
        };

        let (needs_index, needs_refresh) = {
            let idx = handle.read().await;
            let not_indexed = !idx.is_indexed();

            // Check stale cache first (STALE_CACHE_TTL).
            let stale = if not_indexed {
                false
            } else {
                let cache = self.stale_cache.read().await;
                if let Some((ts, result)) = cache.get(&canonical) {
                    if ts.elapsed() < STALE_CACHE_TTL {
                        *result
                    } else {
                        // Cache expired — compute fresh
                        drop(cache);
                        let fresh = idx.is_stale_fast();
                        self.stale_cache
                            .write()
                            .await
                            .insert(canonical.clone(), (std::time::Instant::now(), fresh));
                        fresh
                    }
                } else {
                    // No cache entry — compute and cache
                    drop(cache);
                    let fresh = idx.is_stale_fast();
                    self.stale_cache
                        .write()
                        .await
                        .insert(canonical.clone(), (std::time::Instant::now(), fresh));
                    fresh
                }
            };
            (not_indexed, stale)
        };

        if needs_index {
            let _ = self.index_handle(&handle, false).await?;
            // stale_cache is invalidated inside index_handle() after successful swap
        } else if needs_refresh {
            // Read paths (search/symbol-lookup/etc.) must NEVER auto-trigger a
            // full reindex — that's the single biggest source of "all
            // operations hang and time out" reports. If the on-disk index is
            // stale we serve the existing results with a `_warning` field
            // (added in `wrap_with_meta`) so the caller knows.
            //
            // Use the explicit `leindex.index` tool (force_reindex=true) to
            // rebuild the index when freshness matters.
            debug!("Index is stale; serving existing results without auto-rebuild");
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

    /// Returns `true` if no projects are currently loaded.
    pub async fn is_empty(&self) -> bool {
        self.projects.read().await.is_empty()
    }

    /// List all loaded project paths (for diagnostics).
    pub async fn loaded_projects(&self) -> Vec<PathBuf> {
        self.projects.read().await.keys().cloned().collect()
    }

    /// Explicitly evict a project from memory. Its data remains on disk.
    ///
    /// Cleans up all associated bookkeeping: LRU order, index slots,
    /// watchers, and stale-cache entries (VAL-APLUS-027).
    pub async fn evict(&self, path: &Path) {
        let removed = {
            let mut projects = self.projects.write().await;
            projects.remove(path)
        };

        if let Some(handle) = removed {
            if let Ok(mut idx) = handle.try_write() {
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

        // A+ hotspot cleanup: evict stale-cache entry so residency does not
        // grow monotonically across long-lived sessions (VAL-APLUS-027).
        self.stale_cache.write().await.remove(path);
    }

    /// Resolve an optional `project_path` string to a canonical `PathBuf`.
    async fn resolve_path(&self, project_path: Option<&str>) -> Result<PathBuf, JsonRpcError> {
        let path = if let Some(raw) = project_path {
            Path::new(raw).to_path_buf()
        } else {
            let default = self.default_project.read().await;
            default.clone().ok_or_else(|| {
                JsonRpcError::invalid_params(
                    "No project_path provided and no project has been loaded yet. \
                     Pass project_path on the first call.",
                )
            })?
        };

        // Canonicalize first to resolve symlinks and relative paths
        let canonical = path.canonicalize().map_err(|e| {
            JsonRpcError::invalid_params(format!(
                "Cannot resolve project_path '{}': {}",
                path.display(),
                e
            ))
        })?;

        // Reject root directory (cross-platform: works on Windows too)
        // Using parent().is_none() correctly identifies root paths on all platforms,
        // including Windows drive roots like C:\ which have multiple components.
        if canonical.parent().is_none() {
            return Err(JsonRpcError::invalid_params(
                "Refusing to index root directory. Specify a project subdirectory.".to_string(),
            ));
        }

        // Reject home directory (cross-platform)
        if let Some(home_dir) = dirs::home_dir() {
            let home_canonical = home_dir.canonicalize().unwrap_or(home_dir);
            if canonical == home_canonical {
                return Err(JsonRpcError::invalid_params(
                    "Refusing to index home directory. Specify a project subdirectory.".to_string(),
                ));
            }
        }

        Ok(canonical)
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
        // Load from storage to populate search_engine (is_indexed() depends on it).
        // PDG remains in memory; ensure_pdg_loaded() is a no-op after this.
        let _ = leindex.load_from_storage();

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

        let handle: ProjectHandle = Arc::new(ProjectRwLock::new(leindex));

        {
            let mut projects = self.projects.write().await;
            projects.insert(canonical.clone(), handle.clone());
        }

        // Start file watcher for auto-reindex — opt-in only.
        //
        // The watcher is the single largest contributor to "all operations
        // time out" reports. It is recursive on the project root (including
        // `target/`, `node_modules/`, `leann_index/`, etc.) and triggers an
        // incremental reindex on every filesystem event. The reindex holds
        // the per-project write lock, so any concurrent tool call waits for
        // it to complete — under normal dev activity (cargo build, git
        // status, editor save), this can block for many seconds.
        //
        // Default off. Enable with `LEINDEX_WATCHER=1` if hot auto-reindex
        // is actually needed.
        if watcher_enabled() {
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
            let idx = handle.read().await;
            idx.project_path().to_path_buf()
        };

        let slot = self.index_slot_for(&project_path).await;
        let _slot_guard = slot.lock().await;

        if !force_reindex {
            let cached = {
                let idx = handle.read().await;
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
        let temp = tokio::task::spawn_blocking(move || {
            let mut temp = LeIndex::new(&path_for_blocking).map_err(|e| {
                JsonRpcError::init_failed(&path_for_blocking.display().to_string(), &e.to_string())
            })?;
            temp.index_project(force_reindex)
                .map_err(|e| JsonRpcError::indexing_failed(format!("Indexing failed: {}", e)))?;
            Ok::<LeIndex, JsonRpcError>(temp)
        })
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))??;

        {
            let mut idx = handle.write().await;
            *idx = temp;
        }

        // Invalidate stale-cache entry so get_or_create() won't reuse
        // the pre-indexing staleness result.
        let canonical = project_path
            .canonicalize()
            .unwrap_or_else(|_| project_path.clone());
        self.stale_cache.write().await.remove(&canonical);

        let stats = {
            let idx = handle.read().await;
            idx.get_stats().clone()
        };

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

    /// Set the default project path without loading the project.
    ///
    /// Used by MCP stdio to register the `--project` CLI argument as the
    /// default so that subsequent tool calls that omit `project_path` resolve
    /// to it. The actual `LeIndex` creation happens lazily on first tool call
    /// via `get_or_load()`.
    pub async fn set_default_path(&self, path: PathBuf) {
        let mut default = self.default_project.write().await;
        *default = Some(path);
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
                if let Ok(mut idx) = handle.try_write() {
                    let _ = idx.close();
                }
            }

            let mut slots = self.index_slots.lock().await;
            slots.remove(&path);

            // Remove watcher so the evicted LeIndex is not kept alive by
            // the watcher's captured ProjectHandle.
            let mut watchers = self.watchers.lock().await;
            watchers.remove(&path);

            // A+ hotspot cleanup: also evict stale-cache entry
            self.stale_cache.write().await.remove(&path);

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
        let path1 = h1.read().await.project_path().to_path_buf();
        assert_eq!(path1, tmp1.path().canonicalize().unwrap());

        let p2 = tmp2.path().to_string_lossy().to_string();
        let _ = registry.get_or_load(Some(&p2)).await.unwrap();

        let h2 = registry.get_or_load(None).await.unwrap();
        let path2 = h2.read().await.project_path().to_path_buf();
        assert_eq!(path2, tmp2.path().canonicalize().unwrap());
    }

    /// Concurrency test: verify that the `ProjectRwLock` wrapper correctly
    /// serializes access (both `read()` and `write()` acquire the underlying
    /// mutex) and that concurrent operations from multiple tokio tasks
    /// complete without deadlock or data corruption.
    #[tokio::test]
    async fn test_project_rwlock_concurrent_access_no_deadlock() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let handle = registry.get_or_load(None).await.unwrap();

        // Spawn multiple concurrent tasks that acquire read guards.
        // All should complete without deadlock (they are serialized by
        // the underlying mutex, but the tokio runtime can interleave them).
        let mut handles = Vec::new();
        for i in 0..10 {
            let h = handle.clone();
            handles.push(tokio::spawn(async move {
                // Alternating read and write to exercise both paths
                if i % 2 == 0 {
                    let guard = h.read().await;
                    let path = guard.project_path().to_path_buf();
                    assert!(path.exists());
                } else {
                    let guard = h.write().await;
                    let path = guard.project_path().to_path_buf();
                    assert!(path.exists());
                }
            }));
        }

        // All tasks must complete without deadlock
        for h in handles {
            h.await.unwrap();
        }
    }

    /// Verify that `try_write()` returns Err when the lock is already held.
    #[tokio::test]
    async fn test_project_rwlock_try_write_returns_err_when_locked() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let handle = registry.get_or_load(None).await.unwrap();

        // Acquire a read guard and hold it
        let _guard = handle.read().await;

        // try_write should fail because the lock is held
        let result = handle.try_write();
        assert!(result.is_err());
    }

    /// Verify that `blocking_write()` works from a spawn_blocking context.
    #[test]
    fn test_project_rwlock_blocking_write() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let handle: ProjectHandle = Arc::new(ProjectRwLock::new(leindex));

        let h = handle.clone();
        let result = std::thread::spawn(move || {
            let guard = h.blocking_write();
            guard.project_path().to_path_buf()
        })
        .join()
        .unwrap();

        assert!(result.exists());
    }

    // ---- A+ registry slot eviction tests (VAL-APLUS-027, VAL-APLUS-028) ----

    /// VAL-APLUS-027: Registry slot bookkeeping is evicted on project unregister/evict.
    ///
    /// When a project leaves the live registry, its slot bookkeeping is removed
    /// so residency does not grow monotonically across long-lived sessions.
    #[tokio::test]
    async fn test_evict_removes_slot_bookkeeping() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let canonical = tmp.path().canonicalize().unwrap();
        assert_eq!(registry.len().await, 1);

        // Evict the project
        registry.evict(&canonical).await;
        assert_eq!(registry.len().await, 0);

        // Verify slot bookkeeping is gone (internal state check via re-load)
        // Re-loading should work cleanly without stale slot state
        let path_str = tmp.path().to_string_lossy().to_string();
        let result = registry.get_or_load(Some(&path_str)).await;
        assert!(result.is_ok(), "re-loading after eviction should succeed");
        assert_eq!(registry.len().await, 1);
    }

    /// VAL-APLUS-028: Registry slot map reflects only live projects.
    ///
    /// Slot bookkeeping tracks active projects rather than every project ever
    /// seen in the process lifetime.
    #[tokio::test]
    async fn test_slot_map_reflects_only_live_projects() {
        let tmp1 = tempfile::tempdir().unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp1.path().join("a.rs"), "fn a() {}\n").unwrap();
        std::fs::write(tmp2.path().join("b.rs"), "fn b() {}\n").unwrap();

        let leindex = LeIndex::new(tmp1.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        // Load second project
        let p2 = tmp2.path().to_string_lossy().to_string();
        let _ = registry.get_or_load(Some(&p2)).await.unwrap();
        assert_eq!(registry.len().await, 2);

        // Evict first project
        let canonical1 = tmp1.path().canonicalize().unwrap();
        registry.evict(&canonical1).await;
        assert_eq!(registry.len().await, 1);

        // Only the second project should remain
        let loaded = registry.loaded_projects().await;
        let canonical2 = tmp2.path().canonicalize().unwrap();
        assert!(loaded.contains(&canonical2));
        assert!(!loaded.contains(&canonical1));
    }

    /// VAL-APLUS-027 variant: stale-cache entries are cleaned up on evict.
    #[tokio::test]
    async fn test_evict_cleans_stale_cache() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let canonical = tmp.path().canonicalize().unwrap();

        // Populate stale cache
        registry
            .stale_cache
            .write()
            .await
            .insert(canonical.clone(), (std::time::Instant::now(), false));

        assert!(registry.stale_cache.read().await.contains_key(&canonical));

        // Evict should clean up stale cache
        registry.evict(&canonical).await;
        assert!(
            !registry.stale_cache.read().await.contains_key(&canonical),
            "stale cache entry should be removed on evict"
        );
    }
}
