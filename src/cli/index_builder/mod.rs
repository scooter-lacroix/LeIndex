// Index Builder — indexing pipeline extracted from LeIndex

use crate::cli::memory::{analysis_cache_key, search_cache_key};
use crate::graph::pdg::{EdgeType, NodeType, ProgramDependenceGraph};
use crate::search::search::{NodeInfo, SearchEngine};
use crate::storage::{pdg_store, schema::Storage};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read as _;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[cfg(feature = "onnx")]
use crate::search::onnx::{EmbedResult, EmbeddingClient};

#[cfg(feature = "onnx")]
use tokio::task;

#[cfg(feature = "remote-embeddings")]
use crate::search::onnx::remote::RemoteEmbeddingProvider;
#[cfg(feature = "remote-embeddings")]
use crate::search::onnx::{GenericRemoteProvider, RemoteEmbeddingConfig, RemoteEmbeddingError};

use super::leindex::{
    FileStats, IndexStats, ProjectFileScan, DEPENDENCY_MANIFEST_NAMES, SKIP_DIRS,
    SOURCE_FILE_EXTENSIONS,
};
mod hybrid;
mod tfidf;

pub use hybrid::*;
pub use tfidf::*;

#[cfg(test)]
#[allow(clippy::infallible_destructuring_match)]
#[path = "tests.rs"]
mod tests;

// ============================================================================
// TF-IDF EMBEDDING SYSTEM
// ============================================================================

/// Tokenize a code string into sub-tokens by splitting camelCase, snake_case,
/// acronym boundaries, digit boundaries, whitespace, and punctuation, then
/// lowercasing all tokens.
///
/// Examples:
/// - `"getUserName"` → `["get", "user", "name"]`
/// - `"get_user_name"` → `["get", "user", "name"]`
/// - `"HTTPConnection"` → `["http", "connection"]`
/// - `"HTTP2Connection"` → `["http", "2", "connection"]`
pub(crate) fn tokenize_code(text: &str) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let mut current = String::new();

    for ch in text.chars() {
        if ch.is_alphanumeric() {
            if ch.is_uppercase() && !current.is_empty() {
                let last = current.chars().last().unwrap();
                if last.is_lowercase() || last.is_ascii_digit() {
                    // camelCase or digit→upper boundary: "userName" → "user" | "Name"
                    if current.len() >= 2 {
                        tokens.push(current.to_lowercase());
                    } else if current.chars().all(|c| c.is_ascii_digit()) {
                        tokens.push(current.clone());
                    }
                    current = ch.to_string();
                } else {
                    current.push(ch);
                }
            } else if ch.is_lowercase()
                && !current.is_empty()
                && current.len() > 1
                && current
                    .chars()
                    .last()
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
                && current
                    .chars()
                    .rev()
                    .nth(1)
                    .map(|c| c.is_uppercase())
                    .unwrap_or(false)
            {
                // Acronym→camelCase: "HTTPC" + 'o' → push "HTTP", start "Co"
                let last_char = current.pop().unwrap();
                if current.len() >= 2 {
                    tokens.push(current.to_lowercase());
                }
                current.clear();
                current.push(last_char);
                current.push(ch);
            } else if ch.is_ascii_digit()
                && !current.is_empty()
                && current
                    .chars()
                    .last()
                    .map(|c| c.is_alphabetic())
                    .unwrap_or(false)
            {
                // letter→digit boundary: "HTTP" + '2' → push "http", start "2"
                if current.len() >= 2 {
                    tokens.push(current.to_lowercase());
                }
                current = ch.to_string();
            } else if ch.is_alphabetic()
                && !current.is_empty()
                && current
                    .chars()
                    .last()
                    .map(|c| c.is_ascii_digit())
                    .unwrap_or(false)
            {
                // digit→letter boundary: "2" + 'C' → push "2", start "C"
                tokens.push(current.to_lowercase());
                current = ch.to_string();
            } else {
                current.push(ch);
            }
        } else if ch == '_' || ch == '-' || ch.is_whitespace() || ch.is_ascii_punctuation() {
            if current.len() >= 2 {
                tokens.push(current.to_lowercase());
            } else if !current.is_empty() && current.chars().all(|c| c.is_ascii_digit()) {
                tokens.push(current.clone());
            }
            current = String::new();
        } else {
            current.push(ch);
        }
    }
    if current.len() >= 2 || !current.is_empty() && current.chars().all(|c| c.is_ascii_digit()) {
        tokens.push(current.to_lowercase());
    }
    tokens
}

/// Return a small, non-redundant slice of doc/comment lines immediately
/// preceding a symbol. This keeps semantic chunks useful for review language
/// without creating a second full-file embedding document.
fn preceding_doc_context(bytes: &[u8], start: usize) -> String {
    let prefix = String::from_utf8_lossy(&bytes[..start.min(bytes.len())]);
    let mut lines = Vec::new();
    for line in prefix.lines().rev() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            if lines.is_empty() {
                continue;
            }
            break;
        }
        if trimmed.starts_with("//") || trimmed.starts_with("#") || trimmed.starts_with("/*") {
            // Conceptual-recall fix: strip comment markers so the doc embeds as
            // prose (markers add noise tokens that dilute the semantic signal),
            // and capture the full doc (24 lines, was 8 — long doc comments were
            // truncated mid-concept, losing the statement of purpose).
            lines.push(strip_comment_syntax(line.trim()));
            if lines.len() == 24 {
                break;
            }
        } else {
            break;
        }
    }
    lines.reverse();
    lines.join("\n")
}

/// Strip a leading comment marker (`///`, `//!`, `//`, `/*`, `*/`, `*`, `#`)
/// from a line, returning the prose content. FileSummary doc text must embed as
/// descriptive prose, not commented code — comment markers add noise tokens that
/// dilute the semantic signal (empirically buried summary nodes ~10 ranks).
fn strip_comment_syntax(line: &str) -> String {
    let t = line.trim_start();
    for prefix in ["///", "//!", "//", "/*", "*/", "*", "#"] {
        if let Some(rest) = t.strip_prefix(prefix) {
            return rest.trim_start().to_string();
        }
    }
    t.to_string()
}

/// Forward-scan the leading file-level doc/comment block (`//!`, `///`, `//`,
/// `#`, `/*`) from the start of the file, with comment markers STRIPPED so the
/// text embeds as prose. Used to seed FileSummary node text with the file's
/// stated purpose (the one place a human wrote NL prose naming the concept).
fn leading_file_doc(bytes: &[u8]) -> String {
    let text = String::from_utf8_lossy(bytes);
    let mut lines = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            if lines.is_empty() {
                continue;
            }
            break;
        }
        if trimmed.starts_with("//") || trimmed.starts_with("#") || trimmed.starts_with("/*") {
            lines.push(strip_comment_syntax(line.trim()));
            if lines.len() == 16 {
                break;
            }
        } else {
            break;
        }
    }
    lines.join("\n")
}

/// Build the bounded semantic text used by both the mandatory lexical index
/// and the deferred neural enrichment pass. Keeping this in one place makes
/// the two result layers rank the same node content.
/// Whether a node should be excluded from the search index because it is an
/// external/dependency placeholder.
///
/// These lightweight target nodes are created for unresolved data-flow/import
/// references (file_path "<external>", name = an arbitrary word from the
/// source). Indexing them pollutes search results ("<external>:: which") via
/// common-word lexical matches that outrank real project symbols, so external
/// nodes are dropped at this shared choke point — keeping them out of the
/// TF-IDF vocab, inverted index, vector index, and trigram index. This is the
/// filter that excludes external third-party dependency nodes from the index.
fn is_external_node_excluded(node: &crate::graph::pdg::Node) -> bool {
    node.node_type == NodeType::External
}

pub(crate) fn enriched_node_content(
    pdg: &ProgramDependenceGraph,
    node_idx: petgraph::graph::NodeIndex,
    node: &crate::graph::pdg::Node,
    file_bytes: &[u8],
    connectivity_config: &crate::graph::pdg::TraversalConfig,
) -> String {
    let content = String::from_utf8_lossy(file_bytes);
    let mut enrichment = format!(
        "// type:{} lang:{}",
        match node.node_type {
            NodeType::Function => "function",
            NodeType::Class => "class",
            NodeType::Method => "method",
            NodeType::Variable => "variable",
            NodeType::Module => "module",
            NodeType::External => "external",
            NodeType::FileSummary => "file_summary",
        },
        node.language,
    );
    let callers = pdg.backward_impact(node_idx, connectivity_config);
    let callees = pdg.forward_impact(node_idx, connectivity_config);
    enrichment.push_str(&format!(
        " callers:{} callees:{} complexity:{}",
        callers.len().min(50),
        callees.len().min(50),
        node.complexity,
    ));

    // FileSummary nodes carry no source snippet (byte_range=(0,0)). Build their
    // text from the file's leading doc + the names of same-file items, so a
    // conceptual NL query can match the file by its *purpose* even when no
    // individual function does. (conceptual-recall fix)
    if matches!(node.node_type, NodeType::FileSummary) {
        let doc = leading_file_doc(file_bytes);
        let mut items: Vec<String> = Vec::new();
        for ni in pdg.node_indices() {
            if let Some(n) = pdg.get_node(ni) {
                if !matches!(n.node_type, NodeType::FileSummary)
                    && n.file_path == node.file_path
                    && n.name != node.name
                {
                    items.push(n.name.clone());
                    if items.len() >= 40 {
                        break;
                    }
                }
            }
        }
        let doc_block = if doc.is_empty() {
            String::new()
        } else {
            format!("// file_doc:\n{}\n", doc)
        };
        let items_block = if items.is_empty() {
            String::new()
        } else {
            format!("// items:\n{}\n", items.join("\n"))
        };
        return format!(
            "{}\n// {} in {}\n{}{}",
            enrichment, node.name, node.file_path, doc_block, items_block
        );
    }

    if !content.is_empty() && node.byte_range.1 > node.byte_range.0 {
        let content_bytes = content.as_bytes();
        let start = node.byte_range.0.min(content_bytes.len());
        let end = node.byte_range.1.min(content_bytes.len());
        if start < end {
            let snippet = String::from_utf8_lossy(&content_bytes[start..end]);
            let doc_context = preceding_doc_context(content_bytes, start);
            return format!(
                "{}\n// {} in {}\n{}{}",
                enrichment,
                node.name,
                node.file_path,
                if doc_context.is_empty() {
                    String::new()
                } else {
                    format!("// review_context:\n{}\n", doc_context)
                },
                snippet
            );
        }
    }

    format!(
        "{}\n// {} in {}\n{}",
        enrichment, node.name, node.file_path, "// [No source code available]"
    )
}

// ============================================================================
// FILE SCANNING & HASHING
// ============================================================================

/// Read a file once, returning both its BLAKE3 hash and contents.
///
/// Uses streaming I/O to compute the BLAKE3 hash during the read rather than
/// in a separate pass after buffering the entire file (VAL-IO-004).
pub(crate) fn read_file_once(path: &Path) -> Result<(String, std::sync::Arc<Vec<u8>>)> {
    let file = std::fs::File::open(path)
        .with_context(|| format!("Failed to open file: {}", path.display()))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = blake3::Hasher::new();
    let mut bytes = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("Failed to read file: {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        bytes.extend_from_slice(&buf[..n]);
    }
    let hash = hasher.finalize().to_hex().to_string();
    Ok((hash, std::sync::Arc::new(bytes)))
}

/// Hash a file using BLAKE3.
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    Ok(read_file_once(path)?.0)
}

#[derive(Debug)]
pub(crate) struct FileReadCache {
    capacity: usize,
    entries: HashMap<PathBuf, std::sync::Arc<Vec<u8>>>,
    order: VecDeque<PathBuf>,
}

impl FileReadCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    pub(crate) fn get_or_read(&mut self, path: &Path) -> Result<std::sync::Arc<Vec<u8>>> {
        if let Some(bytes) = self.entries.get(path).cloned() {
            self.touch(path);
            return Ok(bytes);
        }

        let (hash, bytes) = read_file_once(path)?;
        self.insert(path.to_path_buf(), bytes.clone());
        info!(file = %path.display(), hash = %hash, "Read file once for hash and content");
        Ok(bytes)
    }

    fn touch(&mut self, path: &Path) {
        if let Some(pos) = self.order.iter().position(|p| p == path) {
            self.order.remove(pos);
        }
        self.order.push_back(path.to_path_buf());
    }

    fn insert(&mut self, path: PathBuf, bytes: std::sync::Arc<Vec<u8>>) {
        if self.entries.contains_key(&path) {
            self.entries.insert(path.clone(), bytes);
            self.touch(&path);
            return;
        }
        if self.entries.len() >= self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.entries.remove(&old);
            }
        }
        self.order.push_back(path.clone());
        self.entries.insert(path, bytes);
    }
}

/// Check if a filename is a dependency manifest/lockfile.
pub(crate) fn is_dependency_manifest_name(name: &str) -> bool {
    DEPENDENCY_MANIFEST_NAMES.contains(&name)
}

/// Scan the project directory for source and manifest files.
///
/// Enforces configurable limits from `ProjectConfig::indexing`:
/// - `max_file_size`: individual files exceeding this are skipped with a warning.
/// - `max_files`: scanning stops once this many source files have been collected.
/// - `max_total_size`: scanning stops once the cumulative size of collected source
///   files exceeds this threshold.
///
/// Oversized files do not count toward the file count or total size limits.
pub(crate) fn scan_project_files(project_path: &Path) -> Result<ProjectFileScan> {
    if crate::cli::git::is_worktree(project_path) {
        if let Ok(scan) = scan_git_project_files(project_path) {
            return Ok(scan);
        }
        tracing::debug!(project = %project_path.display(), "Git inventory unavailable; using non-Git scanner");
    }
    scan_non_git_project_files(project_path)
}

fn scan_git_project_files(project_path: &Path) -> Result<ProjectFileScan> {
    let project_config = crate::cli::config::ProjectConfig::load(project_path).unwrap_or_default();
    let limits = &project_config.indexing;
    let inventory = crate::cli::git::source_inventory(project_path)
        .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    let mut source_paths = Vec::new();
    let mut manifest_paths = Vec::new();
    let mut total_source_size = 0u64;
    for path in inventory {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        if is_dependency_manifest_name(file_name) {
            manifest_paths.push(path);
            continue;
        }
        if project_config.should_exclude(&path) {
            continue;
        }
        let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
            continue;
        };
        if !SOURCE_FILE_EXTENSIONS
            .iter()
            .any(|known| known.eq_ignore_ascii_case(ext))
        {
            continue;
        }
        let size = std::fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        if limits.max_file_size > 0 && size > limits.max_file_size {
            continue;
        }
        if limits.max_files > 0 && source_paths.len() >= limits.max_files {
            break;
        }
        if limits.max_total_size > 0
            && total_source_size.saturating_add(size) > limits.max_total_size
        {
            break;
        }
        total_source_size = total_source_size.saturating_add(size);
        source_paths.push(path);
    }
    source_paths.sort();
    manifest_paths.sort();
    let source_directories = crate::cli::index_freshness::extract_unique_dirs(&source_paths);
    let manifest_hashes = manifest_paths
        .iter()
        .filter_map(|path| {
            std::fs::read(path).ok().map(|bytes| {
                (
                    path.display().to_string(),
                    blake3::hash(&bytes).to_hex().to_string(),
                )
            })
        })
        .collect();
    let manifest_paths_canonical = manifest_paths
        .iter()
        .map(|path| path.canonicalize().unwrap_or_else(|_| path.clone()))
        .collect();
    Ok(ProjectFileScan {
        source_paths,
        manifest_paths,
        manifest_paths_canonical,
        source_directories,
        manifest_hashes,
    })
}

fn scan_non_git_project_files(project_path: &Path) -> Result<ProjectFileScan> {
    let project_config = crate::cli::config::ProjectConfig::load(project_path).unwrap_or_default();
    let limits = &project_config.indexing;

    let mut source_paths = Vec::new();
    let mut manifest_paths = Vec::new();
    let mut total_source_size: u64 = 0;
    let mut oversized_count: usize = 0;
    let mut walker = walkdir::WalkDir::new(project_path).into_iter();

    while let Some(entry) = walker.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy();

        // Prune dotfiles/dotdirs and skip-listed directories (needs the walker
        // for `skip_current_dir`, so it stays in the loop).
        if path != project_path && file_name.starts_with('.') && file_name != "." {
            if entry.file_type().is_dir() {
                walker.skip_current_dir();
            }
            continue;
        }

        if entry.file_type().is_dir() {
            if SKIP_DIRS.contains(&file_name.as_ref()) {
                walker.skip_current_dir();
            }
            continue;
        }

        if !entry.file_type().is_file() {
            continue;
        }

        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if process_scan_file(
            path,
            name,
            &entry,
            &project_config,
            limits,
            &mut source_paths,
            &mut manifest_paths,
            &mut total_source_size,
            &mut oversized_count,
        ) == ScanFileOutcome::Stop
        {
            break;
        }
    }

    if oversized_count > 5 {
        tracing::warn!(
            total_oversized = oversized_count,
            "Additional oversized files skipped (showing first 5 warnings)"
        );
    }

    Ok(finalize_project_scan(
        source_paths,
        manifest_paths,
        project_path,
    ))
}

#[derive(PartialEq, Eq)]
enum ScanFileOutcome {
    Continue,
    Stop,
}

/// Size/count limit decision for a candidate source file.
enum ScanLimit {
    Keep,
    SkipOversized,
    Stop,
}

/// Classify a single filesystem entry as manifest, excluded, or an acceptable
/// source file (applying the size/count/total limits). Pushes manifest/source
/// paths into the accumulators and returns whether the scan should continue.
fn process_scan_file(
    path: &Path,
    name: &str,
    entry: &walkdir::DirEntry,
    config: &crate::cli::config::ProjectConfig,
    limits: &crate::cli::config::IndexingConfig,
    source_paths: &mut Vec<PathBuf>,
    manifest_paths: &mut Vec<PathBuf>,
    total_source_size: &mut u64,
    oversized_count: &mut usize,
) -> ScanFileOutcome {
    if is_dependency_manifest_name(name) {
        let is_lockfile =
            name.contains("lock") || name.contains(".sum") || name == "npm-shrinkwrap.json";
        if is_lockfile || !config.should_exclude(path) {
            manifest_paths.push(path.to_path_buf());
        }
        return ScanFileOutcome::Continue;
    }

    if config.should_exclude(path) {
        return ScanFileOutcome::Continue;
    }

    let Some(ext) = path.extension().and_then(|ext| ext.to_str()) else {
        return ScanFileOutcome::Continue;
    };
    let ext_lower = ext.to_ascii_lowercase();
    if !SOURCE_FILE_EXTENSIONS.contains(&ext_lower.as_str()) {
        return ScanFileOutcome::Continue;
    }

    let file_size = entry.metadata().map(|metadata| metadata.len()).unwrap_or(0);
    match check_file_size_limits(file_size, limits, source_paths.len(), *total_source_size) {
        ScanLimit::SkipOversized => {
            *oversized_count += 1;
            if *oversized_count <= 5 {
                tracing::warn!(
                    file = %path.display(),
                    size_bytes = file_size,
                    limit_bytes = limits.max_file_size,
                    "Skipping file exceeding max_file_size limit"
                );
            }
            ScanFileOutcome::Continue
        }
        ScanLimit::Stop => {
            tracing::warn!(
                count = source_paths.len(),
                limit = limits.max_files,
                "Reached max_files limit, stopping source file scan"
            );
            ScanFileOutcome::Stop
        }
        ScanLimit::Keep => {
            *total_source_size += file_size;
            source_paths.push(path.to_path_buf());
            ScanFileOutcome::Continue
        }
    }
}

/// Decide whether a candidate file passes the configured size/count/total
/// limits. `max_total_size` produces a `Stop` because adding the file would
/// breach the project-wide byte budget.
fn check_file_size_limits(
    file_size: u64,
    limits: &crate::cli::config::IndexingConfig,
    file_count: usize,
    total_source_size: u64,
) -> ScanLimit {
    if limits.max_file_size > 0 && file_size > limits.max_file_size {
        ScanLimit::SkipOversized
    } else if (limits.max_files > 0 && file_count >= limits.max_files)
        || (limits.max_total_size > 0 && total_source_size + file_size > limits.max_total_size)
    {
        ScanLimit::Stop
    } else {
        ScanLimit::Keep
    }
}

/// Build the final `ProjectFileScan` from collected source/manifest paths:
/// unique source directories, manifest content hashes, and pre-canonicalized
/// manifest paths. Shared by the git and non-git scan paths.
fn finalize_project_scan(
    source_paths: Vec<PathBuf>,
    manifest_paths: Vec<PathBuf>,
    project_path: &Path,
) -> ProjectFileScan {
    let source_directories = crate::cli::index_freshness::extract_unique_dirs(&source_paths);

    let mut manifest_hashes = std::collections::HashMap::new();
    for mp in &manifest_paths {
        if let Ok(bytes) = std::fs::read(mp) {
            let hash = blake3::hash(&bytes).to_hex().to_string();
            manifest_hashes.insert(mp.display().to_string(), hash);
        }
    }

    // Pre-canonicalize manifest paths so freshness checks skip per-file
    // canonicalize calls. Relative paths are joined to project_path first so
    // they resolve against the project root, not CWD.
    let manifest_paths_canonical: Vec<PathBuf> = manifest_paths
        .iter()
        .map(|p| {
            let full = if p.is_relative() {
                project_path.join(p)
            } else {
                p.clone()
            };
            full.canonicalize().unwrap_or(full)
        })
        .collect();

    ProjectFileScan {
        source_paths,
        manifest_paths,
        manifest_paths_canonical,
        source_directories,
        manifest_hashes,
    }
}

/// Collect source files with their content hashes.
///
/// If a `FileReadCache` is provided, it will be populated with file contents
/// so that subsequent calls to `index_nodes` can reuse the same cache and
/// avoid reading files twice.
pub(crate) fn collect_source_files_with_hashes(
    scan: &ProjectFileScan,
    mut file_cache: Option<&mut FileReadCache>,
) -> Result<Vec<(PathBuf, String)>> {
    scan.source_paths
        .iter()
        .map(|path| {
            let hash = if let Some(cache) = file_cache.as_deref_mut() {
                // get_or_read already logs; extract just the hash
                let bytes = cache.get_or_read(path)?;
                // Compute hash from cached bytes (avoiding a second file read)
                blake3::hash(bytes.as_slice()).to_hex().to_string()
            } else {
                read_file_once(path)?.0
            };
            Ok((path.clone(), hash))
        })
        .collect()
}

/// Merge a source PDG into a target PDG.
///
/// Assumes source and target have disjoint node sets (e.g., merging a
/// per-file PDG into the global index). Does not deduplicate by symbol
/// name to preserve overloaded methods that share the same qualified name.
pub(crate) fn merge_pdgs(target: &mut ProgramDependenceGraph, source: ProgramDependenceGraph) {
    let mut id_map: std::collections::HashMap<
        petgraph::graph::NodeIndex,
        petgraph::graph::NodeIndex,
    > = std::collections::HashMap::with_capacity(source.node_count());

    for node_idx in source.node_indices() {
        if let Some(node) = source.get_node(node_idx) {
            let new_idx = target.add_node(node.clone());
            id_map.insert(node_idx, new_idx);
        }
    }

    for edge_idx in source.edge_indices() {
        if let Some(edge) = source.get_edge(edge_idx) {
            if let Some((s, t)) = source.edge_endpoints(edge_idx) {
                if let (Some(&si), Some(&ti)) = (id_map.get(&s), id_map.get(&t)) {
                    target.add_edge(si, ti, edge.clone());
                }
            }
        }
    }
}

/// Remove all nodes and edges for a file from the PDG.
pub(crate) fn remove_file_from_pdg(
    pdg: &mut ProgramDependenceGraph,
    file_path: &str,
) -> Result<()> {
    pdg.remove_file(file_path);
    Ok(())
}

/// Normalize external nodes: ensure any node with `language == "external"`
/// also has `NodeType::External`.
pub(crate) fn normalize_external_nodes(pdg: &mut ProgramDependenceGraph) {
    let mut migrated = 0usize;
    for node in pdg.node_weights_mut() {
        let is_external = node.language == "external" || node.language.starts_with("external:");
        if is_external && node.node_type != NodeType::External {
            node.node_type = NodeType::External;
            migrated += 1;
        }
    }
    if migrated > 0 {
        info!(
            "Normalized {} external nodes to NodeType::External",
            migrated
        );
    }
}

/// Save PDG to storage.
pub(crate) fn save_to_storage(
    storage: &mut Storage,
    project_id: &str,
    pdg: &ProgramDependenceGraph,
) -> Result<()> {
    pdg_store::save_pdg(storage, project_id, pdg).context("Failed to save PDG to storage")?;
    info!("Saved PDG to storage for project: {}", project_id);
    Ok(())
}

/// Index nodes from PDG for search.
///
/// Builds a TF-IDF embedder from the full corpus of node content, then uses
/// it to embed each node. Returns the embedder for query embedding at search time.
pub(crate) fn index_nodes(
    pdg: &ProgramDependenceGraph,
    search_engine: &mut SearchEngine,
    file_stats_cache: &mut Option<HashMap<String, FileStats>>,
    batch_size: usize,
) -> Result<HybridEmbedder> {
    index_nodes_with_embedder(pdg, search_engine, file_stats_cache, batch_size, None, None)
}

pub(crate) fn index_nodes_with_embedder(
    pdg: &ProgramDependenceGraph,
    search_engine: &mut SearchEngine,
    file_stats_cache: &mut Option<HashMap<String, FileStats>>,
    batch_size: usize,
    embedder: Option<HybridEmbedder>,
    shared_file_cache: Option<FileReadCache>,
) -> Result<HybridEmbedder> {
    index_nodes_with_embedder_inner(
        pdg,
        search_engine,
        file_stats_cache,
        batch_size,
        embedder,
        shared_file_cache,
        true,
    )
}

/// Index the mandatory TF-IDF/search layer without touching the neural
/// provider. The returned embedder is still suitable for query embedding;
/// neural rows are appended by the explicit enrichment phase.
pub(crate) fn index_nodes_tfidf_only(
    pdg: &ProgramDependenceGraph,
    search_engine: &mut SearchEngine,
    file_stats_cache: &mut Option<HashMap<String, FileStats>>,
    batch_size: usize,
    embedder: Option<HybridEmbedder>,
    shared_file_cache: Option<FileReadCache>,
) -> Result<HybridEmbedder> {
    index_nodes_with_embedder_inner(
        pdg,
        search_engine,
        file_stats_cache,
        batch_size,
        embedder,
        shared_file_cache,
        false,
    )
}

fn index_nodes_with_embedder_inner(
    pdg: &ProgramDependenceGraph,
    search_engine: &mut SearchEngine,
    file_stats_cache: &mut Option<HashMap<String, FileStats>>,
    batch_size: usize,
    embedder: Option<HybridEmbedder>,
    shared_file_cache: Option<FileReadCache>,
    _allow_neural: bool,
) -> Result<HybridEmbedder> {
    *file_stats_cache = None;

    let batch_size = batch_size.max(1);
    let mut file_cache = shared_file_cache.unwrap_or_else(|| FileReadCache::new(100));
    let connectivity_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[EdgeType::Call, EdgeType::DataDependency]),
        excluded_node_types: Some(vec![NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };

    let node_indices: Vec<petgraph::graph::NodeIndex> = pdg.node_indices().collect();
    let (df, total_docs) =
        build_document_frequencies(pdg, &node_indices, &mut file_cache, &connectivity_config);
    let embedder = build_embedder_from_corpus(embedder, total_docs, df, pdg, _allow_neural);

    // A+ bound-gated admission, selective pruning, and work hoisting.
    let pruner = crate::search::search::ContentPruner::new();
    let mut admission_gate = crate::search::search::IndexingAdmissionGate::new();
    let mut work_hoister = crate::search::search::WorkHoister::new();
    let mut pruned_count: usize = 0;
    let mut shed_count: usize = 0;
    let mut hoisted_count: usize = 0;
    let mut external_skipped_count: usize = 0;

    // Clear the search engine once before batch processing begins.
    // Each batch will append nodes incrementally via append_nodes().
    search_engine.clear_index();

    let mut nodes: Vec<NodeInfo> = Vec::with_capacity(batch_size);

    for batch in node_indices.chunks(batch_size) {
        nodes.clear();
        admission_gate.reset();
        // Collect index-into-nodes for nodes that need a neural embedding.
        let mut neural_pending: Vec<usize> = Vec::new();
        for &node_idx in batch {
            let Some(node) = pdg.get_node(node_idx) else {
                continue;
            };
            match build_indexed_node(
                pdg,
                node_idx,
                node,
                &mut file_cache,
                &connectivity_config,
                &pruner,
                &mut admission_gate,
                &mut work_hoister,
                &embedder,
                _allow_neural,
            ) {
                NodeBuildOutcome::SkippedExternal => external_skipped_count += 1,
                NodeBuildOutcome::Pruned => pruned_count += 1,
                NodeBuildOutcome::Shed => shed_count += 1,
                NodeBuildOutcome::Indexed {
                    node: built,
                    needs_batch_neural,
                    hoisted,
                } => {
                    if hoisted {
                        hoisted_count += 1;
                    }
                    if needs_batch_neural {
                        neural_pending.push(nodes.len());
                    }
                    nodes.push(built);
                }
            }
        }
        // Batch neural embedding: process pending nodes in sub-chunks to cap
        // IPC payload size and ONNX worker memory usage.
        if !neural_pending.is_empty() {
            embed_pending_neural_batch(&embedder, &mut nodes, &neural_pending, &mut work_hoister);
        }

        search_engine.append_nodes(std::mem::replace(
            &mut nodes,
            Vec::with_capacity(batch_size),
        ));
    }

    // A+ logging: per-batch stats at info! level (invisible under default WARN).
    if pruned_count > 0 || shed_count > 0 || hoisted_count > 0 || external_skipped_count > 0 {
        info!(
            pruned = pruned_count,
            shed = shed_count,
            hoisted = hoisted_count,
            external_skipped = external_skipped_count,
            admitted = admission_gate.nodes_admitted(),
            "A+ bound-gated indexing stats"
        );
    }

    // Single summary warn! so users see one warning if any work was shed/pruned.
    if pruned_count > 0 || shed_count > 0 {
        warn!(
            total_pruned = pruned_count,
            total_shed = shed_count,
            total_hoisted = hoisted_count,
            "indexing completed with pruning/shedding — some nodes were filtered"
        );
    }

    Ok(embedder)
}
/// Run deferred neural embedding for the pending batch indices, caching each
/// result in the work hoister and storing it on the node. Sub-chunked to cap
/// IPC payload and worker memory. No-op for an empty pending list.
#[allow(unused_variables)] // embedder/work_hoister used only under onnx/remote-embeddings
fn embed_pending_neural_batch(
    embedder: &HybridEmbedder,
    nodes: &mut [NodeInfo],
    neural_pending: &[usize],
    work_hoister: &mut crate::search::search::WorkHoister,
) {
    const NEURAL_IPC_BATCH: usize = 256;
    for ipc_chunk in neural_pending.chunks(NEURAL_IPC_BATCH) {
        #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
        {
            let texts: Vec<String> = ipc_chunk
                .iter()
                .map(|&idx| nodes[idx].content.clone())
                .collect();
            let batch_results = embedder.embed_neural_batch_blocking(&texts);
            for (i, &node_vec_idx) in ipc_chunk.iter().enumerate() {
                let neural = batch_results.get(i).and_then(|r| r.clone());
                work_hoister.store(
                    &nodes[node_vec_idx].content,
                    nodes[node_vec_idx].tfidf_embedding.clone(),
                    neural.clone(),
                );
                nodes[node_vec_idx].neural_embedding = neural;
            }
        }
        #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
        {
            for &node_vec_idx in ipc_chunk {
                nodes[node_vec_idx].neural_embedding = None;
            }
        }
    }
}
/// Outcome of processing one PDG node during the indexing Pass 2.
// ponytail: NodeInfo is large but this enum is built and consumed immediately
// per node; boxing would add indirection for no benefit.
#[allow(clippy::large_enum_variant)]
enum NodeBuildOutcome {
    Indexed {
        node: NodeInfo,
        needs_batch_neural: bool,
        hoisted: bool,
    },
    Pruned,
    Shed,
    SkippedExternal,
}

/// Build a single indexable `NodeInfo` from a PDG node, applying the external
/// skip, content pruning, and bound-gated admission filters plus work-hoister
/// embedding reuse and neural-embedding deferral. Returns the outcome so the
/// caller can accumulate per-batch counts and the neural-pending queue.
#[allow(clippy::too_many_arguments)]
fn build_indexed_node(
    pdg: &ProgramDependenceGraph,
    node_idx: petgraph::graph::NodeIndex,
    node: &crate::graph::pdg::Node,
    file_cache: &mut FileReadCache,
    connectivity_config: &crate::graph::pdg::TraversalConfig,
    pruner: &crate::search::search::ContentPruner,
    admission_gate: &mut crate::search::search::IndexingAdmissionGate,
    work_hoister: &mut crate::search::search::WorkHoister,
    embedder: &HybridEmbedder,
    allow_neural: bool,
) -> NodeBuildOutcome {
    if is_external_node_excluded(node) {
        return NodeBuildOutcome::SkippedExternal;
    }

    let file_bytes = file_cache
        .get_or_read(Path::new(&*node.file_path))
        .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));
    let node_content = enriched_node_content(pdg, node_idx, node, &file_bytes, connectivity_config);

    let pruning_decision = pruner.evaluate(&node.file_path, &node_content, &node.name);
    if pruning_decision != crate::search::search::PruningDecision::Keep {
        return NodeBuildOutcome::Pruned;
    }

    if !admission_gate.try_admit(node_content.len()) {
        return NodeBuildOutcome::Shed;
    }

    let tokens = tokenize_code(&node_content);

    // Repeated-work hoisting: reuse a cached embedding pair when available.
    let (tfidf_embedding, cached_neural, hoisted) = match work_hoister.lookup(&node_content) {
        Some((tfidf, neural)) => (tfidf, neural, true),
        None => (embedder.embed_tfidf(&tokens), None, false),
    };

    // Determine neural embedding: use the cache hit, defer to the batch call,
    // or store a TF-IDF-only pair when neural is unavailable/disabled.
    let (neural_embedding, needs_batch_neural) = if cached_neural.is_some() {
        (cached_neural, false)
    } else if embedder.has_neural() && allow_neural {
        (None, true)
    } else {
        work_hoister.store(&node_content, tfidf_embedding.clone(), None);
        (None, false)
    };

    let signature =
        crate::search::search::SearchEngine::extract_signature_from_content(&node_content);
    // R8: pre-tokenize for the search engine (different tokenizer than TF-IDF).
    let search_tokens: Vec<String> = node_content
        .split(|c: char| !c.is_alphanumeric())
        .map(|s| s.to_ascii_lowercase())
        .filter(|s| s.len() >= 2)
        .collect();

    NodeBuildOutcome::Indexed {
        node: NodeInfo {
            node_id: node.id.clone(),
            file_path: node.file_path.to_string(),
            symbol_name: node.name.clone(),
            language: node.language.clone(),
            content: node_content,
            byte_range: node.byte_range,
            tfidf_embedding,
            neural_embedding,
            complexity: node.complexity,
            signature,
            pre_tokenized: Some(search_tokens),
        },
        needs_batch_neural,
        hoisted,
    }
}
/// Pass 1: stream over every PDG node, tokenize its enriched content, and
/// accumulate per-token document frequencies. Returns `(df, total_docs)`.
fn build_document_frequencies(
    pdg: &ProgramDependenceGraph,
    node_indices: &[petgraph::graph::NodeIndex],
    file_cache: &mut FileReadCache,
    connectivity_config: &crate::graph::pdg::TraversalConfig,
) -> (std::collections::HashMap<String, usize>, usize) {
    let mut df: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut seen_tokens: HashSet<String> = HashSet::new();
    let mut total_docs = 0usize;
    for &node_idx in node_indices {
        let Some(node) = pdg.get_node(node_idx) else {
            continue;
        };
        if is_external_node_excluded(node) {
            continue;
        }
        let file_bytes = file_cache
            .get_or_read(Path::new(&*node.file_path))
            .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));
        let node_content =
            enriched_node_content(pdg, node_idx, node, &file_bytes, connectivity_config);
        let tokens = tokenize_code(&node_content);
        seen_tokens.clear();
        for tok in &tokens {
            if seen_tokens.insert(tok.clone()) {
                *df.entry(tok.clone()).or_insert(0) += 1;
            }
        }
        total_docs += 1;
    }
    (df, total_docs)
}

/// Resolve the indexing embedder: reuse a caller-provided one, build an empty
/// TF-IDF embedder for an empty corpus, or compute a stratified IDF vocabulary
/// from the document frequencies and upgrade to hybrid_local when allowed.
fn build_embedder_from_corpus(
    embedder: Option<HybridEmbedder>,
    total_docs: usize,
    df: std::collections::HashMap<String, usize>,
    pdg: &ProgramDependenceGraph,
    allow_neural: bool,
) -> HybridEmbedder {
    if let Some(embedder) = embedder {
        return embedder;
    }
    if total_docs == 0 {
        return HybridEmbedder::tfidf_only(TfIdfEmbedder::build_from_tokens(&[]));
    }

    let n_f = total_docs as f32;
    let min_df: usize = if total_docs < 50 {
        1
    } else {
        (total_docs / 1000).max(3)
    };
    let max_df: usize = if total_docs < 50 {
        total_docs
    } else {
        (total_docs / 4).max(min_df + 1)
    };
    let mut idf_scores: Vec<(String, f32)> = df
        .into_iter()
        .filter(|(_, df_count)| *df_count >= min_df && *df_count <= max_df)
        .map(|(tok, df_count)| (tok, (n_f / df_count as f32).ln()))
        .collect();

    info!(
        vocab_candidates = idf_scores.len(),
        min_df,
        max_df,
        n_docs = total_docs,
        "TF-IDF vocabulary candidates (two-pass streaming)"
    );

    idf_scores.sort_by(|a, b| {
        a.1.partial_cmp(&b.1)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.0.cmp(&b.0))
    });

    let final_scores = stratify_vocab(idf_scores);
    let tfidf_embedder = TfIdfEmbedder {
        vocab: final_scores.iter().map(|(t, _)| t.clone()).collect(),
        idf: final_scores.iter().map(|(_, s)| *s).collect(),
        dimension: crate::search::search::DEFAULT_EMBEDDING_DIMENSION,
        pdg_nodes: pdg.node_count(),
        pdg_edges: pdg.edge_count(),
    };

    build_neural_embedder(tfidf_embedder, allow_neural)
}

/// Stratify the IDF vocabulary down to the embedding dimension with an even
/// stride across the sorted score range (preserves coverage when there are
/// more candidates than dimensions).
fn stratify_vocab(idf_scores: Vec<(String, f32)>) -> Vec<(String, f32)> {
    if idf_scores.len() <= crate::search::search::DEFAULT_EMBEDDING_DIMENSION {
        return idf_scores;
    }
    let total = idf_scores.len();
    let target = crate::search::search::DEFAULT_EMBEDDING_DIMENSION;
    let stride = total as f64 / target as f64;
    (0..target)
        .map(|i| {
            let idx = ((i as f64 * stride) as usize).min(total - 1);
            idf_scores[idx].clone()
        })
        .collect()
}

/// Wrap a TF-IDF embedder as hybrid_local (neural query embedding) when the
/// ONNX feature is enabled and neural indexing is allowed, falling back to
/// tfidf_only on initialization failure or when neural is disabled.
#[allow(unused_variables)] // allow_neural used only under the onnx feature
fn build_neural_embedder(tfidf_embedder: TfIdfEmbedder, allow_neural: bool) -> HybridEmbedder {
    #[cfg(feature = "onnx")]
    {
        if allow_neural {
            match HybridEmbedder::hybrid_local(tfidf_embedder.clone(), None) {
                Ok(hybrid) => hybrid,
                Err(e) => {
                    warn!(
                        "Failed to create hybrid_local embedder (ONNX), falling back to tfidf_only: {}",
                        e
                    );
                    HybridEmbedder::tfidf_only(tfidf_embedder)
                }
            }
        } else {
            HybridEmbedder::tfidf_only(tfidf_embedder)
        }
    }
    #[cfg(not(feature = "onnx"))]
    {
        HybridEmbedder::tfidf_only(tfidf_embedder)
    }
}

/// Compute neural rows against an already-published TF-IDF index.
///
/// The mandatory TF-IDF/PDG generation is published first; this phase then
/// actively starts the configured neural worker and appends its rows. If the
/// provider reports a terminal failure, the core generation remains usable.
#[cfg_attr(
    not(any(feature = "onnx", feature = "remote-embeddings")),
    allow(unused_variables)
)]
pub(crate) fn enrich_neural_embeddings(
    pdg: &ProgramDependenceGraph,
    embedder: &HybridEmbedder,
    file_cache: &mut FileReadCache,
) -> Vec<(String, Vec<f32>)> {
    if !embedder.has_neural() {
        return Vec::new();
    }

    #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
    {
        Vec::new()
    }

    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    {
        const NEURAL_IPC_BATCH: usize = 256;
        let connectivity_config = crate::graph::pdg::TraversalConfig {
            max_depth: Some(1),
            max_nodes: Some(1000),
            allowed_edge_types: Some(&[EdgeType::Call, EdgeType::DataDependency]),
            excluded_node_types: Some(vec![NodeType::External]),
            min_complexity: None,
            min_edge_confidence: 0.0,
        };
        let mut pending = Vec::with_capacity(NEURAL_IPC_BATCH);
        let mut rows = Vec::new();
        for node_idx in pdg.node_indices() {
            let Some(node) = pdg.get_node(node_idx) else {
                continue;
            };
            // Match build_indexed_node's inclusion rule: external/excluded nodes
            // never enter the lexical index, so don't send them to the neural batch.
            if is_external_node_excluded(node) {
                continue;
            }
            let file_bytes = file_cache
                .get_or_read(Path::new(&*node.file_path))
                .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));
            let text =
                enriched_node_content(pdg, node_idx, node, &file_bytes, &connectivity_config);
            pending.push((node.id.clone(), text));
            if pending.len() == NEURAL_IPC_BATCH {
                append_neural_batch(embedder, &pending, &mut rows);
                pending.clear();
            }
        }
        if !pending.is_empty() {
            append_neural_batch(embedder, &pending, &mut rows);
        }
        rows
    }
}

#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn append_neural_batch(
    embedder: &HybridEmbedder,
    pending: &[(String, String)],
    rows: &mut Vec<(String, Vec<f32>)>,
) {
    let texts = pending
        .iter()
        .map(|(_, text)| text.clone())
        .collect::<Vec<_>>();
    let embeddings = embedder.embed_neural_batch_blocking(&texts);
    for ((node_id, _), embedding) in pending.iter().zip(embeddings) {
        if let Some(embedding) = embedding.filter(|row| !row.is_empty()) {
            rows.push((node_id.clone(), embedding));
        }
    }
}

/// Compare current manifest hashes against the persisted scan's hashes.
/// Returns the set of manifest file paths whose content has changed.
pub(crate) fn detect_changed_manifests(
    current_scan: &ProjectFileScan,
    project_id: &str,
    cache_spiller: &crate::cli::memory::CacheSpiller,
) -> Vec<PathBuf> {
    let cache_key = crate::cli::memory::project_scan_cache_key(project_id);

    // Try in-memory cache first; fall back to loading persisted scan from disk
    // to avoid false-positive manifest changes on cold start (when the in-memory
    // cache is empty but a previous scan was spilled/persisted).
    let old_hashes: std::collections::HashMap<String, String> = cache_spiller
        .store()
        .peek(&cache_key)
        .and_then(|entry| match entry {
            crate::cli::memory::CacheEntry::Binary {
                serialized_data, ..
            } => bincode::deserialize::<ProjectFileScan>(serialized_data).ok(),
            _ => None,
        })
        .or_else(|| {
            cache_spiller
                .store()
                .load_from_disk(&cache_key)
                .ok()
                .and_then(|entry| match entry {
                    crate::cli::memory::CacheEntry::Binary {
                        serialized_data, ..
                    } => bincode::deserialize::<ProjectFileScan>(&serialized_data).ok(),
                    _ => None,
                })
        })
        .map(|scan| scan.manifest_hashes)
        .unwrap_or_default();

    let mut changed = Vec::new();
    for mp in &current_scan.manifest_paths {
        let key = mp.display().to_string();
        let new_hash = current_scan.manifest_hashes.get(&key);
        let old_hash = old_hashes.get(&key);

        if new_hash != old_hash {
            let path_str = key.to_lowercase();
            let skip = path_str.contains("node_modules")
                || path_str.contains("/build/")
                || path_str.contains("\\build\\")
                || path_str.contains("/dist/")
                || path_str.contains("\\dist\\")
                || path_str.contains("/target/")
                || path_str.contains(".cache");
            if !skip {
                changed.push(mp.clone());
            }
        }
    }
    changed
}

/// Given a set of changed manifests, find which source files import
/// from packages defined in those manifests.
#[allow(dead_code)]
pub(crate) fn files_importing_from_manifests(
    changed_manifests: &[PathBuf],
    all_source_paths: &[PathBuf],
    pdg: &ProgramDependenceGraph,
) -> Vec<PathBuf> {
    if changed_manifests.is_empty() {
        return Vec::new();
    }

    let changed_dirs: HashSet<PathBuf> = changed_manifests
        .iter()
        .filter_map(|p| p.parent().map(|d| d.to_path_buf()))
        .collect();

    let mut affected: Vec<PathBuf> = Vec::new();
    for sp in all_source_paths {
        if let Some(parent) = sp.parent() {
            for dir in &changed_dirs {
                if sp.starts_with(dir) || parent.starts_with(dir) {
                    affected.push(sp.clone());
                    break;
                }
            }
        }
    }

    let affected_set: HashSet<String> = affected.iter().map(|p| p.display().to_string()).collect();

    let source_set: HashSet<String> = all_source_paths
        .iter()
        .map(|p| p.display().to_string())
        .collect();

    let mut affected_set = affected_set;
    for nid in pdg.node_indices() {
        if let Some(node) = pdg.get_node(nid) {
            if node.node_type == NodeType::External {
                let fp = node.file_path.as_ref();
                if !affected_set.contains(fp) && source_set.contains(fp) {
                    affected_set.insert(node.file_path.to_string());
                    affected.push(PathBuf::from(&*node.file_path));
                }
            }
        }
    }

    affected
}

// ============================================================================
// CACHE KEY HELPERS
// ============================================================================

pub(crate) fn index_fingerprint(stats: &IndexStats) -> String {
    // Include all numeric stats so that any content change invalidates
    // the search cache. Previously only pdg_nodes/pdg_edges/indexed_nodes
    // were used, which meant modifying a file (replacing one function with
    // another) produced the same fingerprint and stale cached search
    // results were returned (VAL-INDEX-005).
    format!(
        "{}:{}:{}:{}:{}:{}:{}",
        stats.total_files,
        stats.files_parsed,
        stats.total_signatures,
        stats.pdg_nodes,
        stats.pdg_edges,
        stats.indexed_nodes,
        stats.indexing_time_ms,
    )
}

pub(crate) fn stable_project_cache_id(project_id: &str, project_path: &Path) -> String {
    let path = project_path.to_string_lossy();
    let hash = blake3::hash(path.as_bytes()).to_hex();
    format!("{}:{}", project_id, &hash[..12])
}

pub(crate) fn search_cache_key_for(
    project_id: &str,
    project_path: &Path,
    stats: &IndexStats,
    query: &str,
    top_k: usize,
    query_type: Option<&crate::search::ranking::QueryType>,
    neural_available: bool,
    search_mode: &str,
    neural_weight: f64,
    rerank_enabled: bool,
    rerank_top_n: u32,
    embed_model: &str,
    rerank_model: &str,
) -> String {
    // v2: widens the key to include every result-affecting config knob + model
    // identity (search_mode, neural_weight, rerank_enabled/top_n, embedder +
    // reranker model). v1 keys omitted these, so a config or model change
    // silently served stale cached results until a re-index changed the node
    // count. The `v2:` namespace prefix lands in the (sanitized) cache filename
    // so legacy v1 entries are identifiably stale and sweepable, not silent.
    search_cache_key(&format!(
        "v2:query:{}:{}:{}:{}:{:?}:neural={}:mode={}:nw={}:rr={}|{}|{}:embed={}",
        stable_project_cache_id(project_id, project_path),
        index_fingerprint(stats),
        top_k,
        query.trim().to_lowercase(),
        query_type,
        neural_available,
        search_mode,
        neural_weight,
        rerank_enabled,
        rerank_top_n,
        rerank_model,
        embed_model,
    ))
}

pub(crate) fn analysis_cache_key_for(
    project_id: &str,
    project_path: &Path,
    stats: &IndexStats,
    query: &str,
    token_budget: usize,
) -> String {
    analysis_cache_key(&format!(
        "analyze:{}:{}:{}:{}",
        stable_project_cache_id(project_id, project_path),
        index_fingerprint(stats),
        token_budget,
        query.trim().to_lowercase()
    ))
}

// ============================================================================
// MMAP EMBEDDING PERSISTENCE (R10)
// ============================================================================

/// Persist all embeddings from the search engine to an mmap-backed binary file.
///
/// After indexing completes, call this to write a `.leindex/embeddings.bin`
/// file that can be memory-mapped for fast read-only access without loading
/// the full embedding matrix into heap memory.
pub(crate) fn persist_embeddings_to_mmap(
    search_engine: &SearchEngine,
    project_path: &Path,
) -> Result<()> {
    let path = crate::search::vector::mmap_embeddings_path(project_path);
    if search_engine.is_mmap_backed() && path.is_file() {
        return Ok(());
    }
    let embeddings = search_engine.collect_embeddings();
    if embeddings.is_empty() {
        return Ok(());
    }
    crate::search::vector::write_mmap_embeddings(&path, &embeddings)
        .map_err(|e| anyhow::anyhow!("Failed to write mmap embeddings: {e}"))?;
    info!(
        count = embeddings.len(),
        path = %path.display(),
        "Persisted embeddings to mmap file"
    );
    Ok(())
}

fn search_snapshot_path(project_path: &Path) -> PathBuf {
    project_path.join(".leindex").join("search_snapshot.bin")
}

/// Persist search metadata required for fast load_from_storage hydration.
pub(crate) fn persist_search_snapshot(
    search_engine: &SearchEngine,
    project_path: &Path,
    pdg_nodes: usize,
    pdg_edges: usize,
    pdg_fingerprint: String,
) -> Result<()> {
    let snapshot = search_engine.search_snapshot(pdg_nodes, pdg_edges, pdg_fingerprint);
    if snapshot.indexed_nodes == 0 {
        return Ok(());
    }

    let path = search_snapshot_path(project_path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| {
            format!(
                "Failed to create search snapshot directory: {}",
                parent.display()
            )
        })?;
    }

    let bytes = bincode::serialize(&snapshot).context("Failed to serialize search snapshot")?;
    std::fs::write(&path, bytes)
        .with_context(|| format!("Failed to write search snapshot: {}", path.display()))?;
    info!(
        count = snapshot.indexed_nodes,
        path = %path.display(),
        "Persisted search snapshot"
    );
    Ok(())
}

/// Try to load previously persisted search metadata.
#[allow(dead_code)]
pub(crate) fn try_load_search_snapshot(
    project_path: &Path,
) -> Option<crate::search::search::SearchSnapshot> {
    try_load_search_snapshot_from_storage(&project_path.join(".leindex"))
}

/// Try to load search metadata from an explicit storage directory.
pub(crate) fn try_load_search_snapshot_from_storage(
    storage_path: &Path,
) -> Option<crate::search::search::SearchSnapshot> {
    let path = storage_path.join("search_snapshot.bin");
    if !path.exists() {
        return None;
    }

    match std::fs::read(&path)
        .with_context(|| format!("Failed to read search snapshot: {}", path.display()))
        .and_then(|bytes| {
            bincode::deserialize::<crate::search::search::SearchSnapshot>(&bytes)
                .context("Failed to deserialize search snapshot")
        }) {
        Ok(snapshot) => {
            info!(
                count = snapshot.indexed_nodes,
                path = %path.display(),
                "Loaded search snapshot"
            );
            Some(snapshot)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to load search snapshot"
            );
            None
        }
    }
}

struct Blake3FormatWriter<'a>(&'a mut blake3::Hasher);

impl std::fmt::Write for Blake3FormatWriter<'_> {
    fn write_str(&mut self, value: &str) -> std::fmt::Result {
        self.0.update(value.as_bytes());
        Ok(())
    }
}

/// Stable fingerprint of the PDG state that materially affects search
/// hydration. This prevents a snapshot produced for one PDG from being reused
/// after storage changes that happen to preserve node/edge counts.
pub(crate) fn pdg_search_fingerprint(pdg: &ProgramDependenceGraph) -> String {
    use std::fmt::Write as _;

    let mut hasher = blake3::Hasher::new();
    hasher.update(b"leindex-pdg-search-v2");

    let mut nodes: Vec<[u8; 32]> = pdg
        .node_indices()
        .filter_map(|node_idx| {
            pdg.get_node(node_idx).map(|node| {
                let mut record = blake3::Hasher::new();
                write!(
                    Blake3FormatWriter(&mut record),
                    "{}\0{:?}\0{}\0{}\0{}\0{}\0{}\0{}",
                    node.id,
                    node.node_type,
                    node.name,
                    node.file_path,
                    node.byte_range.0,
                    node.byte_range.1,
                    node.complexity,
                    node.language
                )
                .expect("hash writer is infallible");
                *record.finalize().as_bytes()
            })
        })
        .collect();
    nodes.sort_unstable();
    for node in nodes {
        hasher.update(&node);
    }

    let mut edges: Vec<[u8; 32]> = pdg
        .edge_indices()
        .filter_map(|edge_idx| {
            let edge = pdg.get_edge(edge_idx)?;
            let (from, to) = pdg.edge_endpoints(edge_idx)?;
            let from = pdg.get_node(from)?;
            let to = pdg.get_node(to)?;
            let mut record = blake3::Hasher::new();
            write!(
                Blake3FormatWriter(&mut record),
                "{}\0{}\0{:?}\0{:?}\0{:?}\0{:?}",
                from.id,
                to.id,
                edge.edge_type,
                edge.metadata.call_count,
                edge.metadata.variable_name,
                edge.metadata.confidence.map(f32::to_bits)
            )
            .expect("hash writer is infallible");
            Some(*record.finalize().as_bytes())
        })
        .collect();
    edges.sort_unstable();
    for edge in edges {
        hasher.update(&edge);
    }

    hasher.finalize().to_hex().to_string()
}

/// Persist neural embeddings to a separate mmap file for fast load_from_storage.
///
/// This stores the ONNX neural embeddings (1024-dim) separately from the
/// TF-IDF embeddings (768-dim) so they can be restored without re-computing
/// ONNX inference for all nodes.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn persist_neural_embeddings_to_mmap(
    search_engine: &SearchEngine,
    project_path: &Path,
) -> Result<()> {
    let embeddings = search_engine.collect_neural_embeddings();
    let path = neural_mmap_embeddings_path(project_path);
    if embeddings.is_empty() {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                anyhow::anyhow!("Failed to remove stale neural mmap embeddings: {e}")
            })?;
            info!(
                path = %path.display(),
                "Removed stale neural embeddings mmap file"
            );
        }
        return Ok(());
    }
    crate::search::vector::write_mmap_embeddings(&path, &embeddings)
        .map_err(|e| anyhow::anyhow!("Failed to write neural mmap embeddings: {e}"))?;
    info!(
        count = embeddings.len(),
        path = %path.display(),
        "Persisted neural embeddings to mmap file"
    );
    Ok(())
}

/// Path for the neural embeddings mmap file.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
fn neural_mmap_embeddings_path(project_path: &Path) -> PathBuf {
    project_path.join(".leindex").join("neural_embeddings.bin")
}

/// Try to load previously persisted neural embeddings from mmap file.
///
/// Returns `None` if the file does not exist or is corrupt.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn try_load_neural_mmap_embeddings(
    project_path: &Path,
) -> Option<crate::search::vector::MmapEmbeddingIndex> {
    try_load_neural_mmap_embeddings_from_storage(&project_path.join(".leindex"))
}

/// Try to load neural embeddings from an explicit storage directory.
#[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
pub(crate) fn try_load_neural_mmap_embeddings_from_storage(
    storage_path: &Path,
) -> Option<crate::search::vector::MmapEmbeddingIndex> {
    let path = storage_path.join("neural_embeddings.bin");
    if !path.exists() {
        return None;
    }
    match crate::search::vector::MmapEmbeddingIndex::open(&path) {
        Ok(index) => {
            info!(
                nodes = index.len(),
                dim = index.dimension(),
                path = %path.display(),
                "Loaded neural mmap embedding index"
            );
            Some(index)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to load neural mmap embedding index"
            );
            None
        }
    }
}

/// Clear persisted search query and analysis cache entries for a project.
///
/// After indexing (full or incremental), previously cached search results
/// may be stale. This function removes all `search:query:` and `analysis:`
/// cache entries from both the in-memory store and the disk cache directory.
///
/// This is critical for VAL-INDEX-005: without cache invalidation, modifying
/// a source file and running `leindex.index` (force_reindex=false) would
/// produce a fresh PDG but stale search results would still be served from
/// the disk cache.
pub(crate) fn clear_query_caches(
    cache_spiller: &mut crate::cli::memory::CacheSpiller,
    project_id: &str,
) {
    let store = cache_spiller.store_mut();

    // Build the sanitized prefix for search query cache keys.
    // The key format is: search:query:{project_id}:{fingerprint}:{top_k}:{query:?}
    // After sanitization, colons become underscores.
    let sanitized_project = sanitize_for_prefix(project_id);
    let search_prefix = format!("search_query_{}_", sanitized_project);
    let analysis_prefix = format!("analysis_analyze_{}_", sanitized_project);

    // Remove from in-memory cache
    let mem_search = store.remove_prefix(&format!("search:query:{}:", project_id));
    let mem_analysis = store.remove_prefix(&format!("analysis:analyze:{}:", project_id));

    // Remove from disk
    let disk_search = store.remove_spilled_prefix(&search_prefix);
    let disk_analysis = store.remove_spilled_prefix(&analysis_prefix);

    if mem_search + mem_analysis + disk_search + disk_analysis > 0 {
        info!(
            "Cleared query caches: {} search (mem:{} disk:{}), {} analysis (mem:{} disk:{})",
            mem_search + disk_search,
            mem_search,
            disk_search,
            mem_analysis + disk_analysis,
            mem_analysis,
            disk_analysis,
        );
    }
}

/// Sanitize a project ID for use as a disk cache filename prefix.
/// The disk cache filenames are sanitized versions of the full cache key.
fn sanitize_for_prefix(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Try to load a previously persisted mmap embedding index.
///
/// Returns `None` if the file does not exist or is corrupt.
#[allow(dead_code)]
pub(crate) fn try_load_mmap_embeddings(
    project_path: &Path,
) -> Option<crate::search::vector::MmapEmbeddingIndex> {
    try_load_mmap_embeddings_from_storage(&project_path.join(".leindex"))
}

/// Try to load TF-IDF embeddings from an explicit storage directory.
pub(crate) fn try_load_mmap_embeddings_from_storage(
    storage_path: &Path,
) -> Option<crate::search::vector::MmapEmbeddingIndex> {
    let path = storage_path.join("embeddings.bin");
    if !path.exists() {
        return None;
    }
    match crate::search::vector::MmapEmbeddingIndex::open(&path) {
        Ok(index) => {
            info!(
                nodes = index.len(),
                dim = index.dimension(),
                path = %path.display(),
                "Loaded mmap embedding index"
            );
            Some(index)
        }
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to load mmap embedding index"
            );
            None
        }
    }
}

// ============================================================================
// TESTS (moved from leindex.rs)
// ============================================================================
