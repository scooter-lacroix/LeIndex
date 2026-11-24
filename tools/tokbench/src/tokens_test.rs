//! Token accounting tests (assignment: "the token accounting").
//!
//! Every counter here is built in-test: one fixed tokenizer with a known
//! vocabulary and one explicit estimator. Nothing reads `HOME` or
//! `LEINDEX_TOKBENCH_TOKENIZER`, so the counts are exact and a developer's
//! tokenizer override (which may pad, truncate, or rank a longer sample as
//! fewer tokens) cannot change the outcome.

use super::*;
use tokenizers::models::wordlevel::WordLevel;
use tokenizers::pre_tokenizers::whitespace::Whitespace;
use tokenizers::{PaddingParams, PaddingStrategy, Tokenizer};

/// The house estimator: ceil(chars / 4).
#[test]
fn test_estimate_tokens_divides_by_four_rounding_up() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcde"), 2);
    assert_eq!(estimate_tokens("abcdefgh"), 2);
}

/// One token per whitespace-separated word, from a fixed vocabulary.
fn fixed_tokenizer() -> Tokenizer {
    let vocab: std::collections::HashMap<String, u32> =
        ["fn", "main", "println", "tokbench", "corpus", "task"]
            .into_iter()
            .enumerate()
            .map(|(index, word)| (word.to_string(), index as u32))
            .collect();
    let model = WordLevel::builder()
        .vocab(vocab)
        .build()
        .expect("wordlevel vocab");
    let mut tokenizer = Tokenizer::new(model);
    tokenizer.with_pre_tokenizer(Some(Whitespace));
    tokenizer
}

#[test]
fn test_counter_counts_with_a_fixed_tokenizer_exactly() {
    let counter = Counter::from_tokenizer(fixed_tokenizer());
    assert_eq!(counter.kind(), TokenCounter::Tokenizer);
    assert_eq!(counter.count("").unwrap(), 0);
    assert_eq!(counter.count("fn main").unwrap(), 2);
    let short = "fn main";
    let long = "fn main println tokbench corpus task";
    assert_eq!(counter.count(short).unwrap(), 2);
    assert_eq!(counter.count(long).unwrap(), 6);
    assert!(counter.provenance().contains("real"));
}

#[test]
fn test_counter_counts_with_the_estimator_exactly() {
    let counter = Counter::estimate();
    assert_eq!(counter.kind(), TokenCounter::Estimate);
    assert_eq!(counter.count("").unwrap(), 0);
    // 7 chars -> ceil(7/4) = 2; 36 chars -> ceil(36/4) = 9.
    assert_eq!(counter.count("fn main").unwrap(), 2);
    assert_eq!(
        counter
            .count("fn main println tokbench corpus task")
            .unwrap(),
        9
    );
    assert!(counter.provenance().contains("chars/4"));
}

/// A tokenizer file may carry training-time truncation and padding. The
/// counter must clear both, or a capped or padded encoding would be reported
/// as the size of a tool response.
#[test]
fn test_counter_clears_truncation_and_padding_before_measuring() {
    let mut padded = fixed_tokenizer();
    padded.with_padding(Some(PaddingParams {
        strategy: PaddingStrategy::Fixed(16),
        ..PaddingParams::default()
    }));
    let counter = Counter::from_tokenizer(padded);
    // With padding still active the empty input would read as 16 tokens.
    assert_eq!(counter.count("").unwrap(), 0);
    assert_eq!(counter.count("fn main").unwrap(), 2);

    let mut truncated = fixed_tokenizer();
    truncated
        .with_truncation(Some(tokenizers::TruncationParams {
            max_length: 2,
            ..tokenizers::TruncationParams::default()
        }))
        .expect("set truncation");
    let counter = Counter::from_tokenizer(truncated);
    // With truncation still active the six-word sample would read as 2.
    assert_eq!(
        counter
            .count("fn main println tokbench corpus task")
            .unwrap(),
        6
    );
}

/// A loadable tokenizer can still fail to encode (for example a WordLevel
/// model meeting a word missing from its vocabulary). The failure must
/// surface as an error, never as a chars/4 estimate wearing the
/// `Tokenizer` label.
#[test]
fn test_counter_reports_tokenizer_errors_instead_of_estimating() {
    let counter = Counter::from_tokenizer(fixed_tokenizer());
    let error = counter
        .count("fn unlisted_word main")
        .expect_err("out-of-vocabulary word must fail");
    assert!(
        error.to_string().contains("tokenizer encode failed"),
        "{error}"
    );
    assert_eq!(counter.kind(), TokenCounter::Tokenizer);
}
