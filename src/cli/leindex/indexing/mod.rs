// Indexing pipeline methods for LeIndex: index_project and load_from_storage.

use super::{LeIndex, ProjectFileScan};
use crate::cli::index_builder;
use crate::cli::index_job::{
    CheckpointStore, JobPaths, LexicalCheckpoint, NeuralCheckpoint, ParseCheckpoint,
    ParsedFileCheckpoint, PdgCheckpoint, PublishedGeneration, ScanCheckpoint,
    latest_incomplete_job,
};
use crate::cli::memory_cap::MemoryCapGuard;
use anyhow::{Context, Result, bail};
use rayon::prelude::*;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::Instant;
use tracing::{info, warn};
mod helpers;
use helpers::*;

pub(crate) mod watcher_delta;

mod neural_publish;

mod layer_artifacts;
mod load;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;

/// Streaming index pipeline modules (WS6-9 Tasks 1-7).
///
/// Each stage converts from "materialize the whole corpus" to "stream bounded
/// chunks" with direct CAS staging. Feature-flagged via
/// `LEINDEX_FEATURE_STREAMING_*`, default OFF. The legacy pipeline runs
/// unmodified when flags are disabled.
///
/// `#[allow(dead_code)]` marks modules whose public API will be consumed by
/// the streaming pipeline integration landing in SP4 Task 8-9 (the follow-up
/// feature that wires these stages behind feature flags into the main
/// `index_project_inner` loop). Tests exercise every public item today.
#[allow(dead_code)]
pub(crate) mod streaming;

/// Runtime state carried between the six explicit indexing phases. It is
/// present only while `index_project_inner` is executing and is cleared before
/// the call returns. Durable checkpoints remain the restart contract.
pub(crate) struct IndexPipelineState {
    pub(crate) force: bool,
    pub(crate) start_time: Instant,
    /// Wall-clock instant the run began (before the scan). A no-op run
    /// acknowledges only filesystem changes made before it.
    pub(crate) started_at: std::time::SystemTime,
    pub(crate) job: JobPaths,
    pub(crate) checkpoint_store: Option<CheckpointStore>,
    pub(crate) indexed_files: HashMap<String, String>,
    pub(crate) old_scan: Option<ProjectFileScan>,
    pub(crate) source_files_with_hashes: Vec<(PathBuf, String)>,
    pub(crate) source_file_hashes: HashMap<String, String>,
    pub(crate) current_file_paths: HashSet<String>,
    pub(crate) files_to_parse: Vec<PathBuf>,
    pub(crate) unchanged_files: HashSet<String>,
    pub(crate) deleted_files: Vec<String>,
    pub(crate) resumed_scan: Option<ScanCheckpoint>,
    pub(crate) resumed_parse: Option<ParseCheckpoint>,
    pub(crate) resumed_pdg: Option<PdgCheckpoint>,
    pub(crate) resumed_lexical: Option<LexicalCheckpoint>,
    pub(crate) resumed_neural: Option<NeuralCheckpoint>,
    pub(crate) parsing_results: Vec<crate::parse::parallel::ParsingResult>,
    pub(crate) parse_checkpoint: Option<ParseCheckpoint>,
    pub(crate) pdg: Option<crate::graph::pdg::ProgramDependenceGraph>,
    pub(crate) pdg_checkpoint: Option<PdgCheckpoint>,
    pub(crate) pdg_node_count: usize,
    pub(crate) pdg_edge_count: usize,
    pub(crate) files_parsed: usize,
    pub(crate) successful: usize,
    pub(crate) failed: usize,
    pub(crate) total_sigs: usize,
    pub(crate) ext_in_lockfile: usize,
    pub(crate) ext_resolved: usize,
    pub(crate) ext_unresolved: usize,
    pub(crate) ext_total: usize,
    pub(crate) ext_builtin: usize,
    pub(crate) lexical_checkpoint: Option<LexicalCheckpoint>,
    pub(crate) lexical_hash: Option<String>,
    pub(crate) core_health: Option<super::IndexHealth>,
    pub(crate) neural_checkpoint: Option<NeuralCheckpoint>,
    pub(crate) neural_rows: usize,
    pub(crate) neural_resume_loaded: bool,
    pub(crate) admitted_node_ids: HashSet<String>,
    pub(crate) skip: bool,
}

fn sorted_admitted_node_ids(admitted_node_ids: &HashSet<String>) -> Vec<String> {
    let mut ids: Vec<String> = admitted_node_ids.iter().cloned().collect();
    ids.sort_unstable();
    ids
}

fn restored_admitted_node_ids(checkpoint: Option<&LexicalCheckpoint>) -> HashSet<String> {
    checkpoint
        .map(|checkpoint| checkpoint.admitted_node_ids.iter().cloned().collect())
        .unwrap_or_default()
}

impl IndexPipelineState {
    fn new(force: bool, start_time: Instant, job: JobPaths) -> Self {
        Self {
            force,
            start_time,
            started_at: std::time::SystemTime::now(),
            job,
            checkpoint_store: None,
            indexed_files: HashMap::new(),
            old_scan: None,
            source_files_with_hashes: Vec::new(),
            source_file_hashes: HashMap::new(),
            current_file_paths: HashSet::new(),
            files_to_parse: Vec::new(),
            unchanged_files: HashSet::new(),
            deleted_files: Vec::new(),
            resumed_scan: None,
            resumed_parse: None,
            resumed_pdg: None,
            resumed_lexical: None,
            resumed_neural: None,
            parsing_results: Vec::new(),
            parse_checkpoint: None,
            pdg: None,
            pdg_checkpoint: None,
            pdg_node_count: 0,
            pdg_edge_count: 0,
            files_parsed: 0,
            successful: 0,
            failed: 0,
            total_sigs: 0,
            ext_in_lockfile: 0,
            ext_resolved: 0,
            ext_unresolved: 0,
            ext_total: 0,
            ext_builtin: 0,
            lexical_checkpoint: None,
            lexical_hash: None,
            core_health: None,
            neural_checkpoint: None,
            neural_rows: 0,
            neural_resume_loaded: false,
            admitted_node_ids: HashSet::new(),
            skip: false,
        }
    }
}

/// Save-stage regression gate (step 5 of the graph-layer flip).
///
/// The pre-flip save stage (SQLite row-per-edge diff-upsert) measured 869 ms
/// for 175k edges / 22k nodes; the extrapolated failure mode was minutes at
/// 1.83M edges. Post-flip the save stage is whole-graph encode + one CAS
/// blob write per layer (~5 µs/edge measured, release). The gate enforces a
/// per-edge budget so a regression fails the index run loudly instead of
/// shipping quietly.
///
/// Override: `LEINDEX_SAVE_STAGE_GATE_MS` sets an absolute budget in
/// milliseconds (slow/loaded machines, CI). `0` disables — benchmarks only.
fn save_stage_gate(
    elapsed: std::time::Duration,
    pdg_edges: usize,
    staging: &std::path::Path,
) -> Result<()> {
    let override_ms = std::env::var("LEINDEX_SAVE_STAGE_GATE_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok());
    if override_ms == Some(0) {
        return Ok(());
    }
    let budget_ms = save_stage_budget_ms(pdg_edges, override_ms);
    let elapsed_ms = elapsed.as_millis() as u64;
    if elapsed_ms > budget_ms {
        bail!(
            "save-stage regression gate: publish took {elapsed_ms} ms for {pdg_edges} edges \
             (budget {budget_ms} ms; staging {}). If this machine is slow, set \
             LEINDEX_SAVE_STAGE_GATE_MS=<abs-ms> (0 disables, benchmarks only).",
            staging.display()
        );
    }
    Ok(())
}

/// Pure budget computation for [`save_stage_gate`], unit-tested without env
/// access. 20 µs/edge baseline (~4× headroom over the measured ~5 µs/edge),
/// a 2 s floor for tiny graphs (the fixed publish cost — VACUUM + CAS
/// staging + fsyncs — measured ~600 ms on shared CI runners; a floor near
/// that number false-trips the gate on 0-edge fixtures), ×10 in debug builds
/// (unoptimized encode).
fn save_stage_budget_ms(pdg_edges: usize, override_ms: Option<u64>) -> u64 {
    const US_PER_EDGE: u64 = 20;
    const FLOOR_MS: u64 = 2_000;
    let scale = if cfg!(debug_assertions) { 10 } else { 1 };
    override_ms.unwrap_or_else(|| {
        FLOOR_MS
            .max(pdg_edges as u64 * US_PER_EDGE / 1000)
            .saturating_mul(scale)
    })
}

fn parse_files_for_route(
    files: Vec<PathBuf>,
    scan: &ScanCheckpoint,
    route: streaming::routes::ParseRoute,
) -> Vec<crate::parse::parallel::ParsingResult> {
    match route {
        streaming::routes::ParseRoute::Streaming => {
            // Byte sizes come from the scan inventory (single hashing
            // authority); anything missing sizes to 0 and still parses.
            let file_sizes: HashMap<PathBuf, u64> = scan
                .files
                .iter()
                .map(|file| (file.canonical_path.clone(), file.bytes))
                .collect();
            let bounded = files
                .into_iter()
                .map(|path| {
                    let bytes = file_sizes.get(&path).copied().unwrap_or(0);
                    (path, bytes)
                })
                .collect();
            streaming::parse::stream_parse_parallel(
                bounded,
                &streaming::parse::ParseBudget::default(),
            )
        }
        streaming::routes::ParseRoute::Legacy => {
            crate::parse::parallel::ParallelParser::new().parse_files(files)
        }
    }
}

impl LeIndex {
    fn checkpoint_store(&self, generation: u64) -> CheckpointStore {
        CheckpointStore::new(self.storage_path(), generation)
    }

    fn checkpoint_state(&self, store: &CheckpointStore, phase: &str, hash: String) {
        let mut state = store.read_state().ok().flatten().unwrap_or_default();
        if state.job_id.is_empty() {
            state.job_id = format!("index-{}", store.paths.generation);
        }
        state.input_generation = store.paths.generation.saturating_sub(1);
        state.last_reusable_phase = Some(phase.to_string());
        state.artifact_hashes.insert(phase.to_string(), hash);
        state.updated_at_unix_ms = crate::cli::index_job::checkpoint_now_unix_ms();
        let _ = store.write_state(&state);
    }

    fn checkpoint_generation(&self) -> u64 {
        let max_generation = std::fs::read_dir(self.storage_path().join("generations"))
            .ok()
            .map(|entries| {
                entries
                    .flatten()
                    .filter_map(|entry| entry.file_name().to_str()?.parse::<u64>().ok())
                    .max()
                    .unwrap_or(0)
            })
            .unwrap_or(0);
        let root_generation = crate::cli::index_freshness::load_health(self.storage_path())
            .map(|health| health.generation)
            .unwrap_or(0);
        max_generation.max(root_generation).saturating_add(1)
    }

    /// Stage every layer of a generation into CAS via [`GenerationWriter`].
    ///
    /// The five required layers are always staged (Db from the VACUUMed
    /// catalog; Pdg/Symbols encoded from the resident in-memory graph; and
    /// Tfidf/Neural from the mmap embedding files). The optional layers are
    /// staged when their artifacts exist. Returns the writer with layers
    /// staged but NOT published — [`Self::promote_generation_snapshot`]
    /// publishes.
    fn stage_generation_layers(
        &self,
        include_neural: bool,
    ) -> Result<crate::storage::generation::GenerationWriter> {
        use std::sync::{Arc, Mutex};

        use crate::storage::cas::CasStore;
        use crate::storage::generation::{
            GenerationWriter, LayerKind, ModelIdentity, graph_codec, migrate as gen_migrate,
        };

        // WAL is checkpointed before encoding the immutable catalog snapshot;
        // query readers never observe a half-written generation.
        self.storage
            .conn()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .context("checkpoint SQLite WAL before generation publication")?;

        let storage_root = self.storage_path();
        let artifact_dir = self.project_path.join(".leindex");
        let cas = Arc::new(Mutex::new(CasStore::open(storage_root.join("cas"))?));
        let mut writer = GenerationWriter::new(storage_root, cas);
        writer.set_model_identity(ModelIdentity {
            name: "tfidf-hybrid".to_string(),
            digest: String::new(),
            dimensions: 768,
        });

        // Db layer: the VACUUM-normalized catalog (byte-deterministic).
        let db_bytes = gen_migrate::vacuum_bytes(&storage_root.join("leindex.db"))?;
        writer.stage(LayerKind::Db, &db_bytes)?;

        // Graph layers, encoded losslessly from the resident graph (D3): the
        // catalog no longer holds graph rows, so the Pdg and Symbols layers
        // are built from the in-memory PDG via the D1 codec. The layered node
        // assignment replaces the catalog's integer row ids for the vector
        // layers below.
        let pdg = self
            .pdg
            .as_ref()
            .context("generation publication requires a resident PDG")?;
        let (pdg_bytes, node_ids) = graph_codec::encode_pdg_v2_from_graph(pdg)?;
        // Vector layers key rows by the same layered assignment.
        let node_ids = node_ids.as_map();
        writer.stage(LayerKind::Pdg, &pdg_bytes)?;
        writer.stage(
            LayerKind::Symbols,
            &graph_codec::encode_symbols_layer_from_graph(pdg)?,
        )?;

        // Vector layers from the mmap embedding files (`embeddings.bin` is
        // absent on a docs-only index: the canonical empties keep the
        // publish contract).
        stage_vector_layers(&mut writer, &artifact_dir, &node_ids, include_neural)?;

        // Optional layers: staged only when the artifacts exist.
        stage_optional_layers(&mut writer, &artifact_dir)?;

        Ok(writer)
    }

    fn prepare_generation_snapshot(
        &self,
        staging: &std::path::Path,
        health: &super::IndexHealth,
        _include_neural: bool,
    ) -> Result<()> {
        // WAL is checkpointed before the immutable catalog snapshot is
        // encoded into the Db layer; query readers never observe a
        // half-written generation.
        self.storage
            .conn()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .context("checkpoint SQLite WAL before generation publication")?;
        // The layer payloads (Db/Tfidf/Neural/Pdg/Symbols plus the optional
        // Search/Embedder/Fragments) were already staged into CAS by
        // `stage_generation_layers`; nothing is copied here. A generation
        // directory carries only KB-scale metadata (health + stats), which
        // generation reads hydrate directly from the directory.
        let stats_path = self.storage_path().join("index_stats.json");
        if stats_path.is_file() {
            let next = staging.join("index_stats.json.next");
            std::fs::copy(&stats_path, &next).with_context(|| {
                format!(
                    "copy generation stats {} -> {}",
                    stats_path.display(),
                    next.display()
                )
            })?;
            std::fs::rename(next, staging.join("index_stats.json"))?;
        }
        crate::cli::index_freshness::save_health(staging, health)?;
        #[cfg(unix)]
        std::fs::File::open(staging)?.sync_all()?;
        Ok(())
    }

    fn promote_generation_snapshot(
        &self,
        staging: &std::path::Path,
        target: &std::path::Path,
        _generations: &std::path::Path,
        generation: u64,
        writer: &mut crate::storage::generation::GenerationWriter,
    ) -> Result<()> {
        // Rename the complete directory once. Readers either see no new
        // generation or the fully materialized immutable snapshot.
        std::fs::rename(staging, target).with_context(|| {
            format!(
                "promote staged generation {} -> {}",
                staging.display(),
                target.display()
            )
        })?;
        #[cfg(unix)]
        std::fs::File::open(_generations)?.sync_all()?;
        // The writer's publish is the commit point: manifest first, then the
        // atomic CURRENT swap LAST (crash-safety per GenerationWriter). The
        // old inline CURRENT write is gone — a single authority moves it.
        writer
            .publish(generation)
            .context("publish generation manifest + CURRENT via GenerationWriter")?;
        #[cfg(unix)]
        std::fs::File::open(self.storage_path())?.sync_all()?;
        Ok(())
    }

    fn publish_generation_snapshot(
        &self,
        requested_generation: u64,
        health: &mut super::IndexHealth,
        include_neural: bool,
    ) -> Result<PublishedGeneration> {
        let generations = self.storage_path().join("generations");
        std::fs::create_dir_all(&generations)?;

        // Allocate the generation number under contention. The requested
        // number is only a hint computed earlier (max+1 at planning time —
        // a TOCTOU guess): a concurrent writer (a second MCP server, or the
        // edit-triggered incremental refresh racing the watcher refresh)
        // may publish the same number between planning and this call.
        // Previously the loser bailed with "generation N already exists",
        // which failed the entire index job and left the store
        // status=failed with a wrecked signature count (N-10). Snapshots
        // are immutable, but the NUMBER is allocatable: bump past every
        // number that exists on disk — or that wins the rename race — and
        // retry.
        let mut generation = requested_generation;
        loop {
            while generations.join(generation.to_string()).exists() {
                generation += 1;
            }
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let staging = generations.join(format!(
                ".staging-{generation}-{}-{nonce}",
                std::process::id()
            ));
            std::fs::create_dir(&staging).with_context(|| {
                format!("create staging generation directory {}", staging.display())
            })?;
            // The health record embedded in the snapshot (and later
            // persisted by the caller) must carry the number actually
            // published, not the stale planning hint.
            health.generation = generation;
            let target = generations.join(generation.to_string());
            // Stage every layer into CAS first (additive, crash-safe), then
            // materialize the file mirror, rename it into place, and let the
            // writer commit the manifest + CURRENT. The whole attempt is the
            // save stage — timed for the regression gate below.
            let save_started = Instant::now();
            let attempt = self
                .stage_generation_layers(include_neural)
                .and_then(|mut writer| {
                    self.prepare_generation_snapshot(&staging, health, include_neural)
                        .and_then(|()| {
                            self.promote_generation_snapshot(
                                &staging,
                                &target,
                                &generations,
                                generation,
                                &mut writer,
                            )
                        })
                });
            let save_elapsed = save_started.elapsed();
            if attempt.is_ok() {
                let pdg_edges = self.pdg.as_ref().map_or(0, |p| p.edge_count());
                save_stage_gate(save_elapsed, pdg_edges, &staging)?;
            }
            if let Err(error) = attempt {
                let _ = std::fs::remove_dir_all(&staging);
                // Lost the rename race to a concurrent publisher: the
                // target appeared between the exists() check and the
                // atomic rename. Retry on the next free number; anything
                // else is a real failure.
                if target.exists() {
                    generation += 1;
                    continue;
                }
                return Err(error);
            }
            self.hydrated_generation
                .store(generation, std::sync::atomic::Ordering::Release);
            return Ok(PublishedGeneration {
                generation,
                storage_path: target,
                health: health.clone(),
            });
        }
    }

    /// The generation this process's in-memory index state was hydrated (or
    /// last published) from — `None` until the first hydration. Compared by
    /// the registry against the persisted `CURRENT` pointer to detect
    /// external rebuilds (N-13).
    pub fn hydrated_generation(&self) -> Option<u64> {
        let value = self
            .hydrated_generation
            .load(std::sync::atomic::Ordering::Acquire);
        (value > 0).then_some(value)
    }

    /// Index the project with an optional memory cap.
    ///
    /// This is the same as `index_project(force)` but additionally monitors RSS
    /// memory usage throughout the indexing pipeline. When `max_memory_bytes` is
    /// `Some(bytes)`, a `MemoryCapGuard` is created that:
    /// - Logs a warning when RSS exceeds 90% of the cap
    /// - Reports `OverCap` (a deferral signal) when RSS exceeds 100% of the cap
    ///
    /// VAL-SCHED-015: the cap is an ADMISSION cap, not a hard error. Over-cap
    /// pressure defers heavy work (and the global admission controller owns the
    /// actual defer/reduce/evict decision) — indexing is never aborted at the
    /// cap. The memory check is performed at key checkpoints during indexing to
    /// avoid excessive overhead while still catching runaway memory usage.
    pub fn index_project_with_memory_cap(
        &mut self,
        force: bool,
        max_memory_bytes: Option<u64>,
    ) -> Result<super::IndexStats> {
        let mut cap_guard = match max_memory_bytes {
            Some(bytes) => {
                let mb = bytes / (1024 * 1024);
                if mb == 0 {
                    bail!("--max-memory must be at least 1 MB");
                }
                info!("Memory cap enabled: {} MB", mb);
                Some(MemoryCapGuard::new(mb))
            }
            None => None,
        };

        self.index_project_inner(force, cap_guard.as_mut())
    }

    /// Index the project
    ///
    /// This executes the full indexing pipeline:
    /// 1. Parse all source files in parallel (incrementally)
    /// 2. Extract PDG from parsed signatures
    /// 3. Index nodes for semantic search
    /// 4. Persist PDG to storage
    ///
    /// # Arguments
    ///
    /// * `force` - If true, re-index all files regardless of changes
    ///
    /// # Returns
    ///
    /// `Result<IndexStats>` - Statistics from the indexing operation
    pub fn index_project(&mut self, force: bool) -> Result<super::IndexStats> {
        self.index_project_inner(force, None)
    }

    fn index_project_inner(
        &mut self,
        force: bool,
        mut cap_guard: Option<&mut MemoryCapGuard>,
    ) -> Result<super::IndexStats> {
        // Serialize concurrent writers across processes (e.g. a second MCP
        // instance, or MCP + CLI) so two processes never write leindex.db at
        // once. Blocks until exclusive; RAII releases on return. Without this,
        // concurrent writers contend on SQLite WAL (one writer max) and can
        // corrupt the DB, bricking the generation. See `ProjectWriteLock`.
        //
        // Scope note: this guards the HEAVY writes (PDG + neural embeddings +
        // publish) — the contention that actually bricked gen-93. The brief
        // startup writes in `LeIndex::new` (schema migration, project-metadata
        // insert) happen before this guard runs; they are idempotent and
        // serialized by SQLite's own busy_timeout, so they are not a bricking
        // risk. If startup-write contention is ever observed, extend the lock
        // to a write-mode `Storage::open` (or make `ProjectWriteLock`
        // re-entrant so `new()` can also acquire it without self-deadlock).
        //
        // Non-forced runs coalesce with a concurrent index in another
        // process: if that process publishes a fresh index while we wait for
        // the lock, this run returns immediately instead of queueing a
        // redundant full index behind it.
        self.last_index_coalesced = false;
        let Some(_write_lock) = self.acquire_write_lock_coalescing(force)? else {
            info!(
                "Skipping index for {}: another process published a fresh index \
                 while this writer waited for the project write lock",
                self.project_id
            );
            // Flag it so the registry keeps the resident instance (which may
            // hold a hydrated core this never-loaded temp lacks) and
            // refreshes it from the peer's published generation instead of
            // installing an un-hydrated replacement.
            self.last_index_coalesced = true;
            return Ok(self.stats.clone());
        };
        let start_time = Instant::now();
        let job = JobPaths::new(self.storage_path(), self.checkpoint_generation());
        self.pipeline = Some(IndexPipelineState::new(force, start_time, job.clone()));
        self.mark_index_phase(
            super::IndexPhase::Scan,
            super::ComponentStatus::Initializing,
        );
        info!(
            "Starting project indexing for: {} (force={})",
            self.project_id, force
        );

        let scan = self.run_scan(&job)?;
        check_memory_cap(&mut cap_guard)?;
        self.mark_index_phase(
            super::IndexPhase::Parse,
            super::ComponentStatus::Initializing,
        );
        let parsed = self.run_parse(&job, &scan)?;
        check_memory_cap(&mut cap_guard)?;
        if self.pipeline.as_ref().is_some_and(|state| state.skip) {
            self.pipeline = None;
            progress_clear();
            return Ok(self.stats.clone());
        }

        let pdg = self.run_pdg(&job, &parsed)?;
        check_memory_cap(&mut cap_guard)?;
        let lexical = self.run_lexical(&job, &pdg)?;

        self.run_and_publish_neural(&job, &lexical)?;
        self.finalize_indexing()?;
        Ok(self.stats.clone())
    }

    fn run_and_publish_neural(
        &mut self,
        job: &JobPaths,
        lexical: &LexicalCheckpoint,
    ) -> Result<()> {
        // Publish the core (lexical-only) generation as a crash-recovery
        // checkpoint.  If the process dies during neural, the resumed run
        // can skip straight to the neural phase using this snapshot.
        let _core = self.publish_generation(job, None)?;
        // The text index only needs the core generation's symbols, so it builds
        // beside the neural phase instead of after it.
        let text_index_job = self.spawn_text_index_refresh();

        let neural = self.run_neural(job, lexical);
        // Never leave the builder running past this run, even on failure.
        if text_index_job.join().is_err() {
            warn!("Text index build panicked (search will scan live)");
        }
        let neural = neural?;
        let _enhanced = self.publish_generation(job, Some(&neural))?;
        Ok(())
    }

    fn finalize_indexing(&mut self) -> Result<()> {
        let state = self
            .pipeline
            .take()
            .context("indexing pipeline state missing during finalization")?;
        let store = state
            .checkpoint_store
            .as_ref()
            .context("indexing checkpoint store missing during finalization")?;
        index_builder::clear_query_caches(&mut self.cache.cache_spiller, &self.project_id);
        info!("Indexing completed in {}ms", self.stats.indexing_time_ms);
        crate::cli::memory_report::observe_rss("post_index");
        progress_clear();
        let health =
            crate::cli::index_freshness::load_health(self.storage_path()).unwrap_or_default();
        self.checkpoint_state(
            store,
            "complete",
            blake3::hash(&serde_json::to_vec(&health).unwrap_or_default())
                .to_hex()
                .to_string(),
        );
        self.retain_published_generations();
        Ok(())
    }

    /// Bound the store's disk use after a successful publish: keep the current
    /// generation and its predecessor (the rollback point), drop older
    /// generations, and cap completed job artifacts.
    ///
    /// Nothing ran this automatically -- retention was only reachable through
    /// `leindex retention --gc` -- so every index run left another full copy
    /// behind (13 generations and 460 MB of job scratch, 2.5 GB, for a 20 MB
    /// repository). Best effort: a failure here never fails indexing.
    fn retain_published_generations(&self) {
        use crate::storage::generation::retention::{
            DEFAULT_MAX_GENERATIONS, RetentionConfig, retain_after_publish,
            retain_generations_no_cas,
        };
        let root = self.storage_path();
        let gens = root.join("generations");
        let jobs = root.join("jobs");
        let cas_dir = root.join("cas");
        let outcome = if cas_dir.exists() {
            crate::storage::cas::CasStore::open(&cas_dir)
                .map_err(|error| error.to_string())
                .and_then(|mut cas| {
                    retain_after_publish(&mut cas, &gens, &jobs, &RetentionConfig::default())
                        .map_err(|error| error.to_string())
                })
        } else {
            retain_generations_no_cas(&gens, &jobs, DEFAULT_MAX_GENERATIONS, false)
                .map_err(|error| error.to_string())
        };
        match outcome {
            Ok(report) if report.generations_removed > 0 => info!(
                removed = report.generations_removed,
                retained = report.generations_retained,
                "Pruned superseded generations"
            ),
            Ok(_) => {}
            Err(error) => warn!("Generation retention failed (store keeps growing): {error}"),
        }
    }

    pub(crate) fn run_scan(&mut self, _job: &JobPaths) -> Result<ScanCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("scan phase started without pipeline state")?;
        progress_stderr("Indexing: scanning files...");
        let indexed_files =
            crate::storage::pdg_store::get_indexed_files(&self.storage, &self.project_id)
                .context("Failed to load indexed files from storage")?;
        let old_scan = self.get_project_scan(false).ok();
        // Hash source files without caching bodies (VAL-STREAM-012: no
        // cross-phase source-body retention). Each phase re-reads per chunk.
        let scan_route = streaming::routes::scan_route_for_current_flag();
        info!(
            "Index scan: {} route selected by LEINDEX_FEATURE_STREAMING_SCAN",
            match scan_route {
                streaming::routes::ScanRoute::Streaming => "streaming",
                streaming::routes::ScanRoute::Legacy => "legacy",
            }
        );
        let source_files_with_hashes = match scan_route {
            streaming::routes::ScanRoute::Streaming => {
                let project_scan = self.get_project_scan(true)?;
                let mut writer = streaming::scan::VecScanRecordWriter::new();
                streaming::scan::stream_scan_paths(
                    &self.project_path,
                    &project_scan.source_paths,
                    &mut writer,
                )
                .context("Failed to stream-hash project source files")?;
                project_scan
                    .source_paths
                    .into_iter()
                    .zip(writer.records)
                    .map(|(path, record)| (path, record.hash))
                    .collect()
            }
            streaming::routes::ScanRoute::Legacy => self.collect_source_files_with_hashes(true)?,
        };
        info!("Found {} source files", source_files_with_hashes.len());
        let scan = scan_checkpoint(&source_files_with_hashes);
        let generation = state.job.generation;
        // force_reindex=true must bypass resume entirely. The resume reuses a
        // prior job's parse/PDG artifacts when the source hash matches — correct
        // for non-force incremental runs (and crash recovery), but on a forced
        // rebuild it would re-publish stale artifacts and prevent picking up
        // parser / indexing-logic changes (the whole point of --force). Note
        // `latest_incomplete_job` keys on `last_reusable_phase != "complete"`,
        // and no publication path writes "complete", so a successfully published
        // job remains forever "resumable" — force therefore has to skip the
        // lookup rather than rely on a completeness marker.
        let (checkpoint_store, resumed_scan) = if state.force {
            (self.checkpoint_store(generation), None)
        } else {
            latest_incomplete_job(self.storage_path())
                .and_then(|(paths, _)| {
                    let store = CheckpointStore::from_paths(paths);
                    let saved = store.read_scan().ok().flatten()?;
                    (saved.input_hash == scan.input_hash).then_some((store, saved))
                })
                .map(|(store, saved)| (store, Some(saved)))
                .unwrap_or_else(|| (self.checkpoint_store(generation), None))
        };
        state.job = checkpoint_store.paths.clone();
        if resumed_scan.is_none() {
            let scan_hash = checkpoint_store.write_scan(&scan)?;
            self.checkpoint_state(&checkpoint_store, "scan", scan_hash);
        }
        let checkpoint_state = checkpoint_store.read_state().ok().flatten();
        let resumed_parse = read_verified_artifact(
            checkpoint_state.as_ref(),
            "parse",
            &checkpoint_store.paths.parse(),
            CheckpointStore::read_parse,
            &checkpoint_store,
        )
        .filter(|checkpoint| checkpoint.scan_hash == scan.input_hash);
        let resumed_pdg = load_resumed_pdg(
            &checkpoint_store,
            &scan,
            resumed_scan.is_some(),
            checkpoint_state
                .as_ref()
                .and_then(|checkpoint| checkpoint.artifact_hashes.get("pdg").cloned()),
        );
        let resumed_lexical = read_verified_artifact(
            checkpoint_state.as_ref(),
            "lexical",
            &checkpoint_store.paths.lexical(),
            CheckpointStore::read_lexical,
            &checkpoint_store,
        )
        .filter(valid_lexical_checkpoint);
        let resumed_neural = read_verified_artifact(
            checkpoint_state.as_ref(),
            "neural",
            &checkpoint_store.paths.neural(),
            CheckpointStore::read_neural,
            &checkpoint_store,
        );
        state.indexed_files = indexed_files;
        state.old_scan = old_scan;
        state.source_files_with_hashes = source_files_with_hashes;
        state.resumed_scan = resumed_scan;
        state.resumed_parse = resumed_parse;
        state.resumed_pdg = resumed_pdg
            .as_ref()
            .map(|(checkpoint, _)| checkpoint.clone());
        state.pdg = resumed_pdg.map(|(_, pdg)| pdg);
        state.resumed_lexical = resumed_lexical;
        state.admitted_node_ids = restored_admitted_node_ids(state.resumed_lexical.as_ref());
        state.resumed_neural = resumed_neural;
        state.checkpoint_store = Some(checkpoint_store);
        injected_phase_failure("scan")?;
        let result = scan.clone();
        self.pipeline = Some(state);
        Ok(result)
    }

    fn manifests_changed(&mut self, old_scan: Option<&ProjectFileScan>) -> Result<bool> {
        if self.check_manifest_stale() {
            info!("Manifest files changed — running external dependency annotation");
            return Ok(true);
        }
        let current_scan = self.get_project_scan(false)?;
        let changed_manifests = match old_scan {
            Some(old) => current_scan
                .manifest_paths
                .iter()
                .filter(|manifest| {
                    let key = manifest.display().to_string();
                    current_scan.manifest_hashes.get(&key) != old.manifest_hashes.get(&key)
                        && !key.to_lowercase().contains("node_modules")
                        && !key.to_lowercase().contains("/build/")
                        && !key.to_lowercase().contains("\\build\\")
                        && !key.to_lowercase().contains("/dist/")
                        && !key.to_lowercase().contains("\\dist\\")
                        && !key.to_lowercase().contains("/target/")
                        && !key.to_lowercase().contains(".cache")
                })
                .cloned()
                .collect::<Vec<_>>(),
            None => index_builder::detect_changed_manifests(
                &current_scan,
                &self.project_id,
                &self.cache.cache_spiller,
            ),
        };
        if changed_manifests.is_empty() {
            return Ok(false);
        }
        info!(
            "Manifest content changed ({} files) — re-annotating",
            changed_manifests.len()
        );
        Ok(true)
    }

    fn write_parse_checkpoint(
        &self,
        store: &CheckpointStore,
        scan: &ScanCheckpoint,
        parsing_results: &[crate::parse::parallel::ParsingResult],
        source_file_hashes: &HashMap<String, String>,
    ) -> Result<ParseCheckpoint> {
        let mut artifact_paths = Vec::new();
        let mut artifact_hashes = std::collections::BTreeMap::new();
        let mut parsed_buckets: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, Vec<ParsedFileCheckpoint>>,
        > = std::collections::BTreeMap::new();
        for result in parsing_results.iter().filter(|result| result.is_success()) {
            let path_key = result.file_path.display().to_string();
            if let Some(source_hash) = source_file_hashes.get(&path_key) {
                let checkpoint = ParsedFileCheckpoint {
                    file_path: result.file_path.clone(),
                    language: result.language.as_deref().unwrap_or("unknown").to_string(),
                    signatures: result.signatures.clone(),
                    parse_time_ms: result.parse_time_ms,
                };
                let bucket = source_hash.chars().take(2).collect::<String>();
                parsed_buckets
                    .entry(bucket)
                    .or_default()
                    .entry(source_hash.clone())
                    .or_default()
                    .push(checkpoint);
            }
        }
        for (bucket, files) in parsed_buckets {
            let artifact_hash = store.write_parsed_batch(&bucket, &files)?;
            artifact_paths.push(store.paths.parsed_bucket(&bucket));
            for source_hash in files.keys() {
                artifact_hashes.insert(source_hash.clone(), artifact_hash.clone());
            }
        }
        artifact_paths.sort();
        Ok(ParseCheckpoint {
            scan_hash: scan.input_hash.clone(),
            artifact_paths,
            artifact_hashes,
        })
    }

    pub(crate) fn run_parse(
        &mut self,
        _job: &JobPaths,
        scan: &ScanCheckpoint,
    ) -> Result<ParseCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("parse phase started without pipeline state")?;
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("parse phase missing checkpoint store")?;
        let mut plan = parse_plan(&state);
        let resumed_parse_results = reuse_parse_results(
            state.resumed_scan.is_some(),
            state.resumed_parse.as_ref(),
            checkpoint_store,
            &plan.source_file_hashes,
            &mut plan.files_to_parse,
        )?;
        info!(
            "Incremental analysis: {} to parse, {} unchanged, {} deleted",
            plan.files_to_parse.len(),
            plan.unchanged_files.len(),
            plan.deleted_files.len()
        );
        if plan.files_to_parse.is_empty()
            && plan.deleted_files.is_empty()
            && self.is_indexed()
            && state.resumed_lexical.is_none()
            && !self.manifests_changed(state.old_scan.as_ref())?
        {
            info!("No changes detected, skipping indexing");
            self.mark_index_phase(super::IndexPhase::Complete, super::ComponentStatus::Fresh);
            // Content is current, but HEAD may have moved or a directory may
            // have changed (a commit or new file that touches nothing indexed).
            // Acknowledge that, otherwise the freshness check keeps reporting
            // drift and re-launches this scan in the background after every
            // request, forever.
            self.record_clean_scan(state.started_at);
            state.skip = true;
            let result = ParseCheckpoint {
                scan_hash: scan.input_hash.clone(),
                artifact_paths: Vec::new(),
                artifact_hashes: std::collections::BTreeMap::new(),
            };
            self.pipeline = Some(state);
            return Ok(result);
        }
        progress_stderr(&format!(
            "Indexing: parsing {} files...",
            plan.files_to_parse.len()
        ));
        let parse_route = streaming::routes::parse_route_for_current_flag();
        info!(
            "Index parse: {} route selected by LEINDEX_FEATURE_STREAMING_PARSE",
            match parse_route {
                streaming::routes::ParseRoute::Streaming => "streaming",
                streaming::routes::ParseRoute::Legacy => "legacy",
            }
        );
        let mut parsing_results = if plan.files_to_parse.is_empty() {
            Vec::new()
        } else {
            parse_files_for_route(std::mem::take(&mut plan.files_to_parse), scan, parse_route)
        };
        parsing_results.extend(resumed_parse_results);
        let parse_checkpoint = self.write_parse_checkpoint(
            checkpoint_store,
            scan,
            &parsing_results,
            &plan.source_file_hashes,
        )?;
        let parse_hash = checkpoint_store.write_parse(&parse_checkpoint)?;
        self.checkpoint_state(checkpoint_store, "parse", parse_hash);
        injected_phase_failure("parse")?;
        state.source_file_hashes = plan.source_file_hashes;
        state.current_file_paths = plan.current_file_paths;
        state.files_to_parse = plan.files_to_parse;
        state.unchanged_files = plan.unchanged_files;
        state.deleted_files = plan.deleted_files;
        state.parsing_results = parsing_results;
        state.parse_checkpoint = Some(parse_checkpoint.clone());
        self.pipeline = Some(state);
        Ok(parse_checkpoint)
    }

    fn apply_pdg_file_changes(
        &mut self,
        state: &IndexPipelineState,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
        parsing_results: Vec<crate::parse::parallel::ParsingResult>,
        use_streaming: bool,
    ) -> Result<()> {
        for path in &state.deleted_files {
            index_builder::remove_file_from_pdg(pdg, path)?;
            if let Err(error) = crate::storage::pdg_store::delete_file_data(
                &mut self.storage,
                &self.project_id,
                path,
            ) {
                warn!(
                    "Failed to delete file data from storage for '{}' during indexing: {}",
                    path, error
                );
            }
        }
        let newly_parsed: Vec<crate::parse::parallel::ParsingResult> = parsing_results
            .into_iter()
            .filter(|result| result.is_success())
            .collect();
        // Drop stale nodes for freshly parsed files *before* the merge, so node
        // id namespaces (file_path:qualified_name) stay disjoint when the new
        // graphs are added.
        let mut changed_file_count = 0usize;
        for result in &newly_parsed {
            let file_path = result.file_path.display().to_string();
            index_builder::remove_file_from_pdg(pdg, &file_path)?;
            if let Some(hash) = state.source_file_hashes.get(&file_path) {
                if let Err(error) = crate::storage::pdg_store::update_indexed_file(
                    &mut self.storage,
                    &self.project_id,
                    &file_path,
                    hash,
                ) {
                    warn!(
                        "Failed to update indexed file record for '{}' during indexing: {}",
                        file_path, error
                    );
                }
            }
            changed_file_count += 1;
        }
        if !newly_parsed.is_empty() {
            let (new_pdg, route) = build_changed_file_pdg(newly_parsed, use_streaming);
            if pdg.node_count() == 0 {
                // A full rebuild merges into an empty graph: adopt the built
                // one instead of re-inserting every node and edge (and
                // rebuilding its trigram index) a second time.
                *pdg = new_pdg;
            } else {
                index_builder::merge_pdgs(pdg, new_pdg);
            }
            info!(
                "PDG: rebuilt {} changed file(s) via {:?} ({} nodes, {} edges)",
                changed_file_count,
                route,
                pdg.node_count(),
                pdg.edge_count()
            );
        }
        Ok(())
    }

    fn annotate_external_dependencies(
        &self,
        state: &mut IndexPipelineState,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    ) {
        let manifest_paths = self
            .cache
            .project_scan
            .as_ref()
            .map(|scan| scan.manifest_paths.clone())
            .unwrap_or_default();
        let registry = crate::graph::ExternalDependencyRegistry::from_manifest_paths(
            &self.project_path,
            &manifest_paths,
        );
        let stats = crate::graph::annotate_external_nodes(pdg, &registry);
        if !registry.is_empty() {
            info!(
                "External dependency resolution: {}/{} resolved via lock files, {} recognized builtins ({} packages in registry)",
                stats.resolved,
                stats.total_external,
                stats.builtin,
                registry.len()
            );
        } else if stats.total_external > 0 {
            info!(
                "External dependency resolution: no lockfile registry found, {} builtins recognized, {} unresolved external imports",
                stats.builtin, stats.unresolved
            );
        }
        state.ext_in_lockfile = registry.len();
        state.ext_resolved = stats.resolved;
        state.ext_unresolved = stats.unresolved;
        state.ext_total = stats.total_external;
        state.ext_builtin = stats.builtin;
    }

    /// (Re)build the trigram text index used by `leindex_find`, with symbol
    /// spans read from the generation just published. Best effort: search
    /// falls back to live scanning, so a failure here never fails indexing.
    /// Runs on its own thread; join the handle before the run ends.
    fn spawn_text_index_refresh(&self) -> std::thread::JoinHandle<()> {
        let root = self.project_path().to_path_buf();
        let storage = self.storage_path().to_path_buf();
        std::thread::spawn(move || {
            if let Err(error) = crate::cli::textindex::build(&root, &storage) {
                warn!("Text index build failed (search will scan live): {error}");
            }
        })
    }

    fn prepare_pdg_for_building(
        &mut self,
        state: &mut IndexPipelineState,
    ) -> Result<(
        crate::graph::pdg::ProgramDependenceGraph,
        Vec<crate::parse::parallel::ParsingResult>,
    )> {
        progress_stderr("Indexing: building PDG...");
        if !state.unchanged_files.is_empty() && self.pdg.is_none() && state.pdg.is_none() {
            self.load_pdg_from_storage().context(
                "Failed to load existing PDG for incremental reindex. Please reindex with --force if corruption persists.",
            )?;
        }
        let resumed_pdg_loaded = state.resumed_pdg.is_some() && state.pdg.is_some();
        let pdg = if resumed_pdg_loaded {
            state.pdg.take().unwrap_or_default()
        } else {
            state
                .pdg
                .take()
                .or_else(|| self.take_owned_pdg())
                .unwrap_or_default()
        };
        let parsing_results = if resumed_pdg_loaded {
            Vec::new()
        } else {
            std::mem::take(&mut state.parsing_results)
        };
        Ok((pdg, parsing_results))
    }

    fn finalize_pdg(
        &mut self,
        pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
        state: &mut IndexPipelineState,
        all_signatures: &[(String, crate::parse::traits::SignatureInfo)],
    ) {
        // Resume-proof FileSummary pass: covers files loaded from storage on
        // resume (the merge_pdgs loop above only fires for freshly-parsed files).
        pdg.ensure_file_summary_nodes();
        if !all_signatures.is_empty() {
            crate::graph::resolve_cross_file_call_edges_for_files(pdg, all_signatures);
            crate::graph::resolve_cross_file_flow_edges_for_files(pdg, all_signatures);
        }
        self.annotate_external_dependencies(state, pdg);
        add_submodule_summary_nodes(pdg, &self.project_path);
        index_builder::normalize_external_nodes(pdg);
        // Precision must be merged before checkpoint counts and fingerprints are
        // captured; otherwise resumable artifacts describe a different graph.
        self.run_precision_ingest(pdg);
    }

    pub(crate) fn run_pdg(
        &mut self,
        _job: &JobPaths,
        parsed: &ParseCheckpoint,
    ) -> Result<PdgCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("PDG phase started without pipeline state")?;
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("PDG phase missing checkpoint store")?
            .clone();

        let (mut pdg, parsing_results) = self.prepare_pdg_for_building(&mut state)?;
        let parse_stats = pdg_parse_stats(&parsing_results);
        // Route PDG construction: the streaming fragment/segment pipeline
        // (SP4) is the default; `LEINDEX_FEATURE_STREAMING_PDG=0` reverts to
        // the legacy per-file extraction + merge loop.
        let use_streaming = pdg_route_for_current_flag() == PdgBuildRoute::Streaming;
        info!(
            "PDG construction: streaming fragment pipeline {}",
            if use_streaming {
                "enabled"
            } else {
                "disabled (legacy merge)"
            }
        );
        self.apply_pdg_file_changes(&state, &mut pdg, parsing_results, use_streaming)?;
        self.finalize_pdg(&mut pdg, &mut state, &parse_stats.all_signatures);

        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();
        let pdg_checkpoint = checkpoint_store.write_pdg(parsed.scan_hash.clone(), &pdg)?;
        self.checkpoint_state(
            &checkpoint_store,
            "pdg",
            pdg_checkpoint.artifact_hash.clone(),
        );
        injected_phase_failure("pdg")?;
        self.mark_index_phase(super::IndexPhase::Pdg, super::ComponentStatus::Initializing);
        info!(
            "Updated PDG has {} nodes and {} edges",
            pdg_node_count, pdg_edge_count
        );
        state.files_parsed = parse_stats.files_parsed;
        state.successful = parse_stats.successful;
        state.failed = parse_stats.failed;
        state.total_sigs = parse_stats.total_sigs;
        state.pdg_node_count = pdg_node_count;
        state.pdg_edge_count = pdg_edge_count;
        state.pdg_checkpoint = Some(pdg_checkpoint.clone());
        state.pdg = Some(pdg);
        self.pipeline = Some(state);
        Ok(pdg_checkpoint)
    }

    fn build_lexical_embedder(
        &mut self,
        pdg: &crate::graph::pdg::ProgramDependenceGraph,
        resume_valid: bool,
    ) -> Result<index_builder::HybridEmbedder> {
        let batch_size = self.indexing_batch_size();
        let persisted = index_builder::TfIdfEmbedder::load_from_storage(&self.project_path)
            .ok()
            .flatten();
        if resume_valid {
            return match self.load_from_mutable_storage() {
                Ok(()) => {
                    self.search_engine.clear_neural_embeddings();
                    Ok(self
                        .embedder
                        .as_ref()
                        .map(|embedder| {
                            index_builder::HybridEmbedder::tfidf_only(embedder.tfidf().clone())
                        })
                        .or_else(|| {
                            persisted
                                .clone()
                                .map(index_builder::HybridEmbedder::tfidf_only)
                        })
                        .context("resumed lexical checkpoint has no TF-IDF embedder")?)
                }
                Err(error) => {
                    warn!(
                        "Failed to hydrate resumed lexical checkpoint; rebuilding core: {error:#}"
                    );
                    index_builder::index_nodes_tfidf_only(
                        pdg,
                        &mut self.search_engine,
                        &mut self.cache.file_stats_cache,
                        batch_size,
                        persisted.map(index_builder::HybridEmbedder::tfidf_only),
                    )
                }
            };
        }
        let persisted = match persisted {
            Some(embedder)
                if embedder.is_fresh(
                    pdg.node_count(),
                    pdg.edge_count(),
                    &crate::cli::index_builder::pdg_search_fingerprint(pdg),
                ) =>
            {
                info!("Loaded persisted embedder from storage");
                Some(embedder)
            }
            Some(_) => {
                info!("Persisted embedder is stale; rebuilding TF-IDF index");
                None
            }
            None => None,
        };
        let tfidf_route = streaming::routes::tfidf_route_for_current_flag();
        info!(
            "Index TF-IDF: {} route selected by LEINDEX_FEATURE_STREAMING_TFIDF",
            match tfidf_route {
                streaming::routes::TfidfRoute::Streaming => "streaming",
                streaming::routes::TfidfRoute::Legacy => "legacy",
            }
        );
        match tfidf_route {
            streaming::routes::TfidfRoute::Streaming => {
                let embedder = streaming::tfidf::build_streaming_embedder(pdg, batch_size)
                    .context("Failed to build streaming TF-IDF embedder")?;
                index_builder::index_nodes_tfidf_only_with_embedder(
                    pdg,
                    &mut self.search_engine,
                    &mut self.cache.file_stats_cache,
                    batch_size,
                    index_builder::HybridEmbedder::tfidf_only(embedder),
                )
            }
            streaming::routes::TfidfRoute::Legacy => index_builder::index_nodes_tfidf_only(
                pdg,
                &mut self.search_engine,
                &mut self.cache.file_stats_cache,
                batch_size,
                persisted.map(index_builder::HybridEmbedder::tfidf_only),
            ),
        }
    }

    pub(crate) fn run_lexical(
        &mut self,
        _job: &JobPaths,
        pdg_checkpoint: &PdgCheckpoint,
    ) -> Result<LexicalCheckpoint> {
        let mut state = self
            .pipeline
            .take()
            .context("lexical phase started without pipeline state")?;
        let mut pdg = state
            .pdg
            .take()
            .or_else(|| self.take_owned_pdg())
            .context("lexical phase missing resident PDG")?;
        let pdg_node_count = pdg.node_count();
        let pdg_edge_count = pdg.edge_count();
        progress_stderr(&format!("Indexing: embedding {} nodes...", pdg_node_count));
        let lexical_resume_valid = state
            .resumed_lexical
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.pdg_hash == pdg_checkpoint.artifact_hash);
        let embedder = self.build_lexical_embedder(&pdg, lexical_resume_valid)?;
        self.embedder = Some(embedder);
        let indexed_count = self.search_engine.node_count();
        state.admitted_node_ids = self.search_engine.live_node_ids().into_iter().collect();
        self.mark_index_phase(
            super::IndexPhase::Lexical,
            super::ComponentStatus::Initializing,
        );
        info!("Indexed {} nodes for search", indexed_count);
        progress_stderr("Indexing: publishing generation...");
        self.mark_index_phase(
            super::IndexPhase::Persist,
            super::ComponentStatus::Initializing,
        );
        // D5: the graph no longer persists as SQL rows. It is published as
        // the Pdg layer of a CAS generation (stage_generation_layers); the
        // catalog keeps only `indexed_files` and metadata tables.
        // Snapshot/embedder freshness identity must describe the graph as the
        // published layer reconstructs it (the in-memory graph may hold
        // duplicate node_ids and parallel edges the layer collapses).
        let persisted_identity = index_builder::persisted_search_identity_from_graph(&pdg);
        if let Some(embedder) = &self.embedder {
            embedder.persist_to_storage(
                &self.project_path,
                &pdg,
                Some(persisted_identity.clone()),
            )?;
        }
        self.compute_and_persist_communities(&mut pdg);
        let checkpoint_store = state
            .checkpoint_store
            .as_ref()
            .context("lexical phase missing checkpoint store")?;
        self.checkpoint_state(
            checkpoint_store,
            "persist",
            pdg_checkpoint.artifact_hash.clone(),
        );
        injected_phase_failure("persist")?;
        self.stats = super::IndexStats {
            total_files: state.source_files_with_hashes.len(),
            files_parsed: state.files_parsed,
            successful_parses: state.successful,
            failed_parses: state.failed,
            total_signatures: state.total_sigs,
            // An incremental run parses only the changed subset; its signature
            // count is a delta, not the project total — label it so readers
            // stop mistaking "2" for a collapse from thousands.
            signature_scope: if state.files_parsed < state.source_files_with_hashes.len() {
                "delta".to_string()
            } else {
                "full".to_string()
            },
            pdg_nodes: pdg_node_count,
            pdg_edges: pdg_edge_count,
            indexed_nodes: indexed_count,
            indexing_time_ms: state.start_time.elapsed().as_millis() as u64,
            external_deps_in_lockfile: state.ext_in_lockfile,
            external_deps_resolved: state.ext_resolved,
            external_deps_unresolved: state.ext_unresolved,
            external_deps_total: state.ext_total,
            external_deps_builtin: state.ext_builtin,
        };
        self.pdg = Some(std::sync::Arc::new(pdg));
        // A core generation is intentionally lexical/PDG-only. Existing
        // neural rows are reattached only by run_neural after CURRENT moves.
        self.search_engine.clear_neural_embeddings();
        self.build_file_stats_cache();
        index_builder::persist_embeddings_to_mmap(&self.search_engine, &self.project_path)?;
        // Fragment layer (Task 7): incremental sync before the snapshot
        // persist. On failure the layer is CLEARED (not just logged) — see
        // `sync_fragment_layer_or_clear` (Codex wave-4 P2).
        self.sync_fragment_layer_or_clear();
        let (identity_nodes, identity_edges, identity_fingerprint) = persisted_identity;
        index_builder::persist_search_snapshot(
            &self.search_engine,
            &self.project_path,
            identity_nodes,
            identity_edges,
            identity_fingerprint,
        )?;
        let admitted_node_ids = sorted_admitted_node_ids(&state.admitted_node_ids);
        let lexical_checkpoint = LexicalCheckpoint {
            pdg_hash: pdg_checkpoint.artifact_hash.clone(),
            snapshot_path: self.project_path.join(".leindex/search_snapshot.bin"),
            tfidf_path: self.project_path.join(".leindex/tfidf_embedder.bin"),
            admitted_node_ids,
        };
        let lexical_hash = checkpoint_store.write_lexical(&lexical_checkpoint)?;
        self.checkpoint_state(checkpoint_store, "lexical", lexical_hash.clone());
        state.lexical_hash = Some(lexical_hash);
        state.lexical_checkpoint = Some(lexical_checkpoint.clone());
        state.pdg = None;
        state.pdg_checkpoint = Some(pdg_checkpoint.clone());
        self.pipeline = Some(state);
        Ok(lexical_checkpoint)
    }

    /// Update the last_indexed timestamp in project_metadata
    fn update_last_indexed_timestamp(&self) -> Result<()> {
        let conn = self.storage.conn();
        conn.execute(
            "UPDATE project_metadata SET last_indexed = CURRENT_TIMESTAMP WHERE unique_project_id = ?1",
            [&self.project_id],
        )
        .context("Failed to update last_indexed timestamp")?;
        Ok(())
    }

    /// Load a previously indexed project from the generation selected by
    /// `CURRENT` (or the legacy root when no generation is published).
    ///
    /// # Returns
    ///
    /// `Result<()>` - Success or error
    pub fn load_from_storage(&mut self) -> Result<()> {
        self.load_from_active_storage()
    }

    /// Hydrate directly from the mutable root. Indexing recovery uses this
    /// only for validated checkpoint artifacts that are not current yet.
    fn load_from_mutable_storage(&mut self) -> Result<()> {
        self.load_from_storage_inner(false)
    }

    /// Hydrate queries from the generation selected by `CURRENT`.
    ///
    /// Indexing still writes the mutable root, but normal registry hydration
    /// must never read that in-progress state after a crash or concurrent job.
    pub(crate) fn load_from_active_storage(&mut self) -> Result<()> {
        // WS4 Task 14: when the `generation-readers` flag is enabled and the
        // project has a current generation, hydrate the read path from the
        // leased mmap generation instead of the legacy heap-mirror store. The
        // lease never touches the writer Mutex, so reads keep working while a
        // concurrent index/publish is in progress (VAL-EQUIV-002/003).
        if self.try_hydrate_from_generation()? {
            return Ok(());
        }
        let active = self.active_storage_path();
        if active == self.storage_path || !active.join("leindex.db").is_file() {
            return self.load_from_mutable_storage();
        }

        // Open the published (immutable) generation read-only: no WAL, no DDL,
        // no `INSERT OR REPLACE schema_version`. Mutating a published snapshot
        // would (a) make concurrent readers contend as writers and (b) fail on
        // read-only archived artifacts. See `Storage::open_readonly`.
        let active_storage =
            crate::storage::schema::Storage::open_readonly(active.join("leindex.db"))
                .with_context(|| {
                    format!("Failed to open active generation at {}", active.display())
                })?;
        self.load_from_storage_inner_at(false, Some(&active_storage), active, None)
    }

    /// Re-hydrate from the CURRENT generation even when a generation snapshot
    /// is already held (which `load_from_active_storage` treats as done).
    ///
    /// Used after a coalesced index: another process published a fresh
    /// generation while this instance waited on the write lock, and a held
    /// snapshot of the OLD generation would otherwise keep serving stale
    /// data under a moved CURRENT pointer. Dropping the old snapshot
    /// releases its lease; the new hydration leases the new generation.
    pub(crate) fn force_reload_from_active_storage(&mut self) -> Result<()> {
        self.generation_snapshot = None;
        self.load_from_active_storage()
    }

    /// WS4 Task 14: wire the read path (PDG + search engine) onto the leased
    /// mmap generation when `LEINDEX_FEATURE_GENERATION_READERS` is enabled
    /// and the project has a current generation.
    ///
    /// Returns `Ok(true)` when the read path is served from the generation
    /// (or was already wired on a previous call), `Ok(false)` when the flag is
    /// off or no generation exists (the caller falls back to the legacy
    /// heap-mirror path). The returned snapshot is retained on `self` so its
    /// [`GenerationLease`] keeps the generation's CAS blobs pinned for the
    /// lifetime of this process.
    pub(crate) fn try_hydrate_from_generation(&mut self) -> Result<bool> {
        self.try_hydrate_from_generation_inner(false)
    }

    /// Graph-only variant: hydrate the PDG from the leased generation WITHOUT
    /// restoring the search engine (snapshot + embedding mmaps). Graph-only
    /// tools (read-symbol, symbol-lookup, project-map) never query the search
    /// engine, so paying ~1s of artifact restoration per cold call was pure
    /// added latency.
    pub(crate) fn try_hydrate_generation_pdg_only(&mut self) -> Result<bool> {
        if self.pdg.is_some() {
            return Ok(true);
        }
        self.try_hydrate_from_generation_inner(true)
    }

    fn try_hydrate_from_generation_inner(&mut self, pdg_only: bool) -> Result<bool> {
        if !crate::feature_flags::FeatureFlag::GenerationReaders.is_enabled() {
            return Ok(false);
        }
        if pdg_only && self.pdg.is_some() {
            return Ok(true);
        }
        if self.generation_snapshot.is_some() {
            return Ok(true);
        }
        let Some(storage_path) =
            crate::cli::leindex::resolve_existing_storage_path(&self.project_path)
        else {
            return Ok(false);
        };
        let Some(generation) =
            crate::storage::generation::lease::read_current_generation(&storage_path)
        else {
            return Ok(false);
        };
        // Check whether a CAS manifest exists for this generation. Legacy
        // layouts (pre-migration full-copy) write a CURRENT file but do not
        // have a manifest. In that case gracefully fall back to the legacy
        // heap-mirror path instead of erroring.
        let manifest_path = storage_path
            .join(crate::storage::generation::lease::GENERATIONS_DIR)
            .join(generation.to_string())
            .join(crate::storage::generation::lease::MANIFEST_FILE);
        if !manifest_path.exists() {
            return Ok(false);
        }
        let snapshot = crate::storage::generation::GenerationSnapshot::open(&storage_path)
            .with_context(|| {
                format!(
                    "Failed to open generation snapshot at {}",
                    storage_path.display()
                )
            })?;
        let generation_db = crate::storage::schema::Storage::open_readonly(snapshot.db_path())?;
        // Artifact path points at the snapshot's temp dir. The Search /
        // Embedder / Tfidf / Neural / Fragments layers are materialized there
        // from CAS (D2) so the SAME decoders and freshness checks as the
        // mutable-root path restore the engine; if that fails or a layer is
        // absent, hydration takes the rebuild path. `persist_artifacts` stays
        // false — the generation read path never writes legacy artifacts back
        // into the store.
        if !pdg_only {
            if let Err(error) = layer_artifacts::materialize_search_artifacts(&snapshot) {
                warn!(%error, "Search layer materialization failed; rebuilding search from the graph");
            }
        }
        let artifact_path = snapshot
            .db_path()
            .parent()
            .map(ToOwned::to_owned)
            .unwrap_or_else(|| storage_path.clone());
        // D4: hydrate the graph from the Pdg layer when the leased generation
        // carries a v2 layer (the only kind this branch publishes). Decode
        // failure or a missing/v1 layer falls back to the SQL catalog in the
        // generation's Db layer — pre-migration generations still keep their
        // graph rows there.
        let prebuilt_pdg =
            snapshot
                .pdg()
                .and_then(|reader| match reader.to_program_dependence_graph() {
                    Ok(pdg) => Some(pdg),
                    Err(error) => {
                        warn!(
                            %error,
                            "Generation Pdg layer decode failed; falling back to the SQL catalog"
                        );
                        None
                    }
                });
        self.load_from_storage_inner_at(
            pdg_only,
            Some(&generation_db),
            artifact_path,
            prebuilt_pdg,
        )?;
        self.hydrated_generation
            .store(snapshot.generation(), std::sync::atomic::Ordering::Release);
        self.generation_snapshot = Some(snapshot);
        info!(
            project = %self.project_path.display(),
            "Hydrated read path from leased mmap generation"
        );
        Ok(true)
    }

    /// Load PDG from storage without populating the search engine.
    /// Used by index_project() when it will call index_nodes() afterwards.
    pub fn load_pdg_from_storage(&mut self) -> Result<()> {
        self.load_from_storage_inner(true)
    }

    /// Graph-only hydration from the generation selected by `CURRENT`.
    ///
    /// Reading the mutable root here would pair a PDG that a concurrent or
    /// failed index run has since rewritten with the search artifacts of the
    /// published generation. Those never agree, so every hydration would
    /// rebuild the search index and never persist the result.
    pub(crate) fn load_pdg_from_active_storage(&mut self) -> Result<()> {
        let active = self.active_storage_path();
        if active == self.storage_path || !active.join("leindex.db").is_file() {
            return self.load_pdg_from_storage();
        }
        let active_storage =
            crate::storage::schema::Storage::open_readonly(active.join("leindex.db"))
                .with_context(|| {
                    format!("Failed to open active generation at {}", active.display())
                })?;
        self.load_from_storage_inner_at(true, Some(&active_storage), active, None)
    }

    fn load_from_storage_inner(&mut self, pdg_only: bool) -> Result<()> {
        self.load_from_storage_inner_at(pdg_only, None, self.storage_path.clone(), None)
    }
}

/// Stage the Tfidf/Neural layers from the mmap embedding files, keyed by the
/// graph layer's node assignment. `embeddings.bin` is absent on a docs-only
/// index (no code nodes): the canonical empty TF-IDF layer keeps the publish
/// contract rather than failing the publish.
fn stage_vector_layers(
    writer: &mut crate::storage::generation::GenerationWriter,
    artifact_dir: &std::path::Path,
    node_ids: &HashMap<String, u32>,
    include_neural: bool,
) -> Result<()> {
    use crate::storage::generation::{LayerKind, migrate as gen_migrate};

    let embeddings_path = artifact_dir.join("embeddings.bin");
    let tfidf = if embeddings_path.is_file() {
        gen_migrate::encode_tfidf_layer(&embeddings_path, node_ids)?
    } else {
        gen_migrate::encode_empty_tfidf()
    };
    writer.stage(LayerKind::Tfidf, &tfidf)?;
    let neural_path = artifact_dir.join("neural_embeddings.bin");
    let neural = if include_neural && neural_path.is_file() {
        gen_migrate::encode_neural_layer(&neural_path, node_ids)?
    } else {
        gen_migrate::encode_empty_neural()
    };
    writer.stage(LayerKind::Neural, &neural)?;
    Ok(())
}

/// Stage the optional layers (Search, Embedder, Fragments) when their
/// artifacts exist on disk.
fn stage_optional_layers(
    writer: &mut crate::storage::generation::GenerationWriter,
    artifact_dir: &std::path::Path,
) -> Result<()> {
    use crate::storage::generation::{LayerKind, migrate as gen_migrate};

    if artifact_dir.join("search_snapshot.bin").is_file() {
        let bytes = std::fs::read(artifact_dir.join("search_snapshot.bin"))?;
        writer.stage(LayerKind::Search, &bytes)?;
    }
    if artifact_dir.join("tfidf_embedder.bin").is_file() {
        let bytes = std::fs::read(artifact_dir.join("tfidf_embedder.bin"))?;
        writer.stage(LayerKind::Embedder, &bytes)?;
    }
    if let Some(bundle) = gen_migrate::encode_fragment_bundle(artifact_dir)? {
        writer.stage(LayerKind::Fragments, &bundle)?;
    }
    Ok(())
}
/// Resolve the source bytes for a parsed file at PDG-extraction time.
///
/// The streaming parse route strips `source_bytes` from its returned results
/// (peak-RSS contract: no whole-corpus retention), so file-backed results
/// are re-read lazily here — at most one file per parallel worker, never the
/// corpus. Missing or unreadable files yield an empty slice, matching the
/// `unwrap_or(&[])` semantics of the inline path (parse-failure results
/// carry `None` too, and extraction treats empty source as absent). Inline
/// bytes are borrowed, never copied; legacy results never touch the
/// filesystem. Takes the field references (not the whole result) so callers
/// can move `result.signatures` while the bytes are still borrowed.
fn extraction_source_bytes<'a>(
    source_bytes: &'a Option<Vec<u8>>,
    file_path: &std::path::Path,
) -> std::borrow::Cow<'a, [u8]> {
    match source_bytes {
        Some(bytes) => std::borrow::Cow::Borrowed(bytes),
        None => std::borrow::Cow::Owned(std::fs::read(file_path).unwrap_or_default()),
    }
}

/// Which PDG construction route produced a given combined graph.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PdgBuildRoute {
    /// Compact streaming fragment/segment pipeline
    /// (`fragment_from_pdg` + `merge_fragments_to_segment` + `pdg_from_segment`).
    Streaming,
    /// Legacy per-file extraction + `merge_pdgs`.
    Legacy,
}

/// The construction route selected by the current `StreamingPdg` feature flag.
///
/// This is the single dispatch point the indexing phase consults so the flag
/// cleanly toggles between the streaming fragment pipeline (default) and the
/// legacy merge loop.
pub(crate) fn pdg_route_for_current_flag() -> PdgBuildRoute {
    if crate::feature_flags::FeatureFlag::StreamingPdg.is_enabled() {
        PdgBuildRoute::Streaming
    } else {
        PdgBuildRoute::Legacy
    }
}

/// Build a combined PDG for the freshly parsed files using the selected route.
fn build_changed_file_pdg(
    parsing_results: Vec<crate::parse::parallel::ParsingResult>,
    use_streaming: bool,
) -> (crate::graph::pdg::ProgramDependenceGraph, PdgBuildRoute) {
    if use_streaming {
        (
            build_pdg_streaming(parsing_results),
            PdgBuildRoute::Streaming,
        )
    } else {
        (build_pdg_legacy(parsing_results), PdgBuildRoute::Legacy)
    }
}

/// Legacy route: extract a per-file PDG via `extract_pdg_from_signatures` and
/// merge each into a combined graph (clone-free `merge_pdgs`).
fn build_pdg_legacy(
    parsing_results: Vec<crate::parse::parallel::ParsingResult>,
) -> crate::graph::pdg::ProgramDependenceGraph {
    let mut combined = crate::graph::pdg::ProgramDependenceGraph::new();
    let file_pdgs: Vec<_> = parsing_results
        .into_par_iter()
        .filter(|result| result.is_success())
        .map(|result| {
            let file_path = result.file_path.display().to_string();
            let language = result.language.as_deref().unwrap_or("unknown");
            let source_bytes = extraction_source_bytes(&result.source_bytes, &result.file_path);
            crate::graph::extract_pdg_from_signatures(
                result.signatures,
                &source_bytes,
                &file_path,
                language,
            )
        })
        .collect();
    for file_pdg in file_pdgs {
        index_builder::merge_pdgs(&mut combined, file_pdg);
    }
    combined
}

/// Map a `SignatureInfo` to the canonical streaming node-type string (mirrors
/// the legacy `signature_to_node` mapping so graphs rebuilt from streamed
/// segments stay type-equivalent to the legacy route).
/// Streaming route: build a per-file `PdgFragment` from each parsed file
/// via the real extraction pipeline, merge fragments into a compact
/// segment, and materialize the combined graph from it.
/// them into a compact `PdgSegment` via `merge_fragments_to_segment`, then
/// rebuild the `ProgramDependenceGraph` via `pdg_from_segment`.
fn build_pdg_streaming(
    parsing_results: Vec<crate::parse::parallel::ParsingResult>,
) -> crate::graph::pdg::ProgramDependenceGraph {
    let fragments: Vec<_> = parsing_results
        .into_par_iter()
        .filter(|result| result.is_success())
        .map(|result| {
            let file_path = result.file_path.display().to_string();
            let language = result
                .language
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let source_bytes = extraction_source_bytes(&result.source_bytes, &result.file_path);
            // Route through the REAL extraction pipeline
            // (`extract_pdg_from_signatures` → `fragment_from_pdg`, its
            // documented production realization) instead of the skeleton
            // `build_fragment_from_parsed`. The skeleton flattened
            // signatures to name/kind/bytes, hardcoding complexity 0 and
            // dropping ALL intra-file call/data edges — which is why the
            // streaming route's stored graph showed complexity 0 on every
            // node, empty callee lists, and `forward_impact` returning
            // nothing. The per-file PDG here is single-file (cheap to
            // build), so the streaming memory contract (no whole-graph
            // clone, compact per-file records) is preserved.
            let file_pdg = crate::graph::extract_pdg_from_signatures(
                result.signatures,
                &source_bytes,
                &file_path,
                &language,
            );
            streaming::pdg::fragment_from_pdg(&file_pdg)
        })
        .collect();
    let (segment, stats) = streaming::pdg::merge_fragments_to_segment(fragments);
    info!(
        "Streaming PDG merge: {} fragments, {} nodes, {} edges ({} cross-file resolved, {} unresolved, {} duplicate node records dropped)",
        stats.fragments,
        stats.node_count,
        stats.edge_count,
        stats.cross_file_resolved,
        stats.cross_file_unresolved,
        stats.duplicate_nodes_dropped
    );
    streaming::pdg::pdg_from_segment(&segment)
}
