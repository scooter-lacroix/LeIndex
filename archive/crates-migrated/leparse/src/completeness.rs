//! Parser completeness scoring utilities.

use crate::parallel::ParsingResult;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Completeness score summary for one language.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LanguageCompleteness {
    /// Language name.
    pub language: String,
    /// Number of successfully parsed signatures considered.
    pub signatures: usize,
    /// Ratio of signatures with call extraction.
    pub calls_ratio: f32,
    /// Ratio of signatures with import extraction.
    pub imports_ratio: f32,
    /// Ratio of signatures with non-empty byte range.
    pub byte_range_ratio: f32,
    /// Composite completeness score in [0,1].
    pub score: f32,
}

/// Build per-language completeness scores from parsing results.
pub fn score_languages(results: &[ParsingResult]) -> Vec<LanguageCompleteness> {
    #[derive(Default)]
    struct Acc {
        signatures: usize,
        with_calls: usize,
        with_imports: usize,
        with_byte_range: usize,
    }

    let mut map: HashMap<String, Acc> = HashMap::new();

    for result in results {
        if !result.is_success() {
            continue;
        }

        let language = result
            .language
            .clone()
            .unwrap_or_else(|| "unknown".to_string())
            .to_ascii_lowercase();

        let acc = map.entry(language).or_default();

        for signature in &result.signatures {
            acc.signatures += 1;
            if !signature.calls.is_empty() {
                acc.with_calls += 1;
            }
            if !signature.imports.is_empty() {
                acc.with_imports += 1;
            }
            if signature.byte_range.1 > signature.byte_range.0 {
                acc.with_byte_range += 1;
            }
        }
    }

    let mut out = map
        .into_iter()
        .map(|(language, acc)| {
            let denom = acc.signatures.max(1) as f32;
            let calls_ratio = acc.with_calls as f32 / denom;
            let imports_ratio = acc.with_imports as f32 / denom;
            let byte_range_ratio = acc.with_byte_range as f32 / denom;
            let score = (calls_ratio + imports_ratio + byte_range_ratio) / 3.0;

            LanguageCompleteness {
                language,
                signatures: acc.signatures,
                calls_ratio,
                imports_ratio,
                byte_range_ratio,
                score,
            }
        })
        .collect::<Vec<_>>();

    out.sort_by(|a, b| a.language.cmp(&b.language));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::{ImportInfo, SignatureInfo, Visibility};
    use std::path::PathBuf;

    fn signature(with_calls: bool, with_imports: bool, with_range: bool) -> SignatureInfo {
        SignatureInfo {
            name: "f".to_string(),
            qualified_name: "f".to_string(),
            parameters: Vec::new(),
            return_type: None,
            visibility: Visibility::Public,
            is_async: false,
            is_method: false,
            docstring: None,
            calls: if with_calls {
                vec!["g".to_string()]
            } else {
                Vec::new()
            },
            imports: if with_imports {
                vec![ImportInfo {
                    path: "mod.g".to_string(),
                    alias: None,
                }]
            } else {
                Vec::new()
            },
            byte_range: if with_range { (1, 3) } else { (0, 0) },
        }
    }

    #[test]
    fn score_languages_computes_composite_score() {
        let results = vec![ParsingResult {
            file_path: PathBuf::from("src/lib.rs"),
            language: Some("rust".to_string()),
            signatures: vec![signature(true, false, true), signature(false, true, false)],
            error: None,
            parse_time_ms: 1,
        }];

        let scores = score_languages(&results);
        assert_eq!(scores.len(), 1);
        let rust = &scores[0];

        assert_eq!(rust.language, "rust");
        assert_eq!(rust.signatures, 2);
        assert!((rust.calls_ratio - 0.5).abs() < 0.0001);
        assert!((rust.imports_ratio - 0.5).abs() < 0.0001);
        assert!((rust.byte_range_ratio - 0.5).abs() < 0.0001);
        assert!((rust.score - 0.5).abs() < 0.0001);
    }
}
