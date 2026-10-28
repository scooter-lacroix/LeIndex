use super::*;

#[test]
fn test_default_config_round_trip() {
    let config = LeIndexConfig::default();
    let toml_str = toml::to_string(&config).unwrap();
    let parsed: LeIndexConfig = toml::from_str(&toml_str).unwrap();
    assert_eq!(config, parsed);
}

#[test]
fn test_fresh_neural_default_and_legacy_toml_boundary() {
    let fresh = LeIndexConfig::default();
    assert!(fresh.neural.enabled);
    assert!(fresh.indexing.embeddings.enable_hybrid);
    assert_eq!(
        fresh.indexing.embeddings.remote_preset_label(),
        "embeddinggemma-2"
    );
    for existing in [
        "",
        "[neural]\nexecution_provider = \"cpu\"\n",
        "[neural]\nenabled = false\n",
    ] {
        let parsed: LeIndexConfig = toml::from_str(existing).expect("existing TOML");
        assert!(!parsed.neural.enabled);
    }
    let explicit: LeIndexConfig =
        toml::from_str("[neural]\nenabled = true\n").expect("opt-in TOML");
    assert!(explicit.neural.enabled);
}

#[test]
fn test_neural_config_schema() {
    let config = NeuralConfig {
        enabled: true,
        execution_provider: "cpu".to_string(),
        ort_dylib_path: Some("/usr/local/lib/libonnxruntime.so".to_string()),
        ort_version: Some("1.25.0".to_string()),
        model_dir: "/home/user/.leindex/models".to_string(),
        model_name: "qwen3-embed-0.6b-dynamic-uint8".to_string(),
    };

    let toml_str = toml::to_string(&config).unwrap();
    assert!(toml_str.contains("enabled = true"));
    assert!(toml_str.contains("execution_provider = \"cpu\""));
    assert!(toml_str.contains("ort_dylib_path"));
    assert!(toml_str.contains("ort_version"));
    assert!(toml_str.contains("model_dir"));
}

#[test]
fn test_parse_malformed_returns_error() {
    let bad_toml = "[neural\nenabled = true\n";
    let result = LeIndexConfig::parse_toml(bad_toml);
    assert!(result.is_err());
}

#[test]
fn test_config_roundtrip_preserves_rerank_fields() {
    let mut config = LeIndexConfig::default();
    config.search.rerank_enabled = true;
    config.search.rerank_top_n = 80;
    config.search.rerank_backend = "system-one".to_string();
    let decoded: LeIndexConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert!(decoded.search.rerank_enabled);
    assert_eq!(decoded.search.rerank_top_n, 80);
    assert_eq!(decoded.search.rerank_backend, "system-one");
}

#[test]
fn test_rerank_backend_semantics() {
    // Default: onnx, rerank off unless rerank_enabled.
    let mut config = LeIndexConfig::default();
    assert_eq!(config.search.rerank_backend, "onnx");
    assert!(!config.search.rerank_effectively_enabled());
    // onnx + rerank_enabled => rerank on.
    config.search.rerank_enabled = true;
    assert!(config.search.rerank_effectively_enabled());
    // system-one implies rerank regardless of rerank_enabled — but only when
    // the backend is constructible: the flag must be on AND the feature
    // compiled in. A config asking for it otherwise reports OFF, matching
    // the dispatch, which refuses to construct the reranker.
    //
    // Driven through `with_flag_override`, which takes the test lock and
    // restores the previous state on unwind: a failing assertion here must
    // not leak a `SystemOne = true` override into every later test in the
    // crate. Not nested — the lock is not reentrant — so the two sides are
    // sequential, and the config is mutated in between while the lock is
    // free.
    config.search.rerank_backend = "system-one".to_string();
    config.search.rerank_enabled = false;
    crate::feature_flags::with_flag_override(
        crate::feature_flags::FeatureFlag::SystemOne,
        false,
        || {
            assert!(
                !config.search.rerank_effectively_enabled(),
                "system-one without LEINDEX_FEATURE_SYSTEM_ONE cannot rerank"
            );
        },
    );
    crate::feature_flags::with_flag_override(
        crate::feature_flags::FeatureFlag::SystemOne,
        true,
        || {
            assert_eq!(
                config.search.rerank_effectively_enabled(),
                cfg!(feature = "system-one"),
                "available system-one reranks regardless of rerank_enabled; a build \
                     without the feature still cannot"
            );
        },
    );
    // off is an explicit no-rerank even with rerank_enabled set.
    config.search.rerank_backend = "off".to_string();
    config.search.rerank_enabled = true;
    assert!(!config.search.rerank_effectively_enabled());
    // Unknown strings are treated as the onnx default (rerank_enabled gates).
    config.search.rerank_backend = "bogus".to_string();
    assert!(config.search.rerank_effectively_enabled());
}

#[test]
fn test_default_execution_provider_is_auto() {
    assert_eq!(LeIndexConfig::default().neural.execution_provider, "auto");
}

#[test]
fn test_default_neural_weight_is_0_4() {
    // VAL-CONFIG: config default must match the scorer-side defaults
    // (HybridScorer::for_code / HybridScoringWeights both use 0.40).
    assert_eq!(LeIndexConfig::default().search.neural_weight, 0.4);
}

#[test]
fn test_config_missing_keys_uses_defaults() {
    // VAL-SETUP-030: stale config from older version gets defaults for new keys
    let toml_str = "[neural]\nenabled = true\n";
    let config: LeIndexConfig = toml::from_str(toml_str).unwrap();
    assert!(config.neural.enabled);
    assert_eq!(config.search.search_mode, "hybrid");
    assert_eq!(config.search.neural_weight, 0.4);
    assert_eq!(config.indexing.batch_size, 500);
    assert_eq!(config.neural.model_name, "qwen3-embed-0.6b-dynamic-uint8");
}

#[test]
fn test_config_empty_uses_defaults() {
    let config: LeIndexConfig = toml::from_str("").unwrap();
    assert!(!config.neural.enabled);
    assert_eq!(config.search.search_mode, "hybrid");
    assert_eq!(config.search.neural_weight, 0.4);
}

#[test]
fn test_fragment_config_defaults() {
    let config = LeIndexConfig::default();
    assert!(!config.search.fragment_index_enabled);
    assert_eq!(config.search.fragment_max_bytes, 12_000);
    assert_eq!(config.search.fragment_weight, 0.35);
    assert!(config.search.fragment_orphan_enabled);
    assert!(config.search.fragment_naive_fallback);

    // Existing TOML retains legacy embeddings; search defaults are unchanged.
    let parsed: LeIndexConfig = toml::from_str("").unwrap();
    assert_eq!(parsed.search, config.search);
    assert!(!parsed.indexing.embeddings.enable_hybrid);
    assert_eq!(parsed.indexing.embeddings.remote_preset_label(), "custom");
    assert!(config.indexing.embeddings.enable_hybrid);
    assert_eq!(
        config.indexing.embeddings.remote_preset_label(),
        "embeddinggemma-2"
    );
}

#[test]
fn test_fragment_config_round_trip() {
    let mut config = LeIndexConfig::default();
    config.search.fragment_index_enabled = true;
    config.search.fragment_max_bytes = 24_000;
    config.search.fragment_weight = 0.20;
    config.search.fragment_orphan_enabled = false;
    config.search.fragment_naive_fallback = false;
    let decoded: LeIndexConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(config, decoded);
}

#[test]
fn test_full_config_round_trip() {
    let config = LeIndexConfig {
        neural: NeuralConfig {
            enabled: true,
            execution_provider: "migraphx".to_string(),
            ort_dylib_path: Some("/usr/local/lib/libonnxruntime.so.1.25.0".to_string()),
            ort_version: Some("1.25.0".to_string()),
            model_dir: "/home/user/.leindex/models".to_string(),
            model_name: "qwen3-embed-0.6b".to_string(),
        },
        search: SearchConfig {
            search_mode: "hybrid".to_string(),
            neural_weight: 0.35,
            rerank_enabled: true,
            rerank_top_n: 80,
            rerank_backend: "onnx".to_string(),
            fragment_index_enabled: false,
            fragment_max_bytes: 12_000,
            fragment_weight: 0.35,
            fragment_orphan_enabled: true,
            fragment_naive_fallback: true,
        },
        indexing: IndexingConfig {
            batch_size: 1000,
            max_files: 100_000,
            embeddings: EmbeddingConfig::default(),
        },
        mcp: McpConfig {
            idle_timeout_secs: 3600,
            engine_max_idle_secs: 1200,
            prewarm: "graph".to_string(),
            auto_index: false,
        },
        system_one: SystemOneConfig::default(),
        services: std::collections::BTreeMap::new(),
    };
    let toml_str = toml::to_string_pretty(&config).unwrap();
    let parsed: LeIndexConfig = toml::from_str(&toml_str).unwrap();
    assert_eq!(config, parsed);
}

#[test]
fn test_mcp_config_defaults() {
    // Memory-pressure remediation 1.11.0 (D-1/D-2).
    let config = LeIndexConfig::default();
    assert_eq!(config.mcp.idle_timeout_secs, 1800);
    assert_eq!(config.mcp.engine_max_idle_secs, 600);
    assert_eq!(config.mcp.prewarm, "full");

    // Parse from empty TOML -> same defaults (backward compatible: existing
    // configs without an [mcp] section keep working).
    let parsed: LeIndexConfig = toml::from_str("").unwrap();
    assert_eq!(parsed.mcp, McpConfig::default());
    let legacy = "[neural]\nenabled = true\n";
    let parsed_legacy: LeIndexConfig = toml::from_str(legacy).unwrap();
    assert_eq!(parsed_legacy.mcp, McpConfig::default());
}

#[test]
fn test_mcp_config_round_trip() {
    let mut config = LeIndexConfig::default();
    config.mcp.idle_timeout_secs = 60;
    config.mcp.engine_max_idle_secs = 0;
    let decoded: LeIndexConfig = toml::from_str(&toml::to_string(&config).unwrap()).unwrap();
    assert_eq!(config.mcp, decoded.mcp);
}

#[test]
fn test_ort_dylib_path_skip_serializing_if_none() {
    let config = NeuralConfig {
        enabled: true,
        execution_provider: "cpu".to_string(),
        ort_dylib_path: None,
        ort_version: None,
        model_dir: "/models".to_string(),
        model_name: "qwen3-embed-0.6b".to_string(),
    };
    let toml_str = toml::to_string(&config).unwrap();
    assert!(!toml_str.contains("ort_dylib_path"));
}

/// Serializes env-var-mutating tests across the whole lib test binary, not
/// just this module. `LEINDEX_HOME` is process-global, and so is the config
/// cache these tests populate: a module-local lock let a config test install
/// a cached config rooted at its own tempdir while `cli::mcp::search_handler`
/// read it through `LeIndexConfig::load_cached()`.
use crate::test_env::EnvLock as ENV_LOCK;

#[test]
fn test_load_or_recover_corrupt_file() {
    let _g = ENV_LOCK.lock().unwrap();
    let tmp = tempfile::tempdir().unwrap();
    let config_path = tmp.path().join("config").join("leindex.toml");
    std::fs::create_dir_all(config_path.parent().unwrap()).unwrap();
    std::fs::write(&config_path, "[neural\nbroken toml").unwrap();
    // SAFETY: env mutation serialized by ENV_LOCK; single-threaded under the lock.
    unsafe { std::env::set_var(LEINDEX_HOME_ENV, tmp.path()) };
    let (config, action) = LeIndexConfig::load_or_recover().unwrap();
    assert!(matches!(action, RecoveryAction::RecoveredFromCorrupt(_)));
    assert!(!config.neural.enabled);
    assert!(config_path.with_extension("toml.bak").exists());
    unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
}

#[test]
fn test_legacy_toml_migrates_to_v2_yaml_without_dropping_sections() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let legacy_path = config_dir.join("leindex.toml");
    std::fs::write(
        &legacy_path,
        "[neural]\nenabled = false\n\n[indexing.embeddings]\nremote_provider = \"custom\"\n",
    )
    .unwrap();
    unsafe { std::env::set_var(LEINDEX_HOME_ENV, tmp.path()) };

    let (config, action) = LeIndexConfig::load_or_recover().unwrap();
    assert!(!config.neural.enabled);
    assert!(matches!(action, RecoveryAction::MigratedFromLegacy(_)));
    let yaml_path = config_dir.join("config.yaml");
    assert!(yaml_path.exists());
    assert!(!legacy_path.exists());
    assert!(legacy_path.with_extension("toml.migrated").exists());
    let yaml = std::fs::read_to_string(&yaml_path).unwrap();
    assert!(yaml.contains("dal_settings:"));
    assert!(yaml.contains("vector_store:"));
    assert!(yaml.contains("services:"));
    assert!(yaml.contains("  laya:"));
    assert!(yaml.contains("  gemma:"));
    assert!(yaml.contains("remote_provider: custom"));

    let (_, second_action) = LeIndexConfig::load_or_recover().unwrap();
    assert!(matches!(second_action, RecoveryAction::Loaded));
    unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
}
#[test]
fn test_config_load_returns_default_when_missing() {
    let _g = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var(LEINDEX_HOME_ENV, "/nonexistent/path/for/testing") };
    let (config, action) = LeIndexConfig::load_or_recover().unwrap();
    assert!(matches!(action, RecoveryAction::CreatedDefault));
    assert!(config.neural.enabled);
    unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
}

#[test]
fn test_resolve_leindex_home_env_override() {
    let _g = ENV_LOCK.lock().unwrap();
    unsafe { std::env::set_var(LEINDEX_HOME_ENV, "/custom/leindex") };
    assert_eq!(
        resolve_leindex_home(),
        Some(PathBuf::from("/custom/leindex"))
    );
    unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
}

fn service(
    name: &str,
    host: &str,
    port: u16,
    model: Option<&str>,
    alias: Option<&str>,
) -> (String, ServiceConfig) {
    (
        name.to_string(),
        ServiceConfig {
            host: host.to_string(),
            port,
            model: model.map(str::to_string),
            alias: alias.map(str::to_string),
            ..ServiceConfig::default()
        },
    )
}

#[test]
fn test_service_base_url_and_served_name() {
    let (_, laya) = service(
        "laya",
        "127.0.0.1",
        8181,
        Some("/models/laya-bf16.gguf"),
        None,
    );
    assert_eq!(laya.base_url(), "http://127.0.0.1:8181");
    // No alias: served name falls back to the model file stem.
    assert_eq!(laya.served_name().as_deref(), Some("laya-bf16"));
    let (_, aliased) = service("e", "127.0.0.1", 8080, None, Some("embeddinggemma-2"));
    assert_eq!(aliased.served_name().as_deref(), Some("embeddinggemma-2"));
}

/// A resolved route must carry the provider's per-request input ceiling, or
/// the index builder sends a throughput-tuned batch into an API that rejects
/// it: Gemini answers anything over 100 with
/// `BatchEmbedContentsRequest.requests: at most 100 requests can be in one
/// batch`, which 128-document batches hit on every request.
#[test]
fn test_resolved_remote_carries_the_preset_batch_limit() {
    let services = std::collections::BTreeMap::new();
    let gemini = EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: "gemini".into(),
        ..EmbeddingConfig::default()
    };
    assert_eq!(
        gemini.resolved_remote_with(&services).unwrap().batch_limit,
        Some(100),
        "the gemini preset must cap a request at the API's documented maximum"
    );
    // An explicit override wins over the preset either way.
    let lowered = EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: "gemini".into(),
        remote_batch_limit: Some(25),
        ..EmbeddingConfig::default()
    };
    assert_eq!(
        lowered.resolved_remote_with(&services).unwrap().batch_limit,
        Some(25)
    );
    // A provider with no preset and no override resolves no cap rather than
    // inventing one: an over-large guess fails whole batches.
    let custom = EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: "custom".into(),
        remote_endpoint: Some("http://127.0.0.1:9/v1".into()),
        ..EmbeddingConfig::default()
    };
    assert_eq!(
        custom.resolved_remote_with(&services).unwrap().batch_limit,
        None
    );
}

#[test]
fn test_resolved_remote_endpoint_ladder() {
    let mut services: std::collections::BTreeMap<String, ServiceConfig> =
        std::collections::BTreeMap::new();
    services.insert(
        "embed".to_string(),
        ServiceConfig {
            port: 8123,
            ..ServiceConfig::default()
        },
    );
    // Service derives the endpoint when no explicit one is set.
    let via_service = EmbeddingConfig {
        enable_hybrid: true,
        remote_service: Some("embed".into()),
        ..EmbeddingConfig::default()
    };
    let resolved = via_service.resolved_remote_with(&services).unwrap();
    assert_eq!(resolved.endpoint, "http://127.0.0.1:8123/v1");
    // Explicit endpoint beats the service.
    let explicit_wins = EmbeddingConfig {
        enable_hybrid: true,
        remote_service: Some("embed".into()),
        remote_endpoint: Some("http://explicit:9/v1".into()),
        ..EmbeddingConfig::default()
    };
    assert_eq!(
        explicit_wins
            .resolved_remote_with(&services)
            .unwrap()
            .endpoint,
        "http://explicit:9/v1"
    );
    // Preset default applies when neither is set.
    let preset_only = EmbeddingConfig {
        enable_hybrid: true,
        ..EmbeddingConfig::default()
    };
    assert_eq!(
        preset_only
            .resolved_remote_with(&services)
            .unwrap()
            .endpoint,
        EMBEDDINGGEMMA2_DEFAULT_ENDPOINT
    );
}

#[test]
fn test_resolved_remote_model_falls_back_to_service() {
    let mut services: std::collections::BTreeMap<String, ServiceConfig> =
        std::collections::BTreeMap::new();
    services.insert(
        "embed".to_string(),
        ServiceConfig {
            model: Some("/models/embeddinggemma-2-Q8_0.gguf".into()),
            ..ServiceConfig::default()
        },
    );
    let cfg = EmbeddingConfig {
        enable_hybrid: true,
        remote_service: Some("embed".into()),
        remote_provider: "custom".into(),
        ..EmbeddingConfig::default()
    };
    // Custom preset (no model fill) + no remote_model -> service stem.
    assert_eq!(
        cfg.resolved_remote_with(&services)
            .unwrap()
            .model
            .as_deref(),
        Some("embeddinggemma-2-Q8_0")
    );
}

/// A `custom` provider that names an EmbeddingGemma2 model must get the
/// task templates. It has no preset row, so `remote_model` is the only
/// identification available; reading only `remote_provider` left both
/// templates `None` and embedded the whole corpus with no task prefix.
#[test]
fn test_custom_provider_serving_embeddinggemma2_gets_task_templates() {
    let cfg = EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: "custom".into(),
        remote_endpoint: Some("https://api.example.com/v1".into()),
        remote_model: Some("embeddinggemma-2".into()),
        ..EmbeddingConfig::default()
    };
    let resolved = cfg.resolved_remote_with(&Default::default()).unwrap();
    assert_eq!(
        resolved.doc_template.as_deref(),
        Some(EMBEDDINGGEMMA2_DOC_TEMPLATE)
    );
    assert_eq!(
        resolved.query_template.as_deref(),
        Some(EMBEDDINGGEMMA2_QUERY_TEMPLATE)
    );
    // The label and the templates now agree instead of contradicting.
    assert_eq!(cfg.remote_preset_label(), "embeddinggemma-2");

    // Host spellings: vendor prefix and a quantized file stem.
    for model in [
        "text-embedding-embeddinggemma-2",
        "embeddinggemma2",
        "embeddinggemma-2-Q8_0",
    ] {
        let spelled = EmbeddingConfig {
            remote_model: Some(model.into()),
            ..cfg.clone()
        };
        assert!(
            spelled.embeddinggemma2_preset_active(),
            "expected {model} to select the embeddinggemma-2 templates"
        );
    }
}

/// A recognized preset owns its own templating. A contradictory
/// `remote_model` on it must NOT silently swap in Gemma templates.
#[test]
fn test_known_preset_is_not_overridden_by_model_name() {
    let cfg = EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: "openai".into(),
        remote_model: Some("embeddinggemma-2".into()),
        ..EmbeddingConfig::default()
    };
    assert!(!cfg.embeddinggemma2_preset_active());
    let resolved = cfg.resolved_remote_with(&Default::default()).unwrap();
    assert!(resolved.doc_template.is_none());
    assert!(resolved.query_template.is_none());
}

/// A `custom` provider serving some other model stays untemplated.
#[test]
fn test_custom_provider_with_unrelated_model_stays_untemplated() {
    let cfg = EmbeddingConfig {
        enable_hybrid: true,
        remote_provider: "custom".into(),
        remote_endpoint: Some("https://api.example.com/v1".into()),
        remote_model: Some("some-other-embedder".into()),
        ..EmbeddingConfig::default()
    };
    assert!(!cfg.embeddinggemma2_preset_active());
    let resolved = cfg.resolved_remote_with(&Default::default()).unwrap();
    assert!(resolved.doc_template.is_none());
    assert!(resolved.query_template.is_none());
}

#[test]
fn test_system_one_preset_overlay_swaps_provider() {
    let mut cfg = SystemOneConfig {
        provider: "laya".into(),
        mode: "enforce".into(),
        ..SystemOneConfig::default()
    };
    cfg.presets.insert(
        "openai-prod".into(),
        SystemOnePreset {
            provider: Some("openai".into()),
            model: Some("gpt-6-luna".into()),
            api_key_env: Some("OPENAI_API_KEY".into()),
            ..SystemOnePreset::default()
        },
    );
    cfg.active_preset = Some("openai-prod".into());
    let effective = cfg.effective();
    assert_eq!(effective.provider, "openai");
    assert_eq!(effective.model.as_deref(), Some("gpt-6-luna"));
    // Section-level fields survive when the preset leaves them unset.
    assert_eq!(effective.mode, "enforce");
    // Clearing the preset (or naming a missing one) reverts to laya.
    cfg.active_preset = Some("nope".into());
    assert_eq!(cfg.effective().provider, "laya");
}

#[test]
fn test_system_one_provider_change_clears_unset_provider_fields() {
    let mut cfg = SystemOneConfig {
        provider: "laya".into(),
        model: Some("local-model".into()),
        base_url: Some("http://127.0.0.1:8000".into()),
        service: Some("laya".into()),
        api_key_env: Some("LOCAL_API_KEY".into()),
        mode: "enforce".into(),
        active_preset: Some("remote".into()),
        ..SystemOneConfig::default()
    };
    cfg.presets.insert(
        "remote".into(),
        SystemOnePreset {
            provider: Some("openai".into()),
            ..SystemOnePreset::default()
        },
    );
    let effective = cfg.effective();
    assert_eq!(effective.provider, "openai");
    assert_eq!(effective.model, None);
    assert_eq!(effective.base_url, None);
    assert_eq!(effective.service, None);
    assert_eq!(effective.api_key_env, None);
    assert_eq!(effective.mode, "enforce");

    let preset = cfg.presets.get_mut("remote").unwrap();
    preset.model = Some("remote-model".into());
    preset.base_url = Some("https://remote.example".into());
    preset.service = Some("remote-service".into());
    preset.api_key_env = Some("REMOTE_API_KEY".into());
    let effective = cfg.effective();
    assert_eq!(effective.model.as_deref(), Some("remote-model"));
    assert_eq!(
        effective.base_url.as_deref(),
        Some("https://remote.example")
    );
    assert_eq!(effective.service.as_deref(), Some("remote-service"));
    assert_eq!(effective.api_key_env.as_deref(), Some("REMOTE_API_KEY"));
}

#[test]
fn test_system_one_same_provider_presets_inherit_unset_fields() {
    let mut cfg = SystemOneConfig {
        provider: "jev".into(),
        model: Some("base-model".into()),
        base_url: Some("https://base.example".into()),
        service: Some("base-service".into()),
        api_key_env: Some("BASE_API_KEY".into()),
        active_preset: Some("same".into()),
        ..SystemOneConfig::default()
    };
    for provider in [None, Some("jev"), Some(" JEV "), Some("typesafe")] {
        cfg.presets.insert(
            "same".into(),
            SystemOnePreset {
                provider: provider.map(str::to_string),
                ..SystemOnePreset::default()
            },
        );
        let effective = cfg.effective();
        assert_eq!(effective.model, cfg.model);
        assert_eq!(effective.base_url, cfg.base_url);
        assert_eq!(effective.service, cfg.service);
        assert_eq!(effective.api_key_env, cfg.api_key_env);
    }
}

#[test]
fn test_service_base_url_brackets_ipv6_host() {
    let v6 = ServiceConfig {
        host: "::1".into(),
        port: 8080,
        ..ServiceConfig::default()
    };
    assert_eq!(v6.base_url(), "http://[::1]:8080");
    let v4 = ServiceConfig {
        host: "127.0.0.1".into(),
        port: 8080,
        ..ServiceConfig::default()
    };
    assert_eq!(v4.base_url(), "http://127.0.0.1:8080");
}

/// The reload contract: a config edit must be observed by a process that
/// already cached a config. This is the regression guard for the bug where
/// `load_cached()` was a `OnceLock` and a long-lived MCP daemon kept serving
/// the startup route forever after `leindex.toml` changed.
///
/// Serialised on the module [`ENV_LOCK`] because it mutates `LEINDEX_HOME`,
/// which is process-global and read by every other config-path test.
#[test]
fn test_load_cached_observes_a_config_edit() {
    let _env = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let sandbox = cache::CacheSandbox::new();

    let home = tempfile::tempdir().expect("config home");
    let config_dir = home.path().join("config");
    std::fs::create_dir_all(&config_dir).expect("config dir");
    let config_path = config_dir.join("config.yaml");

    // SAFETY: serialised by ENV_LOCK; no other test reads LEINDEX_HOME
    // concurrently while this guard is held.
    unsafe { std::env::set_var(LEINDEX_HOME_ENV, home.path()) };

    std::fs::write(
            &config_path,
            "indexing:\n  embeddings:\n    enable_hybrid: true\n    remote_service: gemma\n    remote_provider: embeddinggemma-2\n",
        )
        .expect("initial config");

    let first = LeIndexConfig::load_cached();
    assert_eq!(
        first.indexing.embeddings.remote_service.as_deref(),
        Some("gemma"),
        "initial load must see the written config"
    );

    // Rewrite with a different route, then force the throttle open. Order
    // matters: the write must land first so the stamp read during the probe
    // sees the new file.
    std::fs::write(
            &config_path,
            "indexing:\n  embeddings:\n    enable_hybrid: true\n    remote_service: other\n    remote_provider: custom\n",
        )
        .expect("edited config");
    sandbox.reopen_throttle();

    let second = LeIndexConfig::load_cached();
    assert_eq!(
        second.indexing.embeddings.remote_service.as_deref(),
        Some("other"),
        "load_cached must observe the edit instead of serving the cached config"
    );

    // The snapshot taken before the reload must still be valid — a reload
    // must not free a config another caller is holding — and must still
    // carry the value it was loaded with.
    assert_eq!(
        first.indexing.embeddings.remote_service.as_deref(),
        Some("gemma"),
        "a snapshot taken before a reload must stay valid and unchanged"
    );

    // A malformed rewrite must NOT drop the process to defaults mid-session.
    std::fs::write(&config_path, "not: [valid yaml").expect("broken config");
    sandbox.reopen_throttle();
    let third = LeIndexConfig::load_cached();
    assert_eq!(
        third.indexing.embeddings.remote_service.as_deref(),
        Some("other"),
        "a parse failure must keep the last good config, not fall back to defaults"
    );
}

/// Route-affecting keys are exactly what earns the "caches may be cold" WARN.
#[test]
fn test_route_identity_changes_names_the_route_keys() {
    let base = LeIndexConfig::default();
    assert!(
        base.route_identity_changes(&base).is_empty(),
        "a config must not report changes against itself"
    );

    let mut changed = base.clone();
    changed.indexing.embeddings.remote_service = Some("gemma".to_string());
    let keys = base.route_identity_changes(&changed);
    assert!(
        keys.contains(&"[indexing.embeddings] remote_service"),
        "changing remote_service must be reported, got {keys:?}"
    );

    let mut model_changed = base.clone();
    model_changed.neural.model_name = "some-other-model".to_string();
    let keys = base.route_identity_changes(&model_changed);
    assert!(
        keys.contains(&"[neural] model_name"),
        "changing the local model must be reported, got {keys:?}"
    );

    // A purely cosmetic section must not look like a route change.
    let mut unrelated = base.clone();
    unrelated.mcp.idle_timeout_secs = unrelated.mcp.idle_timeout_secs.wrapping_add(1);
    assert!(
        base.route_identity_changes(&unrelated).is_empty(),
        "unrelated edits must not be reported as route changes"
    );
}

/// PR 105 (Codex P1): a cleared runtime-owned option must not resurrect
/// its stale on-disk value. `save` merges the typed config into the
/// existing YAML document; fields skipped by serialization previously
/// survived that merge, so switching from env-var auth to a stored key
/// kept the old `remote_api_key_env` — and `resolved_api_key()` kept
/// preferring the (possibly unset) variable over the newly stored key.
#[test]
fn test_save_removes_cleared_runtime_keys_and_preserves_unknown_sections() {
    let _g = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let tmp = tempfile::tempdir().unwrap();
    let config_dir = tmp.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(
            config_dir.join("config.yaml"),
            "indexing:\n  embeddings:\n    enable_hybrid: true\n    remote_provider: custom\n    remote_endpoint: http://127.0.0.1:9/v1\n    remote_api_key_env: STALE_KEY_ENV\nsystem_one:\n  active_preset: stale-preset\nvector_store:\n  backend_type: leann\n",
        )
        .unwrap();
    unsafe { std::env::set_var(LEINDEX_HOME_ENV, tmp.path()) };

    let mut config = LeIndexConfig::load().unwrap();
    config.indexing.embeddings.remote_api_key = Some("sk-fresh".to_string());
    config.indexing.embeddings.remote_api_key_env = None;
    config.system_one.active_preset = None;
    config.save().unwrap();

    let yaml = std::fs::read_to_string(config_dir.join("config.yaml")).unwrap();
    assert!(
        !yaml.contains("STALE_KEY_ENV"),
        "cleared env-var key resurrected: {yaml}"
    );
    assert!(
        !yaml.contains("stale-preset"),
        "cleared preset resurrected: {yaml}"
    );
    assert!(yaml.contains("sk-fresh"), "stored key lost: {yaml}");
    assert!(
        yaml.contains("vector_store"),
        "unknown section dropped: {yaml}"
    );
    assert!(
        yaml.contains("backend_type: leann"),
        "unknown section emptied: {yaml}"
    );

    // The reloaded config resolves auth from the stored key, not the
    // removed env-var name (which would error as unset).
    let reloaded = LeIndexConfig::load().unwrap();
    assert_eq!(
        reloaded.indexing.embeddings.resolved_api_key().unwrap(),
        Some("sk-fresh".to_string())
    );
    unsafe { std::env::remove_var(LEINDEX_HOME_ENV) };
}
