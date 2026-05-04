use super::helpers::{extract_string, wrap_with_meta};
use super::protocol::JsonRpcError;
use crate::cli::registry::ProjectRegistry;
use crate::edit::atomic_write_async;
use crate::parse::parallel::ParallelParser;
use once_cell::sync::Lazy;
use serde_json::Value;
use std::sync::Arc;

static GLOBAL_PARSER: Lazy<ParallelParser> = Lazy::new(|| ParallelParser::new().without_stats());

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
                    "description": "Absolute or project-relative path. Relative paths resolve against the project root."
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
        let project_path_arg = args.get("project_path").and_then(|v| v.as_str());

        let handle = registry.get_or_create(project_path_arg).await?;

        let abs_path = {
            let guard = handle.read().await;
            let project_root = guard.project_path();

            let path = std::path::Path::new(&file_path);
            let resolved = if path.is_relative() {
                project_root.join(path)
            } else {
                path.to_path_buf()
            };

            let canonical = if resolved.exists() {
                resolved.canonicalize().map_err(|e| {
                    JsonRpcError::invalid_params(format!(
                        "Cannot resolve file path '{}': {}",
                        file_path, e
                    ))
                })?
            } else {
                let parent = resolved.parent().ok_or_else(|| {
                    JsonRpcError::invalid_params(format!(
                        "Invalid file path '{}': no parent directory",
                        file_path
                    ))
                })?;
                let canonical_parent = parent.canonicalize().map_err(|e| {
                    JsonRpcError::invalid_params(format!(
                        "Cannot resolve parent directory of '{}': {}",
                        file_path, e
                    ))
                })?;
                canonical_parent.join(resolved.file_name().ok_or_else(|| {
                    JsonRpcError::invalid_params(format!(
                        "Invalid file path '{}': no file name",
                        file_path
                    ))
                })?)
            };

            if !canonical.starts_with(project_root) {
                return Err(JsonRpcError::invalid_params(format!(
                    "File '{}' is outside the project boundary '{}'",
                    file_path,
                    project_root.display()
                )));
            }
            canonical
        };
        // Registry lock dropped here

        // Atomic write (perform IO without holding registry lock)
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
            abs_path.extension().and_then(|e| e.to_str()).unwrap_or(""),
        )
        .map(|id| id.config().name.clone())
        .unwrap_or_else(|| "unknown".to_string());

        let signatures = if language != "unknown" {
            let abs_path_for_spawn = abs_path.clone();
            // We just parse this one file for immediate surfacing.
            // Wrap in spawn_blocking to avoid blocking the async executor.
            let results = tokio::task::spawn_blocking(move || {
                GLOBAL_PARSER.parse_files(vec![abs_path_for_spawn])
            })
            .await
            .map_err(|e| {
                JsonRpcError::internal_error(format!("Parser task panicked: {}", e))
            })?;

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

        // Re-acquire READ lock for response wrapping
        let read_guard = handle.read().await;
        Ok(wrap_with_meta(response, &read_guard))
    }
}
