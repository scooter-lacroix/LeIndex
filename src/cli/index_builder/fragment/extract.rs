// Tier-2/Tier-3 fragment extraction from the PDG + source files (Task 7).
//
// The incremental sync engine (`sync::incremental_sync_fragments`) needs a
// chunk function that turns a changed file's bytes into `FragmentCandidate`s —
// the exact enriched text that will be embedded and content-hashed (invariant
// 3: cache key ≡ embedding input). This module is that function:
//
//   - Tier-2: each PDG node's source range is semantically chunked (sub-symbol
//     fragments), enriched with the owner header (`owner_header`) + preceding
//     doc context, and owned by the node.
//   - Tier-3: module-level orphan regions (complement of the node ranges,
//     excluding the leading file-doc block) are naively chunked and enriched
//     with the module header. Orphans are attributed to the file's FileSummary
//     node when one exists (invariant 6: fragment hits always map to an owner
//     node before surfacing); owner is `None` only for files without a
//     FileSummary node.
//
// Both tiers are fed to the same store, so dedup, root-hash, and hydration
// treat them identically.

use std::path::Path;

use crate::graph::pdg::{EdgeType, NodeType, ProgramDependenceGraph, TraversalConfig};

use super::enrich::{enrich_fragment, enrich_orphan, orphan_header, owner_header};
use super::orphan::{OrphanInput, orphan_fragments};
use super::sync::FragmentCandidate;
use super::{FragmentMetadata, chunk_code};

/// Byte offset of the end of the leading file-doc/comment block.
///
/// Mirrors `index_builder::leading_file_doc`'s scan semantics so the excluded
/// region matches what FileSummary nodes embed (the one region Tier-3 orphans
/// must not double-index). Approximate for CRLF but sufficient as a boundary.
fn leading_file_doc_end(bytes: &[u8]) -> usize {
    let text = String::from_utf8_lossy(bytes);
    let mut offset = 0usize;
    let mut count = 0usize;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.is_empty() {
            if count == 0 {
                offset += line.len() + 1;
                continue;
            }
            break;
        }
        if trimmed.starts_with("//") || trimmed.starts_with('#') || trimmed.starts_with("/*") {
            count += 1;
            // `lines()` strips the terminator; add one byte for `\n`.
            offset += line.len() + 1;
            if count == 16 {
                break;
            }
        } else {
            break;
        }
    }
    offset.min(bytes.len())
}

/// 0-based line number containing `offset` (count of `\n` before it).
fn line_of(bytes: &[u8], offset: usize) -> usize {
    bytes[..offset.min(bytes.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
}

fn blake3_hex(text: &str) -> String {
    blake3::hash(text.as_bytes()).to_hex().to_string()
}

fn node_type_to_str(node_type: &NodeType) -> &'static str {
    match node_type {
        NodeType::Function => "function",
        NodeType::Class => "class",
        NodeType::Method => "method",
        NodeType::Variable => "variable",
        NodeType::Module => "module",
        NodeType::External => "external",
        NodeType::FileSummary => "file_summary",
    }
}

/// Extract Tier-2 (sub-symbol) + Tier-3 (orphan) fragment candidates for one
/// file, ready for the incremental sync engine.
///
/// `max_bytes` bounds both Tier-2 sub-symbol and Tier-3 orphan chunk size
/// (mirrors `[search] fragment_max_bytes`), and `naive_fallback` gates the
/// naive 200-line chunker when a tree-sitter grammar is unavailable (mirrors
/// `[search] fragment_naive_fallback`). Returns an empty vec when the file is
/// not valid UTF-8 or produces no fragments (fully node-covered file).
pub(crate) fn extract_file_fragments(
    pdg: &ProgramDependenceGraph,
    path: &Path,
    file_bytes: &[u8],
    max_bytes: usize,
    orphan_enabled: bool,
    naive_fallback: bool,
) -> Vec<FragmentCandidate> {
    let Ok(code) = std::str::from_utf8(file_bytes) else {
        return Vec::new();
    };
    if code.is_empty() {
        return Vec::new();
    }

    let connectivity_config = TraversalConfig {
        max_depth: Some(1),
        max_nodes: Some(1000),
        allowed_edge_types: Some(&[EdgeType::Call, EdgeType::DataDependency]),
        excluded_node_types: Some(vec![NodeType::External]),
        min_complexity: None,
        min_edge_confidence: 0.0,
    };

    struct NodeInfo {
        id: String,
        name: String,
        node_type: String,
        language: String,
        callers: usize,
        callees: usize,
        complexity: u32,
        byte_range: (usize, usize),
    }

    let file_path_str = path.display().to_string();
    let mut nodes: Vec<NodeInfo> = Vec::new();
    let mut node_ranges: Vec<(usize, usize)> = Vec::new();
    for node_idx in pdg.node_indices() {
        let Some(node) = pdg.get_node(node_idx) else {
            continue;
        };
        if node.file_path.as_ref() != file_path_str {
            continue;
        }
        if matches!(node.node_type, NodeType::External | NodeType::FileSummary) {
            continue;
        }
        if node.byte_range.1 <= node.byte_range.0 {
            continue;
        }
        let callers = pdg.backward_impact(node_idx, &connectivity_config).len();
        let callees = pdg.forward_impact(node_idx, &connectivity_config).len();
        nodes.push(NodeInfo {
            id: node.id.clone(),
            name: node.name.clone(),
            node_type: node_type_to_str(&node.node_type).to_string(),
            language: node.language.clone(),
            callers,
            callees,
            complexity: node.complexity,
            byte_range: node.byte_range,
        });
        node_ranges.push(node.byte_range);
    }

    let mut candidates = Vec::new();

    // Tier-2: sub-symbol fragments inside each node's source range.
    for info in &nodes {
        let (start, end) = info.byte_range;
        if start >= end || end > code.len() {
            continue;
        }
        let node_code = &code[start..end];
        let mut fragments = chunk_code(node_code, path, max_bytes, naive_fallback);
        // Re-base byte offsets to file coordinates (chunk_code offsets are
        // relative to the slice it was given).
        for frag in &mut fragments {
            frag.start_byte_index += start;
            frag.end_byte_index += start;
            let header = owner_header(
                &info.node_type,
                &info.language,
                info.callers,
                info.callees,
                info.complexity as usize,
            );
            let enriched = enrich_fragment(frag, file_bytes, &header, &info.name);
            let content_hash = blake3_hex(&enriched);
            candidates.push(FragmentCandidate {
                content_hash: content_hash.clone(),
                enriched_text: enriched,
                meta: FragmentMetadata {
                    content_hash,
                    owner: Some(info.id.clone()),
                    file_path: file_path_str.clone(),
                    byte_range: (frag.start_byte_index, frag.end_byte_index),
                    line_range: (
                        line_of(file_bytes, frag.start_byte_index),
                        line_of(file_bytes, frag.end_byte_index.saturating_sub(1)),
                    ),
                    embedding_offset: 0,
                },
            });
        }
    }

    // Tier-3: module-level orphan regions (naive chunks). Orphans are
    // attributed to the file's FileSummary node when one exists so their hits
    // map back to a searchable file-level result (invariant 6: fragment hits
    // always map to an owner node before surfacing). Without a FileSummary the
    // orphan keeps owner: None and is stored but not independently searchable.
    let file_summary_id: Option<String> = pdg
        .node_indices()
        .filter_map(|idx| pdg.get_node(idx))
        .find(|n| {
            n.file_path.as_ref() == file_path_str && matches!(n.node_type, NodeType::FileSummary)
        })
        .map(|n| n.id.clone());
    if orphan_enabled && !node_ranges.is_empty() {
        let file_doc_end = leading_file_doc_end(file_bytes);
        for frag in orphan_fragments(OrphanInput {
            file_bytes,
            path,
            node_ranges: &node_ranges,
            file_doc_end,
            max_bytes,
        }) {
            // Language for the module header: prefer the file's own nodes.
            let language = nodes
                .first()
                .map(|n| n.language.clone())
                .unwrap_or_else(|| "unknown".to_string());
            let header = orphan_header(&language, path);
            let enriched = enrich_orphan(&frag, &header);
            let content_hash = blake3_hex(&enriched);
            candidates.push(FragmentCandidate {
                content_hash: content_hash.clone(),
                enriched_text: enriched,
                meta: FragmentMetadata {
                    content_hash,
                    owner: file_summary_id.clone(),
                    file_path: file_path_str.clone(),
                    byte_range: (frag.start_byte_index, frag.end_byte_index),
                    line_range: (
                        line_of(file_bytes, frag.start_byte_index),
                        line_of(file_bytes, frag.end_byte_index.saturating_sub(1)),
                    ),
                    embedding_offset: 0,
                },
            });
        }
    }

    candidates
}
