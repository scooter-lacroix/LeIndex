# LeIndex Dashboard - Backend Implementation Plan

## Overview

Rust backend extensions for LeIndex dashboard with Turso global registry, unique project IDs, real-time sync, and code editing capabilities.

## New Crates

```
crates/
├── leserve/           # HTTP API server
├── leedit/            # Code editing engine
├── levalidation/      # Edit validation
└── leglobal/          # Turso global registry
```

## Phase 1: Core ID System (Days 1-3)

### Day 1: UniqueProjectId Implementation
- [ ] Create UniqueProjectId struct
- [ ] Path hashing with BLAKE3
- [ ] Instance counter for conflicts
- [ ] Display formatting

```rust
// lestockage/src/project_id.rs
pub struct UniqueProjectId {
    pub base_name: String,
    pub path_hash: String,
    pub instance: u32,
}

impl UniqueProjectId {
    pub fn generate(project_path: &Path) -> Self;
    pub fn to_string(&self) -> String;
    pub fn display(&self) -> String;
}
```

### Day 2: Schema Updates
- [ ] Add project_metadata table
- [ ] Migration for existing projects
- [ ] Update Storage initialization
- [ ] Legacy ID handling

```rust
// lestockage/src/schema.rs additions
pub const PROJECT_METADATA_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS project_metadata (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    unique_project_id TEXT UNIQUE NOT NULL,
    base_name TEXT NOT NULL,
    path_hash TEXT NOT NULL,
    instance INTEGER DEFAULT 0,
    canonical_path TEXT NOT NULL,
    display_name TEXT,
    is_clone BOOLEAN DEFAULT 0,
    cloned_from TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);
"#;
```

### Day 3: LeIndex Integration
- [ ] Update LeIndex::new() to generate unique IDs
- [ ] Conflict resolution logic
- [ ] Backward compatibility layer
- [ ] Tests for ID generation

## Phase 2: Global Registry (Days 4-7)

### Day 4: Turso Setup
- [ ] Create leglobal crate
- [ ] Turso/libSQL integration
- [ ] Global registry schema
- [ ] Connection management

```rust
// leglobal/src/lib.rs
pub struct GlobalRegistry {
    db: libsql::Connection,
}

impl GlobalRegistry {
    pub async fn init() -> Result<Self>;
    pub async fn register_project(&self, path: &Path) -> Result<String>;
    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>>;
}
```

### Day 5: Discovery Engine
- [ ] fd integration with fallback
- [ ] ripgrep fallback
- [ ] walkdir final fallback
- [ ] Alias detection (find, fzf, etc.)

```rust
// leglobal/src/discovery.rs
pub struct DiscoveryEngine;

impl DiscoveryEngine {
    pub async fn discover() -> Result<Vec<DiscoveredProject>>;
    pub fn has_fd() -> bool;
    pub fn has_ripgrep() -> bool;
    pub fn detect_fd_alias() -> Option<String>;
}
```

### Day 6: Sync Engine
- [ ] Validation and metadata extraction
- [ ] Sync report generation
- [ ] Conflict detection
- [ ] Clone detection with content fingerprint

```rust
// leglobal/src/sync.rs
pub struct SyncEngine;

impl SyncEngine {
    pub async fn validate_and_sync(
        &self,
        discovered: Vec<DiscoveredProject>
    ) -> Result<SyncReport>;
    
    pub async fn detect_clones(&self) -> Result<Vec<CloneGroup>>;
}
```

### Day 7: Background Sync
- [ ] Exponential backoff implementation
- [ ] Low-resource validation
- [ ] Sync status tracking
- [ ] Manual refresh endpoint

## Phase 3: HTTP Server (Days 8-10)

### Day 8: leserve Setup
- [ ] Axum server initialization
- [ ] Route structure
- [ ] Middleware (cors, logging)
- [ ] Error handling

```rust
// leserve/src/main.rs
#[tokio::main]
async fn main() {
    let app = Router::new()
        .nest("/api", api_routes())
        .layer(CorsLayer::permissive());
    
    axum::serve(listener, app).await.unwrap();
}
```

### Day 9: API Endpoints
- [ ] GET /api/codebases
- [ ] GET /api/codebases/:id
- [ ] POST /api/codebases/:id/refresh
- [ ] GET /api/codebases/:id/graph
- [ ] GET /api/search
- [ ] WebSocket /ws/events

### Day 10: WebSocket Implementation
- [ ] WebSocket connection handling
- [ ] Event broadcasting
- [ ] Client subscription management
- [ ] Message serialization

## Phase 4: Code Editing (Days 11-15)

### Day 11: leedit Core
- [ ] EditEngine struct
- [ ] Change representation
- [ ] Diff generation
- [ ] Preview endpoint

```rust
// leedit/src/lib.rs
pub struct EditEngine {
    pdg: Arc<ProgramDependenceGraph>,
    storage: Arc<Storage>,
}

impl EditEngine {
    pub async fn preview_edit(&self, request: EditRequest) -> Result<EditPreview>;
    pub async fn apply_edit(&self, request: EditRequest) -> Result<EditResult>;
}
```

### Day 12: Git Worktree Integration
- [ ] Worktree creation
- [ ] Staged edit management
- [ ] Apply/discard logic
- [ ] Cleanup handling

### Day 13: levalidation
- [ ] Syntax validation
- [ ] Reference integrity
- [ ] Semantic drift detection
- [ ] Impact analysis

```rust
// levalidation/src/lib.rs
pub struct LogicValidator;

impl LogicValidator {
    pub fn validate_structure(&self, changes: &[Change]) -> ValidationResult;
    pub fn check_references(&self) -> Vec<ReferenceIssue>;
    pub fn analyze_impact(&self, changes: &[Change]) -> ImpactAnalysis;
}
```

### Day 14: AST Refactoring
- [ ] Rename symbol
- [ ] Extract function
- [ ] Inline variable
- [ ] Move to module

### Day 15: Edit History
- [ ] Command pattern implementation
- [ ] Undo/redo support
- [ ] Rollback points
- [ ] History persistence

## Phase 5: Real-time Sync (Days 16-18)

### Day 16: File Watching
- [ ] inotify/fsevents setup
- [ ] Exponential backoff
- [ ] Change batching
- [ ] Resource limiting

```rust
// leglobal/src/watcher.rs
pub struct FileWatcher {
    backoff: ExponentialBackoff,
    pending_changes: Vec<PathBuf>,
}

impl FileWatcher {
    pub fn start(watch_paths: Vec<PathBuf>) -> Self;
    pub fn on_change(&mut self, path: PathBuf);
    pub async fn process_batch(&mut self);
}
```

### Day 17: Incremental Updates
- [ ] Changed file detection
- [ ] Partial re-indexing
- [ ] PDG delta updates
- [ ] Vector index updates

### Day 18: Integration & Testing
- [ ] End-to-end testing
- [ ] Performance profiling
- [ ] Resource monitoring
- [ ] Documentation

## Cross-Project Features (v1)

### HNSW Vector Index in Global Registry

```sql
-- Turso schema addition
CREATE TABLE global_vectors (
    id TEXT PRIMARY KEY,
    project_id TEXT REFERENCES indexed_projects(id),
    symbol_name TEXT,
    symbol_type TEXT,
    file_path TEXT,
    embedding F32_BLOB(768),
    FOREIGN KEY(project_id) REFERENCES indexed_projects(id)
);

CREATE INDEX idx_global_vectors_embedding ON global_vectors 
USING libsql_hnsw(embedding);
```

### API Endpoints

```rust
// Cross-project search
GET /api/search/global?q=...&projects=...

// Cross-project graph (limited to 3)
GET /api/graph/cross-project?projects=...
```

## Dependencies

```toml
# leglobal/Cargo.toml
[dependencies]
libsql = "0.3"
tokio = { version = "1", features = ["full"] }
tokio-process = "0.2"
walkdir = "2"
blake3 = "1"
serde = { version = "1", features = ["derive"] }
anyhow = "1"
tracing = "0.1"

# leserve/Cargo.toml
[dependencies]
axum = { version = "0.7", features = ["ws"] }
tokio = { version = "1", features = ["full"] }
tower-http = { version = "0.5", features = ["cors"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
leglobal = { path = "../leglobal" }
leedit = { path = "../leedit" }
levalidation = { path = "../levalidation" }

# leedit/Cargo.toml
[dependencies]
git2 = "0.18"
diffy = "0.3"
tree-sitter = "0.20"
anyhow = "1"

# levalidation/Cargo.toml
[dependencies]
lestockage = { path = "../lestockage" }
legraphe = { path = "../legraphe" }
anyhow = "1"
```

## Configuration

```yaml
# ~/.leindex/config.yaml additions
global_registry:
  db_path: "~/.leindex/global.db"
  sync_interval: 300  # seconds
  discovery_roots:
    - "~/projects"
    - "~/code"
    - "~/dev"
  max_depth: 4
  
file_watcher:
  enabled: true
  backoff_initial: 1.0  # seconds
  backoff_max: 300.0    # 5 minutes
  batch_timeout: 5.0    # seconds
  
server:
  host: "127.0.0.1"
  port: 47269
  cors_origins:
    - "http://localhost:5173"
```
