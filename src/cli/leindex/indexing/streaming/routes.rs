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
}
