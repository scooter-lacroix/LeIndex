//! Deterministic routing for exact and semantic queries.

/// The caller's explicit search intent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestedMode {
    /// Infer intent from the query text.
    Auto,
    /// Search literal text without semantic expansion.
    Exact,
    /// Search semantic similarity.
    Semantic,
    /// Run PDG-aware deep analysis.
    Deep,
}

/// The inexpensive path selected before search state is touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryRoute {
    /// A single identifier or qualified symbol path.
    ExactSymbol,
    /// Literal text matching.
    ExactText,
    /// TF-IDF and optionally ready neural search.
    Semantic,
    /// PDG-aware analysis.
    DeepPdg,
}

/// Classify a query without loading search, PDG, or embedding state.
pub fn classify(query: &str, requested: RequestedMode) -> QueryRoute {
    match requested {
        RequestedMode::Exact => return QueryRoute::ExactText,
        RequestedMode::Semantic => return QueryRoute::Semantic,
        RequestedMode::Deep => return QueryRoute::DeepPdg,
        RequestedMode::Auto => {}
    }

    let query = query.trim_matches(|c| matches!(c, '`' | '"' | '\''));
    let identifier = !query.is_empty()
        && !query.chars().any(char::is_whitespace)
        && query
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | ':' | '.'));

    if identifier {
        QueryRoute::ExactSymbol
    } else {
        QueryRoute::Semantic
    }
}

#[cfg(test)]
mod tests {
    use super::{classify, QueryRoute, RequestedMode};

    #[test]
    fn routes_identifier_and_natural_language_queries() {
        assert_eq!(
            classify("`Askpass::new`", RequestedMode::Auto),
            QueryRoute::ExactSymbol
        );
        assert_eq!(
            classify("registry_record", RequestedMode::Exact),
            QueryRoute::ExactText
        );
        assert_eq!(
            classify("how are sudo credentials propagated", RequestedMode::Auto),
            QueryRoute::Semantic
        );
        assert_eq!(
            classify("sudo credential flow", RequestedMode::Deep),
            QueryRoute::DeepPdg
        );
    }
}
