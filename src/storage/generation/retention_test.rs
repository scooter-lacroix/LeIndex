//! Tests for WS4 Task 9: byte-bounded retention.
//!
//! Validates:
//! - VAL-CAS-RET-001: Retention keeps current + previous + leased only
//! - VAL-CAS-RET-002: Completed jobs deleted immediately on publication
//! - VAL-CAS-RET-003: Jobs capped at 128 MiB per project (default)
//! - VAL-FOOTPRINT-004: Generations directory has at most 2 entries
//! - VAL-FOOTPRINT-005: Job directory total stays under 128 MiB after publish

use std::fs;
use std::path::Path;

use crate::storage::cas::CasStore;
use crate::storage::generation::lease::{CURRENT_FILE, GENERATIONS_DIR, MANIFEST_FILE};
use crate::storage::generation::manifest::{
    ALL_LAYER_KINDS, MANIFEST_VERSION, Manifest, ModelIdentity,
};
use crate::storage::generation::retention::{
    DEFAULT_JOB_BYTES_MAX, DEFAULT_MAX_GENERATIONS, RetentionConfig, retain_after_publish,
    retention_report,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a CAS store in a tempdir root.
fn make_store(root: &Path) -> CasStore {
    CasStore::open(root.join("cas")).expect("failed to open CAS store")
}

/// Write a valid manifest for `generation` with `layer_hashes` into `gens_dir`.
fn write_manifest(gens_dir: &Path, generation: u64, layer_hashes: &[[u8; 32]; 5]) {
    let gen_dir = gens_dir.join(generation.to_string());
    fs::create_dir_all(&gen_dir).unwrap();
    let mut layers = std::collections::HashMap::new();
    for (i, kind) in ALL_LAYER_KINDS.iter().enumerate() {
        layers.insert(*kind, layer_hashes[i]);
    }
    let manifest = Manifest {
        version: MANIFEST_VERSION,
        generation,
        model_identity: ModelIdentity {
            name: "test-model".into(),
            digest: "sha256:abc".into(),
            dimensions: 384,
        },
        graph_fingerprint: [0u8; 32],
        search_fingerprint: [0u8; 32],
        layers,
    };
    let bytes = manifest.to_bytes().unwrap();
    fs::write(gen_dir.join(MANIFEST_FILE), bytes).unwrap();
}

/// Stage 5 unique blobs for `generation` and return their hashes + data.
///
/// The payloads embed the generation number so that different generations
/// reference distinct CAS hashes (matching real publishes where each
/// generation's layer content differs). Use the same seed to share hashes.
fn stage_blobs(store: &CasStore, generation: u64) -> Vec<([u8; 32], Vec<u8>)> {
    let seed = generation.to_string();
    let data = vec![
        format!("db-layer-data-{seed}").into_bytes(),
        format!("tfidf-layer-data-{seed}").into_bytes(),
        format!("neural-layer-data-{seed}").into_bytes(),
        format!("pdg-layer-data-{seed}").into_bytes(),
        format!("symbols-layer-data-{seed}").into_bytes(),
    ];
    let mut out = Vec::new();
    for d in &data {
        let h = store.put(d).unwrap();
        out.push((h, d.clone()));
    }
    out
}

/// Create a generation directory structure under a tempdir.
struct TestEnv {
    _tmp: tempfile::TempDir,
    root: std::path::PathBuf,
    gens_dir: std::path::PathBuf,
    jobs_dir: std::path::PathBuf,
}

impl TestEnv {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let gens_dir = root.join(GENERATIONS_DIR);
        let jobs_dir = root.join("jobs");
        fs::create_dir_all(&gens_dir).unwrap();
        fs::create_dir_all(&jobs_dir).unwrap();
        TestEnv {
            _tmp: tmp,
            root,
            gens_dir,
            jobs_dir,
        }
    }

    fn write_current(&self, generation: u64) {
        fs::write(self.root.join(CURRENT_FILE), format!("{}\n", generation)).unwrap();
    }

    fn make_generation(&self, generation: u64) -> [[u8; 32]; 5] {
        let store = make_store(&self.root);
        let blobs = stage_blobs(&store, generation);
        let hashes: [[u8; 32]; 5] = [blobs[0].0, blobs[1].0, blobs[2].0, blobs[3].0, blobs[4].0];
        write_manifest(&self.gens_dir, generation, &hashes);
        hashes
    }

    /// Make a generation with provided blob hashes (for shared-blob testing).
    fn make_generation_with_hashes(&self, generation: u64, hashes: &[[u8; 32]; 5]) {
        write_manifest(&self.gens_dir, generation, hashes);
    }
}

/// Create a job directory with the given generation, size, and status.
fn make_job(
    jobs_dir: &Path,
    job_id: &str,
    generation: Option<u64>,
    data_bytes: usize,
    completed: bool,
) -> std::path::PathBuf {
    let job_dir = jobs_dir.join(job_id);
    fs::create_dir_all(&job_dir).unwrap();

    if let Some(gen_num) = generation {
        fs::write(job_dir.join("generation"), format!("{}", gen_num)).unwrap();
    }

    // Write data
    if data_bytes > 0 {
        fs::write(job_dir.join("checkpoint.bin"), vec![0xABu8; data_bytes]).unwrap();
    }

    if completed {
        fs::write(job_dir.join("completed"), b"1").unwrap();
    }

    job_dir
}

// ---------------------------------------------------------------------------
// RetentionConfig defaults
// ---------------------------------------------------------------------------

#[test]
fn test_retention_config_defaults() {
    let cfg = RetentionConfig::default();
    assert_eq!(cfg.max_generations, DEFAULT_MAX_GENERATIONS);
    assert_eq!(cfg.max_generations, 2);
    assert_eq!(cfg.job_bytes_max, DEFAULT_JOB_BYTES_MAX);
    assert_eq!(cfg.job_bytes_max, 128 * 1024 * 1024);
}

// ---------------------------------------------------------------------------
// VAL-CAS-RET-001: Retention keeps current + previous + leased only
// ---------------------------------------------------------------------------

#[test]
fn test_retention_current_prev_leased() {
    let env = TestEnv::new();

    // Create 5 generations, each with unique blobs.
    let _h1 = env.make_generation(1);
    let h2 = env.make_generation(2);
    let _h3 = env.make_generation(3);
    let _h4 = env.make_generation(4);
    let _h5 = env.make_generation(5);

    // Set CURRENT = 5
    env.write_current(5);

    // Create a lease on gen 2 (blob refcounts plus the generation hold that
    // records the lease's identity).
    let mut store = make_store(&env.root);
    {
        for hash in &h2 {
            store.incr(hash);
        }
        store.record_generation_hold(2);
        store.persist().unwrap();
    }

    let cfg = RetentionConfig::default();
    let report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    // Gen 5 (current) and gen 4 (previous) should be retained.
    // Gen 2 should be retained (leased: refcount > 0).
    // Gen 1 and gen 3 should be deleted.
    assert!(env.gens_dir.join("5").exists(), "current gen 5 must exist");
    assert!(env.gens_dir.join("4").exists(), "previous gen 4 must exist");
    assert!(env.gens_dir.join("2").exists(), "leased gen 2 must exist");
    assert!(
        !env.gens_dir.join("1").exists(),
        "old gen 1 must be deleted"
    );
    assert!(
        !env.gens_dir.join("3").exists(),
        "old gen 3 must be deleted"
    );

    // Report should reflect this.
    assert_eq!(report.generations_retained, 3);
    assert_eq!(report.generations_removed, 2);
}

#[test]
fn test_retention_shared_layer_does_not_make_an_old_generation_leased() {
    let env = TestEnv::new();

    // Generations 1..=4 have unique layers; generation 5 (current) is leased
    // and reuses ONE layer (the db blob) of generation 2.
    let _h1 = env.make_generation(1);
    let h2 = env.make_generation(2);
    let _h3 = env.make_generation(3);
    let _h4 = env.make_generation(4);
    let mut h5 = env.make_generation(5);
    h5[0] = h2[0];
    env.make_generation_with_hashes(5, &h5);
    env.write_current(5);

    let mut store = make_store(&env.root);
    for hash in &h5 {
        store.incr(hash); // a live lease on generation 5
    }
    store.record_generation_hold(5);
    store.persist().unwrap();

    let cfg = RetentionConfig::default();
    retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    assert!(env.gens_dir.join("5").exists(), "current gen 5 must exist");
    assert!(env.gens_dir.join("4").exists(), "previous gen 4 must exist");
    assert!(
        !env.gens_dir.join("2").exists(),
        "gen 2 only shares one blob with leased gen 5; it is not itself leased"
    );
    assert!(!env.gens_dir.join("1").exists());
    assert!(!env.gens_dir.join("3").exists());
}

#[test]
fn test_retention_identical_layer_set_does_not_make_an_old_generation_leased() {
    // A forced / no-content-change republish produces a generation whose
    // layer set is IDENTICAL to an older one. With lease state inferred from
    // blob refcounts, every layer of the old generation is above zero (the
    // lease on the current one pins them all), so the old look-alike
    // generation was retained forever, defeating the current-plus-previous
    // bound. Leases are tracked by generation identity, so only the
    // generation actually leased is retained.
    let env = TestEnv::new();

    let h1 = env.make_generation(1);
    let h2 = env.make_generation(2);
    let h3 = env.make_generation(3);
    let h4 = env.make_generation(4);
    // Generation 5: identical layers to generation 2 (nothing changed).
    env.make_generation_with_hashes(5, &h2);
    env.write_current(5);
    let _ = (h1, h3, h4);

    let mut store = make_store(&env.root);
    for hash in &h2 {
        store.incr(hash); // a live lease on generation 5 pins all of h2
    }
    store.record_generation_hold(5);
    store.persist().unwrap();

    let cfg = RetentionConfig::default();
    let report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    assert!(env.gens_dir.join("5").exists(), "current gen 5 must exist");
    assert!(env.gens_dir.join("4").exists(), "previous gen 4 must exist");
    assert!(
        !env.gens_dir.join("2").exists(),
        "gen 2 shares every layer with leased gen 5 but is not itself leased"
    );
    assert_eq!(report.generations_retained, 2);
}

#[test]
fn test_retention_no_lease_only_two() {
    let env = TestEnv::new();

    // Create 3 generations.
    env.make_generation(1);
    env.make_generation(2);
    env.make_generation(3);

    // Set CURRENT = 3, no leases.
    env.write_current(3);

    let mut store = make_store(&env.root);
    let cfg = RetentionConfig::default();
    let report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    // Only gen 3 (current) and gen 2 (previous) should survive.
    assert!(env.gens_dir.join("3").exists());
    assert!(env.gens_dir.join("2").exists());
    assert!(!env.gens_dir.join("1").exists());
    assert_eq!(report.generations_retained, 2);
    assert_eq!(report.generations_removed, 1);
}

// ---------------------------------------------------------------------------
// VAL-CAS-RET-001 variant: current is anchored on the CURRENT pointer, not
// the max generation number (crash-orphan safety per VAL-WRITER-004).
// ---------------------------------------------------------------------------

#[test]
fn test_retention_current_pointer_anchored_not_max() {
    let env = TestEnv::new();

    // Gens 3, 4, 5 exist on disk. CURRENT = 4, meaning gen 5 is a crash
    // orphan: its manifest was renamed but CURRENT was never updated
    // (VAL-WRITER-004 crash point 6). No leases.
    env.make_generation(3);
    env.make_generation(4);
    env.make_generation(5);
    env.write_current(4);

    let mut store = make_store(&env.root);
    let cfg = RetentionConfig::default();
    let report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    // The CURRENT-pointed generation (4) and its previous (3) survive; the
    // orphan gen 5 is pruned rather than evicting the live generation.
    assert!(
        env.gens_dir.join("4").exists(),
        "CURRENT-pointed gen 4 must survive retention"
    );
    assert!(
        env.gens_dir.join("3").exists(),
        "previous gen 3 must survive"
    );
    assert!(
        !env.gens_dir.join("5").exists(),
        "crash-orphan gen 5 must be pruned"
    );
    assert_eq!(report.generations_retained, 2);
    assert_eq!(report.generations_removed, 1);
}

// ---------------------------------------------------------------------------
// VAL-CAS-RET-002: Completed jobs deleted immediately on publication
// ---------------------------------------------------------------------------

#[test]
fn test_completed_job_deleted_on_publish() {
    let env = TestEnv::new();

    // Create a generation.
    env.make_generation(5);
    env.write_current(5);

    // Create a completed job that produced gen 5.
    make_job(&env.jobs_dir, "job-A", Some(5), 1024, true);

    // Create an in-progress job that produced gen 5 (should NOT be deleted).
    make_job(&env.jobs_dir, "job-B", Some(5), 1024, false);

    // Create a completed job for a different generation (should NOT be
    // immediately deleted; byte-cap may get it later).
    make_job(&env.jobs_dir, "job-C", Some(999), 1024, true);

    let mut store = make_store(&env.root);
    let cfg = RetentionConfig::default();
    let report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    assert!(
        !env.jobs_dir.join("job-A").exists(),
        "completed job for published gen must be deleted immediately"
    );
    assert!(
        env.jobs_dir.join("job-B").exists(),
        "in-progress job must NOT be deleted"
    );
    assert!(
        env.jobs_dir.join("job-C").exists(),
        "completed job for non-published gen must survive immediate deletion"
    );
    assert!(report.jobs_completed_deleted >= 1);
}

// ---------------------------------------------------------------------------
// VAL-CAS-RET-003: Jobs capped at 128 MiB (oldest-first)
// ---------------------------------------------------------------------------

#[test]
fn test_job_bytes_cap() {
    let env = TestEnv::new();

    // Create a generation.
    env.make_generation(1);
    env.write_current(1);

    // Create 5 completed jobs totaling more than 128 MiB, each 30 MiB.
    // Total: 5 * 30 MiB = 150 MiB > 128 MiB cap.
    let job_size: usize = 30 * 1024 * 1024;
    for i in 1..=5u32 {
        // Use different generations so they're not immediately deleted.
        make_job(
            &env.jobs_dir,
            &format!("job-{}", i),
            Some((i + 100) as u64), // gen != current(1)
            job_size,
            true,
        );
        // Small sleep to ensure mtimes differ.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    let mut store = make_store(&env.root);
    let cfg = RetentionConfig::default();
    let report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    let total_remaining = dir_total(&env.jobs_dir);
    assert!(
        total_remaining <= DEFAULT_JOB_BYTES_MAX,
        "remaining job bytes ({}) must be <= 128 MiB ({})",
        total_remaining,
        DEFAULT_JOB_BYTES_MAX
    );

    // At least one job should have been byte-capped.
    assert!(
        report.jobs_byte_capped >= 1,
        "at least one job should be byte-capped"
    );
    assert!(
        report.job_bytes_reclaimed > 0,
        "must report bytes reclaimed from jobs"
    );
}

#[test]
fn test_job_bytes_cap_preserves_in_progress() {
    let env = TestEnv::new();

    env.make_generation(1);
    env.write_current(1);

    // Create one large in-progress job (150 MiB, over cap).
    let big_size: usize = 150 * 1024 * 1024;
    make_job(&env.jobs_dir, "big-wip", Some(999), big_size, false);

    let mut store = make_store(&env.root);
    let cfg = RetentionConfig::default();
    let _report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    // In-progress job is preserved even though total exceeds cap.
    assert!(
        env.jobs_dir.join("big-wip").exists(),
        "in-progress job must NOT be deleted even if over cap"
    );
}

// ---------------------------------------------------------------------------
// VAL-FOOTPRINT-004: Generations directory has at most 2 entries (no leases)
// ---------------------------------------------------------------------------

#[test]
fn test_footprint_max_two_generations_no_leases() {
    let env = TestEnv::new();

    // Create 5 generations sequentially, publishing each.
    for generation in 1..=5u64 {
        env.make_generation(generation);
        env.write_current(generation);

        let mut store = make_store(&env.root);
        let cfg = RetentionConfig::default();
        let _ = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();
    }

    // After all publishes with no active leases, at most 2 generations remain.
    let count = count_gen_dirs(&env.gens_dir);
    assert!(
        count <= 2,
        "generations directory must have <= 2 entries (current + previous), got {}",
        count
    );
}

// ---------------------------------------------------------------------------
// VAL-FOOTPRINT-005: Job directory stays under 128 MiB after each publish
// ---------------------------------------------------------------------------

#[test]
fn test_footprint_jobs_under_cap_after_publish() {
    let env = TestEnv::new();

    for generation in 1..=3u64 {
        env.make_generation(generation);
        env.write_current(generation);

        // Add a 50 MiB completed job for a non-current gen between publishes.
        make_job(
            &env.jobs_dir,
            &format!("job-{}", generation),
            Some(generation + 100),
            50 * 1024 * 1024,
            true,
        );
        std::thread::sleep(std::time::Duration::from_millis(10));

        let mut store = make_store(&env.root);
        let cfg = RetentionConfig::default();
        let _ = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

        let job_bytes = dir_total(&env.jobs_dir);
        assert!(
            job_bytes <= DEFAULT_JOB_BYTES_MAX,
            "after publish {}, job dir must be <= 128 MiB, got {} bytes",
            generation,
            job_bytes
        );
    }
}

// ---------------------------------------------------------------------------
// CAS GC: blobs with no retained reference and refcount 0 are collected
// ---------------------------------------------------------------------------

#[test]
fn test_cas_gc_no_retained_reference() {
    let env = TestEnv::new();

    // Generation 1 with blobs.
    let _h1 = env.make_generation(1);
    env.write_current(1);

    // Create an orphan blob (not referenced by any manifest, refcount 0).
    let mut store = make_store(&env.root);
    let orphan_hash = store.put(b"orphan-blob-data").unwrap();

    let cfg = RetentionConfig::default();
    let report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    // Orphan blob should be collected.
    assert!(!store.exists(&orphan_hash), "orphan blob must be GC'd");
    assert!(report.cas.blobs_removed >= 1);
    assert!(report.cas.reclaimed_bytes > 0);
}

#[test]
fn test_cas_gc_retains_pinned_blobs() {
    let env = TestEnv::new();

    let h1 = env.make_generation(1);
    env.write_current(1);

    let mut store = make_store(&env.root);
    let cfg = RetentionConfig::default();
    let _report = retain_after_publish(&mut store, &env.gens_dir, &env.jobs_dir, &cfg).unwrap();

    // Generation 1's blobs are pinned by its manifest — they must survive GC.
    for hash in &h1 {
        assert!(
            store.exists(hash),
            "blob referenced by retained generation must NOT be GC'd"
        );
    }
}

// ---------------------------------------------------------------------------
// Job detection: completed via status file
// ---------------------------------------------------------------------------

#[test]
fn test_job_is_completed_via_status_file() {
    let tmp = tempfile::tempdir().unwrap();
    let job_dir = tmp.path().join("job-X");
    fs::create_dir_all(&job_dir).unwrap();
    fs::write(job_dir.join("status"), b"completed").unwrap();

    assert!(
        super::job_is_completed(&job_dir),
        "status file with 'completed' must be detected"
    );
}

#[test]
fn test_job_not_completed_no_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let job_dir = tmp.path().join("job-Y");
    fs::create_dir_all(&job_dir).unwrap();

    assert!(
        !super::job_is_completed(&job_dir),
        "job without completion marker must NOT be considered completed"
    );
}

// ---------------------------------------------------------------------------
// Retention report (read-only)
// ---------------------------------------------------------------------------

#[test]
fn test_retention_report_no_deletion() {
    let env = TestEnv::new();

    env.make_generation(1);
    env.write_current(1);

    let store = make_store(&env.root);
    let report = retention_report(&store, &env.gens_dir, &env.jobs_dir).unwrap();

    assert_eq!(report.generation_count, 1);
    assert!(report.cas_blob_count >= 5); // 5 layer blobs
    assert!(report.cas_bytes > 0);
    // No GC should happen (report-only function).
    assert_eq!(report.cas.reclaimed_bytes, 0);
}

#[test]
fn test_retention_report_dedup_ratio() {
    let env = TestEnv::new();

    // Two generations sharing the SAME blobs (identical layer data).
    let store = make_store(&env.root);
    let blobs = stage_blobs(&store, 1);
    let hashes = [blobs[0].0, blobs[1].0, blobs[2].0, blobs[3].0, blobs[4].0];
    env.make_generation_with_hashes(1, &hashes);
    env.make_generation_with_hashes(2, &hashes);
    env.write_current(2);

    let store = make_store(&env.root);
    let report = retention_report(&store, &env.gens_dir, &env.jobs_dir).unwrap();

    // 10 total references (5 per gen), 5 unique -> dedup ratio = 0.5
    assert!(
        report.dedup_ratio > 0.4 && report.dedup_ratio < 0.6,
        "dedup ratio with one shared gen should be ~0.5, got {}",
        report.dedup_ratio
    );
}

// ---------------------------------------------------------------------------
// Helpers used by tests
// ---------------------------------------------------------------------------

fn dir_total(path: &Path) -> u64 {
    super::dir_total_bytes(path)
}

fn count_gen_dirs(gens_dir: &Path) -> usize {
    super::count_generation_dirs(gens_dir)
}

// ---------------------------------------------------------------------------
// Legacy (no-CAS) store pruning
// ---------------------------------------------------------------------------

/// Build a legacy full-copy generation store: `generations/<N>/` with real
/// files, NO manifest, NO cas/ directory, and a CURRENT pointer.
fn build_legacy_store(
    generations: &[u64],
    current: Option<u64>,
) -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let gens_dir = root.join(GENERATIONS_DIR);
    let jobs_dir = root.join("jobs");
    fs::create_dir_all(&gens_dir).unwrap();
    fs::create_dir_all(&jobs_dir).unwrap();
    for &gen_num in generations {
        let gen_dir = gens_dir.join(gen_num.to_string());
        fs::create_dir_all(&gen_dir).unwrap();
        fs::write(gen_dir.join("leindex.db"), format!("db-{gen_num}")).unwrap();
        fs::write(gen_dir.join("embeddings.bin"), format!("emb-{gen_num}")).unwrap();
    }
    if let Some(current) = current {
        fs::write(root.join(CURRENT_FILE), current.to_string()).unwrap();
    }
    (dir, root)
}

#[test]
fn test_no_cas_prune_keeps_current_window() {
    use crate::storage::generation::retention::retain_generations_no_cas;
    let (_dir, root) = build_legacy_store(&[1, 2, 3, 4, 5, 6], Some(6));
    let gens_dir = root.join(GENERATIONS_DIR);
    let jobs_dir = root.join("jobs");

    let report =
        retain_generations_no_cas(&gens_dir, &jobs_dir, 3, false).expect("prune must succeed");

    assert_eq!(report.generations_removed, 3, "gens 1-3 pruned");
    assert_eq!(report.generations_retained, 3, "gens 4-6 retained");
    assert_eq!(count_gen_dirs(&gens_dir), 3);
    for kept in [4u64, 5, 6] {
        assert!(gens_dir.join(kept.to_string()).exists(), "gen {kept} kept");
    }
    for removed in [1u64, 2, 3] {
        assert!(
            !gens_dir.join(removed.to_string()).exists(),
            "gen {removed} removed"
        );
    }
}

#[test]
fn test_no_cas_prune_dry_run_removes_nothing() {
    use crate::storage::generation::retention::retain_generations_no_cas;
    let (_dir, root) = build_legacy_store(&[1, 2, 3, 4], Some(4));
    let gens_dir = root.join(GENERATIONS_DIR);

    let report =
        retain_generations_no_cas(&gens_dir, &root.join("jobs"), 2, true).expect("dry run");

    assert_eq!(report.generations_removed, 2, "dry run reports candidates");
    assert_eq!(count_gen_dirs(&gens_dir), 4, "dry run deletes nothing");
}

#[test]
fn test_no_cas_prune_missing_current_falls_back_to_newest() {
    use crate::storage::generation::retention::retain_generations_no_cas;
    let (_dir, root) = build_legacy_store(&[10, 11, 12, 13], None);
    let gens_dir = root.join(GENERATIONS_DIR);

    let report = retain_generations_no_cas(&gens_dir, &root.join("jobs"), 2, false).expect("prune");

    // No CURRENT: window anchors at the newest (13): keep 12, 13.
    assert_eq!(report.generations_removed, 2);
    assert!(gens_dir.join("13").exists());
    assert!(gens_dir.join("12").exists());
    assert!(!gens_dir.join("11").exists());
    assert!(!gens_dir.join("10").exists());
}

#[test]
fn test_no_cas_prune_max_one_keeps_only_current() {
    use crate::storage::generation::retention::retain_generations_no_cas;
    let (_dir, root) = build_legacy_store(&[1, 2, 3], Some(2));
    let gens_dir = root.join(GENERATIONS_DIR);

    let report = retain_generations_no_cas(&gens_dir, &root.join("jobs"), 1, false).expect("prune");

    assert_eq!(report.generations_retained, 1);
    assert!(gens_dir.join("2").exists(), "current always kept");
    assert!(
        !gens_dir.join("3").exists(),
        "newer-than-current dirs are still pruned"
    );
    assert!(!gens_dir.join("1").exists());
}

#[test]
fn test_no_cas_prune_deletes_completed_job_for_current_generation() {
    use crate::storage::generation::retention::retain_generations_no_cas;
    let (_dir, root) = build_legacy_store(&[1, 2, 3], Some(3));
    let jobs_dir = root.join("jobs");
    let job_dir = jobs_dir.join("job-a");
    fs::create_dir_all(&job_dir).unwrap();
    fs::write(job_dir.join("generation"), "3").unwrap();
    fs::write(job_dir.join("completed"), "").unwrap();
    fs::write(job_dir.join("checkpoint.bin"), "checkpoint").unwrap();

    let report =
        retain_generations_no_cas(&root.join(GENERATIONS_DIR), &jobs_dir, 2, false).expect("prune");

    assert_eq!(
        report.jobs_completed_deleted, 1,
        "completed job for current gen removed"
    );
    assert!(!job_dir.exists());
}
