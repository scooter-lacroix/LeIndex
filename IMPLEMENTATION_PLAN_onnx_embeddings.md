# Implementation Plan: True Neural Embeddings with ONNX Runtime

**PR:** Unified Crate - ONNX Embeddings Implementation  
**Commit Reference:** After b0509b1  
**Feature:** R15 - True neural embeddings with cross-language support

## Context

From `docs/mcp_injection_debug_plan.md`:
- **Bug 2:** Semantic search always returns 0.0 score
- **Root Cause:** `DefaultHasher` only hashes symbol names, not content
- **Solution:** Integrate ONNX Runtime with compact code embedding model

From `maestro/tracks/mcp_fix_tool_supremacy_20260227/spec.md`:
- Goal: Replace `DefaultHasher` with real embeddings
- Expected: Non-zero semantic_score for related queries
- Constraint: Sub-1B model for resource-constrained users

## Success Criteria

✅ `semantic_score` is non-zero for semantically related search results  
✅ ONNX integration is opt-in via feature flag  
✅ Tiny (< 1B parameter) model for constrained environments  
✅ Cross-language semantic understanding  
✅ Backward compatible with existing TF-IDF pipelines  

## Implementation Strategy

### Phase 1: ONNX Runtime Integration (2-3 days)
- Add `ort` crate to Cargo.toml
- Implement ONNX model loader for embeddings
- Create embedding generation pipeline

### Phase 2: Model Integration (1-2 days)  
- Support multiple model formats (ONNX, tiny models)
- Implement caching and model sharing
- Add configuration options

### Phase 3: Cross-Language Capabilities (3-4 days)
- Integrate tokenizer for multilingual code
- Test with multiple languages
- Performance optimization

---

## File Changes Required

### 1. Cargo.toml
Add ONNX dependencies as optional:

```toml
[dependencies]
# Add ONNX Runtime (optional)
ort = { version = "2", optional = true }
tinyjson = { version = "0.5", optional = true }
dfdx = { version = "0.4", optional = true }

[features]
# Enable neural embeddings with ONNX
onnx = ["dep:ort", "dep:tinyjson", "dep:dfdx"]

# Or minimal model support
tiny-onnx = ["dep:ort", "dep:tinyjson"]
```

### 2. src/search/vector.rs
- Add ONNX model field
- Implement hybrid search (ONNX + TF-IDF)

### 3. src/cli/index_builder.rs  
- Add TF-IDF embedder ONNX compatibility
- Implement model switching logic

### 4. Configuration
- Add environment variable: `LEINDEX_EMBEDDING_MODEL`
- Default: `all-MiniLM-L6-v2` (384dim, 80MB)
- Allow: `BAAI/bge-small-en-v1.5`, `all-MiniLM-L6-v2`, custom ONNX

---

## Model Selection

### Option A: ONNX Runtime with tiny models
| Model | Params | Size | Dim | Supported |
|-------|--------|------|-----|-----------|
| all-MiniLM-L6-v2 | 22M | ~80MB | 384 | CPU, GPU |
| BAAI/bge-small-en-v1.5 | 33M | ~130MB | 384 | CPU, GPU |
| paraphrase-MiniLM-L3-v2 | 13M | ~50MB | 384 | CPU, GPU |

### Option B: External API integration
- HuggingFace Inference API
- Custom embedding service
- Requires network, no local dependency

### Option C: Download and embed model
- ONNX files distributed with binary (~100MB)
- Model in `src/data/models/`
- Compile-time embedding

**RECOMMENDED:** Option A with `all-MiniLM-L6-v2` as default - lightweight, cross-platform, full privacy.

---

## Migration Path

1. **Default:** Keep TF-IDF as default, skip ONNX
2. **Opt-in:** User sets `LEINDEX_ONNX_MODEL=all-MiniLM-L6-v2` 
3. **Enhanced:** User sets `LEINDEX_ONNX_MODEL=BAAI/bge-small-en-v1.5`

---

## Testing Requirements

- Unit tests: Embedding generation quality metrics
- Integration: Cross-language search validation  
- Benchmark: Search quality vs TF-IDF baseline
- Backward compatibility: TF-IDF still works

---

## Timeline

- **Days 1-2:** ONNX integration (crate, model loader)
- **Days 3-4:** Embedding pipeline implementation
- **Days 5-8:** Testing, benchmarking, optimization

**Total:** 2 weeks with full testing coverage
