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
use std::sync::{Mutex, OnceLock};

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
    /// Enable the one-time legacy→CAS store migration sweep (WS4 Task 10).
    ///
    /// When enabled, `LeIndex::new` converts a legacy full-copy `.leindex/`
    /// store to the content-addressed generation layout on first run. The
    /// sweep is destructive (stale generations, completed jobs, and redundant
    /// full-copy artifacts are removed), so it ships behind this flag and a
    /// backup warning rather than running unconditionally.
    GenerationMigration,
    /// Wire the search/symbol/deep-analyze read path onto leased mmap
    /// generations (WS4 Task 14) instead of the legacy heap-mirror store.
    ///
    /// When enabled and the project has a current generation, MCP read-path
    /// handlers acquire a [`GenerationLease`](crate::storage::generation::lease::GenerationLease),
    /// drop the legacy heap-mirror structures, and read from the generation's
    /// content-addressed mmap layers. When disabled (the default), handlers
    /// keep reading from the legacy heap-mirror path so the two can be
    /// compared bit-for-bit (VAL-EQUIV-001/002/003).
    GenerationReaders,
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
            Self::GenerationMigration => "LEINDEX_FEATURE_GENERATION_MIGRATION",
            Self::GenerationReaders => "LEINDEX_FEATURE_GENERATION_READERS",
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
    ///
    /// A test-only override (set via [`set_flag_override_for_test`]) takes
    /// precedence so a single process can exercise both sides of a feature.
    pub fn is_enabled(&self) -> bool {
        if let Ok(guard) = TEST_OVERRIDES.lock() {
            if let Some(map) = guard.as_ref() {
                if let Some(v) = map.get(self) {
                    return *v;
                }
            }
        }
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
            Self::GenerationMigration => "Enable the one-time legacy→CAS store migration sweep",
            Self::GenerationReaders => {
                "Read-path handlers use leased mmap generations instead of heap mirrors"
            }
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
            FeatureFlag::GenerationMigration,
            FeatureFlag::GenerationReaders,
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

/// Test-only overrides. When `Some`, values in the map shadow the env-derived
/// store for the flagged features. Serialized by a `Mutex` so `is_enabled`
/// callers never observe a torn map.
static TEST_OVERRIDES: Mutex<Option<HashMap<FeatureFlag, bool>>> = Mutex::new(None);

/// Test-only: force a flag's effective value regardless of the environment.
///
/// Overrides take precedence over the (OnceLock-cached) env-derived store, so
/// tests can exercise both sides of a flag in one process without env
/// mutation races. Production callers must not use this.
#[doc(hidden)]
pub fn set_flag_override_for_test(flag: FeatureFlag, value: bool) {
    let mut guard = TEST_OVERRIDES.lock().expect("flag override mutex poisoned");
    guard.get_or_insert_with(HashMap::new).insert(flag, value);
}

/// Test-only: clear all overrides, restoring env-derived flag behavior.
#[doc(hidden)]
pub fn clear_flag_overrides_for_test() {
    let mut guard = TEST_OVERRIDES.lock().expect("flag override mutex poisoned");
    *guard = None;
}

/// Serializes flag-override toggling across tests in one process.
///
/// `set_flag_override_for_test`/`clear_flag_overrides_for_test` mutate a
/// process-global store, so tests that exercise a flag on both sides must hold
/// this lock for their whole duration to avoid racing sibling tests.
#[doc(hidden)]
pub static FLAG_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Run `f` with `flag` overridden to `value`, restoring the env-derived state
/// afterwards. Serialized by [`FLAG_TEST_LOCK`] so tests never race the shared
/// override store.
#[doc(hidden)]
pub fn with_flag_override(flag: FeatureFlag, value: bool, f: impl FnOnce()) {
    let _g = FLAG_TEST_LOCK.lock().expect("flag test lock poisoned");
    set_flag_override_for_test(flag, value);
    f();
    clear_flag_overrides_for_test();
}

fn flag_store() -> &'static FlagStore {
    FLAG_STORE.get_or_init(FlagStore::new)
}

/// Returns a list of all feature flags and their current state.
///
/// Useful for CLI output (`leindex feature-flags`) and debugging.
pub fn all_flags() -> Vec<(FeatureFlag, bool)> {
    [
        FeatureFlag::NeuralSearch,
        FeatureFlag::RemoteEmbeddings,
        FeatureFlag::CrossLanguageResolution,
        FeatureFlag::ExperimentalHnsw,
        FeatureFlag::StreamingMcp,
        FeatureFlag::GlobalAutoSync,
        FeatureFlag::GenerationMigration,
        FeatureFlag::GenerationReaders,
    ]
    .into_iter()
    .map(|f| (f, f.is_enabled()))
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

    #[test]
    fn test_generation_readers_is_new_and_off_by_default() {
        // A genuinely new/experimental feature must ship default OFF so the
        // legacy heap-mirror read path stays the default until proven.
        assert_eq!(
            FeatureFlag::GenerationReaders.env_var(),
            "LEINDEX_FEATURE_GENERATION_READERS"
        );
        assert!(!FeatureFlag::GenerationReaders.default_value());
        assert!(!FeatureFlag::GenerationReaders.is_enabled());
    }

    #[test]
    fn test_override_toggles_flag() {
        // Serialize the shared override store across this test only.
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        assert!(!FeatureFlag::GenerationReaders.is_enabled());
        set_flag_override_for_test(FeatureFlag::GenerationReaders, true);
        assert!(FeatureFlag::GenerationReaders.is_enabled());
        clear_flag_overrides_for_test();
        assert!(!FeatureFlag::GenerationReaders.is_enabled());
    }
}
