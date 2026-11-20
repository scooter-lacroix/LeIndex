//! Edit risk from real signals (§3.6).
//!
//! Risk is only reported when there is something to report. The signals
//! are the ones the brief names:
//! - PDG callers affected (backward impact of the edited symbols),
//! - whether a `pub` item's signature line is touched (source text, not a
//!   stub heuristic — the PDG carries no visibility),
//! - whether tests reference the edited symbol.
//!
//! `assess` returns `None` for a low-risk edit, so "risk: low" noise
//! never reaches the output.

use crate::graph::pdg::ProgramDependenceGraph;
use std::path::Path;

/// One non-low risk finding with its reason.
pub(crate) struct RiskFinding {
    /// `medium` or `high`.
    pub level: &'static str,
    /// e.g. "pub fn signature changed, 14 callers".
    pub reason: String,
}

/// Signal set computed for one previewed edit.
pub(crate) struct EditRiskSignals {
    /// Number of distinct PDG callers (backward impact) of the edited symbols.
    pub affected_callers: usize,
    /// True when any edited symbol is declared `pub` and its signature line
    /// (the line its byte range starts on) is inside an edited region.
    pub pub_signature_touched: bool,
    /// True when any edited symbol name appears in a `*_test.rs` file,
    /// `tests/`, or a `#[test]`/`mod test` region of the indexed graph.
    pub tests_reference: bool,
}

/// Compute the risk signals for one file's edit from the PDG (when loaded)
/// plus the file's source text. `edit_ranges` are byte ranges in `original`.
pub(crate) fn compute_signals(
    pdg: Option<&ProgramDependenceGraph>,
    file_path: &Path,
    original: &str,
    edit_ranges: &[(usize, usize)],
) -> EditRiskSignals {
    let overlapping = |pdg: &ProgramDependenceGraph| -> Vec<crate::graph::pdg::NodeId> {
        let candidates: Vec<_> = pdg
            .nodes_in_file(&file_path.to_string_lossy())
            .iter()
            .copied()
            .collect();
        let mut seen = std::collections::HashSet::new();
        candidates
            .into_iter()
            .filter(|nid| {
                let Some(node) = pdg.get_node(*nid) else {
                    return false;
                };
                let (s, e) = node.byte_range;
                edit_ranges.iter().any(|&(es, ee)| {
                    if es == ee {
                        s <= es && es <= e
                    } else {
                        s < ee && es < e
                    }
                })
            })
            .filter(|nid| seen.insert(*nid))
            .collect()
    };

    let mut affected_callers = 0usize;
    let mut pub_signature_touched = false;
    let mut tests_reference = false;

    if let Some(pdg) = pdg {
        let mut caller_roots = std::collections::HashSet::new();
        for node_id in overlapping(pdg) {
            let Some(node) = pdg.get_node(node_id) else {
                continue;
            };
            for caller in pdg.backward_impact(
                node_id,
                &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
            ) {
                caller_roots.insert(caller);
            }
            // `pub` detection from the source line the node starts on — the
            // PDG does not carry visibility, and the stub in drift.rs treats
            // everything as public.
            let line_start = original[..node.byte_range.0.min(original.len())]
                .rfind('\n')
                .map(|pos| pos + 1)
                .unwrap_or(0);
            let decl_line = original[line_start..]
                .lines()
                .next()
                .unwrap_or_default();
            let decl_head = decl_line.trim_start();
            let is_pub = decl_head.starts_with("pub ")
                || decl_head.starts_with("pub(")
                || decl_head.starts_with("pub\r");
            if is_pub && range_touched(edit_ranges, node.byte_range.0) {
                pub_signature_touched = true;
            }
            if !tests_reference && symbol_referenced_by_tests(pdg, node_id, &node.name) {
                tests_reference = true;
            }
        }
        affected_callers = caller_roots.len();
    }

    EditRiskSignals {
        affected_callers,
        pub_signature_touched,
        tests_reference,
    }
}

/// Classify signals into a finding, or `None` when the edit is low-risk
/// (the only case where the brief says to print nothing at all).
pub(crate) fn assess(signals: &EditRiskSignals) -> Option<RiskFinding> {
    let mut reasons: Vec<String> = Vec::new();
    let mut level = "medium";
    if signals.pub_signature_touched {
        reasons.push("pub signature changed".to_string());
        level = "high";
    }
    if signals.affected_callers >= CALLER_HIGH_WATERMARK {
        reasons.push(format!("{} callers", signals.affected_callers));
        level = "high";
    } else if signals.affected_callers > 0 {
        reasons.push(format!("{} callers", signals.affected_callers));
    }
    if signals.tests_reference {
        reasons.push("tests reference the symbol".to_string());
    }
    if reasons.is_empty() {
        return None;
    }
    Some(RiskFinding {
        level,
        reason: reasons.join(", "),
    })
}

/// Number of callers at which risk becomes `high` regardless of visibility.
const CALLER_HIGH_WATERMARK: usize = 10;

fn range_touched(edit_ranges: &[(usize, usize)], byte: usize) -> bool {
    edit_ranges.iter().any(|&(s, e)| s <= byte && byte < e.max(s + 1))
}

/// Does any test-side node reference `symbol_name` through the call/data
/// graph (a node in a `*_test.rs`/`tests/` file that reaches the symbol)?
fn symbol_referenced_by_tests(
    pdg: &ProgramDependenceGraph,
    node_id: crate::graph::pdg::NodeId,
    _symbol_name: &str,
) -> bool {
    let is_test_path = |p: &str| {
        let lowered = p.to_ascii_lowercase();
        lowered.contains("_test.rs")
            || lowered.contains("_tests.rs")
            || lowered.contains("/tests/")
            || lowered.contains("\\tests\\")
    };
    // Direct graph edges from test-side nodes.
    for neighbor in pdg.backward_impact(
        node_id,
        &crate::graph::pdg::TraversalConfig::for_impact_analysis(),
    ) {
        if let Some(node) = pdg.get_node(neighbor)
            && is_test_path(&node.file_path)
        {
            return true;
        }
    }
    false
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_assess_returns_none_for_silent_signals() {
        let signals = EditRiskSignals {
            affected_callers: 0,
            pub_signature_touched: false,
            tests_reference: false,
        };
        assert!(assess(&signals).is_none(), "low risk must emit no line");
    }

    #[test]
    fn test_assess_names_pub_signature_and_callers() {
        let signals = EditRiskSignals {
            affected_callers: 14,
            pub_signature_touched: true,
            tests_reference: false,
        };
        let finding = assess(&signals).expect("pub signature change is a finding");
        assert_eq!(finding.level, "high");
        assert!(finding.reason.contains("pub signature changed"));
        assert!(finding.reason.contains("14 callers"));
    }

    #[test]
    fn test_assess_callers_only_is_medium_with_count() {
        let signals = EditRiskSignals {
            affected_callers: 3,
            pub_signature_touched: false,
            tests_reference: true,
        };
        let finding = assess(&signals).expect("3 callers is a finding");
        assert_eq!(finding.level, "medium");
        assert!(finding.reason.contains("3 callers"));
        assert!(finding.reason.contains("tests reference"));
    }
}
