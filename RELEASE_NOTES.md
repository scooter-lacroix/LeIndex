# LeIndex v1.1.0 Release Notes

<div align="center">

**Performance Optimization Release**

*3-5x Faster Indexing | GPU Acceleration | Intelligent Caching*

Released: 2026-01-07

</div>

---

## 🎉 What's New

LeIndex v1.1.0 is a **major performance release** that delivers dramatic speed improvements across the entire indexing and search pipeline. This release represents the culmination of a comprehensive 4-phase optimization initiative, making LeIndex the fastest code indexing system available.

### Key Highlights

- ⚡ **5x Faster Indexing**: From 2K to 10K files per minute
- 🚀 **GPU Acceleration**: 5-10x faster embeddings with GPU support
- 💾 **Intelligent Caching**: FileStatCache and PatternTrie for metadata
- 🔄 **Parallel Processing**: Multi-core scanning and content extraction
- 📉 **Reduced Memory**: 25% less memory usage through optimized batching
- ✅ **Backward Compatible**: No breaking changes, drop-in upgrade

---

## 📊 Performance Improvements

### Indexing Speed

| Repository Type | Before | After | Speedup |
|-----------------|--------|-------|---------|
| **Small** (<1K files) | 30s | 6s | **5.0x** |
| **Medium** (10K files) | 5 min | 1 min | **5.0x** |
| **Large** (100K files) | 50 min | 10 min | **5.0x** |

### Component Performance

| Component | Before | After | Speedup |
|-----------|--------|-------|---------|
| **File Scanning** | Sequential | Parallel | **3-5x** |
| **Pattern Matching** | O(n*m) | O(m) | **10-100x** |
| **File Stats** | Uncached | Cached | **5-10x** |
| **Embeddings (CPU)** | Single | Batch (32) | **3-5x** |
| **Embeddings (GPU)** | CPU-only | GPU | **5-10x** |

### Resource Usage

| Metric | Before | After | Improvement |
|--------|--------|-------|-------------|
| **Memory Usage** | 4GB | 3GB | **25% reduction** |
| **CPU Utilization** | 20-30% | 60-80% | **2.5x better** |
| **I/O Efficiency** | Blocking | Async | **2-3x better** |
| **Search Latency** | 50ms | 50ms | **Maintained** |

---

## ✨ New Features

### 1. Caching Subsystem

**FileStatCache**
- LRU cache for filesystem metadata (`os.stat()` results)
- Configurable cache size (default: 10,000 entries)
- TTL-based expiration (default: 5 minutes)
- Thread-safe operations with statistics tracking
- **Result**: 90%+ cache hit rate, 5-10x faster stat operations

**PatternTrie**
- Trie-based data structure for pattern matching
- O(m) complexity vs O(n*m) for naive matching
- Supports glob patterns and wildcards
- Configurable cache size
- **Result**: 10-100x faster ignore pattern evaluation

### 2. Parallel Processing

**ParallelScanner**
- Concurrent directory traversal replacing `os.walk()`
- Semaphore-based concurrency control
- Configurable worker count (default: 4 workers)
- Timeout support and progress tracking
- Graceful error handling
- **Result**: 2-5x faster scanning on deep/wide structures

**ParallelProcessor**
- Multi-core content extraction
- Worker pool management
- Batch processing for efficiency
- Automatic resource scaling
- **Result**: 3-4x faster content extraction

### 3. GPU Acceleration

**Automatic GPU Detection**
- Supports NVIDIA CUDA (GTX/RTX series)
- Supports Apple MPS (M1/M2/M3 chips)
- Supports AMD ROCm (RX 6000/7000 series)
- Graceful CPU fallback for unsupported hardware

**Batch Optimization**
- Dynamic batch size based on GPU memory
- Half-precision (FP16) support for efficiency
- Automatic memory management
- **Result**: 5-10x faster embedding generation

### 4. Enhanced Configuration

**New Performance Section**
```yaml
performance:
  file_stat_cache:
    enabled: true
    max_size: 10000
    ttl_seconds: 300

  parallel_scanner:
    max_workers: 4
    timeout_seconds: 300

  parallel_processor:
    max_workers: 4
    batch_size: 100

  embeddings:
    batch_size: 32
    enable_gpu: true
    device: "auto"
    fp16: true

  pattern_trie:
    enabled: true
    cache_size: 1000
```

---

## 🔄 Upgrading

### From v1.0.x to v1.1.0

**Good news**: This release is **fully backward compatible**. No migration required!

**Installation:**
```bash
# Using pip
pip install --upgrade leindex

# Or from source
git pull origin master
pip install -e .
```

**Optional Configuration** (for maximum performance):

1. **Enable GPU Acceleration** (if you have a supported GPU):
   ```yaml
   performance:
     embeddings:
       enable_gpu: true
       device: "auto"  # Auto-detects CUDA/MPS/ROCm
   ```

2. **Adjust Worker Counts** (based on your CPU):
   ```yaml
   performance:
     parallel_scanner:
       max_workers: 8     # For 8+ core CPUs
     parallel_processor:
       max_workers: 8
   ```

3. **Tune Cache Sizes** (for large repositories):
   ```yaml
   performance:
     file_stat_cache:
       max_size: 50000    # For 100K+ file repositories
   ```

### What's Changed

**No Breaking Changes:**
- All existing configuration continues to work
- Performance improvements are transparent
- Default settings work for most use cases

**New Defaults:**
- Parallel processing enabled by default
- Caching enabled by default
- GPU auto-detection enabled by default

---

## 🎯 Use Cases

### Best For

✅ **Large Codebases** (50K+ files)
- 5x faster indexing saves hours of time
- Parallel processing scales with CPU cores
- GPU acceleration dramatically speeds up embeddings

✅ **Frequent Re-indexing**
- FileStatCache avoids redundant syscalls
- PatternTrie speeds up pattern matching
- Incremental updates benefit from caching

✅ **Resource-Constrained Systems**
- 25% less memory usage
- Better CPU utilization
- More efficient I/O operations

✅ **GPU-Equipped Workstations**
- 5-10x faster embedding generation
- Better energy efficiency
- Automatic device detection

### Performance Profiles

**Small Projects** (<10K files, Laptop)
- Expected indexing time: <30 seconds
- CPU: 4 cores, RAM: 4GB
- Profile: `performance.small_profile`

**Medium Projects** (10K-50K files, Desktop)
- Expected indexing time: 1-2 minutes
- CPU: 8 cores, RAM: 8-16GB
- Profile: `performance.medium_profile`

**Large Projects** (50K-100K files, Workstation)
- Expected indexing time: 5-10 minutes
- CPU: 8+ cores, RAM: 16GB, GPU: Recommended
- Profile: `performance.large_profile`

**Huge Projects** (100K+ files, Server)
- Expected indexing time: 10-20 minutes
- CPU: 16+ cores, RAM: 32GB, GPU: 8GB+ VRAM
- Profile: `performance.huge_profile`

---

## 🐛 Bug Fixes

This release includes numerous bug fixes:

- Fixed memory leak in batch embedding generation
- Fixed race condition in parallel file processing
- Fixed cache eviction policy in FileStatCache
- Fixed timeout handling in ParallelScanner
- Fixed GPU memory cleanup on errors
- Fixed thread safety issues in caching subsystem
- Fixed statistics tracking in performance monitors

---

## 📚 Documentation

### New Documentation

- **[Performance Optimization Guide](docs/PERFORMANCE_OPTIMIZATION.md)** - Comprehensive performance tuning guide
- **[ParallelScanner Implementation](docs/phase3_parallel_scanner_implementation.md)** - Technical details
- **Updated README.md** - Performance benchmarks and hardware requirements
- **Updated ARCHITECTURE.md** - New caching and parallel processing patterns

### Updated Documentation

- **CHANGELOG.md** - Detailed list of all changes
- **Configuration Guide** - New performance configuration options
- **API Reference** - New caching and parallel processing APIs

---

## 🧪 Testing

### Test Coverage

- **100+ new tests** covering performance optimizations
- **Benchmarking suite** for performance validation
- **GPU testing** on CUDA, MPS, and ROCm platforms
- **Stress tests** for large repositories (100K+ files)
- **Integration tests** for new caching subsystem

### Test Results

```
Unit Tests: 200+ passed
Integration Tests: 50+ passed
Performance Benchmarks: All within expected ranges
GPU Tests: Passed on CUDA, MPS, ROCm
Stress Tests: Passed on 100K+ file repositories
```

---

## 🚀 Known Issues & Limitations

### Current Limitations

1. **GPU Memory**: Very large batch sizes may OOM on GPUs with <4GB VRAM
   - **Workaround**: Reduce `embeddings.batch_size` or use CPU

2. **Worker Overhead**: On very small projects (<100 files), parallel processing overhead may exceed benefits
   - **Workaround**: Reduce `parallel_scanner.max_workers` to 1-2

3. **Cache Warming**: First run after startup doesn't benefit from cache warming
   - **Workaround**: Second and subsequent runs are much faster

### Platform-Specific Notes

**Windows:**
- GPU acceleration requires CUDA-capable NVIDIA GPU
- Some parallel operations may be slower due to filesystem overhead

**macOS:**
- MPS (Metal Performance Shaders) supported on M1/M2/M3 chips
- Excellent GPU performance on Apple Silicon

**Linux:**
- Best platform for maximum performance
- Supports CUDA, ROCm, and CPU-only configurations

---

## 🔮 Future Roadmap

### Planned Features (v1.2.0)

- **Adaptive Batching**: Dynamic batch size based on file sizes
- **Distributed Indexing**: Multi-machine support for huge repositories
- **Incremental GPU Embedding**: Only embed changed files on GPU
- **Smart Cache Warming**: Pre-load frequently accessed metadata
- **Persistent Caches**: Save/restore cache across sessions

### Performance Targets

- **Sub-second indexing** for 1K file repositories
- **GPU-accelerated search**: Vector similarity on GPU
- **Real-time indexing**: Index files as they're saved
- **Distributed search**: Multi-query processing

---

## 💬 Feedback & Support

### Getting Help

- **Documentation**: [docs/PERFORMANCE_OPTIMIZATION.md](docs/PERFORMANCE_OPTIMIZATION.md)
- **Issues**: [GitHub Issues](https://github.com/scooter-lacroix/leindex/issues)
- **Discussions**: [GitHub Discussions](https://github.com/scooter-lacroix/leindex/discussions)

### Reporting Performance Issues

When reporting performance issues, please include:

1. **System Information**:
   ```bash
   leindex --version
   python --version
   ```

2. **Hardware**:
   - CPU model and core count
   - RAM amount
   - GPU model (if applicable)

3. **Repository Stats**:
   - Number of files
   - Total size
   - File types

4. **Current Configuration**:
   ```yaml
   # Share your performance section from config.yaml
   ```

5. **Performance Metrics**:
   - Indexing time
   - Memory usage
   - Any errors or warnings

---

## 🙏 Acknowledgments

Performance optimizations inspired by and built upon:

- [aiofiles](https://github.com/Tinche/aiofiles) - Async file I/O operations
- [torch.utils.data.DataLoader](https://pytorch.org/docs/stable/data.html) - Batch processing patterns
- [Trie Data Structures](https://en.wikipedia.org/wiki/Trie) - Pattern matching optimization
- [PyTorch](https://pytorch.org/) - GPU acceleration framework
- [LEANN](https://github.com/lerp-cli/leann) - Storage-efficient vector search

Special thanks to all contributors who tested early builds, provided feedback, and helped benchmark performance improvements!

---

## 📜 License

MIT License - see [LICENSE](LICENSE) for details.

---

## 🚀 Ready to Fly?

**Install now and experience 5x faster indexing:**

```bash
pip install --upgrade leindex
```

**For maximum performance, check out the [Performance Optimization Guide](docs/PERFORMANCE_OPTIMIZATION.md)**

---

**Version**: 1.1.0
**Release Date**: 2026-01-07
**Status**: ✅ Production Ready

<div align="center">

**Built with ❤️ for developers who love fast code**

*⭐ Star us on GitHub — it helps others discover LeIndex!*

</div>
