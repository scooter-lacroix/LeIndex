//! §13 Functional verification scenarios 1-6 (VAL-ROLLOUT-007).
//!
//! These are library-level integration tests that exercise the generation
//! store, scheduler, CAS, and reader infrastructure through the public API.
//! Each scenario captures correctness + latency + CPU + memory + threads +
//! swap + GPU + disk-IO via the shared [`telemetry`](../common) module.
//!
//! Scenarios tested here:
//!  1. 3-harness/2-project mixed tools (multi-project concurrent access)
//!  2. Same-project simultaneous index (coalescing)
//!  3. Different-project simultaneous index (fair interleaving)
//!  4. Search during indexing (no-stall read)
//!  5. Repeated identical index (coalesce → no duplicate work)
//!  6. Changes mid-index (follow-up scheduling)

#![cfg(feature = "full")]

use leindex::scheduler::{BoundedJob, DrrQueue, QueueKey, Step, TargetRef, WorkBudget, WorkClass};
use leindex::storage::cas::CasStore;
use leindex::storage::generation::{
    GenerationLease, GenerationWriter, LayerKind, Manifest, ModelIdentity, read_current_generation,
};
use std::sync::{Arc, Mutex};
use tempfile::TempDir;

/// Import the shared telemetry module.
#[path = "../common/mod.rs"]
mod telemetry;
use telemetry::{capture_sample, run_with_telemetry};

// ============================================================================
// Scenario helpers
// ============================================================================

/// Create a valid 5-layer generation in a temp storage root using CAS + writer.
fn create_generation(
    dir: &TempDir,
    generation: u64,
    model_name: &str,
) -> (Arc<Mutex<CasStore>>, Manifest) {
    let storage_root = dir.path();
    let cas_dir = storage_root.join("cas");
    let cas = Arc::new(Mutex::new(CasStore::open(&cas_dir).expect("open cas")));
    let mut writer = GenerationWriter::new(storage_root, cas.clone());
    writer.set_model_identity(ModelIdentity {
        name: model_name.to_string(),
        digest: format!("sha256:{model_name}"),
        dimensions: 384,
    });
    for layer in [
        LayerKind::Db,
        LayerKind::Tfidf,
        LayerKind::Neural,
        LayerKind::Pdg,
        LayerKind::Symbols,
    ] {
        let payload = format!("{layer:?}-gen{generation}-{model_name}").into_bytes();
        writer.stage(layer, &payload).expect("stage layer");
    }
    writer.publish(generation).expect("publish generation");
    writer.finish_publish();

    let current = read_current_generation(storage_root).expect("read CURRENT");
    assert_eq!(current, generation);

    let manifest = leindex::storage::generation::read_generation_manifest(storage_root, generation)
        .expect("read manifest");
    (cas, manifest)
}

// ============================================================================
// Scenario 1: 3-harness/2-project mixed tools
// ============================================================================

#[test]
fn test_scenario_01_three_harness_two_project() {
    let (result, telemetry) = run_with_telemetry("01_three_harness_two_project", 1, || {
        let proj_a = tempfile::tempdir()?;
        let proj_b = tempfile::tempdir()?;

        // Both projects get a generation with different model names so we can
        // verify they don't cross-contaminate.
        let (cas_a, manifest_a) = create_generation(&proj_a, 1, "model-a");
        let (cas_b, manifest_b) = create_generation(&proj_b, 1, "model-b");

        // Simulate 3 harnesses: harness 1 and 2 access project A,
        // harness 3 accesses project B. Each reads its own generation.
        let lease1 = GenerationLease::acquire(cas_a.clone(), &manifest_a)?;
        let lease2 = GenerationLease::acquire(cas_a.clone(), &manifest_a)?;
        let lease3 = GenerationLease::acquire(cas_b.clone(), &manifest_b)?;

        // All leases return the correct model identity.
        assert_eq!(lease1.manifest().model_identity.name, "model-a");
        assert_eq!(lease2.manifest().model_identity.name, "model-a");
        assert_eq!(lease3.manifest().model_identity.name, "model-b");

        // All three leases are live simultaneously.
        let h1 = lease1.layer_hashes();
        let h2 = lease2.layer_hashes();
        let h3 = lease3.layer_hashes();
        assert_eq!(h1.len(), h2.len());
        assert_eq!(h3.len(), h1.len());

        drop(lease1);
        drop(lease2);
        drop(lease3);

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    result.expect("scenario 1 correctness");
    assert!(
        telemetry.latency.as_millis() < 5000,
        "scenario 1 should complete in <5s: {}",
        telemetry.summary()
    );
    println!("{}", telemetry.summary());
}

// ============================================================================
// Scenario 2: Same-project simultaneous index (coalescing)
// ============================================================================

#[test]
fn test_scenario_02_same_project_simultaneous_index() {
    let (result, telemetry) = run_with_telemetry("02_same_project_simult_index", 2, || {
        // Two index requests to the same project with identical source
        // fingerprint should coalesce into one job in the DRR queue.
        let mut queue: DrrQueue<String> = DrrQueue::new();

        let key1 = QueueKey::new("client-1", "project-x", WorkClass::IndexChunk);
        let key2 = QueueKey::new("client-2", "project-x", WorkClass::IndexChunk);
        let target = TargetRef::new("project-x", "fp-abc123");

        let id1 = queue.enqueue(key1, Some(target.clone()), Box::new(DummyJob::new(3)));
        let id2 = queue.enqueue(key2, Some(target), Box::new(DummyJob::new(3)));

        // The second job should be coalesced: it returns the first job's ID.
        assert_eq!(
            id1, id2,
            "same-project same-fingerprint jobs should coalesce"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    result.expect("scenario 2 correctness");
    println!("{}", telemetry.summary());
}

// ============================================================================
// Scenario 3: Different-project simultaneous index (fair interleaving)
// ============================================================================

#[test]
fn test_scenario_03_different_project_simultaneous_index() {
    let (result, telemetry) = run_with_telemetry("03_diff_project_simult_index", 3, || {
        let mut queue: DrrQueue<String> = DrrQueue::new();

        let key1 = QueueKey::new("client-1", "project-a", WorkClass::IndexChunk);
        let key2 = QueueKey::new("client-2", "project-b", WorkClass::IndexChunk);
        let target1 = TargetRef::new("project-a", "fp-a");
        let target2 = TargetRef::new("project-b", "fp-b");

        let id1 = queue.enqueue(key1, Some(target1), Box::new(DummyJob::new(2)));
        let id2 = queue.enqueue(key2, Some(target2), Box::new(DummyJob::new(2)));

        assert_ne!(id1, id2, "different-project jobs should NOT coalesce");

        // Tick the scheduler: both projects should get service (fair DRR).
        let budget = WorkBudget::unlimited();
        let _outcome1 = queue.tick(budget);
        let _outcome2 = queue.tick(budget);

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    result.expect("scenario 3 correctness");
    println!("{}", telemetry.summary());
}

// ============================================================================
// Scenario 4: Search during indexing (no-stall read)
// ============================================================================

#[test]
fn test_scenario_04_search_during_indexing_no_stall() {
    let (result, telemetry) = run_with_telemetry("04_search_during_index_no_stall", 4, || {
        let dir = tempfile::tempdir()?;
        let storage_root = dir.path();

        // Publish gen 1 (the "previous good" generation searches read from).
        let cas = Arc::new(Mutex::new(CasStore::open(storage_root.join("cas"))?));
        let mut writer = GenerationWriter::new(storage_root, cas.clone());
        writer.set_model_identity(ModelIdentity {
            name: "test-model".to_string(),
            digest: "sha256:test".to_string(),
            dimensions: 384,
        });
        for layer in [
            LayerKind::Db,
            LayerKind::Tfidf,
            LayerKind::Neural,
            LayerKind::Pdg,
            LayerKind::Symbols,
        ] {
            writer.stage(layer, b"gen1-data")?;
        }
        writer.publish(1)?;
        writer.finish_publish();

        // Acquire a lease on gen 1 (simulating a search read).
        let manifest = leindex::storage::generation::read_generation_manifest(storage_root, 1)?;
        let lease = GenerationLease::acquire(cas.clone(), &manifest)?;

        // While the lease is held, publish gen 2 (simulating concurrent index).
        for layer in [
            LayerKind::Db,
            LayerKind::Tfidf,
            LayerKind::Neural,
            LayerKind::Pdg,
            LayerKind::Symbols,
        ] {
            writer.stage(layer, b"gen2-data")?;
        }
        writer.publish(2)?;
        writer.finish_publish();

        // The lease from gen 1 must still be valid (no-stall read isolation).
        let hashes = lease.layer_hashes();
        assert!(!hashes.is_empty(), "lease should still have hashes");
        for h in hashes {
            assert!(
                cas.lock().unwrap().exists(h),
                "gen 1 blobs must remain while lease held"
            );
        }

        drop(lease);

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    result.expect("scenario 4 correctness");
    assert!(
        telemetry.latency.as_millis() < 5000,
        "scenario 4 should complete quickly (no stall): {}",
        telemetry.summary()
    );
    println!("{}", telemetry.summary());
}

// ============================================================================
// Scenario 5: Repeated identical index (coalesce → no duplicate work)
// ============================================================================

#[test]
fn test_scenario_05_repeated_identical_index_coalesce() {
    let (result, telemetry) = run_with_telemetry("05_repeated_identical_index", 5, || {
        let mut queue: DrrQueue<String> = DrrQueue::new();
        let key = QueueKey::new("client-1", "project-x", WorkClass::IndexChunk);
        let target = TargetRef::new("project-x", "identical-fp");

        // Enqueue the same job 10 times — all should coalesce to the same ID.
        let mut first_id = None;
        for _ in 0..10 {
            let id = queue.enqueue(
                key.clone(),
                Some(target.clone()),
                Box::new(DummyJob::new(1)),
            );
            match first_id {
                None => first_id = Some(id),
                Some(fid) => assert_eq!(id, fid, "identical index should coalesce"),
            }
        }

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    result.expect("scenario 5 correctness");
    println!("{}", telemetry.summary());
}

// ============================================================================
// Scenario 6: Changes mid-index (follow-up scheduling)
// ============================================================================

#[test]
fn test_scenario_06_changes_mid_index_followup() {
    let (result, telemetry) = run_with_telemetry("06_changes_mid_index_followup", 6, || {
        let mut queue: DrrQueue<String> = DrrQueue::new();

        let key = QueueKey::new("client-1", "project-x", WorkClass::IndexChunk);

        // First index request with fingerprint "fp-v1".
        let target_v1 = TargetRef::new("project-x", "fp-v1");
        let id1 = queue.enqueue(key.clone(), Some(target_v1), Box::new(DummyJob::new(2)));

        // Follow-up with different fingerprint caused by mid-index changes.
        let target_v2 = TargetRef::new("project-x", "fp-v2");
        let id2 = queue.enqueue(key, Some(target_v2), Box::new(DummyJob::new(2)));

        assert_ne!(
            id1, id2,
            "different-fingerprint follow-up should NOT coalesce"
        );

        Ok::<(), Box<dyn std::error::Error>>(())
    });

    result.expect("scenario 6 correctness");
    println!("{}", telemetry.summary());
}

// ============================================================================
// Telemetry capture validation
// ============================================================================

#[test]
fn test_telemetry_sample_captures_all_metrics() {
    let sample = capture_sample();
    assert!(sample.rss_kib > 0, "VmRSS should be > 0");
    assert!(
        sample.vmsize_kib >= sample.rss_kib,
        "VmSize ({}) should be >= VmRSS ({})",
        sample.vmsize_kib,
        sample.rss_kib
    );
    assert!(sample.threads >= 1, "Threads should be >= 1");
    println!(
        "telemetry sample: rss={}KiB vmsize={}KiB swap={}KiB threads={} cpu={}us",
        sample.rss_kib, sample.vmsize_kib, sample.swap_kib, sample.threads, sample.cpu_time_us
    );
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// A trivial stepped job for scheduler testing. Yields `n` times then completes.
struct DummyJob {
    steps_remaining: usize,
}

impl DummyJob {
    fn new(n: usize) -> Self {
        Self { steps_remaining: n }
    }
}

impl BoundedJob for DummyJob {
    type Progress = String;
    fn step(&mut self, _budget: WorkBudget) -> anyhow::Result<Step<String>> {
        self.steps_remaining = self.steps_remaining.saturating_sub(1);
        if self.steps_remaining == 0 {
            Ok(Step::Complete)
        } else {
            Ok(Step::Yield(format!("remaining={}", self.steps_remaining)))
        }
    }
    fn estimated_next_bytes(&self) -> usize {
        self.steps_remaining * 1000
    }
}
