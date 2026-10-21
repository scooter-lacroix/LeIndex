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
                "per_file_cap": { "type": "integer", "default": DEFAULT_PER_FILE, "minimum": 0, "maximum": MAX_LIMIT, "description": "Shown per file; 0 = no cap" },
                "timeout_ms": { "type": "integer", "default": DEFAULT_TIMEOUT_MS, "minimum": 0, "description": "Time budget; partial results + has_more. 0 = none" },
                "project_path": { "type": "string", "description": "Project directory; omit to use the current project" }
            },
            "required": ["pattern"]
        })
    }

    pub async fn execute(
        &self,
        registry: &std::sync::Arc<crate::cli::registry::ProjectRegistry>,
        args: Value,
    ) -> Result<Value, JsonRpcError> {
        run(registry, &args).await
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

async fn run(
    registry: &std::sync::Arc<crate::cli::registry::ProjectRegistry>,
    args: &Value,
) -> Result<Value, JsonRpcError> {
    if let Some(reason) = unavailable_reason() {
        return Err(gate_error(&reason, args));
    }
    #[cfg(feature = "structural-search")]
    {
        search(registry, args).await
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
async fn search(
    registry: &std::sync::Arc<crate::cli::registry::ProjectRegistry>,
    args: &Value,
) -> Result<Value, JsonRpcError> {
    use crate::search::textsearch::{StructuralOptions, search_structural};

    let pattern_text = parse_pattern(args)?;
    let (limit, offset, per_file_cap, timeout_ms) = parse_paging(args)?;

    let root = resolve_root(registry, args).await?;
    let filter = build_filter(args);
    let (compiled, language_name) = compile_for(args, &root, &filter, pattern_text).await?;

    let options = StructuralOptions {
        limit,
        offset,
        per_file_cap,
        max_match_chars: MAX_MATCH_CHARS,
        deadline: deadline_after(timeout_ms),
    };
    let output =
        tokio::task::spawn_blocking(move || search_structural(&root, &compiled, &filter, &options))
            .await
            .map_err(|e| JsonRpcError::internal_error(format!("structural search failed: {e}")))?;

    Ok(shape(pattern_text, language_name, &output))
}

/// Validate the required pattern, accepting the query alias.
#[cfg(feature = "structural-search")]
fn parse_pattern(args: &Value) -> Result<&str, JsonRpcError> {
    args.get("pattern")
        .or_else(|| args.get("query"))
        .and_then(Value::as_str)
        .filter(|p| !p.trim().is_empty())
        .ok_or_else(|| {
            JsonRpcError::invalid_params_with_suggestion(
                "Missing required argument: pattern",
                "Add \"pattern\": \"<ast-grep pattern>\", e.g. \"fn $NAME($$$ARGS) { $$$BODY }\"",
            )
        })
}

/// The wall-clock deadline for one call, or `None` when no budget was asked for
/// (`timeout_ms: 0`).
#[cfg(feature = "structural-search")]
fn deadline_after(timeout_ms: usize) -> Option<std::time::Instant> {
    (timeout_ms > 0)
        .then(|| std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms as u64))
}

/// The project whose files are searched: `project_path` when given, otherwise
/// the registry's current project.
///
/// The fallback must come from the registry, not from `.`: the daemon's working
/// directory is whatever it was launched in and has no relation to the project
/// the caller is talking to. Resolving `.` silently defined `scope` and the
/// include/exclude globs against a different tree than every sibling tool
/// (`read_symbol`, `find`, `git_status`, `file_summary` all use the registry
/// default), so a bare `leindex_structural` call walked the wrong project — or,
/// when the daemon was started from `/`, an enormous one.
#[cfg(feature = "structural-search")]
async fn resolve_root(
    registry: &std::sync::Arc<crate::cli::registry::ProjectRegistry>,
    args: &Value,
) -> Result<std::path::PathBuf, JsonRpcError> {
    use crate::cli::live_project::LiveProject;

    let resolved = match args.get("project_path").and_then(Value::as_str) {
        Some(path) => LiveProject::resolve(path).map_err(|e| {
            JsonRpcError::invalid_params_with_suggestion(
                format!("Cannot resolve project_path {path:?}: {e}"),
                "Pass an existing project directory, or omit project_path for the current one",
            )
        })?,
        None => {
            let default = registry.default_project_path().await?;
            LiveProject::resolve(&default.to_string_lossy()).map_err(|e| {
                JsonRpcError::invalid_params_with_suggestion(
                    format!(
                        "Cannot resolve the current project {}: {e}",
                        default.display()
                    ),
                    "Pass project_path explicitly",
                )
            })?
        }
    };
    Ok(resolved.root().to_path_buf())
}

/// Compile the pattern for the target language, inferring the language when the
/// caller named none.
///
/// Compiling a pattern parses it as source for the target grammar — pure CPU,
/// and stack-hungry — so it stays off the async worker, exactly as `find` keeps
/// regex compilation off it.
#[cfg(feature = "structural-search")]
async fn compile_for(
    args: &Value,
    root: &std::path::Path,
    filter: &crate::search::textsearch::FileFilter,
    pattern_text: &str,
) -> Result<(crate::search::textsearch::CompiledPattern, &'static str), JsonRpcError> {
    use crate::search::textsearch::CompiledPattern;

    let requested_lang = args
        .get("lang")
        .or_else(|| args.get("language"))
        .and_then(Value::as_str)
        .map(str::to_string);

    let root = root.to_path_buf();
    let filter = filter.clone();
    let pattern = pattern_text.to_string();
    tokio::task::spawn_blocking(move || {
        let language = match requested_lang {
            Some(name) => name,
            None => infer_language(&root, &filter)?,
        };
        let compiled = CompiledPattern::new(&language, &pattern).map_err(|e| {
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
    .map_err(|e| JsonRpcError::internal_error(format!("structural search failed: {e}")))?
}

/// Parse the three paging knobs, each with the contract its own semantics
/// demand.
///
/// `limit` is a budget: a page larger than [`MAX_LIMIT`] is a request for "as
/// much as you can give", and the page is still well defined, so it is clamped.
/// `offset` is a cursor: silently rewriting it moves the window and returns a
/// page from the wrong position, which a paging caller cannot detect — it
/// either loops forever or skips/duplicates a range. It is rejected.
/// `per_file_cap` is a display request too, but `NO_CAP` (0) is remapped to
/// `UNLIMITED_FILE_BUFFER` inside `scan`, so clamping only the explicit numbers
/// keeps "uncapped" meaning the documented ceiling instead of letting a caller
/// allocate past it by asking for a huge number instead of `0`.
#[cfg(feature = "structural-search")]
fn parse_paging(args: &Value) -> Result<(usize, usize, usize, usize), JsonRpcError> {
    let limit = match extract_usize(args, "limit", DEFAULT_LIMIT)? {
        0 => MAX_LIMIT,
        n => n.min(MAX_LIMIT),
    };
    let offset = extract_usize(args, "offset", 0)?;
    if offset > MAX_LIMIT {
        return Err(JsonRpcError::invalid_params_with_suggestion(
            format!("offset {offset} exceeds the maximum {MAX_LIMIT}"),
            "Page with `offset` at most 10000; narrow the query (path/lang/include) rather than \
             paging deeper",
        ));
    }
    let per_file_cap = match extract_usize(args, "per_file_cap", DEFAULT_PER_FILE)? {
        n @ 0..=MAX_LIMIT => n,
        _ => MAX_LIMIT,
    };
    let timeout_ms = extract_usize(args, "timeout_ms", DEFAULT_TIMEOUT_MS)?;
    Ok((limit, offset, per_file_cap, timeout_ms))
}

/// Files inspected when guessing the target language for a caller that named
/// none.
///
/// Deliberately NOT `structural::MAX_CANDIDATE_FILES`, which is an order of
/// magnitude larger (300_000): inference only needs enough files to see which
/// languages are present, and dragging a 300k-file inventory forward before the
/// real search even starts would make a language-less call far slower than a
/// scoped one. Being a sample is exactly why truncation is reported rather than
/// hidden.
#[cfg(feature = "structural-search")]
const LANGUAGE_INFERENCE_FILES: usize = 10_000;

/// Pick the language from the files in scope when the caller named none.
///
/// Refuses to guess between two languages rather than searching the wrong one:
/// a Rust pattern run against Python files returns nothing, and "nothing" reads
/// as "the code does not exist".
///
/// Inference sees only [`LANGUAGE_INFERENCE_FILES`] of the scope, so an empty
/// result on a larger tree must not claim the language is absent — see the
/// `truncated` branch below.
#[cfg(feature = "structural-search")]
fn infer_language(
    root: &std::path::Path,
    filter: &crate::search::textsearch::FileFilter,
) -> Result<String, JsonRpcError> {
    use std::collections::BTreeSet;
    // The filter is applied INSIDE the walk, so `LANGUAGE_INFERENCE_FILES`
    // bounds the matching files rather than the tree. Filtering afterwards
    // sampled "the first N paths root-wide, of which maybe none are in scope",
    // which made a scoped call report no language for a scope full of them —
    // and then advise widening a scope that was never examined.
    let (rels, truncation) = crate::search::textsearch::engine::list_files_matching(
        root,
        LANGUAGE_INFERENCE_FILES,
        None,
        |rel| filter.allows(rel),
    );
    let truncated = truncation.truncated();
    // shh: a one-language scope that is 99% another language still infers the
    // minority one when truncation hid the majority — inference is a sample, not
    // a survey. Fixing that needs a sampling strategy, not a bigger constant.
    let languages: BTreeSet<&'static str> = rels
        .into_iter()
        .filter_map(|rel| {
            crate::search::textsearch::language_for_path(std::path::Path::new(&rel))
                .map(|id| id.config().name.as_str())
        })
        .collect();
    match languages.len() {
        0 => Err(JsonRpcError::invalid_params_with_suggestion(
            if truncated {
                format!(
                    "No structural-search language found in the first \
                     {LANGUAGE_INFERENCE_FILES} files of scope; more remain unexamined"
                )
            } else {
                "No structural-search language found in scope".to_string()
            },
            "Pass \"lang\" explicitly (rust, python, go, typescript, …) or widen the scope",
        )),
        1 => Ok(languages.into_iter().next().unwrap_or_default().to_string()),
        _ => Err(JsonRpcError::invalid_params_with_suggestion(
            {
                let found = languages.into_iter().collect::<Vec<_>>().join(", ");
                if truncated {
                    // The same cut that can HIDE the majority language can also
                    // invent a false ambiguity: a scope that is overwhelmingly
                    // one language still reports "several" when the sample
                    // happened to catch a minority one. Saying so is the
                    // difference between a caller choosing `lang` and a caller
                    // concluding the scope is genuinely polyglot.
                    format!(
                        "Scope holds several languages ({found}) in the first \
                         {LANGUAGE_INFERENCE_FILES} files; more remain unexamined, so this set \
                         may be incomplete"
                    )
                } else {
                    format!(
                        "Scope holds several languages ({found}); refusing to guess which to parse"
                    )
                }
            },
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
        "stopped_by_deadline": output.stopped_by_deadline,
        "stats": {
            "candidates": output.stats.candidates,
            "scanned": output.stats.scanned,
            "skipped_deadline": output.stats.skipped_deadline,
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
    if output.stopped_by_deadline {
        let note = "Search stopped at the time budget; results are partial. Raise timeout_ms or set it to 0 to scan without a deadline";
        value["note"] = json!(match value.get("note").and_then(Value::as_str) {
            Some(existing) => format!("{existing}. {note}"),
            None => note.to_string(),
        });
    }
    value
}

#[cfg(all(test, feature = "structural-search"))]
mod tests {
    use super::{MAX_LIMIT, parse_paging, shape};
    use serde_json::json;

    #[test]
    fn test_shape_reports_deadline_and_preserves_cap_note() {
        let mut output = crate::search::textsearch::StructuralOutput {
            stopped_by_deadline: true,
            has_more: true,
            ..Default::default()
        };
        for capped in [0, 1] {
            output.stats.files_capped = capped;
            let value = shape("fn $NAME() {}", "rust", &output);
            assert_eq!(value["stopped_by_deadline"], true);
            assert_eq!(value["has_more"], true);
            let note = value["note"].as_str().unwrap();
            assert!(note.contains("results are partial"));
            assert_eq!(note.contains("per_file_cap"), capped > 0);
        }
    }

    /// `per_file_cap` must not be a back door around the documented
    /// `UNLIMITED_FILE_BUFFER` ceiling: `scan` uses a non-zero value verbatim as
    /// a per-file hit-buffer capacity, so an arbitrary number is an arbitrary
    /// allocation. `0` is the sanctioned way to ask for "uncapped".
    #[test]
    fn test_per_file_cap_clamped_to_max_limit() {
        let (_, _, cap, _) =
            parse_paging(&json!({ "per_file_cap": MAX_LIMIT * 1000 })).expect("parsed");
        assert_eq!(
            cap, MAX_LIMIT,
            "an oversized per_file_cap must be clamped to the ceiling, not passed through"
        );
    }

    /// `0` is [`NO_CAP`](crate::search::textsearch::NO_CAP) and must survive the
    /// clamp untouched, or the caller's "show me everything" request silently
    /// becomes a cap of 10,000.
    #[test]
    fn test_per_file_cap_zero_still_means_uncapped() {
        let (_, _, cap, _) = parse_paging(&json!({ "per_file_cap": 0 })).expect("parsed");
        assert_eq!(cap, 0, "0 must survive as NO_CAP");
    }
}
