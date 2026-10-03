use super::*;
use crate::storage::schema::Storage;
use std::ffi::CStr;
use std::os::raw::{c_char, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use tempfile::NamedTempFile;

/// Counts of INSERT/DELETE statements emitted against intel_nodes /
/// intel_edges, populated by a raw `sqlite3_trace` callback installed for
/// the batch tests below. Statics (not a captured closure) because the
/// trace hook is an `unsafe extern "C"` function pointer and cannot
/// capture state. Note: legacy sqlite3_trace delivers SQL with bound
/// values EXPANDED inline, so only prefix/statement-level matching is
/// sound — never count "?," tuples in the traced text.
struct TraceCounts {
    node: AtomicUsize,
    edge: AtomicUsize,
    edge_delete: AtomicUsize,
}
static TRACE_COUNTS: TraceCounts = TraceCounts {
    node: AtomicUsize::new(0),
    edge: AtomicUsize::new(0),
    edge_delete: AtomicUsize::new(0),
};
/// The sqlite3_trace hook is connection-scoped, but this callback writes to
/// process-global counters. Two trace-harness tests running concurrently
/// would therefore cross-contaminate counts, so every trace-harness test
/// holds this lock across its enable/save/disable window.
static TRACE_HARNESS_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

unsafe extern "C" fn sql_trace_cb(_user: *mut c_void, sql_ptr: *const c_char) {
    if sql_ptr.is_null() {
        return;
    }
    // SAFETY: sqlite3 hands us a NUL-terminated C string for the duration
    // of the callback; we only read it, never hold it past the call.
    let sql = unsafe { CStr::from_ptr(sql_ptr) }.to_string_lossy();
    if sql.starts_with("INSERT INTO intel_nodes") {
        TRACE_COUNTS.node.fetch_add(1, Ordering::Relaxed);
    } else if sql.starts_with("INSERT INTO intel_edges") {
        TRACE_COUNTS.edge.fetch_add(1, Ordering::Relaxed);
    } else if sql.starts_with("DELETE FROM intel_edges") {
        TRACE_COUNTS.edge_delete.fetch_add(1, Ordering::Relaxed);
    }
}

/// Install/clear a raw `sqlite3_trace` callback on the connection.
///
/// rusqlite's ergonomic `Connection::trace` is gated behind the unused
/// `trace` crate feature; the underlying `sqlite3_trace` FFI is always
/// available through `rusqlite::ffi`, so we call it directly to count the
/// exact number of executed statements deterministically.
fn set_sql_trace(conn: &rusqlite::Connection, enabled: bool) {
    let db = unsafe { conn.handle() };
    unsafe {
        if enabled {
            rusqlite::ffi::sqlite3_trace(db, Some(sql_trace_cb), std::ptr::null_mut());
        } else {
            rusqlite::ffi::sqlite3_trace(db, None, std::ptr::null_mut());
        }
    }
}

fn create_large_pdg(node_count: usize, edge_count: usize) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();
    let mut node_ids = Vec::with_capacity(node_count);
    for i in 0..node_count {
        let node_id = pdg.add_node(PDGNode {
            id: format!("src/main.rs:func{i}"),
            node_type: PDGNodeType::Function,
            name: format!("func{i}"),
            file_path: Arc::from("src/main.rs"),
            byte_range: (i * 10, i * 10 + 8),
            complexity: (i % 7) as u32,
            language: "rust".to_string(),
        });
        node_ids.push(node_id);
    }
    for i in 0..edge_count {
        let a = node_ids[i];
        let b = node_ids[(i + 1) % node_count];
        pdg.add_edge(
            a,
            b,
            PDGEdge {
                edge_type: PDGEdgeType::Call,
                metadata: PDGEdgeMetadata {
                    call_count: Some(1),
                    variable_name: None,
                    confidence: Some(0.7),
                    channel: None,
                    position: None,
                },
            },
        );
    }
    pdg
}

fn create_test_pdg() -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();

    let n1 = pdg.add_node(PDGNode {
        id: "func1".to_string(),
        node_type: PDGNodeType::Function,
        name: "func1".to_string(),
        file_path: Arc::from("test.rs"),
        byte_range: (0, 100),
        complexity: 5,
        language: "rust".to_string(),
    });

    let n2 = pdg.add_node(PDGNode {
        id: "func2".to_string(),
        node_type: PDGNodeType::Function,
        name: "func2".to_string(),
        file_path: Arc::from("test.rs"),
        byte_range: (100, 200),
        complexity: 3,
        language: "rust".to_string(),
    });

    pdg.add_edge(
        n1,
        n2,
        PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata {
                call_count: Some(5),
                variable_name: None,
                confidence: None,
                channel: None,
                position: None,
            },
        },
    );

    pdg
}

#[test]
#[cfg(feature = "precision")]
fn test_precision_marker_round_trips_through_pdg_store() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();
    let mut pdg = create_test_pdg();
    pdg.mark_precision_symbol("func1");

    save_pdg(&mut storage, "precision_roundtrip", &pdg).unwrap();
    let loaded = load_pdg(&storage, "precision_roundtrip").unwrap();

    assert!(loaded.is_precision_symbol("func1"));
    assert!(!loaded.is_precision_symbol("func2"));
    let precision: i32 = storage
        .conn()
        .query_row(
            "SELECT precision FROM intel_nodes WHERE project_id = ?1 AND node_id = ?2",
            params!["precision_roundtrip", "func1"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(precision, 1);
}

#[test]
#[cfg(feature = "precision")]
fn test_precision_marker_change_invalidates_node_noop() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();
    let mut pdg = create_test_pdg();
    save_pdg(&mut storage, "precision_change", &pdg).unwrap();

    pdg.mark_precision_symbol("func1");
    save_pdg(&mut storage, "precision_change", &pdg).unwrap();

    let precision: i32 = storage
        .conn()
        .query_row(
            "SELECT precision FROM intel_nodes WHERE project_id = ?1 AND node_id = ?2",
            params!["precision_change", "func1"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(precision, 1);

    pdg.precision_symbols.clear();
    save_pdg(&mut storage, "precision_change", &pdg).unwrap();
    let cleared: i32 = storage
        .conn()
        .query_row(
            "SELECT precision FROM intel_nodes WHERE project_id = ?1 AND node_id = ?2",
            params!["precision_change", "func1"],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(cleared, 0);
}

#[test]
fn test_save_and_load_pdg() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg = create_test_pdg();
    save_pdg(&mut storage, "test_project", &pdg).unwrap();

    assert!(pdg_exists(&storage, "test_project").unwrap());

    let loaded = load_pdg(&storage, "test_project").unwrap();
    assert_eq!(loaded.node_count(), 2);
    assert_eq!(loaded.edge_count(), 1);

    let func1 = loaded.find_by_symbol("func1").unwrap();
    let node1 = loaded.get_node(func1).unwrap();
    assert_eq!(node1.complexity, 5);
}

#[test]
fn test_is_transient_lock_error_classification() {
    use rusqlite::ErrorCode;
    let busy = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: ErrorCode::DatabaseBusy,
            extended_code: 5,
        },
        None,
    );
    assert!(is_transient_lock_error(&busy));
    let locked = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: ErrorCode::DatabaseLocked,
            extended_code: 6,
        },
        None,
    );
    assert!(is_transient_lock_error(&locked));
    let constraint = rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error {
            code: ErrorCode::ConstraintViolation,
            extended_code: 19,
        },
        None,
    );
    assert!(!is_transient_lock_error(&constraint));
}

#[test]
fn test_save_pdg_survives_competing_writer_lock() {
    // A competing connection holds the write lock briefly while save_pdg
    // runs. The busy_timeout (re-asserted by save_pdg) must wait out the
    // competing writer instead of failing the index persist.
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();
    let pdg = create_test_pdg();

    let db_path = temp_file.path().to_path_buf();
    let holder = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS lock_probe(value INTEGER);
             INSERT INTO lock_probe VALUES (1);",
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(400));
        conn.execute_batch("ROLLBACK;").unwrap();
    });

    // Give the holder time to acquire the write lock before saving.
    std::thread::sleep(std::time::Duration::from_millis(150));
    let started = std::time::Instant::now();
    save_pdg(&mut storage, "lock_proj", &pdg)
        .expect("save_pdg must wait out a brief competing write lock");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(4),
        "save_pdg should not exceed the busy window for a short lock"
    );
    holder.join().unwrap();

    let loaded = load_pdg(&storage, "lock_proj").unwrap();
    assert_eq!(loaded.node_count(), 2);
    assert_eq!(loaded.edge_count(), 1);
}

#[test]
fn test_large_save_pdg_survives_pinned_reader_and_wal_stays_bounded() {
    // Reproduces the stress-test report's actual failure signature: the
    // MCP server's long-lived catalog/search connections hold a read
    // snapshot that pins the WAL, so `wal_checkpoint(TRUNCATE)` cannot
    // shrink it and it grows without bound (57MB in the report). This
    // must not fail the writer — WAL readers do not block writers — and
    // once the reader releases, the WAL must be recoverable to a bounded
    // size rather than leaking forever.
    let temp_file = NamedTempFile::new().unwrap();
    let db_path = temp_file.path().to_path_buf();
    let wal_path = std::path::PathBuf::from(format!("{}-wal", db_path.display()));

    let mut storage = Storage::open(&db_path).unwrap();
    // A multi-thousand-node PDG so the write genuinely produces a
    // multi-page WAL instead of a trivial one.
    let pdg = create_large_pdg(2000, 1999);

    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<()>();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let reader_db = db_path.clone();
    let reader = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(reader_db).unwrap();
        // Open a read transaction and hold its snapshot open: this is what
        // pins the WAL and blocks `wal_checkpoint(TRUNCATE)`.
        conn.execute_batch("BEGIN").unwrap();
        let _count: i64 = conn
            .query_row("SELECT COUNT(*) FROM intel_nodes", [], |row| row.get(0))
            .unwrap();
        ready_tx.send(()).unwrap();
        let _ = release_rx.recv();
        conn.execute_batch("COMMIT").unwrap();
    });

    // Wait until the reader has actually pinned its snapshot before writing.
    ready_rx.recv().unwrap();

    save_pdg(&mut storage, "pinned_reader_proj", &pdg)
        .expect("save_pdg must commit while a reader pins the WAL");

    let loaded = load_pdg(&storage, "pinned_reader_proj").unwrap();
    assert_eq!(loaded.node_count(), 2000);
    assert_eq!(loaded.edge_count(), 1999);

    // Release the reader and checkpoint; the WAL must shrink back to a
    // bounded size, proving the bloat came from the pinned reader and not
    // from a leaked/corrupted WAL.
    release_tx.send(()).unwrap();
    reader.join().unwrap();
    storage
        .conn()
        .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
        .unwrap();
    let wal_len = std::fs::metadata(&wal_path).map(|m| m.len()).unwrap_or(0);
    assert!(
        wal_len < 1024 * 1024,
        "WAL should shrink below 1MiB after the reader releases; got {wal_len}"
    );
}

#[test]
fn test_save_pdg_retries_after_busy_timeout_expires() {
    // Distinguishes the retry loop from the busy_timeout alone: the
    // competing writer holds the write lock *longer* than the 5s
    // busy_timeout, so the first attempt must fail with SQLITE_BUSY and
    // the bounded retry must rescue the save. A lock held briefly (as in
    // test_save_pdg_survives_competing_writer_lock) is absorbed by
    // busy_timeout and never exercises the retry path.
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();
    let pdg = create_test_pdg();

    let db_path = temp_file.path().to_path_buf();
    let (held_tx, held_rx) = std::sync::mpsc::channel::<()>();
    let holder = std::thread::spawn(move || {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS lock_probe(value INTEGER);
             INSERT INTO lock_probe VALUES (1);",
        )
        .unwrap();
        held_tx.send(()).unwrap();
        // Hold past the 5s busy_timeout so the writer's first attempt
        // genuinely fails with SQLITE_BUSY and the retry loop must run.
        std::thread::sleep(std::time::Duration::from_millis(6000));
        conn.execute_batch("ROLLBACK;").unwrap();
    });

    // Start the writer only after the lock is held.
    held_rx.recv().unwrap();
    let started = std::time::Instant::now();
    save_pdg(&mut storage, "retry_proj", &pdg)
        .expect("save_pdg must retry after busy_timeout expires");
    holder.join().unwrap();

    // The save must have taken at least the full busy_timeout window,
    // proving the retry (not just busy_timeout) rescued the commit.
    assert!(
        started.elapsed() >= std::time::Duration::from_millis(5000),
        "expected save_pdg to exhaust busy_timeout and retry"
    );
    let loaded = load_pdg(&storage, "retry_proj").unwrap();
    assert_eq!(loaded.node_count(), 2);
}

#[test]
fn test_save_pdg_duplicate_node_id_keeps_all_edges_referencable() {
    // Reproduces the real stress-test root cause. The graph legitimately
    // holds multiple nodes that share the same `node_id` string (e.g.
    // `__external__` import nodes collapse to the same symbol in
    // different files). When those land in the same upsert chunk, a
    // conditional `DO UPDATE ... WHERE content_hash != excluded.content_hash`
    // used to suppress the second UPDATE, so `RETURNING id` returned fewer
    // rows than the chunk and the tail nodes were dropped from the
    // `node_id_map` — their edges then failed with `EdgeNodeMissing` and
    // the whole persist aborted. The upsert must return exactly one id per
    // tuple so every node (and thus every edge) resolves.
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let mut pdg = ProgramDependenceGraph::new();
    // Two distinct nodes with the SAME `id` string, so they collide on the
    // unique (project_id, node_id) index. They must both end up in the
    // node map (mapping to the same db row id is fine — the DB dedupes by
    // node_id, the graph keeps both for in-memory analysis).
    let duplicate_id = "shared.rs:collide";
    let a = pdg.add_node(PDGNode {
        id: duplicate_id.to_string(),
        node_type: PDGNodeType::External,
        name: "collide".to_string(),
        file_path: Arc::from("shared.rs"),
        byte_range: (0, 10),
        complexity: 0,
        language: "external".to_string(),
    });
    let b = pdg.add_node(PDGNode {
        id: duplicate_id.to_string(),
        node_type: PDGNodeType::External,
        name: "collide".to_string(),
        file_path: Arc::from("shared.rs"),
        byte_range: (0, 10),
        complexity: 0,
        language: "external".to_string(),
    });
    let importer = pdg.add_node(PDGNode {
        id: "src/main.rs:main".to_string(),
        node_type: PDGNodeType::Function,
        name: "main".to_string(),
        file_path: Arc::from("src/main.rs"),
        byte_range: (0, 50),
        complexity: 1,
        language: "rust".to_string(),
    });
    // Two Import edges whose targets are the colliding nodes. Before the
    // fix, the colliding node b fell out of the map and this save failed.
    pdg.add_edge(
        importer,
        a,
        PDGEdge {
            edge_type: PDGEdgeType::Import,
            metadata: PDGEdgeMetadata::empty(),
        },
    );
    pdg.add_edge(
        importer,
        b,
        PDGEdge {
            edge_type: PDGEdgeType::Import,
            metadata: PDGEdgeMetadata::empty(),
        },
    );

    save_pdg(&mut storage, "dup_proj", &pdg)
        .expect("duplicate node_id must not drop nodes from the map");

    let loaded = load_pdg(&storage, "dup_proj").unwrap();
    // The DB dedupes the duplicate node_id to a single row (unique
    // (project_id, node_id) index) and the two identical Import edges onto
    // the single (caller, callee, edge_type) row. The point of this test
    // is that the save SUCCEEDS with both edges resolved — before the fix
    // the colliding node fell out of the map and save_pdg failed with
    // `EdgeNodeMissing`.
    assert_eq!(loaded.node_count(), 2);
    assert_eq!(loaded.edge_count(), 1);
}

#[test]
fn test_delete_file_data_keeps_shared_external_placeholder() {
    // The shared `external::` row lives under whatever file's extraction
    // pass created it, so deleting that file must not reap the placeholder
    // (or the other files' edges to it). Both the node delete and the edge
    // delete exclude external rows.
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let mut pdg = ProgramDependenceGraph::new();
    let a_main = pdg.add_node(PDGNode {
        id: "a.rs:main".to_string(),
        node_type: PDGNodeType::Function,
        name: "main".to_string(),
        file_path: Arc::from("a.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });
    let b_main = pdg.add_node(PDGNode {
        id: "b.rs:main".to_string(),
        node_type: PDGNodeType::Function,
        name: "main".to_string(),
        file_path: Arc::from("b.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });
    let shared = pdg.add_node(PDGNode {
        id: "external::String".to_string(),
        node_type: PDGNodeType::External,
        name: "String".to_string(),
        file_path: Arc::from("a.rs"),
        byte_range: (0, 0),
        complexity: 0,
        language: "external".to_string(),
    });
    pdg.add_edge(
        a_main,
        shared,
        PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata::empty(),
        },
    );
    pdg.add_edge(
        b_main,
        shared,
        PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata::empty(),
        },
    );
    save_pdg(&mut storage, "del_ext_proj", &pdg).unwrap();

    delete_file_data(&mut storage, "del_ext_proj", "a.rs").unwrap();

    let loaded = load_pdg(&storage, "del_ext_proj").unwrap();
    assert!(
        loaded.find_by_id("external::String").is_some(),
        "the shared external survives its creating file's deletion"
    );
    assert_eq!(
        loaded.node_count(),
        2,
        "b.rs:main + the shared external (a.rs:main gone)"
    );
    assert_eq!(
        loaded.edge_count(),
        1,
        "b.rs's edge to the shared external survives"
    );
}

#[test]
fn test_shared_external_placeholder_round_trips_losslessly() {
    // The shape `merge_pdgs` produces after exact-id dedup: two callers and
    // ONE shared `external::` placeholder per target (the pre-dedup graph
    // held one placeholder per file, and every copy upserted onto the single
    // (project_id, node_id) row, conflating their columns and collapsing
    // node_id_map entries on every subsequent save). The deduped graph must
    // persist and reload with node and edge counts intact, and its save must
    // be a no-op on the second pass (stable content hashes, no flapping).
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let mut pdg = ProgramDependenceGraph::new();
    let a_main = pdg.add_node(PDGNode {
        id: "a.rs:main".to_string(),
        node_type: PDGNodeType::Function,
        name: "main".to_string(),
        file_path: Arc::from("a.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });
    let b_main = pdg.add_node(PDGNode {
        id: "b.rs:main".to_string(),
        node_type: PDGNodeType::Function,
        name: "main".to_string(),
        file_path: Arc::from("b.rs"),
        byte_range: (0, 10),
        complexity: 1,
        language: "rust".to_string(),
    });
    let shared = pdg.add_node(PDGNode {
        id: "external::String".to_string(),
        node_type: PDGNodeType::External,
        name: "String".to_string(),
        file_path: Arc::from("<external>"),
        byte_range: (0, 0),
        complexity: 0,
        language: "external".to_string(),
    });
    pdg.add_edge(
        a_main,
        shared,
        PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata::empty(),
        },
    );
    pdg.add_edge(
        b_main,
        shared,
        PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata::empty(),
        },
    );

    save_pdg(&mut storage, "shared_external_proj", &pdg).unwrap();
    let loaded = load_pdg(&storage, "shared_external_proj").unwrap();
    assert_eq!(loaded.node_count(), 3, "3 graph nodes -> 3 rows -> 3 nodes");
    assert_eq!(loaded.edge_count(), 2, "both calls to the shared external");
    assert!(loaded.find_by_id("external::String").is_some());

    // Re-saving the same graph must converge: every node diffs unchanged
    // against the first pass, so counts stay stable.
    save_pdg(&mut storage, "shared_external_proj", &pdg).unwrap();
    let reloaded = load_pdg(&storage, "shared_external_proj").unwrap();
    assert_eq!(reloaded.node_count(), 3);
    assert_eq!(reloaded.edge_count(), 2);
}

#[test]
fn test_legacy_schema_missing_timestamp_columns_repaired() {
    // Simulate a database created before intel_nodes had created_at/
    // updated_at columns. `CREATE TABLE IF NOT EXISTS` never repairs an
    // existing table, so Storage::open must add the columns through the
    // ensure-columns path; otherwise save_pdg's INSERT fails with
    // "no such column" and every index generation dies at persist.
    let temp_file = NamedTempFile::new().unwrap();
    {
        let conn = rusqlite::Connection::open(temp_file.path()).unwrap();
        conn.execute_batch(
            "CREATE TABLE intel_nodes (
                id INTEGER PRIMARY KEY,
                project_id TEXT NOT NULL,
                file_path TEXT NOT NULL,
                node_id TEXT NOT NULL,
                symbol_name TEXT NOT NULL,
                qualified_name TEXT NOT NULL,
                language TEXT NOT NULL DEFAULT 'unknown',
                node_type TEXT NOT NULL,
                signature TEXT,
                complexity INTEGER,
                embedding BLOB,
                byte_range_start INTEGER,
                byte_range_end INTEGER,
                embedding_format INTEGER
            );
            CREATE TABLE intel_edges (
                caller_id INTEGER NOT NULL,
                callee_id INTEGER NOT NULL,
                edge_type TEXT NOT NULL,
                metadata TEXT,
                FOREIGN KEY(caller_id) REFERENCES intel_nodes(id),
                FOREIGN KEY(callee_id) REFERENCES intel_nodes(id),
                PRIMARY KEY(caller_id, callee_id, edge_type)
            );",
        )
        .unwrap();
    }

    let mut storage = Storage::open(temp_file.path()).unwrap();
    let columns: Vec<String> = storage
        .conn()
        .prepare("PRAGMA table_info(intel_nodes)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    assert!(
        columns.iter().any(|column| column == "created_at"),
        "legacy schema must gain created_at"
    );
    assert!(
        columns.iter().any(|column| column == "updated_at"),
        "legacy schema must gain updated_at"
    );
    assert!(
        columns.iter().any(|column| column == "content_hash"),
        "legacy schema must gain content_hash"
    );

    // A full save/load round-trip must work on the repaired schema.
    let pdg = create_test_pdg();
    save_pdg(&mut storage, "legacy_proj", &pdg).unwrap();
    let loaded = load_pdg(&storage, "legacy_proj").unwrap();
    assert_eq!(loaded.node_count(), 2);
}

#[test]
fn test_save_pdg_replaces_existing() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg1 = create_test_pdg();
    save_pdg(&mut storage, "test_project", &pdg1).unwrap();
    assert_eq!(load_pdg(&storage, "test_project").unwrap().node_count(), 2);

    let mut pdg2 = ProgramDependenceGraph::new();
    pdg2.add_node(PDGNode {
        id: "new_func".to_string(),
        node_type: PDGNodeType::Function,
        name: "new_func".to_string(),
        file_path: Arc::from("new.rs"),
        byte_range: (0, 50),
        complexity: 1,
        language: "rust".to_string(),
    });

    save_pdg(&mut storage, "test_project", &pdg2).unwrap();
    assert_eq!(load_pdg(&storage, "test_project").unwrap().node_count(), 1);
}

#[test]
fn test_load_nonexistent_project() {
    let temp_file = NamedTempFile::new().unwrap();
    let storage = Storage::open(temp_file.path()).unwrap();

    let loaded = load_pdg(&storage, "nonexistent").unwrap();
    assert_eq!(loaded.node_count(), 0);
    assert_eq!(loaded.edge_count(), 0);
}

#[test]
fn test_delete_pdg() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg = create_test_pdg();
    save_pdg(&mut storage, "test_project", &pdg).unwrap();
    assert!(pdg_exists(&storage, "test_project").unwrap());

    delete_pdg(&mut storage, "test_project").unwrap();
    assert!(!pdg_exists(&storage, "test_project").unwrap());
}

#[test]
fn test_convert_node_types() {
    assert_eq!(
        convert_node_type(&PDGNodeType::Function),
        StorageNodeType::Function
    );
    assert_eq!(
        convert_node_type(&PDGNodeType::Class),
        StorageNodeType::Class
    );
    assert_eq!(
        convert_node_type(&PDGNodeType::Method),
        StorageNodeType::Method
    );
    assert_eq!(
        convert_node_type(&PDGNodeType::Variable),
        StorageNodeType::Variable
    );
    assert_eq!(
        convert_node_type(&PDGNodeType::Module),
        StorageNodeType::Module
    );

    assert_eq!(
        convert_storage_node_type(&StorageNodeType::Function),
        PDGNodeType::Function
    );
    assert_eq!(
        convert_storage_node_type(&StorageNodeType::Class),
        PDGNodeType::Class
    );
    assert_eq!(
        convert_storage_node_type(&StorageNodeType::Method),
        PDGNodeType::Method
    );
    assert_eq!(
        convert_storage_node_type(&StorageNodeType::Variable),
        PDGNodeType::Variable
    );
    assert_eq!(
        convert_storage_node_type(&StorageNodeType::Module),
        PDGNodeType::Module
    );

    // External node type round-trip
    assert_eq!(
        convert_node_type(&PDGNodeType::External),
        StorageNodeType::External
    );
    assert_eq!(
        convert_storage_node_type(&StorageNodeType::External),
        PDGNodeType::External
    );
}

#[test]
fn test_convert_edge_types() {
    assert_eq!(convert_edge_type(&PDGEdgeType::Call), StorageEdgeType::Call);
    assert_eq!(
        convert_edge_type(&PDGEdgeType::DataDependency),
        StorageEdgeType::DataDependency
    );
    assert_eq!(
        convert_edge_type(&PDGEdgeType::Inheritance),
        StorageEdgeType::Inheritance
    );
    assert_eq!(
        convert_edge_type(&PDGEdgeType::Import),
        StorageEdgeType::Import
    );
    for (pdg, storage) in [
        (PDGEdgeType::Containment, StorageEdgeType::Containment),
        (
            PDGEdgeType::StateTransition,
            StorageEdgeType::StateTransition,
        ),
        (
            PDGEdgeType::CommandArgument,
            StorageEdgeType::CommandArgument,
        ),
        (PDGEdgeType::Environment, StorageEdgeType::Environment),
        (PDGEdgeType::Stdin, StorageEdgeType::Stdin),
    ] {
        assert_eq!(convert_edge_type(&pdg), storage);
        assert_eq!(convert_storage_edge_type(&storage), pdg);
    }

    assert_eq!(
        convert_storage_edge_type(&StorageEdgeType::Call),
        PDGEdgeType::Call
    );
    assert_eq!(
        convert_storage_edge_type(&StorageEdgeType::DataDependency),
        PDGEdgeType::DataDependency
    );
    assert_eq!(
        convert_storage_edge_type(&StorageEdgeType::Inheritance),
        PDGEdgeType::Inheritance
    );
    assert_eq!(
        convert_storage_edge_type(&StorageEdgeType::Import),
        PDGEdgeType::Import
    );
}

#[test]
fn test_edge_metadata_conversion() {
    let pdg_meta = PDGEdgeMetadata {
        call_count: Some(42),
        variable_name: Some("x".to_string()),
        confidence: None,
        channel: Some("env".to_string()),
        position: Some(1),
    };

    let storage_meta = convert_edge_metadata(&pdg_meta);
    assert_eq!(storage_meta.call_count, Some(42));
    assert_eq!(storage_meta.variable_name, Some("x".to_string()));
    assert_eq!(storage_meta.channel.as_deref(), Some("env"));
    assert_eq!(storage_meta.position, Some(1));

    let converted_back = convert_storage_edge_metadata(&storage_meta);
    assert_eq!(converted_back.call_count, Some(42));
    assert_eq!(converted_back.channel.as_deref(), Some("env"));
    assert_eq!(converted_back.position, Some(1));
    assert_eq!(converted_back.variable_name, Some("x".to_string()));
}

#[test]
fn test_save_pdg_with_inheritance_and_data_dependency_edges() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let mut pdg = ProgramDependenceGraph::new();

    let n1 = pdg.add_node(PDGNode {
        id: "child".to_string(),
        node_type: PDGNodeType::Class,
        name: "Child".to_string(),
        file_path: Arc::from("test.rs"),
        byte_range: (0, 50),
        complexity: 1,
        language: "rust".to_string(),
    });

    let n2 = pdg.add_node(PDGNode {
        id: "parent".to_string(),
        node_type: PDGNodeType::Class,
        name: "Parent".to_string(),
        file_path: Arc::from("test.rs"),
        byte_range: (50, 100),
        complexity: 1,
        language: "rust".to_string(),
    });

    let n3 = pdg.add_node(PDGNode {
        id: "data_user".to_string(),
        node_type: PDGNodeType::Function,
        name: "data_user".to_string(),
        file_path: Arc::from("test.rs"),
        byte_range: (100, 150),
        complexity: 1,
        language: "rust".to_string(),
    });

    pdg.add_edge(
        n1,
        n2,
        PDGEdge {
            edge_type: PDGEdgeType::Inheritance,
            metadata: PDGEdgeMetadata {
                call_count: None,
                variable_name: None,
                confidence: None,
                channel: None,
                position: None,
            },
        },
    );

    pdg.add_edge(
        n3,
        n1,
        PDGEdge {
            edge_type: PDGEdgeType::DataDependency,
            metadata: PDGEdgeMetadata {
                call_count: None,
                variable_name: Some("child_instance".to_string()),
                confidence: None,
                channel: None,
                position: None,
            },
        },
    );

    save_pdg(&mut storage, "test_project", &pdg).unwrap();

    let loaded = load_pdg(&storage, "test_project").unwrap();
    assert_eq!(loaded.node_count(), 3);
    assert_eq!(loaded.edge_count(), 2);

    // Verify edges by checking connectivity
    let child_id = loaded.find_by_symbol("child").unwrap();
    let parent_id = loaded.find_by_symbol("parent").unwrap();
    let data_user_id = loaded.find_by_symbol("data_user").unwrap();

    // Child should have Parent as neighbor (inheritance)
    let child_neighbors = loaded.neighbors(child_id);
    assert!(child_neighbors.contains(&parent_id));

    // data_user should have Child as neighbor (data dependency)
    let data_user_neighbors = loaded.neighbors(data_user_id);
    assert!(data_user_neighbors.contains(&child_id));
}

#[test]
fn test_save_nodes_uses_fewer_than_100_inserts_for_10k_nodes() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg = create_large_pdg(10_000, 10_000);

    let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
    TRACE_COUNTS.node.store(0, Ordering::Relaxed);
    TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
    set_sql_trace(storage.conn(), true);
    save_pdg(&mut storage, "big_project", &pdg).unwrap();
    set_sql_trace(storage.conn(), false);
    drop(_harness);

    let node_stmts = TRACE_COUNTS.node.load(Ordering::Relaxed);
    assert!(
        node_stmts < 100,
        "save_nodes emitted {node_stmts} INSERT statements for 10K nodes (expected < 100)"
    );
    // Every row batch must map back: node_id_map -> db_id preserved correctly.
    let loaded = load_pdg(&storage, "big_project").unwrap();
    assert_eq!(loaded.node_count(), 10_000);
    // Sanity-check a specific sorted node survived the round trip.
    assert!(
        loaded.find_by_symbol("src/main.rs:func0").is_some(),
        "node 'src/main.rs:func0' should round-trip through batched inserts"
    );
}

#[test]
fn test_save_edges_uses_fewer_than_100_inserts_for_10k_edges() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg = create_large_pdg(10_000, 10_000);

    let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
    TRACE_COUNTS.node.store(0, Ordering::Relaxed);
    TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
    set_sql_trace(storage.conn(), true);
    save_pdg(&mut storage, "big_project", &pdg).unwrap();
    set_sql_trace(storage.conn(), false);
    drop(_harness);

    let edge_stmts = TRACE_COUNTS.edge.load(Ordering::Relaxed);
    assert!(
        edge_stmts < 100,
        "save_edges emitted {edge_stmts} INSERT statements for 10K edges (expected < 100)"
    );

    let loaded = load_pdg(&storage, "big_project").unwrap();
    assert_eq!(loaded.edge_count(), 10_000);
}

#[test]
fn test_resave_unchanged_pdg_issues_no_node_writes() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg = create_large_pdg(1_000, 1_000);
    save_pdg(&mut storage, "resave_proj", &pdg).unwrap();

    // Second save with an identical PDG: every node's content hash matches
    // its stored row, so the upsert path must issue ZERO node writes and
    // reuse the existing db ids. Edges are still fully rebuilt by design.
    let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
    TRACE_COUNTS.node.store(0, Ordering::Relaxed);
    TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
    set_sql_trace(storage.conn(), true);
    save_pdg(&mut storage, "resave_proj", &pdg).unwrap();
    set_sql_trace(storage.conn(), false);
    drop(_harness);

    let node_stmts = TRACE_COUNTS.node.load(Ordering::Relaxed);
    assert_eq!(
        node_stmts, 0,
        "unchanged resave emitted {node_stmts} node INSERT/upsert statements (expected 0)"
    );

    let loaded = load_pdg(&storage, "resave_proj").unwrap();
    assert_eq!(loaded.node_count(), 1_000);
    assert_eq!(loaded.edge_count(), 1_000);
}

#[test]
fn test_resave_with_changed_node_writes_only_changed_rows() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let mut pdg = create_large_pdg(100, 100);
    save_pdg(&mut storage, "resave_proj", &pdg).unwrap();

    // Mutate exactly one node's complexity; the other 99 are unchanged.
    let first_node = pdg.node_indices().next().expect("pdg has nodes");
    pdg.get_node_mut(first_node)
        .expect("node exists")
        .complexity += 1;

    let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
    TRACE_COUNTS.node.store(0, Ordering::Relaxed);
    TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
    set_sql_trace(storage.conn(), true);
    save_pdg(&mut storage, "resave_proj", &pdg).unwrap();
    set_sql_trace(storage.conn(), false);
    drop(_harness);

    let node_stmts = TRACE_COUNTS.node.load(Ordering::Relaxed);
    assert_eq!(
        node_stmts, 1,
        "one-node change emitted {node_stmts} node INSERT/upsert statements (expected 1)"
    );

    let loaded = load_pdg(&storage, "resave_proj").unwrap();
    assert_eq!(loaded.node_count(), 100);
    assert_eq!(loaded.edge_count(), 100);
    // The changed node's complexity round-tripped.
    let changed = loaded.get_node(first_node).expect("node survived resave");
    assert!(changed.complexity > 0);
}

#[test]
fn test_resave_unchanged_pdg_skips_trigram_blob_rewrite() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg = create_test_pdg();
    save_pdg(&mut storage, "trigram_skip_proj", &pdg).unwrap();
    let first_updated_at: i64 = storage
        .conn()
        .query_row(
            "SELECT updated_at FROM trigram_index WHERE project_id = 'trigram_skip_proj'",
            [],
            |row| row.get(0),
        )
        .unwrap();

    // An identical re-save must skip the blob rewrite: updated_at would
    // move forward on any write, so a stalled timestamp proves the skip.
    std::thread::sleep(std::time::Duration::from_millis(1100));
    save_pdg(&mut storage, "trigram_skip_proj", &pdg).unwrap();
    let second_updated_at: i64 = storage
        .conn()
        .query_row(
            "SELECT updated_at FROM trigram_index WHERE project_id = 'trigram_skip_proj'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        first_updated_at, second_updated_at,
        "unchanged trigram index must not be rewritten"
    );
}

#[test]
fn test_resave_unchanged_pdg_issues_no_edge_writes() {
    // The save-PDG hot path: an identical re-save used to DELETE and
    // re-INSERT every edge row (110K+ on large projects). The edge diff
    // must issue ZERO edge write statements when nothing changed.
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let pdg = create_large_pdg(1_000, 1_000);
    save_pdg(&mut storage, "edge_resave_proj", &pdg).unwrap();

    let _harness = TRACE_HARNESS_LOCK.lock().unwrap();
    TRACE_COUNTS.node.store(0, Ordering::Relaxed);
    TRACE_COUNTS.edge.store(0, Ordering::Relaxed);
    TRACE_COUNTS.edge_delete.store(0, Ordering::Relaxed);
    set_sql_trace(storage.conn(), true);
    save_pdg(&mut storage, "edge_resave_proj", &pdg).unwrap();
    set_sql_trace(storage.conn(), false);
    drop(_harness);

    let inserted = TRACE_COUNTS.edge.load(Ordering::Relaxed);
    let deleted = TRACE_COUNTS.edge_delete.load(Ordering::Relaxed);
    assert_eq!(
        inserted, 0,
        "unchanged resave emitted {inserted} edge INSERT statements (expected 0)"
    );
    assert_eq!(
        deleted, 0,
        "unchanged resave emitted {deleted} edge DELETE statements (expected 0)"
    );

    let loaded = load_pdg(&storage, "edge_resave_proj").unwrap();
    assert_eq!(loaded.node_count(), 1_000);
    assert_eq!(loaded.edge_count(), 1_000);
}

#[test]
fn test_resave_edge_delta_persists_exactly_the_delta() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let mut pdg = create_large_pdg(100, 99);
    save_pdg(&mut storage, "edge_delta_proj", &pdg).unwrap();

    // Delta: flip one edge's call_count, drop the LAST edge, add one
    // node+edge. (Legacy sqlite3_trace expands bound values into the SQL
    // text, so row counts are asserted through persisted DB state rather
    // than statement text.)
    let first_edge = pdg.edge_indices().next().expect("pdg has edges");
    pdg.graph[first_edge].metadata.call_count = Some(42);
    let last_edge = pdg.edge_indices().last().expect("pdg has edges");
    let (last_from, last_to) = pdg.edge_endpoints(last_edge).expect("endpoints");
    let removed_pair = (
        pdg.get_node(last_from).expect("node").id.clone(),
        pdg.get_node(last_to).expect("node").id.clone(),
    );
    pdg.remove_edge(last_edge);
    let extra = pdg.add_node(PDGNode {
        id: "src/main.rs:extra".to_string(),
        node_type: PDGNodeType::Function,
        name: "extra".to_string(),
        file_path: Arc::from("src/main.rs"),
        byte_range: (9_000, 9_010),
        complexity: 1,
        language: "rust".to_string(),
    });
    let any_node = pdg.node_indices().next().expect("pdg has nodes");
    pdg.add_edge(
        any_node,
        extra,
        PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata {
                call_count: Some(7),
                variable_name: None,
                confidence: None,
                channel: None,
                position: None,
            },
        },
    );

    save_pdg(&mut storage, "edge_delta_proj", &pdg).unwrap();

    // Persisted state reflects exactly the delta:
    let loaded = load_pdg(&storage, "edge_delta_proj").unwrap();
    assert_eq!(loaded.node_count(), 101);
    // 99 - 1 removed + 1 added.
    assert_eq!(loaded.edge_count(), 99);
    // The changed edge's metadata round-tripped.
    let changed = loaded
        .edge_indices()
        .map(|edge| loaded.get_edge(edge).expect("edge"))
        .filter(|edge| edge.metadata.call_count == Some(42))
        .count();
    assert_eq!(changed, 1, "changed edge metadata must persist");
    // The new edge exists with its distinctive call_count.
    let added = loaded
        .edge_indices()
        .map(|edge| loaded.get_edge(edge).expect("edge"))
        .filter(|edge| edge.metadata.call_count == Some(7))
        .count();
    assert_eq!(added, 1, "new edge must persist");
    // The removed edge is gone: no edge connects the removed pair.
    let (from_id, to_id) = &removed_pair;
    let from_loaded = loaded.find_by_id(from_id).expect("from node survived");
    let to_loaded = loaded.find_by_id(to_id).expect("to node survived");
    let still_connected = loaded.edge_indices().any(|edge| {
        let (source, target) = loaded.edge_endpoints(edge).expect("endpoints");
        (source == from_loaded && target == to_loaded)
            || (source == to_loaded && target == from_loaded)
    });
    assert!(!still_connected, "removed edge must not survive the resave");
}

#[test]
fn test_resave_after_node_removal_cleans_its_edges() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    // Path graph 0→1→...→49: node 0 participates in exactly ONE edge.
    let mut pdg = create_large_pdg(50, 49);
    save_pdg(&mut storage, "node_removal_proj", &pdg).unwrap();

    let victim = pdg.node_indices().next().expect("pdg has nodes");
    let victim_id = pdg.get_node(victim).expect("node").id.clone();
    pdg.remove_node(victim);

    save_pdg(&mut storage, "node_removal_proj", &pdg).unwrap();

    let loaded = load_pdg(&storage, "node_removal_proj").unwrap();
    assert_eq!(loaded.node_count(), 49);
    assert_eq!(
        loaded.edge_count(),
        48,
        "node 0's single edge must be removed with it (49 - 1)"
    );
    assert!(loaded.find_by_id(&victim_id).is_none());
}

#[test]
fn test_save_pdg_bulk_rebuild_on_major_edge_churn() {
    // When most edges churn (different node identities), the diff takes
    // the bulk subquery DELETE + full reinsert path. Correctness must
    // match the row-by-row path.
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    let before = create_large_pdg(2_000, 1_999);
    save_pdg(&mut storage, "churn_proj", &before).unwrap();

    // Completely different node ids (different file path) ⇒ zero shared
    // edges ⇒ stale fraction 100% ⇒ bulk path.
    let after = {
        let mut pdg = ProgramDependenceGraph::new();
        let mut ids = Vec::new();
        for i in 0..500 {
            ids.push(pdg.add_node(PDGNode {
                id: format!("src/other.rs:func{i}"),
                node_type: PDGNodeType::Function,
                name: format!("func{i}"),
                file_path: Arc::from("src/other.rs"),
                byte_range: (i * 10, i * 10 + 8),
                complexity: (i % 5) as u32,
                language: "rust".to_string(),
            }));
        }
        for i in 0..499 {
            pdg.add_edge(
                ids[i],
                ids[i + 1],
                PDGEdge {
                    edge_type: PDGEdgeType::Call,
                    metadata: PDGEdgeMetadata {
                        call_count: Some(1),
                        variable_name: None,
                        confidence: None,
                        channel: None,
                        position: None,
                    },
                },
            );
        }
        pdg
    };
    save_pdg(&mut storage, "churn_proj", &after).unwrap();

    let loaded = load_pdg(&storage, "churn_proj").unwrap();
    assert_eq!(
        loaded.node_count(),
        500,
        "old nodes must be pruned on churn"
    );
    assert_eq!(loaded.edge_count(), 499);
    assert!(
        loaded.find_by_symbol("src/other.rs:func0").is_some(),
        "new graph must fully replace the old one"
    );
}

#[test]
fn test_batch_delete_equivalent_to_single() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    // Build a PDG with nodes across 5 files
    let mut pdg = ProgramDependenceGraph::new();
    let files = ["a.rs", "b.rs", "c.rs", "d.rs", "e.rs"];
    let mut node_ids = Vec::new();
    for (fi, file) in files.iter().enumerate() {
        for ni in 0..3 {
            let id = pdg.add_node(PDGNode {
                id: format!("{file}:func{ni}"),
                node_type: PDGNodeType::Function,
                name: format!("func_{fi}_{ni}"),
                file_path: Arc::from(*file),
                byte_range: (ni * 10, ni * 10 + 5),
                complexity: ni as u32,
                language: "rust".to_string(),
            });
            node_ids.push(id);
        }
    }
    // Add some edges between nodes in different files
    for i in 0..(node_ids.len() - 1) {
        pdg.add_edge(
            node_ids[i],
            node_ids[i + 1],
            PDGEdge {
                edge_type: PDGEdgeType::Call,
                metadata: PDGEdgeMetadata {
                    call_count: Some(1),
                    variable_name: None,
                    confidence: None,
                    channel: None,
                    position: None,
                },
            },
        );
    }
    save_pdg(&mut storage, "proj_batch_del", &pdg).unwrap();

    // Delete 3 files via single calls (a.rs, b.rs, c.rs)
    delete_file_data(&mut storage, "proj_batch_del", "a.rs").unwrap();
    delete_file_data(&mut storage, "proj_batch_del", "b.rs").unwrap();
    delete_file_data(&mut storage, "proj_batch_del", "c.rs").unwrap();

    let loaded_single = load_pdg(&storage, "proj_batch_del").unwrap();
    let single_nodes = loaded_single.node_count();
    let single_edges = loaded_single.edge_count();

    // Now test batch delete on a fresh copy
    let temp_file2 = NamedTempFile::new().unwrap();
    let mut storage2 = Storage::open(temp_file2.path()).unwrap();
    save_pdg(&mut storage2, "proj_batch_del", &pdg).unwrap();

    let tx = storage2.conn_mut().transaction().unwrap();
    delete_files_data_tx(
        &tx,
        "proj_batch_del",
        &["a.rs".to_string(), "b.rs".to_string(), "c.rs".to_string()],
    )
    .unwrap();
    tx.commit().unwrap();

    let loaded_batch = load_pdg(&storage2, "proj_batch_del").unwrap();
    assert_eq!(loaded_batch.node_count(), single_nodes);
    assert_eq!(loaded_batch.edge_count(), single_edges);
    // 5 files * 3 nodes each = 15 total, deleted 3 files * 3 = 9, expect 6 remaining
    assert_eq!(single_nodes, 6);
}

#[test]
fn test_batch_update_equivalent_to_single() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    // Update 10 indexed_files via single calls
    for i in 0..10 {
        update_indexed_file(
            &mut storage,
            "proj_batch_upd",
            &format!("src/file{i}.rs"),
            &format!("hash{i}"),
        )
        .unwrap();
    }
    let single_files = get_indexed_files(&storage, "proj_batch_upd").unwrap();

    // Batch update on a fresh storage
    let temp_file2 = NamedTempFile::new().unwrap();
    let mut storage2 = Storage::open(temp_file2.path()).unwrap();

    let files: Vec<(String, String)> = (0..10)
        .map(|i| (format!("src/file{i}.rs"), format!("hash{i}")))
        .collect();
    let tx = storage2.conn_mut().transaction().unwrap();
    update_indexed_files_tx(&tx, "proj_batch_upd", &files).unwrap();
    tx.commit().unwrap();

    let batch_files = get_indexed_files(&storage2, "proj_batch_upd").unwrap();

    assert_eq!(batch_files.len(), single_files.len());
    for (path, hash) in &single_files {
        assert_eq!(batch_files.get(path), Some(hash));
    }
}

#[test]
fn test_pdg_roundtrip_preserves_data() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    // Create PDG with 50 nodes, 49 edges across 10 files
    let mut pdg = ProgramDependenceGraph::new();
    let files: Vec<String> = (0..10).map(|i| format!("src/mod{i}.rs")).collect();
    let mut node_ids = Vec::new();
    for i in 0..50 {
        let file = &files[i % 10];
        let id = pdg.add_node(PDGNode {
            id: format!("{file}:func{i}"),
            node_type: PDGNodeType::Function,
            name: format!("func{i}"),
            file_path: Arc::from(file.as_str()),
            byte_range: (i * 20, i * 20 + 10),
            complexity: (i % 5) as u32,
            language: "rust".to_string(),
        });
        node_ids.push(id);
    }
    for i in 0..49 {
        pdg.add_edge(
            node_ids[i],
            node_ids[i + 1],
            PDGEdge {
                edge_type: PDGEdgeType::Call,
                metadata: PDGEdgeMetadata {
                    call_count: Some(i + 1),
                    variable_name: None,
                    confidence: Some(0.9),
                    channel: None,
                    position: None,
                },
            },
        );
    }

    save_pdg(&mut storage, "proj_roundtrip", &pdg).unwrap();
    let loaded = load_pdg(&storage, "proj_roundtrip").unwrap();

    assert_eq!(loaded.node_count(), 50);
    assert_eq!(loaded.edge_count(), 49);

    // Verify a sampling of nodes survived intact
    for i in [0, 10, 25, 49] {
        let file = &files[i % 10];
        let sym = format!("{file}:func{i}");
        assert!(loaded.find_by_symbol(&sym).is_some(), "node {sym} missing");
    }
}

#[test]
fn test_incremental_refresh_no_corruption() {
    let temp_file = NamedTempFile::new().unwrap();
    let mut storage = Storage::open(temp_file.path()).unwrap();

    // Phase 1: Add file_a with 2 nodes
    let mut pdg_a = ProgramDependenceGraph::new();
    let a1 = pdg_a.add_node(PDGNode {
        id: "a.rs:alpha".to_string(),
        node_type: PDGNodeType::Function,
        name: "alpha".to_string(),
        file_path: Arc::from("a.rs"),
        byte_range: (0, 50),
        complexity: 1,
        language: "rust".to_string(),
    });
    let a2 = pdg_a.add_node(PDGNode {
        id: "a.rs:beta".to_string(),
        node_type: PDGNodeType::Function,
        name: "beta".to_string(),
        file_path: Arc::from("a.rs"),
        byte_range: (50, 100),
        complexity: 1,
        language: "rust".to_string(),
    });
    pdg_a.add_edge(
        a1,
        a2,
        PDGEdge {
            edge_type: PDGEdgeType::Call,
            metadata: PDGEdgeMetadata {
                call_count: Some(1),
                variable_name: None,
                confidence: None,
                channel: None,
                position: None,
            },
        },
    );
    save_pdg(&mut storage, "proj_refresh", &pdg_a).unwrap();
    update_indexed_file(&mut storage, "proj_refresh", "a.rs", "hash_a_v1").unwrap();

    // Phase 2: Delete a.rs, then re-add with modified content
    delete_file_data(&mut storage, "proj_refresh", "a.rs").unwrap();

    let mut pdg_a2 = ProgramDependenceGraph::new();
    pdg_a2.add_node(PDGNode {
        id: "a.rs:alpha".to_string(),
        node_type: PDGNodeType::Function,
        name: "alpha".to_string(),
        file_path: Arc::from("a.rs"),
        byte_range: (0, 60),
        complexity: 2,
        language: "rust".to_string(),
    });
    pdg_a2.add_node(PDGNode {
        id: "a.rs:gamma".to_string(),
        node_type: PDGNodeType::Function,
        name: "gamma".to_string(),
        file_path: Arc::from("a.rs"),
        byte_range: (60, 120),
        complexity: 3,
        language: "rust".to_string(),
    });

    // Use transaction-aware delete + save for incremental update
    let tx = storage.conn_mut().transaction().unwrap();
    delete_file_data_tx(&tx, "proj_refresh", "a.rs").unwrap();
    tx.commit().unwrap();

    save_pdg(&mut storage, "proj_refresh", &pdg_a2).unwrap();
    update_indexed_file(&mut storage, "proj_refresh", "a.rs", "hash_a_v2").unwrap();

    // Verify: no orphaned nodes/edges, correct data
    let loaded = load_pdg(&storage, "proj_refresh").unwrap();
    assert_eq!(loaded.node_count(), 2);
    assert_eq!(loaded.edge_count(), 0); // pdg_a2 has no edges

    // Verify the right nodes are present
    assert!(loaded.find_by_symbol("a.rs:alpha").is_some());
    assert!(loaded.find_by_symbol("a.rs:gamma").is_some());
    assert!(loaded.find_by_symbol("a.rs:beta").is_none()); // old node should be gone

    // Verify indexed_files is correct
    let indexed = get_indexed_files(&storage, "proj_refresh").unwrap();
    assert_eq!(indexed.get("a.rs"), Some(&"hash_a_v2".to_string()));
}
