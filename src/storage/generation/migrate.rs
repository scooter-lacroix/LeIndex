//! One-time legacy → CAS generation-store migration sweep (WS4 Task 10).
//!
//! The legacy `.leindex/` layout keeps a *full copy* of the index artifacts
//! per published generation (`generations/<N>/{leindex.db, embeddings.bin,
//! neural_embeddings.bin, search_snapshot.bin, tfidf_embedder.bin, ...}`), a
//! heap-mirrored top-level copy of the same artifacts, and an unbounded
//! `jobs/` accumulation. On a mature codebase this routinely measures in the
//! multi-GiB (this repository: ~5.9 GiB). [`migrate_legacy_store`] converts a
//! legacy store in place to the content-addressed generation layout: five
//! [`LayerKind`] blobs per retained generation, deduplicated into [`CasStore`]
//! and referenced from a [`Manifest`], with `CURRENT` swapped atomically
//! **last** so every intermediate crash point leaves the prior store serving.
//!
//! ## What is produced
//!
//! For the current + previous generations the migration synthesizes five
//! layers from the legacy artifacts:
//!
//! - **Db** — the legacy SQLite catalog, VACUUM-normalized into a canonical
//!   byte form (same engine as `db_layer`; byte-deterministic for dedup).
//! - **Tfidf** — the legacy `embeddings.bin` (`LIEE` frame) holds the dense
//!   768-d TF-IDF document vectors keyed by `intel_nodes.node_id`. They are
//!   re-encoded as sparse `(node, term, value)` triples in `LIDX-TFD1` (zeros
//!   dropped, so the dense vector is exactly recoverable). `doc_id` is the
//!   catalog integer id, matching the Pdg/Symbols layers.
//! - **Neural** — the legacy `neural_embeddings.bin` (same `LIEE` frame; only
//!   present when a neural model ran) becomes a version-2 `LIDX-NRL1` payload
//!   carrying the f32 rows plus a per-row PDG node-id table. A store with no
//!   neural file gets the canonical empty payload.
//! - **Pdg** — reconstructed from the legacy catalog's `intel_nodes` /
//!   `intel_edges` tables into the **version-2 (lossless)** `LIDX-PDG1`
//!   payload: graph node ids, symbol names, file paths, languages, types,
//!   complexity, byte ranges, precision markers and full edge metadata.
//! - **Symbols** — reconstructed from `intel_nodes` into `LIDX-SYM1`.
//!
//! ## Crash safety / idempotency
//!
//! The sequence is ordered so that `CURRENT` (the store's commit point) is
//! only moved after the new state is fully durable:
//!
//! 1. Stage every layer blob into CAS (additive).
//! 2. Write the manifest for each retained generation via the atomic
//!    partial→rename dance (`publish_manifest_only`), leaving `CURRENT`
//!    untouched.
//! 3. Swap `CURRENT` to the current generation **last**.
//! 4. Perform the destructive cleanup (stale generations, legacy full-copy
//!    files, top-level copies, job cap) — safe to interrupt, since the store
//!    is already fully migrated.
//!
//! A crash before step 3 leaves the legacy `CURRENT`-pointing layout intact
//! (an extra manifest is harmless); re-running resumes. A crash after
//! step 3 leaves a fully migrated store; re-running detects it and resumes
//! the cleanup from the last `CURRENT` swap. See [`MigrationReport`].
//!
//! ## Wiring
//!
//! [`migrate_legacy_store`] is invoked from `LeIndex::new` on the first-run
//! path (gated by the `GenerationMigration` feature flag; the migration is
//! destructive and ships behind a backup warning). It is also exposed as an
//! explicit `leindex storage migrate` CLI command for sanctioned one-time
//! runs.

use std::collections::{BTreeMap, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::storage::cas::CasStore;
use crate::storage::cas::RetentionReport;
use crate::storage::cas::blob::hash_to_hex;

use super::lease::{GENERATIONS_DIR, MANIFEST_FILE, read_current_generation};
use super::manifest::{LayerKind, Manifest};
use super::reader::{
    NEURAL_HEADER_LEN, NEURAL_MAGIC, NEURAL_VERSION_WITH_IDS, PDG_EDGE_LEN, PDG_HEADER_LEN,
    PDG_MAGIC, PDG_NODE_V2_LEN, PDG_STRING_OFFSET_LEN, PDG_V2_NONE, SYMBOL_ENTRY_LEN,
    SYMBOLS_HEADER_LEN, SYMBOLS_MAGIC, TFIDF_ENTRY_LEN, TFIDF_HEADER_LEN, TFIDF_MAGIC,
};
use super::retention::DEFAULT_JOB_BYTES_MAX;
use super::writer::GenerationWriter;

/// Default total footprint goal for a migrated store in bytes (200 MiB).
///
/// The destructive sweep prunes jobs so the whole `.leindex/` (CAS + retained
/// generations + cache + remaining jobs) lands at or below this size. This is
/// the Task 10 observable target this repository's store must hit.
pub const DEFAULT_FOOTPRINT_GOAL_BYTES: u64 = 200 * 1024 * 1024;

/// Bytes reserved for non-CAS, non-job top-level state (cache, edits, marker,
/// CURRENT, manifests) when computing the job budget from the footprint goal.
const RESERVED_MISC_BYTES: u64 = 8 * 1024 * 1024;

const JOB_MARKERS: [&str; 4] = [
    "parse.complete",
    "pdg.complete",
    "lexical.complete",
    "neural.complete",
];

/// Prefix of the mmap embedding file magic (`LIEE`).
const LIEE_MAGIC: [u8; 4] = *b"LIEE";
/// Size of the `LIEE` mmap header (magic + version + node_count + dimension).
const LIEE_HEADER_LEN: usize = 16;

/// Configuration for [`migrate_legacy_store`].
#[derive(Debug, Clone)]
pub struct MigrationConfig {
    /// Maximum total bytes for jobs kept after migration (default 128 MiB).
    pub job_bytes_max: u64,
    /// Total footprint goal for the migrated `.leindex/` (default 200 MiB).
    /// The job budget is clamped to `job_bytes_max` and to the headroom
    /// required to stay at or below this goal.
    pub total_footprint_goal_bytes: Option<u64>,
    /// Emit a prominent warning before the destructive sweep (backup advice).
    pub emit_backup_warning: bool,
    /// Test hook: halt immediately after the `CURRENT` swap (before cleanup)
    /// to simulate a crash at the post-commit boundary. Defaults to `false`.
    #[doc(hidden)]
    pub stop_after_publish: bool,
}

impl Default for MigrationConfig {
    fn default() -> Self {
        MigrationConfig {
            job_bytes_max: DEFAULT_JOB_BYTES_MAX,
            total_footprint_goal_bytes: Some(DEFAULT_FOOTPRINT_GOAL_BYTES),
            emit_backup_warning: true,
            stop_after_publish: false,
        }
    }
}

/// Detailed accounting of a migration run.
#[derive(Debug, Clone)]
pub struct MigrationReport {
    /// A legacy full-copy layout was detected at this storage root.
    pub detected_legacy: bool,
    /// A conversion actually ran (vs. an idempotent no-op / resume).
    pub migrated: bool,
    /// The current generation number (from `CURRENT`) after migration.
    pub current_generation: Option<u64>,
    /// The retained previous generation, if any.
    pub previous_generation: Option<u64>,
    /// Number of generations converted to manifests (current + previous).
    pub generations_converted: usize,
    /// Number of non-retained generation directories deleted.
    pub generations_deleted: usize,
    /// Bytes deleted from the CAS garbage-collection pass.
    pub cas_reclaimed_bytes: u64,
    /// Number of blobs in the CAS after migration.
    pub cas_blob_count: usize,
    /// Bytes live in the CAS after migration.
    pub cas_bytes: u64,
    /// Number of completed jobs deleted (zero resume value).
    pub jobs_completed_deleted: usize,
    /// Number of jobs deleted to satisfy the byte cap / footprint goal.
    pub jobs_byte_capped: usize,
    /// Bytes reclaimed from job deletions.
    pub job_bytes_reclaimed: u64,
    /// Bytes remaining in the jobs directory after migration.
    pub job_bytes_remaining: u64,
    /// Bytes reclaimed from deleting legacy full-copy + top-level artifacts.
    pub artifact_bytes_reclaimed: u64,
    /// Total apparent bytes of `.leindex/` before migration.
    pub total_bytes_before: u64,
    /// Total apparent bytes of `.leindex/` after migration.
    pub total_bytes_after: u64,
    /// Migrant layer blob hashes (useful for auditing the dedup).
    pub blob_hashes: Vec<String>,
    /// Warning messages emitted about destructive behavior.
    pub warnings: Vec<String>,
}

impl MigrationReport {
    /// True when the migration is a confirmed no-op because the store was
    /// already migrated.
    pub fn was_noop(&self) -> bool {
        !self.detected_legacy && self.generations_converted == 0
    }
}

/// Errors returned by [`migrate_legacy_store`].
#[derive(Debug, thiserror::Error)]
pub enum MigrationError {
    /// I/O error during conversion or sweep.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// CAS error.
    #[error("cas error: {0}")]
    Cas(#[from] crate::storage::cas::CasError),
    /// Writer error (stage / manifest / CURRENT).
    #[error("writer error: {0}")]
    Writer(#[from] super::writer::WriterError),
    /// SQLite error reading the legacy catalog.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// A legacy artifact was missing or malformed.
    #[error("legacy artifact error: {0}")]
    Legacy(String),
    /// Invalid / unsupported legacy payload.
    #[error("legacy payload error: {0}")]
    Payload(String),
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// The pre-migration report: legacy-layout detection plus zeroed counters.
fn empty_migration_report(storage_root: &Path) -> MigrationReport {
    MigrationReport {
        detected_legacy: is_legacy_full_copy_layout(storage_root),
        migrated: false,
        current_generation: None,
        previous_generation: None,
        generations_converted: 0,
        generations_deleted: 0,
        cas_reclaimed_bytes: 0,
        cas_blob_count: 0,
        cas_bytes: 0,
        jobs_completed_deleted: 0,
        jobs_byte_capped: 0,
        job_bytes_reclaimed: 0,
        job_bytes_remaining: 0,
        artifact_bytes_reclaimed: 0,
        total_bytes_before: dir_total_bytes(storage_root),
        total_bytes_after: 0,
        blob_hashes: Vec::new(),
        warnings: Vec::new(),
    }
}

/// The legacy generations to convert (current + existing previous) and the
/// previous generation, if it exists.
fn retained_legacy_generations(
    storage_root: &Path,
) -> Result<(Vec<u64>, Option<u64>), MigrationError> {
    let current = read_current_generation(storage_root)
        .ok_or_else(|| MigrationError::Legacy("legacy CURRENT points to no generation".into()))?;
    let previous = current
        .checked_sub(1)
        .filter(|p| legacy_generation_dir_exists(storage_root, *p));
    let retained: Vec<u64> = [previous, Some(current)].into_iter().flatten().collect();
    if retained.is_empty() {
        return Err(MigrationError::Legacy(format!(
            "no retained generations found near current {current}"
        )));
    }
    Ok((retained, previous))
}

/// Emit the one-time migration warning (irreversible rewrite) and record it
/// in the report.
fn warn_and_record_backup_advice(
    storage_root: &Path,
    retained: &[u64],
    current: u64,
    previous: Option<u64>,
    report: &mut MigrationReport,
) {
    let msg = format!(
        "one-time legacy→CAS store migration: converting {} generation(s) \
         (current={}, previous={:?}) at {}; this irreversibly rewrites the \
         store footprint. Back up `.leindex/` before proceeding if the \
         content must be preserved verbatim.",
        retained.len(),
        current,
        previous,
        storage_root.display()
    );
    tracing::warn!("{}", msg);
    report.warnings.push(msg);
}

/// Convert every retained generation and publish it: manifest-only for the
/// previous generation, full `CURRENT` swap for the current one — the swap
/// happens last so the store is fully migrated at the commit point.
fn migrate_retained_generations(
    writer: &mut GenerationWriter,
    storage_root: &Path,
    retained: &[u64],
    current: u64,
    report: &mut MigrationReport,
) -> Result<(), MigrationError> {
    for g in retained {
        convert_generation(writer, storage_root, *g)?;
        if *g == current {
            // Swap CURRENT last: the store is now fully migrated.
            writer.publish(current)?;
            report.migrated = true;
        } else {
            writer.publish_manifest_only(*g)?;
        }
        report.generations_converted += 1;
    }
    Ok(())
}

/// Record the current manifest's layer hashes (dedup audit); a store without
/// a readable manifest simply contributes none.
fn record_current_manifest_hashes(storage_root: &Path, current: u64, report: &mut MigrationReport) {
    if let Ok(manifest) = read_generation_manifest(storage_root, current) {
        for hash in manifest.layer_hashes() {
            report.blob_hashes.push(hash_to_hex(&hash));
        }
    }
}

/// Store stats now that CAS is populated.
fn record_cas_stats(
    cas: &Arc<Mutex<CasStore>>,
    report: &mut MigrationReport,
) -> Result<(), MigrationError> {
    report.cas_blob_count = cas.lock().expect("cas lock").blob_count()?;
    report.cas_bytes = compute_cas_bytes(&cas.lock().expect("cas lock"));
    Ok(())
}

/// Migrate a legacy full-copy `.leindex/` store in place to the CAS-backed
/// generation layout (WS4 Task 10 / VAL-MIGRATE-001..005).
///
/// Idempotent and crash-safe: a no-op when the store is already migrated, and
/// a crash at any point resumes from the last `CURRENT` swap. The destructive
/// cleanup runs only after the store is durably migrated.
pub fn migrate_legacy_store(
    storage_root: &Path,
    cfg: &MigrationConfig,
) -> Result<MigrationReport, MigrationError> {
    let mut report = empty_migration_report(storage_root);

    // Idempotency / crash-resume: a store that is not in the legacy layout but
    // already migrated (CURRENT → manifest) still runs the destructive cleanup
    // to finish a crash- or error-interrupted transition.
    if !report.detected_legacy {
        if is_migrated_store(storage_root) {
            cleanup_migrated_store(storage_root, cfg, &mut report)?;
        }
        report.total_bytes_after = dir_total_bytes(storage_root);
        return Ok(report);
    }

    let (retained, previous) = retained_legacy_generations(storage_root)?;
    let current = *retained.last().expect("retained generations are non-empty");

    if cfg.emit_backup_warning {
        warn_and_record_backup_advice(storage_root, &retained, current, previous, &mut report);
    }

    let cas_root = storage_root.join("cas");
    let cas = Arc::new(Mutex::new(CasStore::open(&cas_root)?));

    let mut writer = GenerationWriter::new(storage_root, cas.clone());
    migrate_retained_generations(&mut writer, storage_root, &retained, current, &mut report)?;
    report.current_generation = Some(current);
    report.previous_generation = previous;

    // Record layer hashes for the current manifest (dedup audit).
    record_current_manifest_hashes(storage_root, current, &mut report);

    // Store stats now that CAS is populated.
    record_cas_stats(&cas, &mut report)?;

    report.total_bytes_after = dir_total_bytes(storage_root);

    // Test hook: simulate a crash immediately after the CURRENT swap but
    // before the destructive cleanup (the resume boundary).
    if cfg.stop_after_publish {
        return Ok(report);
    }

    cleanup_migrated_store(storage_root, cfg, &mut report)?;

    report.total_bytes_after = dir_total_bytes(storage_root);
    Ok(report)
}

/// Detect the legacy full-copy layout: `CURRENT` -> `generations/<N>` that
/// holds a full copy (`leindex.db`) and no `manifest`.
pub fn is_legacy_full_copy_layout(storage_root: &Path) -> bool {
    let Some(current) = read_current_generation(storage_root) else {
        return false;
    };
    let gen_dir = generation_dir(storage_root, current);
    gen_dir.join("leindex.db").is_file() && !gen_dir.join(MANIFEST_FILE).is_file()
}

/// True when the store is already in the CAS-backed layout: `CURRENT` points
/// at a generation with a valid `manifest`.
pub fn is_migrated_store(storage_root: &Path) -> bool {
    let Some(current) = read_current_generation(storage_root) else {
        return false;
    };
    read_generation_manifest(storage_root, current).is_ok()
}

// ---------------------------------------------------------------------------
// Conversion
// ---------------------------------------------------------------------------

/// Convert every layer for `g` into stage blobs on `writer`.
fn convert_generation(
    writer: &mut GenerationWriter,
    storage_root: &Path,
    g: u64,
) -> Result<(), MigrationError> {
    let gen_dir = generation_dir(storage_root, g);
    if !gen_dir.join("leindex.db").is_file() {
        return Err(MigrationError::Legacy(format!(
            "generation {} has no leindex.db full copy",
            g
        )));
    }

    // Db layer: VACUUM-normalized legacy catalog. Also yields a checkpointed
    // copy handle used by the Pdg/Symbols reconstruction.
    let db_handle = copy_and_checkpoint(&gen_dir.join("leindex.db"))?;
    let db_bytes = vacuum_bytes(&db_handle.path)?;
    writer.stage(LayerKind::Db, &db_bytes)?;

    let conn = Connection::open(&db_handle.path)?;
    let node_ids = load_node_id_map(&conn)?;

    // Tfidf layer: `embeddings.bin` holds the dense TF-IDF document vectors;
    // re-encode them as sparse (node, term, value) triples.
    let tfidf_bytes = encode_tfidf_layer(&gen_dir.join("embeddings.bin"), &node_ids)?;
    writer.stage(LayerKind::Tfidf, &tfidf_bytes)?;

    // Neural layer: `neural_embeddings.bin` (absent when no neural model ran).
    let neural_bytes = encode_neural_layer(&gen_dir.join("neural_embeddings.bin"), &node_ids)?;
    writer.stage(LayerKind::Neural, &neural_bytes)?;

    // Pdg + Symbols: reconstructed from the legacy catalog. The PDG layer is
    // the lossless version-2 form (see encode_pdg_layer_v2).
    let pdg_bytes = encode_pdg_layer_v2(&conn)?;
    writer.stage(LayerKind::Pdg, &pdg_bytes)?;
    let symbols_bytes = encode_symbols_layer(&conn)?;
    writer.stage(LayerKind::Symbols, &symbols_bytes)?;

    // The db_handle tempdir dir lives until drop at end of this function.
    drop(db_handle);
    Ok(())
}

/// A checkpointed copy of a legacy catalog (temp-owned) plus its tempdir guard.
struct NormalizedDb {
    _tmp: tempfile::TempDir,
    path: PathBuf,
}

/// Copy `src` (replaying any WAL) into a temp dir and checkpoint it, so
/// subsequent VACUUM / reads see a single consistent snapshot without mutating
/// the source. Returns the checkpointed copy path.
///
/// A `-wal` sidecar that exists but cannot be copied is a hard error, not a
/// skip: opening the copy without it silently rolls back to the pre-WAL
/// state, and the destructive sweep later deletes both `leindex.db` and its
/// WAL from the retained generation — every WAL-only transaction would be
/// lost with no surfaced failure.
fn copy_and_checkpoint(src: &Path) -> Result<NormalizedDb, MigrationError> {
    let tmp = tempfile::tempdir()?;
    let copy = tmp.path().join("catalog.db");
    fs::copy(src, &copy)?;
    for sidecar in ["-wal", "-shm"] {
        let s = src.with_extension(format!("db{}", sidecar));
        if s.is_file() {
            fs::copy(&s, tmp.path().join(format!("catalog.db{}", sidecar)))?;
        }
    }
    // Open read-write so a trailing WAL is replayed, then checkpoint it away.
    let conn = Connection::open(&copy)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    drop(conn);
    Ok(NormalizedDb {
        _tmp: tmp,
        path: copy,
    })
}

/// Produce the canonical VACUUM-normalized byte form of a catalog. This is the
/// byte-deterministic DB payload staged into CAS (same engine as `db_layer`).
pub(crate) fn vacuum_bytes(db_path: &Path) -> Result<Vec<u8>, MigrationError> {
    let tmp = tempfile::tempdir()?;
    let out = tmp.path().join("normalized.db");
    let conn = Connection::open(db_path)?;
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    let escaped = out.to_string_lossy().replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{}';", escaped))?;
    drop(conn);
    canonicalize_volatile_rows(&out)?;
    let bytes = fs::read(&out)?;
    Ok(bytes)
}

/// Whether `table` exists (fixtures and legacy catalogs may lack the newer
/// bookkeeping tables entirely).
fn table_exists(conn: &Connection, table: &str) -> Result<bool, MigrationError> {
    let mut stmt =
        conn.prepare("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1")?;
    Ok(stmt.exists([table])?)
}

/// Whether `table` exists AND carries every column in `columns`. A fixture or
/// legacy catalog may reuse a table name with an older layout, which must be
/// left untouched rather than rewritten.
fn table_has_columns(
    conn: &Connection,
    table: &str,
    columns: &[&str],
) -> Result<bool, MigrationError> {
    if !table_exists(conn, table)? {
        return Ok(false);
    }
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let present = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<String>, _>>()?;
    Ok(columns
        .iter()
        .all(|want| present.iter().any(|have| have == want)))
}

/// Rewrite `table` with its rows in `ORDER BY <key>` order, in place. VACUUM
/// alone preserves a b-tree's existing cell order, so two catalogs whose rows
/// were inserted in different orders stay byte-different even when their
/// logical contents are identical; rebuilding through a temp table with a
/// deterministic ORDER BY yields one canonical byte image.
fn rebuild_table_sorted(
    conn: &Connection,
    table: &str,
    order_by: &str,
) -> Result<(), MigrationError> {
    if !table_exists(conn, table)? {
        return Ok(());
    }
    conn.execute_batch(&format!(
        "CREATE TABLE {table}_canonical AS
             SELECT * FROM {table} ORDER BY {order_by};
         DROP TABLE {table};
         ALTER TABLE {table}_canonical RENAME TO {table};"
    ))?;
    Ok(())
}

/// Zero or collapse the per-run rows that make two catalogs of identical
/// content compare unequal, so the staged Db layer dedups across generations.
///
/// - `project_metadata` grows one row per CLI invocation (the instance counter
///   disambiguates concurrent opens of the same base name). The staged copy
///   keeps a single row with the canonical `<base>_<hash>_0` identity, and
///   every `project_id` reference in the catalog is rewritten to match.
/// - Telemetry counters, community timestamps and `last_indexed` clocks are
///   zeroed: they are mutable-root bookkeeping, not reader state.
/// - Large tables are rebuilt in canonical row order so the byte image does
///   not depend on the insertion order of the run that produced it.
///
/// A final in-place `VACUUM` renormalizes the page layout after the edits.
pub(crate) fn canonicalize_volatile_rows(db_path: &Path) -> Result<(), MigrationError> {
    use rusqlite::OptionalExtension;

    let conn = Connection::open(db_path)?;
    // Collapse the per-invocation project rows to the canonical identity.
    // Fixtures and legacy catalogs may lack the table entirely.
    let survivor: Option<(String, String, String, String, String, bool, String)> =
        if table_has_columns(
            &conn,
            "project_metadata",
            &[
                "unique_project_id",
                "base_name",
                "path_hash",
                "instance",
                "canonical_path",
            ],
        )? {
            conn.query_row(
                "SELECT unique_project_id, base_name, path_hash, canonical_path,
                    COALESCE(display_name, ''), is_clone, COALESCE(cloned_from, '')
             FROM project_metadata
             ORDER BY instance DESC, id DESC
             LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?
        } else {
            None
        };
    if let Some((
        _old_id,
        base_name,
        path_hash,
        canonical_path,
        display_name,
        is_clone,
        cloned_from,
    )) = survivor
    {
        let canonical_id = format!("{base_name}_{path_hash}_0");
        let stale_ids: Vec<String> = {
            let mut stmt = conn.prepare("SELECT unique_project_id FROM project_metadata")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        conn.execute("DELETE FROM project_metadata", [])?;
        conn.execute(
            "INSERT INTO project_metadata
                 (id, unique_project_id, base_name, path_hash, instance, canonical_path,
                  display_name, is_clone, cloned_from, created_at, last_indexed)
             VALUES (0, ?1, ?2, ?3, 0, ?4, ?5, ?6, ?7,
                     '1970-01-01 00:00:00', '1970-01-01 00:00:00')",
            rusqlite::params![
                canonical_id,
                base_name,
                path_hash,
                canonical_path,
                display_name,
                is_clone,
                cloned_from
            ],
        )?;
        // Rewrite every table that keys rows by the project identity.
        let tables: Vec<String> = {
            let mut stmt = conn.prepare("SELECT name FROM sqlite_master WHERE type = 'table'")?;
            let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
            rows.collect::<Result<Vec<_>, _>>()?
        };
        for table in tables {
            let has_project_id = {
                let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
                let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
                rows.collect::<Result<Vec<_>, _>>()?
                    .iter()
                    .any(|column| column == "project_id")
            };
            if !has_project_id {
                continue;
            }
            for stale in &stale_ids {
                if stale != &canonical_id {
                    conn.execute(
                        &format!("UPDATE {table} SET project_id = ?1 WHERE project_id = ?2"),
                        rusqlite::params![canonical_id, stale],
                    )?;
                }
            }
        }
    }
    // Every statement below is guarded per table: fixtures and legacy
    // catalogs may not carry the newer bookkeeping tables.
    if table_exists(&conn, "cache_telemetry")? {
        conn.execute_batch(
            "UPDATE cache_telemetry
                 SET cache_hits = 0,
                     cache_misses = 0,
                     cache_writes = 0,
                     updated_at = 0,
                     community_recompute_ms = 0;",
        )?;
    }
    if table_exists(&conn, "intel_communities")? {
        conn.execute_batch("UPDATE intel_communities SET computed_at = 0;")?;
    }
    if table_exists(&conn, "indexed_files")? {
        conn.execute_batch("UPDATE indexed_files SET last_indexed = 0;")?;
    }
    if table_exists(&conn, "sqlite_sequence")? {
        conn.execute_batch("DELETE FROM sqlite_sequence WHERE name = 'project_metadata';")?;
    }
    rebuild_table_sorted(&conn, "indexed_files", "file_path")?;
    rebuild_table_sorted(
        &conn,
        "intel_community_memberships",
        "project_id, node_id, community",
    )?;
    rebuild_table_sorted(
        &conn,
        "intel_communities",
        "community, algorithm, quality_name",
    )?;
    // `schema_version` is rewritten via INSERT OR REPLACE each run, so its
    // rowid drifts (1, 2, ...); rebuilding pins it.
    rebuild_table_sorted(&conn, "schema_version", "key")?;
    conn.execute_batch("VACUUM;")?;
    Ok(())
}

/// Catalog `intel_nodes.node_id` (the text id legacy embedding files are keyed
/// by) -> `intel_nodes.id` (the integer id the PDG/Symbols layers use).
pub(crate) fn load_node_id_map(conn: &Connection) -> Result<HashMap<String, u32>, MigrationError> {
    let mut q = conn.prepare("SELECT node_id, id FROM intel_nodes")?;
    let rows = q.query_map([], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (node_id, db_id) = row?;
        let db_id = u32::try_from(db_id)
            .map_err(|_| MigrationError::Payload("node id exceeds u32".into()))?;
        map.insert(node_id, db_id);
    }
    Ok(map)
}

fn liee_u32(bytes: &[u8], offset: usize) -> Result<usize, MigrationError> {
    bytes
        .get(offset..offset + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
        .ok_or_else(|| MigrationError::Payload("embeddings file truncated".into()))
}

/// Parse a legacy `LIEE` file (layout: see `write_mmap_embeddings`) into its
/// row ids, dimension and raw little-endian f32 matrix bytes. Strict: any
/// truncation or non-UTF-8 id aborts the migration before anything destructive.
fn parse_liee(bytes: &[u8]) -> Result<(Vec<&str>, usize, &[u8]), MigrationError> {
    if bytes.len() < LIEE_HEADER_LEN || bytes[0..4] != LIEE_MAGIC {
        return Err(MigrationError::Payload(
            "embeddings file has no LIEE header".into(),
        ));
    }
    let count = liee_u32(bytes, 8)?;
    let dim = liee_u32(bytes, 12)?;
    let lengths_start = LIEE_HEADER_LEN + count * 8;
    let ids_start = lengths_start + count * 4;
    let mut ids = Vec::with_capacity(count);
    let mut ids_end = ids_start;
    for i in 0..count {
        let off_at = LIEE_HEADER_LEN + i * 8;
        let offset = bytes
            .get(off_at..off_at + 8)
            .map(|b| u64::from_le_bytes(b.try_into().expect("8-byte slice")) as usize)
            .ok_or_else(|| MigrationError::Payload("embeddings offset table truncated".into()))?;
        let len = liee_u32(bytes, lengths_start + i * 4)?;
        let (start, end) = (ids_start + offset, ids_start + offset + len);
        let raw = bytes
            .get(start..end)
            .ok_or_else(|| MigrationError::Payload("embeddings id section truncated".into()))?;
        ids.push(
            std::str::from_utf8(raw)
                .map_err(|_| MigrationError::Payload("embeddings id is not UTF-8".into()))?,
        );
        ids_end = ids_end.max(end);
    }
    let matrix_offset = (ids_end + 3) & !3;
    let matrix_len = count
        .checked_mul(dim)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| MigrationError::Payload("embeddings matrix size overflow".into()))?;
    let matrix = bytes
        .get(matrix_offset..matrix_offset + matrix_len)
        .ok_or_else(|| MigrationError::Payload("embeddings matrix truncated".into()))?;
    Ok((ids, dim, matrix))
}

/// Map each LIEE row to its catalog integer id (last row wins on duplicate
/// ids). Rows whose id is not in the catalog are stale and dropped, loudly.
fn rows_by_node(
    ids: &[&str],
    node_ids: &HashMap<String, u32>,
    layer: &str,
) -> BTreeMap<u32, usize> {
    let mut rows = BTreeMap::new();
    let mut dropped = 0usize;
    for (row, id) in ids.iter().enumerate() {
        match node_ids.get(*id) {
            Some(&node) => {
                rows.insert(node, row);
            }
            None => dropped += 1,
        }
    }
    if dropped > 0 {
        tracing::warn!(
            dropped,
            layer,
            "legacy embedding rows not present in the catalog were dropped"
        );
    }
    rows
}

/// Convert the legacy dense TF-IDF document vectors (`embeddings.bin`, `LIEE`)
/// into a [`LIDX-TFD1`] payload of sparse `(node, term, value)` triples.
/// Zero components are omitted, so the dense vector is exactly recoverable;
/// an all-zero document leaves no entries.
pub(crate) fn encode_tfidf_layer(
    embeddings_path: &Path,
    node_ids: &HashMap<String, u32>,
) -> Result<Vec<u8>, MigrationError> {
    let bytes = fs::read(embeddings_path).map_err(|_| {
        MigrationError::Legacy(format!(
            "missing embeddings file at {}",
            embeddings_path.display()
        ))
    })?;
    let (ids, dim, matrix) = parse_liee(&bytes)?;
    let rows = rows_by_node(&ids, node_ids, "tfidf");
    let mut entries: Vec<(u32, u32, f32)> = Vec::new();
    for (&node, &row) in &rows {
        let vector = &matrix[row * dim * 4..(row + 1) * dim * 4];
        for (term, chunk) in vector.chunks_exact(4).enumerate() {
            let value = f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
            if value != 0.0 {
                entries.push((node, term as u32, value));
            }
        }
    }
    let num_docs = rows.keys().next_back().map_or(0, |last| last + 1);
    Ok(encode_tfidf_payload(num_docs, dim as u32, &entries))
}

/// Convert the legacy neural embedding file (`neural_embeddings.bin`, `LIEE`)
/// into a version-2 [`LIDX-NRL1`] payload: f32 rows plus the PDG node id of
/// each row, sorted by node id. A missing file means no neural model ran, so
/// the layer is the canonical empty payload.
pub(crate) fn encode_neural_layer(
    neural_path: &Path,
    node_ids: &HashMap<String, u32>,
) -> Result<Vec<u8>, MigrationError> {
    if !neural_path.is_file() {
        return Ok(encode_empty_neural());
    }
    let bytes = fs::read(neural_path)?;
    let (ids, dim, matrix) = parse_liee(&bytes)?;
    let rows = rows_by_node(&ids, node_ids, "neural");
    let mut data = Vec::with_capacity(rows.len() * dim * 4);
    for &row in rows.values() {
        data.extend_from_slice(&matrix[row * dim * 4..(row + 1) * dim * 4]);
    }
    let node_order: Vec<u32> = rows.keys().copied().collect();
    Ok(encode_neural_payload(&node_order, dim, &data))
}

/// Assemble a version-2 [`LIDX-NRL1`] F32 payload: `nodes[i]` is the PDG node
/// id of row `i` of `data` (little-endian f32, `dim` per row). The content
/// hash covers the id table followed by the data.
pub(crate) fn encode_neural_payload(nodes: &[u32], dim: usize, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(nodes.len() * 4 + data.len());
    for node in nodes {
        body.extend_from_slice(&node.to_le_bytes());
    }
    body.extend_from_slice(data);
    let mut payload = Vec::with_capacity(NEURAL_HEADER_LEN + body.len());
    payload.extend_from_slice(NEURAL_MAGIC);
    payload.push(NEURAL_VERSION_WITH_IDS);
    payload.extend_from_slice(&[0, 0, 0]); // pad
    payload.extend_from_slice(&(nodes.len() as u32).to_le_bytes());
    payload.extend_from_slice(&(dim as u32).to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes()); // dtype = F32
    payload.extend_from_slice(&1.0f32.to_le_bytes()); // scale (unused for F32)
    payload.extend_from_slice(&0.0f32.to_le_bytes()); // zero_point (unused for F32)
    payload.extend_from_slice(&crate::storage::cas::blob::blob_hash(&body));
    payload.push(0); // 4-byte alignment pad for the id table / f32 array
    payload.extend_from_slice(&body);
    payload
}

/// Assemble a [`LIDX-TFD1`] payload from `(doc, term, value)` triples that
/// are already sorted by `(doc, term)`.
pub(crate) fn encode_tfidf_payload(
    num_docs: u32,
    num_terms: u32,
    entries: &[(u32, u32, f32)],
) -> Vec<u8> {
    let mut body = Vec::with_capacity(entries.len() * TFIDF_ENTRY_LEN);
    for (doc, term, value) in entries {
        body.extend_from_slice(&doc.to_le_bytes());
        body.extend_from_slice(&term.to_le_bytes());
        body.extend_from_slice(&value.to_le_bytes());
    }
    let mut payload = Vec::with_capacity(TFIDF_HEADER_LEN + body.len());
    payload.extend_from_slice(TFIDF_MAGIC);
    payload.push(1); // version
    payload.extend_from_slice(&[0, 0, 0]); // pad
    payload.extend_from_slice(&num_docs.to_le_bytes());
    payload.extend_from_slice(&num_terms.to_le_bytes());
    payload.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    payload.extend_from_slice(&crate::storage::cas::blob::blob_hash(&body));
    payload.extend_from_slice(&body);
    payload
}

/// The canonical empty [`LIDX-TFD1`] payload (no documents).
pub(crate) fn encode_empty_tfidf() -> Vec<u8> {
    encode_tfidf_payload(0, 0, &[])
}

/// The canonical empty [`LIDX-NRL1`] payload (no vectors).
pub(crate) fn encode_empty_neural() -> Vec<u8> {
    encode_neural_payload(&[], 0, &[])
}

/// Reconstruct a [`LIDX-SYM1`] layer from the legacy catalog's `intel_nodes`.
pub(crate) fn encode_symbols_layer(conn: &Connection) -> Result<Vec<u8>, MigrationError> {
    let mut interner = StringInterner::new();

    let mut symbols: Vec<u8> = Vec::new();
    {
        let mut q = conn.prepare(
            "SELECT symbol_name, node_type, file_path, complexity \
             FROM intel_nodes ORDER BY id",
        )?;
        let rows = q.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<i64>>(3)?,
            ))
        })?;
        for row in rows {
            let (symbol_name, node_type, file_path, complexity) = row?;
            let name_id = interner.intern(&symbol_name);
            let sym_type = legacy_node_type_code(&node_type);
            let file_path_id = interner.intern(&file_path);
            let complexity = complexity.unwrap_or(1) as f32;
            symbols.extend_from_slice(&name_id.to_le_bytes());
            symbols.extend_from_slice(&sym_type.to_le_bytes());
            symbols.extend_from_slice(&file_path_id.to_le_bytes());
            symbols.extend_from_slice(&0u32.to_le_bytes()); // start_line
            symbols.extend_from_slice(&0u32.to_le_bytes()); // end_line
            symbols.extend_from_slice(&complexity.to_le_bytes());
        }
    }

    let (string_table, string_bytes) = interner.into_bytes();
    let num_symbols = symbols.len() / SYMBOL_ENTRY_LEN;
    let num_strings = string_table.len() / PDG_STRING_OFFSET_LEN;
    let strings_bytes_len = string_bytes.len();

    let mut data = Vec::with_capacity(symbols.len() + string_table.len() + string_bytes.len());
    data.extend_from_slice(&symbols);
    data.extend_from_slice(&string_table);
    data.extend_from_slice(&string_bytes);
    let content_hash = crate::storage::cas::blob::blob_hash(&data);

    let mut payload = Vec::with_capacity(SYMBOLS_HEADER_LEN + data.len());
    payload.extend_from_slice(SYMBOLS_MAGIC);
    payload.push(1);
    payload.extend_from_slice(&[0, 0, 0]);
    payload.extend_from_slice(&(num_symbols as u32).to_le_bytes());
    payload.extend_from_slice(&(num_strings as u32).to_le_bytes());
    payload.extend_from_slice(&(strings_bytes_len as u32).to_le_bytes());
    payload.extend_from_slice(&content_hash);
    payload.extend_from_slice(&data);
    Ok(payload)
}

// ---------------------------------------------------------------------------
// Destructive cleanup (runs only once the store is migrated)
// ---------------------------------------------------------------------------

/// Legacy artifact filenames kept inside a retained generation directory that
/// are redundant once the generation's layers live in CAS.
const LEGACY_GEN_ARTIFACTS: &[&str] = &[
    "leindex.db",
    "leindex.db-wal",
    "leindex.db-shm",
    "embeddings.bin",
    "neural_embeddings.bin",
    "search_snapshot.bin",
    "tfidf_embedder.bin",
    "index-state.json",
    "index_stats.json",
];

/// Top-level legacy full-copy artifacts that are fully captured in CAS.
const LEGACY_TOP_LEVEL_ARTIFACTS: &[&str] = &[
    "leindex.db",
    "leindex.db-wal",
    "leindex.db-shm",
    "embeddings.bin",
    "neural_embeddings.bin",
    "search_snapshot.bin",
    "tfidf_embedder.bin",
];

/// Delete every non-retained generation directory under `generations/`
/// (numeric subdirectories outside `retained`); best-effort per directory.
fn delete_non_retained_generations(
    storage_root: &Path,
    retained: &[u64],
    report: &mut MigrationReport,
) -> Result<(), MigrationError> {
    let gens_dir = storage_root.join(GENERATIONS_DIR);
    if !gens_dir.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(&gens_dir)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let name_str = entry.file_name().to_string_lossy().into_owned();
        let Ok(num) = name_str.parse::<u64>() else {
            continue;
        };
        if retained.contains(&num) {
            continue;
        }
        if fs::remove_dir_all(entry.path()).is_ok() {
            report.generations_deleted += 1;
        }
    }
    Ok(())
}

/// Delete one redundant legacy artifact, crediting its size to the report.
fn remove_legacy_artifact(path: &Path, report: &mut MigrationReport) {
    if !path.is_file() {
        return;
    }
    let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if fs::remove_file(path).is_ok() {
        report.artifact_bytes_reclaimed += size;
    }
}

/// Delete the redundant legacy full-copy files inside the retained
/// generations, plus stale manifest partials.
fn delete_retained_generation_artifacts(
    storage_root: &Path,
    retained: &[u64],
    report: &mut MigrationReport,
) {
    for g in retained {
        let gen_dir = generation_dir(storage_root, *g);
        for artifact in LEGACY_GEN_ARTIFACTS {
            remove_legacy_artifact(&gen_dir.join(artifact), report);
        }
        let _ = fs::remove_file(gen_dir.join("manifest.partial"));
    }
}

/// Delete the top-level legacy full-copy artifacts (fully captured in CAS).
fn delete_top_level_artifacts(storage_root: &Path, report: &mut MigrationReport) {
    for artifact in LEGACY_TOP_LEVEL_ARTIFACTS {
        remove_legacy_artifact(&storage_root.join(artifact), report);
    }
}

/// CAS GC: remove blobs not referenced by any retained manifest (they are
/// unreachable once the retained generations are live). Pins are the current
/// + previous manifest layer hashes.
fn garbage_collect_unpinned_blobs(
    storage_root: &Path,
    retained: &[u64],
    report: &mut MigrationReport,
) -> Result<(), MigrationError> {
    let cas_root = storage_root.join("cas");
    if !cas_root.is_dir() {
        return Ok(());
    }
    let mut store = CasStore::open(&cas_root)?;
    let pinned = retained_manifest_pins(storage_root, retained);
    let gc = match store.gc_with_pins(&pinned) {
        Ok(gc) => gc,
        Err(error) => match error.partial_sweep() {
            // A mid-sweep failure must not abort the migration: the reclaim
            // already performed is real, and the remaining candidates are
            // retried by the next retention sweep.
            Some((reclaimed_bytes, blobs_removed)) => {
                tracing::warn!(
                    reclaimed_bytes,
                    blobs_removed,
                    "migration CAS GC failed part-way; crediting the partial reclaim and continuing"
                );
                RetentionReport {
                    reclaimed_bytes,
                    blobs_removed,
                    partial: true,
                }
            }
            None => return Err(error.into()),
        },
    };
    report.cas_reclaimed_bytes += gc.reclaimed_bytes;
    report.cas_blob_count = store.blob_count()?;
    report.cas_bytes = compute_cas_bytes(&store);
    store.persist()?;
    Ok(())
}

/// Perform the post-commit destructive sweep. Safe to run after the store is
/// migrated; idempotent.
fn cleanup_migrated_store(
    storage_root: &Path,
    cfg: &MigrationConfig,
    report: &mut MigrationReport,
) -> Result<(), MigrationError> {
    let Some(current) = read_current_generation(storage_root) else {
        return Ok(());
    };
    let previous = current.checked_sub(1);
    let retained = [previous, Some(current)]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();

    // 1. Delete non-retained generation directories.
    delete_non_retained_generations(storage_root, &retained, report)?;

    // 2. Delete redundant legacy full-copy files inside retained generations.
    delete_retained_generation_artifacts(storage_root, &retained, report);

    // 3. Delete redundant top-level legacy full-copy artifacts.
    delete_top_level_artifacts(storage_root, report);

    // 4. Prune jobs: completed jobs immediately, then byte-cap oldest-first.
    let jobs_dir = storage_root.join("jobs");
    prune_jobs(&jobs_dir, cfg, report);

    // 5. CAS GC: remove blobs not referenced by any retained manifest (they
    //    are unreachable once the retained generations are live). Pins are the
    //    current + previous manifest layer hashes.
    garbage_collect_unpinned_blobs(storage_root, &retained, report)?;

    Ok(())
}

/// Collect the CAS layer hashes referenced by the retained generations'
/// manifests (the pin set for GC).
fn retained_manifest_pins(
    storage_root: &Path,
    retained: &[u64],
) -> std::collections::HashSet<[u8; 32]> {
    let mut pins = std::collections::HashSet::new();
    for g in retained {
        if let Ok(manifest) = read_generation_manifest(storage_root, *g) {
            for hash in manifest.layer_hashes() {
                pins.insert(hash);
            }
        }
    }
    pins
}

/// Delete completed jobs immediately (zero resume value). In-progress jobs
/// are never deleted — they hold checkpoint resume value and may belong to a
/// concurrently running index — so when they alone exceed the effective job
/// cap (job cap, or the footprint-goal headroom), a warning is recorded
/// instead of destroying their checkpoints.
fn prune_jobs(jobs_dir: &Path, cfg: &MigrationConfig, report: &mut MigrationReport) {
    if !jobs_dir.is_dir() {
        return;
    }

    // Enumerate numeric job directories with their apparent sizes.
    let mut jobs: Vec<(u64, PathBuf, u64)> = Vec::new();
    if let Ok(entries) = fs::read_dir(jobs_dir) {
        for entry in entries.flatten() {
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let name_str = entry.file_name().to_string_lossy().into_owned();
            if let Ok(g) = name_str.parse::<u64>() {
                let size = dir_total_bytes(&entry.path());
                jobs.push((g, entry.path(), size));
            }
        }
    }
    if jobs.is_empty() {
        return;
    }

    // Phase 1: completed jobs (zero resume value) deleted unconditionally.
    let mut remaining: Vec<(u64, PathBuf, u64)> = Vec::new();
    for (g, path, size) in jobs {
        if job_is_completed(&path) {
            if fs::remove_dir_all(&path).is_ok() {
                report.jobs_completed_deleted += 1;
                report.job_bytes_reclaimed += size;
            }
        } else {
            remaining.push((g, path, size));
        }
    }

    // Phase 2: byte-cap the remaining (in-progress) jobs oldest-first.
    //
    // In-progress jobs are excluded, matching the retention sweep: they hold
    // checkpoint resume value, and this cleanup also runs from `LeIndex::new`
    // on already-migrated stores, where it could otherwise destroy the
    // checkpoints of a job that is indexing concurrently in another process.
    // The footprint goal is simply reported as unmet while they remain.
    let job_cap = effective_job_cap(cfg, report);
    if dir_total_bytes(jobs_dir) > job_cap {
        let in_progress_bytes: u64 = remaining.iter().map(|(_, _, size)| *size).sum();
        if in_progress_bytes > job_cap {
            let msg = format!(
                "jobs directory exceeds the {job_cap}-byte cap but {} bytes of \
                 in-progress jobs remain (checkpoint resume value); skipping \
                 their deletion",
                in_progress_bytes
            );
            tracing::warn!("{}", msg);
            report.warnings.push(msg);
        }
    }
    report.job_bytes_remaining = dir_total_bytes(jobs_dir);
}

/// The maximum bytes the jobs directory may occupy, computed as
/// `min(job_bytes_max, footprint_goal − (cas + reserved))`, floored at 0.
fn effective_job_cap(cfg: &MigrationConfig, report: &MigrationReport) -> u64 {
    let mut cap = cfg.job_bytes_max;
    if let Some(goal) = cfg.total_footprint_goal_bytes {
        let non_job = report.cas_bytes + RESERVED_MISC_BYTES;
        let budget = goal.saturating_sub(non_job);
        cap = cap.min(budget);
    }
    cap
}

/// Classify a legacy job as completed: its `state.json` reports
/// `last_reusable_phase == "complete"`, or all four `.complete` markers are
/// present, or a generation with its number was published.
fn job_is_completed(job_dir: &Path) -> bool {
    // All four phase markers present → finished through the neural phase.
    let all_markers = JOB_MARKERS.iter().all(|m| job_dir.join(m).is_file());
    if all_markers {
        return true;
    }
    // state.json last_reusable_phase == "complete" (the final phase).
    if let Ok(content) = fs::read_to_string(job_dir.join("state.json")) {
        if content.contains("\"complete\"") && content.contains("last_reusable_phase") {
            return true;
        }
    }
    // A published generation dir with this job's number signals the job's
    // generation was published (full-copy legacy or manifest layout).
    if let Ok(num) = job_dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
        .parse::<u64>()
    {
        if let Some(parent) = job_dir.parent() {
            if let Some(root) = parent.parent() {
                let gd = root.join(GENERATIONS_DIR).join(num.to_string());
                if gd.join(MANIFEST_FILE).is_file() || gd.join("leindex.db").is_file() {
                    return true;
                }
            }
        }
    }
    false
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Path to a generation directory.
fn generation_dir(storage_root: &Path, g: u64) -> PathBuf {
    storage_root.join(GENERATIONS_DIR).join(g.to_string())
}

/// Whether a legacy full-copy generation directory exists.
fn legacy_generation_dir_exists(storage_root: &Path, g: u64) -> bool {
    generation_dir(storage_root, g).join("leindex.db").is_file()
}

/// Read the current generation manifest (see `lease::read_generation_manifest`).
fn read_generation_manifest(
    storage_root: &Path,
    g: u64,
) -> Result<Manifest, super::manifest::ManifestError> {
    let manifest_path = storage_root
        .join(GENERATIONS_DIR)
        .join(g.to_string())
        .join(MANIFEST_FILE);
    let bytes =
        fs::read(&manifest_path).map_err(|_| super::manifest::ManifestError::MissingManifest {
            generation: g,
            path: manifest_path.to_string_lossy().into_owned(),
        })?;
    Manifest::from_bytes(&bytes)
}

/// Total apparent bytes of a directory subtree.
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

/// Total apparent bytes used by the CAS store (blobs under prefix dirs,
/// excluding refs sidecar and staging).
fn compute_cas_bytes(store: &CasStore) -> u64 {
    let root = store.root();
    let mut total: u64 = 0;
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name == ".staging"
                || crate::storage::cas::refs::REFS_AUX_FILES.contains(&name.as_ref())
            {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                total += dir_total_bytes(&entry.path());
            } else if entry.file_type().is_ok_and(|t| t.is_file()) {
                if let Ok(meta) = entry.metadata() {
                    total += meta.len();
                }
            }
        }
    }
    total
}

/// Deterministic legacy node/symbol `node_type` string → u32 code. The
/// `LIDX-PDG1`/`LIDX-SYM1` readers treat this as an opaque enum owned by the
/// writer; the migration defines the mapping for the legacy types so that all
/// migrated generations agree.
fn legacy_node_type_code(node_type: &str) -> u32 {
    match node_type.to_ascii_lowercase().as_str() {
        "module" => 1,
        "function" | "fn" | "method" => 2,
        "class" | "struct" | "interface" | "type" | "enum" => 3,
        "import" | "external" => 4,
        "variable" | "const" | "field" | "property" => 5,
        "macro" => 6,
        _ => 0,
    }
}

// ---------------------------------------------------------------------------
// Version-2 (lossless) PDG layer
// ---------------------------------------------------------------------------

/// Lossless node-type codes: the storage `NodeType` vocabulary in
/// declaration order (`src/storage/nodes.rs`). Codes are 1-based so 0 can
/// never be a valid type (a 0 in a v1 payload means "unknown legacy type").
/// Unknown types are a hard error — a lossless layer must not erase them.
pub(crate) fn node_type_code_v2(node_type: &str) -> Result<u32, MigrationError> {
    Ok(match node_type {
        "function" => 1,
        "class" => 2,
        "method" => 3,
        "variable" => 4,
        "module" => 5,
        "external" => 6,
        "doc_section" => 7,
        "file_summary" => 8,
        other => {
            return Err(MigrationError::Payload(format!(
                "unknown node type {other:?}; refusing to encode a lossless layer without it"
            )));
        }
    })
}

/// Lossless edge-type codes: the storage `EdgeType` vocabulary in declaration
/// order (`src/storage/edges.rs`), 1-based. See [`node_type_code_v2`].
pub(super) fn edge_type_code_v2(edge_type: &str) -> Result<u32, MigrationError> {
    Ok(match edge_type {
        "call" => 1,
        "data_dependency" => 2,
        "inheritance" => 3,
        "import" => 4,
        "containment" => 5,
        "type_of" => 6,
        "state_transition" => 7,
        "command_argument" => 8,
        "environment" => 9,
        "stdin" => 10,
        other => {
            return Err(MigrationError::Payload(format!(
                "unknown edge type {other:?}; refusing to encode a lossless layer without it"
            )));
        }
    })
}

/// Decode half of the lossless node-type vocabulary shared with
/// [`graph_codec`](super::graph_codec). Codes are 1-based; an unknown code is
/// a hard error so a lossless layer can never silently erase a node kind.
pub(super) fn node_type_name_v2(code: u32) -> Result<&'static str, MigrationError> {
    Ok(match code {
        1 => "function",
        2 => "class",
        3 => "method",
        4 => "variable",
        5 => "module",
        6 => "external",
        7 => "doc_section",
        8 => "file_summary",
        other => {
            return Err(MigrationError::Payload(format!(
                "unknown node type code {other}; refusing to decode a lossless layer"
            )));
        }
    })
}

/// Decode half of the lossless edge-type vocabulary shared with
/// [`graph_codec`](super::graph_codec). Codes are 1-based; an unknown code is
/// a hard error.
pub(super) fn edge_type_name_v2(code: u32) -> Result<&'static str, MigrationError> {
    Ok(match code {
        1 => "call",
        2 => "data_dependency",
        3 => "inheritance",
        4 => "import",
        5 => "containment",
        6 => "type_of",
        7 => "state_transition",
        8 => "command_argument",
        9 => "environment",
        10 => "stdin",
        other => {
            return Err(MigrationError::Payload(format!(
                "unknown edge type code {other}; refusing to decode a lossless layer"
            )));
        }
    })
}

/// Reconstruct a version-2 (lossless) [`LIDX-PDG1`] payload from the legacy
/// catalog. Unlike the v1 encoder, every `ProgramDependenceGraph` field is
/// preserved: graph node id, symbol name, file path, language, type,
/// complexity, byte range, precision markers, and full edge metadata. Edge
/// endpoints reference nodes by their interned graph node id (not the
/// storage row id, which is a catalog artifact).
pub(crate) fn encode_pdg_layer_v2(conn: &Connection) -> Result<Vec<u8>, MigrationError> {
    use crate::storage::edges::EdgeMetadata as StorageEdgeMetadata;

    let mut interner = StringInterner::new();
    let mut nodes: Vec<u8> = Vec::new();
    {
        let mut q = conn.prepare(
            "SELECT node_id, symbol_name, file_path, language, node_type, \
             complexity, byte_range_start, byte_range_end, precision \
             FROM intel_nodes ORDER BY id",
        )?;
        let rows = q.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<i64>>(5)?,
                row.get::<_, Option<i64>>(6)?,
                row.get::<_, Option<i64>>(7)?,
                row.get::<_, Option<i64>>(8)?,
            ))
        })?;
        for row in rows {
            let (
                node_id,
                symbol_name,
                file_path,
                language,
                node_type,
                complexity,
                start,
                end,
                precision,
            ) = row?;
            let type_code = node_type_code_v2(&node_type)?;
            let complexity = u32::try_from(complexity.unwrap_or(0))
                .map_err(|_| MigrationError::Payload("node complexity exceeds u32".into()))?;
            let byte = |value: Option<i64>| {
                u32::try_from(value.unwrap_or(0))
                    .map_err(|_| MigrationError::Payload("node byte range exceeds u32".into()))
            };
            let flags: u32 = u32::from(precision.unwrap_or(0) != 0);
            nodes.extend_from_slice(&interner.intern(&node_id).to_le_bytes());
            nodes.extend_from_slice(&interner.intern(&symbol_name).to_le_bytes());
            nodes.extend_from_slice(&interner.intern(&file_path).to_le_bytes());
            nodes.extend_from_slice(&interner.intern(&language).to_le_bytes());
            nodes.extend_from_slice(&type_code.to_le_bytes());
            nodes.extend_from_slice(&complexity.to_le_bytes());
            nodes.extend_from_slice(&byte(start)?.to_le_bytes());
            nodes.extend_from_slice(&byte(end)?.to_le_bytes());
            nodes.extend_from_slice(&flags.to_le_bytes());
        }
    }

    // Resolve each edge's endpoints to the interned graph node ids, then
    // append the edge records and their metadata records. `intern` is
    // idempotent, so the second pass over node rows just recovers the ids.
    let mut row_to_interned: HashMap<i64, u32> = HashMap::new();
    {
        let mut q = conn.prepare("SELECT id, node_id FROM intel_nodes")?;
        let rows = q.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;
        for row in rows {
            let (row_id, node_id) = row?;
            row_to_interned.insert(row_id, interner.intern(&node_id));
        }
    }

    let opt_u32 = |value: Option<u32>| value.unwrap_or(PDG_V2_NONE).to_le_bytes();
    let mut edges: Vec<u8> = Vec::new();
    let mut edge_meta: Vec<u8> = Vec::new();
    {
        let mut q =
            conn.prepare("SELECT caller_id, callee_id, edge_type, metadata FROM intel_edges")?;
        let rows = q.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;
        for row in rows {
            let (caller, callee, edge_type, metadata_json) = row?;
            let type_code = edge_type_code_v2(&edge_type)?;
            let src = *row_to_interned.get(&caller).ok_or_else(|| {
                MigrationError::Payload(format!("edge caller {caller} has no node"))
            })?;
            let dst = *row_to_interned.get(&callee).ok_or_else(|| {
                MigrationError::Payload(format!("edge callee {callee} has no node"))
            })?;
            edges.extend_from_slice(&src.to_le_bytes());
            edges.extend_from_slice(&dst.to_le_bytes());
            edges.extend_from_slice(&type_code.to_le_bytes());

            let metadata: StorageEdgeMetadata = match metadata_json {
                Some(json) => serde_json::from_str(&json)
                    .map_err(|e| MigrationError::Payload(format!("invalid edge metadata: {e}")))?,
                None => StorageEdgeMetadata {
                    call_count: None,
                    variable_name: None,
                    confidence: None,
                    channel: None,
                    position: None,
                },
            };
            let narrow =
                |value: Option<usize>, field: &str| -> Result<Option<u32>, MigrationError> {
                    value
                        .map(u32::try_from)
                        .transpose()
                        .map_err(|_| MigrationError::Payload(format!("edge {field} exceeds u32")))
                };
            edge_meta.extend_from_slice(&opt_u32(narrow(metadata.call_count, "call_count")?));
            let variable = metadata
                .variable_name
                .as_deref()
                .map(|s| interner.intern(s));
            edge_meta.extend_from_slice(&opt_u32(variable));
            let confidence = metadata
                .confidence
                .map(f32::to_bits)
                .unwrap_or(f32::NAN.to_bits());
            edge_meta.extend_from_slice(&confidence.to_le_bytes());
            let channel = metadata.channel.as_deref().map(|s| interner.intern(s));
            edge_meta.extend_from_slice(&opt_u32(channel));
            edge_meta.extend_from_slice(&opt_u32(narrow(metadata.position, "position")?));
        }
    }

    let (string_table, string_bytes) = interner.into_bytes();
    let num_nodes = nodes.len() / PDG_NODE_V2_LEN;
    let num_edges = edges.len() / PDG_EDGE_LEN;
    let num_strings = string_table.len() / PDG_STRING_OFFSET_LEN;
    let strings_bytes_len = string_bytes.len();

    let mut data = Vec::with_capacity(
        nodes.len() + edges.len() + edge_meta.len() + string_table.len() + string_bytes.len(),
    );
    data.extend_from_slice(&nodes);
    data.extend_from_slice(&edges);
    data.extend_from_slice(&edge_meta);
    data.extend_from_slice(&string_table);
    data.extend_from_slice(&string_bytes);
    let content_hash = crate::storage::cas::blob::blob_hash(&data);

    let mut payload = Vec::with_capacity(PDG_HEADER_LEN + data.len());
    payload.extend_from_slice(PDG_MAGIC);
    payload.push(2); // version 2: lossless graph
    payload.extend_from_slice(&[0, 0, 0]);
    payload.extend_from_slice(&(num_nodes as u32).to_le_bytes());
    payload.extend_from_slice(&(num_edges as u32).to_le_bytes());
    payload.extend_from_slice(&(num_strings as u32).to_le_bytes());
    payload.extend_from_slice(&(strings_bytes_len as u32).to_le_bytes());
    payload.extend_from_slice(&content_hash);
    payload.extend_from_slice(&data);
    Ok(payload)
}

/// Interned string table for the PDG/SYMBOLS payloads. Strings are stored once
/// and referenced by id; the reader locates them via an (offset, length) table
/// whose offsets are relative to the start of the string bytes region.
pub(super) struct StringInterner {
    map: HashMap<String, u32>,
    offsets: Vec<(u32, u32)>,
    bytes: Vec<u8>,
}

impl StringInterner {
    pub(super) fn new() -> Self {
        StringInterner {
            map: HashMap::new(),
            offsets: Vec::new(),
            bytes: Vec::new(),
        }
    }

    /// Intern `s`, returning its stable id (idempotent).
    pub(super) fn intern(&mut self, s: &str) -> u32 {
        if let Some(id) = self.map.get(s) {
            return *id;
        }
        let id = self.offsets.len() as u32;
        let offset = self.bytes.len() as u32;
        self.offsets.push((offset, s.len() as u32));
        self.bytes.extend_from_slice(s.as_bytes());
        self.map.insert(s.to_string(), id);
        id
    }

    /// Produce `(offset_table_bytes, string_bytes)` in reader layout.
    pub(super) fn into_bytes(self) -> (Vec<u8>, Vec<u8>) {
        let mut table = Vec::with_capacity(self.offsets.len() * PDG_STRING_OFFSET_LEN);
        for (offset, len) in &self.offsets {
            table.extend_from_slice(&offset.to_le_bytes());
            table.extend_from_slice(&len.to_le_bytes());
        }
        (table, self.bytes)
    }
}

#[cfg(test)]
#[path = "migrate_test.rs"]
mod tests;
