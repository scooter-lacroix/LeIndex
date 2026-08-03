// Hybrid scoring algorithm

use serde::{Deserialize, Serialize};

/// Combined score from multiple signals
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
pub struct Score {
    /// Overall score (0-1)
    pub overall: f32,

    /// TF-IDF similarity component (keyword-based semantic)
    pub tfidf: f32,

    /// Neural/remote similarity component (deep semantic)
    pub neural: f32,

    /// Structural relevance component
    pub structural: f32,

    /// Text match component
    pub text_match: f32,

    /// Fragment (sub-symbol) similarity component. Always 0.0 unless the
    /// fragment layer is enabled (fragment-embeddings 1.11.0 Task 6).
    /// Serde-defaults so legacy persisted scores decode as 0.0.
    #[serde(default)]
    pub fragment: f32,
}

impl Score {
    /// Create a new score using default code-search weights (legacy method for compatibility)
    #[deprecated(
        since = "1.6.4",
        note = "Use new_hybrid instead for TF-IDF + neural scoring"
    )]
    pub fn new(semantic: f32, structural: f32, text_match: f32) -> Self {
        Self::new_hybrid(semantic, 0.0, structural, text_match)
    }

    /// Create a new hybrid score with TF-IDF and neural components
    pub fn new_hybrid(tfidf: f32, neural: f32, structural: f32, text_match: f32) -> Self {
        let overall = HybridScorer::new()
            .score_hybrid(tfidf, neural, structural, text_match)
            .overall;
        Self {
            overall,
            tfidf,
            neural,
            structural,
            text_match,
            fragment: 0.0,
        }
    }

    /// Get the overall score
    pub fn value(&self) -> f32 {
        self.overall
    }
}

/// Hybrid scorer combining TF-IDF, neural, structural, and text signals
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct HybridScorer {
    /// Weight for TF-IDF component
    tfidf_weight: f32,

    /// Weight for neural component
    neural_weight: f32,

    /// Weight for structural component
    structural_weight: f32,

    /// Weight for text match component
    text_weight: f32,

    /// Weight for the fragment (sub-symbol) component. 0.0 unless the
    /// fragment layer is enabled (fragment-embeddings 1.11.0 Task 6).
    fragment_weight: f32,
}

impl HybridScorer {
    /// Create a new hybrid scorer
    ///
    /// Default weights are optimized for code search with neural embeddings:
    /// - tfidf: 0.30 (keyword-based semantic)
    /// - neural: 0.40 (deep semantic understanding)
    /// - structural: 0.15 (moderate complexity signal)
    /// - text: 0.15 (exact keyword matching)
    pub fn new() -> Self {
        Self::for_code()
    }

    /// Scorer tuned for code symbol search (with neural embeddings)
    ///
    /// Optimized for finding code symbols where semantic understanding
    /// and keyword overlap are both important.
    pub fn for_code() -> Self {
        Self {
            tfidf_weight: 0.30,
            neural_weight: 0.40,
            structural_weight: 0.15,
            text_weight: 0.15,
            fragment_weight: 0.0,
        }
    }

    /// Scorer tuned for code search without neural embeddings (TF-IDF only)
    ///
    /// When neural embeddings are unavailable, TF-IDF gets higher weight.
    pub fn for_code_without_neural() -> Self {
        Self {
            tfidf_weight: 0.60,
            neural_weight: 0.00,
            structural_weight: 0.20,
            text_weight: 0.20,
            fragment_weight: 0.0,
        }
    }

    /// Scorer tuned for natural-language/prose search
    ///
    /// Optimized for searching documentation, READMEs, and other
    /// prose where semantic understanding is more valuable.
    pub fn for_prose() -> Self {
        Self {
            tfidf_weight: 0.25,
            neural_weight: 0.55,
            structural_weight: 0.10,
            text_weight: 0.10,
            fragment_weight: 0.0,
        }
    }

    /// Set custom weights (legacy method for compatibility)
    #[deprecated(since = "1.6.4", note = "Use with_weights_hybrid instead")]
    pub fn with_weights(mut self, semantic: f32, structural: f32, text: f32) -> Self {
        // Map legacy semantic to tfidf for backward compatibility
        self.tfidf_weight = semantic;
        self.neural_weight = 0.0;
        self.structural_weight = structural;
        self.text_weight = text;
        self
    }

    /// Set custom hybrid weights
    pub fn with_weights_hybrid(
        mut self,
        tfidf: f32,
        neural: f32,
        structural: f32,
        text: f32,
    ) -> Self {
        self.tfidf_weight = tfidf;
        self.neural_weight = neural;
        self.structural_weight = structural;
        self.text_weight = text;
        self.fragment_weight = 0.0;
        self
    }

    /// Set custom hybrid weights including the fragment (sub-symbol) component
    /// (fragment-embeddings 1.11.0 Task 6). Callers are expected to renormalize
    /// the five weights to sum to 1.0 (see [`HybridScorer::renormalize_weights`]).
    pub fn with_weights_hybrid5(
        mut self,
        tfidf: f32,
        neural: f32,
        structural: f32,
        text: f32,
        fragment: f32,
    ) -> Self {
        self.tfidf_weight = tfidf;
        self.neural_weight = neural;
        self.structural_weight = structural;
        self.text_weight = text;
        self.fragment_weight = fragment;
        self
    }

    /// Calculate combined score (legacy method for compatibility)
    #[deprecated(
        since = "1.6.4",
        note = "Use score_hybrid instead for TF-IDF + neural scoring"
    )]
    pub fn score(&self, semantic: f32, structural: f32, text_match: f32) -> Score {
        self.score_hybrid(semantic, 0.0, structural, text_match)
    }

    /// Calculate combined hybrid score with TF-IDF and neural components
    pub fn score_hybrid(&self, tfidf: f32, neural: f32, structural: f32, text_match: f32) -> Score {
        self.score_hybrid5(tfidf, neural, structural, text_match, 0.0)
    }

    /// Calculate the combined hybrid score with a fragment (sub-symbol)
    /// component (fragment-embeddings 1.11.0 Task 6). With a 0.0 fragment
    /// weight the result is byte-identical to [`Self::score_hybrid`], keeping
    /// the default (feature-off) path unchanged.
    pub fn score_hybrid5(
        &self,
        tfidf: f32,
        neural: f32,
        structural: f32,
        text_match: f32,
        fragment: f32,
    ) -> Score {
        let overall = tfidf * self.tfidf_weight
            + neural * self.neural_weight
            + structural * self.structural_weight
            + text_match * self.text_weight
            + fragment * self.fragment_weight;

        Score {
            overall: overall.clamp(0.0, 1.0),
            tfidf,
            neural,
            structural,
            text_match,
            fragment,
        }
    }

    fn recompute_overall(&self, score: &mut Score) {
        score.overall = (score.tfidf * self.tfidf_weight
            + score.neural * self.neural_weight
            + score.structural * self.structural_weight
            + score.text_match * self.text_weight
            + score.fragment * self.fragment_weight)
            .clamp(0.0, 1.0);
    }

    /// Renormalize the five hybrid weights to sum to 1.0 (fragment fusion,
    /// fragment-embeddings 1.11.0 Task 6).
    ///
    /// The base four weights already sum to 1.0; the fragment weight is added
    /// and every component divided by the new total, so enabling the fragment
    /// layer keeps the composite in [0, 1] without exceeding it. With a 0.0
    /// fragment weight the tuple is unchanged (byte-identical default).
    pub fn renormalize_weights(
        tfidf: f32,
        neural: f32,
        structural: f32,
        text: f32,
        fragment: f32,
    ) -> (f32, f32, f32, f32, f32) {
        let sum = tfidf + neural + structural + text + fragment;
        if sum <= f32::EPSILON {
            return (0.0, 0.0, 0.0, 0.0, 0.0);
        }
        (
            tfidf / sum,
            neural / sum,
            structural / sum,
            text / sum,
            fragment / sum,
        )
    }

    /// Re-rank results based on query type (legacy method for compatibility)
    #[deprecated(
        since = "1.6.4",
        note = "Use rerank_hybrid instead for TF-IDF + neural reranking"
    )]
    pub fn rerank(&self, results: Vec<ScoreResult>, query_type: QueryType) -> Vec<ScoreResult> {
        self.rerank_hybrid(results, query_type)
    }

    /// Re-rank hybrid results based on query type
    pub fn rerank_hybrid(
        &self,
        results: Vec<ScoreResult>,
        query_type: QueryType,
    ) -> Vec<ScoreResult> {
        let mut ranked = results;
        match query_type {
            QueryType::Semantic => {
                // Boost neural and TF-IDF scores
                for result in &mut ranked {
                    result.score.neural *= 1.2;
                    result.score.tfidf *= 1.1;
                    self.recompute_overall(&mut result.score);
                }
            }
            QueryType::Structural => {
                // Boost structural scores
                for result in &mut ranked {
                    result.score.structural *= 1.2;
                    self.recompute_overall(&mut result.score);
                }
            }
            QueryType::Text => {
                // Boost text match scores
                for result in &mut ranked {
                    result.score.text_match *= 1.2;
                    self.recompute_overall(&mut result.score);
                }
            }
            QueryType::Exact => {
                // Boost text match scores even more aggressively for exact mode
                for result in &mut ranked {
                    result.score.text_match *= 1.5;
                    self.recompute_overall(&mut result.score);
                }
            }
        }

        ranked.sort_by(|a, b| {
            b.score
                .overall
                .partial_cmp(&a.score.overall)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        ranked
    }
}

/// Query type for adaptive ranking
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum QueryType {
    /// Semantic-heavy query (conceptual relevance, TF-IDF focused)
    Semantic,

    /// Structural-heavy query
    Structural,

    /// Text-heavy query
    Text,

    /// Exact-match query (prioritize exact symbol name matches)
    Exact,
}

/// Score result with metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoreResult {
    /// Node ID
    pub node_id: String,

    /// Calculated score
    pub score: Score,

    /// Query type detected
    pub query_type: QueryType,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_score_creation_legacy() {
        #[allow(deprecated)]
        let score = Score::new(0.9, 0.7, 0.5);
        assert_eq!(score.tfidf, 0.9); // Legacy semantic mapped to tfidf
        assert_eq!(score.neural, 0.0);
        assert_eq!(score.structural, 0.7);
        assert_eq!(score.text_match, 0.5);
    }

    #[test]
    fn test_score_creation_hybrid() {
        let score = Score::new_hybrid(0.7, 0.9, 0.6, 0.5);
        assert_eq!(score.tfidf, 0.7);
        assert_eq!(score.neural, 0.9);
        assert_eq!(score.structural, 0.6);
        assert_eq!(score.text_match, 0.5);
    }

    #[test]
    fn test_hybrid_scorer_legacy() {
        #[allow(deprecated)]
        {
            let scorer = HybridScorer::new();
            let score = scorer.score(0.8, 0.6, 0.4);
            // Default weights: 0.30 * 0.8 + 0.40 * 0.0 + 0.15 * 0.6 + 0.15 * 0.4 = 0.39
            assert!((score.overall - 0.39).abs() < 0.01);
        }
    }

    #[test]
    fn test_hybrid_scorer_new() {
        let scorer = HybridScorer::new();
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Default weights: 0.30 * 0.8 + 0.40 * 0.9 + 0.15 * 0.6 + 0.15 * 0.4 = 0.75
        assert!((score.overall - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_custom_weights_legacy() {
        #[allow(deprecated)]
        let scorer = HybridScorer::new().with_weights(0.3, 0.5, 0.2);
        #[allow(deprecated)]
        let score = scorer.score(0.8, 0.6, 0.4);
        // Custom weights (mapped): 0.3 * 0.8 + 0.0 * 0.0 + 0.5 * 0.6 + 0.2 * 0.4 = 0.62
        assert!((score.overall - 0.62).abs() < 0.01);
    }

    #[test]
    fn test_custom_weights_hybrid() {
        let scorer = HybridScorer::new().with_weights_hybrid(0.3, 0.4, 0.2, 0.1);
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Custom hybrid weights: 0.3 * 0.8 + 0.4 * 0.9 + 0.2 * 0.6 + 0.1 * 0.4 = 0.76
        assert!((score.overall - 0.76).abs() < 0.01);
    }

    #[test]
    fn test_for_code_scorer_with_neural() {
        let scorer = HybridScorer::for_code();
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Code weights with neural: 0.30 * 0.8 + 0.40 * 0.9 + 0.15 * 0.6 + 0.15 * 0.4 = 0.75
        assert!((score.overall - 0.75).abs() < 0.01);
    }

    #[test]
    fn test_for_code_scorer_without_neural() {
        let scorer = HybridScorer::for_code_without_neural();
        let score = scorer.score_hybrid(0.8, 0.0, 0.6, 0.4);
        // Code weights without neural: 0.60 * 0.8 + 0.00 * 0.0 + 0.20 * 0.6 + 0.20 * 0.4 = 0.68
        assert!((score.overall - 0.68).abs() < 0.01);
    }

    #[test]
    fn test_for_prose_scorer() {
        let scorer = HybridScorer::for_prose();
        let score = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        // Prose weights: 0.25 * 0.8 + 0.55 * 0.9 + 0.10 * 0.6 + 0.10 * 0.4 = 0.795
        assert!((score.overall - 0.795).abs() < 0.01);
    }

    #[test]
    fn test_rerank_hybrid_recomputes_scores_for_each_query_type() {
        let scorer = HybridScorer::new();

        for (query_type, expected) in [
            (QueryType::Semantic, (0.22, 0.36, 0.4, 0.5, 0.345)),
            (QueryType::Structural, (0.2, 0.3, 0.48, 0.5, 0.327)),
            (QueryType::Text, (0.2, 0.3, 0.4, 0.6, 0.33)),
            (QueryType::Exact, (0.2, 0.3, 0.4, 0.75, 0.3525)),
        ] {
            let ranked = scorer.rerank_hybrid(
                vec![ScoreResult {
                    node_id: String::from("node"),
                    score: Score {
                        overall: 0.0,
                        tfidf: 0.2,
                        neural: 0.3,
                        structural: 0.4,
                        text_match: 0.5,
                        fragment: 0.0,
                    },
                    query_type,
                }],
                query_type,
            );
            let score = ranked[0].score;

            assert!((score.tfidf - expected.0).abs() < f32::EPSILON);
            assert!((score.neural - expected.1).abs() < f32::EPSILON);
            assert!((score.structural - expected.2).abs() < f32::EPSILON);
            assert!((score.text_match - expected.3).abs() < f32::EPSILON);
            assert!((score.overall - expected.4).abs() < f32::EPSILON);
        }
    }

    #[test]
    fn test_renormalize_weights_sums_to_one() {
        let (t, n, s, tx, f) = HybridScorer::renormalize_weights(0.3, 0.3, 0.1, 0.3, 0.12);
        let sum = t + n + s + tx + f;
        assert!(
            (sum - 1.0).abs() < 1e-6,
            "renormalized weights must sum to 1.0"
        );
        // Base proportions are preserved: tfidf / neural stays 1.0 (0.3/0.3).
        assert!((t / n - 1.0).abs() < 1e-6);
        // Degenerate input yields all zeros rather than NaN.
        let (t2, n2, s2, tx2, f2) = HybridScorer::renormalize_weights(0.0, 0.0, 0.0, 0.0, 0.0);
        assert_eq!((t2, n2, s2, tx2, f2), (0.0, 0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn test_score_hybrid5_fragment_component() {
        let scorer = HybridScorer::new().with_weights_hybrid5(0.3, 0.3, 0.1, 0.3, 0.12);
        let score = scorer.score_hybrid5(0.8, 0.9, 0.6, 0.4, 0.5);
        assert_eq!(score.fragment, 0.5);
        assert!(score.overall <= 1.0);
        // The 4-arg score_hybrid stays fragment-free (byte-identical default).
        let base = scorer.score_hybrid(0.8, 0.9, 0.6, 0.4);
        assert_eq!(base.fragment, 0.0);
    }
}
