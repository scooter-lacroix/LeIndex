// Tests for setup_config.rs. Path-parameterized by design: no test mutates
// process environment state (no LEINDEX_HOME, no unsafe env lock); every
// helper receives an explicit config path. `run()` is a thin dispatch over
// the *_at() internals tested here.
use super::*;

use crate::config::{
    EmbeddingConfig, IndexingConfig, NeuralConfig, SearchConfig, ServiceConfig, SystemOneConfig,
};
use std::collections::BTreeMap;

/// Write TOML contents to `<dir>/config/leindex.toml` and return the path.
fn write_config(dir: &tempfile::TempDir, contents: &str) -> PathBuf {
    let path = dir.path().join("config").join("leindex.toml");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, contents).unwrap();
    path
}

fn service(port: u16) -> ServiceConfig {
    ServiceConfig {
        port,
        ..ServiceConfig::default()
    }
}

#[test]
fn test_validate_accepts_default_config_without_model_or_secret() {
    // The default (fresh-install) config enables the hybrid ladder but sets
    // no model files and no API key: validate must stay purely structural.
    assert!(validate(&LeIndexConfig::default()).is_ok());
}

#[test]
fn test_validate_rejects_unknown_search_mode() {
    let config = LeIndexConfig {
        search: SearchConfig {
            search_mode: "bogus".to_string(),
            ..SearchConfig::default()
        },
        ..LeIndexConfig::default()
    };
    let err = validate(&config).unwrap_err();
    assert!(err.to_string().contains("search.search_mode"), "got: {err}");
}

#[test]
fn test_validate_rejects_out_of_range_probabilities_but_accepts_boundaries() {
    for weight in [1.5, -0.1, f64::NAN] {
        let config = LeIndexConfig {
            search: SearchConfig {
                neural_weight: weight,
                ..SearchConfig::default()
            },
            ..LeIndexConfig::default()
        };
        assert!(
            validate(&config).is_err(),
            "neural_weight {weight} must fail"
        );
    }
    for weight in [0.0, 1.0] {
        let config = LeIndexConfig {
            search: SearchConfig {
                neural_weight: weight,
                ..SearchConfig::default()
            },
            ..LeIndexConfig::default()
        };
        assert!(
            validate(&config).is_ok(),
            "neural_weight {weight} is in range"
        );
    }
    let config = LeIndexConfig {
        system_one: SystemOneConfig {
            relevant_min: 2.0,
            ..SystemOneConfig::default()
        },
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("relevant_min")
    );
}

#[test]
fn test_validate_rejects_zero_batch_size() {
    let config = LeIndexConfig {
        indexing: IndexingConfig {
            batch_size: 0,
            ..IndexingConfig::default()
        },
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("batch_size")
    );
}

#[test]
fn test_validate_rejects_unknown_execution_provider() {
    let config = LeIndexConfig {
        neural: NeuralConfig {
            execution_provider: "tpu".to_string(),
            ..NeuralConfig::default()
        },
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("execution_provider")
    );
}

#[test]
fn test_validate_hybrid_requires_resolvable_endpoint() {
    // custom provider + hybrid on + no endpoint source: the ladder cannot
    // resolve, so validate must reject it.
    let err = validate(&LeIndexConfig {
        indexing: IndexingConfig {
            embeddings: EmbeddingConfig {
                enable_hybrid: true,
                remote_provider: "custom".to_string(),
                ..EmbeddingConfig::default()
            },
            ..IndexingConfig::default()
        },
        ..LeIndexConfig::default()
    })
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("no endpoint resolves"),
        "unexpected message: {err}"
    );

    // The legacy-inert ladder (hybrid off) is valid by definition.
    let inert = EmbeddingConfig {
        enable_hybrid: false,
        ..EmbeddingConfig::default()
    };
    assert!(
        validate(&LeIndexConfig {
            indexing: IndexingConfig {
                embeddings: inert,
                ..IndexingConfig::default()
            },
            ..LeIndexConfig::default()
        })
        .is_ok(),
        "hybrid-off config must validate"
    );
}

#[test]
fn test_validate_rejects_missing_service_reference() {
    let config = LeIndexConfig {
        indexing: IndexingConfig {
            embeddings: EmbeddingConfig {
                remote_service: Some("ghost".to_string()),
                ..EmbeddingConfig::default()
            },
            ..IndexingConfig::default()
        },
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("references missing [services.ghost]")
    );
}

#[test]
fn test_set_on_config_with_missing_service_reference_names_unset_escape() {
    // Cross-reference failures make the WHOLE document unsaveable, so `set`
    // on an unrelated key fails until the dangling reference is gone. The
    // error must name the escape hatch or the user is stuck in a loop.
    let dir = tempfile::tempdir().unwrap();
    let original = "[system_one]\nservice = \"ghost\"\n";
    let path = write_config(&dir, original);

    let err = apply_set_at(&path, "search.search_mode", "\"hybrid\"")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("references missing [services.ghost]")
            && err.contains("leindex config unset system_one.service"),
        "error must name the unset escape hatch: {err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

    // And the escape hatch actually unsticks the file.
    apply_unset_at(&path, "system_one.service").unwrap();
    apply_set_at(&path, "search.search_mode", "\"hybrid\"").unwrap();
}

#[test]
fn test_validate_rejects_non_http_url() {
    // "http:///v1" is NOT hostless: the WHATWG special-authority state skips
    // extra slashes, so it parses as http://v1/ (probed against url 2.5.8).
    // The genuinely hostless forms are "http://:8765" and scheme-only input.
    for endpoint in [
        "not a url",
        "ftp://example.com/v1",
        "http://:8765",
        "http:///v1:99999",
    ] {
        let config = LeIndexConfig {
            indexing: IndexingConfig {
                embeddings: EmbeddingConfig {
                    remote_endpoint: Some(endpoint.to_string()),
                    ..EmbeddingConfig::default()
                },
                ..IndexingConfig::default()
            },
            ..LeIndexConfig::default()
        };
        assert!(validate(&config).is_err(), "{endpoint:?} must fail");
    }
    // Mirror of the setup_routes/setup_status endpoint contract: an embed
    // URL carries no credentials, query, or fragment.
    for endpoint in [
        "https://user:pw@example.com/v1",
        "https://example.com/v1?key=1",
        "https://example.com/v1#frag",
    ] {
        let config = LeIndexConfig {
            indexing: IndexingConfig {
                embeddings: EmbeddingConfig {
                    remote_endpoint: Some(endpoint.to_string()),
                    ..EmbeddingConfig::default()
                },
                ..IndexingConfig::default()
            },
            ..LeIndexConfig::default()
        };
        assert!(
            validate(&config)
                .unwrap_err()
                .to_string()
                .contains("query, or fragment"),
            "{endpoint:?} must be rejected"
        );
    }
    let config = LeIndexConfig {
        indexing: IndexingConfig {
            embeddings: EmbeddingConfig {
                remote_endpoint: Some("https://api.example.com/v1".to_string()),
                ..EmbeddingConfig::default()
            },
            ..IndexingConfig::default()
        },
        ..LeIndexConfig::default()
    };
    assert!(validate(&config).is_ok(), "https endpoint must validate");
}

#[test]
fn test_validate_rejects_unsafe_service_and_preset_names() {
    let mut services = BTreeMap::new();
    services.insert("../evil".to_string(), service(8080));
    let config = LeIndexConfig {
        services,
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("safe filename")
    );

    let mut presets = BTreeMap::new();
    presets.insert(
        "has space".to_string(),
        crate::config::SystemOnePreset::default(),
    );
    let config = LeIndexConfig {
        system_one: SystemOneConfig {
            presets,
            ..SystemOneConfig::default()
        },
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("system_one.presets")
    );
}

#[test]
fn test_validate_rejects_zero_service_port() {
    let mut services = BTreeMap::new();
    services.insert("gemma".to_string(), service(0));
    let config = LeIndexConfig {
        services,
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("services.gemma.port")
    );
}

#[test]
fn test_validate_rejects_system_one_top_n_below_two() {
    for bad in [0, 1] {
        let config = LeIndexConfig {
            system_one: SystemOneConfig {
                top_n: bad,
                ..SystemOneConfig::default()
            },
            ..LeIndexConfig::default()
        };
        let err = validate(&config).unwrap_err().to_string();
        assert!(
            err.contains("system_one.top_n"),
            "top_n={bad} must be rejected with the field path, got: {err}"
        );
    }
    // 2 is the boundary the gate needs (>=2 judges) and must pass.
    let ok = LeIndexConfig {
        system_one: SystemOneConfig {
            top_n: 2,
            ..SystemOneConfig::default()
        },
        ..LeIndexConfig::default()
    };
    assert!(validate(&ok).is_ok());
}

#[test]
fn test_show_redacts_joined_form_service_secret_flags() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        &dir,
        "[indexing.embeddings]\nremote_api_key = \"sk-super-secret\"\n\n[services.llama]\nport = 8080\nextra_args = [\"--api-key=hunter2\", \"--port\", \"1234\", \"--host\", \"10.9.8.7\", \"--api-key\", \"hunter3\"]\n",
    );

    let shown = show_at(&path).unwrap();
    // Joined form is masked as `--flag=REDACTED` without leaking the value.
    assert!(
        shown.contains("--api-key=[REDACTED]"),
        "joined form must be masked: {shown}"
    );
    assert!(!shown.contains("hunter2"), "joined secret leaked: {shown}");
    // The bare form still masks its value...
    assert!(!shown.contains("hunter3"), "bare secret leaked: {shown}");
    // ...and, critically, a joined secret flag does NOT consume the next
    // argument: `1234` stays visible after --api-key=hunter2.
    assert!(
        shown.contains("1234"),
        "value after joined secret must stay visible: {shown}"
    );
    // --host is also secret-bearing here: its bare value is masked (the
    // service's own `host` field is a struct default and stays visible).
    assert!(
        !shown.contains("10.9.8.7"),
        "--host value must be masked: {shown}"
    );
    assert!(!shown.contains("sk-super-secret"));
    // The on-disk file keeps the real secrets (redaction is display-only).
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("hunter2"));
}

#[test]
fn test_show_and_get_redact_remote_api_key() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        &dir,
        "[indexing.embeddings]\nremote_api_key = \"sk-super-secret\"\n",
    );

    let shown = show_at(&path).unwrap();
    assert!(shown.contains("[REDACTED]"), "show output: {shown}");
    assert!(!shown.contains("sk-super-secret"));

    let got = get_at(&path, "indexing.embeddings.remote_api_key").unwrap();
    assert_eq!(got, "\"[REDACTED]\"");
    // The unredacted config never leaves this function; the on-disk file
    // keeps the real secret (redaction is display-only).
    let raw = std::fs::read_to_string(&path).unwrap();
    assert!(raw.contains("sk-super-secret"));
}

#[test]
fn test_set_parses_values_as_toml_not_strings() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(&dir, "");

    apply_set_at(&path, "search.neural_weight", "0.9").unwrap();
    apply_set_at(&path, "services.gemma.port", "8765").unwrap();
    apply_set_at(&path, "search.search_mode", "\"text\"").unwrap();

    let config = LeIndexConfig::load_from_path(&path).unwrap();
    assert_eq!(config.search.neural_weight, 0.9);
    assert_eq!(config.search.search_mode, "text");
    let gemma = config.services.get("gemma").expect("service created");
    assert_eq!(gemma.port, 8765);
    // Serde defaults filled the rest of the service section.
    assert_eq!(gemma.binary, "llama-server");
    assert_eq!(gemma.host, "127.0.0.1");
}

#[test]
fn test_set_rejects_unknown_keys_and_never_writes() {
    let dir = tempfile::tempdir().unwrap();
    let original = "[search]\nsearch_mode = \"text\"\n";
    let path = write_config(&dir, original);

    for key in ["made_up.field", "search.nope"] {
        let err = apply_set_at(&path, key, "1").unwrap_err().to_string();
        assert!(err.contains("unknown config key"), "{key}: {err}");
    }
    // "services" IS a known section; assigning a non-table to it is caught
    // by the typed parse instead of key lookup.
    let err = apply_set_at(&path, "services", "1")
        .unwrap_err()
        .to_string();
    assert!(err.contains("would make the config invalid"), "got: {err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn test_set_rejects_invalid_toml_and_type_mismatches_without_writing() {
    let dir = tempfile::tempdir().unwrap();
    let original = "[search]\nneural_weight = 0.4\n";
    let path = write_config(&dir, original);

    let err = apply_set_at(&path, "search.neural_weight", "half")
        .unwrap_err()
        .to_string();
    // The outer anyhow context names the key; the inner one names the TOML error.
    assert!(err.contains("invalid TOML value"), "got: {err}");

    let err = apply_set_at(&path, "search.neural_weight", "\"0.9\"")
        .unwrap_err()
        .to_string();
    assert!(err.contains("would make the config invalid"), "got: {err}");

    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn test_set_rejects_value_that_fails_validate_and_keeps_file_intact() {
    let dir = tempfile::tempdir().unwrap();
    let original = "[system_one]\nrelevant_min = 0.45\n";
    let path = write_config(&dir, original);

    let err = apply_set_at(&path, "system_one.relevant_min", "5.0")
        .unwrap_err()
        .to_string();
    assert!(err.contains("outside [0, 1]"), "got: {err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn test_set_sweeps_unknown_keys_already_in_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let original = "totally_unknown = 1\n\n[search]\nsearch_mode = \"text\"\n";
    let path = write_config(&dir, original);

    let err = apply_set_at(&path, "mcp.prewarm", "\"off\"")
        .unwrap_err()
        .to_string();
    assert!(err.contains("totally_unknown"), "got: {err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}

#[test]
fn test_unset_removes_keys_and_validates_surviving_references() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        &dir,
        "[indexing.embeddings]\nremote_service = \"gemma\"\n\n\
         [services.gemma]\nport = 8765\n",
    );

    // Unsetting the service while it is still referenced must fail (validate
    // runs on the edited document) and leave the file untouched.
    let before = std::fs::read_to_string(&path).unwrap();
    let err = apply_unset_at(&path, "services.gemma")
        .unwrap_err()
        .to_string();
    assert!(err.contains("missing [services.gemma]"), "got: {err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);

    // Drop the reference first, then the service removes cleanly.
    apply_unset_at(&path, "indexing.embeddings.remote_service").unwrap();
    apply_unset_at(&path, "services.gemma").unwrap();
    let config = LeIndexConfig::load_from_path(&path).unwrap();
    assert!(config.services.is_empty());
    assert!(config.indexing.embeddings.remote_service.is_none());
}

#[test]
fn test_unset_errors_for_missing_keys() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(&dir, "[search]\nsearch_mode = \"text\"\n");

    assert!(
        apply_unset_at(&path, "mcp.prewarm")
            .unwrap_err()
            .to_string()
            .contains("not set")
    );
    assert!(
        apply_unset_at(&path, "services.gemma.port")
            .unwrap_err()
            .to_string()
            .contains("not set")
    );
}

#[test]
fn test_set_on_missing_file_starts_from_defaults() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config").join("leindex.toml");

    apply_set_at(&path, "mcp.prewarm", "\"graph\"").unwrap();
    let config = LeIndexConfig::load_from_path(&path).unwrap();
    assert_eq!(config.mcp.prewarm, "graph");
    // Everything else stays at the serde default (nothing was dropped).
    assert_eq!(config.mcp.idle_timeout_secs, 1800);
}

#[test]
fn test_get_reports_unset_keys_and_rejects_unknown_paths() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(&dir, "[search]\nsearch_mode = \"text\"\n");

    assert_eq!(get_at(&path, "search.search_mode").unwrap(), "\"text\"");
    // Known scalar keys are always present in the typed config (serde
    // defaults fill them); the genuine "not set" case is a dynamic section
    // that does not exist in the file.
    assert_eq!(get_at(&path, "mcp.prewarm").unwrap(), "\"full\"");
    assert!(
        get_at(&path, "services.gemma.port")
            .unwrap_err()
            .to_string()
            .contains("not set")
    );
    assert!(
        get_at(&path, "nope.nope")
            .unwrap_err()
            .to_string()
            .contains("unknown config key")
    );
}

#[test]
fn test_parse_toml_value_preserves_toml_types() {
    assert_eq!(
        parse_toml_value("8765").unwrap(),
        toml::Value::Integer(8765)
    );
    assert_eq!(
        parse_toml_value("\"hi\"").unwrap(),
        toml::Value::String("hi".to_string())
    );
    assert_eq!(
        parse_toml_value("true").unwrap(),
        toml::Value::Boolean(true)
    );
    assert!(parse_toml_value("open").is_err());
}

#[test]
fn test_is_known_path_covers_schema_including_optional_keys() {
    assert!(is_known_path(&["search"]));
    assert!(is_known_path(&["search", "neural_weight"]));
    // Optional (skip_serializing_if) fields are absent from the reflected
    // default but remain settable.
    assert!(is_known_path(&["neural", "ort_dylib_path"]));
    assert!(is_known_path(&["indexing", "embeddings", "remote_api_key"]));
    assert!(is_known_path(&[
        "indexing",
        "embeddings",
        "remote_api_key_env"
    ]));
    assert!(is_known_path(&["services", "gemma", "extra_args"]));
    assert!(is_known_path(&["system_one", "presets", "jev", "provider"]));
    assert!(is_known_path(&["system_one", "presets", "jev"]));

    assert!(!is_known_path(&["nope"]));
    assert!(!is_known_path(&["search", "nope"]));
    assert!(!is_known_path(&["services", "gemma", "nope"]));
    assert!(!is_known_path(&["system_one", "presets", "jev", "nope"]));
    assert!(!is_known_path(&["neural", "model_dir", "extra"]));
}

#[test]
fn test_key_segments_rejects_empty_and_oversized_paths() {
    assert!(key_segments("").is_err());
    assert!(key_segments("search.").is_err());
    assert!(key_segments(".search").is_err());
    assert!(key_segments("a.b.c.d.e").is_err());
    assert_eq!(
        key_segments("search.neural_weight").unwrap(),
        vec!["search", "neural_weight"]
    );
}

#[test]
fn test_validate_flags_feature_gated_enums_when_system_one_enabled() {
    // Statically guarantee the gated branch stays compiled and wired: this
    // file is only built under the cli feature set that ships system-one
    // (full), so the parse checks run here rather than silently vanishing.
    let config = LeIndexConfig {
        system_one: SystemOneConfig {
            mode: "sideways".to_string(),
            ..SystemOneConfig::default()
        },
        ..LeIndexConfig::default()
    };
    #[cfg(feature = "system-one")]
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("system_one.mode")
    );
    #[cfg(not(feature = "system-one"))]
    assert!(validate(&config).is_ok());
}

#[test]
fn test_schema_drift_every_serialized_default_key_is_known() {
    // Regressions against config.rs schema drift: EVERY key the serde
    // default serializes must be accepted by is_known_path, and the
    // optional-keys list must not name fields the schema has dropped.
    // A field added to config.rs without updating this module's schema map
    // fails here instead of being rejected by `config set` (or silently
    // dropped on save).
    let rendered =
        toml::to_string_pretty(&LeIndexConfig::default()).expect("default config serializes");
    let value: toml::Value = toml::from_str(&rendered).expect("rendered default parses");
    let mut checked = 0;
    let root = value.as_table().expect("top-level table");
    for (section, section_value) in root {
        assert!(is_known_path(&[section]), "unknown section '{section}'");
        let Some(fields) = section_value.as_table() else {
            continue;
        };
        for field in fields.keys() {
            assert!(
                is_known_path(&[section, field]),
                "unknown field '{section}.{field}' — add it to the schema map in \
                 setup_config.rs (EMBEDDINGS_FIELDS/optional_keys_at/...)"
            );
            checked += 1;
            if let Some(nested) = fields.get(field).and_then(|v| v.as_table()) {
                for name in nested.keys() {
                    assert!(
                        is_known_path(&[section.as_str(), field.as_str(), name.as_str()]),
                        "unknown nested key '{section}.{field}.{name}'"
                    );
                }
            }
        }
    }
    assert!(checked >= 25, "schema reflection saw only {checked} fields");

    // The reverse direction: optional keys must still exist in the real
    // schema. `remote_api_key_env` regressed exactly this way — absent from
    // both the schema map and the default serialization, so no round-trip
    // caught it. Asserting the optional list's settable-ness directly pins
    // each entry against the config structs at compile time.
    let probe = crate::config::EmbeddingConfig::default();
    let _ = (&probe.remote_api_key, &probe.remote_api_key_env);
    assert!(
        is_known_path(&["indexing", "embeddings", "remote_api_key_env"]),
        "remote_api_key_env must remain a settable config key"
    );
}

#[test]
fn test_set_and_unset_remote_api_key_env_name() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(&dir, "[search]\nsearch_mode = \"text\"\n");

    apply_set_at(
        &path,
        "indexing.embeddings.remote_api_key_env",
        "\"OPENAI_API_KEY\"",
    )
    .unwrap();
    assert_eq!(
        get_at(&path, "indexing.embeddings.remote_api_key_env").unwrap(),
        "\"OPENAI_API_KEY\""
    );

    // Only the NAME is validated; presence in the environment is runtime.
    apply_set_at(
        &path,
        "indexing.embeddings.remote_api_key_env",
        "\"DEFINITELY_UNSET_VAR\"",
    )
    .unwrap();

    apply_unset_at(&path, "indexing.embeddings.remote_api_key_env").unwrap();
    let config = LeIndexConfig::load_from_path(&path).unwrap();
    assert!(config.indexing.embeddings.remote_api_key_env.is_none());
}

#[test]
fn test_validate_rejects_malformed_env_var_names() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(&dir, "[search]\nsearch_mode = \"text\"\n");

    for (key, bad) in [
        (
            "indexing.embeddings.remote_api_key_env",
            "9STARTS_WITH_DIGIT",
        ),
        ("indexing.embeddings.remote_api_key_env", "has-dash"),
        ("system_one.api_key_env", "has space"),
        ("system_one.api_key_env", ""),
    ] {
        let err = apply_set_at(&path, key, &format!("{bad:?}"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("environment variable name"),
            "{key}={bad:?}: {err}"
        );
    }
    // The file is never rewritten on rejection.
    assert!(
        LeIndexConfig::load_from_path(&path)
            .unwrap()
            .indexing
            .embeddings
            .remote_api_key_env
            .is_none()
    );
}

#[test]
fn test_validate_accepts_valid_env_var_names_everywhere() {
    let mut services = BTreeMap::new();
    services.insert("gemma".to_string(), service(8765));
    let config = LeIndexConfig {
        indexing: IndexingConfig {
            embeddings: EmbeddingConfig {
                remote_api_key_env: Some("GEMMA_API_KEY".to_string()),
                ..EmbeddingConfig::default()
            },
            ..IndexingConfig::default()
        },
        system_one: SystemOneConfig {
            api_key_env: Some("SYSTEM_ONE_API_KEY".to_string()),
            presets: BTreeMap::from([(
                "jev".to_string(),
                crate::config::SystemOnePreset {
                    api_key_env: Some("JEV_API_KEY".to_string()),
                    ..crate::config::SystemOnePreset::default()
                },
            )]),
            ..SystemOneConfig::default()
        },
        services,
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config).is_ok(),
        "valid env NAMEs pass with no environment lookup: {:?}",
        validate(&config).err()
    );
}

#[test]
fn test_validate_rejects_zero_start_timeout_and_s1_timeout() {
    let mut services = BTreeMap::new();
    services.insert("gemma".to_string(), service(8765));
    let config = LeIndexConfig {
        services,
        system_one: SystemOneConfig {
            timeout_ms: 0,
            ..SystemOneConfig::default()
        },
        ..LeIndexConfig::default()
    };
    let err = validate(&config).unwrap_err().to_string();
    assert!(err.contains("timeout"), "got: {err}");

    let mut services = BTreeMap::new();
    services.insert(
        "gemma".to_string(),
        ServiceConfig {
            start_timeout_secs: 0,
            ..service(8765)
        },
    );
    let config = LeIndexConfig {
        services,
        ..LeIndexConfig::default()
    };
    assert!(
        validate(&config)
            .unwrap_err()
            .to_string()
            .contains("start_timeout_secs")
    );
}

#[test]
fn test_show_and_get_redact_secret_extra_args() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_config(
        &dir,
        "[services.gemma]\nport = 8765\nextra_args = [\"--host\", \"secret-host-key\", \
         \"--ctx-size\", \"4096\"]\n",
    );

    let shown = show_at(&path).unwrap();
    assert!(!shown.contains("secret-host-key"), "leaked: {shown}");
    assert!(shown.contains("[REDACTED]"), "no redaction marker: {shown}");
    // The non-secret flag value survives.
    assert!(shown.contains("4096"), "over-redacted: {shown}");

    let got = get_at(&path, "services.gemma.extra_args").unwrap();
    assert!(!got.contains("secret-host-key"), "leaked: {got}");
}

#[test]
fn test_unset_removes_each_of_two_dangling_references() {
    // The unset escape hatch must not dead-end on a SECOND dangling
    // reference: unset is the repair command, so the edited document saves
    // while another dangling reference remains (deferred strict validation,
    // see commit_table_at), and only a fully-resolved document validates
    // strictly again.
    let dir = tempfile::tempdir().unwrap();
    let original = concat!(
        "[indexing.embeddings]\n",
        "remote_service = \"ghost1\"\n",
        "\n",
        "[system_one]\n",
        "service = \"ghost2\"\n",
    );
    let path = write_config(&dir, original);

    // While both references dangle, `set` is still refused (whole-document
    // strict validation) and the file is untouched.
    let err = apply_set_at(&path, "search.search_mode", "\"hybrid\"")
        .unwrap_err()
        .to_string();
    assert!(err.contains("references missing"), "must refuse set: {err}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);

    // The suggested escape for ghost2 works even though ghost1 still dangles…
    apply_unset_at(&path, "system_one.service").unwrap();
    // …and so does the escape for ghost1.
    apply_unset_at(&path, "indexing.embeddings.remote_service").unwrap();

    // Fully resolved again: set validates strictly and succeeds.
    apply_set_at(&path, "search.search_mode", "\"hybrid\"").unwrap();
}

#[test]
fn test_unset_repair_refuses_when_scrubbed_document_still_invalid() {
    // A pre-existing dangling reference must not shield other violations:
    // validate is short-circuiting, so the repair re-validates with the
    // pre-existing references blanked out. Here the zero timeout_ms hides
    // behind the surviving ghost reference in the first pass — the repair
    // must refuse instead of saving an invalid document.
    let dir = tempfile::tempdir().unwrap();
    let original = "[indexing.embeddings]\nremote_service = \"ghost1\"\n\n\
                    [system_one]\nservice = \"ghost2\"\ntimeout_ms = 0\n";
    let path = write_config(&dir, original);

    let err = apply_unset_at(&path, "indexing.embeddings.remote_service")
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("timeout_ms"),
        "must surface the masked violation, not the reference error: {err}"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
}
