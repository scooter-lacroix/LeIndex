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

mod index_jobs;

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

/// How much of a project a tool needs resident before it runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hydration {
    /// Nothing beyond the persisted stats (text search, reads, index control).
    None,
    /// The program dependence graph only (symbol, impact, edit, diff tools).
    Graph,
    /// The graph plus the semantic search engine (search, deep analyze, context).
    Full,
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

    /// Background pre-warm has been started (once per registry).
    prewarm_started: std::sync::atomic::AtomicBool,
    /// Single-flight latches for off-lock hydration, one per project.
    hydration_flights: Mutex<HashMap<PathBuf, Arc<Mutex<()>>>>,
    /// Projects a `spawn_prewarm_at` is currently warming.
    prewarming: std::sync::Mutex<std::collections::HashSet<PathBuf>>,
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
            prewarm_started: std::sync::atomic::AtomicBool::new(false),
            hydration_flights: Mutex::new(HashMap::new()),
            prewarming: std::sync::Mutex::new(std::collections::HashSet::new()),
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

    /// Start loading the default project in the background (once).
    ///
    /// Called when a client completes `initialize`. Cold hydration is the only
    /// slow part of a first tool call; the model spends longer than that
    /// deciding what to ask, so doing it now hides it. Two phases, so a tool
    /// that needs only the graph never waits for the search engine:
    /// the PDG first (~0.2 s), then the engine (`[mcp] prewarm = "full"`).
    ///
    /// Never builds an index and never creates storage: a project that has not
    /// been indexed is left alone. One-shot processes skip it entirely.
    pub fn spawn_prewarm(self: &Arc<Self>) {
        use std::sync::atomic::Ordering;
        if self.is_one_shot() || self.prewarm_started.swap(true, Ordering::AcqRel) {
            return;
        }
        self.spawn_prewarm_at(None);
    }

    /// Stop `initialize` from warming the process's default project. A daemon
    /// serves many clients whose projects it learns from their hello, so
    /// warming whatever directory it happened to be started in would only cost
    /// memory.
    pub fn disable_default_prewarm(&self) {
        self.prewarm_started
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Warm a specific project (the daemon calls this with each client's
    /// working directory as soon as it connects). `None` means the default
    /// project. Calls for a project already being warmed are dropped, and a
    /// project that is already resident returns immediately, so repeated
    /// connections cost nothing.
    pub fn spawn_prewarm_at(self: &Arc<Self>, target: Option<PathBuf>) {
        // Initialize handlers can be reached from synchronous code (and tests)
        // with no runtime; pre-warming is an optimization, so skip quietly.
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if self.is_one_shot() {
            return;
        }
        let mode = crate::config::LeIndexConfig::load_cached()
            .mcp
            .prewarm
            .clone();
        let full = match mode.trim().to_ascii_lowercase().as_str() {
            "off" | "false" | "none" | "0" => return,
            "graph" => false,
            _ => true,
        };
        if let Some(path) = &target {
            let mut in_flight = self
                .prewarming
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if !in_flight.insert(path.clone()) {
                return;
            }
        }
        let registry = Arc::clone(self);
        runtime.spawn(async move {
            let requested = target
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned());
            registry.prewarm_project(requested.as_deref(), full).await;
            if let Some(path) = &target {
                registry
                    .prewarming
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .remove(path);
            }
        });
    }

    async fn prewarm_project(self: &Arc<Self>, requested: Option<&str>, full: bool) {
        let Ok(path) = self.resolve_path(requested).await else {
            return;
        };
        let has_index = crate::cli::live_project::LiveProject::resolve(&path.to_string_lossy())
            .is_ok_and(|live| live.active_storage().join("leindex.db").is_file());
        if !has_index {
            debug!(project = %path.display(), "Prewarm skipped: project is not indexed");
            return;
        }
        let started = std::time::Instant::now();
        // Full loads the graph and the engine together (the engine restore
        // runs beside the graph read), which beats graph-then-engine.
        let level = if full {
            Hydration::Full
        } else {
            Hydration::Graph
        };
        let ok = self.ensure_hydrated(requested, level).await;
        info!(
            project = %path.display(),
            total_ms = started.elapsed().as_millis() as u64,
            ok,
            full,
            "Prewarmed project"
        );
    }

    /// Make sure `level` of the project is resident, without holding the
    /// project lock while it loads.
    ///
    /// The graph and search engine take up to a second to build. Building them
    /// on the instance behind the per-project lock froze every other call —
    /// even ones that need neither — for that long. This builds a detached
    /// sibling instance on a blocking thread and takes the lock only to swap
    /// its state in. Concurrent callers share one build (single flight). A
    /// build that raced a newly published generation is discarded, and the
    /// handler's own on-demand load then runs as before, so this is purely an
    /// accelerator: it never builds an index and never creates storage.
    ///
    /// Returns whether the project is hydrated to `level` afterwards.
    pub async fn ensure_hydrated(
        self: &Arc<Self>,
        project_path: Option<&str>,
        level: Hydration,
    ) -> bool {
        let full = match level {
            Hydration::None => return true,
            Hydration::Graph => false,
            Hydration::Full => true,
        };
        let Ok(handle) = self.get_or_load(project_path).await else {
            return false;
        };
        if self.is_one_shot() {
            // A one-shot process has no other caller to keep responsive, so
            // load in place -- but only what this tool needs: eagerly loading
            // the graph and search engine cost ~350 ms even for `find`.
            let mut idx = handle.write().await;
            if idx.is_hydrated(full) || !idx.is_indexed() {
                return idx.is_hydrated(full);
            }
            let loaded = if full {
                idx.ensure_analysis_context_loaded()
            } else {
                idx.ensure_pdg_loaded_graph_only()
            };
            return loaded.is_ok() && idx.is_hydrated(full);
        }
        let (root, indexed) = {
            let idx = handle.read().await;
            if idx.is_hydrated(full) {
                return true;
            }
            (idx.project_path().to_path_buf(), idx.is_indexed())
        };
        if !indexed {
            return false;
        }
        let flight = {
            let mut flights = self.hydration_flights.lock().await;
            Arc::clone(flights.entry(root.clone()).or_default())
        };
        let _flight = flight.lock().await;
        if handle.read().await.is_hydrated(full) {
            return true;
        }
        let generation = || {
            crate::cli::leindex::resolve_existing_storage_path(&root).and_then(|storage| {
                crate::storage::generation::lease::read_current_generation(&storage)
            })
        };
        let before = generation();
        let build_root = root.clone();
        let built = tokio::task::spawn_blocking(move || {
            let mut sibling = LeIndex::new(&build_root).ok()?;
            let loaded = if full {
                sibling.ensure_analysis_context_loaded()
            } else {
                sibling.ensure_pdg_loaded_graph_only()
            };
            loaded.ok()?;
            Some(sibling)
        })
        .await
        .ok()
        .flatten();
        let Some(sibling) = built else {
            return false;
        };
        if generation() != before {
            debug!(project = %root.display(), "Discarding off-lock hydration: a new generation was published");
            return false;
        }
        handle.write().await.adopt_hydration(sibling, full);
        handle.read().await.is_hydrated(full)
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
            prewarm_started: std::sync::atomic::AtomicBool::new(false),
            hydration_flights: Mutex::new(HashMap::new()),
            prewarming: std::sync::Mutex::new(std::collections::HashSet::new()),
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
            return self.index_handle(handle, false, false).await.map(|_| ());
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
            // It runs on the low-priority refresh pool so it never competes
            // with foreground tool calls for CPU.
            let result = match registry.get_or_load(Some(&path_string)).await {
                Ok(handle) => registry.index_handle(&handle, false, true).await,
                Err(error) => Err(error),
            };

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
        self.index_handle(&handle, force_reindex, false).await
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
    /// The canonical project root a call refers to, without loading, indexing
    /// or refreshing anything (same resolution rules as every other tool:
    /// explicit path, else the startup designation, else the CWD).
    ///
    /// Tools that only need to know *where* the project is — and bring their
    /// own analysis pipeline — use this instead of `get_or_create`, which
    /// would also start an index or background refresh they do not need.
    pub async fn resolve_project_root(
        &self,
        project_path: Option<&str>,
    ) -> Result<PathBuf, JsonRpcError> {
        self.resolve_path(project_path).await
    }

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
    ///
    /// `background` runs the (CPU-heavy) build on the low-priority refresh
    /// pool, used by opportunistic staleness refreshes so a user's foreground
    /// call is never starved by work they did not ask for.
    async fn index_handle(
        &self,
        handle: &ProjectHandle,
        force_reindex: bool,
        background: bool,
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
            let run = |temp: &mut LeIndex| {
                temp.index_project(force_reindex)
                    .map_err(|e| JsonRpcError::indexing_failed(format!("Indexing failed: {}", e)))
            };
            if background {
                background_pool().install(|| run(&mut temp))?;
            } else {
                run(&mut temp)?;
            }
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
/// Thread pool for opportunistic background index refreshes.
///
/// A stale index triggers a refresh on the first call after startup. That
/// work is CPU-bound (parse, TF-IDF, fingerprinting) and would otherwise
/// contend with the foreground tool call that caused it, so it gets at most
/// half the cores (capped at two) and runs at a lowered scheduling priority.
fn background_pool() -> &'static rayon::ThreadPool {
    static POOL: std::sync::OnceLock<rayon::ThreadPool> = std::sync::OnceLock::new();
    POOL.get_or_init(|| {
        let cores = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(2);
        rayon::ThreadPoolBuilder::new()
            .num_threads((cores / 2).clamp(1, 2))
            .thread_name(|i| format!("leindex-refresh-{i}"))
            .start_handler(|_| lower_thread_priority())
            .build()
            .unwrap_or_else(|error| panic!("failed to build refresh pool: {error}"))
    })
}

/// Lower the calling thread's scheduling priority (nice +10). Best effort:
/// failure only means the refresh competes at normal priority.
#[cfg(target_os = "linux")]
fn lower_thread_priority() {
    // SAFETY: `gettid` and `setpriority` take plain integers and have no
    // memory-safety preconditions. On Linux, PRIO_PROCESS with a thread id
    // adjusts only that thread.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        let _ = libc::setpriority(libc::PRIO_PROCESS, tid, 10);
    }
}

#[cfg(not(target_os = "linux"))]
fn lower_thread_priority() {}

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
#[path = "registry_test.rs"]
mod tests;
