// Cross-project resolution for global symbol tracking
//
// This module provides cross-project resolution capabilities, enabling
// symbols to be resolved across project boundaries with lazy PDG loading.

use crate::global_symbols::{GlobalSymbol, GlobalSymbolId};
use crate::pdg_store::{load_pdg, PdgStoreError};
use legraphe::pdg::{EdgeId, NodeId, ProgramDependenceGraph};
use std::collections::{HashMap, HashSet};
use thiserror::Error;

/// Cross-project resolver
pub struct CrossProjectResolver {
    /// Storage backend
    storage: crate::Storage,

    /// PDG cache (project_id -> PDG)
    pdg_cache: HashMap<String, ProgramDependenceGraph>,

    /// Max depth for external PDG loading
    max_depth: usize,
}

impl CrossProjectResolver {
    /// Create new cross-project resolver
    pub fn new(storage: crate::Storage) -> Self {
        Self {
            storage,
            pdg_cache: HashMap::new(),
            max_depth: 3, // Default max depth for external loading
        }
    }

    /// Create resolver with custom max depth
    pub fn with_max_depth(storage: crate::Storage, max_depth: usize) -> Self {
        Self {
            storage,
            pdg_cache: HashMap::new(),
            max_depth,
        }
    }

    /// Get the underlying storage
    pub fn storage(&self) -> &crate::Storage {
        &self.storage
    }

    /// Clear PDG cache
    pub fn clear_cache(&mut self) {
        self.pdg_cache.clear();
    }

    /// Resolve a symbol across all projects
    ///
    /// Returns all matching symbols with their project context and PDG load status
    pub fn resolve_symbol(
        &mut self,
        project_id: &str,
        symbol_name: &str,
    ) -> Result<Vec<ResolvedSymbol>, ResolutionError> {
        let symbol_table = crate::GlobalSymbolTable::new(&self.storage);

        let symbols = symbol_table.resolve_by_name(symbol_name)?;

        if symbols.is_empty() {
            return Err(ResolutionError::SymbolNotFound(symbol_name.to_string()));
        }

        // Convert to resolved symbols with load status
        let mut results = Vec::new();
        for symbol in symbols {
            let is_local = symbol.project_id == project_id;
            let is_loaded = self.pdg_cache.contains_key(&symbol.project_id);

            results.push(ResolvedSymbol {
                project_id: symbol.project_id.clone(),
                is_local,
                is_loaded,
                symbol,
            });
        }

        Ok(results)
    }

    /// Load external PDG on demand
    pub fn load_external_pdg(&mut self, project_id: &str) -> Result<(), ResolutionError> {
        // Check if already loaded
        if self.pdg_cache.contains_key(project_id) {
            return Ok(());
        }

        // Check if we've exceeded max depth
        if self.pdg_cache.len() >= self.max_depth {
            return Err(ResolutionError::MaxDepthExceeded);
        }

        // Load PDG from storage
        let pdg = load_pdg(&self.storage, project_id)
            .map_err(|e| ResolutionError::PdgLoadFailed(project_id.to_string(), e))?;

        // Cache it
        self.pdg_cache.insert(project_id.to_string(), pdg);

        Ok(())
    }

    /// Build cross-project PDG by merging local and external PDGs
    ///
    /// This merges the root project's PDG with all external PDGs it references,
    /// creating a unified view of the cross-project dependency graph.
    pub fn build_cross_project_pdg(
        &mut self,
        root_project_id: &str,
        max_depth: Option<usize>,
    ) -> Result<ProgramDependenceGraph, ResolutionError> {
        let max_depth = max_depth.unwrap_or(self.max_depth);

        // Load root PDG
        self.load_external_pdg(root_project_id)?;

        // Get the root PDG from cache (it was just loaded)
        // We need to remove it from the cache to get ownership, then re-add it
        let root_pdg = self.pdg_cache.remove(root_project_id).ok_or_else(|| {
            ResolutionError::SymbolNotFound(format!(
                "PDG for project {} not found",
                root_project_id
            ))
        })?;

        // Track visited projects to avoid cycles
        let mut visited = HashSet::new();
        visited.insert(root_project_id.to_string());

        // Collect all external symbols to load
        let mut to_load = Vec::new();
        self.collect_external_projects(&root_pdg, &mut to_load, 0, max_depth)?;

        // Load all external PDGs
        for project_id in &to_load {
            if !visited.contains(project_id) {
                self.load_external_pdg(project_id)?;
                visited.insert(project_id.clone());
            }
        }

        // Merge all PDGs - use the root PDG directly
        let mut merged_pdg = root_pdg;
        for (project_id, pdg) in &self.pdg_cache {
            if project_id != root_project_id {
                Self::merge_pdgs(&mut merged_pdg, pdg)
                    .map_err(|e| ResolutionError::MergeError(format!("{:?}", e)))?;
            }
        }

        Ok(merged_pdg)
    }

    /// Collect all external projects referenced by a PDG
    fn collect_external_projects(
        &self,
        pdg: &ProgramDependenceGraph,
        to_load: &mut Vec<String>,
        current_depth: usize,
        max_depth: usize,
    ) -> Result<(), ResolutionError> {
        if current_depth >= max_depth {
            return Ok(());
        }

        let symbol_table = crate::GlobalSymbolTable::new(&self.storage);

        // Iterate through all nodes to find external references
        for node_id in pdg.node_indices() {
            if let Some(node) = pdg.get_node(node_id) {
                // Check if this node's symbol has external references
                // Use the node's ID (which should be the symbol ID string)
                if let Ok(refs) = symbol_table.get_outgoing_refs(&node.id) {
                    for ext_ref in refs {
                        if !to_load.contains(&ext_ref.target_project_id) {
                            to_load.push(ext_ref.target_project_id.clone());
                        }
                    }
                }
            }
        }

        Ok(())
    }

    /// Merge external PDG into root PDG
    fn merge_pdgs(
        root_pdg: &mut ProgramDependenceGraph,
        external_pdg: &ProgramDependenceGraph,
    ) -> Result<(), MergeError> {
        // Track ID mappings to avoid conflicts
        let mut node_map = HashMap::new();

        // Add all nodes from external PDG
        for node_id in external_pdg.node_indices() {
            if let Some(node) = external_pdg.get_node(node_id) {
                // Add node to root PDG - returns the new NodeId
                let new_id = root_pdg.add_node(node.clone());

                // Store mapping
                node_map.insert(node_id, new_id);
            }
        }

        // Add all edges from external PDG
        for edge_id in external_pdg.edge_indices() {
            if let Some(edge) = external_pdg.get_edge(edge_id) {
                // Get edge endpoints
                if let Some((source, target)) = external_pdg.edge_endpoints(edge_id) {
                    let new_source = node_map.get(&source).copied().unwrap_or(source);
                    let new_target = node_map.get(&target).copied().unwrap_or(target);

                    // Add edge to root PDG (returns Option<EdgeId>)
                    root_pdg.add_edge(new_source, new_target, edge.clone());
                }
            }
        }

        Ok(())
    }

    /// Track which external symbols are used by a project
    pub fn track_external_usage(
        &self,
        project_id: &str,
        used_symbols: &[GlobalSymbolId],
    ) -> Result<(), ResolutionError> {
        let symbol_table = crate::GlobalSymbolTable::new(&self.storage);

        // Verify all symbols exist and track them
        for symbol_id in used_symbols {
            let symbol = symbol_table
                .get_symbol(symbol_id)?
                .ok_or_else(|| ResolutionError::SymbolNotFound(symbol_id.clone()))?;

            if symbol.project_id != project_id {
                // This is an external symbol being used
                // We could track this in a usage table if needed
            }
        }

        Ok(())
    }

    /// Find all projects that depend on a given symbol
    pub fn find_dependents(
        &self,
        symbol_id: &GlobalSymbolId,
    ) -> Result<Vec<String>, ResolutionError> {
        let symbol_table = crate::GlobalSymbolTable::new(&self.storage);

        // Get all incoming references to this symbol
        let refs = symbol_table.get_incoming_refs(symbol_id)?;

        // Collect unique project IDs
        let mut projects = HashSet::new();
        for ext_ref in refs {
            projects.insert(ext_ref.source_project_id);
        }

        Ok(projects.into_iter().collect())
    }

    /// Propagate changes through dependency graph
    ///
    /// Returns list of projects that need re-indexing when a symbol changes.
    /// This includes both direct symbol references and transitive project dependencies.
    pub fn propagate_changes(
        &self,
        changed_project_id: &str,
        changed_symbols: &[GlobalSymbolId],
    ) -> Result<Vec<String>, ResolutionError> {
        let mut affected_projects = HashSet::new();
        let symbol_table = crate::GlobalSymbolTable::new(&self.storage);

        // Worklist for iterative transitive dependency resolution
        let mut worklist: Vec<String> = Vec::new();

        // For each changed symbol, find all direct dependents
        for symbol_id in changed_symbols {
            let dependents = self.find_dependents(symbol_id)?;
            for project_id in dependents {
                if project_id != changed_project_id && !affected_projects.contains(&project_id) {
                    affected_projects.insert(project_id.clone());
                    worklist.push(project_id);
                }
            }
        }

        // Propagate transitively through project dependencies
        while let Some(project_id) = worklist.pop() {
            // Find all projects that depend on this project
            if let Ok(deps) = symbol_table.get_reverse_project_deps(&project_id) {
                for dep in deps {
                    let dep_project = dep.project_id.clone();
                    if !affected_projects.contains(&dep_project) {
                        affected_projects.insert(dep_project.clone());
                        worklist.push(dep_project);
                    }
                }
            }
        }

        Ok(affected_projects.into_iter().collect())
    }

    /// Get all PDGs currently loaded in cache
    pub fn loaded_pdgs(&self) -> Vec<String> {
        self.pdg_cache.keys().cloned().collect()
    }

    /// Check if a PDG is loaded
    pub fn is_pdg_loaded(&self, project_id: &str) -> bool {
        self.pdg_cache.contains_key(project_id)
    }

    /// Get PDG from cache if available
    pub fn get_cached_pdg(&self, project_id: &str) -> Option<&ProgramDependenceGraph> {
        self.pdg_cache.get(project_id)
    }
}

/// Resolved symbol with context
#[derive(Debug, Clone)]
pub struct ResolvedSymbol {
    /// The global symbol information
    pub symbol: GlobalSymbol,
    /// ID of the project containing the symbol
    pub project_id: String,
    /// Whether the symbol is local to the current project
    pub is_local: bool,
    /// Whether the project's PDG is currently loaded in memory
    pub is_loaded: bool,
}

/// Errors for cross-project resolution
#[derive(Debug, Error)]
pub enum ResolutionError {
    /// The specified symbol was not found
    #[error("Symbol not found: {0}")]
    SymbolNotFound(String),

    /// Multiple symbols match the name across different projects
    #[error("Ambiguous symbol: {0} found in {1} projects")]
    AmbiguousSymbol(String, usize),

    /// Failed to load the Program Dependence Graph for an external project
    #[error("Failed to load external PDG for project {0}: {1}")]
    PdgLoadFailed(String, #[source] PdgStoreError),

    /// A circular dependency was detected between projects
    #[error("Circular dependency detected: {0}")]
    CircularDependency(String),

    /// The resolution process exceeded the maximum allowed depth
    #[error("Maximum depth exceeded")]
    MaxDepthExceeded,

    /// An error occurred while merging Program Dependence Graphs
    #[error("Merge error: {0}")]
    MergeError(String),

    /// An error occurred during global symbol operations
    #[error("Global symbol error: {0}")]
    GlobalSymbolError(#[from] crate::global_symbols::GlobalSymbolError),
}

/// Error for PDG merging
#[derive(Debug, Error)]
pub enum MergeError {
    /// Conflict between local and external node IDs
    #[error("Node ID conflict: {0:?} exists in both local and external")]
    NodeConflict(NodeId),

    /// Conflict between local and external edge IDs
    #[error("Edge ID conflict: {0:?} exists in both local and external")]
    EdgeConflict(EdgeId),

    /// Merging exceeded the maximum allowed depth
    #[error("Max depth exceeded: {0}")]
    MaxDepthExceeded(usize),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pdg_store::save_pdg;
    use tempfile::NamedTempFile;

    fn create_test_resolver() -> CrossProjectResolver {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = crate::Storage::open(temp_file.path()).unwrap();
        CrossProjectResolver::new(storage)
    }

    fn create_test_pdg(project_id: &str) -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();

        // Add a simple node
        let node_id_str = format!("{}::test_func", project_id);
        let node = legraphe::pdg::Node {
            id: node_id_str,
            node_type: legraphe::pdg::NodeType::Function,
            name: "test_func".to_string(),
            file_path: "src/test.rs".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            language: "rust".to_string(),
            embedding: None,
        };
        pdg.add_node(node);

        pdg
    }

    #[test]
    fn test_resolve_symbol_not_found() {
        let mut resolver = create_test_resolver();

        let result = resolver.resolve_symbol("test_proj", "nonexistent");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ResolutionError::SymbolNotFound(_)
        ));
    }

    #[test]
    fn test_resolve_symbol_single_match() {
        let mut resolver = create_test_resolver();
        let symbol_table = crate::GlobalSymbolTable::new(resolver.storage());

        // Add a symbol
        let symbol = crate::GlobalSymbol {
            symbol_id: crate::GlobalSymbolTable::generate_symbol_id("proj_a", "foo", None),
            project_id: "proj_a".to_string(),
            symbol_name: "foo".to_string(),
            symbol_type: crate::global_symbols::SymbolType::Function,
            signature: None,
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        symbol_table.upsert_symbol(&symbol).unwrap();

        let results = resolver.resolve_symbol("proj_b", "foo").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].symbol.symbol_name, "foo");
        assert!(!results[0].is_local); // Not local to proj_b
    }

    #[test]
    fn test_load_external_pdg() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = crate::Storage::open(temp_file.path()).unwrap();
        let mut resolver = CrossProjectResolver::new(storage);

        // First load should succeed
        let pdg = create_test_pdg("test_proj");
        {
            let temp_file_path = temp_file.path();
            let mut temp_storage = crate::Storage::open(temp_file_path).unwrap();
            save_pdg(&mut temp_storage, "test_proj", &pdg).unwrap();
        }

        resolver.load_external_pdg("test_proj").unwrap();
        assert!(resolver.is_pdg_loaded("test_proj"));

        // Second load should be a no-op
        resolver.load_external_pdg("test_proj").unwrap();
    }

    #[test]
    fn test_max_depth_exceeded() {
        // Create a new storage for this test
        let temp_file = NamedTempFile::new().unwrap();
        let storage = crate::Storage::open(temp_file.path()).unwrap();
        let mut resolver = CrossProjectResolver::with_max_depth(storage, 0);

        // Max depth is 0, so any load beyond root should fail
        assert!(matches!(
            resolver.load_external_pdg("external_proj"),
            Err(ResolutionError::MaxDepthExceeded)
        ));
    }

    #[test]
    fn test_find_dependents() {
        let resolver = create_test_resolver();
        let symbol_table = crate::GlobalSymbolTable::new(resolver.storage());

        // Create symbols in different projects
        let symbol_a = crate::GlobalSymbol {
            symbol_id: crate::GlobalSymbolTable::generate_symbol_id("proj_a", "foo", None),
            project_id: "proj_a".to_string(),
            symbol_name: "foo".to_string(),
            symbol_type: crate::global_symbols::SymbolType::Function,
            signature: None,
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        let symbol_b = crate::GlobalSymbol {
            symbol_id: crate::GlobalSymbolTable::generate_symbol_id("proj_b", "bar", None),
            project_id: "proj_b".to_string(),
            symbol_name: "bar".to_string(),
            symbol_type: crate::global_symbols::SymbolType::Function,
            signature: None,
            file_path: "src/b.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        symbol_table.upsert_symbol(&symbol_a).unwrap();
        symbol_table.upsert_symbol(&symbol_b).unwrap();

        // Add external reference from proj_b to proj_a's foo
        let ext_ref = crate::global_symbols::ExternalRef {
            ref_id: "ref_123".to_string(),
            source_project_id: "proj_b".to_string(),
            source_symbol_id: symbol_b.symbol_id.clone(),
            target_project_id: "proj_a".to_string(),
            target_symbol_id: symbol_a.symbol_id.clone(),
            ref_type: crate::global_symbols::RefType::Call,
        };

        symbol_table.add_external_ref(&ext_ref).unwrap();

        // Find dependents of symbol_a
        let dependents = resolver.find_dependents(&symbol_a.symbol_id).unwrap();
        assert_eq!(dependents.len(), 1);
        assert_eq!(dependents[0], "proj_b");
    }

    #[test]
    fn test_propagate_changes() {
        let resolver = create_test_resolver();
        let symbol_table = crate::GlobalSymbolTable::new(resolver.storage());

        // Create symbols
        let symbol_a = crate::GlobalSymbol {
            symbol_id: crate::GlobalSymbolTable::generate_symbol_id("proj_a", "util", None),
            project_id: "proj_a".to_string(),
            symbol_name: "util".to_string(),
            symbol_type: crate::global_symbols::SymbolType::Function,
            signature: None,
            file_path: "src/a.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        let symbol_b = crate::GlobalSymbol {
            symbol_id: crate::GlobalSymbolTable::generate_symbol_id("proj_b", "bar", None),
            project_id: "proj_b".to_string(),
            symbol_name: "bar".to_string(),
            symbol_type: crate::global_symbols::SymbolType::Function,
            signature: None,
            file_path: "src/b.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        let symbol_c = crate::GlobalSymbol {
            symbol_id: crate::GlobalSymbolTable::generate_symbol_id("proj_c", "baz", None),
            project_id: "proj_c".to_string(),
            symbol_name: "baz".to_string(),
            symbol_type: crate::global_symbols::SymbolType::Function,
            signature: None,
            file_path: "src/c.rs".to_string(),
            byte_range: (0, 50),
            complexity: 1,
            is_public: true,
        };

        symbol_table.upsert_symbol(&symbol_a).unwrap();
        symbol_table.upsert_symbol(&symbol_b).unwrap();
        symbol_table.upsert_symbol(&symbol_c).unwrap();

        // proj_b depends on proj_a
        let ref_ba = crate::global_symbols::ExternalRef {
            ref_id: "ref_ba".to_string(),
            source_project_id: "proj_b".to_string(),
            source_symbol_id: symbol_b.symbol_id.clone(),
            target_project_id: "proj_a".to_string(),
            target_symbol_id: symbol_a.symbol_id.clone(),
            ref_type: crate::global_symbols::RefType::Call,
        };

        // proj_c depends on proj_b
        let ref_cb = crate::global_symbols::ExternalRef {
            ref_id: "ref_cb".to_string(),
            source_project_id: "proj_c".to_string(),
            source_symbol_id: symbol_c.symbol_id.clone(),
            target_project_id: "proj_b".to_string(),
            target_symbol_id: symbol_b.symbol_id.clone(),
            ref_type: crate::global_symbols::RefType::Call,
        };

        symbol_table.add_external_ref(&ref_ba).unwrap();
        symbol_table.add_external_ref(&ref_cb).unwrap();

        // Add project dependencies
        let dep_ab = crate::global_symbols::ProjectDep {
            dep_id: "dep_ab".to_string(),
            project_id: "proj_b".to_string(),
            depends_on_project_id: "proj_a".to_string(),
            dependency_type: crate::global_symbols::DepType::Direct,
        };

        let dep_bc = crate::global_symbols::ProjectDep {
            dep_id: "dep_bc".to_string(),
            project_id: "proj_c".to_string(),
            depends_on_project_id: "proj_b".to_string(),
            dependency_type: crate::global_symbols::DepType::Direct,
        };

        symbol_table.add_project_dep(&dep_ab).unwrap();
        symbol_table.add_project_dep(&dep_bc).unwrap();

        // Propagate changes from proj_a
        let affected = resolver
            .propagate_changes("proj_a", std::slice::from_ref(&symbol_a.symbol_id))
            .unwrap();

        // Should include proj_b (direct dependent) and proj_c (transitive through proj_b)
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&"proj_b".to_string()));
        assert!(affected.contains(&"proj_c".to_string()));
    }

    #[test]
    fn test_track_external_usage() {
        let resolver = create_test_resolver();
        let symbol_table = crate::GlobalSymbolTable::new(resolver.storage());

        // Create external symbol
        let ext_symbol = crate::GlobalSymbol {
            symbol_id: crate::GlobalSymbolTable::generate_symbol_id("external", "util", None),
            project_id: "external".to_string(),
            symbol_name: "util".to_string(),
            symbol_type: crate::global_symbols::SymbolType::Function,
            signature: None,
            file_path: "lib/util.rs".to_string(),
            byte_range: (0, 100),
            complexity: 10,
            is_public: true,
        };

        symbol_table.upsert_symbol(&ext_symbol).unwrap();

        // Track usage
        let result = resolver
            .track_external_usage("my_project", std::slice::from_ref(&ext_symbol.symbol_id));

        assert!(result.is_ok());
    }

    #[test]
    fn test_persistent_cache() {
        let temp_file = NamedTempFile::new().unwrap();
        let storage = crate::Storage::open(temp_file.path()).unwrap();
        let mut resolver = CrossProjectResolver::new(storage);

        // Load PDG
        let pdg = create_test_pdg("test_proj");
        {
            // Need a mutable storage for save_pdg
            let temp_file_path = temp_file.path();
            let mut temp_storage = crate::Storage::open(temp_file_path).unwrap();
            save_pdg(&mut temp_storage, "test_proj", &pdg).unwrap();
        }

        resolver.load_external_pdg("test_proj").unwrap();
        assert!(resolver.is_pdg_loaded("test_proj"));

        // Get cached PDG
        let cached = resolver.get_cached_pdg("test_proj");
        assert!(cached.is_some());
        assert_eq!(cached.unwrap().node_count(), 1);

        // Check loaded PDGs list
        let loaded = resolver.loaded_pdgs();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], "test_proj");

        // Clear cache
        resolver.clear_cache();
        assert!(!resolver.is_pdg_loaded("test_proj"));
    }

    #[test]
    fn test_build_cross_project_pdg() {
        let temp_file = NamedTempFile::new().unwrap();
        let _storage = crate::Storage::open(temp_file.path()).unwrap();

        // Create root project PDG
        let mut root_pdg = ProgramDependenceGraph::new();
        let root_node = legraphe::pdg::Node {
            id: "root_func".to_string(),
            node_type: legraphe::pdg::NodeType::Function,
            name: "root_func".to_string(),
            file_path: "src/root.rs".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            language: "rust".to_string(),
            embedding: None,
        };
        root_pdg.add_node(root_node);

        // Create external project PDG
        let mut ext_pdg = ProgramDependenceGraph::new();
        let ext_node = legraphe::pdg::Node {
            id: "ext_func".to_string(),
            node_type: legraphe::pdg::NodeType::Function,
            name: "ext_func".to_string(),
            file_path: "src/ext.rs".to_string(),
            byte_range: (0, 100),
            complexity: 3,
            language: "rust".to_string(),
            embedding: None,
        };
        ext_pdg.add_node(ext_node);

        // Save PDGs
        {
            let mut temp_storage = crate::Storage::open(temp_file.path()).unwrap();
            save_pdg(&mut temp_storage, "root_proj", &root_pdg).unwrap();
        }
        {
            let mut temp_storage = crate::Storage::open(temp_file.path()).unwrap();
            save_pdg(&mut temp_storage, "ext_proj", &ext_pdg).unwrap();
        }

        // Build cross-project PDG - load both PDGs and merge them
        let storage_for_resolver = crate::Storage::open(temp_file.path()).unwrap();
        let mut resolver = CrossProjectResolver::new(storage_for_resolver);

        // Load both PDGs manually
        resolver.load_external_pdg("root_proj").unwrap();
        resolver.load_external_pdg("ext_proj").unwrap();

        // Build merged PDG
        let merged = resolver
            .build_cross_project_pdg("root_proj", Some(1))
            .unwrap();

        // Should have nodes from both projects
        assert!(merged.node_count() >= 2);
    }
}
