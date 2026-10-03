// AST → PDG Extraction — Rewrite
//
// Key changes from original:
//   - Type dependency extraction: 3 directional data-flow signals replacing clique generation
//   - Inheritance detection: 4-signal evidence model with confidence scoring
//   - Containment edges: use EdgeType::Containment, not Call
//   - Import parsing: regex-based multi-line handling for all 12 supported languages
//   - All inferred edges carry confidence scores in EdgeMetadata

#![warn(missing_docs)]

use crate::fast_hash::{FastMap as HashMap, FastSet as HashSet};
use crate::graph::pdg::{Edge, EdgeMetadata, EdgeType, Node, NodeType, ProgramDependenceGraph};
use crate::parse::prelude::{FlowChannel, FlowFact, ImportInfo, SignatureInfo};
use regex::Regex;
use std::path::Path;
use std::sync::Arc;

mod import_edges;
pub use import_edges::extract_import_paths_from_source;
use import_edges::*;

type LocalNodeIds = HashMap<String, Vec<crate::graph::pdg::NodeId>>;

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------

/// Extract a PDG from parsed signatures for a single file.
pub fn extract_pdg_from_signatures(
    signatures: Vec<SignatureInfo>,
    source_code: &[u8],
    file_path: &str,
    language: &str,
) -> ProgramDependenceGraph {
    let mut pdg = ProgramDependenceGraph::new();
    let mut node_ids: HashMap<String, crate::graph::pdg::NodeId> = HashMap::default();
    let mut local_node_ids = LocalNodeIds::default();
    let mut seen_qnames = HashSet::default();
    let duplicate_qnames: HashSet<&str> = signatures
        .iter()
        .filter_map(|sig| {
            (!seen_qnames.insert(sig.qualified_name.as_str()))
                .then_some(sig.qualified_name.as_str())
        })
        .collect();

    // Phase 1a: Create function/method nodes
    for sig in &signatures {
        // Import-marker signatures (`return_type == "use"`) exist so callers
        // can discover a file's imports from the parser API; they are NOT
        // code symbols. Creating PDG nodes for them indexed every `use`
        // path segment as a searchable "function" — `Arc`, `Lazy`, `*` and
        // bare module names — polluting search ranking, grep output, and
        // git-status enrichment. They carry no calls, parameters, or
        // imports, so no edge phase references their node ids; skipping
        // them here is safe and simply never materializes the noise.
        if sig.return_type.as_deref() == Some("use") {
            continue;
        }
        // Defensive name guard: a symbol with a blank or glob-star name is
        // never a navigable definition (observed from macro-generated and
        // re-export extractions across parsers); indexing it produces
        // blank bullets in enrichment output and unaddressable nodes.
        let trimmed_name = sig.name.trim();
        if trimmed_name.is_empty() || trimmed_name == "*" {
            continue;
        }
        let mut node = signature_to_node(sig, file_path, language);
        if duplicate_qnames.contains(sig.qualified_name.as_str()) {
            node.id = format!(
                "{}:{}@{}..{}",
                file_path, sig.qualified_name, sig.byte_range.0, sig.byte_range.1
            );
        }
        let nid = pdg.add_node(node);
        local_node_ids
            .entry(sig.qualified_name.clone())
            .or_default()
            .push(nid);
        // Use or_insert to retain the FIRST insertion (more intuitive than
        // last-wins). Operations using node_ids (inheritance, source-level
        // flow, import/class inference) see the first-inserted duplicate.
        node_ids.entry(sig.qualified_name.clone()).or_insert(nid);
    }

    // Phase 1b: Infer Class nodes from method qualified names.
    //           Add CONTAINMENT edges (Class → Method), not Call edges.
    let containment = infer_class_nodes_and_containment(
        &signatures,
        &mut pdg,
        &mut node_ids,
        &local_node_ids,
        file_path,
        language,
    );
    pdg.add_containment_edges(containment);

    // Phase 2: Type-based data flow edges (multi-signal, directional)
    let data_edges = extract_data_flow_edges_for_nodes(&signatures, &local_node_ids);
    pdg.add_data_flow_edges(data_edges);

    // Phase 3: Inheritance edges (4-signal evidence model)
    let inheritance = extract_inheritance_edges(&signatures, &node_ids);
    pdg.add_inheritance_edges(inheritance);

    // Phase 4: Explicit call edges from parser, plus one shared External
    // node per distinct std/external call target so relationship renders can
    // show `String.truncate [external]` instead of guessing a project
    // namesake (N-03).
    let (call_edges, external_calls) =
        extract_call_edges_and_externals(&signatures, &local_node_ids);
    pdg.add_call_edges(call_edges);
    for (caller_id, external_target) in external_calls {
        let external_id = format!("external::{}", external_target);
        let target_id = pdg.find_by_id(&external_id).unwrap_or_else(|| {
            pdg.add_node(Node {
                id: external_id,
                node_type: NodeType::External,
                name: external_target,
                file_path: std::sync::Arc::from(file_path),
                byte_range: (0, 0),
                complexity: 0,
                language: "external".to_string(),
            })
        });
        pdg.add_call_edges(vec![(caller_id, target_id)]);
    }

    // Phase 4b: source-level value/state/command channels. These edges are
    // intentionally bounded and additive; ordinary call extraction remains
    // the authoritative control-flow relation.
    extract_flow_edges(&signatures, &node_ids, &mut pdg);

    // Phase 5: Import edges with multi-line source fallback
    let import_edges = extract_import_edges(
        &signatures,
        &node_ids,
        &mut pdg,
        file_path,
        language,
        source_code,
    );
    pdg.add_import_edges(import_edges);

    pdg
}

// ---------------------------------------------------------------------------
// Phase 1b: Class node inference + containment edges
// ---------------------------------------------------------------------------

fn infer_class_nodes_and_containment(
    signatures: &[SignatureInfo],
    pdg: &mut ProgramDependenceGraph,
    node_ids: &mut HashMap<String, crate::graph::pdg::NodeId>,
    local_node_ids: &LocalNodeIds,
    file_path: &str,
    language: &str,
) -> Vec<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)> {
    let mut class_methods: HashMap<String, HashSet<crate::graph::pdg::NodeId>> = HashMap::default();

    for sig in signatures {
        if !sig.is_method {
            continue;
        }
        let normalized = normalize_symbol(&sig.qualified_name);
        if let Some(dot_pos) = normalized.rfind('.') {
            let class_prefix = normalized[..dot_pos].to_string();
            if let Some(method_ids) = local_node_ids.get(&sig.qualified_name) {
                class_methods
                    .entry(class_prefix)
                    .or_default()
                    .extend(method_ids);
            }
        }
    }

    let mut containment = Vec::new();

    // Iterate in sorted class-name order so inferred inheritance/containment
    // edges (and any node creation) are deterministic across runs.
    let mut class_names: Vec<&String> = class_methods.keys().collect();
    class_names.sort();
    for class_name in class_names {
        let method_nids = &class_methods[class_name];
        let already_exists = node_ids.contains_key(class_name)
            || node_ids.keys().any(|k| normalize_symbol(k) == *class_name);

        if already_exists {
            // Still wire containment edges to existing class node
            if let Some(&class_nid) = node_ids.get(class_name).or_else(|| {
                node_ids
                    .iter()
                    .find(|(k, _)| normalize_symbol(k) == *class_name)
                    .map(|(_, v)| v)
            }) {
                for &mnid in method_nids {
                    containment.push((class_nid, mnid));
                }
            }
            continue;
        }

        let (min_start, max_end) = method_nids.iter().fold((usize::MAX, 0), |(mn, mx), &mnid| {
            pdg.get_node(mnid)
                .map(|n| (mn.min(n.byte_range.0), mx.max(n.byte_range.1)))
                .unwrap_or((mn, mx))
        });

        let short_name = class_name
            .rsplit('.')
            .next()
            .unwrap_or(class_name)
            .to_string();

        // Sum the complexities of all member methods instead of just counting them
        let class_complexity: u32 = method_nids
            .iter()
            .filter_map(|&mnid| pdg.get_node(mnid))
            .map(|node| node.complexity)
            .sum();

        let class_node = Node {
            id: format!("{}:{}", file_path, class_name),
            node_type: NodeType::Class,
            name: short_name,
            file_path: Arc::from(file_path),
            byte_range: (
                if min_start == usize::MAX {
                    0
                } else {
                    min_start
                },
                max_end,
            ),
            complexity: if class_complexity > 0 {
                class_complexity
            } else {
                method_nids.len() as u32
            },
            language: language.to_string(),
        };
        let class_nid = pdg.add_node(class_node);
        node_ids.insert(class_name.clone(), class_nid);

        for &mnid in method_nids {
            containment.push((class_nid, mnid));
        }
    }

    containment
}

// ---------------------------------------------------------------------------
// Phase 2: Data flow edges — multi-signal, directional
//
// Three signals, each with a distinct confidence level:
//
// Signal A (confidence 0.85): Return type of A matches a parameter type of B.
//   "A produces T, B consumes T" — directional, semantically strong.
//   Edge direction: A → B
//
// Signal B (confidence 0.65): Return type of A matches return type of B AND A
//   calls B (or vice versa). This captures pipeline patterns: functions that
//   produce and return the same type as part of a transform chain.
//   Edge direction: caller → callee (already captured by Call edge; this adds
//   a DataDependency annotation with the shared type)
//
// Signal C (confidence 0.45): A and B share a parameter type AND one calls
//   the other. Shared type alone is noise; with an explicit call relationship
//   it suggests data is passed along the call.
//   Edge direction: caller → callee
//
// All signals:
//   - Skip primitive/universal types (str, String, int, bool, void, None, etc.)
//   - Produce directed edges, never bidirectional cliques
//   - Carry variable_name = the shared type name for traceability
// ---------------------------------------------------------------------------

/// Types too common to serve as meaningful data flow signals.
/// Extend this list if false positives appear for domain-specific ubiquitous types.
const EXCLUDED_TYPES: &[&str] = &[
    "str",
    "string",
    "String",
    "&str",
    "int",
    "i32",
    "i64",
    "u32",
    "u64",
    "usize",
    "f32",
    "f64",
    "bool",
    "void",
    "None",
    "null",
    "undefined",
    "any",
    "Any",
    "object",
    "Object",
    "self",
    "Self",
    "cls",
    "this",
    "bytes",
    "Bytes",
    "Vec",
    "List",
    "list",
    "dict",
    "Dict",
    "HashMap",
    "Option",
    "Result",
    "Error",
    "Exception",
    "T",
    "U",
    "K",
    "V",
];

fn is_excluded_type(t: &str) -> bool {
    // Strip generic brackets: "Vec<User>" → check "Vec" (excluded) and "User" (not excluded)
    let base = t.split('<').next().unwrap_or(t).trim();
    EXCLUDED_TYPES.contains(&base)
}

type DataFlowEdge = (
    crate::graph::pdg::NodeId,
    crate::graph::pdg::NodeId,
    String,
    f32,
);
type DataFlowSeen = HashSet<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)>;

struct DataFlowIndexes<'a> {
    producers: HashMap<String, Vec<&'a SignatureInfo>>,
    consumers: HashMap<String, Vec<&'a SignatureInfo>>,
    call_set: HashMap<String, HashSet<String>>,
    by_normalized_name: HashMap<String, Vec<&'a SignatureInfo>>,
}

/// Extracts data flow edges using a 3-signal directional model.
///
/// This function implements a sophisticated data flow analysis that creates
/// semantic edges between functions based on type relationships. It uses
/// three signals with decreasing confidence levels:
///
/// - **Signal A (0.85 confidence)**: Return-to-parameter flow. When a function
///   returns a type that another function accepts as a parameter, a high-confidence
///   data dependency edge is created.
///
/// - **Signal B (0.65 confidence)**: Shared return type with call relationship.
///   When two functions return the same type AND one calls the other, a
///   medium-confidence edge is created.
///
/// - **Signal C (0.45 confidence)**: Shared parameter type with call relationship.
///   When two functions accept the same type as a parameter AND one calls the other,
///   a lower-confidence edge is created.
///
/// The function filters out ubiquitous types (String, i32, bool, etc.) to avoid
/// creating meaningless O(n²) cliques that would dominate the graph.
///
/// # Arguments
///
/// * `signatures` - A slice of function signature information extracted from the codebase
/// * `node_ids` - A mapping from symbol IDs to PDG node IDs
///
/// # Returns
///
/// A vector of tuples containing (source_node, target_node, variable_name, confidence)
/// representing the extracted data flow edges with their associated metadata.
pub fn extract_data_flow_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::graph::pdg::NodeId>,
) -> Vec<(
    crate::graph::pdg::NodeId,
    crate::graph::pdg::NodeId,
    String,
    f32,
)> {
    let local_node_ids = node_ids
        .iter()
        .map(|(name, &id)| (name.clone(), vec![id]))
        .collect();
    extract_data_flow_edges_for_nodes(signatures, &local_node_ids)
}

fn extract_data_flow_edges_for_nodes(
    signatures: &[SignatureInfo],
    node_ids: &LocalNodeIds,
) -> Vec<DataFlowEdge> {
    let indexes = build_data_flow_indexes(signatures);
    let mut edges = Vec::new();
    let mut seen = HashSet::default();

    add_return_to_parameter_edges(&indexes, node_ids, &mut edges, &mut seen);
    add_shared_return_call_edges(&indexes, node_ids, &mut edges, &mut seen);
    add_shared_parameter_call_edges(signatures, &indexes, node_ids, &mut edges, &mut seen);

    edges
}

fn build_data_flow_indexes(signatures: &[SignatureInfo]) -> DataFlowIndexes<'_> {
    let mut indexes = DataFlowIndexes {
        producers: HashMap::default(),
        consumers: HashMap::default(),
        call_set: HashMap::default(),
        by_normalized_name: HashMap::default(),
    };

    for sig in signatures {
        if let Some(ret) = &sig.return_type {
            let norm = normalize_type_name(ret);
            if !norm.is_empty() && !is_excluded_type(&norm) {
                indexes.producers.entry(norm).or_default().push(sig);
            }
        }
        for param in &sig.parameters {
            if let Some(t) = &param.type_annotation {
                let norm = normalize_type_name(t);
                if !norm.is_empty() && !is_excluded_type(&norm) {
                    indexes.consumers.entry(norm).or_default().push(sig);
                }
            }
        }
        let calls = sig.calls.iter().map(|c| normalize_symbol(c)).collect();
        indexes
            .call_set
            .insert(normalize_symbol(&sig.qualified_name), calls);
        indexes
            .by_normalized_name
            .entry(normalize_symbol(&sig.qualified_name))
            .or_default()
            .push(sig);
    }

    indexes
}

fn add_return_to_parameter_edges(
    indexes: &DataFlowIndexes<'_>,
    node_ids: &LocalNodeIds,
    edges: &mut Vec<DataFlowEdge>,
    seen: &mut DataFlowSeen,
) {
    for (type_name, producer_sigs) in &indexes.producers {
        if let Some(consumer_sigs) = indexes.consumers.get(type_name) {
            for prod in producer_sigs {
                for cons in consumer_sigs {
                    if prod.qualified_name == cons.qualified_name {
                        continue;
                    }
                    let (Some(from_ids), Some(to_ids)) = (
                        node_ids.get(&prod.qualified_name),
                        node_ids.get(&cons.qualified_name),
                    ) else {
                        continue;
                    };
                    for &from in from_ids {
                        for &to in to_ids {
                            if seen.insert((from, to)) {
                                edges.push((from, to, type_name.clone(), 0.85));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn add_shared_return_call_edges(
    indexes: &DataFlowIndexes<'_>,
    node_ids: &LocalNodeIds,
    edges: &mut Vec<DataFlowEdge>,
    seen: &mut DataFlowSeen,
) {
    for (type_name, ret_sigs) in &indexes.producers {
        if ret_sigs.len() < 2 {
            continue;
        }
        for i in 0..ret_sigs.len() {
            for j in 0..ret_sigs.len() {
                if i == j {
                    continue;
                }
                let a = ret_sigs[i];
                let b = ret_sigs[j];
                let a_norm = normalize_symbol(&a.qualified_name);
                let b_norm = normalize_symbol(&b.qualified_name);
                let a_calls_b = indexes
                    .call_set
                    .get(&a_norm)
                    .map(|s| s.contains(&b_norm))
                    .unwrap_or(false);
                if a_calls_b {
                    let (Some(from_ids), Some(to_ids)) = (
                        node_ids.get(&a.qualified_name),
                        node_ids.get(&b.qualified_name),
                    ) else {
                        continue;
                    };
                    for &from in from_ids {
                        for &to in to_ids {
                            if seen.insert((from, to)) {
                                edges.push((from, to, format!("ret:{}", type_name), 0.65));
                            }
                        }
                    }
                }
            }
        }
    }
}

fn add_shared_parameter_call_edges(
    signatures: &[SignatureInfo],
    indexes: &DataFlowIndexes<'_>,
    node_ids: &LocalNodeIds,
    edges: &mut Vec<DataFlowEdge>,
    seen: &mut DataFlowSeen,
) {
    for sig_a in signatures {
        let a_norm = normalize_symbol(&sig_a.qualified_name);
        let Some(a_calls) = indexes.call_set.get(&a_norm) else {
            continue;
        };
        for called_norm in a_calls {
            let Some(callee_sigs) = indexes.by_normalized_name.get(called_norm) else {
                continue;
            };
            for sig_b in callee_sigs {
                let a_types: HashSet<String> = sig_a
                    .parameters
                    .iter()
                    .filter_map(|p| p.type_annotation.as_ref())
                    .map(|t| normalize_type_name(t))
                    .filter(|t| !t.is_empty() && !is_excluded_type(t))
                    .collect();
                let b_types: HashSet<String> = sig_b
                    .parameters
                    .iter()
                    .filter_map(|p| p.type_annotation.as_ref())
                    .map(|t| normalize_type_name(t))
                    .filter(|t| !t.is_empty() && !is_excluded_type(t))
                    .collect();
                let mut shared: Vec<&String> = a_types.intersection(&b_types).collect();
                shared.sort();
                if shared.is_empty() {
                    continue;
                }
                let (Some(from_ids), Some(to_ids)) = (
                    node_ids.get(&sig_a.qualified_name),
                    node_ids.get(&sig_b.qualified_name),
                ) else {
                    continue;
                };
                for &from in from_ids {
                    for &to in to_ids {
                        if seen.insert((from, to)) {
                            edges.push((from, to, format!("param:{}", shared[0]), 0.45));
                        }
                    }
                }
            }
        }
    }
}

/// Normalize a type annotation for matching.
/// "Vec<User>" → "User", "&User" → "User", "Option<User>" → "User"
fn normalize_type_name(raw: &str) -> String {
    let stripped = raw
        .trim()
        .trim_start_matches('&')
        .trim_start_matches("mut ")
        .trim();

    // Extract inner type from generics: Vec<T>, Option<T>, Result<T, E>
    if let Some(inner_start) = stripped.find('<') {
        let inner = &stripped[inner_start + 1..];
        let inner_end = inner.rfind('>').unwrap_or(inner.len());
        let inner_type = inner[..inner_end].split(',').next().unwrap_or("").trim();
        if !inner_type.is_empty() && !is_excluded_type(inner_type) {
            return inner_type.to_string();
        }
    }

    stripped.to_string()
}

// ---------------------------------------------------------------------------
// Phase 3: Inheritance detection — 4-signal evidence model
//
// Language-agnostic design rationale:
// All 12 supported languages have some form of inheritance/interface
// implementation. Rather than parsing language-specific syntax, we mine
// the information already captured in SignatureInfo:
//   - qualified_name: encodes class membership
//   - calls: encodes what a method calls, including super/parent calls
//   - name: method name enables override detection
//   - parameters/return_type: signature compatibility
//
// The 4 signals:
//
// Signal 1 — Super/parent call (confidence 0.90)
//   If Dog::speak calls super.speak, Animal.speak, Base.speak, or
//   parent.speak, we infer Dog inherits Animal.
//   Implementation: scan calls for patterns matching sibling method names
//   with super/parent/base/this.__class__ prefixes, or an exact match of
//   the same method name under a different class prefix.
//   This is the HIGHEST confidence signal and is language-agnostic because
//   all OOP languages encode super calls in the AST (parsers should capture
//   them in calls).
//
// Signal 2 — Method override count (confidence scales with count)
//   Two classes sharing N methods with identical names and compatible
//   signatures (same param count) suggests one overrides the other.
//   Thresholds:
//     1 shared method + common name (new/init/toString): skip (noise)
//     2 shared methods: confidence 0.45
//     3 shared methods: confidence 0.60
//     4+ shared methods: confidence 0.75
//   Direction heuristic: shorter class name = likely base (abstract classes
//   are often named "Base", "Abstract", "Animal" vs "ConcreteAnimalImpl")
//
// Signal 3 — Naming convention (confidence 0.50)
//   Prefixes/suffixes strongly suggesting abstract base classes:
//     Abstract*, Base*, *Base, *Mixin, *Interface, *Protocol, *Trait,
//     I* (C# convention), *ABC
//   If ClassA has one of these markers and ClassB shares methods,
//   ClassA is likely the parent.
//
// Signal 4 — Qualified name nesting (confidence 0.70)
//   Some languages encode parent class in qualified name:
//     "Outer.Inner::method" suggests Inner is nested in/inherits Outer
//   If Class B's qualified name contains Class A's name as a prefix segment,
//   B likely inherits or is nested within A.
//
// Combination rule:
//   Signals are ORed with the highest applicable confidence used.
//   Any signal reaching >= MIN_INHERITANCE_CONFIDENCE (0.45) produces an edge.
//   This threshold is intentionally permissive; callers using TraversalConfig
//   can filter edges by min_edge_confidence for tighter analysis.
// ---------------------------------------------------------------------------

const MIN_INHERITANCE_CONFIDENCE: f32 = 0.45;

/// Method names so common they don't signal inheritance on their own.
const COMMON_METHOD_NAMES: &[&str] = &[
    "new",
    "init",
    "__init__",
    "constructor",
    "create",
    "build",
    "toString",
    "to_string",
    "__str__",
    "__repr__",
    "equals",
    "__eq__",
    "hashCode",
    "__hash__",
    "clone",
    "__clone__",
    "copy",
    "dispose",
    "close",
    "__del__",
    "finalize",
    "update",
    "get",
    "set",
    "run",
    "start",
    "stop",
    "execute",
];

fn is_common_method(name: &str) -> bool {
    COMMON_METHOD_NAMES.contains(&name)
}

const ABSTRACT_BASE_PREFIXES: &[&str] = &["Abstract", "Base", "I"];
const ABSTRACT_BASE_SUFFIXES: &[&str] = &[
    "Base",
    "Mixin",
    "Interface",
    "Protocol",
    "Trait",
    "ABC",
    "Abstract",
];

fn looks_like_abstract_base(class_name: &str) -> bool {
    ABSTRACT_BASE_PREFIXES
        .iter()
        .any(|p| class_name.starts_with(p) && class_name.len() > p.len())
        || ABSTRACT_BASE_SUFFIXES
            .iter()
            .any(|s| class_name.ends_with(s) && class_name.len() > s.len())
}

#[derive(Debug, Default)]
struct InheritanceEvidence {
    super_call_confidence: f32,
    override_confidence: f32,
    naming_confidence: f32,
    nesting_confidence: f32,
}

impl InheritanceEvidence {
    fn max_confidence(&self) -> f32 {
        self.super_call_confidence
            .max(self.override_confidence)
            .max(self.naming_confidence)
            .max(self.nesting_confidence)
    }
}

fn group_methods_by_class(signatures: &[SignatureInfo]) -> HashMap<String, Vec<&SignatureInfo>> {
    let mut class_methods: HashMap<String, Vec<&SignatureInfo>> = HashMap::default();

    for sig in signatures {
        if !sig.is_method {
            continue;
        }

        let normalized = normalize_symbol(&sig.qualified_name);
        let Some(dot_pos) = normalized.rfind('.') else {
            continue;
        };
        class_methods
            .entry(normalized[..dot_pos].to_string())
            .or_default()
            .push(sig);
    }

    class_methods
}

fn class_method_names<'class, 'sig>(
    class_methods: &'class HashMap<String, Vec<&'sig SignatureInfo>>,
) -> HashMap<&'class str, HashSet<&'sig str>> {
    class_methods
        .iter()
        .map(|(class_name, methods)| {
            (
                class_name.as_str(),
                methods.iter().map(|sig| sig.name.as_str()).collect(),
            )
        })
        .collect()
}

fn method_calls_parent(method: &SignatureInfo, parent_class: &str) -> bool {
    let method_name = &method.name;
    let super_patterns = [
        format!("super.{}", method_name),
        format!("super::{}", method_name),
        format!("parent.{}", method_name),
        format!("Base.{}", method_name),
        format!("{}.{}", parent_class, method_name),
        format!("{}::{}", parent_class, method_name),
    ];

    method.calls.iter().any(|call| {
        let norm_call = normalize_symbol(call);
        super_patterns
            .iter()
            .any(|pat| norm_call.ends_with(&normalize_symbol(pat)))
            || norm_call.starts_with("super.")
            || norm_call.starts_with("super::")
            || norm_call.starts_with("parent.")
    })
}

fn class_calls_parent(methods: &[&SignatureInfo], parent_class: &str) -> bool {
    methods
        .iter()
        .any(|method| method_calls_parent(method, parent_class))
}

fn super_call_confidence(
    methods_a: &[&SignatureInfo],
    methods_b: &[&SignatureInfo],
    cls_a: &str,
    cls_b: &str,
) -> f32 {
    if class_calls_parent(methods_a, cls_b) || class_calls_parent(methods_b, cls_a) {
        0.90
    } else {
        0.0
    }
}

fn override_confidence(shared_count: usize) -> f32 {
    match shared_count {
        0 | 1 => 0.0,
        2 => 0.45,
        3 => 0.60,
        _ => 0.75,
    }
}

fn naming_confidence(cls_a: &str, cls_b: &str, shared_count: usize) -> f32 {
    if (looks_like_abstract_base(cls_a) || looks_like_abstract_base(cls_b)) && shared_count >= 1 {
        0.50
    } else {
        0.0
    }
}

fn is_qualified_class_prefix(prefix: &str, class_name: &str) -> bool {
    class_name.starts_with(prefix)
        && class_name
            .chars()
            .nth(prefix.len())
            .map(|character| character == '.')
            .unwrap_or(false)
}

fn nesting_confidence(cls_a: &str, cls_b: &str) -> f32 {
    if is_qualified_class_prefix(cls_a, cls_b) || is_qualified_class_prefix(cls_b, cls_a) {
        0.70
    } else {
        0.0
    }
}

fn inheritance_evidence(
    cls_a: &str,
    cls_b: &str,
    methods_a: &[&SignatureInfo],
    methods_b: &[&SignatureInfo],
    class_method_names: &HashMap<&str, HashSet<&str>>,
) -> InheritanceEvidence {
    let names_a = class_method_names.get(cls_a).cloned().unwrap_or_default();
    let names_b = class_method_names.get(cls_b).cloned().unwrap_or_default();
    let shared_count = names_a
        .intersection(&names_b)
        .filter(|&&name| !is_common_method(name))
        .count();
    let short_a = cls_a.rsplit('.').next().unwrap_or(cls_a);
    let short_b = cls_b.rsplit('.').next().unwrap_or(cls_b);

    InheritanceEvidence {
        super_call_confidence: super_call_confidence(methods_a, methods_b, cls_a, cls_b),
        override_confidence: override_confidence(shared_count),
        naming_confidence: naming_confidence(short_a, short_b, shared_count),
        nesting_confidence: nesting_confidence(cls_a, cls_b),
    }
}

fn representative_class_node(
    class_name: &str,
    class_methods: &HashMap<String, Vec<&SignatureInfo>>,
    node_ids: &HashMap<String, crate::graph::pdg::NodeId>,
) -> Option<crate::graph::pdg::NodeId> {
    node_ids
        .get(class_name)
        .or_else(|| {
            class_methods
                .get(class_name)
                .and_then(|methods| methods.first())
                .and_then(|sig| node_ids.get(&sig.qualified_name))
        })
        .copied()
}

/// Extracts inheritance edges using a 4-signal evidence model.
///
/// This function identifies inheritance relationships between classes by analyzing
/// multiple signals of evidence, each with a different confidence level:
///
/// - **Super calls (0.90 confidence)**: Explicit calls to `super()` indicate a
///   direct parent-child relationship with high certainty.
///
/// - **Override count (0.45-0.75 confidence)**: When a class overrides methods from
///   another class, confidence increases with the number of overrides:
///   - 1 override: 0.45 confidence
///   - 2 overrides: 0.60 confidence
///   - 3+ overrides: 0.75 confidence
///
/// - **Naming conventions (0.50 confidence)**: Classes with abstract base prefixes
///   (Base, Abstract) or suffixes (Base, Impl, Trait, ABC) suggest inheritance patterns.
///
/// - **Nesting (0.70 confidence)**: Inner classes within another class often indicate
///   a strong containment relationship that may imply inheritance.
///
/// The minimum confidence threshold is set at 0.45 to ensure only meaningful
/// inheritance relationships are captured.
///
/// # Arguments
///
/// * `signatures` - A slice of function signature information containing class data
/// * `node_ids` - A mapping from symbol IDs to PDG node IDs
///
/// # Returns
///
/// A vector of tuples containing (child_node, parent_node, confidence) representing
/// the extracted inheritance edges with their associated confidence scores.
pub fn extract_inheritance_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::graph::pdg::NodeId>,
) -> Vec<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId, f32)> {
    let mut edges = Vec::new();
    let class_methods = group_methods_by_class(signatures);
    let class_names: Vec<&str> = class_methods.keys().map(String::as_str).collect();

    if class_names.len() < 2 {
        return edges;
    }

    let method_names_by_class = class_method_names(&class_methods);

    for (i, &cls_a) in class_names.iter().enumerate() {
        // Starting at i + 1 retains exactly one edge candidate per unordered pair.
        for &cls_b in &class_names[i + 1..] {
            let methods_a = &class_methods[cls_a];
            let methods_b = &class_methods[cls_b];
            let evidence =
                inheritance_evidence(cls_a, cls_b, methods_a, methods_b, &method_names_by_class);
            let confidence = evidence.max_confidence();

            if confidence < MIN_INHERITANCE_CONFIDENCE {
                continue;
            }

            let short_a = cls_a.rsplit('.').next().unwrap_or(cls_a);
            let short_b = cls_b.rsplit('.').next().unwrap_or(cls_b);
            let (child_cls, parent_cls) = determine_inheritance_direction(
                cls_a, cls_b, methods_a, methods_b, &evidence, short_a, short_b,
            );

            let child_nid = representative_class_node(child_cls, &class_methods, node_ids);
            let parent_nid = representative_class_node(parent_cls, &class_methods, node_ids);

            if let (Some(child_id), Some(parent_id)) = (child_nid, parent_nid) {
                edges.push((child_id, parent_id, confidence));
            }
        }
    }

    edges
}

fn determine_inheritance_direction<'a>(
    cls_a: &'a str,
    cls_b: &'a str,
    methods_a: &[&SignatureInfo],
    _methods_b: &[&SignatureInfo],
    evidence: &InheritanceEvidence,
    short_a: &str,
    short_b: &str,
) -> (&'a str, &'a str) {
    // If super_call signal fired, the class making super calls is the child
    if evidence.super_call_confidence > 0.0 {
        let a_calls_super = methods_a.iter().any(|sig| {
            sig.calls.iter().any(|c| {
                let norm = normalize_symbol(c);
                norm.starts_with("super.")
                    || norm.starts_with("super::")
                    || norm.starts_with("parent.")
                    || norm.contains(cls_b)
            })
        });
        if a_calls_super {
            return (cls_a, cls_b);
        }
        return (cls_b, cls_a);
    }

    // Naming convention: abstract base is the parent
    if looks_like_abstract_base(short_a) {
        return (cls_b, cls_a);
    }
    if looks_like_abstract_base(short_b) {
        return (cls_a, cls_b);
    }

    // Qualified name nesting: more nested class is the child
    if cls_b.starts_with(cls_a) {
        return (cls_b, cls_a);
    }
    if cls_a.starts_with(cls_b) {
        return (cls_a, cls_b);
    }

    // Fallback: shorter name = parent (less specific = more abstract)
    if cls_a.len() <= cls_b.len() {
        (cls_b, cls_a)
    } else {
        (cls_a, cls_b)
    }
}

// ---------------------------------------------------------------------------
// Phase 4: Call edge extraction (unchanged logic, cleaned up)
// ---------------------------------------------------------------------------

fn caller_namespace(qualified_name: &str) -> Option<String> {
    let normalized = normalize_symbol(qualified_name);
    let segments: Vec<&str> = normalized.split('.').collect();
    (segments.len() > 1).then(|| segments[..segments.len() - 1].join("."))
}

fn ordered_resolution_candidates(
    call_target: &str,
    alias_map: &HashMap<String, String>,
    caller_ns: Option<&str>,
) -> Vec<String> {
    let mut candidates = vec![call_target.to_string()];
    let normalized = normalize_symbol(call_target);
    let call_segments: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();

    if let Some(first) = call_segments.first() {
        if let Some(import_path) = alias_map.get(*first) {
            let alias_target = if call_segments.len() == 1 {
                import_path.clone()
            } else {
                format!("{}.{}", import_path, call_segments[1..].join("."))
            };
            candidates.push(alias_target);
        }

        if let Some(namespace) = caller_ns {
            if matches!(
                *first,
                "self" | "this" | "super" | "Self" | "crate" | "base"
            ) {
                let rest = call_segments[1..].join(".");
                if !rest.is_empty() {
                    candidates.push(format!("{}.{}", namespace, rest));
                }
            } else if call_segments.len() == 1 {
                candidates.push(format!("{}.{}", namespace, first));
            }
        }
    }

    candidates
}

fn local_call_targets(
    candidates: &[String],
    exact_map: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    last_map: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    suffix_map: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
) -> Vec<crate::graph::pdg::NodeId> {
    let mut targets = Vec::new();

    for candidate in candidates {
        let normalized = normalize_symbol(candidate);
        let segments: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();

        // Exact qualified-name matches are always trusted (multiple ids can
        // legitimately exist for the same qualified name across files).
        if let Some(ids) = exact_map.get(&normalized) {
            targets.extend(ids);
        }
        // Fuzzy fallbacks (bare last segment, 2-3 segment suffix) resolve to
        // a project namesake in ANOTHER file. When the same short name is
        // defined in several files — the common case for utility names like
        // `truncate` — linking the call to ALL of them merged every
        // namesake's relationships into one conflation blob (N-04), and a
        // std/external call (`String::truncate`) linked to whichever
        // project namesake shared the method name (N-03). These fallbacks
        // now only fire when the name is UNAMBIGUOUS project-wide; an
        // ambiguous short name resolves to none of the namesakes.
        if let Some(last) = segments.last() {
            if let Some(ids) = last_map.get(*last) {
                if ids.len() == 1 {
                    targets.extend(ids);
                }
            }
        }
        for len in 2..=3_usize.min(segments.len()) {
            let start = segments.len() - len;
            let suffix = segments[start..].join(".");
            if let Some(ids) = suffix_map.get(&suffix) {
                if ids.len() == 1 {
                    targets.extend(ids);
                }
            }
        }
    }

    targets.sort_unstable();
    targets.dedup();
    targets
}

/// Call targets that are standard-library/external and must never resolve to
/// project namesakes (N-03): explicit `std`/`core`/`alloc` namespaces plus
/// the common Rust prelude/container types whose methods (`String::truncate`,
/// `Vec::push`, `Option::unwrap`, …) routinely collide with short project
/// symbol names.
fn is_external_call_target(call_target: &str) -> bool {
    let normalized = normalize_symbol(call_target);
    let first = normalized.split('.').next().unwrap_or("");
    matches!(
        first,
        "std"
            | "core"
            | "alloc"
            | "String"
            | "str"
            | "Vec"
            | "Option"
            | "Result"
            | "Box"
            | "Arc"
            | "Rc"
            | "Cell"
            | "RefCell"
            | "HashMap"
            | "BTreeMap"
            | "HashSet"
            | "BTreeSet"
    ) || call_target.starts_with("std::")
        || call_target.starts_with("core::")
        || call_target.starts_with("alloc::")
}

fn type_node_target(
    call_target: &str,
    node_ids: &LocalNodeIds,
    last_map: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
) -> Option<crate::graph::pdg::NodeId> {
    let callee_name = normalize_symbol(call_target);
    let (scoped_prefix, _) = callee_name.rsplit_once('.')?;
    let bare_type = scoped_prefix.rsplit('.').next().unwrap_or(scoped_prefix);

    if !bare_type.chars().next().is_some_and(|c| c.is_uppercase()) {
        return None;
    }

    node_ids
        .get(scoped_prefix)
        .and_then(|ids| ids.first())
        .or_else(|| node_ids.get(bare_type).and_then(|ids| ids.first()))
        .or_else(|| last_map.get(bare_type).and_then(|ids| ids.first()))
        .copied()
}

/// Extracts call edges from function signatures.
///
/// This function analyzes function signatures to identify call relationships
/// between functions. It builds resolution maps to efficiently match callees
/// and creates edges in the PDG representing the call graph.
///
/// The function deduplicates edges to avoid creating multiple edges between
/// the same pair of functions.
///
/// # Arguments
///
/// * `signatures` - A slice of function signature information containing call data
/// * `node_ids` - A mapping from symbol IDs to PDG node IDs
///
/// # Returns
///
/// A vector of tuples containing (caller_node, callee_node) representing
/// the extracted call graph edges.
pub fn extract_call_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::graph::pdg::NodeId>,
) -> Vec<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)> {
    let local_node_ids = node_ids
        .iter()
        .map(|(name, &id)| (name.clone(), vec![id]))
        .collect();
    extract_call_edges_for_nodes(signatures, &local_node_ids)
}

fn extract_call_edges_for_nodes(
    signatures: &[SignatureInfo],
    node_ids: &LocalNodeIds,
) -> Vec<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)> {
    extract_call_edges_and_externals(signatures, node_ids).0
}

struct SymbolResolutionMaps {
    exact_map: HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    last_map: HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    suffix_map: HashMap<String, Vec<crate::graph::pdg::NodeId>>,
}

fn build_symbol_resolution_maps(
    signatures: &[SignatureInfo],
    node_ids: &LocalNodeIds,
) -> SymbolResolutionMaps {
    let mut exact_map: HashMap<String, Vec<crate::graph::pdg::NodeId>> = HashMap::default();
    let mut last_map: HashMap<String, Vec<crate::graph::pdg::NodeId>> = HashMap::default();
    let mut suffix_map: HashMap<String, Vec<crate::graph::pdg::NodeId>> = HashMap::default();

    for signature in signatures {
        if let Some(ids) = node_ids.get(&signature.qualified_name) {
            let normalized = normalize_symbol(&signature.qualified_name);
            let segments: Vec<&str> = normalized.split('.').filter(|s| !s.is_empty()).collect();

            exact_map.entry(normalized.clone()).or_default().extend(ids);
            if let Some(last) = segments.last() {
                last_map.entry((*last).to_string()).or_default().extend(ids);
            }
            for len in 2..=3_usize.min(segments.len()) {
                let start = segments.len() - len;
                suffix_map
                    .entry(segments[start..].join("."))
                    .or_default()
                    .extend(ids);
            }
        }
    }

    SymbolResolutionMaps {
        exact_map,
        last_map,
        suffix_map,
    }
}

struct CallCollector {
    edges: Vec<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)>,
    external_calls: Vec<(crate::graph::pdg::NodeId, String)>,
    seen: HashSet<(crate::graph::pdg::NodeId, crate::graph::pdg::NodeId)>,
    seen_external: HashSet<(crate::graph::pdg::NodeId, String)>,
}

impl CallCollector {
    fn new() -> Self {
        Self {
            edges: Vec::new(),
            external_calls: Vec::new(),
            seen: HashSet::default(),
            seen_external: HashSet::default(),
        }
    }

    fn record_external(&mut self, caller_id: crate::graph::pdg::NodeId, call_target: &str) {
        let normalized = normalize_symbol(call_target);
        if self.seen_external.insert((caller_id, normalized.clone())) {
            self.external_calls.push((caller_id, normalized));
        }
    }

    fn record_edge(
        &mut self,
        caller_id: crate::graph::pdg::NodeId,
        target_id: crate::graph::pdg::NodeId,
    ) {
        if caller_id != target_id && self.seen.insert((caller_id, target_id)) {
            self.edges.push((caller_id, target_id));
        }
    }
}

fn resolve_call_target(
    caller_id: crate::graph::pdg::NodeId,
    call_target: &str,
    alias_map: &HashMap<String, String>,
    caller_ns: Option<&str>,
    maps: &SymbolResolutionMaps,
    node_ids: &LocalNodeIds,
    collector: &mut CallCollector,
) {
    if is_external_call_target(call_target) {
        // External call: record one marker per distinct target
        // for this caller; never resolve it into project nodes.
        collector.record_external(caller_id, call_target);
        return;
    }
    let candidates = ordered_resolution_candidates(call_target, alias_map, caller_ns);

    for target_id in local_call_targets(
        &candidates,
        &maps.exact_map,
        &maps.last_map,
        &maps.suffix_map,
    ) {
        collector.record_edge(caller_id, target_id);
    }

    if let Some(target_id) = type_node_target(call_target, node_ids, &maps.last_map) {
        collector.record_edge(caller_id, target_id);
    }
}

/// A resolved call edge: (caller node, callee node).
type CallEdge = (crate::graph::pdg::NodeId, crate::graph::pdg::NodeId);

/// Extract call edges plus the set of distinct external call targets made by
/// each caller. External targets (`String::truncate`, `Vec::push`, …) are
/// deliberately NOT resolved to project symbols (N-03); the caller links
/// them to one shared External node per distinct target so relationship
/// renders can show `String.truncate [external]` instead of guessing a
/// project namesake (N-03/N-04).
fn extract_call_edges_and_externals(
    signatures: &[SignatureInfo],
    node_ids: &LocalNodeIds,
) -> (Vec<CallEdge>, Vec<(crate::graph::pdg::NodeId, String)>) {
    let maps = build_symbol_resolution_maps(signatures, node_ids);
    let mut collector = CallCollector::new();

    for signature in signatures {
        let Some(caller_ids) = node_ids.get(&signature.qualified_name) else {
            continue;
        };
        let alias_map = import_alias_map(&signature.imports);
        let caller_ns = caller_namespace(&signature.qualified_name);

        for &caller_id in caller_ids {
            for call_target in &signature.calls {
                resolve_call_target(
                    caller_id,
                    call_target,
                    &alias_map,
                    caller_ns.as_deref(),
                    &maps,
                    node_ids,
                    &mut collector,
                );
            }
        }
    }

    (collector.edges, collector.external_calls)
}

fn add_local_flow_fact_edge(
    fact: &FlowFact,
    channel: &str,
    caller_id: crate::graph::pdg::NodeId,
    by_normalized: &HashMap<String, crate::graph::pdg::NodeId>,
    by_last: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
    pdg: &mut ProgramDependenceGraph,
) {
    let Some(target) = resolve_flow_target(&fact.target, by_normalized, by_last) else {
        return;
    };
    if target == caller_id {
        return;
    }

    let mut metadata = EdgeMetadata::with_variable(fact.source.clone());
    metadata.channel = Some(channel.to_string());
    metadata.position = fact.position;
    pdg.add_edge(
        caller_id,
        target,
        Edge {
            edge_type: EdgeType::DataDependency,
            metadata,
        },
    );
}

/// Add explicit source-level flow facts to a per-file PDG.
///
/// Facts are resolved locally by qualified/last symbol name. Unresolved
/// command and state labels become lightweight external nodes so tools can
/// still explain argv/env/stdin and registry/verification channels without
/// hydrating unrelated project state.
fn extract_flow_edges(
    signatures: &[SignatureInfo],
    node_ids: &HashMap<String, crate::graph::pdg::NodeId>,
    pdg: &mut ProgramDependenceGraph,
) {
    let mut by_normalized: HashMap<String, crate::graph::pdg::NodeId> = HashMap::default();
    let mut by_last: HashMap<String, Vec<crate::graph::pdg::NodeId>> = HashMap::default();
    for sig in signatures {
        if let Some(&id) = node_ids.get(&sig.qualified_name) {
            by_normalized.insert(normalize_symbol(&sig.qualified_name), id);
            if let Some(last) = normalize_symbol(&sig.qualified_name).rsplit('.').next() {
                by_last.entry(last.to_string()).or_default().push(id);
            }
        }
    }

    let mut external: HashMap<String, crate::graph::pdg::NodeId> = HashMap::default();
    for sig in signatures {
        let Some(&caller_id) = node_ids.get(&sig.qualified_name) else {
            continue;
        };
        let command = sig
            .flow_facts
            .iter()
            .find(|fact| fact.channel == FlowChannel::CommandArgument && fact.target == "command")
            .map(|fact| fact.source.clone());

        for fact in &sig.flow_facts {
            let (edge_type, target_label, channel) = match &fact.channel {
                FlowChannel::Argument | FlowChannel::ReturnValue => {
                    // Argument and return facts only connect symbols in this file.
                    add_local_flow_fact_edge(
                        fact,
                        &flow_channel_name(&fact.channel),
                        caller_id,
                        &by_normalized,
                        &by_last,
                        pdg,
                    );
                    continue;
                }
                FlowChannel::StateRead | FlowChannel::StateWrite => {
                    let target = resolve_flow_target(&fact.target, &by_normalized, &by_last);
                    let target_label = target
                        .and_then(|id| pdg.get_node(id).map(|node| node.name.to_string()))
                        .unwrap_or_else(|| fact.target.clone());
                    (
                        EdgeType::StateTransition,
                        target_label,
                        flow_channel_name(&fact.channel),
                    )
                }
                FlowChannel::CommandArgument => {
                    if fact.target == "command" {
                        continue;
                    }
                    let label = match fact.target.as_str() {
                        "argv" => command.clone().unwrap_or_else(|| fact.source.clone()),
                        "env" => fact.source.clone(),
                        _ => fact.target.clone(),
                    };
                    let edge_type = match fact.target.as_str() {
                        "env" => EdgeType::Environment,
                        "stdin" => EdgeType::Stdin,
                        _ => EdgeType::CommandArgument,
                    };
                    (edge_type, label, fact.target.clone())
                }
                FlowChannel::Environment => (
                    EdgeType::Environment,
                    fact.target.clone(),
                    "env".to_string(),
                ),
                FlowChannel::Stdin => (EdgeType::Stdin, fact.target.clone(), "stdin".to_string()),
            };

            let target_id =
                if let Some(id) = resolve_flow_target(&target_label, &by_normalized, &by_last) {
                    id
                } else {
                    *external.entry(target_label.clone()).or_insert_with(|| {
                        pdg.add_node(Node {
                            id: format!("external:{}", target_label),
                            node_type: NodeType::External,
                            name: target_label.clone(),
                            file_path: Arc::from("<external>"),
                            byte_range: (0, 0),
                            complexity: 0,
                            language: "external".to_string(),
                        })
                    })
                };
            let mut metadata = EdgeMetadata::with_variable(fact.source.clone());
            metadata.channel = Some(channel);
            metadata.position = fact.position;
            pdg.add_edge(
                caller_id,
                target_id,
                Edge {
                    edge_type,
                    metadata,
                },
            );
        }
    }
}

fn resolve_flow_target(
    target: &str,
    by_normalized: &HashMap<String, crate::graph::pdg::NodeId>,
    by_last: &HashMap<String, Vec<crate::graph::pdg::NodeId>>,
) -> Option<crate::graph::pdg::NodeId> {
    let normalized = normalize_symbol(target);
    by_normalized.get(&normalized).copied().or_else(|| {
        normalized
            .rsplit('.')
            .next()
            .and_then(|last| by_last.get(last)?.first().copied())
    })
}

fn flow_channel_name(channel: &FlowChannel) -> String {
    match channel {
        FlowChannel::StateRead => "state_read",
        FlowChannel::StateWrite => "state_write",
        FlowChannel::Argument => "argument",
        FlowChannel::ReturnValue => "return_value",
        FlowChannel::CommandArgument => "argv",
        FlowChannel::Environment => "env",
        FlowChannel::Stdin => "stdin",
    }
    .to_string()
}

#[path = "extraction_cross_file.rs"]
mod cross_file;

pub use cross_file::{
    resolve_cross_file_call_edges, resolve_cross_file_call_edges_for_files,
    resolve_cross_file_flow_edges_for_files,
};

fn qualified_name_from_node(node: &Node) -> Option<&str> {
    if let Some(qname) = node
        .id
        .strip_prefix(node.file_path.as_ref())
        .and_then(|rest| rest.strip_prefix(':'))
    {
        return Some(qname.split_once('@').map_or(qname, |(qname, _)| qname));
    }

    let expected_file_name = Path::new(node.file_path.as_ref()).file_name()?.to_str()?;
    node.id.char_indices().find_map(|(idx, ch)| {
        if ch != ':' {
            return None;
        }
        let (id_path, rest) = node.id.split_at(idx);
        let rest = rest.strip_prefix(':')?;
        if rest.is_empty() {
            return None;
        }
        let id_file_name = Path::new(id_path).file_name()?.to_str()?;
        (id_file_name == expected_file_name)
            .then(|| rest.split_once('@').map_or(rest, |(qname, _)| qname))
    })
}

fn import_alias_map(imports: &[ImportInfo]) -> HashMap<String, String> {
    let mut alias_map = HashMap::default();
    for import in imports {
        let alias = import.alias.clone().or_else(|| {
            import
                .path
                .split(['.', ':', '/', '\\'])
                .next_back()
                .map(|s| s.to_string())
        });
        if let Some(alias) = alias {
            alias_map
                .entry(alias)
                .or_insert_with(|| import.path.clone());
        }
    }
    alias_map
}

// ---------------------------------------------------------------------------
// Symbol normalization
// ---------------------------------------------------------------------------

/// Normalizes a symbol name for consistent lookup and comparison.
///
/// This function converts various language-specific symbol separators into
/// a unified dot notation. It performs the following transformations:
///
/// - Strips function arguments (everything after `(`)
/// - Replaces optional chaining (`?.`) with `.`
/// - Replaces namespace separators (`::`) with `.`
/// - Replaces arrow notation (`->`) with `.`
/// - Replaces backslashes (`\`) with `.`
/// - Replaces forward slashes (`/`) with `.`
///
/// # Arguments
///
/// * `raw` - The raw symbol name as extracted from source code
///
/// # Returns
///
/// A normalized symbol string using dot notation for all separators.
///
/// # Examples
///
/// - `std::io::Read` → `std.io.Read`
/// - `obj?.property` → `obj.property`
/// - `module/function` → `module.function`
pub fn normalize_symbol(raw: &str) -> String {
    let trimmed = raw.split('(').next().unwrap_or(raw).trim();
    // One pass instead of five chained `replace` calls (five allocations and
    // five scans per call, on a function that runs for every call target and
    // every comparison during cross-file resolution -- about a tenth of all
    // indexing CPU). The rewrites are applied left to right exactly as the
    // chain did: `?.`, `::` and `->` become `.`, then each remaining `\`, `/`
    // and `:` becomes `.`.
    let mut out = String::with_capacity(trimmed.len());
    let mut chars = trimmed.chars().peekable();
    while let Some(ch) = chars.next() {
        match (ch, chars.peek().copied()) {
            ('?', Some('.')) | (':', Some(':')) | ('-', Some('>')) => {
                chars.next();
                out.push('.');
            }
            ('\\' | '/' | ':', _) => out.push('.'),
            _ => out.push(ch),
        }
    }
    // `replace("..", ".")` runs once (not to a fixed point): `...` becomes `..`.
    let collapsed = if out.contains("..") {
        out.replace("..", ".")
    } else {
        out
    };
    collapsed.trim_matches('.').to_string()
}

// ---------------------------------------------------------------------------
// Node construction
// ---------------------------------------------------------------------------

fn signature_to_node(sig: &SignatureInfo, file_path: &str, language: &str) -> Node {
    let node_type = match sig.return_type.as_deref() {
        Some("doc_section") => NodeType::DocSection,
        Some("module") => NodeType::Module,
        Some("enum_variant") => NodeType::Variable,
        Some("enum") | Some("trait") => NodeType::Class,
        Some(value) if value.starts_with("struct") => NodeType::Class,
        _ if sig.is_method => NodeType::Method,
        _ => NodeType::Function,
    };
    let complexity = if sig.cyclomatic_complexity > 0 {
        sig.cyclomatic_complexity
    } else {
        1u32 + sig.parameters.len() as u32
    };
    Node {
        id: format!("{}:{}", file_path, sig.qualified_name),
        node_type,
        name: sig.name.clone(),
        file_path: Arc::from(file_path),
        byte_range: sig.byte_range,
        complexity,
        language: language.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[path = "extraction_test.rs"]
mod tests;

#[cfg(test)]
mod normalize_symbol_test {
    use super::normalize_symbol;

    /// The original chained-`replace` implementation, kept as the reference.
    fn reference(raw: &str) -> String {
        let trimmed = raw.split('(').next().unwrap_or(raw).trim();
        trimmed
            .replace("?.", ".")
            .replace("::", ".")
            .replace("->", ".")
            .replace(['\\', '/', ':'], ".")
            .replace("..", ".")
            .trim_matches('.')
            .to_string()
    }

    #[test]
    fn test_normalize_symbol_matches_the_replace_chain_on_edge_cases() {
        for case in [
            "",
            ".",
            "..",
            "...",
            "a::b",
            "a:::b",
            "a::::b",
            "a:b",
            "a->b",
            "a-->b",
            "a?.b",
            "a?::b",
            "a??.b",
            "std::io::Read",
            "obj?.property",
            "module/function",
            "a\\b/c:d",
            "::a::",
            "->x->",
            "x(y)::z",
            "  pad::name  ",
            "a.b..c...d",
            "é::ü->ñ",
            "?",
            "-",
            "-:>",
            "a?.?.b",
            "path/to/file.rs:Type::method",
        ] {
            assert_eq!(normalize_symbol(case), reference(case), "input {case:?}");
        }
    }

    #[test]
    fn test_normalize_symbol_matches_the_replace_chain_on_generated_strings() {
        // Deterministic pseudo-random strings over the characters that matter.
        let alphabet: Vec<char> = "ab.:/\\-?>( _é".chars().collect();
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        for _ in 0..20_000 {
            let mut input = String::new();
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let mut bits = state;
            for _ in 0..(bits % 11) {
                bits = bits.rotate_left(7) ^ 0x9E37_79B9;
                input.push(alphabet[(bits as usize) % alphabet.len()]);
            }
            assert_eq!(
                normalize_symbol(&input),
                reference(&input),
                "input {input:?}"
            );
        }
    }
}
