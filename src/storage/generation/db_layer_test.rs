//! Tests for DB→CAS normalisation (WS4 Task 8 / VAL-CAS-019).
//!
//! These tests prove the dedup guarantee: two DBs with the same logical
//! content but different physical page layouts produce byte-identical
//! VACUUM-normalised CAS blobs.

use super::*;
use crate::storage::cas::CasStore;
use rusqlite::Connection;
use std::fs;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Open a SQLite connection in WAL mode at `<dir>/<name>`.
fn open_sqlite(dir: &std::path::Path, name: &str) -> (PathBuf, Connection) {
    let path = dir.join(name);
    let conn = Connection::open(&path).expect("open sqlite");
    conn.pragma_update(None, "journal_mode", "WAL")
        .expect("enable WAL");
    (path, conn)
}

/// Populate a DB with a representative schema and a few rows.
fn populate_db(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE symbols (id INTEGER PRIMARY KEY, name TEXT, file TEXT);
         CREATE INDEX idx_symbols_name ON symbols(name);
         INSERT INTO symbols (name, file) VALUES ('foo', 'src/main.rs');
         INSERT INTO symbols (name, file) VALUES ('bar', 'src/lib.rs');
         INSERT INTO symbols (name, file) VALUES ('baz', 'src/lib.rs');",
    )
    .expect("populate");
}

/// Populate the same logical content, but insert+delete extra rows first so
/// the B-tree and freelist differ on disk.
fn populate_db_with_churn(conn: &Connection) {
    conn.execute_batch(
        "CREATE TABLE symbols (id INTEGER PRIMARY KEY, name TEXT, file TEXT);
         CREATE INDEX idx_symbols_name ON symbols(name);
         -- Bulk insert churn that fragments the B-tree and builds a freelist.
         INSERT INTO symbols (name, file) VALUES ('churn1', 'x.rs');
         INSERT INTO symbols (name, file) VALUES ('churn2', 'x.rs');
         INSERT INTO symbols (name, file) VALUES ('churn3', 'x.rs');
         INSERT INTO symbols (name, file) VALUES ('churn4', 'x.rs');
         INSERT INTO symbols (name, file) VALUES ('churn5', 'x.rs');
         DELETE FROM symbols WHERE file = 'x.rs';
         -- Now insert the same final rows as the clean DB.
         INSERT INTO symbols (name, file) VALUES ('foo', 'src/main.rs');
         INSERT INTO symbols (name, file) VALUES ('bar', 'src/lib.rs');
         INSERT INTO symbols (name, file) VALUES ('baz', 'src/lib.rs');",
    )
    .expect("populate with churn");
}

// ===========================================================================
// VAL-CAS-019 primary: VACUUM-normalised dedup
// ===========================================================================

#[test]
fn test_db_to_cas_roundtrip_returns_hash() {
    let dir = tempfile::tempdir().unwrap();
    let cas_dir = dir.path().join("cas");
    let cas = CasStore::open(&cas_dir).unwrap();
    let (_db_path, conn) = open_sqlite(dir.path(), "leindex.db");
    populate_db(&conn);

    let hash = db_to_cas_conn(&conn, &cas).expect("db_to_cas");

    // Hash exists in CAS.
    assert!(cas.exists(&hash), "blob must exist in CAS");
    // Bytes are non-empty.
    let bytes = cas.get(&hash).expect("get blob");
    assert!(!bytes.is_empty(), "blob must not be empty");
    // Sanity: the bytes are a SQLite DB (magic header "SQLite format 3\0").
    assert_eq!(
        &bytes[..16],
        b"SQLite format 3\0",
        "blob must start with SQLite magic"
    );
}

#[test]
fn test_vacuum_dedup_identical_hash() {
    // Two DBs with identical logical content but different page layouts must
    // produce the same VACUUM-normalised CAS hash. This is VAL-CAS-019 and
    // the core dedup guarantee for the DB layer.
    let dir = tempfile::tempdir().unwrap();
    let cas_dir = dir.path().join("cas");
    let cas = CasStore::open(&cas_dir).unwrap();

    let (_path_a, conn_a) = open_sqlite(dir.path(), "a.db");
    populate_db(&conn_a);
    let hash_a = db_to_cas_conn(&conn_a, &cas).expect("db_to_cas a");

    let (_path_b, conn_b) = open_sqlite(dir.path(), "b.db");
    populate_db_with_churn(&conn_b);
    let hash_b = db_to_cas_conn(&conn_b, &cas).expect("db_to_cas b");

    assert_eq!(
        hash_a, hash_b,
        "VACUUM-normalised content hash must be identical for same logical content"
    );

    // Logical content really is the same: compare row counts.
    let count_a: i64 = conn_a
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap();
    let count_b: i64 = conn_b
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count_a, count_b, "row counts must match for dedup test");
}

#[test]
fn test_noop_reindex_identical_hash() {
    // Two sequential no-op reindexes on the same DB produce identical hashes.
    // Collapses the user-observed 4→2 generation duplication (spec §4.4).
    let dir = tempfile::tempdir().unwrap();
    let cas_dir = dir.path().join("cas");
    let cas = CasStore::open(&cas_dir).unwrap();
    let (_db_path, conn) = open_sqlite(dir.path(), "leindex.db");
    populate_db(&conn);

    let hash_1 = db_to_cas_conn(&conn, &cas).expect("first run");
    let hash_2 = db_to_cas_conn(&conn, &cas).expect("second run");

    assert_eq!(
        hash_1, hash_2,
        "no-op reindex must produce identical DB blob hash"
    );

    // CAS only stores one copy (dedup at the storage layer too).
    assert_eq!(
        cas.blob_count().unwrap(),
        1,
        "CAS must not duplicate identical DB blobs"
    );
}

// ===========================================================================
// WAL checkpoint + non-disturbance invariants
// ===========================================================================

#[test]
fn test_checkpoint_flushes_wal_before_vacuum() {
    let dir = tempfile::tempdir().unwrap();
    let cas_dir = dir.path().join("cas");
    let cas = CasStore::open(&cas_dir).unwrap();
    let (db_path, conn) = open_sqlite(dir.path(), "leindex.db");
    populate_db(&conn);
    // Extra write that lands in the WAL, so the checkpoint has something to do.
    conn.execute(
        "INSERT INTO symbols (name, file) VALUES ('post', 'y.rs')",
        [],
    )
    .unwrap();

    let _hash = db_to_cas_conn(&conn, &cas).expect("db_to_cas");

    // WAL file should be empty (truncated) after checkpoint(TRUNCATE).
    let wal_path = db_path.with_extension("db-wal");
    if wal_path.exists() {
        let meta = fs::metadata(&wal_path).expect("wal metadata");
        assert_eq!(
            meta.len(),
            0,
            "WAL must be truncated to 0 bytes after checkpoint(TRUNCATE)"
        );
    }
}

#[test]
fn test_vacuum_into_does_not_disturb_live_db() {
    let dir = tempfile::tempdir().unwrap();
    let cas_dir = dir.path().join("cas");
    let cas = CasStore::open(&cas_dir).unwrap();
    let (_db_path, conn) = open_sqlite(dir.path(), "leindex.db");
    populate_db(&conn);

    let _hash = db_to_cas_conn(&conn, &cas).expect("db_to_cas");

    // The live DB is fully usable: query still returns the right rows.
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .expect("live query");
    assert_eq!(count, 3, "live DB content must be intact after VACUUM INTO");

    // We can even continue writing to it.
    conn.execute(
        "INSERT INTO symbols (name, file) VALUES ('after', 'z.rs')",
        [],
    )
    .unwrap();
    let new_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM symbols", [], |r| r.get(0))
        .unwrap();
    assert_eq!(new_count, 4, "live DB still writable after VACUUM INTO");
}

#[test]
fn test_db_to_cas_via_storage_wrapper() {
    // The Storage-wrapper API `db_to_cas(&Storage, &CasStore)` must agree with
    // `db_to_cas_conn(&conn, &CasStore)` for the same underlying DB.
    use crate::storage::schema::Storage;

    let dir = tempfile::tempdir().unwrap();
    let cas_dir = dir.path().join("cas");
    let cas = CasStore::open(&cas_dir).unwrap();
    let db_path = dir.path().join("leindex.db");
    let storage = Storage::open(&db_path).expect("open storage");
    storage
        .conn()
        .execute_batch(
            "CREATE TABLE t(x INTEGER);
             INSERT INTO t VALUES (42);",
        )
        .unwrap();

    let hash_via_storage = db_to_cas(&storage, &cas).expect("db_to_cas via storage");
    let hash_via_conn = db_to_cas_conn(storage.conn(), &cas).expect("db_to_cas via conn");
    assert_eq!(
        hash_via_storage, hash_via_conn,
        "Storage wrapper and Connection API must produce identical hashes"
    );
}

// ===========================================================================
// Cross-store determinism
// ===========================================================================

#[test]
fn test_hash_determinism_across_cas_stores() {
    // The blake3 content hash is purely a function of blob bytes; a different
    // CAS instance must produce the same hash for the same normalised DB.
    let dir = tempfile::tempdir().unwrap();

    let (_path_a, conn_a) = open_sqlite(dir.path(), "a.db");
    populate_db(&conn_a);

    let cas_a = CasStore::open(dir.path().join("cas_a")).unwrap();
    let cas_b = CasStore::open(dir.path().join("cas_b")).unwrap();

    let hash_a = db_to_cas_conn(&conn_a, &cas_a).expect("run on cas_a");
    let hash_b = db_to_cas_conn(&conn_a, &cas_b).expect("run on cas_b");

    assert_eq!(
        hash_a, hash_b,
        "hash must be deterministic across CAS instances"
    );
}
