// Tier-3 module-level orphan fragment extraction.
//
// Orphan regions are the complement of the Tier-1 node byte ranges within a
// file: statements and comments that no PDG node covers. They are chunked with
// the byte-safe naive chunker (a mid-file slice has no meaningful tree-sitter
// root, so semantic splitting is not attempted) and prefixed with a light
// module header so conceptual queries can match module-level glue code.
//
// Invariant: the leading file-doc region (`[0, file_doc_end)`) is excluded —
// FileSummary nodes already cover it, and double-indexing it would produce
// duplicate hits for file-level queries.

use std::path::Path;

use super::Fragment;
use super::LINES_PER_CHUNK;
use super::chunker::chunk_naive;

/// Inputs for Tier-3 orphan extraction over one file.
pub(crate) struct OrphanInput<'a> {
    /// Raw file bytes (must be valid UTF-8).
    pub(crate) file_bytes: &'a [u8],
    /// File path (used for fragment metadata + module header).
    pub(crate) path: &'a Path,
    /// Tier-1 node byte ranges in this file (may overlap; order-insensitive).
    pub(crate) node_ranges: &'a [(usize, usize)],
    /// End byte of the leading file-doc region; `[0, file_doc_end)` is excluded.
    pub(crate) file_doc_end: usize,
    /// Max bytes per orphan fragment (mirrors `[search] fragment_max_bytes`).
    pub(crate) max_bytes: usize,
}

/// Compute the Tier-3 orphan fragments for one file.
///
/// Returns an empty vector when the file is not valid UTF-8 or every byte is
/// covered by a node range or the file-doc region.
pub(crate) fn orphan_fragments<'a>(input: OrphanInput<'a>) -> Vec<Fragment<'a>> {
    let code = match std::str::from_utf8(input.file_bytes) {
        Ok(code) => code,
        Err(_) => return Vec::new(),
    };
    let file_len = code.len();
    let doc_end = input.file_doc_end.min(file_len);

    // Merge overlapping Tier-1 node ranges into a sorted, disjoint list.
    let mut ranges: Vec<(usize, usize)> = input.node_ranges.to_vec();
    ranges.retain(|(start, end)| start < end && *start < file_len);
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        let end = end.min(file_len);
        match merged.last_mut() {
            Some(last) if start <= last.1 => {
                last.1 = last.1.max(end);
            }
            _ => merged.push((start, end)),
        }
    }

    // Orphan regions = complement of the merged ranges, excluding [0, doc_end).
    let mut regions: Vec<(usize, usize)> = Vec::new();
    let mut cursor = doc_end;
    for (start, end) in merged {
        let start = start.max(doc_end);
        if start > cursor {
            regions.push((cursor, start));
        }
        cursor = cursor.max(end);
    }
    if cursor < file_len {
        regions.push((cursor, file_len));
    }

    // Chunk each region naively; re-base byte/line offsets to file coordinates.
    let mut orphans = Vec::new();
    for (region_start, region_end) in regions {
        if region_end <= region_start {
            continue;
        }
        let region_code = &code[region_start..region_end];
        let line_base = code[..region_start].bytes().filter(|&b| b == b'\n').count();
        let mut fragments = chunk_naive(region_code, input.path, input.max_bytes, LINES_PER_CHUNK);
        for fragment in &mut fragments {
            fragment.start_byte_index += region_start;
            fragment.end_byte_index += region_start;
            fragment.start_line += line_base;
            fragment.end_line += line_base;
        }
        orphans.append(&mut fragments);
    }
    orphans
}
