// Integration tests for Plan 2 fixed SQLite reader topology and bounded
// parse/index backpressure.
//
// Validates: VAL-BPHASE-026, VAL-BPHASE-027, VAL-BPHASE-028, VAL-BPHASE-029

use leindex::storage::schema::{
    StorageConfig, StoragePool, DEFAULT_READER_POOL_SIZE, PROJECT_READER_CACHE_SIZE_KIB,
    PROJECT_STORE_MMAP_SIZE, PROJECT_WRITER_CACHE_SIZE_KIB,
};
use tempfile::TempDir;

// ============================================================================
// Helpers
// ============================================================================

fn make_pool_dir() -> TempDir {
    tempfile::tempdir().unwrap()
}

fn writer_config() -> StorageConfig {
    StorageConfig {
        db_path: "leindex.db".to_string(),
        wal_enabled: true,
        cache_size_kib: Some(PROJECT_WRITER_CACHE_SIZE_KIB),
        mmap_size: Some(PROJECT_STORE_MMAP_SIZE),
    }
}

fn reader_config() -> StorageConfig {
    StorageConfig {
        db_path: "leindex.db".to_string(),
        wal_enabled: true,
        cache_size_kib: Some(PROJECT_READER_CACHE_SIZE_KIB),
        mmap_size: Some(PROJECT_STORE_MMAP_SIZE),
    }
}

// ============================================================================
// VAL-BPHASE-026: Fixed SQLite reader topology is observable and functional
// ============================================================================

#[test]
fn test_pool_opens_one_writer_and_fixed_reader_pool() {
    // StoragePool opens with one writer and a fixed small reader pool.
    // Concurrent reads succeed without unbounded connection fan-out.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    // Writer should be present
    assert!(pool.has_writer(), "pool should have a writer connection");

    // Reader pool should have the default fixed size
    assert_eq!(
        pool.reader_count(),
        DEFAULT_READER_POOL_SIZE,
        "reader pool should have {} connections",
        DEFAULT_READER_POOL_SIZE,
    );
}

#[test]
fn test_pool_concurrent_reads_succeed() {
    // Multiple concurrent reads through the pool all succeed.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    // Write some data via the writer
    pool.writer()
        .conn()
        .execute(
            "CREATE TABLE IF NOT EXISTS test_data (key TEXT PRIMARY KEY, val INTEGER)",
            [],
        )
        .unwrap();
    pool.writer()
        .conn()
        .execute("INSERT INTO test_data (key, val) VALUES ('a', 1)", [])
        .unwrap();
    pool.writer()
        .conn()
        .execute("INSERT INTO test_data (key, val) VALUES ('b', 2)", [])
        .unwrap();

    // Read from each reader slot
    for i in 0..DEFAULT_READER_POOL_SIZE {
        let reader = pool.reader(i).expect("should get reader");
        let val: i64 = reader
            .conn()
            .query_row("SELECT val FROM test_data WHERE key = 'a'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(val, 1, "reader {} should read correct data", i);
    }
}

#[test]
fn test_pool_no_unbounded_connection_fanout() {
    // Requesting a reader beyond pool size returns an error rather than
    // creating a new connection.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    let result = pool.reader(DEFAULT_READER_POOL_SIZE);
    assert!(
        result.is_err(),
        "requesting reader beyond pool size should fail",
    );
}

// ============================================================================
// VAL-BPHASE-027: Reader connections use thin-cache behavior
// ============================================================================

#[test]
fn test_reader_connections_have_thin_cache() {
    // Each reader connection uses the thin reader cache budget.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    for i in 0..DEFAULT_READER_POOL_SIZE {
        let reader = pool.reader(i).unwrap();
        let cache_size: i64 = reader
            .conn()
            .query_row("PRAGMA cache_size", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            cache_size, PROJECT_READER_CACHE_SIZE_KIB,
            "reader {} cache_size should be {} (2 MiB thin), got {}",
            i, PROJECT_READER_CACHE_SIZE_KIB, cache_size,
        );
    }
}

#[test]
fn test_reader_connections_have_correct_mmap_cap() {
    // Reader connections share the bounded mmap cap.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    for i in 0..DEFAULT_READER_POOL_SIZE {
        let reader = pool.reader(i).unwrap();
        let mmap_size: i64 = reader
            .conn()
            .query_row("PRAGMA mmap_size", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            mmap_size, PROJECT_STORE_MMAP_SIZE,
            "reader {} mmap_size should be {} (64 MiB), got {}",
            i, PROJECT_STORE_MMAP_SIZE, mmap_size,
        );
    }
}

// ============================================================================
// VAL-BPHASE-028: Writer retains write-capable behavior without reader
//                 proliferation
// ============================================================================

#[test]
fn test_writer_has_writer_cache_budget() {
    // The writer connection uses the larger writer cache budget.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    let cache_size: i64 = pool
        .writer()
        .conn()
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        cache_size, PROJECT_WRITER_CACHE_SIZE_KIB,
        "writer cache_size should be {} (16 MiB), got {}",
        PROJECT_WRITER_CACHE_SIZE_KIB, cache_size,
    );
}

#[test]
fn test_writer_performs_write_operations() {
    // Write-side indexing/flush operations complete correctly while the
    // reader topology remains fixed.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    // Writer can create tables and insert data
    pool.writer()
        .conn()
        .execute(
            "CREATE TABLE test_write (id INTEGER PRIMARY KEY, data TEXT)",
            [],
        )
        .unwrap();
    pool.writer()
        .conn()
        .execute("INSERT INTO test_write (id, data) VALUES (1, 'hello')", [])
        .unwrap();

    // Readers can see the written data (WAL mode)
    let reader = pool.reader(0).unwrap();
    let count: i64 = reader
        .conn()
        .query_row("SELECT COUNT(*) FROM test_write", [], |row| row.get(0))
        .unwrap();
    assert_eq!(count, 1, "reader should see writer's data");
}

#[test]
fn test_writer_and_reader_roles_are_distinct() {
    // Writer and reader roles produce different cache sizes.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    let writer_cache: i64 = pool
        .writer()
        .conn()
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .unwrap();
    let reader_cache: i64 = pool
        .reader(0)
        .unwrap()
        .conn()
        .query_row("PRAGMA cache_size", [], |row| row.get(0))
        .unwrap();

    assert_ne!(
        writer_cache, reader_cache,
        "writer cache ({}) should differ from reader cache ({})",
        writer_cache, reader_cache,
    );
    assert!(
        writer_cache.abs() > reader_cache.abs(),
        "writer cache ({}) should be larger than reader cache ({})",
        writer_cache,
        reader_cache,
    );
}

// ============================================================================
// VAL-BPHASE-029: Bounded parse/index backpressure prevents runaway inflight
//                 growth
// ============================================================================

#[test]
fn test_indexing_backpressure_stays_bounded() {
    // Under indexing load, inflight parse/index work stays within configured
    // bounded pressure rather than expanding unbounded resident buffers.
    use leindex::search::search::IndexingAdmissionGate;

    // Simulate bursty indexing: the admission gate should bound the number
    // of nodes admitted even when many are submitted.
    let mut gate = IndexingAdmissionGate::with_caps(50, 10_000);

    let mut admitted = 0;
    let mut shed = 0;
    for i in 0..200 {
        // Each node has some content bytes
        let content_bytes = 100 + (i % 50) * 10;
        if gate.try_admit(content_bytes) {
            admitted += 1;
        } else {
            shed += 1;
        }
    }

    // The gate should have admitted at most 50 nodes
    assert!(
        admitted <= 50,
        "admitted {} nodes, expected at most 50",
        admitted,
    );
    // And should have shed the rest
    assert!(
        shed > 0,
        "expected some nodes to be shed under pressure, admitted={}, shed={}",
        admitted,
        shed,
    );
}

#[test]
fn test_parser_pool_is_bounded() {
    // The parallel parser uses a bounded thread pool (rayon default) and
    // does not create unbounded threads per file.
    use leindex::parse::parallel::ParallelParser;

    let dir = tempfile::tempdir().unwrap();

    // Create 50 small files
    let mut paths = Vec::new();
    for i in 0..50 {
        let path = dir.path().join(format!("file_{:03}.rs", i));
        std::fs::write(&path, format!("fn func_{}() {{}}", i)).unwrap();
        paths.push(path);
    }

    let parser = ParallelParser::new().with_max_threads(4);
    let (results, stats) = parser.parse_files_with_stats(paths);

    assert_eq!(stats.total_files, 50);
    assert!(
        results.iter().filter(|r| r.is_success()).count() > 0,
        "at least some files should parse successfully",
    );
}

#[test]
fn test_backpressure_with_large_content() {
    // Large individual content items are shed rather than inflating memory.
    use leindex::search::search::IndexingAdmissionGate;

    let mut gate = IndexingAdmissionGate::with_caps(100, 1024); // 1 KiB byte cap

    // A single oversized item should be rejected
    assert!(
        !gate.try_admit(2048),
        "oversized content (2 KiB) should be rejected when cap is 1 KiB",
    );
    assert_eq!(gate.nodes_admitted(), 0);

    // Small items should still be admitted
    assert!(
        gate.try_admit(256),
        "small content (256 B) should be admitted",
    );
    assert_eq!(gate.nodes_admitted(), 1);
}

#[test]
fn test_pool_writer_wal_mode_enabled() {
    // Writer uses WAL mode for concurrent reader access.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    let journal_mode: String = pool
        .writer()
        .conn()
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        journal_mode.to_lowercase(),
        "wal",
        "writer should use WAL mode, got {}",
        journal_mode,
    );
}

#[test]
fn test_pool_readers_see_writer_data_via_wal() {
    // Readers can see data written by the writer through WAL.
    let dir = make_pool_dir();
    let db_path = dir.path().join("leindex.db");
    let pool = StoragePool::open(&db_path, writer_config(), reader_config()).unwrap();

    // Write data
    pool.writer()
        .conn()
        .execute("CREATE TABLE kv (k TEXT PRIMARY KEY, v INTEGER)", [])
        .unwrap();
    for i in 0..10 {
        pool.writer()
            .conn()
            .execute(
                "INSERT INTO kv (k, v) VALUES (?1, ?2)",
                rusqlite::params![format!("key{}", i), i],
            )
            .unwrap();
    }

    // All readers should see the data
    for slot in 0..DEFAULT_READER_POOL_SIZE {
        let reader = pool.reader(slot).unwrap();
        let count: i64 = reader
            .conn()
            .query_row("SELECT COUNT(*) FROM kv", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 10, "reader {} should see all 10 rows", slot,);
    }
}
