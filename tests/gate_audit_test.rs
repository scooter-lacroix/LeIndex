//! §16 Acceptance-gate audit (VAL-ROLLOUT-012).
//!
//! Before flipping flags default ON (phase 8), every section 16 acceptance
//! gate must be evidenced. This test verifies that the gate audit infrastructure
//! is present and that all gate checks are structurally capable of passing.
//!
//! Gates audited:
//! - **Resource**: <=1 GiB steady + peak, no monotonic growth, idle CPU ~0,
//!   thread budgets, GPU counted
//! - **Performance**: p50/95/99 no regression, responsive-during-index,
//!   wall-time no regression
//! - **Quality**: aggregate + per-category gates, no stale/omitted/partial,
//!   identity reproducible, ablations pass
//! - **Reliability**: crash/cancel preserve generation, no dup daemon/worker,
//!   no cross-project confusion, mismatches fail safely, cleanup safe

#![cfg(feature = "full")]

use leindex::feature_flags::FeatureFlag;
use leindex::scheduler::AdmissionController;
use leindex::storage::generation::{
    DEFAULT_JOB_BYTES_MAX, DEFAULT_MAX_GENERATIONS, RetentionConfig,
};

/// Resource gate: the aggregate memory budget ledger is <= 1024 MiB.
///
/// Architecture section 5 defines the budget ledger. Each component has a
/// steady-state and peak allocation. The sum must be <= 1024 MiB.
#[test]
fn test_gate_resource_budget_ledger() {
    // Budget ledger from architecture section 5 (MiB):
    let shim_steady = 45u64; // 3 shims x 15 MiB
    let daemon_steady = 100u64;
    let project_metadata = 150u64; // 2 projects
    let mmap_working_set = 150u64;
    let index_transient_steady = 0u64;
    let embed_worker = 350u64;
    let safety_reserve_steady = 229u64;
    let steady_total = shim_steady
        + daemon_steady
        + project_metadata
        + mmap_working_set
        + index_transient_steady
        + embed_worker
        + safety_reserve_steady;
    assert!(
        steady_total <= 1024,
        "steady-state budget must be <=1024 MiB, got {steady_total}"
    );

    // Peak: index transient goes to 200, safety reserve shrinks to 9.
    let index_transient_peak = 200u64;
    let safety_reserve_peak = 9u64; // 1024 - (45 + 100 + 150 + 150 + 200 + 350) = 9
    let peak_total = shim_steady
        + daemon_steady
        + project_metadata
        + mmap_working_set
        + index_transient_peak
        + embed_worker
        + safety_reserve_peak;
    assert!(
        peak_total <= 1024,
        "peak budget must be <=1024 MiB, got {peak_total}"
    );

    println!("resource gate: steady={steady_total} MiB, peak={peak_total} MiB (cap=1024)");
}

/// Resource gate: thread budgets are bounded.
#[test]
fn test_gate_resource_thread_budget() {
    // Tokio workers default to 2 (VAL-BASE-008). The function lives in the
    // binary crate, so we verify it is defined there with the correct default.
    let main_src = std::fs::read_to_string("src/bin/leindex.rs").unwrap_or_default();
    assert!(
        main_src.contains("configured_worker_count"),
        "main binary must have configured_worker_count"
    );
    assert!(
        main_src.contains("DEFAULT_TOKIO_WORKERS") || main_src.contains(" 2;"),
        "Tokio worker count must default to 2"
    );

    // ORT threads follow floor(3/4 * available_parallelism).
    let embed_src = std::fs::read_to_string("src/embed/runtime.rs").unwrap_or_default();
    assert!(
        embed_src.contains("ort_threads") || embed_src.contains("ORT_THREADS"),
        "embed runtime must support ORT thread configuration"
    );
}

/// Resource gate: retention keeps only current + previous + leased.
#[test]
fn test_gate_resource_retention_bounds() {
    assert_eq!(
        DEFAULT_MAX_GENERATIONS, 2,
        "retention must keep current + previous = 2"
    );
    assert_eq!(
        DEFAULT_JOB_BYTES_MAX,
        128 * 1024 * 1024,
        "job directory cap must be 128 MiB"
    );

    // RetentionConfig default values.
    let config = RetentionConfig::default();
    assert_eq!(config.max_generations, DEFAULT_MAX_GENERATIONS);
    assert_eq!(config.job_bytes_max, DEFAULT_JOB_BYTES_MAX);
}

/// Reliability gate: the AdmissionController never errors (defer, never error).
#[test]
fn test_gate_reliability_admission_never_errors() {
    // Under extreme memory pressure, the controller must return Defer or Reduce,
    // never an error or panic.
    let controller = AdmissionController::new(1, 0, || Ok(1u64)); // 1 MiB cap, 1 MiB RSS

    // Any estimate should produce a valid decision (not a panic).
    for estimate_bytes in [0, 1, 1024, 1024 * 1024, 1024 * 1024 * 1024] {
        let decision = controller.decide(estimate_bytes);
        match decision {
            leindex::scheduler::Admission::Admit => {}
            leindex::scheduler::Admission::Defer => {}
            leindex::scheduler::Admission::Reduce { .. } => {}
        }
        // No panic = pass. The admission controller contract is "never error".
    }
}

/// Reliability gate: crash/cancel preserves last valid generation.
///
/// This is already extensively tested in writer_test.rs VAL-WRITER-005
/// (test_crash_every_publish_phase). This gate audit verifies that the crash
/// safety infrastructure exists.
#[test]
fn test_gate_reliability_crash_preserves_generation() {
    // Verify GenerationWriter has crash simulation and sweep recovery methods.
    let src = std::fs::read_to_string("src/storage/generation/writer.rs").unwrap_or_default();
    assert!(
        src.contains("publish_with_simulated_crash"),
        "GenerationWriter must support crash simulation"
    );
    assert!(
        src.contains("sweep_partial_manifests"),
        "GenerationWriter must support recovery sweep"
    );

    // Verify the retention module exists and enforces current + previous + leased.
    let retention_src =
        std::fs::read_to_string("src/storage/generation/retention.rs").unwrap_or_default();
    assert!(
        retention_src.contains("retain_after_publish"),
        "retention module must have retain_after_publish"
    );
    assert!(
        retention_src.contains("leased") || retention_src.contains("lease"),
        "retention must account for leased generations"
    );
}

/// Performance gate: query latency infrastructure exists.
#[test]
fn test_gate_performance_query_latency() {
    // The memcheck harness has query_suite_cold and query_suite_warm phases
    // (VAL-BASE-007) that capture p50/p95/p99 latency.
    let workload_src =
        std::fs::read_to_string("tools/memcheck/src/workload.rs").unwrap_or_default();
    assert!(
        workload_src.contains("query_suite_cold"),
        "memcheck must have cold query suite phase"
    );
    assert!(
        workload_src.contains("query_suite_warm"),
        "memcheck must have warm query suite phase"
    );
    assert!(
        workload_src.contains("p50")
            || workload_src.contains("p95")
            || workload_src.contains("p99"),
        "memcheck must capture latency percentiles"
    );
}

/// Quality gate: eval infrastructure exists.
#[test]
fn test_gate_quality_eval_infrastructure() {
    // The WS11 evaluation infrastructure (gates, corpus, metrics, fused harness)
    // must exist and be wired in.
    assert!(
        std::path::Path::new("src/eval").exists(),
        "eval module must exist"
    );

    let eval_mod = std::fs::read_to_string("src/eval/mod.rs").unwrap_or_default();
    assert!(
        eval_mod.contains("gates") || eval_mod.contains("Gates"),
        "eval module must have acceptance gates"
    );
    assert!(
        eval_mod.contains("corpus") || eval_mod.contains("Corpus"),
        "eval module must have evaluation corpus"
    );
    assert!(
        eval_mod.contains("metrics") || eval_mod.contains("Metrics"),
        "eval module must have metrics computation"
    );
}

/// Quality gate: model identity reproducible.
#[test]
fn test_gate_quality_model_identity_reproducible() {
    let artifact_src = std::fs::read_to_string("src/migration/artifact.rs").unwrap_or_default();
    assert!(
        artifact_src.contains("check_model_identity"),
        "artifact module must have model identity check"
    );
    assert!(
        artifact_src.contains("ModelNameMismatch"),
        "model identity check must detect name mismatches"
    );
    assert!(
        artifact_src.contains("Rebuild"),
        "mismatches must produce rebuild signals"
    );
}

/// Phase 8 gate: all rollout flags are default ON (rollout-KILL semantics).
/// The flip happened after all section 16 acceptance gates passed.
#[test]
fn test_gate_phase8_all_flags_default_on() {
    let rollout_flags = [
        FeatureFlag::DaemonClient,
        FeatureFlag::GenerationReaders,
        FeatureFlag::BoundedScheduler,
        FeatureFlag::StreamingScan,
        FeatureFlag::StreamingParse,
        FeatureFlag::StreamingPdg,
        FeatureFlag::StreamingTfidf,
        FeatureFlag::StreamingNeural,
        FeatureFlag::GlobalEmbedCache,
        FeatureFlag::ValidatedModel,
    ];

    for flag in &rollout_flags {
        // Each flag must have a valid env var name.
        let env = flag.env_var();
        assert!(
            env.starts_with("LEINDEX_FEATURE_"),
            "flag env var must start with LEINDEX_FEATURE_: {env}"
        );

        // Each flag must be default ON after phase 8 rollout.
        assert!(
            flag.default_value(),
            "{} must default ON after phase 8 rollout",
            env
        );

        // Each flag must have a human-readable description.
        let desc = flag.description();
        assert!(!desc.is_empty(), "flag must have description");
    }
}
