# Memory Management in LePasserelle

This document describes the memory management features implemented in LePasserelle, including cache spilling, reloading, and warming strategies.

## Overview

LePasserelle implements intelligent memory management to handle large codebases efficiently. The system monitors memory usage and automatically spills caches to disk when thresholds are exceeded, then reloads them on demand.

## Architecture

### Components

1. **MemoryManager** - Monitors process RSS memory and system memory
2. **CacheSpiller** - Manages cache spilling and restoration
3. **CacheStore** - Stores spilled cache entries on disk
4. **LeIndex** - Orchestrates cache operations

### Cache Types

- **PDG Cache** - Program Dependence Graph (code structure and relationships)
- **Vector Cache** - HNSW search index (semantic search vectors)
- **Analysis Cache** - Natural language query results

## Cache Spilling

### Automatic Spilling

When memory usage exceeds the configured threshold (default: 85% of system memory), the system automatically spills caches:

```rust
use lepasserelle::LeIndex;

let mut leindex = LeIndex::new("/path/to/project")?;

// Check memory and spill if needed (returns true if spilled)
if leindex.check_memory_and_spill()? {
    println!("Caches were automatically spilled");
}
```

### Manual Spilling

You can manually spill caches to free memory before large operations:

```rust
// Spill PDG cache only
leindex.spill_pdg_cache()?;

// Spill vector cache only
leindex.spill_vector_cache()?;

// Spill all caches
let (pdg_bytes, vector_bytes) = leindex.spill_all_caches()?;
```

### How Spilling Works

1. **PDG Spilling**: The PDG is already persisted to lestockage (SQLite). Spilling simply:
   - Removes the PDG from memory
   - Creates a marker entry in the cache store
   - The actual data remains in lestockage for later reloading

2. **Vector Spilling**: The HNSW vector index is rebuilt from the PDG when needed:
   - Creates a marker entry in the cache store
   - Removes the vector index from memory
   - On reload, rebuilds the index by re-processing PDG nodes

## Cache Reloading

### Automatic Reloading

Caches are automatically reloaded when accessed:

```rust
// If PDG was spilled, it's automatically reloaded from lestockage
leindex.search("function call")?;
```

### Manual Reloading

You can explicitly reload caches:

```rust
// Reload PDG from lestockage
leindex.reload_pdg_from_cache()?;

// Rebuild vector index from PDG
let node_count = leindex.reload_vector_from_pdg()?;
```

## Cache Warming

Warm caches proactively load frequently accessed data:

```rust
use lepasserelle::memory::WarmStrategy;

// Warm all caches
leindex.warm_caches(WarmStrategy::All)?;

// Warm PDG only
leindex.warm_caches(WarmStrategy::PDGOnly)?;

// Warm search index only
leindex.warm_caches(WarmStrategy::SearchIndexOnly)?;

// Warm recently used caches first
leindex.warm_caches(WarmStrategy::RecentFirst)?;
```

### Warm Strategies

- **All** - Load both PDG and vector cache
- **PDGOnly** - Load only the Program Dependence Graph
- **SearchIndexOnly** - Load only the vector search index
- **RecentFirst** - Prioritize recently accessed cache entries

## Cache Statistics

Monitor cache usage:

```rust
let stats = leindex.get_cache_stats()?;

println!("Cache entries: {}", stats.cache_entries);
println!("Cache bytes: {}", stats.cache_bytes);
println!("Spilled entries: {}", stats.spilled_entries);
println!("Spilled bytes: {}", stats.spilled_bytes);
```

## Configuration

Configure memory management behavior:

```rust
use lepasserelle::memory::MemoryConfig;
use std::path::PathBuf;

let config = MemoryConfig {
    // Directory for spilled cache files
    cache_dir: PathBuf::from("/tmp/leindex_cache"),

    // Maximum cache size before spilling (default: 500MB)
    max_cache_bytes: 1_000_000_000,

    // Memory threshold for automatic spilling (default: 0.85 = 85%)
    spill_threshold: 0.90,

    // Check interval in seconds (default: 30)
    check_interval_secs: 60,

    // Enable automatic spilling (default: true)
    auto_spill: true,
};
```

## Performance Considerations

### Memory Reduction

- **PDG Memory**: ~32 bytes per node (vs 400+ bytes in Python)
- **Vector Memory**: Depends on embedding dimension (typically 512-1536 floats per node)
- **Overall**: 10x memory reduction compared to Python implementation

### Spilling Overhead

- **PDG Spilling**: Negligible (data already in lestockage)
- **Vector Spilling**: Fast (just creates marker)
- **PDG Reloading**: Medium speed (SQLite read)
- **Vector Rebuilding**: Slower (requires re-indexing all nodes)

### Best Practices

1. **Pre-warm caches** before heavy search operations
2. **Spill caches** before running other memory-intensive processes
3. **Use RecentFirst warming** for interactive workflows
4. **Monitor cache stats** to understand your usage patterns

## Error Handling

Cache operations return `Result` types for proper error handling:

```rust
use lepasserelle::LeIndex;
use anyhow::Result;

fn index_and_search(project_path: &str) -> Result<()> {
    let mut leindex = LeIndex::new(project_path)?;

    // Index the project
    leindex.index_project()?;

    // Search with automatic cache management
    let results = leindex.search("authentication")?;

    Ok(())
}
```

## Testing

Unit tests cover all cache operations:

```bash
cargo test -p lepasserelle cache_spill_reload_tests
```

Running the full test suite:

```bash
cargo test -p lepasserelle
```

## Future Enhancements

Potential improvements to memory management:

1. **Intelligent warming** - Use ML to predict which caches to warm
2. **Background spilling** - Spill caches asynchronously
3. **Compressed storage** - Compress spilled caches on disk
4. **Shared memory** - Share PDG across processes
5. **Tiered caching** - L1 (RAM), L2 (SSD), L3 (HDD)

## References

- **Source**: `crates/lepasserelle/src/leindex.rs` (cache spilling/reloading methods)
- **Source**: `crates/lepasserelle/src/memory.rs` (MemoryManager, CacheSpiller)
- **Tests**: `crates/lepasserelle/tests/integration_test.rs` (cache_spill_reload_tests module)
