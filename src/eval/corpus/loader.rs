//! Corpus loading and verification utilities.
//!
//! The loader reads corpus data (from the built-in default or an external
//! JSON file), verifies category coverage and split integrity, and returns
//! a ready-to-use [`Corpus`].

use super::{Corpus, CorpusVerificationError};
use std::path::Path;

/// Load and verify the default built-in corpus.
///
/// This builds the corpus from the hardcoded `data::build_default_corpus`
/// data, then runs both verification checks (category coverage and split
/// integrity). Returns an error if verification fails.
pub fn load_and_verify_corpus() -> Result<Corpus, CorpusVerificationError> {
    let corpus = super::data::build_default_corpus();
    corpus.verify_category_coverage()?;
    corpus.verify_split_integrity()?;
    Ok(corpus)
}

/// Load a corpus from a JSON file.
///
/// The JSON file should contain a serialized [`Corpus`] (a map with a
/// `cases` key containing a list of `CorpusCase` objects).
pub fn load_corpus_from_file(path: &Path) -> Result<Corpus, LoadError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| LoadError::Io(format!("{}: {}", path.display(), e)))?;
    let corpus: Corpus = serde_json::from_str(&content)
        .map_err(|e| LoadError::Parse(format!("{}: {}", path.display(), e)))?;
    Ok(corpus)
}

/// Load and verify a corpus from a JSON file.
pub fn load_and_verify_corpus_from_file(path: &Path) -> Result<Corpus, CorpusVerificationError> {
    let corpus = load_corpus_from_file(path).map_err(|e| {
        CorpusVerificationError::MissingCategories(vec![format!("Load error: {e}")])
    })?;
    corpus.verify_category_coverage()?;
    corpus.verify_split_integrity()?;
    Ok(corpus)
}

/// Save a corpus to a JSON file.
pub fn save_corpus_to_file(corpus: &Corpus, path: &Path) -> Result<(), SaveError> {
    let json =
        serde_json::to_string_pretty(corpus).map_err(|e| SaveError::Serialize(e.to_string()))?;
    std::fs::write(path, json).map_err(|e| SaveError::Io(format!("{}", e)))?;
    Ok(())
}

/// Get all unique languages covered by the corpus.
pub fn corpus_languages(corpus: &Corpus) -> Vec<super::Language> {
    use std::collections::HashSet;
    let mut langs: HashSet<super::Language> = HashSet::new();
    for case in &corpus.cases {
        for lang in &case.languages {
            langs.insert(*lang);
        }
    }
    let mut result: Vec<_> = langs.into_iter().collect();
    result.sort_by_key(|l| l.as_str());
    result
}

/// Get category case counts.
pub fn category_counts(corpus: &Corpus) -> std::collections::HashMap<super::EvalCategory, usize> {
    let mut counts = std::collections::HashMap::new();
    for case in &corpus.cases {
        *counts.entry(case.category).or_insert(0) += 1;
    }
    counts
}

/// Error loading a corpus.
#[derive(Debug, Clone)]
pub enum LoadError {
    /// I/O error reading the file.
    Io(String),
    /// JSON parsing error.
    Parse(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(msg) => write!(f, "IO error: {msg}"),
            Self::Parse(msg) => write!(f, "Parse error: {msg}"),
        }
    }
}

impl std::error::Error for LoadError {}

/// Error saving a corpus.
#[derive(Debug, Clone)]
pub enum SaveError {
    /// Serialization error.
    Serialize(String),
    /// I/O error writing the file.
    Io(String),
}

impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Serialize(msg) => write!(f, "Serialize error: {msg}"),
            Self::Io(msg) => write!(f, "IO error: {msg}"),
        }
    }
}

impl std::error::Error for SaveError {}

#[cfg(test)]
mod tests {
    use super::super::{EvalCategory, Language, Split};
    use super::*;

    #[test]
    fn test_load_and_verify_default_corpus() {
        let corpus = load_and_verify_corpus().expect("default corpus should verify");
        assert!(corpus.len() >= 28); // At least 2 per category x 14 categories
        assert!(corpus.eval_len() > 0);
        assert!(corpus.train_len() > 0);
    }

    #[test]
    fn test_default_corpus_covers_all_categories() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        for cat in EvalCategory::all() {
            let count = corpus.cases_for_category(cat).count();
            assert!(
                count >= 2,
                "Category '{}' has {} cases, need >= 2",
                cat.as_str(),
                count
            );
        }
    }

    #[test]
    fn test_default_corpus_has_eval_cases_for_every_category() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        for cat in EvalCategory::all() {
            let eval_count = corpus
                .cases_for_category(cat)
                .filter(|c| c.split == Split::Eval)
                .count();
            assert!(
                eval_count >= 1,
                "Category '{}' has {} eval cases, need >= 1",
                cat.as_str(),
                eval_count
            );
        }
    }

    #[test]
    fn test_corpus_languages_covers_rust() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        let langs = corpus_languages(&corpus);
        assert!(
            langs.contains(&Language::Rust),
            "Corpus should include Rust"
        );
    }

    #[test]
    fn test_category_counts_all_present() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        let counts = category_counts(&corpus);
        for cat in EvalCategory::all() {
            assert!(
                counts.contains_key(&cat),
                "Category '{}' missing from counts",
                cat.as_str()
            );
        }
    }

    #[test]
    fn test_corpus_roundtrip_json() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        let json = serde_json::to_string(&corpus).expect("serialize");
        let back: Corpus = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back.len(), corpus.len());
    }

    #[test]
    fn test_save_and_load_corpus_file() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        let tmp = tempfile::NamedTempFile::new().expect("create temp file");
        let path = tmp.path();
        save_corpus_to_file(&corpus, path).expect("save");
        let loaded = load_corpus_from_file(path).expect("load");
        assert_eq!(loaded.len(), corpus.len());
    }

    #[test]
    fn test_multi_language_coverage() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        let multi_cases: Vec<_> = corpus
            .cases_for_category(EvalCategory::MultiLanguage)
            .collect();
        assert!(multi_cases.len() >= 2);

        // The multi_language category should span multiple languages
        let langs: std::collections::HashSet<_> = multi_cases
            .iter()
            .flat_map(|c| c.languages.iter().copied())
            .collect();
        // Should have at least a few different languages
        assert!(
            langs.len() >= 3,
            "Multi-language category covers {:?} languages, expected >= 3",
            langs
        );
    }

    #[test]
    fn test_hard_negatives_have_hard_negative_labels() {
        let corpus = load_and_verify_corpus().expect("corpus should verify");
        let hn_cases: Vec<_> = corpus
            .cases_for_category(EvalCategory::HardNegatives)
            .collect();

        for case in &hn_cases {
            assert!(
                !case.hard_negatives.is_empty(),
                "Hard negative case '{}' has no hard_negatives labels",
                case.id
            );
        }
    }
}
