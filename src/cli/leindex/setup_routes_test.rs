use super::*;

fn external() -> LeIndexConfig {
    let mut config = LeIndexConfig::default();
    let embedding = &mut config.indexing.embeddings;
    embedding.remote_provider = "custom".into();
    embedding.remote_endpoint = Some("http://127.0.0.1:9000/v1".into());
    embedding.remote_model = Some("fixture".into());
    embedding.remote_dimension = Some(1024);
    embedding.remote_api_key_env = Some("LEINDEX_TEST_UNSET_KEY_NAME".into());
    embedding.remote_doc_template = Some("document {text}".into());
    embedding.remote_query_template = Some("query {text}".into());
    config
}

#[test]
fn test_external_rerun_preserves_unspecified_fields() {
    let mut config = external();
    let before = serde_json::to_value(&config.indexing.embeddings).unwrap();
    configure_external(&mut config, &SetupOptions::default()).unwrap();
    assert_eq!(
        before,
        serde_json::to_value(&config.indexing.embeddings).unwrap()
    );
}

#[test]
fn test_external_empty_flags_clear_templates_and_authentication() {
    let mut config = external();
    let options = SetupOptions {
        embedding_api_key_env: Some(String::new()),
        embedding_doc_template: Some(String::new()),
        embedding_query_template: Some(String::new()),
        ..Default::default()
    };
    configure_external(&mut config, &options).unwrap();
    assert!(config.indexing.embeddings.remote_api_key_env.is_none());
    assert!(config.indexing.embeddings.remote_api_key.is_none());
    assert!(config.indexing.embeddings.remote_doc_template.is_none());
    assert!(config.indexing.embeddings.remote_query_template.is_none());
    assert_eq!(config.indexing.embeddings.remote_dimension, Some(1024));
}

#[test]
fn test_external_rejects_url_query_fragment_and_zero_dimension() {
    for endpoint in [
        "http://127.0.0.1:9000/v1?token=not-a-secret",
        "http://127.0.0.1:9000/v1#fragment",
    ] {
        let mut config = external();
        let before = serde_json::to_value(&config).unwrap();
        let options = SetupOptions {
            embedding_endpoint: Some(endpoint.into()),
            ..Default::default()
        };
        assert!(configure_external(&mut config, &options).is_err());
        assert_eq!(before, serde_json::to_value(&config).unwrap());
    }
    assert!(
        configure_external(
            &mut external(),
            &SetupOptions {
                embedding_dimension: Some(0),
                ..Default::default()
            }
        )
        .is_err()
    );
}

#[test]
fn test_strip_gpu_options_handles_a_trailing_flag() {
    for flag in ["-ngl", "--gpu-layers", "--n-gpu-layers"] {
        let mut args = vec!["--ctx-size".into(), "8192".into(), flag.into()];
        strip_gpu_options(&mut args);
        assert_eq!(args, ["--ctx-size", "8192"]);
    }
}

#[test]
fn test_gpu_arguments_preserve_and_override_without_duplicates() {
    let mut args: Vec<String> = [
        "--ctx-size",
        "8192",
        "--batch-size",
        "8192",
        "--embedding",
        "--pooling",
        "mean",
        "--gpu-layers=0",
    ]
    .into_iter()
    .map(String::from)
    .collect();
    let before = args.clone();
    configure_args(&mut args, None, false).unwrap();
    assert_eq!(args, before);
    configure_args(&mut args, Some(7), false).unwrap();
    assert_eq!(gpu_layers(&args).unwrap(), Some(7));
    assert!(args.windows(2).any(|pair| pair == ["--batch-size", "8192"]));
    assert_eq!(
        args.iter()
            .filter(|arg| arg.as_str() == "--embedding")
            .count(),
        1
    );
    assert!(configure_args(&mut args, None, true).is_err());
    configure_args(&mut args, Some(0), true).unwrap();
    assert_eq!(gpu_layers(&args).unwrap(), Some(0));
    assert!(gpu_layers(&["-ngl".into()]).is_err());
    assert!(configure_args(&mut vec!["--pooling".into()], None, false).is_err());
}

#[test]
fn test_managed_public_binding_and_invalid_timeout_fail_before_runtime_io() {
    for host in ["0.0.0.0", "::", "example.com", ""] {
        let mut config = LeIndexConfig::default();
        let options = SetupOptions {
            download_llama: Some("cpu".into()),
            embedding_host: Some(host.into()),
            ..Default::default()
        };
        let result = configure_gemma(&mut config, &options)
            .unwrap_err()
            .to_string();
        assert!(result.contains("loopback"), "{result}");
        assert!(config.services.is_empty());
    }
    let mut config = LeIndexConfig::default();
    config.services.insert(
        "gemma".into(),
        crate::config::ServiceConfig {
            start_timeout_secs: 0,
            ..Default::default()
        },
    );
    let result = configure_gemma(
        &mut config,
        &SetupOptions {
            download_llama: Some("cpu".into()),
            ..Default::default()
        },
    )
    .unwrap_err()
    .to_string();
    assert!(result.contains("timeout"));
    assert!(require_neural(false).is_err());
    assert!(require_neural(true).is_ok());
}

#[cfg(target_os = "linux")]
#[test]
fn test_managed_rerun_preserves_alias_autostart_and_extra_arguments() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let binary = temp.path().join("llama-server");
    let model = temp.path().join("fixture.gguf");
    std::fs::write(&binary, b"#!/bin/sh\nprintf 'fixture version\\n'\n").unwrap();
    std::fs::set_permissions(&binary, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(&model, b"fixture").unwrap();
    let service = crate::config::ServiceConfig {
        binary: binary.to_str().unwrap().into(),
        model: Some(model.to_str().unwrap().into()),
        alias: Some("custom-gemma".into()),
        port: 8080,
        auto_start: false,
        extra_args: [
            "--embedding",
            "--pooling",
            "mean",
            "-ngl",
            "0",
            "--batch-size",
            "8192",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        ..Default::default()
    };
    let mut config = LeIndexConfig::default();
    config.services.insert("gemma".into(), service.clone());
    configure_gemma(&mut config, &SetupOptions::default()).unwrap();
    assert_eq!(config.services["gemma"], service);
    assert_eq!(
        config.indexing.embeddings.remote_model.as_deref(),
        Some("custom-gemma")
    );
}

#[test]
fn test_external_rerun_preserves_legacy_auth_and_local_model_override() {
    let mut config = external();
    config.indexing.embeddings.remote_api_key =
        Some(String::from_iter(['f', 'i', 'x', 't', 'u', 'r', 'e']));
    config.indexing.embeddings.onnx_model_path = Some("/fixture/local.onnx".into());
    let before = serde_json::to_value(&config.indexing.embeddings).unwrap();
    configure_external(&mut config, &SetupOptions::default()).unwrap();
    assert!(before == serde_json::to_value(&config.indexing.embeddings).unwrap());
    configure_external(
        &mut config,
        &SetupOptions {
            embedding_api_key_env: Some(String::new()),
            ..Default::default()
        },
    )
    .unwrap();
    assert!(config.indexing.embeddings.remote_api_key.is_none());
}

#[test]
fn test_qwen_clears_remote_resolution() {
    let mut config = external();
    let choices = SetupChoices {
        neural_enabled: true,
        provider: None,
        remote_gemma: false,
        options: SetupOptions {
            embedding: Some("qwen3".into()),
            ..Default::default()
        },
    };
    configure(&mut config, &choices).unwrap();
    assert!(
        config
            .indexing
            .embeddings
            .resolved_remote_with(&config.services)
            .is_none()
    );
}
