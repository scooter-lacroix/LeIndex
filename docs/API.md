# LeIndex API Integration Guide

LeIndex is a modular Rust library for code intelligence, providing AST parsing, Program Dependence Graph (PDG) construction, semantic search, and persistent storage. This guide covers integrating LeIndex crates into your Rust projects.

## Overview

LeIndex follows a layered architecture with five core crates:

| Crate | Purpose | Dependencies |
|-------|---------|--------------|
| `leparse` | AST parsing with tree-sitter | Standalone |
| `legraphe` | PDG construction and traversal | leparse |
| `lerecherche` | Semantic and text search | leparse, legraphe |
| `lestockage` | SQLite-based persistence | leparse, legraphe |
| `lepasserelle` | High-level orchestration | All above |

**Design Philosophy:**
- **Zero-copy parsing**: AST extraction without unnecessary allocations
- **Incremental computation**: Salsa-inspired caching for efficiency
- **Thread-safe reads**: Concurrent search access, exclusive writes
- **Modular composition**: Use individual crates or the full stack

## Cargo.toml Setup

Add LeIndex crates to your `Cargo.toml`:

```toml
[dependencies]
# Use all crates via the orchestration layer
lepasserelle = { path = "../path/to/LeIndexer/crates/lepasserelle" }

# Or use individual crates for finer control
leparse = { path = "../path/to/LeIndexer/crates/leparse" }
legraphe = { path = "../path/to/LeIndexer/crates/legraphe" }
lerecherche = { path = "../path/to/LeIndexer/crates/lerecherche" }
lestockage = { path = "../path/to/LeIndexer/crates/lestockage" }
```

For production use, reference the workspace version:

```toml
[dependencies]
lepasserelle = { version = "0.1.0", path = "crates/lepasserelle" }
```

### Feature Flags

`lepasserelle` supports optional features:

```toml
[dependencies.lepasserelle]
version = "0.1.0"
features = ["mcp-server"]  # Default: enables MCP JSON-RPC server
# features = []            # Disable MCP for minimal binary
```

## Quick Start

The simplest way to use LeIndex is through `lepasserelle::LeIndex`:

```rust
use lepasserelle::LeIndex;
use anyhow::Result;

fn main() -> Result<()> {
    // Create a LeIndex instance for your project
    let mut leindex = LeIndex::new("/path/to/your/project")?;
    
    // Index the project (parses files, builds PDG, indexes for search)
    let stats = leindex.index_project(false)?;  // false = incremental index
    println!("Indexed {} files, {} nodes", stats.files_parsed, stats.pdg_nodes);
    
    // Search for code
    let results = leindex.search("authentication", 10)?;
    for result in results {
        println!("{}: {} (score: {:.2})", 
            result.rank, result.symbol_name, result.score.overall);
    }
    
    // Deep analysis with context expansion
    let analysis = leindex.analyze("How does authentication work?", 2000)?;
    println!("Found {} entry points", analysis.results.len());
    if let Some(context) = analysis.context {
        println!("Expanded context:\n{}", context);
    }
    
    // Clean up (checkpoints WAL)
    leindex.close()?;
    
    Ok(())
}
```

## Crate Documentation

### leparse: Parsing Code

`leparse` provides zero-copy AST extraction with multi-language support via tree-sitter.

#### Supported Languages

| Language | Extensions | Module |
|----------|------------|--------|
| Python | `.py` | `leparse::python` |
| JavaScript | `.js`, `.jsx` | `leparse::javascript` |
| TypeScript | `.ts`, `.tsx` | `leparse::javascript` (typescript) |
| Go | `.go` | `leparse::go` |
| Rust | `.rs` | `leparse::rust` |
| Java | `.java` | `leparse::java` |
| C++ | `.cpp`, `.cc`, `.cxx`, `.hpp`, `.h` | `leparse::cpp` |
| C# | `.cs` | `leparse::csharp` |
| Ruby | `.rb` | `leparse::ruby` |
| PHP | `.php` | `leparse::php` |
| Lua | `.lua` | `leparse::lua` |
| Scala | `.scala`, `.sc` | `leparse::scala` |
| C | `.c`, `.h` | `leparse::c` |
| Bash | `.sh`, `.bash` | `leparse::bash` |
| JSON | `.json` | `leparse::json` |

#### Example: Parse a Single File

```rust
use leparse::traits::{CodeIntelligence, SignatureInfo};
use leparse::python::PythonParser;
use anyhow::Result;

fn parse_python_file(source: &[u8]) -> Result<Vec<SignatureInfo>> {
    let parser = PythonParser::new();
    let signatures = parser.get_signatures(source)?;
    
    for sig in &signatures {
        println!("Function: {} (lines {}-{})", 
            sig.name, 
            sig.byte_range.0,
            sig.byte_range.1
        );
        println!("  Parameters: {:?}", sig.parameters.iter().map(|p| &p.name).collect::<Vec<_>>());
        println!("  Return type: {:?}", sig.return_type);
        println!("  Async: {}", sig.is_async);
    }
    
    Ok(signatures)
}
```

#### Example: Parallel Parsing

```rust
use leparse::parallel::{ParallelParser, ParsingResult};
use std::path::PathBuf;
use anyhow::Result;

fn parse_project_files(files: Vec<PathBuf>) -> Result<Vec<ParsingResult>> {
    let parser = ParallelParser::new()
        .with_max_threads(8);  // Optional: limit threads
    
    let results = parser.parse_files(files);
    
    let successful = results.iter().filter(|r| r.is_success()).count();
    let failed = results.iter().filter(|r| r.is_failure()).count();
    
    println!("Parsed {} files: {} success, {} failed", 
        results.len(), successful, failed);
    
    Ok(results)
}
```

#### Error Handling

```rust
use leparse::traits::{Error, Result};

fn handle_parse_error(result: Result<Vec<SignatureInfo>>) {
    match result {
        Ok(sigs) => { /* success */ },
        Err(Error::ParseFailed(msg)) => eprintln!("Parse failed: {}", msg),
        Err(Error::SyntaxError { position, message }) => {
            eprintln!("Syntax error at byte {}: {}", position, message);
        },
        Err(Error::UnsupportedLanguage(lang)) => {
            eprintln!("Language not supported: {}", lang);
        },
        Err(Error::Io(e)) => eprintln!("IO error: {}", e),
        Err(Error::Utf8(e)) => eprintln!("UTF-8 error: {}", e),
    }
}
```

### legraphe: Building and Traversing PDG

`legraphe` implements the Program Dependence Graph with gravity-based traversal for context expansion.

#### Core Types

```rust
use legraphe::{
    ProgramDependenceGraph, Node, Edge, NodeType, EdgeType,
    GravityTraversal, TraversalConfig,
};
```

#### Example: Build a PDG

```rust
use legraphe::{ProgramDependenceGraph, Node, Edge, NodeType, EdgeType, EdgeMetadata};
use leparse::traits::SignatureInfo;

fn build_pdg_from_signatures(signatures: Vec<SignatureInfo>, file_path: &str) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();
    
    // Add nodes from signatures
    for sig in &signatures {
        let node = Node {
            id: sig.qualified_name.clone(),
            node_type: if sig.is_method { NodeType::Method } else { NodeType::Function },
            name: sig.name.clone(),
            file_path: file_path.to_string(),
            byte_range: sig.byte_range,
            complexity: 1,  // Calculate from actual metrics
            language: "python".to_string(),
            embedding: None,
        };
        pdg.add_node(node);
    }
    
    // Add edges based on call relationships
    for sig in &signatures {
        if let Some(caller_id) = pdg.find_by_symbol(&sig.qualified_name) {
            for called_name in &sig.calls {
                if let Some(callee_id) = pdg.find_by_symbol(called_name) {
                    pdg.add_edge(caller_id, callee_id, Edge {
                        edge_type: EdgeType::Call,
                        metadata: EdgeMetadata {
                            call_count: None,
                            variable_name: None,
                        },
                    });
                }
            }
        }
    }
    
    pdg
}
```

#### Example: Graph Traversal

```rust
use legraphe::{ProgramDependenceGraph, GravityTraversal, TraversalConfig};

fn analyze_impact(pdg: &ProgramDependenceGraph, function_name: &str) {
    // Find the function node
    let node_id = match pdg.find_by_symbol(function_name) {
        Some(id) => id,
        None => {
            eprintln!("Function not found: {}", function_name);
            return;
        }
    };
    
    // Forward impact: what does this function affect?
    let forward_impact = pdg.get_forward_impact(node_id);
    println!("Changes to {} will affect {} nodes:", function_name, forward_impact.len());
    for affected_id in &forward_impact {
        if let Some(node) = pdg.get_node(*affected_id) {
            println!("  - {} ({})", node.name, node.file_path);
        }
    }
    
    // Backward impact: what affects this function?
    let backward_impact = pdg.get_backward_impact(node_id);
    println!("{} depends on {} nodes:", function_name, backward_impact.len());
    for dep_id in &backward_impact {
        if let Some(node) = pdg.get_node(*dep_id) {
            println!("  - {} ({})", node.name, node.file_path);
        }
    }
}

fn expand_context(pdg: &ProgramDependenceGraph, entry_points: Vec<String>, token_budget: usize) {
    let config = TraversalConfig {
        max_tokens: token_budget,
        ..TraversalConfig::default()
    };
    let traversal = GravityTraversal::with_config(config);
    
    // Convert symbol names to node IDs
    let entry_ids: Vec<_> = entry_points
        .iter()
        .filter_map(|s| pdg.find_by_symbol(s))
        .collect();
    
    // Get expanded context nodes
    let expanded = traversal.expand_context(pdg, entry_ids);
    
    println!("Expanded to {} nodes:", expanded.len());
    for node_id in expanded {
        if let Some(node) = pdg.get_node(node_id) {
            println!("  {} ({})", node.name, node.file_path);
        }
    }
}
```

#### Serialization

```rust
use legraphe::ProgramDependenceGraph;

fn save_pdg(pdg: &ProgramDependenceGraph, path: &str) -> Result<(), String> {
    let serialized = pdg.serialize()?;
    std::fs::write(path, serialized)?;
    Ok(())
}

fn load_pdg(path: &str) -> Result<ProgramDependenceGraph, String> {
    let data = std::fs::read(path)
        .map_err(|e| format!("Failed to read file: {}", e))?;
    ProgramDependenceGraph::deserialize(&data)
}
```

### lerecherche: Searching with Semantic Support

`lerecherche` provides hybrid search combining text matching and semantic similarity.

#### Core Types

```rust
use lerecherche::{
    SearchEngine, SearchQuery, SearchResult, NodeInfo, Score,
    HNSWParams,
};
```

#### Example: Index and Search

```rust
use lerecherche::{SearchEngine, SearchQuery, NodeInfo};

fn search_example() {
    let mut engine = SearchEngine::new();
    
    // Create nodes to index
    let nodes = vec![
        NodeInfo {
            node_id: "auth.login".to_string(),
            file_path: "src/auth.py".to_string(),
            symbol_name: "login".to_string(),
            language: "python".to_string(),
            content: "def login(username, password):\n    # Authenticate user".to_string(),
            byte_range: (0, 50),
            embedding: Some(vec![0.1; 768]),  // 768-dim embedding
            complexity: 5,
        },
        NodeInfo {
            node_id: "auth.logout".to_string(),
            file_path: "src/auth.py".to_string(),
            symbol_name: "logout".to_string(),
            language: "python".to_string(),
            content: "def logout():\n    # End user session".to_string(),
            byte_range: (51, 100),
            embedding: Some(vec![0.2; 768]),
            complexity: 2,
        },
    ];
    
    // Index nodes
    engine.index_nodes(nodes);
    println!("Indexed {} nodes", engine.node_count());
    
    // Execute search
    let query = SearchQuery {
        query: "authenticate user".to_string(),
        top_k: 5,
        token_budget: None,
        semantic: true,
        expand_context: false,
        query_embedding: Some(vec![0.15; 768]),  // Query embedding
        threshold: Some(0.1),  // Minimum relevance
    };
    
    let results = engine.search(query).unwrap();
    for result in results {
        println!("{}. {} (score: {:.3})", 
            result.rank, result.symbol_name, result.score.overall);
        println!("   File: {}", result.file_path);
    }
}
```

#### Example: HNSW for Fast Semantic Search

```rust
use lerecherche::{SearchEngine, HNSWParams};

fn hnsw_example() {
    // Create engine with HNSW index for fast approximate search
    let params = HNSWParams {
        ef_construction: 200,
        m: 16,
        ..HNSWParams::default()
    };
    
    let mut engine = SearchEngine::with_hnsw(768, params);
    
    // Index many nodes...
    // engine.index_nodes(nodes);
    
    // HNSW provides 10-100x speedup for large datasets (>10K vectors)
    println!("HNSW enabled: {}", engine.is_hnsw_enabled());
    
    // You can also switch between brute-force and HNSW
    let mut engine = SearchEngine::new();
    // ... index nodes ...
    engine.enable_hnsw(HNSWParams::default()).unwrap();
}
```

#### Example: Natural Language Search

```rust
use lerecherche::SearchEngine;

fn natural_search_example() {
    let mut engine = SearchEngine::new();
    // ... index nodes ...
    
    // Natural language queries are parsed and classified
    let results = engine.natural_search(
        "show me how authentication works",
        10
    ).unwrap();
    
    // Supported patterns:
    // - "show me how X works" → semantic + context expansion
    // - "where is X handled?" → structural search
    // - "what are bottlenecks?" → complexity-ranked results
}
```

#### Thread Safety

```rust
use lerecherche::SearchEngine;
use std::sync::{Arc, RwLock};

// For concurrent read-write access
let engine = Arc::new(RwLock::new(SearchEngine::new()));

// Multiple readers
{
    let reader = engine.read().unwrap();
    // Safe for concurrent reads
}

// Single writer
{
    let mut writer = engine.write().unwrap();
    writer.index_nodes(nodes);  // Exclusive access required
}
```

### lestockage: Persisting Data

`lestockage` provides SQLite-based persistence with WAL mode for concurrency.

#### Core Types

```rust
use lestockage::{
    Storage, StorageConfig,
    pdg_store::{save_pdg, load_pdg, pdg_exists, delete_pdg},
    IncrementalCache, NodeHash,
};
```

#### Example: Storage Operations

```rust
use lestockage::{Storage, StorageConfig};
use legraphe::ProgramDependenceGraph;
use std::path::Path;

fn storage_example(project_path: &Path) -> anyhow::Result<()> {
    let db_path = project_path.join(".leindex/leindex.db");
    
    // Open storage with default config (WAL enabled)
    let mut storage = Storage::open(&db_path)?;
    
    // Or with custom config
    let config = StorageConfig {
        db_path: db_path.display().to_string(),
        wal_enabled: true,
        cache_size_pages: Some(10000),
    };
    let mut storage = Storage::open_with_config(&db_path, config)?;
    
    // Save PDG
    let pdg = ProgramDependenceGraph::new();
    // ... build PDG ...
    
    lestockage::pdg_store::save_pdg(&mut storage, "my_project", &pdg)?;
    
    // Check if PDG exists
    if lestockage::pdg_store::pdg_exists(&storage, "my_project")? {
        // Load PDG
        let loaded_pdg = lestockage::pdg_store::load_pdg(&storage, "my_project")?;
        println!("Loaded PDG with {} nodes", loaded_pdg.node_count());
    }
    
    // Delete PDG when no longer needed
    lestockage::pdg_store::delete_pdg(&mut storage, "my_project")?;
    
    Ok(())
}
```

#### Example: Incremental Caching

```rust
use lestockage::{IncrementalCache, NodeHash};
use std::path::Path;

fn incremental_example(source: &[u8]) {
    // Compute content hash
    let hash = NodeHash::compute(source);
    
    // Check cache
    let cache = IncrementalCache::new();
    
    if let Some(cached) = cache.get(&hash) {
        println!("Cache hit! Using cached result");
        // Use cached signatures
    } else {
        println!("Cache miss, parsing...");
        // Parse and cache result
        cache.insert(hash, parsed_result);
    }
}
```

### lepasserelle: Full Orchestration

`lepasserelle` unifies all crates into a cohesive API.

#### Core Types

```rust
use lepasserelle::{
    LeIndex, IndexStats, Diagnostics,
    LeIndexError, Result as LeIndexResult,
};
```

#### Example: Complete Workflow

```rust
use lepasserelle::LeIndex;
use anyhow::Result;

fn complete_workflow(project_path: &str) -> Result<()> {
    // Initialize LeIndex
    let mut leindex = LeIndex::new(project_path)?;
    
    // Index project (incremental by default)
    let stats = leindex.index_project(false)?;
    print_stats(&stats);
    
    // Search for code
    let results = leindex.search("database connection", 10)?;
    for result in &results {
        println!("{}: {} ({:.2})", 
            result.rank, result.symbol_name, result.score.overall);
    }
    
    // Deep analysis
    let analysis = leindex.analyze("How is database pooling implemented?", 2000)?;
    println!("Analysis took {}ms", analysis.processing_time_ms);
    println!("Tokens used: {}", analysis.tokens_used);
    
    // Get diagnostics
    let diag = leindex.get_diagnostics()?;
    println!("Memory usage: {:.1}%", diag.memory_usage_percent);
    println!("Cache entries: {}", diag.cache_entries);
    
    // Memory management
    if diag.memory_threshold_exceeded {
        leindex.spill_all_caches()?;
        println!("Spilled caches to disk");
    }
    
    // Close and checkpoint
    leindex.close()?;
    
    Ok(())
}

fn print_stats(stats: &lepasserelle::IndexStats) {
    println!("Indexing Statistics:");
    println!("  Files parsed: {}/{}", stats.successful_parses, stats.files_parsed);
    println!("  Failed: {}", stats.failed_parses);
    println!("  Signatures: {}", stats.total_signatures);
    println!("  PDG nodes: {}", stats.pdg_nodes);
    println!("  PDG edges: {}", stats.pdg_edges);
    println!("  Time: {}ms", stats.indexing_time_ms);
}
```

#### Example: Memory Management

```rust
use lepasserelle::LeIndex;

fn memory_management_example() -> anyhow::Result<()> {
    let mut leindex = LeIndex::new("/path/to/project")?;
    
    // Check memory before large operation
    if leindex.check_memory_and_spill()? {
        println!("Memory threshold exceeded, cache spilled");
    }
    
    // Manually spill caches
    leindex.spill_pdg_cache()?;
    leindex.spill_vector_cache()?;
    
    // Or spill everything at once
    let (pdg_bytes, vector_bytes) = leindex.spill_all_caches()?;
    println!("Spilled {} + {} bytes", pdg_bytes, vector_bytes);
    
    // Warm caches when needed
    use lepasserelle::MemoryManagementConfig;
    let warm_result = leindex.warm_caches(
        lepasserelle::memory::WarmStrategy::All
    )?;
    println!("Warmed {} entries", warm_result.entries_warmed);
    
    Ok(())
}
```

#### Error Handling

```rust
use lepasserelle::{LeIndex, LeIndexError};

fn handle_errors() {
    match LeIndex::new("/invalid/path") {
        Ok(leindex) => { /* success */ },
        Err(e) => {
            // LeIndexError provides context and recovery suggestions
            eprintln!("Error: {}", e);
            if let Some(context) = e.context() {
                eprintln!("Context: {:?}", context);
            }
            if let Some(recovery) = e.recovery_strategy() {
                eprintln!("Suggested recovery: {:?}", recovery);
            }
        }
    }
}
```

## Common Patterns

### Pattern: Index and Search a Project

```rust
use lepasserelle::LeIndex;
use anyhow::Result;

fn index_and_search(project_path: &str, query: &str) -> Result<Vec<String>> {
    let mut leindex = LeIndex::new(project_path)?;
    
    // Load existing index or create new one
    if !leindex.is_indexed() {
        let stats = leindex.index_project(false)?;
        println!("Indexed {} files", stats.files_parsed);
    }
    
    // Execute search
    let results = leindex.search(query, 10)?;
    
    let symbols: Vec<String> = results
        .iter()
        .map(|r| format!("{}:{}", r.file_path, r.symbol_name))
        .collect();
    
    leindex.close()?;
    Ok(symbols)
}
```

### Pattern: Build a Custom Analyzer

```rust
use leparse::parallel::ParallelParser;
use legraphe::{ProgramDependenceGraph, extract_pdg_from_signatures};
use lerecherche::{SearchEngine, NodeInfo};
use anyhow::Result;

struct CustomAnalyzer {
    pdg: ProgramDependenceGraph,
    search_engine: SearchEngine,
}

impl CustomAnalyzer {
    fn new() -> Self {
        Self {
            pdg: ProgramDependenceGraph::new(),
            search_engine: SearchEngine::new(),
        }
    }
    
    fn analyze_files(&mut self, files: Vec<std::path::PathBuf>) -> Result<()> {
        // Parse files in parallel
        let parser = ParallelParser::new();
        let results = parser.parse_files(files);
        
        // Build PDG from successful parses
        for result in results.iter().filter(|r| r.is_success()) {
            let lang = result.language.as_ref().unwrap();
            let file_pdg = extract_pdg_from_signatures(
                result.signatures.clone(),
                &[],
                &result.file_path.display().to_string(),
                lang,
            );
            
            // Merge into main PDG
            for node_id in file_pdg.node_indices() {
                if let Some(node) = file_pdg.get_node(node_id) {
                    self.pdg.add_node(node.clone());
                }
            }
        }
        
        // Build search index
        let nodes: Vec<NodeInfo> = self.pdg.node_indices()
            .filter_map(|id| self.pdg.get_node(id))
            .map(|node| NodeInfo {
                node_id: node.id.clone(),
                file_path: node.file_path.clone(),
                symbol_name: node.name.clone(),
                language: node.language.clone(),
                content: String::new(),  // Load from file if needed
                byte_range: node.byte_range,
                embedding: node.embedding.clone(),
                complexity: node.complexity,
            })
            .collect();
        
        self.search_engine.index_nodes(nodes);
        
        Ok(())
    }
    
    fn find_hotspots(&self, top_n: usize) -> Vec<String> {
        // Find high-complexity nodes
        let mut nodes: Vec<_> = self.pdg.node_indices()
            .filter_map(|id| self.pdg.get_node(id))
            .collect();
        
        nodes.sort_by(|a, b| b.complexity.cmp(&a.complexity));
        
        nodes.iter()
            .take(top_n)
            .map(|n| format!("{} (complexity: {})", n.name, n.complexity))
            .collect()
    }
}
```

### Pattern: Extend with New Languages

```rust
use leparse::traits::{CodeIntelligence, SignatureInfo, Result, Error};
use tree_sitter::Parser;

struct CustomLanguageParser {
    parser: Parser,
}

impl CustomLanguageParser {
    fn new() -> Self {
        let mut parser = Parser::new();
        // Set your tree-sitter grammar here
        // parser.set_language(&tree_sitter_custom::language()).unwrap();
        Self { parser }
    }
}

impl CodeIntelligence for CustomLanguageParser {
    fn get_signatures(&self, source: &[u8]) -> Result<Vec<SignatureInfo>> {
        let tree = self.parser.parse(source)
            .ok_or_else(|| Error::ParseFailed("Failed to parse".to_string()))?;
        
        let root = tree.root_node();
        let mut signatures = Vec::new();
        
        // Query the tree for function/class definitions
        // Use tree-sitter queries to extract signature info
        
        Ok(signatures)
    }
    
    fn compute_cfg(&self, source: &[u8], node_id: usize) -> Result<leparse::traits::Graph<leparse::traits::Block, leparse::traits::Edge>> {
        // Implement control flow graph construction
        unimplemented!("CFG computation not implemented")
    }
    
    fn extract_complexity(&self, node: &tree_sitter::Node) -> leparse::traits::ComplexityMetrics {
        leparse::traits::ComplexityMetrics {
            cyclomatic: 1,
            nesting_depth: 0,
            line_count: 1,
            token_count: 0,
        }
    }
}
```

## Performance Tips

### Memory Management

1. **Use incremental indexing** to avoid re-parsing unchanged files:
   ```rust
   leindex.index_project(false)?;  // false = incremental
   ```

2. **Spill caches for large projects**:
   ```rust
   if leindex.check_memory_and_spill()? {
       // Cache was spilled, subsequent ops will be slower
   }
   ```

3. **Use HNSW for large datasets** (>10K nodes):
   ```rust
   let engine = SearchEngine::with_hnsw(768, HNSWParams::default());
   ```

### Parallelism

1. **Parallel parsing** uses Rayon automatically:
   ```rust
   let parser = ParallelParser::new()
       .with_max_threads(num_cpus::get());
   ```

2. **Thread-local parsers** avoid allocation overhead - already implemented in `leparse::parallel`.

### Storage

1. **WAL mode** is enabled by default for better concurrency.

2. **Cache size** can be tuned:
   ```rust
   let config = StorageConfig {
       cache_size_pages: Some(20000),  // ~80MB cache
       ..Default::default()
   };
   ```

3. **Close properly** to checkpoint WAL:
   ```rust
   leindex.close()?;  // Ensures WAL is checkpointed
   ```

## Error Handling

### Result Types

Each crate defines its own result type:

```rust
// leparse
use leparse::traits::{Result as ParseResult, Error as ParseError};

// legraphe (uses String for errors in serialize/deserialize)
let result: Result<ProgramDependenceGraph, String> = 
    ProgramDependenceGraph::deserialize(&data);

// lerecherche
use lerecherche::Error as SearchError;

// lestockage
use lestockage::PdgStoreError;

// lepasserelle
use lepasserelle::{LeIndexError, Result as LeIndexResult};
```

### Error Propagation

```rust
use anyhow::{Context, Result};

fn robust_indexing(path: &str) -> Result<()> {
    let mut leindex = LeIndex::new(path)
        .context("Failed to initialize LeIndex")?;
    
    let stats = leindex.index_project(false)
        .context("Indexing failed")?;
    
    if stats.failed_parses > 0 {
        tracing::warn!(
            "{} files failed to parse", 
            stats.failed_parses
        );
    }
    
    leindex.close()
        .context("Failed to close LeIndex")?;
    
    Ok(())
}
```

### Thread Safety Summary

| Component | Concurrent Reads | Concurrent Writes | Notes |
|-----------|-----------------|-------------------|-------|
| `SearchEngine` | Safe | Unsafe | Wrap in `Arc<RwLock<_>>` |
| `ProgramDependenceGraph` | Safe | Unsafe | Serialize for sharing |
| `Storage` | Safe | Unsafe | WAL enables read concurrency |
| `LeIndex` | Safe | Unsafe | Single-threaded writes |

```rust
// Recommended pattern for concurrent access
use std::sync::{Arc, RwLock};

let leindex = Arc::new(RwLock::new(LeIndex::new("/project")?));

// Spawn reader threads
let readers: Vec<_> = (0..4).map(|_| {
    let engine = Arc::clone(&leindex);
    std::thread::spawn(move || {
        let reader = engine.read().unwrap();
        reader.search("query", 10)
    })
}).collect();

// Single writer
{
    let mut writer = leindex.write().unwrap();
    writer.index_project(false)?;
}
```

## Next Steps

- **CLI Usage**: Run `leindex --help` for command-line interface
- **MCP Server**: See `src/mcp/` for LLM tool integration
- **Examples**: Check `tests/` and `examples/` for more patterns
