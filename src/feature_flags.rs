//! Feature flag infrastructure for controlled rollout of experimental features.
//!
//! Feature flags allow enabling/disabling functionality at runtime without
//! recompilation. Flags are read from environment variables with the
//! `LEINDEX_FEATURE_` prefix, or from the optional config file.
//!
//! ## Usage
//!
//! ```rust,ignore
//! use leindex::feature_flags::FeatureFlag;
//!
//! if FeatureFlag::NeuralSearch.is_enabled() {
//!     // Use neural search path
//! }
//! ```

use std::collections::HashMap;
use std::env;
use std::sync::OnceLock;

/// All supported feature flags.
///
/// Each variant maps to an environment variable `LEINDEX_FEATURE_<NAME>`.
/// Flags default to `false` unless explicitly marked as default-on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FeatureFlag {
    /// Enable neural (ONNX) embedding search at runtime.
    NeuralSearch,
    /// Enable remote embedding API (OpenAI, etc.).
    RemoteEmbeddings,
    /// Enable cross-language symbol resolution.
    CrossLanguageResolution,
    /// Enable experimental HNSW algorithm parameters.
    ExperimentalHnsw,
    /// Enable streaming MCP notifications.
    StreamingMcp,
    /// Enable global index auto-sync.
    GlobalAutoSync,
}

impl FeatureFlag {
    /// Returns the environment variable name for this flag.
    pub fn env_var(&self) -> &'static str {
        match self {
            Self::NeuralSearch => "LEINDEX_FEATURE_NEURAL_SEARCH",
            Self::RemoteEmbeddings => "LEINDEX_FEATURE_REMOTE_EMBEDDINGS",
            Self::CrossLanguageResolution => "LEINDEX_FEATURE_CROSS_LANGUAGE",
            Self::ExperimentalHnsw => "LEINDEX_FEATURE_EXPERIMENTAL_HNSW",
            Self::StreamingMcp => "LEINDEX_FEATURE_STREAMING_MCP",
            Self::GlobalAutoSync => "LEINDEX_FEATURE_GLOBAL_AUTO_SYNC",
        }
    }

    /// Returns whether this flag is enabled by default (without env override).
    pub fn default_value(&self) -> bool {
        match self {
            // GA production features default ON so the flag acts as a
            // per-deployment rollout-KILL (explicit `false` disables; unset
            // follows normal config). Genuinely new/experimental features below
            // still default off.
            Self::StreamingMcp | Self::GlobalAutoSync | Self::NeuralSearch => true,
            // Everything else defaults off
            _ => false,
        }
    }

    /// Checks whether this feature flag is currently enabled.
    ///
    /// Reads the corresponding env var. Values "1", "true", "yes" enable.
    /// Values "0", "false", "no" disable. If unset, uses `default_value()`.
    pub fn is_enabled(&self) -> bool {
        flag_store().get(self)
    }

    /// Returns a human-readable description of the flag.
    pub fn description(&self) -> &'static str {
        match self {
            Self::NeuralSearch => "Enable neural (ONNX) embedding search at runtime",
            Self::RemoteEmbeddings => "Enable remote embedding API (OpenAI, etc.)",
            Self::CrossLanguageResolution => "Enable cross-language symbol resolution",
            Self::ExperimentalHnsw => "Enable experimental HNSW algorithm parameters",
            Self::StreamingMcp => "Enable streaming MCP notifications",
            Self::GlobalAutoSync => "Enable global index auto-sync",
        }
    }
}

/// Helper to compute the effective neural search enablement flag.
///
/// This combines the runtime feature flag (`LEINDEX_FEATURE_NEURAL_SEARCH`)
/// with the user-level config knob (`leindex.toml` `[neural] enabled`).
/// Conservative semantics: the config can disable neural (set to false),
/// but the runtime flag cannot enable what the config disabled.
pub fn is_neural_enabled(config_value: bool) -> bool {
    crate::feature_flags::FeatureFlag::NeuralSearch.is_enabled() && config_value
}
/// Internal store that caches env-var lookups in a OnceLock for zero-cost reads.
struct FlagStore {
    values: HashMap<FeatureFlag, bool>,
}

impl FlagStore {
    fn new() -> Self {
        let mut values = HashMap::new();
        for flag in [
            FeatureFlag::NeuralSearch,
            FeatureFlag::RemoteEmbeddings,
            FeatureFlag::CrossLanguageResolution,
            FeatureFlag::ExperimentalHnsw,
            FeatureFlag::StreamingMcp,
            FeatureFlag::GlobalAutoSync,
        ] {
            let enabled = match env::var(flag.env_var()) {
                Ok(v) => matches!(
                    v.to_lowercase().as_str(),
                    "1" | "true" | "yes" | "on" | "enable" | "enabled"
                ),
                Err(_) => flag.default_value(),
            };
            values.insert(flag, enabled);
        }
        Self { values }
    }

    fn get(&self, flag: &FeatureFlag) -> bool {
        *self.values.get(flag).unwrap_or(&false)
    }
}

static FLAG_STORE: OnceLock<FlagStore> = OnceLock::new();

fn flag_store() -> &'static FlagStore {
    FLAG_STORE.get_or_init(FlagStore::new)
}

/// Returns a list of all feature flags and their current state.
///
/// Useful for CLI output (`leindex feature-flags`) and debugging.
pub fn all_flags() -> Vec<(FeatureFlag, bool)> {
    let store = flag_store();
    [
        FeatureFlag::NeuralSearch,
        FeatureFlag::RemoteEmbeddings,
        FeatureFlag::CrossLanguageResolution,
        FeatureFlag::ExperimentalHnsw,
        FeatureFlag::StreamingMcp,
        FeatureFlag::GlobalAutoSync,
    ]
    .into_iter()
    .map(|f| (f, store.get(&f)))
    .collect()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_flag_env_var_names() {
        assert_eq!(
            FeatureFlag::NeuralSearch.env_var(),
            "LEINDEX_FEATURE_NEURAL_SEARCH"
        );
        assert_eq!(
            FeatureFlag::GlobalAutoSync.env_var(),
            "LEINDEX_FEATURE_GLOBAL_AUTO_SYNC"
        );
    }

    #[test]
    fn test_default_values() {
        // NeuralSearch is a GA feature: its flag defaults ON so it acts as a
        // per-deployment rollout-KILL (explicit `false` disables; unset follows
        // normal config) rather than gating a not-yet-released capability.
        assert!(FeatureFlag::NeuralSearch.default_value());
        assert!(FeatureFlag::StreamingMcp.default_value());
    }
}
