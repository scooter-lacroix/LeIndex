// Index Builder — indexing pipeline extracted from LeIndex

use crate::cli::memory::{analysis_cache_key, search_cache_key};
use crate::graph::pdg::{EdgeType, NodeType, ProgramDependenceGraph};
use crate::search::search::{NodeInfo, SearchEngine};
use crate::storage::{pdg_store, schema::Storage};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use tracing::info;

use super::leindex::{
    IndexStats, ProjectFileScan, FileStats,
    SOURCE_FILE_EXTENSIONS, DEPENDENCY_MANIFEST_NAMES, SKIP_DIRS,
};

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
                current = format!("{}{}", last_char, ch);
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
    if current.len() >= 2 {
        tokens.push(current.to_lowercase());
    } else if !current.is_empty() && current.chars().all(|c| c.is_ascii_digit()) {
        tokens.push(current.to_lowercase());
    }
    tokens
}

/// TF-IDF based embedding system for code content.
///
/// Produces 768-dimensional vectors by computing TF-IDF scores for the
/// top-768 tokens by IDF value, then L2-normalizing the result.
///
/// This provides meaningful cosine similarity (> 0 for related code) unlike
/// the previous hash-based approach which produced random vectors.
pub(crate) struct TfIdfEmbedder {
    /// Ordered vocabulary (top-K tokens by IDF, K ≤ 768)
    vocab: Vec<String>,
    /// IDF values indexed by vocab position
    idf: Vec<f32>,
    /// Embedding dimension (matches existing vector index: 768)
    dimension: usize,
}

impl TfIdfEmbedder {
    /// Build a TF-IDF embedder from a corpus of (id, content) documents.
    ///
    /// # Steps
    /// 1. Tokenize every document
    /// 2. Build document-frequency table (df[token] = # docs containing token)
    /// 3. Compute IDF = ln(N / df) per token
    /// 4. Select top-768 tokens by IDF as vocabulary
    pub(crate) fn build(documents: &[(String, String)]) -> Self {
        const TARGET_DIM: usize = 768;
        let n = documents.len();

        if n == 0 {
            return Self {
                vocab: Vec::new(),
                idf: Vec::new(),
                dimension: TARGET_DIM,
            };
        }

        // Count document frequency per token
        let mut df: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for (_, content) in documents {
            let toks: std::collections::HashSet<String> =
                tokenize_code(content).into_iter().collect();
            for tok in toks {
                *df.entry(tok).or_insert(0) += 1;
            }
        }

        // Compute IDF for each token using a moderate-frequency filter.
        let n_f = n as f32;
        let min_df: usize = (n / 1000).max(3); // at least 3 occurrences
        let max_df: usize = (n / 4).max(min_df + 1);

        let mut idf_scores: Vec<(String, f32)> = df
            .into_iter()
            .filter(|(_, df_count)| *df_count >= min_df && *df_count <= max_df)
            .map(|(tok, df_count)| {
                let idf = (n_f / df_count as f32).ln();
                (tok, idf)
            })
            .collect();

        info!(
            vocab_candidates = idf_scores.len(),
            min_df,
            max_df,
            n_docs = n,
            "TF-IDF vocabulary candidates (moderate-IDF filter)"
        );

        // Stratified vocabulary selection across the full IDF range.
        idf_scores.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

        let final_scores: Vec<(String, f32)> = if idf_scores.len() <= TARGET_DIM {
            idf_scores
        } else {
            let total = idf_scores.len();
            let stride = total as f64 / TARGET_DIM as f64;
            (0..TARGET_DIM)
                .map(|i| {
                    let idx = ((i as f64 * stride) as usize).min(total - 1);
                    idf_scores[idx].clone()
                })
                .collect()
        };

        let idf_scores = final_scores;

        let vocab: Vec<String> = idf_scores.iter().map(|(t, _)| t.clone()).collect();
        let idf: Vec<f32> = idf_scores.iter().map(|(_, s)| *s).collect();

        Self {
            vocab,
            idf,
            dimension: TARGET_DIM,
        }
    }

    /// Embed a text string to a 768-dimensional L2-normalized TF-IDF vector.
    pub(crate) fn embed(&self, text: &str) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dimension];

        if self.vocab.is_empty() {
            return vec;
        }

        // Compute term frequencies
        let tokens = tokenize_code(text);
        let total = tokens.len() as f32;
        if total == 0.0 {
            return vec;
        }

        let mut tf_map: std::collections::HashMap<&str, f32> = std::collections::HashMap::new();
        for tok in &tokens {
            *tf_map.entry(tok.as_str()).or_insert(0.0) += 1.0;
        }

        // Compute TF-IDF for each vocab position
        for (i, (word, idf_val)) in self.vocab.iter().zip(self.idf.iter()).enumerate() {
            if let Some(&count) = tf_map.get(word.as_str()) {
                vec[i] = (count / total) * idf_val;
            }
        }

        // L2 normalize
        let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if magnitude > 1e-9 {
            for v in &mut vec {
                *v /= magnitude;
            }
        }

        vec
    }
}

// ============================================================================
// FILE SCANNING & HASHING
// ============================================================================

/// Hash a file using BLAKE3.
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("Failed to read file for hashing: {}", path.display()))?;
    Ok(blake3::hash(&bytes).to_hex().to_string())
}

/// Check if a filename is a dependency manifest/lockfile.
pub(crate) fn is_dependency_manifest_name(name: &str) -> bool {
    DEPENDENCY_MANIFEST_NAMES.contains(&name)
}

/// Scan the project directory for source and manifest files.
pub(crate) fn scan_project_files(project_path: &Path) -> Result<ProjectFileScan> {
    let project_config =
        crate::cli::config::ProjectConfig::load(project_path).unwrap_or_default();
    let mut source_paths = Vec::new();
    let mut manifest_paths = Vec::new();
    let mut walker = walkdir::WalkDir::new(project_path).into_iter();

    while let Some(entry) = walker.next() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy();

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

        if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
            if is_dependency_manifest_name(name) {
                let is_lockfile = name.contains("lock") || name.contains(".sum") || name == "npm-shrinkwrap.json";
                if is_lockfile || !project_config.should_exclude(path) {
                    manifest_paths.push(path.to_path_buf());
                }
                continue;
            }
        }

        if project_config.should_exclude(path) {
            continue;
        }

        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext_lower = ext.to_ascii_lowercase();
            if SOURCE_FILE_EXTENSIONS.contains(&ext_lower.as_str()) {
                source_paths.push(path.to_path_buf());
            }
        }
    }

    let source_directories = crate::cli::index_freshness::extract_unique_dirs(&source_paths);

    let mut manifest_hashes = std::collections::HashMap::new();
    for mp in &manifest_paths {
        if let Ok(bytes) = std::fs::read(mp) {
            let hash = blake3::hash(&bytes).to_hex().to_string();
            manifest_hashes.insert(mp.display().to_string(), hash);
        }
    }

    Ok(ProjectFileScan {
        source_paths,
        manifest_paths,
        source_directories,
        manifest_hashes,
    })
}

/// Collect source files with their content hashes.
pub(crate) fn collect_source_files_with_hashes(
    scan: &ProjectFileScan,
) -> Result<Vec<(PathBuf, String)>> {
    scan.source_paths
        .iter()
        .map(|path| Ok((path.clone(), hash_file(path)?)))
        .collect()
}

/// Merge a source PDG into a target PDG.
pub(crate) fn merge_pdgs(target: &mut ProgramDependenceGraph, source: ProgramDependenceGraph) {
    for node_idx in source.node_indices() {
        if let Some(node) = source.get_node(node_idx) {
            target.add_node(node.clone());
        }
    }

    for edge_idx in source.edge_indices() {
        if let Some(edge) = source.get_edge(edge_idx) {
            if let Some((s, t)) = source.edge_endpoints(edge_idx) {
                if let (Some(sn), Some(tn)) = (source.get_node(s), source.get_node(t)) {
                    if let (Some(si), Some(ti)) =
                        (target.find_by_symbol(&sn.id), target.find_by_symbol(&tn.id))
                    {
                        target.add_edge(si, ti, edge.clone());
                    }
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
        let is_external = node.language == "external"
            || node.language.starts_with("external:");
        if is_external && node.node_type != NodeType::External {
            node.node_type = NodeType::External;
            migrated += 1;
        }
    }
    if migrated > 0 {
        info!("Normalized {} external nodes to NodeType::External", migrated);
    }
}

/// Save PDG to storage.
pub(crate) fn save_to_storage(
    storage: &mut Storage,
    project_id: &str,
    pdg: &ProgramDependenceGraph,
) -> Result<()> {
    pdg_store::save_pdg(storage, project_id, pdg)
        .context("Failed to save PDG to storage")?;
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
) -> Result<TfIdfEmbedder> {
    // Invalidate file stats cache on reindex
    *file_stats_cache = None;

    let mut file_cache: std::collections::HashMap<String, std::sync::Arc<String>> =
        std::collections::HashMap::new();

    // --- Pass 1: collect all node content for TF-IDF corpus building ---
    let mut corpus: Vec<(String, String)> = Vec::new();
    let mut raw_nodes: Vec<(_, String)> = Vec::new();

    let connectivity_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(vec![EdgeType::Call, EdgeType::DataDependency]),
        excluded_node_types: Some(vec![NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };

    for node_idx in pdg.node_indices() {
        if let Some(node) = pdg.get_node(node_idx) {
            let content = file_cache
                .entry(node.file_path.clone())
                .or_insert_with(|| {
                    std::sync::Arc::new(
                        std::fs::read(&node.file_path)
                            .map(|bytes| String::from_utf8_lossy(&bytes).to_string())
                            .unwrap_or_default(),
                    )
                })
                .clone();

            let mut enrichment = format!(
                "// type:{} lang:{}",
                match node.node_type {
                    NodeType::Function => "function",
                    NodeType::Class => "class",
                    NodeType::Method => "method",
                    NodeType::Variable => "variable",
                    NodeType::Module => "module",
                    NodeType::External => "external",
                },
                node.language,
            );

            let callers = pdg.backward_impact(node_idx, &connectivity_config);
            let callees = pdg.forward_impact(node_idx, &connectivity_config);
            enrichment.push_str(&format!(
                " callers:{} callees:{} complexity:{}",
                callers.len().min(50),
                callees.len().min(50),
                node.complexity,
            ));

            let node_content = if !content.is_empty() && node.byte_range.1 > node.byte_range.0 {
                let content_bytes = content.as_bytes();
                let start = node.byte_range.0.min(content_bytes.len());
                let end = node.byte_range.1.min(content_bytes.len());

                if start < end {
                    let snippet = String::from_utf8_lossy(&content_bytes[start..end]);
                    format!("{}\n// {} in {}\n{}", enrichment, node.name, node.file_path, snippet)
                } else {
                    format!(
                        "{}\n// {} in {}\n{}",
                        enrichment, node.name, node.file_path, "// [No source code available]"
                    )
                }
            } else {
                format!(
                    "{}\n// {} in {}\n{}",
                    enrichment, node.name, node.file_path, "// [No source code available]"
                )
            };

            corpus.push((node.id.clone(), node_content.clone()));
            raw_nodes.push((node_idx, node_content));
        }
    }

    // --- Build TF-IDF embedder from the full corpus ---
    let embedder = TfIdfEmbedder::build(&corpus);

    // --- Pass 2: build NodeInfo vec using the embedder for embeddings ---
    let mut nodes: Vec<NodeInfo> = Vec::new();

    for (node_idx, node_content) in raw_nodes {
        if let Some(node) = pdg.get_node(node_idx) {
            let embedding = embedder.embed(&node_content);

            let node_info = NodeInfo {
                node_id: node.id.clone(),
                file_path: node.file_path.clone(),
                symbol_name: node.name.clone(),
                language: node.language.clone(),
                content: node_content,
                byte_range: node.byte_range,
                embedding: Some(embedding),
                complexity: node.complexity,
            };

            nodes.push(node_info);
        }
    }

    // Index the nodes
    search_engine.index_nodes(nodes);

    Ok(embedder)
}

/// Compare current manifest hashes against the persisted scan's hashes.
/// Returns the set of manifest file paths whose content has changed.
pub(crate) fn detect_changed_manifests(
    current_scan: &ProjectFileScan,
    project_id: &str,
    cache_spiller: &crate::cli::memory::CacheSpiller,
) -> Vec<PathBuf> {
    let cache_key = crate::cli::memory::project_scan_cache_key(project_id);
    let old_hashes: std::collections::HashMap<String, String> = cache_spiller
        .store()
        .peek(&cache_key)
        .and_then(|entry| match entry {
            crate::cli::memory::CacheEntry::Binary { serialized_data, .. } => {
                bincode::deserialize::<ProjectFileScan>(serialized_data).ok()
            }
            _ => None,
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

    let affected_set: HashSet<String> =
        affected.iter().map(|p| p.display().to_string()).collect();

    let source_set: HashSet<String> =
        all_source_paths.iter().map(|p| p.display().to_string()).collect();

    for nid in pdg.node_indices() {
        if let Some(node) = pdg.get_node(nid) {
            if node.node_type == NodeType::External {
                if !affected_set.contains(&node.file_path) {
                    if source_set.contains(&node.file_path) {
                        affected.push(PathBuf::from(&node.file_path));
                    }
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
    format!(
        "{}:{}:{}",
        stats.pdg_nodes, stats.pdg_edges, stats.indexed_nodes
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
) -> String {
    search_cache_key(&format!(
        "query:{}:{}:{}:{}:{:?}",
        stable_project_cache_id(project_id, project_path),
        index_fingerprint(stats),
        top_k,
        query.trim().to_lowercase(),
        query_type,
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
// TESTS (moved from leindex.rs)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tokenize_code_camel_case() {
        let toks = tokenize_code("getUserName");
        assert!(
            toks.contains(&"get".to_string()),
            "expected 'get', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"user".to_string()),
            "expected 'user', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"name".to_string()),
            "expected 'name', got {:?}",
            toks
        );
    }

    #[test]
    fn test_tokenize_code_acronyms_and_digits() {
        let toks = tokenize_code("HTTPConnection");
        assert!(
            toks.contains(&"http".to_string()),
            "expected 'http', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"connection".to_string()),
            "expected 'connection', got {:?}",
            toks
        );

        let toks2 = tokenize_code("HTTP2Connection");
        assert!(
            toks2.contains(&"http".to_string()),
            "expected 'http', got {:?}",
            toks2
        );
        assert!(
            toks2.contains(&"2".to_string()),
            "expected '2', got {:?}",
            toks2
        );
        assert!(
            toks2.contains(&"connection".to_string()),
            "expected 'connection', got {:?}",
            toks2
        );
    }

    #[test]
    fn test_tokenize_code_snake_case() {
        let toks = tokenize_code("get_user_name");
        assert!(
            toks.contains(&"get".to_string()),
            "expected 'get', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"user".to_string()),
            "expected 'user', got {:?}",
            toks
        );
        assert!(
            toks.contains(&"name".to_string()),
            "expected 'name', got {:?}",
            toks
        );
    }

    #[test]
    fn test_tokenize_code_filters_short_tokens() {
        let toks = tokenize_code("a b c xyz");
        assert!(!toks.contains(&"a".to_string()));
        assert!(!toks.contains(&"b".to_string()));
        assert!(!toks.contains(&"c".to_string()));
        assert!(toks.contains(&"xyz".to_string()));
    }

    #[test]
    fn test_tokenize_code_empty() {
        let toks = tokenize_code("");
        assert!(toks.is_empty());
    }

    #[test]
    fn test_tfidf_embedder_empty_corpus() {
        let embedder = TfIdfEmbedder::build(&[]);
        let vec = embedder.embed("test query");
        assert_eq!(
            vec.len(),
            768,
            "must produce 768-dim vector even for empty corpus"
        );
        assert!(vec.iter().all(|&v| v == 0.0), "empty corpus → zero vector");
    }

    #[test]
    fn test_tfidf_embedding_dimension() {
        let docs: Vec<(String, String)> = (0..10)
            .map(|i| {
                (
                    format!("doc_{}", i),
                    format!(
                        "fn handle_request_{} {{ let result = process(); result }}",
                        i
                    ),
                )
            })
            .collect();
        let embedder = TfIdfEmbedder::build(&docs);
        let vec = embedder.embed("handle request process");
        assert_eq!(vec.len(), 768, "embedding dimension must be 768");
    }

    #[test]
    fn test_tfidf_embedding_normalized() {
        let docs: Vec<(String, String)> = vec![
            (
                "auth".to_string(),
                "fn authenticate_user(token: &str) -> bool { verify_token(token) }".to_string(),
            ),
            (
                "db".to_string(),
                "fn connect_database(url: &str) -> Connection { open_connection(url) }".to_string(),
            ),
            (
                "http".to_string(),
                "fn send_request(endpoint: &str) -> Response { http_get(endpoint) }".to_string(),
            ),
        ];
        let embedder = TfIdfEmbedder::build(&docs);
        let vec = embedder.embed("authenticate token verify");
        let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if magnitude > 1e-9 {
            assert!(
                (magnitude - 1.0).abs() < 1e-4,
                "embedding should be L2-normalized, got magnitude {}",
                magnitude
            );
        }
    }

    #[test]
    fn test_tfidf_related_content_higher_similarity() {
        let docs: Vec<(String, String)> = vec![
            (
                "a1".into(),
                "fn authenticate_user(token: &str) -> bool { verify_token(token) }".into(),
            ),
            (
                "a2".into(),
                "fn check_user_credentials(password: &str) -> bool { hash_check(password) }".into(),
            ),
            (
                "b1".into(),
                "fn connect_database(url: &str) -> Connection { open_connection(url) }".into(),
            ),
            (
                "b2".into(),
                "fn execute_sql_query(query: &str) -> Vec<Row> { db_execute(query) }".into(),
            ),
            (
                "c1".into(),
                "fn parse_json_payload(data: &str) -> Value { serde_parse(data) }".into(),
            ),
        ];
        let embedder = TfIdfEmbedder::build(&docs);

        let auth1 = embedder.embed("fn authenticate_user token verify");
        let auth2 = embedder.embed("fn check_user credentials password hash");
        let db1 = embedder.embed("fn connect database execute sql query");

        let cosine =
            |a: &[f32], b: &[f32]| -> f32 { a.iter().zip(b.iter()).map(|(x, y)| x * y).sum() };

        let sim_related = cosine(&auth1, &auth2);
        let sim_unrelated = cosine(&auth1, &db1);

        assert!(
            sim_related >= sim_unrelated - 0.1,
            "related similarity ({}) should not be much lower than unrelated similarity ({})",
            sim_related,
            sim_unrelated
        );
    }

    #[test]
    fn test_tfidf_zero_vector_for_unseen_terms() {
        let docs: Vec<(String, String)> =
            vec![("a".into(), "fn foo_bar() -> bool { true }".into())];
        let embedder = TfIdfEmbedder::build(&docs);
        let vec = embedder.embed("zzzzzz aaaaaaa bbbbbbb cccccccc");
        let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        assert!(magnitude < 1.1, "magnitude out of range: {}", magnitude);
    }
}
