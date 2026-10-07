use crate::graph::{extract_pdg_from_signatures, pdg::ProgramDependenceGraph};
use crate::parse::{parallel::ParsingResult, prelude::ParallelParser, traits::SignatureInfo};
use crate::phase::docs::{DocsSummary, analyze_docs};
use crate::phase::freshness::{FreshnessState, compute_freshness};
use crate::phase::options::PhaseOptions;
use crate::phase::pdg_utils::merge_pdgs;
use crate::phase::utils::{collect_files, hash_inventory};
use crate::storage::{
    pdg_store::{
        delete_files_data_tx, get_indexed_files, load_pdg, pdg_exists, update_indexed_file,
        update_indexed_files_tx,
    },
    schema::Storage,
};
use anyhow::{Context, Result, bail};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::warn;

/// Shared runtime context reused across all five phases.
pub struct PhaseExecutionContext {
    /// Project root.
    pub root: PathBuf,
    /// Project id.
    pub project_id: String,
    /// Storage backend.
    pub storage: Storage,

    /// Full inventory (path + hash).
    pub file_inventory: Vec<(PathBuf, String)>,
    /// Changed/new files detected by freshness checks.
    pub changed_files: Vec<PathBuf>,
    /// Deleted file paths detected by freshness checks.
    pub deleted_files: Vec<String>,

    /// Parse outputs reused by phases.
    pub parse_results: Vec<ParsingResult>,
    /// Signatures grouped by file path.
    pub signatures_by_file: HashMap<String, (String, Vec<SignatureInfo>)>,
    /// Reused project PDG.
    pub pdg: ProgramDependenceGraph,

    /// Optional docs summary (explicit opt-in only).
    pub docs_summary: Option<DocsSummary>,
    /// Freshness generation hash.
    pub generation_hash: String,

    /// Graph refresh that has not run yet. A run whose phases are all cached
    /// never needs the graph, so loading it (~0.3 s, plus community and
    /// storage work) waits for the first cache miss; see [`Self::ensure_graph`].
    pub(crate) pending_graph: Option<PendingGraph>,
}

/// Inputs for the deferred graph load/refresh.
pub(crate) struct PendingGraph {
    options: PhaseOptions,
    freshness: FreshnessState,
}

impl PhaseExecutionContext {
    /// Prepare execution context using incremental freshness-aware updates.
    pub fn prepare(options: &PhaseOptions) -> Result<Self> {
        if options.root.as_os_str().is_empty() {
            bail!("phase analysis requires an explicit root path");
        }

        let root = options
            .root
            .canonicalize()
            .with_context(|| format!("failed to canonicalize root {}", options.root.display()))?;

        let project_id = project_id(&root);
        let storage = open_storage(&root)?;
        let collected = collect_files(&root, options)?;
        let inventory = hash_inventory(&collected.code_files)?;

        let indexed_files = get_indexed_files(&storage, &project_id).unwrap_or_default();
        let freshness = compute_freshness(&root, inventory, &indexed_files)?;

        let mut context = Self {
            root: root.clone(),
            project_id: project_id.clone(),
            storage,
            file_inventory: freshness.file_inventory.clone(),
            changed_files: freshness.changed_files.clone(),
            deleted_files: freshness.deleted_files.clone(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg: ProgramDependenceGraph::new(),
            docs_summary: None,
            generation_hash: freshness.generation_hash.clone(),
            pending_graph: Some(PendingGraph {
                options: options.clone(),
                freshness,
            }),
        };

        if options.include_docs {
            context.docs_summary = Some(analyze_docs(&collected.docs_files)?);
        }

        Ok(context)
    }

    /// Load or refresh the graph if that has not happened yet. Phases call this
    /// before computing anything; a cached phase result never does.
    pub fn ensure_graph(&mut self) -> Result<()> {
        if let Some(PendingGraph { options, freshness }) = self.pending_graph.take() {
            self.load_or_refresh_graph(&options, &freshness)?;
        }
        Ok(())
    }

    #[cfg(feature = "community")]
    fn compute_and_persist_communities(&mut self) -> Result<()> {
        if !crate::feature_flags::FeatureFlag::CommunityDetection.is_enabled() {
            return Ok(());
        }
        let stats = crate::storage::community_store::compute_and_persist(
            &mut self.storage,
            &self.project_id,
            &mut self.pdg,
        )
        .context("community persistence failed")?;
        tracing::info!(
            communities = stats.community_count,
            quality = stats.quality,
            recompute_ms = stats.recompute_ms,
            "community detection complete"
        );
        Ok(())
    }

    #[cfg(not(feature = "community"))]
    fn compute_and_persist_communities(&mut self) -> Result<()> {
        Ok(())
    }

    #[cfg(feature = "precision")]
    fn run_precision_ingest(&mut self) {
        if !crate::feature_flags::FeatureFlag::PrecisionIngest.is_enabled() {
            return;
        }
        let report = crate::intel::run_precision_ingest(&mut self.pdg, &self.root);
        if report.definitions_seen > 0 || report.relationships_seen > 0 {
            tracing::info!(
                definitions_seen = report.definitions_seen,
                definitions_matched = report.definitions_matched,
                relationships_upgraded = report.relationships_upgraded,
                relationships_added = report.relationships_added,
                "SCIP precision ingest complete"
            );
        }
    }

    #[cfg(not(feature = "precision"))]
    fn run_precision_ingest(&mut self) {}

    #[cfg(feature = "precision")]
    fn run_precision_ingest_for(&self, pdg: &mut ProgramDependenceGraph) {
        if !crate::feature_flags::FeatureFlag::PrecisionIngest.is_enabled() {
            return;
        }
        let report = crate::intel::run_precision_ingest(pdg, &self.root);
        if report.definitions_seen > 0 || report.relationships_seen > 0 {
            tracing::info!(
                definitions_seen = report.definitions_seen,
                definitions_matched = report.definitions_matched,
                relationships_upgraded = report.relationships_upgraded,
                relationships_added = report.relationships_added,
                "SCIP precision ingest complete"
            );
        }
    }

    #[cfg(not(feature = "precision"))]
    fn run_precision_ingest_for(&self, _pdg: &mut ProgramDependenceGraph) {}

    fn should_run_precision_ingest(
        pdg: &ProgramDependenceGraph,
        freshness: &FreshnessState,
    ) -> bool {
        #[cfg(feature = "precision")]
        {
            if !crate::feature_flags::FeatureFlag::PrecisionIngest.is_enabled()
                || !freshness.changed_files.is_empty()
                || !freshness.deleted_files.is_empty()
            {
                return false;
            }

            // The no-change path still needs one precision pass for legacy or
            // Tier-0-only persisted graphs. Once SCIP has confirmed at least
            // one canonical node, the marker set is the cheap durable guard
            // that prevents launching an external indexer on every request.
            pdg.precision_symbols().is_empty()
        }
        #[cfg(not(feature = "precision"))]
        {
            let _ = (pdg, freshness);
            false
        }
    }

    fn load_or_refresh_graph(
        &mut self,
        options: &PhaseOptions,
        freshness: &FreshnessState,
    ) -> Result<()> {
        let has_persisted =
            Self::persisted_graph_exists(&self.root, &self.storage, &self.project_id);

        if options.use_incremental_refresh && has_persisted {
            return self.refresh_persisted_graph(freshness);
        }

        // Cold/full path
        let parse_targets = freshness
            .file_inventory
            .iter()
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>();
        self.parse_results = ParallelParser::new().parse_files(parse_targets);
        self.signatures_by_file = signatures_from_results(&self.root, &self.parse_results);
        let source_bytes_map = source_bytes_from_results(&self.root, &self.parse_results);

        let mut pdg = ProgramDependenceGraph::new();
        merge_file_fragments(
            &self.root,
            &self.signatures_by_file,
            &source_bytes_map,
            &mut pdg,
        );
        self.pdg = pdg;

        self.run_precision_ingest();
        // Communities BEFORE the publish: the generation's Db layer is a
        // vacuum of the mutable catalog, so memberships written after the
        // snapshot would miss the published generation and generation
        // readers would hydrate a graph with no (or stale) community data
        // until another generation was published.
        self.compute_and_persist_communities()
            .context("failed persisting communities for phase analysis")?;
        self.persist_graph_via_generation()
            .context("failed persisting full PDG for phase analysis")?;
        // Analysis-only relink, AFTER persist: the persisted graph must keep
        // the indexer's form (unresolved imports as external placeholders) —
        // persisting the relinked form makes phase analysis and indexing
        // trade graph forms on every alternating run. Only the resident
        // analysis copy is relinked.
        relink_for_analysis(&mut self.pdg);

        let inventory_hashes = inventory_hash_map(&self.root, &freshness.file_inventory);

        for file_path in self.signatures_by_file.keys() {
            let normalized = normalize_file_key(&self.root, file_path);
            if let Some(hash) = inventory_hashes.get(&normalized) {
                if let Err(e) =
                    update_indexed_file(&mut self.storage, &self.project_id, &normalized, hash)
                {
                    warn!(
                        "Phase context: failed to update indexed file record for '{}' (cold path): {}",
                        normalized, e
                    );
                }
            }
        }

        Ok(())
    }

    fn refresh_persisted_graph(&mut self, freshness: &FreshnessState) -> Result<()> {
        let mut pdg = Self::load_persisted_graph(&self.root, &self.storage, &self.project_id)
            .context("failed loading cached PDG for incremental phase run")?;
        self.hydrate_community_memberships(&mut pdg);

        // Collect all file keys that need deletion (from deleted files +
        // changed files) so we can batch them in a single transaction.
        let mut files_to_delete: Vec<String> = Vec::new();
        for path in &freshness.deleted_files {
            remove_file_fragments(&self.root, path, &mut pdg, &mut files_to_delete);
        }

        if !freshness.changed_files.is_empty() {
            let source_bytes_map = self.parse_changed_files(freshness);
            let inventory_hashes = inventory_hash_map(&self.root, &freshness.file_inventory);

            // Collect changed file keys for batch deletion.
            for file_path in self.signatures_by_file.keys() {
                remove_file_fragments(&self.root, file_path, &mut pdg, &mut files_to_delete);
            }

            // Batch-delete all stale file data and update indexed_files in a
            // single transaction to avoid N x fsync overhead.
            let file_updates =
                changed_file_updates(&self.root, &self.signatures_by_file, &inventory_hashes);
            self.persist_stale_file_updates(&files_to_delete, &file_updates);

            // Build new PDG fragments from parsed results.
            merge_file_fragments(
                &self.root,
                &self.signatures_by_file,
                &source_bytes_map,
                &mut pdg,
            );
        } else if !files_to_delete.is_empty() {
            // Deletions-only run: the publish below removes the graph nodes,
            // but the `indexed_files` rows are only cleared here. Skipping
            // the transaction would leave every later freshness pass
            // rediscovering the same deleted files and re-treating the
            // graph as changed, forever.
            self.persist_stale_file_updates(&files_to_delete, &[]);
        }

        let graph_changed =
            !freshness.deleted_files.is_empty() || !self.signatures_by_file.is_empty();
        self.persist_refreshed_graph(&mut pdg, freshness, graph_changed)?;
        Ok(())
    }

    /// Load a persisted graph: prefer the CURRENT generation's Pdg layer,
    /// fall back to the legacy SQL catalog for pre-migration stores.
    fn load_persisted_graph(
        root: &Path,
        storage: &Storage,
        project_id: &str,
    ) -> Result<ProgramDependenceGraph> {
        let storage_root = root.join(".leindex");
        if crate::storage::generation::lease::read_current_generation(&storage_root).is_some() {
            if let Ok(snapshot) =
                crate::storage::generation::GenerationSnapshot::open(&storage_root)
            {
                if let Some(reader) = snapshot.pdg() {
                    match reader.to_program_dependence_graph() {
                        Ok(pdg) => return Ok(pdg),
                        Err(error) => {
                            warn!(
                                %error,
                                "phase graph: Pdg layer decode failed; using SQL fallback"
                            );
                        }
                    }
                }
            }
        }
        load_pdg(storage, project_id).context("no persisted graph in layer or catalog")
    }

    /// Whether a persisted graph exists: a CURRENT generation manifest (the
    /// graph lives in its Pdg layer) or legacy SQL rows.
    fn persisted_graph_exists(root: &Path, storage: &Storage, project_id: &str) -> bool {
        let storage_root = root.join(".leindex");
        let has_generation =
            crate::storage::generation::lease::read_current_generation(&storage_root)
                .map(|generation| {
                    storage_root
                        .join(crate::storage::generation::lease::GENERATIONS_DIR)
                        .join(generation.to_string())
                        .join(crate::storage::generation::lease::MANIFEST_FILE)
                        .exists()
                })
                .unwrap_or(false);
        has_generation || pdg_exists(storage, project_id).unwrap_or(false)
    }

    /// Persist the phase graph by publishing a new CAS generation (D5): the
    /// graph lives in the Pdg layer; the SQL catalog carries only metadata
    /// and `indexed_files`. The generation's file mirror is refreshed so
    /// flag-off readers keep working.
    ///
    /// The whole publish — WAL checkpoint, catalog vacuum, layer staging,
    /// generation allocation, and the `CURRENT` swap — runs under the same
    /// cross-process write lock the indexer holds: a concurrent indexer
    /// picking the same "max existing + 1" number would share
    /// `manifest.partial`/`CURRENT.tmp` with this path, and one writer
    /// would clobber the other or publish mismatched layers.
    fn persist_graph_via_generation(&mut self) -> Result<()> {
        use crate::storage::generation::{
            GenerationWriter, LayerKind, ModelIdentity, graph_codec, migrate as gen_migrate,
        };

        let storage_root = self.root.join(".leindex");
        let _write_lock = crate::storage::ProjectWriteLock::acquire(&storage_root)
            .context("phase graph: acquire project write lock")?;
        let db_path = storage_root.join("leindex.db");
        // WAL checkpoint before vacuum so the layer snapshots committed rows.
        self.storage
            .conn()
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")
            .context("phase graph: WAL checkpoint before publish")?;

        let cas = std::sync::Arc::new(std::sync::Mutex::new(
            crate::storage::cas::CasStore::open(storage_root.join("cas"))
                .context("phase graph: open CAS")?,
        ));
        let mut writer = GenerationWriter::new(&storage_root, cas);
        writer.set_model_identity(ModelIdentity {
            name: "tfidf-hybrid".to_string(),
            digest: String::new(),
            dimensions: 768,
        });

        // Db layer: the mutable-root catalog (metadata + indexed_files).
        let db_bytes = gen_migrate::vacuum_bytes(&db_path)
            .context("phase graph: vacuum catalog for Db layer")?;
        writer.stage(LayerKind::Db, &db_bytes)?;

        // Graph layers from the in-memory graph (D3 codec).
        let (pdg_bytes, _) = graph_codec::encode_pdg_v2_from_graph(&self.pdg)?;
        writer.stage(LayerKind::Pdg, &pdg_bytes)?;
        let symbols_bytes = graph_codec::encode_symbols_layer_from_graph(&self.pdg)?;
        writer.stage(LayerKind::Symbols, &symbols_bytes)?;

        // Vector + optional layers: see `stage_carried_forward_layers` — a
        // phase publish re-encodes only the graph/catalog and must not strip
        // the layers a previous index published.
        let previous = crate::storage::generation::GenerationSnapshot::open(&storage_root).ok();
        Self::stage_carried_forward_layers(&mut writer, previous.as_ref())?;

        // Allocate the next generation number (max existing + 1). Under the
        // write lock above, so a concurrent indexer cannot pick the same
        // number and race this publish for the staging files.
        let next_generation = {
            let max_existing = std::fs::read_dir(storage_root.join("generations"))
                .map(|entries| {
                    entries
                        .flatten()
                        .filter_map(|entry| entry.file_name().to_str()?.parse::<u64>().ok())
                        .max()
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            max_existing.saturating_add(1)
        };

        // The graph lives in the Pdg layer; the generation directory itself
        // carries only metadata (manifest + CURRENT). Flag-off graph readers
        // fall back to the SQL catalog exactly as they did for generations
        // that never carried a mirror, and flag-on readers hydrate the layer.
        writer
            .publish(next_generation)
            .context("phase graph: publish generation")?;
        Ok(())
    }

    /// Stage the non-graph layers onto `writer`: Tfidf/Neural carried
    /// forward from the CURRENT generation when it has them (canonical
    /// empties otherwise — phase runs produce no embeddings), plus the
    /// optional Search/Embedder/Fragments layers when the current generation
    /// carries them. Publishing canonical empties — or omitting the optional
    /// layers — would REPLACE what a previous index published: generation
    /// hydration reads the published layers, so a phase run after an index
    /// would silently strip neural/lexical vector data until the next full
    /// reindex. Vectors for nodes this phase edited are stale by design (the
    /// next full index re-embeds them), which beats losing every vector
    /// outright.
    fn stage_carried_forward_layers(
        writer: &mut crate::storage::generation::GenerationWriter,
        previous: Option<&crate::storage::generation::GenerationSnapshot>,
    ) -> Result<()> {
        use crate::storage::generation::{LayerKind, migrate as gen_migrate};
        for kind in [LayerKind::Tfidf, LayerKind::Neural] {
            match Self::carry_forward_layer(previous, kind) {
                Some(bytes) => {
                    writer.stage(kind, &bytes)?;
                }
                None => {
                    let empty = match kind {
                        LayerKind::Tfidf => gen_migrate::encode_empty_tfidf(),
                        _ => gen_migrate::encode_empty_neural(),
                    };
                    writer.stage(kind, &empty)?;
                }
            }
        }
        for kind in [LayerKind::Search, LayerKind::Embedder, LayerKind::Fragments] {
            if let Some(bytes) = Self::carry_forward_layer(previous, kind) {
                writer.stage(kind, &bytes)?;
            }
        }
        Ok(())
    }

    /// The CURRENT generation's bytes for `kind`, or `None` when there is no
    /// current generation, the layer is absent, or the read fails (the caller
    /// falls back to its canonical layer; a failed carry-forward must not
    /// fail the publish).
    fn carry_forward_layer(
        previous: Option<&crate::storage::generation::GenerationSnapshot>,
        kind: crate::storage::generation::LayerKind,
    ) -> Option<Vec<u8>> {
        let snapshot = previous?;
        match snapshot.layer_bytes(kind) {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(
                    %error,
                    layer = %kind,
                    "phase graph: carry-forward read failed; staging the canonical layer"
                );
                None
            }
        }
    }

    /// Hydrate persisted community memberships into a loaded PDG. Failures
    /// are non-fatal: analysis proceeds without community data.
    fn hydrate_community_memberships(&self, pdg: &mut ProgramDependenceGraph) {
        #[cfg(feature = "community")]
        if let Err(error) = crate::storage::community_store::load_community_memberships(
            &self.storage,
            &self.project_id,
            crate::graph::community::COMMUNITY_ALGORITHM,
            crate::graph::community::COMMUNITY_QUALITY,
            crate::graph::community::COMMUNITY_RESOLUTION,
            pdg,
        ) {
            warn!(%error, "Phase context: failed to hydrate community memberships");
        }
        #[cfg(not(feature = "community"))]
        let _ = pdg;
    }

    /// Parse the changed files detected by freshness and store the parse
    /// results and per-file signatures on the context. Returns the parsed
    /// source bytes keyed by normalized file path.
    fn parse_changed_files(&mut self, freshness: &FreshnessState) -> HashMap<String, Vec<u8>> {
        self.parse_results = ParallelParser::new().parse_files(freshness.changed_files.clone());
        self.signatures_by_file = signatures_from_results(&self.root, &self.parse_results);
        source_bytes_from_results(&self.root, &self.parse_results)
    }

    /// Batch-delete stale file data and update indexed-file records in a
    /// single transaction to avoid N x fsync overhead. Individual statement
    /// failures are logged and skipped; the rest of the transaction commits.
    fn persist_stale_file_updates(
        &mut self,
        files_to_delete: &[String],
        file_updates: &[(String, String)],
    ) {
        if files_to_delete.is_empty() && file_updates.is_empty() {
            return;
        }
        let tx = match self.storage.conn_mut().transaction() {
            Ok(tx) => tx,
            Err(e) => {
                warn!("Phase context: failed to commit batch transaction: {}", e);
                return;
            }
        };
        if !files_to_delete.is_empty() {
            if let Err(e) = delete_files_data_tx(&tx, &self.project_id, files_to_delete) {
                warn!(
                    "Phase context: failed to batch-delete file data for {} files: {}",
                    files_to_delete.len(),
                    e
                );
            }
        }
        if !file_updates.is_empty() {
            if let Err(e) = update_indexed_files_tx(&tx, &self.project_id, file_updates) {
                warn!(
                    "Phase context: failed to batch-update {} indexed file records: {}",
                    file_updates.len(),
                    e
                );
            }
        }
        if let Err(e) = tx.commit() {
            warn!("Phase context: failed to commit batch transaction: {}", e);
        }
    }

    /// Persist the refreshed PDG (running precision ingest when applicable),
    /// then install it as the analysis graph.
    fn persist_refreshed_graph(
        &mut self,
        pdg: &mut ProgramDependenceGraph,
        freshness: &FreshnessState,
        graph_changed: bool,
    ) -> Result<()> {
        self.pdg = std::mem::take(pdg);
        if graph_changed || Self::should_run_precision_ingest(&self.pdg, freshness) {
            // A Tier-0 graph can predate precision ingest (or have no
            // matched markers yet): run the opt-in pass, then persist.
            let mut enriched = std::mem::take(&mut self.pdg);
            self.run_precision_ingest_for(&mut enriched);
            self.pdg = enriched;
            // Communities BEFORE the publish: the Db layer vacuums the
            // mutable catalog, so memberships written after the snapshot
            // would miss the published generation.
            if graph_changed {
                self.compute_and_persist_communities()
                    .context("failed persisting communities for phase analysis")?;
            }
            self.persist_graph_via_generation()
                .context("failed persisting refreshed PDG")?;
        }

        // Analysis-only relink, AFTER persist: the persisted graph keeps the
        // indexer's form (unresolved imports as external placeholders);
        // persisting the relinked form made phase analysis and indexing
        // trade graph forms on every alternating run (the exact ping-pong
        // `relink_for_analysis`'s contract documents as in-memory-only).
        // Only the resident analysis copy is relinked.
        relink_for_analysis(&mut self.pdg);
        Ok(())
    }
}

/// Resolve import edges to internal symbols for the analysis, in memory only.
///
/// The persisted graph is shared with the indexer, which keeps unresolved
/// imports as external placeholder nodes. Saving the resolved form made the
/// two writers trade thousands of nodes on every alternating run (a phase call
/// after an index spent ~15 s deleting ~4,000 external nodes, and the next
/// index re-created them). The analysis sees the resolved graph; storage keeps
/// the form the indexer wrote.
fn relink_for_analysis(pdg: &mut ProgramDependenceGraph) {
    crate::phase::pdg_utils::relink_external_import_edges(
        pdg,
        &crate::phase::pdg_utils::RelinkConfig::default(),
    );
}

fn signatures_from_results(
    root: &Path,
    results: &[ParsingResult],
) -> HashMap<String, (String, Vec<SignatureInfo>)> {
    results
        .iter()
        .filter_map(|result| {
            if !result.is_success() {
                return None;
            }

            let language = result
                .language
                .clone()
                .unwrap_or_else(|| "unknown".to_string());
            let file = normalize_file_key(root, &result.file_path.display().to_string());
            Some((file, (language, result.signatures.clone())))
        })
        .collect()
}

/// Build a map from file path → source bytes from ParsingResults.
/// Returns an empty Vec for results without source_bytes.
fn source_bytes_from_results(root: &Path, results: &[ParsingResult]) -> HashMap<String, Vec<u8>> {
    results
        .iter()
        .filter_map(|result| {
            if !result.is_success() {
                return None;
            }
            let file = normalize_file_key(root, &result.file_path.display().to_string());
            Some((file, result.source_bytes.clone().unwrap_or_default()))
        })
        .collect()
}

/// Inventory hashes keyed by normalized file path.
fn inventory_hash_map(
    root: &Path,
    file_inventory: &[(PathBuf, String)],
) -> HashMap<String, String> {
    file_inventory
        .iter()
        .map(|(path, hash)| {
            (
                normalize_file_key(root, &path.display().to_string()),
                hash.clone(),
            )
        })
        .collect()
}

/// Remove a file's old fragment from the PDG and record every equivalent
/// key for it in `files_to_delete` (the keys are sorted by
/// [`equivalent_file_keys`], so removal order is deterministic).
fn remove_file_fragments(
    root: &Path,
    file: &str,
    pdg: &mut ProgramDependenceGraph,
    files_to_delete: &mut Vec<String>,
) {
    for key in equivalent_file_keys(root, file) {
        pdg.remove_file(&key);
        files_to_delete.push(key);
    }
}

/// Indexed-file record updates `(path, hash)` for every parsed file that the
/// current inventory has a hash for.
fn changed_file_updates(
    root: &Path,
    signatures_by_file: &HashMap<String, (String, Vec<SignatureInfo>)>,
    inventory_hashes: &HashMap<String, String>,
) -> Vec<(String, String)> {
    let mut file_updates: Vec<(String, String)> = Vec::new();
    for file_path in signatures_by_file.keys() {
        let normalized = normalize_file_key(root, file_path);
        if let Some(hash) = inventory_hashes.get(&normalized) {
            file_updates.push((normalized.clone(), hash.clone()));
        }
    }
    file_updates
}

/// Build per-file PDG fragments from the parsed signatures and merge them
/// into `pdg`. Uses source bytes captured at parse time when available,
/// falling back to a disk read.
fn merge_file_fragments(
    root: &Path,
    signatures_by_file: &HashMap<String, (String, Vec<SignatureInfo>)>,
    source_bytes_map: &HashMap<String, Vec<u8>>,
    pdg: &mut ProgramDependenceGraph,
) {
    for (file_path, (language, signatures)) in signatures_by_file {
        // Use source_bytes from ParsingResult when available, fall back to disk read
        let source_bytes_fallback = source_bytes_for_file(root, file_path);
        let source_bytes = source_bytes_map
            .get(file_path)
            .map(|s| s.as_slice())
            .unwrap_or_else(|| source_bytes_fallback.as_slice());
        let file_pdg =
            extract_pdg_from_signatures(signatures.clone(), source_bytes, file_path, language);
        merge_pdgs(pdg, &file_pdg);
    }
}

fn project_id(root: &Path) -> String {
    root.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string()
}

fn normalize_file_key(root: &Path, file: &str) -> String {
    let path = Path::new(file);

    if path.is_relative() {
        return path.display().to_string();
    }

    if let Ok(relative) = path.strip_prefix(root) {
        return relative.display().to_string();
    }

    if let Ok(absolute) = path.canonicalize() {
        if let Ok(relative) = absolute.strip_prefix(root) {
            return relative.display().to_string();
        }
        return absolute.display().to_string();
    }

    path.display().to_string()
}

fn source_bytes_for_file(root: &Path, file: &str) -> Vec<u8> {
    let path = Path::new(file);
    if path.is_relative() {
        std::fs::read(root.join(path)).unwrap_or_default()
    } else {
        std::fs::read(path).unwrap_or_default()
    }
}

fn equivalent_file_keys(root: &Path, file: &str) -> Vec<String> {
    let mut keys = Vec::new();
    let normalized = normalize_file_key(root, file);
    keys.push(normalized.clone());

    let absolute = Path::new(file);
    if absolute.is_relative() {
        keys.push(root.join(absolute).display().to_string());
    } else {
        keys.push(absolute.display().to_string());
    }

    keys.sort();
    keys.dedup();
    keys
}

fn open_storage(root: &Path) -> Result<Storage> {
    let dir = root.join(".leindex");
    std::fs::create_dir_all(&dir).context("failed creating .leindex directory")?;
    let db_path = dir.join("leindex.db");
    Storage::open(db_path).context("failed opening phase storage")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::traits::{SignatureInfo, Visibility};

    #[test]
    fn signatures_from_results_filters_out_failed_parses() {
        let success = ParsingResult {
            file_path: PathBuf::from("src/main.rs"),
            language: Some("rust".to_string()),
            signatures: vec![SignatureInfo {
                name: "main".to_string(),
                qualified_name: "main".to_string(),
                parameters: Vec::new(),
                return_type: None,
                visibility: Visibility::Public,
                is_async: false,
                is_method: false,
                docstring: None,
                calls: Vec::new(),
                imports: Vec::new(),
                byte_range: (0, 10),
                flow_facts: vec![],

                cyclomatic_complexity: 0,
            }],
            error: None,
            parse_time_ms: 1,
            source_bytes: None,
        };

        let failure = ParsingResult {
            file_path: PathBuf::from("src/bad.rs"),
            language: None,
            signatures: Vec::new(),
            error: Some("Parse error: test".to_string()),
            parse_time_ms: 0,
            source_bytes: None,
        };

        let grouped = signatures_from_results(Path::new("."), &[success, failure]);
        assert_eq!(grouped.len(), 1);
        assert!(grouped.contains_key("src/main.rs"));
        assert!(!grouped.contains_key("src/bad.rs"));
    }

    #[test]
    fn signatures_from_results_defaults_unknown_language() {
        let success_without_language = ParsingResult {
            file_path: PathBuf::from("src/main.rs"),
            language: None,
            signatures: vec![SignatureInfo {
                name: "main".to_string(),
                qualified_name: "main".to_string(),
                parameters: Vec::new(),
                return_type: None,
                visibility: Visibility::Public,
                is_async: false,
                is_method: false,
                docstring: None,
                calls: Vec::new(),
                imports: Vec::new(),
                byte_range: (0, 1),
                flow_facts: vec![],

                cyclomatic_complexity: 0,
            }],
            error: None,
            parse_time_ms: 1,
            source_bytes: None,
        };

        let grouped = signatures_from_results(Path::new("."), &[success_without_language]);
        assert_eq!(
            grouped.get("src/main.rs").map(|(l, _)| l.as_str()),
            Some("unknown")
        );
    }

    #[test]
    fn normalize_file_key_prefers_project_relative_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path();
        let absolute = root.join("src/lib.rs");
        std::fs::create_dir_all(absolute.parent().expect("parent")).expect("mkdir");
        std::fs::write(&absolute, "pub fn x(){}\n").expect("write");

        let normalized = normalize_file_key(root, &absolute.display().to_string());
        assert_eq!(normalized, "src/lib.rs");

        let already_relative = normalize_file_key(root, "src/lib.rs");
        assert_eq!(already_relative, "src/lib.rs");
    }

    #[test]
    fn normalize_file_key_keeps_absolute_paths_outside_root() {
        let root_dir = tempfile::tempdir().expect("root");
        let other_dir = tempfile::tempdir().expect("other");
        let outside = other_dir.path().join("outside.rs");
        std::fs::write(&outside, "pub fn y(){}\n").expect("write");

        let normalized = normalize_file_key(root_dir.path(), &outside.display().to_string());
        assert!(normalized.starts_with('/'));
        assert!(normalized.ends_with("outside.rs"));
    }

    #[test]
    fn prepare_requires_explicit_root_path() {
        let err = PhaseExecutionContext::prepare(&PhaseOptions::default())
            .err()
            .expect("must fail");
        assert!(
            err.to_string().contains("explicit root path"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn cold_path_does_not_mark_failed_parse_files_as_indexed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let missing_file = root.join("src/missing.rs");

        let storage = open_storage(&root).expect("open storage");
        let project_id = project_id(&root);

        let mut context = PhaseExecutionContext {
            root: root.clone(),
            project_id: project_id.clone(),
            storage,
            file_inventory: Vec::new(),
            changed_files: vec![missing_file.clone()],
            deleted_files: Vec::new(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg: ProgramDependenceGraph::new(),
            docs_summary: None,
            generation_hash: "gen".to_string(),
            pending_graph: None,
        };

        let freshness = FreshnessState {
            generation_hash: "gen".to_string(),
            file_inventory: vec![(missing_file.clone(), "hash".to_string())],
            changed_files: vec![missing_file],
            deleted_files: Vec::new(),
        };

        let options = PhaseOptions {
            root,
            use_incremental_refresh: false,
            ..PhaseOptions::default()
        };

        context
            .load_or_refresh_graph(&options, &freshness)
            .expect("cold refresh");

        assert_eq!(context.signatures_by_file.len(), 0);
        let indexed = get_indexed_files(&context.storage, &project_id).expect("indexed files");
        assert!(
            indexed.is_empty(),
            "failed parse files must not be recorded as indexed"
        );
    }

    #[test]
    fn persisted_refresh_replaces_changed_file_graph_and_updates_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let file = root.join("src/lib.rs");
        std::fs::create_dir_all(file.parent().expect("parent")).expect("mkdir");
        std::fs::write(&file, "pub fn before() {}\n").expect("write initial source");

        let storage = open_storage(&root).expect("open storage");
        let project_id = project_id(&root);
        let mut context = PhaseExecutionContext {
            root: root.clone(),
            project_id: project_id.clone(),
            storage,
            file_inventory: Vec::new(),
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg: ProgramDependenceGraph::new(),
            docs_summary: None,
            generation_hash: "initial".to_string(),
            pending_graph: None,
        };
        let initial_freshness = FreshnessState {
            generation_hash: "initial".to_string(),
            file_inventory: vec![(file.clone(), "initial-hash".to_string())],
            changed_files: vec![file.clone()],
            deleted_files: Vec::new(),
        };
        let full_options = PhaseOptions {
            root: root.clone(),
            use_incremental_refresh: false,
            ..PhaseOptions::default()
        };

        context
            .load_or_refresh_graph(&full_options, &initial_freshness)
            .expect("initial full refresh");
        assert!(PhaseExecutionContext::persisted_graph_exists(
            &context.root,
            &context.storage,
            &project_id
        ));

        std::fs::write(&file, "pub fn after() {}\n").expect("write changed source");
        let incremental_freshness = FreshnessState {
            generation_hash: "changed".to_string(),
            file_inventory: vec![(file.clone(), "changed-hash".to_string())],
            changed_files: vec![file],
            deleted_files: Vec::new(),
        };
        let incremental_options = PhaseOptions {
            root,
            use_incremental_refresh: true,
            ..PhaseOptions::default()
        };

        context
            .load_or_refresh_graph(&incremental_options, &incremental_freshness)
            .expect("persisted incremental refresh");

        let node_names = context
            .pdg
            .node_indices()
            .filter_map(|id| context.pdg.get_node(id).map(|node| node.name.as_str()))
            .collect::<Vec<_>>();
        assert!(node_names.contains(&"after"));
        assert!(!node_names.contains(&"before"));
        assert_eq!(
            get_indexed_files(&context.storage, &project_id)
                .expect("indexed files")
                .get("src/lib.rs")
                .map(String::as_str),
            Some("changed-hash")
        );
    }

    #[cfg(feature = "precision")]
    #[test]
    fn test_no_change_precision_trigger_is_gated_by_flag_and_markers() {
        let freshness = FreshnessState::default();
        let mut marked = ProgramDependenceGraph::new();
        marked.mark_precision_symbol("src/main.py:main");

        crate::feature_flags::with_flag_override(
            crate::feature_flags::FeatureFlag::PrecisionIngest,
            true,
            || {
                assert!(PhaseExecutionContext::should_run_precision_ingest(
                    &ProgramDependenceGraph::new(),
                    &freshness
                ));
                assert!(!PhaseExecutionContext::should_run_precision_ingest(
                    &marked, &freshness
                ));
            },
        );
        crate::feature_flags::with_flag_override(
            crate::feature_flags::FeatureFlag::PrecisionIngest,
            false,
            || {
                assert!(!PhaseExecutionContext::should_run_precision_ingest(
                    &ProgramDependenceGraph::new(),
                    &freshness
                ));
            },
        );
    }

    #[cfg(all(feature = "precision", unix))]
    #[test]
    fn test_no_change_persisted_refresh_runs_precision_ingest() {
        use protobuf::Message;
        use scip::types::{self, PositionEncoding, SymbolRole};
        use std::os::unix::fs::PermissionsExt;
        use std::sync::Arc;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().to_path_buf();
        let source = root.join("src/main.py");
        std::fs::create_dir_all(source.parent().expect("source parent")).expect("mkdir");
        std::fs::write(&source, "def main():\n    pass\n").expect("write source");

        let mut index = types::Index::new();
        let mut document = types::Document::new();
        document.relative_path = "src/main.py".to_string();
        document.text = "def main():\n    pass\n".to_string();
        document.position_encoding =
            protobuf::EnumOrUnknown::new(PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
        let mut symbol = types::SymbolInformation::new();
        symbol.symbol = "python test src/main.py/main".to_string();
        symbol.display_name = "main".to_string();
        let mut occurrence = types::Occurrence::new();
        occurrence.symbol = symbol.symbol.clone();
        occurrence.symbol_roles = SymbolRole::Definition as i32;
        occurrence.range = vec![0, 4, 0, 8];
        document.occurrences.push(occurrence);
        document.symbols.push(symbol);
        index.documents.push(document);
        std::fs::write(
            root.join(".scip-fixture"),
            index.write_to_bytes().expect("encode SCIP fixture"),
        )
        .expect("write SCIP fixture");

        let indexer_dir = tempfile::tempdir().expect("indexer tempdir");
        let indexer = indexer_dir.path().join("scip-python-fixture.sh");
        std::fs::write(&indexer, "#!/bin/sh\ncp \"$1/.scip-fixture\" \"$2\"\n")
            .expect("write indexer fixture");
        let mut permissions = std::fs::metadata(&indexer)
            .expect("indexer metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&indexer, permissions).expect("chmod indexer fixture");

        let storage = open_storage(&root).expect("open storage");
        let project_id = project_id(&root);
        let mut persisted = ProgramDependenceGraph::new();
        persisted.add_node(crate::graph::pdg::Node {
            id: "src/main.py:main".to_string(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "main".to_string(),
            file_path: Arc::from("src/main.py"),
            byte_range: (4, 8),
            complexity: 1,
            language: "python".to_string(),
        });
        let mut storage = storage;
        // Seed a Tier-0 graph the way a pre-migration legacy store holds it:
        // raw SQL rows (the layer path publishes a generation instead).
        storage
            .conn()
            .execute(
                "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, complexity, content_hash, byte_range_start, byte_range_end, created_at, updated_at)
                 VALUES (?1, 'src/main.py', 'src/main.py:main', 'main', 'main', 'python', 'function', 1, 'seed-hash', 4, 8, 0, 0)",
                rusqlite::params![project_id],
            )
            .expect("seed Tier-0 graph row");
        update_indexed_file(&mut storage, &project_id, "src/main.py", "hash")
            .expect("save indexed file");

        let mut context = PhaseExecutionContext {
            root: root.clone(),
            project_id: project_id.clone(),
            storage,
            file_inventory: vec![(source.clone(), "hash".to_string())],
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
            parse_results: Vec::new(),
            signatures_by_file: HashMap::new(),
            pdg: ProgramDependenceGraph::new(),
            docs_summary: None,
            generation_hash: "same".to_string(),
            pending_graph: None,
        };
        let freshness = FreshnessState {
            generation_hash: "same".to_string(),
            file_inventory: vec![(source, "hash".to_string())],
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
        };

        let _flag_guard = crate::feature_flags::lock_flag_tests();
        crate::feature_flags::set_flag_override_for_test(
            crate::feature_flags::FeatureFlag::PrecisionIngest,
            true,
        );
        unsafe {
            std::env::set_var("LEINDEX_SCIP_PYTHON_BIN", &indexer);
            std::env::set_var("LEINDEX_SCIP_MIN_AVAILABLE_MB", "0");
            std::env::set_var("LEINDEX_SCIP_TIMEOUT_SECS", "1");
        }
        let refresh = context.refresh_persisted_graph(&freshness);
        unsafe {
            std::env::remove_var("LEINDEX_SCIP_PYTHON_BIN");
            std::env::remove_var("LEINDEX_SCIP_MIN_AVAILABLE_MB");
            std::env::remove_var("LEINDEX_SCIP_TIMEOUT_SECS");
        }
        crate::feature_flags::clear_flag_overrides_for_test();
        refresh.expect("no-change precision refresh");

        assert!(context.pdg.is_precision_symbol("src/main.py:main"));
        // Post-D5 the enriched graph persists as the CURRENT generation's
        // Pdg layer; reload through the layer path and re-check the marker.
        let reloaded = PhaseExecutionContext::load_persisted_graph(
            &context.root,
            &context.storage,
            &project_id,
        )
        .expect("reload persisted graph");
        assert!(reloaded.is_precision_symbol("src/main.py:main"));
    }
    #[test]
    fn test_prepare_defers_graph_until_first_use() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn alpha() {}\npub fn beta() { alpha() }\n",
        )
        .expect("write");
        let options = PhaseOptions {
            root: dir.path().to_path_buf(),
            ..PhaseOptions::default()
        };
        let mut context = PhaseExecutionContext::prepare(&options).expect("prepare");
        assert!(context.pending_graph.is_some());
        assert_eq!(
            context.pdg.node_count(),
            0,
            "an all-cached run must not pay for the graph"
        );
        assert!(
            !context.file_inventory.is_empty(),
            "freshness is still computed"
        );

        context.ensure_graph().expect("ensure graph");
        assert!(context.pending_graph.is_none());
        assert!(context.pdg.node_count() > 0);
        context.ensure_graph().expect("second call is a no-op");
    }

    /// Deletions-only incremental runs must persist their `indexed_files`
    /// deletions (round-10 Codex P2): the batch transaction used to live in
    /// the changed-files-only branch, so a run that only deleted files left
    /// the rows in place — every later freshness pass rediscovered the same
    /// deleted files and re-treated the graph as changed, forever.
    #[test]
    fn test_deletions_only_run_persists_indexed_file_removals() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/keep.rs"), "pub fn keep() {}\n").expect("write");
        let options = PhaseOptions {
            root: dir.path().to_path_buf(),
            ..PhaseOptions::default()
        };
        let mut context = PhaseExecutionContext::prepare(&options).expect("prepare");
        let project_id = context.project_id.clone();

        // The previously indexed tree contained src/gone.py; the file is now
        // deleted from disk and from the inventory. Seed the graph the way a
        // pre-flip legacy store holds it: raw SQL rows.
        context
            .storage
            .conn()
            .execute(
                "INSERT INTO intel_nodes (project_id, file_path, node_id, symbol_name, qualified_name, language, node_type, complexity, content_hash, byte_range_start, byte_range_end, created_at, updated_at)
                 VALUES (?1, 'src/gone.py', 'src/gone.py:main', 'main', 'main', 'python', 'function', 0, 'seed-hash', 0, 4, 0, 0)",
                rusqlite::params![project_id],
            )
            .expect("seed persisted graph row");
        crate::storage::pdg_store::update_indexed_file(
            &mut context.storage,
            &project_id,
            "src/gone.py",
            "old-hash",
        )
        .expect("seed indexed file");

        let freshness = FreshnessState {
            generation_hash: "after-delete".to_string(),
            file_inventory: Vec::new(),
            changed_files: Vec::new(),
            deleted_files: vec!["src/gone.py".to_string()],
        };
        context
            .refresh_persisted_graph(&freshness)
            .expect("deletions-only refresh");

        let indexed = crate::storage::pdg_store::get_indexed_files(&context.storage, &project_id)
            .expect("read indexed files");
        assert!(
            !indexed.contains_key("src/gone.py"),
            "the deleted file's indexed_files row must be removed, got: {indexed:?}"
        );
    }
    /// A phase republish must CARRY FORWARD the current generation's vector
    /// and optional layers (round-11 Codex P1): publishing canonical empty
    /// Tfidf/Neural layers — and omitting Search/Embedder/Fragments —
    /// replaced what a previous index published, so a phase run after an
    /// index silently stripped neural/lexical vector data until the next
    /// full reindex.
    #[test]
    fn test_phase_publish_carries_forward_vector_and_optional_layers() {
        use crate::storage::generation::{
            GenerationSnapshot, GenerationWriter, LayerKind, ModelIdentity,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(dir.path().join("src/lib.rs"), "pub fn alpha() {}\n").expect("write");
        // Incremental refresh OFF: the second publish must take the cold
        // path (which republishes unconditionally) so the carry-forward is
        // actually exercised.
        let options = PhaseOptions {
            root: dir.path().to_path_buf(),
            use_incremental_refresh: false,
            ..PhaseOptions::default()
        };
        let mut context = PhaseExecutionContext::prepare(&options).expect("prepare");
        context.ensure_graph().expect("cold publish");
        let storage_root = context.root.join(".leindex");

        // A later index publishes real vector layers plus an optional one:
        // marker bytes stand in for the embeddings the indexer would write
        // (layers are CAS blobs keyed by content hash, so byte equality is
        // hash equality).
        let cas = std::sync::Arc::new(std::sync::Mutex::new(
            crate::storage::cas::CasStore::open(storage_root.join("cas")).expect("cas"),
        ));
        let mut writer = GenerationWriter::new(&storage_root, cas);
        writer.set_model_identity(ModelIdentity {
            name: "tfidf-hybrid".to_string(),
            digest: String::new(),
            dimensions: 768,
        });
        let db_bytes =
            crate::storage::generation::migrate::vacuum_bytes(&storage_root.join("leindex.db"))
                .expect("vacuum");
        writer.stage(LayerKind::Db, &db_bytes).expect("stage db");
        let (pdg_bytes, _) =
            crate::storage::generation::graph_codec::encode_pdg_v2_from_graph(&context.pdg)
                .expect("encode pdg");
        writer.stage(LayerKind::Pdg, &pdg_bytes).expect("stage pdg");
        let symbols_bytes =
            crate::storage::generation::graph_codec::encode_symbols_layer_from_graph(&context.pdg)
                .expect("encode symbols");
        writer
            .stage(LayerKind::Symbols, &symbols_bytes)
            .expect("stage symbols");
        // A REAL non-empty Tfidf layer (sparse LIDX-TFD1, encoded by the same
        // migrator the indexer uses from a synthesized one-row legacy LIEE
        // file): the snapshot's eager reader validation rejects arbitrary
        // bytes, which is exactly why the canonical empties are invalid
        // stand-ins for real layers.
        let liee_path = dir.path().join("tfidf.bin");
        std::fs::write(&liee_path, liee_one_row("idx-node-1", &[1.5, 0.0]))
            .expect("write liee fixture");
        let tfidf_bytes = crate::storage::generation::migrate::encode_tfidf_layer(
            &liee_path,
            &std::collections::HashMap::from([("idx-node-1".to_string(), 1u32)]),
        )
        .expect("encode tfidf layer");
        assert_ne!(
            tfidf_bytes,
            crate::storage::generation::migrate::encode_empty_tfidf(),
            "fixture must be a non-empty layer to be distinguishable from the fallback"
        );
        writer
            .stage(LayerKind::Tfidf, &tfidf_bytes)
            .expect("stage tfidf");
        // Neural has no legacy artifact in this fixture: the canonical empty
        // is what a real index without a neural model publishes.
        writer
            .stage(
                LayerKind::Neural,
                &crate::storage::generation::migrate::encode_empty_neural(),
            )
            .expect("stage neural");
        // Fragments is optional and NOT eagerly validated by snapshot open —
        // raw marker bytes stand in for the indexer's fragment bundle.
        let fragments_marker = b"real-fragments-layer-bytes".to_vec();
        writer
            .stage(LayerKind::Fragments, &fragments_marker)
            .expect("stage fragments");
        writer.publish(2).expect("publish generation 2");

        // The phase runs again (a no-op refresh still republishes through
        // ensure_graph's cold path in this harness). The new CURRENT
        // generation must carry the published layers forward, byte for byte.
        let mut context = PhaseExecutionContext::prepare(&options).expect("re-prepare");
        context.ensure_graph().expect("second publish");

        let snapshot = GenerationSnapshot::open(&storage_root).expect("open current");
        assert_eq!(
            snapshot
                .layer_bytes(LayerKind::Tfidf)
                .expect("tfidf")
                .as_deref(),
            Some(tfidf_bytes.as_slice()),
            "the phase publish must not replace the published Tfidf layer with an empty one"
        );
        // Neural had no previous non-empty layer: the canonical empty is the
        // correct fallback (and proves the fallback arm, not a skip).
        assert_eq!(
            snapshot
                .layer_bytes(LayerKind::Neural)
                .expect("neural")
                .as_deref(),
            Some(crate::storage::generation::migrate::encode_empty_neural().as_slice()),
            "a phase publish with no previous Neural layer stages the canonical empty"
        );
        assert_eq!(
            snapshot
                .layer_bytes(LayerKind::Fragments)
                .expect("fragments")
                .as_deref(),
            Some(fragments_marker.as_slice()),
            "the phase publish must not drop the optional Fragments layer from the manifest"
        );
    }

    /// Synthesize a minimal legacy LIEE embeddings file (one row) for the
    /// Tfidf-layer encoder.
    fn liee_one_row(id: &str, vector: &[f32]) -> Vec<u8> {
        let id_bytes = id.as_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(b"LIEE");
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&1u32.to_le_bytes());
        out.extend_from_slice(&(vector.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u64.to_le_bytes());
        out.extend_from_slice(&(id_bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(id_bytes);
        while out.len() % 4 != 0 {
            out.push(0);
        }
        for value in vector {
            out.extend_from_slice(&value.to_le_bytes());
        }
        out
    }

    /// The analysis-only relink must stay OUT of the persisted graph
    /// (round-11 Codex P1): the refreshed graph is persisted BEFORE the
    /// relink, so the published Pdg layer keeps the indexer's form
    /// (unresolved imports as external placeholders) while only the resident
    /// analysis copy is relinked. Persisting the relinked form made phase
    /// analysis and indexing trade graph forms on every alternating run.
    #[test]
    fn test_refresh_persists_unrelinked_graph_and_relinks_only_the_resident_copy() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("src")).expect("mkdir");
        std::fs::write(
            dir.path().join("src/lib.rs"),
            "pub fn alpha() {}\npub fn beta() { alpha() }\n",
        )
        .expect("write");
        let options = PhaseOptions {
            root: dir.path().to_path_buf(),
            ..PhaseOptions::default()
        };
        let mut context = PhaseExecutionContext::prepare(&options).expect("prepare");
        context.ensure_graph().expect("cold publish");

        // Hand-build a refreshed graph in the indexer's form: an import edge
        // from a real node to an external placeholder whose name matches the
        // real `beta` definition — exactly the edge relinking replaces (the
        // placeholder then becomes an orphan and is removed).
        let mut pdg = ProgramDependenceGraph::new();
        let caller = pdg.add_node(crate::graph::pdg::Node {
            id: "src/lib.rs:alpha".to_string(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "alpha".to_string(),
            file_path: std::sync::Arc::from("src/lib.rs"),
            byte_range: (0, 10),
            complexity: 1,
            language: "rust".to_string(),
        });
        let beta = pdg.add_node(crate::graph::pdg::Node {
            id: "src/lib.rs:beta".to_string(),
            node_type: crate::graph::pdg::NodeType::Function,
            name: "beta".to_string(),
            file_path: std::sync::Arc::from("src/lib.rs"),
            byte_range: (12, 30),
            complexity: 1,
            language: "rust".to_string(),
        });
        let placeholder = pdg.add_node(crate::graph::pdg::Node {
            id: "external::beta".to_string(),
            node_type: crate::graph::pdg::NodeType::External,
            name: "beta".to_string(),
            file_path: std::sync::Arc::from("src/lib.rs"),
            byte_range: (0, 0),
            complexity: 0,
            language: "external".to_string(),
        });
        pdg.add_edge(
            caller,
            placeholder,
            crate::graph::pdg::Edge {
                edge_type: crate::graph::pdg::EdgeType::Import,
                metadata: crate::graph::pdg::EdgeMetadata::empty(),
            },
        );

        // Only `graph_changed` matters here (the precision gate is flag-off
        // in tests); the freshness fields are read by the caller, not this
        // persistence step.
        let freshness = FreshnessState {
            generation_hash: "gen-2".to_string(),
            file_inventory: Vec::new(),
            changed_files: Vec::new(),
            deleted_files: Vec::new(),
        };
        context
            .persist_refreshed_graph(&mut pdg, &freshness, true)
            .expect("persist refreshed graph");

        // The PUBLISHED graph keeps the indexer's form: the placeholder node
        // survives. The RESIDENT analysis graph is relinked: the placeholder
        // is gone and the import edge points at the real definition.
        let persisted = PhaseExecutionContext::load_persisted_graph(
            &context.root,
            &context.storage,
            &context.project_id,
        )
        .expect("reload persisted graph");
        assert!(
            persisted.find_by_id("external::beta").is_some(),
            "the persisted Pdg layer must keep the indexer's external placeholder"
        );
        assert!(
            context.pdg.find_by_id("external::beta").is_none(),
            "the resident analysis graph is relinked (placeholder gone)"
        );
        assert!(
            has_import_edge(&context.pdg, caller, beta),
            "the relinked import edge points at the internal definition"
        );
    }

    fn has_import_edge(
        pdg: &ProgramDependenceGraph,
        from: crate::graph::pdg::NodeId,
        to: crate::graph::pdg::NodeId,
    ) -> bool {
        use petgraph::visit::EdgeRef;
        pdg.graph.edges(from).any(|reference| {
            reference.target() == to
                && reference.weight().edge_type == crate::graph::pdg::EdgeType::Import
        })
    }
}
