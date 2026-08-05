//! DB layer normalisation: copy live SQLite DB into CAS via VACUUM.
//!
//! Implements [`db_to_cas`] / [`db_to_cas_conn`] (WS4 Task 8 / VAL-CAS-019),
//! which:
//!
//! 1. **Checkpoints the WAL** into the main DB file
//!    (`PRAGMA wal_checkpoint(TRUNCATE)`). This flushes pending WAL frames so
//!    the subsequent VACUUM reads a single consistent snapshot.
//! 2. **VACUUM INTO** a normalised temp file. VACUUM rebuilds the file from
//!    scratch, producing a deterministic page layout: no freelist, no unused
//!    pages, no WAL/SHM sidecars. Two DBs with identical logical content but
//!    different physical layouts (B-tree splits, freelist, fragmentation)
//!    thus produce byte-identical VACUUM output.
//! 3. **`cas.put`** the normalised bytes to obtain a blake3 content hash.
//!
//! This is the deduplication guarantee for the DB layer (spec §4.4): the
//! observed 4→2 generation duplication collapses to one blob per distinct
//! logical DB state.
//!
//! ## Why VACUUM-normalize?
//!
//! SQLite's file layout is not deterministic across DB lifetimes. Insertion
//! order, B-tree page splits, freelist growth, and WAL checkpoints all
//! influence the on-disk byte representation. Two databases with identical
//! logical content (rows + schema) can therefore differ at the byte level,
//! defeating CAS content-addressing.
//!
//! `VACUUM INTO` produces a byte-deterministic representation by rebuilding
//! the file from a canonical page-by-page copy: no freelist, no fragmented
//! B-trees, no WAL. Given the same logical content, VACUUM always emits the
//! same byte sequence, so `cas.put` returns the same blake3 hash. This is
//! the foundation of cross-generation dedup.

use std::path::Path;

use crate::storage::cas::{CasError, CasStore};
use crate::storage::schema::Storage;

/// Errors returned by [`db_to_cas`] / [`db_to_cas_conn`].
#[derive(Debug, thiserror::Error)]
pub enum DbToCasError {
    /// SQLite error during checkpoint or VACUUM.
    #[error("sqlite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// I/O error reading or writing the normalised temp file.
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    /// CAS error during `put`.
    #[error("cas error: {0}")]
    Cas(#[from] CasError),
}

/// Copy a live SQLite DB at `storage` into the CAS via VACUUM-normalisation.
///
/// Convenience wrapper around [`db_to_cas_conn`] that pulls the underlying
/// [`rusqlite::Connection`] out of a [`Storage`] handle.
pub fn db_to_cas(storage: &Storage, cas: &CasStore) -> Result<[u8; 32], DbToCasError> {
    db_to_cas_conn(storage.conn(), cas)
}

/// Copy a live SQLite DB at `conn` into the CAS via VACUUM-normalisation.
///
/// Steps:
/// 1. `PRAGMA wal_checkpoint(TRUNCATE)` — flush pending WAL frames into the
///    main DB file and truncate the WAL.
/// 2. `VACUUM INTO '<temp>'` — write a normalised copy to a fresh file.
///    VACUUM rebuilds the file page-by-page, producing a deterministic layout
///    with no freelist and no fragmented B-trees. Errors if the target file
///    already exists, so [`db_to_cas_conn`] always writes to a fresh path
///    inside a private temp directory.
/// 3. Read the temp file into memory and `cas.put` its bytes. The CAS hashes
///    the payload with blake3 and stores it deduplicated.
/// 4. Remove the temp file.
///
/// The live DB is **not** disturbed by step 2: `VACUUM INTO` writes to a
/// separate target file. After step 1 the WAL is truncated (its contents are
/// durably in the main DB file); the live connection remains usable for
/// further reads and writes.
///
/// Returns the blake3 hash of the normalised DB bytes. The same logical DB
/// content always produces the same hash, regardless of B-tree shape, page
/// fragmentation, or WAL state at the time of the call.
pub fn db_to_cas_conn(
    conn: &rusqlite::Connection,
    cas: &CasStore,
) -> Result<[u8; 32], DbToCasError> {
    // Step 1: checkpoint(TRUNCATE) so the main DB file contains all writes.
    //
    // `execute_batch` routes through `sqlite3_exec`, which discards the
    // PRAGMA result rows (busy, log, checkpointed) and runs outside any
    // implicit transaction. This leaves the connection in autocommit mode,
    // which is required for VACUUM INTO below.
    //
    // Errors from the checkpoint (e.g. the DB is in a transaction on another
    // connection) are surfaced to the caller; we don't swallow them.
    conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;

    // Step 2: VACUUM INTO a fresh path. SQLite refuses to overwrite an
    // existing file, so we create a private temp directory and name a file
    // inside it. The directory guarantees the file is fresh.
    let tmp_dir = tempfile::tempdir()?;
    let target_path = tmp_dir.path().join("vacuum_normalized.db");
    let sql = format!(
        "VACUUM INTO '{}';",
        escape_sqlite_string(&target_string(&target_path))
    );
    conn.execute_batch(&sql)?;

    // Step 3: read the normalised bytes and put them into the CAS.
    let bytes = std::fs::read(&target_path)?;
    let hash = cas.put(&bytes)?;

    // Step 4: best-effort cleanup. `tmp_dir` would do this on drop, but we
    // explicitly remove the file first to release space promptly on systems
    // where the temp dir cleanup is asynchronous.
    let _ = std::fs::remove_file(&target_path);

    Ok(hash)
}

/// Format `path` as a string for embedding in a SQL string literal.
///
/// On Unix we forward the path bytes lossily. SQLite accepts both UTF-8 byte
/// sequences and UTF-16 file paths via the VFS; the bundled SQLite on Linux
/// uses the UTF-8 VFS, so a lossy UTF-8 string is correct.
fn target_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Escape a string for inclusion inside SQLite single-quoted string literal.
///
/// SQLite doubles embedded single quotes (`'` → `''`) inside a single-quoted
/// string. We don't need backslash escapes here because the bundled VFS does
/// not interpret backslashes in URI-style paths on Linux.
fn escape_sqlite_string(s: &str) -> String {
    s.replace('\'', "''")
}

#[cfg(test)]
#[path = "db_layer_test.rs"]
mod tests;
