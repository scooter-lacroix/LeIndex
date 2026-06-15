use crate::parse::prelude::{score_languages, LanguageCompleteness};
use crate::phase::context::PhaseExecutionContext;
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
    /// True when incremental refresh found no changed files and the
    /// PDG was loaded from cache without re-parsing. This distinguishes
    /// a cache hit (parsed_files=0 because nothing changed) from a
    /// parse failure (parsed_files=0 because parsing broke).
    #[serde(default)]
    pub cache_hit: bool,
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

    // On an incremental run with no changed files, parse_results is empty
    // and the PDG was loaded from cache. Distinguish this from a parse
    // failure by checking whether the inventory is non-empty but no
    // files were parsed (indicating cache hit, not failure).
    let cache_hit = context.parse_results.is_empty()
        && !context.file_inventory.is_empty()
        && context.changed_files.is_empty()
        && context.deleted_files.is_empty();

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
        cache_hit,
    }
}
