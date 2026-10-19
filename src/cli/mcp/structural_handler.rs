//! Handler for LeIndex \[Structural\] — AST-pattern search with ast-grep.
//!
//! The pattern is *code*, not text: `fn $NAME($$$ARGS) { $$$BODY }` matches
//! every Rust function regardless of formatting or comments. It is the only
//! search mode that answers "where is this shape used" instead of "where does
//! this string appear".
//!
//! ## Gating
//!
//! Two independent gates, and both must be open:
//! * the `structural-search` Cargo feature must be compiled in, and
//! * [`FeatureFlag::StructuralSearch`] must be enabled
//!   (`LEINDEX_FEATURE_STRUCTURAL_SEARCH=1`), per AGENTS.md progressive
//!   rollout — new capabilities default off.
//!
//! Both failures are hard errors with an actionable message. A structural
//! search that silently returned "no matches" because it never ran would be
//! worse than an error: the caller would conclude the code does not exist.

// Used only by `search`, which is feature-gated: in a build without the
// capability this handler still advertises its schema (so callers discover the
// tool and get the gate error naming the feature) but compiles no search code.
#[cfg(feature = "structural-search")]
use super::helpers::extract_usize;
use super::protocol::JsonRpcError;
use crate::feature_flags::FeatureFlag;
use serde_json::{Value, json};

/// Hits returned per call when the caller does not say.
const DEFAULT_LIMIT: usize = 100;
/// Matches reported per file when the caller does not say.
const DEFAULT_PER_FILE: usize = 20;
/// Wall-clock budget for one call.
const DEFAULT_TIMEOUT_MS: usize = 20_000;
/// Hard ceiling on a page, mirroring `leindex_find`: an unbounded limit lets
/// one request buffer an arbitrary number of hits in a shared daemon. Larger
/// values are clamped, not rejected.
const MAX_LIMIT: usize = 10_000;
/// Longest matched-text snippet kept per hit.
#[cfg(feature = "structural-search")]
const MAX_MATCH_CHARS: usize = 400;

/// Handler for LeIndex \[Structural\].
#[derive(Clone)]
pub struct StructuralHandler;

#[allow(missing_docs)]
impl StructuralHandler {
    pub fn name(&self) -> &str {
        "leindex_structural"
    }

    pub fn title(&self) -> &str {
        "LeIndex [Structural]"
    }

    pub fn description(&self) -> &str {
        "Search by code shape with an ast-grep pattern, e.g. `fn $NAME($$$ARGS) { $$$BODY }`. Matches the AST, so whitespace, comments and reformatting do not matter. Requires the structural-search build feature and LEINDEX_FEATURE_STRUCTURAL_SEARCH=1."
    }

    pub fn argument_schema(&self) -> Value {
        // Descriptions are deliberately terse. `grouped::compact_property`
        // briefs them to 80 chars and folds them into the `leindex_explore`
        // router schema, which has a byte ceiling in
        // `grouped::tests::test_benchmark_tools_list_sizes`. Keys this branch
        // shares with `leindex_find` are worded exactly as it words them, so
        // adding the branch costs only the new `lang` key rather than inflating
        // keys the two already had in common.
        json!({
            "type": "object",
            "properties": {
                "pattern": { "type": "string", "description": "ast-grep pattern; $NAME one node, $$$ARGS a list" },
                "lang": { "type": "string", "description": "Language name or extension (rust, rs, py, go); omit to infer" },
                "scope": { "type": "string", "description": "Restrict to a project-relative directory or file" },
                "include_globs": { "type": "array", "items": { "type": "string" }, "description": "Only these, e.g. [\"*.rs\"]" },
                "exclude_globs": { "type": "array", "items": { "type": "string" }, "description": "Skip these, e.g. [\"vendor/\"]" },
                "limit": { "type": "integer", "default": DEFAULT_LIMIT, "minimum": 0, "maximum": MAX_LIMIT, "description": "Matches per page; 0 = ceiling (10000)" },
                "offset": { "type": "integer", "default": 0, "minimum": 0, "maximum": MAX_LIMIT, "description": "Matches to skip" },
                "per_file_cap": { "type": "integer", "default": DEFAULT_PER_FILE, "minimum": 0, "description": "Shown per file; 0 = no cap" },
                "timeout_ms": { "type": "integer", "default": DEFAULT_TIMEOUT_MS, "minimum": 0, "description": "Time budget; partial results + has_more. 0 = none" },
                "project_path": { "type": "string", "description": "Project directory; omit to use the current project" }
            },
            "required": ["pattern"]
        })
    }

    pub async fn execute(
        &self,
        _registry: &std::sync::Arc<crate::cli::registry::ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        run(&args).await
    }
}

/// Why the capability is unavailable, or `None` when it is usable.
///
/// Split out from `run` so tests can assert both gate messages without needing
/// a project on disk.
pub(crate) fn unavailable_reason() -> Option<String> {
    if !cfg!(feature = "structural-search") {
        return Some(
            "structural search is not compiled into this build: rebuild with \
             --features structural-search (or --features full)"
                .to_string(),
        );
    }
    if !FeatureFlag::StructuralSearch.is_enabled() {
        return Some(
            "structural search is disabled by feature flag: set \
             LEINDEX_FEATURE_STRUCTURAL_SEARCH=1 (it defaults off under the \
             progressive-rollout policy)"
                .to_string(),
        );
    }
    None
}

/// The gate error. Echoes the pattern back and states plainly that nothing was
/// scanned, so a caller cannot mistake it for an empty result set.
fn gate_error(reason: &str, args: &Value) -> JsonRpcError {
    let pattern = args
        .get("pattern")
        .and_then(Value::as_str)
        .unwrap_or("<pattern>");
    JsonRpcError::invalid_params_with_suggestion(
        format!("Structural search is unavailable: {reason}"),
        format!(
            "Pattern {pattern:?} was NOT searched and no file was scanned. Enable the \
             capability and retry, or use mode: find for a text match."
        ),
    )
}

async fn run(args: &Value) -> Result<Value, JsonRpcError> {
    if let Some(reason) = unavailable_reason() {
        return Err(gate_error(&reason, args));
    }
    #[cfg(feature = "structural-search")]
    {
        search(args).await
    }
    // `unavailable_reason` above is the single source of truth for this gate;
    // reaching here without the feature would mean the two disagreed.
    #[cfg(not(feature = "structural-search"))]
    {
        Err(gate_error("structural search is not compiled in", args))
    }
}

/// The search itself. Only compiled when the capability exists.
#[cfg(feature = "structural-search")]
async fn search(args: &Value) -> Result<Value, JsonRpcError> {
    use crate::cli::live_project::LiveProject;
    use crate::search::textsearch::{CompiledPattern, StructuralOptions, search_structural};
    use std::path::PathBuf;
    use std::time::{Duration, Instant};

    let pattern_text = args
        .get("pattern")
        .or_else(|| args.get("query"))
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                "Missing required argument: pattern",
                "Add \"pattern\": \"<ast-grep pattern>\", e.g. \"fn $NAME($$$ARGS) { $$$BODY }\"",
            )
        })?
        .to_string();

    let limit = match extract_usize(args, "limit", DEFAULT_LIMIT)? {
        0 => MAX_LIMIT,
        n => n.min(MAX_LIMIT),
    };
    let offset = extract_usize(args, "offset", 0)?.min(MAX_LIMIT);
    let per_file_cap = extract_usize(args, "per_file_cap", DEFAULT_PER_FILE)?;
    let timeout_ms = extract_usize(args, "timeout_ms", DEFAULT_TIMEOUT_MS)?;

    // ── Root ────────────────────────────────────────────────────────────────
    let root: PathBuf = match args.get("project_path").and_then(Value::as_str) {
        Some(path) => LiveProject::resolve(path)
            .map(|live| live.root().to_path_buf())
            .map_err(|e| {
                JsonRpcError::invalid_params_with_suggestion(
                    format!("Cannot resolve project_path {path:?}: {e}"),
                    "Pass an existing project directory, or omit project_path for the current one",
                )
            })?,
        None => LiveProject::resolve(".")
            .map(|live| live.root().to_path_buf())
            .map_err(|e| {
                JsonRpcError::invalid_params_with_suggestion(
                    format!("Cannot resolve the current project: {e}"),
                    "Pass project_path explicitly",
                )
            })?,
    };

    let filter = build_filter(args);
    let requested_lang = args
        .get("lang")
        .or_else(|| args.get("language"))
        .and_then(Value::as_str)
        .map(str::to_string);

    // Compiling a pattern parses it as source for the target grammar — pure
    // CPU, and stack-hungry: keep it off the async worker like `find` does for
    // regex compilation.
    let root_for_lang = root.clone();
    let filter_for_lang = filter.clone();
    let pattern_for_compile = pattern_text.clone();
    let (compiled, language_name) = tokio::task::spawn_blocking(move || {
        let language = match requested_lang {
            Some(name) => name,
            None => infer_language(&root_for_lang, &filter_for_lang)?,
        };
        let compiled = CompiledPattern::new(&language, &pattern_for_compile).map_err(|e| {
            JsonRpcError::invalid_params_with_suggestion(
                e.to_string(),
                "Write the pattern in the target language's own syntax; use $NAME and $$$ARGS \
                 for metavariables",
            )
        })?;
        let name = compiled.language();
        Ok::<_, JsonRpcError>((compiled, name))
    })
    .await
    .map_err(|e| JsonRpcError::internal_error(format!("structural search failed: {e}")))??;

    let options = StructuralOptions {
        limit,
        offset,
        per_file_cap,
        max_match_chars: MAX_MATCH_CHARS,
        deadline: (timeout_ms > 0)
            .then(|| Instant::now() + Duration::from_millis(timeout_ms as u64)),
    };
    let output =
        tokio::task::spawn_blocking(move || search_structural(&root, &compiled, &filter, &options))
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("structural search failed: {e}")))?;

    Ok(shape(&pattern_text, language_name, &output))
}

/// Pick the language from the files in scope when the caller named none.
///
/// Refuses to guess between two languages rather than searching the wrong one:
/// a Rust pattern run against Python files returns nothing, and "nothing" reads
/// as "the code does not exist".
#[cfg(feature = "structural-search")]
fn infer_language(
    root: &std::path::Path,
    filter: &crate::search::textsearch::FileFilter,
) -> Result<String, JsonRpcError> {
    use std::collections::BTreeSet;
    let languages: BTreeSet<&'static str> =
        crate::search::textsearch::engine::list_files(root, 10_000, None)
            .into_iter()
            .filter(|rel| filter.allows(rel))
            .filter_map(|rel| {
                crate::search::textsearch::language_for_path(std::path::Path::new(&rel))
                    .map(|id| id.config().name.as_str())
            })
            .collect();
    match languages.len() {
        0 => Err(JsonRpcError::invalid_params_with_suggestion(
            "No structural-search language found in scope".to_string(),
            "Pass \"lang\" explicitly (rust, python, go, typescript, …) or widen the scope",
        )),
        1 => Ok(languages.into_iter().next().unwrap_or_default().to_string()),
        _ => Err(JsonRpcError::invalid_params_with_suggestion(
            format!(
                "Scope holds several languages ({}); refusing to guess which to parse",
                languages.into_iter().collect::<Vec<_>>().join(", ")
            ),
            "Pass \"lang\" to choose one",
        )),
    }
}

/// Build the include/exclude/scope filter from the arguments.
#[cfg(feature = "structural-search")]
fn build_filter(args: &Value) -> crate::search::textsearch::FileFilter {
    let strings = |key: &str| -> Vec<String> {
        match args.get(key) {
            Some(Value::Array(items)) => items
                .iter()
                .filter_map(|v| v.as_str().map(str::to_string))
                .collect(),
            Some(Value::String(one)) if !one.is_empty() => vec![one.clone()],
            _ => Vec::new(),
        }
    };
    crate::search::textsearch::FileFilter::new(
        &strings("include_globs"),
        &strings("exclude_globs"),
        args.get("scope").and_then(Value::as_str),
    )
}

/// Render the search output for the model.
#[cfg(feature = "structural-search")]
fn shape(
    pattern: &str,
    language: &str,
    output: &crate::search::textsearch::StructuralOutput,
) -> Value {
    let files: Vec<Value> = output
        .files
        .iter()
        .map(|file| {
            let hits: Vec<Value> = file
                .hits
                .iter()
                .map(|hit| {
                    let mut entry = json!({
                        "line": hit.line,
                        "col": hit.col,
                        "kind": hit.kind,
                        "text": hit.text,
                        "start_byte": hit.start_byte,
                        "end_byte": hit.end_byte,
                    });
                    if let Some(name) = &hit.capture {
                        entry["capture"] = json!(name);
                    }
                    if let Some(text) = &hit.captured {
                        entry["captured"] = json!(text);
                    }
                    entry
                })
                .collect();
            json!({
                "path": file.rel,
                "lang": file.lang,
                "total_matches": file.total,
                "capped": file.capped,
                "hits": hits,
            })
        })
        .collect();

    let mut value = json!({
        "mode": "structural",
        "pattern": pattern,
        "lang": language,
        "files": files,
        "returned": output.returned,
        "total_matches": output.stats.total_matches,
        "has_more": output.has_more,
        "stats": {
            "candidates": output.stats.candidates,
            "scanned": output.stats.scanned,
            "files_matched": output.stats.files_matched,
            "files_capped": output.stats.files_capped,
            "millis": output.stats.millis,
        },
    });
    if output.stats.files_capped > 0 {
        value["note"] = json!(format!(
            "{} file(s) held more matches than per_file_cap; those extra matches appear on no \
             page — raise per_file_cap or set it to 0 to see them",
            output.stats.files_capped
        ));
    }
    value
}
