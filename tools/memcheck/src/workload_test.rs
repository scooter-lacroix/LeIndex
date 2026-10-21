use super::*;

#[test]
fn test_canonical_phases_order() {
    assert_eq!(
        CANONICAL_PHASES,
        &[
            "idle_warm",
            "index",
            "idle_post",
            "query",
            "reindex",
            "idle_final",
            "embed_idle",
            "embed_active",
            "embed_teardown",
            "mcp_idle_proliferation",
            "worker_ort_threads",
            "stale_artifacts",
            "contention_3c_2p",
            "incremental_noop",
            "incremental_one_file",
            "incremental_burst",
            "incremental_delete",
            "query_suite_cold",
            "query_suite_warm",
            "full_index_run2",
            "full_index_run3",
        ]
    );
}

#[test]
fn test_canonical_phases_count() {
    assert_eq!(CANONICAL_PHASES.len(), 21);
}

#[test]
fn test_copy_fixture_source_excludes_root_leindex() {
    let source = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(source.path().join("src")).unwrap();
    std::fs::create_dir_all(source.path().join(".leindex")).unwrap();
    std::fs::write(source.path().join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    std::fs::write(source.path().join(".leindex/index.db"), "source index").unwrap();

    let isolated = copy_fixture_source(source.path()).unwrap();

    assert_eq!(
        std::fs::read_to_string(isolated.path().join("src/lib.rs")).unwrap(),
        "pub fn fixture() {}\n"
    );
    assert!(!isolated.path().join(".leindex").exists());
    assert_eq!(
        std::fs::read_to_string(source.path().join(".leindex/index.db")).unwrap(),
        "source index"
    );
}

#[test]
fn test_build_phase_report_empty() {
    let mut samples = Vec::new();
    let report = build_phase_report("test", &mut samples, Duration::from_secs(1));
    assert_eq!(report.phase, "test");
    assert_eq!(report.sample_count, 0);
    assert_eq!(report.rss_min_kib, 0);
    assert_eq!(report.rss_max_kib, 0);
    assert_eq!(report.rss_p95_kib, 0);
    assert_eq!(report.mapped_file_kib, 0);
    assert_eq!(report.anon_kib, 0);
    assert_eq!(report.worker_rss_max_kib, 0);
    assert_eq!(report.combined_rss_max_kib, 0);
}

#[test]
fn test_non_linux_command_phase_fails_before_launch_or_measurement() {
    let fixture = tempfile::tempdir().unwrap();
    let config = WorkloadConfig {
        binary: PathBuf::from("missing-binary-must-not-be-launched"),
        fixture: fixture.path().to_path_buf(),
        sample_interval: Duration::from_millis(10),
        verbose: false,
        worker_capable: false,
        heap_profile_dir: None,
    };
    let error = run_command_phase_with_peak_support(
        &config,
        "unsupported_platform",
        |_, _| panic!("unsupported command phase must fail before building its command"),
        false,
    )
    .unwrap_err();

    assert!(
        format!("{error:#}").contains("requires Linux"),
        "unexpected error: {error:#}"
    );
}

#[test]
fn test_descendant_observation_certifies_own_peak_only_without_descendants() {
    // The condition that decides whether ru_maxrss may extend rss_max_kib.
    // It is exact and cheap, and it is the whole of finding 8's safety.
    assert!(
        DescendantObservation {
            seen: false,
            observable: true,
        }
        .certifies_no_descendants(),
        "a clean poll of a childless process certifies its own peak"
    );
    assert!(
        !DescendantObservation {
            seen: true,
            observable: true,
        }
        .certifies_no_descendants(),
        "a descendant was seen — folding would be charged to the main process"
    );
    assert!(
        !DescendantObservation {
            seen: false,
            observable: false,
        }
        .certifies_no_descendants(),
        "an unreadable children file must not be mistaken for 'no children'"
    );
    assert!(
        !DescendantObservation::default().certifies_no_descendants(),
        "a phase that could not poll must not certify anything"
    );
}

#[test]
fn test_lifetime_fold_refuses_a_peak_no_observation_accounts_for() {
    // `certifies_no_descendants` is necessary but not sufficient. `children`
    // lists DIRECT children only, so "not seen" is not evidence that nothing
    // ran — a reaped, reparented grandchild leaves `seen == false` while its
    // high-water mark is already inside `ru_maxrss`. The magnitude guard is
    // what stops that from being certified as the measured process's own peak.
    let clean = DescendantObservation {
        seen: false,
        observable: true,
    };
    // Agreement inside the tolerance: fold, because it recovers the final-tick
    // allocation the VmHWM poll can miss.
    assert!(matches!(
        lifetime_fold(clean, 100_000, 100_500),
        LifetimeFold::Accepted
    ));
    // A lifetime mark far above every peak we actually measured is a folded
    // descendant, not our own allocation.
    assert!(matches!(
        lifetime_fold(clean, 40_000, 750_000),
        LifetimeFold::ExceedsObserved
    ));
    // The regression this guard exists for, at its real magnitude: `index`
    // measured 735848 KiB instead of ~38 MiB because rust-analyzer scip's peak
    // was folded in. A tolerance wide enough to accept this (1 GiB was the
    // first attempt) would certify the regression instead of catching it.
    assert!(
        matches!(
            lifetime_fold(clean, 38 * 1024, 735_848),
            LifetimeFold::ExceedsObserved
        ),
        "the 733 MiB SCIP fold must be refused, not certified"
    );
    // A descendant we DID see is refused regardless of magnitude, since its
    // peak may be folded in.
    assert!(matches!(
        lifetime_fold(
            DescendantObservation {
                seen: true,
                observable: true
            },
            100_000,
            100_500
        ),
        LifetimeFold::DescendantsSeen
    ));
    // Nothing was observed, so nothing is certifiable.
    assert!(matches!(
        lifetime_fold(DescendantObservation::default(), 100_000, 100_500),
        LifetimeFold::DescendantsSeen
    ));
}

#[cfg(target_os = "linux")]
#[test]
fn test_descendant_phase_does_not_fold_reaped_peak_into_main_rss() {
    // The mirror of `test_command_peak_captures_final_tick_allocation`: the
    // same reaped-peak fold must NOT happen here, or `index` would report
    // rust-analyzer scip's 733 MiB as leindex's own.
    //
    // Unlike the non-descendant side this one is deterministic. The
    // descendant allocates far more than `sh` ever will, so the
    // descendant-inclusive lifetime peak and the main process's own peak
    // differ by two orders of magnitude — no reliance on a poll landing
    // inside the allocation window.
    const DESCENDANT_KIB: u64 = 128 * 1024;
    let fixture = tempfile::tempdir().unwrap();
    let config = WorkloadConfig {
        binary: PathBuf::from("/bin/sh"),
        fixture: fixture.path().to_path_buf(),
        sample_interval: Duration::from_millis(10),
        verbose: false,
        worker_capable: false,
        heap_profile_dir: None,
    };
    let report = run_command_phase_with_peak_support(
        &config,
        "descendant_reaped",
        |_, _| {
            let mut command = Command::new("/bin/sh");
            command.arg("-c").arg(format!(
                "MEMCHECK_PEAK_CHILD=transient MEMCHECK_PEAK_CHILD_KIB={DESCENDANT_KIB} \
                 MEMCHECK_PEAK_CHILD_SLEEP_MS=0 '{helper}' --exact \
                 workload::tests::test_command_peak_child & wait",
                helper = std::env::current_exe().unwrap().display(),
            ));
            command
        },
        true,
    )
    .unwrap();

    assert_eq!(
        report.descendants.total, 0,
        "the shell reaped the helper, so the post-phase scan must see nothing"
    );
    assert!(
        report.descendants.peak_rss_kib >= DESCENDANT_KIB,
        "the lifetime peak {} KiB must include the {DESCENDANT_KIB} KiB descendant",
        report.descendants.peak_rss_kib
    );
    assert!(
        report.rss_max_kib < DESCENDANT_KIB,
        "rss_max_kib {} KiB must not fold the descendant's peak into the main process",
        report.rss_max_kib
    );
}

#[cfg(target_os = "linux")]
#[test]
fn test_reaped_unseen_descendant_is_not_folded_into_main_rss() {
    // The case `seen == false` cannot rule out, and the reason the fold cannot
    // rest on the live poller alone: `/proc/<pid>/task/<tid>/children` lists
    // DIRECT children, so a descendant that is reaped and reparented is never
    // observed. Here the shell spawns a memory-hungry helper in the background
    // and exits without waiting for it, so `wait4` still folds the helper's
    // peak into `ru_maxrss` while the poller only ever saw an immediate child
    // it can no longer report.
    //
    // Without a magnitude guard this phase certifies "ran no descendants" and
    // charges the helper's peak to leindex — precisely the 733 MiB SCIP
    // regression the descendant check exists to prevent. The guard rejects it
    // because the lifetime mark dwarfs every peak actually measured on the
    // measured process.
    const DESCENDANT_KIB: u64 = 128 * 1024;
    let fixture = tempfile::tempdir().unwrap();
    let config = WorkloadConfig {
        binary: PathBuf::from("/bin/sh"),
        fixture: fixture.path().to_path_buf(),
        sample_interval: Duration::from_millis(10),
        verbose: false,
        worker_capable: false,
        heap_profile_dir: None,
    };
    let report = run_command_phase_with_peak_support(
        &config,
        "descendant_orphaned",
        |_, _| {
            let mut command = Command::new("/bin/sh");
            // The helper is backgrounded and NOT waited on; the shell exits
            // first, leaving the helper reparented to init.
            command.arg("-c").arg(format!(
                "MEMCHECK_PEAK_CHILD=transient MEMCHECK_PEAK_CHILD_KIB={DESCENDANT_KIB} \
                 MEMCHECK_PEAK_CHILD_SLEEP_MS=0 '{helper}' --exact \
                 workload::tests::test_command_peak_child & sleep 0.15",
                helper = std::env::current_exe().unwrap().display(),
            ));
            command
        },
        true,
    )
    .unwrap();

    assert!(
        report.descendants.peak_rss_kib >= DESCENDANT_KIB,
        "the lifetime peak {} KiB must include the orphaned descendant",
        report.descendants.peak_rss_kib
    );
    assert!(
        report.rss_max_kib < DESCENDANT_KIB,
        "rss_max_kib {} KiB must not adopt a descendant's peak that was reaped \
         before the poller could see it",
        report.rss_max_kib
    );
}

#[cfg(target_os = "linux")]
#[test]
fn test_command_phase_keeps_descendant_peak_out_of_main_rss() {
    // A command that runs a memory-hungry descendant must not have that
    // descendant's peak charged to main-process RSS. Regression guard: the
    // `index` phase measured 735848 KiB instead of ~38 MiB purely because
    // SCIP precision ingest launched `rust-analyzer scip` (~733 MiB) and
    // `wait4`'s ru_maxrss folds reaped descendants into the parent's peak.
    //
    // The descendant is a second `memcheck` process that allocates and
    // touches GRANDCHILD_KIB, so it is genuinely resident — the previous
    // `dd`-to-a-file grandchild lived in the page cache, reached a few MiB,
    // and never exercised the invariant at all.
    const GRANDCHILD_KIB: u64 = 131_072; // 128 MiB: far above the harness's sh.
    let fixture = tempfile::tempdir().unwrap();
    let config = WorkloadConfig {
        binary: PathBuf::from("/bin/sh"),
        fixture: fixture.path().to_path_buf(),
        sample_interval: Duration::from_millis(10),
        verbose: false,
        worker_capable: false,
        heap_profile_dir: None,
    };
    let report = run_command_phase_with_peak_support(
        &config,
        "descendant_peak",
        |_, _| {
            let mut command = Command::new("/bin/sh");
            // `sh` stays resident for the whole phase, spawns the helper as
            // a child, and waits for it — so the descendant is reaped before
            // the phase ends, which is the `index` shape.
            command.arg("-c").arg(format!(
                "MEMCHECK_PEAK_CHILD=resident MEMCHECK_PEAK_CHILD_KIB={GRANDCHILD_KIB} \
                 '{helper}' --exact workload::tests::test_command_peak_child & wait",
                helper = std::env::current_exe().unwrap().display(),
            ));
            command
        },
        true,
    )
    .unwrap();

    // The descendant must have been observed at all, or the assertion below
    // passes for the wrong reason. `wait` reaped it, so the post-phase scan
    // cannot see it; falling back to `VmHWM` would let this test pass with no
    // descendant resident, which is exactly how the previous version rotted.
    // The kernel's descendant-inclusive lifetime peak is the witness.
    assert!(
        report.descendants.peak_rss_kib > GRANDCHILD_KIB,
        "no {GRANDCHILD_KIB} KiB descendant was observed (kernel lifetime peak {} KiB) — \
         the guard would pass vacuously",
        report.descendants.peak_rss_kib
    );
    // ...and that peak must not be charged to the main process.
    assert!(
        report.rss_max_kib < GRANDCHILD_KIB,
        "main-process rss_max_kib {} KiB must not absorb the descendant peak",
        report.rss_max_kib
    );
}

#[cfg(target_os = "linux")]
#[test]
fn test_measured_pid_rejects_wait4_selectors() {
    assert_eq!(measured_pid(1).unwrap(), 1);
    assert_eq!(measured_pid(i32::MAX as u32).unwrap(), i32::MAX);
    assert!(measured_pid(0).is_err());
    assert!(measured_pid(i32::MAX as u32 + 1).is_err());
    assert!(measured_pid(u32::MAX).is_err());
}

/// Payload for the self-invoked helper processes the tests around it spawn.
/// A no-op unless `MEMCHECK_PEAK_CHILD` selects a mode.
///
/// `resident` touches every page of a large allocation and exits, so the
/// kernel's lifetime peak for it is genuinely large even though the process
/// is short-lived. `transient` outlives the startup sample, then touches and
/// immediately releases its allocation — after `MEMCHECK_PEAK_CHILD_SLEEP_MS`,
/// zero meaning "peak and exit immediately".
#[cfg(target_os = "linux")]
#[test]
fn test_command_peak_child() {
    let Some(mode) = std::env::var_os("MEMCHECK_PEAK_CHILD") else {
        return;
    };
    let kib: usize = std::env::var("MEMCHECK_PEAK_CHILD_KIB")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(64 * 1024);
    if mode == "resident" {
        let mut allocation = vec![0u8; kib * 1024];
        for byte in allocation.iter_mut().step_by(4096) {
            *byte = 1;
        }
        std::hint::black_box(&allocation);
        return;
    }
    // Outlive the startup sample, then touch and immediately free an
    // allocation between periodic ticks.
    let sleep_ms = std::env::var("MEMCHECK_PEAK_CHILD_SLEEP_MS")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(100);
    std::thread::sleep(Duration::from_millis(sleep_ms));
    let mut allocation = vec![0u8; kib * 1024];
    for byte in allocation.iter_mut().step_by(4096) {
        *byte = 1;
    }
    std::hint::black_box(&allocation);
}

#[cfg(target_os = "linux")]
#[test]
fn test_command_peak_captures_between_tick_allocation() {
    let fixture = tempfile::tempdir().unwrap();
    let config = WorkloadConfig {
        binary: std::env::current_exe().unwrap(),
        fixture: fixture.path().to_path_buf(),
        sample_interval: Duration::from_millis(500),
        verbose: false,
        worker_capable: false,
        heap_profile_dir: None,
    };
    let report = run_command_phase(&config, "between_ticks", |bin, _| {
        let mut command = Command::new(bin);
        command
            .args(["--exact", "workload::tests::test_command_peak_child"])
            .env("MEMCHECK_PEAK_CHILD", "transient");
        command
    })
    .unwrap();
    assert!(
        report.rss_max_kib >= 64 * 1024,
        "lifetime peak must include the allocation, got {} KiB",
        report.rss_max_kib
    );
    assert!(report.combined_rss_max_kib >= report.rss_max_kib);
}

#[cfg(target_os = "linux")]
#[test]
fn test_command_peak_captures_final_tick_allocation() {
    // A peak reached in the last few hundred microseconds of a process's
    // life escapes both the 250us VmHWM poll and the periodic samples, and
    // `/proc/<pid>` stops answering the moment `wait4` reaps it — so
    // neither can be recovered afterwards. `wait4`'s ru_maxrss is exact, and
    // when the phase ran no descendants there is nothing else folded into
    // it, which is what makes folding it into `rss_max_kib` sound.
    //
    // The allocation is what the child realistically carries: its own
    // startup footprint plus 64 MiB. Asserting on the allocation alone is
    // not enough — the child's baseline already clears 64 MiB, so the test
    // would pass without ever touching the fold. The real witness is that
    // `rss_max_kib` now agrees with the kernel's exact lifetime mark; every
    // other peak source is sampled and under-reports it.
    // 16 MiB, deliberately WELL BELOW `LIFETIME_FOLD_TOLERANCE_KIB` (64 MiB).
    // This is the final-tick allocation the fold exists to recover: it must be
    // accepted even when the VmHWM poll misses it. At 64 MiB the margin was
    // exactly the tolerance, so whether the fold accepted depended on
    // millisecond timing in the child's last tick — the assertion passed for the
    // wrong reason on a fast machine.
    const ALLOCATION_KIB: u64 = 16 * 1024;
    let fixture = tempfile::tempdir().unwrap();
    let config = WorkloadConfig {
        binary: std::env::current_exe().unwrap(),
        fixture: fixture.path().to_path_buf(),
        sample_interval: Duration::from_millis(500),
        verbose: false,
        worker_capable: false,
        heap_profile_dir: None,
    };
    let report = run_command_phase(&config, "final_tick", |bin, _| {
        let mut command = Command::new(bin);
        command
            .args(["--exact", "workload::tests::test_command_peak_child"])
            .env("MEMCHECK_PEAK_CHILD", "transient")
            .env("MEMCHECK_PEAK_CHILD_KIB", ALLOCATION_KIB.to_string())
            .env("MEMCHECK_PEAK_CHILD_SLEEP_MS", "0");
        command
    })
    .unwrap();
    assert_eq!(
        report.descendants.total, 0,
        "this phase must run no descendants for ru_maxrss to be a main-process peak"
    );
    assert!(
        report.descendants.peak_rss_kib >= ALLOCATION_KIB,
        "the kernel lifetime peak {} KiB must include the {ALLOCATION_KIB} KiB allocation",
        report.descendants.peak_rss_kib
    );
    assert!(
        report.rss_max_kib >= report.descendants.peak_rss_kib,
        "rss_max_kib {} KiB must not under-report the main process's own peak ({} KiB) — \
         the final tick is missing",
        report.rss_max_kib,
        report.descendants.peak_rss_kib
    );
}

#[test]
fn test_build_phase_report_with_samples() {
    let mut samples = vec![
        sampler::MemorySample {
            rss_kib: 100,
            mapped_file_kib: 10,
            anon_kib: 90,
            pss_kib: 100,
            worker_rss_kib: 0,
            gpu: sampler::GpuSample::default(),
        },
        sampler::MemorySample {
            rss_kib: 200,
            mapped_file_kib: 20,
            anon_kib: 180,
            pss_kib: 200,
            worker_rss_kib: 50,
            gpu: sampler::GpuSample::default(),
        },
        sampler::MemorySample {
            rss_kib: 150,
            mapped_file_kib: 15,
            anon_kib: 135,
            pss_kib: 150,
            worker_rss_kib: 30,
            gpu: sampler::GpuSample::default(),
        },
    ];
    let report = build_phase_report("test", &mut samples, Duration::from_millis(500));
    assert_eq!(report.phase, "test");
    assert_eq!(report.sample_count, 3);
    assert_eq!(report.rss_min_kib, 100);
    assert_eq!(report.rss_max_kib, 200);
    assert!(report.rss_p95_kib >= 100);
    assert!(report.rss_p95_kib <= 200);
    assert_eq!(report.duration_ms, 500);
    // Peak mapped_file and anon
    assert_eq!(report.mapped_file_kib, 20);
    assert_eq!(report.anon_kib, 180);
    // Worker-aware metrics
    assert_eq!(report.worker_rss_max_kib, 50);
    assert_eq!(report.combined_rss_max_kib, 250); // 200 + 50
}

#[test]
fn test_build_phase_report_p95() {
    // With 20 samples, p95 should be the 19th value when sorted.
    let mut samples: Vec<sampler::MemorySample> = (1..=20)
        .map(|i| sampler::MemorySample {
            rss_kib: i * 10,
            mapped_file_kib: 0,
            anon_kib: 0,
            pss_kib: i * 10,
            worker_rss_kib: 0,
            gpu: sampler::GpuSample::default(),
        })
        .collect();
    let report = build_phase_report("test", &mut samples, Duration::from_secs(1));
    // p95 of 20 samples: index = ceil(20*0.95) = 19, so 19th value = 190
    assert_eq!(report.rss_p95_kib, 190);
}

#[test]
fn test_build_phase_report_peak_mapped_anon() {
    let mut samples = vec![
        sampler::MemorySample {
            rss_kib: 100,
            mapped_file_kib: 50,
            anon_kib: 50,
            pss_kib: 100,
            worker_rss_kib: 0,
            gpu: sampler::GpuSample::default(),
        },
        sampler::MemorySample {
            rss_kib: 120,
            mapped_file_kib: 80,
            anon_kib: 40,
            pss_kib: 120,
            worker_rss_kib: 0,
            gpu: sampler::GpuSample::default(),
        },
        sampler::MemorySample {
            rss_kib: 110,
            mapped_file_kib: 30,
            anon_kib: 80,
            pss_kib: 110,
            worker_rss_kib: 0,
            gpu: sampler::GpuSample::default(),
        },
    ];
    let report = build_phase_report("test", &mut samples, Duration::from_secs(1));
    assert_eq!(report.mapped_file_kib, 80); // peak
    assert_eq!(report.anon_kib, 80); // peak
}

#[test]
fn test_build_phase_report_worker_aware() {
    // Test that worker RSS is tracked and combined correctly
    let mut samples = vec![
        sampler::MemorySample {
            rss_kib: 50000,
            mapped_file_kib: 0,
            anon_kib: 0,
            pss_kib: 0,
            worker_rss_kib: 0,
            gpu: sampler::GpuSample::default(),
        },
        sampler::MemorySample {
            rss_kib: 60000,
            mapped_file_kib: 0,
            anon_kib: 0,
            pss_kib: 0,
            worker_rss_kib: 80000,
            gpu: sampler::GpuSample::default(),
        },
        sampler::MemorySample {
            rss_kib: 55000,
            mapped_file_kib: 0,
            anon_kib: 0,
            pss_kib: 0,
            worker_rss_kib: 90000,
            gpu: sampler::GpuSample::default(),
        },
    ];
    let report = build_phase_report("embed_active", &mut samples, Duration::from_secs(1));
    assert_eq!(report.rss_max_kib, 60000);
    assert_eq!(report.worker_rss_max_kib, 90000);
    assert_eq!(report.combined_rss_max_kib, 145000); // 55000 + 90000
}

// ── Worker-reaping cleanup tests ───────────────────────────────────
//
// These tests exercise the helper functions used by `kill_child` to
// scan /proc for orphaned embed workers after a phase ends.

#[test]
fn test_read_proc_ppid_for_self() {
    let pid = std::process::id();
    let ppid = read_proc_ppid(pid);
    // ppid should be a positive value (the test runner or shell).
    assert!(ppid > 0, "ppid should be positive, got {}", ppid);
}

#[test]
fn test_read_proc_ppid_missing_pid() {
    // A PID that almost certainly does not exist.
    let ppid = read_proc_ppid(u32::MAX);
    assert_eq!(ppid, 0, "missing pid should return 0 ppid");
}

#[test]
fn test_find_worker_pids_finds_no_workers_for_self() {
    // The memcheck test process should not have any embed-worker children
    // (a worker is only ever a `leindex` child carrying the argv token).
    let pid = std::process::id();
    let workers = find_worker_pids(pid);
    assert!(
        workers.is_empty(),
        "expected no worker children for test process, found {:?}",
        workers
    );
}

#[test]
fn test_reap_worker_processes_no_crash_with_no_workers() {
    // Ensures the reaper is a no-op when no workers are running.
    reap_worker_processes(&[]);
}
