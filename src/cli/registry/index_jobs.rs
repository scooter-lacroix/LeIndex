use super::*;

impl ProjectRegistry {
    /// Start (or coalesce with) an owned indexing job.
    ///
    /// The returned task is detached from the caller's future. Dropping the
    /// MCP request therefore cannot cancel a parse, transaction, or
    /// generation swap. `wait=true` is an explicit compatibility mode for
    /// interactive callers; MCP defaults to polling.
    pub async fn start_index_job(
        self: &Arc<Self>,
        project_path: Option<&str>,
        force_reindex: bool,
        wait: bool,
    ) -> Result<IndexJobSnapshot, JsonRpcError> {
        let canonical = self.resolve_path(project_path).await?;
        let previous_generation = crate::cli::index_freshness::load_health(
            &crate::cli::leindex::resolve_existing_storage_path(&canonical)
                .unwrap_or_else(|| canonical.join(".leindex")),
        )
        .map(|health| health.generation)
        .unwrap_or(0);
        let storage_root = crate::cli::leindex::resolve_existing_storage_path(&canonical)
            .unwrap_or_else(|| canonical.join(".leindex"));
        let next_state_path =
            JobPaths::new(&storage_root, previous_generation.saturating_add(1)).job_status();
        let state = self
            .select_index_job_state(&canonical, &next_state_path)
            .await;
        self.spawn_owned_index_job(
            &state,
            canonical,
            storage_root,
            previous_generation,
            force_reindex,
        )
        .await;

        if wait {
            Ok(state.wait().await)
        } else {
            Ok(state.snapshot().await)
        }
    }

    /// Read the current owned indexing-job snapshot without starting or
    /// coalescing a job. Used by polling clients and lifecycle tests.
    pub async fn get_index_job_snapshot(
        &self,
        project_path: Option<&str>,
    ) -> Result<Option<IndexJobSnapshot>, JsonRpcError> {
        let canonical = self.resolve_path(project_path).await?;
        let state = self.index_jobs.lock().await.get(&canonical).cloned();
        Ok(match state {
            Some(state) => Some(state.snapshot().await),
            None => None,
        })
    }

    /// Select the existing owned job or replace a terminal one.
    ///
    /// Only a live job is coalesced with (a freshly created state also
    /// reports `Running` — it is `IndexJobSnapshot`'s default — so the
    /// insert/spawn race between two concurrent callers still coalesces).
    /// Returning a terminal `Failed` job made a project that failed to index
    /// once unindexable for the life of the process — every later
    /// `auto_index` (always `force_reindex=false`) got the same stale failure
    /// snapshot back and `spawn_owned_index_job` started nothing. A terminal
    /// `Complete` job is replaced too: if the caller is asking again, either
    /// the index went missing (re-run heals) or nothing changed and the
    /// pipeline's freshness check makes the re-run a fast no-op.
    pub(super) async fn select_index_job_state(
        &self,
        canonical: &Path,
        next_state_path: &Path,
    ) -> Arc<IndexJobState> {
        let mut jobs = self.index_jobs.lock().await;
        if let Some(existing) = jobs.get(canonical).cloned() {
            let current = existing.snapshot().await;
            if current.status == JobStatus::Running {
                return existing;
            }
        }

        let state = Arc::new(IndexJobState::with_state_path(
            new_job_id(canonical),
            next_state_path.to_path_buf(),
        ));
        jobs.insert(canonical.to_path_buf(), state.clone());
        state
    }

    /// Start the detached outer task only once for a running owned job.
    pub(super) async fn spawn_owned_index_job(
        self: &Arc<Self>,
        state: &Arc<IndexJobState>,
        path: PathBuf,
        storage_root: PathBuf,
        previous_generation: u64,
        force_reindex: bool,
    ) {
        if state.snapshot().await.status != JobStatus::Running || !state.try_start() {
            return;
        }

        let registry = Arc::clone(self);
        let task_state = Arc::clone(state);
        tokio::spawn(async move {
            // The inner task captures panics so the outer task can publish a
            // terminal snapshot and wake every waiter.
            let state_for_exit = Arc::clone(&task_state);
            let path_for_exit = path.clone();
            let inner = tokio::spawn(async move {
                registry
                    .run_owned_index_work(
                        task_state,
                        path,
                        storage_root,
                        previous_generation,
                        force_reindex,
                    )
                    .await;
            });

            if let Err(join_error) = inner.await {
                if join_error.is_panic() {
                    let payload = join_error.into_panic();
                    let panic_msg = payload
                        .downcast_ref::<&str>()
                        .map(|message| message.to_string())
                        .or_else(|| payload.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "non-string panic payload".to_string());
                    warn!(
                        project = %path_for_exit.display(),
                        "Indexing task panicked: {}; marking job as failed", panic_msg
                    );
                    state_for_exit
                        .fail(format!("indexing panicked: {panic_msg}"))
                        .await;
                } else {
                    warn!(
                        project = %path_for_exit.display(),
                        "Indexing task was cancelled; marking job as failed"
                    );
                    state_for_exit.fail("indexing task was cancelled").await;
                }
            }
        });
    }

    pub(super) async fn run_owned_index_work(
        self: &Arc<Self>,
        state: Arc<IndexJobState>,
        path: PathBuf,
        storage_root: PathBuf,
        previous_generation: u64,
        force_reindex: bool,
    ) {
        let mut resident_core_generation = previous_generation;
        state.set_phase("scan", 0, 0).await;

        // Test-only panic injection. Used by
        // `panic_during_index_sets_failed_status` to verify that the outer
        // task catches panics and marks the job as failed.
        if std::env::var("LEINDEX_INJECT_PANIC")
            .ok()
            .is_some_and(|value| value == "1")
        {
            panic!("injected test panic for index job lifecycle test");
        }

        match self
            .await_index_with_job_progress(
                &state,
                &path,
                &storage_root,
                &mut resident_core_generation,
                force_reindex,
            )
            .await
        {
            Ok(_) => {
                self.finish_owned_index(&state, &path, previous_generation)
                    .await
            }
            Err(error) => {
                self.preserve_core_after_job_error(
                    &state,
                    &path,
                    &storage_root,
                    previous_generation,
                    resident_core_generation,
                    &error,
                )
                .await;
            }
        }
    }

    pub(super) async fn await_index_with_job_progress(
        &self,
        state: &IndexJobState,
        path: &Path,
        storage_root: &Path,
        resident_core_generation: &mut u64,
        force_reindex: bool,
    ) -> Result<IndexStats, JsonRpcError> {
        let path_string = path.to_string_lossy().into_owned();
        let indexing = self.index_project(Some(&path_string), force_reindex);
        tokio::pin!(indexing);
        loop {
            tokio::select! {
                result = &mut indexing => break result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {
                    self.update_job_progress(state, path, storage_root, resident_core_generation).await;
                }
            }
        }
    }

    pub(super) async fn update_job_progress(
        &self,
        state: &IndexJobState,
        path: &Path,
        storage_root: &Path,
        resident_core_generation: &mut u64,
    ) {
        let Some(health) = crate::cli::index_freshness::load_health(storage_root) else {
            return;
        };
        if health.phase == crate::cli::leindex::IndexPhase::Complete
            && health.generation > *resident_core_generation
        {
            match self.refresh_loaded_from_active_generation(path).await {
                Ok(()) => {
                    *resident_core_generation = health.generation;
                    state.mark_core_published(health.generation).await;
                }
                Err(error) => {
                    warn!(
                        project = %path.display(),
                        "Core generation is published but resident hydration is pending: {error}"
                    );
                }
            }
        }
        let phase = format!("{:?}", health.phase).to_ascii_lowercase();
        let total = health.indexed_file_count;
        let completed = if health.phase == crate::cli::leindex::IndexPhase::Complete {
            total
        } else {
            0
        };
        state.set_phase(phase, completed, total).await;
    }

    pub(super) async fn finish_owned_index(
        &self,
        state: &IndexJobState,
        path: &Path,
        previous_generation: u64,
    ) {
        let generation = crate::cli::leindex::resolve_existing_storage_path(path)
            .and_then(|storage| crate::cli::index_freshness::load_health(&storage))
            .map(|health| health.generation)
            .unwrap_or(previous_generation.saturating_add(1));
        // Neural presence is a property of the published manifest (its Neural
        // layer hash differs from the canonical empty payload), not of any
        // file under `generations/<N>/`.
        let neural_published = crate::cli::leindex::resolve_existing_storage_path(path)
            .is_some_and(|storage| {
                crate::cli::index_freshness::load_health(&storage).is_some_and(|health| {
                    crate::storage::generation::read_generation_manifest(
                        &storage,
                        health.generation,
                    )
                    .is_ok_and(|manifest| {
                        crate::storage::generation::manifest_has_neural_vectors(&manifest)
                    })
                })
            });
        if neural_published {
            state.mark_neural_published().await;
        }
        state.complete(generation).await;
        // Mark the job as complete in the checkpoint state to prevent reuse
        // of stale artifacts on subsequent force_reindex runs.
        let storage_root = crate::cli::leindex::resolve_existing_storage_path(path)
            .unwrap_or_else(|| path.join(".leindex"));
        let job_paths = crate::cli::index_job::JobPaths::new(&storage_root, generation);
        let _ = crate::cli::index_job::mark_checkpoint_complete(&job_paths.state(), generation);
    }

    pub(super) async fn preserve_core_after_job_error(
        &self,
        state: &IndexJobState,
        path: &Path,
        storage_root: &Path,
        previous_generation: u64,
        resident_core_generation: u64,
        error: &JsonRpcError,
    ) {
        // The core snapshot is published before neural enrichment. Preserve
        // those layer flags if the optional follow-up fails afterward.
        if let Some(health) = crate::cli::index_freshness::load_health(storage_root) {
            if health.generation > previous_generation {
                let core_loaded = if health.generation > resident_core_generation {
                    match self.refresh_loaded_from_active_generation(path).await {
                        Ok(()) => true,
                        Err(refresh_error) => {
                            warn!(
                                project = %path.display(),
                                "Core generation remains durable but resident hydration failed: {refresh_error}"
                            );
                            false
                        }
                    }
                } else {
                    true
                };
                if core_loaded {
                    state.mark_core_published(health.generation).await;
                }
            }
        }
        state.fail(error.to_string()).await;
    }
}
