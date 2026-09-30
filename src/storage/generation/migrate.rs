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
//! - **Tfidf** — a structurally valid, *empty* `LIDX-TFD1` payload. The
//!   legacy `tfidf_embedder.bin` stores only vocabulary + IDF (needed to
//!   compute query embeddings), not the sparse document×term matrix the
//!   TF-IDF layer encodes. The dense document vectors are preserved verbatim
//!   in the Neural layer; sparse-doc reconstruction is deferred to the
//!   read-path wiring task.
//! - **Neural** — a real conversion of the legacy mmap embedding file
//!   (`embeddings.bin`, `LIEE` frame) into a `LIDX-NRL1` payload that
//!   preserves the full `count × dim` f32 matrix.
//! - **Pdg** — reconstructed from the legacy catalog's `intel_nodes` /
//!   `intel_edges` tables into `LIDX-PDG1`.
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

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use rusqlite::Connection;

use crate::storage::cas::CasStore;
use crate::storage::cas::blob::hash_to_hex;

use super::lease::{GENERATIONS_DIR, MANIFEST_FILE, read_current_generation};
use super::manifest::{LayerKind, Manifest};
use super::reader::{
    NEURAL_HEADER_LEN, NEURAL_MAGIC, PDG_EDGE_LEN, PDG_HEADER_LEN, PDG_MAGIC, PDG_NODE_LEN,
    PDG_STRING_OFFSET_LEN, SYMBOL_ENTRY_LEN, SYMBOLS_HEADER_LEN, SYMBOLS_MAGIC, TFIDF_HEADER_LEN,
    TFIDF_MAGIC,
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
    let mut report = MigrationReport {
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
    };

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

    if cfg.emit_backup_warning {
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

    let cas_root = storage_root.join("cas");
    let cas = Arc::new(Mutex::new(CasStore::open(&cas_root)?));

    let mut writer = GenerationWriter::new(storage_root, cas.clone());
    for g in &retained {
        convert_generation(&mut writer, storage_root, *g)?;
        if *g == current {
            // Swap CURRENT last: the store is now fully migrated.
            writer.publish(current)?;
            report.migrated = true;
        } else {
            writer.publish_manifest_only(*g)?;
        }
        report.generations_converted += 1;
    }
    report.current_generation = Some(current);
    report.previous_generation = previous;

    // Record layer hashes for the current manifest (dedup audit).
    if let Ok(manifest) = read_generation_manifest(storage_root, current) {
        for hash in manifest.layer_hashes() {
            report.blob_hashes.push(hash_to_hex(&hash));
        }
    }

    // Store stats now that CAS is populated.
    report.cas_blob_count = cas.lock().expect("cas lock").blob_count()?;
    report.cas_bytes = compute_cas_bytes(&cas.lock().expect("cas lock"));

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

    // Neural layer: real conversion of the LIEE mmap embedding matrix.
    let embeddings_path = gen_dir.join("embeddings.bin");
    let neural_bytes = encode_neural_layer(&embeddings_path)?;
    writer.stage(LayerKind::Neural, &neural_bytes)?;

    // Tfidf layer: structurally valid empty payload (see module docs).
    writer.stage(LayerKind::Tfidf, &encode_empty_tfidf())?;

    // Pdg + Symbols: reconstructed from the legacy catalog.
    let conn = Connection::open(&db_handle.path)?;
    let pdg_bytes = encode_pdg_layer(&conn)?;
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
fn copy_and_checkpoint(src: &Path) -> Result<NormalizedDb, MigrationError> {
    let tmp = tempfile::tempdir()?;
    let copy = tmp.path().join("catalog.db");
    fs::copy(src, &copy)?;
    for sidecar in ["-wal", "-shm"] {
        let s = src.with_extension(format!("db{}", sidecar));
        if s.is_file() {
            let _ = fs::copy(&s, tmp.path().join(format!("catalog.db{}", sidecar)));
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
    let bytes = fs::read(&out)?;
    drop(conn);
    Ok(bytes)
}

/// Convert a legacy `LIEE` mmap embedding file into a [`LIDX-NRL1`] payload
/// preserving the full f32 matrix.
pub(crate) fn encode_neural_layer(embeddings_path: &Path) -> Result<Vec<u8>, MigrationError> {
    let bytes = fs::read(embeddings_path).map_err(|_| {
        MigrationError::Legacy(format!(
            "missing embeddings file at {}",
            embeddings_path.display()
        ))
    })?;
    if bytes.len() < LIEE_HEADER_LEN {
        return Err(MigrationError::Payload(
            "embeddings file shorter than LIEE header".into(),
        ));
    }
    if bytes[0..4] != LIEE_MAGIC {
        return Err(MigrationError::Payload(
            "embeddings file has no LIEE magic".into(),
        ));
    }
    let node_count = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]) as usize;
    let dimension = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]) as usize;

    // Recompute the matrix offset exactly as `write_mmap_embeddings` does.
    let offsets_start = LIEE_HEADER_LEN;
    let lengths_start = offsets_start + node_count * 8;
    let ids_start = lengths_start + node_count * 4;
    let mut ids_len = 0usize;
    for i in 0..node_count {
        let base = lengths_start + i * 4;
        if base + 4 > bytes.len() {
            return Err(MigrationError::Payload(
                "embeddings length table truncated".into(),
            ));
        }
        ids_len += u32::from_le_bytes([
            bytes[base],
            bytes[base + 1],
            bytes[base + 2],
            bytes[base + 3],
        ]) as usize;
    }
    let ids_end = ids_start + ids_len;
    let matrix_offset = (ids_end + 3) & !3;
    let matrix_len = node_count * dimension * 4;
    let data = bytes
        .get(matrix_offset..matrix_offset + matrix_len)
        .ok_or_else(|| MigrationError::Payload("embeddings matrix truncated".into()))?;

    let content_hash = crate::storage::cas::blob::blob_hash(data);
    let mut payload = Vec::with_capacity(NEURAL_HEADER_LEN + data.len());
    payload.extend_from_slice(NEURAL_MAGIC);
    payload.push(1); // version
    payload.extend_from_slice(&[0, 0, 0]); // pad
    payload.extend_from_slice(&(node_count as u32).to_le_bytes());
    payload.extend_from_slice(&(dimension as u32).to_le_bytes());
    payload.extend_from_slice(&0u32.to_le_bytes()); // dtype = F32
    payload.extend_from_slice(&1.0f32.to_le_bytes()); // scale (unused for F32)
    payload.extend_from_slice(&0.0f32.to_le_bytes()); // zero_point (unused for F32)
    payload.extend_from_slice(&content_hash);
    payload.push(0); // 4-byte alignment pad for the f32 array
    payload.extend_from_slice(data);
    Ok(payload)
}

/// Build a structurally valid, empty [`LIDX-TFD1`] payload. The legacy store
/// does not persist a sparse document×term matrix (only vocabulary + IDF),
/// so the migration stages an empty sparse layer and preserves the dense
/// vectors in the Neural layer. See module docs.
pub(crate) fn encode_empty_tfidf() -> Vec<u8> {
    let mut payload = Vec::with_capacity(TFIDF_HEADER_LEN);
    payload.extend_from_slice(TFIDF_MAGIC);
    payload.push(1); // version
    payload.extend_from_slice(&[0, 0, 0]); // pad
    payload.extend_from_slice(&0u32.to_le_bytes()); // num_docs
    payload.extend_from_slice(&0u32.to_le_bytes()); // num_terms
    payload.extend_from_slice(&0u32.to_le_bytes()); // num_entries
    let content_hash = crate::storage::cas::blob::blob_hash(&[]);
    payload.extend_from_slice(&content_hash);
    payload
}

/// Build a structurally valid, empty [`LIDX-NRL1`] payload (0 vectors). The
/// legacy store's neural layer is preserved only when dense vectors exist;
/// fixtures and the empty store use this canonical empty form.
#[cfg(test)]
pub(crate) fn encode_empty_neural() -> Vec<u8> {
    let mut payload = Vec::with_capacity(NEURAL_HEADER_LEN);
    payload.extend_from_slice(NEURAL_MAGIC);
    payload.push(1); // version
    payload.extend_from_slice(&[0, 0, 0]); // pad
    payload.extend_from_slice(&0u32.to_le_bytes()); // node_count
    payload.extend_from_slice(&0u32.to_le_bytes()); // dimension
    payload.extend_from_slice(&0u32.to_le_bytes()); // dtype = F32
    payload.extend_from_slice(&1.0f32.to_le_bytes()); // scale
    payload.extend_from_slice(&0.0f32.to_le_bytes()); // zero_point
    let content_hash = crate::storage::cas::blob::blob_hash(&[]);
    payload.extend_from_slice(&content_hash);
    payload.push(0); // alignment pad
    payload
}

/// Reconstruct a [`LIDX-PDG1`] layer from the legacy catalog's `intel_nodes`
/// and `intel_edges` tables.
pub(crate) fn encode_pdg_layer(conn: &Connection) -> Result<Vec<u8>, MigrationError> {
    let mut interner = StringInterner::new();

    let mut nodes: Vec<u8> = Vec::new();
    {
        let mut q = conn.prepare(
            "SELECT id, symbol_name, node_type, file_path \
             FROM intel_nodes ORDER BY id",
        )?;
        let rows = q.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        for row in rows {
            let (db_id, symbol_name, node_type, file_path) = row?;
            let node_id = u32::try_from(db_id)
                .map_err(|_| MigrationError::Payload("node id exceeds u32".into()))?;
            let node_type = legacy_node_type_code(&node_type);
            let file_path_id = interner.intern(&file_path);
            let sym_name_id = interner.intern(&symbol_name);
            nodes.extend_from_slice(&node_id.to_le_bytes());
            nodes.extend_from_slice(&node_type.to_le_bytes());
            nodes.extend_from_slice(&file_path_id.to_le_bytes());
            nodes.extend_from_slice(&0u32.to_le_bytes()); // start_line (legacy stores bytes, not lines)
            nodes.extend_from_slice(&0u32.to_le_bytes()); // end_line
            nodes.extend_from_slice(&sym_name_id.to_le_bytes());
        }
    }

    let mut edges: Vec<u8> = Vec::new();
    {
        let mut q = conn.prepare("SELECT caller_id, callee_id, edge_type FROM intel_edges")?;
        let rows = q.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        for row in rows {
            let (caller, callee, edge_type) = row?;
            let src = u32::try_from(caller)
                .map_err(|_| MigrationError::Payload("caller id exceeds u32".into()))?;
            let dst = u32::try_from(callee)
                .map_err(|_| MigrationError::Payload("callee id exceeds u32".into()))?;
            edges.extend_from_slice(&src.to_le_bytes());
            edges.extend_from_slice(&dst.to_le_bytes());
            edges.extend_from_slice(&legacy_edge_type_code(&edge_type).to_le_bytes());
        }
    }

    let (string_table, string_bytes) = interner.into_bytes();
    let num_nodes = nodes.len() / PDG_NODE_LEN;
    let num_edges = edges.len() / PDG_EDGE_LEN;
    let num_strings = string_table.len() / PDG_STRING_OFFSET_LEN;
    let strings_bytes_len = string_bytes.len();

    let mut data =
        Vec::with_capacity(nodes.len() + edges.len() + string_table.len() + string_bytes.len());
    data.extend_from_slice(&nodes);
    data.extend_from_slice(&edges);
    data.extend_from_slice(&string_table);
    data.extend_from_slice(&string_bytes);
    let content_hash = crate::storage::cas::blob::blob_hash(&data);

    let mut payload = Vec::with_capacity(PDG_HEADER_LEN + data.len());
    payload.extend_from_slice(PDG_MAGIC);
    payload.push(1);
    payload.extend_from_slice(&[0, 0, 0]);
    payload.extend_from_slice(&(num_nodes as u32).to_le_bytes());
    payload.extend_from_slice(&(num_edges as u32).to_le_bytes());
    payload.extend_from_slice(&(num_strings as u32).to_le_bytes());
    payload.extend_from_slice(&(strings_bytes_len as u32).to_le_bytes());
    payload.extend_from_slice(&content_hash);
    payload.extend_from_slice(&data);
    Ok(payload)
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
    let gens_dir = storage_root.join(GENERATIONS_DIR);
    if gens_dir.is_dir() {
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
    }

    // 2. Delete redundant legacy full-copy files inside retained generations.
    for g in &retained {
        let gen_dir = generation_dir(storage_root, *g);
        for artifact in LEGACY_GEN_ARTIFACTS {
            let p = gen_dir.join(artifact);
            if p.is_file() {
                let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
                if fs::remove_file(&p).is_ok() {
                    report.artifact_bytes_reclaimed += size;
                }
            }
        }
        let _ = fs::remove_file(gen_dir.join("manifest.partial"));
    }

    // 3. Delete redundant top-level legacy full-copy artifacts.
    for artifact in LEGACY_TOP_LEVEL_ARTIFACTS {
        let p = storage_root.join(artifact);
        if p.is_file() {
            let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if fs::remove_file(&p).is_ok() {
                report.artifact_bytes_reclaimed += size;
            }
        }
    }

    // 4. Prune jobs: completed jobs immediately, then byte-cap oldest-first.
    let jobs_dir = storage_root.join("jobs");
    prune_jobs(&jobs_dir, cfg, report);

    // 5. CAS GC: remove blobs not referenced by any retained manifest (they
    //    are unreachable once the retained generations are live). Pins are the
    //    current + previous manifest layer hashes.
    let cas_root = storage_root.join("cas");
    if cas_root.is_dir() {
        let mut store = CasStore::open(&cas_root)?;
        let pinned = retained_manifest_pins(storage_root, &retained);
        let gc = store.gc_with_pins(&pinned)?;
        report.cas_reclaimed_bytes += gc.reclaimed_bytes;
        report.cas_blob_count = store.blob_count()?;
        report.cas_bytes = compute_cas_bytes(&store);
        store.persist()?;
    }

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

/// Delete completed jobs immediately, then byte-cap the remaining jobs
/// oldest-first to meet the job cap and the total footprint goal.
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
    let job_cap = effective_job_cap(cfg, report);
    report.job_bytes_remaining = dir_total_bytes(jobs_dir);

    // Oldest first = smallest generation number first.
    remaining.sort_by_key(|(g, _, _)| *g);
    for (_gen, path, size) in remaining {
        let current_total = dir_total_bytes(jobs_dir);
        if current_total <= job_cap {
            break;
        }
        if fs::remove_dir_all(&path).is_ok() {
            report.jobs_byte_capped += 1;
            report.job_bytes_reclaimed += size;
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
            if name == "refs.json" || name == ".staging" {
                continue;
            }
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                total += dir_total_bytes(&entry.path());
            } else if entry.file_type().is_ok_and(|t| t.is_file()) && name != "refs.json" {
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

/// Deterministic legacy edge `edge_type` string → u32 code.
fn legacy_edge_type_code(edge_type: &str) -> u32 {
    match edge_type.to_ascii_lowercase().as_str() {
        "call" | "calls" => 1,
        "data" | "dataflow" | "data_flow" => 2,
        "import" | "imports" | "dependency" => 3,
        "definition" | "defines" => 4,
        "inherit" | "inherits" | "extends" | "implements" => 5,
        // Keep this code aligned with the PDG edge-key encoding. TypeOf was
        // added after the original migration mapping and must not be erased
        // as an unknown legacy edge during conversion.
        "type_of" => 10,
        _ => 0,
    }
}

/// Interned string table for the PDG/SYMBOLS payloads. Strings are stored once
/// and referenced by id; the reader locates them via an (offset, length) table
/// whose offsets are relative to the start of the string bytes region.
struct StringInterner {
    map: HashMap<String, u32>,
    offsets: Vec<(u32, u32)>,
    bytes: Vec<u8>,
}

impl StringInterner {
    fn new() -> Self {
        StringInterner {
            map: HashMap::new(),
            offsets: Vec::new(),
            bytes: Vec::new(),
        }
    }

    fn intern(&mut self, s: &str) -> u32 {
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
    fn into_bytes(self) -> (Vec<u8>, Vec<u8>) {
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
