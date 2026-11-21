//! Token accounting tests (assignment: "the token accounting").

use super::*;
use crate::tokens::{TokenCounter, estimate_tokens};

#[test]
fn test_estimate_tokens_divides_by_four_rounding_up() {
    assert_eq!(estimate_tokens(""), 0);
    assert_eq!(estimate_tokens("abcd"), 1);
    assert_eq!(estimate_tokens("abcde"), 2);
    assert_eq!(estimate_tokens("abcdefgh"), 2);
}

#[test]
fn test_counter_counts_monotonically_either_branch() {
    // Works under BOTH counters (tokenizer present or chars/4 fallback):
    // empty text is free, more text is more tokens, and provenance always
    // matches the kind so reports can never mislabel the estimator.
    let counter = Counter::load();
    assert_eq!(counter.count(""), 0);
    let short = counter.count("fn main() {}");
    let long = counter.count("fn main() { println!(\"tokbench corpus task\"); }");
    assert!(short >= 1);
    assert!(long > short);
    match counter.kind() {
        TokenCounter::Tokenizer => assert!(counter.provenance().contains("real")),
        TokenCounter::Estimate => assert!(counter.provenance().contains("chars/4")),
    }
}
