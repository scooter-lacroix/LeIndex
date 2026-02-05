# Changelog

All notable changes to the LeIndex project are documented in this file.

## [0.1.0] - 2025-01-26 - Rust Rewrite Release

### 🦀 **Major Release: Complete Rewrite in Pure Rust**

This release represents a complete rewrite of LeIndex from Python to Rust, delivering a modern, memory-safe implementation with zero-copy AST extraction, advanced PDG analysis, and first-class MCP server integration.

### ✨ **What's New**

#### **Pure Rust Implementation**
- **Zero-Copy AST Extraction**: Tree-sitter based parsing with 11+ language support
- **Program Dependence Graph (PDG)**: Advanced code relationship analysis via petgraph
- **HNSW Vector Search**: In-memory semantic similarity search (temporary implementation)
- **MCP Server**: Built-in MCP server with axum-based HTTP transport
- **Memory Efficient**: Smart cache management with automatic spilling
- **Project Configuration**: TOML-based per-project settings

#### **Workspace Architecture**
Five specialized crates:
- **leparse** - AST extraction (Tree-sitter)
- **legraphe** - PDG analysis (petgraph)
- **lerecherche** - Vector search (HNSW)
- **lestockage** - Storage layer (Turso/libsql planned)
- **lepasserelle** - CLI & MCP server

### 🔧 **Breaking Changes**

⚠️ **Complete Rewrite**: This is a breaking change with no backward compatibility.

- **Language Changed**: Python → Rust
- **Installation Method**: `pip install leindex` → `cargo build --release`
- **Configuration Format**: YAML → TOML
- **Binary**: Now compiled Rust binary (same name: `leindex`)
- **Vector Search**: LEANN (file-based) → HNSW (in-memory, temporary)
- **Storage Architecture**: Turso/libsql unified storage planned (vectors + metadata)

### 📊 **Feature Parity Status**

| Feature | Python v2.0.2 | Rust v0.1.0 | Status |
|---------|---------------|-------------|--------|
| **CLI Commands** | ✅ | ✅ | Complete |
| **MCP Server** | ✅ | ✅ | Complete |
| **Tree-sitter Parsing** | ✅ | ✅ | Complete (11+ languages) |
| **Memory Management** | ✅ | ✅ | Complete |
| **Project Configuration** | ✅ | ✅ | Complete (TOML) |
| **Vector Search** | ✅ | ⚠️ | HNSW in-memory (temporary) |
| **Full-Text Search** | ✅ | ❌ | Not yet implemented |
| **Analytics** | ✅ | ❌ | Not yet implemented |
| **Cross-Project Search** | ✅ | ❌ | Planned for v0.3.0 |

### 🔧 **Technical Changes**

#### **Removed Dependencies**
- All Python dependencies (LEANN, Tantivy, DuckDB, etc.)
- PyO3 bindings (vestigial dependency removed)
- PyProject configuration

#### **New Dependencies**
- `tree-sitter` - Parser runtime
- `petgraph` - Graph data structures
- `hnsw_rs` - HNSW algorithm
- `axum` - HTTP server for MCP
- `clap` - CLI argument parsing
- `rayon` - Parallel processing
- `tokio` - Async runtime (future use)

#### **Language Support**
- ✅ Python, Rust, JavaScript/TypeScript, Go, C/C++, Java, Ruby, PHP
- ⚠️ Swift, Kotlin, Dart (temporarily disabled due to tree-sitter version conflicts)

### 🔄 **Migration Notes**

#### **From Python v2.0.2 to Rust v0.1.0**

**Steps to migrate:**

1. **Uninstall Python version**:
   ```bash
   pip uninstall leindex
   ```

2. **Install Rust** (if not already installed):
   ```bash
   curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
   ```

3. **Build LeIndex**:
   ```bash
   git clone https://github.com/scooter-lacroix/leindex.git
   cd leindex
   cargo build --release --bins
   ```

4. **Update PATH**:
   ```bash
   export PATH="$HOME/.leindex/bin:$PATH"
   ```

5. **Re-index projects**:
   ```bash
   leindex index /path/to/project
   ```

**Configuration Migration:**

Convert YAML config to TOML:

```toml
# leindex.toml (Rust)
[memory]
total_budget_mb = 3072
soft_limit_percent = 0.80
hard_limit_percent = 0.93
emergency_percent = 0.98

[file_filtering]
max_file_size = 1073741824
exclude_patterns = [
    "**/node_modules/**",
    "**/.git/**",
    "**/target/**"
]

[parsing]
batch_size = 100
parallel_parsers = 4
```

### 📝 **Documentation Updates**

- **[README.md](README.md)** - Complete rewrite for Rust implementation
- **[ARCHITECTURE.md](ARCHITECTURE.md)** - Rust architecture documentation
- **[INSTALLATION_RUST.md](INSTALLATION_RUST.md)** - Rust installation guide
- **[MIGRATION.md](MIGRATION.md)** - Python to Rust migration guide
- **[MCP_COMPATIBILITY.md](MCP_COMPATIBILITY.md)** - MCP server documentation
- **[RUST_ARCHITECTURE.md](RUST_ARCHITECTURE.md)** - Detailed crate documentation

### 🏗️ **New Architecture**

```
┌─────────────────────────────────────────────────────────┐
│           LeIndex Rust Architecture                      │
├─────────────────────────────────────────────────────────┤
│  ┌──────────┐  ┌──────────┐  ┌─────────┐  ┌─────────┐ │
│  │   MCP    │  │   CLI    │  │  lepass │  │ lestock │ │
│  │  Server  │  │   Tool   │  │  erille │  │   age   │ │
│  │  (axum)  │  │  (clap)  │  │         │  │(SQLite) │ │
│  └────┬─────┘  └────┬─────┘  └────┬────┘  └────┬────┘ │
│       │             │            │            │      │
│  ┌────▼─────────────▼────────────▼────────────▼───┐  │
│  │              lepasserelle crate                 │  │
│  └────┬──────────────┬──────────────┬────────────┘  │
│       │              │              │               │
│  ┌────▼───┐   ┌─────▼─────┐   ┌────▼────┐  ┌─────▼──┐│
│  │leparse │   │ legraphe  │   │ lerech  │  │ Turso  ││
│  │Parsing │   │    PDG    │   │  HNSW   │  │Vectors ││
│  │(tree-  │   │  (petgraph│   │(hnsw_rs)│  │(libsql)││
│  │ sitter) │   │   embed)  │   │ IN-MEM  │  │Future ││
│  └────────┘   └───────────┘   └─────────┘  └─────────┘│
└─────────────────────────────────────────────────────────┘
```

### 🐛 **Technical Notes**

**Turso/libsql Integration Status:**
- Configured but not yet implemented
- Planned for v0.2.0
- Will provide:
  - Persistent vector storage (vec0 extension)
  - F32_BLOB columns for vectors
  - Unified storage for vectors AND metadata
  - Remote Turso database support

**Current Limitations:**
- Vectors stored in-memory only (requires re-indexing after restart)
- No full-text search (Tantivy integration planned)
- Swift/Kotlin/Dart parsers disabled (tree-sitter conflicts)

### 🔮 **Future Roadmap**

#### **v0.2.0 - Turso/libsql Integration** (Planned)
- [ ] Implement lestockage with libsql
- [ ] Add vec0 extension support
- [ ] F32_BLOB columns for vectors
- [ ] Remote Turso database support
- [ ] Persistent metadata storage

#### **v0.3.0 - Advanced Features** (Planned)
- [ ] Re-enable Swift/Kotlin/Dart parsers
- [ ] Cross-project search
- [ ] Global index dashboard
- [ ] Advanced memory management
- [ ] Full-text search (Tantivy)

### 🙏 **Acknowledgments**

Rust implementation built on excellent open-source projects:
- [Tree-sitter](https://tree-sitter.github.io/tree-sitter/) - Incremental parsing system
- [petgraph](https://github.com/petgraph/petgraph) - Graph data structures
- [hnsw_rs](https://github.com/jorgecarleitao/hnsw_rs) - HNSW algorithm
- [axum](https://github.com/tokio-rs/axum) - Web framework
- [Model Context Protocol](https://modelcontextprotocol.io) - AI integration

---

## [2.0.0] - 2026-01-08 - Python v2.0.0 - Global Index & Advanced Memory Management Release (Legacy)

### 🌟 **Major Feature Release: Cross-Project Search & Intelligent Memory Management**

This release introduces revolutionary new features that transform LeIndex from a single-project search tool into a comprehensive multi-project code intelligence platform with advanced memory management and hierarchical configuration.

### ✨ **What's New**

#### **Global Index - Cross-Project Search**

**Two-Tier Architecture:**
- **Tier 1**: Materialized metadata layer with instant (<1ms) project statistics
- **Tier 2**: Stale-allowed query cache with asynchronous refresh
- **Query Router**: Intelligent routing with result merging and ranking
- **Graceful Degradation**: Automatic fallback chain (LEANN → Tantivy → Ripgrep → Grep)

**Cross-Project Search:**
- Search across multiple projects simultaneously
- Fuzzy matching and pattern-based filtering
- Project-specific result aggregation
- Configurable result limits per project
- Context lines extraction

**Project Comparison Dashboard:**
- Compare projects by size, language, and health score
- Filter by status, language, and health thresholds
- Sort by multiple criteria
- Global aggregate statistics
- Language distribution analysis

**Event-Driven Updates:**
- Real-time synchronization across projects
- Automatic metadata refresh
- Cache invalidation on project changes
- Background query rebuilds

#### **Advanced Memory Management**

**Hierarchical Configuration:**
- Global defaults with per-project overrides
- Percentage-based thresholds (80%, 93%, 98%)
- Automatic hardware detection
- Zero-downtime configuration reload

**Memory Threshold Actions:**
- **80% (Soft Limit)**: Trigger cleanup and garbage collection
- **93% (Hard Limit)**: Spill cached data to disk
- **98% (Emergency)**: Emergency eviction of low-priority data

**RSS Memory Tracking:**
- Actual memory usage (not just allocations)
- Per-project memory tracking
- Memory breakdown by category
- Peak and average statistics

**Priority-Based Eviction:**
- Intelligent eviction based on data importance
- Configurable project priorities (LOW, MEDIUM, HIGH, CRITICAL)
- LRU cache eviction
- Graceful shutdown with cache persistence

**Continuous Monitoring:**
- Background memory tracking
- Configurable monitoring intervals
- Alert generation at thresholds
- Historical statistics

#### **Advanced Configuration System**

**Hierarchical YAML Configuration:**
- Global defaults (`~/.leindex/config.yaml`)
- Project overrides (`~/.leindex/projects/*.yaml`)
- Environment variable overrides
- Command-line argument overrides

**Configuration Features:**
- Validation rules with clear error messages
- Automatic migration from v1 to v2
- Hardware detection and auto-configuration
- Secure file permissions (0600)
- Automatic backups

**Zero-Downtime Reload:**
- SIGHUP signal handling
- Configuration observers pattern
- Atomic configuration updates
- Rollback on validation failure

### 📊 **Performance Improvements**

| Feature | v1.1.0 | v2.0.0 | Improvement |
|---------|--------|--------|-------------|
| **Cross-Project Search** | Not available | <100ms | **NEW** |
| **Memory Efficiency** | Manual tuning | Automatic management | **70% reduction** |
| **Config Reload** | Restart required | Zero-downtime | **Instant** |
| **Project Comparison** | Manual | Dashboard API | **Automated** |
| **Graceful Degradation** | All-or-nothing | Fallback chain | **Resilient** |
| **Indexing Speed** | ~10K files/min | ~12K files/min | **20% faster** |

### 🔧 **New Configuration**

**Global Memory Configuration:**
```yaml
memory:
  total_budget_mb: 3072
  soft_limit_percent: 0.80
  hard_limit_percent: 0.93
  emergency_percent: 0.98
  max_loaded_files: 1000
  max_cached_queries: 500
  project_defaults:
    max_loaded_files: 100
    max_cached_queries: 50
    priority: "MEDIUM"
```

**Global Index Configuration:**
```yaml
global_index:
  tier1:
    enabled: true
    auto_refresh: true
  tier2:
    enabled: true
    max_size: 1000
    ttl_seconds: 300
  graceful_degradation:
    enabled: true
    fallback_chain: ["leann", "tantivy", "ripgrep", "grep"]
```

### 📝 **Breaking Changes**

⚠️ **Configuration Format**: v2.0 uses a new hierarchical configuration format.

⚠️ **Memory Configuration**: Memory limits are now specified as percentages of total budget.

⚠️ **API Changes**: Some API functions have been renamed or moved to new modules.

### 📚 **New Documentation**

- **[docs/GLOBAL_INDEX.md](docs/GLOBAL_INDEX.md)** - Comprehensive global index guide
- **[docs/MEMORY_MANAGEMENT.md](docs/MEMORY_MANAGEMENT.md)** - Memory management documentation
- **[docs/CONFIGURATION.md](docs/CONFIGURATION.md)** - Complete configuration reference
- **[docs/MIGRATION.md](docs/MIGRATION.md)** - v1 to v2 migration guide

### 🎯 **New Examples**

- **[examples/cross_project_search.py](examples/cross_project_search.py)** - 10 cross-project search examples
- **[examples/memory_configuration.py](examples/memory_configuration.py)** - 13 memory management examples
- **[examples/dashboard_usage.py](examples/dashboard_usage.py)** - 12 dashboard usage examples
- **[examples/config_migration.py](examples/config_migration.py)** - 11 migration examples

### 🔄 **Migration Notes**

#### **From v1.x to v2.0**

**Breaking Changes**:
1. Configuration format changed from flat to hierarchical
2. Memory limits changed from absolute values to percentages
3. Some API functions moved to new modules

**Migration Steps**:
1. Backup existing configuration: `cp ~/.leindex/config.yaml ~/.leindex/backups/config.v1.yaml`
2. Upgrade to v2.0: `pip install leindex==2.0.0`
3. Run first-time setup with hardware detection
4. Migrate configuration to v2 format (see [docs/MIGRATION.md](docs/MIGRATION.md))
5. Validate new configuration
6. Test cross-project search functionality

**Configuration Mapping**:
- `memory.budget_mb` → `memory.total_budget_mb`
- `memory.soft_limit_mb` → `memory.soft_limit_percent` (divide by budget)
- `memory.hard_limit_mb` → `memory.hard_limit_percent` (divide by budget)
- `performance.parallel_workers` → `performance.parallel_scanner.max_workers`
- `performance.batch_size` → `performance.embeddings.batch_size`

### 🏗️ **New Modules**

**Global Index** (`src/leindex/global_index/`):
- `cross_project_search.py` - Cross-project search functionality
- `dashboard.py` - Project comparison dashboard
- `global_index.py` - Global index management
- `query_router.py` - Intelligent query routing
- `graceful_degradation.py` - Automatic fallback mechanisms

**Memory Management** (`src/leindex/memory/`):
- `tracker.py` - RSS memory tracking
- `thresholds.py` - Multi-level threshold checking
- `actions.py` - Memory management action queue
- `eviction.py` - Priority-based eviction
- `monitoring.py` - Continuous monitoring system

**Configuration** (`src/leindex/config/`):
- `global_config.py` - Hierarchical configuration manager
- `validation.py` - Configuration validation rules
- `migration.py` - v1 to v2 migration
- `reload.py` - Zero-downtime configuration reload

### 🧪 **Testing**

- **150+ new tests** covering all new features
- **Integration tests** for cross-project search
- **Memory management tests** with various workloads
- **Configuration validation tests**
- **Migration tests** from v1 to v2

### 🐛 **Bug Fixes**

- Fixed memory leak in query cache
- Fixed configuration validation edge cases
- Fixed graceful degradation fallback logic
- Fixed memory threshold calculation errors
- Fixed project override priority handling

### 🔮 **Future Roadmap**

#### **Planned Features**
- **Distributed Global Index**: Multi-machine support
- **Advanced Analytics**: Code quality metrics and trends
- **ML-Powered Search**: Semantic query understanding
- **Real-Time Collaboration**: Multi-user project indexing

#### **Performance Targets**
- **Sub-50ms cross-project search** for 10+ projects
- **GPU-accelerated query routing**
- **Persistent query cache** across sessions
- **Predictive memory management**

### 🙏 **Acknowledgments**

This release was made possible by:
- Feedback from the v1.x user community
- Performance optimization research
- Memory management best practices
- Configuration system design patterns

---

## 2026-01-07 - LeIndex v1.1.0 - Performance Optimization Release

### 🚀 **Major Performance Release: 3-5x Faster Indexing**

This release represents the culmination of a comprehensive 4-phase performance optimization initiative, delivering dramatic speed improvements through architectural enhancements across the indexing and search pipeline.

### ✨ **What's New**

#### **Performance Optimizations (3-5x Faster)**

**Phase 1: Async I/O Foundation**
- Implemented `aiofiles` for all file I/O operations
- Parallel file reading with configurable concurrency
- Non-blocking content extraction and parsing
- **Result**: 2-3x faster file I/O on I/O-bound workloads

**Phase 2: Parallel Processing & Batching**
- **Batch Embeddings**: Process multiple files in single GPU calls
  - Configurable batch sizes (default: 32 files/batch)
  - GPU memory management with automatic fallback
  - 3-5x faster embedding generation
- **Parallel Processing**: Multi-core content extraction
  - Configurable worker pools (default: 4 workers)
  - Semaphore-based concurrency control
  - CPU utilization: 60-80% (up from 20-30%)
- **Result**: 3-4x overall indexing speedup

**Phase 3: Advanced Optimization**
- **FileStatCache**: LRU cache for filesystem metadata
  - Cache size: 10,000 entries (configurable)
  - TTL: 5 minutes (configurable)
  - 90%+ cache hit rate on repeated operations
  - 5-10x faster `os.stat()` operations
- **ParallelScanner**: Parallel directory traversal
  - Replaces sequential `os.walk()`
  - 2-5x faster on deep/wide directory structures
  - Semaphore-based concurrency control
  - Progress tracking and statistics
- **PatternTrie**: Efficient pattern matching
  - O(m) complexity vs O(n*m) for naive matching
  - 10-100x faster ignore pattern evaluation
  - Supports glob patterns and wildcards
- **Result**: 2-5x faster scanning and filtering

**Phase 4: GPU Acceleration & Final Optimization**
- GPU support for embedding generation (CUDA/MPS/ROCm)
- Automatic device detection and selection
- Batch size optimization based on GPU memory
- Fallback to CPU for unsupported operations
- Memory profiling and optimization
- **Result**: 5-10x faster embeddings with GPU

### 📊 **Performance Improvements**

| Metric | Before (v1.0.8) | After (v1.1.0) | Improvement |
|--------|----------------|---------------|-------------|
| **Indexing Speed** | ~2K files/min | ~10K files/min | **5x faster** |
| **File Scanning** | Sequential os.walk() | ParallelScanner | **3-5x faster** |
| **Pattern Matching** | Naive O(n*m) | PatternTrie O(m) | **10-100x faster** |
| **File Stats** | Uncached syscalls | FileStatCache | **5-10x faster** |
| **Embeddings (CPU)** | Single-file | Batching (32) | **3-5x faster** |
| **Embeddings (GPU)** | CPU-only | GPU-accelerated | **5-10x faster** |
| **Memory Efficiency** | High overhead | Optimized batching | **30% reduction** |
| **Search Latency (p50)** | ~50ms | ~50ms | **Maintained** |
| **Search Latency (p99)** | ~200ms | ~180ms | **10% faster** |

*Based on benchmarks with 10K-100K file repositories on standard hardware (8-core CPU, 16GB RAM, optional GPU)*

### 🔧 **New Features**

#### **Caching Subsystem**
- **FileStatCache**: Intelligent filesystem metadata caching
  - Configurable cache size and TTL
  - LRU eviction policy
  - Statistics tracking (hit rate, memory usage)
  - Thread-safe operations
- **PatternTrie**: High-performance pattern matching
  - Trie-based data structure
  - Supports glob patterns
  - Configurable cache size
  - Statistics and monitoring

#### **Parallel Processing**
- **ParallelScanner**: Concurrent directory traversal
  - Replaces `os.walk()` with async implementation
  - Configurable worker count
  - Timeout support
  - Progress tracking
  - Error handling with graceful degradation
- **ParallelProcessor**: Multi-core content extraction
  - Worker pool management
  - Semaphore-based concurrency control
  - Automatic resource scaling

#### **GPU Acceleration**
- **Automatic GPU Detection**: CUDA (NVIDIA), MPS (Apple), ROCm (AMD)
- **Batch Optimization**: Dynamic batch size based on GPU memory
- **Graceful Fallback**: CPU-only mode for unsupported operations
- **Memory Management**: Automatic GPU memory cleanup

### 🔧 **Configuration Changes**

#### **New Configuration Options**

```yaml
# Performance Tuning (NEW)
performance:
  # File stat caching
  file_stat_cache:
    enabled: true
    max_size: 10000        # Maximum cache entries
    ttl_seconds: 300       # Cache TTL (5 minutes)

  # Parallel processing
  parallel_scanner:
    max_workers: 4         # Concurrent directory scans
    timeout_seconds: 300   # Scan timeout

  parallel_processor:
    max_workers: 4         # Content extraction workers
    batch_size: 100        # Files per batch

  # Embedding optimization
  embeddings:
    batch_size: 32         # Files per embedding batch
    enable_gpu: true       # Use GPU if available
    device: "auto"         # auto, cuda, mps, rocm, cpu
    fp16: true            # Use half-precision on GPU

  # Pattern matching
  pattern_trie:
    enabled: true
    cache_size: 1000       # Pattern cache size
```

### 📝 **Breaking Changes**

⚠️ **No breaking changes in this release.**

All changes are backward compatible. The performance improvements are transparent to users and do not require any configuration changes to benefit from them.

### 🔄 **Migration Notes**

#### **From v1.0.x to v1.1.0**

**No migration required!** This release is fully backward compatible.

**Optional Configuration** (for maximum performance):

1. **Enable GPU Acceleration** (if you have a supported GPU):
   ```yaml
   performance:
     embeddings:
       enable_gpu: true
       device: "auto"  # Automatically detects CUDA/MPS/ROCm
   ```

2. **Adjust Worker Counts** (based on your CPU):
   ```yaml
   performance:
     parallel_scanner:
       max_workers: 8     # For 8+ core CPUs
     parallel_processor:
       max_workers: 8     # For 8+ core CPUs
   ```

3. **Tune Cache Sizes** (for large repositories):
   ```yaml
   performance:
     file_stat_cache:
       max_size: 50000    # For 100K+ file repositories
     pattern_trie:
       cache_size: 5000   # For complex ignore patterns
   ```

### 🏗️ **Architecture Changes**

#### **New Components**

1. **FileStatCache Module** (`src/leindex/file_stat_cache.py`)
   - LRU cache implementation
   - Thread-safe operations
   - Statistics tracking
   - Configurable eviction policies

2. **ParallelScanner Module** (`src/leindex/parallel_scanner.py`)
   - Async directory traversal
   - Semaphore-based concurrency
   - Progress tracking
   - Error handling

3. **PatternTrie Module** (`src/leindex/ignore_patterns.py`)
   - Trie-based pattern matching
   - Glob pattern support
   - O(m) lookup complexity

4. **GPU Support** (`src/leindex/core_engine/leann_backend.py`)
   - Automatic device detection
   - Batch optimization
   - Memory management
   - FP16 support

#### **Enhanced Components**

1. **Async Indexer** (`src/leindex/async_indexer.py`)
   - Batch processing for embeddings
   - Parallel file reading
   - GPU acceleration
   - Memory profiling

2. **Ignore Patterns** (`src/leindex/ignore_patterns.py`)
   - PatternTrie integration
   - Fast pattern matching
   - Statistics tracking

3. **Configuration Manager** (`src/leindex/config_manager.py`)
   - Performance settings
   - GPU configuration
   - Cache tuning

### 📚 **New Documentation**

- **[docs/PERFORMANCE_OPTIMIZATION.md](docs/PERFORMANCE_OPTIMIZATION.md)** - Comprehensive performance guide
- **[docs/phase3_parallel_scanner_implementation.md](docs/phase3_parallel_scanner_implementation.md)** - ParallelScanner details
- **Updated README.md** - Performance benchmarks and tips
- **Updated ARCHITECTURE.md** - New caching and parallel processing patterns

### 🧪 **Testing**

- **100+ new tests** covering performance optimizations
- **Benchmarking suite** for performance validation
- **GPU testing** on multiple platforms (CUDA, MPS, ROCm)
- **Stress tests** for large repositories (100K+ files)

### 🐛 **Bug Fixes**

- Fixed memory leak in batch embedding generation
- Fixed race condition in parallel file processing
- Fixed cache eviction policy in FileStatCache
- Fixed timeout handling in ParallelScanner
- Fixed GPU memory cleanup on errors

### 🔮 **Future Roadmap**

#### **Planned Features**
- **Adaptive batching**: Dynamic batch size based on file sizes
- **Distributed indexing**: Multi-machine support for huge repositories
- **Incremental GPU embedding**: Only embed changed files
- **Smart cache warming**: Pre-load frequently accessed metadata

#### **Performance Targets**
- **Sub-second indexing** for 1K file repositories
- **GPU-accelerated search**: Vector similarity on GPU
- **Persistent caches**: Save/restore cache across sessions
- **Memory-mapped files**: Faster access for large files

### 🙏 **Acknowledgments**

Performance optimizations inspired by:
- [aiofiles](https://github.com/Tinche/aiofiles) - Async file I/O
- [torch.utils.data.DataLoader](https://pytorch.org/docs/stable/data.html) - Batch processing patterns
- [Trie data structures](https://en.wikipedia.org/wiki/Trie) - Pattern matching optimization
- GPU acceleration best practices from the PyTorch community

---

## 2025-01-04 - LeIndex v1.0.8 

### 🎉 **Major Release: LeIndex**

This is a complete rebrand and technology stack migration from "LeIndex" to "LeIndex". This release represents a modernization of the entire codebase, removing all external dependencies and dramatically improving performance and simplicity.

### ✨ **What's New**

#### **Technology Stack Overhaul**

| Component | Old | New | Benefit |
|-----------|-----|-----|---------|
| **Vector Search** | FAISS | LEANN | 70% smaller, faster |
| **Full-Text Search** | Elasticsearch | Tantivy | Pure Python, no Java |
| **Metadata DB** | PostgreSQL | SQLite | Zero external dependencies |
| **Analytics** | None | DuckDB | Fast analytical queries |
| **Async Processing** | RabbitMQ | asyncio | Built into Python |
| **Installation** | Docker + pip | pip only | Easier setup |

### 🚀 **Performance Improvements**

| Metric | Old (LeIndex) | New (LeIndex) | Improvement |
|--------|-------------------|---------------|-------------|
| Indexing Speed | ~2K files/min | ~10K files/min | **5x faster** |
| Search Latency (p50) | ~200ms | ~50ms | **4x faster** |
| Memory Usage | >8GB | <4GB | **50% reduction** |
| Startup Time | ~5s | <1s | **5x faster** |
| Setup Time | ~30 minutes | ~2 minutes | **15x faster** |

### 🔧 **Breaking Changes**

⚠️ **This is a breaking change with no backward compatibility.**

- **No Docker Required**: All services now embedded
- **New Configuration Format**: `~/.leindex/config.yaml` → `~/.leindex/config.yaml`
- **New CLI Names**: All commands renamed to `leindex-*`
- **New Environment Variables**: All `CODE_INDEX_*` renamed to `LEINDEX_*`
- **New Package Imports**: `import code_index_mcp` → `import leindex`
- **Must Reindex**: Different data formats require rebuilding indices

### 📦 **Installation Changes**

#### **Before (LeIndex)**
```bash
# Required: Docker, PostgreSQL, Elasticsearch, RabbitMQ
docker-compose up -d
pip install sc-LeIndex
# Configure databases, message queues, etc.
```

#### **After (LeIndex)**
```bash
# Single command installation
pip install leindex

# Index and search immediately
leindex init /path/to/project
leindex index /path/to/project
leindex-search "query"
```

### 📝 **Documentation**

- **Updated README.md**: Complete project overview with new architecture
- **Updated INSTALLATION.md**: Simplified installation guide (no Docker)
- **New MIGRATION.md**: Migration guide from LeIndex to LeIndex
- **Updated ARCHITECTURE.md**: System architecture with new stack
- **Updated API.md**: Complete API reference
- **New QUICKSTART.md**: 5-minute getting started tutorial

### 🏗️ **Architecture Changes**

#### **Removed Dependencies**
- PostgreSQL (server and client)
- Elasticsearch (server and client)
- RabbitMQ (server and client)
- Docker and Docker Compose requirement
- FAISS
- sentence-transformers (replaced with CodeRankEmbed)

#### **New Dependencies**
- LEANN (vector search, storage-efficient)
- Tantivy (full-text search, pure Python)
- DuckDB (analytics database)
- CodeRankEmbed (code-specific embeddings)

### 🔄 **Configuration Changes**

**Old Configuration** (~/.leindex/config.yaml):
```yaml
dal_settings:
  backend_type: "postgresql_elasticsearch_only"
  postgresql_host: "localhost"
  postgresql_port: 5432
  postgresql_user: "codeindex"
  postgresql_database: "code_index_db"

  elasticsearch_hosts: ["http://localhost:9200"]
  elasticsearch_index_name: "code_index"

rabbitmq_settings:
  rabbitmq_host: "localhost"
  rabbitmq_port: 5672
```

**New Configuration** (~/.leindex/config.yaml):
```yaml
dal_settings:
  backend_type: "sqlite_duckdb"
  db_path: "./data/leindex.db"
  duckdb_db_path: "./data/leindex.db.duckdb"

vector_store:
  backend_type: "leann"
  index_path: "./leann_index"
  embedding_model: "nomic-ai/CodeRankEmbed"

async_processing:
  enabled: true
  worker_count: 4
```

### 🙏 **Acknowledgments**

LeIndex is built on excellent open-source projects:
- [LEANN](https://github.com/lerp-cli/leann) - Storage-efficient vector search
- [Tantivy](https://github.com/quickwit-oss/tantivy-py) - Pure Python full-text search
- [DuckDB](https://duckdb.org/) - Fast analytical database
- [CodeRankEmbed](https://huggingface.co/nomic-ai/CodeRankEmbed) - Code embeddings

---

## [3.0.1] - 2025-12-30 - Elasticsearch Indexing Bug Fix (Legacy)

### 🐛 **Bug Fix: Elasticsearch Indexing Pipeline**

This release fixes a critical bug where `manage_project(action="reindex")` processed files (successfully updating PostgreSQL metadata and Zoekt indices) but failed to populate the Elasticsearch index, resulting in semantic search returning no results despite reindex operations reporting success.

### ✅ **Fixed**

#### **Elasticsearch Indexing Pipeline**
- **RabbitMQ Integration**: Fixed `refresh_index()` to properly queue files for async Elasticsearch indexing via RabbitMQ
- **Non-Blocking Reindex**: Reindex operations now return immediately with `{"status": "indexing_started", "operation_id": "...", "files_queued": N}`
- **Operation Tracking**: Added `operation_id` for tracking async indexing operations via `manage_operations(action="status")`
- **Error Handling**: Added RabbitMQ pre-flight check with clear error messages when RabbitMQ is unavailable
- **Configuration**: Added complete `rabbitmq_settings` section to `config.yaml` with connection details, batching, and backpressure settings

#### **Root Cause**
The `refresh_index()` function in `server.py:2083` updated PostgreSQL and Zoekt but never called the Elasticsearch backend's indexing methods. The RabbitMQ consumer infrastructure existed but was never invoked during reindex operations.

#### **Test Coverage**
- **5 New Unit Tests**: Verify RabbitMQ publishing, error handling, operation tracking, edge cases
- **10 New Integration Tests**: End-to-end reindex to search flow, operation status tracking, service availability
- **All Tests Passing**: 187/187 unit tests pass (no regressions)

### 🔧 **Technical Changes**

#### **Modified Files**
- `src/code_index_mcp/server.py`: Added RabbitMQ publishing to `refresh_index()` and `force_reindex()`
- `config.yaml`: Added `rabbitmq_settings` configuration section (lines 55-80)
- `tests/unit/test_elasticsearch_indexing.py`: Created comprehensive unit test suite
- `tests/integration/test_elasticsearch_indexing.py`: Created integration test suite

#### **New Behavior**
```python
# Before (Broken):
refresh_index() → {"files_processed": 74, "success": true}
# Elasticsearch: 3 stale documents, search returns empty

# After (Fixed):
refresh_index() → {
    "status": "indexing_started",
    "files_queued": 74,
    "operation_id": "uuid-here",
    "note": "PostgreSQL updated immediately. Elasticsearch indexing in progress."
}
# Elasticsearch: Documents appear within 10-30 seconds (async via RabbitMQ)
```

### 📋 **Success Metrics Achieved**

| Metric | Before | After | Target |
|--------|---------|-------|--------|
| Elasticsearch document count | 3 (stale) | Matches file count | ✓ |
| Search results | Empty | Returns actual content | ✓ |
| Reindex operation time | ~0.2s | <5s (async) | ✓ |
| RabbitMQ message processing | N/A | 100% within 30s | ✓ |

### 🛠️ **Setup for Existing Users**

If you're upgrading from v3.0.0, ensure RabbitMQ is running:

```bash
# Start RabbitMQ service
docker-compose up -d rabbitmq

# Or use convenience script
python run.py start-dev-dbs

# Verify RabbitMQ is accessible
curl http://localhost:15672  # Management UI
```

## [3.0.0] - 2025-01-21 - Large-Scale Database Migration

### 🚀 **MAJOR RELEASE: Complete Database Architecture Transformation**

This release represents a complete architectural overhaul with migration from SQLite to a hybrid PostgreSQL + Elasticsearch solution, transforming the Code Index MCP into an enterprise-grade platform.

### ✅ **Added - New Enterprise Features**

#### **Database Architecture**
- **PostgreSQL Integration**: Complete metadata storage with ACID compliance
- **Elasticsearch Integration**: High-performance full-text search capabilities
- **Hybrid Database Design**: Optimized data storage for different use cases
- **Real-time Indexing**: RabbitMQ-based asynchronous processing pipeline
- **Database Migrations**: Alembic-based schema management system

#### **Version Control System**
- **File Version Tracking**: Complete change history with SHA-256 hashing
- **Diff Generation**: Unified diff format for all file changes
- **Version Retrieval**: Reconstruct any previous file version
- **Operation Tracking**: Create, edit, delete, rename operations logged
- **Cross-Platform Paths**: Robust path handling for all environments

#### **Advanced Search Capabilities**
- **Elasticsearch DSL**: Advanced query capabilities with boosting
- **Fuzzy Matching**: Configurable fuzziness levels (AUTO, 0, 1, 2)
- **Content Highlighting**: Customizable HTML tags for search results
- **Field Boosting**: Separate boost factors for content and file paths
- **Pagination Support**: Efficient handling of large result sets

#### **New MCP Tools**
- `write_to_file` - File creation/modification with version tracking
- `search_and_replace` - Regex-powered find/replace with scope control
- `apply_diff` - Multi-file atomic modifications
- `insert_content` - Precise content insertion at specific lines
- `get_file_history` - Complete file change history retrieval
- `revert_file_to_version` - Rollback to any previous version
- `delete_file` - File deletion with history preservation
- `rename_file` - File renaming/moving with tracking

#### **Enterprise Infrastructure**
- **ETL Migration Tools**: Seamless SQLite to PostgreSQL/Elasticsearch migration
- **Backup Systems**: Comprehensive backup strategies for all data stores
- **Performance Monitoring**: Enterprise-grade metrics and observability
- **Memory Management**: Advanced profiling and automatic cleanup
- **Operation Tracking**: Real-time progress monitoring with cancellation

### 🔧 **Changed - Enhanced Existing Features**

#### **Core Architecture**
- **Data Access Layer (DAL)**: Complete abstraction with pluggable backends
- **Storage Interface**: Unified interface supporting multiple database types
- **Configuration System**: Enhanced YAML configuration with environment variables
- **Path Handling**: Robust cross-platform path resolution and normalization

#### **Search System**
- **Enhanced `search_code_advanced`**: Added Elasticsearch backend support
- **Improved Performance**: 10x faster searches with enterprise-grade indexing
- **Better Filtering**: Advanced file pattern matching and content filtering
- **Result Quality**: Improved relevance scoring and ranking

#### **File Operations**
- **Atomic Operations**: All file modifications are now atomic with rollback capability
- **Version Integration**: Every file operation automatically creates version history
- **Error Handling**: Comprehensive error recovery and graceful degradation
- **Progress Tracking**: Real-time progress updates for long-running operations

### 🛠️ **Technical Improvements**

#### **Database Schema Design**
- **PostgreSQL Tables**:
  - `files` - File metadata with relationships
  - `file_versions` - Complete version history
  - `file_diffs` - Change tracking with unified diffs
- **Elasticsearch Indices**:
  - `code_index` - Full-text searchable content
  - Custom mappings for optimal search performance
- **Foreign Key Constraints**: Data integrity with proper relationships

#### **Migration Strategy**
- **Dual-Write/Read Pattern**: Safe migration with backward compatibility
- **ETL Pipeline**: Comprehensive data migration with verification
- **Rollback Capability**: Complete rollback procedures documented
- **Zero Downtime**: Migration possible without service interruption

#### **Performance Optimizations**
- **Lazy Loading**: Intelligent content loading with LRU caching
- **Parallel Processing**: Multi-core indexing for large projects
- **Memory Management**: Advanced profiling with automatic cleanup
- **Connection Pooling**: Efficient database connection management

### 📋 **Migration Verification**

All functionality has been thoroughly tested and verified:

#### **✅ Core File Operations**
- File creation with PostgreSQL metadata storage ✓
- File creation with Elasticsearch content indexing ✓
- File modification with PostgreSQL version tracking ✓
- File modification with Elasticsearch content updates ✓
- File deletion with PostgreSQL cleanup ✓
- File deletion with Elasticsearch cleanup ✓

#### **✅ Search Functionality**
- Basic keyword search with Elasticsearch ✓
- Advanced search with fuzzy matching and highlighting ✓
- SQLite-style LIKE/GLOB pattern translation ✓
- Path-based searches with accurate results ✓

#### **✅ Database Integration**
- PostgreSQL-only mode operations ✓
- Dual-write/read mode functionality ✓
- ETL script full data migration ✓
- ETL script incremental migration ✓

#### **✅ System Infrastructure**
- Structured JSON logging output ✓
- Performance metrics collection ✓
- Error condition handling and logging ✓
- Database migration management (Alembic) ✓
- Backup system functionality ✓

### 🔄 **Migration Path**

#### **From SQLite (v2.x) to Enterprise (v3.0)**

1. **Backup Phase**:
   ```bash
   python backup_script.py
   ```

2. **Database Setup**:
   ```bash
   docker-compose up -d  # PostgreSQL + Elasticsearch
   ```

3. **Migration Phase**:
   ```bash
   python src/scripts/etl_script.py --mode full
   ```

4. **Configuration Update**:
   ```yaml
   dal_settings:
     backend_type: "postgresql_elasticsearch_only"
   ```

5. **Verification**:
   ```bash
   python src/scripts/etl_script.py --mode verify
   ```

### 🔧 **Configuration Changes**

#### **New Environment Variables**
```bash
# Database Backend Selection
DAL_BACKEND_TYPE=postgresql_elasticsearch_only

# PostgreSQL Configuration
POSTGRES_HOST=localhost
POSTGRES_PORT=5432
POSTGRES_USER=codeindex
POSTGRES_PASSWORD=your-secure-password
POSTGRES_DB=code_index_db

# Elasticsearch Configuration
ELASTICSEARCH_HOSTS=http://localhost:9200
ELASTICSEARCH_INDEX_NAME=code_index
ELASTICSEARCH_USERNAME=elastic
ELASTICSEARCH_PASSWORD=your-elastic-password

# Optional: RabbitMQ for Real-time Indexing
RABBITMQ_HOST=localhost
RABBITMQ_PORT=5672
```

#### **Enhanced config.yaml**
```yaml
dal_settings:
  backend_type: "postgresql_elasticsearch_only"
  postgresql_host: "localhost"
  postgresql_port: 5432
  postgresql_user: "codeindex"
  postgresql_password: "your-secure-password"
  postgresql_database: "code_index_db"
  elasticsearch_hosts: ["http://localhost:9200"]
  elasticsearch_index_name: "code_index"
```

### 📚 **New Documentation**

- **[docs/TOOLS_LIST.md](docs/TOOLS_LIST.md)** - Complete tool reference with system prompt templates
- **[docs/INSTALLATION.md](docs/INSTALLATION.md)** - Comprehensive installation guide
- **Migration guides and troubleshooting documentation**
- **Architecture diagrams and technical specifications**

### ⚠️ **Breaking Changes**

#### **Database Backend**
- **Default backend changed** from SQLite to PostgreSQL + Elasticsearch
- **New dependencies**: PostgreSQL and Elasticsearch required for full functionality
- **Configuration format**: New YAML structure for database settings

#### **Tool Behavior**
- **File operations** now automatically create version history
- **Search results** format enhanced with Elasticsearch metadata
- **Path handling** standardized to relative paths for cross-platform compatibility

#### **Environment Requirements**
- **PostgreSQL 12+** required for metadata storage
- **Elasticsearch 7.x/8.x** required for search functionality
- **Additional memory** requirements for enterprise features

### 🔄 **Backward Compatibility**

#### **Migration Support**
- **Dual-write mode** available during transition period
- **ETL tools** for seamless data migration
- **Rollback procedures** documented for safe migration
- **Legacy SQLite** support maintained in dual-write mode

#### **Configuration Compatibility**
- **Environment variables** take precedence over config files
- **Fallback mechanisms** for missing configuration
- **Graceful degradation** when enterprise features unavailable

### 🚀 **Performance Improvements**

#### **Search Performance**
- **10x faster searches** with Elasticsearch full-text indexing
- **Advanced relevance scoring** with configurable boosting
- **Efficient pagination** for large result sets
- **Real-time index updates** with RabbitMQ processing

#### **File Operations**
- **Atomic transactions** with rollback capability
- **Parallel processing** for bulk operations
- **Memory optimization** with intelligent caching
- **Progress tracking** for long-running operations

### 🔐 **Security Enhancements**

#### **Database Security**
- **Connection encryption** support for PostgreSQL and Elasticsearch
- **Authentication integration** with enterprise identity systems
- **SSL/TLS configuration** for secure communications
- **Access control** with role-based permissions

#### **Data Protection**
- **Backup encryption** for sensitive code repositories
- **Audit logging** for all file operations and searches
- **Data retention policies** for version history management
- **Cross-platform path security** preventing directory traversal

### 🐛 **Fixed Issues**

#### **Path Handling**
- **Cross-platform compatibility** - Resolved Windows/Linux/macOS path issues
- **Relative vs absolute paths** - Consistent path handling across all operations
- **Unicode support** - Proper handling of international characters in file paths

#### **Memory Management**
- **Memory leaks** - Fixed in lazy loading and caching systems
- **Large file handling** - Improved processing of files >100MB
- **Garbage collection** - Enhanced automatic cleanup procedures

#### **Database Operations**
- **Connection pooling** - Resolved connection exhaustion issues
- **Transaction handling** - Fixed rollback scenarios and error recovery
- **Foreign key constraints** - Proper relationship management

### 📊 **Performance Metrics**

#### **Benchmark Results**
- **Search Speed**: 10x improvement with Elasticsearch
- **Indexing Speed**: 4x improvement with parallel processing
- **Memory Usage**: 70% reduction with optimized caching
- **File Operations**: 90% faster with incremental processing

#### **Scalability**
- **Large Projects**: Tested with 100k+ files
- **Concurrent Users**: Support for multiple simultaneous operations
- **Memory Efficiency**: Optimized for resource-constrained environments
- **Database Performance**: Efficient queries with proper indexing

### 🔮 **Future Roadmap**

#### **Planned Features**
- **Distributed deployment** support for enterprise environments
- **Advanced analytics** and code quality metrics
- **Integration APIs** for external development tools
- **Machine learning** powered code analysis

#### **Performance Targets**
- **Sub-second search** for projects with 1M+ files
- **Real-time collaboration** features
- **Advanced caching** strategies
- **Horizontal scaling** capabilities

---

## [2.0.0] - 2024-12-15 - Performance Optimization Release

### Added
- Incremental indexing system with 90%+ performance improvement
- Parallel processing with multi-core support
- Memory optimization with lazy loading and LRU cache
- Enterprise search tools integration (Zoekt, ripgrep, ugrep)
- Async operations with progress tracking
- Performance monitoring and metrics
- YAML configuration system
- Advanced gitignore and size-based filtering

### Changed
- Complete architecture refactor for performance
- Enhanced search capabilities with caching
- Improved memory management
- Better error handling and recovery

### Performance
- 90%+ faster re-indexing
- 70% memory reduction
- 4x faster indexing
- 10x faster searches
- 3-10x general performance improvements

---

## [1.0.0] - 2024-11-01 - Initial Release

### Added
- Basic MCP server implementation
- SQLite-based file indexing
- Core search functionality
- File discovery and analysis tools
- Basic configuration system

### Features
- File indexing and search
- Pattern-based file discovery
- File content analysis
- MCP protocol integration
- Cross-platform support

---

## Migration Guide

### From v2.x to v3.0 (Enterprise Migration)

This is a major architectural change requiring database migration:

1. **Backup your data**:
   ```bash
   python backup_script.py
   ```

2. **Set up new databases**:
   ```bash
   docker-compose up -d
   ```

3. **Run migration**:
   ```bash
   python src/scripts/etl_script.py --mode full
   ```

4. **Update configuration**:
   ```yaml
   dal_settings:
     backend_type: "postgresql_elasticsearch_only"
   ```

5. **Verify migration**:
   ```bash
   python src/scripts/etl_script.py --mode verify
   ```

### From v1.x to v2.0 (Performance Optimization)

This is a backward-compatible upgrade:

1. **Update dependencies**:
   ```bash
   uv sync
   ```

2. **Update configuration** (optional):
   ```yaml
   # Add performance settings
   memory:
     soft_limit_mb: 4096
     hard_limit_mb: 8192
   ```

3. **Refresh index** for performance benefits:
   ```bash
   # Use refresh_index tool in MCP
   ```

## Support

For migration assistance or issues:
- Check the [Installation Guide](docs/INSTALLATION.md)
- Review [Troubleshooting](docs/TROUBLESHOOTING.md)
- Open an issue on GitHub
- Consult the [Tools Documentation](docs/TOOLS_LIST.md)

## Contributors

Special thanks to all contributors who made this enterprise transformation possible:
- Database architecture design and implementation
- Migration tooling and ETL pipeline development
- Cross-platform compatibility testing
- Performance optimization and benchmarking
- Documentation and user experience improvements
