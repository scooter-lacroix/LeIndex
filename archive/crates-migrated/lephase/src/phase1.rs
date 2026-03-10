use crate::context::PhaseExecutionContext;
use leparse::prelude::{score_languages, LanguageCompleteness};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Structural scan output.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Phase1Summary {
    /// Number of source files considered.
    pub total_files: usize,
    /// Number of files parsed in this run (incremental subset when refresh is enabled).
    pub parsed_files: usize,
    /// Number of parse failures in this run.
    pub parse_failures: usize,
    /// Total signatures from current parse batch.
    pub signatures: usize,
    /// Language distribution inferred from PDG nodes.
    pub language_distribution: HashMap<String, usize>,
    /// Parser completeness scores by language.
    pub parser_completeness: Vec<LanguageCompleteness>,
}

/// Run phase 1 structural scan.
pub fn run(context: &PhaseExecutionContext) -> Phase1Summary {
    let mut language_distribution: HashMap<String, usize> = HashMap::new();
    for node_idx in context.pdg.node_indices() {
        if let Some(node) = context.pdg.get_node(node_idx) {
            *language_distribution
                .entry(node.language.clone())
                .or_insert(0) += 1;
        }
    }

    Phase1Summary {
        total_files: context.file_inventory.len(),
        parsed_files: context.parse_results.len(),
        parse_failures: context
            .parse_results
            .iter()
            .filter(|r| r.is_failure())
            .count(),
        signatures: context
            .parse_results
            .iter()
            .map(|r| r.signatures.len())
            .sum(),
        language_distribution,
        parser_completeness: score_languages(&context.parse_results),
    }
}
