use super::helpers::{
    extract_string, validate_file_within_project, wrap_with_meta,
};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::atomic_write_async;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;

/// Handler for leindex_write — atomic file creation/overwrite with immediate PDG surfacing.
#[derive(Clone)]
pub struct WriteHandler;

#[allow(missing_docs)]
impl WriteHandler {
    pub fn name(&self) -> &str {
        "leindex_write"
    }

    pub fn description(&self) -> &str {
        "Create a new file or overwrite an existing one. Returns the file's structural \
context (symbols, types) immediately so the model knows how the new file fits into the PDG."
    }

    pub fn argument_schema(&self) -> Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_path": {
                    "type": "string",
                    "description": "Absolute path to the file to write"
                },
                "content": {
                    "type": "string",
                    "description": "Full content to write to the file"
                },
                "project_path": {
                    "type": "string",
                    "description": "Project directory (auto-indexes on first use; omit to use current project)"
                }
            },
            "required": ["file_path", "content"]
        })
    }

    pub async fn execute(
        &self,
        registry: &Arc<ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        let file_path = extract_string(&args, "file_path")?;
        let content = extract_string(&args, "content")?;
        let project_path = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path).await?;
        let mut guard = handle.write().await;

        // Enforce project boundary
        let project_root = guard.project_path().to_path_buf();
        let requested_path = if PathBuf::from(&file_path).is_absolute() {
            PathBuf::from(&file_path)
        } else {
            project_root.join(&file_path)
        };

        // Normalize path to resolve .. and . components without requiring filesystem access
        let mut abs_path = PathBuf::new();
        for component in requested_path.components() {
            match component {
                std::path::Component::ParentDir => {
                    abs_path.pop();
                }
                std::path::Component::CurDir => {}
                _ => {
                    abs_path.push(component);
                }
            }
        }

        if !abs_path.starts_with(&project_root) {
             return Err(JsonRpcError::invalid_params(format!(
                "File '{}' is outside the project boundary '{}'",
                file_path,
                project_root.display()
            )));
        }

        // Atomic write
        atomic_write_async(abs_path.clone(), content.as_bytes().to_vec())
            .await
            .map_err(|e| {
                JsonRpcError::internal_error(format!(
                    "Failed to write '{}': {}",
                    abs_path.display(),
                    e
                ))
            })?;

        // Surface PDG context for the new file
        let language = crate::parse::grammar::LanguageId::from_extension(
            abs_path.extension().and_then(|e| e.to_str()).unwrap_or("")
        ).map(|id| id.config().name.clone()).unwrap_or_else(|| "unknown".to_string());

        let signatures = if language != "unknown" {
            let parser = crate::parse::parallel::ParallelParser::new();
            // We just parse this one file for immediate surfacing
            let results = parser.parse_files(vec![abs_path.clone()]);
            if let Some(res) = results.first() {
                res.signatures.clone()
            } else {
                vec![]
            }
        } else {
            vec![]
        };

        let response = serde_json::json!({
            "success": true,
            "file_path": abs_path.to_string_lossy(),
            "language": language,
            "symbols": signatures.iter().map(|s| {
                serde_json::json!({
                    "name": s.name,
                    "type": s.return_type,
                    "range": [s.byte_range.0, s.byte_range.1]
                })
            }).collect::<Vec<_>>()
        });

        Ok(wrap_with_meta(response, &guard))
    }
}
