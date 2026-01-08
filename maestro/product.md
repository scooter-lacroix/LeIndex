# Product Guide: LeIndex

## Initial Concept

LeIndex: AI-powered code search and indexing system with MCP integration. A zero-dependency code search engine that uses semantic understanding (LEANN vector embeddings) and full-text search (Tantivy) to help developers find code by meaning, not just text matching. Designed as an MCP server for seamless AI assistant integration.

---

## Product Vision

LeIndex transforms how developers interact with code by providing **intelligent, semantic code search** that actually understands what you're looking for, not just where it might be typed. Unlike traditional search tools that match text patterns, LeIndex comprehends code meaning, intent, and context—delivering relevant results even when function names, comments, or implementations don't match your search terms.

Built as a **first-class MCP (Model Context Protocol) server**, LeIndex integrates seamlessly with AI assistants like Claude Code, Cursor, and Windsurf, providing them with deep codebase understanding for more accurate, context-aware assistance.

### The Problem We Solve

**Developers waste hours searching through unfamiliar codebases:**
- Text-based search tools miss code that uses different terminology
- Navigating large monorepos (100K+ files) is painfully slow
- AI assistants lack deep codebase context, leading to hallucinations or irrelevant suggestions
- Existing code search tools require complex infrastructure (Docker, PostgreSQL, Elasticsearch)
- Privacy concerns prevent using cloud-based code intelligence tools

**LeIndex solves all of this.**

---

## Target Users

### Primary Users

1. **AI-Assisted Developers**
   - Use Claude Code, Cursor, Windsurf, or other MCP-compatible AI assistants
   - Need fast, accurate code context to improve AI assistance quality
   - Work on complex codebases where understanding relationships between components is critical
   - Value token efficiency—want AI assistants to have relevant context without bloated prompts

2. **Teams Maintaining Large Codebases**
   - Monorepos with 50K-100K+ files
   - Legacy code with inconsistent naming and documentation
   - Multiple programming languages and frameworks
   - Need search that works across the entire organization's code

3. **Open-Source Contributors**
   - Frequently join unfamiliar projects
   - Need to quickly understand code architecture and patterns
   - Search for implementation patterns, not just text matches
   - Value tools that work offline and don't require external services

4. **Privacy-Conscious Organizations**
   - Cannot use cloud-based code intelligence tools (IP concerns)
   - Require self-hosted, on-premise solutions
   - Need offline-capable tools for air-gapped environments
   - Demand zero-telemetry and data residency guarantees

### Secondary Users

5. **DevOps and Platform Engineers**
   - Build developer tooling and infrastructure
   - Need embeddable search capabilities for internal platforms
   - Require reliable, low-maintenance services

6. **Technical Leads and Architects**
   - Need to analyze code patterns across large projects
   - Search for architectural decisions and implementation approaches
   - Understand code evolution and dependencies

---

## Core Goals

### Functional Goals

1. **Semantic Code Understanding**
   - Search by concept, not text: "authentication logic" finds login handlers, session management, security patterns—even if named differently
   - Understand code intent, structure, and relationships
   - Support multi-language codebases with language-aware parsing

2. **Lightning-Fast Performance**
   - Index 50K files in under 30 seconds (current target: <1 minute after optimization)
   - Search results in milliseconds
   - Handle 100K+ file codebases without performance degradation
   - Efficient incremental indexing for real-time updates

3. **Zero-Dependency Simplicity**
   - `pip install` and everything just works
   - No Docker, no PostgreSQL, no Elasticsearch, no RabbitMQ
   - Runs on any machine with Python 3.10+
   - Minimal resource footprint (4GB RAM minimum, 8GB+ recommended)

4. **Privacy-First Design**
   - Everything runs locally on your machine
   - No data leaves your environment
   - No telemetry or analytics
   - Works completely offline after installation

5. **Seamless AI Integration**
   - Native MCP server for AI assistant integration
   - Token-efficient context delivery (saves ~200 tokens per session)
   - Semantic search results improve AI accuracy and relevance
   - No custom hooks or complex configuration required

### Non-Functional Goals

1. **Reliability**
   - Battle-tested storage backends (SQLite, Tantivy, LEANN)
   - Graceful error handling and recovery
   - No single points of failure
   - Handles corrupted files, permission errors, and edge cases

2. **Maintainability**
   - Clean, modular architecture
   - Comprehensive documentation and examples
   - Easy to extend with new search backends
   - Clear upgrade paths between versions

3. **Developer Experience**
   - Intuitive CLI tools (`leindex`, `leindex-search`)
   - Clear error messages and actionable feedback
   - Works out of the box with sensible defaults
   - Optional configuration for advanced use cases

4. **Performance**
   - Sub-second search responses
   - Minimal memory footprint during indexing
   - Efficient incremental updates
   - No blocking operations in async contexts

---

## Key Features

### Search Capabilities

1. **Semantic Search**
   - LEANN vector embeddings with CodeRankEmbed model
   - Find code by meaning and intent
   - Ranks results by conceptual similarity
   - Supports natural language queries

2. **Full-Text Search**
   - Tantivy (Rust-powered Lucene) for fast keyword search
   - Regex support for precise pattern matching
   - Case-sensitive and insensitive search
   - Context-aware result highlighting

3. **Hybrid Scoring**
   - Combines semantic and lexical signals
   - Optimizes for relevance and precision
   - Adaptive ranking based on query type

4. **Multi-Backend Support**
   - LEANN (vector search)
   - Tantivy (full-text)
   - SQLite (metadata queries)
   - DuckDB (analytics)
   - Fallback grep/ripgrep/ag/ugrep

### Indexing Features

1. **Fast Initial Indexing**
   - Parallel file processing
   - Intelligent filtering (ignore patterns, file size limits)
   - Incremental change detection
   - Supports 100K+ file codebases

2. **Incremental Updates**
   - File system watcher for real-time updates
   - Efficient change detection (hash-based)
   - Smart caching and lazy loading
   - Minimal re-indexing on file changes

3. **Language Support**
   - Python, JavaScript, TypeScript, Go, Rust, Java, C++, and more
   - Extensible parser architecture
   - Syntax-aware tokenization

### Integration Features

1. **MCP Server**
   - First-class Model Context Protocol support
   - Comprehensive tool set for AI assistants
   - Efficient context delivery
   - No custom hooks required

2. **CLI Tools**
   - `leindex init` - Initialize project index
   - `leindex index` - Create or update index
   - `leindex-search` - Fast command-line search
   - Minimal dependencies, easy to install

3. **Python API**
   - Programmatic access to all features
   - Simple, intuitive interface
   - Async/await support for high-performance applications

---

## Design Philosophy

### Core Principles

1. **Simplicity Over Complexity**
   - Zero external dependencies when possible
   - Prefer battle-tested libraries over experimental ones
   - Avoid infrastructure complexity (no Docker, no microservices)
   - Every feature should justify its existence

2. **Privacy by Default**
   - Local-first architecture
   - No telemetry, no phone-home
   - User owns their data
   - Works offline

3. **Performance Is a Feature**
   - Fast indexing = happy developers
   - Millisecond search responses
   - Efficient resource usage
   - Scale from side projects to enterprise monorepos

4. **Developer Experience Matters**
   - Clear error messages
   - Sensible defaults
   - Works out of the box
   - Optional power-user features

### Tradeoffs

1. **CPU-Only PyTorch**
   - ✅ No GPU dependencies, works everywhere
   - ✅ Simpler installation
   - ❌ Slower embedding generation for very large codebases
   - **Decision:** Accept slower embedding for universal compatibility

2. **SQLite over PostgreSQL**
   - ✅ Zero setup, embedded database
   - ✅ ACID compliant, battle-tested
   - ❌ Limited concurrent write performance
   - **Decision:** SQLite is sufficient for metadata workload

3. **LEANN over FAISS**
   - ✅ Storage-efficient, no external dependencies
   - ✅ Fast HNSW/DiskANN indexing
   - ❌ Less widely adopted than FAISS
   - **Decision:** LEANN's zero-dependency approach aligns with project goals

---

## Success Metrics

### Technical Metrics

1. **Performance**
   - Index 50K files in <60 seconds (current baseline: 7-16 minutes)
   - Search latency: P50 <100ms, P95 <500ms
   - Memory usage: <2GB during indexing, <500MB idle
   - Incremental index updates: <5 seconds for 100 changed files

2. **Reliability**
   - 99.9% uptime for MCP server
   - Graceful handling of 99% of error conditions
   - Zero data loss scenarios
   - Recovery from corrupted indexes

3. **Adoption**
   - MCP server installed and configured by default
   - Used by AI assistants in >80% of sessions
   - Token savings: >200 tokens per session
   - Search accuracy: >90% relevance in top 5 results

### User Metrics

1. **Developer Satisfaction**
   - Setup time: <2 minutes from install to first search
   - Zero configuration for 90% of use cases
   - Clear error messages with actionable guidance
   - Comprehensive documentation for advanced scenarios

2. **Community Growth**
   - Active GitHub issues resolved within 48 hours
   - Regular releases with backward compatibility
   - Growing contributor base
   - Integration with popular AI tools

---

## Out of Scope

### Explicitly Not Our Focus

1. **Web UI**
   - LeIndex is a backend service and CLI tool
   - Users can build web interfaces on top of the API
   - We prioritize CLI and MCP integration

2. **Cloud/SaaS Offering**
   - LeIndex is self-hosted and local-only
   - No managed service or cloud version
   - Privacy-first design means no hosted variant

3. **Enterprise Features**
   - No RBAC, audit logs, or SSO
   - No multi-tenancy or team collaboration features
   - Organizations can build these on top

4. **Language-Specific Features**
   - No IDE-specific integrations (VS Code, JetBrains, etc.)
   - No language server protocol (LSP) support
   - Focus on general-purpose code search

### Future Considerations (Not Now)

1. **Distributed Indexing**
   - No support for federated search across multiple machines
   - Single-machine architecture for simplicity

2. **Real-Time Collaboration**
   - No shared indexes or collaborative features
   - Personal, local indexing model

3. **Advanced Analytics**
   - DuckDB provides analytics capabilities
   - No dashboards, visualization, or reporting features
   - Users can query analytics data directly
