use super::*;

#[test]
fn test_cli_parsing() {
    let cli = Cli::try_parse_from(["leindex", "index", "/path/to/project"]).unwrap();
    assert!(matches!(cli.command, Some(Commands::Index { .. })));
}

#[test]
fn test_mcp_command_parsing() {
    let cli = Cli::try_parse_from(["leindex", "mcp"]).unwrap();
    assert!(matches!(cli.command, Some(Commands::Mcp { .. })));
}

#[test]
fn test_stdio_flag_parsing() {
    let cli = Cli::try_parse_from(["leindex", "--stdio"]).unwrap();
    assert!(cli.stdio);
}

#[test]
fn test_search_command() {
    let cli = Cli::try_parse_from(["leindex", "search", "test query"]).unwrap();
    match cli.command {
        Some(Commands::Search { query, top_k, .. }) => {
            assert_eq!(query, "test query");
            assert_eq!(top_k, 10);
        }
        _ => panic!("Expected Search command"),
    }
}

#[test]
fn test_phase_command_parsing() {
    let cli = Cli::try_parse_from(["leindex", "phase", "--phase", "2", "--mode", "ultra"]).unwrap();
    match cli.command {
        Some(Commands::Phase {
            phase, all, mode, ..
        }) => {
            assert_eq!(phase, Some(2));
            assert!(!all);
            assert_eq!(mode, "ultra");
        }
        _ => panic!("Expected Phase command"),
    }
}

#[test]
fn test_dashboard_command_parsing() {
    let cli = Cli::try_parse_from(["leindex", "dashboard"]).unwrap();
    match cli.command {
        Some(Commands::Dashboard { port, prod }) => {
            assert_eq!(port, 5173);
            assert!(!prod);
        }
        _ => panic!("Expected Dashboard command"),
    }
}

#[test]
fn test_dashboard_command_with_port() {
    let cli = Cli::try_parse_from(["leindex", "dashboard", "--port", "3000"]).unwrap();
    match cli.command {
        Some(Commands::Dashboard { port, prod }) => {
            assert_eq!(port, 3000);
            assert!(!prod);
        }
        _ => panic!("Expected Dashboard command"),
    }
}

#[test]
fn test_dashboard_command_prod() {
    let cli = Cli::try_parse_from(["leindex", "dashboard", "--prod"]).unwrap();
    match cli.command {
        Some(Commands::Dashboard { port, prod }) => {
            assert_eq!(port, 5173);
            assert!(prod);
        }
        _ => panic!("Expected Dashboard command"),
    }
}

#[test]
fn test_tools_help_command_parsing() {
    let cli = Cli::try_parse_from(["leindex", "tools", "help", "project_map"]).unwrap();
    match cli.command {
        Some(Commands::Tools {
            command: ToolCommands::Inspect { name },
        }) => assert_eq!(name, "project_map"),
        _ => panic!("Expected tools help command"),
    }
}

#[test]
fn test_tools_run_command_parsing() {
    let cli = Cli::try_parse_from([
        "leindex",
        "tools",
        "run",
        "project_map",
        "--args",
        "{\"depth\":1}",
        "--set",
        "include_symbols=true",
    ])
    .unwrap();

    match cli.command {
        Some(Commands::Tools {
            command:
                ToolCommands::Run {
                    name,
                    args_json,
                    set,
                },
        }) => {
            assert_eq!(name, "project_map");
            assert_eq!(args_json, "{\"depth\":1}");
            assert_eq!(set, vec!["include_symbols=true"]);
        }
        _ => panic!("Expected tools run command"),
    }
}

#[test]
fn test_find_tool_handler_accepts_short_and_full_names() {
    assert!(find_tool_handler("LeIndex [Project Map]").is_some());
    assert!(find_tool_handler("project_map").is_some());
    assert!(find_tool_handler("project-map").is_some());
}

#[test]
fn test_cleanup_command_parsing() {
    let cli = Cli::try_parse_from(["leindex", "cleanup"]).unwrap();
    match cli.command {
        Some(Commands::Cleanup {
            max_age_days,
            dry_run,
            stale_daemons,
            store,
        }) => {
            assert_eq!(max_age_days, 7);
            assert!(!dry_run);
            assert!(!stale_daemons);
            assert!(!store);
        }
        _ => panic!("Expected Cleanup command"),
    }
}

#[test]
fn test_cleanup_command_with_flags() {
    let cli = Cli::try_parse_from([
        "leindex",
        "cleanup",
        "--max-age-days",
        "14",
        "--dry-run",
        "--stale-daemons",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Cleanup {
            max_age_days,
            dry_run,
            stale_daemons,
            store,
        }) => {
            assert_eq!(max_age_days, 14);
            assert!(dry_run);
            assert!(stale_daemons);
            assert!(!store);
        }
        _ => panic!("Expected Cleanup command"),
    }
}

#[test]
fn test_memory_report_flag_parsing() {
    // VAL-MEASURE-020: --memory-report is an opt-in CLI surface
    let cli = Cli::try_parse_from([
        "leindex",
        "--memory-report",
        "/tmp/mem-report.json",
        "index",
        "/tmp/project",
    ])
    .unwrap();
    assert_eq!(
        cli.memory_report,
        Some(PathBuf::from("/tmp/mem-report.json"))
    );
    assert!(matches!(cli.command, Some(Commands::Index { .. })));
}

#[test]
fn test_memory_report_flag_absent_by_default() {
    let cli = Cli::try_parse_from(["leindex", "index", "/tmp/project"]).unwrap();
    assert!(cli.memory_report.is_none());
}

#[test]
fn test_retention_command_parsing() {
    // WS4 Task 9: `leindex retention --report` is registered.
    let cli = Cli::try_parse_from(["leindex", "retention", "--report"]).unwrap();
    match cli.command {
        Some(Commands::Retention { report, .. }) => {
            assert!(report, "--report must be parsed");
        }
        _ => panic!("Expected Retention command"),
    }
}

#[test]
fn test_retention_command_without_report_flag() {
    let cli = Cli::try_parse_from(["leindex", "retention"]).unwrap();
    match cli.command {
        Some(Commands::Retention { report, .. }) => {
            assert!(!report, "report flag defaults to false");
        }
        _ => panic!("Expected Retention command"),
    }
}

#[test]
fn test_retention_gc_command_parsing() {
    let cli =
        Cli::try_parse_from(["leindex", "retention", "--gc", "--max-generations", "3"]).unwrap();
    match cli.command {
        Some(Commands::Retention {
            gc,
            max_generations,
            dry_run,
            ..
        }) => {
            assert!(gc, "--gc must be parsed");
            assert_eq!(max_generations, 3, "--max-generations must be parsed");
            assert!(!dry_run, "dry_run defaults to false");
        }
        _ => panic!("Expected Retention command"),
    }
}

#[test]
fn test_setup_command_parsing() {
    // VAL-SETUP-001: setup command is registered
    let cli = Cli::try_parse_from(["leindex", "setup", "--neural", "--cpu"]).unwrap();
    match cli.command {
        Some(Commands::Setup {
            neural,
            no_neural,
            cpu,
            gpu,
            check,
            warmup: _,
        }) => {
            assert!(neural);
            assert!(!no_neural);
            assert!(cpu);
            assert!(gpu.is_none());
            assert!(!check);
        }
        _ => panic!("Expected Setup command"),
    }
}

#[test]
fn test_setup_command_gpu_amd() {
    let cli = Cli::try_parse_from(["leindex", "setup", "--neural", "--gpu", "amd"]).unwrap();
    match cli.command {
        Some(Commands::Setup {
            neural, cpu, gpu, ..
        }) => {
            assert!(neural);
            assert!(!cpu);
            assert_eq!(gpu.as_deref(), Some("amd"));
        }
        _ => panic!("Expected Setup command"),
    }
}

#[test]
fn test_setup_command_gpu_nvidia() {
    let cli = Cli::try_parse_from(["leindex", "setup", "--neural", "--gpu", "nvidia"]).unwrap();
    match cli.command {
        Some(Commands::Setup {
            neural, cpu, gpu, ..
        }) => {
            assert!(neural);
            assert!(!cpu);
            assert_eq!(gpu.as_deref(), Some("nvidia"));
        }
        _ => panic!("Expected Setup command"),
    }
}

#[test]
fn test_setup_command_no_neural() {
    // VAL-SETUP-013: --no-neural flag
    let cli = Cli::try_parse_from(["leindex", "setup", "--no-neural"]).unwrap();
    match cli.command {
        Some(Commands::Setup {
            neural,
            no_neural,
            cpu,
            gpu,
            check,
            warmup: _,
        }) => {
            assert!(!neural);
            assert!(no_neural);
            assert!(!cpu);
            assert!(gpu.is_none());
            assert!(!check);
        }
        _ => panic!("Expected Setup command"),
    }
}

#[test]
fn test_setup_command_check() {
    // VAL-SETUP-014: --check flag
    let cli = Cli::try_parse_from(["leindex", "setup", "--check"]).unwrap();
    match cli.command {
        Some(Commands::Setup { check, .. }) => assert!(check),
        _ => panic!("Expected Setup command"),
    }
}

#[test]
fn test_setup_command_neural_gpu_conflict_rejected() {
    // VAL-SETUP-015: conflicting flags produce error
    let result = Cli::try_parse_from(["leindex", "setup", "--neural", "--cpu", "--gpu", "amd"]);
    assert!(result.is_err());
}

#[test]
fn test_setup_command_neural_no_neural_conflict_rejected() {
    // VAL-SETUP-015: --neural + --no-neural is a conflict
    let result = Cli::try_parse_from(["leindex", "setup", "--neural", "--no-neural"]);
    assert!(result.is_err());
}

#[test]
fn test_setup_help_is_valid() {
    // VAL-SETUP-001: leindex setup --help exits 0
    let result = Cli::try_parse_from(["leindex", "setup", "--help"]);
    assert!(result.is_err()); // clap exits with error for --help
    let err = result.unwrap_err();
    assert!(matches!(err.kind(), ErrorKind::DisplayHelp));
}

// -----------------------------------------------------------------------
// format_analysis_output (VAL-OUT-001..006)
// -----------------------------------------------------------------------

fn mock_search_result(
    rank: usize,
    file_path: &str,
    symbol: &str,
    score: f32,
) -> crate::search::SearchResult {
    crate::search::SearchResult {
        rank,
        node_id: format!("node-{rank}"),
        file_path: file_path.to_string(),
        symbol_name: symbol.to_string(),
        symbol_type: Some("function".to_string()),
        signature: None,
        complexity: 1,
        caller_count: None,
        dependency_count: None,
        language: "rust".to_string(),
        score: crate::search::Score {
            overall: score,
            tfidf: 0.0,
            neural: 0.0,
            structural: 0.0,
            text_match: 0.0,
            fragment: 0.0,
        },
        context: None,
        byte_range: (0, 0),
        fragment_byte_range: None,
        line_number: Some(10 + rank),
    }
}

fn mock_analysis_result(
    results: Vec<crate::search::SearchResult>,
) -> crate::cli::leindex::AnalysisResult {
    crate::cli::leindex::AnalysisResult {
        query: "test query".to_string(),
        results,
        // > 300 chars so the context section exercises the expanded budget.
        context: Some("context line\n".repeat(60)),
        tokens_used: 500,
        processing_time_ms: 12,
    }
}

#[test]
fn test_analysis_output_shows_each_file_path() {
    // VAL-OUT-001: every result's file_path appears as readable text.
    let result = mock_analysis_result(vec![
        mock_search_result(1, "src/main.rs", "main", 0.950),
        mock_search_result(2, "src/lib.rs", "helper", 0.800),
        mock_search_result(3, "src/util.rs", "parse", 0.600),
    ]);
    let out = format_analysis_output("test query", &result);
    for path in ["src/main.rs", "src/lib.rs", "src/util.rs"] {
        assert!(out.contains(path), "output must contain file path {}", path);
    }
}

#[test]
fn test_analysis_output_shows_symbol_names() {
    // VAL-OUT-002: every result's symbol_name appears as readable text.
    let result = mock_analysis_result(vec![
        mock_search_result(1, "src/main.rs", "main", 0.950),
        mock_search_result(2, "src/lib.rs", "helper", 0.800),
        mock_search_result(3, "src/util.rs", "parse", 0.600),
    ]);
    let out = format_analysis_output("test query", &result);
    for symbol in ["main", "helper", "parse"] {
        assert!(
            out.contains(symbol),
            "output must contain symbol {}",
            symbol
        );
    }
}

#[test]
fn test_analysis_output_shows_numeric_scores() {
    // VAL-OUT-003: score.overall rendered as a numeric value per entry.
    let result = mock_analysis_result(vec![
        mock_search_result(1, "src/main.rs", "main", 0.950),
        mock_search_result(2, "src/lib.rs", "helper", 0.803),
    ]);
    let out = format_analysis_output("test query", &result);
    assert!(out.contains("score: 0.950"), "output must show score 0.950");
    assert!(out.contains("score: 0.803"), "output must show score 0.803");
}

#[test]
fn test_analysis_context_budget_at_least_1000() {
    // VAL-OUT-004: the budget constant is >= 1000 (enforced by the
    // `const _: () = assert!(...)` at the definition). Behaviorally, a
    // context longer than 1000 chars must be displayed in full rather than
    // truncated at the old 300-char cap.
    let body = "context line\n".repeat(120); // ~1560 chars, well over 1000
    let result = crate::cli::leindex::AnalysisResult {
        query: "test query".to_string(),
        results: vec![],
        context: Some(body),
        tokens_used: 500,
        processing_time_ms: 12,
    };
    let out = format_analysis_output("test query", &result);
    assert_eq!(
        out.matches("context line").count(),
        120,
        "context longer than 1000 chars must be displayed without truncation"
    );
}

#[test]
fn test_analysis_results_appear_before_context() {
    // VAL-OUT-005: result entries precede the Context section.
    let result = mock_analysis_result(vec![mock_search_result(1, "src/main.rs", "main", 0.950)]);
    let out = format_analysis_output("test query", &result);
    let results_pos = out.find("src/main.rs").expect("file path present");
    let context_pos = out.find("Context:").expect("context header present");
    assert!(
        results_pos < context_pos,
        "results must appear before the Context section"
    );
}

#[test]
fn test_analysis_output_not_raw_json() {
    // VAL-OUT-006: structured, human-readable output — never raw JSON.
    let result = mock_analysis_result(vec![mock_search_result(1, "src/main.rs", "main", 0.950)]);
    let out = format_analysis_output("test query", &result);
    assert!(!out.starts_with('{'), "output must not be raw JSON");
    assert!(
        !out.contains("\"results\""),
        "output must not contain a JSON results key"
    );
    assert!(
        !out.contains("\"rank\":"),
        "output must not contain JSON rank keys"
    );
}

#[test]
fn test_analysis_output_empty_results_message() {
    let result = mock_analysis_result(vec![]);
    let out = format_analysis_output("test query", &result);
    assert!(
        out.contains("No results found"),
        "empty results must print 'No results found'"
    );
}
