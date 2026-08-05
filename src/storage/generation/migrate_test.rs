//! Tests for the one-time legacy → CAS generation-store migration
//! (WS4 Task 10, VAL-MIGRATE-001..005).
//!
//! Fixtures build a legacy full-copy `.leindex/` layout: `CURRENT` ->
//! `generations/<N>/{leindex.db, embeddings.bin, ...}` with no CAS, plus
//! heap-mirrored top-level artifacts and an unbounded `jobs/` directory.

use std::collections::HashSet;
use std::fs::{self, File};
use std::path::Path;

use rusqlite::{Connection, params};

use crate::storage::cas::CasStore;
use crate::storage::generation::lease::read_current_generation;

use super::{
    MigrationConfig, MigrationReport, dir_total_bytes, is_legacy_full_copy_layout,
    is_migrated_store, migrate_legacy_store, read_generation_manifest,
};

/// Migration config for tests: silent, no crash hook, footprint goal capped.
fn test_cfg() -> MigrationConfig {
    MigrationConfig {
        emit_backup_warning: false,
        stop_after_publish: false,
        total_footprint_goal_bytes: Some(200 * 1024 * 1024),
        ..Default::default()
    }
}

/// Create a legacy catalog DB with two nodes and one edge, parameterized by
/// `variant` so distinct generations get distinct (dedupable) content.
fn write_catalog(path: &Path, variant: u64) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE intel_nodes (
            id INTEGER PRIMARY KEY,
            symbol_name TEXT NOT NULL,
            node_type TEXT NOT NULL,
            file_path TEXT NOT NULL,
            complexity INTEGER DEFAULT 1
        );
        CREATE TABLE intel_edges (
            caller_id INTEGER NOT NULL,
            callee_id INTEGER NOT NULL,
            edge_type TEXT NOT NULL,
            metadata TEXT,
            PRIMARY KEY (caller_id, callee_id, edge_type)
        );",
    )
    .unwrap();
    conn.execute(
        "INSERT INTO intel_nodes (id, symbol_name, node_type, file_path, complexity)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            1,
            format!("fn_variant_{variant}"),
            "function",
            "/src/a.rs",
            3
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO intel_nodes (id, symbol_name, node_type, file_path, complexity)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            2,
            format!("struct_variant_{variant}"),
            "struct",
            "/src/b.rs",
            5
        ],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO intel_edges (caller_id, callee_id, edge_type, metadata)
         VALUES (1, 2, 'call', NULL)",
        [],
    )
    .unwrap();
}

/// Write a legacy `LIEE` mmap embedding file (header + id tables + f32 matrix)
/// whose matrix values depend on `variant`.
fn write_liee(path: &Path, variant: u64) {
    let ids = [format!("node_{variant}_a"), format!("node_{variant}_b")];
    let dim: u32 = 8;
    let node_count = ids.len() as u32;
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"LIEE");
    buf.extend_from_slice(&1u32.to_le_bytes());
    buf.extend_from_slice(&node_count.to_le_bytes());
    buf.extend_from_slice(&dim.to_le_bytes());
    let mut id_bytes = Vec::new();
    for id in &ids {
        buf.extend_from_slice(&(id_bytes.len() as u64).to_le_bytes());
        id_bytes.extend_from_slice(id.as_bytes());
    }
    for id in &ids {
        buf.extend_from_slice(&(id.len() as u32).to_le_bytes());
    }
    buf.extend_from_slice(&id_bytes);
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
    for i in 0..node_count {
        for d in 0..dim {
            let v = (variant as f32) + (i as f32) * 100.0 + (d as f32);
            buf.extend_from_slice(&v.to_le_bytes());
        }
    }
    fs::write(path, &buf).unwrap();
}

/// Write the full-copy artifacts of one legacy generation directory. When
/// `shared` is true, `variant` is pinned to the first generation number so
/// later generations get byte-identical content (the dedup scenario).
fn write_generation_dir(root: &Path, g: u64, shared: bool) {
    let variant = if shared && g > 1 { 1 } else { g };
    let gd = root.join("generations").join(g.to_string());
    fs::create_dir_all(&gd).unwrap();
    write_catalog(&gd.join("leindex.db"), variant);
    write_liee(&gd.join("embeddings.bin"), variant);
    fs::write(
        gd.join("neural_embeddings.bin"),
        format!("neural-v{variant}"),
    )
    .unwrap();
    fs::write(
        gd.join("search_snapshot.bin"),
        format!("snapshot-v{variant}"),
    )
    .unwrap();
    fs::write(gd.join("tfidf_embedder.bin"), format!("tfidf-v{variant}")).unwrap();
    fs::write(gd.join("index_stats.json"), "{\"generation\":0}").unwrap();
}

/// Build a legacy store: generations 1..3 (2 and 3 byte-identical when
/// `dedup_shared`), `CURRENT -> 3`, heap-mirrored top-level artifacts, and a
/// small cache dir.
fn build_legacy_store(root: &Path, dedup_shared: bool) {
    for g in [1u64, 2, 3] {
        write_generation_dir(root, g, dedup_shared);
    }
    fs::write(root.join("CURRENT"), "3\n").unwrap();
    // Heap-mirrored top-level full-copy artifacts (redundant with the gens).
    write_catalog(&root.join("leindex.db"), 3);
    write_liee(&root.join("embeddings.bin"), 3);
    fs::write(root.join("neural_embeddings.bin"), "top-neural").unwrap();
    fs::write(root.join("search_snapshot.bin"), "top-snapshot").unwrap();
    fs::write(root.join("tfidf_embedder.bin"), "top-tfidf").unwrap();
    fs::create_dir_all(root.join(".cache")).unwrap();
    fs::write(root.join(".cache").join("x"), "cached").unwrap();
}

/// Build `count` completed job dirs named `start..start+count`, each with an
/// apparent-size filler of `bytes` and all four `.complete` markers.
fn build_completed_jobs(root: &Path, start: u64, count: u64, bytes: u64) {
    for n in start..start + count {
        let jd = root.join("jobs").join(n.to_string());
        fs::create_dir_all(&jd).unwrap();
        for marker in [
            "parse.complete",
            "pdg.complete",
            "lexical.complete",
            "neural.complete",
        ] {
            fs::write(jd.join(marker), "done").unwrap();
        }
        File::create(jd.join("filler.bin"))
            .unwrap()
            .set_len(bytes)
            .unwrap();
    }
}

/// Build `count` in-progress job dirs (state.json mid-pipeline, no markers).
fn build_in_progress_jobs(root: &Path, start: u64, count: u64, bytes: u64) {
    for n in start..start + count {
        let jd = root.join("jobs").join(n.to_string());
        fs::create_dir_all(&jd).unwrap();
        fs::write(
            jd.join("state.json"),
            "{\"last_reusable_phase\":\"lexical\"}",
        )
        .unwrap();
        File::create(jd.join("filler.bin"))
            .unwrap()
            .set_len(bytes)
            .unwrap();
    }
}

#[test]
fn test_legacy_to_cas_migration() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_legacy_store(root, true);

    assert!(is_legacy_full_copy_layout(root));
    let report = migrate_legacy_store(root, &test_cfg()).unwrap();

    // Conversion produced manifests + CAS for current and previous generations.
    assert!(report.detected_legacy, "legacy layout must be detected");
    assert!(report.migrated, "migration must run on a legacy store");
    assert_eq!(report.current_generation, Some(3));
    assert_eq!(report.previous_generation, Some(2));
    assert_eq!(report.generations_converted, 2);
    assert_eq!(
        report.generations_deleted, 1,
        "non-retained gen 1 must be swept"
    );

    // CURRENT points at a manifest-format generation; legacy detection clears.
    assert_eq!(read_current_generation(root), Some(3));
    assert!(is_migrated_store(root));
    assert!(!is_legacy_full_copy_layout(root));

    // The current + previous manifests reference real CAS blobs (LIDX-GEN1).
    let manifest_cur = read_generation_manifest(root, 3).unwrap();
    let manifest_prev = read_generation_manifest(root, 2).unwrap();
    assert_eq!(manifest_cur.layer_hashes().len(), 5);
    assert_eq!(manifest_prev.layer_hashes().len(), 5);
    let store = CasStore::open(root.join("cas")).unwrap();
    let mut unique: HashSet<[u8; 32]> = HashSet::new();
    for h in manifest_cur.layer_hashes() {
        assert!(store.exists(&h), "manifest layer hash missing from CAS");
        unique.insert(h);
    }
    for h in manifest_prev.layer_hashes() {
        assert!(
            store.exists(&h),
            "previous manifest layer hash missing from CAS"
        );
        unique.insert(h);
    }

    // Dedup: generations 2 and 3 are byte-identical, so their 10 layer
    // payloads collapse to 5 unique blobs. Anything else in CAS would be an
    // orphan that GC should have removed.
    assert_eq!(
        unique.len(),
        5,
        "byte-identical generations must dedup to 5 blobs"
    );
    assert_eq!(
        store.blob_count().unwrap(),
        unique.len(),
        "CAS must hold exactly the retained manifests' pinned blobs"
    );
    assert_eq!(report.cas_blob_count, unique.len());

    // Legacy full-copy files are gone from retained generations and the root.
    assert!(!root.join("generations/1").exists());
    assert!(!root.join("generations/2/leindex.db").exists());
    assert!(!root.join("generations/3/leindex.db").exists());
    assert!(!root.join("generations/3/embeddings.bin").exists());
    assert!(!root.join("leindex.db").exists());
    assert!(!root.join("embeddings.bin").exists());
    assert!(!root.join("search_snapshot.bin").exists());

    // Footprint stays within the configured goal.
    assert!(
        report.total_bytes_after <= test_cfg().total_footprint_goal_bytes.unwrap(),
        "post-migration footprint {} exceeds the {} MiB goal",
        report.total_bytes_after,
        test_cfg().total_footprint_goal_bytes.unwrap() / (1024 * 1024)
    );
}

#[test]
fn test_migration_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_legacy_store(root, true);

    // First run: full conversion.
    let r1 = migrate_legacy_store(root, &test_cfg()).unwrap();
    assert!(r1.migrated);
    let store = CasStore::open(root.join("cas")).unwrap();
    let blob_count = store.blob_count().unwrap();
    let cas_bytes = r1.cas_bytes;
    let current_before = read_current_generation(root);
    let manifest_before = fs::read(root.join("generations/3/manifest")).unwrap();

    // Second run on the migrated store: zero writes, zero changes.
    let r2 = migrate_legacy_store(root, &test_cfg()).unwrap();
    assert!(
        !r2.detected_legacy,
        "migrated store must not detect legacy layout"
    );
    assert!(!r2.migrated, "second run must not re-convert");
    assert!(r2.was_noop(), "second run must be a confirmed no-op");
    assert_eq!(r2.generations_converted, 0);
    assert_eq!(
        r2.cas_blob_count, blob_count,
        "no duplicate blobs on re-run"
    );
    assert_eq!(r2.cas_bytes, cas_bytes, "CAS bytes unchanged on re-run");
    assert_eq!(read_current_generation(root), current_before);
    assert_eq!(
        fs::read(root.join("generations/3/manifest")).unwrap(),
        manifest_before,
        "manifest must not be rewritten on re-run"
    );
}

#[test]
fn test_migration_crash_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_legacy_store(root, true);
    build_completed_jobs(root, 10, 4, 64 * 1024);

    // Simulate a crash mid-migration *before* the CURRENT swap: a partially
    // populated CAS with blobs that belong to no retained manifest.
    let cas = CasStore::open(root.join("cas")).unwrap();
    cas.put(b"orphaned blob from an interrupted run").unwrap();
    assert!(cas.exists(&crate::storage::cas::blob::blob_hash(
        b"orphaned blob from an interrupted run"
    )));
    drop(cas);

    // Restart: CURRENT still points at the legacy full-copy generation and the
    // legacy data is fully readable.
    assert_eq!(read_current_generation(root), Some(3));
    assert!(is_legacy_full_copy_layout(root));
    assert!(root.join("generations/3/leindex.db").is_file());

    // Resume: the migration completes and GCs the orphaned blob.
    let report = migrate_legacy_store(root, &test_cfg()).unwrap();
    assert!(report.migrated);
    assert!(is_migrated_store(root));
    let store = CasStore::open(root.join("cas")).unwrap();
    let mut pinned: HashSet<[u8; 32]> = HashSet::new();
    for h in read_generation_manifest(root, 3).unwrap().layer_hashes() {
        pinned.insert(h);
    }
    for h in read_generation_manifest(root, 2).unwrap().layer_hashes() {
        pinned.insert(h);
    }
    for h in store.stored_hashes().unwrap() {
        assert!(
            pinned.contains(&h),
            "every CAS blob after resume must be pinned by a retained manifest"
        );
    }

    // Simulate a crash *after* the CURRENT swap but before the destructive
    // cleanup (the exact resume boundary): stop_after_publish halts with the
    // store migrated and the legacy full-copy files still present.
    let tmp2 = tempfile::tempdir().unwrap();
    let root2 = tmp2.path();
    build_legacy_store(root2, true);
    build_completed_jobs(root2, 10, 4, 64 * 1024);
    let mut crash_cfg = test_cfg();
    crash_cfg.stop_after_publish = true;
    let r_crash = migrate_legacy_store(root2, &crash_cfg).unwrap();
    assert!(r_crash.migrated);
    assert!(is_migrated_store(root2));
    assert!(
        root2.join("generations/3/leindex.db").is_file(),
        "crash before cleanup must leave legacy full-copy intact (reads still work)"
    );
    assert!(
        root2.join("jobs/10").is_dir(),
        "crash before cleanup must leave jobs intact"
    );

    // Restart resumes from the last CURRENT swap and completes the cleanup.
    let r_resume = migrate_legacy_store(root2, &test_cfg()).unwrap();
    assert!(!r_resume.detected_legacy);
    assert!(
        !r_resume.migrated,
        "resume must not re-convert already-migrated gens"
    );
    assert!(!root2.join("generations/3/leindex.db").exists());
    assert!(
        !root2.join("jobs/10").exists(),
        "completed jobs swept on resume"
    );
    assert_eq!(read_current_generation(root2), Some(3));
}

#[test]
fn test_migration_job_bounding() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_legacy_store(root, true);

    // 40 completed jobs (6 MiB each = 240 MiB) + 10 in-progress jobs
    // (6 MiB each = 60 MiB): the 50-job / 300 MiB accumulation observed on the
    // real repository, minus completed-job-for-current-gen special casing.
    build_completed_jobs(root, 10, 40, 6 * 1024 * 1024);
    build_in_progress_jobs(root, 200, 10, 6 * 1024 * 1024);

    let mut cfg = test_cfg();
    cfg.job_bytes_max = 128 * 1024 * 1024;
    cfg.total_footprint_goal_bytes = Some(256 * 1024 * 1024);
    let report = migrate_legacy_store(root, &cfg).unwrap();

    // Completed jobs are deleted immediately (zero resume value).
    assert_eq!(report.jobs_completed_deleted, 40);
    assert!(!root.join("jobs/10").exists());
    assert!(!root.join("jobs/49").exists());

    // The jobs directory is byte-bounded after migration.
    let jobs_bytes = dir_total_bytes(&root.join("jobs"));
    assert!(
        jobs_bytes <= cfg.job_bytes_max,
        "jobs after migration: {jobs_bytes} bytes, cap {}",
        cfg.job_bytes_max
    );
    assert!(report.job_bytes_remaining <= cfg.job_bytes_max);
    assert!(
        report.job_bytes_reclaimed >= 240 * 1024 * 1024,
        "completed-job reclamation must account for 240 MiB, got {}",
        report.job_bytes_reclaimed
    );
}

#[test]
fn test_migration_footprint_goal_repo_shaped() {
    // A fixture shaped like this repository's real `.leindex/`: generations
    // are small, but heap-mirrored top-level artifacts and the job
    // accumulation dominate (~2.6 GiB apparent). Migration must land the
    // whole store at or below the 200 MiB footprint goal.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_legacy_store(root, false);

    // Redundant top-level full-copy artifacts (not captured by CAS).
    File::create(root.join("search_snapshot.bin"))
        .unwrap()
        .set_len(1200 * 1024 * 1024)
        .unwrap();
    File::create(root.join("tfidf_embedder.bin"))
        .unwrap()
        .set_len(600 * 1024 * 1024)
        .unwrap();
    // 20 completed jobs × 40 MiB = 800 MiB.
    build_completed_jobs(root, 10, 20, 40 * 1024 * 1024);

    let before = dir_total_bytes(root);
    assert!(
        before >= 2 * 1024 * 1024 * 1024,
        "fixture must be >= 2 GiB before migration, was {before}"
    );

    let cfg = test_cfg(); // 200 MiB footprint goal, 128 MiB job cap
    let report = migrate_legacy_store(root, &cfg).unwrap();
    assert!(report.migrated);
    assert!(
        report.total_bytes_before >= 2 * 1024 * 1024 * 1024,
        "before bytes recorded: {}",
        report.total_bytes_before
    );
    assert!(
        report.total_bytes_after <= cfg.total_footprint_goal_bytes.unwrap(),
        "post-migration footprint {} MiB exceeds the {} MiB goal",
        report.total_bytes_after / (1024 * 1024),
        cfg.total_footprint_goal_bytes.unwrap() / (1024 * 1024)
    );
    assert_eq!(report.generations_deleted, 1);
    assert_eq!(report.jobs_completed_deleted, 20);
    assert!(!root.join("search_snapshot.bin").exists());
    assert!(!root.join("leindex.db").exists());
}

#[test]
fn test_migration_error_on_empty_store() {
    // A store with no CURRENT at all is neither legacy nor migrated: the
    // migration must be a no-op, not an error.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("generations")).unwrap();
    let report = migrate_legacy_store(root, &test_cfg()).unwrap();
    assert!(report.was_noop());
    assert!(!report.migrated);
}

#[test]
fn test_migration_noop_on_partial_generation_dir() {
    // CURRENT exists but the pointed-to generation directory holds neither a
    // legacy full copy nor a manifest (a partial/ambiguous store): the
    // migration must be a safe no-op rather than half-convert or error.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    fs::create_dir_all(root.join("generations/5")).unwrap();
    fs::write(root.join("CURRENT"), "5\n").unwrap();
    let report = migrate_legacy_store(root, &test_cfg()).unwrap();
    assert!(!report.detected_legacy);
    assert!(!report.migrated);
    assert!(report.was_noop());
}

/// Sanity-check the report's accounting invariants used by the CLI output.
#[test]
fn test_migration_report_accounting() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    build_legacy_store(root, true);
    let report: MigrationReport = migrate_legacy_store(root, &test_cfg()).unwrap();
    assert!(report.total_bytes_before > 0);
    assert!(report.total_bytes_after > 0);
    assert!(report.total_bytes_before > report.total_bytes_after);
    assert!(
        !report.blob_hashes.is_empty(),
        "current manifest hashes recorded"
    );
    assert_eq!(report.blob_hashes.len(), 5);
    assert!(report.cas_bytes > 0);
}
