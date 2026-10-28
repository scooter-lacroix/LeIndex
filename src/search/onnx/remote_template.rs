//! Task-template rendering for remote embeddings (`{text}` / `{title}`).
//!
//! Moved out of `remote.rs` by pure code motion (Large File Detection gate).
//! Both helpers are re-exported from the parent, so the
//! `crate::search::onnx::remote::apply_doc_template` path keeps resolving.

/// Apply a task template to `text`: replace every `{text}` placeholder with
/// the text (prefix-only templates — no placeholder — prepend + space).
pub(crate) fn apply_template<'a>(
    template: Option<&str>,
    text: &'a str,
) -> std::borrow::Cow<'a, str> {
    match template {
        None => std::borrow::Cow::Borrowed(text),
        Some(t) if t.contains("{text}") => std::borrow::Cow::Owned(t.replace("{text}", text)),
        Some(t) => std::borrow::Cow::Owned(format!("{t} {text}")),
    }
}

/// Apply the DOCUMENT task template with a title.
///
/// EmbeddingGemma2 document inputs are `title: {title} | text: {text}`; the
/// model card mandates `title: none` when no title is available, so callers
/// without a title (queries aside, the title-less doc paths, fragments)
/// pass `None` and every `{title}` placeholder resolves to `none`.
///
/// Substitution is a SINGLE LEFT-TO-RIGHT PASS over the template, and neither
/// substituted value is ever rescanned. Replacing `{title}` and then searching
/// the already-filled string for `{text}` — which is what this used to do —
/// let a title containing the literal `{text}` swallow the document body: a
/// source file legitimately named `{text}.rs` produces exactly that title, and
/// its embedding would be built from the body twice and no title at all.
pub(crate) fn apply_doc_template<'a>(
    template: Option<&str>,
    title: Option<&str>,
    text: &'a str,
) -> std::borrow::Cow<'a, str> {
    let title = title.unwrap_or("none");
    let Some(template) = template else {
        return std::borrow::Cow::Borrowed(text);
    };
    if !template.contains("{text}") && !template.contains("{title}") {
        // Prefix-only template: prepend and separate.
        return std::borrow::Cow::Owned(format!("{template} {text}"));
    }
    let mut out = String::with_capacity(template.len() + text.len() + title.len());
    let mut rest = template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        let after = &rest[open..];
        if let Some(tail) = after.strip_prefix("{title}") {
            out.push_str(title);
            rest = tail;
        } else if let Some(tail) = after.strip_prefix("{text}") {
            out.push_str(text);
            rest = tail;
        } else {
            // A literal brace that starts no placeholder.
            out.push('{');
            rest = &after[1..];
        }
    }
    out.push_str(rest);
    // A template with `{title}` but no `{text}` still needs the body appended,
    // matching the historical behaviour.
    if !template.contains("{text}") {
        out.push(' ');
        out.push_str(text);
    }
    std::borrow::Cow::Owned(out)
}

#[cfg(test)]
mod template_tests {
    use super::apply_doc_template;

    #[test]
    fn test_title_containing_a_placeholder_does_not_swallow_the_body() {
        // A source file named `{text}.rs` yields exactly this title. The old
        // two-pass replace substituted the BODY into the title slot.
        let out = apply_doc_template(
            Some("title: {title} | text: {text}"),
            Some("{text}.rs"),
            "BODY",
        );
        assert_eq!(
            out, "title: {text}.rs | text: BODY",
            "a title must be inserted verbatim and never rescanned for placeholders"
        );
        assert!(
            !out.contains("text: BODY |"),
            "the body must not replace the title"
        );
    }

    #[test]
    fn test_missing_title_resolves_to_none() {
        assert_eq!(
            apply_doc_template(Some("title: {title} | text: {text}"), None, "B"),
            "title: none | text: B"
        );
    }

    #[test]
    fn test_prefix_only_template_appends_the_body() {
        assert_eq!(
            apply_doc_template(Some("search_document:"), None, "B"),
            "search_document: B"
        );
    }

    #[test]
    fn test_no_template_returns_the_raw_text() {
        assert_eq!(apply_doc_template(None, Some("t"), "B"), "B");
    }
}
