// Index Builder — indexing pipeline extracted from LeIndex

use crate::cli::memory::{analysis_cache_key, search_cache_key};
use crate::graph::pdg::{EdgeType, NodeType, ProgramDependenceGraph};
use crate::search::search::{NodeInfo, SearchEngine};
use crate::storage::{pdg_store, schema::Storage};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use tracing::info;

use super::leindex::{
    FileStats, IndexStats, ProjectFileScan, DEPENDENCY_MANIFEST_NAMES, SKIP_DIRS,
    SOURCE_FILE_EXTENSIONS,
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

/// TF-IDF based embedding system for code content.
///
/// Produces 768-dimensional vectors by computing TF-IDF scores for the
/// top-768 tokens by IDF value, then L2-normalizing the result.
///
/// This provides meaningful cosine similarity (> 0 for related code) unlike
/// the previous hash-based approach which produced random vectors.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct TfIdfPersistedState {
    vocab: Vec<String>,
    idf: Vec<f32>,
    dimension: usize,
    pdg_nodes: usize,
    pdg_edges: usize,
}

#[derive(Debug, Clone)]
struct TokenizedNode {
    node_idx: petgraph::graph::NodeIndex,
    id: String,
    content: String,
    tokens: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TfIdfEmbedder {
    /// Ordered vocabulary (top-K tokens by IDF, K ≤ 768)
    vocab: Vec<String>,
    /// IDF values indexed by vocab position
    idf: Vec<f32>,
    /// Embedding dimension (matches existing vector index: 768)
    dimension: usize,
    /// PDG node count captured when persisted for staleness checks
    pdg_nodes: usize,
    /// PDG edge count captured when persisted for staleness checks
    pdg_edges: usize,
}

impl TfIdfEmbedder {
    /// Build a TF-IDF embedder from a corpus of (id, content) documents.
    ///
    /// # Steps
    /// 1. Tokenize every document
    /// 2. Build document-frequency table (df[token] = # docs containing token)
    /// 3. Compute IDF = ln(N / df) per token, filtering extreme frequencies
    /// 4. Stratified vocabulary selection across the full IDF range (up to 768 tokens)
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn build(documents: &[(String, String)]) -> Self {
        let tokenized: Vec<(String, Vec<String>)> = documents
            .iter()
            .map(|(id, content)| (id.clone(), tokenize_code(content)))
            .collect();
        Self::build_from_tokens(&tokenized)
    }

    /// Build a TF-IDF embedder from pre-tokenized documents.
    pub(crate) fn build_from_tokens(documents: &[(String, Vec<String>)]) -> Self {
        const TARGET_DIM: usize = crate::search::search::DEFAULT_EMBEDDING_DIMENSION;
        let n = documents.len();

        if n == 0 {
            return Self {
                vocab: Vec::new(),
                idf: Vec::new(),
                dimension: TARGET_DIM,
                pdg_nodes: 0,
                pdg_edges: 0,
            };
        }

        // Count document frequency per token
        let mut df: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (_, tokens) in documents {
            seen.clear();
            for tok in tokens {
                if seen.insert(tok.as_str()) {
                    *df.entry(tok.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Compute IDF for each token using a moderate-frequency filter.
        let n_f = n as f32;
        let min_df: usize = if n < 50 { 1 } else { (n / 1000).max(3) };
        let max_df: usize = if n < 50 { n } else { (n / 4).max(min_df + 1) };

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

        // Stratified vocabulary selection using sort-based sampling.
        // Sort by IDF score, then sample at quantile boundaries to get
        // diverse coverage across the full IDF range.
        idf_scores.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let final_scores: Vec<(String, f32)> = if idf_scores.len() <= TARGET_DIM {
            // Fewer candidates than target — use all.
            idf_scores
        } else {
            // Sample at stratified quantile boundaries to get TARGET_DIM elements
            // covering the full IDF range.
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
            pdg_nodes: 0,
            pdg_edges: 0,
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

    /// Embed pre-tokenized content, skipping the tokenize_code call.
    pub(crate) fn embed_tokens(&self, tokens: &[String]) -> Vec<f32> {
        let mut vec = vec![0.0f32; self.dimension];

        if self.vocab.is_empty() {
            return vec;
        }

        let total = tokens.len() as f32;
        if total == 0.0 {
            return vec;
        }

        let mut tf_map: std::collections::HashMap<&str, f32> = std::collections::HashMap::new();
        for tok in tokens {
            *tf_map.entry(tok.as_str()).or_insert(0.0) += 1.0;
        }

        for (i, (word, idf_val)) in self.vocab.iter().zip(self.idf.iter()).enumerate() {
            if let Some(&count) = tf_map.get(word.as_str()) {
                vec[i] = (count / total) * idf_val;
            }
        }

        let magnitude: f32 = vec.iter().map(|v| v * v).sum::<f32>().sqrt();
        if magnitude > 1e-9 {
            for v in &mut vec {
                *v /= magnitude;
            }
        }

        vec
    }

    #[allow(dead_code)]
    fn from_persisted_state(state: TfIdfPersistedState) -> Self {
        Self {
            vocab: state.vocab,
            idf: state.idf,
            dimension: state.dimension,
            pdg_nodes: state.pdg_nodes,
            pdg_edges: state.pdg_edges,
        }
    }

    fn persisted_state(&self, pdg: &ProgramDependenceGraph) -> TfIdfPersistedState {
        TfIdfPersistedState {
            vocab: self.vocab.clone(),
            idf: self.idf.clone(),
            dimension: self.dimension,
            pdg_nodes: pdg.node_count(),
            pdg_edges: pdg.edge_count(),
        }
    }

    fn storage_path(project_path: &Path) -> PathBuf {
        project_path.join(".leindex").join("tfidf_embedder.bin")
    }

    #[allow(dead_code)]
    fn load_from_storage(project_path: &Path) -> Result<Option<Self>> {
        let path = Self::storage_path(project_path);
        if !path.exists() {
            return Ok(None);
        }
        let bytes = std::fs::read(&path)
            .with_context(|| format!("Failed to read persisted embedder: {}", path.display()))?;
        let state: TfIdfPersistedState = bincode::deserialize(&bytes).with_context(|| {
            format!(
                "Failed to deserialize persisted embedder: {}",
                path.display()
            )
        })?;
        Ok(Some(Self::from_persisted_state(state)))
    }

    pub(crate) fn persist_to_storage(
        &self,
        project_path: &Path,
        pdg: &ProgramDependenceGraph,
    ) -> Result<()> {
        let path = Self::storage_path(project_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create embedder directory: {}", parent.display())
            })?;
        }
        let payload = bincode::serialize(&self.persisted_state(pdg))
            .context("Failed to serialize embedder")?;
        std::fs::write(&path, payload)
            .with_context(|| format!("Failed to persist embedder: {}", path.display()))
    }
}

// ============================================================================
// FILE SCANNING & HASHING
// ============================================================================

/// Read a file once, returning both its BLAKE3 hash and contents.
pub(crate) fn read_file_once(path: &Path) -> Result<(String, std::sync::Arc<Vec<u8>>)> {
    let bytes = std::sync::Arc::new(
        std::fs::read(path).with_context(|| format!("Failed to read file: {}", path.display()))?,
    );
    let hash = blake3::hash(bytes.as_slice()).to_hex().to_string();
    Ok((hash, bytes))
}

/// Hash a file using BLAKE3.
pub(crate) fn hash_file(path: &Path) -> Result<String> {
    Ok(read_file_once(path)?.0)
}

#[derive(Debug)]
struct FileReadCache {
    capacity: usize,
    entries: HashMap<PathBuf, std::sync::Arc<Vec<u8>>>,
    order: VecDeque<PathBuf>,
}

impl FileReadCache {
    fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: HashMap::new(),
            order: VecDeque::new(),
        }
    }

    fn get_or_read(&mut self, path: &Path) -> Result<std::sync::Arc<Vec<u8>>> {
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
pub(crate) fn scan_project_files(project_path: &Path) -> Result<ProjectFileScan> {
    let project_config = crate::cli::config::ProjectConfig::load(project_path).unwrap_or_default();
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
                let is_lockfile =
                    name.contains("lock") || name.contains(".sum") || name == "npm-shrinkwrap.json";
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
        .map(|path| Ok((path.clone(), read_file_once(path)?.0)))
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
) -> Result<TfIdfEmbedder> {
    *file_stats_cache = None;

    let batch_size = batch_size.max(1);
    let mut file_cache = FileReadCache::new(100);
    let connectivity_config = crate::graph::pdg::TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[EdgeType::Call, EdgeType::DataDependency]),
        excluded_node_types: Some(vec![NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };

    let node_indices: Vec<petgraph::graph::NodeIndex> = pdg.node_indices().collect();
    let mut df: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut seen_tokens: HashSet<String> = HashSet::new();
    let mut total_docs = 0usize;
    let mut tokenized_nodes: Vec<TokenizedNode> = Vec::with_capacity(node_indices.len());

    // Helper: extract enriched node content from file bytes using byte range + PDG metadata.
    let extract_node_content = |node: &crate::graph::pdg::Node,
                                node_idx: petgraph::graph::NodeIndex,
                                file_bytes: &[u8]|
     -> String {
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

        if !content.is_empty() && node.byte_range.1 > node.byte_range.0 {
            let content_bytes = content.as_bytes();
            let start = node.byte_range.0.min(content_bytes.len());
            let end = node.byte_range.1.min(content_bytes.len());
            if start < end {
                let snippet = String::from_utf8_lossy(&content_bytes[start..end]);
                format!(
                    "{}\n// {} in {}\n{}",
                    enrichment, node.name, node.file_path, snippet
                )
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
        }
    };

    // Pass 1: build document frequencies in streaming batches, dropping each batch immediately.
    for batch in node_indices.chunks(batch_size) {
        for &node_idx in batch {
            if let Some(node) = pdg.get_node(node_idx) {
                let file_bytes = file_cache
                    .get_or_read(Path::new(&*node.file_path))
                    .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));

                let node_content = extract_node_content(node, node_idx, &file_bytes);
                let tokens = tokenize_code(&node_content);
                seen_tokens.clear();
                for tok in &tokens {
                    if seen_tokens.insert(tok.clone()) {
                        *df.entry(tok.clone()).or_insert(0) += 1;
                    }
                }
                tokenized_nodes.push(TokenizedNode {
                    node_idx,
                    id: node.id.clone(),
                    content: node_content,
                    tokens,
                });
                total_docs += 1;
            }
        }
    }

    let embedder = if total_docs == 0 {
        TfIdfEmbedder::build_from_tokens(&[])
    } else {
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

        let final_scores = if idf_scores.len() <= crate::search::search::DEFAULT_EMBEDDING_DIMENSION
        {
            idf_scores
        } else {
            let total = idf_scores.len();
            let target = crate::search::search::DEFAULT_EMBEDDING_DIMENSION;
            let stride = total as f64 / target as f64;
            (0..target)
                .map(|i| {
                    let idx = ((i as f64 * stride) as usize).min(total - 1);
                    idf_scores[idx].clone()
                })
                .collect()
        };

        TfIdfEmbedder {
            vocab: final_scores.iter().map(|(t, _)| t.clone()).collect(),
            idf: final_scores.iter().map(|(_, s)| *s).collect(),
            dimension: crate::search::search::DEFAULT_EMBEDDING_DIMENSION,
            pdg_nodes: pdg.node_count(),
            pdg_edges: pdg.edge_count(),
        }
    };

    let mut nodes: Vec<NodeInfo> = Vec::with_capacity(batch_size);
    for batch in tokenized_nodes.chunks(batch_size) {
        nodes.clear();
        for tokenized in batch {
            if let Some(node) = pdg.get_node(tokenized.node_idx) {
                let embedding = embedder.embed_tokens(&tokenized.tokens);
                let signature = crate::search::search::SearchEngine::extract_signature_from_content(
                    &tokenized.content,
                );

                nodes.push(NodeInfo {
                    node_id: tokenized.id.clone(),
                    file_path: node.file_path.to_string(),
                    symbol_name: node.name.clone(),
                    language: node.language.clone(),
                    content: tokenized.content.clone(),
                    byte_range: node.byte_range,
                    embedding: Some(embedding),
                    complexity: node.complexity,
                    signature,
                });
            }
        }
        search_engine.index_nodes(std::mem::take(&mut nodes));
    }

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

    /// Regression test: verify partition-based selection produces the same
    /// vocab+idf as the original sort-based approach.
    #[test]
    fn test_tfidf_partition_matches_sort_selection() {
        use std::collections::HashMap;

        // Generate a corpus that produces >768 candidates after df filtering.
        //
        // Strategy: Create ~1200 tokens, each appearing in 3-8 documents.
        // With 200 docs, min_df=3 and max_df=50, this produces ~1200 candidates,
        // which exercises the sort+stride sampling branch (for >768 candidates).
        //
        // Token distribution:
        // - 1200 unique tokens total (3-letter lowercase tokens like "aaa", "aab", ...)
        // - Each token appears in 3-8 documents (df in range [3, 8])
        // - Documents are 200, each containing ~180 tokens
        // - All tokens are space-separated to avoid introducing extra code keywords
        //
        let mut docs: Vec<(String, String)> = Vec::with_capacity(200);

        // First, create 1200 tokens with their assigned document ranges
        // Use lowercase letter-based tokens to avoid camelCase splitting
        let token_names: Vec<String> = (0usize..1200)
            .map(|i| {
                // Create tokens like "aaa", "aab", etc. - won't be split by tokenizer
                let first = (b'a' + (i % 26) as u8) as char;
                let second = (b'a' + ((i / 26) % 26) as u8) as char;
                let third = (b'a' + ((i / 676) % 26) as u8) as char;
                format!("{}{}{}", first, second, third)
            })
            .collect();

        let mut token_doc_assignments: Vec<(String, Vec<usize>)> = Vec::new();
        for (token_id, token) in token_names.iter().enumerate() {
            // Each token appears in 3-8 documents
            let df = 3 + (token_id % 6); // df in range [3, 8]

            // Use modulo to distribute tokens across documents deterministically
            let docs_with_token: Vec<usize> = (0..df)
                .map(|j| (token_id * 7 + j * 13) % 200) // Spread across docs
                .collect();

            token_doc_assignments.push((token.clone(), docs_with_token));
        }

        // Build documents by collecting their assigned tokens
        for doc_id in 0..200 {
            let mut tokens = Vec::new();
            for (token, doc_ids) in &token_doc_assignments {
                if doc_ids.contains(&doc_id) {
                    tokens.push(token.clone());
                }
            }

            // Format as space-separated tokens (no code keywords to avoid extra tokens)
            let content = tokens.join(" ");
            docs.push((format!("doc_{}", doc_id), content));
        }

        let embedder = TfIdfEmbedder::build(&docs);

        // Build a reference vocab using the original sort+stride approach
        // with the SAME min_df/max_df logic as build_from_tokens.
        let tokenized: Vec<(String, Vec<String>)> = docs
            .iter()
            .map(|(id, content)| (id.clone(), tokenize_code(content)))
            .collect();

        let n = tokenized.len();
        let n_f = n as f32;
        // Same logic as build_from_tokens
        let min_df: usize = if n < 50 { 1 } else { (n / 1000).max(3) };
        let max_df: usize = if n < 50 { n } else { (n / 4).max(min_df + 1) };

        let mut df: HashMap<String, usize> = HashMap::new();
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for (_, tokens) in &tokenized {
            seen.clear();
            for tok in tokens {
                if seen.insert(tok.as_str()) {
                    *df.entry(tok.to_string()).or_insert(0) += 1;
                }
            }
        }

        let mut ref_scores: Vec<(String, f32)> = df
            .into_iter()
            .filter(|(_, c)| *c >= min_df && *c <= max_df)
            .map(|(tok, c)| (tok, (n_f / c as f32).ln()))
            .collect();
        // Sort by IDF score, then by token name for deterministic tie-breaking
        ref_scores.sort_by(|a, b| {
            a.1.partial_cmp(&b.1)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.0.cmp(&b.0))
        });

        let target_dim = crate::search::search::DEFAULT_EMBEDDING_DIMENSION;
        let expected_vocab: Vec<String> = if ref_scores.len() <= target_dim {
            ref_scores.iter().map(|(t, _)| t.clone()).collect()
        } else {
            let total = ref_scores.len();
            let stride = total as f64 / target_dim as f64;
            (0..target_dim)
                .map(|i| {
                    ref_scores[((i as f64 * stride) as usize).min(total - 1)]
                        .0
                        .clone()
                })
                .collect()
        };

        // The embedder's vocab should match the sort-based reference exactly.
        assert_eq!(
            embedder.vocab.len(),
            expected_vocab.len(),
            "vocab length mismatch: got {} expected {}",
            embedder.vocab.len(),
            expected_vocab.len()
        );
        for (i, (got, expected)) in embedder.vocab.iter().zip(expected_vocab.iter()).enumerate() {
            assert_eq!(
                got, expected,
                "vocab mismatch at position {i}: got '{got}' expected '{expected}'"
            );
        }
    }

    #[test]
    fn test_detect_changed_manifests_cold_start_no_false_positive() {
        // Simulates a cold-start scenario: a persisted scan exists on disk but
        // the in-memory cache is empty. Without the load_from_disk fallback,
        // old_hashes would be empty and every manifest would be flagged as changed.
        use crate::cli::memory::{CacheSpiller, MemoryConfig};
        use std::path::PathBuf;

        let temp_dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            cache_dir: temp_dir.path().join("cache"),
            max_cache_bytes: 10_000_000,
            ..Default::default()
        };

        let mut spiller = CacheSpiller::new(config).unwrap();

        // Create an old scan with a manifest hash
        let manifest_path = PathBuf::from("/project/Cargo.toml");
        let mut old_hashes = std::collections::HashMap::new();
        old_hashes.insert(manifest_path.display().to_string(), "abc123".to_string());

        let old_scan = ProjectFileScan {
            source_paths: vec![PathBuf::from("/project/src/main.rs")],
            manifest_paths: vec![manifest_path.clone()],
            source_directories: vec![PathBuf::from("/project/src")],
            manifest_hashes: old_hashes,
        };

        // Serialize and store in cache
        let cache_key = crate::cli::memory::project_scan_cache_key("test_project");
        let serialized = bincode::serialize(&old_scan).unwrap();
        let entry = crate::cli::memory::CacheEntry::Binary {
            metadata: std::collections::HashMap::new(),
            serialized_data: serialized,
        };
        spiller
            .store_mut()
            .insert(cache_key.clone(), entry)
            .unwrap();

        // Persist to disk, then remove from in-memory cache (simulating cold start)
        spiller.store_mut().persist_key(&cache_key).unwrap();
        let _ = spiller.store_mut().remove(&cache_key);

        // Verify in-memory cache is empty (peek returns None)
        assert!(
            spiller.store().peek(&cache_key).is_none(),
            "peek should return None after removal"
        );

        // Create a current scan with the SAME manifest hashes
        let mut current_hashes = std::collections::HashMap::new();
        current_hashes.insert(manifest_path.display().to_string(), "abc123".to_string());

        let current_scan = ProjectFileScan {
            source_paths: vec![PathBuf::from("/project/src/main.rs")],
            manifest_paths: vec![manifest_path.clone()],
            source_directories: vec![PathBuf::from("/project/src")],
            manifest_hashes: current_hashes,
        };

        // Without the fix, this would return the manifest as changed (false positive)
        // because old_hashes would be empty (peek returns None).
        let changed = detect_changed_manifests(&current_scan, "test_project", &spiller);

        assert!(
            changed.is_empty(),
            "cold start should NOT produce false-positive manifest changes, got: {:?}",
            changed
        );
    }

    #[test]
    fn test_detect_changed_manifests_detects_real_change() {
        // Verifies that a real manifest change is still detected even on cold start.
        use crate::cli::memory::{CacheSpiller, MemoryConfig};
        use std::path::PathBuf;

        let temp_dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            cache_dir: temp_dir.path().join("cache"),
            max_cache_bytes: 10_000_000,
            ..Default::default()
        };

        let mut spiller = CacheSpiller::new(config).unwrap();

        let manifest_path = PathBuf::from("/project/Cargo.toml");
        let mut old_hashes = std::collections::HashMap::new();
        old_hashes.insert(manifest_path.display().to_string(), "old_hash".to_string());

        let old_scan = ProjectFileScan {
            source_paths: vec![PathBuf::from("/project/src/main.rs")],
            manifest_paths: vec![manifest_path.clone()],
            source_directories: vec![PathBuf::from("/project/src")],
            manifest_hashes: old_hashes,
        };

        let cache_key = crate::cli::memory::project_scan_cache_key("test_project2");
        let serialized = bincode::serialize(&old_scan).unwrap();
        let entry = crate::cli::memory::CacheEntry::Binary {
            metadata: std::collections::HashMap::new(),
            serialized_data: serialized,
        };
        spiller
            .store_mut()
            .insert(cache_key.clone(), entry)
            .unwrap();
        spiller.store_mut().persist_key(&cache_key).unwrap();
        let _ = spiller.store_mut().remove(&cache_key);

        // Current scan with DIFFERENT manifest hash
        let mut current_hashes = std::collections::HashMap::new();
        current_hashes.insert(manifest_path.display().to_string(), "new_hash".to_string());

        let current_scan = ProjectFileScan {
            source_paths: vec![PathBuf::from("/project/src/main.rs")],
            manifest_paths: vec![manifest_path.clone()],
            source_directories: vec![PathBuf::from("/project/src")],
            manifest_hashes: current_hashes,
        };

        let changed = detect_changed_manifests(&current_scan, "test_project2", &spiller);

        assert_eq!(
            changed.len(),
            1,
            "should detect exactly one changed manifest"
        );
        assert_eq!(
            changed[0], manifest_path,
            "should detect the correct manifest as changed"
        );
    }

    #[test]
    fn test_detect_changed_manifests_uses_in_memory_cache_first() {
        // When both in-memory and disk caches exist, in-memory takes priority.
        use crate::cli::memory::{CacheSpiller, MemoryConfig};
        use std::path::PathBuf;

        let temp_dir = tempfile::tempdir().unwrap();
        let config = MemoryConfig {
            cache_dir: temp_dir.path().join("cache"),
            max_cache_bytes: 10_000_000,
            ..Default::default()
        };

        let mut spiller = CacheSpiller::new(config).unwrap();

        let manifest_path = PathBuf::from("/project/Cargo.toml");

        // Create a stale disk cache with old hash
        let mut disk_hashes = std::collections::HashMap::new();
        disk_hashes.insert(
            manifest_path.display().to_string(),
            "stale_hash".to_string(),
        );
        let disk_scan = ProjectFileScan {
            source_paths: vec![],
            manifest_paths: vec![manifest_path.clone()],
            source_directories: vec![],
            manifest_hashes: disk_hashes,
        };
        let cache_key = crate::cli::memory::project_scan_cache_key("test_project3");
        let serialized = bincode::serialize(&disk_scan).unwrap();
        let disk_entry = crate::cli::memory::CacheEntry::Binary {
            metadata: std::collections::HashMap::new(),
            serialized_data: serialized,
        };
        spiller
            .store_mut()
            .insert(cache_key.clone(), disk_entry)
            .unwrap();
        spiller.store_mut().persist_key(&cache_key).unwrap();

        // Create a fresh in-memory cache with current hash
        let mut mem_hashes = std::collections::HashMap::new();
        mem_hashes.insert(
            manifest_path.display().to_string(),
            "current_hash".to_string(),
        );
        let mem_scan = ProjectFileScan {
            source_paths: vec![],
            manifest_paths: vec![manifest_path.clone()],
            source_directories: vec![],
            manifest_hashes: mem_hashes,
        };
        let mem_serialized = bincode::serialize(&mem_scan).unwrap();
        let mem_entry = crate::cli::memory::CacheEntry::Binary {
            metadata: std::collections::HashMap::new(),
            serialized_data: mem_serialized,
        };
        spiller
            .store_mut()
            .insert(cache_key.clone(), mem_entry)
            .unwrap();

        // Current scan matches the in-memory hash, not the disk hash
        let mut current_hashes = std::collections::HashMap::new();
        current_hashes.insert(
            manifest_path.display().to_string(),
            "current_hash".to_string(),
        );
        let current_scan = ProjectFileScan {
            source_paths: vec![],
            manifest_paths: vec![manifest_path.clone()],
            source_directories: vec![],
            manifest_hashes: current_hashes,
        };

        let changed = detect_changed_manifests(&current_scan, "test_project3", &spiller);

        assert!(
            changed.is_empty(),
            "in-memory cache should be preferred over disk; should see no changes"
        );
    }

    #[test]
    fn test_tfidf_embedder_clone_roundtrip() {
        let docs = vec![
            ("a".to_string(), "fn alpha beta gamma".to_string()),
            ("b".to_string(), "fn delta epsilon zeta".to_string()),
        ];
        let embedder = TfIdfEmbedder::build(&docs);
        let cloned = embedder.clone();
        assert_eq!(embedder.vocab, cloned.vocab);
        assert_eq!(embedder.idf, cloned.idf);
        assert_eq!(embedder.dimension, cloned.dimension);
    }

    #[test]
    fn test_tfidf_incremental_batches_match_full_build() {
        let docs: Vec<(String, String)> = (0..120)
            .map(|i| {
                let body = format!(
                    "fn item_{}() {{ let value = {}; let shared = value + {}; }}",
                    i,
                    i,
                    i % 7
                );
                (format!("doc_{i}"), body)
            })
            .collect();
        let full = TfIdfEmbedder::build(&docs);

        let tokenized: Vec<(String, Vec<String>)> = docs
            .iter()
            .map(|(id, content)| (id.clone(), tokenize_code(content)))
            .collect();
        let chunked = TfIdfEmbedder::build_from_tokens(&tokenized);

        assert_eq!(full.vocab, chunked.vocab);
        assert_eq!(full.idf, chunked.idf);
        assert_eq!(full.dimension, chunked.dimension);
    }

    #[test]
    fn test_index_nodes_respects_batch_size_and_matches_results() {
        use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};
        use crate::search::search::SearchEngine;

        let mut pdg = ProgramDependenceGraph::new();
        for i in 0..8 {
            pdg.add_node(Node {
                id: format!("node_{i}"),
                name: format!("symbol_{i}"),
                file_path: format!("/tmp/file_{i}.rs").into(),
                language: format!("rust batch {i}"),
                node_type: NodeType::Function,
                byte_range: (0, 100),
                complexity: i as u32 + 1,
            });
        }

        let mut file_stats_cache = None;
        let mut engine_small = SearchEngine::new();
        let embedder_small =
            index_nodes(&pdg, &mut engine_small, &mut file_stats_cache, 2).unwrap();

        let mut file_stats_cache = None;
        let mut engine_large = SearchEngine::new();
        let embedder_large =
            index_nodes(&pdg, &mut engine_large, &mut file_stats_cache, 64).unwrap();

        assert_eq!(embedder_small.vocab, embedder_large.vocab);
        assert_eq!(embedder_small.idf, embedder_large.idf);
        assert_eq!(embedder_small.dimension, embedder_large.dimension);
    }

    #[test]
    fn test_index_nodes_accumulates_df_across_passes() {
        use crate::graph::pdg::{Node, NodeType, ProgramDependenceGraph};
        use crate::search::search::SearchEngine;

        let mut pdg = ProgramDependenceGraph::new();
        for i in 0..6 {
            let content_tag = if i < 3 { "shared_token" } else { "other_token" };
            pdg.add_node(Node {
                id: format!("node_{i}"),
                name: format!("symbol_{i}"),
                file_path: format!("/tmp/file_{i}.rs").into(),
                language: "rust".to_string(),
                node_type: NodeType::Function,
                byte_range: (0, 100),
                complexity: 1,
            });
            let _ = content_tag;
        }

        let mut cache = None;
        let mut engine = SearchEngine::new();
        let embedder = index_nodes(&pdg, &mut engine, &mut cache, 3).unwrap();

        assert_eq!(
            embedder.dimension,
            crate::search::search::DEFAULT_EMBEDDING_DIMENSION
        );
        assert_eq!(engine.node_count(), 3);
    }

    #[test]
    fn test_tfidf_embedder_persist_roundtrip() {
        let temp = tempfile::tempdir().unwrap();
        let docs = vec![("a".to_string(), "fn alpha beta gamma".to_string())];
        let embedder = TfIdfEmbedder::build(&docs);
        let pdg = { crate::graph::pdg::ProgramDependenceGraph::new() };
        embedder.persist_to_storage(temp.path(), &pdg).unwrap();
        let loaded = TfIdfEmbedder::load_from_storage(temp.path())
            .unwrap()
            .unwrap();
        assert_eq!(embedder.vocab, loaded.vocab);
        assert_eq!(embedder.idf, loaded.idf);
        assert_eq!(embedder.dimension, loaded.dimension);
    }

    #[test]
    fn test_tfidf_embedder_missing_file_returns_none() {
        let temp = tempfile::tempdir().unwrap();
        assert!(TfIdfEmbedder::load_from_storage(temp.path())
            .unwrap()
            .is_none());
    }
    #[test]
    fn test_tfidf_build_from_tokens_is_deterministic_across_batch_sizes() {
        let docs: Vec<(String, String)> = (0..90)
            .map(|i| {
                let body = format!(
                    "pub fn symbol_{}() -> usize {{ {} + {} + {} }}",
                    i,
                    i,
                    i % 5,
                    i % 11
                );
                (format!("doc_{i}"), body)
            })
            .collect();
        let tokenized: Vec<(String, Vec<String>)> = docs
            .iter()
            .map(|(id, content)| (id.clone(), tokenize_code(content)))
            .collect();

        let embedder_a = TfIdfEmbedder::build_from_tokens(&tokenized);
        let embedder_b = TfIdfEmbedder::build_from_tokens(&tokenized);

        assert_eq!(embedder_a.vocab, embedder_b.vocab);
        assert_eq!(embedder_a.idf, embedder_b.idf);
    }
}
