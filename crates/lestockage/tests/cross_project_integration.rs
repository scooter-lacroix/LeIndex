// Integration tests for cross-project resolution
//
// These tests verify the end-to-end functionality of cross-project
// symbol resolution, PDG merging, and change propagation.

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;
    use lestockage::{Storage, GlobalSymbolTable, GlobalSymbol, CrossProjectResolver};
    use lestockage::global_symbols::{ExternalRef, ProjectDep, SymbolType, RefType, DepType};
    use legraphe::pdg::{ProgramDependenceGraph, Node, NodeType, Edge, EdgeType, EdgeMetadata};
    use lestockage::pdg_store::save_pdg;

    /// Helper: Create a test storage with temp file
    fn create_test_storage() -> Storage {
        let temp_file = NamedTempFile::new().unwrap();
        Storage::open(temp_file.path()).unwrap()
    }

    /// Helper: Create a simple test PDG
    fn create_test_pdg(project_id: &str, functions: &[&str]) -> ProgramDependenceGraph {
        let mut pdg = ProgramDependenceGraph::new();
        for func_name in functions {
            let node = Node {
                id: format!("{}::{}", project_id, func_name),
                node_type: NodeType::Function,
                name: func_name.to_string(),
                file_path: format!("src/{}.rs", func_name),
                byte_range: (0, 100),
                complexity: 5,
                embedding: None,
            };
            pdg.add_node(node);
        }
        pdg
    }

    /// Helper: Create a test symbol
    fn create_test_symbol(project_id: &str, name: &str) -> GlobalSymbol {
        GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id(project_id, name, None),
            project_id: project_id.to_string(),
            symbol_name: name.to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: format!("src/{}.rs", name),
            byte_range: (0, 100),
            complexity: 5,
            is_public: true,
        }
    }

    #[test]
    fn test_resolve_symbol_across_two_projects() {
        // Create Project A with function foo()
        let storage = create_test_storage();
        let symbol_table = GlobalSymbolTable::new(&storage);

        let symbol_a = create_test_symbol("project_a", "foo");
        symbol_table.upsert_symbol(&symbol_a).unwrap();

        // Save PDG for project A
        let pdg_a = create_test_pdg("project_a", &["foo", "bar"]);
        {
            let temp_file = NamedTempFile::new().unwrap();
            let mut temp_storage = Storage::open(temp_file.path()).unwrap();
            save_pdg(&mut temp_storage, "project_a", &pdg_a).unwrap();
        }

        // Create Project B with function that references foo()
        let symbol_b = create_test_symbol("project_b", "caller");
        symbol_table.upsert_symbol(&symbol_b).unwrap();

        // Add external reference from project_b to project_a's foo
        let ext_ref = ExternalRef {
            ref_id: "ref_1".to_string(),
            source_project_id: "project_b".to_string(),
            source_symbol_id: symbol_b.symbol_id.clone(),
            target_project_id: "project_a".to_string(),
            target_symbol_id: symbol_a.symbol_id.clone(),
            ref_type: RefType::Call,
        };
        symbol_table.add_external_ref(&ext_ref).unwrap();

        // Verify resolution finds foo in project_a
        let results = symbol_table.resolve_by_name("foo").unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].project_id, "project_a");
    }

    #[test]
    fn test_ambiguous_symbol_resolution() {
        // Create Project A with function util()
        // Create Project B with function util()
        // Verify resolution returns both with context

        let storage = create_test_storage();
        let symbol_table = GlobalSymbolTable::new(&storage);

        let util_a = create_test_symbol("project_a", "util");
        let util_b = create_test_symbol("project_b", "util");

        symbol_table.upsert_symbol(&util_a).unwrap();
        symbol_table.upsert_symbol(&util_b).unwrap();

        // Resolve by name should return both
        let results = symbol_table.resolve_by_name("util").unwrap();
        assert_eq!(results.len(), 2);

        // Verify we got one from each project
        let project_ids: Vec<&str> = results.iter().map(|s| s.project_id.as_str()).collect();
        assert!(project_ids.contains(&"project_a"));
        assert!(project_ids.contains(&"project_b"));
    }

    #[test]
    fn test_external_pdg_lazy_loading() {
        // Create project with external reference
        // Verify PDG loads on demand
        // Verify cache works

        let storage = create_test_storage();
        let mut resolver = CrossProjectResolver::new(storage);

        // Save PDG for external project
        let ext_pdg = create_test_pdg("external_lib", &["external_func"]);
        {
            let temp_file = NamedTempFile::new().unwrap();
            let mut temp_storage = Storage::open(temp_file.path()).unwrap();
            save_pdg(&mut temp_storage, "external_lib", &ext_pdg).unwrap();
        }

        // First load should succeed and populate cache
        resolver.load_external_pdg("external_lib").unwrap();
        assert!(resolver.is_pdg_loaded("external_lib"));

        let loaded = resolver.loaded_pdgs();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0], "external_lib");

        // Second load should be a no-op (already cached)
        resolver.load_external_pdg("external_lib").unwrap();

        // Verify cache still has only one entry
        let loaded = resolver.loaded_pdgs();
        assert_eq!(loaded.len(), 1);
    }

    #[test]
    fn test_change_propagation() {
        // Create dependency chain: A -> B -> C
        // Modify symbol in C
        // Verify A and B marked for re-index

        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();
        let symbol_table = GlobalSymbolTable::new(&storage);

        // Create symbols in projects A, B, C
        let sym_c = create_test_symbol("project_c", "shared_util");
        let sym_b = create_test_symbol("project_b", "consumer");
        let sym_a = create_test_symbol("project_a", "app");

        symbol_table.upsert_symbol(&sym_c).unwrap();
        symbol_table.upsert_symbol(&sym_b).unwrap();
        symbol_table.upsert_symbol(&sym_a).unwrap();

        // B depends on C (direct dependency)
        let ref_bc = ExternalRef {
            ref_id: "ref_bc".to_string(),
            source_project_id: "project_b".to_string(),
            source_symbol_id: sym_b.symbol_id.clone(),
            target_project_id: "project_c".to_string(),
            target_symbol_id: sym_c.symbol_id.clone(),
            ref_type: RefType::Call,
        };
        symbol_table.add_external_ref(&ref_bc).unwrap();

        // A depends on B (transitive through B)
        let ref_ab = ExternalRef {
            ref_id: "ref_ab".to_string(),
            source_project_id: "project_a".to_string(),
            source_symbol_id: sym_a.symbol_id.clone(),
            target_project_id: "project_b".to_string(),
            target_symbol_id: sym_b.symbol_id.clone(),
            ref_type: RefType::Call,
        };
        symbol_table.add_external_ref(&ref_ab).unwrap();

        // Add project dependencies
        let dep_ab = ProjectDep {
            dep_id: "dep_ab".to_string(),
            project_id: "project_a".to_string(),
            depends_on_project_id: "project_b".to_string(),
            dependency_type: DepType::Direct,
        };
        symbol_table.add_project_dep(&dep_ab).unwrap();

        let dep_bc = ProjectDep {
            dep_id: "dep_bc".to_string(),
            project_id: "project_b".to_string(),
            depends_on_project_id: "project_c".to_string(),
            dependency_type: DepType::Direct,
        };
        symbol_table.add_project_dep(&dep_bc).unwrap();

        // Create resolver using the same storage file
        let resolver = CrossProjectResolver::new(storage);

        let affected = resolver
            .propagate_changes("project_c", &[sym_c.symbol_id.clone()])
            .unwrap();

        // Should include project_b (direct dependent) and project_a (transitive)
        assert_eq!(affected.len(), 2);
        assert!(affected.contains(&"project_b".to_string()));
        assert!(affected.contains(&"project_a".to_string()));
    }

    #[test]
    fn test_cross_project_pdg_merging() {
        // Create two projects with their own PDGs
        // Merge them into a single cross-project PDG
        // Verify nodes and edges are preserved

        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();

        // Create root project PDG with connected nodes
        let mut root_pdg = ProgramDependenceGraph::new();
        let node_a = Node {
            id: "root_project::func_a".to_string(),
            node_type: NodeType::Function,
            name: "func_a".to_string(),
            file_path: "src/func_a.rs".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            embedding: None,
        };
        let id_a = root_pdg.add_node(node_a);

        let node_b = Node {
            id: "root_project::func_b".to_string(),
            node_type: NodeType::Function,
            name: "func_b".to_string(),
            file_path: "src/func_b.rs".to_string(),
            byte_range: (0, 100),
            complexity: 3,
            embedding: None,
        };
        let id_b = root_pdg.add_node(node_b);

        // Add edge
        let edge = Edge {
            edge_type: EdgeType::Call,
            metadata: EdgeMetadata {
                call_count: Some(1),
                variable_name: None,
            },
        };
        root_pdg.add_edge(id_a, id_b, edge);

        // Save root PDG using the same temp file
        {
            let mut temp_storage = Storage::open(temp_file.path()).unwrap();
            save_pdg(&mut temp_storage, "root_project", &root_pdg).unwrap();
        }

        // Create external project PDG
        let mut ext_pdg = ProgramDependenceGraph::new();
        let node_x = Node {
            id: "ext_project::func_x".to_string(),
            node_type: NodeType::Function,
            name: "func_x".to_string(),
            file_path: "src/func_x.rs".to_string(),
            byte_range: (0, 100),
            complexity: 4,
            embedding: None,
        };
        let id_x = ext_pdg.add_node(node_x);

        let node_y = Node {
            id: "ext_project::func_y".to_string(),
            node_type: NodeType::Function,
            name: "func_y".to_string(),
            file_path: "src/func_y.rs".to_string(),
            byte_range: (0, 100),
            complexity: 2,
            embedding: None,
        };
        let id_y = ext_pdg.add_node(node_y);

        let ext_edge = Edge {
            edge_type: EdgeType::DataDependency,
            metadata: EdgeMetadata {
                call_count: None,
                variable_name: Some("data".to_string()),
            },
        };
        ext_pdg.add_edge(id_x, id_y, ext_edge);

        // Save external PDG using the same temp file
        {
            let mut temp_storage = Storage::open(temp_file.path()).unwrap();
            save_pdg(&mut temp_storage, "ext_project", &ext_pdg).unwrap();
        }

        // Create cross-project resolver using the same storage file
        let mut resolver = CrossProjectResolver::new(storage);

        resolver.load_external_pdg("root_project").unwrap();
        resolver.load_external_pdg("ext_project").unwrap();

        let merged = resolver
            .build_cross_project_pdg("root_project", Some(2))
            .unwrap();

        // Verify nodes and edges from both projects are present
        assert!(merged.node_count() >= 4);
        assert!(merged.edge_count() >= 2);
    }

    #[test]
    fn test_find_dependents() {
        // Create symbol with multiple dependents
        // Verify find_dependents returns all dependent projects

        let temp_file = NamedTempFile::new().unwrap();
        let storage = Storage::open(temp_file.path()).unwrap();
        let symbol_table = GlobalSymbolTable::new(&storage);

        let shared_sym = create_test_symbol("shared_lib", "common");
        let caller_a = create_test_symbol("project_a", "caller_a");
        let caller_b = create_test_symbol("project_b", "caller_b");

        symbol_table.upsert_symbol(&shared_sym).unwrap();
        symbol_table.upsert_symbol(&caller_a).unwrap();
        symbol_table.upsert_symbol(&caller_b).unwrap();

        // Both callers depend on shared_lib
        let ref_a = ExternalRef {
            ref_id: "ref_a".to_string(),
            source_project_id: "project_a".to_string(),
            source_symbol_id: caller_a.symbol_id.clone(),
            target_project_id: "shared_lib".to_string(),
            target_symbol_id: shared_sym.symbol_id.clone(),
            ref_type: RefType::Call,
        };
        symbol_table.add_external_ref(&ref_a).unwrap();

        let ref_b = ExternalRef {
            ref_id: "ref_b".to_string(),
            source_project_id: "project_b".to_string(),
            source_symbol_id: caller_b.symbol_id.clone(),
            target_project_id: "shared_lib".to_string(),
            target_symbol_id: shared_sym.symbol_id.clone(),
            ref_type: RefType::Call,
        };
        symbol_table.add_external_ref(&ref_b).unwrap();

        // Create resolver using the same storage file
        let resolver = CrossProjectResolver::new(storage);

        let dependents = resolver
            .find_dependents(&shared_sym.symbol_id)
            .unwrap();

        assert_eq!(dependents.len(), 2);
        assert!(dependents.contains(&"project_a".to_string()));
        assert!(dependents.contains(&"project_b".to_string()));
    }

    #[test]
    fn test_max_depth_limiting() {
        // Create resolver with max depth = 1
        // Verify loading beyond max depth fails

        let storage = create_test_storage();
        let mut resolver = CrossProjectResolver::with_max_depth(storage, 1);

        // First external PDG should load (depth 1)
        let pdg_1 = create_test_pdg("ext_1", &["func"]);
        {
            let temp_file = NamedTempFile::new().unwrap();
            let mut temp_storage = Storage::open(temp_file.path()).unwrap();
            save_pdg(&mut temp_storage, "ext_1", &pdg_1).unwrap();
        }
        resolver.load_external_pdg("ext_1").unwrap();

        // Second external PDG should fail (would exceed depth 2)
        let result = resolver.load_external_pdg("ext_2");
        assert!(matches!(
            result,
            Err(lestockage::cross_project::ResolutionError::MaxDepthExceeded)
        ));
    }

    #[test]
    fn test_public_symbol_discovery() {
        // Create mix of public and private symbols
        // Verify find_public_symbols only returns public ones

        let storage = create_test_storage();
        let symbol_table = GlobalSymbolTable::new(&storage);

        // Public symbol
        let public_sym = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("my_lib", "api", None),
            project_id: "my_lib".to_string(),
            symbol_name: "api".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/api.rs".to_string(),
            byte_range: (0, 100),
            complexity: 5,
            is_public: true,
        };

        // Private symbol
        let private_sym = GlobalSymbol {
            symbol_id: GlobalSymbolTable::generate_symbol_id("my_lib", "internal", None),
            project_id: "my_lib".to_string(),
            symbol_name: "internal".to_string(),
            symbol_type: SymbolType::Function,
            signature: None,
            file_path: "src/internal.rs".to_string(),
            byte_range: (0, 100),
            complexity: 3,
            is_public: false,
        };

        symbol_table.upsert_symbol(&public_sym).unwrap();
        symbol_table.upsert_symbol(&private_sym).unwrap();

        // Find public symbols
        let public_syms = symbol_table.find_public_symbols("my_lib").unwrap();
        assert_eq!(public_syms.len(), 1);
        assert_eq!(public_syms[0].symbol_name, "api");
        assert!(public_syms[0].is_public);
    }

    #[test]
    fn test_symbol_conflict_detection() {
        // Create two symbols with same name in different projects
        // Verify detect_conflicts identifies them

        let storage = create_test_storage();
        let symbol_table = GlobalSymbolTable::new(&storage);

        let util_a = create_test_symbol("project_a", "util");
        let util_b = create_test_symbol("project_b", "util");

        symbol_table.upsert_symbol(&util_a).unwrap();
        symbol_table.upsert_symbol(&util_b).unwrap();

        // Detect conflicts
        let conflicts = symbol_table.detect_conflicts("util").unwrap();
        assert_eq!(conflicts.len(), 2);

        // Verify both projects are represented
        let project_ids: Vec<&str> = conflicts.iter().map(|s| s.project_id.as_str()).collect();
        assert!(project_ids.contains(&"project_a"));
        assert!(project_ids.contains(&"project_b"));
    }

    #[test]
    fn test_batch_symbol_insert() {
        // Test batch insert performance and correctness
        let storage = create_test_storage();
        let symbol_table = GlobalSymbolTable::new(&storage);

        let symbols: Vec<GlobalSymbol> = (0..100)
            .map(|i| GlobalSymbol {
                symbol_id: GlobalSymbolTable::generate_symbol_id(
                    "test_project",
                    &format!("func_{}", i),
                    None
                ),
                project_id: "test_project".to_string(),
                symbol_name: format!("func_{}", i),
                symbol_type: SymbolType::Function,
                signature: None,
                file_path: format!("src/func_{}.rs", i),
                byte_range: (0, 100),
                complexity: (i % 10) as u32,
                is_public: i % 2 == 0,
            })
            .collect();

        // Batch insert should succeed
        symbol_table.upsert_symbols_batch(&symbols).unwrap();

        // Verify all symbols were inserted
        let all_syms = symbol_table.get_project_symbols("test_project").unwrap();
        assert_eq!(all_syms.len(), 100);
    }

    #[test]
    fn test_incoming_outgoing_refs() {
        // Create a chain of references: A -> B -> C
        // Verify get_outgoing_refs and get_incoming_refs work correctly

        let storage = create_test_storage();
        let symbol_table = GlobalSymbolTable::new(&storage);

        let sym_a = create_test_symbol("project_a", "func_a");
        let sym_b = create_test_symbol("project_b", "func_b");
        let sym_c = create_test_symbol("project_c", "func_c");

        symbol_table.upsert_symbol(&sym_a).unwrap();
        symbol_table.upsert_symbol(&sym_b).unwrap();
        symbol_table.upsert_symbol(&sym_c).unwrap();

        // A calls B
        let ref_ab = ExternalRef {
            ref_id: "ref_ab".to_string(),
            source_project_id: "project_a".to_string(),
            source_symbol_id: sym_a.symbol_id.clone(),
            target_project_id: "project_b".to_string(),
            target_symbol_id: sym_b.symbol_id.clone(),
            ref_type: RefType::Call,
        };
        symbol_table.add_external_ref(&ref_ab).unwrap();

        // B calls C
        let ref_bc = ExternalRef {
            ref_id: "ref_bc".to_string(),
            source_project_id: "project_b".to_string(),
            source_symbol_id: sym_b.symbol_id.clone(),
            target_project_id: "project_c".to_string(),
            target_symbol_id: sym_c.symbol_id.clone(),
            ref_type: RefType::Call,
        };
        symbol_table.add_external_ref(&ref_bc).unwrap();

        // A -> B: outgoing from A should include B
        let outgoing_a = symbol_table.get_outgoing_refs(&sym_a.symbol_id).unwrap();
        assert_eq!(outgoing_a.len(), 1);
        assert_eq!(outgoing_a[0].target_project_id, "project_b");

        // B <- A: incoming to B should include A
        let incoming_b = symbol_table.get_incoming_refs(&sym_b.symbol_id).unwrap();
        assert_eq!(incoming_b.len(), 1);
        assert_eq!(incoming_b[0].source_project_id, "project_a");

        // B -> C: outgoing from B should include C
        let outgoing_b = symbol_table.get_outgoing_refs(&sym_b.symbol_id).unwrap();
        assert_eq!(outgoing_b.len(), 1);
        assert_eq!(outgoing_b[0].target_project_id, "project_c");

        // C <- B: incoming to C should include B
        let incoming_c = symbol_table.get_incoming_refs(&sym_c.symbol_id).unwrap();
        assert_eq!(incoming_c.len(), 1);
        assert_eq!(incoming_c[0].source_project_id, "project_b");
    }
}
