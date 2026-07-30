use super::*;

/// Resolve cross-file call edges after all per-file PDGs have been merged.
///
/// During per-file PDG extraction, `extract_call_edges` can only resolve calls
/// to symbols defined in the same file (because the resolution maps are built
/// from that file's signatures alone). This function performs a second pass
/// over the merged PDG using ALL signatures from ALL files, adding call edges
/// that span file boundaries.
///
/// # Arguments
///
/// * `pdg` - The merged PDG containing nodes from all files
/// * `all_signatures` - Signatures from all files in the project
///
/// This function mutates the PDG in-place, adding new `Call` edges for
/// cross-file call relationships that were not resolved during per-file
/// extraction.
pub fn resolve_cross_file_call_edges(
    pdg: &mut ProgramDependenceGraph,
    all_signatures: &[SignatureInfo],
) {
    let owned: Vec<(Option<&str>, &SignatureInfo)> =
        all_signatures.iter().map(|sig| (None, sig)).collect();
    resolve_cross_file_call_edges_inner(pdg, &owned);
}

/// Resolve cross-file call edges with each signature bound to its source file.
///
/// Real indexing still knows which parse result produced each signature. Keeping
/// that ownership here prevents duplicate `qualified_name` definitions in
/// different files from receiving each other's calls.
pub fn resolve_cross_file_call_edges_for_files(
    pdg: &mut ProgramDependenceGraph,
    all_signatures: &[(String, SignatureInfo)],
) {
    let owned: Vec<(Option<&str>, &SignatureInfo)> = all_signatures
        .iter()
        .map(|(file_path, sig)| (Some(file_path.as_str()), sig))
        .collect();
    resolve_cross_file_call_edges_inner(pdg, &owned);
}

struct CrossFileCallIndexes {
    qname_to_node: HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    qname_file_to_node: HashMap<(String, String), Vec<crate::graph::pdg::NodeId>>,
    exact_map: HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    last_map: HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    suffix_map: HashMap<String, Vec<crate::graph::pdg::NodeId>>,
}

fn build_cross_file_call_indexes(pdg: &ProgramDependenceGraph) -> CrossFileCallIndexes {
    let mut indexes = CrossFileCallIndexes {
        qname_to_node: HashMap::new(),
        qname_file_to_node: HashMap::new(),
        exact_map: HashMap::new(),
        last_map: HashMap::new(),
        suffix_map: HashMap::new(),
    };

    for nid in pdg.node_indices() {
        let Some(node) = pdg.get_node(nid) else {
            continue;
        };
        if node.node_type == NodeType::External {
            continue;
        }
        let Some(qname) = qualified_name_from_node(node) else {
            tracing::debug!(
                node_id = %node.id,
                file_path = %node.file_path,
                "Skipping node whose id does not start with its file_path prefix"
            );
            continue;
        };

        let is_module = node.node_type == NodeType::Module;
        if !is_module {
            indexes
                .qname_to_node
                .entry(qname.to_string())
                .or_default()
                .push(nid);
            indexes
                .qname_file_to_node
                .entry((qname.to_string(), node.file_path.to_string()))
                .or_default()
                .push(nid);
        } else {
            indexes.qname_to_node.entry(qname.to_string()).or_default();
        }

        let normalized = normalize_symbol(qname);
        let segments: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();
        indexes
            .exact_map
            .entry(normalized.clone())
            .or_default()
            .push(nid);
        if let Some(last) = segments.last() {
            indexes
                .last_map
                .entry(last.to_string())
                .or_default()
                .push(nid);
        }
        for len in 2..=3_usize.min(segments.len()) {
            indexes
                .suffix_map
                .entry(segments[segments.len() - len..].join("."))
                .or_default()
                .push(nid);
        }
        if !is_module {
            indexes
                .last_map
                .entry(node.name.to_string())
                .or_default()
                .push(nid);
        }
    }

    indexes
}

const COMMON_CALL_NAMES: &[&str] = &[
    "new",
    "clone",
    "name",
    "len",
    "get",
    "set",
    "add",
    "remove",
    "push",
    "pop",
    "iter",
    "next",
    "send",
    "recv",
    "read",
    "write",
    "open",
    "close",
    "start",
    "stop",
    "run",
    "exec",
    "call",
    "apply",
    "map",
    "filter",
    "fold",
    "collect",
    "into",
    "from",
    "to",
    "as",
    "is",
    "has",
    "can",
    "should",
    "will",
    "do",
    "done",
    "ok",
    "err",
    "some",
    "none",
    "true",
    "false",
    "nil",
    "null",
    "self",
    "super",
    "init",
    "free",
    "drop",
    "copy",
    "dup",
    "swap",
    "cmp",
    "eq",
    "ne",
    "lt",
    "le",
    "gt",
    "ge",
    "hash",
    "fmt",
    "dbg",
    "print",
    "println",
    "format",
    "parse",
    "unwrap",
    "expect",
    "ok_or",
    "ok_or_else",
    "map_err",
    "and_then",
    "or_else",
    "unwrap_or",
    "unwrap_or_else",
    "unwrap_or_default",
    "is_some",
    "is_none",
    "is_ok",
    "is_err",
    "as_ref",
    "as_mut",
    "as_str",
    "as_bytes",
    "trim",
    "split",
    "join",
    "replace",
    "contains",
    "starts_with",
    "ends_with",
    "find",
    "rfind",
    "matches",
    "to_string",
    "to_owned",
    "to_vec",
    "into_string",
    "into_bytes",
    "into_vec",
    "into_iter",
    "keys",
    "values",
    "entries",
    "execute",
    "handle",
    "process",
    "update",
    "create",
    "delete",
    "save",
    "load",
    "fetch",
    "build",
    "make",
    "spawn",
    "fork",
    "warn",
    "info",
    "error",
    "trace",
    "debug",
    "log",
    "json",
    "yaml",
    "toml",
    "xml",
    "html",
    "csv",
    "Ok",
    "Err",
    "Some",
    "None",
    "Result",
    "Option",
    "Vec",
    "Box",
    "Rc",
    "Arc",
    "Cell",
    "RefCell",
    "Mutex",
    "RwLock",
    "String",
    "str",
    "HashMap",
    "HashSet",
    "BTreeMap",
    "BTreeSet",
    "VecDeque",
    "LinkedList",
    "JsonRpcError",
    "Error",
    "Response",
    "Request",
    "Value",
    "serde",
    "serde_json",
    "display",
    "render",
];

fn resolve_cross_file_call_targets(
    candidates: &[String],
    indexes: &CrossFileCallIndexes,
) -> Vec<crate::graph::pdg::NodeId> {
    let mut targets = Vec::new();
    let mut last_segment_fallback = None;

    for candidate in candidates {
        let normalized = normalize_symbol(candidate);
        let segments: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();
        if let Some(ids) = indexes.exact_map.get(&normalized) {
            targets.extend(ids);
        }
        for len in 2..=3_usize.min(segments.len()) {
            if let Some(ids) = indexes
                .suffix_map
                .get(&segments[segments.len() - len..].join("."))
            {
                targets.extend(ids);
            }
        }
        if segments.len() == 1 {
            if let Some(ids) = indexes.last_map.get(&normalized) {
                targets.extend(ids);
            }
        }
        if last_segment_fallback.is_none() {
            last_segment_fallback = segments.last().map(|segment| (*segment).to_string());
        }
    }

    if targets.is_empty() {
        if let Some(last) = last_segment_fallback {
            if last.len() > 2 && !COMMON_CALL_NAMES.contains(&last.as_str()) {
                if let Some(ids) = indexes.last_map.get(&last) {
                    targets.extend(ids);
                }
            }
        }
    }
    targets
}

fn cross_file_type_target(
    call_target: &str,
    indexes: &CrossFileCallIndexes,
) -> Option<crate::graph::pdg::NodeId> {
    let callee_name = normalize_symbol(call_target);
    let (scoped_prefix, _) = callee_name.rsplit_once('.')?;
    let bare_type = scoped_prefix.rsplit('.').next().unwrap_or(scoped_prefix);
    bare_type
        .chars()
        .next()
        .is_some_and(|c| c.is_uppercase())
        .then_some(())?;
    indexes
        .qname_to_node
        .get(scoped_prefix)
        .and_then(|ids| ids.first())
        .or_else(|| indexes.last_map.get(bare_type).and_then(|ids| ids.first()))
        .copied()
}

fn existing_call_edges(
    pdg: &ProgramDependenceGraph,
) -> HashSet<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)> {
    pdg.edge_indices()
        .filter(|&edge_idx| {
            pdg.get_edge(edge_idx)
                .is_some_and(|edge| edge.edge_type == EdgeType::Call)
        })
        .filter_map(|edge_idx| pdg.edge_endpoints(edge_idx))
        .collect()
}

fn queue_cross_file_call_edge(
    caller_id: crate::graph::pdg::NodeId,
    target_id: crate::graph::pdg::NodeId,
    existing_edges: &mut HashSet<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)>,
    new_edges: &mut Vec<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)>,
) {
    if caller_id != target_id && existing_edges.insert((caller_id, target_id)) {
        new_edges.push((caller_id, target_id));
    }
}

fn resolve_cross_file_call_edges_inner(
    pdg: &mut ProgramDependenceGraph,
    all_signatures: &[(Option<&str>, &SignatureInfo)],
) {
    let indexes = build_cross_file_call_indexes(pdg);
    let mut existing_edges = existing_call_edges(pdg);

    let mut new_edges = Vec::new();

    for (source_file, sig) in all_signatures {
        let alias_map = import_alias_map(&sig.imports);
        // Real indexing passes the source file for each signature. Use it to
        // bind duplicate qualified names to their owning PDG node; the legacy
        // no-file API falls back to all matching definitions.
        let caller_ids = if let Some(source_file) = source_file {
            match indexes
                .qname_file_to_node
                .get(&(sig.qualified_name.clone(), (*source_file).to_string()))
            {
                Some(ids) if !ids.is_empty() => ids.clone(),
                _ => continue,
            }
        } else {
            match indexes.qname_to_node.get(&sig.qualified_name) {
                Some(ids) if !ids.is_empty() => ids.clone(),
                _ => continue,
            }
        };

        for caller_id in caller_ids {
            let caller_ns = caller_namespace(&sig.qualified_name);

            for call_target in &sig.calls {
                let candidates =
                    ordered_resolution_candidates(call_target, &alias_map, caller_ns.as_deref());

                let targets = resolve_cross_file_call_targets(&candidates, &indexes);
                {
                    const COMMON_NAMES: &[&str] = &[
                        "new",
                        "clone",
                        "name",
                        "len",
                        "get",
                        "set",
                        "add",
                        "remove",
                        "push",
                        "pop",
                        "iter",
                        "next",
                        "send",
                        "recv",
                        "read",
                        "write",
                        "open",
                        "close",
                        "start",
                        "stop",
                        "run",
                        "exec",
                        "call",
                        "apply",
                        "map",
                        "filter",
                        "fold",
                        "collect",
                        "into",
                        "from",
                        "to",
                        "as",
                        "is",
                        "has",
                        "can",
                        "should",
                        "will",
                        "do",
                        "done",
                        "ok",
                        "err",
                        "some",
                        "none",
                        "true",
                        "false",
                        "nil",
                        "null",
                        "self",
                        "super",
                        "init",
                        "free",
                        "drop",
                        "copy",
                        "dup",
                        "swap",
                        "cmp",
                        "eq",
                        "ne",
                        "lt",
                        "le",
                        "gt",
                        "ge",
                        "hash",
                        "fmt",
                        "dbg",
                        "print",
                        "println",
                        "format",
                        "parse",
                        "unwrap",
                        "expect",
                        "ok_or",
                        "ok_or_else",
                        "map_err",
                        "and_then",
                        "or_else",
                        "unwrap_or",
                        "unwrap_or_else",
                        "unwrap_or_default",
                        "is_some",
                        "is_none",
                        "is_ok",
                        "is_err",
                        "as_ref",
                        "as_mut",
                        "as_str",
                        "as_bytes",
                        "trim",
                        "split",
                        "join",
                        "replace",
                        "contains",
                        "starts_with",
                        "ends_with",
                        "find",
                        "rfind",
                        "matches",
                        "parse",
                        "to_string",
                        "to_owned",
                        "to_vec",
                        "into_string",
                        "into_bytes",
                        "into_vec",
                        "iter",
                        "into_iter",
                        "keys",
                        "values",
                        "entries",
                        // Additional common names that produce false positives
                        "execute",
                        "handle",
                        "process",
                        "update",
                        "create",
                        "delete",
                        "save",
                        "load",
                        "fetch",
                        "build",
                        "make",
                        "spawn",
                        "fork",
                        "warn",
                        "info",
                        "error",
                        "trace",
                        "debug",
                        "log",
                        "json",
                        "yaml",
                        "toml",
                        "xml",
                        "html",
                        "csv",
                        "Ok",
                        "Err",
                        "Some",
                        "None",
                        "Result",
                        "Option",
                        "Vec",
                        "Box",
                        "Rc",
                        "Arc",
                        "Cell",
                        "RefCell",
                        "Mutex",
                        "RwLock",
                        "String",
                        "str",
                        "Vec",
                        "HashMap",
                        "HashSet",
                        "BTreeMap",
                        "BTreeSet",
                        "VecDeque",
                        "LinkedList",
                        "JsonRpcError",
                        "Error",
                        "Result",
                        "Response",
                        "Request",
                        "Value",
                        "serde",
                        "serde_json",
                        "display",
                        "render",
                    ];
                    let _ = COMMON_NAMES;
                }

                for target_id in targets {
                    queue_cross_file_call_edge(
                        caller_id,
                        target_id,
                        &mut existing_edges,
                        &mut new_edges,
                    );
                }
                if let Some(target_id) = cross_file_type_target(call_target, &indexes) {
                    queue_cross_file_call_edge(
                        caller_id,
                        target_id,
                        &mut existing_edges,
                        &mut new_edges,
                    );
                }
            }
        }
    }

    if !new_edges.is_empty() {
        tracing::debug!(
            "Cross-file call edge resolution: added {} new edges",
            new_edges.len()
        );
        pdg.add_call_edges(new_edges);
    }
}

fn existing_typed_edges(
    pdg: &ProgramDependenceGraph,
) -> HashSet<(
    crate::graph::pdg::NodeId,
    crate::graph::pdg::NodeId,
    EdgeType,
)> {
    pdg.edge_indices()
        .filter_map(|edge_id| {
            let edge = pdg.get_edge(edge_id)?;
            let (from, to) = pdg.edge_endpoints(edge_id)?;
            Some((from, to, edge.edge_type.clone()))
        })
        .collect()
}

/// Resolve source-level value/state channels after all per-file PDGs merge.
///
/// Per-file extraction already emits local flow edges. This pass connects the
/// same facts to definitions in other files, using the existing call-resolution
/// name maps rather than attempting type inference or alias analysis.
// ponytail: syntax/name matching keeps indexing bounded; add type/alias analysis
// only when measured flow misses justify its cost.
pub fn resolve_cross_file_flow_edges_for_files(
    pdg: &mut ProgramDependenceGraph,
    all_signatures: &[(String, SignatureInfo)],
) {
    use crate::graph::pdg::{EdgeType, NodeId};

    let mut by_qname: HashMap<String, Vec<NodeId>> = HashMap::new();
    let mut by_file_qname: HashMap<(String, String), Vec<NodeId>> = HashMap::new();
    let mut by_last: HashMap<String, Vec<NodeId>> = HashMap::new();

    for nid in pdg.node_indices() {
        let Some(node) = pdg.get_node(nid) else {
            continue;
        };
        if node.node_type == NodeType::External {
            continue;
        }
        let Some(qname) = qualified_name_from_node(node) else {
            continue;
        };
        let normalized = normalize_symbol(qname);
        by_qname.entry(normalized.clone()).or_default().push(nid);
        by_file_qname
            .entry((normalized.clone(), node.file_path.to_string()))
            .or_default()
            .push(nid);
        if let Some(last) = normalized.rsplit('.').next() {
            by_last.entry(last.to_string()).or_default().push(nid);
        }
    }

    let mut existing = existing_typed_edges(pdg);

    let mut added = 0usize;
    for (source_file, sig) in all_signatures {
        let normalized_sig = normalize_symbol(&sig.qualified_name);
        let caller_ids = by_file_qname
            .get(&(normalized_sig.clone(), source_file.clone()))
            .cloned()
            .or_else(|| by_qname.get(&normalized_sig).cloned())
            .unwrap_or_default();
        if caller_ids.is_empty() {
            continue;
        }

        for fact in &sig.flow_facts {
            let (edge_type, target_label) = match fact.channel {
                FlowChannel::Argument => (EdgeType::DataDependency, fact.target.as_str()),
                FlowChannel::StateRead | FlowChannel::StateWrite => {
                    (EdgeType::StateTransition, fact.target.as_str())
                }
                FlowChannel::ReturnValue if fact.target != "return" => {
                    (EdgeType::DataDependency, fact.target.as_str())
                }
                _ => continue,
            };
            let targets = resolve_cross_file_flow_targets(target_label, &by_qname, &by_last);
            for caller_id in &caller_ids {
                for target_id in &targets {
                    if caller_id == target_id
                        || !existing.insert((*caller_id, *target_id, edge_type.clone()))
                    {
                        continue;
                    }
                    let mut metadata = EdgeMetadata::with_variable(fact.source.clone());
                    metadata.channel = Some(flow_channel_name(&fact.channel));
                    metadata.position = fact.position;
                    pdg.add_edge(
                        *caller_id,
                        *target_id,
                        Edge {
                            edge_type: edge_type.clone(),
                            metadata,
                        },
                    );
                    added += 1;
                }
            }
        }
    }

    if added > 0 {
        tracing::debug!("Cross-file flow edge resolution: added {added} edges");
    }
}

fn resolve_cross_file_flow_targets(
    target: &str,
    by_qname: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    by_last: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
) -> Vec<crate::graph::pdg::NodeId> {
    let normalized = normalize_symbol(target);
    let mut targets = by_qname.get(&normalized).cloned().unwrap_or_default();
    if targets.is_empty() {
        let segments: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();
        for len in 2..=3_usize.min(segments.len()) {
            let suffix = segments[segments.len() - len..].join(".");
            if let Some(ids) = by_qname.get(&suffix) {
                targets.extend(ids);
            }
        }
    }
    if targets.is_empty() {
        if let Some(last) = normalized.rsplit('.').next() {
            if let Some(ids) = by_last.get(last) {
                targets.extend(ids);
            }
        }
    }
    targets.sort_unstable();
    targets.dedup();
    targets
}
