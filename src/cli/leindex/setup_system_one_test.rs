use super::*;
use crate::config::SystemOnePreset;

fn enabled_laya() -> LeIndexConfig {
    let mut config = LeIndexConfig::default();
    config.system_one.mode = "shadow".to_string();
    config
}

#[test]
fn test_section_issues_accepts_default_laya_shadow() {
    assert!(section_issues(&enabled_laya()).is_empty());
}

#[test]
fn test_section_issues_rejects_unknown_mode_and_provider() {
    let mut config = enabled_laya();
    config.system_one.mode = "on".to_string();
    config.system_one.provider = "anthropic".to_string();
    let issues = section_issues(&config);
    assert!(issues.iter().any(|i| i.contains("unknown mode")));
    assert!(issues.iter().any(|i| i.contains("unknown provider")));
}

#[test]
fn test_section_issues_requires_key_env_name_for_remote_provider() {
    let mut config = enabled_laya();
    config.system_one.provider = "openai".to_string();
    assert!(
        section_issues(&config)
            .iter()
            .any(|i| i.contains("api_key_env"))
    );
}

#[test]
fn test_section_issues_rejects_a_pasted_key_as_env_name() {
    let mut config = enabled_laya();
    config.system_one.provider = "openai".to_string();
    config.system_one.api_key_env = Some("sk-live-abc123".to_string());
    assert!(
        section_issues(&config)
            .iter()
            .any(|i| i.contains("NAME, not a key") || i.contains("environment variable NAME"))
    );
}

#[test]
fn test_section_issues_thresholds_keep_runtime_invariants() {
    let mut config = enabled_laya();
    config.system_one.relevant_min = 1.5;
    config.system_one.tau_apply = -0.1;
    config.system_one.timeout_ms = 0;
    config.system_one.top_n = 0;
    let issues = section_issues(&config);
    assert!(issues.iter().any(|i| i.starts_with("relevant_min")));
    assert!(issues.iter().any(|i| i.starts_with("tau_apply")));
    assert!(issues.iter().any(|i| i.starts_with("timeout_ms")));
    assert!(issues.iter().any(|i| i.starts_with("top_n")));
}

#[test]
fn test_section_issues_accepts_large_timeout_and_top_n() {
    let mut config = enabled_laya();
    config.system_one.timeout_ms = 600_000;
    config.system_one.top_n = 500;
    assert!(section_issues(&config).is_empty());
}

#[test]
fn test_section_issues_flags_service_without_entry() {
    let mut config = enabled_laya();
    config.system_one.service = Some("laya".to_string());
    assert!(
        section_issues(&config)
            .iter()
            .any(|i| i.contains("no [services.laya]"))
    );
}

#[test]
fn test_finish_section_clears_active_preset_on_change() {
    let original = SystemOneConfig {
        active_preset: Some("local".to_string()),
        ..SystemOneConfig::default()
    };
    let edited = SystemOneConfig {
        mode: "shadow".to_string(),
        ..original.clone()
    };
    assert_eq!(finish_section(&original, edited).active_preset, None);
}

#[test]
fn test_finish_section_keeps_active_preset_when_unchanged() {
    let original = SystemOneConfig {
        active_preset: Some("local".to_string()),
        ..SystemOneConfig::default()
    };
    assert_eq!(
        finish_section(&original, original.clone())
            .active_preset
            .as_deref(),
        Some("local")
    );
}

#[test]
fn test_saved_presets_survive_a_change() {
    let mut presets = std::collections::BTreeMap::new();
    presets.insert(
        "openai".to_string(),
        SystemOnePreset {
            provider: Some("openai".to_string()),
            ..SystemOnePreset::default()
        },
    );
    let original = SystemOneConfig {
        presets: presets.clone(),
        active_preset: Some("openai".to_string()),
        ..SystemOneConfig::default()
    };
    let edited = SystemOneConfig {
        mode: "enforce".to_string(),
        ..original.clone()
    };
    assert_eq!(finish_section(&original, edited).presets, presets);
}

#[test]
fn test_env_name_accepts_identifier_with_underscores() {
    assert!(is_env_name("LEINDEX_OPENAI_KEY"));
    assert!(is_env_name("_private1"));
}

#[test]
fn test_env_name_rejects_pasted_key_shapes() {
    assert!(!is_env_name("sk-live-abc123"));
    assert!(!is_env_name("1ABC"));
    assert!(!is_env_name("KEYé"));
    assert!(!is_env_name("a.b"));
    assert!(!is_env_name("A=B"));
    assert!(!is_env_name(""));
    assert!(!is_env_name("has space"));
}

#[test]
fn test_redact_userinfo_hides_credentials() {
    assert_eq!(
        redact_userinfo("https://user:hunter2@api.example.com/v1"),
        "https://<redacted>@api.example.com/v1"
    );
    assert_eq!(
        redact_userinfo("http://127.0.0.1:8000"),
        "http://127.0.0.1:8000"
    );
}

#[test]
fn test_endpoint_label_never_prints_userinfo() {
    let mut config = enabled_laya();
    config.system_one.base_url = Some("http://svc:pw@127.0.0.1:8000".to_string());
    let label = endpoint_label(&config, &config.system_one.clone());
    assert!(!label.contains("pw"), "{label}");
}

#[test]
fn test_set_extra_arg_replaces_existing_pair_only() {
    let mut args = vec![
        "--ctx-size".to_string(),
        "2048".to_string(),
        "--embeddings".to_string(),
    ];
    set_extra_arg(&mut args, &["--ctx-size", "-c"], "8192");
    assert_eq!(args, vec!["--embeddings", "--ctx-size", "8192"]);
}

#[test]
fn test_set_extra_arg_replaces_equals_form() {
    let mut args = vec!["--ctx-size=2048".to_string()];
    set_extra_arg(&mut args, &["--ctx-size", "-c"], "4096");
    assert_eq!(args, vec!["--ctx-size", "4096"]);
}

#[test]
fn test_enable_guard_matches_build_feature() {
    let result = enable_guard("shadow");
    assert_eq!(result.is_ok(), cfg!(feature = "system-one"));
    assert!(enable_guard("off").is_ok());
}

#[test]
fn test_print_status_reports_default_config_inactive() {
    assert!(!print_status(&LeIndexConfig::default()));
}

#[test]
fn test_smoke_test_refuses_when_mode_off_without_network() {
    let result = smoke_test(&LeIndexConfig::default());
    assert!(matches!(result, Err(SetupError::Conflict { .. })));
}

#[test]
fn test_smoke_test_refuses_remote_provider_without_key_without_network() {
    let mut config = enabled_laya();
    config.system_one.provider = "jev".to_string();
    assert!(matches!(
        smoke_test(&config),
        Err(SetupError::Conflict { .. })
    ));
}

#[test]
fn test_check_probe_refuses_when_mode_off_without_network() {
    assert!(matches!(
        check_probe(&LeIndexConfig::default()),
        Err(SetupError::Conflict { .. })
    ));
}

#[cfg(feature = "system-one")]
#[test]
fn test_check_probe_reports_missing_model_without_spawning() {
    // Structural validation stays in the no-start probe: a nonexistent model is a
    // Conflict before any HTTP or spawn.
    let mut config = enabled_laya();
    config.system_one.service = Some("svc".to_string());
    config.services.insert(
        "svc".to_string(),
        crate::config::ServiceConfig {
            binary: "/nonexistent/leindex-no-such-binary".to_string(),
            model: Some("/nonexistent/model.gguf".to_string()),
            port: 1,
            auto_start: true,
            ..Default::default()
        },
    );
    crate::feature_flags::with_flag_override(
        crate::feature_flags::FeatureFlag::SystemOne,
        true,
        || {
            let result = check_probe(&config);
            assert!(
                matches!(&result, Err(SetupError::Conflict { message }) if message.contains("model file not found")),
                "{result:?}"
            );
        },
    );
}

#[cfg(feature = "system-one")]
#[test]
fn test_check_probe_never_spawns_managed_service() {
    // Valid model file + closed port + binary that would write a marker if executed.
    // The probe must fail at the HTTP step and never run the binary. smoke_test would
    // reach ensure_running and fail with a `service 'svc'` error instead.
    let dir = std::env::temp_dir().join(format!("leindex-s1-probe-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create temp dir");
    let model = dir.join("model.gguf");
    std::fs::write(&model, b"fake").expect("write model");
    let marker = dir.join("spawned");
    let binary = dir.join("svc-bin");
    std::fs::write(&binary, format!("#!/bin/sh\ntouch {}\n", marker.display()))
        .expect("write binary");
    std::fs::set_permissions(&binary, std::os::unix::fs::PermissionsExt::from_mode(0o755))
        .expect("chmod binary");
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let mut config = enabled_laya();
    config.system_one.service = Some("svc".to_string());
    config.services.insert(
        "svc".to_string(),
        crate::config::ServiceConfig {
            binary: binary.display().to_string(),
            model: Some(model.display().to_string()),
            port,
            auto_start: true,
            ..Default::default()
        },
    );
    crate::feature_flags::with_flag_override(
        crate::feature_flags::FeatureFlag::SystemOne,
        true,
        || {
            let result = check_probe(&config);
            assert!(
                matches!(&result, Err(SetupError::Io(message)) if message.contains("smoke test request failed")),
                "{result:?}"
            );
            assert!(!marker.exists(), "check_probe spawned the managed service");
        },
    );
    std::fs::remove_dir_all(&dir).expect("remove temp dir");
}
