use super::*;

#[test]
fn test_background_pool_is_small_and_runs_at_lower_priority() {
    let pool = background_pool();
    assert!((1..=2).contains(&pool.current_num_threads()));
    let name = pool.install(|| std::thread::current().name().map(str::to_owned));
    assert!(name.unwrap_or_default().starts_with("leindex-refresh-"));
    #[cfg(target_os = "linux")]
    {
        let nice = pool.install(|| unsafe {
            let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
            libc::getpriority(libc::PRIO_PROCESS, tid)
        });
        assert!(nice >= 10, "refresh thread nice was {nice}");
    }
}

#[tokio::test]
async fn test_join_error_cancellation_does_not_mark_failure() {
    // Audit Issue 5: a cancelled index task must not poison freshness.
    let cancelled = tokio::spawn(std::future::pending::<()>());
    cancelled.abort();
    let error = cancelled.await.unwrap_err();
    assert!(error.is_cancelled());
    assert!(!join_error_should_mark_failure(&error));
}

#[tokio::test]
async fn test_join_error_panic_marks_failure() {
    let panicking = tokio::spawn(async { panic!("pipeline defect") });
    let error = panicking.await.unwrap_err();
    assert!(error.is_panic());
    assert!(join_error_should_mark_failure(&error));
}

#[test]
fn test_registry_heap_budget_env_parse() {
    // Default budget: 1536 MiB.
    assert_eq!(registry_heap_budget_bytes(), 1536 * 1024 * 1024);
}

#[cfg(all(unix, feature = "onnx"))]
#[test]
fn test_terminate_superseded_daemons_cleans_stale_artifacts() {
    // Stale pid files (dead/missing pid) must have their socket/status
    // artifacts removed; the keep-socket's own pid file must survive.
    let dir = tempfile::tempdir().unwrap();
    let keep_socket = dir.path().join("leindex-embed-aaaa.sock");
    let keep_pid = dir.path().join("leindex-embed-aaaa.pid");
    std::fs::write(&keep_pid, "1234\n").unwrap();

    let stale_pid = dir.path().join("leindex-embed-bbbb.pid");
    std::fs::write(&stale_pid, "999999999\n").unwrap(); // no such pid
    let stale_socket = dir.path().join("leindex-embed-bbbb.sock");
    let stale_status = dir.path().join("leindex-embed-bbbb.status");
    std::fs::write(&stale_socket, b"x").unwrap();
    std::fs::write(&stale_status, b"x").unwrap();

    crate::search::onnx::client::terminate_superseded_daemons(&keep_socket);

    assert!(keep_pid.exists(), "keep-socket pid file must survive");
    assert!(!stale_pid.exists(), "stale pid file removed");
    assert!(!stale_socket.exists(), "stale socket removed");
    assert!(!stale_status.exists(), "stale status removed");
}

#[test]
fn test_restore_latest_generation_removes_stale_wal_sidecars() {
    // The permanent-brick vector: after swapping a generation's DB over the
    // live leindex.db, the OLD database's -wal/-shm side files must be
    // removed. Their salts are keyed to the replaced main file, so SQLite
    // refuses to open (or "recovers" garbage from) the mismatched pair —
    // every subsequent tool call then fails -32008 forever.
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path().join("store");
    let gen_dir = storage.join("generations").join("7");
    std::fs::create_dir_all(&gen_dir).unwrap();

    // Valid generation DB (contents never opened by restore, only copied).
    std::fs::write(gen_dir.join("leindex.db"), b"generation-db-bytes").unwrap();
    // Live DB plus stale side files from the pre-swap database.
    std::fs::write(storage.join("leindex.db"), b"old-live-db-bytes").unwrap();
    std::fs::write(storage.join("leindex.db-wal"), b"stale-wal").unwrap();
    std::fs::write(storage.join("leindex.db-shm"), b"stale-shm").unwrap();

    assert!(restore_latest_generation(&storage));
    assert_eq!(
        std::fs::read(storage.join("leindex.db")).unwrap(),
        b"generation-db-bytes"
    );
    assert!(
        !storage.join("leindex.db-wal").exists(),
        "stale WAL sidecar must be removed with the swapped DB"
    );
    assert!(
        !storage.join("leindex.db-shm").exists(),
        "stale SHM sidecar must be removed with the swapped DB"
    );
    assert_eq!(
        std::fs::read_to_string(storage.join("CURRENT"))
            .unwrap()
            .trim(),
        "7"
    );
}

#[tokio::test]
async fn test_registry_creation() {
    let registry = ProjectRegistry::new(5);
    assert_eq!(registry.len().await, 0);
}

#[tokio::test]
async fn test_get_or_create_skips_auto_index_within_failure_cooldown() {
    // A recent failed auto-index must suppress further full-index attempts
    // (coalescing the degraded state) while get_or_create still succeeds
    // so read/edit tools can degrade gracefully instead of erroring.
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
    let canonical = dir.path().canonicalize().unwrap();

    let registry = Arc::new(ProjectRegistry::new(5));
    // Simulate an auto-index attempt that failed moments ago.
    registry
        .failed_index_attempts
        .write()
        .await
        .insert(canonical.clone(), std::time::Instant::now());

    let handle = registry
        .get_or_create(Some(&canonical.to_string_lossy()))
        .await
        .expect("get_or_create must not fail when the index attempt is skipped");
    assert!(
        !handle.read().await.is_indexed(),
        "full index attempt must be skipped within the failure cooldown"
    );

    // An explicit index request still works and clears the failure marker.
    registry
        .index_project(Some(&canonical.to_string_lossy()), false)
        .await
        .expect("explicit index must succeed on a healthy project");
    assert!(
        !registry
            .failed_index_attempts
            .read()
            .await
            .contains_key(&canonical),
        "a successful index must clear the failure marker"
    );
}

#[test]
fn post_core_failure_preserves_published_health() {
    let temp = tempfile::tempdir().unwrap();
    let storage = temp.path().join(".leindex");
    let health = crate::cli::leindex::IndexHealth {
        generation: 4,
        phase: crate::cli::leindex::IndexPhase::Complete,
        status: crate::cli::leindex::ComponentStatus::Fresh,
        indexed_at_unix_ms: Some(1),
        ..Default::default()
    };
    crate::cli::index_freshness::save_health(&storage, &health).unwrap();

    mark_index_failure(temp.path(), "neural failed", true);

    let recorded = crate::cli::index_freshness::load_health(&storage).unwrap();
    assert_eq!(recorded.status, crate::cli::leindex::ComponentStatus::Fresh);
    assert_eq!(
        recorded.last_failure_phase,
        Some(crate::cli::leindex::IndexPhase::Neural)
    );
    assert_eq!(recorded.last_failure.as_deref(), Some("neural failed"));
}

#[test]
fn restore_latest_generation_preserves_corrupt_root() {
    let temp = tempfile::tempdir().unwrap();
    let storage = temp.path().join(".leindex");
    let generation = storage.join("generations/3");
    std::fs::create_dir_all(&generation).unwrap();
    std::fs::write(storage.join("leindex.db"), b"corrupt").unwrap();
    std::fs::write(generation.join("leindex.db"), b"usable").unwrap();
    assert!(restore_latest_generation(&storage));
    assert_eq!(
        std::fs::read(storage.join("leindex.db")).unwrap(),
        b"usable"
    );
    assert_eq!(
        std::fs::read(storage.join("leindex.db.corrupt-3")).unwrap(),
        b"corrupt"
    );
}

#[tokio::test]
async fn test_resolve_path_defaults_to_cwd_without_startup_project() {
    // No explicit arg and no startup --project: resolution must fall back
    // to the process CWD (the MCP client's workspace) — never to $HOME or
    // to whichever project another tool touched most recently.
    let registry = ProjectRegistry::new(5);
    let resolved = registry.resolve_path(None).await.unwrap();
    let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
    assert_eq!(resolved, expected);
}

#[tokio::test]
async fn test_resolve_path_startup_project_beats_cwd() {
    // A daemon started with `-p`/`--project` must keep serving path-less
    // calls from that designation even when its process CWD differs.
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();
    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let resolved = registry.resolve_path(None).await.unwrap();
    assert_eq!(resolved, tmp.path().canonicalize().unwrap());
}

#[tokio::test]
async fn test_resolve_path_explicit_arg_beats_everything() {
    // An explicit per-call project_path always wins over both the startup
    // designation and the CWD fallback.
    let tmp1 = tempfile::tempdir().unwrap();
    std::fs::write(tmp1.path().join("main.rs"), "fn one() {}\n").unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    std::fs::write(tmp2.path().join("main.rs"), "fn two() {}\n").unwrap();

    let leindex = LeIndex::new(tmp1.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let resolved = registry
        .resolve_path(Some(tmp2.path().to_str().unwrap()))
        .await
        .unwrap();
    assert_eq!(resolved, tmp2.path().canonicalize().unwrap());
}

#[tokio::test]
async fn test_resolve_path_rejects_home_directory() {
    let registry = ProjectRegistry::new(5);
    let home = dirs::home_dir().expect("test requires a resolvable home directory");
    let result = registry
        .resolve_path(Some(home.to_str().unwrap()))
        .await
        .unwrap_err();
    assert!(
        result.message.contains("home"),
        "expected home-dir rejection, got: {}",
        result.message
    );
}

#[tokio::test]
async fn test_registry_nonexistent_path_error() {
    let registry = ProjectRegistry::new(5);
    let result = registry.get_or_load(Some("/nonexistent/path/12345")).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_registry_with_initial_project() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    assert_eq!(registry.len().await, 1);
    let handle = registry.get_or_load(None).await;
    assert!(handle.is_ok());
}

#[tokio::test]
async fn core_generation_refreshes_the_resident_handle() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn resident_core() {}\n").unwrap();

    let mut builder = LeIndex::new(tmp.path()).unwrap();
    builder.index_project(true).unwrap();
    drop(builder);

    let resident = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(2, resident);
    let canonical = tmp.path().canonicalize().unwrap();
    let handle = registry
        .get_or_load(Some(canonical.to_str().unwrap()))
        .await
        .unwrap();
    assert!(handle.read().await.pdg().is_none());

    registry
        .refresh_loaded_from_active_generation(&canonical)
        .await
        .unwrap();
    let guard = handle.read().await;
    assert!(guard.pdg().is_some());
    assert!(guard.search_engine().node_count() > 0);
}

#[tokio::test]
async fn test_registry_same_project_returns_same_handle() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let path_str = tmp.path().to_string_lossy().to_string();
    let h1 = registry.get_or_load(Some(&path_str)).await.unwrap();
    let h2 = registry.get_or_load(Some(&path_str)).await.unwrap();

    assert!(Arc::ptr_eq(&h1, &h2));
}

#[tokio::test]
async fn test_registry_two_different_projects() {
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    std::fs::write(tmp1.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(tmp2.path().join("b.rs"), "fn b() {}\n").unwrap();

    let leindex = LeIndex::new(tmp1.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let p2 = tmp2.path().to_string_lossy().to_string();
    let h2 = registry.get_or_load(Some(&p2)).await.unwrap();

    assert_eq!(registry.len().await, 2);

    let p1 = tmp1.path().to_string_lossy().to_string();
    let h1 = registry.get_or_load(Some(&p1)).await.unwrap();
    assert!(!Arc::ptr_eq(&h1, &h2));
}

#[tokio::test]
async fn test_registry_eviction_at_capacity() {
    let dirs: Vec<_> = (0..3)
        .map(|i| {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join(format!("f{}.rs", i)), "fn f() {}\n").unwrap();
            d
        })
        .collect();

    let leindex = LeIndex::new(dirs[0].path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(2, leindex);

    let p1 = dirs[1].path().to_string_lossy().to_string();
    let _ = registry.get_or_load(Some(&p1)).await.unwrap();
    assert_eq!(registry.len().await, 2);

    let p2 = dirs[2].path().to_string_lossy().to_string();
    let _ = registry.get_or_load(Some(&p2)).await.unwrap();
    assert_eq!(registry.len().await, 2);

    let loaded = registry.loaded_projects().await;
    let canonical0 = dirs[0].path().canonicalize().unwrap();
    assert!(!loaded.contains(&canonical0));
}

#[tokio::test]
async fn test_registry_evicted_project_reloads() {
    let dirs: Vec<_> = (0..3)
        .map(|i| {
            let d = tempfile::tempdir().unwrap();
            std::fs::write(d.path().join(format!("f{}.rs", i)), "fn f() {}\n").unwrap();
            d
        })
        .collect();

    let leindex = LeIndex::new(dirs[0].path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(2, leindex);

    let p1 = dirs[1].path().to_string_lossy().to_string();
    let _ = registry.get_or_load(Some(&p1)).await.unwrap();

    let p2 = dirs[2].path().to_string_lossy().to_string();
    let _ = registry.get_or_load(Some(&p2)).await.unwrap();

    let p0 = dirs[0].path().to_string_lossy().to_string();
    let result = registry.get_or_load(Some(&p0)).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_registry_default_project_never_tracks_last_used() {
    // The old last-touched-wins behavior made a path-less call silently
    // bind to whichever project another tool touched most recently —
    // the reported cross-project binding bug. The startup designation is
    // immutable: touching tmp2 must not move it.
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    std::fs::write(tmp1.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(tmp2.path().join("b.rs"), "fn b() {}\n").unwrap();

    let leindex = LeIndex::new(tmp1.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let h1 = registry.get_or_load(None).await.unwrap();
    let path1 = h1.read().await.project_path().to_path_buf();
    assert_eq!(path1, tmp1.path().canonicalize().unwrap());

    let p2 = tmp2.path().to_string_lossy().to_string();
    let _ = registry.get_or_load(Some(&p2)).await.unwrap();

    // Path-less resolution still lands on the startup project...
    let h2 = registry.get_or_load(None).await.unwrap();
    let path2 = h2.read().await.project_path().to_path_buf();
    assert_eq!(path2, tmp1.path().canonicalize().unwrap());

    // ...and resolve_path agrees.
    assert_eq!(
        registry.resolve_path(None).await.unwrap(),
        tmp1.path().canonicalize().unwrap()
    );
}

/// Concurrency test: verify that the `ProjectRwLock` wrapper correctly
/// serializes access (both `read()` and `write()` acquire the underlying
/// mutex) and that concurrent operations from multiple tokio tasks
/// complete without deadlock or data corruption.
#[tokio::test]
async fn test_project_rwlock_concurrent_access_no_deadlock() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let handle = registry.get_or_load(None).await.unwrap();

    // Spawn multiple concurrent tasks that acquire read guards.
    // All should complete without deadlock (they are serialized by
    // the underlying mutex, but the tokio runtime can interleave them).
    let mut handles = Vec::new();
    for i in 0..10 {
        let h = handle.clone();
        handles.push(tokio::spawn(async move {
            // Alternating read and write to exercise both paths
            if i % 2 == 0 {
                let guard = h.read().await;
                let path = guard.project_path().to_path_buf();
                assert!(path.exists());
            } else {
                let guard = h.write().await;
                let path = guard.project_path().to_path_buf();
                assert!(path.exists());
            }
        }));
    }

    // All tasks must complete without deadlock
    for h in handles {
        h.await.unwrap();
    }
}

/// Verify that `try_write()` returns Err when the lock is already held.
#[tokio::test]
async fn test_project_rwlock_try_write_returns_err_when_locked() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let handle = registry.get_or_load(None).await.unwrap();

    // Acquire a read guard and hold it
    let _guard = handle.read().await;

    // try_write should fail because the lock is held
    let result = handle.try_write();
    assert!(result.is_err());
}

/// Verify that `blocking_write()` works from a spawn_blocking context.
#[test]
fn test_project_rwlock_blocking_write() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let handle: ProjectHandle = Arc::new(ProjectRwLock::new(leindex));

    let h = handle.clone();
    let result = std::thread::spawn(move || {
        let guard = h.blocking_write();
        guard.project_path().to_path_buf()
    })
    .join()
    .unwrap();

    assert!(result.exists());
}

// ---- A+ registry slot eviction tests (VAL-APLUS-027, VAL-APLUS-028) ----

/// VAL-APLUS-027: Registry slot bookkeeping is evicted on project unregister/evict.
///
/// When a project leaves the live registry, its slot bookkeeping is removed
/// so residency does not grow monotonically across long-lived sessions.
#[tokio::test]
async fn test_evict_removes_slot_bookkeeping() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let canonical = tmp.path().canonicalize().unwrap();
    assert_eq!(registry.len().await, 1);

    // Evict the project
    registry.evict(&canonical).await;
    assert_eq!(registry.len().await, 0);

    // Verify slot bookkeeping is gone (internal state check via re-load)
    // Re-loading should work cleanly without stale slot state
    let path_str = tmp.path().to_string_lossy().to_string();
    let result = registry.get_or_load(Some(&path_str)).await;
    assert!(result.is_ok(), "re-loading after eviction should succeed");
    assert_eq!(registry.len().await, 1);
}

/// VAL-APLUS-028: Registry slot map reflects only live projects.
///
/// Slot bookkeeping tracks active projects rather than every project ever
/// seen in the process lifetime.
#[tokio::test]
async fn test_slot_map_reflects_only_live_projects() {
    let tmp1 = tempfile::tempdir().unwrap();
    let tmp2 = tempfile::tempdir().unwrap();
    std::fs::write(tmp1.path().join("a.rs"), "fn a() {}\n").unwrap();
    std::fs::write(tmp2.path().join("b.rs"), "fn b() {}\n").unwrap();

    let leindex = LeIndex::new(tmp1.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    // Load second project
    let p2 = tmp2.path().to_string_lossy().to_string();
    let _ = registry.get_or_load(Some(&p2)).await.unwrap();
    assert_eq!(registry.len().await, 2);

    // Evict first project
    let canonical1 = tmp1.path().canonicalize().unwrap();
    registry.evict(&canonical1).await;
    assert_eq!(registry.len().await, 1);

    // Only the second project should remain
    let loaded = registry.loaded_projects().await;
    let canonical2 = tmp2.path().canonicalize().unwrap();
    assert!(loaded.contains(&canonical2));
    assert!(!loaded.contains(&canonical1));
}

/// VAL-APLUS-027 variant: stale-cache entries are cleaned up on evict.
#[tokio::test]
async fn test_evict_cleans_stale_cache() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let canonical = tmp.path().canonicalize().unwrap();

    // Populate stale cache
    registry
        .stale_cache
        .write()
        .await
        .insert(canonical.clone(), (std::time::Instant::now(), false));

    assert!(registry.stale_cache.read().await.contains_key(&canonical));

    // Evict should clean up stale cache
    registry.evict(&canonical).await;
    assert!(
        !registry.stale_cache.read().await.contains_key(&canonical),
        "stale cache entry should be removed on evict"
    );
}

/// Regression for P2 round 15 (codex `3344884534`): write
/// handlers (`edit-apply`, `write-file`, `rename-symbol`) must
/// invalidate the staleness cache after a successful write so
/// that the next read tool re-runs `is_stale_fast` instead of
/// reusing a pre-write `false` cached result. The watcher
/// (when enabled) does this on its own reindex path; the
/// explicit call covers the watcher-disabled default mode
/// where the 30-second negative-cache TTL would otherwise
/// silently mask the edit.
#[tokio::test]
async fn test_invalidate_stale_cache_removes_entry() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let canonical = tmp.path().canonicalize().unwrap();

    // Prime the cache with a `false` result (the scenario
    // the codex comment describes: a previous read tool ran
    // `is_stale_fast` and got back `false`).
    registry
        .stale_cache
        .write()
        .await
        .insert(canonical.clone(), (std::time::Instant::now(), false));
    assert!(registry.stale_cache.read().await.contains_key(&canonical));

    // The write handler calls this after the disk write.
    registry.invalidate_stale_cache(&canonical).await;

    assert!(
        !registry.stale_cache.read().await.contains_key(&canonical),
        "stale cache entry must be removed on invalidate"
    );
}

/// `invalidate_stale_cache` requires an already-canonicalized
/// path. The cache key is built from `LeIndex::project_path`,
/// which is canonicalized at construction, so callers must pass
/// `guard.project_path().to_path_buf()` (or equivalent).
#[tokio::test]
async fn test_invalidate_stale_cache_requires_canonical_input() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("main.rs"), "fn main() {}\n").unwrap();

    let leindex = LeIndex::new(tmp.path()).unwrap();
    let registry = ProjectRegistry::with_initial_project(5, leindex);

    let canonical = tmp.path().canonicalize().unwrap();
    registry
        .stale_cache
        .write()
        .await
        .insert(canonical.clone(), (std::time::Instant::now(), false));

    // Must pass the canonicalized path — the function no longer
    // re-canonicalizes internally.
    registry.invalidate_stale_cache(&canonical).await;
    assert!(
        !registry.stale_cache.read().await.contains_key(&canonical),
        "stale cache entry must be removed on invalidate with canonical input"
    );
}

// ── Background pre-warm ──────────────────────────────────────────────

fn write_prewarm_fixture(dir: &std::path::Path) {
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("src/lib.rs"),
        "pub fn authenticate_user(name: &str) -> bool {\n    !name.is_empty()\n}\n\npub fn hash_password(p: &str) -> u64 {\n    p.len() as u64\n}\n",
    )
    .unwrap();
}

#[tokio::test]
async fn test_prewarm_leaves_an_unindexed_project_alone() {
    let dir = tempfile::tempdir().unwrap();
    write_prewarm_fixture(dir.path());
    let registry = Arc::new(ProjectRegistry::new(2));
    registry
        .set_default_path(dir.path().canonicalize().unwrap())
        .await;
    registry.spawn_prewarm();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(
        registry.len().await,
        0,
        "an unindexed project is never loaded"
    );
    assert!(
        !dir.path().join(".leindex").exists(),
        "prewarm must not create storage"
    );
}

#[tokio::test]
async fn test_prewarm_loads_graph_and_engine_of_an_indexed_project() {
    let dir = tempfile::tempdir().unwrap();
    write_prewarm_fixture(dir.path());
    let root = dir.path().canonicalize().unwrap();
    {
        let mut index = LeIndex::new(&root).unwrap();
        index.index_project(true).unwrap();
    }
    let registry = Arc::new(ProjectRegistry::new(2));
    registry.set_default_path(root.clone()).await;
    registry.spawn_prewarm();
    registry.spawn_prewarm(); // idempotent

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        if let Some(handle) = registry.try_get_loaded(&root).await {
            let index = handle.read().await;
            if index.pdg().is_some() && !index.search_engine().is_empty() {
                break;
            }
        }
        assert!(
            std::time::Instant::now() < deadline,
            "prewarm never completed"
        );
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn test_one_shot_registry_never_prewarms() {
    let dir = tempfile::tempdir().unwrap();
    write_prewarm_fixture(dir.path());
    let root = dir.path().canonicalize().unwrap();
    {
        let mut index = LeIndex::new(&root).unwrap();
        index.index_project(true).unwrap();
    }
    let registry = Arc::new(ProjectRegistry::new(2));
    registry.mark_one_shot();
    registry.set_default_path(root).await;
    registry.spawn_prewarm();
    tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    assert_eq!(registry.len().await, 0);
}
