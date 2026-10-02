// leindex - Core Orchestration
//
// *L'Index* (The Index) - Unified API that brings together all LeIndex crates

mod diagnostics;
mod indexing;
pub(crate) mod model_download;
mod query;
pub(crate) mod setup;
mod types;

#[cfg(test)]
mod tests;

#[cfg(test)]
#[path = "generation_read_test.rs"]
mod generation_read_tests;

// Re-export public types for external callers
pub use types::{
    AnalysisResult, ComponentStatus, CoverageReport, Diagnostics, FileStats, IndexHealth,
    IndexPhase, IndexStats,
};
// Re-export crate-internal types for sibling modules (index_builder, index_cache, etc.)
pub(crate) use types::{
    DEPENDENCY_MANIFEST_NAMES, ProjectFileScan, SKIP_DIRS, SOURCE_FILE_EXTENSIONS,
};

use crate::cli::index_builder;
use crate::cli::memory::WarmStrategy;
use crate::graph::pdg::ProgramDependenceGraph;
use crate::search::search::SearchEngine;
use crate::storage::{UniqueProjectId, schema::Storage};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

/// Find existing index storage without creating a directory or opening SQLite.
pub(crate) fn resolve_existing_storage_path(project_path: &Path) -> Option<PathBuf> {
    LeIndex::resolve_existing_storage_path(project_path)
}

/// LeIndex - Main orchestration struct for the entire LeIndex system.
///
/// ```ignore
/// let leindex = LeIndex::new("/path/to/project")?;
/// leindex.index_project()?;
/// let results = leindex.search("authentication", 10).await?;
/// ```
pub struct LeIndex {
    /// Project path
    pub(crate) project_path: PathBuf,

    /// Resolved storage root for index artifacts (may be outside project)
    pub(crate) storage_path: PathBuf,

    /// Project identifier (legacy, for backward compatibility)
    pub(crate) project_id: String,

    /// Unique project identifier with BLAKE3-based path hashing
    pub(crate) unique_id: UniqueProjectId,

    /// Storage backend
    pub(crate) storage: Storage,

    /// Search engine
    pub(crate) search_engine: SearchEngine,

    /// Program Dependence Graph
    /// Shared, copy-on-write: a validator (or any reader) can hold the graph
    /// without cloning it, and writers use [`Self::take_owned_pdg`], which only
    /// copies if a reader is still alive.
    pub(crate) pdg: Option<std::sync::Arc<ProgramDependenceGraph>>,

    /// Cache subsystem (spiller, project scan, file stats)
    pub(crate) cache: crate::cli::index_cache::IndexCache,

    /// Cached project configuration.
    pub(crate) project_config: crate::cli::config::ProjectConfig,

    /// Indexing statistics
    pub(crate) stats: IndexStats,

    /// TF-IDF embedder (None until index_nodes() runs).
    pub(crate) embedder: Option<index_builder::HybridEmbedder>,

    /// Ephemeral state shared by the explicit indexing phases.
    pub(crate) pipeline: Option<indexing::IndexPipelineState>,

    /// Live generation read path (WS4 Task 14): when
    /// `LEINDEX_FEATURE_GENERATION_READERS` is enabled and the project has a
    /// current generation, this holds the leased snapshot that the
    /// search/symbol/deep-analyze read path reads from. Holding the snapshot
    /// keeps the generation's CAS blobs pinned (via `GenerationLease`) for the
    /// lifetime of this process, so reads never touch the writer Mutex and
    /// never race a concurrent publish.
    pub(crate) generation_snapshot: Option<crate::storage::generation::GenerationSnapshot>,

    /// The generation this process's in-memory PDG/search state was loaded
    /// from (set at hydration and at each successful publish; 0 = never
    /// hydrated). Atomic because publish paths hold `&self`. The registry
    /// compares it against the persisted `CURRENT` pointer to detect
    /// external rebuilds (another server or a CLI `--force`) and re-hydrate
    /// instead of serving a stale snapshot under a fresh footer (N-13).
    pub(crate) hydrated_generation: std::sync::atomic::AtomicU64,

    /// Whether the most recent `index_project` call was coalesced away (a
    /// fresh index published by another process while this one waited for
    /// the project write lock). The registry uses this to keep the resident
    /// instance — which may hold a hydrated core — instead of installing an
    /// un-hydrated temp over it.
    pub(crate) last_index_coalesced: bool,
}

/// Cross-process exclusive lock guarding writes to a project's storage.
///
/// SQLite WAL permits exactly **one writer**. When two leindex processes write
/// the same `leindex.db` at once (a second MCP instance, or MCP + CLI), they
/// contend on the database lock, exhaust the open-retry budget, and can corrupt
/// the WAL — the failure mode that bricks a generation. This advisory
/// `flock(2)` serializes writers across processes: a second writer blocks until
/// the holder drops the guard. `flock` is released automatically on process
/// death (close of the underlying fd), so a crash can never leave a stale lock.
///
/// Readers (search/load) do **not** take this lock, so concurrent reads stay
/// fast and uncontended. Held for the lifetime of a single write operation
/// (`index_project_inner` / `incremental_reindex_from_watcher`) via RAII.
pub(crate) struct ProjectWriteLock {
    _file: std::fs::File,
}

impl ProjectWriteLock {
    /// Acquire an exclusive cross-process write lock for `storage_path`.
    /// Blocks until the lock becomes available.
    pub(crate) fn acquire(storage_path: &Path) -> Result<Self> {
        let lock_path = storage_path.join("index.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            // Lock marker file: create if missing, never clobber if present
            // (its content is irrelevant — only the fd is flocked).
            .truncate(false)
            .open(&lock_path)
            .with_context(|| {
                format!("Failed to open write-lock file at {}", lock_path.display())
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // LOCK_EX blocks until exclusive ownership is obtained. POSIX
            // guarantees release on close/exec/process-exit, so a holder that
            // crashes frees the lock automatically (no stale PID file).
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                anyhow::bail!(
                    "Failed to acquire cross-process write lock at {}: {err}",
                    lock_path.display()
                );
            }
        }
        #[cfg(windows)]
        {
            // Blocking exclusive LockFileEx. Released automatically when the
            // handle closes (process death), matching flock semantics.
            windows_lock::lock(&file, true).map_err(|err| {
                anyhow::anyhow!(
                    "Failed to acquire cross-process write lock at {}: {err}",
                    lock_path.display()
                )
            })?;
        }
        Ok(Self { _file: file })
    }

    /// Non-blocking variant: returns `Ok(Some(guard))` if the lock was free,
    /// `Ok(None)` if another process holds it. Used by the watcher (skip a
    /// reindex when another process is already writing) and by the
    /// mutual-exclusion self-check.
    pub(crate) fn try_acquire(storage_path: &Path) -> Result<Option<Self>> {
        let lock_path = storage_path.join("index.lock");
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(&lock_path)
            .with_context(|| {
                format!("Failed to open write-lock file at {}", lock_path.display())
            })?;
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if rc != 0 {
                let err = std::io::Error::last_os_error();
                // EWOULDBLOCK / EAGAIN = locked by someone else (expected).
                // Compare by value (not pattern): on Linux these two errno
                // constants are identical, which would make an alternation
                // pattern unreachable.
                let raw = err.raw_os_error();
                if raw == Some(libc::EWOULDBLOCK) || raw == Some(libc::EAGAIN) {
                    return Ok(None);
                }
                anyhow::bail!(
                    "Failed to probe write lock at {}: {err}",
                    lock_path.display()
                );
            }
        }
        #[cfg(windows)]
        {
            // LOCKFILE_FAIL_IMMEDIATELY: returns ERROR_LOCK_VIOLATION (33) or
            // ERROR_SHARING_VIOLATION (32) if another process holds it.
            match windows_lock::lock(&file, false) {
                Ok(()) => {}
                Err(err)
                    if matches!(
                        err.raw_os_error(),
                        Some(33) | Some(32) // LOCK_VIOLATION | SHARING_VIOLATION
                    ) =>
                {
                    return Ok(None);
                }
                Err(err) => {
                    anyhow::bail!(
                        "Failed to probe write lock at {}: {err}",
                        lock_path.display()
                    );
                }
            }
        }
        Ok(Some(Self { _file: file }))
    }
}

impl Drop for ProjectWriteLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            // Explicit unlock for determinism (the File drop also closes the fd,
            // which releases flock, but be explicit about intent).
            let _ = unsafe { libc::flock(self._file.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(windows)]
        {
            windows_lock::unlock(&self._file);
        }
    }
}

/// Windows cross-process file locking via `LockFileEx`/`UnlockFileEx`
/// (kernel32), used by `ProjectWriteLock` on the Windows release target
/// (see AGENTS.md: builds Linux/macOS/Windows). No extra crate — raw FFI.
/// Locks byte `[0, 1)` exclusively; a second exclusive lock on the same byte
/// blocks (blocking) or fails immediately (try), providing mutual exclusion.
/// The handle close on `File` drop releases the lock, matching `flock`.
#[cfg(windows)]
mod windows_lock {
    use std::fs::File;
    use std::os::windows::io::AsRawHandle;

    #[repr(C)]
    #[derive(Default)]
    struct Overlapped {
        internal: usize,
        internal_high: usize,
        offset_low: u32,
        offset_high: u32,
        event: usize,
    }

    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x0000_0002;
    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x0000_0001;

    unsafe extern "system" {
        fn LockFileEx(
            handle: usize,
            flags: u32,
            reserved: u32,
            len_low: u32,
            len_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
        fn UnlockFileEx(
            handle: usize,
            reserved: u32,
            len_low: u32,
            len_high: u32,
            overlapped: *mut Overlapped,
        ) -> i32;
    }

    /// Lock byte `[0, 1)` exclusively. `blocking = false` adds
    /// `LOCKFILE_FAIL_IMMEDIATELY`.
    pub(super) fn lock(file: &File, blocking: bool) -> std::io::Result<()> {
        let handle = file.as_raw_handle() as usize;
        let mut overlapped = Overlapped::default();
        let mut flags = LOCKFILE_EXCLUSIVE_LOCK;
        if !blocking {
            flags |= LOCKFILE_FAIL_IMMEDIATELY;
        }
        // SAFETY: FFI to kernel32 `LockFileEx` with a valid file handle and a
        // valid `Overlapped` pointer. Byte range [0,1).
        let ok = unsafe { LockFileEx(handle, flags, 0, 1, 0, &mut overlapped) };
        if ok == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    /// Release the byte `[0, 1)` lock. Best-effort — the handle close on drop
    /// also releases it.
    pub(super) fn unlock(file: &File) {
        let handle = file.as_raw_handle() as usize;
        let mut overlapped = Overlapped::default();
        let _ = unsafe { UnlockFileEx(handle, 0, 1, 0, &mut overlapped) };
    }
}

impl LeIndex {
    /// Try to create a directory and verify it is writable.
    fn try_create_dir(path: &Path) -> bool {
        std::fs::create_dir_all(path).is_ok()
            && std::fs::metadata(path)
                .map(|m| !m.permissions().readonly())
                .unwrap_or(false)
    }

    /// Find an existing storage directory without creating or modifying it.
    pub(crate) fn resolve_existing_storage_path(project_path: &Path) -> Option<PathBuf> {
        let path_hash = &blake3::hash(project_path.to_string_lossy().as_bytes()).to_hex()[..12];
        let dir_name = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");
        let in_project = project_path.join(".leindex");
        let env_path = std::env::var("LEINDEX_HOME")
            .ok()
            .map(|home| PathBuf::from(home).join(format!("{}-{}", dir_name, path_hash)));
        let xdg_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        let tmp_path = std::env::temp_dir()
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        std::iter::once(in_project)
            .chain(env_path)
            .chain([xdg_dir, tmp_path])
            .find(|candidate| candidate.is_dir())
    }

    /// Resolve the storage directory (in-project → LEINDEX_HOME → XDG → tmp).
    fn resolve_storage_path(project_path: &Path) -> Result<PathBuf> {
        if let Some(existing) = Self::resolve_existing_storage_path(project_path) {
            if Self::try_create_dir(&existing) {
                return Ok(existing);
            }
        }
        let path_hash = &blake3::hash(project_path.to_string_lossy().as_bytes()).to_hex()[..12];
        let dir_name = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown");

        // 1. Prefer in-project .leindex
        let in_project = project_path.join(".leindex");
        if Self::try_create_dir(&in_project) {
            return Ok(in_project);
        }

        // 2. LEINDEX_HOME env var
        if let Ok(home) = std::env::var("LEINDEX_HOME") {
            let env_path = PathBuf::from(home).join(format!("{}-{}", dir_name, path_hash));
            if Self::try_create_dir(&env_path) {
                warn!(
                    "Using LEINDEX_HOME fallback for storage: {}",
                    env_path.display()
                );
                return Ok(env_path);
            }
        }

        // 3. XDG data dir (~/.local/share/leindex/<hash>)
        let xdg_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        if Self::try_create_dir(&xdg_dir) {
            warn!(
                "Using XDG data dir fallback for storage: {}",
                xdg_dir.display()
            );
            return Ok(xdg_dir);
        }

        // 4. System temp dir
        let tmp_path = std::env::temp_dir()
            .join("leindex")
            .join(format!("{}-{}", dir_name, path_hash));
        std::fs::create_dir_all(&tmp_path).with_context(|| {
            format!(
                "Failed to create .leindex storage directory.\n\
                 Tried:\n\
                 1. {} (in-project)\n\
                 2. $LEINDEX_HOME (not set or not writable)\n\
                 3. {} (XDG data dir)\n\
                 4. {} (temp dir)\n\n\
                 Fix: Check directory permissions, or set LEINDEX_HOME env var to a writable path.",
                in_project.display(),
                xdg_dir.display(),
                tmp_path.display(),
            )
        })?;
        warn!(
            "Using temp dir fallback for storage: {}",
            tmp_path.display()
        );
        Ok(tmp_path)
    }

    /// Open storage with retry and exponential backoff.
    ///
    /// Each `Storage::open` attempt itself waits out SQLite's 5s busy_timeout
    /// (schema init takes write locks), so the retry budget must outlast a
    /// competing writer's WHOLE indexing run, not just one transaction: an
    /// external `leindex index --force` holds intermittent write locks for
    /// tens of seconds on large projects (stress-test measured ~31s on this
    /// repo with the legacy full-rewrite save). 6 attempts ≈ 6×5s busy
    /// windows + capped backoff ≈ 36s worst case, which covers the rebuild
    /// while remaining bounded.
    fn open_storage_with_retry(db_path: &Path, max_retries: u32) -> Result<Storage> {
        let mut attempt = 0;
        loop {
            match Storage::open(db_path) {
                Ok(s) => return Ok(s),
                Err(e) if attempt < max_retries => {
                    attempt += 1;
                    // Cap the backoff so late attempts do not stack multi-second
                    // sleeps on top of the multi-second busy windows.
                    let delay_ms = (100 * 2u64.saturating_pow(attempt)).min(2_000);
                    let delay = std::time::Duration::from_millis(delay_ms);
                    warn!(
                        "Storage open attempt {}/{} failed: {}. Retrying in {:?}",
                        attempt, max_retries, e, delay
                    );
                    std::thread::sleep(delay);
                }
                Err(e) => {
                    // Distinguish transient lock contention (SQLITE_BUSY /
                    // SQLITE_LOCKED — clears once the other writer finishes) from
                    // a genuine failure (corrupt DB, disk full). Only the
                    // contention case is tagged `[transient:lock-contention]` so
                    // the registry layer can avoid permanently bricking a
                    // generation on a transient storm (see
                    // `is_transient_storage_open_failure`) and the MCP layer can
                    // render honest "retry shortly" remediation instead of
                    // telling the user to delete a perfectly valid database. A
                    // genuine failure still bricks, correctly.
                    let lower = e.to_string().to_lowercase();
                    // Whitelist the exact SQLite transient-lock messages
                    // (SQLITE_BUSY/LOCKED from rusqlite) rather than a loose
                    // "lock"/"busy" substring. A loose match risks false
                    // positives (an error mentioning "lock" in prose) that
                    // would skip mark_index_failure for a genuine failure and
                    // leave it un-bricked, or false negatives that brick on a
                    // transient storm.
                    let is_lock_contention = lower.contains("database is locked")
                        || lower.contains("database table is locked")
                        || lower.contains("could not obtain a lock")
                        || lower.contains("database is busy");
                    return Err(e).with_context(|| {
                        if is_lock_contention {
                            format!(
                                "Failed to open storage at {} after {} attempts \
                                 [transient:lock-contention]. Another leindex process \
                                 is writing the database; the data is intact — retry \
                                 once it completes. Do NOT delete the database.",
                                db_path.display(),
                                max_retries,
                            )
                        } else {
                            format!(
                                "Failed to open storage at {} after {} attempts.\n\
                                 Suggestion: Delete {} and re-index, or check disk space.",
                                db_path.display(),
                                max_retries,
                                db_path.display()
                            )
                        }
                    });
                }
            }
        }
    }

    /// Acquire the cross-process write lock for this project's storage.
    ///
    /// Call at the top of any write entry point (`index_project_inner`,
    /// `incremental_reindex_from_watcher`) and hold the returned guard for the
    /// duration of the write (RAII releases on drop). Serializes concurrent
    /// writers across processes to prevent SQLite WAL contention/corruption.
    fn acquire_write_lock(&self) -> Result<ProjectWriteLock> {
        ProjectWriteLock::acquire(self.storage_path())
    }

    /// Non-blocking cross-process write lock: `Ok(Some(guard))` if free,
    /// `Ok(None)` if another process currently holds it. Used by the watcher
    /// so an in-flight index in another process makes the reindex SKIP (not
    /// block indefinitely — a blocking flock here would stall the watcher,
    /// since spawn_blocking can't be cancelled; see watcher.rs).
    pub(crate) fn try_acquire_write_lock(&self) -> Result<Option<ProjectWriteLock>> {
        ProjectWriteLock::try_acquire(self.storage_path())
    }

    /// Acquire the cross-process write lock, coalescing with a concurrent
    /// index in another process instead of queueing a redundant one.
    ///
    /// With multiple processes on one project (two MCP instances, MCP + CLI),
    /// the old blocking flock made the second writer sit in the queue for the
    /// first's *entire* index and then re-scan the tree itself — two tool
    /// calls would stall for minutes and finish within a second of each
    /// other. This variant polls the (cheap) lock while another process
    /// holds it, and once the lock is acquired re-checks staleness once: if
    /// the other process published a fresh index while we waited, this
    /// caller is done — no scan, no parse, no second generation. A forced
    /// reindex always blocks for the lock and always runs.
    ///
    /// Returns `Ok(None)` when the index was coalesced away (the caller
    /// should treat the project as freshly indexed).
    fn acquire_write_lock_coalescing(&self, force: bool) -> Result<Option<ProjectWriteLock>> {
        if force {
            return self.acquire_write_lock().map(Some);
        }
        loop {
            match self.try_acquire_write_lock()? {
                Some(guard) => {
                    // We hold the lock. If another process finished indexing
                    // while we waited, there is nothing left to do. The
                    // staleness check is one O(N) stat scan, paid once per
                    // coalesced run, never per poll. A run is skipped ONLY
                    // when no incomplete job checkpoint exists either: a
                    // failed attempt leaves a resumable checkpoint (marked
                    // "complete" only on success) that the next non-forced
                    // index is expected to finish publishing.
                    if !self.is_stale_fast() && self.no_incomplete_job() {
                        return Ok(None);
                    }
                    return Ok(Some(guard));
                }
                None => std::thread::sleep(std::time::Duration::from_millis(200)),
            }
        }
    }

    /// Whether every indexing job checkpoint is marked complete. An
    /// incomplete checkpoint means an attempt failed mid-pipeline and the
    /// next non-forced index must resume it (run_scan's checkpoint-reuse
    /// path), not treat the project as done.
    fn no_incomplete_job(&self) -> bool {
        crate::cli::index_job::latest_incomplete_job(self.storage_path()).is_none()
    }

    /// Create a new LeIndex instance for a project.
    ///
    /// ```ignore
    /// let leindex = LeIndex::new("/path/to/project")?;
    /// ```
    pub fn new<P: AsRef<Path>>(project_path: P) -> Result<Self> {
        let project_path = project_path
            .as_ref()
            .canonicalize()
            .context("Failed to canonicalize project path")?;

        let project_id = project_path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("unknown")
            .to_string();

        // Initialize storage with multi-location fallback and retry
        let storage_path = Self::resolve_storage_path(&project_path)?;

        // Write artifact ownership marker for GC (only for non-in-project storage)
        crate::cli::cleanup::write_artifact_marker(&storage_path);

        // Register at-exit cleanup for temp-based storage
        crate::cli::cleanup::register_at_exit_cleanup(storage_path.clone());

        // WS4 Task 10: one-time legacy → CAS generation-store migration on the
        // first-run path. Flag-gated (destructive sweep; ships behind a backup
        // warning). Runs before `open_storage_with_retry` so a migrated store
        // opens a fresh catalog; the search data lives in the CAS generation
        // store. Idempotent: no-op for stores already migrated; a failed
        // migration never blocks opening the project (the legacy layout still
        // serves).
        if crate::feature_flags::FeatureFlag::GenerationMigration.is_enabled() {
            let migrate_cfg = crate::storage::generation::migrate::MigrationConfig::default();
            match crate::storage::generation::migrate::migrate_legacy_store(
                &storage_path,
                &migrate_cfg,
            ) {
                Ok(report) if report.migrated || !report.was_noop() => {
                    info!(
                        storage = %storage_path.display(),
                        before = report.total_bytes_before,
                        after = report.total_bytes_after,
                        generations = report.generations_converted,
                        jobs_deleted = report.jobs_completed_deleted + report.jobs_byte_capped,
                        cas_blobs = report.cas_blob_count,
                        "Legacy store migration complete"
                    );
                }
                Ok(_) => {
                    debug!(
                        storage = %storage_path.display(),
                        "No legacy store migration needed"
                    );
                }
                Err(e) => {
                    warn!(
                        storage = %storage_path.display(),
                        error = %e,
                        "Legacy store migration failed; continuing with existing layout"
                    );
                }
            }
        }

        let db_path = storage_path.join("leindex.db");
        let storage = Self::open_storage_with_retry(&db_path, 6)?;

        // Generate unique project ID with conflict resolution
        // Load existing projects with same base name
        let existing_ids = storage
            .load_existing_ids(&project_id)
            .context("Failed to load existing project IDs from storage")?;
        let unique_id = UniqueProjectId::generate(&project_path, &existing_ids);

        // Store the project metadata
        storage
            .store_project_metadata(&unique_id, &project_path)
            .context("Failed to store project metadata")?;

        info!(
            "Creating LeIndex for project: {} (unique ID: {}) at {:?}",
            project_id,
            unique_id.to_string(),
            project_path
        );

        // Initialize search engine, configured with the documented `[search]`
        // knobs: `neural_weight` (previously dead config) plus the fragment
        // layer master switch + fusion weight. VAL-CONFIG.
        let search_engine = Self::configured_search_engine();

        // Initialize cache subsystem
        let cache_dir = storage_path.join("cache");
        let cache = crate::cli::index_cache::IndexCache::new(cache_dir)?;
        let project_config =
            crate::cli::config::ProjectConfig::load(&project_path).unwrap_or_default();

        let instance = Self {
            project_path,
            storage_path,
            project_id,
            unique_id,
            storage,
            search_engine,
            pdg: None,
            cache,
            project_config,
            stats: IndexStats {
                total_files: 0,
                files_parsed: 0,
                successful_parses: 0,
                failed_parses: 0,
                total_signatures: 0,
                signature_scope: "full".to_string(),
                pdg_nodes: 0,
                pdg_edges: 0,
                indexed_nodes: 0,
                indexing_time_ms: 0,
                external_deps_in_lockfile: 0,
                external_deps_resolved: 0,
                external_deps_unresolved: 0,
                external_deps_total: 0,
                external_deps_builtin: 0,
            },
            embedder: None,
            pipeline: None,
            generation_snapshot: None,
            hydrated_generation: std::sync::atomic::AtomicU64::new(0),
            last_index_coalesced: false,
        };

        // Restore persisted index stats (if any) so diagnostics can report
        // accurate totals without requiring a full re-index.
        let mut instance = instance;
        if let Err(err) = instance.load_stats_from_storage() {
            warn!("Failed to load persisted index stats: {err:#}");
        }

        Ok(instance)
    }

    // ---- Internal helpers ----

    fn collect_source_files_with_hashes(
        &mut self,
        refresh: bool,
    ) -> Result<Vec<(PathBuf, String)>> {
        let scan = self.get_project_scan(refresh)?;
        index_builder::collect_source_files_with_hashes(&scan)
    }

    fn collect_source_file_paths(&mut self, refresh: bool) -> Result<Vec<PathBuf>> {
        Ok(self.get_project_scan(refresh)?.source_paths)
    }

    fn get_project_scan(&mut self, refresh: bool) -> Result<ProjectFileScan> {
        if !refresh {
            if let Some(scan) = &self.cache.project_scan {
                return Ok(scan.clone());
            }
        }
        let project_id = self.project_id.clone();
        if !refresh {
            if let result @ Ok(_) = self
                .cache
                .get_project_scan(&project_id, false, || Err(anyhow::anyhow!("cache miss")))
            {
                return result;
            }
        }
        let scan = self.scan_project_files()?;
        self.cache.cache_project_scan(&project_id, &scan);
        self.cache.project_scan = Some(scan.clone());
        Ok(scan)
    }

    fn scan_project_files(&self) -> Result<ProjectFileScan> {
        index_builder::scan_project_files(&self.project_path)
    }

    /// Build a FreshnessContext for delegation to index_freshness module.
    fn freshness_context(&self) -> crate::cli::index_freshness::FreshnessContext<'_> {
        crate::cli::index_freshness::FreshnessContext {
            project_path: &self.project_path,
            storage_path: &self.storage_path,
            project_id: &self.project_id,
            storage: &self.storage,
            project_scan: self.cache.project_scan.as_ref(),
            cache_spiller: &self.cache.cache_spiller,
        }
    }

    pub(crate) fn indexing_batch_size(&self) -> usize {
        self.project_config.indexing.batch_size
    }

    fn search_cache_key_for(
        &self,
        query: &str,
        top_k: usize,
        query_type: Option<&crate::search::ranking::QueryType>,
        neural_available: bool,
    ) -> String {
        // Fold every result-affecting config knob + model identity into the key
        // so a config/model change invalidates the cache (not just a re-index).
        let cfg = crate::config::LeIndexConfig::load_cached();
        let rerank_model = std::env::var("LEINDEX_WORKER_RERANK_MODEL")
            .unwrap_or_else(|_| "qwen3-reranker-0.6b-seq-cls".to_string());
        // Task 8: the persisted fragment-layer content root hash also lands in
        // the key — a fragment re-embed (generation change) must invalidate the
        // query-result cache, mirroring the embed/rerank model discipline. The
        // manifest + root live under `.leindex/` (see persist_search_snapshot).
        let fragment_root_hash =
            index_builder::fragment::sync::load_fragment_root(&self.project_path.join(".leindex"))
                .ok()
                .flatten()
                .map(|root| root.root_hash)
                .unwrap_or_default();
        index_builder::search_cache_key_for(
            &self.project_id,
            &self.project_path,
            &self.stats,
            query,
            top_k,
            query_type,
            neural_available,
            &cfg.search.search_mode,
            cfg.search.neural_weight,
            cfg.search.rerank_enabled,
            cfg.search.rerank_top_n,
            cfg.search.fragment_index_enabled,
            cfg.search.fragment_weight,
            &fragment_root_hash,
            &cfg.neural.model_name,
            &rerank_model,
        )
    }

    fn analysis_cache_key_for(&self, query: &str, token_budget: usize) -> String {
        index_builder::analysis_cache_key_for(
            &self.project_id,
            &self.project_path,
            &self.stats,
            query,
            token_budget,
        )
    }

    // ---- Freshness delegation ----

    /// Check which source files have changed since last index.
    /// Returns (changed_paths, deleted_paths).
    pub fn check_freshness(&self) -> Result<(Vec<PathBuf>, Vec<String>)> {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::check_freshness(
            &ctx,
            || self.scan_project_files(),
            index_builder::hash_file,
        )
    }

    /// Check if any dependency manifest/lockfile has changed since last index.
    fn check_manifest_stale(&self) -> bool {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::check_manifest_stale(&ctx, || self.scan_project_files())
    }

    /// Fast-path freshness check: O(1) for indexed files, O(D) for source
    /// directories (typically 10-20), and O(M) for manifest files.
    pub fn is_stale_fast(&self) -> bool {
        let ctx = self.freshness_context();
        crate::cli::index_freshness::is_stale_fast(&ctx, || self.scan_project_files())
    }

    // ---- Accessors ----

    /// Get the project path.
    #[inline]
    pub fn project_path(&self) -> &Path {
        &self.project_path
    }

    /// Get the storage path used for index artifacts.
    #[inline]
    pub fn storage_path(&self) -> &Path {
        &self.storage_path
    }

    /// Get the project ID (legacy, for backward compatibility).
    #[inline]
    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    /// Get the unique project identifier (BLAKE3-based, conflict-free).
    #[inline]
    pub fn unique_id(&self) -> &UniqueProjectId {
        &self.unique_id
    }

    /// Get the display name for the project.
    #[inline]
    pub fn display_name(&self) -> String {
        self.unique_id.display()
    }

    /// Get a reference to the search engine.
    #[inline]
    pub fn search_engine(&self) -> &SearchEngine {
        &self.search_engine
    }

    /// Shut down any persistent ONNX daemon spawned during indexing or search.
    ///
    /// This should be called by CLI commands after their work is complete, before
    /// the process exits. The default `Drop` impl for `EmbeddingClient` passes
    /// `kill_persistent=false`, which leaves daemon workers running for reuse by
    /// the MCP server. CLI commands are short-lived and have no reason to keep
    /// the daemon alive, so this method forces a clean shutdown.
    ///
    /// This is a no-op when the `onnx` feature is disabled or no worker was
    /// ever spawned.
    pub fn shutdown_daemon(&mut self) {
        #[cfg(feature = "onnx")]
        if let Some(index_builder::HybridEmbedder::HybridLocal { neural, .. }) =
            self.embedder.as_ref()
        {
            neural.force_shutdown_daemon();
        }
    }

    /// Return the current neural enrichment state without starting a worker.
    pub fn neural_status(&self) -> &'static str {
        self.embedder
            .as_ref()
            .map(index_builder::HybridEmbedder::neural_status)
            .unwrap_or("absent")
    }

    /// Return the immutable generation selected for normal reads.
    pub(crate) fn active_storage_path(&self) -> PathBuf {
        crate::cli::live_project::LiveProject::resolve(&self.project_path.to_string_lossy())
            .map(|project| project.active_storage())
            .unwrap_or_else(|_| self.storage_path.clone())
    }

    /// Check indexed content in the same storage generation used by hydration.
    pub(crate) fn active_has_indexed_files(&self) -> bool {
        let active = self.active_storage_path();
        if active == self.storage_path {
            return crate::storage::pdg_store::has_indexed_files(&self.storage, &self.project_id);
        }
        crate::storage::schema::Storage::open_readonly(active.join("leindex.db"))
            .ok()
            .is_some_and(|storage| {
                crate::storage::pdg_store::has_indexed_files(&storage, &self.project_id)
            })
    }

    /// Get the PDG, if the project has been indexed.
    #[inline]
    pub fn pdg(&self) -> Option<&ProgramDependenceGraph> {
        self.pdg.as_deref()
    }

    /// Take the graph out as an owned value for mutation.
    ///
    /// Unshared (the normal case) it is unwrapped without copying; if a
    /// validator still holds it, it is cloned so that reader keeps a consistent
    /// snapshot.
    pub(crate) fn take_owned_pdg(&mut self) -> Option<ProgramDependenceGraph> {
        self.pdg.take().map(|shared| {
            std::sync::Arc::try_unwrap(shared).unwrap_or_else(|shared| (*shared).clone())
        })
    }

    /// Create a LogicValidator for this project's PDG and storage.
    ///
    /// Returns `None` if no PDG is available (project not yet indexed).
    /// The validator can be used to check edit changes for syntax errors,
    /// reference issues, semantic drift, and impact before applying.
    ///
    /// Opens a new Storage connection for the validator to avoid cloning
    /// the main connection (rusqlite::Connection is not Clone).
    pub fn create_validator(&self) -> Option<crate::validation::LogicValidator> {
        let pdg = self.pdg.as_ref()?;

        // Open a separate Storage connection for the validator.
        // Storage wraps rusqlite::Connection which is not Clone, so we
        // create a new read-only handle to the same database.
        let db_path = self.storage_path.join("leindex.db");
        let storage = crate::storage::schema::Storage::open(&db_path).ok()?;

        Some(crate::validation::LogicValidator::new(
            std::sync::Arc::clone(pdg),
            // Storage wraps rusqlite::Connection which is not Sync;
            // Arc is required by the LogicValidator interface for shared ownership.
            #[allow(clippy::arc_with_non_send_sync)]
            std::sync::Arc::new(storage),
        ))
    }

    /// Ensure the PDG is loaded from storage (deferred load on first use).
    pub fn ensure_pdg_loaded(&mut self) -> Result<()> {
        if self.pdg.is_none() {
            // WS4 Task 14: when the generation-read path is enabled, load the
            // PDG (and search engine) from the leased mmap generation instead.
            match self.try_hydrate_from_generation() {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => {
                    warn!(
                        "Generation read path unavailable ({}); falling back to legacy load",
                        e
                    );
                }
            }
            let has_content = self.active_has_indexed_files();
            if has_content {
                crate::cli::mcp::request_meta::PDG_LOADS
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let pdg_load_started = std::time::Instant::now();
                let result = self.load_from_storage();
                let pdg_ms = pdg_load_started.elapsed().as_millis().min(u64::MAX as u128) as u64;
                tracing::debug!(
                    project = %self.project_path.display(),
                    pdg_ms,
                    "PDG load attempt complete"
                );
                crate::cli::mcp::request_meta::record_pdg_ms(pdg_ms);
                result?;
            }
        }
        Ok(())
    }

    /// Ensure ONLY the PDG is loaded — never the search engine.
    ///
    /// Graph-only tools (read-symbol relations, symbol-lookup, project-map)
    /// traverse the graph but never query TF-IDF/neural vectors; hydrating
    /// the snapshot + embedding mmaps + index structures for them was ~1s of
    /// pure added latency per cold call. Falls back to the plain DB
    /// `load_pdg_from_active_storage` when the generation-read path is unavailable
    /// (legacy layout); it reads the published generation, never the mutable root.
    pub fn ensure_pdg_loaded_graph_only(&mut self) -> Result<()> {
        if self.pdg.is_some() {
            return Ok(());
        }
        match self.try_hydrate_generation_pdg_only() {
            Ok(true) => return Ok(()),
            Ok(false) => {}
            Err(e) => {
                warn!(
                    "Generation read path unavailable ({}); falling back to legacy PDG-only load",
                    e
                );
            }
        }
        let has_content = self.active_has_indexed_files();
        if has_content {
            // An empty/unindexed graph surfaces as pdg=None here; callers
            // already report "not loaded" semantics for that.
            let _ = self.load_pdg_from_active_storage();
        }
        Ok(())
    }

    /// A new search engine carrying the documented `[search]` knobs:
    /// `neural_weight`, the fragment layer master switch and its fusion weight.
    pub(crate) fn configured_search_engine() -> SearchEngine {
        let mut search_engine = SearchEngine::new();
        let cfg = crate::config::LeIndexConfig::load_cached();
        search_engine.set_neural_weight(cfg.neural_weight_f32());
        search_engine.set_fragment_index_enabled(cfg.search.fragment_index_enabled);
        search_engine.set_fragment_weight(cfg.search.fragment_weight as f32);
        search_engine
    }

    /// Whether this instance already holds what `full` (graph + search engine)
    /// or graph-only tools need.
    pub(crate) fn is_hydrated(&self, full: bool) -> bool {
        self.pdg.is_some() && (!full || !self.search_engine.is_empty())
    }

    /// Take over hydrated state built off-lock by `other` (a sibling instance
    /// of the same project).
    ///
    /// Building the graph and search engine takes up to a second; doing it on
    /// the instance that lives behind the per-project lock stalls every other
    /// call for that long. Callers build a detached instance instead and swap
    /// its state in here, which is a handful of pointer moves. Returns `false`
    /// (leaving `self` untouched) when `self` was hydrated in the meantime or
    /// `other` did not produce what was asked for.
    pub(crate) fn adopt_hydration(&mut self, mut other: LeIndex, full: bool) -> bool {
        if self.is_hydrated(full) || !other.is_hydrated(full) {
            return false;
        }
        self.pdg = other.pdg.take();
        self.stats.pdg_nodes = other.stats.pdg_nodes;
        self.stats.pdg_edges = other.stats.pdg_edges;
        self.generation_snapshot = other.generation_snapshot.take();
        self.hydrated_generation.store(
            other
                .hydrated_generation
                .load(std::sync::atomic::Ordering::Acquire),
            std::sync::atomic::Ordering::Release,
        );
        if full {
            self.search_engine = std::mem::take(&mut other.search_engine);
            self.embedder = other.embedder.take();
            self.stats = other.stats.clone();
            self.cache.file_stats_cache = other.cache.file_stats_cache.take();
        }
        true
    }

    /// Ensure the searchable context is ready for deep analysis / context tools.
    ///
    /// This loads the PDG if needed and performs a focused refresh when the
    /// in-memory search index is empty but indexed files already exist.
    pub fn ensure_analysis_context_loaded(&mut self) -> Result<()> {
        // WS4 Task 14: prefer the generation read path when enabled. It
        // hydrates both the PDG and the search engine in one shot, so the
        // legacy load only runs when the flag is off or no generation exists.
        match self.try_hydrate_from_generation() {
            Ok(true) if self.pdg.is_some() && !self.search_engine.is_empty() => return Ok(()),
            Ok(true) | Ok(false) => {}
            Err(e) => warn!(
                "Generation read path unavailable ({}); falling back to legacy load",
                e
            ),
        }
        self.ensure_pdg_loaded()?;
        if self.search_engine.is_empty() && self.active_has_indexed_files() {
            // The graph may already be resident (graph-only tool, prewarm):
            // hydrate just the engine on top of it rather than reloading both.
            self.hydrate_search_engine_from_loaded_pdg()?;
        }
        Ok(())
    }

    /// Get the current indexing statistics.
    #[inline]
    pub fn get_stats(&self) -> &IndexStats {
        &self.stats
    }

    /// Build file statistics cache from PDG
    pub(crate) fn build_file_stats_cache(&mut self) {
        if let Some(pdg) = &self.pdg {
            self.cache.build_file_stats_cache(pdg);
        }
    }

    /// Get file statistics cache.
    #[inline]
    pub fn file_stats(&self) -> Option<&HashMap<String, FileStats>> {
        self.cache.file_stats()
    }

    /// Get source file paths for the project (uses cached scan).
    pub fn source_file_paths(&mut self) -> Result<Vec<PathBuf>> {
        self.collect_source_file_paths(false)
    }

    /// Check if the project has been indexed.
    #[inline]
    pub fn is_indexed(&self) -> bool {
        // Persisted-stats truth, NOT the resident search engine: hydration
        // is lazy (graph-only tools never populate the engine), and keying
        // this on the engine would make every lazily-loaded project look
        // unindexed and trigger pointless auto-reindexes.
        self.stats.indexed_nodes > 0
    }

    /// Close the LeIndex and ensure WAL is checkpointed.
    pub fn close(&mut self) -> Result<()> {
        self.storage.close().context("Failed to close storage")?;
        info!("Closed LeIndex for project: {}", self.project_id);
        Ok(())
    }

    // ---- Cache Spilling & Reloading ----

    /// Check memory and spill cache if threshold exceeded.
    #[inline]
    pub fn check_memory_and_spill(&mut self) -> Result<bool> {
        self.cache.check_memory_and_spill()
    }

    /// Spill PDG cache to disk.
    #[inline]
    pub fn spill_pdg_cache(&mut self) -> Result<()> {
        self.cache.spill_pdg_cache(&self.project_id, &mut self.pdg)
    }

    /// Spill vector search cache to disk.
    #[inline]
    pub fn spill_vector_cache(&mut self) -> Result<()> {
        self.cache
            .spill_vector_cache(&self.project_id, self.search_engine.node_count())
    }

    /// Spill all caches (PDG and vector) to disk.
    #[inline]
    pub fn spill_all_caches(&mut self) -> Result<(usize, usize)> {
        self.cache.spill_all_caches(
            &self.project_id,
            &mut self.pdg,
            self.search_engine.node_count(),
        )
    }

    /// Reload PDG from cache (load from storage if not in memory).
    pub fn reload_pdg_from_cache(&mut self) -> Result<()> {
        if self.pdg.is_some() {
            info!("PDG already in memory, no reload needed");
            return Ok(());
        }
        info!("PDG not in memory, attempting to load from lestockage");
        self.load_from_storage()
    }

    /// Reload vector index from PDG.
    pub fn reload_vector_from_pdg(&mut self) -> Result<usize> {
        let pdg = self
            .take_owned_pdg()
            .ok_or_else(|| anyhow::anyhow!("No PDG available for vector rebuild"))?;

        let batch_size = self.indexing_batch_size();
        self.embedder = Some(index_builder::index_nodes(
            &pdg,
            &mut self.search_engine,
            &mut self.cache.file_stats_cache,
            batch_size,
        )?);
        let indexed_count = self.search_engine.node_count();

        self.pdg = Some(std::sync::Arc::new(pdg));
        self.build_file_stats_cache();

        info!("Rebuilt vector index from PDG: {} nodes", indexed_count);
        Ok(indexed_count)
    }

    /// Warm caches with frequently accessed data
    pub fn warm_caches(
        &mut self,
        strategy: WarmStrategy,
    ) -> Result<crate::cli::memory::WarmResult> {
        let result = self.cache.warm_cache(strategy)?;

        if (strategy == crate::cli::memory::WarmStrategy::PDGOnly
            || strategy == crate::cli::memory::WarmStrategy::All
            || strategy == crate::cli::memory::WarmStrategy::RecentFirst)
            && self.pdg.is_none()
        {
            info!("PDG warming requested but not in memory, reloading from lestockage");
            self.load_from_storage()?;
        }

        Ok(result)
    }

    /// Get cache statistics.
    #[inline]
    pub fn get_cache_stats(&self) -> Result<crate::cli::memory::MemoryStats> {
        self.cache.get_cache_stats()
    }

    // ---- Index Stats Persistence ----

    /// Persist IndexStats to a JSON file in the storage directory so that
    /// diagnostics can report accurate totals after loading from storage.
    pub(crate) fn save_stats_to_storage(&self) -> Result<()> {
        let stats_path = self.storage_path.join("index_stats.json");
        let json = serde_json::to_string(&self.stats).context("Failed to serialize IndexStats")?;
        std::fs::write(&stats_path, json)
            .with_context(|| format!("Failed to write index stats to {:?}", stats_path))?;
        Ok(())
    }

    /// Load IndexStats from the JSON file in the storage directory.
    /// Returns silently if the file does not exist (first run or pre-feature).
    pub(crate) fn load_stats_from_storage(&mut self) -> Result<()> {
        self.load_stats_from_path(&self.storage_path.clone())
    }

    /// Load IndexStats from an explicit storage directory.
    pub(crate) fn load_stats_from_path(&mut self, storage_path: &Path) -> Result<()> {
        let stats_path = storage_path.join("index_stats.json");
        if !stats_path.exists() {
            return Ok(());
        }
        let json = std::fs::read_to_string(&stats_path)
            .with_context(|| format!("Failed to read index stats from {:?}", stats_path))?;
        let stored: IndexStats =
            serde_json::from_str(&json).context("Failed to deserialize IndexStats")?;
        self.stats = stored;
        Ok(())
    }
}
