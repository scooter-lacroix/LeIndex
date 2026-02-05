use crate::docs::{analyze_docs, DocsSummary};
use crate::freshness::{compute_freshness, FreshnessState};
use crate::options::PhaseOptions;
use crate::pdg_utils::merge_pdgs;
use crate::utils::{collect_files, hash_inventory};
use anyhow::{bail, Context, Result};
use legraphe::{extract_pdg_from_signatures, pdg::ProgramDependenceGraph};
use leparse::{parallel::ParsingResult, prelude::ParallelParser, traits::SignatureInfo};
use lestockage::{
    pdg_store::{
        delete_file_data, get_indexed_files, load_pdg, pdg_exists, save_pdg, update_indexed_file,
    },
    schema::Storage,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

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
        };

        context.load_or_refresh_graph(options, &freshness)?;

        if options.include_docs {
            context.docs_summary = Some(analyze_docs(&collected.docs_files)?);
        }

        Ok(context)
    }

    fn load_or_refresh_graph(
        &mut self,
        options: &PhaseOptions,
        freshness: &FreshnessState,
    ) -> Result<()> {
        let has_persisted = pdg_exists(&self.storage, &self.project_id).unwrap_or(false);

        if options.use_incremental_refresh && has_persisted {
            let mut pdg = load_pdg(&self.storage, &self.project_id)
                .context("failed loading cached PDG for incremental phase run")?;

            for path in &freshness.deleted_files {
                for key in equivalent_file_keys(&self.root, path) {
                    pdg.remove_file(&key);
                    let _ = delete_file_data(&mut self.storage, &self.project_id, &key);
                }
            }

            let parse_paths = freshness.changed_files.clone();
            if !parse_paths.is_empty() {
                self.parse_results = ParallelParser::new().parse_files(parse_paths);
                self.signatures_by_file = signatures_from_results(&self.root, &self.parse_results);

                let inventory_hashes = freshness
                    .file_inventory
                    .iter()
                    .map(|(path, hash)| {
                        (
                            normalize_file_key(&self.root, &path.display().to_string()),
                            hash.clone(),
                        )
                    })
                    .collect::<HashMap<_, _>>();

                for (file_path, (language, signatures)) in &self.signatures_by_file {
                    // Parse succeeded: now safe to replace stale file graph/state.
                    for key in equivalent_file_keys(&self.root, file_path) {
                        pdg.remove_file(&key);
                        let _ = delete_file_data(&mut self.storage, &self.project_id, &key);
                    }

                    let source_bytes = source_bytes_for_file(&self.root, file_path);
                    let file_pdg = extract_pdg_from_signatures(
                        signatures.clone(),
                        &source_bytes,
                        file_path,
                        language,
                    );
                    merge_pdgs(&mut pdg, &file_pdg);

                    let normalized = normalize_file_key(&self.root, file_path);
                    if let Some(hash) = inventory_hashes.get(&normalized) {
                        let _ = update_indexed_file(
                            &mut self.storage,
                            &self.project_id,
                            &normalized,
                            hash,
                        );
                    }
                }
            }

            if !freshness.deleted_files.is_empty() || !self.signatures_by_file.is_empty() {
                crate::pdg_utils::relink_external_import_edges(&mut pdg);
                save_pdg(&mut self.storage, &self.project_id, &pdg)
                    .context("failed saving refreshed PDG")?;
            }

            self.pdg = pdg;
            return Ok(());
        }

        // Cold/full path
        let parse_targets = freshness
            .file_inventory
            .iter()
            .map(|(p, _)| p.clone())
            .collect::<Vec<_>>();
        self.parse_results = ParallelParser::new().parse_files(parse_targets);
        self.signatures_by_file = signatures_from_results(&self.root, &self.parse_results);

        let mut pdg = ProgramDependenceGraph::new();
        for (file_path, (language, signatures)) in &self.signatures_by_file {
            let source_bytes = source_bytes_for_file(&self.root, file_path);
            let file_pdg =
                extract_pdg_from_signatures(signatures.clone(), &source_bytes, file_path, language);
            merge_pdgs(&mut pdg, &file_pdg);
        }
        crate::pdg_utils::relink_external_import_edges(&mut pdg);
        self.pdg = pdg;

        save_pdg(&mut self.storage, &self.project_id, &self.pdg)
            .context("failed saving full PDG for phase analysis")?;

        let inventory_hashes = freshness
            .file_inventory
            .iter()
            .map(|(path, hash)| {
                (
                    normalize_file_key(&self.root, &path.display().to_string()),
                    hash.clone(),
                )
            })
            .collect::<HashMap<_, _>>();

        for file_path in self.signatures_by_file.keys() {
            let normalized = normalize_file_key(&self.root, file_path);
            if let Some(hash) = inventory_hashes.get(&normalized) {
                let _ = update_indexed_file(&mut self.storage, &self.project_id, &normalized, hash);
            }
        }

        Ok(())
    }
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
    use leparse::traits::{SignatureInfo, Visibility};

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
            }],
            error: None,
            parse_time_ms: 1,
        };

        let failure = ParsingResult {
            file_path: PathBuf::from("src/bad.rs"),
            language: None,
            signatures: Vec::new(),
            error: Some("Parse error: test".to_string()),
            parse_time_ms: 0,
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
            }],
            error: None,
            parse_time_ms: 1,
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
}
