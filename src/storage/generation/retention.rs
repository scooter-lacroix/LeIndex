//! Byte-bounded retention for generations, CAS blobs, and jobs (WS4 Task 9).
//!
//! After a generation is published, [`retain_after_publish`] enforces three
//! independent retention policies:
//!
//! ## Generation retention
//!
//! Only **current + previous + leased** generations survive:
//! - **Current**: the generation pointed to by `CURRENT`.
//! - **Previous**: the immediately preceding generation (if any).
//! - **Leased**: any generation with at least one live `GenerationLease`,
//!   tracked by generation identity in the CAS refcount sidecar (not
//!   inferred from blob refcounts, which are shared across generations).
//!
//! All other generation directories are deleted entirely.
//!
//! ## CAS blob GC
//!
//! After generation pruning, blobs with refcount 0 that are not referenced
//! by any retained generation manifest are garbage-collected. This reclaims
//! disk space from superseded layers.
//!
//! ## Job retention
//!
//! Jobs under `jobs_dir` are bounded by [`RetentionConfig::job_bytes_max`]
//! (default 128 MiB):
//! - **Completed jobs** for the just-published generation are deleted
//!   immediately (zero resume value, spec section 3.6).
//! - Remaining completed jobs are pruned oldest-first until total bytes
//!   are at or below `job_bytes_max`.
//! - In-progress jobs are never deleted (they have checkpoint resume value).
//!
//! See BENCHMARKS.md Section 3 for the realized retention effect on this repo.

use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use tracing::{debug, info, warn};

use crate::storage::cas::blob::hash_to_hex;
use crate::storage::cas::{CasStore, RetentionReport};

use super::lease::{MANIFEST_FILE, read_current_generation};
use super::manifest::Manifest;

// ---------------------------------------------------------------------------
// RetentionConfig
// ---------------------------------------------------------------------------

/// Default maximum total bytes for jobs per project (128 MiB).
pub const DEFAULT_JOB_BYTES_MAX: u64 = 128 * 1024 * 1024;

/// Default number of retained generations (current + previous).
pub const DEFAULT_MAX_GENERATIONS: usize = 2;

/// Configuration for [`retain_after_publish`].
#[derive(Debug, Clone)]
pub struct RetentionConfig {
    /// Maximum number of generations to retain (current + previous = 2).
    /// Leased generations are always retained regardless of this limit.
    pub max_generations: usize,
    /// Maximum total bytes for completed jobs under `jobs_dir`.
    /// Oldest completed jobs are pruned first.
    pub job_bytes_max: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        RetentionConfig {
            max_generations: DEFAULT_MAX_GENERATIONS,
            job_bytes_max: DEFAULT_JOB_BYTES_MAX,
        }
    }
}

// ---------------------------------------------------------------------------
// Extended RetentionReport (generation + job stats)
// ---------------------------------------------------------------------------

/// Extended retention report covering generations, CAS blobs, and jobs.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct GenerationRetentionReport {
    /// CAS GC report (reclaimed bytes + blobs removed).
    pub cas: RetentionReport,
    /// Number of generation directories deleted.
    pub generations_removed: usize,
    /// Number of generation directories retained.
    pub generations_retained: usize,
    /// Number of completed jobs deleted because their generation is published.
    pub jobs_completed_deleted: usize,
    /// Number of jobs deleted to meet the byte cap (oldest-first).
    pub jobs_byte_capped: usize,
    /// Total bytes reclaimed from job deletions.
    pub job_bytes_reclaimed: u64,
    /// Total bytes currently in the jobs directory after retention.
    pub job_bytes_remaining: u64,
    /// Total bytes currently in the CAS store after GC.
    pub cas_bytes: u64,
    /// Number of blobs in the CAS store.
    pub cas_blob_count: usize,
    /// Number of generations in the generations directory.
    pub generation_count: usize,
    /// Dedup ratio: unique CAS blobs vs. total layer references across
    /// retained generations. E.g., 0.5 means half the references are
    /// duplicates (one blob serves multiple generations).
    pub dedup_ratio: f64,
    /// List of CAS blob hashes that are GC candidates (refcount 0, not pinned).
    pub gc_candidates: Vec<String>,
}

impl GenerationRetentionReport {
    fn write_generation_summary(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "Retention Report:")?;
        writeln!(f, "  Generations:")?;
        writeln!(f, "    retained: {}", self.generations_retained)?;
        writeln!(f, "    removed:  {}", self.generations_removed)
    }

    fn write_cas_summary(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  CAS:")?;
        writeln!(f, "    blob count:    {}", self.cas_blob_count)?;
        writeln!(f, "    total bytes:   {}", self.cas_bytes)?;
        writeln!(
            f,
            "    reclaimed:     {} bytes ({} blobs)",
            self.cas.reclaimed_bytes, self.cas.blobs_removed
        )?;
        writeln!(f, "    dedup ratio:   {:.2}", self.dedup_ratio)?;
        writeln!(f, "    GC candidates: {}", self.gc_candidates.len())
    }

    fn write_job_summary(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  Jobs:")?;
        writeln!(f, "    remaining:     {} bytes", self.job_bytes_remaining)?;
        writeln!(f, "    completed del: {}", self.jobs_completed_deleted)?;
        writeln!(f, "    byte-capped:   {}", self.jobs_byte_capped)?;
        writeln!(f, "    reclaimed:     {} bytes", self.job_bytes_reclaimed)
    }
}

impl std::fmt::Display for GenerationRetentionReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.write_generation_summary(f)?;
        self.write_cas_summary(f)?;
        self.write_job_summary(f)
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors returned by [`retain_after_publish`].
#[derive(Debug, thiserror::Error)]
pub enum RetentionError {
    /// I/O error during retention sweep.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// CAS error during GC.
    #[error("cas error: {0}")]
    Cas(#[from] crate::storage::cas::CasError),
    /// Manifest error reading generation manifests.
    #[error("manifest error: {0}")]
    Manifest(#[from] super::manifest::ManifestError),
}

// ---------------------------------------------------------------------------
// Generation directory scan helpers
// ---------------------------------------------------------------------------

/// Collect the generation numbers of the directories under `gens_dir`,
/// sorted ascending. Entries that are not directories or whose names are
/// not plain digits are ignored; a missing `gens_dir` yields an empty list.
fn read_generation_numbers(gens_dir: &Path) -> Result<Vec<u64>, RetentionError> {
    let mut gen_numbers: Vec<u64> = Vec::new();
    if gens_dir.exists() {
        for entry in fs::read_dir(gens_dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_dir() {
                continue;
            }
            if let Ok(num) = entry.file_name().to_string_lossy().parse::<u64>() {
                gen_numbers.push(num);
            }
        }
    }
    gen_numbers.sort_unstable();
    Ok(gen_numbers)
}

/// Load a generation's manifest, or `None` when it is missing or unparsable —
/// retention is best-effort and never fails on a single bad manifest.
fn generation_manifest(gen_dir: &Path) -> Option<Manifest> {
    let bytes = fs::read(gen_dir.join(MANIFEST_FILE)).ok()?;
    Manifest::from_bytes(&bytes).ok()
}

/// Anchor of the retention window: the generation pointed to by `CURRENT`
/// when it exists on disk, otherwise the newest generation. `gen_numbers`
/// must be non-empty and sorted.
fn window_anchor(gen_numbers: &[u64], current_gen: Option<u64>) -> u64 {
    current_gen
        .filter(|g| gen_numbers.contains(g))
        .unwrap_or_else(|| gen_numbers[gen_numbers.len() - 1])
}

/// Generations inside the retention window: the anchor plus its
/// `keep_count - 1` immediate predecessors (default 2 = current + previous).
/// When the anchor itself is missing from `gen_numbers` (stale `CURRENT` over
/// a partially cleaned store), the newest `keep_count` generations are kept
/// as a conservative fallback.
fn retention_window(gen_numbers: &[u64], anchor: u64, keep_count: usize) -> HashSet<u64> {
    if let Some(idx) = gen_numbers.iter().position(|g| *g == anchor) {
        let start = idx.saturating_sub(keep_count - 1);
        gen_numbers[start..=idx].iter().copied().collect()
    } else {
        let start = gen_numbers.len().saturating_sub(keep_count);
        gen_numbers[start..].iter().copied().collect()
    }
}

/// Remove a generation directory, treating an already-missing directory as
/// success (best-effort sweep). Failures other than `NotFound` are logged
/// and swallowed. Returns `true` when the directory was removed.
///
/// `label` distinguishes the CAS layout ("generation") from the legacy
/// full-copy layout ("legacy generation") in log output.
fn remove_generation_dir(gen_dir: &Path, label: &str, gen_num: u64) -> bool {
    match fs::remove_dir_all(gen_dir) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            warn!("retention: failed to remove {} {}: {}", label, gen_num, e);
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Core retention function
// ---------------------------------------------------------------------------

/// Enforce byte-bounded retention after a publish.
///
/// This is the main entry point for post-publish cleanup. It:
///
/// 1. Reads `CURRENT` to identify the current generation.
/// 2. Enumerates all generation directories under `gens_dir`.
/// 3. Retains: current, previous (current - 1), and leased (refcount > 0).
/// 4. Deletes all other generation directories.
/// 5. Collects blob hashes referenced by all retained generation manifests.
/// 6. Runs CAS GC: deletes blobs with refcount 0 not in the pinned set.
/// 7. Deletes completed jobs for the published generation.
/// 8. Prunes oldest completed jobs to meet `job_bytes_max`.
///
/// # Arguments
///
/// * `store` - The CAS store (refcounts determine which generations are leased).
/// * `gens_dir` - The `generations/` directory (e.g. `.leindex/generations/`).
/// * `jobs_dir` - The `jobs/` directory (e.g. `.leindex/jobs/`).
/// * `cfg` - Retention configuration (max_generations, job_bytes_max).
///
/// # Returns
///
/// A [`GenerationRetentionReport`] with detailed accounting of what was
/// retained, removed, and reclaimed.
pub fn retain_after_publish(
    store: &mut CasStore,
    gens_dir: &Path,
    jobs_dir: &Path,
    cfg: &RetentionConfig,
) -> Result<GenerationRetentionReport, RetentionError> {
    let mut report = GenerationRetentionReport::default();

    // Leases taken by other handles and processes since this store opened must
    // be visible before anything is deleted on the strength of a refcount.
    store.reload()?;

    // The current generation is the one pointed to by `CURRENT`. It is the
    // anchor for both generation pruning and the completed-job sweep.
    let current_gen = read_current_generation_from_gens_dir(gens_dir);

    // ---------------------------------------------------------------
    // Phase 1: Generation retention
    // ---------------------------------------------------------------
    let (retained, removed, pinned_hashes) =
        prune_generations(store, gens_dir, current_gen, cfg.max_generations)?;
    report.generations_retained = retained;
    report.generations_removed = removed;

    // ---------------------------------------------------------------
    // Phase 2: CAS GC — delete blobs with refcount 0 not pinned
    // ---------------------------------------------------------------
    let pinned_set: HashSet<[u8; 32]> = pinned_hashes.into_iter().collect();
    let gc_candidates = find_gc_candidates(store, &pinned_set);
    for hash in &gc_candidates {
        report.gc_candidates.push(hash_to_hex(hash));
    }
    report.cas = store.gc_with_pins(&pinned_set)?;

    // ---------------------------------------------------------------
    // Phase 3: Job retention
    // ---------------------------------------------------------------
    let (completed_deleted, byte_capped, job_bytes_reclaimed) =
        prune_jobs(jobs_dir, current_gen, cfg.job_bytes_max)?;
    report.jobs_completed_deleted = completed_deleted;
    report.jobs_byte_capped = byte_capped;
    report.job_bytes_reclaimed = job_bytes_reclaimed;

    // ---------------------------------------------------------------
    // Phase 4: Populate summary stats
    // ---------------------------------------------------------------
    report.cas_bytes = compute_cas_bytes(store)?;
    report.cas_blob_count = store.blob_count()?;
    report.generation_count = count_generation_dirs(gens_dir);
    report.job_bytes_remaining = dir_total_bytes(jobs_dir);
    report.dedup_ratio = compute_dedup_ratio(gens_dir, store)?;

    info!(
        "retention sweep complete: {} gens retained, {} gens removed, \
         {} CAS blobs ({} bytes), {} jobs deleted, {} bytes reclaimed from jobs",
        report.generations_retained,
        report.generations_removed,
        report.cas_blob_count,
        report.cas_bytes,
        report.jobs_completed_deleted + report.jobs_byte_capped,
        report.job_bytes_reclaimed,
    );

    Ok(report)
}

/// Generate a retention report without performing any deletions.
///
/// This is the read-only variant called by `leindex retention --report`.
/// It scans the generation store and reports statistics without modifying
/// anything.
pub fn retention_report(
    store: &CasStore,
    gens_dir: &Path,
    jobs_dir: &Path,
) -> Result<GenerationRetentionReport, RetentionError> {
    let mut report = GenerationRetentionReport::default();

    // Count generations and collect the layer hashes their manifests reference.
    let gen_numbers = read_generation_numbers(gens_dir)?;
    report.generation_count = gen_numbers.len();
    report.generations_retained = gen_numbers.len();
    let (total_layer_refs, unique_layer_hashes) = count_manifest_layers(gens_dir, &gen_numbers);

    // CAS stats
    report.cas_blob_count = store.blob_count()?;
    report.cas_bytes = compute_cas_bytes(store)?;

    // GC candidates: refcount 0 blobs not in any manifest
    report.gc_candidates = unmanaged_blob_hashes(store, &unique_layer_hashes)?;

    // Dedup ratio
    if total_layer_refs > 0 {
        report.dedup_ratio = 1.0 - (unique_layer_hashes.len() as f64 / total_layer_refs as f64);
    }

    // Job stats
    report.job_bytes_remaining = dir_total_bytes(jobs_dir);

    Ok(report)
}

/// Layer references across the given generations' manifests:
/// `(total_references, unique_hashes)`. Missing or unparsable manifests are
/// skipped (best-effort scan).
fn count_manifest_layers(gens_dir: &Path, gen_numbers: &[u64]) -> (usize, HashSet<[u8; 32]>) {
    let mut total_layer_refs = 0usize;
    let mut unique_layer_hashes: HashSet<[u8; 32]> = HashSet::new();
    for gen_num in gen_numbers {
        if let Some(manifest) = generation_manifest(&gens_dir.join(gen_num.to_string())) {
            let hashes = manifest.layer_hashes();
            total_layer_refs += hashes.len();
            for hash in &hashes {
                unique_layer_hashes.insert(*hash);
            }
        }
    }
    (total_layer_refs, unique_layer_hashes)
}

/// Stored CAS blob hashes with refcount 0 that no manifest references —
/// the GC candidate set for the read-only report.
fn unmanaged_blob_hashes(
    store: &CasStore,
    manifest_hashes: &HashSet<[u8; 32]>,
) -> Result<Vec<String>, RetentionError> {
    let stored = store.stored_hashes()?;
    let mut candidates = Vec::new();
    for hash in &stored {
        if store.refcount(hash) == 0 && !manifest_hashes.contains(hash) {
            candidates.push(hash_to_hex(hash));
        }
    }
    Ok(candidates)
}

// ---------------------------------------------------------------------------
// Generation pruning
// ---------------------------------------------------------------------------

/// Prune old generations, keeping current + previous + leased.
///
/// `current_gen` is the generation pointed to by `CURRENT` (if the pointer
/// exists). It anchors the retention window: the current generation plus its
/// `max_generations - 1` immediate predecessors are always retained, as are
/// any generations with a live generation lease (tracked by identity; see
/// [`retain_leased_generations`]).
///
/// Returns `(retained_count, removed_count, pinned_hashes)`.
fn prune_generations(
    store: &CasStore,
    gens_dir: &Path,
    current_gen: Option<u64>,
    max_generations: usize,
) -> Result<(usize, usize, Vec<[u8; 32]>), RetentionError> {
    let gen_numbers = read_generation_numbers(gens_dir)?;
    if gen_numbers.is_empty() {
        return Ok((0, 0, Vec::new()));
    }

    // Determine which generations to retain. The current generation is the
    // one pointed to by `CURRENT`; when the pointer is missing (e.g. first
    // publish, or a partially-written store) fall back to the newest
    // generation on disk.
    let current_gen = window_anchor(&gen_numbers, current_gen);
    let mut retained_gens = retention_window(&gen_numbers, current_gen, max_generations.max(1));

    // Check each generation for a live lease (tracked by generation
    // identity; see `retain_leased_generations`).
    retain_leased_generations(store, &gen_numbers, &mut retained_gens);

    // Collect pinned hashes from retained manifests.
    let pinned_hashes = collect_pinned_hashes(gens_dir, &retained_gens);

    // Delete non-retained generations. Leases are re-checked here: one taken
    // (and persisted) after the window was computed must still win, so the
    // directory removal itself cannot race a fresh reader.
    let leased_now = store.held_generations();
    let mut removed = 0;
    let retained_count = retained_gens.len();
    for gen_num in &gen_numbers {
        if !retained_gens.contains(gen_num) && !leased_now.contains(gen_num) {
            let gen_dir = gens_dir.join(gen_num.to_string());
            debug!(
                "retention: removing generation {} (not current/prev/leased)",
                gen_num
            );
            if remove_generation_dir(&gen_dir, "generation", *gen_num) {
                removed += 1;
            }
        }
    }

    Ok((retained_count, removed, pinned_hashes))
}

/// Retain every generation outside the window that is still leased.
///
/// Lease state is tracked by generation identity (recorded by
/// [`GenerationLease::acquire`](super::GenerationLease::acquire) alongside
/// the blob refcounts), not inferred from blob counts: a leased generation
/// and a historical one that shares its entire — unchanged — layer set have
/// identical blob refcounts, so any count-based test either pins every
/// look-alike generation forever (defeating the current-plus-previous bound)
/// or frees a genuinely leased one.
fn retain_leased_generations(store: &CasStore, gen_numbers: &[u64], retained: &mut HashSet<u64>) {
    let leased = store.held_generations();
    for gen_num in gen_numbers {
        if !retained.contains(gen_num) && leased.contains(gen_num) {
            retained.insert(*gen_num);
        }
    }
}

/// Blob hashes referenced by the retained manifests, deduplicated.
fn collect_pinned_hashes(gens_dir: &Path, retained: &HashSet<u64>) -> Vec<[u8; 32]> {
    let mut pinned_hashes: Vec<[u8; 32]> = Vec::new();
    for gen_num in retained {
        if let Some(manifest) = generation_manifest(&gens_dir.join(gen_num.to_string())) {
            for hash in manifest.layer_hashes() {
                if !pinned_hashes.contains(&hash) {
                    pinned_hashes.push(hash);
                }
            }
        }
    }
    pinned_hashes
}

/// Prune a legacy (pre-CAS, full-copy) generation store.
///
/// Legacy layout: `generations/<N>/` directories each contain complete
/// layer files (`leindex.db`, `embeddings.bin`, …) with NO `manifest` and
/// NO CAS blobs. Every generation is self-contained, so deleting a
/// directory can never corrupt another generation — unlike CAS stores,
/// where blobs are shared across generations and manifest pinning is
/// mandatory before GC. Leases cannot exist in this layout (a lease is a
/// CAS refcount), so the retention window is purely positional: the
/// current generation plus its `max_generations - 1` immediate
/// predecessors.
///
/// This is the reclaim path for stores that predate the CAS migration
/// (`leindex storage --migrate`) and therefore have no `cas/` directory;
/// [`retain_after_publish`] refuses to run without a CAS store, which is
/// how legacy stores historically accumulated unbounded generations.
///
/// When `dry_run` is true nothing is deleted; the report reflects what
/// *would* be removed.
pub fn retain_generations_no_cas(
    gens_dir: &Path,
    jobs_dir: &Path,
    max_generations: usize,
    dry_run: bool,
) -> Result<GenerationRetentionReport, RetentionError> {
    let mut report = GenerationRetentionReport::default();
    let current_gen = read_current_generation_from_gens_dir(gens_dir);

    let gen_numbers = read_generation_numbers(gens_dir)?;
    report.generation_count = gen_numbers.len();

    if gen_numbers.is_empty() {
        report.job_bytes_remaining = dir_total_bytes(jobs_dir);
        return Ok(report);
    }

    // Anchor the retention window at CURRENT; fall back to the newest
    // generation when the pointer is missing or dangling.
    let current = window_anchor(&gen_numbers, current_gen);
    let keep_count = max_generations.max(1);
    let retained = retention_window(&gen_numbers, current, keep_count);

    for gen_num in &gen_numbers {
        if retained.contains(gen_num) {
            continue;
        }
        if dry_run {
            report.generations_removed += 1;
            continue;
        }
        let gen_dir = gens_dir.join(gen_num.to_string());
        debug!(
            "retention: removing legacy generation {} (outside the {}-generation window)",
            gen_num, keep_count
        );
        if remove_generation_dir(&gen_dir, "legacy generation", *gen_num) {
            report.generations_removed += 1;
        }
    }
    report.generations_retained = retained.len();

    if !dry_run {
        let (completed_deleted, byte_capped, job_bytes_reclaimed) =
            prune_jobs(jobs_dir, Some(current), DEFAULT_JOB_BYTES_MAX)?;
        report.jobs_completed_deleted = completed_deleted;
        report.jobs_byte_capped = byte_capped;
        report.job_bytes_reclaimed = job_bytes_reclaimed;
    }
    report.job_bytes_remaining = dir_total_bytes(jobs_dir);
    report.generation_count = count_generation_dirs(gens_dir);

    Ok(report)
}

// ---------------------------------------------------------------------------
// Job pruning
// ---------------------------------------------------------------------------

/// Prune completed jobs for the current generation, then enforce byte cap.
///
/// Returns `(completed_deleted, byte_capped, bytes_reclaimed)`.
fn prune_jobs(
    jobs_dir: &Path,
    current_gen: Option<u64>,
    job_bytes_max: u64,
) -> Result<(usize, usize, u64), RetentionError> {
    if !jobs_dir.exists() {
        return Ok((0, 0, 0));
    }

    // Phase 1: Delete completed jobs whose generation is the current/published gen.
    // A job directory contains a `generation` file or the directory name encodes
    // the generation number. We look for jobs whose generation matches current_gen.
    let (completed_deleted, mut bytes_reclaimed) = match current_gen {
        Some(cur_gen) => delete_completed_jobs_for_current_gen(jobs_dir, cur_gen)?,
        None => (0, 0),
    };

    // Phase 2: Enforce byte cap on remaining completed jobs (oldest-first).
    let total_job_bytes = dir_total_bytes(jobs_dir);
    if total_job_bytes <= job_bytes_max {
        return Ok((completed_deleted, 0, bytes_reclaimed));
    }

    let completed_jobs = completed_jobs_oldest_first(jobs_dir)?;
    let (byte_capped, reclaimed) =
        enforce_job_byte_cap(&completed_jobs, total_job_bytes, job_bytes_max);
    bytes_reclaimed += reclaimed;

    Ok((completed_deleted, byte_capped, bytes_reclaimed))
}

/// Delete completed jobs that produced `current_gen` (zero resume value once
/// the generation is published). Returns the number of jobs deleted and the
/// bytes they occupied.
fn delete_completed_jobs_for_current_gen(
    jobs_dir: &Path,
    current_gen: u64,
) -> Result<(usize, u64), RetentionError> {
    let job_entries: Vec<PathBuf> = fs::read_dir(jobs_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.path())
        .collect();

    let mut completed_deleted = 0;
    let mut bytes_reclaimed: u64 = 0;
    for job_dir in &job_entries {
        // Check if this job produced the current generation.
        if job_produced_generation(job_dir, current_gen) && job_is_completed(job_dir) {
            let size = dir_total_bytes(job_dir);
            if let Err(e) = fs::remove_dir_all(job_dir) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    warn!(
                        "retention: failed to remove completed job {}: {}",
                        job_dir.display(),
                        e
                    );
                }
            } else {
                debug!(
                    "retention: deleted completed job {} (gen {} published)",
                    job_dir.display(),
                    current_gen
                );
                completed_deleted += 1;
                bytes_reclaimed += size;
            }
        }
    }
    Ok((completed_deleted, bytes_reclaimed))
}

/// Remaining completed jobs under `jobs_dir` as `(mtime, path, size)`, sorted
/// oldest first. In-progress jobs are excluded — they are never deleted to
/// meet the cap (they have checkpoint resume value).
fn completed_jobs_oldest_first(
    jobs_dir: &Path,
) -> Result<Vec<(std::time::SystemTime, PathBuf, u64)>, RetentionError> {
    let mut completed_jobs: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
    for entry in fs::read_dir(jobs_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if !job_is_completed(&path) {
            // In-progress jobs are never deleted to meet the cap.
            debug!(
                "retention: skipping in-progress job {} for byte cap",
                path.display()
            );
            continue;
        }
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        let size = dir_total_bytes(&path);
        completed_jobs.push((mtime, path, size));
    }

    // Sort oldest first.
    completed_jobs.sort_by_key(|(mtime, _, _)| *mtime);
    Ok(completed_jobs)
}

/// Delete oldest completed jobs until the total is at or below the cap.
/// Returns `(deleted_count, bytes_reclaimed)`.
fn enforce_job_byte_cap(
    completed_jobs: &[(std::time::SystemTime, PathBuf, u64)],
    total_job_bytes: u64,
    job_bytes_max: u64,
) -> (usize, u64) {
    let mut byte_capped = 0;
    let mut bytes_reclaimed: u64 = 0;
    let mut current_bytes = total_job_bytes;
    for (_mtime, path, size) in completed_jobs {
        if current_bytes <= job_bytes_max {
            break;
        }
        debug!(
            "retention: byte-capping job {} ({} bytes, oldest completed)",
            path.display(),
            size
        );
        if let Err(e) = fs::remove_dir_all(path) {
            if e.kind() != std::io::ErrorKind::NotFound {
                warn!(
                    "retention: failed to byte-cap job {}: {}",
                    path.display(),
                    e
                );
            }
        } else {
            byte_capped += 1;
            bytes_reclaimed += size;
            current_bytes = current_bytes.saturating_sub(*size);
        }
    }
    (byte_capped, bytes_reclaimed)
}

/// Read the current generation number from the gens_dir's parent (which
/// contains the CURRENT file).
fn read_current_generation_from_gens_dir(gens_dir: &Path) -> Option<u64> {
    // The CURRENT file lives in the storage root, which is the parent of
    // the generations/ directory.
    let storage_root = gens_dir.parent()?;
    read_current_generation(storage_root)
}

/// Check if a job directory produced a specific generation number.
///
/// Jobs record their target generation in a `generation` file inside the
/// job directory. The file contains the generation number as ASCII digits.
/// Checkpoint-style job stores name the directory after the generation
/// (plain digits) — those match on exact name as well.
fn job_produced_generation(job_dir: &Path, generation: u64) -> bool {
    let gen_file = job_dir.join("generation");
    if let Ok(content) = fs::read_to_string(&gen_file) {
        return content.trim() == generation.to_string();
    }
    // Fallback: check directory name for the generation number pattern.
    // Some job directories are named `<gen>-<timestamp>` or `<gen>_<id>`;
    // checkpoint stores use the bare `<gen>`.
    let name = job_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name == generation.to_string()
        || name.starts_with(&format!("{}-", generation))
        || name.starts_with(&format!("{}_", generation))
}

/// Check if a job is marked completed.
///
/// A job is "completed" if it has a `completed` marker file, a `status`
/// file containing "completed", a `checkpoint.json` recording completion,
/// or — the checkpoint-store layout — all three phase-complete markers
/// (`lexical.complete`, `pdg.complete`, `neural.complete`). The last phase
/// (`neural.complete`) only exists after the final phase succeeded, so its
/// presence alone also proves completion; requiring all three keeps the
/// check conservative against partially-renamed markers.
fn job_is_completed(job_dir: &Path) -> bool {
    // Check for a `completed` marker file.
    if job_dir.join("completed").exists() {
        return true;
    }
    // Check for a `status` file containing "completed".
    if let Ok(content) = fs::read_to_string(job_dir.join("status")) {
        if content.trim().eq_ignore_ascii_case("completed") {
            return true;
        }
    }
    // Check for `checkpoint.json` with a "completed" status.
    if let Ok(content) = fs::read_to_string(job_dir.join("checkpoint.json")) {
        if content.contains("\"completed\"") || content.contains("\"status\":\"completed\"") {
            return true;
        }
    }
    // Checkpoint-store layout: phase-complete markers.
    if job_dir.join("neural.complete").exists()
        && job_dir.join("pdg.complete").exists()
        && job_dir.join("lexical.complete").exists()
    {
        return true;
    }
    false
}

/// Compute total bytes of a directory recursively.
fn dir_total_bytes(path: &Path) -> u64 {
    if !path.exists() {
        return 0;
    }
    let mut total: u64 = 0;
    let mut stack = vec![path.to_path_buf()];
    while let Some(current) = stack.pop() {
        let entries = match fs::read_dir(&current) {
            Ok(e) => e,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(entry.path());
            } else {
                total += meta.len();
            }
        }
    }
    total
}

/// Count generation directories under `gens_dir`.
fn count_generation_dirs(gens_dir: &Path) -> usize {
    if !gens_dir.exists() {
        return 0;
    }
    let mut count = 0;
    if let Ok(entries) = fs::read_dir(gens_dir) {
        for entry in entries.flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                let name = entry.file_name();
                if name.to_string_lossy().parse::<u64>().is_ok() {
                    count += 1;
                }
            }
        }
    }
    count
}

/// Compute total bytes used by the CAS store on disk.
fn compute_cas_bytes(store: &CasStore) -> Result<u64, RetentionError> {
    let root = store.root();
    let mut total: u64 = 0;
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name();
            let name = name.to_string_lossy();
            // Skip refs.json sidecar directory entry (it's a file, not a dir).
            if name == "refs.json" || name == ".staging" {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                total += dir_total_bytes(&path);
            } else if entry.file_type().is_ok_and(|t| t.is_file()) && name != "refs.json" {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    Ok(total)
}

/// Compute the dedup ratio: fraction of layer references that are duplicates.
///
/// Returns a value in [0, 1): 0 means every layer reference is unique
/// (no dedup possible), 0.5 means half the references share an existing blob.
fn compute_dedup_ratio(gens_dir: &Path, _store: &CasStore) -> Result<f64, RetentionError> {
    let mut total_refs = 0usize;
    let mut unique: HashSet<[u8; 32]> = HashSet::new();

    if !gens_dir.exists() {
        return Ok(0.0);
    }

    for entry in fs::read_dir(gens_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let manifest_path = entry.path().join(MANIFEST_FILE);
        if let Ok(bytes) = fs::read(&manifest_path) {
            if let Ok(manifest) = Manifest::from_bytes(&bytes) {
                for hash in manifest.layer_hashes() {
                    total_refs += 1;
                    unique.insert(hash);
                }
            }
        }
    }

    if total_refs == 0 {
        return Ok(0.0);
    }
    Ok(1.0 - (unique.len() as f64 / total_refs as f64))
}

/// Find CAS blobs that are GC candidates (refcount 0 and not in pinned set).
fn find_gc_candidates(store: &CasStore, pinned: &HashSet<[u8; 32]>) -> Vec<[u8; 32]> {
    let mut candidates = Vec::new();
    if let Ok(hashes) = store.stored_hashes() {
        for hash in hashes {
            let rc = store.refcount(&hash);
            if rc == 0 && !pinned.contains(&hash) {
                candidates.push(hash);
            }
        }
    }
    candidates
}

#[cfg(test)]
#[path = "retention_test.rs"]
mod tests;
