//! Durable checkpoint/recovery contract tests.

#![cfg(feature = "cli")]
use leindex::cli::index_job::{
    CheckpointStore, FileFingerprint, JobCheckpointState, JobStatus, LexicalCheckpoint,
    NeuralCheckpoint, ParseCheckpoint, ParsedFileCheckpoint, ScanCheckpoint,
};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

/// Sync env-test lock used by `#[test]` callers and by sync blocks *inside*
/// `#[tokio::test]` callers. `std::sync::Mutex` is safe to acquire here
/// because the guarded region contains no `.await`. Async tests must NOT
/// hold this guard across `.await` (clippy::await_holding_lock).
fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn test_resume_each_phase() {
    let temp = tempfile::tempdir().expect("checkpoint tempdir");
    let store = CheckpointStore::new(temp.path(), 7);
    let scan = ScanCheckpoint {
        input_hash: "scan".to_string(),
        files: vec![FileFingerprint {
            canonical_path: PathBuf::from("src/lib.rs"),
            blake3: "file".to_string(),
            bytes: 10,
            language: "rust".to_string(),
        }],
    };

    let scan_hash = store.write_scan(&scan).expect("persist scan");
    let parsed_hash = store
        .write_parsed(
            "file",
            &ParsedFileCheckpoint {
                file_path: PathBuf::from("src/lib.rs"),
                language: "rust".to_string(),
                signatures: Vec::new(),
                parse_time_ms: 1,
            },
        )
        .expect("persist parse");
    let parse_hash = store
        .write_parse(&ParseCheckpoint {
            scan_hash: scan.input_hash.clone(),
            artifact_paths: vec![store.paths.parsed("file")],
            artifact_hashes: [("file".to_string(), parsed_hash.clone())]
                .into_iter()
                .collect(),
        })
        .expect("persist parse phase");
    assert!(
        store
            .read_parsed_verified("file", &parsed_hash)
            .expect("verify parse artifact")
            .is_some()
    );
    assert!(
        store
            .read_parsed_verified("file", "wrong-hash")
            .expect("reject wrong parse artifact hash")
            .is_none()
    );
    let pdg = leindex::graph::pdg::ProgramDependenceGraph::default();
    let pdg_checkpoint = store
        .write_pdg("scan".to_string(), &pdg)
        .expect("persist pdg");
    let snapshot_path = temp.path().join("search_snapshot.bin");
    let tfidf_path = temp.path().join("tfidf_embedder.bin");
    std::fs::write(&snapshot_path, b"snapshot").expect("snapshot artifact");
    std::fs::write(&tfidf_path, b"tfidf").expect("tfidf artifact");
    let lexical_hash = store
        .write_lexical(&LexicalCheckpoint {
            pdg_hash: pdg_checkpoint.artifact_hash.clone(),
            snapshot_path,
            tfidf_path,
            admitted_node_ids: vec![
                "node-z".to_string(),
                "node-a".to_string(),
                "node-a".to_string(),
            ],
        })
        .expect("persist lexical");
    let neural_hash = store
        .write_neural(&NeuralCheckpoint {
            lexical_hash,
            mmap_path: PathBuf::from("neural_embeddings.bin"),
            rows: 0,
            provider: "unavailable".to_string(),
            model: "none".to_string(),
        })
        .expect("persist neural");

    let mut state = JobCheckpointState {
        job_id: "job-7".to_string(),
        input_generation: 6,
        ..JobCheckpointState::default()
    };
    state.artifact_hashes.extend([
        ("scan".to_string(), scan_hash),
        ("parse".to_string(), parse_hash),
        ("pdg".to_string(), pdg_checkpoint.artifact_hash.clone()),
        ("neural".to_string(), neural_hash),
    ]);
    state.last_reusable_phase = Some("neural".to_string());
    store.write_state(&state).expect("persist state");

    // A fresh process can load every reusable artifact and resume from the
    // last phase without re-running scan, parse, or PDG construction.
    assert_eq!(store.read_scan().expect("load scan"), Some(scan.clone()));
    assert!(store.read_parsed("file").expect("load parse").is_some());
    assert_eq!(
        store
            .read_parse()
            .expect("load parse phase")
            .unwrap()
            .scan_hash,
        scan.input_hash
    );
    assert!(
        store
            .read_pdg_artifact(state.artifact_hashes.get("pdg").unwrap())
            .expect("load pdg")
            .is_some()
    );
    let lexical = store
        .read_lexical()
        .expect("load lexical")
        .expect("lexical checkpoint");
    assert_eq!(lexical.pdg_hash, pdg_checkpoint.artifact_hash);
    assert_eq!(lexical.admitted_node_ids, ["node-a", "node-z"]);
    assert_eq!(store.read_neural().expect("load neural").unwrap().rows, 0);
    assert_eq!(
        store.read_state().expect("load state").unwrap().job_id,
        "job-7"
    );
}

#[test]
fn test_bucketed_parse_artifacts_keep_path_identity() {
    let temp = tempfile::tempdir().expect("checkpoint tempdir");
    let store = CheckpointStore::new(temp.path(), 8);
    let parsed = ParsedFileCheckpoint {
        file_path: PathBuf::from("src/lib.rs"),
        language: "rust".to_string(),
        signatures: Vec::new(),
        parse_time_ms: 1,
    };
    let mut bucket = BTreeMap::new();
    bucket.insert("abcdef".to_string(), vec![parsed.clone()]);
    let hash = store
        .write_parsed_batch("ab", &bucket)
        .expect("persist parse bucket");
    assert!(
        store
            .read_parsed_for_path_verified("abcdef", &hash, PathBuf::from("src/lib.rs").as_path())
            .expect("read parse bucket")
            .is_some()
    );
    assert!(
        store
            .read_parsed_for_path_verified("abcdef", &hash, PathBuf::from("src/main.rs").as_path())
            .expect("reject wrong path")
            .is_none()
    );
}

#[test]
fn test_lexical_failure_keeps_core_current_and_restart_reuses_checkpoint() {
    let _env_lock = env_test_lock();
    let temp = tempfile::tempdir().expect("project tempdir");
    std::fs::create_dir_all(temp.path().join("src")).expect("source directory");
    std::fs::write(
        temp.path().join("src/main.rs"),
        "fn main() { println!(\"checkpoint\"); }\n",
    )
    .expect("source file");

    let mut first = leindex::cli::leindex::LeIndex::new(temp.path()).expect("create index");
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("LEINDEX_INJECT_FAILURE_PHASE", "lexical") };
    let first_result = first.index_project(true);
    // FIXME: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::remove_var("LEINDEX_INJECT_FAILURE_PHASE") };
    assert!(first_result.is_err(), "failure injection must stop the job");

    let storage = temp.path().join(".leindex");
    let current = std::fs::read_to_string(storage.join("CURRENT")).expect("core CURRENT");
    assert_eq!(current.trim(), "1");
    assert!(storage.join("generations/1/search_snapshot.bin").is_file());

    let mut resumed = leindex::cli::leindex::LeIndex::new(temp.path()).expect("reopen index");
    resumed
        .index_project(false)
        .expect("resume from lexical checkpoint");
    let current_after = std::fs::read_to_string(storage.join("CURRENT")).expect("resumed CURRENT");
    let generation = current_after
        .trim()
        .parse::<u64>()
        .expect("CURRENT must contain a numeric generation");
    assert!(
        generation >= 2,
        "resume must publish a generation after the failed lexical attempt"
    );
    assert!(
        storage
            .join("generations")
            .join(generation.to_string())
            .join("leindex.db")
            .is_file()
    );
}

#[tokio::test]
async fn test_registry_hydrates_current_generation_not_mutable_root() {
    let temp = tempfile::tempdir().expect("project tempdir");
    std::fs::create_dir_all(temp.path().join("src")).expect("source directory");
    std::fs::write(
        temp.path().join("src/main.rs"),
        "fn current_generation() {}\n",
    )
    .expect("source file");

    {
        let _env_lock = env_test_lock();
        let mut first = leindex::cli::leindex::LeIndex::new(temp.path()).expect("create index");
        first.index_project(true).expect("publish generation");
    }

    // Simulate a process that opened the mutable root after a crash. The
    // immutable generation remains the only trustworthy query source.
    let storage = temp.path().join(".leindex");
    std::fs::rename(
        storage.join("leindex.db"),
        storage.join("leindex.db.interrupted"),
    )
    .expect("preserve mutable root");

    let registry = leindex::cli::registry::ProjectRegistry::new(2);
    let handle = registry
        .get_or_load(Some(temp.path().to_str().expect("utf8 project path")))
        .await
        .expect("hydrate current generation");
    assert!(handle.read().await.is_indexed());
    assert!(handle.read().await.get_stats().pdg_nodes > 0);
}

// ---------------------------------------------------------------------------
// Index-job lifecycle tests (VAL-JOBLIFECYCLE-001 .. VAL-JOBLIFECYCLE-004)
// ---------------------------------------------------------------------------

use leindex::cli::leindex::LeIndex;
use leindex::cli::registry::ProjectRegistry;
use std::sync::Arc;

/// Async-friendly RAII guard that holds the process-wide [`env_test_lock`]
/// (a `std::sync::Mutex`) for the lifetime of an async test.
///
/// The actual lock acquisition happens on a dedicated `spawn_blocking`
/// worker thread, and the returned guard is a plain struct (not a
/// `MutexGuard`), so `clippy::await_holding_lock` does not fire when it is
/// held across `.await`. This lets `#[tokio::test]` lifecycle tests
/// serialize with the sync `#[test]` `lexical_failure_keeps_core_current_and_restart_reuses_checkpoint`,
/// which sets `LEINDEX_INJECT_FAILURE_PHASE` process-wide while holding the
/// same std lock. Without this coordination, a parallel test's spawned
/// indexing pipeline would read the leaked env var and fail spuriously.
struct AsyncEnvTestLock {
    release: Option<tokio::sync::oneshot::Sender<()>>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

impl AsyncEnvTestLock {
    /// Acquire the process-wide env-test lock asynchronously. Returns once
    /// the lock has actually been acquired on the worker thread.
    async fn acquire() -> Self {
        let (acquired_tx, acquired_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            // Acquire the process-wide std env_test_lock. This blocks the
            // worker thread (not the async runtime) until the lock is free.
            let _guard = env_test_lock();
            // Notify the caller that the lock is held.
            let _ = acquired_tx.send(());
            // Wait for the release signal before returning (and dropping
            // the std lock). If the sender is dropped first (panic/abort),
            // the recv returns Err and we exit cleanly without deadlock.
            let _ = release_rx.blocking_recv();
        });
        // Wait for the worker to confirm acquisition before proceeding.
        let _ = acquired_rx.await;
        Self {
            release: Some(release_tx),
            worker: Some(worker),
        }
    }
}

impl Drop for AsyncEnvTestLock {
    fn drop(&mut self) {
        if let Some(tx) = self.release.take() {
            let _ = tx.send(());
        }
        // Detach the blocking worker thread. It will exit cleanly once it
        // receives the release signal above. We do not await the JoinHandle
        // here (Drop is sync); tokio runs spawn_blocking tasks to completion
        // even during runtime shutdown.
        if let Some(worker) = self.worker.take() {
            // Explicitly drop to detach rather than await.
            drop(worker);
        }
    }
}

/// RAII guard that restores an environment variable to its previous value
/// (or removes it) on drop. Used by the panic-recovery test to ensure
/// `LEINDEX_INJECT_PANIC` cannot leak into concurrent tests.
struct EnvVarGuard {
    key: String,
    original: Option<String>,
}

impl EnvVarGuard {
    fn set(key: &str, value: &str) -> Self {
        let original = std::env::var(key).ok();
        // FIXME: Audit that the environment access only happens in single-threaded code.
        unsafe { std::env::set_var(key, value) };
        Self {
            key: key.to_string(),
            original,
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.original {
            // FIXME: Audit that the environment access only happens in single-threaded code.
            Some(value) => unsafe { std::env::set_var(&self.key, value) },
            // FIXME: Audit that the environment access only happens in single-threaded code.
            None => unsafe { std::env::remove_var(&self.key) },
        }
    }
}

/// Build a tiny one-file Rust project under a fresh tempdir and return the
/// tempdir handle. The caller is responsible for keeping the tempdir alive
/// for the duration of the test.
fn make_tiny_rust_project() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("project tempdir");
    let src = temp.path().join("src");
    std::fs::create_dir_all(&src).expect("source directory");
    std::fs::write(
        src.join("main.rs"),
        "fn main() { println!(\"lifecycle-fixture\"); }\n",
    )
    .expect("source file");
    temp
}

/// VAL-JOBLIFECYCLE-001: Starting an index job, then dropping the returned
/// snapshot (simulating an MCP client disconnect), must NOT cancel the job.
/// The owned, detached task continues to run and reaches `complete` status
/// independently of the caller that started it.
#[tokio::test]
async fn test_disconnect_survival_job_continues_to_completion() {
    let _env_lock = AsyncEnvTestLock::acquire().await;
    let temp = make_tiny_rust_project();
    let registry = Arc::new(ProjectRegistry::new(2));
    let project = temp.path().to_str().expect("utf8 project path").to_string();

    // Start the job and immediately drop the snapshot, simulating an MCP
    // request that disconnects after starting the job but before polling.
    {
        let snapshot = registry
            .start_index_job(Some(&project), false, false)
            .await
            .expect("start job");
        assert_eq!(
            snapshot.status,
            JobStatus::Running,
            "freshly started job must report running status"
        );
        drop(snapshot);
    }

    // The detached indexing task must survive the caller dropping its
    // snapshot. Poll until the job reaches a terminal state.
    let mut attempts = 0usize;
    let max_attempts = 480; // 240 seconds at 500ms intervals
    let final_snapshot;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let snapshot = registry
            .get_index_job_snapshot(Some(&project))
            .await
            .expect("read job status")
            .expect("owned job status");
        if snapshot.status != JobStatus::Running {
            final_snapshot = snapshot;
            break;
        }
        attempts += 1;
        if attempts >= max_attempts {
            panic!(
                "disconnect-survival job never reached a terminal state after {} poll attempts (last status: {})",
                max_attempts, snapshot.status
            );
        }
    }

    assert_eq!(
        final_snapshot.status,
        JobStatus::Complete,
        "disconnect-survival job must reach complete status, got: {:?}",
        final_snapshot.last_error
    );
    assert!(
        final_snapshot.published.pdg,
        "PDG layer must be published after disconnect-survival"
    );
}

/// VAL-JOBLIFECYCLE-002: When two `start_index_job` calls are made for the
/// same project path while a job is running, the second call must return the
/// same `job_id` rather than starting a second job.
#[tokio::test]
async fn test_concurrent_requests_coalesce_into_single_job() {
    let _env_lock = AsyncEnvTestLock::acquire().await;
    let temp = make_tiny_rust_project();
    let registry = Arc::new(ProjectRegistry::new(2));
    let project = temp.path().to_str().expect("utf8 project path").to_string();

    // First call kicks off the indexing job.
    let snap1 = registry
        .start_index_job(Some(&project), false, false)
        .await
        .expect("first start_index_job");

    assert_eq!(
        snap1.status,
        JobStatus::Running,
        "the first start_index_job call must publish Running before coalescing"
    );

    // Second concurrent call (force_reindex=false) must coalesce onto the
    // running job rather than creating a second job.
    let snap2 = registry
        .start_index_job(Some(&project), false, false)
        .await
        .expect("second start_index_job");

    assert_eq!(
        snap1.job_id, snap2.job_id,
        "concurrent start_index_job calls for the same project must coalesce into the same job_id"
    );

    // The job_id equality holds while the same owned job is running. Wait for
    // terminal completion before dropping the temporary project so detached
    // work cannot race teardown.
    let mut attempts = 0usize;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        let snapshot = registry
            .get_index_job_snapshot(Some(&project))
            .await
            .expect("read coalesced job status")
            .expect("coalesced job status");
        if snapshot.status != JobStatus::Running {
            assert_eq!(snapshot.status, JobStatus::Complete);
            break;
        }
        attempts += 1;
        assert!(attempts < 1200, "coalesced job did not finish");
    }
}

/// VAL-JOBLIFECYCLE-003: When 20 concurrent `get_or_create` calls are issued
/// for the same project path, all must succeed without panic or database lock
/// conflict. The `.leindex` storage directory must exist after all calls
/// complete, and the project must be loaded exactly once.
#[tokio::test]
async fn test_concurrent_first_load_creates_project_once() {
    let _env_lock = AsyncEnvTestLock::acquire().await;
    let temp = make_tiny_rust_project();

    // Pre-build the durable index so the 20 concurrent first-loads exercise
    // the registry's per-project creation/locking path without each caller
    // fighting to be the one that builds the index from scratch. The contract
    // under test is the single-creation invariant of the registry itself.
    {
        let mut pre = LeIndex::new(temp.path()).expect("create pre-index");
        pre.index_project(true).expect("build pre-index");
    }

    let registry = Arc::new(ProjectRegistry::new(20));
    let project = temp.path().to_str().expect("utf8 project path").to_string();

    let caller_count = 20usize;
    let mut set = tokio::task::JoinSet::new();
    for _ in 0..caller_count {
        let reg = Arc::clone(&registry);
        let path = project.clone();
        set.spawn(async move { reg.get_or_create(Some(&path)).await });
    }

    let mut successes = 0usize;
    let mut failures = Vec::new();
    while let Some(join_result) = set.join_next().await {
        match join_result {
            Ok(Ok(_handle)) => successes += 1,
            Ok(Err(error)) => failures.push(format!("get_or_create error: {error}")),
            Err(join_error) => failures.push(format!("task join error: {join_error}")),
        }
    }

    assert!(
        failures.is_empty(),
        "{successes}/{caller_count} concurrent get_or_create calls succeeded; failures: {}",
        failures.join("; ")
    );
    assert_eq!(
        successes, caller_count,
        "all 20 concurrent get_or_create calls must succeed"
    );

    let storage = temp.path().join(".leindex");
    assert!(
        storage.is_dir(),
        ".leindex storage directory must exist after concurrent first-load"
    );
    assert_eq!(
        registry.len().await,
        1,
        "registry must hold exactly one project entry after 20 concurrent first-loads"
    );
}

/// VAL-JOBLIFECYCLE-004: When a panic occurs during indexing (triggered by
/// `LEINDEX_INJECT_PANIC=1`), the job status must transition to `failed`
/// rather than remaining stuck in `running`. The `catch_unwind` boundary
/// (here, the outer JoinHandle on the inner spawned task) must capture the
/// panic and update the job state with a message containing "panic".
#[tokio::test]
async fn test_panic_during_index_sets_failed_status() {
    let _env_lock = AsyncEnvTestLock::acquire().await;
    let _panic_marker = EnvVarGuard::set("LEINDEX_INJECT_PANIC", "1");

    let temp = make_tiny_rust_project();
    let registry = Arc::new(ProjectRegistry::new(2));
    let project = temp.path().to_str().expect("utf8 project path").to_string();

    let snapshot = registry
        .start_index_job(Some(&project), false, false)
        .await
        .expect("start job");
    assert_eq!(
        snapshot.status,
        JobStatus::Running,
        "job must be running immediately after start"
    );
    drop(snapshot);

    // Poll until the panic propagates and the outer task marks the job failed.
    let mut attempts = 0usize;
    let max_attempts = 120; // 60 seconds at 500ms intervals
    let final_snapshot;
    loop {
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        let snapshot = registry
            .get_index_job_snapshot(Some(&project))
            .await
            .expect("read job status")
            .expect("owned job status");
        if snapshot.status != JobStatus::Running {
            final_snapshot = snapshot;
            break;
        }
        attempts += 1;
        if attempts >= max_attempts {
            panic!(
                "panic-injected job never reached a terminal state after {} attempts (still running)",
                max_attempts
            );
        }
    }

    assert_eq!(
        final_snapshot.status,
        JobStatus::Failed,
        "panic-injected job must report failed status, got {:?}",
        final_snapshot.status
    );
    let error_message = final_snapshot
        .last_error
        .as_deref()
        .expect("failed job must have a last_error message");
    assert!(
        error_message.contains("panic"),
        "last_error must mention 'panic', got: {error_message:?}"
    );
}
