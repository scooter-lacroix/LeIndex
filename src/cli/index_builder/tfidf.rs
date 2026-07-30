//! TF-IDF embedder: persisted state, vocabulary building, and embedding.

use super::*;

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
    pub(crate) vocab: Vec<String>,
    /// IDF values indexed by vocab position
    pub(crate) idf: Vec<f32>,
    /// Embedding dimension (matches existing vector index: 768)
    pub(crate) dimension: usize,
    /// PDG node count captured when persisted for staleness checks
    pub(crate) pdg_nodes: usize,
    /// PDG edge count captured when persisted for staleness checks
    pub(crate) pdg_edges: usize,
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

        // Compute TF-IDF in lockstep over the output vector so a mismatched
        // persisted vocabulary/dimension can't index out of bounds.
        for (slot, (word, idf_val)) in vec.iter_mut().zip(self.vocab.iter().zip(self.idf.iter())) {
            if let Some(&count) = tf_map.get(word.as_str()) {
                *slot = (count / total) * idf_val;
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

        for (slot, (word, idf_val)) in vec.iter_mut().zip(self.vocab.iter().zip(self.idf.iter())) {
            if let Some(&count) = tf_map.get(word.as_str()) {
                *slot = (count / total) * idf_val;
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
        Self::load_from_artifact_path(&project_path.join(".leindex"))
    }

    /// Load a persisted TF-IDF embedder from an explicit storage directory.
    ///
    /// Generation hydration uses this path so an interrupted write to the
    /// mutable root cannot change the embedder selected by `CURRENT`.
    pub(crate) fn load_from_artifact_path(storage_path: &Path) -> Result<Option<Self>> {
        let path = storage_path.join("tfidf_embedder.bin");
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
