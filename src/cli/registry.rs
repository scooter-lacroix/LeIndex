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
use crate::cli::index_job::{IndexJobSnapshot, IndexJobState, JobPaths, JobStatus, new_job_id};
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

/// Cooldown after a failed auto-index before `get_or_create` retries the full
/// index for the same project.
///
/// A degraded index (e.g. a storage layer that cannot persist the PDG) would
/// otherwise trigger a full, slow index attempt on *every* tool call. The
/// cooldown bounds that to one attempt per window while still letting a
/// recovered environment (disk space freed, lock released, schema repaired)
/// reindex on the next call after the window elapses.
pub const INDEX_ATTEMPT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(30);

/// Environment variable overriding how long a tool call waits for a cold
/// first-use index before continuing in the background (milliseconds).
pub const AUTO_INDEX_WAIT_ENV: &str = "LEINDEX_AUTO_INDEX_WAIT_MS";

/// Default wait for a cold first-use index inside a tool call.
///
/// Small projects finish inside the window and the call returns real
/// results; larger ones hand back immediately with the index still building
/// (see [`ProjectRegistry::get_or_create`]) instead of stalling the client.
pub const DEFAULT_AUTO_INDEX_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Resolve the bounded cold-index wait (`0` = do not wait at all).
pub fn auto_index_wait() -> std::time::Duration {
    std::env::var(AUTO_INDEX_WAIT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_millis)
        .unwrap_or(DEFAULT_AUTO_INDEX_WAIT)
}

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
        Ok(v) => matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        ),
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

    /// Acquire a synchronous read guard from a `spawn_blocking` context.
    pub fn blocking_read(&self) -> ProjectReadGuard<'_> {
        ProjectReadGuard {
            inner: self.inner.blocking_lock(),
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

/// RAII guard that clears a project's `incremental_refresh_guard` flag on drop.
///
/// `maybe_incremental_refresh` sets the flag to `true` before spawning the
/// background index, and must clear it on EVERY exit path — ok, error, OR
/// panic. If a panic in `index_project` skipped the clear, the flag would stay
/// `true` forever and permanently disable background refreshes for that
/// project (kilo CRITICAL). Tokio unwinds a panicking task's stack, so this
/// Drop runs even on panic.
struct RefreshGuard {
    registry: Arc<ProjectRegistry>,
    path: PathBuf,
}

impl Drop for RefreshGuard {
    fn drop(&mut self) {
        if let Ok(mut map) = self.registry.incremental_refresh_guard.try_lock() {
            map.insert(self.path.clone(), false);
        }
    }
}

/// Multi-project registry.
pub struct ProjectRegistry {
    /// Canonical path -> project handle. `pub(crate)` for the D-2 eviction
    /// impl in `registry_evict.rs`.
    pub(crate) projects: RwLock<HashMap<PathBuf, ProjectHandle>>,

    /// LRU order tracker. Most-recently-used at the back.
    lru_order: Mutex<VecDeque<PathBuf>>,

    /// Which project to use when `project_path` is omitted.
    default_project: RwLock<Option<PathBuf>>,

    /// Per-project indexing slots used to consolidate concurrent reindex requests.
    index_slots: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,

    /// Owned indexing jobs survive the MCP request that started them.
    index_jobs: Mutex<HashMap<PathBuf, Arc<IndexJobState>>>,

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

    /// Per-project timestamp of the last failed auto-index attempt.
    ///
    /// Populated by `get_or_create` when the best-effort auto-index fails;
    /// a fresh entry suppresses further full-index attempts until
    /// `INDEX_ATTEMPT_COOLDOWN` elapses so a degraded storage layer does not
    /// serialize a full reindex behind every tool call.
    failed_index_attempts: RwLock<HashMap<PathBuf, std::time::Instant>>,

    /// Per-project incremental refresh guard. When `true`, an incremental
    /// refresh is in progress for that project and new requests skip the
    /// refresh to avoid duplicate work.
    incremental_refresh_guard: Mutex<HashMap<PathBuf, bool>>,

    /// Per-project last-access timestamps for idle-engine eviction (D-2).
    ///
    /// Touched on every `get_or_load`; `evict_idle_engines` (defined in
    /// `registry_evict.rs`) drops projects that are unused for
    /// `[mcp] engine_max_idle_secs` so a long-lived MCP process releases
    /// loaded-project mmaps instead of retaining every project it ever
    /// touched (memory-pressure remediation).
    pub(crate) last_used: RwLock<HashMap<PathBuf, std::time::Instant>>,

    /// One-shot mode (CLI `tools run`): the process exits right after the
    /// tool call, so a spawned background incremental refresh can never
    /// finish — it just logs a cancellation warning and races the exit.
    /// When set, `maybe_incremental_refresh` is a no-op; the next invocation
    /// re-evaluates staleness anyway.
    one_shot: std::sync::atomic::AtomicBool,
}

impl ProjectRegistry {
    /// Create a new registry with the given project capacity.
    pub fn new(max_projects: usize) -> Self {
        Self {
            projects: RwLock::new(HashMap::new()),
            lru_order: Mutex::new(VecDeque::new()),
            default_project: RwLock::new(None),
            index_slots: Mutex::new(HashMap::new()),
            index_jobs: Mutex::new(HashMap::new()),
            max_projects,
            watchers: Mutex::new(HashMap::new()),
            stale_cache: RwLock::new(HashMap::new()),
            failed_index_attempts: RwLock::new(HashMap::new()),
            incremental_refresh_guard: Mutex::new(HashMap::new()),
            last_used: RwLock::new(HashMap::new()),
            one_shot: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Mark this registry as belonging to a one-shot process (CLI
    /// `tools run`): background incremental refreshes are suppressed because
    /// the process exits before they could complete.
    pub fn mark_one_shot(&self) {
        self.one_shot
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Whether this registry belongs to a one-shot process (CLI `tools run`).
    /// Handlers use this to decide between inline post-processing (one-shot:
    /// the process exits when the response is written, so background work
    /// would be killed) and spawned background work (server: keeps the
    /// response latency off the slow path).
    pub fn is_one_shot(&self) -> bool {
        self.one_shot.load(std::sync::atomic::Ordering::Acquire)
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

        let mut last_used = HashMap::new();
        last_used.insert(path.clone(), std::time::Instant::now());

        Self {
            projects: RwLock::new(map),
            lru_order: Mutex::new(lru),
            default_project: RwLock::new(Some(path)),
            index_slots: Mutex::new(slots),
            index_jobs: Mutex::new(HashMap::new()),
            max_projects,
            watchers: Mutex::new(watchers),
            stale_cache: RwLock::new(HashMap::new()),
            failed_index_attempts: RwLock::new(HashMap::new()),
            incremental_refresh_guard: Mutex::new(HashMap::new()),
            last_used: RwLock::new(last_used),
            one_shot: std::sync::atomic::AtomicBool::new(false),
        }
    }

    /// Invalidate the staleness cache entry for `path`.
    ///
    /// Write handlers (`edit-apply`, `write-file`, `rename-symbol`)
    /// must call this after a successful write so that the next
    /// read tool re-runs `is_stale_fast` instead of reusing a
    /// pre-write `false` cached result.
    ///
    /// `path` **must** be an already-canonicalized project path (e.g.
    /// the return value of [`ProjectHandle::project_path`]). The cache
    /// key is built from [`LeIndex::project_path`], which is
    /// canonicalized at construction time. Every built-in caller
    /// passes `guard.project_path().to_path_buf()`, which satisfies
    /// this contract.
    pub async fn invalidate_stale_cache(&self, path: &Path) {
        self.stale_cache.write().await.remove(path);
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
                // N-13: an external rebuild (CLI --force, another MCP
                // server) advances the persisted CURRENT pointer while this
                // process keeps serving its in-memory snapshot —
                // diagnostics then reported generation-4 stats under a
                // generation-6 "fresh" footer. CURRENT is a tiny read;
                // when it has moved past the hydrated generation, evict so
                // this call re-hydrates from the new generation.
                let advanced = {
                    let idx = handle.read().await;
                    idx.hydrated_generation().is_some_and(|hydrated| {
                        let storage_root =
                            crate::cli::leindex::resolve_existing_storage_path(&canonical)
                                .unwrap_or_else(|| canonical.join(".leindex"));
                        crate::storage::generation::lease::read_current_generation(&storage_root)
                            .is_some_and(|disk| disk > hydrated)
                    })
                };
                if advanced {
                    drop(projects);
                    warn!(
                        project = %canonical.display(),
                        "Persisted generation advanced past the hydrated snapshot; re-hydrating"
                    );
                    self.evict(&canonical).await;
                    return self.create_and_insert(canonical).await;
                }
                self.touch_lru(&canonical).await;
                self.touch_last_used(&canonical).await;
                return Ok(handle.clone());
            }
        }

        self.create_and_insert(canonical).await
    }

    /// Get or create a project, auto-indexing if it has no stored index.
    pub async fn get_or_create(
        self: &Arc<Self>,
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
            // Auto-index is best-effort: a failed index attempt (degraded
            // storage, transient lock contention, disk pressure) must not turn
            // every subsequent tool call into a hard error. Tools that can
            // operate without an index (read, edit, text search, git status)
            // degrade gracefully; tools that need the index report their own
            // "not indexed" errors with remediation guidance.
            let skip_attempt = {
                let cache = self.failed_index_attempts.read().await;
                cache
                    .get(&canonical)
                    .is_some_and(|ts| ts.elapsed() < INDEX_ATTEMPT_COOLDOWN)
            };
            if skip_attempt {
                debug!(
                    project = %canonical.display(),
                    "Skipping auto-index within cooldown after a recent failure"
                );
            } else if let Err(error) = self.auto_index(&handle, &canonical).await {
                warn!(
                    project = %canonical.display(),
                    "Auto-index failed; serving project without a fresh index: {error}"
                );
                self.failed_index_attempts
                    .write()
                    .await
                    .insert(canonical.clone(), std::time::Instant::now());
            }
            // stale_cache is invalidated inside index_handle() after successful swap
        } else if needs_refresh {
            // The index is stale but still usable. Serve existing results
            // immediately and trigger a lightweight incremental refresh in
            // the background. The next request will see fresh data.
            //
            // Read paths must NEVER auto-trigger a FULL reindex (that was the
            // single biggest source of "all operations hang" reports), but an
            // incremental refresh only re-parses changed files and is safe to
            // run opportunistically.
            self.maybe_incremental_refresh(&handle, &canonical);
            debug!(
                "Index is stale; serving existing results while incremental refresh runs in background"
            );
        }

        Ok(handle)
    }

    /// First-use indexing for a tool call on an unindexed project.
    ///
    /// One-shot processes (the CLI) index inline: the process exits with the
    /// response, so there is nothing to keep responsive. A long-lived MCP
    /// server must never hold a request open for a whole cold index — clients
    /// time out, and on the stdio transport everything queued behind it
    /// stalls. It starts (or coalesces with) the owned, detached index job and
    /// waits at most [`auto_index_wait`]; on timeout the job keeps running in
    /// the background and the call proceeds against whatever is resident, so
    /// index-dependent tools answer "not indexed / indexing in progress"
    /// immediately and the next call sees the finished index.
    async fn auto_index(
        self: &Arc<Self>,
        handle: &ProjectHandle,
        canonical: &Path,
    ) -> Result<(), JsonRpcError> {
        if self.is_one_shot() {
            return self.index_handle(handle, false).await.map(|_| ());
        }
        let path_string = canonical.to_string_lossy().into_owned();
        let job = self.start_index_job(Some(&path_string), false, true);
        match tokio::time::timeout(auto_index_wait(), job).await {
            Ok(Ok(snapshot)) if snapshot.status == JobStatus::Failed => {
                Err(JsonRpcError::indexing_failed(
                    snapshot
                        .last_error
                        .unwrap_or_else(|| "background index job failed".to_string()),
                ))
            }
            Ok(Ok(_)) => Ok(()),
            Ok(Err(error)) => Err(error),
            Err(_) => {
                info!(
                    project = %canonical.display(),
                    wait_ms = auto_index_wait().as_millis() as u64,
                    "Cold index still running; continuing in the background"
                );
                Ok(())
            }
        }
    }

    /// Trigger a lightweight incremental index refresh in the background if one
    /// is not already running for this project.
    ///
    /// This is non-blocking: the caller proceeds with existing data and the
    /// next request will see fresh results. The incremental refresh re-parses
    /// only changed files and updates their symbols/edges/embeddings in-place.
    /// A per-project guard prevents concurrent refreshes.
    pub fn maybe_incremental_refresh(
        self: &Arc<Self>,
        _handle: &ProjectHandle,
        project_path: &Path,
    ) {
        // One-shot processes (CLI `tools run`) exit before a spawned
        // refresh could finish; the cancellation would only log noise and
        // race the exit. The next invocation re-evaluates staleness.
        if self.one_shot.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        // Check if a refresh is already in progress for this project.
        {
            let guard = self.incremental_refresh_guard.try_lock();
            match guard {
                Ok(mut map) => {
                    if *map.get(project_path).unwrap_or(&false) {
                        return;
                    }
                    map.insert(project_path.to_path_buf(), true);
                }
                Err(_) => {
                    // Lock contention means another thread is managing refreshes;
                    // skip this one.
                    return;
                }
            }
        }

        let registry = Arc::clone(self);
        let path = project_path.to_path_buf();
        let path_string = path.to_string_lossy().into_owned();

        tokio::spawn(async move {
            // Panic-safe guard: clears the refresh flag on drop (ok, error, OR
            // panic). Without this, a panic in `index_project` would leave the
            // flag `true` and disable all future background refreshes for this
            // project.
            let _guard = RefreshGuard {
                registry: Arc::clone(&registry),
                path: path.clone(),
            };
            debug!(
                project = %path.display(),
                "Starting background incremental refresh"
            );

            // Run the incremental index (force_reindex=false means only
            // changed files are re-parsed).
            let result = registry.index_project(Some(&path_string), false).await;

            match result {
                Ok(stats) => {
                    debug!(
                        project = %path.display(),
                        files_parsed = stats.files_parsed,
                        "Background incremental refresh completed"
                    );
                    // Invalidate stale cache so next request sees fresh state.
                    registry.invalidate_stale_cache(&path).await;
                }
                Err(error) => {
                    warn!(
                        project = %path.display(),
                        error = %error,
                        "Background incremental refresh failed; existing index remains usable"
                    );
                }
            }
        });
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

    /// Start (or coalesce with) an owned indexing job.
    ///
    /// The returned task is detached from the caller's future. Dropping the
    /// MCP request therefore cannot cancel a parse, transaction, or
    /// generation swap. `wait=true` is an explicit compatibility mode for
    /// interactive callers; MCP defaults to polling.
    pub async fn start_index_job(
        self: &Arc<Self>,
        project_path: Option<&str>,
        force_reindex: bool,
        wait: bool,
    ) -> Result<IndexJobSnapshot, JsonRpcError> {
        let canonical = self.resolve_path(project_path).await?;
        let previous_generation = crate::cli::index_freshness::load_health(
            &crate::cli::leindex::resolve_existing_storage_path(&canonical)
                .unwrap_or_else(|| canonical.join(".leindex")),
        )
        .map(|health| health.generation)
        .unwrap_or(0);
        let storage_root = crate::cli::leindex::resolve_existing_storage_path(&canonical)
            .unwrap_or_else(|| canonical.join(".leindex"));
        let next_state_path =
            JobPaths::new(&storage_root, previous_generation.saturating_add(1)).job_status();
        let state = self
            .select_index_job_state(&canonical, &next_state_path, force_reindex)
            .await;
        self.spawn_owned_index_job(
            &state,
            canonical,
            storage_root,
            previous_generation,
            force_reindex,
        )
        .await;

        if wait {
            Ok(state.wait().await)
        } else {
            Ok(state.snapshot().await)
        }
    }

    /// Read the current owned indexing-job snapshot without starting or
    /// coalescing a job. Used by polling clients and lifecycle tests.
    pub async fn get_index_job_snapshot(
        &self,
        project_path: Option<&str>,
    ) -> Result<Option<IndexJobSnapshot>, JsonRpcError> {
        let canonical = self.resolve_path(project_path).await?;
        let state = self.index_jobs.lock().await.get(&canonical).cloned();
        Ok(match state {
            Some(state) => Some(state.snapshot().await),
            None => None,
        })
    }

    /// Select the existing owned job or replace a completed one for an explicit reindex.
    async fn select_index_job_state(
        &self,
        canonical: &Path,
        next_state_path: &Path,
        force_reindex: bool,
    ) -> Arc<IndexJobState> {
        let mut jobs = self.index_jobs.lock().await;
        if let Some(existing) = jobs.get(canonical).cloned() {
            let current = existing.snapshot().await;
            if current.status == JobStatus::Running || !force_reindex {
                return existing;
            }
        }

        let state = Arc::new(IndexJobState::with_state_path(
            new_job_id(canonical),
            next_state_path.to_path_buf(),
        ));
        jobs.insert(canonical.to_path_buf(), state.clone());
        state
    }

    /// Start the detached outer task only once for a running owned job.
    async fn spawn_owned_index_job(
        self: &Arc<Self>,
        state: &Arc<IndexJobState>,
        path: PathBuf,
        storage_root: PathBuf,
        previous_generation: u64,
        force_reindex: bool,
    ) {
        if state.snapshot().await.status != JobStatus::Running || !state.try_start() {
            return;
        }

        let registry = Arc::clone(self);
        let task_state = Arc::clone(state);
        tokio::spawn(async move {
            // The inner task captures panics so the outer task can publish a
            // terminal snapshot and wake every waiter.
            let state_for_exit = Arc::clone(&task_state);
            let path_for_exit = path.clone();
            let inner = tokio::spawn(async move {
                registry
                    .run_owned_index_work(
                        task_state,
                        path,
                        storage_root,
                        previous_generation,
                        force_reindex,
                    )
                    .await;
            });

            if let Err(join_error) = inner.await {
                if join_error.is_panic() {
                    let payload = join_error.into_panic();
                    let panic_msg = payload
                        .downcast_ref::<&str>()
                        .map(|message| message.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic payload".to_string());
                    warn!(
                        project = %path_for_exit.display(),
                        "Indexing task panicked: {}; marking job as failed", panic_msg
                    );
                    state_for_exit
                        .fail(format!("indexing panicked: {panic_msg}"))
                        .await;
                } else {
                    warn!(
                        project = %path_for_exit.display(),
                        "Indexing task was cancelled; marking job as failed"
                    );
                    state_for_exit.fail("indexing task was cancelled").await;
                }
            }
        });
    }

    async fn run_owned_index_work(
        self: &Arc<Self>,
        state: Arc<IndexJobState>,
        path: PathBuf,
        storage_root: PathBuf,
        previous_generation: u64,
        force_reindex: bool,
    ) {
        let mut resident_core_generation = previous_generation;
        state.set_phase("scan", 0, 0).await;

        // Test-only panic injection. Used by
        // `panic_during_index_sets_failed_status` to verify that the outer
        // task catches panics and marks the job as failed.
        if std::env::var("LEINDEX_INJECT_PANIC")
            .ok()
            .is_some_and(|value| value == "1")
        {
            panic!("injected test panic for index job lifecycle test");
        }

        match self
            .await_index_with_job_progress(
                &state,
                &path,
                &storage_root,
                &mut resident_core_generation,
                force_reindex,
            )
            .await
        {
            Ok(_) => {
                self.finish_owned_index(&state, &path, previous_generation)
                    .await
            }
            Err(error) => {
                self.preserve_core_after_job_error(
                    &state,
                    &path,
                    &storage_root,
                    previous_generation,
                    resident_core_generation,
                    &error,
                )
                .await;
            }
        }
    }

    async fn await_index_with_job_progress(
        &self,
        state: &IndexJobState,
        path: &Path,
        storage_root: &Path,
        resident_core_generation: &mut u64,
        force_reindex: bool,
    ) -> Result<IndexStats, JsonRpcError> {
        let path_string = path.to_string_lossy().into_owned();
        let indexing = self.index_project(Some(&path_string), force_reindex);
        tokio::pin!(indexing);
        loop {
            tokio::select! {
                result = &mut indexing => break result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    self.update_job_progress(state, path, storage_root, resident_core_generation).await;
                }
            }
        }
    }

    async fn update_job_progress(
        &self,
        state: &IndexJobState,
        path: &Path,
        storage_root: &Path,
        resident_core_generation: &mut u64,
    ) {
        let Some(health) = crate::cli::index_freshness::load_health(storage_root) else {
            return;
        };
        if health.phase == crate::cli::leindex::IndexPhase::Complete
            && health.generation > *resident_core_generation
        {
            match self.refresh_loaded_from_active_generation(path).await {
                Ok(()) => {
                    *resident_core_generation = health.generation;
                    state.mark_core_published(health.generation).await;
                }
                Err(error) => {
                    warn!(
                        project = %path.display(),
                        "Core generation is published but resident hydration is pending: {error}"
                    );
                }
            }
        }
        let phase = format!("{:?}", health.phase).to_ascii_lowercase();
        let total = health.indexed_file_count;
        let completed = if health.phase == crate::cli::leindex::IndexPhase::Complete {
            total
        } else {
            0
        };
        state.set_phase(phase, completed, total).await;
    }

    async fn finish_owned_index(
        &self,
        state: &IndexJobState,
        path: &Path,
        previous_generation: u64,
    ) {
        let generation = crate::cli::leindex::resolve_existing_storage_path(path)
            .and_then(|storage| crate::cli::index_freshness::load_health(&storage))
            .map(|health| health.generation)
            .unwrap_or(previous_generation.saturating_add(1));
        let neural_path =
            crate::cli::leindex::resolve_existing_storage_path(path).and_then(|storage| {
                crate::cli::index_freshness::load_health(&storage).map(|health| {
                    storage
                        .join("generations")
                        .join(health.generation.to_string())
                        .join("neural_embeddings.bin")
                })
            });
        if neural_path.as_ref().is_some_and(|path| {
            path.is_file() && std::fs::metadata(path).is_ok_and(|metadata| metadata.len() > 0)
        }) {
            state.mark_neural_published().await;
        }
        state.complete(generation).await;
        // Mark the job as complete in the checkpoint state to prevent reuse
        // of stale artifacts on subsequent force_reindex runs.
        let storage_root = crate::cli::leindex::resolve_existing_storage_path(path)
            .unwrap_or_else(|| path.join(".leindex"));
        let job_paths = crate::cli::index_job::JobPaths::new(&storage_root, generation);
        let _ = crate::cli::index_job::mark_checkpoint_complete(&job_paths.state(), generation);
    }

    async fn preserve_core_after_job_error(
        &self,
        state: &IndexJobState,
        path: &Path,
        storage_root: &Path,
        previous_generation: u64,
        resident_core_generation: u64,
        error: &JsonRpcError,
    ) {
        // The core snapshot is published before neural enrichment. Preserve
        // those layer flags if the optional follow-up fails afterward.
        if let Some(health) = crate::cli::index_freshness::load_health(storage_root) {
            if health.generation > previous_generation {
                let core_loaded = if health.generation > resident_core_generation {
                    match self.refresh_loaded_from_active_generation(path).await {
                        Ok(()) => true,
                        Err(refresh_error) => {
                            warn!(
                                project = %path.display(),
                                "Core generation remains durable but resident hydration failed: {refresh_error}"
                            );
                            false
                        }
                    }
                } else {
                    true
                };
                if core_loaded {
                    state.mark_core_published(health.generation).await;
                }
            }
        }
        state.fail(error.to_string()).await;
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
        // Codex P2: the map removal and the storage close run under ONE
        // `projects` write-lock hold, so a concurrent `get_or_load` cannot
        // clone the Arc in between and later resume on a closed index.
        // `try_write()` still skips a caller that is *currently* inside the
        // inner lock.
        let removed = {
            let mut projects = self.projects.write().await;
            match projects.remove(path) {
                Some(handle) => {
                    if let Ok(mut idx) = handle.try_write() {
                        if let Err(e) = idx.close() {
                            warn!(
                                "Failed to close storage for evicted project {}: {}",
                                path.display(),
                                e
                            );
                        }
                    }
                    info!("Evicted project: {}", path.display());
                    true
                }
                None => false,
            }
        };
        if removed {
            self.cleanup_evicted(path).await;
        }
    }

    /// Shared bookkeeping cleanup after a project has been removed from the
    /// map (LRU order, index slots, watchers, stale cache, index jobs,
    /// incremental-refresh guard, and the D-2 idle-eviction timestamp). Used by
    /// [`Self::evict`], [`Self::evict_lru_if_needed`], and the D-2 idle sweep
    /// so every eviction path leaves identical state behind.
    pub(crate) async fn cleanup_evicted(&self, path: &Path) {
        let mut lru = self.lru_order.lock().await;
        lru.retain(|p| p != path);
        drop(lru);

        let mut slots = self.index_slots.lock().await;
        slots.remove(path);
        drop(slots);

        // Remove watcher so the evicted LeIndex is not kept alive by the
        // watcher's captured ProjectHandle.
        let mut watchers = self.watchers.lock().await;
        watchers.remove(path);
        drop(watchers);

        // A+ hotspot cleanup: evict stale-cache entry so residency does not
        // grow monotonically across long-lived sessions (VAL-APLUS-027).
        self.stale_cache.write().await.remove(path);

        // Drop per-project bookkeeping so a reloaded project starts clean.
        // Without this, `index_jobs` leaks one entry per distinct project
        // visited (memory growth over a long-lived session), and a stale
        // `true` in `incremental_refresh_guard` would block future background
        // refreshes for this path when it is reloaded.
        self.index_jobs.lock().await.remove(path);

        // Clean up incremental refresh guard.
        self.incremental_refresh_guard.lock().await.remove(path);

        // Clean up the D-2 idle-eviction timestamp.
        self.last_used.write().await.remove(path);
    }

    /// Resolve an optional `project_path` string to a canonical `PathBuf`.
    ///
    /// Precedence: an explicit per-call argument wins; then the startup
    /// `--project` / `-p` designation (`default_project`, set once at process
    /// start and never mutated afterwards); then the process CWD — which is
    /// what an MCP client's workspace resolves to when the server was started
    /// without an explicit project. Last-touched projects can NEVER become the
    /// fallback, and `$HOME` is rejected outright.
    async fn resolve_path(&self, project_path: Option<&str>) -> Result<PathBuf, JsonRpcError> {
        let path = if let Some(raw) = project_path {
            Path::new(raw).to_path_buf()
        } else if let Some(designated) = self.default_project.read().await.clone() {
            designated
        } else if let Ok(cwd) = std::env::current_dir() {
            cwd
        } else {
            return Err(JsonRpcError::invalid_params(
                "No project_path provided, no startup --project given, and CWD is \
                 unavailable. Pass project_path on the first call.",
            ));
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
                return Ok(handle.clone());
            }
        }

        let mut leindex = LeIndex::new(&canonical).map_err(|e| {
            JsonRpcError::init_failed(&canonical.display().to_string(), &e.to_string())
        })?;
        // Hydration is LAZY and need-based: `LeIndex::new` already restored
        // persisted stats (so is_indexed() is truthful), and every tool
        // hydrates exactly what it uses — graph-only tools via
        // ensure_pdg_loaded_graph_only, search tools via
        // ensure_analysis_context_loaded. Eagerly loading PDG + search
        // snapshot + embedding mmaps here cost ~1.5s per cold project for
        // every tool, including ones that never touch the engine.
        let hydrate_ms = 0_u64;
        crate::cli::mcp::request_meta::record_hydrate_ms(hydrate_ms);

        // Corruption detection and auto-repair. Never delete the whole
        // storage root: an interrupted build may still have reusable job
        // artifacts and an older generation for rollback.
        let corruption =
            detect_corruption(&canonical).unwrap_or(crate::cli::errors::CorruptionStatus::Healthy);
        if !corruption.is_usable() {
            warn!(
                "Corruption detected in {}: {}. Auto-repairing...",
                canonical.display(),
                corruption.message()
            );
            let storage_path = crate::cli::leindex::resolve_existing_storage_path(&canonical)
                .unwrap_or_else(|| canonical.join(".leindex"));
            if restore_latest_generation(&storage_path) {
                warn!(
                    "Rolled back {} to latest usable generation; preserved corrupt root artifact",
                    canonical.display()
                );
            }
            let mut fresh = LeIndex::new(&canonical).map_err(|e| {
                JsonRpcError::init_failed(
                    &canonical.display().to_string(),
                    &format!(
                        "Original: {}. Preserving artifacts: {}",
                        corruption.message(),
                        e
                    ),
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
        self.touch_last_used(&canonical).await;

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

        let previous_generation = crate::cli::index_freshness::load_health(
            &crate::cli::leindex::resolve_existing_storage_path(&project_path)
                .unwrap_or_else(|| project_path.join(".leindex")),
        )
        .map(|health| health.generation)
        .unwrap_or(0);
        let path_for_blocking = project_path.clone();
        let indexing = tokio::task::spawn_blocking(move || {
            let mut temp = LeIndex::new(&path_for_blocking).map_err(|e| {
                JsonRpcError::init_failed(&path_for_blocking.display().to_string(), &e.to_string())
            })?;
            temp.index_project(force_reindex)
                .map_err(|e| JsonRpcError::indexing_failed(format!("Indexing failed: {}", e)))?;
            Ok::<LeIndex, JsonRpcError>(temp)
        });
        tokio::pin!(indexing);
        let mut resident_core_generation = previous_generation;
        let indexing_result = loop {
            tokio::select! {
                result = &mut indexing => break result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    self.refresh_resident_core_if_published(
                        &project_path,
                        &mut resident_core_generation,
                    )
                    .await;
                }
            }
        };
        let temp = match indexing_result {
            Ok(Ok(temp)) => temp,
            Ok(Err(error)) => {
                let core_published = self
                    .refresh_core_after_index_failure(&project_path, resident_core_generation)
                    .await;
                let message = error.to_string();
                if is_transient_storage_open_failure(&message) {
                    // A transient lock-contention storm (concurrent writers held
                    // the database longer than the open-retry budget) must NOT
                    // permanently brick this generation. The index data is
                    // intact; the failure clears once the contention does. Leave
                    // the previous health/status untouched and only surface the
                    // error to this caller.
                    warn!(
                        project = %project_path.display(),
                        "Transient storage-open failure (lock contention); \
                         leaving generation status unchanged (not bricking): {message}"
                    );
                } else {
                    mark_index_failure(&project_path, &message, core_published);
                }
                return Err(error);
            }
            Err(error) => {
                // JoinError splits into two very different cases:
                // - cancellation (runtime shutdown / task abort): the index
                //   pipeline did NOT fail — it was abandoned. Persisting
                //   last_failure here poisoned freshness for an intact
                //   generation (audit Issue 5): every later diagnostics read
                //   "failed" until a full reindex cleared it.
                // - panic: a genuine pipeline defect; mark failure as before.
                let should_mark_failure = join_error_should_mark_failure(&error);
                let cancelled = error.is_cancelled();
                let error = JsonRpcError::internal_error(format!(
                    "{} error: {}",
                    if cancelled {
                        "Indexing task cancelled"
                    } else {
                        "Task join"
                    },
                    error
                ));
                let core_published = self
                    .refresh_core_after_index_failure(&project_path, resident_core_generation)
                    .await;
                if should_mark_failure {
                    mark_index_failure(&project_path, &error.to_string(), core_published);
                } else {
                    warn!(
                        project = %project_path.display(),
                        "Indexing task was cancelled; leaving generation health untouched \
                         (cancellation is not an indexing failure): {}",
                        error.to_string()
                    );
                }
                return Err(error);
            }
        };

        {
            let mut idx = handle.write().await;
            *idx = temp;
        }

        // Invalidate stale-cache entry so get_or_create() won't reuse
        // the pre-indexing staleness result. `project_path` is
        // already canonical (from `LeIndex::project_path`).
        self.stale_cache.write().await.remove(&project_path);
        // A successful index clears any recent auto-index failure marker so
        // the next get_or_create call can rely on the fresh index.
        self.failed_index_attempts
            .write()
            .await
            .remove(&project_path);

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

    /// Set the default project path without loading the project.
    ///
    /// Used by MCP stdio to register the `--project` CLI argument as the
    /// default so that subsequent tool calls that omit `project_path` resolve
    /// to it. The actual `LeIndex` creation happens lazily on first tool call
    /// via `get_or_load()`.
    pub async fn set_default_path(&self, path: PathBuf) {
        let canonical = path.canonicalize().unwrap_or_else(|err| {
            // Canonicalization can fail for a transiently-missing mount or a
            // permission error. Fall back to the raw path rather than panicking:
            // the caller supplied a real path, and `default_project_path()`
            // re-canonicalizes on lookup, so a non-canonical stored form degrades
            // to an extra reload rather than breaking the registry.
            warn!(
                "Failed to canonicalize default project path {}: {}",
                path.display(),
                err
            );
            path.clone()
        });
        let mut default = self.default_project.write().await;
        *default = Some(canonical);
    }

    /// Return the configured default path without creating or hydrating a project.
    pub async fn default_project_path(&self) -> Result<PathBuf, JsonRpcError> {
        self.resolve_path(None).await
    }

    /// Return an already-loaded project without creating or hydrating it.
    pub async fn try_get_loaded(&self, path: &Path) -> Option<ProjectHandle> {
        self.projects.read().await.get(path).cloned()
    }

    /// Acquire a [`GenerationLease`] for the currently-published generation of
    /// `project`.
    ///
    /// Reads the `CURRENT` file to find the current generation number, loads
    /// the manifest from `generations/<N>/manifest`, opens the CAS store at
    /// `<storage>/cas/`, and increments the refcount of every blob referenced
    /// by the manifest. The returned lease decrements the refcounts on drop.
    ///
    /// This method does **not** acquire the per-project `LeIndex` writer
    /// Mutex or `ProjectWriteLock` (flock). The lease guards blobs purely via
    /// the CAS refcount mechanism, satisfying the no-stall read invariant
    /// (architecture section 4.1).
    pub async fn lease_generation(
        &self,
        project: &Path,
    ) -> Result<crate::storage::GenerationLease, crate::storage::LeaseError> {
        use crate::storage::GenerationLease;
        use crate::storage::generation::{read_current_generation, read_generation_manifest};
        use std::sync::{Arc as StdArc, Mutex as StdMutex};

        // Resolve the project's storage path without creating or hydrating a
        // project entry. The lease is a purely file-and-CAS operation.
        let storage_path = crate::cli::leindex::resolve_existing_storage_path(project)
            .unwrap_or_else(|| project.join(".leindex"));

        let cas_dir = storage_path.join("cas");
        let generation = read_current_generation(&storage_path).ok_or_else(|| {
            crate::storage::LeaseError::NoCurrentGeneration(project.display().to_string())
        })?;
        let manifest = read_generation_manifest(&storage_path, generation)
            .map_err(crate::storage::LeaseError::InvalidManifest)?;

        // Open (or re-open) the CAS store. Each call opens a fresh handle
        // backed by the same on-disk data. The refcount sidecar is
        // read+merged on open so increments survive across openings.
        let store = StdArc::new(StdMutex::new(
            crate::storage::CasStore::open(&cas_dir).map_err(|e| {
                crate::storage::LeaseError::Io(std::io::Error::other(format!(
                    "cas open failed: {e}"
                )))
            })?,
        ));

        GenerationLease::acquire(store, &manifest)
    }

    async fn refresh_resident_core_if_published(&self, path: &Path, resident_generation: &mut u64) {
        let storage_path = crate::cli::leindex::resolve_existing_storage_path(path)
            .unwrap_or_else(|| path.join(".leindex"));
        let Some(health) = crate::cli::index_freshness::load_health(&storage_path) else {
            return;
        };
        if health.phase != crate::cli::leindex::IndexPhase::Complete
            || health.generation <= *resident_generation
        {
            return;
        }
        match self.refresh_loaded_from_active_generation(path).await {
            Ok(()) => *resident_generation = health.generation,
            Err(error) => debug!(
                project = %path.display(),
                "Published core generation is waiting for resident hydration: {error}"
            ),
        }
    }

    /// Refresh the resident handle from the immutable generation selected by
    /// `CURRENT`. Owned jobs publish PDG/TF-IDF before neural enrichment; the
    /// poller uses this short reload so registry-backed tools see that core
    /// generation while the builder continues in the background.
    async fn refresh_loaded_from_active_generation(&self, path: &Path) -> Result<(), JsonRpcError> {
        let Some(handle) = self.try_get_loaded(path).await else {
            return Ok(());
        };
        tokio::task::spawn_blocking(move || {
            let mut index = handle.blocking_write();
            index.load_from_active_storage().map_err(|error| {
                JsonRpcError::internal_error(format!(
                    "Failed to hydrate published core generation: {error:#}"
                ))
            })
        })
        .await
        .map_err(|error| {
            JsonRpcError::internal_error(format!("Core hydration task failed: {error}"))
        })?
    }

    async fn refresh_core_after_index_failure(
        &self,
        path: &Path,
        previous_generation: u64,
    ) -> bool {
        let storage_path = crate::cli::leindex::resolve_existing_storage_path(path)
            .unwrap_or_else(|| path.join(".leindex"));
        let Some(health) = crate::cli::index_freshness::load_health(&storage_path) else {
            return false;
        };
        if health.phase != crate::cli::leindex::IndexPhase::Complete
            || health.generation <= previous_generation
        {
            return false;
        }
        if let Err(refresh_error) = self.refresh_loaded_from_active_generation(path).await {
            debug!(
                project = %path.display(),
                "Published core generation could not be made resident after indexing failure: {refresh_error}"
            );
        }
        true
    }

    /// Evict the least-recently-used project if we're at or over capacity,
    /// or if the aggregate estimated heap of resident projects exceeds the
    /// byte budget (RAM safety: count-based capping alone allowed ~5 × 500 MB
    /// of hydrated projects on the stress-test box). The byte budget is
    /// env-tunable via LEINDEX_REGISTRY_MAX_HEAP_MB (default 1536); 0
    /// disables the byte check.
    async fn evict_lru_if_needed(&self) {
        let current_count = self.projects.read().await.len();
        if current_count < self.max_projects && !self.over_heap_budget().await {
            return;
        }

        let evict_path = {
            let mut lru = self.lru_order.lock().await;
            lru.pop_front()
        };

        if let Some(path) = evict_path {
            // Codex P2: remove + close under ONE projects write-lock hold — no
            // gap in which a concurrent `get_or_load` could clone the Arc and
            // later resume on a closed index. `try_write()` still skips a
            // caller that is currently inside the inner lock.
            let removed = {
                let mut projects = self.projects.write().await;
                match projects.remove(&path) {
                    Some(handle) => {
                        if let Ok(mut idx) = handle.try_write() {
                            if let Err(e) = idx.close() {
                                warn!(
                                    "Failed to close storage for LRU-evicted project {}: {}",
                                    path.display(),
                                    e
                                );
                            }
                        }
                        true
                    }
                    None => false,
                }
            };
            if removed {
                self.cleanup_evicted(&path).await;
                info!(
                    "Evicted LRU project: {} (capacity: {})",
                    path.display(),
                    self.max_projects
                );
            }
        }
    }

    /// True when the aggregate estimated search-engine heap of resident
    /// projects exceeds the byte budget. Collects handles under the map read
    /// lock, then locks each project AFTER releasing it — no map-lock/project
    /// lock ordering inversion with the eviction path.
    async fn over_heap_budget(&self) -> bool {
        let budget_bytes = registry_heap_budget_bytes();
        if budget_bytes == 0 {
            return false;
        }
        let handles: Vec<ProjectHandle> = {
            let projects = self.projects.read().await;
            if projects.len() < 2 {
                // A single project is never evicted by the byte budget — the
                // count cap and the OS manage the one-project case.
                return false;
            }
            projects.values().cloned().collect()
        };
        let mut total: usize = 0;
        for handle in &handles {
            let idx = handle.read().await;
            total = total.saturating_add(idx.search_engine.estimated_memory_bytes());
        }
        if total > budget_bytes {
            warn!(
                estimated_bytes = total,
                budget_bytes, "registry heap budget exceeded; evicting LRU projects"
            );
            true
        } else {
            false
        }
    }
}

/// Resident-heap budget for the project registry in bytes.
/// `LEINDEX_REGISTRY_MAX_HEAP_MB` overrides (0 disables); default 1536.
fn registry_heap_budget_bytes() -> usize {
    let mb = std::env::var("LEINDEX_REGISTRY_MAX_HEAP_MB")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(1536);
    mb.saturating_mul(1024 * 1024)
}

fn restore_latest_generation(storage_path: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(storage_path.join("generations")) else {
        return false;
    };
    let mut generations = entries
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u64>().ok())
        .collect::<Vec<_>>();
    generations.sort_unstable_by(|a, b| b.cmp(a));
    for generation in generations {
        let source = storage_path
            .join("generations")
            .join(generation.to_string())
            .join("leindex.db");
        if !source.is_file() {
            continue;
        }
        let target = storage_path.join("leindex.db");
        if target.is_file() {
            let backup = storage_path.join(format!("leindex.db.corrupt-{generation}"));
            if std::fs::rename(&target, &backup).is_err() {
                continue;
            }
        }
        let next = storage_path.join("leindex.db.recovery.next");
        if std::fs::copy(&source, &next).is_err() || std::fs::rename(&next, &target).is_err() {
            let _ = std::fs::remove_file(&next);
            return false;
        }
        // The swapped-in database must not inherit the old database's WAL/SHM
        // side files: their salts are keyed to the replaced main file, and
        // SQLite refuses to open (or "recovers" garbage from) a mismatched
        // pair — turning a repair into a permanently unopenable store
        // (persistent -32008 on every tool call). Best-effort removal; the
        // files are recreated cleanly on the next open.
        let _ = std::fs::remove_file(storage_path.join("leindex.db-wal"));
        let _ = std::fs::remove_file(storage_path.join("leindex.db-shm"));
        let _ = std::fs::write(storage_path.join("CURRENT"), format!("{generation}\n"));
        return true;
    }
    false
}

/// True if `message` is a *transient* storage-open failure (lock contention),
/// as tagged by `LeIndex::open_storage_with_retry` with the
/// `[transient:lock-contention]` sentinel.
///
/// Such failures clear once the contending writer finishes and must NOT
/// permanently brick a generation via `mark_index_failure` — the underlying
/// index data is intact and a retry succeeds. Genuine failures (corrupt DB,
/// disk full) carry no sentinel and brick as before.
fn is_transient_storage_open_failure(message: &str) -> bool {
    message.contains("[transient:lock-contention]")
}

/// Whether a `spawn_blocking` JoinError from the index pipeline should persist
/// an index-health failure. Cancellation (runtime shutdown / task abort) means
/// the pipeline was abandoned, not that it failed — marking failure there
/// poisons freshness for an intact generation. A panic is a genuine defect and
/// still marks.
fn join_error_should_mark_failure(error: &tokio::task::JoinError) -> bool {
    !error.is_cancelled()
}

fn mark_index_failure(project_path: &Path, message: &str, core_published: bool) {
    let storage_path = crate::cli::leindex::resolve_existing_storage_path(project_path)
        .unwrap_or_else(|| project_path.join(".leindex"));
    let previous = crate::cli::index_freshness::load_health(&storage_path).unwrap_or_default();
    let health = crate::cli::leindex::IndexHealth {
        generation: previous.generation,
        phase: previous.phase,
        status: if core_published {
            previous.status
        } else {
            crate::cli::leindex::ComponentStatus::Failed
        },
        head_oid: previous.head_oid,
        tree_oid: previous.tree_oid,
        indexed_file_count: previous.indexed_file_count,
        dirty_file_count: previous.dirty_file_count,
        changed_unindexed_count: previous.changed_unindexed_count,
        indexed_at_unix_ms: previous.indexed_at_unix_ms,
        last_failure_phase: Some(if core_published {
            crate::cli::leindex::IndexPhase::Neural
        } else {
            previous.phase
        }),
        last_failure: Some(message.to_string()),
    };
    let _ = crate::cli::index_freshness::save_health(&storage_path, &health);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_join_error_cancellation_does_not_mark_failure() {
        // Audit Issue 5: a cancelled index task must not poison freshness.
        let cancelled = tokio::spawn(std::future::pending::<()>());
        cancelled.abort();
        let error = cancelled.await.unwrap_err();
        assert!(error.is_cancelled());
        assert!(!join_error_should_mark_failure(&error));
    }

    #[tokio::test]
    async fn test_join_error_panic_marks_failure() {
        let panicking = tokio::spawn(async { panic!("pipeline defect") });
        let error = panicking.await.unwrap_err();
        assert!(error.is_panic());
        assert!(join_error_should_mark_failure(&error));
    }

    #[test]
    fn test_registry_heap_budget_env_parse() {
        // Default budget: 1536 MiB.
        assert_eq!(registry_heap_budget_bytes(), 1536 * 1024 * 1024);
    }

    #[cfg(all(unix, feature = "onnx"))]
    #[test]
    fn test_terminate_superseded_daemons_cleans_stale_artifacts() {
        // Stale pid files (dead/missing pid) must have their socket/status
        // artifacts removed; the keep-socket's own pid file must survive.
        let dir = tempfile::tempdir().unwrap();
        let keep_socket = dir.path().join("leindex-embed-aaaa.sock");
        let keep_pid = dir.path().join("leindex-embed-aaaa.pid");
        std::fs::write(&keep_pid, "1234\n").unwrap();

        let stale_pid = dir.path().join("leindex-embed-bbbb.pid");
        std::fs::write(&stale_pid, "999999999\n").unwrap(); // no such pid
        let stale_socket = dir.path().join("leindex-embed-bbbb.sock");
        let stale_status = dir.path().join("leindex-embed-bbbb.status");
        std::fs::write(&stale_socket, b"x").unwrap();
        std::fs::write(&stale_status, b"x").unwrap();

        crate::search::onnx::client::terminate_superseded_daemons(&keep_socket);

        assert!(keep_pid.exists(), "keep-socket pid file must survive");
        assert!(!stale_pid.exists(), "stale pid file removed");
        assert!(!stale_socket.exists(), "stale socket removed");
        assert!(!stale_status.exists(), "stale status removed");
    }

    #[test]
    fn test_restore_latest_generation_removes_stale_wal_sidecars() {
        // The permanent-brick vector: after swapping a generation's DB over the
        // live leindex.db, the OLD database's -wal/-shm side files must be
        // removed. Their salts are keyed to the replaced main file, so SQLite
        // refuses to open (or "recovers" garbage from) the mismatched pair —
        // every subsequent tool call then fails -32008 forever.
        let dir = tempfile::tempdir().unwrap();
        let storage = dir.path().join("store");
        let gen_dir = storage.join("generations").join("7");
        std::fs::create_dir_all(&gen_dir).unwrap();

        // Valid generation DB (contents never opened by restore, only copied).
        std::fs::write(gen_dir.join("leindex.db"), b"generation-db-bytes").unwrap();
        // Live DB plus stale side files from the pre-swap database.
        std::fs::write(storage.join("leindex.db"), b"old-live-db-bytes").unwrap();
        std::fs::write(storage.join("leindex.db-wal"), b"stale-wal").unwrap();
        std::fs::write(storage.join("leindex.db-shm"), b"stale-shm").unwrap();

        assert!(restore_latest_generation(&storage));
        assert_eq!(
            std::fs::read(storage.join("leindex.db")).unwrap(),
            b"generation-db-bytes"
        );
        assert!(
            !storage.join("leindex.db-wal").exists(),
            "stale WAL sidecar must be removed with the swapped DB"
        );
        assert!(
            !storage.join("leindex.db-shm").exists(),
            "stale SHM sidecar must be removed with the swapped DB"
        );
        assert_eq!(
            std::fs::read_to_string(storage.join("CURRENT"))
                .unwrap()
                .trim(),
            "7"
        );
    }

    #[tokio::test]
    async fn test_registry_creation() {
        let registry = ProjectRegistry::new(5);
        assert_eq!(registry.len().await, 0);
    }

    #[tokio::test]
    async fn test_get_or_create_skips_auto_index_within_failure_cooldown() {
        // A recent failed auto-index must suppress further full-index attempts
        // (coalescing the degraded state) while get_or_create still succeeds
        // so read/edit tools can degrade gracefully instead of erroring.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        let canonical = dir.path().canonicalize().unwrap();

        let registry = Arc::new(ProjectRegistry::new(5));
        // Simulate an auto-index attempt that failed moments ago.
        registry
            .failed_index_attempts
            .write()
            .await
            .insert(canonical.clone(), std::time::Instant::now());

        let handle = registry
            .get_or_create(Some(&canonical.to_string_lossy()))
            .await
            .expect("get_or_create must not fail when the index attempt is skipped");
        assert!(
            !handle.read().await.is_indexed(),
            "full index attempt must be skipped within the failure cooldown"
        );

        // An explicit index request still works and clears the failure marker.
        registry
            .index_project(Some(&canonical.to_string_lossy()), false)
            .await
            .expect("explicit index must succeed on a healthy project");
        assert!(
            !registry
                .failed_index_attempts
                .read()
                .await
                .contains_key(&canonical),
            "a successful index must clear the failure marker"
        );
    }

    #[test]
    fn post_core_failure_preserves_published_health() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join(".leindex");
        let health = crate::cli::leindex::IndexHealth {
            generation: 4,
            phase: crate::cli::leindex::IndexPhase::Complete,
            status: crate::cli::leindex::ComponentStatus::Fresh,
            indexed_at_unix_ms: Some(1),
            ..Default::default()
        };
        crate::cli::index_freshness::save_health(&storage, &health).unwrap();

        mark_index_failure(temp.path(), "neural failed", true);

        let recorded = crate::cli::index_freshness::load_health(&storage).unwrap();
        assert_eq!(recorded.status, crate::cli::leindex::ComponentStatus::Fresh);
        assert_eq!(
            recorded.last_failure_phase,
            Some(crate::cli::leindex::IndexPhase::Neural)
        );
        assert_eq!(recorded.last_failure.as_deref(), Some("neural failed"));
    }

    #[test]
    fn restore_latest_generation_preserves_corrupt_root() {
        let temp = tempfile::tempdir().unwrap();
        let storage = temp.path().join(".leindex");
        let generation = storage.join("generations/3");
        std::fs::create_dir_all(&generation).unwrap();
        std::fs::write(storage.join("leindex.db"), b"corrupt").unwrap();
        std::fs::write(generation.join("leindex.db"), b"usable").unwrap();
        assert!(restore_latest_generation(&storage));
        assert_eq!(
            std::fs::read(storage.join("leindex.db")).unwrap(),
            b"usable"
        );
        assert_eq!(
            std::fs::read(storage.join("leindex.db.corrupt-3")).unwrap(),
            b"corrupt"
        );
    }

    #[tokio::test]
    async fn test_resolve_path_defaults_to_cwd_without_startup_project() {
        // No explicit arg and no startup --project: resolution must fall back
        // to the process CWD (the MCP client's workspace) — never to $HOME or
        // to whichever project another tool touched most recently.
        let registry = ProjectRegistry::new(5);
        let resolved = registry.resolve_path(None).await.unwrap();
        let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
        assert_eq!(resolved, expected);
    }

    #[tokio::test]
    async fn test_resolve_path_startup_project_beats_cwd() {
        // A daemon started with `-p`/`--project` must keep serving path-less
        // calls from that designation even when its process CWD differs.
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();
        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let resolved = registry.resolve_path(None).await.unwrap();
        assert_eq!(resolved, tmp.path().canonicalize().unwrap());
    }

    #[tokio::test]
    async fn test_resolve_path_explicit_arg_beats_everything() {
        // An explicit per-call project_path always wins over both the startup
        // designation and the CWD fallback.
        let tmp1 = tempfile::tempdir().unwrap();
        std::fs::write(tmp1.path().join("main.rs"), "fn one() {}\n").unwrap();
        let tmp2 = tempfile::tempdir().unwrap();
        std::fs::write(tmp2.path().join("main.rs"), "fn two() {}\n").unwrap();

        let leindex = LeIndex::new(tmp1.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let resolved = registry
            .resolve_path(Some(tmp2.path().to_str().unwrap()))
            .await
            .unwrap();
        assert_eq!(resolved, tmp2.path().canonicalize().unwrap());
    }

    #[tokio::test]
    async fn test_resolve_path_rejects_home_directory() {
        let registry = ProjectRegistry::new(5);
        let home = dirs::home_dir().expect("test requires a resolvable home directory");
        let result = registry
            .resolve_path(Some(home.to_str().unwrap()))
            .await
            .unwrap_err();
        assert!(
            result.message.contains("home"),
            "expected home-dir rejection, got: {}",
            result.message
        );
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
    async fn core_generation_refreshes_the_resident_handle() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn resident_core() {}\n").unwrap();

        let mut builder = LeIndex::new(tmp.path()).unwrap();
        builder.index_project(true).unwrap();
        drop(builder);

        let resident = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(2, resident);
        let canonical = tmp.path().canonicalize().unwrap();
        let handle = registry
            .get_or_load(Some(canonical.to_str().unwrap()))
            .await
            .unwrap();
        assert!(handle.read().await.pdg().is_none());

        registry
            .refresh_loaded_from_active_generation(&canonical)
            .await
            .unwrap();
        let guard = handle.read().await;
        assert!(guard.pdg().is_some());
        assert!(guard.search_engine().node_count() > 0);
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
    async fn test_registry_default_project_never_tracks_last_used() {
        // The old last-touched-wins behavior made a path-less call silently
        // bind to whichever project another tool touched most recently —
        // the reported cross-project binding bug. The startup designation is
        // immutable: touching tmp2 must not move it.
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

        // Path-less resolution still lands on the startup project...
        let h2 = registry.get_or_load(None).await.unwrap();
        let path2 = h2.read().await.project_path().to_path_buf();
        assert_eq!(path2, tmp1.path().canonicalize().unwrap());

        // ...and resolve_path agrees.
        assert_eq!(
            registry.resolve_path(None).await.unwrap(),
            tmp1.path().canonicalize().unwrap()
        );
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

    /// Regression for P2 round 15 (codex `3344884534`): write
    /// handlers (`edit-apply`, `write-file`, `rename-symbol`) must
    /// invalidate the staleness cache after a successful write so
    /// that the next read tool re-runs `is_stale_fast` instead of
    /// reusing a pre-write `false` cached result. The watcher
    /// (when enabled) does this on its own reindex path; the
    /// explicit call covers the watcher-disabled default mode
    /// where the 30-second negative-cache TTL would otherwise
    /// silently mask the edit.
    #[tokio::test]
    async fn test_invalidate_stale_cache_removes_entry() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let canonical = tmp.path().canonicalize().unwrap();

        // Prime the cache with a `false` result (the scenario
        // the codex comment describes: a previous read tool ran
        // `is_stale_fast` and got back `false`).
        registry
            .stale_cache
            .write()
            .await
            .insert(canonical.clone(), (std::time::Instant::now(), false));
        assert!(registry.stale_cache.read().await.contains_key(&canonical));

        // The write handler calls this after the disk write.
        registry.invalidate_stale_cache(&canonical).await;

        assert!(
            !registry.stale_cache.read().await.contains_key(&canonical),
            "stale cache entry must be removed on invalidate"
        );
    }

    /// `invalidate_stale_cache` requires an already-canonicalized
    /// path. The cache key is built from `LeIndex::project_path`,
    /// which is canonicalized at construction, so callers must pass
    /// `guard.project_path().to_path_buf()` (or equivalent).
    #[tokio::test]
    async fn test_invalidate_stale_cache_requires_canonical_input() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

        let leindex = LeIndex::new(tmp.path()).unwrap();
        let registry = ProjectRegistry::with_initial_project(5, leindex);

        let canonical = tmp.path().canonicalize().unwrap();
        registry
            .stale_cache
            .write()
            .await
            .insert(canonical.clone(), (std::time::Instant::now(), false));

        // Must pass the canonicalized path — the function no longer
        // re-canonicalizes internally.
        registry.invalidate_stale_cache(&canonical).await;
        assert!(
            !registry.stale_cache.read().await.contains_key(&canonical),
            "stale cache entry must be removed on invalidate with canonical input"
        );
    }
}
