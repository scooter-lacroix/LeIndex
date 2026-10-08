//! Runtime route selection for indexing feature flags.

use crate::feature_flags::FeatureFlag;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScanRoute {
    Streaming,
    Legacy,
}

pub(crate) fn scan_route_for_current_flag() -> ScanRoute {
    if FeatureFlag::StreamingScan.is_enabled() {
        ScanRoute::Streaming
    } else {
        ScanRoute::Legacy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseRoute {
    Streaming,
    Legacy,
}

pub(crate) fn parse_route_for_current_flag() -> ParseRoute {
    if FeatureFlag::StreamingParse.is_enabled() {
        ParseRoute::Streaming
    } else {
        ParseRoute::Legacy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TfidfRoute {
    Streaming,
    Legacy,
}

pub(crate) fn tfidf_route_for_current_flag() -> TfidfRoute {
    if FeatureFlag::StreamingTfidf.is_enabled() {
        TfidfRoute::Streaming
    } else {
        TfidfRoute::Legacy
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NeuralRoute {
    Streaming,
    Legacy,
}

pub(crate) fn neural_route_for_current_flag() -> NeuralRoute {
    if FeatureFlag::StreamingNeural.is_enabled() {
        NeuralRoute::Streaming
    } else {
        NeuralRoute::Legacy
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_scan_route_flag_selects_both_routes() {
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingScan, false);
        assert_eq!(scan_route_for_current_flag(), ScanRoute::Legacy);
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingScan, true);
        assert_eq!(scan_route_for_current_flag(), ScanRoute::Streaming);
    }

    #[test]
    fn test_parse_route_flag_selects_both_routes() {
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingParse, false);
        assert_eq!(parse_route_for_current_flag(), ParseRoute::Legacy);
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingParse, true);
        assert_eq!(parse_route_for_current_flag(), ParseRoute::Streaming);
    }

    #[test]
    fn test_tfidf_route_flag_selects_both_routes() {
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingTfidf, false);
        assert_eq!(tfidf_route_for_current_flag(), TfidfRoute::Legacy);
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingTfidf, true);
        assert_eq!(tfidf_route_for_current_flag(), TfidfRoute::Streaming);
    }

    #[test]
    fn test_neural_route_flag_selects_both_routes() {
        let _guard = crate::feature_flags::lock_flag_tests();
        let _reset = crate::feature_flags::FlagOverrideReset;
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingNeural, false);
        assert_eq!(neural_route_for_current_flag(), NeuralRoute::Legacy);
        crate::feature_flags::set_flag_override_for_test(FeatureFlag::StreamingNeural, true);
        assert_eq!(neural_route_for_current_flag(), NeuralRoute::Streaming);
    }
}
