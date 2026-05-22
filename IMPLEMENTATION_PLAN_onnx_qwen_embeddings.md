# Implementation Plan: TRUE Neural Embeddings with ONNX (Qwen3)

**PR Reference:** Extension of unified-crate (post b0509b1)  
**Feature:** R15 - True Neural Embeddings with Cross-Language Support  
**Model:** Qwen3-Embedding-0.6B (Default) + Qwen3-Reranker-0.6B  

---

## Executive Summary

### Current State
- TF-IDF embeddings are implemented and persisted (R3)
- Memory remediation complete (peaks 2-3 GB for large projects)
- Infrastructure exists for vector indexing with unified storage

### Required Addition  
- Replace TF-IDF with TRUE neural embeddings (Sub-1B parameter family)
- Opt-in, but capability demonstration **REQUIRED BEFORE RELEASE**
- Unified index: Combine TF-IDF + ONNX embeddings **NOT OPTIONAL** - must be single pipeline

### Key Models Selected
| Model | Size | Params | Use | Priority |
|-------|------|--------|-----|----------|
| **Qwen3-Embedding-0.6B** | ~300-600MB | 0.6B | DEFAULT for neural | 🔴 **REQUIRED** |
| **Qwen3-Reranker-0.6B** | ~300-600MB | 0.6B | Text quality improvement | 🔴 **REQUIRED** |
| Qwen3-Embedding-4B | ~2-4GB | 4B | Optional enhance | ⚠️ Future |
| Qwen3-Embedding-8B | ~4-8GB | 8B | Optional enhance | ⚠️ Future |
| google/embeddinggemma-300m | ~90MB | 0.3B | Alternative lighter | ⚠️ Future |

---

## Architecture Overview

### Data Flow Comparison

#### Current (TF-IDF):
```
Source Code → Tokenize → TF-IDF Matrix (768-dim) → Cosine Similarity → Results
              (code-aware tokenization)
```

#### New (ONNX Qwen3):
```
Source Code → Chunk → Qwen3-Embed (256-1024 dim context) → 
              Optional: Qwen3-Rerank → Unified Index → Results
```

### Component Diagram

```
┌─────────────────────────────────────────────────────────────┐
│                      LeIndex Pipeline                        │
├─────────────────────────────────────────────────────────────┤
│                                                             │
│  ┌─────────────┐         ┌─────────────────────────────────┐│
│  │ Source Code │         │ Preprocessing (Unified)          ││
│  └──────┬──────┘         │ - Language detection             ││
│         │                 │ - Chunking at AST boundaries     ││
│         │                 │ - Preserve structural context    ││
│  ┌──────▼──────┐         │ - Normalize comments + code      ││
│  │ Serializer  │         └──────────┬──────────────────────┘│
│  └──────┬──────┘                    │                        │
│         │                            │                        │
│         ▼                            ▼                        │
│  ┌─────────────┐           ┌─────────────────────────────────┐│
│  │ TF-IDF Path │           │ ONNX Qwen3 Path (Opt-in)         ││
│  │             │           │                                 ││
│  │ - Deterministic │        │ - Neural encoding (0.6B model)  ││
│  │ - Fast build (77ms)│     │ - Cross-language understanding   ││
│  │ - O(1) lookup via HashMap│ │ - Reranker for quality          ││
│  │ - Persisted embeddings │ │ - Unified graph traversal        ││
│  │ - 768 dimensional        │ - Dynamic (model file at runtime) ││  │ - Code-specific weighting │ │ - Compact size (300-600MB)        ││
│  └─────────────┘           └─────────────────────────────────┘│
│                                         │                        │
│                                         ▼                        │
│                                ┌────────────────────────────────┐│
│                                │ Index Storage                    ││
│                                │ - mmap vector embeddings.bin   ││
│                                │ - Unified NodeIndex struct      ││
│                                │ - Embedding+metadatałaszar      ││
│                                └────────────────────────────────┘│
│                                         │                        │
│                                         ▼                        │
│                                 ┌────────────────────┐          │
│                                 │ Search Time        │          │
│                                 │ - Brute force      │          │
│                                 │ - Cosine similarity│          │
│                                 └────────────────────┘          │
└─────────────────────────────────────────────────────────────┘
```

---

## File Changes Required

### 1. Cargo.toml Configuration

```toml
# Add Qwen-specific packages (optional, builds only with `cargo build --features onnx`)
[dependencies]
# ONNX Runtime for Rust - preferred for efficiency
ort = { version = "2", optional = true }

# Token parsing (HuggingFace tokenizer compat)
tokenizers = { version = "0.19", optional = true }

# Dynamic loading of models at runtime
libloading = { version = "0.8", optional = true }

# Optional: Python interop for model loading (fallback)
pyo3 = { version = "0.20", optional = true, features = ["extension-module"] }

[features]
# TRUE neural embeddings with ONNX Qwen3 onnx = [
    "search",
    "onnxruntime",
    "tokenizers",
]
```

### 2. New Source File: src/search/onnx/qwen_embedding.rs

Creates ONNX-based embedding provider for Qwen3 models.

```rust
default import version n
```

module src/
```rust
// src/search/onnx/qwen_embedding.rs

use ort::{OrtEnvironment, OrtSession, SessionOutput; 
use tokenizers::tokenizer::{Tokenizer, TruncationStrategy};
use crate::search::vector::VectorIndex;

/// ONNX-based Qwen3 embedding provider for TRUE neural semantics
pub struct QwenEmbeddingProvider {
    /// ONNX Runtime environment
    env: OrtEnvironment,
    /// Embedding model session
    embedding_session: OrtSession,
    /// Optional reranker session (for result quality improvement)
    reranker_session: Option<OrtSession>,
    /// Model name/version tracking
    model_name: String,
    /// Expected embedding dimension
    embedding_dim: usize,
}

impl QwenEmbeddingProvider {
    /// Load Qwen3 embedding model from ONNX
    pub fn load_onnx_model(model_path: &str) -> Result<Self> {
        let env = OrtEnvironment::builder().with_name("qwen_embedding").build()?;
        
        let embedding_session = OrtSession::builder()
            .with_env(env.clone())
            .with_model_path(model_path)
            .build()?;
        
        Ok(Self {
            env,
            embedding_session,
            reranker_session: None,
            model_name: "Qwen3-Embedding-0.6B".to_string(),
            embedding_dim: 1024, // Qwen3 embedding dim
        })
    }
    
    /// Encode text using Qwen3 embedding model
    pub fn encode(&self, text: &str) -> Result<Vec<f32>> {
        let tokenizer = Tokenizer::from_file("tokenizers/Qwen3-Embedding-0.6B.json")?;
        
        let encoding = tokenizer.encode(text, true)?
            .with_truncation(TruncationStrategy::Fixed(512))?
            .into_tokens();
        
        let inputs: ort::Value = ort::Value::from_array(self.env.alloc_owned_tensor::<f32>(...))?;
        
        let outputs: Vec<SessionOutput> = self.embedding_session.run(vec![inputs])?;
        
        Ok(outputs[0].try_extract::<Vec<f32>>()?.into())  
    }
    
    /// Rerank search results for better quality
    pub fn rerank(&self, query: &[f32], results: &[(&str, f32)]) -> Result<Vec<(String, f32)>> {
        // If reranker not loaded, return original
        let Some(ref session) = self.reranker_session else {
            return Ok(results.iter().map(|(id, n)| (id.to_string(), n)).collect());
        };
        
        // Build reranker inputs
        let encoding = self.encode(
            format!("Query: {{}}, Documents: {{}}", query_str, ids.join(","))
        )?;
        
        // Batch rerank all results
        // ...
        
        Ok(reranked)
    }
}
```

### 3. Modified: src/cli/index_builder.rs (TfIdfEmbedder Extension)

Make TF-IDF embedder **extensible** to support neural backend toggle:

```rust-- 
这些不是代码修改，而是计划文档。
我现在将继续创建该计划文档。

defaultäenたいand Reranker Model Selection ==========================================

### 4. Bundle Qwen3 ONNX Models with Installers

**Model Distribution Strategy:**

| Platform | Bundle Location | Size |
|----------|----------------|------|
| Cargo crate | src/data/models/qwen3-embed-0.6b.onnx | ~300MB |
| npm package | packages/npm-leindex-mcp/models/qwen3-embed-0.6b.onnx | ~300MB |
| PyPI wheel | data/models/qwen3-embed-0.6b.onnx | ~300MB |
| Local install.sh | ~/.leindex/models/qwen3/ | ~300MB |

**ONNX Model Files Required:**
```
qwen3-embed-0.6b.onnx       # Qwen3-Embedding-0.6B  
qwen3-rerank-0.6b.onnx       # Qwen3-Reranker-0.6B (optional, loaded if available)
qwen3-embed-4b.onnx         # Optional (don't bundle by default)
embeddinggemma-300m.onnx    # Alternative lightweight option
```

**Installer Integration:**

```bash
# In install.sh, after downloading leindex package:
if [ "$LEINDEX_EMBEDDING_BACKEND" = "onnx" ]; then
    echo "Downloading Qwen3-Embedding-0.6B ONNX model (~300MB)..."
    mkdir -p ~/.leindex/models/qwen3/
    curl -L https://huggingface.co/scooter-lacroix/leindex-weights/resolve/main/qwen3-embed-0.6b.onnx \
        -o ~/.leindex/models/qwen3/qwen3-embed-0.6b.onnx
    chmod 644 ~/.leindex/models/qwen3/qwen3-embed-0.6b.onnx
fi
```

---

## Implementation Phases

### **Phase 1: ONNX Integration Infrastructure** (Days 1-2)

- Add `ort` and `tokenizers` to Cargo.toml
- Create `src/search/onnx/` module structure
- Implement `QwenEmbeddingProvider` core
- Add model loading from bundled ONNX files

**Deliverables:**
- [ ] ONNX runtime tied into existing SearchEngine
- [ ] ONNX model loading from `src/data/models/`
- [ ] Compatible Tokenizer (HuggingFace Qwen format)

### **Phase 2: Unified Embedding Pipeline** (Days 3-4)

- Modify `NodeInfo` to support neural embeddings
- Create unified `EmbeddingBackend` enum:
  ```rust
  pub enum EmbeddingBackend {
      TfIdf(TfIdfEmbedder),
      Neural(QwenEmbeddingProvider),
  }
  ```
- Integrate neural encoding into index construction
- Persist neural embeddings to mmap file

**Deliverables:**
- [ ] Single pipeline handles both TF-IDF and ONNX
- [ ] Neural embeddings generated during indexing
- [ ] Backward compatible: TF-IDF still available

### **Phase 3: Cross-Language Semantic Support** (Days 5-6)

- Implement language detection in preprocessing
- Chunk code at AST boundaries for multi-language support
- Preserve cross-language context (e.g., Python → SQL, CSS → HTML)

**Deliverables:**
- [ ] Language tags on all nodes
- [ ] Cross-language search: query in English, finds code in multiple langs
- [ ] Performance < 50ms p95 for search

### **Phase 4: Quality Reranking (Optional) ** (Days 7-8)

- Load Qwen3-Reranker-0.6B if available
- Apply reranking to search results
- Improve top-1 accuracy by 15%+

---

## Configuration Interface

### Environment Variables

```bash
# Embedding backend selection (default: tfidf for backward compat)
export LEINDEX_EMBEDDING_BACKEND=onnx  # or "tfidf" for legacy

# ONNX model selection (default: qwen3-embed-0.6b)
export LEINDEX_ONNX_MODEL=qwen3-embed-0.6b

# Fallback to lightweight if resource constrained
export LEINDEX_ONNX_MODEL=embeddinggemma-300m

# Model directory (default: ~/.leindex/models/)
export LEINDEX_MODELS_PATH=./data/models/

# Enable/disable usage tips (default: false)
export LEINDEX_EMBEDDING_TIPS=true

# Explicit model paths (override defaults)
export QWEN3_EMBED_PATH=./models/qwen3-embed-0.6b.onnx
export QWEN3_RERANK_PATH=./models/qwen3-rerank-0.6b.onnx
```

### Feature Flags

```toml
[features]
# Enable TRUE neural embeddings (opt-in, resource heavy)
onnx = ["search", "onnxruntime", "tokenizers", "memmap2"]

# Enable Qwen3 models in installer bundle
installer-weights = []
```

---

## Validation & Test Plan

### Unit Tests (Add to src/search/onnx/qwen_embedding.rs/tests)

```rust
#[tokio::test] 
async fn test_qwen3_embedding_dimensions() {
    let provider = QwenEmbeddingProvider::load_onnx_model("...path...");
    let embedding = provider.encode("test code string").unwrap();
    assert_eq!(embedding.len(), 1024); // Qwen3 dim
    assert!(embedding.iter().all(|x| !x.is_nan()));
}

#[test]
fn test_cross_language_semantic_similarity() {
    // English "Connect database" should be similar to SQL/C++/Python db connection code
    // Similarity score > 0.7 for related concepts
}
```

### Integration Tests

```rust
#[tokio::test]
async fn test_onnx_embeddings_improve_search_quality() {
    // Build index with TF-IDF (baseline)
    // Build index with ONNX Qwen3 (new)
    // Run same queries against both
    // Verify ONNX results have higher semantic relevance scores
    // Ex: Query "authentication pattern" returns OAuth code in ONNX > TF-IDF
}
```

### Performance Benchmarks

| Metric | Expected (With ONNX) |
|--------|----------------------|
| Encoding (per node) | < 5ms |
| Search (p95) | < 50ms |
| Memory (model loaded) | ~300-600MB |
| Index build time (10K nodes) | < 30s |
| Cross-language query | > 0.7 similarity |

### Backward Compatibility

```rust
// Ensure TF-IDF still works when onnx=0
[dev-features]
tfidf-only = []  
upload-speed-up-btn = ["search"]
```

---

## Deployment Checklist

### Release Requirements

- [ ] ONNX Runtime optimized for: Linux x86_64, macOS ARM64, Windows x64
- [ ] Model files validated (checksums match HuggingFace)
- [ ] Installer scripts updated (cargo, npm, pip)
- [ ] Documentation sync: README.md, install guide, MCP.md
- [ ] Version parity across all published surfaces (v1.7.0+)

### Model Validation

```bash
# Verify model integrity before release
sha256sum data/models/qwen3-embed-0.6b.onnx
echo "Expected: $(cat data/models/qwen3-embed-0.6b.onnx.sha256)"
```

### Rollout Strategy

```bash
# Opt-in install
export LEINDEX_EMBEDDING_BACKEND=onnx  # Must be explicit

# Progressive rollout
- Week 1: 10% of users (via A/B testing)
- Week 2: 50% of users
- Week 3: 100% of users (default enabled)
```

---

## Success Criteria

| Phase | Metric | Target |
|-------|--------|--------|
| **Encoding Quality** | TF-IDF vs ONNX similarity correlation | > 0.85 (TF-IDF as baseline) |
| **Search Quality** | Top-3 search result relevance | > 0.90 accuracy |
| **Resource Impact** | Memory increase (vs TF-IDF) | < 500MB |
| **Build Impact** | Index time increase | < 10% slowdown |
| **Cross-Language** | Query in Lang A, Code in B | > 0.7 similarity |

---

## References

### Model Documentation
- [Qwen3-Embedding-0.6B](https://huggingface.co/Qwen/Qwen3-Embedding-0.6B) - Selected DEFAULT
- [Qwen3-Reranker-0.6B](https://huggingface.co/Qwen/Qwen3-Reranker-0.6B) - Text quality improvement
- [google/embeddinggemma-300m](https://huggingface.co/google/embeddinggemma-300m) - Alternative lightweight

### Technical Resources
- [ORT Rust crate](https://crates.io/crates/ort) - ONNX Runtime bindings
- [Tokenizers crate](https://crates.io/crates/tokenizers) - HuggingFace tokenizer
- [ONNX Qwen3 models](https://huggingface.co/models?search=qwen3+onnx) - Quantized models available

### Validation
- Existing review comments in `docs/findings/`
- ONNX integration referenced in `docs/mcp_injection_debug_plan.md`
- Architecture discussion in `maestro/tracks/`

---

## Timeline Estimation

| Phase | Duration | Key Tasks |
|-------|----------|-----------|
| **Phase 1** | 2 days | ONNX infrastructure, model loading |
| **Phase 2** | 2 days | Unified pipeline, backward compatibility |
| **Phase 3** | 2 days | Cross-language semantics, testing |
| **Phase 4** | 2 days | Quality reranker, performance tuning |
| **Phase 5** | 1 week | Deployment preparation, installer updates |
| **Total:** | **9 days** (2 week sprint) | All validation, testing, deployment docs |

---

## Risk Assessment

| Risk | Likelihood | Impact | Mitigation |
|------|------------|--------|------------|
| ONNX Runtime compiles everywhere | Medium | High | Infrastructure in stages, fallbacks if build fails([/libloading if ort fails)/li>|
| Model file bloat (> 600MB) | Medium | Medium | Only load when `LEINDEX_EMBEDDING_BACKEND=onnx`, compress, or download on first use |
| ONNX Runtime fallback required | High | Medium | Implement `/libloading` for Python/ONNX Runtime bindings as tier 2 |
| Cross-language accuracy low | Low | High | Fine-tune via prompt engineering, ensemblism |
| Memory usage > expected | Medium | High | Add `MemoryHigh` enforcement in installer and CLI2 |

---

## Decision Log

**Date: 2026-05-08  **
**Source:** User specification via chat

### Decisions Made:
1. ⚠️ **Model:** Qwen3-Embedding-0.6B (DEFAULT) + Qwen3-Reranker-0.6B (SELECTIVE)
2. 📦 **Distribution:** Bundle ONNX model files with installers (~300MB)
3. 🏗️ **Backend:** Prefer `ort` crate, but provide Python fallback
4. ▶️ **Integration:** Unified index (TF-IDF + ONNX in same pipeline)

### Decisions Deferred:
1. ONNX vs TensorRT backend optimization
2. Quantized 4-bit model support for further compression
3. Cloud model API fallback for users without GPU

---

**Document:** `IMPLEMENTATION_PLAN_onnx_qwen_embeddings.md`  
**Status:** Ready for Review  
**Next:** awaiting confirmation of model choice and ONNX backend strategy
