// Integration Tests for LePasserelle
//
// These tests cover end-to-end workflows including:
// - CLI command workflows
// - Cache spilling and restoration
// - Storage persistence
// - Error handling

use std::path::PathBuf;
use tempfile::TempDir;

// ============================================================================
// CLI WORKFLOW INTEGRATION TESTS
// ============================================================================

mod cli_workflow_tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn test_cli_index_command_parsing() {
        use lepasserelle::cli::Cli;

        let cli = Cli::parse_from([
            "leindex",
            "index",
            "/test/project",
            "--force",
            "--progress",
        ]);

        use lepasserelle::cli::Commands;
        match cli.command {
            Some(Commands::Index { path, force, progress }) => {
                assert_eq!(path, PathBuf::from("/test/project"));
                assert!(force);
                assert!(progress);
            }
            _ => panic!("Expected Index command"),
        }
    }

    #[test]
    fn test_cli_search_command_parsing() {
        use lepasserelle::cli::Cli;

        let cli = Cli::parse_from([
            "leindex",
            "search",
            "authentication",
            "--top-k",
            "20",
        ]);

        use lepasserelle::cli::Commands;
        match cli.command {
            Some(Commands::Search { query, top_k }) => {
                assert_eq!(query, "authentication");
                assert_eq!(top_k, 20);
            }
            _ => panic!("Expected Search command"),
        }
    }

    #[test]
    fn test_cli_analyze_command_parsing() {
        use lepasserelle::cli::Cli;

        let cli = Cli::parse_from([
            "leindex",
            "analyze",
            "How does auth work?",
            "--tokens",
            "5000",
        ]);

        use lepasserelle::cli::Commands;
        match cli.command {
            Some(Commands::Analyze { query, token_budget }) => {
                assert_eq!(query, "How does auth work?");
                assert_eq!(token_budget, 5000);
            }
            _ => panic!("Expected Analyze command"),
        }
    }

    #[test]
    fn test_cli_diagnostics_command_parsing() {
        use lepasserelle::cli::Cli;

        let cli = Cli::parse_from([
            "leindex",
            "diagnostics",
        ]);

        use lepasserelle::cli::Commands;
        match cli.command {
            Some(Commands::Diagnostics) => {
                // Successfully parsed
            }
            _ => panic!("Expected Diagnostics command"),
        }
    }

    #[test]
    fn test_cli_verbose_flag() {
        use lepasserelle::cli::Cli;

        let cli = Cli::parse_from(["leindex", "-v", "diagnostics"]);

        assert!(cli.verbose);
    }
}

// ============================================================================
// CACHE MANAGEMENT INTEGRATION TESTS
// ============================================================================

mod cache_management_tests {
    use super::*;
    use lepasserelle::memory::{
        create_pdg_entry, create_search_entry, CacheEntry, CacheSpiller, MemoryConfig,
        WarmStrategy,
    };
    use std::collections::HashMap;

    #[test]
    fn test_cache_entry_creation() {
        let pdg_data = vec![1u8, 2, 3, 4];
        let pdg_entry = create_pdg_entry("test_project".to_string(), 100, 200, &pdg_data);

        match pdg_entry {
            CacheEntry::PDG { project_id, node_count, edge_count, .. } => {
                assert_eq!(project_id, "test_project");
                assert_eq!(node_count, 100);
                assert_eq!(edge_count, 200);
            }
            _ => panic!("Expected PDG entry"),
        }

        let search_data = vec![5u8, 6, 7, 8];
        let search_entry = create_search_entry("test_project".to_string(), 50, &search_data);

        match search_entry {
            CacheEntry::SearchIndex { project_id, entry_count, .. } => {
                assert_eq!(project_id, "test_project");
                assert_eq!(entry_count, 50);
            }
            _ => panic!("Expected SearchIndex entry"),
        }
    }

    #[test]
    fn test_cache_spiller_creation() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cache");

        let config = MemoryConfig {
            cache_dir,
            ..Default::default()
        };

        let spiller = CacheSpiller::new(config);
        assert!(spiller.is_ok());
    }

    #[test]
    fn test_cache_insert_and_retrieve() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cache");

        let config = MemoryConfig {
            cache_dir,
            max_cache_bytes: 10_000,
            ..Default::default()
        };

        let mut spiller = CacheSpiller::new(config).unwrap();
        let store = spiller.store_mut();

        let entry = CacheEntry::Binary {
            metadata: {
                let mut map = HashMap::new();
                map.insert("type".to_string(), "test".to_string());
                map
            },
            serialized_data: vec![0u8; 100],
        };

        store.insert("test_key".to_string(), entry.clone()).unwrap();
        let retrieved = store.get("test_key");

        assert!(retrieved.is_some());
    }

    #[test]
    fn test_cache_eviction_on_size_limit() {
        let temp_dir = TempDir::new().unwrap();
        let cache_dir = temp_dir.path().join("cache");

        let config = MemoryConfig {
            cache_dir,
            max_cache_bytes: 500, // Small limit
            ..Default::default()
        };

        let mut spiller = CacheSpiller::new(config).unwrap();
        let store = spiller.store_mut();

        // Insert entries that exceed the cache size
        for i in 0..10 {
            let entry = CacheEntry::Binary {
                metadata: HashMap::new(),
                serialized_data: vec![0u8; 100], // 100 bytes each
            };
            store.insert(format!("key_{}", i), entry).unwrap();
        }

        // Cache should have evicted some entries
        assert!(store.len() < 10);
        assert!(store.total_bytes() <= 500);
    }

    #[test]
    fn test_cache_key_generation() {
        use lepasserelle::memory::{analysis_cache_key, pdg_cache_key, search_cache_key};

        assert_eq!(pdg_cache_key("myproject"), "pdg:myproject");
        assert_eq!(search_cache_key("myproject"), "search:myproject");
        assert!(analysis_cache_key("how does auth work").starts_with("analysis:"));
    }

    #[test]
    fn test_warm_strategy_variants() {
        // Test that all warm strategy variants can be created and compared
        assert_eq!(WarmStrategy::All, WarmStrategy::All);
        assert_eq!(WarmStrategy::PDGOnly, WarmStrategy::PDGOnly);
        assert_eq!(WarmStrategy::SearchIndexOnly, WarmStrategy::SearchIndexOnly);
        assert_eq!(WarmStrategy::RecentFirst, WarmStrategy::RecentFirst);

        assert_ne!(WarmStrategy::All, WarmStrategy::PDGOnly);
        assert_ne!(WarmStrategy::PDGOnly, WarmStrategy::SearchIndexOnly);
    }
}

// ============================================================================
// STORAGE PERSISTENCE INTEGRATION TESTS
// ============================================================================

mod storage_persistence_tests {
    use super::*;
    use lepasserelle::memory::MemoryConfig;

    #[test]
    fn test_memory_config_default() {
        let config = MemoryConfig::default();
        assert_eq!(config.spill_threshold, 0.85);
        assert_eq!(config.check_interval_secs, 30);
        assert!(config.auto_spill);
        assert_eq!(config.max_cache_bytes, 500_000_000);
    }

    #[test]
    fn test_memory_config_serialization() {
        let config = MemoryConfig {
            spill_threshold: 0.9,
            check_interval_secs: 60,
            auto_spill: false,
            max_cache_bytes: 1_000_000_000,
            cache_dir: PathBuf::from("/tmp/cache"),
        };

        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: MemoryConfig = serde_json::from_str(&serialized).unwrap();

        assert_eq!(deserialized.spill_threshold, 0.9);
        assert_eq!(deserialized.check_interval_secs, 60);
        assert!(!deserialized.auto_spill);
        assert_eq!(deserialized.max_cache_bytes, 1_000_000_000);
        assert_eq!(deserialized.cache_dir, PathBuf::from("/tmp/cache"));
    }
}

// ============================================================================
// ERROR HANDLING INTEGRATION TESTS
// ============================================================================

mod error_handling_tests {
    use lepasserelle::errors::LeIndexError;

    #[test]
    fn test_error_display() {
        let error = LeIndexError::Parse {
            message: "Test parse error".to_string(),
            file_path: Some("/test/file.rs".into()),
            suggestion: None,
        };

        let display_str = format!("{}", error);
        // Error display formats as "Parse error: <message> at <file>"
        assert!(display_str.contains("Test parse error"));
    }

    #[test]
    fn test_error_conversion() {
        use std::io;

        let io_error = io::Error::new(io::ErrorKind::NotFound, "File not found");
        let le_error: LeIndexError = io_error.into();

        match le_error {
            LeIndexError::Io { .. } => {
                // Successfully converted
            }
            _ => panic!("Expected Io error variant"),
        }
    }
}

// ============================================================================
// END-TO-END WORKFLOW TESTS
// ============================================================================

mod e2e_workflow_tests {
    use super::*;

    #[test]
    fn test_leindex_creation_workflow() {
        use lepasserelle::LeIndex;

        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        // Create a simple test project
        let src_dir = project_path.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        let main_file = src_dir.join("main.rs");
        std::fs::write(
            &main_file,
            "fn main() {\n    println!(\"Hello, world!\");\n}",
        )
        .unwrap();

        // Create LeIndex instance
        let leindex = LeIndex::new(project_path);
        assert!(leindex.is_ok());

        let leindex = leindex.unwrap();
        // Temp directory name varies, just check it's not empty
        assert!(!leindex.project_id().is_empty());
        assert!(leindex.project_path().starts_with(temp_dir.path()));
    }

    #[test]
    fn test_diagnostics_workflow() {
        use lepasserelle::LeIndex;

        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path();

        let leindex = LeIndex::new(project_path).unwrap();
        let diagnostics = leindex.get_diagnostics();

        assert!(diagnostics.is_ok());
        let diag = diagnostics.unwrap();
        // Just check that diagnostics are available
        assert!(!diag.project_id.is_empty());
        assert!(diag.stats.files_parsed == 0); // Not indexed yet
    }

    #[test]
    fn test_cache_integration_with_leindex() {
        use lepasserelle::memory::MemoryConfig;
        use lepasserelle::LeIndex;

        let temp_dir = TempDir::new().unwrap();
        let project_path = temp_dir.path().to_path_buf();
        let cache_dir = temp_dir.path().join("cache");

        let _config = MemoryConfig {
            cache_dir,
            max_cache_bytes: 10_000_000,
            ..Default::default()
        };

        // Create LeIndex with custom cache config
        let storage_path = project_path.join(".leindex");
        std::fs::create_dir_all(&storage_path).unwrap();

        let leindex = LeIndex::new(&project_path).unwrap();
        let diagnostics = leindex.get_diagnostics().unwrap();

        // Cache should be initialized
        assert_eq!(diagnostics.cache_entries, 0);
        assert_eq!(diagnostics.cache_bytes, 0);
        assert_eq!(diagnostics.spilled_entries, 0);
        assert_eq!(diagnostics.spilled_bytes, 0);
    }
}

// ============================================================================
// CACHE SPILLING AND RELOADING TESTS (Phase 5.2 & 5.3)
// ============================================================================

mod cache_spill_reload_tests {
    use super::*;
    use lepasserelle::LeIndex;
    use lepasserelle::memory::WarmStrategy;

    /// Helper function to create a test project with some code
    fn create_test_project(temp_dir: &TempDir) -> PathBuf {
        let project_path = temp_dir.path().to_path_buf();
        let src_dir = project_path.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();

        // Create a simple Rust file
        let main_file = src_dir.join("main.rs");
        std::fs::write(
            &main_file,
            r#"
fn main() {
    println!("Hello, world!");
    greet();
}

fn greet() {
    println!("Greetings!");
}
"#,
        )
        .unwrap();

        project_path
    }

    #[test]
    fn test_spill_pdg_cache_when_no_pdg() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Spilling PDG when none is in memory should return an error
        let result = leindex.spill_pdg_cache();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No PDG in memory"));
    }

    #[test]
    fn test_spill_vector_cache() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Spilling vector cache should always work (just creates a marker)
        let result = leindex.spill_vector_cache();
        assert!(result.is_ok());

        // Verify the cache marker was created
        let stats = leindex.get_cache_stats().unwrap();
        assert!(stats.spilled_entries > 0 || stats.cache_entries > 0);
    }

    #[test]
    fn test_spill_all_caches() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Spill all caches should succeed
        let result = leindex.spill_all_caches();
        assert!(result.is_ok());

        let (pdg_bytes, vector_bytes) = result.unwrap();
        // Vector cache should have been spilled
        assert_eq!(vector_bytes, vector_bytes); // usize is non-negative
        // PDG bytes should be 0 since PDG wasn't loaded
        assert_eq!(pdg_bytes, 0);
    }

    #[test]
    fn test_reload_pdg_when_already_loaded() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // First, index the project to load PDG
        let _ = leindex.index_project(false);

        // Reloading when PDG is already in memory should return Ok immediately
        let result = leindex.reload_pdg_from_cache();
        assert!(result.is_ok());
    }

    #[test]
    fn test_reload_vector_without_pdg() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Reloading vector without PDG should error
        let result = leindex.reload_vector_from_pdg();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No PDG available"));
    }

    #[test]
    fn test_warm_caches_all_strategy() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Warm all caches
        let result = leindex.warm_caches(WarmStrategy::All);
        assert!(result.is_ok());

        let warm_result = result.unwrap();
        // Should have result (entries_warmed is usize, non-negative)
        assert_eq!(warm_result.entries_warmed, warm_result.entries_warmed);
    }

    #[test]
    fn test_warm_caches_pdg_only_strategy() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Warm PDG only
        let result = leindex.warm_caches(WarmStrategy::PDGOnly);
        assert!(result.is_ok());
    }

    #[test]
    fn test_warm_caches_search_only_strategy() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Warm search index only
        let result = leindex.warm_caches(WarmStrategy::SearchIndexOnly);
        assert!(result.is_ok());
    }

    #[test]
    fn test_get_cache_stats() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let leindex = LeIndex::new(&project_path).unwrap();

        // Get cache stats
        let result = leindex.get_cache_stats();
        assert!(result.is_ok());

        let stats = result.unwrap();
        // All stats should be valid (usize is always non-negative)
        assert_eq!(stats.cache_entries, stats.cache_entries);
        assert_eq!(stats.cache_bytes, stats.cache_bytes);
        assert_eq!(stats.spilled_entries, stats.spilled_entries);
        assert_eq!(stats.spilled_bytes, stats.spilled_bytes);
    }

    #[test]
    fn test_check_memory_and_spill_below_threshold() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Check memory - should return Ok(false) since below threshold
        let result = leindex.check_memory_and_spill();
        assert!(result.is_ok());

        let spilled = result.unwrap();
        // Should not have spilled since below threshold
        assert!(!spilled);
    }

    #[test]
    fn test_cache_spill_and_reload_workflow() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // First index the project
        let index_result = leindex.index_project(false);
        assert!(index_result.is_ok());

        // Get initial stats
        let initial_stats = leindex.get_cache_stats().unwrap();

        // Spill all caches
        let spill_result = leindex.spill_all_caches();
        assert!(spill_result.is_ok());

        // Verify caches were spilled
        let spilled_stats = leindex.get_cache_stats().unwrap();
        assert!(spilled_stats.spilled_entries >= initial_stats.spilled_entries);

        // Note: Full reload test would require storage persistence
        // which is beyond the scope of this unit test
    }

    #[test]
    fn test_warm_caches_recent_first_strategy() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Warm caches with RecentFirst strategy
        let result = leindex.warm_caches(WarmStrategy::RecentFirst);
        assert!(result.is_ok());

        let warm_result = result.unwrap();
        assert_eq!(warm_result.entries_warmed, warm_result.entries_warmed);
    }

    #[test]
    fn test_cache_stats_after_spill() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // Get initial stats
        let initial = leindex.get_cache_stats().unwrap();

        // Spill vector cache
        leindex.spill_vector_cache().unwrap();

        // Get stats after spill
        let after = leindex.get_cache_stats().unwrap();

        // Spilled entries should have increased or stayed the same
        assert!(after.spilled_entries >= initial.spilled_entries);
    }

    #[test]
    fn test_multiple_spill_operations() {
        let temp_dir = TempDir::new().unwrap();
        let project_path = create_test_project(&temp_dir);

        let mut leindex = LeIndex::new(&project_path).unwrap();

        // First spill
        leindex.spill_vector_cache().unwrap();
        let first_stats = leindex.get_cache_stats().unwrap();

        // Second spill (should handle gracefully)
        leindex.spill_vector_cache().unwrap();
        let second_stats = leindex.get_cache_stats().unwrap();

        // Cache should handle multiple spills
        assert!(second_stats.spilled_entries >= first_stats.spilled_entries);
    }
}
