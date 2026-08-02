// Semantic (tree-sitter) and naive fragment chunkers.
//
// Ported from Warp's `full_source_code_embedding/chunker/{semantic,naive}.rs`.
// Adaptations: byte offsets are plain `usize` (no `string_offset::ByteOffset`),
// line spans are computed in-tree (no `line_span` crate), and the malloc_trim
// release (a `nix`-dependent linux-only nicety) is intentionally omitted —
// zero new dependencies.

use std::path::Path;

use super::Fragment;
use super::coalesce_fragments;

/// Maximum depth for recursive tree traversal to prevent infinite recursion
/// or excessive depth in malformed/deeply nested code.
const MAX_TRAVERSAL_DEPTH: usize = 200;

/// Chunks code semantically using a tree-sitter `language`.
///
/// The parse is scoped to a block so the Parser/Tree are dropped once the
/// fragments are created (the borrows the fragments hold are into `code`, not
/// into the tree). Returns an error when parsing or splitting fails; callers
/// fall back to naive chunking.
pub(crate) fn chunk_semantic<'a>(
    code: &'a str,
    path: &'a Path,
    max_bytes_per_chunk: usize,
    language: &tree_sitter::Language,
) -> anyhow::Result<Vec<Fragment<'a>>> {
    // Wrap this in a block to ensure the treesitter Parser / Tree are dropped
    // after creating the fragments.
    let fragments = {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(language)?;

        let tree = parser
            .parse(code, None /* old_tree */)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse code"))?;

        let mut cursor = tree.walk();

        let nodes = split_node(
            tree.root_node(),
            code,
            max_bytes_per_chunk,
            path,
            &mut cursor,
            0, // initial depth
        )?;

        coalesce_fragments(nodes.into_iter(), code, max_bytes_per_chunk)
    };

    Ok(fragments)
}

/// Splits a [`tree_sitter::Node`] into a series of [`Fragment`]s that are at
/// most `max_bytes_per_chunk` bytes.
///
/// Recursion stays within the node's own byte range (invariant 5: fragments
/// never cross the owner node's range) — `current_fragment` always starts at
/// the node's start byte and grows only through the node's children.
fn split_node<'a, 'b>(
    node: tree_sitter::Node<'b>,
    code: &'a str,
    max_bytes_per_chunk: usize,
    path: &'a Path,
    cursor: &mut tree_sitter::TreeCursor<'b>,
    depth: usize,
) -> anyhow::Result<Vec<Fragment<'a>>> {
    // Check if we've exceeded the maximum traversal depth.
    if depth > MAX_TRAVERSAL_DEPTH {
        return Err(anyhow::anyhow!(
            "Maximum traversal depth {} exceeded, falling back to naive chunking",
            MAX_TRAVERSAL_DEPTH
        ));
    }

    let mut current_fragment = Fragment::from_node_start(node, path);
    let mut fragments = vec![];

    // Collect into a vec to avoid a double mutable borrow with `cursor` when we
    // make the recursive call below.
    let children: Vec<_> = node.children(cursor).collect();

    for child in children {
        let child_size = child.end_byte().saturating_sub(child.start_byte());

        // The child is larger than the max chunk size, so we need to split it recursively.
        if child_size > max_bytes_per_chunk {
            let mut new_fragment = Fragment::from_node_end(child, path);
            std::mem::swap(&mut current_fragment, &mut new_fragment);
            fragments.push(new_fragment);

            fragments.append(&mut split_node(
                child,
                code,
                max_bytes_per_chunk,
                path,
                cursor,
                depth + 1,
            )?);
        } else if child_size + current_fragment.size() > max_bytes_per_chunk {
            // The child would make the current fragment too large, so we finalize the current
            // fragment and create a new one.
            fragments.push(current_fragment);
            current_fragment = Fragment::from_node_start(child, path);
            current_fragment.append(&Fragment::from_node_end(child, path), code);
        } else {
            // The child fits within the current fragment.
            current_fragment.end_line = child.end_position().row;
            current_fragment.end_byte_index = child.end_byte();
            current_fragment.content = &code[current_fragment.start_byte_index..child.end_byte()];
        }
    }

    fragments.push(current_fragment);

    Ok(fragments)
}

impl<'a> Fragment<'a> {
    /// Creates a fragment comprised solely of the start of the given node.
    fn from_node_start(node: tree_sitter::Node<'_>, path: &'a Path) -> Self {
        Fragment {
            content: "",
            start_line: node.start_position().row,
            end_line: node.start_position().row,
            start_byte_index: node.start_byte(),
            end_byte_index: node.start_byte(),
            file_path: path,
        }
    }

    /// Creates a fragment comprised solely of the end of the given node.
    fn from_node_end(node: tree_sitter::Node<'_>, path: &'a Path) -> Self {
        Fragment {
            content: "",
            start_line: node.end_position().row,
            end_line: node.end_position().row,
            start_byte_index: node.end_byte(),
            end_byte_index: node.end_byte(),
            file_path: path,
        }
    }
}

// ---------------------------------------------------------------------------
// Naive fallback chunker (ported from Warp `chunker/naive.rs`)
// ---------------------------------------------------------------------------

/// Chunks the given code into [`Fragment`]s. Each chunk is at most
/// `num_lines_per_chunk` lines long, and contains at most
/// `max_bytes_per_chunk` bytes.
pub(crate) fn chunk_naive<'a>(
    code: &'a str,
    path: &'a Path,
    max_bytes_per_chunk: usize,
    num_lines_per_chunk: usize,
) -> Vec<Fragment<'a>> {
    let lines = line_spans(code);
    let chunks = lines.chunks(num_lines_per_chunk);

    chunks
        .into_iter()
        .flat_map(|chunk| {
            let (start_line, start_range) = chunk[0];
            // `slice::chunks()` never yields an empty slice, so `last()` is
            // always `Some` here — the expect branch is unreachable.
            let (end_line, end_range) =
                chunk.last().expect("Chunks must have at least one element");

            if (end_range.1 - start_range.0) > max_bytes_per_chunk {
                let chunked_fragments = chunk.iter().flat_map(|(line, line_span)| {
                    chunk_line_by_bytes(code, path, max_bytes_per_chunk, *line, *line_span)
                });

                return coalesce_fragments(chunked_fragments, code, max_bytes_per_chunk);
            }

            vec![Fragment {
                content: &code[start_range.0..end_range.1],
                start_line,
                end_line: *end_line,
                file_path: path,
                start_byte_index: start_range.0,
                end_byte_index: end_range.1,
            }]
        })
        .collect()
}

/// Byte ranges of each line in `code` as `(start_byte, end_byte)` pairs with
/// `end_byte` exclusive of the trailing newline (mirrors Warp's `line_span`
/// semantics: content spans exclude the line terminator).
fn line_spans(code: &str) -> Vec<(usize, (usize, usize))> {
    let bytes = code.as_bytes();
    let mut spans = Vec::new();
    let mut line_start = 0;
    let mut line_number = 0;

    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'\n' {
            spans.push((line_number, (line_start, index)));
            line_number += 1;
            line_start = index + 1;
        }
    }

    // Emit the final line only when it is non-empty (code does not end with a
    // newline). Mirrors `str::lines()` / Warp's `LineSpans`: a trailing empty
    // line after a final newline is not a real line.
    if line_start < bytes.len() {
        spans.push((line_number, (line_start, bytes.len())));
    }

    spans
}

/// Chunks the line represented by `(line_start, line_end)` into multiple
/// fragments if it exceeds `max_bytes_per_chunk`.
fn chunk_line_by_bytes<'a>(
    code: &'a str,
    path: &'a Path,
    max_bytes_per_chunk: usize,
    line_number: usize,
    line_span: (usize, usize),
) -> Vec<Fragment<'a>> {
    let (line_start, line_end) = line_span;
    let line_content = &code[line_start..line_end];
    let line_length = line_end - line_start;

    // If the line is smaller than max_bytes_per_chunk, return it as a single fragment.
    if line_length <= max_bytes_per_chunk {
        return vec![Fragment {
            content: line_content,
            start_line: line_number,
            end_line: line_number,
            file_path: path,
            start_byte_index: line_start,
            end_byte_index: line_end,
        }];
    }

    // Otherwise, split the line into multiple fragments.
    let mut fragments = Vec::new();
    let mut current_start = line_start;

    while current_start < line_end {
        let remaining_bytes = line_end - current_start;
        let chunk_size = std::cmp::min(remaining_bytes, max_bytes_per_chunk);
        let mut chunk_end = current_start + chunk_size;

        // Ensure chunk_end is on a UTF-8 character boundary.
        while chunk_end > current_start && !code.is_char_boundary(chunk_end) {
            chunk_end -= 1;
        }

        // If we couldn't find a valid boundary within reasonable distance,
        // move forward to the next character boundary instead.
        if chunk_end <= current_start {
            chunk_end = current_start + chunk_size;
            while chunk_end < line_end && !code.is_char_boundary(chunk_end) {
                chunk_end += 1;
            }
        }

        fragments.push(Fragment {
            content: &code[current_start..chunk_end],
            start_line: line_number,
            end_line: line_number,
            file_path: path,
            start_byte_index: current_start,
            end_byte_index: chunk_end,
        });

        current_start = chunk_end;
    }

    fragments
}
