use serde::{Deserialize, Serialize};

/// Recommendation confidence labels.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Confidence {
    /// Strong evidence from exact graph relationships.
    Exact,
    /// Reasonable but heuristic signal.
    Heuristic,
    /// Based on unresolved/external signals.
    External,
}

/// Optimization recommendation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Recommendation {
    /// Human-readable recommendation text.
    pub message: String,
    /// Priority score (higher first).
    pub priority: f32,
    /// Confidence label.
    pub confidence: Confidence,
    /// Why this recommendation was produced.
    pub rationale: String,
}
