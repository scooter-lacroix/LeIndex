# Hybrid Embedding System Design

## Current Architecture Problems

### Problem 1: Mutually Exclusive Backends
The current implementation has two separate "unified" systems that are actually mutually exclusive:

**`UnifiedEmbedder` (src/cli/index_builder.rs):**
```rust
pub enum UnifiedEmbedder {
    TfIdf(TfIdfEmbedder),
    #[cfg(feature = "onnx")]
    Neural(QwenEmbeddingProvider),
}
```

**`EmbeddingBackend` (src/search/onnx/mod.rs):**
```rust
pub enum EmbeddingBackend {
    TfIdf,
    #[cfg(feature = "onnx")]
    Neural(QwenEmbeddingProvider),
    #[cfg(feature = "remote-embeddings")]
    Remote(GenericRemoteProvider),
}
```

Both systems require choosing ONE backend: TF-IDF OR Neural OR Remote. This does NOT meet the requirement for a hybrid system.

### Problem 2: No Hybrid Scoring
The current scoring system (`HybridScorer`) combines semantic, structural, and text_match signals, but:
- `semantic` comes from neural embeddings (if available)
- `text_match` comes from substring/token matching (NOT TF-IDF vectors)
- TF-IDF is used for generating embeddings but not combined with neural embeddings in scoring

### Problem 3: Remote Embeddings Not Integrated
Remote embeddings are only in `EmbeddingBackend`, not in `UnifiedEmbedder` which is actually used in the indexing pipeline. This creates a disconnect.

## Required Architecture

### Requirement: Always Use TF-IDF as Base Signal
- TF-IDF should always be computed and available
- TF-IDF vectors provide keyword-based semantic understanding
- Works offline, no API costs, fast

### Requirement: Neural/Remote as Enhancement Layers
- Neural embeddings (local ONNX or remote API) are optional enhancements
- When available, they add semantic understanding beyond keywords
- Should be combined with TF-IDF, not replace it

### Requirement: Hybrid Scoring
- Combine TF-IDF similarity + neural similarity in final score
- Weighted combination based on query type and availability
- Fallback gracefully when neural embeddings unavailable

### Requirement: Single Unified System
- One embedder type, one configuration, one pipeline
- Remote embeddings have feature parity with local embeddings
- No mutual exclusivity

## Proposed Architecture

### HybridEmbedder Structure

```rust
pub enum HybridEmbedder {
    /// TF-IDF only (base signal)
    TfIdfOnly(TfIdfEmbedder),

    /// TF-IDF + Local ONNX neural embeddings
    HybridLocal {
        tfidf: TfIdfEmbedder,
        neural: QwenEmbeddingProvider,
        neural_weight: f32,  // Weight for neural signal in scoring
    },

    /// TF-IDF + Remote embeddings (OpenAI, Cohere, custom)
    HybridRemote {
        tfidf: TfIdfEmbedder,
        remote: GenericRemoteProvider,
        remote_weight: f32,  // Weight for remote signal in scoring
    },
}
```

### Hybrid Scoring

```rust
pub struct HybridScore {
    /// Overall combined score
    pub overall: f32,

    /// TF-IDF similarity score (0-1)
    pub tfidf_score: f32,

    /// Neural/remote similarity score (0-1, or 0 if unavailable)
    pub neural_score: f32,

    /// Structural relevance (from PDG)
    pub structural_score: f32,

    /// Exact text match (substring)
    pub text_match_score: f32,
}
```

**Scoring Formula:**
```
overall = (tfidf_weight * tfidf_score)
       + (neural_weight * neural_score)
       + (structural_weight * structural_score)
       + (text_weight * text_match_score)

Default weights:
- tfidf_weight: 0.30
- neural_weight: 0.40 (if available, else redistributed)
- structural_weight: 0.15
- text_weight: 0.15

When neural unavailable:
- tfidf_weight: 0.60
- neural_weight: 0.00
- structural_weight: 0.20
- text_weight: 0.20
```

### Embedding Generation

**Indexing Phase:**
1. Always generate TF-IDF embedding for each node
2. If neural/remote available, also generate neural embedding
3. Store both embeddings separately in NodeInfo
4. Mark which embeddings are available

```rust
pub struct NodeInfo {
    pub node_id: String,
    pub content: String,
    pub tfidf_embedding: Vec<f32>,  // Always present
    pub neural_embedding: Option<Vec<f32>>,  // Optional enhancement
    // ... other fields
}
```

**Query Phase:**
1. Always generate TF-IDF embedding for query
2. If neural/remote available, generate neural embedding for query
3. Search both indexes (TF-IDF vector index, neural vector index)
4. Combine scores using hybrid scoring

### Configuration

```rust
pub struct HybridEmbeddingConfig {
    /// Always use TF-IDF
    pub use_tfidf: bool,  // Always true

    /// Optional neural enhancement
    pub neural_provider: Option<NeuralProviderConfig>,

    /// Optional remote enhancement
    pub remote_provider: Option<RemoteEmbeddingConfig>,

    /// Scoring weights
    pub scoring_weights: ScoringWeights,
}

pub struct NeuralProviderConfig {
    pub provider_type: NeuralProviderType,  // LocalONNX
    pub model_path: Option<String>,
    pub use_reranker: bool,
}

pub enum NeuralProviderType {
    LocalONNX,  // Default
    // Future: LocalGGUF, etc.
}

pub struct ScoringWeights {
    pub tfidf: f32,
    pub neural: f32,
    pub structural: f32,
    pub text: f32,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            tfidf: 0.30,
            neural: 0.40,
            structural: 0.15,
            text: 0.15,
        }
    }
}
```

### Feature Parity: Local vs Remote

Both local ONNX and remote embeddings must support:

1. **Single text embedding**
   - `embed(text: &str) -> Vec<f32>`

2. **Batch embedding**
   - `embed_batch(texts: Vec<&str>) -> Vec<Vec<f32>>`

3. **Dimensionality**
   - `dimension() -> usize`

4. **Error handling**
   - Consistent error types
   - Graceful fallback to TF-IDF on failure

5. **Async interface**
   - Remote providers are async
   - Local providers should also support async interface for consistency

### Migration Path

1. **Step 1:** Create `HybridEmbedder` in `src/cli/index_builder.rs`
2. **Step 2:** Move `RemoteEmbeddingConfig` and related types to `src/search/onnx/mod.rs`
3. **Step 3:** Implement hybrid scoring in `src/search/ranking.rs`
4. **Step 4:** Update `NodeInfo` to store separate TF-IDF and neural embeddings
5. **Step 5:** Update indexing pipeline to generate both embeddings
6. **Step 6:** Update search pipeline to use hybrid scoring
7. **Step 7:** Update CLI configuration for hybrid system
8. **Step 8:** Remove old mutually exclusive enums
9. **Step 9:** Add comprehensive tests
10. **Step 10:** Update documentation

## Benefits

1. **Always Works:** TF-IDF ensures baseline functionality even without neural models
2. **Enhanced When Available:** Neural embeddings provide semantic boost when available
3. **Flexible Configuration:** Users can choose local or remote neural providers
4. **Graceful Degradation:** Falls back to TF-IDF if neural fails
5. **Single System:** One embedder type, one configuration, clear semantics
6. **Feature Parity:** Local and remote embeddings have identical capabilities

## Implementation Order

1. Design complete (this document) ✓
2. Implement `HybridEmbedder` structure
3. Implement hybrid scoring algorithm
4. Add remote embeddings to hybrid embedder
5. Update NodeInfo for dual embeddings
6. Update indexing pipeline
7. Update search/query pipeline
8. Update CLI configuration
9. Write tests
10. Build and verify
11. Update documentation (README, changelog)
