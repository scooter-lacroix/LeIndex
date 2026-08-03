// Fragment enrichment.
//
// A Tier-2 fragment embeds with a compact owner header (type, language,
// connectivity, complexity — the same header shape `enriched_node_content`
// uses for node-level embeddings) plus the owning symbol's file path and the
// comment-marker-stripped doc context immediately preceding the fragment.
// The exact text returned here is what gets embedded and content-hashed, so
// the cache key ≡ embedding input invariant is anchored in this function.

use std::path::Path;

use super::Fragment;

/// Build the owner header line for a Tier-2 fragment.
///
/// Mirrors the `// type:… lang:… callers:… callees:… complexity:…` header
/// produced by `enriched_node_content` so fragment embeddings and node
/// embeddings share the same semantic envelope.
pub(crate) fn owner_header(
    node_type: &str,
    language: &str,
    callers: usize,
    callees: usize,
    complexity: usize,
) -> String {
    format!(
        "// type:{node_type} lang:{language} callers:{} callees:{} complexity:{}",
        callers.min(50),
        callees.min(50),
        complexity,
    )
}

/// Build the light header for a Tier-3 orphan fragment.
pub(crate) fn orphan_header(language: &str, file_path: &Path) -> String {
    format!(
        "// type:module lang:{language} file:{}",
        file_path.display()
    )
}

/// Enrich a Tier-2 fragment into the exact text that is embedded and hashed.
///
/// Layout (mirrors `enriched_node_content`'s node envelope):
/// ```text
/// {owner_header}
/// // {symbol} in {path}
/// {preceding_doc_context}   (comment markers stripped, ≤24 lines)
/// {fragment content}
/// ```
pub(crate) fn enrich_fragment(
    fragment: &Fragment<'_>,
    file_bytes: &[u8],
    header: &str,
    symbol: &str,
) -> String {
    let path = fragment.file_path.display();
    let doc =
        crate::cli::index_builder::preceding_doc_context(file_bytes, fragment.start_byte_index);
    if doc.is_empty() {
        format!("{header}\n// {symbol} in {path}\n{}", fragment.content)
    } else {
        format!(
            "{header}\n// {symbol} in {path}\n{doc}\n{}",
            fragment.content
        )
    }
}

/// Enrich a Tier-3 orphan fragment with its module header.
pub(crate) fn enrich_orphan(fragment: &Fragment<'_>, header: &str) -> String {
    format!("{header}\n{}", fragment.content)
}
