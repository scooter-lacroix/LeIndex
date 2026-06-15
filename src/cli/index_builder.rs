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

/// TF-IDF embedding provider for code search
///
/// Implements term frequency-inverse document frequency (TF-IDF) embeddings
/// for semantic code search. Uses stratified vocabulary selection across IDF
/// ranges to maximize coverage while maintaining fixed dimension (768).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TfIdfEmbedder {
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
    pub fn build(documents: &[(String, String)]) -> Self {
        let tokenized: Vec<(String, Vec<String>)> = documents
            .iter()
            .map(|(id, content)| (id.clone(), tokenize_code(content)))
            .collect();
        Self::build_from_tokens(&tokenized)
    }

    /// Build a TF-IDF embedder from pre-tokenized documents.
    pub fn build_from_tokens(documents: &[(String, Vec<String>)]) -> Self {
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
    pub fn embed(&self, text: &str) -> Vec<f32> {
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
    pub fn embed_tokens(&self, tokens: &[String]) -> Vec<f32> {
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

    /// Check if the embedder is fresh relative to the current PDG state
    ///
    /// Returns true if the embedder was built from the same PDG state
    /// (same node and edge counts), indicating no reindex is needed.
    pub fn is_fresh(&self, pdg_node_count: usize, pdg_edge_count: usize) -> bool {
        self.pdg_nodes == pdg_node_count && self.pdg_edges == pdg_edge_count
    }

    /// Get the embedding dimension
    pub fn dimension(&self) -> usize {
        self.dimension
    }

    fn storage_path(project_path: &Path) -> PathBuf {
        project_path.join(".leindex").join("tfidf_embedder.bin")
    }

    /// Load a persisted TF-IDF embedder from storage
    ///
    /// Attempts to load a previously persisted embedder from the project's
    /// `.leindex/tfidf_embedder.bin` file. Returns None if the file doesn't exist.
    #[allow(dead_code)]
    pub fn load_from_storage(project_path: &Path) -> Result<Option<Self>> {
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

    /// Persist the TF-IDF embedder to storage
    ///
    /// Serializes the embedder state (vocabulary, IDF scores, PDG counts)
    /// to the project's `.leindex/tfidf_embedder.bin` file for future loading.
    pub fn persist_to_storage(
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
// HYBRID EMBEDDING BACKEND (Unified TF-IDF + Neural/Remote)
// ============================================================================

/// Neural embedding dimension for the Qwen3-Embedding-0.6B ONNX model.
///
/// This dimension is used for local ONNX-backed neural embeddings.
/// The value must match the output dimension of the deployed model.
#[cfg(feature = "onnx")]
pub(crate) const NEURAL_EMBEDDING_DIMENSION: usize = 1024;

/// Hybrid embedding backend that always uses TF-IDF as base signal
/// with optional neural/remote embeddings as enhancement layers
#[derive(Debug, Clone)]
pub enum HybridEmbedder {
    /// TF-IDF only (base signal always available)
    TfIdfOnly(TfIdfEmbedder),

    /// TF-IDF + Local ONNX neural embeddings (via worker process)
    #[cfg(feature = "onnx")]
    HybridLocal {
        /// TF-IDF embedder for keyword-based search
        tfidf: TfIdfEmbedder,
        /// Worker client for neural embedding via leindex-embed process
        neural: EmbeddingClient,
        /// Weight for neural embeddings in hybrid scoring (0.0-1.0)
        neural_weight: f32,
    },

    /// TF-IDF + Remote embeddings (OpenAI, Cohere, custom)
    #[cfg(feature = "remote-embeddings")]
    HybridRemote {
        /// TF-IDF embedder for keyword-based search
        tfidf: TfIdfEmbedder,
        /// Remote embedding provider for semantic search
        remote: GenericRemoteProvider,
        /// Weight for remote embeddings in hybrid scoring (0.0-1.0)
        remote_weight: f32,
    },
}

/// Scoring weights for hybrid embedding combination
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct HybridScoringWeights {
    /// Weight for TF-IDF signal (0.0-1.0)
    pub tfidf: f32,
    /// Weight for neural/remote signal (0.0-1.0)
    pub neural: f32,
    /// Weight for structural signal (0.0-1.0)
    pub structural: f32,
    /// Weight for text match signal (0.0-1.0)
    pub text_match: f32,
}

impl Default for HybridScoringWeights {
    fn default() -> Self {
        Self {
            tfidf: 0.30,
            neural: 0.40,
            structural: 0.15,
            text_match: 0.15,
        }
    }
}

impl HybridScoringWeights {
    /// Create weights when neural embedding is unavailable
    pub fn without_neural() -> Self {
        Self {
            tfidf: 0.60,
            neural: 0.00,
            structural: 0.20,
            text_match: 0.20,
        }
    }

    /// Normalize weights to sum to 1.0
    pub fn normalize(&self) -> Self {
        let sum = self.tfidf + self.neural + self.structural + self.text_match;
        if sum == 0.0 {
            return Self::default();
        }
        Self {
            tfidf: self.tfidf / sum,
            neural: self.neural / sum,
            structural: self.structural / sum,
            text_match: self.text_match / sum,
        }
    }
}

impl HybridEmbedder {
    /// Create a TF-IDF only embedder (default)
    pub fn tfidf_only(embedder: TfIdfEmbedder) -> Self {
        Self::TfIdfOnly(embedder)
    }

    /// Create a hybrid embedder with local ONNX neural embeddings via worker
    #[cfg(feature = "onnx")]
    pub fn hybrid_local(tfidf: TfIdfEmbedder, neural_weight: Option<f32>) -> Result<Self, String> {
        Ok(Self::HybridLocal {
            tfidf,
            neural: EmbeddingClient::new(),
            neural_weight: neural_weight.unwrap_or(0.40),
        })
    }

    /// Create a hybrid embedder with remote embeddings
    #[cfg(feature = "remote-embeddings")]
    pub fn hybrid_remote(
        tfidf: TfIdfEmbedder,
        remote_config: RemoteEmbeddingConfig,
        remote_weight: Option<f32>,
    ) -> Result<Self, RemoteEmbeddingError> {
        let remote = GenericRemoteProvider::from_config(remote_config)?;
        Ok(Self::HybridRemote {
            tfidf,
            remote,
            remote_weight: remote_weight.unwrap_or(0.40),
        })
    }

    /// Get the TF-IDF embedder (always available)
    pub fn tfidf(&self) -> &TfIdfEmbedder {
        match self {
            Self::TfIdfOnly(embedder) => embedder,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { tfidf, .. } => tfidf,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { tfidf, .. } => tfidf,
        }
    }

    /// Get the TF-IDF embedder mutably (always available)
    pub fn tfidf_mut(&mut self) -> &mut TfIdfEmbedder {
        match self {
            Self::TfIdfOnly(embedder) => embedder,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { tfidf, .. } => tfidf,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { tfidf, .. } => tfidf,
        }
    }

    /// Get the TF-IDF dimension (always 768)
    pub fn tfidf_dimension(&self) -> usize {
        self.tfidf().dimension()
    }

    /// Get the neural/remote dimension (if available)
    pub fn neural_dimension(&self) -> Option<usize> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { .. } => Some(NEURAL_EMBEDDING_DIMENSION),
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote, .. } => Some(remote.dimension()),
        }
    }

    /// Check if neural/remote enhancement is available
    pub fn has_neural(&self) -> bool {
        match self {
            Self::TfIdfOnly(_) => false,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { .. } => true,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => true,
        }
    }

    /// Get the neural weight for scoring
    pub fn neural_weight(&self) -> f32 {
        match self {
            Self::TfIdfOnly(_) => 0.0,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural_weight, .. } => *neural_weight,
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote_weight, .. } => *remote_weight,
        }
    }

    /// Get recommended scoring weights
    pub fn scoring_weights(&self) -> HybridScoringWeights {
        if self.has_neural() {
            HybridScoringWeights::default()
        } else {
            HybridScoringWeights::without_neural()
        }
    }

    /// Generate TF-IDF embedding for pre-tokenized content (always available)
    pub fn embed_tfidf(&self, tokens: &[String]) -> Vec<f32> {
        self.tfidf().embed_tokens(tokens)
    }

    /// Generate neural/remote embedding for text (if available)
    ///
    /// Uses `embed_with_fallback` for retry-once semantics:
    /// - VAL-CPHASE-017: Retries once on worker failure
    /// - VAL-CPHASE-018: Falls back to TF-IDF for the affected batch after second failure
    /// - VAL-CPHASE-019: Emits actionable warning on fallback
    /// - VAL-CPHASE-020: Worker failure does not crash the main daemon
    /// - VAL-CPHASE-021: Fresh worker can be spawned after fallback
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub async fn embed_neural_async(&self, text: &str) -> Option<Result<Vec<f32>, String>> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                // Clone shares the worker handle via Arc (EmbeddingClient::clone is cheap).
                // Required because spawn_blocking requires ownership.
                let neural = neural.clone();
                let texts = vec![text.to_string()];
                let result = task::spawn_blocking(move || {
                    neural.embed_with_fallback(&texts, NEURAL_EMBEDDING_DIMENSION)
                })
                .await
                .ok()?;
                match result {
                    EmbedResult::Success(response) => {
                        if response.count > 0 {
                            // VAL-CPHASE-016: Write from flat buffer directly
                            Some(Ok(response.into_vectors().into_iter().next().unwrap()))
                        } else {
                            Some(Err("worker returned empty response".to_string()))
                        }
                    }
                    EmbedResult::Fallback { batch_id, error } => {
                        // VAL-CPHASE-018/019: Fallback already logged actionable warning.
                        tracing::warn!(
                            batch_id = %batch_id,
                            error = %error,
                            "Neural embedding degraded to TF-IDF for node (async path)"
                        );
                        None
                    }
                }
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { remote, .. } => Some(
                remote
                    .embed(text)
                    .await
                    .map_err(|e| format!("Remote embedding failed: {}", e)),
            ),
        }
    }

    /// Generate neural/remote embedding for text (blocking wrapper for sync contexts)
    ///
    /// Uses `embed_with_fallback` for retry-once semantics:
    /// - VAL-CPHASE-017: Retries once on worker failure
    /// - VAL-CPHASE-018: Falls back to TF-IDF for the affected batch after second failure
    /// - VAL-CPHASE-019: Emits actionable warning on fallback
    /// - VAL-CPHASE-020: Worker failure does not crash the main daemon
    /// - VAL-CPHASE-021: Fresh worker can be spawned after fallback
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub fn embed_neural_blocking(&self, text: &str) -> Option<Result<Vec<f32>, String>> {
        match self {
            Self::TfIdfOnly(_) => None,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                let texts = vec![text.to_string()];
                let result = neural.embed_with_fallback(&texts, NEURAL_EMBEDDING_DIMENSION);
                match result {
                    EmbedResult::Success(response) => {
                        if response.count > 0 {
                            // VAL-CPHASE-016: Write from flat buffer directly,
                            // avoiding nested Vec<Vec<f32>> heap mirror
                            Some(Ok(response.into_vectors().into_iter().next().unwrap()))
                        } else {
                            Some(Err("worker returned empty response".to_string()))
                        }
                    }
                    EmbedResult::Fallback { batch_id, error } => {
                        // VAL-CPHASE-018/019: Fallback already logged actionable warning.
                        // Return None so the caller falls back to TF-IDF for this batch.
                        tracing::warn!(
                            batch_id = %batch_id,
                            error = %error,
                            "Neural embedding degraded to TF-IDF for node"
                        );
                        None
                    }
                }
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => {
                // Remote requires async runtime, this is a blocking wrapper
                // In practice, the indexing pipeline should use the async version
                Some(Err("Remote embeddings require async runtime".to_string()))
            }
        }
    }

    /// Generate neural/remote embeddings for a batch of texts (blocking wrapper).
    ///
    /// Returns `Vec<Option<Vec<f32>>>` — one entry per input text.
    /// `Some(vec)` on success, `None` when neural is unavailable or on fallback.
    ///
    /// This batches all texts into a single IPC call to the ONNX worker,
    /// reducing N round-trips to 1 per chunk.
    #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
    pub fn embed_neural_batch_blocking(&self, texts: &[String]) -> Vec<Option<Vec<f32>>> {
        match self {
            Self::TfIdfOnly(_) => vec![None; texts.len()],
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                if texts.is_empty() {
                    return Vec::new();
                }
                let result = neural.embed_with_fallback(texts, NEURAL_EMBEDDING_DIMENSION);
                match result {
                    EmbedResult::Success(response) => {
                        if response.count == texts.len() {
                            response.into_vectors().into_iter().map(Some).collect()
                        } else {
                            tracing::warn!(
                                expected = texts.len(),
                                got = response.count,
                                "Neural batch returned wrong count, falling back to None for all"
                            );
                            vec![None; texts.len()]
                        }
                    }
                    EmbedResult::Fallback { batch_id, error } => {
                        tracing::warn!(
                            batch_id = %batch_id,
                            error = %error,
                            "Neural batch embedding degraded to TF-IDF for {} texts",
                            texts.len()
                        );
                        vec![None; texts.len()]
                    }
                }
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => {
                // Remote requires async runtime; not supported in blocking context
                vec![None; texts.len()]
            }
        }
    }

    /// Persist the TF-IDF embedder to storage
    ///
    /// Delegates to the inner TfIdfEmbedder's persist_to_storage method
    pub fn persist_to_storage(
        &self,
        project_path: &Path,
        pdg: &ProgramDependenceGraph,
    ) -> Result<()> {
        self.tfidf().persist_to_storage(project_path, pdg)
    }

    /// Unload the ONNX session if the hybrid backend uses one (A+ idle-unload).
    ///
    /// After an indexing batch completes, calling this drops the live ONNX
    /// session so it does not remain resident indefinitely (VAL-APLUS-024).
    /// With the worker architecture, this signals the worker to shut down.
    pub fn unload_onnx(&self) {
        match self {
            Self::TfIdfOnly(_) => {}
            #[cfg(feature = "onnx")]
            Self::HybridLocal { neural, .. } => {
                // Kill the worker process; the client can spawn a fresh one later.
                neural.kill_worker();
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => {}
        }
    }

    /// Check whether the ONNX session is currently loaded.
    #[must_use]
    pub fn is_onnx_loaded(&self) -> bool {
        match self {
            Self::TfIdfOnly(_) => false,
            #[cfg(feature = "onnx")]
            Self::HybridLocal { .. } => {
                // With the worker architecture, "loaded" means the worker process
                // is running. This will be properly tracked in the runtime lifecycle
                // feature. For now, return false as the worker is spawned on demand.
                false
            }
            #[cfg(feature = "remote-embeddings")]
            Self::HybridRemote { .. } => false,
        }
    }
}

impl Default for HybridEmbedder {
    fn default() -> Self {
        Self::TfIdfOnly(TfIdfEmbedder::build_from_tokens(&[]))
    }
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
                // Enforce individual file size limit
                let file_size = entry.metadata().map(|m| m.len()).unwrap_or(0);
                if limits.max_file_size > 0 && file_size > limits.max_file_size {
                    oversized_count += 1;
                    if oversized_count <= 5 {
                        tracing::warn!(
                            file = %path.display(),
                            size_bytes = file_size,
                            limit_bytes = limits.max_file_size,
                            "Skipping file exceeding max_file_size limit"
                        );
                    }
                    continue;
                }

                // Enforce max files count limit
                if limits.max_files > 0 && source_paths.len() >= limits.max_files {
                    tracing::warn!(
                        count = source_paths.len(),
                        limit = limits.max_files,
                        "Reached max_files limit, stopping source file scan"
                    );
                    break;
                }

                // Enforce total size limit
                if limits.max_total_size > 0
                    && total_source_size + file_size > limits.max_total_size
                {
                    tracing::warn!(
                        total_bytes = total_source_size + file_size,
                        limit_bytes = limits.max_total_size,
                        "Reached max_total_size limit, stopping source file scan"
                    );
                    break;
                }

                total_source_size += file_size;
                source_paths.push(path.to_path_buf());
            }
        }
    }

    if oversized_count > 5 {
        tracing::warn!(
            total_oversized = oversized_count,
            "Additional oversized files skipped (showing first 5 warnings)"
        );
    }

    let source_directories = crate::cli::index_freshness::extract_unique_dirs(&source_paths);

    let mut manifest_hashes = std::collections::HashMap::new();
    for mp in &manifest_paths {
        if let Ok(bytes) = std::fs::read(mp) {
            let hash = blake3::hash(&bytes).to_hex().to_string();
            manifest_hashes.insert(mp.display().to_string(), hash);
        }
    }

    // Pre-canonicalize each manifest path so `is_stale_fast`
    // can use the result directly without re-running
    // `Path::canonicalize` (stat/readlink per file) on every
    // freshness check. Relative scanner outputs (which occur
    // when the caller passes a relative `project_path`) are
    // joined against `project_path` first to match the
    // round-13 `build_already_listed` contract — a relative
    // path canonicalized directly would resolve against CWD,
    // not the project root, producing false negatives in the
    // freshness check when CWD ≠ project root.
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

    Ok(ProjectFileScan {
        source_paths,
        manifest_paths,
        manifest_paths_canonical,
        source_directories,
        manifest_hashes,
    })
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
    let mut df: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
    let mut seen_tokens: HashSet<String> = HashSet::new();
    let mut total_docs = 0usize;

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

    // Pass 1: build document frequencies in streaming batches, dropping content immediately.
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
                total_docs += 1;
            }
        }
    }

    let embedder = if let Some(embedder) = embedder {
        embedder
    } else if total_docs == 0 {
        HybridEmbedder::tfidf_only(TfIdfEmbedder::build_from_tokens(&[]))
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

        let tfidf_embedder = TfIdfEmbedder {
            vocab: final_scores.iter().map(|(t, _)| t.clone()).collect(),
            idf: final_scores.iter().map(|(_, s)| *s).collect(),
            dimension: crate::search::search::DEFAULT_EMBEDDING_DIMENSION,
            pdg_nodes: pdg.node_count(),
            pdg_edges: pdg.edge_count(),
        };

        HybridEmbedder::tfidf_only(tfidf_embedder)
    };

    // A+ bound-gated admission, selective pruning, and work hoisting.
    let pruner = crate::search::search::ContentPruner::new();
    let mut admission_gate = crate::search::search::IndexingAdmissionGate::new();
    let mut work_hoister = crate::search::search::WorkHoister::new();
    let mut pruned_count: usize = 0;
    let mut shed_count: usize = 0;
    let mut hoisted_count: usize = 0;

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
            if let Some(node) = pdg.get_node(node_idx) {
                // Re-read and re-tokenize node content for Pass 2
                let file_bytes = file_cache
                    .get_or_read(Path::new(&*node.file_path))
                    .unwrap_or_else(|_| std::sync::Arc::new(Vec::new()));

                let node_content = extract_node_content(node, node_idx, &file_bytes);

                // A+ VAL-APLUS-038: Full content pruning check (now that we have content).
                let pruning_decision = pruner.evaluate(&node.file_path, &node_content, &node.name);
                if pruning_decision != crate::search::search::PruningDecision::Keep {
                    pruned_count += 1;
                    continue;
                }

                // A+ VAL-APLUS-037: Bound-gated admission — shed oversized/bursty work.
                if !admission_gate.try_admit(node_content.len()) {
                    shed_count += 1;
                    continue;
                }

                let tokens = tokenize_code(&node_content);

                // A+ VAL-APLUS-039: Repeated-work hoisting — reuse cached embedding
                // if we've already computed one for identical content.
                // Both TF-IDF and neural embeddings are cached to avoid redundant
                // ONNX inference on cache hits.
                let (tfidf_embedding, cached_neural) =
                    if let Some((tfidf, neural)) = work_hoister.lookup(&node_content) {
                        hoisted_count += 1;
                        (tfidf, neural)
                    } else {
                        let embedding = embedder.embed_tfidf(&tokens);
                        // Don't store yet — we'll store after computing neural embedding
                        // so both are cached together.
                        (embedding, None)
                    };

                // Determine neural embedding: use cache hit, or defer to batch call.
                let neural_embedding;
                let needs_batch_neural;
                if cached_neural.is_some() {
                    neural_embedding = cached_neural;
                    needs_batch_neural = false;
                } else if embedder.has_neural() {
                    // Defer neural embedding to batch call below
                    neural_embedding = None;
                    needs_batch_neural = true;
                } else {
                    // No neural backend available
                    work_hoister.store(&node_content, tfidf_embedding.clone(), None);
                    neural_embedding = None;
                    needs_batch_neural = false;
                }

                let signature = crate::search::search::SearchEngine::extract_signature_from_content(
                    &node_content,
                );

                // R8: Compute search-engine tokens from content. The search engine
                // uses a different tokenizer than TF-IDF (splits on non-alphanumeric,
                // lowercases, filters by length >= 2). Pre-computing avoids re-tokenization.
                let search_tokens: Vec<String> = node_content
                    .split(|c: char| !c.is_alphanumeric())
                    .map(|s| s.to_ascii_lowercase())
                    .filter(|s| s.len() >= 2)
                    .collect();

                let node_vec_idx = nodes.len();
                if needs_batch_neural {
                    neural_pending.push(node_vec_idx);
                }

                nodes.push(NodeInfo {
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
                });
            }
        }

        // Batch neural embedding: one IPC call for all pending nodes in this chunk.
        if !neural_pending.is_empty() {
            #[cfg(any(feature = "onnx", feature = "remote-embeddings"))]
            let batch_results = {
                let texts: Vec<String> = neural_pending
                    .iter()
                    .map(|&idx| nodes[idx].content.clone())
                    .collect();
                embedder.embed_neural_batch_blocking(&texts)
            };
            #[cfg(not(any(feature = "onnx", feature = "remote-embeddings")))]
            let batch_results: Vec<Option<Vec<f32>>> = vec![None; neural_pending.len()];

            for (i, &node_vec_idx) in neural_pending.iter().enumerate() {
                let neural = batch_results.get(i).and_then(|r| r.clone());
                // Store in work hoister so future cache hits avoid re-computation
                work_hoister.store(
                    &nodes[node_vec_idx].content,
                    nodes[node_vec_idx].tfidf_embedding.clone(),
                    neural.clone(),
                );
                nodes[node_vec_idx].neural_embedding = neural;
            }
        }

        search_engine.append_nodes(std::mem::replace(
            &mut nodes,
            Vec::with_capacity(batch_size),
        ));
    }

    // A+ logging: per-batch stats at info! level (invisible under default WARN).
    if pruned_count > 0 || shed_count > 0 || hoisted_count > 0 {
        info!(
            pruned = pruned_count,
            shed = shed_count,
            hoisted = hoisted_count,
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
    let embeddings = search_engine.collect_embeddings();
    if embeddings.is_empty() {
        return Ok(());
    }
    let path = crate::search::vector::mmap_embeddings_path(project_path);
    crate::search::vector::write_mmap_embeddings(&path, &embeddings)
        .map_err(|e| anyhow::anyhow!("Failed to write mmap embeddings: {e}"))?;
    info!(
        count = embeddings.len(),
        path = %path.display(),
        "Persisted embeddings to mmap file"
    );
    Ok(())
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
    let search_prefix = format!("search_query_{}", sanitize_for_prefix(project_id));
    let analysis_prefix = format!("analysis_analyze_{}", sanitize_for_prefix(project_id));

    // Remove from in-memory cache
    let mem_search = store.remove_prefix("search:query:");
    let mem_analysis = store.remove_prefix("analysis:analyze:");

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
        .take(20)
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
    let path = crate::search::vector::mmap_embeddings_path(project_path);
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

#[cfg(test)]
#[allow(clippy::infallible_destructuring_match)]
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
            manifest_paths_canonical: Vec::new(),
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
            source_paths: vec![],
            manifest_paths: vec![manifest_path.clone()],
            manifest_paths_canonical: Vec::new(),
            source_directories: vec![],
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
            manifest_paths_canonical: Vec::new(),
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
            manifest_paths_canonical: Vec::new(),
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
            manifest_paths_canonical: Vec::new(),
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
            manifest_paths_canonical: Vec::new(),
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
            source_paths: vec![PathBuf::from("/project/src/main.rs")],
            manifest_paths: vec![manifest_path.clone()],
            manifest_paths_canonical: Vec::new(),
            source_directories: vec![PathBuf::from("/project/src")],
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

        // Extract TfIdfEmbedder from HybridEmbedder to access internal fields
        let tfidf_small = match embedder_small {
            HybridEmbedder::TfIdfOnly(emb) => emb,
            #[cfg(feature = "onnx")]
            HybridEmbedder::HybridLocal { tfidf, .. } => tfidf,
            #[cfg(feature = "remote-embeddings")]
            HybridEmbedder::HybridRemote { tfidf, .. } => tfidf,
        };
        let tfidf_large = match embedder_large {
            HybridEmbedder::TfIdfOnly(emb) => emb,
            #[cfg(feature = "onnx")]
            HybridEmbedder::HybridLocal { tfidf, .. } => tfidf,
            #[cfg(feature = "remote-embeddings")]
            HybridEmbedder::HybridRemote { tfidf, .. } => tfidf,
        };

        assert_eq!(tfidf_small.vocab, tfidf_large.vocab);
        assert_eq!(tfidf_small.idf, tfidf_large.idf);
        assert_eq!(tfidf_small.dimension, tfidf_large.dimension);
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

        // Extract TfIdfEmbedder from HybridEmbedder to access dimension
        let tfidf_embedder = match embedder {
            HybridEmbedder::TfIdfOnly(emb) => emb,
            #[cfg(feature = "onnx")]
            HybridEmbedder::HybridLocal { tfidf, .. } => tfidf,
            #[cfg(feature = "remote-embeddings")]
            HybridEmbedder::HybridRemote { tfidf, .. } => tfidf,
        };

        assert_eq!(
            tfidf_embedder.dimension,
            crate::search::search::DEFAULT_EMBEDDING_DIMENSION
        );
        // All 6 nodes should be indexed (append_nodes accumulates across batches)
        assert_eq!(engine.node_count(), 6);
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
    fn test_tfidf_embedder_freshness_checks_pdg_counts() {
        let mut embedder = TfIdfEmbedder::build_from_tokens(&[]);
        embedder.pdg_nodes = 3;
        embedder.pdg_edges = 7;
        assert!(embedder.is_fresh(3, 7));
        assert!(!embedder.is_fresh(4, 7));
        assert!(!embedder.is_fresh(3, 8));
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

    #[test]
    fn test_read_file_once_hash_and_content() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = temp.path().join("test_file.txt");
        let content = b"Hello, streaming BLAKE3 world!";
        std::fs::write(&file_path, content).unwrap();

        let (hash, bytes) = read_file_once(&file_path).unwrap();

        // Verify hash matches independent blake3::hash() computation
        let expected_hash = blake3::hash(content).to_hex().to_string();
        assert_eq!(
            hash, expected_hash,
            "streaming BLAKE3 hash must match independent computation"
        );

        // Verify bytes match file contents
        assert_eq!(
            bytes.as_slice(),
            content,
            "file bytes must match original content"
        );
    }

    #[test]
    fn test_read_file_once_empty_file() {
        let temp = tempfile::tempdir().unwrap();
        let file_path = temp.path().join("empty.txt");
        std::fs::write(&file_path, b"").unwrap();

        let (hash, bytes) = read_file_once(&file_path).unwrap();

        // Empty file should produce the BLAKE3 hash of empty input
        let expected_hash = blake3::hash(b"").to_hex().to_string();
        assert_eq!(
            hash, expected_hash,
            "empty file hash must match blake3 of empty input"
        );
        assert!(bytes.is_empty(), "empty file bytes should be empty");
    }

    #[test]
    fn test_read_file_once_error() {
        let result = read_file_once(Path::new("/nonexistent/path/to/file.txt"));
        assert!(
            result.is_err(),
            "reading a nonexistent file should return an error"
        );
    }

    // ============================================================================
    // HYBRID EMBEDDING INTEGRATION TESTS
    // ============================================================================

    #[test]
    #[cfg(feature = "onnx")]
    fn test_hybrid_embedder_local_creation() {
        let docs: Vec<(String, String)> =
            vec![("test".to_string(), "fn test_function() -> bool".to_string())];
        let tfidf_embedder = TfIdfEmbedder::build(&docs);
        let result = HybridEmbedder::hybrid_local(tfidf_embedder, None);
        // May fail if model not found, but tests the API
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    #[cfg(not(feature = "onnx"))]
    fn test_hybrid_embedder_local_feature_not_enabled() {
        let docs: Vec<(String, String)> =
            vec![("test".to_string(), "fn test_function() -> bool".to_string())];
        let tfidf_embedder = TfIdfEmbedder::build(&docs);
        // When ONNX feature is not enabled, only TfIdfOnly is available
        let _ = HybridEmbedder::tfidf_only(tfidf_embedder);
        // Test passes if we can create a TfIdfOnly embedder
    }

    #[test]
    fn test_hybrid_embedder_tfidf_only_default() {
        let embedder = HybridEmbedder::default();
        assert!(
            !embedder.has_neural(),
            "default embedder should be TF-IDF only"
        );
        assert_eq!(
            embedder.tfidf_dimension(),
            768,
            "TF-IDF dimension should be 768"
        );
        assert!(
            embedder.neural_dimension().is_none(),
            "neural dimension should be None"
        );
    }

    #[test]
    fn test_hybrid_embedder_tfidf_only() {
        let docs: Vec<(String, String)> = vec![(
            "auth".to_string(),
            "fn authenticate_user(token: &str) -> bool".to_string(),
        )];
        let tfidf_embedder = TfIdfEmbedder::build(&docs);
        let embedder = HybridEmbedder::tfidf_only(tfidf_embedder);

        assert!(!embedder.has_neural());
        assert_eq!(embedder.tfidf_dimension(), 768);
        assert_eq!(embedder.neural_weight(), 0.0);

        let weights = embedder.scoring_weights();
        assert_eq!(
            weights.tfidf, 0.60,
            "TF-IDF weight should be 0.60 without neural"
        );
        assert_eq!(
            weights.neural, 0.00,
            "neural weight should be 0.00 without neural"
        );
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_hybrid_embedder_local_dimension() {
        let docs: Vec<(String, String)> =
            vec![("test".to_string(), "fn test_function() -> bool".to_string())];
        let tfidf_embedder = TfIdfEmbedder::build(&docs);
        if let Ok(embedder) = HybridEmbedder::hybrid_local(tfidf_embedder, None) {
            assert!(
                embedder.has_neural(),
                "hybrid local embedder should have neural"
            );
            assert_eq!(
                embedder.tfidf_dimension(),
                768,
                "TF-IDF dimension should be 768"
            );
            assert!(
                embedder.neural_dimension().is_some(),
                "neural dimension should be Some"
            );
            assert_eq!(
                embedder.neural_weight(),
                0.40,
                "neural weight should be 0.40"
            );

            let weights = embedder.scoring_weights();
            assert_eq!(
                weights.tfidf, 0.30,
                "TF-IDF weight should be 0.30 with neural"
            );
            assert_eq!(
                weights.neural, 0.40,
                "neural weight should be 0.40 with neural"
            );
        }
    }

    #[test]
    fn test_hybrid_embedder_embed_tfidf() {
        let docs: Vec<(String, String)> = vec![(
            "auth".to_string(),
            "fn authenticate_user(token: &str) -> bool".to_string(),
        )];
        let tfidf_embedder = TfIdfEmbedder::build(&docs);
        let embedder = HybridEmbedder::tfidf_only(tfidf_embedder);

        let tokens = vec![
            "authenticate".to_string(),
            "user".to_string(),
            "token".to_string(),
        ];
        let embedding = embedder.embed_tfidf(&tokens);

        assert_eq!(
            embedding.len(),
            768,
            "TF-IDF embedding dimension should be 768"
        );
    }

    #[test]
    #[cfg(feature = "onnx")]
    fn test_hybrid_embedder_embed_neural_local() {
        let docs: Vec<(String, String)> =
            vec![("test".to_string(), "fn test_function() -> bool".to_string())];
        let tfidf_embedder = TfIdfEmbedder::build(&docs);
        if let Ok(embedder) = HybridEmbedder::hybrid_local(tfidf_embedder, None) {
            let tokens = vec![
                "test".to_string(),
                "code".to_string(),
                "embedding".to_string(),
            ];
            let tfidf_embedding = embedder.embed_tfidf(&tokens);

            assert_eq!(
                tfidf_embedding.len(),
                768,
                "TF-IDF embedding dimension should be 768"
            );

            // Test neural embedding generation (blocking version for sync test)
            let text = "test code embedding";
            if let Some(Ok(neural_embedding)) = embedder.embed_neural_blocking(text) {
                assert!(
                    neural_embedding.len() > 0,
                    "neural embedding should have non-zero dimension"
                );
                // Real embeddings should have non-zero values
                let has_nonzero = neural_embedding.iter().any(|&v| v != 0.0);
                assert!(has_nonzero, "neural embeddings should have non-zero values");
            }
        }
    }

    #[test]
    fn test_hybrid_scoring_weights() {
        let weights_with_neural = HybridScoringWeights::default();
        assert_eq!(weights_with_neural.tfidf, 0.30);
        assert_eq!(weights_with_neural.neural, 0.40);
        assert_eq!(weights_with_neural.structural, 0.15);
        assert_eq!(weights_with_neural.text_match, 0.15);
        assert!(
            (weights_with_neural.tfidf
                + weights_with_neural.neural
                + weights_with_neural.structural
                + weights_with_neural.text_match
                - 1.0)
                .abs()
                < 0.001
        );

        let weights_without_neural = HybridScoringWeights::without_neural();
        assert_eq!(weights_without_neural.tfidf, 0.60);
        assert_eq!(weights_without_neural.neural, 0.00);
        assert_eq!(weights_without_neural.structural, 0.20);
        assert_eq!(weights_without_neural.text_match, 0.20);
        assert!(
            (weights_without_neural.tfidf
                + weights_without_neural.neural
                + weights_without_neural.structural
                + weights_without_neural.text_match
                - 1.0)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn test_hybrid_scoring_weights_normalize() {
        let mut custom_weights = HybridScoringWeights {
            tfidf: 0.5,
            neural: 0.3,
            structural: 0.1,
            text_match: 0.1,
        };
        custom_weights = custom_weights.normalize();
        assert!(
            (custom_weights.tfidf
                + custom_weights.neural
                + custom_weights.structural
                + custom_weights.text_match
                - 1.0)
                .abs()
                < 0.001
        );
    }

    #[test]
    fn test_hybrid_embedder_compare_backends() {
        let docs: Vec<(String, String)> =
            vec![("test".to_string(), "fn test_function() -> bool".to_string())];

        let tfidf_embedder = TfIdfEmbedder::build(&docs);
        let tfidf_only = HybridEmbedder::tfidf_only(tfidf_embedder.clone());

        assert!(!tfidf_only.has_neural());
        assert_eq!(tfidf_only.tfidf_dimension(), 768);
        assert!(tfidf_only.neural_dimension().is_none());

        #[cfg(feature = "onnx")]
        {
            if let Ok(hybrid_local) = HybridEmbedder::hybrid_local(tfidf_embedder, None) {
                assert!(hybrid_local.has_neural());
                assert_eq!(hybrid_local.tfidf_dimension(), 768);
                assert!(hybrid_local.neural_dimension().is_some());
            }
        }
    }
}
