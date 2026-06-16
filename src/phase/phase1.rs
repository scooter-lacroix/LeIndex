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

    // Compute parser completeness from parse results when available.
    // On a cache hit (no changed files), fall back to PDG node counts
    // so that language coverage is still reported.
    let parser_completeness = if !context.parse_results.is_empty() {
        merge_completeness_with_pdg(
            score_languages(&context.parse_results),
            &language_distribution,
        )
    } else {
        completeness_from_pdg(&language_distribution)
    };

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
        parser_completeness,
        cache_hit,
    }
}

/// Build a best-effort LanguageCompleteness list from PDG language distribution.
///
/// When parse_results are unavailable (cache hit), we infer language coverage
/// from the PDG node counts. Since we don't have per-signature call/import
/// details, we set all ratios to 1.0 (the nodes exist, so they were extracted)
/// and the score to 1.0.
fn completeness_from_pdg(
    language_distribution: &HashMap<String, usize>,
) -> Vec<LanguageCompleteness> {
    language_distribution
        .iter()
        .map(|(language, &count)| LanguageCompleteness {
            language: language.clone(),
            signatures: count,
            calls_ratio: 1.0,
            imports_ratio: 1.0,
            byte_range_ratio: 1.0,
            score: 1.0,
        })
        .collect()
}

/// Merge parse-based completeness scores with PDG language distribution.
///
/// During incremental runs, `score_languages` only reflects files in the
/// current parse batch. Languages whose files were not re-parsed (e.g.,
/// JavaScript when only Rust files changed) would be missing from the
/// completeness report. This function adds entries from the PDG language
/// distribution for any language not already present in the parse-based
/// scores, ensuring all indexed languages appear (VAL-QUALITY-016).
fn merge_completeness_with_pdg(
    parse_scores: Vec<LanguageCompleteness>,
    language_distribution: &HashMap<String, usize>,
) -> Vec<LanguageCompleteness> {
    let mut result = parse_scores;

    // Collect languages already covered by parse-based scores.
    let covered: std::collections::HashSet<String> =
        result.iter().map(|lc| lc.language.clone()).collect();

    // Add entries from PDG for languages not in the current parse batch.
    for (language, &count) in language_distribution {
        // Skip non-language entries that are internal/structural.
        if language == "external" || language.is_empty() {
            continue;
        }
        if !covered.contains(language) {
            result.push(LanguageCompleteness {
                language: language.clone(),
                signatures: count,
                calls_ratio: 1.0,
                imports_ratio: 1.0,
                byte_range_ratio: 1.0,
                score: 1.0,
            });
        }
    }

    result.sort_by(|a, b| a.language.cmp(&b.language));
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merge_completeness_adds_missing_languages() {
        // Simulate an incremental run: only Rust files were re-parsed,
        // but the PDG also contains JavaScript and Python nodes.
        let parse_scores = vec![LanguageCompleteness {
            language: "rust".to_string(),
            signatures: 50,
            calls_ratio: 0.8,
            imports_ratio: 0.9,
            byte_range_ratio: 1.0,
            score: 0.9,
        }];

        let mut pdg_dist = HashMap::new();
        pdg_dist.insert("rust".to_string(), 50);
        pdg_dist.insert("javascript".to_string(), 15);
        pdg_dist.insert("python".to_string(), 10);
        pdg_dist.insert("external".to_string(), 5); // should be skipped

        let merged = merge_completeness_with_pdg(parse_scores, &pdg_dist);

        // Should have 3 languages (rust from parse, js and python from PDG)
        assert_eq!(merged.len(), 3);

        let languages: Vec<&str> = merged.iter().map(|lc| lc.language.as_str()).collect();
        assert!(languages.contains(&"rust"));
        assert!(languages.contains(&"javascript"));
        assert!(languages.contains(&"python"));
        // "external" should NOT appear
        assert!(!languages.contains(&"external"));

        // Rust should retain its parse-based scores
        let rust = merged.iter().find(|lc| lc.language == "rust").unwrap();
        assert_eq!(rust.signatures, 50);
        assert!((rust.score - 0.9).abs() < 0.001);

        // JavaScript should have PDG-based counts
        let js = merged
            .iter()
            .find(|lc| lc.language == "javascript")
            .unwrap();
        assert_eq!(js.signatures, 15);
        assert!((js.score - 1.0).abs() < 0.001);
    }

    #[test]
    fn merge_completeness_empty_pdg() {
        let parse_scores = vec![LanguageCompleteness {
            language: "rust".to_string(),
            signatures: 10,
            calls_ratio: 1.0,
            imports_ratio: 1.0,
            byte_range_ratio: 1.0,
            score: 1.0,
        }];

        let pdg_dist = HashMap::new();
        let merged = merge_completeness_with_pdg(parse_scores, &pdg_dist);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn merge_completeness_all_from_pdg() {
        // No parse results at all - everything comes from PDG
        let parse_scores = vec![];
        let mut pdg_dist = HashMap::new();
        pdg_dist.insert("javascript".to_string(), 20);
        pdg_dist.insert("typescript".to_string(), 30);

        let merged = merge_completeness_with_pdg(parse_scores, &pdg_dist);
        assert_eq!(merged.len(), 2);

        let js = merged
            .iter()
            .find(|lc| lc.language == "javascript")
            .unwrap();
        assert_eq!(js.signatures, 20);
    }
}
