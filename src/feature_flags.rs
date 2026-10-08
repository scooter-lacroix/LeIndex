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
    /// Enable the daemon-client MIME protocol path (WS3).
    ///
    /// When enabled, `leindex mcp --stdio` acts as a thin shim that discovers
    /// (or spawns) the user-scoped `leindexd` daemon and forwards MCP/JSON-RPC
    /// frames to it over a Unix socket. When disabled, the stdio server runs
    /// the full inline engine (legacy v1.9.x behaviour). Default ON since the
    /// v2.0.0 rollout phase 8 (VAL-ROLLOUT-012); set
    /// `LEINDEX_FEATURE_DAEMON_CLIENT=false` (or `LEINDEX_LEGACY=1`) to
    /// revert — the flag doubles as a rollout-KILL.
    ///
    /// This runtime flag works alongside the `daemon-client` Cargo feature:
    /// the Cargo feature compiles the shim code path; this env flag controls
    /// whether the compiled code is used at runtime. When the Cargo feature
    /// is disabled, this flag has no effect (there is no shim code to run).
    DaemonClient,
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
    /// content-addressed mmap layers. When disabled, handlers keep reading
    /// from the legacy heap-mirror path so the two can be compared
    /// bit-for-bit (VAL-EQUIV-001/002/003). Default ON since the v2.0.0
    /// rollout phase 8 (VAL-ROLLOUT-012); set
    /// `LEINDEX_FEATURE_GENERATION_READERS=false` (or `LEINDEX_LEGACY=1`) to
    /// revert.
    GenerationReaders,
    /// Route indexing through the fair bounded scheduler (WS5): heavy work is
    /// executed as stepped [`BoundedJob`](crate::scheduler::budget::BoundedJob)s
    /// behind the DRR queues and admission gate instead of one unstepped
    /// `spawn_blocking` call. Default OFF = legacy spawn_blocking + error-at-cap
    /// indexing path.
    ///
    /// **Status:** declared but NOT consumed — blocked, by design. There is no
    /// honest consumer: `IndexJob::step` invokes `PhaseExecutor::run_phase`
    /// wholesale and ignores `WorkBudget`, and the LeIndex pipeline phases
    /// (`run_scan`, `run_parse`, `run_lexical`, `run_neural`) are monolithic
    /// `&mut self` methods — there is no bounded stepping surface to route
    /// through. Wiring this flag requires splitting the phase executor into
    /// stepped jobs first; wrapping whole-phase methods would be decorative,
    /// not bounded or fair. See the PR-86 spec, D6
    /// (docs/plans/2026-10-07-pr86-flag-consumers.md).
    BoundedScheduler,
    /// Enable the streaming scan stage (WS6-9 Task 1): scan walks files lazily,
    /// hashing via a fixed 64KiB buffer, writing metadata records to CAS-staged
    /// scan blob without retaining source bodies.
    ///
    /// The feature flag gates `run_scan` in
    /// `src/cli/leindex/indexing/mod.rs`: the streaming path hashes the existing
    /// configured source inventory through `stream_scan_paths` without retaining
    /// bodies and propagates hard I/O failures. Default OFF preserves the legacy
    /// path. Coverage: `test_scan_route_flag_selects_both_routes` and
    /// `test_streaming_scan_flag_indexes_fixture_and_searches`.
    StreamingScan,
    /// Enable the streaming parse stage (WS6-9 Task 2): parse chunks bounded
    /// by file count and aggregate bytes through `stream_parse_parallel`.
    ///
    /// Consumer: `run_parse` in `src/cli/leindex/indexing/mod.rs` dispatches
    /// on `parse_route_for_current_flag()`; the streaming route feeds the same
    /// production `ParallelParser` in `ParseBudget`-bounded chunks, producing
    /// the identical `ParsingResult` sequence the legacy route returns. Default
    /// OFF preserves the legacy whole-set parse. Coverage:
    /// `test_parse_route_flag_selects_both_routes`,
    /// `test_chunk_file_inputs_respects_file_limit`,
    /// `test_chunk_file_inputs_respects_byte_limit_and_keeps_order`,
    /// `test_stream_parse_parallel_preserves_order_and_result_shape`, and
    /// `test_streaming_parse_flag_indexes_fixture_and_searches`.
    StreamingParse,
    /// Enable the compact PDG persistence stage (WS6-9 Task 3): per-file graph
    /// fragments to CAS adjacency without whole-PDG clone.
    StreamingPdg,
    /// Consumer: `build_lexical_embedder` in `src/cli/index_builder/mod.rs`
    /// selects the production streaming TF-IDF builder through
    /// `tfidf_route_for_current_flag()` in
    /// `src/cli/leindex/indexing/streaming/routes.rs`. The streaming route
    /// reuses production tokenization and writes the same vocabulary, IDF
    /// values, and vectors as the legacy builder. Default OFF preserves the
    /// legacy route. Coverage: `test_tfidf_route_flag_selects_both_routes`,
    /// `test_streaming_vocab_and_rows_match_production_embedder`, and
    /// `test_streaming_tfidf_flag_indexes_fixture_and_searches`.
    StreamingTfidf,
    /// Enable bounded neural enrichment for the published lexical index
    /// (WS6-9 Task 6).
    ///
    /// Consumer: `run_neural` in `src/cli/leindex/indexing/neural_publish.rs`
    /// selects `enrich_neural_streaming` through
    /// `neural_route_for_current_flag()`. The streaming route feeds the
    /// production hybrid embedder in bounded `BatchBudget` batches and writes
    /// accepted rows to the search engine batch-wise, retaining admission,
    /// capped-text dedupe, cache accounting, and first-row-per-node semantics.
    /// Default OFF keeps `enrich_neural_embeddings`. Coverage:
    /// `test_neural_route_flag_selects_both_routes`,
    /// `test_streaming_neural_bit_identical`, and
    /// `test_streaming_neural_flag_keeps_fixture_searchable`.
    StreamingNeural,
    /// Enable the global content-addressed embedding cache (WS10 Task 1-2).
    /// The cache stores embedding vectors at user-level (e.g.
    /// `~/.leindex/embed-cache/`) keyed by a 6-tuple CacheKey (model digest,
    /// tokenizer digest, prompt role/version, pooling, normalization, output
    /// dimensions, content hash). Cross-project dedup is automatic.
    GlobalEmbedCache,
    /// Leiden community detection over the PDG (roadmap Part IV). Kill
    /// with LEINDEX_FEATURE_COMMUNITY_DETECTION=false to skip computation
    /// and serving of community metadata.
    CommunityDetection,
    /// Enable SCIP precision ingestion when a supported external indexer is available.
    PrecisionIngest,
    /// Enable the embedding-cache debug escape hatch (WS10 privacy remediation).
    ///
    /// When ON *and* the caller provides source text to `GlobalEmbeddingCache::put`,
    /// the source text is appended to the row bytes after the vector payload for
    /// development troubleshooting. Default OFF — privacy gate preserved: no
    /// source text is persisted unless this flag (or the legacy
    /// `LEINDEX_EMBED_CACHE_DEBUG` env var, or `CacheConfig::debug_mode`) is set.
    DebugEscapeHatch,
    /// Enable the WS11 validated model profile as the production default
    /// (WS11 Task 7). When ON, the embed worker uses the model bake-off
    /// winner (CodeRankEmbed 137M, INT8 quantized, no reranker) instead of
    /// the legacy FP16 Qwen3 + reranker baseline. Default ON since the
    /// v2.0.0 rollout phase 8 (VAL-ROLLOUT-012); set
    /// `LEINDEX_FEATURE_VALIDATED_MODEL=false` (or `LEINDEX_LEGACY=1`) to
    /// revert to the legacy FP16 Qwen3 behavior.
    ValidatedModel,
    /// Engram: a persistent, content-addressed phrase-book of neural query
    /// embeddings (`~/.leindex/engram/`). Repeat queries skip the embedder
    /// entirely (no worker spawn, no model digest, no network round trip).
    /// New capability: defaults OFF until enabled per deployment.
    Engram,
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
            Self::DaemonClient => "LEINDEX_FEATURE_DAEMON_CLIENT",
            Self::GenerationMigration => "LEINDEX_FEATURE_GENERATION_MIGRATION",
            Self::GenerationReaders => "LEINDEX_FEATURE_GENERATION_READERS",
            Self::BoundedScheduler => "LEINDEX_FEATURE_BOUNDED_SCHEDULER",
            Self::StreamingScan => "LEINDEX_FEATURE_STREAMING_SCAN",
            Self::StreamingParse => "LEINDEX_FEATURE_STREAMING_PARSE",
            Self::StreamingPdg => "LEINDEX_FEATURE_STREAMING_PDG",
            Self::StreamingTfidf => "LEINDEX_FEATURE_STREAMING_TFIDF",
            Self::StreamingNeural => "LEINDEX_FEATURE_STREAMING_NEURAL",
            Self::GlobalEmbedCache => "LEINDEX_FEATURE_GLOBAL_EMBED_CACHE",
            Self::CommunityDetection => "LEINDEX_FEATURE_COMMUNITY_DETECTION",
            Self::PrecisionIngest => "LEINDEX_FEATURE_PRECISION_INGEST",
            Self::DebugEscapeHatch => "LEINDEX_FEATURE_EMBED_CACHE_DEBUG",
            Self::ValidatedModel => "LEINDEX_FEATURE_VALIDATED_MODEL",
            Self::Engram => "LEINDEX_FEATURE_ENGRAM",
        }
    }

    /// Returns whether this flag is enabled by default (without env override).
    ///
    /// The v2.0.0 rollout follows a two-state lifecycle:
    ///
    /// 1. **Initial state** (VAL-ROLLOUT-001, rollout phases 1–7): every new
    ///    rollout flag defaults OFF, so legacy v1.9.x behavior is preserved and
    ///    no capability is silently enabled until the copy is verified.
    /// 2. **Post-gate state** (VAL-ROLLOUT-012, rollout phase 8): after all
    ///    section 16 acceptance gates pass, the shipped v2.0.0 state flips those
    ///    flags to default ON. Each flag then acts as a rollout-KILL: an
    ///    explicit `"0"`/`"false"` (or the umbrella [`LEGACY_ENV`] switch) reverts
    ///    to legacy behavior for scoped rollback.
    ///
    /// The `LEINDEX_LEGACY=1` umbrella reverts all rollout flags back to OFF for
    /// the phase-9 fallback window independently of this default (see
    /// [`is_enabled`](Self::is_enabled) and [`legacy_mode_enabled`]).
    pub fn default_value(&self) -> bool {
        match self {
            // GA production features default to ON so the flag acts as a
            // per-deployment rollout-KILL (explicit `false` disables; unset
            // follows normal config). Genuinely new/experimental features below
            // still default off.
            Self::StreamingMcp
            | Self::GlobalAutoSync
            | Self::NeuralSearch
            // v2.0.0 rollout phase 8: all section 16 acceptance gates pass.
            // The v2.0.0 resource architecture (CAS generations, daemon + shim,
            // bounded scheduler, streaming pipeline, global embed cache,
            // validated model) is now the production default. Each flag acts as
            // a rollout-KILL: setting it to "0"/"false" reverts to legacy
            // behavior for scoped rollback.
            | Self::DaemonClient
            | Self::GenerationReaders
            | Self::BoundedScheduler
            | Self::StreamingPdg
            | Self::GlobalEmbedCache
            | Self::CommunityDetection
            | Self::ValidatedModel
            // SCIP precision ingest degrades silently to the Tier-0 PDG when
            // no external indexer is discoverable, so defaulting ON is safe:
            // machines without indexers behave exactly as before, machines
            // with them get precise edges. Rollout-KILL via explicit "false".
            | Self::PrecisionIngest => true,
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
        if let Some(map) = TEST_OVERRIDES
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .as_ref()
        {
            if let Some(v) = map.get(self) {
                return *v;
            }
        }
        // Phase-9 legacy fallback: when LEINDEX_LEGACY=1 is active, all
        // v2.0.0 rollout flags revert to OFF (legacy behavior). This allows
        // users to opt back to legacy code paths during the fallback window
        // without setting each flag individually.
        if legacy_mode_enabled() && is_legacy_revertible(self) {
            return false;
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
            Self::DaemonClient => "Stdio MCP shim forwards to user-scoped leindexd daemon (WS3)",
            Self::GenerationMigration => "Enable the one-time legacy→CAS store migration sweep",
            Self::GenerationReaders => {
                "Read-path handlers use leased mmap generations instead of heap mirrors"
            }
            Self::BoundedScheduler => {
                "Route heavy work through the fair bounded scheduler (DRR + admission)"
            }
            Self::StreamingScan => {
                "Streaming scan stage: lazy file walk, 64KiB hash buffer, no source retention"
            }
            Self::StreamingParse => {
                "Streaming parse stage: bounded chunks, per-file persist, tree drop"
            }
            Self::StreamingPdg => {
                "Compact PDG persistence: per-file fragments to CAS, no whole-PDG clone"
            }
            Self::StreamingTfidf => "Streaming two-pass TF-IDF with direct CAS-staged row writes",
            Self::StreamingNeural => {
                "Streaming neural enrichment via NeuralRowWriter (kills Vec accumulation)"
            }
            Self::GlobalEmbedCache => {
                "Global content-addressed embedding cache with cross-project dedup"
            }
            Self::CommunityDetection => {
                "Leiden community detection over the PDG (project_map grouping, impact boundaries)"
            }
            Self::PrecisionIngest => {
                "SCIP precision ingestion (default on) with silent Tier-0 fallback when no indexer is available; set LEINDEX_FEATURE_PRECISION_INGEST=false to disable"
            }
            Self::DebugEscapeHatch => {
                "Embedding-cache debug escape hatch: store source text alongside rows"
            }
            Self::ValidatedModel => {
                "Use WS11 validated model profile (CodeRankEmbed-INT8, no reranker)"
            }
            Self::Engram => {
                "Engram: persistent phrase-book of neural query embeddings (repeat queries skip the embedder)"
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

/// Environment variable name for the legacy fallback umbrella switch (phase 9).
///
/// When set to "1", "true", "yes", or "on", ALL rollout feature flags are
/// forced to their OFF state (legacy behavior). This is the phase-9 fallback
/// window mechanism: `LEINDEX_LEGACY=1` lets users opt back to the legacy
/// v1.9.x code paths without downgrading.
///
/// VAL-ROLLOUT-013: legacy paths reachable during fallback window.
pub const LEGACY_ENV: &str = "LEINDEX_LEGACY";

/// Returns `true` when the legacy fallback umbrella is active (`LEINDEX_LEGACY=1`).
///
/// When active, `FeatureFlag::is_enabled()` returns `false` for every rollout
/// flag, reverting the system to legacy v1.9.x behavior. This is a
/// convenience switch so users don't have to set each `LEINDEX_FEATURE_*=0`
/// individually during the phase-9 fallback window.
pub fn legacy_mode_enabled() -> bool {
    match env::var(LEGACY_ENV) {
        Ok(v) => matches!(
            v.to_lowercase().as_str(),
            "1" | "true" | "yes" | "on" | "enable" | "enabled"
        ),
        Err(_) => false,
    }
}

/// The set of rollout flags that `LEINDEX_LEGACY=1` reverts to legacy behavior.
///
/// GA features (`NeuralSearch`, `StreamingMcp`, `GlobalAutoSync`) are NOT
/// included: they were production before v2.0.0 and the legacy fallback applies
/// only to v2.0.0 resource-architecture flags.
const LEGACY_REVERTIBLE_FLAGS: &[FeatureFlag] = &[
    FeatureFlag::DaemonClient,
    FeatureFlag::GenerationReaders,
    FeatureFlag::BoundedScheduler,
    FeatureFlag::StreamingScan,
    FeatureFlag::StreamingParse,
    FeatureFlag::StreamingPdg,
    FeatureFlag::StreamingTfidf,
    FeatureFlag::StreamingNeural,
    FeatureFlag::GlobalEmbedCache,
    FeatureFlag::CommunityDetection,
    FeatureFlag::PrecisionIngest,
    FeatureFlag::ValidatedModel,
];

/// Check whether a given flag is one that `LEINDEX_LEGACY=1` would revert.
pub fn is_legacy_revertible(flag: &FeatureFlag) -> bool {
    LEGACY_REVERTIBLE_FLAGS.contains(flag)
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
            FeatureFlag::DaemonClient,
            FeatureFlag::GenerationMigration,
            FeatureFlag::GenerationReaders,
            FeatureFlag::BoundedScheduler,
            FeatureFlag::StreamingScan,
            FeatureFlag::StreamingParse,
            FeatureFlag::StreamingPdg,
            FeatureFlag::StreamingTfidf,
            FeatureFlag::StreamingNeural,
            FeatureFlag::GlobalEmbedCache,
            FeatureFlag::DebugEscapeHatch,
            FeatureFlag::ValidatedModel,
            FeatureFlag::CommunityDetection,
            FeatureFlag::PrecisionIngest,
            FeatureFlag::Engram,
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
    let mut guard = TEST_OVERRIDES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard.get_or_insert_with(HashMap::new).insert(flag, value);
}

/// Test-only: clear all overrides, restoring env-derived flag behavior.
#[doc(hidden)]
pub fn clear_flag_overrides_for_test() {
    let mut guard = TEST_OVERRIDES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}

/// Serializes flag-override toggling across tests in one process.
///
/// `set_flag_override_for_test`/`clear_flag_overrides_for_test` mutate a
/// process-global store, so tests that exercise a flag on both sides must hold
/// this lock for their whole duration to avoid racing sibling tests.
#[doc(hidden)]
pub static FLAG_TEST_LOCK: Mutex<()> = Mutex::new(());

/// Take [`FLAG_TEST_LOCK`], ignoring poison.
///
/// A test that panics while holding the lock poisons it; with a plain
/// `lock().unwrap()` every later test that takes the lock then fails too, so
/// one failure (or one timing flake) cascades into unrelated ones. The guarded
/// state is just the override map, which each test resets itself, so poison
/// carries no information worth propagating.
#[doc(hidden)]
pub fn lock_flag_tests() -> std::sync::MutexGuard<'static, ()> {
    FLAG_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Clears all overrides when dropped, including on unwind.
#[doc(hidden)]
pub struct FlagOverrideReset;

impl Drop for FlagOverrideReset {
    fn drop(&mut self) {
        clear_flag_overrides_for_test();
    }
}

/// Run `f` with `flag` overridden to `value`, restoring the env-derived state
/// afterwards. Serialized by [`FLAG_TEST_LOCK`] so tests never race the shared
/// override store.
#[doc(hidden)]
pub fn with_flag_override(flag: FeatureFlag, value: bool, f: impl FnOnce()) {
    let _g = lock_flag_tests();
    // Reset even if `f` panics, so a failing test cannot leak its override
    // into the next one.
    let _reset = FlagOverrideReset;
    set_flag_override_for_test(flag, value);
    f();
}

fn flag_store() -> &'static FlagStore {
    FLAG_STORE.get_or_init(FlagStore::new)
}

/// Log the state of all feature flags at startup (spec §12.3).
///
/// Each flag is logged with its name, env-var, and effective ON/OFF state.
/// Called from the daemon and the inline server entry points so the user can
/// audit which workstream features are active in their deployment. The roll
/// out plan (§12.3) requires flag state to be visible at daemon start.
pub fn log_flag_state() {
    let flags = all_flags();
    let active: Vec<&FeatureFlag> = flags
        .iter()
        .filter(|(_, enabled)| *enabled)
        .map(|(flag, _)| flag)
        .collect();
    let inactive_count = flags.len() - active.len();
    let legacy = legacy_mode_enabled();
    tracing::info!(
        "Feature flags at startup: {} active, {} inactive{}. \
         Active: [{}]",
        active.len(),
        inactive_count,
        if legacy {
            " (LEINDEX_LEGACY=1 — v2.0.0 rollout reverted)"
        } else {
            ""
        },
        active
            .iter()
            .map(|f| format!(
                "{} ({})",
                f.env_var()
                    .strip_prefix("LEINDEX_FEATURE_")
                    .unwrap_or(f.env_var()),
                f.is_enabled()
            ))
            .collect::<Vec<_>>()
            .join(", "),
    );
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
        FeatureFlag::DaemonClient,
        FeatureFlag::GenerationMigration,
        FeatureFlag::GenerationReaders,
        FeatureFlag::BoundedScheduler,
        FeatureFlag::StreamingScan,
        FeatureFlag::StreamingParse,
        FeatureFlag::StreamingPdg,
        FeatureFlag::StreamingTfidf,
        FeatureFlag::StreamingNeural,
        FeatureFlag::GlobalEmbedCache,
        FeatureFlag::CommunityDetection,
        FeatureFlag::PrecisionIngest,
        FeatureFlag::DebugEscapeHatch,
        FeatureFlag::ValidatedModel,
        FeatureFlag::Engram,
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
    fn test_engram_is_opt_in_and_listed() {
        // New capabilities default OFF (AGENTS.md progressive rollout).
        assert!(!FeatureFlag::Engram.default_value());
        assert_eq!(FeatureFlag::Engram.env_var(), "LEINDEX_FEATURE_ENGRAM");
        assert!(all_flags().iter().any(|(f, _)| *f == FeatureFlag::Engram));
        assert!(
            all_flags()
                .iter()
                .any(|(f, _)| *f == FeatureFlag::CommunityDetection)
        );
    }

    #[test]
    fn test_precision_ingest_defaults_on_with_kill_switch() {
        // Precision ingest defaults ON: without a discoverable external
        // indexer it degrades silently to Tier-0, so the default is safe.
        // The flag remains a rollout-KILL (explicit false disables).
        assert_eq!(
            FeatureFlag::PrecisionIngest.env_var(),
            "LEINDEX_FEATURE_PRECISION_INGEST"
        );
        assert!(FeatureFlag::PrecisionIngest.default_value());
    }

    #[test]
    fn test_generation_readers_now_default_on_after_rollout() {
        // v2.0.0 rollout phase 8: all section 16 acceptance gates pass.
        // GenerationReaders now defaults ON as the production read-path.
        // The flag acts as a rollout-KILL: explicit "0"/"false" reverts to
        // the legacy heap-mirror read-path for scoped rollback.
        assert_eq!(
            FeatureFlag::GenerationReaders.env_var(),
            "LEINDEX_FEATURE_GENERATION_READERS"
        );
        assert!(FeatureFlag::GenerationReaders.default_value());
    }

    #[test]
    fn test_debug_escape_hatch_default_off_and_named_correctly() {
        // The privacy escape hatch for the embedding cache MUST default OFF.
        // Source text must never be persisted unless the user explicitly opts
        // in via the env var, the feature flag, or CacheConfig::debug_mode.
        assert_eq!(
            FeatureFlag::DebugEscapeHatch.env_var(),
            "LEINDEX_FEATURE_EMBED_CACHE_DEBUG"
        );
        assert!(!FeatureFlag::DebugEscapeHatch.default_value());
        // is_enabled reads from the cached FlagStore, which reflects unset env
        // as default_value(). Other tests may have set the override so we use
        // default_value() to confirm the production default.
    }

    #[test]
    fn test_debug_escape_hatch_override_toggles_state() {
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        assert!(!FeatureFlag::DebugEscapeHatch.is_enabled());
        set_flag_override_for_test(FeatureFlag::DebugEscapeHatch, true);
        assert!(FeatureFlag::DebugEscapeHatch.is_enabled());
        clear_flag_overrides_for_test();
        assert!(!FeatureFlag::DebugEscapeHatch.is_enabled());
    }

    #[test]
    fn test_override_toggles_flag() {
        // Serialize the shared override store across this test only.
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        assert!(FeatureFlag::GenerationReaders.is_enabled());
        set_flag_override_for_test(FeatureFlag::GenerationReaders, false);
        assert!(!FeatureFlag::GenerationReaders.is_enabled());
        clear_flag_overrides_for_test();
        assert!(FeatureFlag::GenerationReaders.is_enabled());
    }

    #[test]
    fn test_validated_model_flag_default_on_after_rollout() {
        // v2.0.0 rollout phase 8: all section 16 acceptance gates pass.
        // The WS11 validated model profile (CodeRankEmbed INT8, no reranker)
        // is now the production default. The flag acts as a rollout-KILL.
        assert_eq!(
            FeatureFlag::ValidatedModel.env_var(),
            "LEINDEX_FEATURE_VALIDATED_MODEL"
        );
        assert!(FeatureFlag::ValidatedModel.default_value());
    }

    #[test]
    fn test_validated_model_override_toggles() {
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        assert!(FeatureFlag::ValidatedModel.is_enabled());
        set_flag_override_for_test(FeatureFlag::ValidatedModel, false);
        assert!(!FeatureFlag::ValidatedModel.is_enabled());
        clear_flag_overrides_for_test();
        assert!(FeatureFlag::ValidatedModel.is_enabled());
    }

    /// VAL-ROLLOUT-012 (phase 8): after all section 16 acceptance gates pass,
    /// the shipped v2.0.0 state flips all rollout flags to default ON. Each
    /// flag acts as a rollout-KILL: explicit "false" reverts to legacy behavior
    /// for scoped rollback. This asserts the post-flip shipped state, NOT the
    /// initial VAL-ROLLOUT-001 state (which the default_value() doc and
    /// test_rollout_flags_default_off_with_legacy_mode cover).
    #[test]
    fn test_rollout_flags_default_on_after_gates_passed() {
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        clear_flag_overrides_for_test();
        let rollout_flags = [
            FeatureFlag::DaemonClient,
            FeatureFlag::GenerationReaders,
            FeatureFlag::BoundedScheduler,
            FeatureFlag::StreamingPdg,
            FeatureFlag::GlobalEmbedCache,
            FeatureFlag::ValidatedModel,
        ];
        for flag in &rollout_flags {
            assert!(
                flag.default_value(),
                "{} should default ON after gates pass (VAL-ROLLOUT-012 rollout-KILL semantics)",
                flag.env_var()
            );
        }
    }

    /// PR-86 consumer flags are opt-in: each ships default OFF in the same
    /// commit that wires its consumer, so flags-off equals legacy behavior.
    #[test]
    fn test_pr86_consumer_flags_default_off() {
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        clear_flag_overrides_for_test();
        for flag in [
            FeatureFlag::StreamingScan,
            FeatureFlag::StreamingParse,
            FeatureFlag::StreamingTfidf,
            FeatureFlag::StreamingNeural,
        ] {
            assert!(!flag.default_value(), "{} must default OFF", flag.env_var());
        }
    }

    #[test]
    fn test_daemon_client_flag_added_and_on_after_rollout() {
        assert_eq!(
            FeatureFlag::DaemonClient.env_var(),
            "LEINDEX_FEATURE_DAEMON_CLIENT"
        );
        assert!(FeatureFlag::DaemonClient.default_value());
    }

    #[test]
    fn test_daemon_client_override_toggles() {
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        assert!(FeatureFlag::DaemonClient.is_enabled());
        set_flag_override_for_test(FeatureFlag::DaemonClient, false);
        assert!(!FeatureFlag::DaemonClient.is_enabled());
        clear_flag_overrides_for_test();
        assert!(FeatureFlag::DaemonClient.is_enabled());
    }

    // ── Phase 9 legacy fallback tests (VAL-ROLLOUT-013) ──────────────

    /// VAL-ROLLOUT-001 (initial OFF default) is provable at runtime: the v2.0.0
    /// rollback umbrella `LEINDEX_LEGACY=1` reverts ALL 10 v2.0.0 rollout flags
    /// to OFF (legacy v1.9.x behavior), demonstrating that the flag mechanism
    /// genuinely supports OFF defaults even though the shipped state flips them
    /// ON after gates pass (VAL-ROLLOUT-012).
    #[test]
    fn test_rollout_flags_default_off_with_legacy_mode() {
        // Save and restore the env var since this is process-global.
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        clear_flag_overrides_for_test();
        let prev = env::var(LEGACY_ENV).ok();

        // SAFETY: single-threaded test under FLAG_TEST_LOCK.
        unsafe { env::set_var(LEGACY_ENV, "1") };

        // NeuralSearch and StreamingMcp are GA features that predate v2.0.0.
        // They should NOT be affected by LEINDEX_LEGACY.
        assert!(
            FeatureFlag::NeuralSearch.is_enabled(),
            "GA features unaffected by LEINDEX_LEGACY"
        );

        // v2.0.0 rollout flags should all be OFF under LEINDEX_LEGACY=1,
        // proving the flag mechanism supports OFF defaults (VAL-ROLLOUT-001).
        for flag in [
            FeatureFlag::DaemonClient,
            FeatureFlag::GenerationReaders,
            FeatureFlag::BoundedScheduler,
            FeatureFlag::StreamingScan,
            FeatureFlag::StreamingParse,
            FeatureFlag::StreamingPdg,
            FeatureFlag::StreamingTfidf,
            FeatureFlag::StreamingNeural,
            FeatureFlag::GlobalEmbedCache,
            FeatureFlag::ValidatedModel,
            FeatureFlag::CommunityDetection,
            FeatureFlag::PrecisionIngest,
        ] {
            assert!(
                !flag.is_enabled(),
                "{} should be OFF under LEINDEX_LEGACY=1",
                flag.env_var()
            );
        }

        // Restore.
        // SAFETY: single-threaded test under FLAG_TEST_LOCK.
        unsafe {
            match &prev {
                Some(v) => env::set_var(LEGACY_ENV, v),
                None => env::remove_var(LEGACY_ENV),
            }
        }
        clear_flag_overrides_for_test();
    }

    /// `LEINDEX_LEGACY=0` (or unset) leaves rollout flags at default-on.
    #[test]
    fn test_legacy_unset_keeps_default_on() {
        let _g = FLAG_TEST_LOCK.lock().unwrap();
        clear_flag_overrides_for_test();
        let prev = env::var(LEGACY_ENV).ok();

        // SAFETY: single-threaded test under FLAG_TEST_LOCK.
        unsafe { env::remove_var(LEGACY_ENV) };

        assert!(!legacy_mode_enabled());
        assert!(FeatureFlag::GenerationReaders.is_enabled());
        assert!(FeatureFlag::ValidatedModel.is_enabled());

        // Setting to 0 also keeps defaults.
        // SAFETY: single-threaded test under FLAG_TEST_LOCK.
        unsafe { env::set_var(LEGACY_ENV, "0") };
        assert!(!legacy_mode_enabled());

        // SAFETY: single-threaded test under FLAG_TEST_LOCK.
        unsafe {
            match &prev {
                Some(v) => env::set_var(LEGACY_ENV, v),
                None => env::remove_var(LEGACY_ENV),
            }
        }
        clear_flag_overrides_for_test();
    }

    /// `is_legacy_revertible` covers all v2.0.0 rollout flags.
    #[test]
    fn test_is_legacy_revertible() {
        assert!(is_legacy_revertible(&FeatureFlag::DaemonClient));
        assert!(is_legacy_revertible(&FeatureFlag::ValidatedModel));
        // GA features are NOT legacy-revertible.
        assert!(!is_legacy_revertible(&FeatureFlag::NeuralSearch));
        // Debug escape hatch is not a rollout flag.
        assert!(!is_legacy_revertible(&FeatureFlag::DebugEscapeHatch));
    }
}
