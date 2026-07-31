use super::helpers::{extract_bool, extract_usize, phase_analysis_schema, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::phase::{DocsMode, FormatMode, PhaseOptions, PhaseSelection, run_phase_analysis};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Handler for LeIndex [phase_analysis.
#[derive(Clone)]
pub struct PhaseAnalysisHandler;

impl PhaseAnalysisHandler {
    /// Returns the name of this MCP tool (MCP-compliant: leindex.phase-analysis)
    pub fn name(&self) -> &str {
        "leindex.phase-analysis"
    }

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "LeIndex [Phase Analysis]"
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

    /// Returns the human-readable display title for this tool
    pub fn title(&self) -> &str {
        "Phase Analysis"
    }

    /// Returns description.
    pub fn description(&self) -> &str {
        "Alias for LeIndex [Phase Analysis]"
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

fn parse_phase_selection(value: Option<&Value>) -> Result<PhaseSelection, JsonRpcError> {
    match value {
        None => Ok(PhaseSelection::All),
        Some(Value::String(value)) if value.eq_ignore_ascii_case("all") => Ok(PhaseSelection::All),
        Some(Value::String(value)) => {
            let parsed = value.parse::<u8>().map_err(|_| {
                JsonRpcError::invalid_params(
                    "phase must be 1..5, \"1\"..\"5\", or 'all'".to_string(),
                )
            })?;
            PhaseSelection::from_number(parsed).ok_or_else(|| {
                JsonRpcError::invalid_params("phase must be in range 1..5".to_string())
            })
        }
        Some(Value::Number(number)) => {
            let Some(phase) = number.as_u64().and_then(|value| u8::try_from(value).ok()) else {
                return Err(JsonRpcError::invalid_params(
                    "phase must be 1..5 or 'all'".to_string(),
                ));
            };
            PhaseSelection::from_number(phase).ok_or_else(|| {
                JsonRpcError::invalid_params("phase must be in range 1..5".to_string())
            })
        }
        _ => Err(JsonRpcError::invalid_params_with_suggestion(
            "Invalid 'phase'".to_string(),
            "Use phase: 1..5, phase: \"1\"..\"5\", or phase: \"all\" (default)".to_string(),
        )),
    }
}

fn phase_modes(args: &Value) -> Result<(FormatMode, DocsMode, bool), JsonRpcError> {
    let mode = args
        .get("mode")
        .and_then(Value::as_str)
        .unwrap_or("balanced");
    let mode = FormatMode::parse(mode).ok_or_else(|| {
        JsonRpcError::invalid_params("mode must be one of ultra|balanced|verbose".to_string())
    })?;
    let docs_mode = args
        .get("docs_mode")
        .and_then(Value::as_str)
        .unwrap_or("off");
    let docs_mode = DocsMode::parse(docs_mode).ok_or_else(|| {
        JsonRpcError::invalid_params("docs_mode must be one of off|markdown|text|all".to_string())
    })?;
    Ok((mode, docs_mode, extract_bool(args, "include_docs", false)))
}

fn phase_max_files(args: &Value, has_focus: bool) -> Result<usize, JsonRpcError> {
    let max_files = extract_usize(args, "max_files", if has_focus { 1 } else { 2000 })?;
    Ok(if has_focus {
        max_files.max(1)
    } else {
        max_files
    })
}

fn phase_target(
    project_root: &Path,
    requested_path: Option<&str>,
) -> Result<(PathBuf, Vec<PathBuf>, Option<PathBuf>), JsonRpcError> {
    let canonical_root = project_root.canonicalize().map_err(|error| {
        JsonRpcError::invalid_params(format!("project root not accessible: {}", error))
    })?;
    let target = match requested_path {
        Some(path) => {
            // Relative paths resolve against the project root (not the CWD) before
            // canonicalization, then the containment check below rejects escapes.
            let base = if std::path::Path::new(path).is_absolute() {
                PathBuf::from(path)
            } else {
                canonical_root.join(path)
            };
            base.canonicalize().map_err(|error| {
                JsonRpcError::invalid_params(format!(
                    "path must exist and be accessible: {}",
                    error
                ))
            })?
        }
        None => canonical_root.clone(),
    };
    if !target.starts_with(&canonical_root) {
        return Err(JsonRpcError::invalid_params(format!(
            "phase target is outside the project boundary '{}'",
            canonical_root.display()
        )));
    }
    if target.is_file() {
        let root = target
            .parent()
            .map(PathBuf::from)
            .ok_or_else(|| JsonRpcError::invalid_params("file path has no parent".to_string()))?;
        Ok((root, vec![target.clone()], Some(target)))
    } else {
        Ok((target, Vec::new(), None))
    }
}

fn file_symbols(
    pdg: &crate::graph::pdg::ProgramDependenceGraph,
    file_path: &Path,
    content: &str,
) -> Vec<Value> {
    let mut line_starts = vec![0usize];
    for (offset, &byte) in content.as_bytes().iter().enumerate() {
        if byte == b'\n' {
            line_starts.push(offset + 1);
        }
    }
    let byte_to_line = |byte: usize| line_starts.partition_point(|&start| start <= byte);
    let file_path = file_path.to_string_lossy();
    let mut symbols: Vec<Value> = pdg
        .nodes_in_file(&file_path)
        .iter()
        .filter_map(|&node_idx| {
            let node = pdg.get_node(node_idx)?;
            let (start_byte, end_byte) = node.byte_range;
            let signature = content
                .get(start_byte..)
                .and_then(|rest| rest.lines().next())
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with("// ["))
                .map(str::to_owned);
            let cross_file_deps: Vec<Value> = pdg
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
            Some(serde_json::json!({
                "name": node.name,
                "symbol_type": format!("{:?}", node.node_type).to_lowercase(),
                "signature": signature,
                "line_start": byte_to_line(start_byte),
                "line_end": byte_to_line(end_byte.saturating_sub(1)),
                "complexity": node.complexity,
                "caller_count": pdg.predecessor_count(node_idx),
                "dependency_count": pdg.neighbors(node_idx).len(),
                "cross_file_deps": cross_file_deps,
            }))
        })
        .collect();
    symbols.sort_by_key(|symbol| symbol["line_start"].as_u64().unwrap_or(0));
    symbols
}

fn enrich_report(mut report: Value, symbols: Option<Vec<Value>>) -> Value {
    let Value::Object(ref mut map) = report else {
        return report;
    };
    if let Some(symbols) = symbols {
        map.insert("file_symbols".to_string(), serde_json::json!(symbols));
    }
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
    report
}

struct PhaseRequest {
    selection: PhaseSelection,
    options: PhaseOptions,
    single_file_target: Option<PathBuf>,
}

fn phase_request(args: &Value, project_root: &Path) -> Result<PhaseRequest, JsonRpcError> {
    let selection = parse_phase_selection(args.get("phase"))?;
    let (mode, docs_mode, include_docs) = phase_modes(args)?;
    let (root, focus_files, single_file_target) =
        phase_target(project_root, args.get("path").and_then(Value::as_str))?;
    let max_files = phase_max_files(args, !focus_files.is_empty())?;
    let options = PhaseOptions {
        root,
        focus_files,
        mode,
        max_files,
        max_focus_files: extract_usize(args, "max_focus_files", 20)?,
        top_n: extract_usize(args, "top_n", 10)?,
        max_output_chars: extract_usize(args, "max_chars", 12000)?,
        use_incremental_refresh: true,
        include_docs,
        docs_mode,
        hotspot_keywords: PhaseOptions::default().hotspot_keywords,
    };
    Ok(PhaseRequest {
        selection,
        options,
        single_file_target,
    })
}

async fn execute_phase_analysis(
    registry: &Arc<ProjectRegistry>,
    args: Value,
) -> Result<Value, JsonRpcError> {
    let project_path = args.get("project_path").and_then(Value::as_str);
    let handle = registry.get_or_create(project_path).await?;
    let base_project_root = handle.read().await.project_path().to_path_buf();
    let request = phase_request(&args, &base_project_root)?;

    let file_symbols_json = if let Some(file_path) = request.single_file_target.as_deref() {
        let file_path = file_path.to_path_buf();
        let handle = handle.clone();
        tokio::task::spawn_blocking(move || {
            let content = std::fs::read_to_string(&file_path).unwrap_or_default();
            let reader = handle.blocking_read();
            reader
                .pdg()
                .map(|pdg| file_symbols(pdg, &file_path, &content))
        })
        .await
        .map_err(|error| {
            JsonRpcError::internal_error(format!("single-file phase enrichment failed: {error}"))
        })?
    } else {
        None
    };

    let report =
        tokio::task::spawn_blocking(move || run_phase_analysis(request.options, request.selection))
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("Task join error: {}", e)))?
            .map_err(|e| JsonRpcError::internal_error(format!("Phase analysis failed: {}", e)))?;

    let report_value = enrich_report(
        serde_json::to_value(report)
            .map_err(|e| JsonRpcError::internal_error(format!("Serialization error: {}", e)))?,
        file_symbols_json,
    );
    let index_for_meta = handle.read().await;
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
        assert_eq!(phase.get("default").and_then(|v| v.as_str()), Some("all"));
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
        assert_eq!(primary.name(), "leindex.phase-analysis");
        assert_eq!(primary.title(), "LeIndex [Phase Analysis]");

        let alias = PhaseAnalysisAliasHandler;
        assert_eq!(alias.name(), "phase_analysis");
        assert_eq!(alias.title(), "Phase Analysis");
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
        use super::super::grep_symbols_handler::GrepSymbolsHandler;
        use super::super::project_map_handler::ProjectMapHandler;
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
