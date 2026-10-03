use crate::graph::{extract_pdg_from_signatures, pdg::ProgramDependenceGraph};
use crate::parse::{parallel::ParsingResult, prelude::ParallelParser, traits::SignatureInfo};
use crate::phase::docs::{DocsSummary, analyze_docs};
use crate::phase::freshness::{FreshnessState, compute_freshness};
use crate::phase::options::PhaseOptions;
use crate::phase::pdg_utils::merge_pdgs;
use crate::phase::utils::{collect_files, hash_inventory};
use crate::storage::{
    pdg_store::{
        delete_files_data_tx, get_indexed_files, load_pdg, pdg_exists, save_pdg,
        update_indexed_file, update_indexed_files_tx,
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
            pdg.precision_symbols.is_empty()
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
        let has_persisted = pdg_exists(&self.storage, &self.project_id).unwrap_or(false);

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
        save_pdg(&mut self.storage, &self.project_id, &self.pdg)
            .context("failed saving full PDG for phase analysis")?;
        relink_for_analysis(&mut self.pdg);
        self.compute_and_persist_communities()
            .context("failed persisting communities for phase analysis")?;

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
        let mut pdg = load_pdg(&self.storage, &self.project_id)
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
        }

        let graph_changed =
            !freshness.deleted_files.is_empty() || !self.signatures_by_file.is_empty();
        self.persist_refreshed_graph(&mut pdg, freshness, graph_changed)?;
        Ok(())
    }

    /// Hydrate persisted community memberships into a loaded PDG. Failures
    /// are non-fatal: analysis proceeds without community data.
    fn hydrate_community_memberships(&self, pdg: &mut ProgramDependenceGraph) {
        #[cfg(feature = "community")]
        if let Err(error) = crate::storage::community_store::load_community_memberships(
            &self.storage,
            &self.project_id,
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
        if graph_changed {
            self.run_precision_ingest_for(pdg);
            save_pdg(&mut self.storage, &self.project_id, pdg)
                .context("failed saving refreshed PDG")?;
        } else if Self::should_run_precision_ingest(pdg, freshness) {
            // A persisted Tier-0 graph can predate precision ingest (or have
            // no matched markers yet). Allow that opt-in pass to run even when
            // freshness reports no source delta, then persist its markers.
            self.run_precision_ingest_for(pdg);
            save_pdg(&mut self.storage, &self.project_id, pdg)
                .context("failed saving precision-enriched PDG")?;
        }

        relink_for_analysis(pdg);
        self.pdg = std::mem::take(pdg);
        if graph_changed {
            self.compute_and_persist_communities()?;
        }
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
        assert!(pdg_exists(&context.storage, &project_id).expect("persisted graph"));

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
        save_pdg(&mut storage, &project_id, &persisted).expect("save Tier-0 graph");
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
        let loaded = load_pdg(&context.storage, &project_id).expect("reload persisted graph");
        assert!(loaded.is_precision_symbol("src/main.py:main"));
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
}
