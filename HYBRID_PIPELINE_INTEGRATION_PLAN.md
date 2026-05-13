# Hybrid Pipeline Integration Plan

## Overview

This plan details the integration of the new hybrid embedding system into the existing LeIndex pipelines. The core hybrid embedding system is implemented (`HybridEmbedder`, `Score`, `HybridScorer`) and now needs to be integrated into:

1. **Indexing Pipeline** - Generate and store hybrid embeddings during indexing
2. **Search/Query Pipeline** - Use hybrid scoring for search results
3. **CLI Configuration** - Environment variables and CLI flags for hybrid configuration
4. **API Migration** - Update deprecated method calls
5. **Testing** - Comprehensive integration tests

## Current State

### ✅ Completed
- `HybridEmbedder` with TF-IDF always present, neural/remote as enhancements
- `HybridScorer` with 4-component weighting (tfidf, neural, structural, text)
- Backward-compatible deprecated legacy methods
- Core system compiles successfully
- Unit tests for hybrid embedder and scoring

### ⚠️ Depr

ecation Warnings (9 total)
- `src/search/search.rs`: 6 deprecated API calls
- `src/phase/phase4.rs`: 2 deprecated API calls
- `src/cli/cli.rs`: 1 field access update (already fixed)

### 🔜 Integration Points
- Indexing pipeline: `src/cli/index_builder.rs`
- Search pipeline: `src/search/search.rs`
- CLI configuration: `src/cli/config.rs`, `src/cli/cli.rs`
- Phase analysis: `src/phase/phase4.rs`

---

## Phase 1: Indexing Pipeline Integration

### Objective
Update the indexing pipeline to generate and store hybrid embeddings (TF-IDF + optional neural/remote) for all indexed nodes.

### Files to Modify
- `src/cli/index_builder.rs` - Main indexing logic
- `src/search/search.rs` - Update `NodeInfo` structure to support dual embeddings
- `src/storage/schema.rs` - Update database schema if needed

### Step 1.1: Update NodeInfo Structure

**File:** `src/search/search.rs`

**Current:**
```rust
pub struct NodeInfo {
    pub embedding: Option<Vec<f32>>,  // Single embedding
    // ... other fields
}
```

**Target:**
```rust
pub struct NodeInfo {
    /// TF-IDF embedding (always present, 768-dim)
    pub tfidf_embedding: Vec<f32>,
    /// Neural/remote embedding (optional enhancement)
    pub neural_embedding: Option<Vec<f32>>,
    /// Legacy field for backward compatibility (points to tfidf_embedding)
    pub embedding: Option<Vec<f32>>,
    // ... other fields
}
```

**Rationale:**
- TF-IDF always present as `tfidf_embedding`
- Neural/remote optional as `neural_embedding`
- Keep legacy `embedding` field pointing to tfidf for backward compatibility during migration

**Migration Strategy:**
1. Add new fields
2. Update all field access in indexing pipeline
3. Update all field access in search pipeline
4. Keep legacy field temporarily for compatibility
5. Remove legacy field in future breaking change

### Step 1.2: Update Indexing Pipeline to Use HybridEmbedder

**File:** `src/cli/index_builder.rs`

**Current Usage:**
```rust
// Uses UnifiedEmbedder (now replaced with HybridEmbedder)
let embedder = UnifiedEmbedder::tfidf(tfidf_embedder);
let embedding = embedder.embed_tokens(&tokens);
```

**Target Usage:**
```rust
// Always use HybridEmbedder
let embedder = HybridEmbedder::tfidf_only(tfidf_embedder);

// Or with neural enhancement
let embedder = HybridEmbedder::hybrid_local(tfidf_embedder, Some(0.40))?;

// Generate TF-IDF embedding (always available)
let tfidf_embedding = embedder.embed_tfidf(&tokens);

// Generate neural embedding (if available)
let neural_embedding = if embedder.has_neural() {
    let text = &node.content;
    embedder.embed_neural_blocking(text).ok().flatten()
} else {
    None
};

// Store both embeddings
node.tfidf_embedding = tfidf_embedding;
node.neural_embedding = neural_embedding;
```

**Implementation Tasks:**
1. Find all `UnifiedEmbedder` references in indexing pipeline
2. Replace with `HybridEmbedder::tfidf_only()` for default behavior
3. Add logic to generate neural embeddings when available
4. Update NodeInfo construction to set both embedding fields
5. Update vector index insertion to handle dual embeddings

**Search for References:**
```bash
grep -r "UnifiedEmbedder" src/cli/
```

### Step 1.3: Update Vector Index Insertion

**File:** `src/search/search.rs`

**Current:**
```rust
if let Some(embedding) = &node.embedding {
    self.vector_index.insert(node.node_id.clone(), embedding.clone())?;
}
```

**Target:**
```rust
// Insert TF-IDF embedding (always present)
self.vector_index.insert(node.node_id.clone(), node.tfidf_embedding.clone())?;

// Insert neural embedding (if available) into separate index
if let Some(neural_emb) = &node.neural_embedding {
    // TODO: Need dual vector index support
    // For now, we'll use neural_embedding as the primary if available
    // This is a temporary workaround until dual-index is implemented
}
```

**Note:** Full dual-vector-index support is a larger change. For this phase, we can:
- Use TF-IDF as primary vector index
- Use neural as enhancement in scoring (not separate index)
- Implement full dual-index in Phase 2

### Step 1.4: Update Indexing Builder Configuration

**File:** `src/cli/index_builder.rs`

**Add configuration for hybrid embedder:**
```rust
pub struct IndexingConfig {
    pub use_neural: bool,
    pub neural_provider_type: NeuralProviderType,
    pub remote_config: Option<RemoteEmbeddingConfig>,
    pub neural_weight: f32,
}

pub enum NeuralProviderType {
    LocalONNX,
    Remote,
}
```

**Integration:**
- Read environment variables (e.g., `LEINDEX_USE_NEURAL`, `LEINDEX_NEURAL_PROVIDER`)
- Pass config to indexing pipeline
- Create appropriate `HybridEmbedder` variant based on config

### Verification Criteria
- [ ] All indexing tests pass with TF-IDF only
- [ ] Indexing works with local ONNX neural embeddings (if feature enabled)
- [ ] Indexing works with remote embeddings (if feature enabled)
- [ ] Generated embeddings have correct dimensions
- [ ] NodeInfo contains both tfidf_embedding and neural_embedding fields
- [ ] Vector index insertion succeeds

---

## Phase 2: Search/Query Pipeline Integration

### Objective
Update the search pipeline to use hybrid scoring that combines TF-IDF and neural similarity scores.

### Files to Modify
- `src/search/search.rs` - Main search logic
- `src/cli/query.rs` - Query parsing and execution
- `src/cli/leindex/query.rs` - LeIndex-specific query handling

### Step 2.1: Update Search Query Structure

**File:** `src/search/search.rs`

**Current:**
```rust
pub struct SearchQuery {
    pub semantic: bool,
    pub query_embedding: Option<Vec<f32>>,  // Single query embedding
    // ... other fields
}
```

**Target:**
```rust
pub struct SearchQuery {
    pub semantic: bool,
    /// TF-IDF query embedding (always generated)
    pub tfidf_query_embedding: Option<Vec<f32>>,
    /// Neural query embedding (optional)
    pub neural_query_embedding: Option<Vec<f32>>,
    /// Legacy field for backward compatibility
    pub query_embedding: Option<Vec<f32>>,
    // ... other fields
}
```

### Step 2.2: Update Query Embedding Generation

**File:** `src/search/search.rs` (search method)

**Current:**
```rust
let embedding = if let Some(emb) = query.query_embedding {
    Some(emb)
} else {
    // Fallback to finding an embedding from indexed nodes
    self.nodes.iter().find_map(|n| n.embedding.as_ref()).cloned()
};
```

**Target:**
```rust
// Always generate TF-IDF query embedding
let tfidf_embedding = if let Some(emb) = query.tfidf_query_embedding {
    Some(emb)
} else {
    // Fallback: compute TF-IDF from query text
    // This requires access to TF-IDF embedder - TODO: pass embedder to search
    None
};

// Neural query embedding (if available)
let neural_embedding = query.neural_query_embedding;
```

**Dependency:** Need to pass `HybridEmbedder` or TF-IDF embedder to search engine for query embedding generation.

### Step 2.3: Update Search Scoring to Use Hybrid Scores

**File:** `src/search/search.rs` (search method)

**Current Deprecated Calls:**
```rust
// Line 901
self.scorer.with_weights(0.2, 0.05, 0.75).score(
    semantic_score, structural_score, text_score
)

// Line 909
self.scorer.with_weights(0.7, 0.1, 0.2).score(
    semantic_score, structural_score, text_score
)

// Line 917
self.scorer.with_weights(0.3, 0.5, 0.2).score(
    semantic_score, structural_score, text_score
)

// Line 926
.score(semantic_score, structural_score, text_score)
```

**Target New Calls:**
```rust
// Use hybrid scoring with TF-IDF and neural components
let tfidf_score = compute_tfidf_similarity(&query_text, &node.content);
let neural_score = if let (Some(query_emb), Some(node_emb)) = (&neural_embedding, &node.neural_embedding) {
    cosine_similarity(query_emb, node_emb)
} else {
    0.0
};

self.scorer.score_hybrid(
    tfidf_score,
    neural_score,
    structural_score,
    text_score
)
```

**Implementation:**
1. Replace all 6 deprecated calls with `score_hybrid()`
2. Compute TF-IDF similarity from node embeddings and query
3. Compute neural similarity from neural embeddings (if available)
4. Pass both scores to `score_hybrid()`

### Step 2.4: Update CLI Query Construction

**File:** `src/cli/leindex/query.rs` or `src/cli/query.rs`

**Current:**
```rust
let query = SearchQuery {
    semantic: true,
    query_embedding: None,  // Generated later
    // ...
};
```

**Target:**
```rust
let query = SearchQuery {
    semantic: true,
    tfidf_query_embedding: Some(compute_tfidf_embedding(&query_text)),
    neural_query_embedding: if embedder.has_neural() {
        embedder.embed_neural_blocking(&query_text).ok().flatten()
    } else {
        None
    },
    // ...
};
```

**Dependency:** Need access to `HybridEmbedder` in query construction.

### Step 2.5: Update Phase Analysis Scoring

**File:** `src/phase/phase4.rs`

**Current Deprecated Call:**
```rust
// Line 29
let scorer = HybridScorer::new().with_weights(0.45, 0.45, 0.10);

// Line 71
.score(complexity_signal, impact_signal, text_signal)
```

**Target New Call:**
```rust
let scorer = HybridScorer::new().with_weights_hybrid(0.45, 0.45, 0.10, 0.10);

.score_hybrid(complexity_signal, impact_signal, structural_signal, text_signal)
```

**Note:** Phase 4 scoring may need different weights for its specific use case. Preserve the intent while migrating to new API.

### Verification Criteria
- [ ] All deprecated API calls replaced with new hybrid methods
- [ ] Search results include tfidf_score and neural_score in output
- [ ] TF-IDF similarity is computed and used in scoring
- [ ] Neural similarity is computed when available
- [ ] Fallback to TF-IDF-only scoring works when neural unavailable
- [ ] Phase 4 analysis uses new hybrid scoring API
- [ ] All search tests pass

---

## Phase 3: CLI Configuration

### Objective
Add environment variables and CLI flags for configuring the hybrid embedding system.

### Files to Modify
- `src/cli/config.rs` - Configuration structures
- `src/cli/cli.rs` - CLI argument parsing
- `README.md` - Documentation

### Step 3.1: Add Environment Variables

**New Environment Variables:**
```bash
# Neural embedding configuration
LEINDEX_USE_NEURAL=true|false              # Enable neural enhancement (default: false)
LEINDEX_NEURAL_PROVIDER=local|remote      # Provider type (default: local if onnx feature)
LEINDEX_NEURAL_WEIGHT=0.0-1.0             # Neural scoring weight (default: 0.40)
LEINDEX_NEURAL_MODEL_PATH=/path/to/model  # Local model path (for local provider)
LEINDEX_NEURAL_DIMENSION=768|1024|1536    # Neural embedding dimension

# Remote provider configuration
LEINDEX_REMOTE_API_KEY=sk-xxx             # API key for remote provider
LEINDEX_REMOTE_PROVIDER=openai|cohere     # Remote provider type
LEINDEX_REMOTE_MODEL=text-embedding-3-small  # Model name
LEINDEX_REMOTE_BASE_URL=https://api.example.com/v1  # Custom endpoint
LEINDEX_REMOTE_TIMEOUT=30                  # Request timeout in seconds
```

### Step 3.2: Add Configuration Struct

**File:** `src/cli/config.rs`

```rust
#[derive(Debug, Clone)]
pub struct HybridEmbeddingConfig {
    pub use_neural: bool,
    pub provider_type: NeuralProviderType,
    pub neural_weight: f32,
    pub local_config: Option<LocalONNXConfig>,
    pub remote_config: Option<RemoteEmbeddingConfig>,
}

#[derive(Debug, Clone)]
pub enum NeuralProviderType {
    LocalONNX,
    Remote,
}

#[derive(Debug, Clone)]
pub struct LocalONNXConfig {
    pub model_path: Option<PathBuf>,
    pub use_reranker: bool,
}

impl Default for HybridEmbeddingConfig {
    fn default() -> Self {
        Self {
            use_neural: false,
            provider_type: NeuralProviderType::LocalONNX,
            neural_weight: 0.40,
            local_config: None,
            remote_config: None,
        }
    }
}

impl HybridEmbeddingConfig {
    pub fn from_env() -> Self {
        // Read environment variables
        let use_neural = env::var("LEINDEX_USE_NEURAL")
            .map(|s| s == "true" || s == "1")
            .unwrap_or(false);

        let provider_type = if use_neural {
            env::var("LEINDEX_NEURAL_PROVIDER")
                .map(|s| match s.as_str() {
                    "remote" => NeuralProviderType::Remote,
                    _ => NeuralProviderType::LocalONNX,
                })
                .unwrap_or(NeuralProviderType::LocalONNX)
        } else {
            NeuralProviderType::LocalONNX
        };

        let neural_weight = env::var("LEINDEX_NEURAL_WEIGHT")
            .ok()
            .and_then(|s| s.parse::<f32>().ok())
            .unwrap_or(0.40);

        // ... read other configs
        Self { /* ... */ }
    }
}
```

### Step 3.3: Add CLI Flags

**File:** `src/cli/cli.rs`

**New CLI Arguments:**
```rust
// Embedding configuration
.arg(Arg::new("use-neural")
    .long("use-neural")
    .help("Enable neural embedding enhancement")
    .action(ArgAction::SetTrue))

.arg(Arg::new("neural-provider")
    .long("neural-provider")
    .value_parser(["local", "remote"])
    .help("Neural embedding provider (local or remote)"))

.arg(Arg::new("neural-weight")
    .long("neural-weight")
    .value_parser(value_parser!(f32))
    .help("Weight for neural embeddings in scoring (0.0-1.0)"))

.arg(Arg::new("remote-api-key")
    .long("remote-api-key")
    .help("API key for remote embedding provider"))

.arg(Arg::new("remote-provider")
    .long("remote-provider")
    .value_parser(["openai", "cohere"])
    .help("Remote embedding provider"))
```

### Step 3.4: Update Help Text

**Add to CLI help:**
```
Embedding Options:
    --use-neural                    Enable neural embedding enhancement
    --neural-provider <local|remote>  Neural provider type
    --neural-weight <0.0-1.0>        Neural scoring weight
    --remote-api-key <KEY>          API key for remote provider
    --remote-provider <openai|cohere>  Remote provider type
```

### Verification Criteria
- [ ] Environment variables are read correctly
- [ ] CLI flags override environment variables
- [ ] Invalid configuration values are rejected
- [ ] Configuration is passed to indexing pipeline
- [ ] Help text displays new options

---

## Phase 4: Testing

### Objective
Write comprehensive integration tests for the hybrid embedding system.

### Files to Create/Modify
- `src/cli/index_builder.rs` - Add integration tests
- `src/search/search.rs` - Add integration tests
- `tests/integration_hybrid_embeddings.rs` - New test file

### Step 4.1: Unit Tests

**Add tests for:**
- `HybridEmbedder::tfidf_only()` basic functionality
- `HybridEmbedder::hybrid_local()` with ONNX (if feature enabled)
- `HybridEmbedder::hybrid_remote()` with remote mocking
- `HybridScorer::score_hybrid()` with various weight combinations
- `Score::new_hybrid()` construction

### Step 4.2: Integration Tests

**Test Scenarios:**

1. **TF-IDF Only Mode**
   - Index a small project with TF-IDF only
   - Search and verify TF-IDF scores are used
   - Verify neural scores are 0.0

2. **Hybrid Local Mode**
   - Index with local ONNX neural embeddings
   - Verify both embeddings are generated
   - Search and verify hybrid scoring works
   - Verify weight configuration affects scoring

3. **Hybrid Remote Mode**
   - Index with remote embeddings (mock API)
   - Verify API calls are made correctly
   - Verify error handling for API failures
   - Verify fallback to TF-IDF on errors

4. **Configuration Tests**
   - Test environment variable parsing
   - Test CLI flag parsing
   - Test invalid configuration handling

5. **Migration Compatibility**
   - Test that old `UnifiedEmbedder` usage still works (if any remains)
   - Test that deprecated methods still work
   - Verify backward compatibility

### Step 4.3: End-to-End Tests

**Test with actual codebase:**
```rust
#[tokio::test]
async fn test_hybrid_embedding_e2e() {
    // 1. Create temporary project with sample code
    // 2. Index with hybrid embeddings
    // 3. Search for queries
    // 4. Verify results include both tfidf and neural scores
    // 5. Verify ranking is sensible
}
```

### Verification Criteria
- [ ] All unit tests pass
- [ ] All integration tests pass
- [ ] Test coverage > 80% for new hybrid code
- [ ] Mock remote API tests pass
- [ ] Error handling tests pass

---

## Phase 5: Documentation Updates

### Files to Update
- `README.md` - Update embedding configuration section
- `CHANGELOG.md` - Add entry for hybrid embedding system
- `docs/MCP.md` - Update MCP tool documentation
- `HYBRID_EMBEDDING_DESIGN.md` - Mark implementation steps complete

### Step 5.1: Update README

**Current section:**
```markdown
## Embedding Configuration

LeIndex supports multiple embedding backends for semantic search:
```

**Update to:**
```markdown
## Hybrid Embedding System

LeIndex uses a hybrid embedding system that always uses TF-IDF as the base signal,
with optional neural/remote embeddings as enhancement layers.

### Default Behavior (TF-IDF Only)

By default, LeIndex uses TF-IDF embeddings:
- Fast, works offline, no API costs
- Keyword-based semantic understanding
- 768-dimensional vectors
- Good for code symbol search

### With Neural Enhancement (Local)

Build with the `onnx` feature to enable local neural embeddings:
```bash
cargo build --release --features onnx
```

Enable neural embeddings:
```bash
export LEINDEX_USE_NEURAL=true
```

This adds:
- Deep semantic understanding beyond keywords
- Cross-language code understanding
- Qwen3-Embedding-0.6B model via ONNX Runtime
- Optional Qwen3-Reranker-0.6B for result refinement

### With Neural Enhancement (Remote)

Build with the `remote-embeddings` feature to enable cloud embeddings:
```bash
cargo build --release --features remote-embeddings
```

Configure remote provider:
```bash
export LEINDEX_USE_NEURAL=true
export LEINDEX_NEURAL_PROVIDER=remote
export LEINDEX_REMOTE_PROVIDER=openai
export LEINDEX_REMOTE_API_KEY=sk-xxx
```

Supported providers:
- **OpenAI** (`text-embedding-3-small`, `text-embedding-3-large`)
- **Cohere** (`embed-english-v3.0`, `embed-multilingual-v3.0`)
- **Custom** (any OpenAI-compatible endpoint)
```

### Step 5.2: Update CHANGELOG

**Add entry:**
```markdown
## [1.6.4] - 2026-05-12

### 🔄 Breaking Changes

**Hybrid Embedding System**: Replaced mutually exclusive embedding backends with unified hybrid system:
- TF-IDF is now always the base signal (previously optional)
- Neural/remote embeddings are enhancement layers (not replacements)
- `Score` struct now has separate `tfidf` and `neural` components
- `HybridScorer` updated to 4-component scoring (tfidf, neural, structural, text)
- Legacy `UnifiedEmbedder` replaced with `HybridEmbedder`
- Legacy `EmbeddingBackend` and `EmbeddingConfig` removed

**Migration Guide:**
- Update indexing pipeline to use `HybridEmbedder::tfidf_only()` or `hybrid_local()`
- Update scoring calls to use `score_hybrid()` instead of deprecated `score()`
- Update CLI configuration to use new environment variables
- See `HYBRID_PIPELINE_INTEGRATION_PLAN.md` for detailed migration steps
```

### Verification Criteria
- [ ] README updated with hybrid embedding section
- [ ] CHANGELOG includes breaking changes and migration guide
- [ ] Documentation is clear and accurate
- [ ] Examples are tested and work correctly

---

## Phase 6: Build and Verification

### Step 6.1: Full Build

```bash
# Build with all features
cargo build --release --all-features

# Build with minimal features
cargo build --release --no-default-features

# Build with onnx only
cargo build --release --features onnx

# Build with remote-embeddings only
cargo build --release --features remote-embeddings

# Run tests
cargo test --all-features
```

### Step 6.2: Linting

```bash
cargo clippy --all-features
cargo fmt --check
```

### Step 6.3: Manual Testing

**Test Scenarios:**
1. Index a real project with TF-IDF only
2. Index with local ONNX (if models available)
3. Index with remote provider (if API key available)
4. Search and verify scoring
5. Check CLI help text
6. Verify environment variable configuration

### Verification Criteria
- [ ] All feature combinations build successfully
- [ ] No compilation errors
- [ ] No new warnings (except expected deprecation warnings during transition)
- [ ] All tests pass
- [ ] Clippy passes
- [ ] Manual testing successful

---

## Implementation Order

**Recommended Sequence:**
1. Phase 1 (Indexing Pipeline) - Foundation for everything else
2. Phase 2 (Search Pipeline) - Core search functionality
3. Phase 3 (CLI Configuration) - User-facing configuration
4. Phase 4 (Testing) - Verify each phase works
5. Phase 5 (Documentation) - Update docs after implementation is stable
6. Phase 6 (Build & Verification) - Final verification

**Parallel Work Opportunities:**
- Phase 3 (CLI Configuration) can be done in parallel with Phase 2
- Phase 4.1 (Unit Tests) can be done alongside each implementation phase
- Documentation updates can be done incrementally

---

## Risk Mitigation

### Risk 1: Breaking Existing Functionality
**Mitigation:**
- Keep legacy methods deprecated but functional
- Add extensive tests for backward compatibility
- Feature flag new hybrid behavior if needed
- Gradual migration path with clear documentation

### Risk 2: Performance Regression
**Mitigation:**
- Benchmark before and after changes
- Profile neural embedding generation
- Add caching for neural embeddings
- Make neural generation optional and configurable

### Risk 3: Remote API Failures
**Mitigation:**
- Graceful fallback to TF-IDF on errors
- Retry logic with exponential backoff
- Timeout configuration
- Error logging and user feedback

### Risk 4: Dimension Mismatches
**Mitigation:**
- Validate dimensions at indexing time
- Clear error messages for dimension mismatches
- Automatic dimension normalization or projection
- Configuration validation

---

## Success Criteria

The integration is complete when:
- [x] Core hybrid embedding system implemented
- [ ] Indexing pipeline uses hybrid embeddings
- [ ] Search pipeline uses hybrid scoring
- [ ] CLI configuration supports hybrid system
- [ ] All deprecated API calls replaced
- [ ] Comprehensive tests pass
- [ ] Full build succeeds for all feature combinations
- [ ] Documentation updated
- [ ] Manual testing successful
- [ ] No breaking changes for existing users (backward compatible)

---

## Time Estimates

| Phase | Estimated Time | Dependencies |
|-------|----------------|--------------|
| Phase 1: Indexing Pipeline | 4-6 hours | Core system ✅ |
| Phase 2: Search Pipeline | 3-4 hours | Phase 1 |
| Phase 3: CLI Configuration | 2-3 hours | None (parallel) |
| Phase 4: Testing | 4-6 hours | Phases 1-3 |
| Phase 5: Documentation | 1-2 hours | Phases 1-4 |
| Phase 6: Build & Verification | 2-3 hours | All phases |
| **Total** | **16-24 hours** | |

---

## Rollback Plan

If issues arise during integration:

1. **Revert to Legacy System:**
   - Keep deprecated methods functional
   - Add feature flag to disable hybrid behavior
   - `--use-legacy-embeddings` flag

2. **Partial Rollback:**
   - Keep hybrid indexing but use legacy scoring
   - Keep legacy indexing but use hybrid scoring
   - Roll back specific phases independently

3. **Emergency Rollback:**
   - Git revert to commit before integration
   - Fix issues in isolated branch
   - Re-integrate after fixes

---

## Next Steps

1. **Review this plan** with the team/user
2. **Approve implementation order** and prioritize phases
3. **Begin with Phase 1** (Indexing Pipeline Integration)
4. **Commit each phase** separately for easier rollback
5. **Update this plan** as implementation progresses

---

**Document Status:** 📋 Planning
**Last Updated:** 2026-05-12
**Next Review:** After Phase 1 completion
