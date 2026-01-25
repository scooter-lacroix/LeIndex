# Specification: LeIndex Rust Renaissance - Master Track

**Track ID:** `leindex_rust_refactor_20250125`
**Track Type:** Master Track (with orchestrate-capable sub-tracks)
**Status:** New
**Created:** 2025-01-25

---

## Overview

### Vision

Transform LeIndex from a Python-based code search engine into a **Deep Code Intelligence Engine** written in Rust. This Master Track encompasses a complete architectural reimagining that merges the capabilities of the 5-layer code analysis system (formerly TLDR) directly into LeIndex's core, creating a unified, high-performance platform for semantic code understanding, intelligent search, and deep program analysis.

### The "Why"

**Current State:**
- LeIndex is Python-based with good search capabilities but limited deep code understanding
- Memory-intensive due to Python object overhead (~400 bytes per node)
- Limited cross-project intelligence and context expansion

**Target State:**
- Pure Rust core for maximum performance and memory efficiency
- 10x memory reduction through zero-copy AST and efficient graph structures
- Gravity-based traversal for intelligent context expansion
- Node-level semantic search that bridges "finding" and "understanding"
- Cross-project intelligence with global symbol resolution

### Key Principles

1. **Greenfield Architecture** - Write all Rust code from scratch. Reference existing implementations only as patterns, never copy 1:1
2. **Playful Branding** - All components use LeIndex-themed naming (no "TLDR" branding)
3. **Orchestrate-Ready** - Master Track structure with sub-tracks designed for `/maestro:orchestrate`
4. **Zero Data Loss** - Maintain or improve analysis accuracy vs Python baseline
5. **Performance First** - Every component designed for speed and memory efficiency

---

## Master Track Architecture

### Sub-Track Breakdown

This Master Track consists of **5 orchestrate-capable sub-tracks**:

#### Sub-Track 1: `leparse` - Core Parsing Engine
**French-inspired:** *Le Parse* (The Parsing)
- Zero-copy AST extraction using tree-sitter
- Multi-language support (17+ languages)
- Lazy-loaded grammars for reduced memory footprint
- Trait-based `CodeIntelligence` extractor pattern

#### Sub-Track 2: `legraphe` - Graph Intelligence Core
**French-inspired:** *Le Graphe* (The Graph)
- Program Dependence Graph (PDG) engine using `petgraph`
- Gravity-based traversal algorithm
- Node embedding generation for semantic entry points
- Bitmask-based reachability analysis

#### Sub-Track 3: `lerecherche` - Search & Analysis Fusion
**French-inspired:** *La Recherche* (The Search)
- Node-level embeddings (every function/class embedded individually)
- Semantic entry points bridging vector search to graph expansion
- Vector-AST synergy for deep code understanding
- Sub-100ms semantic query responses

#### Sub-Track 4: `lestockage` - Persistent Storage Layer
**French-inspired:** *Le Stockage* (The Storage)
- Extended SQLite schema with `intel_nodes` and `intel_edges`
- Salsa-based incremental computation (node-level hashing)
- DuckDB analytics integration for graph metrics
- Cross-project symbol resolution tables

#### Sub-Track 5: `lepasserelle` - Bridge & Integration
**French-inspired:** *La Passerelle* (The Bridge)
- PyO3 FFI bindings for Python-Rust interop
- Unified MCP tool: `leindex_deep_analyze`
- Memory-aware spilling and resource management
- Shared memory buffers (mmap) for zero-copy data transfer

---

## Functional Requirements

### Sub-Track 1: `leparse` (Core Parsing Engine)

**FR-1.1 Multi-Language AST Extraction**
- Support for Python, JavaScript, TypeScript, Go, Rust, Java, C++, C#, Ruby, PHP, Swift, Kotlin, Dart, Lua, Scala, Elixir, Haskell
- Tree-sitter based parsing with per-language `LanguageConfig`
- Lazy-loaded grammar loading to minimize initial memory footprint

**FR-1.2 Zero-Copy Architecture**
- AST nodes represented as byte-slice references into source buffers
- No intermediate String allocations during parsing
- Direct memory mapping where possible

**FR-1.3 Trait-Based Extractor Pattern**
```rust
pub trait CodeIntelligence {
    fn get_signatures(&self, source: &[u8]) -> Vec<SignatureInfo>;
    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Graph<Block, Edge>;
    fn extract_complexity(&self, node: &Node) -> ComplexityMetrics;
}
```

**FR-1.4 Symbol Identification**
- Function signatures with parameters and return types
- Class definitions with methods and inheritance
- Module-level imports and dependencies
- Docstring extraction with semantic summarization

### Sub-Track 2: `legraphe` (Graph Intelligence Core)

**FR-2.1 Program Dependence Graph (PDG)**
- Unified dependency graph merging: Call Graph + Data Flow Graph + Inheritance Graph
- `petgraph::StableGraph` using `u32` indices for nodes
- Persistent storage in SQLite for instant retrieval

**FR-2.2 Gravity-Based Traversal**
- Relevance formula: `Relevance(N) = (SemanticScore(N) * Complexity(N)) / (Distance(Entry, N)^2)`
- Priority-weighted expansion using binary heap
- Token-budget aware context building
- Flow-aware hotspot detection (high eigenvector centrality)

**FR-2.3 Node Embeddings**
- Individual embeddings for each function/class node
- CodeRankEmbed (nomic-ai) model integration via PyO3
- 768-dimensional vectors stored in BLOB columns

**FR-2.4 Impact Analysis**
- Bitmask-based reachability queries O(V+E)
- Forward and backward impact tracing
- Cross-project dependency resolution

### Sub-Track 3: `lerecherche` (Search & Analysis Fusion)

**FR-3.1 Node-Level Vector Search**
- LEANN backend (HNSW/DiskANN) for node embeddings
- Find functions by semantic meaning, not just text
- Top-K results return symbol IDs, not file paths

**FR-3.2 Semantic Entry Points**
- Pipeline: Query → Vector Search → Symbol ID → PDG Expansion → Context
- Automatic deep analysis triggered by search results
- Gravity-based context expansion from entry nodes

**FR-3.3 Hybrid Scoring**
- Combines semantic similarity (LEANN) with structural relevance (graph position)
- Adaptive ranking based on query type
- Context-aware result highlighting

**FR-3.4 Natural Language Queries**
- "Show me how embedding batching works" → Finds function → Expands call graph
- "Where is authentication logic handled?" → Semantic search across all auth patterns
- "What are the performance bottlenecks?" → Complexity + centrality analysis

### Sub-Track 4: `lestockage` (Persistent Storage Layer)

**FR-4.1 Extended SQLite Schema**
```sql
CREATE TABLE intel_nodes (
    id INTEGER PRIMARY KEY,
    project_id TEXT,
    file_path TEXT,
    symbol_name TEXT,
    node_type TEXT,  -- 'function', 'class', 'method'
    signature TEXT,
    complexity INTEGER,
    content_hash TEXT,
    embedding BLOB,
    FOREIGN KEY(project_id) REFERENCES projects(id)
);

CREATE TABLE intel_edges (
    caller_id INTEGER,
    callee_id INTEGER,
    edge_type TEXT,  -- 'call', 'inheritance', 'data_dependency'
    PRIMARY KEY(caller_id, callee_id, edge_type)
);

CREATE TABLE analysis_cache (
    node_hash TEXT PRIMARY KEY,
    cfg_data BLOB,
    complexity_metrics BLOB,
    timestamp INTEGER
);
```

**FR-4.2 Salsa Incremental Computation**
- Node-level BLAKE3 hashing for AST sub-trees
- Query-based invalidation (only affected nodes re-computed)
- Symbol-level incrementalism (not file-level)

**FR-4.3 Cross-Project Intelligence**
- Global symbol table using LeIndex Tier 1 metadata
- Resolve function calls across repository boundaries
- External project references with lazy loading

**FR-4.4 DuckDB Analytics**
- Graph metrics queries (centrality, complexity distribution)
- Hotspot detection analytics
- Codebase evolution tracking

### Sub-Track 5: `lepasserelle` (Bridge & Integration)

**FR-5.1 PyO3 FFI Bindings**
```rust
#[pymodule]
fn leindex_rust(_py: Python, m: &PyModule) -> PyResult<()> {
    m.add_class::<RustAnalyzer>()?;
    m.add_function(wrap_pyfunction!(build_weighted_context, m)?)?;
    Ok(())
}
```

**FR-5.2 Unified MCP Tool**
```python
async def leindex_deep_analyze(
    ctx: Context,
    query: str,
    budget: int = 2000,
    project_path: Optional[str] = None
) -> str:
    # 1. Semantic search for entry point
    # 2. Trigger Rust analyzer for graph expansion
    # 3. Return LLM-ready summary
```

**FR-5.3 Memory Management**
- Memory-aware spilling when RSS > 90%
- Clear PDG cache for non-active projects
- Spill analysis cache to DuckDB
- Trigger gc.collect() on Python side

**FR-5.4 Zero-Copy Data Transfer**
- mmap for passing large source files
- Avoid O(N) string copies across FFI boundary
- Shared memory buffers for embeddings

---

## Non-Functional Requirements

### Performance Targets

**NFR-1 Indexing Performance**
- Target: Match or beat Python baseline (<60s for 50K files)
- Parallel file processing using `rayon`
- Incremental indexing only for changed symbols

**NFR-2 Memory Efficiency**
- Target: 10x reduction vs Python (400 bytes → 32 bytes per node)
- Zero-copy AST wherever possible
- Lazy-loaded grammars and parsers

**NFR-3 Search Latency**
- Target: Sub-100ms semantic queries (P95)
- Sub-10ms for node-level lookups
- Gravity traversal completes within token budget

**NFR-4 Analysis Accuracy**
- Zero valuable data loss vs Python implementation
- Improved context relevance through gravity-based traversal
- Higher token efficiency (20% improvement target)

### Quality Requirements

**NFR-5 Test Coverage**
- New Rust tests with Python validation
- Contract tests for FFI boundaries
- Integration tests for end-to-end workflows
- Target: >95% code coverage

**NFR-6 Code Quality**
- All Rust code passes clippy lints
- No unsafe code without justification
- Comprehensive error handling with `thiserror`
- Clear documentation for all public APIs

**NFR-7 Maintainability**
- Modular crate structure
- Clear separation of concerns
- Extensible trait system
- Comprehensive inline documentation

---

## Acceptance Criteria

### Master Track Completion

**AC-Master All Sub-Tracks Complete**
- [ ] `leparse` complete with all language parsers working
- [ ] `legraphe` complete with PDG and gravity traversal
- [ ] `lerecherche` complete with node-level search
- [ ] `lestockage` complete with schema and incrementalism
- [ ] `lepasserelle` complete with MCP integration

**AC-Master Performance Validated**
- [ ] Indexing 50K files in <60 seconds
- [ ] Memory usage reduced by 10x vs Python
- [ ] Semantic queries respond in <100ms (P95)
- [ ] Token efficiency improved by 20%

**AC-Master Quality Gates**
- [ ] All tests passing (>95% coverage)
- [ ] No clippy warnings
- [ ] MCP tool functional and validated
- [ ] Documentation complete

### Per Sub-Track Acceptance

#### `leparse` Acceptance
- All 17+ languages parseable without errors
- Zero-copy architecture verified (no unnecessary allocations)
- AST extraction matches Python baseline for accuracy
- Grammars lazy-load correctly

#### `legraphe` Acceptance
- PDG builds correctly for complex codebases
- Gravity traversal produces more relevant results than BFS
- Node embeddings generate and store successfully
- Impact analysis returns correct symbol dependencies

#### `lerecherche` Acceptance
- Node-level search finds functions semantically
- Semantic entry points trigger correct graph expansion
- Natural language queries return relevant code
- Hybrid scoring improves relevance over text-only

#### `lestockage` Acceptance
- SQLite schema supports all required queries
- Salsa incrementalism only re-computes changed nodes
- Cross-project resolution works across multiple repos
- DuckDB analytics return correct graph metrics

#### `lepasserelle` Acceptance
- PyO3 bindings callable from Python without errors
- MCP tool `leindex_deep_analyze` functional
- Memory spilling activates at 90% RSS threshold
- Zero-copy transfer verified (no unnecessary copies)

---

## Out of Scope

### Explicitly Not Included

**OS-1 Web UI**
- LeIndex remains a CLI/MCP service
- No web-based visualization or dashboard
- Users can build UI on top of the API

**OS-2 Distributed Systems**
- No distributed indexing across multiple machines
- Single-machine architecture
- No federated search or shared indexes

**OS-3 Language Server Protocol (LSP)**
- No IDE-specific integrations
- Focus on CLI and MCP integration
- LSP can be built on top by others

**OS-4 Cloud/SaaS Offering**
- Self-hosted and local-only
- No managed service version
- Privacy-first design maintained

**OS-5 Real-Time Collaboration**
- No shared indexes or collaboration features
- Personal, local indexing model
- No real-time sync between users

### Future Considerations (Not Now)

**FC-1 Multi-GPU Support**
- Current focus: Single GPU acceleration
- Multi-GPU distribution deferred

**FC-2 Advanced Visualization**
- No interactive graph visualization
- No call graph rendering
- Command-line output only

**FC-3 Custom Embedding Models**
- Use existing CodeRankEmbed model
- No custom model training infrastructure
- Model serving via existing PyTorch integration

---

## Dependencies

### Internal Dependencies
- Current LeIndex Python codebase (for reference and validation)
- Existing MCP server implementation
- Current SQLite/DuckDB schemas

### External Rust Crates
- `tree-sitter` (parsing)
- `petgraph` (graph structures)
- `rayon` (parallel processing)
- `pyo3` (Python bindings)
- `blake3` (hashing)
- `sled` or `rocksdb` (incremental cache)
- `tantivy` (native Rust FTS)
- `hnsw-rs` or existing LEANN Rust backend

### External Python Dependencies (during transition)
- `sentence-transformers` (CodeRankEmbed)
- PyTorch (GPU/CPU inference)
- Existing LeIndex Python modules (for validation)

---

## Success Metrics

### Technical Metrics
- Index 50K files in <60 seconds
- Memory usage <2GB during indexing (vs Python baseline)
- Search latency P95 <100ms
- 10x memory reduction per node
- 20% token efficiency improvement
- >95% test coverage

### User Experience Metrics
- Setup time: <2 minutes from install to first analysis
- Zero configuration for 90% of use cases
- Clear error messages with actionable guidance
- Comprehensive documentation for advanced scenarios

### Adoption Metrics
- MCP tool used in >80% of sessions after availability
- Search accuracy >90% relevance in top 5 results
- User-reported satisfaction with analysis quality
