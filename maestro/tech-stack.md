# Technology Stack: LeIndex

## Core Philosophy

**LeIndex detects hardware and installs the correct PyTorch variant. ALWAYS.**

We NEVER install NVIDIA torch on AMD systems, or vice versa. Hardware detection is robust, accurate, and respects existing PyTorch installations.

---

## Primary Language

### Python 3.10-3.13
- **Core implementation language**
- Minimum version: Python 3.10
- Maximum tested version: Python 3.13 (3.14 not supported by leann-backend-hnsw)
- **Requirement:** Python 3.10+ required for async/await and type hint features used throughout

---

## GPU Support & Hardware Acceleration (CRITICAL) 🎮

### PyTorch with GPU Variants

**CRITICAL REQUIREMENT:** LeIndex installers MUST detect hardware and install the appropriate PyTorch variant:

1. **NVIDIA GPUs** → Install `torch` with CUDA support
2. **AMD GPUs** → Install `torch` with ROCm support
3. **Apple Silicon (M1/M2/M3)** → Install `torch` with MPS support
4. **Intel/Other GPUs** → Install CPU-optimized `torch` (fallback)
5. **No GPU** → Install CPU-optimized `torch` (fallback)

**Hardware Detection Logic:**

```bash
# Detection priority (check in this order):
1. Check if PyTorch already installed → Use existing installation
2. Check for NVIDIA GPU (nvidia-smi) → Install CUDA torch
3. Check for AMD GPU (rocm-smi) → Install ROCm torch
4. Check for Apple Silicon → Install MPS-enabled torch
5. No GPU detected → Install CPU-optimized torch
```

**CRITICAL: NEVER install wrong torch variant**
- ❌ NEVER install NVIDIA torch on AMD systems
- ❌ NEVER install CUDA dependencies if ROCm is appropriate
- ❌ NEVER override existing PyTorch installation
- ✅ ALWAYS detect hardware first
- ✅ ALWAYS respect existing torch installation
- ✅ ALWAYS fall back to CPU if GPU detection fails

**Installation Strategy:**
- Use `torch` >= 2.0.0 for all variants
- CUDA version: Match system CUDA (detect with `nvidia-smi`)
- ROCm version: Use latest stable ROCm torch
- CPU version: Use PyTorch CPU build for maximum compatibility
- **NO constraint-dependencies blocking GPU packages** (removed from pyproject.toml)

---

## Vector Search & Embeddings

### LEANN (≥0.3.5, <0.4.0)
- **Purpose:** Storage-efficient vector similarity search
- **Algorithms:** HNSW (Hierarchical Navigable Small World) and DiskANN
- **Why LEANN over FAISS:** Zero external dependencies, storage-efficient, fast indexing
- **GPU Acceleration:** LEANN embeddings use GPU when available (via PyTorch)

### sentence-transformers (≥2.2.0)
- **Purpose:** Generate code embeddings for semantic search
- **Model:** CodeRankEmbed (nomic-ai/CodeRankEmbed)
- **Embedding Dimension:** 768-dimensional vectors
- **GPU Acceleration:** Automatic GPU inference when PyTorch has CUDA/ROCm/MPS support
- **CPU Fallback:** CPU inference when no GPU available (slower but works everywhere)

### PyTorch (≥2.0.0) - GPU or CPU
- **Purpose:** ML framework for embedding generation
- **Variants:** CUDA (NVIDIA), ROCm (AMD), MPS (Apple Silicon), CPU (fallback)
- **Detection:** Hardware-aware installer selects correct variant
- **Performance:** 5-10x faster embedding generation on GPU vs CPU
- **Memory:** GPU reduces memory pressure during batch embedding

### einops (≥0.6.0)
- **Purpose:** Elegant tensor operations for CodeRankEmbed model
- **Usage:** Tensor reshaping and transformations in embedding pipeline

### numpy (≥1.24.0)
- **Purpose:** Numerical computing and array operations
- **Integration:** Works with PyTorch tensors for data preprocessing

---

## Full-Text Search

### Tantivy (≥0.20.0)
- **Purpose:** Rust-powered Lucene full-text search backend
- **Why Tantivy over Elasticsearch:** Pure Python wrapper, zero infrastructure, fast
- **Features:**
  - Token-based search with regex support
  - Case-sensitive and insensitive search
  - Context-aware result highlighting (with custom tag support)
  - Fielded search (file name, path, content)
  - Relevance boosting (content and filepath multipliers)
- **Performance:** Millisecond search times even for large codebases

---

## Data Storage

### SQLite (built-in)
- **Purpose:** ACID-compliant metadata storage
- **Usage:**
  - File metadata (path, size, mtime, hash)
  - Index metadata (file count, last indexed time)
  - Search history and statistics
  - Key-value storage for configuration
- **Why SQLite:** Zero setup, embedded, battle-tested, ACID compliant
- **Performance:** Sufficient for metadata workload (not high-concurrency writes)

### DuckDB (≥1.0.0)
- **Purpose:** In-memory analytical query engine
- **Usage:**
  - Aggregate statistics on codebase
  - Complex analytics queries
  - Performance monitoring dashboards
  - Export and reporting
- **Why DuckDB:** In-process, fast analytics, SQL-compatible
- **Integration:** Reads SQLite metadata for analysis

### MessagePack (≥1.0.0)
- **Purpose:** Efficient binary serialization for file indexes
- **Usage:**
  - Serialize in-memory file index to disk
  - Fast load/save of index data
  - More compact than JSON, faster than pickle
- **Why MessagePack:** Speed, size, and Python-native support

---

## Async & Networking

### asyncio (built-in)
- **Purpose:** Async/await framework for non-blocking I/O
- **Usage:**
  - All file I/O operations
  - Database operations
  - Indexing pipeline
  - MCP server request handling
- **Requirement:** No blocking calls in async functions (use `asyncio.to_thread()` for CPU work)

### aiofiles (≥23.2.1)
- **Purpose:** Async file operations
- **Usage:** Reading file contents without blocking event loop

### aiohttp (≥3.9.0)
- **Purpose:** Async HTTP client/server
- **Usage:** MCP server HTTP endpoints (if applicable)

### httpx (≥0.28.1)
- **Purpose:** Modern async HTTP client
- **Usage:** HTTP requests with async/await support

---

## MCP Integration

### MCP SDK (≥0.3.0)
- **Purpose:** Model Context Protocol server implementation
- **Usage:**
  - Expose LeIndex tools to AI assistants
  - Server lifecycle management
  - Tool request/response handling
  - STDIO transport for Claude Code integration

---

## Document Processing

### python-docx (≥1.2.0)
- **Purpose:** Parse DOCX (Word) files
- **Usage:** Extract text content from .docx files for indexing

### pdfminer-six (≥20250506)
- **Purpose:** Extract text from PDF files
- **Usage:** Parse PDF documents and index text content

### lxml (≥6.0.0)
- **Purpose:** XML/HTML parsing
- **Usage:** Parse markup languages for documentation files

---

## System & Utilities

### psutil (≥7.0.0)
- **Purpose:** System and process utilities
- **Usage:**
  - Monitor memory usage during indexing
  - Detect system resources (CPU, RAM)
  - Kill stuck subprocesses
  - File system operations

### PyYAML (≥6.0.0)
- **Purpose:** YAML configuration parsing
- **Usage:**
  - Load LeIndex configuration files
  - Parse ignore patterns
  - User settings management

### python-dateutil (≥2.9.0)
- **Purpose:** Date/time utilities
- **Usage:**
  - Parse timestamps
  - Calculate time deltas
  - Format dates for logging

---

## Testing Framework

### pytest (≥8.4.1)
- **Purpose:** Testing framework
- **Usage:**
  - Unit tests for individual functions
  - Integration tests for components
  - End-to-end tests for MCP server

### pytest-asyncio (≥1.3.0)
- **Purpose:** Async test support
- **Usage:** Test async/await functions with pytest

---

## Security & Cryptography

### cryptography (≥45.0.0)
- **Purpose:** Cryptographic recipes and primitives
- **Usage:**
  - Secure hash generation
  - File integrity verification
  - Encryption if needed

### cffi (≥1.17.0)
- **Purpose:** C Foreign Function Interface for Python
- **Usage:** Required by cryptography package

### pycparser (≥2.22)
- **Purpose:** C parser for cryptography
- **Usage:** Required by cryptography package

---

## Performance Optimization

### greenlet (≥3.2.0)
- **Purpose:** Lightweight concurrent computing
- **Usage:** Asynchronous I/O optimization

### Mako (≥1.3.0)
- **Purpose:** Template engine
- **Usage:** Code generation if needed

### MarkupSafe (≥3.0.0)
- **Purpose:** Safe string/unicode handling
- **Usage:** Required by Mako

---

## Build & Packaging

### setuptools (≥61.0)
- **Purpose:** Package building and distribution
- **Usage:**
  - Build LeIndex package
  - Manage dependencies
  - Package installation

### wheel
- **Purpose:** Distribution format
- **Usage:** Built distribution format for faster installs

---

## Search Backends (External Tools)

### Command-Line Search Tools (Optional Fallbacks)
LeIndex can use external search tools if primary backends fail:

- **ripgrep (rg)** - Fastest grep alternative (recommended)
- **ugrep (ug)** - Universal grep with fuzzy search
- **The Silver Searcher (ag)** - Fast code search
- **grep** - Standard Unix grep (always available as fallback)

These tools are invoked via subprocess with 30-second timeouts to prevent hangs.

**Note:** LeIndex does not depend on these tools—they are optional fallbacks if LEANN/Tantivy are unavailable.

---

## GPU Detection Implementation

### Install Script Logic (CRITICAL)

```bash
#!/bin/bash
# Hardware-aware PyTorch installation

# 1. Check if PyTorch already installed
if python -c "import torch" 2>/dev/null; then
    echo "✓ PyTorch already installed, using existing version"
    # Validate existing installation works
    python -c "import torch; print('PyTorch:', torch.__version__, 'CUDA:', torch.cuda.is_available())"
    exit 0
fi

# 2. Detect GPU hardware
if command -v nvidia-smi &> /dev/null; then
    # NVIDIA GPU detected
    echo "✓ NVIDIA GPU detected, installing CUDA-enabled PyTorch"
    pip install torch --index-url https://download.pytorch.org/whl/cu118

elif command -v rocm-smi &> /dev/null; then
    # AMD GPU detected
    echo "✓ AMD GPU detected, installing ROCm-enabled PyTorch"
    pip install torch --index-url https://download.pytorch.org/whl/rocm5.6

elif [[ "$(uname -m)" == "arm64" && "$(uname)" == "Darwin" ]]; then
    # Apple Silicon detected
    echo "✓ Apple Silicon detected, installing MPS-enabled PyTorch"
    pip install torch

else
    # No GPU detected
    echo "✓ No GPU detected, installing CPU-optimized PyTorch"
    pip install torch --index-url https://download.pytorch.org/whl/cpu
fi
```

**CRITICAL REQUIREMENTS:**
- ✅ Check for existing PyTorch FIRST (never override user installation)
- ✅ Detect hardware BEFORE installing (no blind NVIDIA installs)
- ✅ Match CUDA version to system (detect with `nvidia-smi`)
- ✅ Use ROCm for AMD systems (NEVER install CUDA torch on AMD)
- ✅ Clear logging of what's being installed and why
- ❌ NEVER install NVIDIA packages on AMD systems
- ❌ NEVER install CUDA dependencies if ROCm is appropriate
- ❌ NEVER override existing PyTorch without asking

---

## Architecture Diagram

```
┌─────────────────────────────────────────────────────────┐
│              LeIndex Technology Stack                    │
├─────────────────────────────────────────────────────────┤
│                                                         │
│  ┌──────────────┐     ┌─────────────┐     ┌──────────┐│
│  │   MCP Server │◀──▶│ Core Engine │◀──▶│   LEANN  ││
│  │  (mcp>=0.3)  │     │  (asyncio)  │     │(HNSW/DiskANN)│
│  └──────────────┘     └─────────────┘     └──────────┘│
│         │                     │                    │      │
│         ▼                     ▼                    ▼      │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────┐│
│  │ CLI Tools    │     │   Tantivy    │     │ PyTorch  ││
│  │  (sys.argv)  │     │  (Lucene)    │     │(GPU/CPU) ││
│  └──────────────┘     └──────────────┘     └──────────┘│
│         │                     │                    │      │
│         ▼                     ▼                    ▼      │
│  ┌──────────────┐     ┌──────────────┐     ┌──────────┐│
│  │   SQLite     │     │   DuckDB     │     │ MessagePack││
│  │  (Metadata)  │     │  (Analytics) │     │ (Serialization)││
│  └──────────────┘     └──────────────┘     └──────────┘│
│                                                         │
└─────────────────────────────────────────────────────────┘
```

**Key:**
- **GPU Acceleration:** PyTorch provides GPU inference for LEANN embeddings
- **Async Throughout:** All I/O uses asyncio (no blocking operations)
- **Zero Dependencies:** No Docker, PostgreSQL, Elasticsearch, RabbitMQ
- **Local-First:** Everything runs on user's machine, no cloud services

---

## Dependency Management Strategy

### Installation Methods

**1. pip install (Recommended)**
```bash
pip install leindex
```
- Automatically detects hardware and installs correct PyTorch variant
- All dependencies bundled
- Works on any system with Python 3.10+

**2. Install Scripts**
```bash
# Linux/Unix
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.sh | bash

# macOS
curl -sSL https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install_macos.sh | bash

# Windows
irm https://raw.githubusercontent.com/scooter-lacroix/LeIndex/master/install.ps1 | iex
```
- Hardware detection built-in
- Installs correct PyTorch variant for your GPU
- Never installs wrong torch variant

### No Manual Dependency Installation

**Users should NEVER need to:**
- Manually install PyTorch
- Choose between CUDA/ROCm/MPS variants
- Install GPU drivers separately (assumed already installed)
- Configure environment variables for GPU detection
- Worry about dependency conflicts

**Installers handle EVERYTHING automatically.**

---

## Performance Characteristics

### GPU vs CPU Performance

| Operation | CPU Only | GPU (NVIDIA/AMD/Apple) | Speedup |
|-----------|----------|------------------------|---------|
| LEANN Embeddings (1K files) | ~30s | ~3-5s | **6-10x** |
| LEANN Embeddings (10K files) | ~5min | ~30s | **10x** |
| LEANN Embeddings (50K files) | ~25min | ~2.5min | **10x** |
| Tantivy Search | <100ms | <100ms | No change (CPU-bound) |
| SQLite Metadata | <10ms | <10ms | No change (I/O-bound) |

**Conclusion:** GPU acceleration provides **10x speedup for embedding generation**, which is the bottleneck for large codebases. This is why robust GPU detection is CRITICAL.

---

## Future Considerations

### Optional GPU Enhancements (Out of Scope for Now)

These are NOT current priorities but could be considered later:

1. **Multi-GPU Support** - Distribute embeddings across multiple GPUs
2. **GPU Memory Management** - Batch size optimization for GPU memory
3. **Mixed Precision** - Use FP16 on GPU for 2x speedup (requires testing)
4. **GPU-Aware Scheduling** - Prioritize large files for GPU embedding

**Current Focus:** Robust single-GPU detection and correct PyTorch variant installation.
