//! Post-edit validation (§4.3): `validate: "syntax"`.
//!
//! Tree-sitter parse of each edited file AFTER apply, reporting
//! `syntax: ok` or the first N error nodes as `line:col`. Uses the
//! repo's shared grammar cache (`LanguageId::from_extension` +
//! `from_cache`), never a private grammar table.
//!
//! `validate: "check"` (cargo/tsc/pyright) is wired at INTEGRATION to
//! lane F's `check_scoped`. This module owns the single error site for
//! it so integration replaces exactly one function.

use super::protocol::JsonRpcError;
use crate::parse::grammar::LanguageId;
use std::path::Path;

/// Grammar has no error-node semantics for these doc flavors (they are
/// loaded with a JSON stand-in in the shared cache), so syntax validation
/// is skipped for them.
fn is_doc_flavor(id: LanguageId) -> bool {
    matches!(
        id,
        LanguageId::Markdown | LanguageId::Rst | LanguageId::Adoc | LanguageId::PlainText
    )
}

/// One file's syntax-validation outcome.
pub(crate) struct SyntaxOutcome {
    /// The file that was parsed (as the caller named it).
    pub display_path: String,
    /// `ok`, `skipped (no grammar)`, or `N error(s): line:col …`.
    pub summary: String,
    /// First error nodes as `line:col kind` entries.
    pub errors: Vec<String>,
}

/// Parse every edited file and collect up to `max_errors_per_file` error
/// nodes per file.
pub(crate) fn validate_syntax(
    files: &[(String, std::path::PathBuf, String)],
    max_errors_per_file: usize,
) -> Vec<SyntaxOutcome> {
    files
        .iter()
        .map(|(display, path, content)| {
            let ParseOutcome { total, shown } = parse_one(path, content, max_errors_per_file);
            let summary = if total == 0 {
                "ok".to_string()
            } else if shown.len() < total {
                format!(
                    "{} error(s): {}; +{} more",
                    total,
                    shown.join("; "),
                    total - shown.len()
                )
            } else {
                format!("{} error(s): {}", total, shown.join("; "))
            };
            SyntaxOutcome {
                display_path: display.clone(),
                summary,
                errors: shown,
            }
        })
        .collect()
}

/// All error/missing node counts for one file, with at most `max` of them
/// rendered.
struct ParseOutcome {
    total: usize,
    shown: Vec<String>,
}

fn parse_one(path: &Path, content: &str, max_errors: usize) -> ParseOutcome {
    let skip = |why: &str| ParseOutcome {
        total: 0,
        shown: vec![format!("skipped ({why})")],
    };
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let Some(id) = LanguageId::from_extension(&ext) else {
        return skip("unknown extension");
    };
    if is_doc_flavor(id) {
        return skip("no grammar");
    }
    let Ok(language) = id.from_cache() else {
        return skip("grammar unavailable");
    };
    let mut parser = tree_sitter::Parser::new();
    if parser.set_language(&language).is_err() {
        return skip("grammar unavailable");
    }
    let Some(tree) = parser.parse(content.as_bytes(), None) else {
        return ParseOutcome {
            total: 1,
            shown: vec!["parse failed".to_string()],
        };
    };
    let mut collector = ErrorCollector {
        max: max_errors,
        total: 0,
        shown: Vec::new(),
    };
    collector.walk(tree.root_node());
    ParseOutcome {
        total: collector.total,
        shown: collector.shown,
    }
}

/// Depth-first collector: counts every error/missing node, renders the
/// first `max`.
struct ErrorCollector {
    max: usize,
    total: usize,
    shown: Vec<String>,
}

impl ErrorCollector {
    fn walk(&mut self, node: tree_sitter::Node<'_>) {
        if node.is_error() || node.is_missing() {
            self.total += 1;
            if self.shown.len() < self.max {
                let start = node.start_position();
                let kind = if node.is_missing() {
                    "missing syntax element"
                } else {
                    "syntax error"
                };
                self.shown.push(format!(
                    "{}:{} {}",
                    start.row + 1,
                    start.column + 1,
                    kind
                ));
            }
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            self.walk(child);
        }
    }
}

/// THE single error site for `validate: "check"`.
///
/// Per the roadmap SPEC, cargo/tsc/pyright checking is wired at
/// integration to lane F's `check_scoped`. This build accepts the
/// argument and refuses it with this exact message so callers get a
/// clear, greppable answer instead of silent no-op behavior.
pub(crate) fn validate_check_unavailable() -> JsonRpcError {
    JsonRpcError::invalid_params(
        "validate: \"check\" is not available in this build — cargo/tsc/pyright checking \
         is wired at integration; use validate: \"syntax\" for tree-sitter parsing",
    )
}

/// Resolve the `validate` argument (absent / "syntax" / "check") into the
/// action the handler must take. Errors on unknown values.
pub(crate) enum ValidateRequest {
    /// No validation requested.
    None,
    /// Tree-sitter parse after apply.
    Syntax,
}

pub(crate) fn resolve_validate_arg(args: &serde_json::Value) -> Result<ValidateRequest, JsonRpcError> {
    let Some(raw) = args.get("validate").and_then(|v| v.as_str()) else {
        return Ok(ValidateRequest::None);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "none" => Ok(ValidateRequest::None),
        "syntax" => Ok(ValidateRequest::Syntax),
        "check" => Err(validate_check_unavailable()),
        other => Err(JsonRpcError::invalid_params(format!(
            "Unknown validate value '{other}' — supported: \"syntax\" (tree-sitter parse); \
             \"check\" is wired at integration"
        ))),
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_validate_check_errors_from_one_place() {
        let err = validate_check_unavailable();
        assert!(
            err.message_with_hint()
                .contains("not available in this build"),
            "check must refuse with the integration placeholder message: {err}"
        );
    }

    #[test]
    fn test_resolve_validate_arg_accepts_syntax_and_check() {
        let syntax = resolve_validate_arg(&serde_json::json!({"validate": "syntax"})).unwrap();
        assert!(matches!(syntax, ValidateRequest::Syntax));
        let none = resolve_validate_arg(&serde_json::json!({})).unwrap();
        assert!(matches!(none, ValidateRequest::None));
        let check = resolve_validate_arg(&serde_json::json!({"validate": "check"}));
        assert!(check.is_err(), "check must error in this build");
    }

    #[test]
    fn test_broken_rust_edit_is_flagged_with_line_col() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("broken.rs");
        std::fs::write(&path, "fn broken( {\n    let x = ;\n}\n").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let outcomes = validate_syntax(
            &vec![("broken.rs".to_string(), path.clone(), content)],
            5,
        );
        assert_eq!(outcomes.len(), 1);
        assert!(
            !outcomes[0].errors.is_empty(),
            "broken Rust must produce error nodes: {}",
            outcomes[0].summary
        );
        assert!(
            outcomes[0].errors[0].contains(':'),
            "error must carry line:col: {}",
            outcomes[0].errors[0]
        );
    }

    #[test]
    fn test_valid_rust_edit_reports_ok() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("fine.rs");
        std::fs::write(&path, "fn fine() -> u32 { 42 }\n").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let outcomes = validate_syntax(&vec![("fine.rs".to_string(), path, content)], 5);
        assert_eq!(outcomes[0].summary, "ok");
        assert!(outcomes[0].errors.is_empty());
    }

    #[test]
    fn test_markdown_is_skipped_not_parsed() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.md");
        std::fs::write(&path, "# not rust at all ((( \n").unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let outcomes = validate_syntax(&vec![("notes.md".to_string(), path, content)], 5);
        assert!(
            outcomes[0].summary.contains("skipped"),
            "doc flavors must be skipped: {}",
            outcomes[0].summary
        );
    }
}
