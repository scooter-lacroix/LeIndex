//! Indexing-pipeline free helpers: phase-injection guards, progress output,
//! checkpoint reuse, and scan/parse/PDG-stat computation. Extracted from the
//! main pipeline module to keep it under the line-count gate.

use super::IndexPipelineState;
use crate::cli::index_builder;
use crate::cli::index_job::{
    CheckpointStore, FileFingerprint, LexicalCheckpoint, ParseCheckpoint, PdgCheckpoint,
    ScanCheckpoint,
};
use crate::cli::memory_cap::MemoryCapGuard;
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use tracing::warn;
pub(super) fn injected_phase_failure(phase: &str) -> Result<()> {
    if std::env::var("LEINDEX_INJECT_FAILURE_PHASE")
        .ok()
        .is_some_and(|value| value.eq_ignore_ascii_case(phase))
    {
        bail!("injected indexing failure after reusable {phase} artifact")
    }
    Ok(())
}

pub(super) fn add_submodule_summary_nodes(
    pdg: &mut crate::graph::pdg::ProgramDependenceGraph,
    project_path: &std::path::Path,
) {
    let Ok(summaries) = crate::cli::git::submodule_summaries(project_path) else {
        return;
    };
    for summary in summaries {
        let relative = summary
            .path
            .strip_prefix(project_path)
            .unwrap_or(&summary.path)
            .to_string_lossy()
            .replace('\\', "/");
        let node_id = format!("submodule:{relative}:{}", summary.commit_oid);
        if pdg
            .node_indices()
            .filter_map(|index| pdg.get_node(index))
            .any(|node| node.id == node_id)
        {
            continue;
        }
        let name = summary
            .path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&relative)
            .to_string();
        let module_index = pdg.add_node(crate::graph::pdg::Node {
            id: node_id,
            node_type: crate::graph::pdg::NodeType::Module,
            name,
            file_path: std::sync::Arc::from(summary.path.to_string_lossy().to_string()),
            byte_range: (0, 0),
            complexity: 0,
            language: format!("git-submodule:{}", summary.commit_oid),
        });
        let importers = pdg
            .node_indices()
            .filter(|index| *index != module_index)
            .filter_map(|index| pdg.get_node(index).map(|node| (index, node)))
            .filter(|(_, node)| {
                node.node_type == crate::graph::pdg::NodeType::External
                    && (node.name.contains(&relative) || node.id.contains(&relative))
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for importer in importers {
            pdg.add_edge(
                importer,
                module_index,
                crate::graph::pdg::Edge {
                    edge_type: crate::graph::pdg::EdgeType::Import,
                    metadata: crate::graph::pdg::EdgeMetadata::empty(),
                },
            );
        }
    }
}

/// Write a progress line to stderr if stderr is a terminal.
/// Uses `\r` to overwrite the current line (no newline).
/// This is a no-op when stderr is not a terminal (e.g., MCP/stdio mode).
pub(super) fn progress_stderr(msg: &str) {
    use std::io::{IsTerminal, Write};
    let stderr = std::io::stderr();
    if stderr.is_terminal() {
        let mut handle = stderr.lock();
        // Clear the line first, then write the new content
        let _ = write!(handle, "\r\x1b[K{}", msg);
        let _ = handle.flush();
    }
}

/// Clear the progress line on stderr (when terminal).
pub(super) fn progress_clear() {
    use std::io::{IsTerminal, Write};
    let stderr = std::io::stderr();
    if stderr.is_terminal() {
        let mut handle = stderr.lock();
        let _ = write!(handle, "\r\x1b[K");
        let _ = handle.flush();
    }
}

pub(super) fn check_memory_cap(cap_guard: &mut Option<&mut MemoryCapGuard>) -> Result<()> {
    if let Some(guard) = cap_guard.as_mut() {
        guard.check_now()?;
    }
    Ok(())
}

pub(super) fn load_resumed_pdg(
    store: &CheckpointStore,
    scan: &ScanCheckpoint,
    resumed_scan: bool,
    artifact_hash: Option<String>,
) -> Option<(PdgCheckpoint, crate::graph::pdg::ProgramDependenceGraph)> {
    if !resumed_scan {
        return None;
    }
    let hash = artifact_hash?;
    let metadata = store.read_pdg_checkpoint().ok().flatten();
    let checkpoint = match metadata {
        Some(checkpoint)
            if checkpoint.scan_hash == scan.input_hash && checkpoint.artifact_hash == hash =>
        {
            checkpoint
        }
        Some(_) => return None,
        None => PdgCheckpoint {
            scan_hash: scan.input_hash.clone(),
            artifact_path: store.paths.pdg(),
            artifact_hash: hash,
            nodes: 0,
            edges: 0,
        },
    };
    let pdg = store
        .read_pdg_artifact(&checkpoint.artifact_hash)
        .ok()
        .flatten()?;
    Some((
        PdgCheckpoint {
            nodes: if checkpoint.nodes == 0 {
                pdg.node_count()
            } else {
                checkpoint.nodes
            },
            edges: if checkpoint.edges == 0 {
                pdg.edge_count()
            } else {
                checkpoint.edges
            },
            ..checkpoint
        },
        pdg,
    ))
}

pub(super) fn valid_lexical_checkpoint(checkpoint: &LexicalCheckpoint) -> bool {
    checkpoint.snapshot_path.is_file()
        && checkpoint.tfidf_path.is_file()
        && checkpoint
            .snapshot_path
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0)
        && checkpoint
            .tfidf_path
            .metadata()
            .is_ok_and(|metadata| metadata.len() > 0)
}

pub(super) fn scan_checkpoint(source_files_with_hashes: &[(PathBuf, String)]) -> ScanCheckpoint {
    let files = source_files_with_hashes
        .iter()
        .map(|(path, hash)| FileFingerprint {
            canonical_path: path.clone(),
            blake3: hash.clone(),
            bytes: std::fs::metadata(path)
                .map(|metadata| metadata.len())
                .unwrap_or_default(),
            language: path
                .extension()
                .and_then(|extension| extension.to_str())
                .unwrap_or("unknown")
                .to_string(),
        })
        .collect::<Vec<_>>();
    ScanCheckpoint {
        input_hash: crate::cli::index_job::scan_hash(&files),
        files,
    }
}

pub(super) struct ParsePlan {
    pub(super) source_file_hashes: HashMap<String, String>,
    pub(super) current_file_paths: HashSet<String>,
    pub(super) files_to_parse: Vec<PathBuf>,
    pub(super) unchanged_files: HashSet<String>,
    pub(super) deleted_files: Vec<String>,
}

pub(super) fn parse_plan(state: &IndexPipelineState) -> ParsePlan {
    let source_file_hashes = state
        .source_files_with_hashes
        .iter()
        .map(|(path, hash)| (path.display().to_string(), hash.clone()))
        .collect::<HashMap<_, _>>();
    let current_file_paths = state
        .source_files_with_hashes
        .iter()
        .map(|(path, _)| path.display().to_string())
        .collect::<HashSet<_>>();
    let mut files_to_parse = Vec::new();
    let mut unchanged_files = HashSet::new();
    for (path, hash) in &state.source_files_with_hashes {
        let path_str = path.display().to_string();
        if state.force
            || !state.indexed_files.contains_key(&path_str)
            || state.indexed_files.get(&path_str) != Some(hash)
        {
            files_to_parse.push(path.clone());
        } else {
            unchanged_files.insert(path_str);
        }
    }
    let deleted_files = state
        .indexed_files
        .keys()
        .filter(|path| !current_file_paths.contains(*path))
        .cloned()
        .collect();
    ParsePlan {
        source_file_hashes,
        current_file_paths,
        files_to_parse,
        unchanged_files,
        deleted_files,
    }
}

pub(super) fn reuse_parse_results(
    resumed_scan: bool,
    resumed_parse: Option<&ParseCheckpoint>,
    cache: Option<&mut index_builder::FileReadCache>,
    store: &CheckpointStore,
    source_file_hashes: &HashMap<String, String>,
    files_to_parse: &mut Vec<PathBuf>,
) -> Result<Vec<crate::parse::parallel::ParsingResult>> {
    if !resumed_scan {
        return Ok(Vec::new());
    }
    let cache = cache.context("parse phase missing shared file cache")?;
    let Some(parse_checkpoint) = resumed_parse else {
        return Ok(Vec::new());
    };
    let mut results = Vec::new();
    let mut reused_paths = HashSet::new();
    for path in files_to_parse.iter() {
        let path_key = path.display().to_string();
        let Some(source_hash) = source_file_hashes.get(&path_key) else {
            continue;
        };
        let Some(expected_hash) = parse_checkpoint.artifact_hashes.get(source_hash) else {
            continue;
        };
        let Some(parsed) = store.read_parsed_for_path_verified(source_hash, expected_hash, path)?
        else {
            continue;
        };
        let source_bytes = cache.get_or_read(path)?.as_ref().clone();
        results.push(crate::parse::parallel::ParsingResult {
            file_path: parsed.file_path,
            language: Some(parsed.language),
            signatures: parsed.signatures,
            source_bytes: Some(source_bytes),
            error: None,
            parse_time_ms: parsed.parse_time_ms,
        });
        reused_paths.insert(path.clone());
    }
    files_to_parse.retain(|path| !reused_paths.contains(path));
    Ok(results)
}

pub(super) struct PdgParseStats {
    pub(super) files_parsed: usize,
    pub(super) successful: usize,
    pub(super) failed: usize,
    pub(super) total_sigs: usize,
    pub(super) all_signatures: Vec<(String, crate::parse::prelude::SignatureInfo)>,
}

pub(super) fn pdg_parse_stats(results: &[crate::parse::parallel::ParsingResult]) -> PdgParseStats {
    let successful = results.iter().filter(|result| result.is_success()).count();
    let failed = results.iter().filter(|result| result.is_failure()).count();
    let total_sigs = results.iter().map(|result| result.signatures.len()).sum();
    for result in results.iter().filter(|result| result.is_failure()) {
        warn!(
            "Parse failure for '{}' during indexing: {}",
            result.file_path.display(),
            result
                .error
                .as_deref()
                .filter(|error| !error.is_empty())
                .unwrap_or("unknown error")
        );
    }
    if failed > 0 {
        warn!(
            "Indexing completed with {} parse failure(s) out of {} file(s)",
            failed,
            successful + failed
        );
    }
    let all_signatures = results
        .iter()
        .filter(|result| result.is_success())
        .flat_map(|result| {
            let file_path = result.file_path.display().to_string();
            result
                .signatures
                .iter()
                .cloned()
                .map(move |signature| (file_path.clone(), signature))
        })
        .collect();
    PdgParseStats {
        files_parsed: results.len(),
        successful,
        failed,
        total_sigs,
        all_signatures,
    }
}

pub(super) fn git_tree_oid(project_path: &std::path::Path) -> Option<String> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD^{tree}"])
        .current_dir(project_path)
        .output()
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}
