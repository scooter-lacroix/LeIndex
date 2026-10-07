use super::*;

/// Cleanup command implementation — remove stale LeIndex temp artifacts
/// and/or sweep stale daemon sidecars (memory-pressure T7).
pub(super) async fn cmd_cleanup_impl(
    max_age_days: u64,
    dry_run: bool,
    stale_daemons: bool,
    store: bool,
    project: Option<PathBuf>,
) -> AnyhowResult<()> {
    use crate::cli::cleanup::{run_gc, sweep_stale_daemon_artifacts};
    use std::time::Duration;

    if store {
        println!(
            "LeIndex Cleanup — project store{}\n",
            if dry_run { " (dry run)" } else { "" }
        );
        let project_path = get_project_path(project);
        let canonical = project_path
            .canonicalize()
            .map_err(|e| anyhow::anyhow!("failed to canonicalize project path: {e}"))?;
        let storage_root = crate::cli::leindex::resolve_existing_storage_path(&canonical)
            .unwrap_or_else(|| canonical.join(".leindex"));
        println!("Cleaning: {}\n", storage_root.display());
        // Hold the project write lock for the sweep so it cannot interleave
        // with a concurrent publish (deleting a generation directory between
        // a publisher's manifest.partial write and its CURRENT swap would
        // abort the publish). Programmatic callers of
        // `cleanup_project_store` manage their own locking.
        let _write_guard = crate::cli::leindex::ProjectWriteLock::acquire(&storage_root)
            .map_err(|e| anyhow::anyhow!("failed to acquire project write lock: {e}"))?;
        let report = crate::cli::cleanup::cleanup_project_store(&storage_root, dry_run)?;
        println!("{}", report);
        return Ok(());
    }

    let max_age = Duration::from_secs(max_age_days * 24 * 3600);

    if stale_daemons {
        let label = if dry_run { " (dry run)\n" } else { "\n" };
        println!("LeIndex Cleanup — stale daemon sidecars{}", label);
        println!(
            "Sweeping ~/.leindex/run/ for dead-pid or >{} day(s) old worker/MCP sidecars...\n",
            max_age_days
        );
        let report = sweep_stale_daemon_artifacts(max_age, dry_run);
        println!("{}", report);
        return Ok(());
    }

    if dry_run {
        // In dry-run mode we scan but do not remove
        println!("LeIndex Cleanup (dry run)\n");
        println!(
            "Scanning for artifacts older than {} day(s)...\n",
            max_age_days
        );

        let report = run_gc_dry_run(max_age);
        println!("{}", report);
    } else {
        println!("LeIndex Cleanup\n");
        println!("Removing artifacts older than {} day(s)...\n", max_age_days);

        let report = run_gc(max_age);
        println!("{}", report);
    }

    Ok(())
}

/// `leindex retention` command implementation (WS4 Task 9).
///
/// Only the read-only `--report` mode is wired: it prints the generation
/// store's retention report (generation count, CAS bytes, job bytes, dedup
/// ratio, GC candidates) without modifying anything. The report logic lives
/// in [`crate::cli::cleanup::retention_report_cli`].
pub(super) async fn cmd_retention_impl(
    report: bool,
    gc: bool,
    max_generations: usize,
    dry_run: bool,
    project: Option<PathBuf>,
) -> AnyhowResult<()> {
    if gc {
        let output =
            crate::cli::cleanup::retention_gc_cli(project.as_deref(), max_generations, dry_run)?;
        if dry_run {
            println!("LeIndex Retention GC (dry run)\n");
        } else {
            println!("LeIndex Retention GC\n");
        }
        println!("{}", output.generation_report);
        return Ok(());
    }
    if !report {
        println!(
            "LeIndex Retention\n\n\
             Use `leindex retention --report` to print the generation-store\n\
             retention report (generation count, CAS bytes, job bytes, dedup\n\
             ratio, GC candidates) without deleting anything, or\n\
             `leindex retention --gc [--max-generations N] [--dry-run]` to\n\
             prune generations outside the retained window, GC orphaned CAS\n\
             blobs, and byte-cap completed jobs. The current generation is\n\
             never removed."
        );
        return Ok(());
    }
    let report = crate::cli::cleanup::retention_report_cli(project.as_deref())?;
    println!("{}", report);
    Ok(())
}

/// Resolve the `.leindex/` storage directory for `storage` subcommands without
/// creating anything.
pub(super) fn storage_dir_for(project: Option<PathBuf>) -> AnyhowResult<PathBuf> {
    let project_path = get_project_path(project);
    let canonical_path = project_path
        .canonicalize()
        .context("Failed to canonicalize project path")?;
    crate::cli::leindex::LeIndex::resolve_existing_storage_path(&canonical_path)
        .context("No existing LeIndex storage directory found for this project")
}

/// `leindex storage --status`: print the generation-store layout state.
pub(super) async fn cmd_storage_status_impl(project: Option<PathBuf>) -> AnyhowResult<()> {
    use crate::storage::generation::migrate::{is_legacy_full_copy_layout, is_migrated_store};

    let storage_dir = storage_dir_for(project)?;
    let legacy = is_legacy_full_copy_layout(&storage_dir);
    let migrated = is_migrated_store(&storage_dir);
    let cas_blobs = crate::storage::cas::CasStore::open(storage_dir.join("cas"))
        .map_err(|e| anyhow::anyhow!("failed to open CAS for status: {e}"))?
        .stored_hashes()
        .map(|h| h.len())
        .unwrap_or(0);
    println!(
        "LeIndex Storage\n\n\
         Path:   {}\n\
         Legacy full-copy layout: {}\n\
         CAS generation store:    {}\n\
         CAS blobs:               {}\n\
         CURRENT manifest:        {}\n\n\
         Use `leindex storage --migrate` to run the one-time migration sweep\n\
         (destructive — back up `.leindex/` first). A no-op run is safe.",
        storage_dir.display(),
        if legacy { "yes" } else { "no" },
        if migrated { "yes" } else { "no" },
        cas_blobs,
        match crate::storage::generation::lease::read_current_generation(&storage_dir) {
            Some(g) => format!("generation {g}"),
            None => "none".to_string(),
        }
    );
    Ok(())
}

/// `leindex storage --migrate`: run the one-time legacy → CAS migration sweep.
pub(super) async fn cmd_storage_migrate_impl(
    project: Option<PathBuf>,
    job_bytes_max: Option<u64>,
    footprint_mib: Option<u64>,
) -> AnyhowResult<()> {
    use crate::storage::generation::migrate::{MigrationConfig, migrate_legacy_store};

    let storage_dir = storage_dir_for(project)?;
    let cfg = MigrationConfig {
        job_bytes_max: job_bytes_max
            .unwrap_or(crate::storage::generation::retention::DEFAULT_JOB_BYTES_MAX),
        total_footprint_goal_bytes: footprint_mib.map(|mib| mib.saturating_mul(1024 * 1024)).or(
            Some(crate::storage::generation::migrate::DEFAULT_FOOTPRINT_GOAL_BYTES),
        ),
        emit_backup_warning: true,
        stop_after_publish: false,
    };
    let report =
        migrate_legacy_store(&storage_dir, &cfg).context("legacy store migration failed")?;
    println!(
        "Migration sweep complete\n\n\
         Layout migrated:      {}\n\
         Generations converted: {}\n\
         Generations deleted:   {}\n\
         Jobs completed deleted: {}\n\
         Jobs byte-capped:     {}\n\
         CAS blobs:            {}\n\
         Bytes before:         {} ({:.2} MiB)\n\
         Bytes after:          {} ({:.2} MiB)\n\
         Footprint goal:       {} MiB",
        report.migrated,
        report.generations_converted,
        report.generations_deleted,
        report.jobs_completed_deleted,
        report.jobs_byte_capped,
        report.cas_blob_count,
        report.total_bytes_before,
        report.total_bytes_before as f64 / (1024.0 * 1024.0),
        report.total_bytes_after,
        report.total_bytes_after as f64 / (1024.0 * 1024.0),
        cfg.total_footprint_goal_bytes
            .unwrap_or(crate::storage::generation::migrate::DEFAULT_FOOTPRINT_GOAL_BYTES)
            / (1024 * 1024),
    );
    if report.was_noop() {
        println!("\nStore was already migrated; nothing to do.");
    }
    Ok(())
}

/// Dry-run GC: scan and report without removing anything.
pub(super) fn run_gc_dry_run(max_age: std::time::Duration) -> crate::cli::cleanup::GcReport {
    use crate::cli::cleanup::artifact_scan_roots;
    use std::time::SystemTime;
    use tracing::debug;

    let mut report = crate::cli::cleanup::GcReport::default();
    let cutoff = SystemTime::now() - max_age;

    for root in artifact_scan_roots() {
        if !root.exists() {
            continue;
        }

        if root
            .file_name()
            .map(|n| n.to_string_lossy().starts_with("lephase-"))
            .unwrap_or(false)
        {
            count_artifact(&root, &cutoff, &mut report);
            continue;
        }

        let entries = match std::fs::read_dir(&root) {
            Ok(e) => e,
            Err(err) => {
                debug!("Cannot read {}: {}", root.display(), err);
                continue;
            }
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if path.file_name().map(|n| n == ".leindex").unwrap_or(false) {
                continue;
            }
            count_artifact(&path, &cutoff, &mut report);
        }
    }

    report
}

pub(super) fn count_artifact(
    dir: &std::path::Path,
    cutoff: &std::time::SystemTime,
    report: &mut crate::cli::cleanup::GcReport,
) {
    use crate::cli::cleanup::{
        artifact_age, dir_size, is_leindex_artifact, is_leindex_artifact_by_pattern,
    };
    use tracing::debug;

    if !is_leindex_artifact(dir) && !is_leindex_artifact_by_pattern(dir) {
        return;
    }

    report.scanned += 1;

    let age = artifact_age(dir);
    if age >= *cutoff {
        debug!("Artifact {} is not stale yet", dir.display());
        return;
    }

    let size = dir_size(dir);
    debug!(
        "Would remove stale artifact: {} ({:.2} MB)",
        dir.display(),
        size as f64 / 1024.0 / 1024.0
    );
    report.removed += 1;
    report.bytes_freed += size;
}
