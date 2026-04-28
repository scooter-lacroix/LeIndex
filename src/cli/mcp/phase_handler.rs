use super::helpers::*;
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::phase::{run_phase_analysis, DocsMode, FormatMode, PhaseOptions, PhaseSelection};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

/// Handler for leindex_phase_analysis.
#[derive(Clone)]
pub struct PhaseAnalysisHandler;

impl PhaseAnalysisHandler {
    /// Returns the name of this RPC method.
    pub fn name(&self) -> &str {
        "leindex_phase_analysis"
    }

    /// Returns the description of this RPC method.
    pub fn description(&self) -> &str {
        "Run additive 5-phase analysis with freshness-aware incremental execution. Defaults to all 5 phases when `phase` is omitted."
    }

    /// Returns the JSON schema for the arguments of this RPC method.
    pub fn argument_schema(&self) -> Value {
        phase_analysis_schema()
    }

    /// Executes the RPC method.
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        execute_phase_analysis(registry, args).await
    }
}

/// Optional compatibility alias for phase analysis.
#[derive(Clone)]
pub struct PhaseAnalysisAliasHandler;

impl PhaseAnalysisAliasHandler {
    /// Returns the alias name.
    pub fn name(&self) -> &str {
        "phase_analysis"
    }

    /// Returns description.
    pub fn description(&self) -> &str {
        "Alias for leindex_phase_analysis"
    }

    /// Returns argument schema.
    pub fn argument_schema(&self) -> Value {
        phase_analysis_schema()
    }

    /// Executes the alias method.
    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        execute_phase_analysis(registry, args).await
    }
}

async fn execute_phase_analysis(
    registry: &Arc<ProjectRegistry>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let project_path = args.get("project_path").and_then(|v| v.as_str());
    let handle = registry.get_or_create(project_path).await?;

    let selection = match args.get("phase") {
        None => PhaseSelection::All,
        Some(Value::String(s)) if s.eq_ignore_ascii_case("all") => PhaseSelection::All,
        Some(Value::String(s)) => {
            let parsed = s.parse::<u8>().map_err(|_| {
                JsonRpcError::invalid_params(
                    "phase must be 1..5, \"1\"..\"5\", or 'all'".to_string(),
                )
            })?;
            PhaseSelection::from_number(parsed).ok_or_else(|| {
                JsonRpcError::invalid_params("phase must be in range 1..5".to_string())
            })?
        }
        Some(Value::Number(n)) => {
            let Some(p) = n.as_u64().map(|v| v as u8) else {
                return Err(JsonRpcError::invalid_params(
                    "phase must be 1..5 or 'all'".to_string(),
                ));
            };
            PhaseSelection::from_number(p).ok_or_else(|| {
                JsonRpcError::invalid_params("phase must be in range 1..5".to_string())
            })?
        }
        _ => {
            return Err(JsonRpcError::invalid_params_with_suggestion(
                "Invalid 'phase'".to_string(),
                "Use phase: 1..5, phase: \"1\"..\"5\", or phase: \"all\" (default)".to_string(),
            ));
        }
    };

    let mode = args
        .get("mode")
        .and_then(|v| v.as_str())
        .unwrap_or("balanced");
    let parsed_mode = FormatMode::parse(mode).ok_or_else(|| {
        JsonRpcError::invalid_params("mode must be one of ultra|balanced|verbose".to_string())
    })?;

    let docs_mode_raw = args
        .get("docs_mode")
        .and_then(|v| v.as_str())
        .unwrap_or("off");
    let parsed_docs_mode = DocsMode::parse(docs_mode_raw).ok_or_else(|| {
        JsonRpcError::invalid_params("docs_mode must be one of off|markdown|text|all".to_string())
    })?;

    let include_docs = extract_bool(&args, "include_docs", false);

    let base_project_root = {
        let reader = handle.lock().await;
        reader.project_path().to_path_buf()
    };

    let canonical_target = match args.get("path").and_then(|v| v.as_str()) {
        Some(path) => PathBuf::from(path).canonicalize().map_err(|e| {
            JsonRpcError::invalid_params(format!("path must exist and be accessible: {}", e))
        })?,
        None => base_project_root.clone(),
    };

    // Keep a clone for the C.7 single-file deep-dive enrichment below.
    let single_file_target: Option<PathBuf> = if canonical_target.is_file() {
        Some(canonical_target.clone())
    } else {
        None
    };

    let (root, focus_files) = if canonical_target.is_file() {
        let file_root = canonical_target
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| JsonRpcError::invalid_params("file path has no parent".to_string()))?;
        (file_root, vec![canonical_target.clone()])
    } else {
        (canonical_target, Vec::new())
    };

    let default_max_files = if focus_files.is_empty() { 2000 } else { 1 };
    let mut max_files = extract_usize(&args, "max_files", default_max_files)?;
    if !focus_files.is_empty() {
        max_files = max_files.max(1);
    }

    let max_focus_files = extract_usize(&args, "max_focus_files", 20)?;
    let top_n = extract_usize(&args, "top_n", 10)?;
    let max_output_chars = extract_usize(&args, "max_chars", 12000)?;

    // ── Single-file deep dive (Task C.7) ────────────────────────────────────
    // When `path` is a file, augment the phase report with per-symbol PDG data:
    // signature, line range, complexity, caller_count, cross-file deps.
    let file_symbols_json: Option<Vec<serde_json::Value>> =
        if let Some(ref file_path) = single_file_target {
            // Read the file for byte→line conversion (single file, cheap).
            let file_content = std::fs::read_to_string(file_path).unwrap_or_default();

            // Build line-start offsets for O(log N) byte→line lookups.
            let mut line_starts = vec![0usize];
            for (i, &b) in file_content.as_bytes().iter().enumerate() {
                if b == b'\n' {
                    line_starts.push(i + 1);
                }
            }
            let byte_to_line = |byte: usize| -> usize {
                // Returns 1-indexed line number
                line_starts.partition_point(|&s| s <= byte)
            };

            let file_path_str = file_path.to_string_lossy().to_string();
            let reader = handle.lock().await;
            if let Some(pdg) = reader.pdg() {
                let node_ids = pdg.nodes_in_file(&file_path_str);
                let mut symbols: Vec<serde_json::Value> = node_ids
                    .iter()
                    .filter_map(|&node_idx| {
                        let node = pdg.get_node(node_idx)?;
                        let (start_byte, end_byte) = node.byte_range;
                        let line_start = byte_to_line(start_byte);
                        let line_end = byte_to_line(end_byte.saturating_sub(1));

                        // Signature: first non-empty line at the node's byte offset.
                        let signature: Option<String> = if start_byte < file_content.len() {
                            file_content[start_byte..]
                                .lines()
                                .next()
                                .map(|l| l.trim().to_string())
                                .filter(|l| !l.is_empty() && !l.starts_with("// ["))
                        } else {
                            None
                        };

                        // Cross-file outgoing dependencies (calls/imports to other files).
                        let cross_file_deps: Vec<serde_json::Value> = pdg
                            .neighbors(node_idx)
                            .iter()
                            .filter_map(|&dep_idx| pdg.get_node(dep_idx))
                            .filter(|dep| dep.file_path != node.file_path)
                            .map(|dep| {
                                serde_json::json!({
                                    "name": dep.name,
                                    "file": dep.file_path,
                                    "type": format!("{:?}", dep.node_type).to_lowercase(),
                                })
                            })
                            .collect();

                        let symbol_type = format!("{:?}", node.node_type).to_lowercase();

                        Some(serde_json::json!({
                            "name": node.name,
                            "symbol_type": symbol_type,
                            "signature": signature,
                            "line_start": line_start,
                            "line_end": line_end,
                            "complexity": node.complexity,
                            "caller_count": pdg.predecessor_count(node_idx),
                            "dependency_count": pdg.neighbors(node_idx).len(),
                            "cross_file_deps": cross_file_deps,
                        }))
                    })
                    .collect();

                // Sort by line_start so the LLM reads top-to-bottom.
                symbols.sort_by_key(|s| s["line_start"].as_u64().unwrap_or(0));
                Some(symbols)
            } else {
                // PDG not available (project not yet indexed) — skip enrichment.
                None
            }
        } else {
            None
        };

    let options = PhaseOptions {
        root,
        focus_files,
        mode: parsed_mode,
        max_files,
        max_focus_files,
        top_n,
        max_output_chars,
        use_incremental_refresh: true,
        include_docs,
        docs_mode: parsed_docs_mode,
        hotspot_keywords: PhaseOptions::default().hotspot_keywords,
    };

    let report = tokio::task::spawn_blocking(move || run_phase_analysis(options, selection))
        .await
        .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))?
        .map_err(|e| JsonRpcError::internal_error(format!("Phase analysis failed: {}", e)))?;

    let mut report_value = serde_json::to_value(report)
        .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?;

    // Merge per-symbol data into the response when available.
    if let Some(symbols) = file_symbols_json {
        if let serde_json::Value::Object(ref mut map) = report_value {
            map.insert("file_symbols".to_string(), serde_json::json!(symbols));
        }
    }

    if let serde_json::Value::Object(ref mut map) = report_value {
        map.insert(
            "phase_explanations".to_string(),
            serde_json::json!({
                "1": "File parsing & signature extraction",
                "2": "Import graph construction (internal/external edges)",
                "3": "Entry point identification & impact analysis",
                "4": "Complexity hotspot detection",
                "5": "Actionable recommendations generation"
            }),
        );
        map.insert(
            "example_interpretation".to_string(),
            serde_json::json!({
                "high_unresolved_modules": "Consider adding missing type definitions",
                "many_entry_points": "May indicate architectural coupling issues"
            }),
        );
    }

    let index_for_meta = handle.lock().await;
    Ok(wrap_with_meta(report_value, &index_for_meta))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::mcp::helpers::test_registry_for;
    use tempfile::tempdir;

    #[test]
    fn test_phase_schema_phase_and_path_are_optional() {
        let schema = phase_analysis_schema();
        let required = schema.get("required").and_then(|v| v.as_array());
        assert!(required.is_none() || required.unwrap().is_empty());
    }

    #[test]
    fn test_phase_schema_defaults_phase_to_all() {
        let schema = phase_analysis_schema();
        let phase = schema
            .get("properties")
            .and_then(|v| v.get("phase"))
            .expect("phase schema");
        assert_eq!(
            phase.get("default").and_then(|v| v.as_str()),
            Some("all")
        );
    }

    #[test]
    fn test_phase_schema_mode_options() {
        let schema = phase_analysis_schema();
        let mode = schema
            .get("properties")
            .and_then(|v| v.get("mode"))
            .expect("mode schema");
        let enum_vals = mode.get("enum").and_then(|v| v.as_array()).expect("enum");
        let values: Vec<&str> = enum_vals.iter().filter_map(|v| v.as_str()).collect();
        assert!(values.contains(&"ultra"));
        assert!(values.contains(&"balanced"));
        assert!(values.contains(&"verbose"));
    }

    #[test]
    fn test_phase_schema_docs_mode_options() {
        let schema = phase_analysis_schema();
        let docs_mode = schema
            .get("properties")
            .and_then(|v| v.get("docs_mode"))
            .expect("docs_mode schema");
        let enum_vals = docs_mode
            .get("enum")
            .and_then(|v| v.as_array())
            .expect("enum");
        let values: Vec<&str> = enum_vals.iter().filter_map(|v| v.as_str()).collect();
        assert!(values.contains(&"off"));
        assert!(values.contains(&"markdown"));
        assert!(values.contains(&"text"));
        assert!(values.contains(&"all"));
    }

    #[test]
    fn test_format_mode_parse() {
        assert_eq!(FormatMode::parse("ultra"), Some(FormatMode::Ultra));
        assert_eq!(FormatMode::parse("balanced"), Some(FormatMode::Balanced));
        assert_eq!(FormatMode::parse("verbose"), Some(FormatMode::Verbose));
        assert_eq!(FormatMode::parse("invalid"), None);
    }

    #[test]
    fn test_docs_mode_parse() {
        assert_eq!(DocsMode::parse("off"), Some(DocsMode::Off));
        assert_eq!(DocsMode::parse("markdown"), Some(DocsMode::Markdown));
        assert_eq!(DocsMode::parse("text"), Some(DocsMode::Text));
        assert_eq!(DocsMode::parse("all"), Some(DocsMode::All));
        assert_eq!(DocsMode::parse("invalid"), None);
    }

    #[test]
    fn test_phase_selection_from_number() {
        assert_eq!(
            PhaseSelection::from_number(1),
            Some(PhaseSelection::Single(1))
        );
        assert_eq!(
            PhaseSelection::from_number(5),
            Some(PhaseSelection::Single(5))
        );
        assert_eq!(PhaseSelection::from_number(0), None);
        assert_eq!(PhaseSelection::from_number(6), None);
    }

    #[test]
    fn test_handler_names() {
        let primary = PhaseAnalysisHandler;
        assert_eq!(primary.name(), "leindex_phase_analysis");

        let alias = PhaseAnalysisAliasHandler;
        assert_eq!(alias.name(), "phase_analysis");
    }

    #[tokio::test]
    async fn test_phase_analysis_defaults_to_all_when_phase_missing() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
        std::fs::write(&src, "pub fn ping()->bool{true}\n").expect("write source");

        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({
            "path": src.display().to_string(),
            "mode": "balanced",
            "max_files": 1
        });

        let value = execute_phase_analysis(&registry, args)
            .await
            .expect("phase analysis");
        let phases = value
            .get("executed_phases")
            .and_then(|v| v.as_array())
            .expect("executed phases");

        let as_u8 = phases
            .iter()
            .filter_map(|v| v.as_u64())
            .map(|v| v as u8)
            .collect::<Vec<_>>();
        assert_eq!(as_u8, vec![1, 2, 3, 4, 5]);
    }

    #[tokio::test]
    async fn test_phase_analysis_accepts_string_phase_number() {
        let dir = tempdir().expect("tempdir");
        let src = dir.path().join("src/lib.rs");
        std::fs::create_dir_all(src.parent().expect("parent")).expect("mkdir");
        std::fs::write(&src, "pub fn ping()->bool{true}\n").expect("write source");

        let registry = test_registry_for(dir.path());
        let args = serde_json::json!({
            "path": src.display().to_string(),
            "phase": "1",
            "mode": "balanced",
            "max_files": 1
        });

        let value = execute_phase_analysis(&registry, args)
            .await
            .expect("phase analysis");
        let phases = value
            .get("executed_phases")
            .and_then(|v| v.as_array())
            .expect("executed phases");

        let as_u8 = phases
            .iter()
            .filter_map(|v| v.as_u64())
            .map(|v| v as u8)
            .collect::<Vec<_>>();
        assert_eq!(as_u8, vec![1]);
    }

    #[test]
    fn test_phase_c_handler_schemas() {
        // All Phase C schemas should be valid JSON objects with required fields
        use super::super::file_summary_handler::FileSummaryHandler;
        use super::super::project_map_handler::ProjectMapHandler;
        use super::super::grep_symbols_handler::GrepSymbolsHandler;
        use super::super::read_symbol_handler::ReadSymbolHandler;
        use super::super::symbol_lookup_handler::SymbolLookupHandler;

        let schemas = vec![
            (FileSummaryHandler.argument_schema(), vec!["file_path"]),
            // SymbolLookupHandler has no required fields (symbol or symbols accepted)
            (SymbolLookupHandler.argument_schema(), vec![]),
            (ProjectMapHandler.argument_schema(), vec![]),
            (GrepSymbolsHandler.argument_schema(), vec!["pattern"]),
            (ReadSymbolHandler.argument_schema(), vec!["symbol"]),
        ];

        for (schema, required_fields) in schemas {
            assert!(schema.is_object(), "schema must be a JSON object");
            for field in required_fields {
                let required = schema
                    .get("required")
                    .and_then(|v| v.as_array())
                    .expect("required array");
                assert!(
                    required.iter().any(|v| v.as_str() == Some(field)),
                    "field '{}' must be in required list",
                    field
                );
            }
        }
    }
}
